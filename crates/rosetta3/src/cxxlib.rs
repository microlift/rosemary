use machish::error::Result;
use machish::exports::ExportTable;
use machish::image::MachObject;
use machish::namespace::{DylibRequest, DylibSource, FetchedDylib};
use machish::spec::arch::{Arm64, X86_64};

/// Which architecture's vendored images a link wants.
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

mod intel {
    pub const LIBCXX: &[u8] = include_bytes!("../vendor/libs/intel/libc++.1.dylib");
    pub const LIBCXXABI: &[u8] = include_bytes!("../vendor/libs/intel/libc++abi.dylib");
    pub const LIBUNWIND: &[u8] = include_bytes!("../vendor/libs/intel/libunwind.dylib");
}

mod arm {
    pub const LIBCXX: &[u8] = include_bytes!("../vendor/libs/arm/libc++.1.dylib");
    pub const LIBCXXABI: &[u8] = include_bytes!("../vendor/libs/arm/libc++abi.dylib");
    pub const LIBUNWIND: &[u8] = include_bytes!("../vendor/libs/arm/libunwind.dylib");
}

impl Flavor {
    pub fn libcxx(self) -> &'static [u8] {
        match self {
            Flavor::Intel => intel::LIBCXX,
            Flavor::Arm => arm::LIBCXX,
        }
    }

    pub fn libcxxabi(self) -> &'static [u8] {
        match self {
            Flavor::Intel => intel::LIBCXXABI,
            Flavor::Arm => arm::LIBCXXABI,
        }
    }

    pub fn libunwind(self) -> &'static [u8] {
        match self {
            Flavor::Intel => intel::LIBUNWIND,
            Flavor::Arm => arm::LIBUNWIND,
        }
    }

    /// Parse one of this flavor's images under its own spec. A slice of the wrong
    /// architecture parses to nothing rather than to wrong addresses.
    ///
    /// `pub(crate)` rather than private because `crate::objcexc`'s test needs the
    /// same table [`bind_objc_exceptions_to_the_vendored_cxxabi`] builds, and a
    /// second way to build it is a second thing to keep right.
    pub(crate) fn exports(self, bytes: &[u8]) -> Result<ExportTable> {
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
    // The vendored image's own `LC_ID_DYLIB`, and where the unwinder actually
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

/// The path the vendored image is known by once loaded.
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

/// A [`DylibSource`] that answers `/usr/lib/libc++` with a vendored libc++ of
/// `flavor`, and delegates everything else.
pub struct VendoredCxx<S> {
    inner: S,
    flavor: Flavor,
}

impl<S> VendoredCxx<S> {
    pub fn new(inner: S, flavor: Flavor) -> Self {
        Self { inner, flavor }
    }
}

impl<S: DylibSource> DylibSource for VendoredCxx<S> {
    fn fetch(&mut self, request: &DylibRequest<'_>) -> Result<Option<FetchedDylib>> {
        if is_libcxx(request.install_name) {
            return Ok(Some(FetchedDylib {
                path: AS_PATH.to_string(),
                bytes: self.flavor.libcxx().to_vec(),
                implied: Vec::new(),
            }));
        } else if is_libcxxabi(request.install_name) || is_libstdcxx(request.install_name) {
            return Ok(Some(FetchedDylib {
                path: CXXABI_PATH.to_string(),
                bytes: self.flavor.libcxxabi().to_vec(),
                // The vendored `libc++abi` takes `_Unwind_*` from
                // `libSystem`, which on a real system re-exports the
                // unwinder. Nothing here loads `libSystem`, so the unwinder
                // has to be named directly or it never enters the graph and
                // [`bind_unwinder_to_the_vendored_one`] has nothing to bind
                // to.
                implied: vec![UNWIND_PATH.to_string()],
            }));
        } else if is_libunwind(request.install_name) {
            return Ok(Some(FetchedDylib {
                path: UNWIND_PATH.to_string(),
                bytes: self.flavor.libunwind().to_vec(),
                implied: Vec::new(),
            }));
        }
        self.inner.fetch(request)
    }
}

/// Point every `_Unwind_*` binding at the **vendored** `libunwind`.
///
/// Slots are rewritten in guest memory *and* in the report, so everything built from
/// the report afterwards - [`crate::Program`]'s import maps, the audit - describes what
/// the slot actually holds.
pub fn bind_unwinder_to_the_vendored_one<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    flavor: Flavor,
) -> Result<usize> {
    // The Mach-O spelling carries a leading underscore, so the C identifier
    // `_Unwind_Resume` arrives as `__Unwind_Resume`.
    rebind_to_vendored(
        report,
        memory,
        flavor,
        flavor.libunwind(),
        &is_libunwind,
        |symbol| symbol.starts_with("__Unwind_"),
    )
}

/// Point every C++ ABI binding at the **vendored** `libc++abi`.
///
/// [`is_cxx_abi_symbol`] is deliberately a family test rather than "anything the
/// vendored `libc++abi` exports".
pub fn bind_cxxabi_to_the_vendored_one<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    flavor: Flavor,
) -> Result<usize> {
    rebind_to_vendored(
        report,
        memory,
        flavor,
        flavor.libcxxabi(),
        &is_libcxxabi,
        is_cxx_abi_symbol,
    )
}

/// Whether `symbol` is part of the Itanium C++ ABI *runtime interface* - the half of
/// `libstdc++` that the vendored `libc++abi` is a drop-in for.
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
fn rebind_to_vendored<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    flavor: Flavor,
    bytes: &[u8],
    is_image: &dyn Fn(&str) -> bool,
    wanted: impl Fn(&str) -> bool,
) -> Result<usize> {
    // Where the vendored image landed, if this link wanted one at all. A C
    // program does not, and neither does a C++ program that never throws.
    let Some((base, slide)) = report
        .images
        .iter()
        .find(|i| is_image(&i.name))
        .map(|i| (i.base, i.slide))
    else {
        return Ok(0);
    };
    let exports = flavor.exports(bytes)?;

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
        // The vendored image's own bindings are not rewritten: it is the thing
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

/// Bind the Objective-C exception machinery to the vendored `libc++abi`.
///
/// Last of the three, because every address it writes is one of that image's.
pub fn bind_objc_exceptions_to_the_vendored_cxxabi<M: machish::memory::Regions>(
    report: &mut machish::report::LinkReport,
    memory: &mut M,
    flavor: Flavor,
) -> Result<usize> {
    crate::objcexc::bind(
        report,
        memory,
        &flavor.exports(flavor.libcxxabi())?,
        &is_libcxxabi,
    )
}

/// The source every link in this crate uses: vendored libc++ in front of the
/// base's userland libraries (see [`crate::userland`]), in front of the filesystem.
pub fn source(
    flavor: Flavor,
) -> VendoredCxx<crate::userland::BaseLibraries<machish::namespace::FsDylibSource>> {
    VendoredCxx::new(
        crate::userland::BaseLibraries::new(machish::namespace::FsDylibSource::new(), flavor),
        flavor,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed() -> MachObject<'static, X86_64> {
        MachObject::parse_any(intel::LIBCXX, X86_64)
            .expect("the vendored libc++ is an x86-64 Mach-O")
    }

    /// It has to be the *guest's* architecture, or none of this means anything.
    #[test]
    fn the_vendored_library_is_intel() {
        let obj = parsed();
        assert!(obj.section("__TEXT", "__text").is_some(), "no __text");
    }

    /// The vintage check.
    #[test]
    fn vendored_libcxx_is_the_abi_the_guest_expects() {
        let exports = ExportTable::parse(&parsed()).expect("exports");
        for symbol in [
            "__ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE9__grow_byEmmmmmm",
            "__ZNKSt3__121__basic_string_commonILb1EE20__throw_length_errorEv",
            "__ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE6__initEPKcm",
            "__ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEEaSERKS5_",
            "__ZNSt3__18ios_base4initEPv",
        ] {
            assert!(exports.contains(symbol), "vendored libc++ lacks {symbol}");
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

    /// A `libstdc++` request loads the vendored `libc++abi`, **under its own name**.
    #[test]
    fn a_libstdcxx_request_gets_the_cxxabi_bytes_under_the_cxxabi_name() {
        let mut source = VendoredCxx::new(machish::namespace::NoDylibSource, Flavor::Intel);
        let fetched = source
            .fetch(&DylibRequest {
                install_name: "/usr/lib/libstdc++.6.dylib",
                candidates: &[],
                kind: machish::image::DependencyKind::Load,
                loader_path: None,
            })
            .expect("the vendored source does not fail")
            .expect("libstdc++ is answered");
        assert_eq!(fetched.path, CXXABI_PATH);
        assert_eq!(fetched.bytes.len(), intel::LIBCXXABI.len());
    }

    /// The unwinder has to enter the dependency graph even though nothing in it
    /// names the unwinder.
    ///
    /// The vendored `libc++abi` takes `_Unwind_*` from `libSystem`, which is
    /// never loaded as a guest image, so without the implied dependency the
    /// unwinder is absent and [`bind_unwinder_to_the_vendored_one`] silently
    /// rebinds nothing - which is how `__Unwind_RaiseException` ended up
    /// reaching the host as an unsigned import.
    #[test]
    fn a_cxxabi_request_pulls_in_the_unwinder() {
        let mut source = VendoredCxx::new(machish::namespace::NoDylibSource, Flavor::Intel);
        let fetched = source
            .fetch(&DylibRequest {
                install_name: "/usr/lib/libc++abi.dylib",
                candidates: &[],
                kind: machish::image::DependencyKind::Load,
                loader_path: None,
            })
            .expect("the vendored source does not fail")
            .expect("libc++abi is answered");
        assert_eq!(fetched.implied, vec![UNWIND_PATH.to_string()]);
        assert!(is_libunwind(UNWIND_PATH), "the implied name must resolve");

        // And the implied name really does reach the unwinder's bytes.
        let unwinder = source
            .fetch(&DylibRequest {
                install_name: UNWIND_PATH,
                candidates: &[],
                kind: machish::image::DependencyKind::Load,
                loader_path: None,
            })
            .expect("the vendored source does not fail")
            .expect("libunwind is answered");
        assert_eq!(unwinder.bytes.len(), intel::LIBUNWIND.len());
    }

    /// The unwinder the rebind points everything at actually defines the
    /// entry points a throw goes through.
    #[test]
    fn the_vendored_unwinder_exports_the_unwind_interface() {
        let object = MachObject::parse_any(intel::LIBUNWIND, X86_64).expect("x86-64 Mach-O");
        let exports = ExportTable::parse(&object).expect("exports");
        for symbol in [
            "__Unwind_RaiseException",
            "__Unwind_Resume",
            "__Unwind_DeleteException",
            "__Unwind_GetLanguageSpecificData",
            "__Unwind_SetIP",
        ] {
            assert!(
                exports.contains(symbol),
                "vendored libunwind lacks {symbol}"
            );
        }
    }

    /// The vendored `libc++abi` really does define every C++ ABI symbol a
    /// `libstdc++`-linked binary takes from `libstdc++`.
    ///
    /// This is the list A Hat in Time imports, which is the whole of the ABI half.
    #[test]
    fn the_vendored_cxxabi_covers_what_libstdcxx_supplied() {
        let object = MachObject::parse_any(intel::LIBCXXABI, X86_64).expect("x86-64 Mach-O");
        let exports = ExportTable::parse(&object).expect("exports");
        for symbol in [
            "___cxa_begin_catch",
            "___cxa_call_unexpected",
            "___cxa_end_catch",
            "___cxa_guard_abort",
            "___cxa_guard_acquire",
            "___cxa_guard_release",
            "___cxa_pure_virtual",
            "___gxx_personality_v0",
            "__ZSt9terminatev",
            "__ZTVN10__cxxabiv117__class_type_infoE",
            "__ZTVN10__cxxabiv120__si_class_type_infoE",
        ] {
            assert!(
                exports.contains(symbol),
                "vendored libc++abi lacks {symbol}"
            );
            assert!(is_cxx_abi_symbol(symbol), "{symbol} is not in the family");
        }
    }

    /// What the family test must *not* claim.
    ///
    /// `std::runtime_error`'s destructor is exported by the vendored `libc++abi` and by
    /// `libstdc++`, and they are not the same object - see [`is_cxx_abi_symbol`].
    #[test]
    fn the_family_excludes_the_exception_types_and_the_containers() {
        assert!(!is_cxx_abi_symbol("__ZNSt13runtime_errorD1Ev"));
        assert!(!is_cxx_abi_symbol("__ZNKSt9exception4whatEv"));
        assert!(!is_cxx_abi_symbol("__ZNSt9bad_allocC1Ev"));
        assert!(!is_cxx_abi_symbol(
            "__ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE6__initEPKcm"
        ));
        // Not the ABI's `__cxa_atexit`: that one is shimmed, because the shim
        // has to substitute the DSO handle as well.
        assert!(
            !ExportTable::parse(&MachObject::parse_any(intel::LIBCXXABI, X86_64).unwrap())
                .unwrap()
                .contains("___cxa_atexit")
        );
    }

    /// Each flavor's images really are of that flavor. A vendored image of the
    /// wrong architecture links as nothing rather than as wrong addresses.
    #[test]
    fn each_flavor_carries_its_own_architecture() {
        for (flavor, bytes) in [
            (Flavor::Intel, intel::LIBCXX),
            (Flavor::Arm, arm::LIBCXX),
            (Flavor::Intel, intel::LIBCXXABI),
            (Flavor::Arm, arm::LIBCXXABI),
            (Flavor::Intel, intel::LIBUNWIND),
            (Flavor::Arm, arm::LIBUNWIND),
        ] {
            let exports = flavor.exports(bytes).expect("a {flavor:?} Mach-O");
            assert!(!exports.is_empty(), "{flavor:?} image exports nothing");
        }
        assert!(Flavor::Arm.exports(intel::LIBCXX).is_err());
        assert!(Flavor::Intel.exports(arm::LIBCXX).is_err());
    }

    /// The ARM libc++ is the same ABI as the Intel one, which is what lets the
    /// native backend take it in place of the system's.
    #[test]
    fn the_arm_libcxx_answers_for_the_same_names() {
        let exports = Flavor::Arm.exports(arm::LIBCXX).expect("arm64 Mach-O");
        for symbol in [
            "__ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE9__grow_byEmmmmmm",
            "__ZNSt3__112basic_stringIcNS_11char_traitsIcEENS_9allocatorIcEEE6__initEPKcm",
            "__ZNSt3__18ios_base4initEPv",
        ] {
            assert!(
                exports.contains(symbol),
                "vendored arm libc++ lacks {symbol}"
            );
        }
    }

    /// `operator new` and `operator delete` are `libc++abi`'s, reached through
    /// `libc++`'s re-export of it. They are why the native backend takes the
    /// vendored runtime on macOS too: these call the `malloc` and `free`
    /// [`crate::interpose`] rewrites, where Apple's copy calls the ones dyld
    /// bound before this process started.
    #[test]
    fn operator_new_comes_from_the_vendored_cxxabi() {
        for (flavor, bytes) in [
            (Flavor::Intel, intel::LIBCXXABI),
            (Flavor::Arm, arm::LIBCXXABI),
        ] {
            let exports = flavor.exports(bytes).expect("a Mach-O of its own arch");
            for symbol in ["__Znwm", "__ZdlPv", "__ZnamSt11align_val_t"] {
                assert!(
                    exports.contains(symbol),
                    "vendored {flavor:?} libc++abi lacks {symbol}"
                );
            }
        }
    }

    /// And the ARM `libc++abi` and unwinder define what the two rebinds point at.
    #[test]
    fn the_arm_cxxabi_and_unwinder_cover_the_abi() {
        let abi = Flavor::Arm.exports(arm::LIBCXXABI).expect("arm64 Mach-O");
        for symbol in [
            "___cxa_begin_catch",
            "___cxa_end_catch",
            "___cxa_throw",
            "___gxx_personality_v0",
            "__ZTVN10__cxxabiv117__class_type_infoE",
            "__ZTVN10__cxxabiv120__si_class_type_infoE",
        ] {
            assert!(
                abi.contains(symbol),
                "vendored arm libc++abi lacks {symbol}"
            );
        }
        let unwind = Flavor::Arm.exports(arm::LIBUNWIND).expect("arm64 Mach-O");
        for symbol in ["__Unwind_RaiseException", "__Unwind_Resume"] {
            assert!(
                unwind.contains(symbol),
                "vendored arm libunwind lacks {symbol}"
            );
        }
    }
}
