//! C FFI implementation detail for Rosemary extension libraries to interact with the host
//! operating system in a standardized manner.
//!
//! Useful for implementing Rosemary specific extensions from a portable Darwin dylib,
//! such as an OpenXR runtime, which would normally be out of scope for Rosemary since
//! macOS does not have one itself.

use static_init::dynamic;

pub mod call;
pub mod hostland;

/// Whether Rosemary extensions are enabled or not.
///
/// Disabled by default - it is highly unsafe to expose these
/// functions to guest applications, doing so could give adversarial
/// guests an easy escape opportunity.
pub fn rosemary_extensions_enabled() -> bool {
    #[dynamic]
    static ROSEMARY_EXTENSIONS_ENABLED: bool =
        match std::env::var("ROSEMARY_EXPOSE_LIBSYSTEM_EXTENSIONS").as_deref() {
            Ok(val) => val == "1",
            Err(_) => false,
        };

    *ROSEMARY_EXTENSIONS_ENABLED
}

exports::exports! {
    groups {
        call,
        hostland
    }
}
