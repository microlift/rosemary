use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::exports::ExportTable;
use crate::extensions::ExtensionSet;
use crate::fixups::{Binder, StubMap, decode_image};
use crate::image::{DependencyKind, MachObject, map_segments, protect_segments};
use crate::install_name::{PathContext, dirname, expand};
use crate::memory::{AddressSpace, ProtectionPolicy};
use crate::namespace::{DylibRequest, DylibSource, Namespace, NamespaceImage, NoDylibSource};
use crate::report::{ImageReport, LinkReport, SegmentReport};
use crate::resolve::SymbolProvider;
use crate::spec::MachSpec;

/// Guard against a dependency graph that is a bomb rather than a graph.
const MAX_IMAGES: usize = 512;

/// What to do about the things a link can get partway through.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LoadPolicy {
    /// How to reconcile segment protection with the host's page size.
    pub protection: ProtectionPolicy,
    /// Whether an unresolved non-weak import fails the link.
    ///
    /// `false` by default, which is the useful setting while a shim table is
    /// being built: the [`LinkReport`] then lists *every* missing symbol at
    /// once instead of naming whichever was decoded first. Call
    /// [`LinkReport::require_all_resolved`] to get the strict answer whenever
    /// you want it.
    pub require_all_symbols: bool,
    /// Whether a missing non-weak *dependency* fails the link. Off by default
    /// for the same reason `/usr/lib` is declined rather than opened: a
    /// dependency this crate cannot fetch is usually one a provider will
    /// satisfy symbol by symbol.
    pub require_all_dependencies: bool,
    /// Whether to follow dependencies at all.
    pub load_dependencies: bool,
}

impl Default for LoadPolicy {
    fn default() -> Self {
        LoadPolicy {
            protection: ProtectionPolicy::default(),
            require_all_symbols: false,
            require_all_dependencies: false,
            load_dependencies: true,
        }
    }
}

/// Loads and links Mach-O images of one architecture.
pub struct Loader<S: MachSpec> {
    spec: S,
    policy: LoadPolicy,
    extensions: ExtensionSet<S>,
}

/// An image's bytes plus where they came from, owned for the link's duration.
struct Collected {
    path: String,
    bytes: Vec<u8>,
    /// Install name recorded by the *dependent*, which is how bind ordinals
    /// name it and is not always the same as its own `LC_ID_DYLIB`.
    requested_as: Option<String>,
    /// `LC_RPATH` entries in scope for this image: its own, then its loader's.
    rpaths: Vec<String>,
    /// Dependencies the source added, which are not in the load commands.
    implied: Vec<String>,
}

/// What phase 2 produced for one image.
struct Mapped<'a, S: MachSpec> {
    obj: MachObject<'a, S>,
    path: String,
    exports: ExportTable,
    base: crate::raw::Word,
    slide: crate::raw::Word,
    entry: Option<crate::raw::Word>,
    entry_command: Option<crate::image::EntryPoint>,
}

impl<S: MachSpec + 'static> Loader<S> {
    /// A loader with this build's default extensions, which report
    /// initializers, terminators, and thread-local descriptors.
    pub fn new(spec: S) -> Self {
        Loader {
            spec,
            policy: LoadPolicy::default(),
            extensions: ExtensionSet::defaults(),
        }
    }

    pub fn with_policy(spec: S, policy: LoadPolicy) -> Self {
        Loader {
            spec,
            policy,
            extensions: ExtensionSet::defaults(),
        }
    }

    /// A loader that runs no extensions at all.
    pub fn bare(spec: S) -> Self {
        Loader {
            spec,
            policy: LoadPolicy::default(),
            extensions: ExtensionSet::empty(),
        }
    }

    /// The extension set, to add to or replace.
    pub fn extensions_mut(&mut self) -> &mut ExtensionSet<S> {
        &mut self.extensions
    }

    pub fn policy(&self) -> LoadPolicy {
        self.policy
    }

    pub fn set_policy(&mut self, policy: LoadPolicy) -> &mut Self {
        self.policy = policy;
        self
    }

    /// Link `root` alone, with no dependency loading.
    ///
    /// Every import then falls to `providers`, which is the configuration a
    /// fully high-level-emulated guest wants: nothing but the guest's own
    /// executable is ever mapped, and `/usr/lib` is answered natively.
    pub fn link<M: AddressSpace>(
        &self,
        root: &[u8],
        root_path: &str,
        mem: &mut M,
        providers: &[&dyn SymbolProvider],
    ) -> Result<LinkReport> {
        self.link_with(root, root_path, mem, &mut NoDylibSource, providers)
    }

    /// Link `root` and everything it depends on that `source` can supply.
    pub fn link_with<M: AddressSpace, D: DylibSource>(
        &self,
        root: &[u8],
        root_path: &str,
        mem: &mut M,
        source: &mut D,
        providers: &[&dyn SymbolProvider],
    ) -> Result<LinkReport> {
        let collected = self.collect(root, root_path, source, providers)?;
        let mapped = self.map_all(&collected, mem)?;
        self.bind_all(&collected, &mapped, mem, providers)
    }

    /// Walk the dependency graph breadth-first, fetching bytes.
    ///
    /// Breadth-first with a visited set, rather than dyld's two-phase handling
    /// of `LC_LOAD_UPWARD_DYLIB`. The difference is observable only in
    /// initializer ordering around a deliberate cycle, which this crate does
    /// not run anyway; see the crate scope notes.
    fn collect<D: DylibSource>(
        &self,
        root: &[u8],
        root_path: &str,
        source: &mut D,
        providers: &[&dyn SymbolProvider],
    ) -> Result<Vec<Collected>> {
        let root_obj = MachObject::parse_any(root, self.spec)?;
        root_obj.check_not_encrypted()?;

        let mut images = alloc::vec![Collected {
            path: root_path.to_string(),
            // `parse_any` may have narrowed a fat file; store the thin slice so
            // every later phase sees the same bytes at the same offsets.
            bytes: root_obj.bytes().to_vec(),
            requested_as: None,
            rpaths: root_obj.rpaths().map(|r| r.to_string()).collect(),
            implied: Vec::new(),
        }];
        if !self.policy.load_dependencies {
            return Ok(images);
        }

        let mut visited: Vec<String> = alloc::vec![root_path.to_string()];
        let mut next = 0;

        while next < images.len() {
            if images.len() > MAX_IMAGES {
                return Err(Error::TooDeep("dependency graph"));
            }
            // Re-parse rather than hold a borrow: `images` is still growing, so
            // any `MachObject` borrowed from it would be invalidated by the
            // next push.
            let (deps, loader_path, rpaths) = {
                let img = &images[next];
                let obj = MachObject::parse(&img.bytes, self.spec)?;
                let mut deps: Vec<(String, DependencyKind)> = obj
                    .dependencies()
                    .iter()
                    .map(|d| (d.install_name.to_string(), d.kind))
                    .collect();
                deps.extend(
                    img.implied
                        .iter()
                        .map(|n| (n.clone(), DependencyKind::Load)),
                );
                (deps, img.path.clone(), img.rpaths.clone())
            };
            next += 1;

            for (install_name, kind) in deps {
                let ctx = PathContext {
                    executable: Some(root_path),
                    loader: Some(&loader_path),
                    rpaths: &rpaths,
                };
                let candidates = expand(&install_name, &ctx);

                if visited.iter().any(|v| v == &install_name) {
                    continue;
                }
                // A library a provider stands in for is already loaded, as far
                // as its dependents can tell; fetching the real one as well
                // would put two of it in the namespace, and the image's would win.
                if providers.iter().any(|p| p.stands_for(&install_name)) {
                    continue;
                }
                let request = DylibRequest {
                    install_name: &install_name,
                    candidates: &candidates,
                    kind,
                    loader_path: Some(&loader_path),
                };
                let Some(fetched) = source.fetch(&request)? else {
                    if self.policy.require_all_dependencies && kind != DependencyKind::Weak {
                        return Err(Error::Custom("required dependency not found"));
                    }
                    // Not an error: the import falls through to the providers,
                    // which for `/usr/lib` is the intended path rather than a
                    // fallback.
                    continue;
                };
                if visited.iter().any(|v| v == &fetched.path) {
                    continue;
                }
                visited.push(install_name.clone());
                visited.push(fetched.path.clone());

                let obj = MachObject::parse_any(&fetched.bytes, self.spec)?;
                obj.check_not_encrypted()?;
                // The dependency's own rpaths come first, then those inherited
                // from the image loading it - dyld's search order.
                let mut inherited: Vec<String> = obj.rpaths().map(|r| r.to_string()).collect();
                inherited.extend(rpaths.iter().cloned());

                images.push(Collected {
                    path: fetched.path,
                    bytes: obj.bytes().to_vec(),
                    requested_as: Some(install_name),
                    rpaths: inherited,
                    implied: fetched.implied,
                });
            }
        }
        Ok(images)
    }

    /// Parse and map every collected image.
    fn map_all<'a, M: AddressSpace>(
        &self,
        collected: &'a [Collected],
        mem: &mut M,
    ) -> Result<Vec<Mapped<'a, S>>> {
        let mut out = Vec::with_capacity(collected.len());
        for c in collected {
            let obj = MachObject::parse(&c.bytes, self.spec)?;
            let mapped = map_segments(&obj, mem)?;
            let exports = ExportTable::parse(&obj)?;
            out.push(Mapped {
                obj,
                path: c.path.clone(),
                exports,
                base: mapped.base,
                slide: mapped.slide,
                entry: mapped.entry,
                entry_command: mapped.entry_command,
            });
        }
        Ok(out)
    }

    fn bind_all<M: AddressSpace>(
        &self,
        collected: &[Collected],
        mapped: &[Mapped<'_, S>],
        mem: &mut M,
        providers: &[&dyn SymbolProvider],
    ) -> Result<LinkReport> {
        let mut namespace = Namespace::new();
        for (c, m) in collected.iter().zip(mapped) {
            namespace.push(NamespaceImage {
                // Prefer the image's own `LC_ID_DYLIB`, but fall back to the
                // name its dependent used.
                install_name: m
                    .obj
                    .install_name()
                    .map(|n| n.to_string())
                    .or_else(|| c.requested_as.clone()),
                path: m.path.clone(),
                exports: m.exports.clone(),
                slide: m.slide,
                dependencies: m
                    .obj
                    .dependencies()
                    .iter()
                    .map(|d| d.install_name.to_string())
                    .collect(),
                reexports: m
                    .obj
                    .dependencies()
                    .iter()
                    .filter(|d| d.kind == DependencyKind::Reexport)
                    .map(|d| d.install_name.to_string())
                    .collect(),
            });
        }

        // Loaded images come first, so a bundled dylib beats the host's copy of
        // the same name.
        let mut chain: Vec<&dyn SymbolProvider> = alloc::vec![&namespace];
        chain.extend_from_slice(providers);

        // The main executable is `mapped[0]` by construction, and is what
        // `BIND_SPECIAL_DYLIB_MAIN_EXECUTABLE` refers to.
        let main = mapped.first().map(|m| (&m.exports, m.slide));

        let mut report = LinkReport::default();
        for m in mapped {
            let (encoding, mut fixups) = decode_image(&m.obj)?;
            // Extensions see the decoded stream before anything is resolved,
            // which is the only point at which rewriting an import is still
            // cheap and still affects what gets written.
            self.extensions.transform_fixups(&m.obj, &mut fixups)?;
            let stubs = StubMap::build(&m.obj)?;

            let outcome = Binder {
                slide: m.slide,
                providers: &chain,
                self_exports: Some(&m.exports),
                main_executable: main,
            }
            .apply(&fixups, mem, &stubs)?;

            let segments = m
                .obj
                .mappable_segments()
                .map(|seg| SegmentReport {
                    name: seg.name.to_string(),
                    vmaddr: seg.vmaddr.wrapping_add(m.slide),
                    vmsize: seg.vmsize,
                    file_range: seg.file_range(),
                    executable: seg.prot().exec,
                    writable: seg.prot().write,
                })
                .collect();

            let mut image_report = ImageReport {
                name: m.obj.install_name().unwrap_or(&m.path).to_string(),
                base: m.base,
                slide: m.slide,
                encoding,
                bindings: outcome.bindings,
                missing: outcome.missing,
                rebases: outcome.rebases,
                initializers: Vec::new(),
                terminators: Vec::new(),
                interpositions: Vec::new(),
                thread_locals: Vec::new(),
                tlv_init_funcs: Vec::new(),
                tlv_template: None,
                #[cfg(feature = "ext-objc")]
                objc: None,
                segments,
                entry: m.entry,
                entry_command: m.entry_command,
            };
            self.extensions
                .observe(&m.obj, mem, m.slide, &mut image_report)?;
            report.images.push(image_report);
        }

        for m in mapped {
            protect_segments(&m.obj, mem, m.slide, self.policy.protection)?;
        }

        if self.policy.require_all_symbols {
            report.require_all_resolved()?;
        }
        Ok(report)
    }
}

pub fn loader_directory(image_path: &str) -> &str {
    dirname(image_path)
}
