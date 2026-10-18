//! Refusing the host's own libraries as an answer to a guest.
//!
//! On a host that is not a Mac, a host symbol with a Darwin name is a guess at
//! the Darwin ABI: a guest linked against `libc++` can end up calling the host's
//! `libstdc++`. Under this mode only what this tree implements answers - the
//! link-time chains drop `NativeDylibProvider`, [`crate::symbol`] stops falling
//! back to `dlsym`, and a guest `dlopen` never reaches the host's loader. Shims
//! that forward to a named host function keep it (see [`crate::shim_symbol`]).
//! Off on macOS, whose host libraries are the Darwin ones.

/// The variable that turns the refusal on. Any value but `0`.
pub const VAR: &str = "ROSEMARY_NO_PASSTHROUGH";

/// Whether this run refuses to answer a guest import with a host symbol.
pub fn enabled() -> bool {
    #[cfg(target_os = "macos")]
    {
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| {
            std::env::var_os(VAR).is_some_and(|value| value != "0" && !value.is_empty())
        })
    }
}
