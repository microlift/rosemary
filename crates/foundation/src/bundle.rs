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
    prefix::ns::host_path(&path.to_string_lossy())
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

/// `name` and `extension` as the one filename a resource lookup is for.
///
/// An empty `extension` means the name already carries one, which is how
/// `[NSBundle pathForResource:@"MainMenu" ofType:@"nib"]` and
/// `[... pathForResource:@"MainMenu" ofType:@""]` both reach the same file.
/// Every form below goes through this, so there is one spelling of "name plus
/// type" in this crate rather than one per selector.
fn leaf_of(name: &str, extension: &str) -> String {
    match extension.is_empty() {
        true => name.to_string(),
        false => format!("{name}.{extension}"),
    }
}

/// `root`/`directory`/`leaf`, with an empty `directory` meaning the root itself.
///
/// Apple's `inDirectory:` is a *subdirectory of the bundle's resource
/// directory*, and nil or `""` is how a caller spells "no subdirectory" -
/// which is not the same as a directory whose name is the empty string. The
/// branch is here rather than left to `Path::join`, whose `join("")` writes a
/// trailing separator and would make the two spellings differ in the string
/// handed back even where they name one file.
fn under(root: &Path, directory: &str, leaf: &str) -> PathBuf {
    match directory.is_empty() {
        true => root.join(leaf),
        false => root.join(directory).join(leaf),
    }
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
        if !info_path.is_file() {
            return Layout::framework_at(root);
        }
        let info = plist::parse(&std::fs::read(&info_path).ok()?).ok()?;
        let executable = info
            .get("CFBundleExecutable")
            .and_then(|name| name.as_str())
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

    /// A framework's bundle, which has no `Contents`: `Resources/Info.plist`
    /// and the binary sit directly in the version directory, and the
    /// `.framework` directory itself reaches them through the `Resources` and
    /// executable symlinks into `Versions/Current`.
    ///
    /// Chromium opens its own framework this way before anything else -
    /// `base::apple::SetOverrideFrameworkBundlePath` sends `+bundleWithURL:` the
    /// `Chromium Framework.framework/Versions/<version>` directory and `CHECK`s
    /// that a bundle came back - so a loader that only knows the `.app` shape
    /// stops Chromium at its first line of `ChromeMain`.
    fn framework_at(root: &Path) -> Option<Layout> {
        let resources = root.join("Resources");
        let info = plist::parse(&std::fs::read(resources.join("Info.plist")).ok()?).ok()?;
        let executable = info
            .get("CFBundleExecutable")
            .and_then(|name| name.as_str())
            .map(|name| root.join(name))
            .unwrap_or_default();
        Some(Layout {
            root: root.to_path_buf(),
            resources,
            frameworks: root.join("Frameworks"),
            plugins: plugins_of(root),
            executable,
            contents: root.to_path_buf(),
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
                std::fs::read(&path).ok().and_then(|bytes| crate::strings::parse(&bytes))
            });
            if let Some(found) = parsed.as_ref().and_then(|t| t.get(key)) {
                return found.clone();
            }
        }
        value.filter(|v| !v.is_empty()).unwrap_or(key).to_string()
    }

    /// Localization identifiers declared by the bundle and present as
    /// `Resources/*.lproj` directories.
    /// What localizations this bundle has, which is
    /// `CFBundleCopyBundleLocalizations`.
    ///
    /// Three sources, in Apple's order (`CFBundle_Locale.c` in
    /// `apple-oss-distributions/CF`, tag `CF-1153.18`): `CFBundleLocalizations`
    /// from the plist, the `Resources/*.lproj` directories, and - but only
    /// where the first two say nothing - the development region. That third
    /// one is the rule with a condition in it, and the condition is most of it:
    ///
    /// * neither of the others, so the answer is the development region, or an
    ///   empty array where there is no development region either. This is the
    ///   only case in which the development region stands in for a
    ///   localization rather than joining the list.
    /// * the list is not empty and one of its entries is `Base` - so the
    ///   development region is appended, unless it is already there under
    ///   another spelling (`en` and `English` name one language, and adding
    ///   `en` beside an existing `English` would report the language twice).
    ///   `Base.lproj` *is* the development region's resources, so a lookup that
    ///   did not find it named would reach for some other language instead.
    /// * the list is not empty and none of its entries is `Base` - the
    ///   development region is left out. `en.lproj` is a real localization in
    ///   its own right, and putting `en` on the list when `en.lproj` is what
    ///   supplies it would misreport what the bundle ships.
    ///
    /// The plist array and the directory listing are merged and sorted, where
    /// Apple returns the plist's own entries followed by the directories in
    /// `readdir` order. Sorting is the whole of the difference and it is
    /// deliberate: this list is what decides which localization a bundle that
    /// has none of the user's falls back to, and a filesystem ordering would
    /// make that answer a property of how the directory happens to be laid out
    /// on disk.
    pub fn localization_names(&self) -> Vec<String> {
        let mut names = self
            .info
            .get("CFBundleLocalizations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            // An empty entry is not a localization; Apple's filters them out
            // with `CFStringGetLength(oneEntry) > 0` for the same reason.
            .filter(|name| !name.is_empty())
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
        if names.is_empty() {
            return self.info_string("CFBundleDevelopmentRegion").map_or_else(Vec::new, |region| vec![region.to_owned()]);
        }
        let Some(region) = self.info_string("CFBundleDevelopmentRegion") else {
            return names;
        };
        if !names.iter().any(|name| name == "Base") {
            return names;
        }
        if preference::found_in(&names, region).is_none() {
            names.push(region.to_owned());
        }
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
        let leaf = leaf_of(name, extension);
        std::iter::once(self.resources.clone())
            .chain(self.localizations())
            .map(|root| under(&root, directory, &leaf))
            .find(|p| p.exists())
    }

    /// `-pathForResource:ofType:inDirectory:forLocalization:`.
    ///
    /// `localization` is the whole difference from [`Layout::resource_in`], and
    /// it is the argument that makes this four-argument form exist at all: it
    /// names **one** `.lproj`, and that directory is the only place searched.
    /// There is no walk down to another localization and no fall back to the
    /// unlocalized `Resources`, because a caller that passed a language wants
    /// that language's file *or nothing* - handing it back the unlocalized copy
    /// would answer with a file whose contents are in some other language while
    /// claiming otherwise, which is the one thing this whole lookup must not do.
    ///
    /// `None` - a nil `forLocalization:` - is not that case but the opposite one:
    /// nil means "search the bundle's own preferred localizations", which is
    /// exactly what the two shorter forms do, so it is answered by them rather
    /// than by a rule of its own. Chromium's own source makes the same
    /// distinction, and the guest's copy of its branch is the `test r12,r12` at
    /// `+0x692c003` that chooses between the four- and the two-argument send -
    /// `r12` there is `[mac_locale length]`, so a nil localization never reaches
    /// this method at all and a non-empty one always does.
    ///
    /// An **empty** `localization` is a third thing again, and gets no special
    /// case: it is taken literally, so the directory searched is
    /// `Resources/.lproj`, which is not a directory any bundle ships, and the
    /// answer is nil. Guessing that `""` "meant" nil - or the unlocalized
    /// resources - would answer a question nobody asked; a real directory by
    /// that name would still be found, because the lookup is the filesystem's.
    /// No guest reaches it, for the reason in the paragraph above.
    ///
    /// `inDirectory` keeps the meaning [`Layout::resource_in`] gives it: a
    /// subdirectory of the resource root, and `""` or nil for the resource
    /// directory itself. Chromium passes `CFSTR("")` here and that is a real
    /// spelling of "the bundle's own `Resources`", not a directory named by the
    /// empty string - the guest's `+0x692c016` loads exactly that constant and
    /// the file it reaches is `Resources/en.lproj/locale.pak`.
    pub fn resource_in_localized(
        &self,
        name: &str,
        extension: &str,
        directory: &str,
        localization: Option<&str>,
    ) -> Option<PathBuf> {
        // Nil is the shorter forms' question, so it is *answered by* them.
        let localization = match localization {
            None => return self.resource_in(name, extension, directory),
            Some(localization) => localization,
        };
        let leaf = leaf_of(name, extension);
        let path = under(
            &self.resources.join(format!("{localization}.lproj")),
            directory,
            &leaf,
        );
        path.exists().then_some(path)
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

/// Which of a bundle's localizations to use, and in what order, which is the
/// whole of what `-[NSBundle preferredLocalizations]` is.
///
/// Read out of Apple's own open-source CoreFoundation rather than out of the
/// header, because the header does not say what happens when nothing matches
/// and the binary has to be satisfied: `CFBundle_Locale.c` in
/// `apple-oss-distributions/CF` at tag `CF-1153.18`, which is the release that
/// still carries `CFBundleCopyPreferredLocalizationsFromArray`. The chain of
/// calls there is
///
/// ```text
/// CFBundleCopyPreferredLocalizationsFromArray(localizations)
///   -> _CFBundleCopyLocalizationsForPreferences(localizations, user, considerMain)
///        -> _CFBundleCopyPreferredLanguagesInList(localizations, NULL, user, ...)
///             -> _CFBundleCreateMutableArrayOfFallbackLanguages(localizations, language)
///                  -> _CFBundleCopyLanguageFoundInLocalizations(localizations, language)
///                  -> ualoc_getAppleParent, in a loop
/// ```
///
/// and what it adds up to is: the *first* user language the bundle has anything
/// for, in the bundle's own spelling, followed by that language's less specific
/// forms; failing that `en_US`; failing that the bundle's own first
/// localization; and failing that, for a bundle with no localizations at all,
/// `en`. Note "the first user language" and not "every user language the
/// bundle has": the loop in `_CFBundleCopyPreferredLanguagesInList` breaks at
/// the first preference that resolves, and it is the *user's* order that
/// decides which one that is.
///
/// Two steps of the walk are not implemented here, and both are named rather
/// than approximated:
///
/// * `ualoc_getAppleParent` walks a *tree* of locales, not a suffix, so the
///   parent of `en_IN` is `en_GB`, whose parent is `en_001`, whose parent is
///   `en`. Those intermediate parents are ICU data and this tree carries no
///   ICU data. A rule read off the shape of the identifier instead - "drop the
///   last subtag" - would answer `en` where Apple answers `en_001`, and would
///   do it silently, so it is not here.
/// * `_CFBundleCopyLanguageFoundInLocalizations` finishes by comparing
///   `CFLocaleCreateCanonicalLanguageIdentifierFromString` forms, which is a
///   language-subtag database and not a string operation.
///
/// What *is* implemented is the last hop of the parent walk, because Apple's
/// own comment states it as a rule rather than leaving it to ICU:
/// `_CFBundleCopyPreferredLanguagesInList` is documented as "The users list
/// can contain region names (like "en_US" for US English). In this case, if
/// the region lproj exists, it will be added, and, if the region's associated
/// language lproj exists that will be added." So a preference of `en-US`
/// against a bundle with `en.lproj` and no `en_US.lproj` answers `en`, which
/// is what macOS answers, and which is the case this guest actually hits.
pub(crate) mod preference {
    /// The user's language preferences, in the order Apple would ask for them.
    ///
    /// Apple's is `_CFBundleCopyUserLanguages()`, which reads the
    /// `AppleLanguages` preference. This host has no preferences database -
    /// `crate::locale` says so once and answers every other "which language is
    /// this" question from the environment - so this is built from the same two
    /// accessors `+[NSLocale preferredLanguages]` builds its one entry from.
    /// Reading them rather than re-deriving them from `LANG` is what stops the
    /// class and the bundle being told two different things about one user.
    pub(crate) fn user_languages() -> Vec<String> {
        vec![format!(
            "{}-{}",
            crate::locale::language(),
            crate::locale::country_code()
        )]
    }

    /// The localizations to use, in the order to use them.
    ///
    /// `main` is the main bundle's answer to the same question, which Apple
    /// consults *before* the user's own preferences - but only for a bundle
    /// that is not the main bundle, so a caller asking about the main bundle
    /// passes `None`.
    pub(crate) fn resolve(
        supported: &[String],
        user: &[String],
        main: Option<&[String]>,
    ) -> Vec<String> {
        // `_CFBundleCopyLocalizationsForPreferences` does none of the choosing
        // when there is nothing to choose from, and its backstops sit outside
        // that branch: "Total backstop behavior to avoid having an empty
        // array" is a one-element array holding `en`.
        if supported.is_empty() {
            return vec![String::from("en")];
        }
        let mut result = Vec::new();
        if let Some(main) = main {
            if let Some(first) = main.first() {
                result = chain(supported, first);
            }
        }
        if result.is_empty() {
            for wanted in user {
                result = chain(supported, wanted);
                if !result.is_empty() {
                    break;
                }
            }
        }
        // "use development region and U.S. English as backstops". The
        // development region is `NULL` on this path: that is what
        // `_CFBundleCopyLocalizationsForPreferences` passes, and it is why this
        // is `en_US` alone.
        if result.is_empty() {
            result = chain(supported, "en_US");
        }
        // "Additional backstop behavior: use first entry as backstop".
        if result.is_empty() {
            result.push(supported[0].clone());
        }
        result
    }

    /// One language preference, as the localizations that cover it, most
    /// specific first.
    ///
    /// `_CFBundleCreateMutableArrayOfFallbackLanguages` puts the language
    /// itself in first and then appends each parent the bundle also has; this
    /// is the language itself and the last of those parents, which is the one
    /// this module's note explains. A preference that names no region is tried
    /// once, because its own parent is the root.
    fn chain(supported: &[String], language: &str) -> Vec<String> {
        let bare = language
            .split(['-', '_'])
            .next()
            .filter(|bare| bare.len() < language.len())
            .unwrap_or(language);
        let mut out = Vec::new();
        for wanted in [language, bare] {
            if let Some(found) = found_in(supported, wanted)
                && !out.contains(&found)
            {
                out.push(found);
            }
        }
        out
    }

    /// `language` as `supported` spells it, or `None` for a language this
    /// bundle has nothing for.
    ///
    /// `_CFBundleCopyLanguageFoundInLocalizations`, in its own order: the
    /// identifier itself, then the full language name for the seven languages
    /// Apple keeps one for, then the identifier with the separator swapped.
    /// Its canonicalising last step is the one this module's note declines.
    pub(crate) fn found_in(supported: &[String], language: &str) -> Option<String> {
        let holds = |name: &str| supported.iter().any(|held| held == name);
        if holds(language) {
            return Some(language.to_owned());
        }
        // `__CFBundleCommonLanguageNamesArray` and the abbreviations beside
        // it: seven languages, in each direction, which are the only
        // full-name/abbreviation pairs Apple checks for.
        const NAMED: [(&str, &str); 7] = [
            ("en", "English"),
            ("fr", "French"),
            ("de", "German"),
            ("it", "Italian"),
            ("nl", "Dutch"),
            ("es", "Spanish"),
            ("ja", "Japanese"),
        ];
        for (abbreviation, name) in NAMED {
            for (mine, theirs) in [(abbreviation, name), (name, abbreviation)] {
                if language == mine && holds(theirs) {
                    return Some(theirs.to_owned());
                }
            }
        }
        separator_swapped(language).filter(|swapped| holds(swapped))
    }

    /// `name` with `_` and `-` exchanged at the third character, which is
    /// where a language and its region meet: `en-US` becomes `en_US`, `zh_CN`
    /// becomes `zh-CN`.
    ///
    /// `_CFBundleCopyModifiedLocalization` rewrites the character at index 2
    /// and only that one, and only for a name of four characters or more -
    /// three cannot hold both a language and a region, so `en-` stays `en-`.
    /// Not ASCII, this does not: an identifier with a non-ASCII character in it
    /// is one there is no rule here for, and the rules of another are worse
    /// than none.
    pub(crate) fn separator_swapped(name: &str) -> Option<String> {        if !name.is_ascii() || name.len() < 4 {
            return None;
        }
        let mut bytes = name.as_bytes().to_vec();
        match bytes[2] {
            b'-' => bytes[2] = b'_',
            b'_' => bytes[2] = b'-',
            _ => return None,
        }
        String::from_utf8(bytes).ok()
    }
}

/// A guest's array of `NSString`s, as the text they spell.
///
/// Anything that is not a string is left out rather than refused, which is what
/// the arrays this is asked about are: a localization array, which is a
/// question about which languages a bundle has, and a wrong answer there is
/// better than no answer only if it is a wrong one nobody can see.
///
/// # Safety
///
/// `array` must be null or a `CFArray` this runtime allocated.
unsafe fn strings_of(array: objc::object::Id) -> Vec<String> {
    // SAFETY: as this function's safety note.
    let Some(array) = (unsafe { corefoundation::collections::array(array as corefoundation::base::CFTypeRef) })
    else {
        return Vec::new();
    };
    array
        .values
        .iter()
        .filter_map(|value| {
            // SAFETY: an element of an array of localizations is an `NSString`.
            let bytes = unsafe { crate::string::bytes(*value as objc::object::Id) };
            std::str::from_utf8(&bytes).ok().map(str::to_owned)
        })
        .collect()
}

/// An array of `NSString`s the caller owns, as every `CFBundleCopy…` hands
/// back.
///
/// # Safety
///
/// Each name is copied into a string of its own and the array retains it, so
/// the only reference this creates is the one it returns.
fn string_array(names: &[String]) -> corefoundation::base::CFTypeRef {
    let values = names
        .iter()
        .map(|name| crate::string::from_str(name) as corefoundation::base::CFTypeRef)
        .collect::<Vec<_>>();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for value in values {
        // SAFETY: each is the live object made a line above, and the array
        // retained a reference of its own to it.
        unsafe { corefoundation::api::CFRelease(value) };
    }
    array
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
        _bundle: Id,
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
        let Some(path) = super::main().and_then(|b| b.resource_in(name, kind, subdirectory)) else {
            return std::ptr::null_mut();
        };
        file_url(&path, false)
    }

    /// `CFURLRef CFBundleCopyExecutableURL(CFBundleRef)`
    unsafe extern "C" fn CFBundleCopyExecutableURL(_bundle: Id) -> Id {
        match super::main() {
            Some(bundle) => file_url(&bundle.executable, false),
            None => std::ptr::null_mut(),
        }
    }

    /// `CFURLRef CFBundleCopyBundleURL(CFBundleRef)`
    unsafe extern "C" fn CFBundleCopyBundleURL(_bundle: Id) -> Id {
        match super::main() {
            Some(bundle) => file_url(&bundle.root, true),
            None => std::ptr::null_mut(),
        }
    }

    /// `CFURLRef CFBundleCopyResourcesDirectoryURL(CFBundleRef)`
    ///
    /// The directory rather than a file in it, so the trailing slash a caller
    /// appends a name onto is there.
    unsafe extern "C" fn CFBundleCopyResourcesDirectoryURL(_bundle: Id) -> Id {
        match super::main() {
            Some(bundle) => file_url(&bundle.resources, true),
            None => std::ptr::null_mut(),
        }
    }

    /// `CFURLRef CFBundleCopyPrivateFrameworksURL(CFBundleRef)`
    ///
    /// Answered whether or not the directory exists, which is Apple's behaviour:
    /// the URL is where a private framework *would* be, and a caller that finds
    /// nothing there is entitled to that answer rather than to a null.
    unsafe extern "C" fn CFBundleCopyPrivateFrameworksURL(_bundle: Id) -> Id {
        match super::main() {
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
    unsafe extern "C" fn CFBundleCopyBuiltInPlugInsURL(_bundle: Id) -> Id {
        match super::main() {
            Some(bundle) => file_url(&bundle.plugins, true),
            None => std::ptr::null_mut(),
        }
    }

    /// `CFArrayRef CFBundleCopyBundleLocalizations(CFBundleRef)`.
    unsafe extern "C" fn CFBundleCopyBundleLocalizations(_bundle: Id) -> Id {
        let names = super::main().map(|bundle| bundle.localization_names());
        match names {
            // A `Copy`, so the caller owns the array this returns.
            Some(names) => super::string_array(&names) as Id,
            None => std::ptr::null_mut(),
        }
    }

    /// `CFArrayRef CFBundleCopyPreferredLocalizationsFromArray(CFArrayRef)`.
    ///
    /// The whole contract is [`super::preference`]; this moves the caller's
    /// array in and the answer out. Two things about the answer are Apple's
    /// and worth stating where they are decided: it is a `Copy`, so the caller
    /// owns what it gets back, and an empty or null input answers `["en"]`
    /// rather than an empty array, which is `CFBundleCopyPreferred-
    /// LocalizationsFromArray` inheriting
    /// `_CFBundleCopyLocalizationsForPreferences`'s "Total backstop behavior to
    /// avoid having an empty array".
    unsafe extern "C" fn CFBundleCopyPreferredLocalizationsFromArray(localizations: Id) -> Id {
        // SAFETY: a CFArray or null.
        let candidates = unsafe { super::strings_of(localizations) };
        let user = super::preference::user_languages();
        super::string_array(&super::preference::resolve(&candidates, &user, None)) as Id
    }

    /// `CFArrayRef CFBundleCopyLocalizationsForPreferences(CFArrayRef locArray,
    /// CFArrayRef prefArray)`.
    ///
    /// The same order with the preferences named rather than taken from the
    /// process, and without the main bundle: Apple's own comment on the pair of
    /// functions is that this is the one which answers "what localizations
    /// would *that* application use", so it must not consult this
    /// application's own.
    unsafe extern "C" fn CFBundleCopyLocalizationsForPreferences(
        localizations: Id,
        preferred: Id,
    ) -> Id {
        // SAFETY: two CFArrays or null.
        let (candidates, preferences) =
            unsafe { (super::strings_of(localizations), super::strings_of(preferred)) };
        super::string_array(&super::preference::resolve(&candidates, &preferences, None)) as Id
    }

    /// `CFStringRef CFBundleGetIdentifier(CFBundleRef)`
    ///
    /// A `Get`, so the string is not the caller's to release - which is why it is
    /// interned rather than made fresh each time.
    unsafe extern "C" fn CFBundleGetIdentifier(_bundle: Id) -> Id {
        let Some(identifier) = super::main().and_then(|b| b.identifier()) else {
            return std::ptr::null_mut();
        };
        crate::notification::name_object(identifier) as Id
    }

    fn info_dictionary() -> Id {
        static INFO: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *INFO.get_or_init(|| {
            let Some(bundle) = super::main() else {
                return 0;
            };
            let object = crate::objectify::object(&bundle.info, "CFBundleGetInfoDictionary");
            // A Get result lives with the bundle rather than with an autorelease
            // pool, so keep the objectified plist for the process.
            objc::arc::retain(object);
            object as usize
        }) as Id
    }

    unsafe extern "C" fn CFBundleGetInfoDictionary(_bundle: Id) -> Id {
        info_dictionary()
    }

    unsafe extern "C" fn CFBundleGetValueForInfoDictionaryKey(_bundle: Id, key: Id) -> Id {
        let dictionary = info_dictionary();
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

    /// `UInt32 CFBundleGetVersionNumber(CFBundleRef)`
    ///
    /// Apple's is a packed BCD `vvvv.rr.ss` word. Only the major version is
    /// filled in: a caller compares the whole word against a constant of its
    /// own, and a fabricated revision would make that comparison say something
    /// the `Info.plist` never did.
    unsafe extern "C" fn CFBundleGetVersionNumber(_bundle: Id) -> u32 {
        let Some(version) = super::main().and_then(|b| b.version()) else {
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
    /// Only the main bundle, and only when the URL names it. A guest asking about
    /// its own `.app` by URL - which is what a program that already called
    /// `CFBundleCopyExecutableURL` does - gets the bundle it means; anything else
    /// is a bundle this runtime has not loaded, and a null says so where an
    /// invented one would answer questions about the wrong directory.
    unsafe extern "C" fn CFBundleCreate(_allocator: *const std::ffi::c_void, url: Id) -> Id {
        // SAFETY: a `CFURL` or null.
        let Some(url) = (unsafe { corefoundation::url::as_url(url as CFTypeRef) }) else {
            return std::ptr::null_mut();
        };
        let Some(bundle) = super::main() else {
            return std::ptr::null_mut();
        };
        // A bundle is named by a file URL and by nothing else: a `https:` URL
        // has no directory for this to be about.
        let Some(path) = url.file_path() else {
            return std::ptr::null_mut();
        };
        // The guest named it, so it is compared at the spelling this crate
        // keeps rather than the one the guest used.
        if super::opened(std::path::Path::new(&path)) != bundle.root {
            return std::ptr::null_mut();
        }
        // A `Create`, so the caller owns a reference to the singleton.
        let object = super::class::main_object();
        objc::arc::retain(object);
        object
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
    unsafe extern "C" fn CFBundleCopyLocalizedString(bundle: Id, key: Id, value: Id, table: Id) -> Id {
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
            "CFBundleCopyLocalizationsForPreferences" => CFBundleCopyLocalizationsForPreferences,
            "CFBundleGetIdentifier" => CFBundleGetIdentifier,
            "CFBundleGetInfoDictionary" => CFBundleGetInfoDictionary,
            "CFBundleGetValueForInfoDictionaryKey" => CFBundleGetValueForInfoDictionaryKey,
            "CFBundleGetVersionNumber" => CFBundleGetVersionNumber,
            "CFBundleCreate" => CFBundleCreate,
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

/// The main bundle, or `None` for a program that is not in one.
pub fn main() -> Option<&'static Layout> {
    *MAIN.read().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a bundle-shaped directory, since the claim is about shape.
    fn scratch(name: &str, info: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rosetta3-bundle-{name}"));
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

    /// The shape Chromium's own bundles have: an unlocalized pak in
    /// `Resources`, a *different* pak of the same name in every `.lproj`, and
    /// no `CFBundleLocalizations` to say which exist - only the directories.
    fn pak_bundle(name: &str) -> PathBuf {
        let exe = scratch(name, INFO);
        let resources = exe.parent().unwrap().parent().unwrap().join("Resources");
        std::fs::write(resources.join("chrome_100_percent.pak"), b"common").unwrap();
        for (language, body) in [("en", &b"english"[..]), ("fr", &b"french"[..])] {
            std::fs::create_dir_all(resources.join(format!("{language}.lproj"))).unwrap();
            std::fs::write(resources.join(format!("{language}.lproj/locale.pak")), body).unwrap();
        }
        exe
    }

    /// `forLocalization:` names one `.lproj` and nothing else is searched. The
    /// two assertions that matter are the ones a *fallback* would break: a
    /// language the bundle has no pak for is nil, not some other language's,
    /// and not the unlocalized file that sits one directory up.
    #[test]
    fn a_named_localization_is_the_only_directory_searched() {
        let layout = Layout::enclosing(&pak_bundle("for-localization")).expect("a bundle");
        let pak = |language: Option<&str>| {
            layout.resource_in_localized("locale", "pak", "", language)
        };
        assert_eq!(
            pak(Some("en")),
            Some(layout.resources.join("en.lproj/locale.pak"))
        );
        assert_eq!(
            pak(Some("fr")),
            Some(layout.resources.join("fr.lproj/locale.pak"))
        );
        assert_eq!(pak(Some("de")), None, "and not en's, or the unlocalized one");
        // Nil is the *shorter* forms' question and has to answer as they do.
        // `Resources` has no `locale.pak` at all in this bundle, so the
        // unlocalized search misses it too - which is the point: a nil
        // localization must not be answered out of some `.lproj`.
        assert_eq!(pak(None), layout.resource("locale", "pak"));
        assert_eq!(pak(None), None);
        assert_eq!(
            layout.resource_in_localized("chrome_100_percent", "pak", "", None),
            Some(layout.resources.join("chrome_100_percent.pak"))
        );
    }

    /// An empty localization is taken literally rather than read as nil: it
    /// names `Resources/.lproj`, which nothing ships, so the answer is nil and
    /// not a file from somewhere else.
    #[test]
    fn an_empty_localization_is_not_nil() {
        let layout = Layout::enclosing(&pak_bundle("empty-localization")).expect("a bundle");
        assert_eq!(layout.resource_in_localized("locale", "pak", "", Some("")), None);
        assert_eq!(
            layout.resource_in_localized("chrome_100_percent", "pak", "", Some("")),
            None,
            "the unlocalized pak is one directory up, and is not what was asked for"
        );
    }

    /// `inDirectory:` keeps its meaning with a localization in play: a
    /// subdirectory *of the resource root*, so of the `.lproj` and not of
    /// `Resources`, and `""` for no subdirectory at all.
    #[test]
    fn a_resource_directory_is_relative_to_the_lproj_too() {
        let exe = pak_bundle("localized-directory");
        let resources = exe.parent().unwrap().parent().unwrap().join("Resources");
        std::fs::create_dir_all(resources.join("en.lproj/nested")).unwrap();
        std::fs::write(resources.join("en.lproj/nested/locale.pak"), b"english").unwrap();
        let layout = Layout::enclosing(&exe).expect("a bundle");
        assert_eq!(
            layout.resource_in_localized("locale", "pak", "nested", Some("en")),
            Some(resources.join("en.lproj/nested/locale.pak"))
        );
        assert_eq!(
            layout.resource_in_localized("locale", "pak", "nested", Some("fr")),
            None,
            "fr.lproj has the pak, not fr.lproj/nested"
        );
        // `""` is the resource directory itself, and the empty string does not
        // become a directory of its own on the way.
        assert_eq!(
            layout.resource_in_localized("locale", "pak", "", Some("en")),
            Some(resources.join("en.lproj/locale.pak"))
        );
    }

    /// `ofType:@""` reaches the same file as spelling the extension in the
    /// name, in this form too - the type is joined, not checked.
    #[test]
    fn an_empty_type_is_the_name_as_given() {
        let layout = Layout::enclosing(&pak_bundle("empty-type")).expect("a bundle");
        let resources = &layout.resources;
        assert_eq!(
            layout.resource_in_localized("locale.pak", "", "", Some("en")),
            Some(resources.join("en.lproj/locale.pak"))
        );
        assert_eq!(
            layout.resource_in_localized("locale.pak", "pak", "", Some("en")),
            None,
            "the type is joined to the name, not checked against it"
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

    /// Chromium's shape: a versioned framework, opened both at its version
    /// directory and at the `.framework` itself through the `Versions/Current`
    /// symlinks.
    #[test]
    fn a_framework_is_a_bundle_at_its_version_and_at_its_top() {
        let root = std::env::temp_dir().join("rosetta3-bundle-framework");
        let _ = std::fs::remove_dir_all(&root);
        let framework = root.join("Thing Framework.framework");
        let version = framework.join("Versions/1.2.3");
        std::fs::create_dir_all(version.join("Resources")).unwrap();
        let info = INFO.replace("<string>Thing</string>", "<string>Thing Framework</string>");
        std::fs::write(version.join("Resources/Info.plist"), info).unwrap();
        std::fs::write(version.join("Thing Framework"), b"").unwrap();
        std::os::unix::fs::symlink("1.2.3", framework.join("Versions/Current")).unwrap();
        std::os::unix::fs::symlink("Versions/Current/Resources", framework.join("Resources"))
            .unwrap();

        let layout = Layout::at(&version).expect("the version directory is a bundle");
        assert_eq!(layout.identifier(), Some("com.example.thing"));
        assert_eq!(layout.executable, version.canonicalize().unwrap().join("Thing Framework"));
        assert!(layout.resource("Info", "plist").is_some());

        let layout = Layout::at(&framework).expect("and so is the .framework");
        assert_eq!(layout.identifier(), Some("com.example.thing"));

        // Neither shape: no `Contents/Info.plist`, no `Resources/Info.plist`.
        assert!(Layout::at(&root).is_none());
    }

    /// An `Info.plist` that cannot be read is a bundle this runtime declines to
    /// recognise, rather than one with an empty identity.
    #[test]
    fn an_unreadable_info_plist_is_not_a_bundle() {
        let exe = scratch("broken", "not a plist at all");
        assert!(Layout::enclosing(&exe).is_none());
    }

    /// The development region joins the list only where Apple says it does:
    /// not at all for a bundle whose `.lproj` directories are already
    /// localizations of their own, and appended for one whose only entry is
    /// `Base`, which *is* the development region's resources.
    #[test]
    fn the_development_region_joins_only_where_base_lproj_is() {
        let without_base = scratch(
            "dev-region-unused",
            &INFO.replace(
                "<key>CFBundleDevelopmentRegion</key><string>English</string>",
                "",
            ),
        );
        let layout = Layout::enclosing(&without_base).expect("a bundle");
        // `en` and `English` name one language, and `English.lproj` is a real
        // localization: the development region is left out entirely.
        assert_eq!(layout.localization_names(), ["English"]);

        let base = scratch("dev-region-base", INFO);
        let resources = base.parent().unwrap().parent().unwrap().join("Resources");
        std::fs::create_dir_all(resources.join("Base.lproj")).unwrap();
        let layout = Layout::enclosing(&base).expect("a bundle");
        assert_eq!(layout.localization_names(), ["Base", "English"]);

        // Nothing but a development region to report, which is the one case in
        // which it stands in for a localization rather than joining the list.
        let empty = scratch("dev-region-alone", INFO);
        std::fs::remove_dir_all(
            empty
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("Resources/English.lproj"),
        )
        .unwrap();
        let layout = Layout::enclosing(&empty).expect("a bundle");
        assert_eq!(layout.localization_names(), ["English"]);
    }
}

#[cfg(test)]
mod preference_tests {
    use super::preference::{resolve, separator_swapped};
    use super::cf;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| String::from(*name)).collect()
    }

    /// The order is the *user's*, and it is the first preference the bundle has
    /// anything for that decides it - not the bundle's own order, and not every
    /// preference at once.
    #[test]
    fn the_first_preference_the_bundle_has_decides() {
        let supported = names(&["en", "en_GB", "fr", "zh_CN"]);
        assert_eq!(
            resolve(&supported, &names(&["fr-FR", "en-US"]), None),
            names(&["fr"]),
            "fr comes first and the bundle has it, so en is not consulted"
        );
        assert_eq!(
            resolve(&supported, &names(&["en-US", "fr-FR"]), None),
            names(&["en"]),
            "the same two preferences the other way round"
        );
    }

    /// The case this guest actually hits: Chromium.app has `en.lproj` and
    /// `en_GB.lproj` and no `en_US.lproj`, and the one language this host has
    /// is `en-US`. Apple's parent walk sends that to the bare language, so the
    /// answer is `en` and element zero is `en` - which is what macOS answers,
    /// and what the guest at `Framework+0x692c821` expects to be able to
    /// rewrite into `en-US`.
    #[test]
    fn a_regional_preference_falls_back_to_the_bare_language() {
        let chromium = names(&[
            "af", "am", "ar", "en", "en_GB", "fr", "fr_CA", "pt_BR", "zh_CN",
        ]);
        assert_eq!(
            resolve(&chromium, &names(&["en-US"]), None),
            names(&["en"])
        );
        // And with the region spelled the other way, which is what a
        // `.lproj` directory is named.
        assert_eq!(
            resolve(&names(&["en_GB", "en_US"]), &names(&["en_US"]), None),
            names(&["en_US"])
        );
    }

    /// The fallback ladder, in Apple's order: nothing the user asked for, then
    /// `en_US`, then the bundle's own first localization.
    #[test]
    fn the_fallbacks_are_in_apples_order() {
        // `en_US` is the second backstop, and it is reached before the bundle's
        // first localization.
        assert_eq!(
            resolve(&names(&["af", "am", "en_US"]), &names(&["ja"]), None),
            names(&["en_US"])
        );
        // With no `en_US`, the bundle's own first localization. `af` is first
        // because `localization_names` sorts, which is deliberate: readdir
        // order would make this a property of the filesystem.
        assert_eq!(
            resolve(&names(&["af", "am", "ar"]), &names(&["ja"]), None),
            names(&["af"])
        );
        // And a bundle with no localizations at all answers `en`, which is
        // `CFBundleCopyLocalizationsForPreferences`'s "total backstop behavior
        // to avoid having an empty array" - not an empty array a caller could
        // index into.
        assert_eq!(resolve(&[], &names(&["ja"]), None), names(&["en"]));
        assert_eq!(resolve(&[], &[], None), names(&["en"]));
    }

    /// The main bundle leads the search for a bundle that is not the main
    /// bundle, so a framework that has none of the user's languages still
    /// answers with the application's language.
    #[test]
    fn the_main_bundle_leads_for_another_bundle() {
        let framework = names(&["ja"]);
        let main = names(&["fr"]);
        assert_eq!(
            resolve(&framework, &names(&["ja"]), Some(&main)),
            names(&["ja"]),
            "the main bundle's language leads, but only if this bundle has it"
        );
        assert_eq!(
            resolve(&names(&["fr"]), &names(&["ja"]), Some(&main)),
            names(&["fr"])
        );
    }

    /// A language is found under the spelling the bundle uses: the identifier
    /// itself, the full name Apple keeps for seven languages, or the identifier
    /// with its separator the other way round.
    #[test]
    fn a_language_is_found_in_whichever_spelling_the_bundle_uses() {
        assert_eq!(
            super::preference::found_in(&names(&["English"]), "en"),
            Some(String::from("English"))
        );
        assert_eq!(
            super::preference::found_in(&names(&["en"]), "English"),
            Some(String::from("en"))
        );
        assert_eq!(
            super::preference::found_in(&names(&["en_US"]), "en-US"),
            Some(String::from("en_US"))
        );
        // Only those seven have a full name Apple checks for, so a language
        // outside them is not matched by one.
        assert_eq!(
            super::preference::found_in(&names(&["Polish"]), "pl"),
            None
        );
    }

    /// The swap is at the third character and needs four characters to do
    /// anything, because three cannot hold a language and a region.
    #[test]
    fn the_separator_swaps_only_where_a_region_could_be() {
        assert_eq!(separator_swapped("en-US"), Some(String::from("en_US")));
        assert_eq!(separator_swapped("zh_CN"), Some(String::from("zh-CN")));
        assert_eq!(separator_swapped("en-"), None, "too short to have a region");
        assert_eq!(separator_swapped("en"), None, "no separator at all");
        assert_eq!(
            separator_swapped("en-Latn"),
            Some(String::from("en_Latn")),
            "index 2 is the separator even when a script subtag follows it"
        );
        assert_eq!(
            separator_swapped("en-Latn-US").as_deref(),
            Some("en_Latn-US"),
            "index 2 is the language/region boundary however many subtags follow"
        );
    }

    /// The array half of the contract, which the pure tests above cannot see: a
    /// guest's array in, an array of `NSString`s out, in the order the contract
    /// says.
    #[test]
    fn a_guests_localization_array_comes_back_in_that_order() {
        crate::bootstrap();

        /// A `CFArray` of localizations, which the test owns.
        fn array(list: &[&str]) -> objc::object::Id {
            let values: Vec<corefoundation::base::CFTypeRef> = list
                .iter()
                .map(|name| crate::string::from_str(name) as corefoundation::base::CFTypeRef)
                .collect();
            corefoundation::collections::new_array(
                &values,
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            ) as objc::object::Id
        }

        let address = cf::symbol("CFBundleCopyLocalizationsForPreferences")
            .expect("the name has to resolve");
        // SAFETY: the address of `CFBundleCopyLocalizationsForPreferences`,
        // which takes two arrays and returns one the caller owns.
        let for_preferences: unsafe extern "C" fn(
            objc::object::Id,
            objc::object::Id,
        ) -> objc::object::Id = unsafe { std::mem::transmute(address as usize) };

        let answer_to = |candidates: &[&str], preferences: &[&str]| {
            // SAFETY: both arrays are live objects this test just made.
            let answer =
                unsafe { for_preferences(array(candidates), array(preferences)) };
            assert!(!answer.is_null(), "a Copy function hands back a reference");
            // SAFETY: an `NSArray` this runtime made, holding `NSString`s.
            let values = unsafe {
                corefoundation::collections::array(answer as corefoundation::base::CFTypeRef)
            }
            .expect("a localization array")
            .values
            .iter()
            .map(|value| {
                // SAFETY: each value is an `NSString`.
                let bytes = unsafe { crate::string::bytes(*value as objc::object::Id) };
                String::from_utf8(bytes).expect("utf-8")
            })
            .collect::<Vec<_>>();
            // SAFETY: an array this test owns, by the function's own contract.
            unsafe { corefoundation::api::CFRelease(answer) };
            values
        };

        // The candidates in one order and the preferences in another, so a
        // result that merely echoed either input would show.
        assert_eq!(answer_to(&["af", "en", "en_GB", "fr", "fr_CA"], &["fr-FR", "en-US"]), ["fr"]);
        // A preference the candidates have nothing for falls back to `en_US`
        // before the candidates' own first entry.
        assert_eq!(answer_to(&["af", "am", "en_US"], &["ja"]), ["en_US"]);
        // With no `en_US` either, the candidates' first localization - `af`,
        // because `localization_names` sorts rather than leaving the answer to
        // the filesystem.
        assert_eq!(answer_to(&["af", "am", "ar"], &["ja"]), ["af"]);
        // An empty candidate list is `["en"]` and not an empty array, which is
        // `CFBundleCopyLocalizationsForPreferences`' own total backstop.
        assert_eq!(answer_to(&[], &["ja"]), ["en"]);
    }

    /// The selectors are installed on the side of the class they are sent from:
    /// the instance ones on the class, `+preferredLocalizationsForPreferences:`
    /// on the metaclass, each with the encoding the ABI declares. A
    /// `preferredLocalizations` on the metaclass would be a class method no
    /// guest can reach, and a wrong frame size is a caller that reads its
    /// arguments off the stack.
    #[test]
    fn the_localization_selectors_are_installed_where_they_are_sent() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSBundle").expect("NSBundle is registered");
        for (name, types) in [
            ("localizations", c"@16@0:8"),
            ("preferredLocalizations", c"@16@0:8"),
            ("developmentLocalization", c"@16@0:8"),
        ] {
            let method = class
                .method(objc::sel::get(name))
                .unwrap_or_else(|| panic!("-[NSBundle {name}] is not installed"));
            // `Method::types` answers a raw pointer; the encoding is the text
            // it names, which is what the ABI is checked against.
            // SAFETY: a registered method's types are a NUL-terminated string.
            let text = unsafe { std::ffi::CStr::from_ptr(method.types()) };
            assert_eq!(text, types, "-[{name}]");
        }
        let class_method = class
            .metaclass()
            .method(objc::sel::get("preferredLocalizationsForPreferences:"))
            .expect("+[NSBundle preferredLocalizationsForPreferences:] is not installed");
        // SAFETY: a registered method's types are a NUL-terminated string.
        let class_types = unsafe { std::ffi::CStr::from_ptr(class_method.types()) };
        assert_eq!(class_types, c"@24@0:8@16");
        // And not on the other side, where a guest would not find them: the
        // selector `047.log` names is sent to an *instance*.
        assert!(
            class
                .metaclass()
                .method(objc::sel::get("preferredLocalizations"))
                .is_none(),
            "preferredLocalizations is an instance method"
        );
    }

    /// The four-argument resource lookup, which is the one selector with five
    /// words in it, so the encoding is the whole ABI: a frame sized for four
    /// arguments would read `r9` off the guest's stack.
    ///
    /// The selector string is spelled out here rather than shared with the
    /// `install` table, because a spelling that is wrong in one place is wrong
    /// in the other and a test that quotes the same literal proves nothing. This
    /// one is the spelling, and the image's own `__objc_methname` for it is
    /// `pathForResource:ofType:inDirectory:forLocalization:` - 51 bytes, verified
    /// against the Mach-O rather than against a header, and the only reference
    /// to it in the whole 214 MB of `__TEXT,__text` is the one call site at
    /// `+0x692c008`.
    #[test]
    fn the_four_argument_resource_lookup_is_an_instance_method_of_five_words() {
        crate::bootstrap();
        const SELECTOR: &str = "pathForResource:ofType:inDirectory:forLocalization:";
        const TYPES: &std::ffi::CStr = c"@48@0:8@16@24@32@40";
        let class = objc::class::lookup(c"NSBundle").expect("NSBundle is registered");
        let method = class
            .method(objc::sel::get(SELECTOR))
            .unwrap_or_else(|| panic!("-[NSBundle {SELECTOR}] is not installed"));
        // SAFETY: a registered method's types are a NUL-terminated string.
        let text = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        assert_eq!(text, TYPES);
        // An instance method, which is where `GetResourcesPakFilePath` sends it:
        // the receiver is a bundle, not a class.
        assert!(
            class.metaclass().method(objc::sel::get(SELECTOR)).is_none(),
            "{SELECTOR} is an instance method"
        );
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
        let Ok(path) = std::str::from_utf8(&path) else {
            return std::ptr::null_mut();
        };
        // SAFETY: `class` is the receiver this method was sent to.
        unsafe { bundle_at(class, path) }
    }

    /// `+bundleWithURL:`, autoreleased: `+bundleWithPath:` for a `file:` URL,
    /// and nil for any other, as Apple's is.
    unsafe extern "C" fn bundle_with_url(class: Id, _sel: Sel, url: Id) -> Id {
        // SAFETY: an `NSURL`, or nil.
        let url = unsafe { corefoundation::url::as_url(url as corefoundation::base::CFTypeRef) };
        let Some(path) = url.and_then(|url| url.file_path()) else {
            return std::ptr::null_mut();
        };
        // SAFETY: `class` is the receiver this method was sent to.
        unsafe { bundle_at(class, &path) }
    }

    /// The bundle at a guest path, autoreleased, or nil where there is none.
    ///
    /// # Safety
    ///
    /// `class` is `NSBundle` or nil.
    unsafe fn bundle_at(class: Id, path: &str) -> Id {
        // The guest named it, so it is opened at this crate's spelling.
        let path = super::opened(std::path::Path::new(path));
        if super::main().is_some_and(|main| main.root == path) {
            return main_object();
        }
        let Some(layout) = super::Layout::at(&path) else {
            return std::ptr::null_mut();
        };
        // SAFETY: `class` is `NSBundle`, which this crate registered.
        let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) })
        else {
            return std::ptr::null_mut();
        };
        let object = objc::object::create_instance(class, 0);
        // Leaked rather than owned by the object: a bundle is interned by Apple's
        // Foundation too, and a `-dealloc` freeing it would leave every string
        // this runtime handed out from it pointing at nothing.
        // SAFETY: a live instance whose second word is ours to fill in.
        unsafe { (*(object as *mut Body)).layout = Box::leak(Box::new(layout)) };
        objc::arc::autorelease(object)
    }

    /// The layout a bundle object answers for: its own, or the main one.
    ///
    /// # Safety
    ///
    /// `receiver` must be an `NSBundle` this module made.
    unsafe fn layout_of(receiver: Id) -> Option<&'static super::Layout> {
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

    /// Whether this object *is* the main bundle, which is a question about the
    /// object rather than about the directory it answers for, and which
    /// `-[NSBundle preferredLocalizations]` needs.
    ///
    /// A null second word is the main bundle's own mark: `of` creates the
    /// singleton without a layout so that it keeps answering through
    /// `super::main()`, and `bundle_at` hands that same object out for the main
    /// root.
    ///
    /// # Safety
    ///
    /// As [`layout_of`].
    unsafe fn is_main(receiver: Id) -> bool {
        !receiver.is_null()
            // SAFETY: every `NSBundle` is a `Body`.
            && unsafe { (*(receiver as *const Body)).layout }.is_null()
    }

    /// The main bundle, for a caller that has no class object in hand.
    pub(crate) fn main_object() -> Id {
        match objc::class::lookup(c"NSBundle") {
            Some(class) => of(class),
            None => std::ptr::null_mut(),
        }
    }

    /// Localizations as an autoreleased `NSArray` of `NSString`, which is what
    /// a method hands back and what a `Copy` function does not.
    fn autoreleased_localizations(names: &[String]) -> Id {
        // SAFETY: an array this call just made.
        unsafe { corefoundation::api::CFAutorelease(super::string_array(names)) as Id }
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
            Some(layout) => autoreleased(&layout.localized_string(&key, value.as_deref(), table.as_deref())),
            None => autoreleased(value.as_deref().filter(|v| !v.is_empty()).unwrap_or(&key)),
        }
    }

    /// `CFBundleCopyLocalizedString`, the same lookup at +1.
    pub(crate) unsafe fn copy_localized_string(bundle: Id, key: Id, value: Id, table: Id) -> Id {
        // SAFETY: forwarded; the answer is autoreleased and this spelling owns it.
        objc::arc::retain(unsafe {
            localized_string_for_key(bundle, objc::sel::get("localizedStringForKey:value:table:"), key, value, table)
        })
    }

    fn autoreleased(text: &str) -> Id {
        let string = crate::string::from_str(text);
        // SAFETY: a string this call just created.
        unsafe {
            corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id
        }
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

    /// `-pathForResource:ofType:inDirectory:forLocalization:`, which is the
    /// only form of this question that names a localization, and the only
    /// reason the guest reaches for four arguments instead of two:
    /// `GetResourcesPakFilePath` in `ui/base/resource/resource_bundle_mac.mm`
    /// sends it to fetch *this* helper process's locale pak rather than
    /// whatever `NSBundle` would pick for itself.
    unsafe extern "C" fn path_for_resource_localized(
        receiver: Id,
        _sel: Sel,
        name: Id,
        kind: Id,
        directory: Id,
        localization: Id,
    ) -> Id {
        // Whether the localization was nil at all, which `bytes` cannot report:
        // it answers `""` for nil and for an empty string alike. Taken before
        // the argument is shadowed by its own bytes.
        let absent = localization.is_null();
        // SAFETY: each argument is an `NSString` or nil.
        let (name, kind, directory, localization) = unsafe {
            (
                crate::string::bytes(name),
                crate::string::bytes(kind),
                crate::string::bytes(directory),
                crate::string::bytes(localization),
            )
        };
        let (Ok(name), Ok(kind), Ok(directory), Ok(localization)) = (
            std::str::from_utf8(&name),
            std::str::from_utf8(&kind),
            std::str::from_utf8(&directory),
            std::str::from_utf8(&localization),
        ) else {
            return std::ptr::null_mut();
        };
        // A nil localization and an empty one are different questions, and only
        // one of them is this method's. Nil is the shorter forms' question - "the
        // bundle's own order" - so it is routed back to them; see
        // `Layout::resource_in_localized` for why the empty one is then taken
        // literally.
        let localization = match absent {
            true => None,
            false => Some(localization),
        };
        // SAFETY: the receiver is an `NSBundle`.
        path(unsafe { layout_of(receiver) }
            .and_then(|b| b.resource_in_localized(name, kind, directory, localization)))
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

    /// `-[NSBundle localizations]`, which is
    /// `CFBundleCopyBundleLocalizations` for the same bundle.
    ///
    /// The guest reads this one too, and reads it by fast enumeration rather
    /// than by index: `Chromium Framework+0x2840` sends `localizations` to the
    /// main bundle and then enumerates it, splitting each name on `-` and
    /// collecting the pieces, so the answer has to be a real `NSArray` of real
    /// `NSString`s rather than something that merely counts.
    unsafe extern "C" fn localizations(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: the receiver is an `NSBundle`.
        let names = match unsafe { layout_of(receiver) } {
            Some(bundle) => bundle.localization_names(),
            None => Vec::new(),
        };
        autoreleased_localizations(&names)
    }

    /// `-[NSBundle preferredLocalizations]`, the selector this guest is
    /// stopped on, which `047.log` names:
    /// `unrecognized selector sent to instance of NSBundle:
    /// preferredLocalizations`.
    ///
    /// `Chromium Framework+0x692c821` sends it to the main bundle, keeps the
    /// first element, and rewrites the `_` in it to a `-` - so what matters
    /// here is both the order and the spelling of element zero.
    unsafe extern "C" fn preferred_localizations(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: the receiver is an `NSBundle`.
        let Some(bundle) = (unsafe { layout_of(receiver) }) else {
            return std::ptr::null_mut();
        };
        let supported = bundle.localization_names();
        let user = super::preference::user_languages();
        // `_CFBundleCopyPreferredLanguagesInList` lets the main bundle's own
        // preferred language lead the search - but only for a bundle that is
        // not the main bundle, and the main bundle is the only bundle there
        // is in this runtime unless a guest has asked for another.
        let main = match unsafe { is_main(receiver) } {
            true => None,
            false => super::main().map(|main| {
                let names = main.localization_names();
                super::preference::resolve(&names, &user, None)
            }),
        };
        autoreleased_localizations(&super::preference::resolve(
            &supported,
            &user,
            main.as_deref(),
        ))
    }

    /// `+[NSBundle preferredLocalizationsForPreferences:]`, the class-level
    /// twin: the candidates come from the argument and the order from the same
    /// [`super::preference`] contract, minus the main bundle - Apple's own
    /// comment is that this is the function which answers "what would *that*
    /// application use", so it must not consult this application's own.
    unsafe extern "C" fn preferred_localizations_for_preferences(
        _class: Id,
        _sel: Sel,
        candidates: Id,
    ) -> Id {
        // SAFETY: an `NSArray` or nil.
        let candidates = unsafe { super::strings_of(candidates) };
        let user = super::preference::user_languages();
        autoreleased_localizations(&super::preference::resolve(&candidates, &user, None))
    }

    /// `-[NSBundle developmentLocalization]`, which is the
    /// `CFBundleDevelopmentRegion` the plist names: the language this bundle
    /// was written in, as opposed to any of the ones it has translations for.
    unsafe extern "C" fn development_localization(receiver: Id, _sel: Sel) -> Id {
        // SAFETY: the receiver is an `NSBundle`.
        unsafe { info(receiver, "CFBundleDevelopmentRegion") }
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
            "pathForResource:ofType:inDirectory:forLocalization:" => path_for_resource_localized, c"@48@0:8@16@24@32@40";
            "infoDictionary" => info_dictionary, c"@16@0:8";
            "executableName" => executable_name, c"*16@0:8";
            "localizedStringForKey:value:table:" => localized_string_for_key, c"@40@0:8@16@24@32";
            "localizations" => localizations, c"@16@0:8";
            "preferredLocalizations" => preferred_localizations, c"@16@0:8";
            "developmentLocalization" => development_localization, c"@16@0:8"
        });
        class_methods!(class, {
            "mainBundle" => main_bundle, c"@16@0:8";
            "bundleWithPath:" => bundle_with_path, c"@24@0:8@16";
            "bundleWithURL:" => bundle_with_url, c"@24@0:8@16";
            "preferredLocalizationsForPreferences:" => preferred_localizations_for_preferences, c"@24@0:8@16"
        });
    }
}
