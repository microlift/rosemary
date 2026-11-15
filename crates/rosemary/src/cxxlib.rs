use std::path::PathBuf;
use std::sync::OnceLock;

use machish::error::{Error, Result};
use machish::exports::ExportTable;
use machish::image::MachObject;
use machish::namespace::{DylibRequest, DylibSource, FetchedDylib};
use machish::spec::arch::{Arm64, X86_64};

/// Which architecture's C++ runtime a link wants.
///
/// The emulated backend is always [`Flavor::Intel`]; the native backend takes the
/// host's, because the image it is linking is the host's.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Flavor {
    Intel,
    Arm,
}

/// The flavor the native backend needs: an image of this host's architecture can
/// only be linked against libraries of it.
pub const HOST: Flavor = match cfg!(target_arch = "aarch64") {
    true => Flavor::Arm,
    false => Flavor::Intel,
};

/// Where the runtime's three images live in a MiniDarwin base, as the guest
/// spells them. There is no other source: a runtime comes from the prefix, or from
/// the x86-64 or arm64 MiniDarwin base in the cache, and from nowhere else.
const LIBCXX_FILE: &str = "/usr/lib/libc++.1.dylib";
const LIBCXXABI_FILE: &str = "/usr/lib/libc++abi.dylib";
const LIBUNWIND_FILE: &str = "/usr/lib/system/libunwind.dylib";

/// The GNU `libstdc++` an x86-64 base keeps for pre-libc++ binaries, under the
/// install name it identifies as. Optional: an arm64 base has none, because no
/// arm64 binary ever linked it.
const LIBSTDCXX_FILE: &str = "/compat/OS X/10.8/usr/lib/libstdc++.6.dylib";

/// Where one flavor's runtime was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The prefix's own `/usr/lib`.
    Prefix,
    /// A verified MiniDarwin tree in the rootfs cache, for a flavor the prefix's
    /// base is not (an x86-64 base on an arm64 host) or when there is no prefix.
    Base(PathBuf),
}

impl Source {
    fn file(&self, guest: &str) -> PathBuf {
        match self {
            Source::Prefix => prefix::ns::host_path(guest),
            Source::Base(tree) => tree.join(guest.trim_start_matches('/')),
        }
    }

    fn holds(&self, flavor: Flavor) -> bool {
        [LIBCXX_FILE, LIBCXXABI_FILE, LIBUNWIND_FILE]
            .iter()
            .all(|file| std::fs::read(self.file(file)).is_ok_and(|b| flavor.can_load(&b)))
    }
}

/// MiniDarwin's spelling of `flavor`'s architecture.
fn arch(flavor: Flavor) -> &'static str {
    match flavor {
        Flavor::Intel => "x86_64",
        Flavor::Arm => "aarch64",
    }
}

/// The pinned base for `flavor`, if it is already in the cache.
///
/// A tree only enters the cache after it has been verified against its manifest,
/// so finding one is enough; the full check is `provision::obtain`'s job.
fn cached_base(flavor: Flavor) -> Option<PathBuf> {
    let spec = prefix::pinned::spec(arch(flavor))?.ok()?;
    let digest = spec.tree?;
    let tree = prefix::cache::Cache::from_env()?.tree(&digest);
    tree.is_dir().then_some(tree)
}

/// Where `flavor`'s C++ runtime comes from, or why there is none.
///
/// The prefix first, because it is what the user chose; then the pinned base
/// for that architecture. On a native macOS host with no prefix this is still
/// the arm64 base and not the system's: Apple's `operator new` calls the `malloc`
/// dyld bound before this process started, which [`crate::interpose`] cannot
/// reach.
pub fn locate(flavor: Flavor) -> std::result::Result<Source, String> {
    static FOUND: [OnceLock<Source>; 2] = [OnceLock::new(), OnceLock::new()];
    let slot = &FOUND[flavor as usize];
    if let Some(found) = slot.get() {
        return Ok(found.clone());
    }
    let found = if Source::Prefix.holds(flavor) {
        Source::Prefix
    } else if let Some(tree) = cached_base(flavor).map(Source::Base)
        && tree.holds(flavor)
    {
        tree
    } else {
        return Err(format!(
            "no {} MiniDarwin base provides libc++: the prefix has none for this \
             architecture and none is cached. Run `rosemary prefix create`, or \
             set ROSEMARY_PREFIX_SETUP=download",
            arch(flavor)
        ));
    };
    Ok(slot.get_or_init(|| found).clone())
}

impl Flavor {
    fn read(self, guest: &str) -> Result<Vec<u8>> {
        let source = locate(self).map_err(|why| {
            eprintln!("rosemary: {why}");
            Error::Custom("no MiniDarwin base provides the C++ runtime")
        })?;
        std::fs::read(source.file(guest))
            .map_err(|_| Error::Custom("the C++ runtime is unreadable"))
    }

    pub fn libcxx(self) -> Result<Vec<u8>> {
        self.read(LIBCXX_FILE)
    }

    pub fn libcxxabi(self) -> Result<Vec<u8>> {
        self.read(LIBCXXABI_FILE)
    }

    pub fn libunwind(self) -> Result<Vec<u8>> {
        self.read(LIBUNWIND_FILE)
    }

    /// The base's GNU `libstdc++`, if it has one this flavor can map.
    fn libstdcxx(self) -> Option<Vec<u8>> {
        let bytes = std::fs::read(locate(self).ok()?.file(LIBSTDCXX_FILE)).ok()?;
        self.can_load(&bytes).then_some(bytes)
    }

    /// Whether `bytes` has a slice this flavor's backend can map.
    pub(crate) fn can_load(self, bytes: &[u8]) -> bool {
        match self {
            Flavor::Intel => MachObject::parse_any(bytes, X86_64).is_ok(),
            Flavor::Arm => MachObject::parse_any(bytes, Arm64).is_ok(),
        }
    }

    /// Parse one of this flavor's images under its own spec. A slice of the wrong
    /// architecture parses to nothing rather than to wrong addresses.
    fn exports(self, bytes: &[u8]) -> Result<ExportTable> {
        match self {
            Flavor::Intel => ExportTable::parse(&MachObject::parse_any(bytes, X86_64)?),
            Flavor::Arm => ExportTable::parse(&MachObject::parse_any(bytes, Arm64)?),
        }
    }
}

/// The install names Apple's `libc++` answers to, and which this stands in for.
const NAMES: &[&str] = &[
    "/usr/lib/libc++.1.dylib",
    "/usr/lib/libc++.dylib",
    "/usr/lib/libc++.1.0.dylib",
    "@rpath/libc++.1.dylib",
];

const CXXABI_NAMES: &[&str] = &[
    "/usr/lib/libc++abi.1.dylib",
    "/usr/lib/libc++abi.dylib",
    "/usr/lib/libc++abi.1.0.dylib",
    "@rpath/libc++abi.1.dylib",
];

const UNWIND_NAMES: &[&str] = &[
    "/usr/lib/libunwind.1.dylib",
    "/usr/lib/libunwind.dylib",
    "/usr/lib/libunwind.1.0.dylib",
    "@rpath/libunwind.1.dylib",
    // The runtime image's own `LC_ID_DYLIB`, and where the unwinder actually
    // lives on a real system.
    "/usr/lib/system/libunwind.dylib",
];

/// The GNU C++ runtime a pre-2011 Intel binary was linked against.
///
/// It matters here for one reason: it is where such a binary's *C++ ABI* imports come
/// from. See the module note on the second library that names the ABI.
const LIBSTDCXX_NAMES: &[&str] = &[
    "/usr/lib/libstdc++.6.dylib",
    "/usr/lib/libstdc++.dylib",
    "/usr/lib/libstdc++.6.0.9.dylib",
];

/// The path the runtime image is known by once loaded.
const AS_PATH: &str = "/usr/lib/libc++.1.dylib";
const CXXABI_PATH: &str = "/usr/lib/libc++abi.1.dylib";
const UNWIND_PATH: &str = "/usr/lib/libunwind.1.dylib";

/// Whether `install_name` is a request for the C++ runtime.
pub fn is_libcxx(install_name: &str) -> bool {
    NAMES.contains(&install_name)
}

pub fn is_libcxxabi(install_name: &str) -> bool {
    CXXABI_NAMES.contains(&install_name)
}

pub fn is_libunwind(install_name: &str) -> bool {
    UNWIND_NAMES.contains(&install_name)
}

pub fn is_libstdcxx(install_name: &str) -> bool {
    LIBSTDCXX_NAMES.contains(&install_name)
}

/// The file [`BaseCxx`] reads for an image it reports at `path`, which is the
/// install name the image stands in for and not a file of its own.
pub fn file_of(flavor: Flavor, path: &str) -> Option<PathBuf> {
    let guest = match path {
        AS_PATH => LIBCXX_FILE,
        CXXABI_PATH => LIBCXXABI_FILE,
        UNWIND_PATH => LIBUNWIND_FILE,
        LIBSTDCXX_FILE => LIBSTDCXX_FILE,
        _ => return None,
    };
    Some(locate(flavor).ok()?.file(guest))
}

/// A [`DylibSource`] that answers `/usr/lib/libc++` and its two companions from
/// the base [`locate`] found, and delegates everything else.
pub struct BaseCxx<S> {
    inner: S,
    flavor: Flavor,
}

impl<S> BaseCxx<S> {
    pub fn new(inner: S, flavor: Flavor) -> Self {
        Self { inner, flavor }
    }
}

impl<S: DylibSource> DylibSource for BaseCxx<S> {
    fn fetch(&mut self, request: &DylibRequest<'_>) -> Result<Option<FetchedDylib>> {
        if is_libcxx(request.install_name) {
            return Ok(Some(FetchedDylib {
                path: AS_PATH.to_string(),
                bytes: self.flavor.libcxx()?,
                implied: Vec::new(),
            }));
        } else if is_libstdcxx(request.install_name)
            && let Some(bytes) = self.flavor.libstdcxx()
        {
            // The real library: it re-exports the ABI half from `libc++abi`, so
            // both halves come from one runtime rather than the ABI from here and
            // the containers and streams from whatever the host has loaded.
            return Ok(Some(FetchedDylib {
                path: LIBSTDCXX_FILE.to_string(),
                bytes,
                implied: Vec::new(),
            }));
        } else if is_libcxxabi(request.install_name) || is_libstdcxx(request.install_name) {
            return Ok(Some(FetchedDylib {
                path: CXXABI_PATH.to_string(),
                bytes: self.flavor.libcxxabi()?,
                // `libc++abi` takes `_Unwind_*` from `libSystem`, which on a
                // real system re-exports the unwinder. Nothing here loads
                // `libSystem`, so the unwinder has to be named directly or it
                // never enters the graph and [`bind_unwinder_to_the_runtime`]
                // has nothing to bind to.
                implied: vec![UNWIND_PATH.to_string()],
            }));
        } else if is_libunwind(request.install_name) {
            return Ok(Some(FetchedDylib {
                path: UNWIND_PATH.to_string(),
                bytes: self.flavor.libunwind()?,
                implied: Vec::new(),
            }));
        }
        self.inner.fetch(request)
    }
}

/// Point every `_Unwind_*` binding at the runtime's `libunwind`.
///
/// Slots are rewritten in guest memory *and* in the report, so everything built from
/// the report afterwards - [`crate::Program`]'s import maps, the audit - describes what
/// the slot actually holds.
pub fn bind_unwinder_to_the_runtime<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    flavor: Flavor,
) -> Result<usize> {
    // The Mach-O spelling carries a leading underscore, so the C identifier
    // `_Unwind_Resume` arrives as `__Unwind_Resume`.
    rebind_to_runtime(
        report,
        memory,
        flavor,
        || flavor.libunwind(),
        &is_libunwind,
        |symbol| symbol.starts_with("__Unwind_"),
    )
}

/// Point every C++ ABI binding at the runtime's `libc++abi`.
///
/// [`is_cxx_abi_symbol`] is deliberately a family test rather than "anything the
/// runtime `libc++abi` exports".
pub fn bind_cxxabi_to_the_runtime<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    flavor: Flavor,
) -> Result<usize> {
    rebind_to_runtime(
        report,
        memory,
        flavor,
        || flavor.libcxxabi(),
        &is_libcxxabi,
        is_cxx_abi_symbol,
    )
}

/// Whether `symbol` is part of the Itanium C++ ABI *runtime interface* - the half of
/// `libstdc++` that the runtime `libc++abi` is a drop-in for.
///
/// The three prefixes are the ABI's own namespaces.
fn is_cxx_abi_symbol(symbol: &str) -> bool {
    /// The handler interface, which is spelled in `std::` rather than
    /// `__cxxabiv1::` but is defined by the ABI all the same.
    const TERMINATE_HANDLERS: &[&str] = &[
        "__ZSt9terminatev",
        "__ZSt10unexpectedv",
        "__ZSt13get_terminatev",
        "__ZSt14get_unexpectedv",
        "__ZSt13set_terminatePFvvE",
        "__ZSt14set_unexpectedPFvvE",
        "__ZSt18uncaught_exceptionv",
        "__ZSt19uncaught_exceptionsv",
    ];

    symbol.starts_with("___cxa_")
        || symbol.starts_with("___gxx_personality_")
        || symbol.starts_with("__ZN10__cxxabiv1")
        || symbol.starts_with("__ZTVN10__cxxabiv1")
        || symbol.starts_with("__ZTIN10__cxxabiv1")
        || symbol.starts_with("__ZTSN10__cxxabiv1")
        || TERMINATE_HANDLERS.contains(&symbol)
}

/// The shared body of the two rebinds above.
fn rebind_to_runtime<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    flavor: Flavor,
    bytes: impl FnOnce() -> Result<Vec<u8>>,
    is_image: &dyn Fn(&str) -> bool,
    wanted: impl Fn(&str) -> bool,
) -> Result<usize> {
    // Where the runtime image landed, if this link wanted one at all. A C
    // program does not, and neither does a C++ program that never throws.
    let Some((base, slide)) = report
        .images
        .iter()
        .find(|i| is_image(&i.name))
        .map(|i| (i.base, i.slide))
    else {
        return Ok(0);
    };
    let exports = flavor.exports(&bytes()?)?;

    // Every address the loader mapped, which is what "already in the guest"
    // means.
    let guest: Vec<(u64, u64)> = report
        .images
        .iter()
        .flat_map(|i| &i.segments)
        .map(|s| (s.vmaddr, s.vmaddr.wrapping_add(s.vmsize)))
        .collect();
    let in_guest = |addr: u64| guest.iter().any(|(lo, hi)| addr >= *lo && addr < *hi);

    let mut rebound = 0;
    for image in &mut report.images {
        // The runtime image's own bindings are not rewritten: it is the thing
        // being bound *to*, and its internal references are already its own.
        if image.base == base {
            continue;
        }
        for binding in &mut image.bindings {
            if !wanted(&binding.symbol) {
                continue;
            }
            if in_guest(binding.resolved) {
                continue;
            }
            let Some(export) = exports.get(&binding.symbol) else {
                continue;
            };
            let Some(addr) = export.address(slide) else {
                continue;
            };
            memory.write(binding.slot_va, &addr.to_le_bytes())?;
            binding.resolved = addr;
            // The tag is the *provider's*, and the provider that answered was
            // the host's.
            binding.tag = 0;
            rebound += 1;
        }
    }
    Ok(rebound)
}

/// Bind the Objective-C exception machinery to the runtime `libc++abi`.
///
/// Last of the three, because every address it writes is one of that image's.
pub fn bind_objc_exceptions_to_the_runtime_cxxabi<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    flavor: Flavor,
) -> Result<usize> {
    // No C++ runtime in the link is nothing to bind, and nothing to read. A
    // `dlopen`ed image uses the program's runtime, which is not in its link.
    if !report.images.iter().any(|i| is_libcxxabi(&i.name))
        && crate::dyld::program_image_slide(&is_libcxxabi).is_none()
    {
        return Ok(0);
    }
    crate::objcexc::bind(
        report,
        memory,
        &flavor.exports(&flavor.libcxxabi()?)?,
        &is_libcxxabi,
    )
}

/// The source every link in this crate uses: the base's C++ runtime in front of
/// its other userland libraries (see [`crate::userland`]), in front of the
/// filesystem.
pub fn source(
    flavor: Flavor,
) -> BaseCxx<crate::userland::BaseLibraries<machish::namespace::FsDylibSource>> {
    BaseCxx::new(
        crate::userland::BaseLibraries::new(machish::namespace::FsDylibSource::new(), flavor),
        flavor,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_runtime_names_map_to_a_base_file() {
        assert_eq!(file_of(Flavor::Intel, "/usr/lib/libSystem.B.dylib"), None);
        let Ok(source) = locate(Flavor::Intel) else {
            eprintln!("skipped: no x86_64 MiniDarwin base is available");
            return;
        };
        assert_eq!(
            file_of(Flavor::Intel, AS_PATH),
            Some(source.file(LIBCXX_FILE))
        );
        assert_eq!(
            file_of(Flavor::Intel, CXXABI_PATH),
            Some(source.file(LIBCXXABI_FILE))
        );
        assert_eq!(
            file_of(Flavor::Intel, UNWIND_PATH),
            Some(source.file(LIBUNWIND_FILE))
        );
    }

    /// A flavor's three images, or `None` on a machine with no such base cached.
    ///
    /// A test that returns here has not run; CI has to provide the bases.
    fn runtime(flavor: Flavor) -> Option<[Vec<u8>; 3]> {
        match (flavor.libcxx(), flavor.libcxxabi(), flavor.libunwind()) {
            (Ok(a), Ok(b), Ok(c)) => Some([a, b, c]),
            _ => {
                eprintln!("skipped: no {} MiniDarwin base is available", arch(flavor));
                None
            }
        }
    }

    /// The names it stands in for, and one it must not claim.
    #[test]
    fn it_answers_for_libcxx_and_nothing_else() {
        assert!(is_libcxx("/usr/lib/libc++.1.dylib"));
        assert!(is_libcxx("/usr/lib/libc++.dylib"));
        assert!(is_libcxx("@rpath/libc++.1.dylib"));
        // libc++abi and libstdc++ are answered too, but by their own arms and
        // with their own bytes - `is_libcxx` deciding either would hand a
        // C++-ABI request the library instead.
        assert!(!is_libcxx("/usr/lib/libc++abi.dylib"));
        assert!(is_libcxxabi("/usr/lib/libc++abi.dylib"));
        assert!(!is_libcxx("/usr/lib/libSystem.B.dylib"));
        assert!(!is_libcxx("/usr/lib/libstdc++.6.dylib"));
        assert!(is_libstdcxx("/usr/lib/libstdc++.6.dylib"));
    }

    /// The unwinder is where a MiniDarwin base keeps it, and the implied path
    /// has to resolve back to it.
    #[test]
    fn the_unwinder_is_answered_under_its_base_install_name() {
        assert!(is_libunwind(LIBUNWIND_FILE));
        assert!(is_libunwind(UNWIND_PATH));
    }

    /// A base in a directory that holds nothing is not a runtime, and a wrong
    /// architecture's is not one either.
    #[test]
    fn an_empty_tree_is_no_runtime() {
        let empty = tempfile_dir("cxxlib-empty");
        assert!(!Source::Base(empty.clone()).holds(Flavor::Intel));
        assert!(!Source::Base(empty.clone()).holds(Flavor::Arm));
        let _ = std::fs::remove_dir_all(empty);
    }

    fn tempfile_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The vintage check.
    #[test]
    fn libcxx_is_the_abi_the_guest_expects() {
        for flavor in [Flavor::Intel, Flavor::Arm] {
            let Some([libcxx, ..]) = runtime(flavor) else {
                continue;
            };
            let exports = flavor.exports(&libcxx).expect("exports");
            for symbol in [
                "__ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE9__grow_byEmmmmmm",
                "__ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE6__initEPKcm",
                "__ZNSt3__18ios_base4initEPv",
            ] {
                assert!(exports.contains(symbol), "{flavor:?} libc++ lacks {symbol}");
            }
        }
    }

    /// A `libstdc++` request loads the base's real `libstdc++` when it has one,
    /// and otherwise `libc++abi`, **under its own name**.
    #[test]
    fn a_libstdcxx_request_gets_the_base_libstdcxx_or_else_cxxabi() {
        let Some([_, cxxabi, _]) = runtime(Flavor::Intel) else {
            return;
        };
        let mut source = BaseCxx::new(machish::namespace::NoDylibSource, Flavor::Intel);
        let fetched = source
            .fetch(&DylibRequest {
                install_name: "/usr/lib/libstdc++.6.dylib",
                candidates: &[],
                kind: machish::image::DependencyKind::Load,
                loader_path: None,
            })
            .expect("the base answers")
            .expect("libstdc++ is answered");
        match Flavor::Intel.libstdcxx() {
            Some(real) => {
                assert_eq!(fetched.path, LIBSTDCXX_FILE);
                assert_eq!(fetched.bytes.len(), real.len());
            }
            None => {
                assert_eq!(fetched.path, CXXABI_PATH);
                assert_eq!(fetched.bytes.len(), cxxabi.len());
            }
        }
    }

    /// The GNU runtime takes its ABI half from `libc++abi` by re-export, which is
    /// what keeps a `libstdc++` binary on one exception runtime.
    #[test]
    fn the_base_libstdcxx_reexports_the_abi_from_cxxabi() {
        let Some(real) = Flavor::Intel.libstdcxx() else {
            eprintln!("skipped: the x86_64 base has no libstdc++");
            return;
        };
        let exports = Flavor::Intel.exports(&real).expect("exports");
        for symbol in ["___cxa_throw", "___gxx_personality_v0", "__ZTISt8bad_cast"] {
            let export = exports.get(symbol).expect("libstdc++ exports the ABI");
            assert!(
                matches!(export.value, machish::exports::ExportValue::Reexport { .. }),
                "{symbol} is defined by libstdc++ rather than re-exported"
            );
        }
        assert!(exports.contains("__ZNSt8ios_base4InitC1Ev"));
        // Nothing an arm64 link could take.
        assert!(Flavor::Arm.libstdcxx().is_none());
    }

    /// The unwinder has to enter the dependency graph even though nothing in it
    /// names the unwinder: `libc++abi` takes `_Unwind_*` from `libSystem`, which is
    /// never loaded as a guest image.
    #[test]
    fn a_cxxabi_request_pulls_in_the_unwinder() {
        let Some([_, _, unwind]) = runtime(Flavor::Intel) else {
            return;
        };
        let mut source = BaseCxx::new(machish::namespace::NoDylibSource, Flavor::Intel);
        let request = |install_name| DylibRequest {
            install_name,
            candidates: &[],
            kind: machish::image::DependencyKind::Load,
            loader_path: None,
        };
        let fetched = source
            .fetch(&request("/usr/lib/libc++abi.dylib"))
            .expect("the base answers")
            .expect("libc++abi is answered");
        assert_eq!(fetched.implied, vec![UNWIND_PATH.to_string()]);
        assert!(is_libunwind(UNWIND_PATH), "the implied name must resolve");

        let unwinder = source
            .fetch(&request(UNWIND_PATH))
            .expect("the base answers")
            .expect("libunwind is answered");
        assert_eq!(unwinder.bytes.len(), unwind.len());
    }

    /// The unwinder and `libc++abi` define the entry points a throw goes through
    /// and the ABI half a `libstdc++`-linked binary takes from `libstdc++`.
    #[test]
    fn the_runtime_covers_the_unwind_and_cxx_abi() {
        for flavor in [Flavor::Intel, Flavor::Arm] {
            let Some([_, cxxabi, unwind]) = runtime(flavor) else {
                continue;
            };
            let unwind = flavor.exports(&unwind).expect("exports");
            for symbol in [
                "__Unwind_RaiseException",
                "__Unwind_Resume",
                "__Unwind_DeleteException",
                "__Unwind_GetLanguageSpecificData",
                "__Unwind_SetIP",
            ] {
                assert!(
                    unwind.contains(symbol),
                    "{flavor:?} libunwind lacks {symbol}"
                );
            }
            let abi = flavor.exports(&cxxabi).expect("exports");
            for symbol in [
                "___cxa_begin_catch",
                "___cxa_end_catch",
                "___cxa_guard_acquire",
                "___cxa_throw",
                "___gxx_personality_v0",
                "__ZSt9terminatev",
                "__ZTVN10__cxxabiv117__class_type_infoE",
                "__ZTVN10__cxxabiv120__si_class_type_infoE",
            ] {
                assert!(abi.contains(symbol), "{flavor:?} libc++abi lacks {symbol}");
                assert!(is_cxx_abi_symbol(symbol), "{symbol} is not in the family");
            }
            // Not the ABI's `__cxa_atexit`: that one is shimmed, because the
            // shim has to substitute the DSO handle as well.
            assert!(!abi.contains("___cxa_atexit"));
        }
    }

    /// What the family test must *not* claim: `std::runtime_error`'s destructor is
    /// exported by `libc++abi` and by `libstdc++`, and they are not the same object.
    #[test]
    fn the_family_excludes_the_exception_types_and_the_containers() {
        assert!(!is_cxx_abi_symbol("__ZNSt13runtime_errorD1Ev"));
        assert!(!is_cxx_abi_symbol("__ZNKSt9exception4whatEv"));
        assert!(!is_cxx_abi_symbol("__ZNSt9bad_allocC1Ev"));
        assert!(!is_cxx_abi_symbol(
            "__ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE6__initEPKcm"
        ));
    }

    /// Each flavor's images really are of that flavor, and a wrong one links as
    /// nothing rather than as wrong addresses.
    #[test]
    fn each_flavor_carries_its_own_architecture() {
        for flavor in [Flavor::Intel, Flavor::Arm] {
            let Some([libcxx, ..]) = runtime(flavor) else {
                continue;
            };
            assert!(flavor.can_load(&libcxx));
            let other = match flavor {
                Flavor::Intel => Flavor::Arm,
                Flavor::Arm => Flavor::Intel,
            };
            assert!(!other.can_load(&libcxx));
            assert!(other.exports(&libcxx).is_err());
        }
    }

    /// `operator new` and `operator delete` are `libc++abi`'s. They are why even
    /// the native backend on macOS takes the base's runtime: these call the
    /// `malloc` and `free` [`crate::interpose`] rewrites, where Apple's copy calls
    /// the ones dyld bound before this process started.
    #[test]
    fn operator_new_comes_from_the_base_cxxabi() {
        for flavor in [Flavor::Intel, Flavor::Arm] {
            let Some([_, cxxabi, _]) = runtime(flavor) else {
                continue;
            };
            let exports = flavor.exports(&cxxabi).expect("exports");
            for symbol in ["__Znwm", "__ZdlPv", "__ZnamSt11align_val_t"] {
                assert!(
                    exports.contains(symbol),
                    "{flavor:?} libc++abi lacks {symbol}"
                );
            }
        }
    }
}
