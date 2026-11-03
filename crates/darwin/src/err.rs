//! BSD's `err(3)` and `warn(3)` families.
//!
//! The host's would read the host's `errno` and word it in the host's
//! numbering; these report the guest's own, in Darwin's words, on the guest's
//! `stderr`.

use std::ffi::{CStr, VaList, c_char, c_int};

use crate::stdio::File;

/// `progname: [message: ]reason\n`, or without the reason when `code` is `None`.
///
/// # Safety
///
/// `format` is null or a format that `ap` matches.
unsafe fn report(code: Option<c_int>, format: *const c_char, ap: VaList) {
    let fp = crate::stdio::stderr().cast::<File>();
    // SAFETY: the guest's `stderr`; `getprogname` and `strerror` answer
    // NUL-terminated strings, and the format is the caller's.
    unsafe {
        crate::stdio::fprintf(fp, c"%s: ".as_ptr(), crate::progname::getprogname());
        if !format.is_null() {
            crate::stdio::vfprintf(fp, format, ap);
            if code.is_some() {
                crate::stdio::fprintf(fp, c": ".as_ptr());
            }
        }
        if let Some(code) = code {
            let reason = CStr::from_ptr(crate::string::strerror(code));
            crate::stdio::fprintf(fp, c"%s".as_ptr(), reason.as_ptr());
        }
        crate::stdio::fprintf(fp, c"\n".as_ptr());
    }
}

/// `void vwarnc(int code, const char *fmt, va_list ap)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn vwarnc(code: c_int, format: *const c_char, ap: VaList) {
    // SAFETY: the caller's obligations.
    unsafe { report(Some(code), format, ap) }
}

/// `void vwarn(const char *fmt, va_list ap)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn vwarn(format: *const c_char, ap: VaList) {
    let code = crate::errno::guest_errno();
    // SAFETY: the caller's obligations.
    unsafe { report(Some(code), format, ap) }
}

/// `void vwarnx(const char *fmt, va_list ap)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn vwarnx(format: *const c_char, ap: VaList) {
    // SAFETY: the caller's obligations.
    unsafe { report(None, format, ap) }
}

/// `void warnc(int code, const char *fmt, ...)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn warnc(code: c_int, format: *const c_char, args: ...) {
    // SAFETY: the caller's obligations.
    unsafe { report(Some(code), format, args) }
}

/// `void warn(const char *fmt, ...)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn warn(format: *const c_char, args: ...) {
    let code = crate::errno::guest_errno();
    // SAFETY: the caller's obligations.
    unsafe { report(Some(code), format, args) }
}

/// `void warnx(const char *fmt, ...)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn warnx(format: *const c_char, args: ...) {
    // SAFETY: the caller's obligations.
    unsafe { report(None, format, args) }
}

/// `void verrc(int eval, int code, const char *fmt, va_list ap)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn verrc(eval: c_int, code: c_int, format: *const c_char, ap: VaList) -> ! {
    // SAFETY: the caller's obligations.
    unsafe { report(Some(code), format, ap) };
    crate::bsd::exit(eval)
}

/// `void verr(int eval, const char *fmt, va_list ap)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn verr(eval: c_int, format: *const c_char, ap: VaList) -> ! {
    let code = crate::errno::guest_errno();
    // SAFETY: the caller's obligations.
    unsafe { report(Some(code), format, ap) };
    crate::bsd::exit(eval)
}

/// `void verrx(int eval, const char *fmt, va_list ap)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn verrx(eval: c_int, format: *const c_char, ap: VaList) -> ! {
    // SAFETY: the caller's obligations.
    unsafe { report(None, format, ap) };
    crate::bsd::exit(eval)
}

/// `void errc(int eval, int code, const char *fmt, ...)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn errc(eval: c_int, code: c_int, format: *const c_char, args: ...) -> ! {
    // SAFETY: the caller's obligations.
    unsafe { report(Some(code), format, args) };
    crate::bsd::exit(eval)
}

/// `void err(int eval, const char *fmt, ...)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn err(eval: c_int, format: *const c_char, args: ...) -> ! {
    let code = crate::errno::guest_errno();
    // SAFETY: the caller's obligations.
    unsafe { report(Some(code), format, args) };
    crate::bsd::exit(eval)
}

/// `void errx(int eval, const char *fmt, ...)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn errx(eval: c_int, format: *const c_char, args: ...) -> ! {
    // SAFETY: the caller's obligations.
    unsafe { report(None, format, args) };
    crate::bsd::exit(eval)
}

exports::exports! {
    functions {
        "warn" => warn,
        "warnc" => warnc,
        "warnx" => warnx,
        "vwarn" => vwarn,
        "vwarnc" => vwarnc,
        "vwarnx" => vwarnx,
        "err" => err,
        "errc" => errc,
        "errx" => errx,
        "verr" => verr,
        "verrc" => verrc,
        "verrx" => verrx,
    }
}
