//! BSD's `err(3)` and `warn(3)` families.
//!
//! The host's would read the host's `errno` and word it in the host's
//! numbering; these report the guest's own, in Darwin's words, on the guest's
//! `stderr`.

use std::ffi::{VaList, c_char, c_int};

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
        crate::stdio::fputs(crate::progname::getprogname(), fp);
        crate::stdio::fputs(c": ".as_ptr(), fp);
        if !format.is_null() {
            crate::stdio::vfprintf(fp, format, ap);
            if code.is_some() {
                crate::stdio::fputs(c": ".as_ptr(), fp);
            }
        }
        if let Some(code) = code {
            crate::stdio::fputs(crate::string::strerror(code), fp);
        }
        crate::stdio::fputs(c"\n".as_ptr(), fp);
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

crate::variadic! {
    /// `void warnc(int code, const char *fmt, ...)`
    pub fn warnc(code: c_int, format: *const c_char) => vwarnc;
    /// `void warn(const char *fmt, ...)`
    pub fn warn(format: *const c_char) => vwarn;
    /// `void warnx(const char *fmt, ...)`
    pub fn warnx(format: *const c_char) => vwarnx;
}

/// `void verrc(int eval, int code, const char *fmt, va_list ap)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn verrc(eval: c_int, code: c_int, format: *const c_char, ap: VaList) -> ! {
    // SAFETY: the caller's obligations.
    unsafe { report(Some(code), format, ap) };
    crate::bsd::posix::exit(eval)
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
    crate::bsd::posix::exit(eval)
}

/// `void verrx(int eval, const char *fmt, va_list ap)`
///
/// # Safety
///
/// As [`report`].
pub unsafe extern "C" fn verrx(eval: c_int, format: *const c_char, ap: VaList) -> ! {
    // SAFETY: the caller's obligations.
    unsafe { report(None, format, ap) };
    crate::bsd::posix::exit(eval)
}

crate::variadic! {
    /// `void errc(int eval, int code, const char *fmt, ...)`
    pub fn errc(eval: c_int, code: c_int, format: *const c_char) -> ! => verrc;
    /// `void err(int eval, const char *fmt, ...)`
    pub fn err(eval: c_int, format: *const c_char) -> ! => verr;
    /// `void errx(int eval, const char *fmt, ...)`
    pub fn errx(eval: c_int, format: *const c_char) -> ! => verrx;
}

crate::va_list! {
    darwin_vwarnc = vwarnc, 2;
    darwin_vwarn = vwarn, 1;
    darwin_vwarnx = vwarnx, 1;
    darwin_verrc = verrc, 3;
    darwin_verr = verr, 2;
    darwin_verrx = verrx, 2;
}

exports::exports! {
    functions {
        "warn" => warn,
        "warnc" => warnc,
        "warnx" => warnx,
        "vwarn" => darwin_vwarn,
        "vwarnc" => darwin_vwarnc,
        "vwarnx" => darwin_vwarnx,
        "err" => err,
        "errc" => errc,
        "errx" => errx,
        "verr" => darwin_verr,
        "verrc" => darwin_verrc,
        "verrx" => darwin_verrx,
    }
}
