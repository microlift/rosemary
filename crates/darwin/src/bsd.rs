//! BSD libc entry points. The dyld hooks are all macOS needs: the rest is
//! provided by the host's own libSystem there.

use std::ffi::{CStr, c_char, c_int};

mod posix;
pub use posix::*;

/// `bool _dyld_func_lookup(const char *name, void **address)`
///
/// What `crt1` of the pre-10.6 era calls through the `__DATA,__dyld` section to
/// find dyld's private entry points. Initializers and terminators are run by
/// this runtime, so the two it asks for are no-ops; any other name is answered
/// as dyld answers an unknown one, with a null address.
///
/// # Safety
///
/// `name` is NUL-terminated and `address` is writable.
pub unsafe extern "C" fn dyld_func_lookup(name: *const c_char, address: *mut *const ()) -> c_int {
    extern "C" fn nothing() {}
    if name.is_null() || address.is_null() {
        return 0;
    }
    // SAFETY: the caller's obligation.
    let name = unsafe { CStr::from_ptr(name) }.to_bytes();
    let found: *const () = match name {
        b"__dyld_make_delayed_module_initializer_calls" | b"__dyld_mod_term_funcs" => {
            nothing as extern "C" fn() as *const ()
        }
        _ => std::ptr::null(),
    };
    // SAFETY: the caller's obligation.
    unsafe { address.write(found) };
    c_int::from(!found.is_null())
}

/// The lazy binding helper slot of `__DATA,__dyld`. Every import is bound
/// eagerly, so a call here means a stub was reached unbound.
pub extern "C" fn dyld_lazy_bind_helper() {
    eprintln!("rosemary: lazy binding helper reached; imports are bound eagerly");
    std::process::abort();
}
