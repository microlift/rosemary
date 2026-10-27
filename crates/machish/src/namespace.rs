use alloc::string::String;
#[cfg(any(feature = "std", test))]
use alloc::string::ToString;
use alloc::vec::Vec;

use crate::error::Result;
use crate::exports::{ExportTable, ExportValue};
use crate::fixups::{LibraryRef, SymbolRef};
use crate::image::DependencyKind;
use crate::raw::Word;
use crate::resolve::{Resolved, SymbolKind, SymbolProvider};

/// How many re-export hops to follow before declaring the graph malformed.
///
/// System umbrellas chain two or three deep; anything approaching this is a
/// cycle, and a cycle here is an infinite loop rather than a wrong answer.
const MAX_REEXPORT_DEPTH: usize = 16;

/// A request for one dependency's bytes.
#[derive(Copy, Clone, Debug)]
pub struct DylibRequest<'a> {
    /// The install name exactly as the image records it, `@rpath` and all.
    /// Match on this when a source keys off names rather than paths.
    pub install_name: &'a str,
    /// Concrete paths to try, in search order, already expanded by
    /// [`crate::install_name::expand`].
    pub candidates: &'a [String],
    pub kind: DependencyKind,
    /// Path of the image recording the dependency, for diagnostics and for a
    /// source that wants to do its own relative resolution.
    pub loader_path: Option<&'a str>,
}

impl DylibRequest<'_> {
    /// Whether this dependency's absence is tolerable.
    pub fn is_weak(&self) -> bool {
        self.kind.is_weak()
    }
}

/// A dependency's bytes and where they came from.
#[derive(Clone, Debug)]
pub struct FetchedDylib {
    /// The path actually opened, which becomes the image's `@loader_path`.
    pub path: String,
    pub bytes: Vec<u8>,
    /// Install names to load as though this image had listed them as
    /// dependencies. A substitute library's load commands describe the graph
    /// its own build had; a source standing in for a system library can name
    /// here what the real one would have pulled in.
    pub implied: Vec<String>,
}

/// Something that can produce a dependency's bytes.
///
/// Returning `Ok(None)` means "not mine, and that is fine" - the import falls
/// through to the provider chain. Reserve `Err` for a source that found the
/// library and could not read it, which is a real failure.
pub trait DylibSource {
    fn fetch(&mut self, request: &DylibRequest<'_>) -> Result<Option<FetchedDylib>>;
}

/// A source that finds nothing. Every import then falls to the providers, which
/// is the whole-process configuration for a purely high-level-emulated guest.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoDylibSource;

impl DylibSource for NoDylibSource {
    fn fetch(&mut self, _request: &DylibRequest<'_>) -> Result<Option<FetchedDylib>> {
        Ok(None)
    }
}

/// Reads dependencies from the filesystem, declining OS paths.
#[cfg(feature = "std")]
#[derive(Clone, Debug, Default)]
pub struct FsDylibSource {
    /// Directories prepended to every search, for an embedder that keeps its
    /// guest's libraries somewhere the image does not know about.
    extra_search_paths: Vec<String>,
}

#[cfg(feature = "std")]
impl FsDylibSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Also look for a dependency's basename in `dir`.
    pub fn search_path(&mut self, dir: &str) -> &mut Self {
        self.extra_search_paths.push(dir.to_string());
        self
    }
}

#[cfg(feature = "std")]
impl DylibSource for FsDylibSource {
    fn fetch(&mut self, request: &DylibRequest<'_>) -> Result<Option<FetchedDylib>> {
        use crate::install_name::{is_system_path, join};

        // Checked against the recorded install name, not the expanded
        // candidates, because that is what the image actually asked for.
        if is_system_path(request.install_name) {
            return Ok(None);
        }

        for candidate in request.candidates {
            if is_system_path(candidate) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(candidate) {
                return Ok(Some(FetchedDylib {
                    path: candidate.clone(),
                    bytes,
                    implied: Vec::new(),
                }));
            }
        }

        let base = request
            .install_name
            .rsplit('/')
            .next()
            .unwrap_or(request.install_name);
        for dir in &self.extra_search_paths {
            let path = join(dir, base);
            if let Ok(bytes) = std::fs::read(&path) {
                return Ok(Some(FetchedDylib {
                    path,
                    bytes,
                    implied: Vec::new(),
                }));
            }
        }
        Ok(None)
    }
}

/// One image the [`Namespace`] can resolve against.
#[derive(Clone, Debug)]
pub struct NamespaceImage {
    /// The image's `LC_ID_DYLIB` name, which is what dependents' bind ordinals
    /// resolve to. Absent for an executable.
    pub install_name: Option<String>,
    /// The path it was loaded from, which also satisfies a dependency recorded
    /// by path rather than by install name.
    pub path: String,
    /// The names dependents recorded for it, which a compatibility name lets
    /// differ from both: a dependency on `/usr/lib/libstdc++.6.dylib` answered
    /// by an image elsewhere must still satisfy that dependency's binds.
    pub aliases: Vec<String>,
    pub exports: ExportTable,
    pub slide: Word,
    /// This image's own dependency install names, in ordinal order, so a
    /// re-export's ordinal can be turned back into a name.
    pub dependencies: Vec<String>,
    /// The subset of `dependencies` recorded with `LC_REEXPORT_DYLIB`, whose
    /// exports this image republishes as its own.
    pub reexports: Vec<String>,
}

impl NamespaceImage {
    /// Whether `name` refers to this image.
    ///
    /// Matched on install name, path, *or* any name it was requested as: an
    /// image may be depended on by the `@rpath` name it records and found at a
    /// concrete path, and a dependent linked against either must be satisfied.
    pub fn answers_to(&self, name: &str) -> bool {
        self.install_name.as_deref() == Some(name)
            || self.path == name
            || self.aliases.iter().any(|a| a == name)
    }
}

/// Every image loaded together, as a two-level symbol provider.
#[derive(Clone, Debug, Default)]
pub struct Namespace {
    images: Vec<NamespaceImage>,
}

impl Namespace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, image: NamespaceImage) -> &mut Self {
        self.images.push(image);
        self
    }

    pub fn images(&self) -> &[NamespaceImage] {
        &self.images
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    fn find(&self, name: &str) -> Option<&NamespaceImage> {
        self.images.iter().find(|i| i.answers_to(name))
    }

    /// Look `name` up in `image`, following re-export edges into the
    /// dependencies it forwards to.
    ///
    /// Following them is not optional. A system umbrella defines almost nothing
    /// itself, and an image that binds against one gets a table of forwarding
    /// edges - treating a re-export as a definition would bind every such
    /// import to address zero and report success.
    fn lookup(&self, image: &NamespaceImage, name: &str, depth: usize) -> Option<Resolved> {
        if depth > MAX_REEXPORT_DEPTH {
            return None;
        }
        let Some(export) = image.exports.get(name) else {
            // Not in this image's trie, but an `LC_REEXPORT_DYLIB` dependency
            // may still define it: a re-export edge recorded as a load command
            // publishes the whole of the other image's trie, with nothing
            // written into this one.
            return image
                .reexports
                .iter()
                .filter_map(|target| self.find(target))
                .find_map(|next| self.lookup(next, name, depth + 1));
        };

        if let ExportValue::Reexport {
            ordinal,
            imported_name,
        } = &export.value
        {
            let target = image
                .dependencies
                .get(usize::from(*ordinal).checked_sub(1)?)?;
            let next = self.find(target)?;
            let name = imported_name.as_deref().unwrap_or(name);
            return self.lookup(next, name, depth + 1);
        }

        Some(Resolved {
            addr: export.address(image.slide)?,
            kind: match export.kind {
                crate::exports::ExportKind::ThreadLocal => SymbolKind::ThreadLocal,
                _ => SymbolKind::Function,
            },
            tag: 0,
        })
    }
}

impl SymbolProvider for Namespace {
    fn provides(&self, install_name: &str) -> bool {
        self.find(install_name).is_some()
    }

    fn resolve(&self, sym: &SymbolRef<'_>) -> Option<Resolved> {
        match sym.library {
            LibraryRef::InstallName(name) => self.lookup(self.find(name)?, sym.name, 0),
            // A flat or unknown-ordinal reference searches every loaded image
            // in load order, which is dyld's flat-namespace rule.
            _ => self.images.iter().find_map(|i| self.lookup(i, sym.name, 0)),
        }
    }

    fn enumerate(&self, out: &mut dyn FnMut(&str, Resolved)) {
        for image in &self.images {
            for e in image.exports.iter() {
                if let Some(addr) = e.address(image.slide) {
                    out(
                        &e.name,
                        Resolved {
                            addr,
                            kind: SymbolKind::Function,
                            tag: 0,
                        },
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exports::{Export, ExportKind};
    use alloc::vec;

    fn direct(name: &str, addr: Word) -> Export {
        Export {
            name: name.to_string(),
            flags: 0,
            kind: ExportKind::Regular,
            vmaddr: addr,
            value: ExportValue::Direct,
        }
    }

    fn reexport(name: &str, ordinal: u16, renamed: Option<&str>) -> Export {
        Export {
            name: name.to_string(),
            flags: crate::spec::constants::EXPORT_SYMBOL_FLAGS_REEXPORT,
            kind: ExportKind::Regular,
            vmaddr: 0,
            value: ExportValue::Reexport {
                ordinal,
                imported_name: renamed.map(|s| s.to_string()),
            },
        }
    }

    fn image(install: &str, exports: Vec<Export>, deps: &[&str], slide: Word) -> NamespaceImage {
        NamespaceImage {
            install_name: Some(install.to_string()),
            path: install.to_string(),
            aliases: Vec::new(),
            exports: ExportTable::from_entries(exports),
            slide,
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
            reexports: Vec::new(),
        }
    }

    fn sym<'a>(name: &'a str, library: LibraryRef<'a>) -> SymbolRef<'a> {
        SymbolRef {
            name,
            library,
            weak_import: false,
        }
    }

    #[test]
    fn a_loaded_image_satisfies_a_two_level_import_and_the_slide_is_applied() {
        let mut ns = Namespace::new();
        ns.push(image(
            "libmath.dylib",
            vec![direct("_add", 0x1000)],
            &[],
            0x4000,
        ));

        let r = ns.resolve(&sym("_add", LibraryRef::InstallName("libmath.dylib")));
        assert_eq!(r.map(|r| r.addr), Some(0x5000));
        assert!(ns.provides("libmath.dylib"));
        assert!(!ns.provides("libother.dylib"));
    }

    #[test]
    fn the_wrong_library_is_not_satisfied() {
        let mut ns = Namespace::new();
        ns.push(image("libmath.dylib", vec![direct("_add", 0x1000)], &[], 0));
        assert_eq!(
            ns.resolve(&sym("_add", LibraryRef::InstallName("libother.dylib"))),
            None
        );
        // ...but a flat reference finds it, which is the flat-namespace rule.
        assert!(ns.resolve(&sym("_add", LibraryRef::Flat)).is_some());
    }

    #[test]
    fn a_reexport_forwards_to_the_defining_image() {
        let mut ns = Namespace::new();
        ns.push(image(
            "libumbrella.dylib",
            vec![reexport("_add", 1, None)],
            &["libmath.dylib"],
            0,
        ));
        ns.push(image(
            "libmath.dylib",
            vec![direct("_add", 0x1000)],
            &[],
            0x2000,
        ));

        let r = ns.resolve(&sym("_add", LibraryRef::InstallName("libumbrella.dylib")));
        assert_eq!(
            r.map(|r| r.addr),
            Some(0x3000),
            "the umbrella must forward, not answer zero"
        );
    }

    #[test]
    fn a_renaming_reexport_looks_up_the_other_name() {
        let mut ns = Namespace::new();
        ns.push(image(
            "libumbrella.dylib",
            vec![reexport("_add", 1, Some("_internal_add"))],
            &["libmath.dylib"],
            0,
        ));
        ns.push(image(
            "libmath.dylib",
            vec![direct("_internal_add", 0x1000)],
            &[],
            0,
        ));

        let r = ns.resolve(&sym("_add", LibraryRef::InstallName("libumbrella.dylib")));
        assert_eq!(r.map(|r| r.addr), Some(0x1000));
        // The original name must not also be findable in the target.
        assert_eq!(
            ns.resolve(&sym("_add", LibraryRef::InstallName("libmath.dylib"))),
            None
        );
    }

    #[test]
    fn a_reexport_cycle_terminates() {
        let mut ns = Namespace::new();
        ns.push(image(
            "a.dylib",
            vec![reexport("_x", 1, None)],
            &["b.dylib"],
            0,
        ));
        ns.push(image(
            "b.dylib",
            vec![reexport("_x", 1, None)],
            &["a.dylib"],
            0,
        ));
        assert_eq!(
            ns.resolve(&sym("_x", LibraryRef::InstallName("a.dylib"))),
            None
        );
    }

    #[test]
    fn a_dangling_reexport_is_not_an_address() {
        let mut ns = Namespace::new();
        ns.push(image(
            "a.dylib",
            vec![reexport("_x", 1, None)],
            &["missing.dylib"],
            0,
        ));
        assert_eq!(
            ns.resolve(&sym("_x", LibraryRef::InstallName("a.dylib"))),
            None
        );
        // Specifically: not `Some(0)`.
    }

    #[test]
    fn an_image_answers_to_its_path_as_well_as_its_install_name() {
        let mut ns = Namespace::new();
        ns.push(NamespaceImage {
            install_name: Some("@rpath/libmath.dylib".to_string()),
            path: "/tmp/build/libmath.dylib".to_string(),
            aliases: Vec::new(),
            exports: ExportTable::from_entries(vec![direct("_add", 0x10)]),
            slide: 0,
            dependencies: Vec::new(),
            reexports: Vec::new(),
        });
        assert!(ns.provides("@rpath/libmath.dylib"));
        assert!(ns.provides("/tmp/build/libmath.dylib"));
    }

    #[test]
    fn an_image_answers_to_the_names_it_was_requested_as() {
        let mut ns = Namespace::new();
        ns.push(NamespaceImage {
            install_name: Some("/compat/usr/lib/libstdc++.6.dylib".to_string()),
            path: "/compat/usr/lib/libstdc++.6.dylib".to_string(),
            aliases: vec![
                "/usr/lib/libstdc++.6.dylib".to_string(),
                "/usr/lib/libstdc++.dylib".to_string(),
            ],
            exports: ExportTable::from_entries(vec![direct("_add", 0x10)]),
            slide: 0,
            dependencies: Vec::new(),
            reexports: Vec::new(),
        });
        assert!(ns.provides("/usr/lib/libstdc++.6.dylib"));
        assert!(ns.provides("/usr/lib/libstdc++.dylib"));
        assert!(!ns.provides("/usr/lib/libc++.1.dylib"));
    }
}
