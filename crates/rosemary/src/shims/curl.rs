use core::ffi::{c_int, c_void};

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{Callbacks, GuestMemory, Result};

use super::set_int_return;
use crate::shims::bind;

/// One of this module's implementations, in the shape [`crate::shims`] wants - the
/// wider one, carrying [`Callbacks`].
pub type Shim = crate::signal::Shim;

// The host's own libcurl - `/usr/lib/libcurl.4.dylib`, which is the same
// library the guest links against by install name, so this is the passthrough
// the rest of the runtime performs through a slot, reached here by name.
#[link(name = "curl")]
unsafe extern "C" {
    fn curl_easy_setopt(handle: *mut c_void, option: c_int, ...) -> c_int;
    fn curl_easy_getinfo(handle: *mut c_void, info: c_int, ...) -> c_int;
}

/// The multiplier curl encodes an option's type with, and the tag that means a function
/// pointer.
const CURLOPTTYPE_FUNCTIONPOINT: c_int = 20_000;
const CURLOPTTYPE_OFF_T: c_int = 30_000;

/// `CURLE_BAD_FUNCTION_ARGUMENT`, which is curl's own answer to a call it
/// cannot make sense of - so a guest that reaches a refusal here gets the error
/// its own library would have given it rather than a plausible success.
const CURLE_BAD_FUNCTION_ARGUMENT: i32 = 43;

/// Whether an option's value is a function pointer curl will call back.
fn is_function_option(option: c_int) -> bool {
    (CURLOPTTYPE_FUNCTIONPOINT..CURLOPTTYPE_OFF_T).contains(&option)
}

/// `CURLcode curl_easy_setopt(CURL *handle, CURLoption option, ...)`
pub fn easy_setopt(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let handle = ctx.rdi;
    let option = ctx.rsi as c_int;
    let mut value = ctx.rdx;

    if handle == 0 {
        set_int_return(ctx, CURLE_BAD_FUNCTION_ARGUMENT);
        return Ok(());
    }
    // The one thing a signature could never have expressed. Null stays null:
    // that is how a guest asks for curl's own default handler back.
    if is_function_option(option) && value != 0 {
        value = callbacks.substitute(symbol, value)?;
    }

    // SAFETY: `handle` came from `curl_easy_init` and is the host's own; the
    // declaration is variadic, so the compiler places `value` where curl's
    // `va_arg` reads it. Every type the option list admits is eight bytes on
    // LP64, so one word is the whole of the argument whichever it is.
    let rc = unsafe { curl_easy_setopt(handle as *mut c_void, option, value) };
    set_int_return(ctx, rc);
    Ok(())
}

/// `CURLcode curl_easy_getinfo(CURL *handle, CURLINFO info, ...)`
pub fn easy_getinfo(
    _symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    _callbacks: &dyn Callbacks,
) -> Result<()> {
    let handle = ctx.rdi;
    let info = ctx.rsi as c_int;
    let out = ctx.rdx;

    if handle == 0 {
        set_int_return(ctx, CURLE_BAD_FUNCTION_ARGUMENT);
        return Ok(());
    }
    // Nothing in `CURLINFO_*` is a function pointer, so there is nothing to
    // substitute - and a guest that found one would be handed a raw guest
    // address rather than a thunk, silently. Checked rather than assumed.
    if is_function_option(info) {
        set_int_return(ctx, CURLE_BAD_FUNCTION_ARGUMENT);
        return Ok(());
    }

    // SAFETY: as above. `out` is a guest address curl writes the answer
    // through, and the guest is identity-mapped, so it is the storage the guest
    // asked to have filled in.
    let rc = unsafe { curl_easy_getinfo(handle as *mut c_void, info, out) };
    set_int_return(ctx, rc);
    Ok(())
}

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "curl_easy_setopt" => Some(bind!(callback "curl_easy_setopt" => easy_setopt)),
        "curl_easy_getinfo" => Some(bind!(callback "curl_easy_getinfo" => easy_getinfo)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tag arithmetic, against the numbers `curl.h` actually assigns.
    #[test]
    fn a_function_option_is_the_twenty_thousands() {
        // CURLOPT_WRITEFUNCTION = 20011, HEADERFUNCTION = 20079,
        // XFERINFOFUNCTION = 20219 - the ones a program installs.
        for option in [20_011, 20_079, 20_219, 20_000, 29_999] {
            assert!(is_function_option(option), "{option} is a function option");
        }
        // CURLOPT_URL = 10002 is an object pointer, VERBOSE = 41 a long, and
        // INFILESIZE_LARGE = 30115 an off_t.
        for option in [41, 10_002, 30_115, 40_043, 0] {
            assert!(
                !is_function_option(option),
                "{option} is not a function option"
            );
        }
    }
}
