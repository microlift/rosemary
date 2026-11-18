//! The stream members of the printf family.
//!
//! They render with [`crate::sprintf`]'s Darwin rendering, which shares the
//! guest's calling convention and `va_list`, and write the result through the
//! stream.

use std::ffi::{VaList, c_char, c_int, c_void};

use super::{File, Locked, STDOUTP, engine};

use crate::strfmt::vswprintf;

/// The thread switched into a guest's `locale_t` until this is dropped.
struct InLocale(Option<libc::locale_t>);

impl InLocale {
    fn new(loc: Option<*mut c_void>) -> InLocale {
        let Some(loc) = loc else {
            return InLocale(None);
        };
        let loc = match loc.is_null() {
            // Darwin's null `locale_t` is the C locale; the host's is not.
            true => c_locale(),
            false => loc as libc::locale_t,
        };
        // SAFETY: a `locale_t` the guest obtained from this process, or C's.
        InLocale(Some(unsafe { libc::uselocale(loc) }))
    }
}

impl Drop for InLocale {
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            // SAFETY: the value `uselocale` returned, which it accepts back.
            unsafe { libc::uselocale(previous) };
        }
    }
}

/// A `locale_t` for "C", made once.
fn c_locale() -> libc::locale_t {
    static C: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *C.get_or_init(|| {
        // SAFETY: a fresh locale from a constant name, kept for the process.
        unsafe {
            libc::newlocale(
                crate::locale::all_mask(),
                c"C".as_ptr(),
                std::ptr::null_mut(),
            ) as usize
        }
    }) as libc::locale_t
}

/// Render and write; the length, or -1.
unsafe fn emit(
    fp: *mut File,
    loc: Option<*mut c_void>,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    let mut out: *mut c_char = std::ptr::null_mut();
    // Only the rendering is in the guest's locale; the write is not.
    let n = {
        let _locale = InLocale::new(loc);
        // SAFETY: the caller's format and arguments.
        unsafe { crate::sprintf::rosemary_vasprintf(&mut out, format, ap) }
    };
    if n < 0 {
        return -1;
    }
    // SAFETY: the caller's stream and a rendering of `n` bytes.
    let put = unsafe {
        let _guard = Locked::new(fp);
        engine::orient(fp, -1);
        let put = engine::write_all(fp, out.cast(), n as usize);
        libc::free(out.cast());
        put
    };
    match put == n as usize {
        true => n,
        false => -1,
    }
}

/// `int vfprintf(FILE *stream, const char *format, va_list ap)`
///
/// # Safety
///
/// `stream` is open and `ap` matches `format`.
pub unsafe extern "C" fn vfprintf(fp: *mut File, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { emit(fp, None, format, ap) }
}

/// `int vprintf(const char *format, va_list ap)`
///
/// # Safety
///
/// `ap` matches `format`.
pub unsafe extern "C" fn vprintf(format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdout`, and the caller's obligations.
    unsafe { emit(STDOUTP.get(), None, format, ap) }
}

crate::variadic! {
    /// `int fprintf(FILE *stream, const char *format, ...)`
    pub fn fprintf(fp: *mut File, format: *const c_char) -> c_int => vfprintf;
    /// `int printf(const char *format, ...)`
    pub fn printf(format: *const c_char) -> c_int => vprintf;
    /// `int fprintf_l(FILE *stream, locale_t loc, const char *format, ...)`
    pub fn fprintf_l(fp: *mut File, loc: *mut c_void, format: *const c_char) -> c_int
        => vfprintf_l;
    /// `int printf_l(locale_t loc, const char *format, ...)`
    pub fn printf_l(loc: *mut c_void, format: *const c_char) -> c_int => vprintf_l;
}

/// `int vfprintf_l(FILE *stream, locale_t loc, const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vfprintf`], and `loc` is null or a `locale_t` this process made.
pub unsafe extern "C" fn vfprintf_l(
    fp: *mut File,
    loc: *mut c_void,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { emit(fp, Some(loc), format, ap) }
}

/// `int vprintf_l(locale_t loc, const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vfprintf_l`].
pub unsafe extern "C" fn vprintf_l(loc: *mut c_void, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdout`, and the caller's obligations.
    unsafe { emit(STDOUTP.get(), Some(loc), format, ap) }
}

/// `int vfwprintf(FILE *stream, const wchar_t *format, va_list ap)`
///
/// # Safety
///
/// `stream` is open and `ap` matches `format`.
pub unsafe extern "C" fn vfwprintf(fp: *mut File, format: *const u32, ap: VaList) -> c_int {
    // `vswprintf` has no "how long would it have been" answer, so the buffer
    // grows until the rendering fits. A failure at the cap is an encoding
    // error rather than a long string.
    let mut buf: Vec<u32> = vec![0; 256];
    let n = loop {
        // SAFETY: a buffer of `buf.len()` wide characters and the caller's
        // format, with a copy of the arguments for each attempt.
        let n = unsafe { vswprintf(buf.as_mut_ptr(), buf.len(), format, ap.clone()) };
        if n >= 0 {
            break n as usize;
        }
        if buf.len() >= 1 << 24 {
            return -1;
        }
        buf.resize(buf.len() * 4, 0);
    };
    // SAFETY: the caller's stream.
    unsafe {
        let _guard = Locked::new(fp);
        engine::orient(fp, 1);
        match super::wide::write_wide(fp, &buf[..n]) {
            true => n as c_int,
            false => -1,
        }
    }
}

/// `int vwprintf(const wchar_t *format, va_list ap)`
///
/// # Safety
///
/// `ap` matches `format`.
pub unsafe extern "C" fn vwprintf(format: *const u32, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdout`, and the caller's obligations.
    unsafe { vfwprintf(STDOUTP.get(), format, ap) }
}

crate::variadic! {
    /// `int fwprintf(FILE *stream, const wchar_t *format, ...)`
    pub fn fwprintf(fp: *mut File, format: *const u32) -> c_int => vfwprintf;
    /// `int wprintf(const wchar_t *format, ...)`
    pub fn wprintf(format: *const u32) -> c_int => vwprintf;
}

crate::va_list! {
    pub(super) darwin_vfprintf = vfprintf, 2;
    pub(super) darwin_vprintf = vprintf, 1;
    pub(super) darwin_vfprintf_l = vfprintf_l, 3;
    pub(super) darwin_vprintf_l = vprintf_l, 2;
    pub(super) darwin_vfwprintf = vfwprintf, 2;
    pub(super) darwin_vwprintf = vwprintf, 1;
}
