/// Darwin keeps the public resolver state in a 512-byte, pointer-aligned object.
///
/// Resolver entry points initialise it before consuming its fields. Keeping the
/// guest layout here matters even while it is zeroed: callers are allowed to
/// read and write `_res` directly.
#[cfg(not(target_os = "macos"))]
#[repr(C, align(8))]
struct ResolverState([u8; 512]);

#[cfg(not(target_os = "macos"))]
static mut RESOLVER_STATE: ResolverState = ResolverState([0; 512]);

/// `struct os_log_s` is opaque. Its address is the value of `OS_LOG_DEFAULT`,
/// so it needs stable, distinct storage even though no shim examines it.
#[cfg(not(target_os = "macos"))]
#[repr(C, align(8))]
struct DefaultLog([u8; 8]);

#[cfg(not(target_os = "macos"))]
static DEFAULT_LOG: DefaultLog = DefaultLog([0; 8]);

/// A `const char *const` global.
#[cfg(not(target_os = "macos"))]
#[repr(transparent)]
struct ConstString(*const std::ffi::c_char);

// SAFETY: the pointee is an immutable static string.
#[cfg(not(target_os = "macos"))]
unsafe impl Sync for ConstString {}

/// `kOSThermalNotificationPressureLevelName`, the notify(3) name of the
/// thermal pressure level.
#[cfg(not(target_os = "macos"))]
static THERMAL_PRESSURE_LEVEL_NAME: ConstString =
    ConstString(c"com.apple.system.thermalpressurelevel".as_ptr());

#[cfg(target_os = "macos")]
exports::exports! {}

#[cfg(not(target_os = "macos"))]
exports::exports! {
    // Both C identifiers begin with `_`, in addition to Mach-O's leading one.
    data {
        "__res" => (&raw const RESOLVER_STATE).cast::<u8>() as usize as u64,
        "__os_log_default" => &raw const DEFAULT_LOG as usize as u64,
        "_kOSThermalNotificationPressureLevelName" =>
            &raw const THERMAL_PRESSURE_LEVEL_NAME as usize as u64,
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    use machish::fixups::{LibraryRef, SymbolRef};
    use machish::resolve::SyntheticDylib;
    use machish::resolve::{SymbolKind, SymbolProvider};

    fn resolve(lib: &SyntheticDylib, name: &str) -> machish::resolve::Resolved {
        lib.resolve(&SymbolRef {
            name,
            library: LibraryRef::Flat,
            weak_import: false,
        })
        .unwrap_or_else(|| panic!("{name} resolves"))
    }

    #[test]
    fn libsystem_globals_are_data_at_their_storage() {
        let mut lib = SyntheticDylib::new("<test>");
        provide(&mut lib);

        let res = resolve(&lib, "__res");
        assert_eq!(res.kind, SymbolKind::Data);
        assert_eq!(res.addr, (&raw const RESOLVER_STATE) as usize as u64);

        let log = resolve(&lib, "__os_log_default");
        assert_eq!(log.kind, SymbolKind::Data);
        assert_eq!(log.addr, (&raw const DEFAULT_LOG) as usize as u64);
    }

    #[test]
    fn thermal_pressure_name_is_a_pointer_to_the_notify_name() {
        let mut lib = SyntheticDylib::new("<test>");
        provide(&mut lib);

        let name = resolve(&lib, "_kOSThermalNotificationPressureLevelName");
        assert_eq!(name.kind, SymbolKind::Data);
        // SAFETY: the resolved address is the static `const char *const`.
        let value = unsafe { std::ffi::CStr::from_ptr(*(name.addr as *const *const _)) };
        assert_eq!(value, c"com.apple.system.thermalpressurelevel");
    }
}
