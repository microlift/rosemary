use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::plist::{self, Value};

/// A path this crate holds, as the guest spells it.
///
/// Every `NSBundle` and `CFBundle` answer goes through here. A [`Layout`] holds
/// *host* paths, because it opens files with them - `resource` probes with
/// `exists`, `localization_names` reads a directory, `icon` is handed to the
/// image decoder. What a guest gets back has to be the other spelling: it joins
/// onto `-bundlePath` to find its data beside its saves, and compares
/// `-executablePath` against what `_NSGetExecutablePath` told it.
///
/// `None` for a path no mount reaches, which every caller below turns into the
/// nil that `NSBundle` already answers for a resource that is not there.
fn spelled(path: &Path) -> Option<PathBuf> {
    prefix::ns::to_guest(path)
}

/// A path a guest named, as this crate must open it.
///
/// The way back in, for `+bundleWithPath:` and `CFBundleCreate` - the two
/// places a guest hands a path to this module rather than being given one.
fn opened(path: &Path) -> PathBuf {
    prefix::ns::existing_host_path(&path.to_string_lossy())
}

/// The CPU types a Mach-O file at `path` has code for: every slice of a fat
/// file, or the one type of a thin one.
fn cpu_types(path: &Path) -> Option<Vec<i32>> {
    use std::io::Read;
    let mut head = [0u8; 4096];
    let mut file = std::fs::File::open(path).ok()?;
    let n = file.read(&mut head).ok()?;
    let head = &head[..n];
    let word = |at: usize, be: bool| -> Option<u32> {
        let bytes: [u8; 4] = head.get(at..at + 4)?.try_into().ok()?;
        Some(if be {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };
    match word(0, true)? {
        // FAT_MAGIC and FAT_MAGIC_64: big-endian, one entry per slice, the CPU
        // type first in each. Entries are 20 and 32 bytes respectively.
        magic @ (0xcafe_babe | 0xcafe_babf) => {
            let stride = if magic == 0xcafe_babe { 20 } else { 32 };
            let count = word(4, true)? as usize;
            (0..count)
                .map(|i| word(8 + i * stride, true).map(|t| t as i32))
                .collect()
        }
        // MH_MAGIC and MH_MAGIC_64, in this file's byte order.
        0xcefa_edfe | 0xcffa_edfe => Some(vec![word(4, false)? as i32]),
        0xfeed_face | 0xfeed_facf => Some(vec![word(4, true)? as i32]),
        _ => None,
    }
}

/// A `.app` on disk, and what its `Info.plist` says.
#[derive(Debug)]
pub struct Layout {
    /// `.../DELTARUNE.app`
    pub root: PathBuf,
    /// `.../DELTARUNE.app/Contents`
    pub contents: PathBuf,
    pub resources: PathBuf,
    pub frameworks: PathBuf,
    /// `.../Foo.app/Contents/PlugIns`, spelled the way this bundle spells it -
    /// see [`plugins_of`].
    pub plugins: PathBuf,
    pub executable: PathBuf,
    pub info: Value,
}

/// The built-in plug-ins directory of the bundle at `contents`.
///
/// Apple's spelling is `PlugIns`, and that is what is looked for first. The
/// fallback is what a case-insensitive volume gives away for free: a Mac's HFS+
/// or APFS opens `Contents/PlugIns` on a bundle that ships `Contents/Plugins`
/// without anyone noticing the difference, so a bundle built and tested only on
/// a Mac may have the name in any case at all - an example app ships `Plugins`.
/// On ext4 those are two different directories and the exact name finds nothing,
/// which is a Qt application with no platform plugin and no way to open a
/// window.
///
/// A bundle with neither gets `Contents/PlugIns` anyway, because the answer is
/// where a plug-in *would* be: that is what
/// [`cf::CFBundleCopyBuiltInPlugInsURL`] is for, and a caller that finds nothing
/// there is entitled to the path rather than to a null.
fn plugins_of(contents: &Path) -> PathBuf {
    const APPLE: &str = "PlugIns";
    let exact = contents.join(APPLE);
    if exact.is_dir() {
        return exact;
    }
    let found = std::fs::read_dir(contents).ok().and_then(|entries| {
        entries
            .flatten()
            .find(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(APPLE))
                    && e.path().is_dir()
            })
            .map(|e| e.path())
    });
    found.unwrap_or(exact)
}

/// `CFBundleIdentifier` of the Apple framework named `leaf`, as its
/// `Info.plist` on a Mac spells it.
///
/// Only frameworks whose identifier is known go here: the spellings are not
/// regular (`com.apple.opengl`, `com.apple.framework.IOKit`), and a guessed one
/// would be found by nobody.
const APPLE_IDENTIFIERS: &[(&str, &str)] = &[
    ("AGL", "com.apple.AGL"),
    ("AppKit", "com.apple.AppKit"),
    ("ApplicationServices", "com.apple.ApplicationServices"),
    ("AudioToolbox", "com.apple.audio.toolbox.AudioToolbox"),
    ("Carbon", "com.apple.Carbon"),
    ("Cocoa", "com.apple.Cocoa"),
    ("CoreAudio", "com.apple.audio.CoreAudio"),
    ("CoreFoundation", "com.apple.CoreFoundation"),
    ("CoreGraphics", "com.apple.CoreGraphics"),
    ("CoreServices", "com.apple.CoreServices"),
    ("CoreVideo", "com.apple.CoreVideo"),
    ("Foundation", "com.apple.Foundation"),
    ("GameController", "com.apple.GameController"),
    ("IOKit", "com.apple.framework.IOKit"),
    ("Metal", "com.apple.Metal"),
    ("OpenGL", "com.apple.opengl"),
    ("QuartzCore", "com.apple.QuartzCore"),
    ("Security", "com.apple.security"),
    ("WebKit", "com.apple.WebKit"),
];

fn apple_identifier(leaf: &str) -> Option<&'static str> {
    APPLE_IDENTIFIERS
        .iter()
        .find(|(name, _)| *name == leaf)
        .map(|(_, identifier)| *identifier)
}

/// The guest path of the Apple framework `identifier` names. CFBundle compares
/// identifiers without regard to case.
fn apple_framework(identifier: &str) -> Option<String> {
    APPLE_IDENTIFIERS
        .iter()
        .find(|(_, known)| known.eq_ignore_ascii_case(identifier))
        .map(|(name, _)| format!("/System/Library/Frameworks/{name}.framework"))
}

impl Layout {
    /// The bundle `executable` is inside, or `None` for a loose binary.
    pub fn enclosing(executable: &Path) -> Option<Layout> {
        // Absolute, always. Apple's `-bundlePath` is, and a program that joins
        // it onto another directory - which is exactly what a game does to find
        // its data beside its save files - turns a relative one into a path
        // that does not exist. Symlinks are kept, as `_NSGetExecutablePath`
        // keeps them: a `.app` linked in from elsewhere looks for its data
        // beside the link, not beside its target.
        let absolute = std::path::absolute(executable).ok();
        let executable = absolute.as_deref().unwrap_or(executable);
        let macos = executable.parent()?;
        if macos.file_name()? != "MacOS" {
            return None;
        }
        let contents = macos.parent()?;
        if contents.file_name()? != "Contents" {
            return None;
        }
        let info_path = contents.join("Info.plist");
        if !info_path.is_file() {
            return None;
        }
        let info = plist::parse(&std::fs::read(&info_path).ok()?).ok()?;
        Some(Layout {
            root: contents.parent()?.to_path_buf(),
            resources: contents.join("Resources"),
            frameworks: contents.join("Frameworks"),
            plugins: plugins_of(contents),
            executable: executable.to_path_buf(),
            contents: contents.to_path_buf(),
            info,
        })
    }

    /// The bundle *at* `root`, which is how `+bundleWithPath:` asks.
    ///
    /// The same shape [`Layout::enclosing`] recognises, entered from the other
    /// end: a directory whose `Contents/Info.plist` is a file. The executable is
    /// named by `CFBundleExecutable` rather than found by looking, because a
    /// `.bundle` with a resource beside its binary in `MacOS` has no other way
    /// to say which one it is.
    pub fn at(root: &Path) -> Option<Layout> {
        let absolute = root.canonicalize().ok();
        let root = absolute.as_deref().unwrap_or(root);
        let contents = root.join("Contents");
        let info_path = contents.join("Info.plist");
        // CFBundle takes a bundle with no `Info.plist` at all, as an empty
        // dictionary: plug-in `.bundle`s built outside Xcode are often only
        // `Contents/MacOS/<name>`, and Unity loads every native plug-in that way.
        let info = match info_path.is_file() {
            true => plist::parse(&std::fs::read(&info_path).ok()?).ok()?,
            false if contents.is_dir() => Value::Dictionary(Default::default()),
            false => return None,
        };
        // CFBundle's rule: an empty or missing name falls back to the bundle's
        // own, extension stripped. Roblox Studio's protocol handler ships
        // `CFBundleExecutable` as an empty string.
        let executable = ["CFBundleExecutable", "NSExecutable"]
            .iter()
            .find_map(|key| info.get(key)?.as_str().filter(|name| !name.is_empty()))
            .map(std::ffi::OsStr::new)
            .or_else(|| root.file_stem())
            .map(|name| contents.join("MacOS").join(name))
            .unwrap_or_default();
        Some(Layout {
            root: root.to_path_buf(),
            resources: contents.join("Resources"),
            frameworks: contents.join("Frameworks"),
            plugins: plugins_of(&contents),
            executable,
            contents,
            info,
        })
    }

    /// The bundle of a framework this runtime provides, at its guest path
    /// `/System/Library/Frameworks/X.framework`.
    ///
    /// Nothing of it is on disk, so there is no `Info.plist` to read: the
    /// dictionary holds only what the shape of a framework already says - its
    /// executable is `X`, beside the bundle's top level, and it is a framework -
    /// plus the identifier a Mac gives it, for the frameworks
    /// [`apple_identifier`] knows. Any other identifier, or a version, would be
    /// invented, and a caller comparing one would be told something no Mac says.
    pub fn provided_framework(guest: &str) -> Option<Layout> {
        let guest = guest.trim_end_matches('/');
        if !darwin::images::virtual_directory(guest) {
            return None;
        }
        let leaf = Path::new(guest)
            .file_name()?
            .to_str()?
            .strip_suffix(".framework")?
            .to_string();
        let root = opened(Path::new(guest));
        let mut info = std::collections::BTreeMap::from([
            (
                "CFBundleExecutable".to_string(),
                Value::String(leaf.clone()),
            ),
            (
                "CFBundlePackageType".to_string(),
                Value::String("FMWK".into()),
            ),
        ]);
        if let Some(identifier) = apple_identifier(&leaf) {
            info.insert(
                "CFBundleIdentifier".to_string(),
                Value::String(identifier.into()),
            );
        }
        let info = Value::Dictionary(info);
        Some(Layout {
            resources: root.join("Resources"),
            frameworks: root.join("Frameworks"),
            plugins: root.join("PlugIns"),
            executable: root.join(&leaf),
            contents: root.clone(),
            root,
            info,
        })
    }

    /// An `Info.plist` string, which is how every question below is asked.
    pub fn info_string(&self, key: &str) -> Option<&str> {
        self.info.get(key)?.as_str()
    }

    pub fn identifier(&self) -> Option<&str> {
        self.info_string("CFBundleIdentifier")
    }

    /// The name to show a person, which is not the executable's.
    ///
    /// `CFBundleDisplayName` first, because `CFBundleName` is so often the
    /// executable instead: DELTARUNE's is `Mac_Runner` and its display name is
    /// the game's.
    pub fn display_name(&self) -> Option<&str> {
        self.info_string("CFBundleDisplayName")
            .or_else(|| self.info_string("CFBundleName"))
    }

    /// The version to show a person, which is the marketing one where a bundle
    /// distinguishes it from the build number.
    pub fn version(&self) -> Option<&str> {
        self.info_string("CFBundleShortVersionString")
            .or_else(|| self.info_string("CFBundleVersion"))
    }

    pub fn copyright(&self) -> Option<&str> {
        self.info_string("NSHumanReadableCopyright")
    }

    /// `LSApplicationCategoryType`, as the uniform type identifier the plist
    /// spells it - `public.app-category.action-games` and the rest.
    pub fn category(&self) -> Option<&str> {
        self.info_string("LSApplicationCategoryType")
    }

    /// The development region's `.lproj`, which is where a nib lives.
    fn localizations(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(region) = self.info_string("CFBundleDevelopmentRegion") {
            out.push(self.resources.join(format!("{region}.lproj")));
        }
        // `Base.lproj` is where a modern Xcode puts an unlocalized nib, and
        // `English.lproj` is where an old one did. Both, in that order, because
        // a bundle may have either and a few have both.
        out.push(self.resources.join("Base.lproj"));
        out.push(self.resources.join("English.lproj"));
        out
    }

    /// The `.lproj` directories a localized string is looked for in, most
    /// preferred first: this process's language, then the development region
    /// and the names an English bundle may use for it.
    fn string_localizations(&self) -> Vec<PathBuf> {
        let language = crate::locale::language();
        let mut names = vec![language.clone()];
        if let Some(region) = self.info_string("CFBundleDevelopmentRegion") {
            names.push(region.to_string());
        }
        names.extend(["en", "English", "Base"].map(str::to_owned));
        let mut seen = Vec::new();
        for name in names {
            let path = self.resources.join(format!("{name}.lproj"));
            if !seen.contains(&path) {
                seen.push(path);
            }
        }
        seen
    }

    /// `-preferredLocalizations`: the `.lproj` names strings are read from,
    /// most preferred first, restricted to the ones that exist.
    pub fn preferred_localizations(&self) -> Vec<String> {
        self.string_localizations()
            .into_iter()
            .filter(|path| path.is_dir())
            .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
            .collect()
    }

    /// `-pathsForResourcesOfType:inDirectory:`: every entry with that extension
    /// (any, for an empty one) in `Resources/directory`, then in the preferred
    /// localization's copy of it.
    pub fn resources_of_type(&self, extension: &str, directory: &str) -> Vec<PathBuf> {
        let roots = std::iter::once(self.resources.clone()).chain(
            self.preferred_localizations()
                .into_iter()
                .take(1)
                .map(|name| self.resources.join(format!("{name}.lproj"))),
        );
        roots
            .flat_map(|root| entries_of_type(&root.join(directory), extension))
            .collect()
    }

    /// `-localizedStringForKey:value:table:`: the key's entry in the first
    /// table of that name that has one, else `value`, else the key itself -
    /// Apple's fallback order.
    pub fn localized_string(&self, key: &str, value: Option<&str>, table: Option<&str>) -> String {
        use std::collections::HashMap;
        use std::sync::Mutex;
        type Tables = HashMap<PathBuf, Option<HashMap<String, String>>>;
        static TABLES: Mutex<Option<Tables>> = Mutex::new(None);

        let table = table.filter(|t| !t.is_empty()).unwrap_or("Localizable");
        let mut cache = TABLES.lock().unwrap_or_else(|e| e.into_inner());
        let cache = cache.get_or_insert_with(HashMap::new);
        for directory in self.string_localizations() {
            let path = directory.join(format!("{table}.strings"));
            let parsed = cache.entry(path.clone()).or_insert_with(|| {
                std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| crate::strings::parse(&bytes))
            });
            if let Some(found) = parsed.as_ref().and_then(|t| t.get(key)) {
                return found.clone();
            }
        }
        value.filter(|v| !v.is_empty()).unwrap_or(key).to_string()
    }

    /// Localization identifiers declared by the bundle and present as
    /// `Resources/*.lproj` directories.
    pub fn localization_names(&self) -> Vec<String> {
        let mut names = self
            .info
            .get("CFBundleLocalizations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if let Ok(entries) = std::fs::read_dir(&self.resources) {
            names.extend(entries.filter_map(|entry| {
                let entry = entry.ok()?;
                entry.file_type().ok()?.is_dir().then_some(())?;
                let name = entry.file_name();
                let name = name.to_str()?;
                name.strip_suffix(".lproj").map(str::to_owned)
            }));
        }
        names.sort();
        names.dedup();
        names
    }

    /// `-pathForResource:ofType:`, searching `Resources` and then each
    /// localization.
    ///
    /// `extension` may be empty, which means the name already has one - the
    /// spelling `[NSBundle pathForResource:@"MainMenu" ofType:@"nib"]` and
    /// `...ofType:@""` both reach.
    pub fn resource(&self, name: &str, extension: &str) -> Option<PathBuf> {
        self.resource_in(name, extension, "")
    }

    /// `-pathForResource:ofType:inDirectory:`, with `directory` relative to
    /// each resource root.
    pub fn resource_in(&self, name: &str, extension: &str, directory: &str) -> Option<PathBuf> {
        let leaf = match extension.is_empty() {
            true => name.to_string(),
            false => format!("{name}.{extension}"),
        };
        std::iter::once(self.resources.clone())
            .chain(self.localizations())
            .map(|root| root.join(directory).join(&leaf))
            .find(|p| p.exists())
    }

    /// The main nib, if the `Info.plist` names one and it is there.
    ///
    /// `.nib` and `.storyboardc` are both directories in a modern bundle and a
    /// file in an older one; the name is taken from the plist either way.
    pub fn main_nib(&self) -> Option<PathBuf> {
        let name = self.info_string("NSMainNibFile")?;
        self.resource(name, "nib")
            .or_else(|| self.resource(name, "storyboardc"))
            .or_else(|| self.resource(name, ""))
    }

    /// The `.icns` the `Info.plist` names, if it is there.
    ///
    /// `CFBundleIconFile` is written both ways - with the extension and without
    /// it - and Apple accepts either, so both spellings are tried. A bundle
    /// whose icon is an asset catalog names it with `CFBundleIconName` and has
    /// no file to find, which is a `None` here rather than a diagnostic.
    pub fn icon(&self) -> Option<PathBuf> {
        let name = self.info_string("CFBundleIconFile")?;
        self.resource(name, "icns")
            .or_else(|| self.resource(name, ""))
    }

    /// `NSPrincipalClass`, which `NSApplicationMain` instantiates instead of
    /// `NSApplication`.
    pub fn principal_class(&self) -> Option<&str> {
        self.info_string("NSPrincipalClass")
    }
}

/// `CFBundleRef` is `NSBundle *`, so the CoreFoundation spellings live beside
/// the class rather than in `corefoundation` - which is underneath this crate
/// and has no bundle to answer with.
///
/// `CFBundleCopyResourceURL` is here for what it does when the resource is
/// *absent*, which is the answer a bundle usually has to give: A Hat in Time
/// asks for a `RadioEffectUnit` its own `Resources` does not contain, and a null
/// lets it skip the whole Carbon component path instead of being refused at the
/// import.
pub mod cf {
    // The C spellings are the interface; renaming them would only hide it.
    #![allow(non_snake_case)]

    use corefoundation::base::CFTypeRef;
    use objc::object::Id;

    /// `CFBundleRef CFBundleGetMainBundle(void)`
    ///
    /// A `Get`, so the caller does not own it - which is what the singleton
    /// already is: retained forever, and never handed out with a reference.
    unsafe extern "C" fn CFBundleGetMainBundle() -> Id {
        super::class::main_object()
    }

    /// `CFURLRef CFBundleCopyResourceURL(CFBundleRef, CFStringRef name,
    /// CFStringRef type, CFStringRef subDirName)`
    ///
    unsafe extern "C" fn CFBundleCopyResourceURL(
        bundle: Id,
        name: Id,
        kind: Id,
        subdirectory: Id,
    ) -> Id {
        // SAFETY: each is an `NSString` or nil.
        let (name, kind, subdirectory) = unsafe {
            (
                crate::string::bytes(name),
                crate::string::bytes(kind),
                crate::string::bytes(subdirectory),
            )
        };
        let (Ok(name), Ok(kind), Ok(subdirectory)) = (
            std::str::from_utf8(&name),
            std::str::from_utf8(&kind),
            std::str::from_utf8(&subdirectory),
        ) else {
            return std::ptr::null_mut();
        };
        let Some(path) = layout(bundle).and_then(|b| b.resource_in(name, kind, subdirectory))
        else {
            return std::ptr::null_mut();
        };
        file_url(&path, false)
    }

    /// `CFURLRef CFBundleCopyExecutableURL(CFBundleRef)`
    unsafe extern "C" fn CFBundleCopyExecutableURL(bundle: Id) -> Id {
        match layout(bundle) {
            Some(bundle) => file_url(&bundle.executable, false),
            None => std::ptr::null_mut(),
        }
    }

    /// `CFURLRef CFBundleCopyBundleURL(CFBundleRef)`
    unsafe extern "C" fn CFBundleCopyBundleURL(bundle: Id) -> Id {
        match layout(bundle) {
            Some(bundle) => file_url(&bundle.root, true),
            None => std::ptr::null_mut(),
        }
    }

    /// `CFURLRef CFBundleCopyResourcesDirectoryURL(CFBundleRef)`
    ///
    /// The directory rather than a file in it, so the trailing slash a caller
    /// appends a name onto is there.
    unsafe extern "C" fn CFBundleCopyResourcesDirectoryURL(bundle: Id) -> Id {
        match layout(bundle) {
            Some(bundle) => file_url(&bundle.resources, true),
            None => std::ptr::null_mut(),
        }
    }

    /// `CFURLRef CFBundleCopyPrivateFrameworksURL(CFBundleRef)`
    ///
    /// Answered whether or not the directory exists, which is Apple's behaviour:
    /// the URL is where a private framework *would* be, and a caller that finds
    /// nothing there is entitled to that answer rather than to a null.
    unsafe extern "C" fn CFBundleCopyPrivateFrameworksURL(bundle: Id) -> Id {
        match layout(bundle) {
            Some(bundle) => file_url(&bundle.frameworks, true),
            None => std::ptr::null_mut(),
        }
    }

    /// `CFURLRef CFBundleCopyBuiltInPlugInsURL(CFBundleRef)`
    ///
    /// How a Qt application finds its own plugins: `QCoreApplication`'s default
    /// library path is this URL, and a null here leaves it with an empty plugin
    /// search path - so `libqcocoa` is never found, there is no platform plugin,
    /// and Qt aborts before the first window. It is called during
    /// `libraryPathsLocked`, which is the first thing `QApplication`'s
    /// constructor needs.
    ///
    /// Answered whether or not the directory exists, for the reason
    /// [`CFBundleCopyPrivateFrameworksURL`] gives.
    unsafe extern "C" fn CFBundleCopyBuiltInPlugInsURL(bundle: Id) -> Id {
        match layout(bundle) {
            Some(bundle) => file_url(&bundle.plugins, true),
            None => std::ptr::null_mut(),
        }
    }

    /// `CFArrayRef CFBundleCopyBundleLocalizations(CFBundleRef)`.
    unsafe extern "C" fn CFBundleCopyBundleLocalizations(bundle: Id) -> Id {
        let Some(bundle) = layout(bundle) else {
            return std::ptr::null_mut();
        };
        let strings = bundle
            .localization_names()
            .iter()
            .map(|name| crate::string::from_str(name) as CFTypeRef)
            .collect::<Vec<_>>();
        let array = corefoundation::collections::new_array(
            &strings,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        for string in strings {
            // The array retained each newly-created string and now owns it.
            // SAFETY: each is the live object made immediately above.
            unsafe { corefoundation::api::CFRelease(string) };
        }
        array as Id
    }

    /// `CFArrayRef CFBundleCopyPreferredLocalizationsFromArray(CFArrayRef)`.
    unsafe extern "C" fn CFBundleCopyPreferredLocalizationsFromArray(localizations: Id) -> Id {
        // SAFETY: a CFArray or null.
        let Some(array) =
            (unsafe { corefoundation::collections::array(localizations as CFTypeRef) })
        else {
            return std::ptr::null_mut();
        };
        let language = crate::locale::language();
        let preferred = array.values.iter().copied().find(|value| {
            // SAFETY: localization arrays contain CFStrings.
            let bytes = unsafe { crate::string::bytes(*value as Id) };
            std::str::from_utf8(&bytes).is_ok_and(|name| {
                name.eq_ignore_ascii_case(&language)
                    || name
                        .split(['-', '_'])
                        .next()
                        .is_some_and(|base| base.eq_ignore_ascii_case(&language))
            })
        });
        let values = preferred
            .or_else(|| array.values.first().copied())
            .into_iter()
            .collect::<Vec<_>>();
        corefoundation::collections::new_array(
            &values,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id
    }

    /// `CFStringRef CFBundleGetIdentifier(CFBundleRef)`
    ///
    /// A `Get`, so the string is not the caller's to release - which is why it is
    /// interned rather than made fresh each time.
    unsafe extern "C" fn CFBundleGetIdentifier(bundle: Id) -> Id {
        let Some(identifier) = layout(bundle).and_then(|b| b.identifier()) else {
            return std::ptr::null_mut();
        };
        crate::notification::name_object(identifier) as Id
    }

    /// A bundle's `Info.plist` as a dictionary object, made once per bundle.
    fn info_dictionary(bundle: Id) -> Id {
        static MADE: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
        let Some(layout) = layout(bundle) else {
            return std::ptr::null_mut();
        };
        let key = layout as *const super::Layout as usize;
        let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&(_, object)) = made.iter().find(|(k, _)| *k == key) {
            return object as Id;
        }
        let object = crate::objectify::object(&layout.info, "CFBundleGetInfoDictionary");
        // A Get result lives with the bundle rather than with an autorelease
        // pool, so keep the objectified plist for the process.
        objc::arc::retain(object);
        made.push((key, object as usize));
        object
    }

    unsafe extern "C" fn CFBundleGetInfoDictionary(bundle: Id) -> Id {
        info_dictionary(bundle)
    }

    unsafe extern "C" fn CFBundleGetValueForInfoDictionaryKey(bundle: Id, key: Id) -> Id {
        let dictionary = info_dictionary(bundle);
        if dictionary.is_null() || key.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: the cached object is a dictionary and `key` is the caller's
        // live CFString.
        unsafe {
            corefoundation::api::CFDictionaryGetValue(dictionary as CFTypeRef, key as CFTypeRef)
                as Id
        }
    }

    /// `void CFBundleGetPackageInfo(CFBundleRef, UInt32 *type, UInt32 *creator)`
    ///
    /// The two four-character codes from `Info.plist`, or `????` for one it
    /// does not give - which is what Apple's answers for a missing key.
    unsafe extern "C" fn CFBundleGetPackageInfo(bundle: Id, kind: *mut u32, creator: *mut u32) {
        let code = |key: &str| {
            layout(bundle)
                .and_then(|b| b.info_string(key))
                .and_then(|text| <[u8; 4]>::try_from(text.as_bytes()).ok())
                .map_or(u32::from_be_bytes(*b"????"), u32::from_be_bytes)
        };
        if !kind.is_null() {
            // SAFETY: writable, per the caller.
            unsafe { kind.write(code("CFBundlePackageType")) };
        }
        if !creator.is_null() {
            // SAFETY: writable, per the caller.
            unsafe { creator.write(code("CFBundleSignature")) };
        }
    }

    /// `CFArrayRef CFBundleCopyExecutableArchitectures(CFBundleRef)`
    ///
    /// The CPU types the executable has slices for, read from its header.
    unsafe extern "C" fn CFBundleCopyExecutableArchitectures(bundle: Id) -> Id {
        let Some(layout) = layout(bundle) else {
            return std::ptr::null_mut();
        };
        let Some(types) = super::cpu_types(&layout.executable) else {
            return std::ptr::null_mut();
        };
        let numbers: Vec<CFTypeRef> = types
            .into_iter()
            .map(|t| {
                corefoundation::collections::new_number_of_type(
                    corefoundation::collections::Number::Integer(i64::from(t)),
                    // kCFNumberSInt32Type
                    3,
                )
            })
            .collect();
        let array = corefoundation::collections::new_array(
            &numbers,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        for number in numbers {
            // SAFETY: fresh objects this call owns, now held by the array.
            unsafe { corefoundation::api::CFRelease(number) };
        }
        array as Id
    }

    /// `CFArrayRef CFBundleCopyExecutableArchitecturesForURL(CFURLRef)`
    unsafe extern "C" fn CFBundleCopyExecutableArchitecturesForURL(url: Id) -> Id {
        // SAFETY: a `CFURL` or null.
        let bundle = unsafe { CFBundleCreate(std::ptr::null(), url) };
        if bundle.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: a bundle this call owns a reference to.
        let array = unsafe { CFBundleCopyExecutableArchitectures(bundle) };
        objc::arc::release(bundle);
        array
    }

    /// `UInt32 CFBundleGetVersionNumber(CFBundleRef)`
    ///
    /// Apple's is a packed BCD `vvvv.rr.ss` word. Only the major version is
    /// filled in: a caller compares the whole word against a constant of its
    /// own, and a fabricated revision would make that comparison say something
    /// the `Info.plist` never did.
    unsafe extern "C" fn CFBundleGetVersionNumber(bundle: Id) -> u32 {
        let Some(version) = layout(bundle).and_then(|b| b.version()) else {
            return 0;
        };
        let Some(major) = version
            .split('.')
            .next()
            .and_then(|major| major.parse::<u32>().ok())
        else {
            return 0;
        };
        // The major version occupies the top four BCD digits.
        let mut packed = 0u32;
        for (shift, digit) in format!("{major:0>4}").bytes().rev().enumerate().take(4) {
            packed |= u32::from(digit - b'0') << (16 + 4 * shift);
        }
        packed
    }

    /// `CFBundleRef CFBundleCreate(CFAllocatorRef, CFURLRef bundleURL)`
    ///
    /// The main bundle when the URL names it, and the bundle of a framework this
    /// runtime provides - which is how a program finds `free` or `malloc` by
    /// loading `System.framework`. Anything else is a bundle this runtime has
    /// not loaded, and a null says so where an invented one would answer
    /// questions about the wrong directory.
    unsafe extern "C" fn CFBundleCreate(_allocator: *const std::ffi::c_void, url: Id) -> Id {
        // SAFETY: a `CFURL` or null.
        let Some(url) = (unsafe { corefoundation::url::as_url(url as CFTypeRef) }) else {
            return std::ptr::null_mut();
        };
        // A bundle is named by a file URL and by nothing else: a `https:` URL
        // has no directory for this to be about.
        let Some(path) = url.file_path() else {
            return std::ptr::null_mut();
        };
        // The guest named it, so it is compared at the spelling this crate
        // keeps rather than the one the guest used.
        let Some(class) = objc::class::lookup(c"NSBundle") else {
            return std::ptr::null_mut();
        };
        let object = super::class::at_path(objc::class::ptr(class) as Id, &path);
        // A `Create`, so the caller owns a reference to the interned object.
        if !object.is_null() {
            objc::arc::retain(object);
        }
        object
    }

    /// The loader's handle for a bundle's executable, loading it if it is not
    /// yet. The main bundle's is the program itself.
    pub(super) fn executable_handle(bundle: Id) -> Option<u64> {
        let layout = layout(bundle)?;
        if super::main().is_some_and(|main| std::ptr::eq(main, layout)) {
            return darwin::images::open(None);
        }
        let executable = super::spelled(&layout.executable)?;
        darwin::images::open(Some(executable.to_str()?))
    }

    /// `Boolean CFBundleLoadExecutable(CFBundleRef)`
    unsafe extern "C" fn CFBundleLoadExecutable(bundle: Id) -> u8 {
        u8::from(executable_handle(bundle).is_some())
    }

    /// `void *CFBundleGetFunctionPointerForName(CFBundleRef, CFStringRef)`
    ///
    /// Loads the executable first, as Apple's does, and looks the name up in
    /// it alone.
    unsafe extern "C" fn CFBundleGetFunctionPointerForName(
        bundle: Id,
        name: Id,
    ) -> *mut std::ffi::c_void {
        if name.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: a CFString, as the caller passed it.
        let name = unsafe { crate::string::bytes(name) };
        let (Ok(name), Some(handle)) = (std::str::from_utf8(&name), executable_handle(bundle))
        else {
            return std::ptr::null_mut();
        };
        darwin::images::symbol(handle, name).map_or(std::ptr::null_mut(), |address| {
            address as usize as *mut std::ffi::c_void
        })
    }

    /// `CFBundleRef CFBundleGetBundleWithIdentifier(CFStringRef)`
    ///
    /// Searches the bundles this process already has - the main one and every
    /// one created since - and then the Apple framework of that identifier, if
    /// this runtime provides it. That last is how a program finds OpenGL to
    /// look up an extension in: `com.apple.opengl`, then
    /// `CFBundleGetFunctionPointerForName`.
    ///
    /// A `Get`, so not retained: every bundle here lives for the process.
    unsafe extern "C" fn CFBundleGetBundleWithIdentifier(identifier: Id) -> Id {
        if identifier.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: a CFString, as the caller passed it.
        let identifier = unsafe { crate::string::bytes(identifier) };
        let Ok(identifier) = std::str::from_utf8(&identifier) else {
            return std::ptr::null_mut();
        };
        if super::main().is_some_and(|main| main.identifier() == Some(identifier)) {
            return super::class::main_object();
        }
        let made = super::class::made_with_identifier(identifier);
        if !made.is_null() {
            return made;
        }
        match super::apple_framework(identifier) {
            Some(guest) => super::class::provided(&guest),
            None => std::ptr::null_mut(),
        }
    }

    /// `void *CFBundleGetDataPointerForName(CFBundleRef, CFStringRef)`
    ///
    /// The address of an exported variable - a constant string, a table - in
    /// the bundle's executable alone, loading it first. `dlsym` answers data
    /// and functions alike, so this is the function lookup under another name.
    unsafe extern "C" fn CFBundleGetDataPointerForName(
        bundle: Id,
        name: Id,
    ) -> *mut std::ffi::c_void {
        // SAFETY: as the caller passed them.
        unsafe { CFBundleGetFunctionPointerForName(bundle, name) }
    }

    /// The layout a `CFBundleRef` answers for. A null bundle has always been
    /// answered as the main one here, and still is.
    fn layout(bundle: Id) -> Option<&'static super::Layout> {
        match bundle.is_null() {
            true => super::main(),
            // SAFETY: every `CFBundleRef` this runtime hands out is an
            // `NSBundle` its class module made.
            false => unsafe { super::class::layout_of(bundle) },
        }
    }

    /// A path as a `CFURL` the caller owns, spelled the guest's way.
    fn file_url(path: &std::path::Path, directory: bool) -> Id {
        let Some(path) = super::spelled(path) else {
            return std::ptr::null_mut();
        };
        match path.to_str() {
            Some(path) => corefoundation::url::new_file_url(path, directory) as Id,
            None => std::ptr::null_mut(),
        }
    }

    /// `CFStringRef CFBundleCopyLocalizedString(CFBundleRef, CFStringRef key,
    /// CFStringRef value, CFStringRef tableName)`
    unsafe extern "C" fn CFBundleCopyLocalizedString(
        bundle: Id,
        key: Id,
        value: Id,
        table: Id,
    ) -> Id {
        // SAFETY: a bundle and strings or nil, as the caller passed them.
        unsafe { super::class::copy_localized_string(bundle, key, value, table) }
    }

    exports::exports! {
        functions {
            "CFBundleCopyLocalizedString" => CFBundleCopyLocalizedString,
            "CFBundleGetMainBundle" => CFBundleGetMainBundle,
            "CFBundleCopyResourceURL" => CFBundleCopyResourceURL,
            "CFBundleCopyExecutableURL" => CFBundleCopyExecutableURL,
            "CFBundleCopyBundleURL" => CFBundleCopyBundleURL,
            "CFBundleCopyResourcesDirectoryURL" => CFBundleCopyResourcesDirectoryURL,
            "CFBundleCopyPrivateFrameworksURL" => CFBundleCopyPrivateFrameworksURL,
            "CFBundleCopyBuiltInPlugInsURL" => CFBundleCopyBuiltInPlugInsURL,
            "CFBundleCopyBundleLocalizations" => CFBundleCopyBundleLocalizations,
            "CFBundleCopyPreferredLocalizationsFromArray" => CFBundleCopyPreferredLocalizationsFromArray,
            "CFBundleGetIdentifier" => CFBundleGetIdentifier,
            "CFBundleGetInfoDictionary" => CFBundleGetInfoDictionary,
            "CFBundleGetValueForInfoDictionaryKey" => CFBundleGetValueForInfoDictionaryKey,
            "CFBundleGetVersionNumber" => CFBundleGetVersionNumber,
            "CFBundleGetPackageInfo" => CFBundleGetPackageInfo,
            "CFBundleCopyExecutableArchitectures" => CFBundleCopyExecutableArchitectures,
            "CFBundleCopyExecutableArchitecturesForURL" => CFBundleCopyExecutableArchitecturesForURL,
            "CFBundleCreate" => CFBundleCreate,
            "CFBundleLoadExecutable" => CFBundleLoadExecutable,
            "CFBundleGetFunctionPointerForName" => CFBundleGetFunctionPointerForName,
            "CFBundleGetDataPointerForName" => CFBundleGetDataPointerForName,
            "CFBundleGetBundleWithIdentifier" => CFBundleGetBundleWithIdentifier,
        }
    }
}

/// The bundle the program is, once something has said which.
static MAIN: RwLock<Option<&'static Layout>> = RwLock::new(None);

/// Say which `.app` the guest is.
///
/// Called by the runtime once the program is loaded, with the *guest's*
/// executable path. Nothing here reads the host process's own path: that is the
/// whole difference between this and the host's `NSBundle`, and the reason no
/// re-exec is needed.
///
/// Returns whether a bundle was found, so a caller can report a loose binary
/// rather than leave `+mainBundle` quietly nil.
pub fn set_main(executable: &Path) -> bool {
    let Some(layout) = Layout::enclosing(executable) else {
        return false;
    };
    *MAIN.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::leak(Box::new(layout)));
    true
}

/// The entries of `directory` with `extension` (any, for an empty one), sorted.
fn entries_of_type(directory: &Path, extension: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            extension.is_empty()
                || path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(extension))
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}

/// The main bundle, or `None` for a program that is not in one.
/// The bundle an `NSBundle` object stands for; null is the main one.
///
/// # Safety
///
/// `bundle` must be null or an `NSBundle`.
pub unsafe fn layout_of(bundle: objc::object::Id) -> Option<&'static Layout> {
    if bundle.is_null() {
        return main();
    }
    // SAFETY: an `NSBundle`, every one of which this crate made.
    unsafe { class::layout_of(bundle) }
}

pub fn main() -> Option<&'static Layout> {
    *MAIN.read().unwrap_or_else(|e| e.into_inner())
}

/// Resolve a resource in the bundle object the guest supplied.
///
/// # Safety
///
/// `bundle` must be an `NSBundle` this module made.
pub unsafe fn resource_for_object(
    bundle: objc::object::Id,
    name: &str,
    extension: &str,
) -> Option<PathBuf> {
    unsafe { class::layout_of(bundle) }?.resource(name, extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A framework the loader says exists is a bundle whose executable is
    /// named after it, at the top level as a framework's is; one it does not
    /// know is no bundle at all.
    #[test]
    fn a_provided_framework_is_a_bundle_with_its_executable_at_the_top() {
        darwin::images::install(darwin::images::Loader {
            virtual_directory: |path| {
                path.trim_end_matches('/') == "/System/Library/Frameworks/RosemaryProbe.framework"
            },
            open: |_| None,
            symbol: |_, _| None,
            image_path: |_| None,
        });
        let layout =
            Layout::provided_framework("/System/Library/Frameworks/RosemaryProbe.framework/")
                .expect("a provided framework");
        assert_eq!(
            spelled(&layout.executable).as_deref(),
            Some(Path::new(
                "/System/Library/Frameworks/RosemaryProbe.framework/RosemaryProbe"
            ))
        );
        assert_eq!(
            layout.info_string("CFBundleExecutable"),
            Some("RosemaryProbe")
        );
        assert_eq!(layout.info_string("CFBundlePackageType"), Some("FMWK"));
        assert_eq!(layout.identifier(), None);
        assert!(
            Layout::provided_framework("/System/Library/Frameworks/RosemaryAbsent.framework")
                .is_none()
        );
    }

    /// An Apple framework is found by the identifier a Mac gives it, in any
    /// case, and only one in the table has one.
    #[test]
    fn an_apple_identifier_names_its_framework() {
        assert_eq!(apple_identifier("OpenGL"), Some("com.apple.opengl"));
        assert_eq!(
            apple_framework("com.apple.OpenGL").as_deref(),
            Some("/System/Library/Frameworks/OpenGL.framework")
        );
        assert_eq!(apple_identifier("RosemaryProbe"), None);
        assert_eq!(apple_framework("com.example.thing"), None);
    }

    /// Build a bundle-shaped directory, since the claim is about shape.
    fn scratch(name: &str, info: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rosemary-bundle-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        let app = root.join("Thing.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources/English.lproj")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), info).unwrap();
        std::fs::write(app.join("Contents/MacOS/Thing"), b"").unwrap();
        std::fs::write(app.join("Contents/Resources/icon.icns"), b"").unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources/textures/ui")).unwrap();
        std::fs::write(app.join("Contents/Resources/textures/ui/button.png"), b"").unwrap();
        std::fs::write(
            app.join("Contents/Resources/English.lproj/MainMenu.nib"),
            b"",
        )
        .unwrap();
        app.join("Contents/MacOS/Thing")
    }

    const INFO: &str = r#"<?xml version="1.0"?><plist version="1.0"><dict>
        <key>CFBundleExecutable</key><string>Thing</string>
        <key>CFBundleIdentifier</key><string>com.example.thing</string>
        <key>CFBundleDevelopmentRegion</key><string>English</string>
        <key>CFBundleIconFile</key><string>icon.icns</string>
        <key>NSMainNibFile</key><string>MainMenu</string>
        <key>NSPrincipalClass</key><string>ThingApplication</string>
        </dict></plist>"#;

    #[test]
    fn a_bundle_is_found_by_its_file_url_as_by_its_path() {
        crate::bootstrap();
        let exe = scratch("url", INFO);
        let app = exe.parent().unwrap().parent().unwrap().parent().unwrap();
        let guest = spelled(app).unwrap().to_string_lossy().into_owned();
        let ns_bundle =
            objc::class::ptr(objc::class::lookup(c"NSBundle").unwrap()) as objc::object::Id;
        let by_path = class::at_path(ns_bundle, &guest);
        assert!(!by_path.is_null());

        let url = corefoundation::url::new_file_url(&guest, true);
        // SAFETY: `+bundleWithURL:` takes one object.
        let by_url = unsafe {
            class::bundle_with_url(
                ns_bundle,
                objc::sel::get("bundleWithURL:"),
                url as objc::object::Id,
            )
        };
        assert_eq!(by_url, by_path);

        let web = corefoundation::url::new("https://example.com/Thing.app");
        let none = unsafe {
            class::bundle_with_url(
                ns_bundle,
                objc::sel::get("bundleWithURL:"),
                web as objc::object::Id,
            )
        };
        assert!(none.is_null());
        unsafe {
            corefoundation::api::CFRelease(url);
            corefoundation::api::CFRelease(web);
        }
    }

    #[test]
    fn a_bundle_is_recognised_by_shape_and_read_through() {
        let exe = scratch("shape", INFO);
        let layout = Layout::enclosing(&exe).expect("a bundle");
        assert_eq!(layout.identifier(), Some("com.example.thing"));
        assert_eq!(layout.principal_class(), Some("ThingApplication"));
        assert_eq!(layout.root.file_name().unwrap(), "Thing.app");
        // Found in `Resources`, and in the localization under it.
        assert!(layout.resource("icon", "icns").is_some());
        assert!(layout.main_nib().is_some(), "the nib is in English.lproj");
        assert!(layout.resource("nothing", "txt").is_none());
        assert_eq!(layout.localization_names(), ["English"]);
    }

    #[test]
    fn an_empty_executable_name_falls_back_to_the_bundle_name() {
        let exe = scratch(
            "empty-executable",
            r#"<?xml version="1.0"?><plist version="1.0"><dict>
            <key>CFBundleExecutable</key><string></string>
            </dict></plist>"#,
        );
        let root = exe.parent().unwrap().parent().unwrap().parent().unwrap();
        let layout = Layout::at(root).expect("a bundle");
        assert_eq!(layout.executable.file_name().unwrap(), "Thing");
    }

    #[test]
    fn a_resource_directory_is_relative_to_the_bundle_resources() {
        let exe = scratch("resource-directory", INFO);
        let layout = Layout::enclosing(&exe).expect("a bundle");
        assert_eq!(
            layout.resource_in("button.png", "", "textures/ui"),
            Some(layout.resources.join("textures/ui/button.png"))
        );
        assert!(layout.resource("button.png", "").is_none());
        assert!(
            layout
                .resource_in("button.png", "", "textures/missing")
                .is_none()
        );
    }

    /// `CFBundleIconFile` is written with the extension as often as without,
    /// and a bundle whose icon is named the other way still has to be found.
    #[test]
    fn the_icon_is_found_whichever_way_the_plist_spells_it() {
        let with_extension = scratch("icon-spelt-out", INFO);
        let layout = Layout::enclosing(&with_extension).expect("a bundle");
        assert_eq!(layout.icon(), Some(layout.resources.join("icon.icns")));

        let bare = INFO.replace("<string>icon.icns</string>", "<string>icon</string>");
        let layout = Layout::enclosing(&scratch("icon-bare", &bare)).expect("a bundle");
        assert_eq!(layout.icon(), Some(layout.resources.join("icon.icns")));

        let none = INFO.replace("<key>CFBundleIconFile</key><string>icon.icns</string>", "");
        let layout = Layout::enclosing(&scratch("icon-absent", &none)).expect("a bundle");
        assert_eq!(layout.icon(), None);
    }

    /// The shape is the whole of the test: the same files with the wrong
    /// directory names are not a bundle.
    #[test]
    fn a_directory_that_is_not_bundle_shaped_is_not_a_bundle() {
        let exe = scratch("wrong", INFO);
        let loose = exe.parent().unwrap().parent().unwrap().join("Thing");
        std::fs::write(&loose, b"").unwrap();
        assert!(Layout::enclosing(&loose).is_none(), "Contents/Thing");
        assert!(Layout::enclosing(Path::new("/bin/ls")).is_none());
    }

    /// An `Info.plist` that cannot be read is a bundle this runtime declines to
    /// recognise, rather than one with an empty identity.
    #[test]
    fn an_unreadable_info_plist_is_not_a_bundle() {
        let exe = scratch("broken", "not a plist at all");
        assert!(Layout::enclosing(&exe).is_none());
    }
}

/// `NSBundle`, over the layout above.
///
/// One instance, the main one: nothing in this tree has yet loaded a second
/// bundle, and `+bundleWithPath:` would need a table keyed on the path. A guest
/// that asks for one gets the runtime's unrecognised-selector report with the
/// name in it, which is the work queue.
pub(crate) mod class {
    use std::ffi::c_char;

    use objc::object::Id;
    use objc::sel::Sel;

    use crate::define::{class_methods, methods};

    /// An `NSBundle`: the `isa`, and the layout it answers for.
    ///
    /// The main bundle's is null rather than a copy of `super::main()`, so that
    /// a bundle set up after the object was made still answers through it.
    #[repr(C)]
    struct Body {
        isa: *const objc::class::Class,
        layout: *const super::Layout,
    }

    /// The `NSBundle` singleton, created on the first `+mainBundle`.
    static MAIN_BUNDLE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

    unsafe extern "C" fn main_bundle(class: Id, _sel: Sel) -> Id {
        // SAFETY: `class` is `NSBundle`, which this crate registered.
        let class = match unsafe { objc::class::from_ptr(class as *const objc::class::Class) } {
            Some(class) => class,
            None => objc::fail!("+[NSBundle mainBundle] was sent to nil"),
        };
        of(class)
    }

    /// `+bundleForClass:`. The runtime does not record which image defined a
    /// class, so every class answers with the main bundle - Apple's answer for
    /// the guest's own classes, which are the ones a program asks about.
    unsafe extern "C" fn bundle_for_class(class: Id, sel: Sel, of: Id) -> Id {
        // SAFETY: a class object, or nil.
        let image = unsafe { objc::class::from_ptr(of as *const objc::class::Class) }
            .and_then(|of| of.own_methods().first().map(|m| m.implementation() as u64))
            .and_then(darwin::images::image_path);
        let main = super::main();
        let layout = image
            .and_then(|path| super::Layout::enclosing(std::path::Path::new(&path)))
            .filter(|layout| main.is_none_or(|main| main.root != layout.root));
        match layout {
            Some(layout) => interned(class, layout),
            // SAFETY: as `+mainBundle`, with the same receiver.
            None => unsafe { main_bundle(class, sel) },
        }
    }

    /// Every on-disk bundle object made, by resolved directory.
    static INTERNED: std::sync::Mutex<Vec<(std::path::PathBuf, usize)>> =
        std::sync::Mutex::new(Vec::new());

    /// Every provided-framework bundle object made, by guest path.
    static PROVIDED: std::sync::Mutex<Vec<(String, usize)>> = std::sync::Mutex::new(Vec::new());

    /// A bundle object already made whose identifier is `identifier`, in the
    /// order they were made, or nil.
    pub(crate) fn made_with_identifier(identifier: &str) -> Id {
        let interned = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
        let provided = PROVIDED.lock().unwrap_or_else(|e| e.into_inner());
        interned
            .iter()
            .map(|(_, object)| *object)
            .chain(provided.iter().map(|(_, object)| *object))
            .find(|&object| {
                // SAFETY: both tables hold only `NSBundle`s this module made.
                unsafe { layout_of(object as Id) }
                    .is_some_and(|layout| layout.identifier() == Some(identifier))
            })
            .map_or(std::ptr::null_mut(), |object| object as Id)
    }

    /// One `NSBundle` per bundle directory for the life of the process, as
    /// Apple's: a program compares bundles by identity.
    fn interned(class: Id, layout: super::Layout) -> Id {
        // Keyed by the resolved directory: the loader and a guest's path may
        // spell one bundle two ways.
        let key = layout
            .root
            .canonicalize()
            .unwrap_or_else(|_| layout.root.clone());
        let mut made = INTERNED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&(_, object)) = made.iter().find(|(root, _)| *root == key) {
            return object as Id;
        }
        // SAFETY: `class` is `NSBundle`, which this crate registered.
        let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) })
        else {
            return std::ptr::null_mut();
        };
        let object = objc::object::create_instance(class, 0);
        // SAFETY: a live instance whose second word is ours to fill in.
        unsafe { (*(object as *mut Body)).layout = Box::leak(Box::new(layout)) };
        made.push((key, object as usize));
        object
    }

    /// The one main-bundle object, which `+mainBundle` and
    /// `CFBundleGetMainBundle` both have to answer with.
    fn of(class: &'static objc::class::Class) -> Id {
        *MAIN_BUNDLE.get_or_init(|| {
            let object = objc::object::create_instance(class, 0);
            objc::arc::retain(object);
            object as usize
        }) as Id
    }

    /// `+bundleWithPath:`, autoreleased.
    unsafe extern "C" fn bundle_with_path(class: Id, _sel: Sel, path: Id) -> Id {
        // SAFETY: an `NSString`, or nil.
        let path = unsafe { crate::string::bytes(path) };
        let Ok(guest) = std::str::from_utf8(&path) else {
            return std::ptr::null_mut();
        };
        at_path(class, guest)
    }

    /// `+bundleWithURL:`: `+bundleWithPath:` for a `file:` URL, nil for any
    /// other.
    pub(crate) unsafe extern "C" fn bundle_with_url(class: Id, _sel: Sel, url: Id) -> Id {
        // SAFETY: an `NSURL`, or nil, which is no URL.
        let Some(path) = unsafe { corefoundation::url::as_url(url as _) }
            .and_then(|contents| contents.file_path())
        else {
            return std::ptr::null_mut();
        };
        at_path(class, &path)
    }

    /// The bundle at the guest path `guest`, unretained: the main one, one on
    /// disk, or one this runtime provides - nil for anything else. What
    /// `+bundleWithPath:` and `CFBundleCreate` both answer.
    pub(crate) fn at_path(class: Id, guest: &str) -> Id {
        // The guest named it, so it is opened at this crate's spelling.
        let path = super::opened(std::path::Path::new(guest));
        if super::main().is_some_and(|main| main.root == path) {
            return main_object();
        }
        let Some(layout) = super::Layout::at(&path) else {
            return provided(guest);
        };
        // Interned, and never freed: a `-dealloc` would leave every string this
        // runtime handed out from it pointing at nothing.
        interned(class, layout)
    }

    /// The bundle of a framework this runtime provides, named by its guest
    /// path, or nil for any other path - see [`super::Layout::provided_framework`].
    ///
    /// One object per framework for the life of the process, as Apple's
    /// `CFBundleCreate` also answers a URL it has seen with the same bundle, so
    /// the result is not the caller's to release.
    pub(crate) fn provided(guest: &str) -> Id {
        let guest = guest.trim_end_matches('/');
        let mut made = PROVIDED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&(_, object)) = made.iter().find(|(path, _)| path == guest) {
            return object as Id;
        }
        let Some(layout) = super::Layout::provided_framework(guest) else {
            return std::ptr::null_mut();
        };
        let Some(class) = objc::class::lookup(c"NSBundle") else {
            return std::ptr::null_mut();
        };
        let object = objc::object::create_instance(class, 0);
        // SAFETY: a live instance whose second word is ours to fill in.
        unsafe { (*(object as *mut Body)).layout = Box::leak(Box::new(layout)) };
        made.push((guest.to_string(), object as usize));
        object
    }

    /// The layout a bundle object answers for: its own, or the main one.
    ///
    /// # Safety
    ///
    /// `receiver` must be an `NSBundle` this module made.
    pub(super) unsafe fn layout_of(receiver: Id) -> Option<&'static super::Layout> {
        if receiver.is_null() {
            return None;
        }
        // SAFETY: every `NSBundle` is a `Body`.
        let layout = unsafe { (*(receiver as *const Body)).layout };
        match layout.is_null() {
            // SAFETY: the pointer was leaked by `bundle_with_path`.
            false => Some(unsafe { &*layout }),
            true => super::main(),
        }
    }

    /// The main bundle, for a caller that has no class object in hand.
    pub(crate) fn main_object() -> Id {
        match objc::class::lookup(c"NSBundle") {
            Some(class) => of(class),
            None => std::ptr::null_mut(),
        }
    }

    /// An `Info.plist` string as an autoreleased `NSString`, or nil.
    ///
    /// # Safety
    ///
    /// As [`layout_of`].
    unsafe fn info(receiver: Id, key: &str) -> Id {
        match unsafe { layout_of(receiver) }.and_then(|b| b.info_string(key)) {
            Some(text) => autoreleased(text),
            None => std::ptr::null_mut(),
        }
    }

    /// The text of an `NSString` argument, or `None` for nil.
    ///
    /// # Safety
    ///
    /// `string` is nil or a string object.
    unsafe fn text(string: Id) -> Option<String> {
        (!string.is_null())
            .then(|| String::from_utf8_lossy(&unsafe { crate::string::bytes(string) }).into_owned())
    }

    unsafe extern "C" fn localized_string_for_key(
        receiver: Id,
        _sel: Sel,
        key: Id,
        value: Id,
        table: Id,
    ) -> Id {
        // SAFETY: strings or nil.
        let (key, value, table) = unsafe { (text(key), text(value), text(table)) };
        let Some(key) = key else {
            // A nil key answers the value, or the empty string.
            return autoreleased(value.as_deref().unwrap_or(""));
        };
        // SAFETY: an `NSBundle`.
        match unsafe { layout_of(receiver) } {
            Some(layout) => {
                autoreleased(&layout.localized_string(&key, value.as_deref(), table.as_deref()))
            }
            None => autoreleased(value.as_deref().filter(|v| !v.is_empty()).unwrap_or(&key)),
        }
    }

    /// `CFBundleCopyLocalizedString`, the same lookup at +1.
    pub(crate) unsafe fn copy_localized_string(bundle: Id, key: Id, value: Id, table: Id) -> Id {
        // SAFETY: forwarded; the answer is autoreleased and this spelling owns it.
        objc::arc::retain(unsafe {
            localized_string_for_key(
                bundle,
                objc::sel::get("localizedStringForKey:value:table:"),
                key,
                value,
                table,
            )
        })
    }

    fn autoreleased(text: &str) -> Id {
        let string = crate::string::from_str(text);
        // SAFETY: a string this call just created.
        unsafe {
            corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id
        }
    }

    /// An autoreleased `NSArray` of strings.
    fn string_array<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> Id {
        let strings = items
            .into_iter()
            .map(|item| crate::string::from_str(item.as_ref()) as corefoundation::base::CFTypeRef)
            .collect::<Vec<_>>();
        let array = corefoundation::collections::new_array(
            &strings,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        for string in strings {
            // SAFETY: the array retained each one, and this reference is ours.
            unsafe { corefoundation::api::CFRelease(string) };
        }
        // SAFETY: a fresh array.
        unsafe { corefoundation::api::CFAutorelease(array) as Id }
    }

    unsafe extern "C" fn preferred_localizations(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: the receiver is an `NSBundle`.
        let names = unsafe { layout_of(receiver) }
            .map(|b| b.preferred_localizations())
            .unwrap_or_default();
        string_array(names)
    }

    unsafe extern "C" fn localizations(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: the receiver is an `NSBundle`.
        let names = unsafe { layout_of(receiver) }
            .map(|b| b.localization_names())
            .unwrap_or_default();
        string_array(names)
    }

    unsafe extern "C" fn paths_for_resources_of_type(
        receiver: Id,
        _sel: Sel,
        kind: Id,
        directory: Id,
    ) -> Id {
        // SAFETY: both are `NSString`s or nil.
        let (kind, directory) =
            unsafe { (crate::string::bytes(kind), crate::string::bytes(directory)) };
        let (Ok(kind), Ok(directory)) =
            (std::str::from_utf8(&kind), std::str::from_utf8(&directory))
        else {
            return string_array::<&str>([]);
        };
        // SAFETY: the receiver is an `NSBundle`.
        let paths = unsafe { layout_of(receiver) }
            .map(|b| b.resources_of_type(kind, directory))
            .unwrap_or_default();
        string_array(
            paths
                .iter()
                .filter_map(|p| super::spelled(p))
                .map(|p| p.to_string_lossy().into_owned()),
        )
    }

    /// `-load`: the executable, through the same loader `CFBundleLoadExecutable`
    /// uses, which registers its Objective-C classes as it binds them.
    unsafe extern "C" fn load(receiver: Id, _sel: Sel) -> u8 {
        u8::from(super::cf::executable_handle(receiver).is_some())
    }

    /// `-loadAndReturnError:`. The loader reports its own failure on stderr;
    /// the error is left nil rather than invented.
    unsafe extern "C" fn load_and_return_error(receiver: Id, sel: Sel, error: *mut Id) -> u8 {
        if !error.is_null() {
            // SAFETY: the caller passed a writable `NSError **`.
            unsafe { *error = std::ptr::null_mut() };
        }
        // SAFETY: the same receiver.
        unsafe { load(receiver, sel) }
    }

    /// `-isLoaded`. Asking loads nothing new for an executable already open,
    /// and every caller that asks is about to load it anyway.
    unsafe extern "C" fn is_loaded(receiver: Id, sel: Sel) -> u8 {
        // SAFETY: the same receiver.
        unsafe { load(receiver, sel) }
    }

    /// `-classNamed:`, loading the executable first as Apple's does.
    unsafe extern "C" fn class_named(receiver: Id, sel: Sel, name: Id) -> Id {
        // SAFETY: an `NSString`, or nil.
        let name = unsafe { crate::string::bytes(name) };
        // SAFETY: the same receiver.
        if name.is_empty() || unsafe { load(receiver, sel) } == 0 {
            return std::ptr::null_mut();
        }
        let Ok(name) = std::ffi::CString::new(name) else {
            return std::ptr::null_mut();
        };
        objc::class::lookup(&name).map_or(std::ptr::null_mut(), |c| objc::class::ptr(c) as Id)
    }

    /// `-principalClass`: `NSPrincipalClass`, after loading. Apple falls back
    /// to the executable's first class, which this loader does not record, so
    /// a bundle that names none answers nil.
    unsafe extern "C" fn principal_class(receiver: Id, sel: Sel) -> Id {
        // SAFETY: the receiver is an `NSBundle`.
        let Some(name) = unsafe { layout_of(receiver) }.and_then(|b| b.principal_class()) else {
            return std::ptr::null_mut();
        };
        let name = crate::string::from_str(name);
        // SAFETY: the same receiver, and a string this call owns.
        let class = unsafe { class_named(receiver, sel, name) };
        objc::arc::release(name);
        class
    }

    /// `+pathsForResourcesOfType:inDirectory:`, where the directory is a
    /// bundle's path rather than a subdirectory of one.
    unsafe extern "C" fn class_paths_for_resources_of_type(
        _class: Id,
        _sel: Sel,
        kind: Id,
        bundle_path: Id,
    ) -> Id {
        // SAFETY: both are `NSString`s or nil.
        let (kind, guest) = unsafe {
            (
                crate::string::bytes(kind),
                crate::string::bytes(bundle_path),
            )
        };
        let (Ok(kind), Ok(guest)) = (std::str::from_utf8(&kind), std::str::from_utf8(&guest))
        else {
            return string_array::<&str>([]);
        };
        // A directory that is not a bundle - `-builtInPlugInsPath` is the usual
        // one - is searched as it stands, as CFBundle does.
        let host = super::opened(std::path::Path::new(guest));
        let paths = match super::Layout::at(&host) {
            Some(bundle) => bundle.resources_of_type(kind, ""),
            None => super::entries_of_type(&host, kind),
        };
        string_array(
            paths
                .iter()
                .filter_map(|p| super::spelled(p))
                .map(|p| p.to_string_lossy().into_owned()),
        )
    }

    fn path(p: Option<std::path::PathBuf>) -> Id {
        match p.as_deref().and_then(super::spelled) {
            Some(p) => autoreleased(&p.to_string_lossy()),
            None => std::ptr::null_mut(),
        }
    }

    unsafe extern "C" fn bundle_path(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: the receiver is an `NSBundle`; so for each accessor below.
        path(unsafe { layout_of(receiver) }.map(|b| b.root.clone()))
    }

    /// The `file:` URL forms of the two paths above, which is what a program
    /// that hands the bundle to `NSWorkspace` or `NSTask` asks for.
    fn url(p: Option<std::path::PathBuf>) -> Id {
        let Some(p) = p.as_deref().and_then(super::spelled) else {
            return std::ptr::null_mut();
        };
        // SAFETY: a URL this call just made.
        unsafe {
            corefoundation::api::CFAutorelease(corefoundation::url::new_file_url(
                &p.to_string_lossy(),
                true,
            )) as Id
        }
    }

    fn resource_file_url(p: Option<std::path::PathBuf>) -> Id {
        let Some(p) = p else {
            return std::ptr::null_mut();
        };
        // Asked of the host path, and before the spelling changes: only that
        // one names a directory this process can look at.
        let directory = p.is_dir();
        let Some(p) = super::spelled(&p) else {
            return std::ptr::null_mut();
        };
        // SAFETY: a URL this call just made.
        unsafe {
            corefoundation::api::CFAutorelease(corefoundation::url::new_file_url(
                &p.to_string_lossy(),
                directory,
            )) as Id
        }
    }

    unsafe extern "C" fn bundle_url(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: the receiver is an `NSBundle`.
        url(unsafe { layout_of(receiver) }.map(|b| b.root.clone()))
    }

    unsafe extern "C" fn resource_url(receiver: Id, _sel: Sel) -> Id {
        url(unsafe { layout_of(receiver) }.map(|b| b.resources.clone()))
    }

    unsafe extern "C" fn executable_url(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: as above. Not a directory: this one names the binary.
        let Some(path) = unsafe { layout_of(receiver) }
            .map(|b| b.executable.clone())
            .as_deref()
            .and_then(super::spelled)
        else {
            return std::ptr::null_mut();
        };
        // SAFETY: a URL this call just made.
        unsafe {
            corefoundation::api::CFAutorelease(corefoundation::url::new_file_url(
                &path.to_string_lossy(),
                false,
            )) as Id
        }
    }

    unsafe extern "C" fn resource_path(receiver: Id, _sel: Sel) -> Id {
        path(unsafe { layout_of(receiver) }.map(|b| b.resources.clone()))
    }

    unsafe extern "C" fn executable_path(receiver: Id, _sel: Sel) -> Id {
        path(unsafe { layout_of(receiver) }.map(|b| b.executable.clone()))
    }

    unsafe extern "C" fn private_frameworks_path(receiver: Id, _sel: Sel) -> Id {
        path(unsafe { layout_of(receiver) }.map(|b| b.frameworks.clone()))
    }

    /// `Contents/PlugIns`, which is not `Contents/Frameworks`: a caller looking
    /// for a loadable plug-in that is handed the frameworks directory searches
    /// the wrong one and finds nothing.
    unsafe extern "C" fn built_in_plug_ins_path(receiver: Id, _sel: Sel) -> Id {
        path(unsafe { layout_of(receiver) }.map(|b| b.plugins.clone()))
    }

    unsafe extern "C" fn bundle_identifier(receiver: Id, _sel: Sel) -> Id {
        unsafe { info(receiver, "CFBundleIdentifier") }
    }

    unsafe extern "C" fn object_for_info_key(receiver: Id, _sel: Sel, key: Id) -> Id {
        // SAFETY: the key is an `NSString`.
        let key = unsafe { crate::string::bytes(key) };
        match std::str::from_utf8(&key) {
            // SAFETY: the receiver is an `NSBundle`.
            Ok(key) => unsafe { info(receiver, key) },
            Err(_) => std::ptr::null_mut(),
        }
    }

    unsafe extern "C" fn path_for_resource(receiver: Id, _sel: Sel, name: Id, kind: Id) -> Id {
        // SAFETY: both are `NSString`s or nil.
        let (name, kind) = unsafe { (crate::string::bytes(name), crate::string::bytes(kind)) };
        let (Ok(name), Ok(kind)) = (std::str::from_utf8(&name), std::str::from_utf8(&kind)) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the receiver is an `NSBundle`.
        path(unsafe { layout_of(receiver) }.and_then(|b| b.resource(name, kind)))
    }

    /// `-URLForResource:withExtension:`, the URL form of
    /// `-pathForResource:ofType:`.
    unsafe extern "C" fn url_for_resource(receiver: Id, _sel: Sel, name: Id, kind: Id) -> Id {
        // SAFETY: both are `NSString`s or nil.
        let (name, kind) = unsafe { (crate::string::bytes(name), crate::string::bytes(kind)) };
        let (Ok(name), Ok(kind)) = (std::str::from_utf8(&name), std::str::from_utf8(&kind)) else {
            return std::ptr::null_mut();
        };
        resource_file_url(unsafe { layout_of(receiver) }.and_then(|b| b.resource(name, kind)))
    }

    unsafe extern "C" fn path_for_resource_in_directory(
        receiver: Id,
        _sel: Sel,
        name: Id,
        kind: Id,
        directory: Id,
    ) -> Id {
        // SAFETY: each argument is an `NSString` or nil.
        let (name, kind, directory) = unsafe {
            (
                crate::string::bytes(name),
                crate::string::bytes(kind),
                crate::string::bytes(directory),
            )
        };
        let (Ok(name), Ok(kind), Ok(directory)) = (
            std::str::from_utf8(&name),
            std::str::from_utf8(&kind),
            std::str::from_utf8(&directory),
        ) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the receiver is an `NSBundle`.
        path(unsafe { layout_of(receiver) }.and_then(|b| b.resource_in(name, kind, directory)))
    }

    /// `-infoDictionary`: the whole `Info.plist` as objects.
    ///
    /// This was refused until `crate::objectify` existed, and the reason is
    /// worth keeping: a dictionary missing a key is indistinguishable from a
    /// plist that never had one, so a *partial* conversion is worse than none.
    /// What makes it answerable now is that the conversion is total - every
    /// plist type is either built or refused by name.
    unsafe extern "C" fn info_dictionary(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: the receiver is an `NSBundle`.
        match unsafe { layout_of(receiver) } {
            Some(layout) => crate::objectify::object(&layout.info, "-[NSBundle infoDictionary]"),
            None => std::ptr::null_mut(),
        }
    }

    unsafe extern "C" fn executable_name(_receiver: Id, _sel: Sel) -> *const c_char {
        std::ptr::null()
    }

    pub(crate) fn install() {
        let class =
            corefoundation::base::register(c"NSBundle", None, std::mem::size_of::<Body>(), false);
        methods!(class, {
            "bundlePath" => bundle_path, c"@16@0:8";
            "bundleURL" => bundle_url, c"@16@0:8";
            "resourceURL" => resource_url, c"@16@0:8";
            "executableURL" => executable_url, c"@16@0:8";
            "resourcePath" => resource_path, c"@16@0:8";
            "executablePath" => executable_path, c"@16@0:8";
            "privateFrameworksPath" => private_frameworks_path, c"@16@0:8";
            "builtInPlugInsPath" => built_in_plug_ins_path, c"@16@0:8";
            "bundleIdentifier" => bundle_identifier, c"@16@0:8";
            "objectForInfoDictionaryKey:" => object_for_info_key, c"@24@0:8@16";
            "pathForResource:ofType:" => path_for_resource, c"@32@0:8@16@24";
            "URLForResource:withExtension:" => url_for_resource, c"@32@0:8@16@24";
            "pathForResource:ofType:inDirectory:" => path_for_resource_in_directory, c"@40@0:8@16@24@32";
            "infoDictionary" => info_dictionary, c"@16@0:8";
            "executableName" => executable_name, c"*16@0:8";
            "localizedStringForKey:value:table:" => localized_string_for_key, c"@40@0:8@16@24@32";
            "preferredLocalizations" => preferred_localizations, c"@16@0:8";
            "load" => load, c"c16@0:8";
            "loadAndReturnError:" => load_and_return_error, c"c24@0:8^@16";
            "isLoaded" => is_loaded, c"c16@0:8";
            "classNamed:" => class_named, c"#24@0:8@16";
            "principalClass" => principal_class, c"#16@0:8";
            "localizations" => localizations, c"@16@0:8";
            "pathsForResourcesOfType:inDirectory:" => paths_for_resources_of_type, c"@32@0:8@16@24"
        });
        class_methods!(class, {
            "mainBundle" => main_bundle, c"@16@0:8";
            "bundleForClass:" => bundle_for_class, c"@24@0:8#16";
            "pathsForResourcesOfType:inDirectory:" => class_paths_for_resources_of_type, c"@32@0:8@16@24";
            "bundleWithPath:" => bundle_with_path, c"@24@0:8@16";
            "bundleWithURL:" => bundle_with_url, c"@24@0:8@16"
        });
    }
}
