//! The string and descriptor members of the printf family, rendered as Darwin
//! renders them; see `sprintf.c` for where that differs from the host.

use std::ffi::{VaList, c_char, c_int};

unsafe extern "C" {
    pub(crate) fn rosemary_vasprintf(
        out: *mut *mut c_char,
        format: *const c_char,
        ap: VaList,
    ) -> c_int;
    pub(crate) fn rosemary_vsnprintf(
        buffer: *mut c_char,
        size: usize,
        format: *const c_char,
        ap: VaList,
    ) -> c_int;
    fn rosemary_vsprintf(buffer: *mut c_char, format: *const c_char, ap: VaList) -> c_int;
    fn rosemary_vdprintf(fd: c_int, format: *const c_char, ap: VaList) -> c_int;
}

/// `vasprintf`, with the result **moved into the guest's allocator**.
///
/// The rendering is allocated with this host's `malloc` and the caller frees it
/// with whichever `free` its own image binds - the crossing [`crate::heap`]
/// describes, and one that does not fail at the call: the guest's allocator
/// accepts the foreign pointer and pushes a live block onto a free list.
///
/// # Safety
///
/// `out` is null or writable, and `ap` matches `format`.
pub(crate) unsafe fn vasprintf_into_guest(
    out: *mut *mut c_char,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    if out.is_null() {
        return -1;
    }
    let mut host: *mut c_char = std::ptr::null_mut();
    // SAFETY: a live `char *` slot, and the caller's format and arguments.
    let written = unsafe { rosemary_vasprintf(&raw mut host, format, ap) };
    if written < 0 || host.is_null() {
        return -1;
    }
    // SAFETY: the rendering is NUL-terminated, so the block is `written + 1` bytes.
    let moved = unsafe { crate::heap::malloc(written as usize + 1) }.cast::<c_char>();
    if !moved.is_null() {
        // SAFETY: both blocks hold `written + 1` bytes and do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(host, moved, written as usize + 1) };
    }
    // SAFETY: this host's `malloc` allocated it.
    unsafe { libc::free(host.cast()) };
    if moved.is_null() {
        return -1;
    }
    // SAFETY: the caller states `out` is writable.
    unsafe { *out = moved };
    written
}

crate::variadic! {
    /// `int sprintf(char *, const char *, ...)`
    pub fn sprintf(buffer: *mut c_char, format: *const c_char) -> c_int => vsprintf;
    /// `int snprintf(char *, size_t, const char *, ...)`
    pub fn snprintf(buffer: *mut c_char, size: usize, format: *const c_char) -> c_int
        => vsnprintf;
    /// `int asprintf(char **, const char *, ...)`
    fn asprintf(out: *mut *mut c_char, format: *const c_char) -> c_int => vasprintf;
    /// `int dprintf(int, const char *, ...)`
    fn dprintf(fd: c_int, format: *const c_char) -> c_int => vdprintf;
}

crate::va_list! {
    darwin_vsprintf = vsprintf, 2;
    darwin_vsnprintf = vsnprintf, 3;
    darwin_vasprintf = vasprintf, 2;
    darwin_vdprintf = vdprintf, 2;
}

/// `int vsprintf(char *, const char *, va_list)`
///
/// # Safety
///
/// `buffer` holds the rendering, and the arguments match `format`.
pub unsafe extern "C" fn vsprintf(buffer: *mut c_char, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { rosemary_vsprintf(buffer, format, ap) }
}

/// `int vsnprintf(char *, size_t, const char *, va_list)`
///
/// # Safety
///
/// `buffer` holds `size` bytes, and the arguments match `format`.
pub unsafe extern "C" fn vsnprintf(
    buffer: *mut c_char,
    size: usize,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { rosemary_vsnprintf(buffer, size, format, ap) }
}

/// `int vasprintf(char **, const char *, va_list)`
///
/// # Safety
///
/// As [`vasprintf_into_guest`].
unsafe extern "C" fn vasprintf(out: *mut *mut c_char, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { vasprintf_into_guest(out, format, ap) }
}

/// `int vdprintf(int, const char *, va_list)`
///
/// # Safety
///
/// The arguments match `format`.
unsafe extern "C" fn vdprintf(fd: c_int, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { rosemary_vdprintf(fd, format, ap) }
}

exports::exports! {
    functions {
        "sprintf" => sprintf,
        "vsprintf" => darwin_vsprintf,
        "snprintf" => snprintf,
        "vsnprintf" => darwin_vsnprintf,
        "asprintf" => asprintf,
        "vasprintf" => darwin_vasprintf,
        "dprintf" => dprintf,
        "vdprintf" => darwin_vdprintf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    type Snprintf = unsafe extern "C" fn(*mut c_char, usize, *const c_char, ...) -> c_int;

    /// Called through the published symbol, as a guest would reach it.
    #[cfg(not(all(target_arch = "aarch64", not(target_vendor = "apple"))))]
    fn published_snprintf() -> Snprintf {
        // SAFETY: the export is a variadic function of this shape.
        unsafe { std::mem::transmute(symbol("snprintf").expect("published")) }
    }

    /// The published symbol takes Darwin's convention here, so this host's own
    /// ellipsis reaches the same `va_list` form; the entry is tested below.
    #[cfg(all(target_arch = "aarch64", not(target_vendor = "apple")))]
    fn published_snprintf() -> Snprintf {
        unsafe extern "C" fn host(
            buffer: *mut c_char,
            size: usize,
            format: *const c_char,
            args: ...
        ) -> c_int {
            // SAFETY: the caller's obligations.
            unsafe { vsnprintf(buffer, size, format, args) }
        }
        host
    }

    /// `bash`'s own version string, which is what first showed the host's
    /// registers being read for Darwin's stack.
    #[test]
    #[cfg(target_arch = "aarch64")]
    fn a_darwin_caller_reaches_the_published_entries() {
        let mut buffer = [0 as c_char; 32];
        let tail = [c"3.2".as_ptr() as u64, 57, 1, c"release".as_ptr() as u64];
        let format = c"%s.%d(%d)-%s".as_ptr() as u64;
        let named = [buffer.as_mut_ptr() as u64, buffer.len() as u64, format];
        // SAFETY: a buffer this test sized and a tail matching the format.
        let n = unsafe {
            crate::vararg::call_darwin(symbol("snprintf").expect("published"), &named, &tail)
        };
        assert_eq!(n as c_int, 17);
        // SAFETY: `snprintf` terminated it.
        assert_eq!(
            unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes(),
            b"3.2.57(1)-release"
        );

        // `vsnprintf` with Darwin's `va_list`: a pointer to the same slots.
        let mut again = [0 as c_char; 32];
        let named = [
            again.as_mut_ptr() as u64,
            again.len() as u64,
            format,
            tail.as_ptr() as u64,
        ];
        // SAFETY: as above, with the slots passed by address.
        let n = unsafe {
            crate::vararg::call_darwin(symbol("vsnprintf").expect("published"), &named, &[])
        };
        assert_eq!(n as c_int, 17);
        // SAFETY: as above.
        assert_eq!(
            unsafe { CStr::from_ptr(again.as_ptr()) }.to_bytes(),
            b"3.2.57(1)-release"
        );
    }

    /// Darwin's arm64 `long double` is `double`, so `%Lf` reads one slot.
    #[test]
    #[cfg(all(target_arch = "aarch64", not(target_vendor = "apple")))]
    fn a_long_double_conversion_reads_darwins_double() {
        let mut buffer = [0 as c_char; 32];
        let named = [
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            c"%.2Lf|%d".as_ptr() as u64,
        ];
        // SAFETY: a buffer this test sized and a tail matching the format.
        let n = unsafe {
            crate::vararg::call_darwin(
                symbol("snprintf").expect("published"),
                &named,
                &[2.5f64.to_bits(), 7],
            )
        };
        // SAFETY: `snprintf` terminated it.
        assert_eq!(
            unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes(),
            b"2.50|7"
        );
        assert_eq!(n as c_int, 6);
    }

    fn render_double(format: &CStr, value: f64) -> String {
        let mut buffer = [0 as c_char; 64];
        // SAFETY: a buffer this test sized, a double for the first conversion
        // and a zero for a `%d` after it, which would otherwise read an
        // argument nobody passed.
        let n = unsafe {
            published_snprintf()(buffer.as_mut_ptr(), buffer.len(), format.as_ptr(), value, 0)
        };
        // SAFETY: `snprintf` terminated it.
        let text = unsafe { CStr::from_ptr(buffer.as_ptr()) };
        assert_eq!(n as usize, text.to_bytes().len());
        text.to_str().expect("ASCII").to_owned()
    }

    #[test]
    fn a_nan_prints_without_a_sign() {
        let negative = -f64::NAN;
        assert_eq!(render_double(c"%f", negative), "nan");
        assert_eq!(render_double(c"%E", negative), "NAN");
        assert_eq!(render_double(c"[%+6g]", f64::NAN), "[   nan]");
        assert_eq!(render_double(c"[% -5a]", negative), "[nan  ]");
        assert_eq!(render_double(c"%.3f|%d", negative), "nan|0");
    }

    #[test]
    fn a_signed_infinity_keeps_its_sign() {
        assert_eq!(render_double(c"%+f", f64::INFINITY), "+inf");
        assert_eq!(render_double(c"%f", f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn the_other_conversions_around_a_nan_are_unchanged() {
        let mut buffer = [0 as c_char; 128];
        let mut count = 0 as c_int;
        // SAFETY: a buffer this test sized and arguments matching the format.
        let n = unsafe {
            published_snprintf()(
                buffer.as_mut_ptr(),
                buffer.len(),
                c"%s %5.2f %*.*f %ld %c %% %n%-4x|".as_ptr(),
                c"pi".as_ptr(),
                2.5f64,
                7 as c_int,
                1 as c_int,
                -f64::NAN,
                -42i64,
                b'z' as c_int,
                &raw mut count,
                255 as c_int,
            )
        };
        // SAFETY: `snprintf` terminated it.
        let text = unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes();
        assert_eq!(text, b"pi  2.50     nan -42 z % ff  |");
        assert_eq!(n as usize, text.len());
        assert_eq!(count, 25);
    }

    #[test]
    fn positional_arguments_reach_their_conversions() {
        let mut buffer = [0 as c_char; 64];
        // SAFETY: as above, with positions naming each argument.
        unsafe {
            published_snprintf()(
                buffer.as_mut_ptr(),
                buffer.len(),
                c"%2$s=%1$*3$f".as_ptr(),
                -f64::NAN,
                c"x".as_ptr(),
                5 as c_int,
            );
            assert_eq!(CStr::from_ptr(buffer.as_ptr()).to_bytes(), b"x=  nan");
        }
    }

    #[test]
    fn a_truncated_rendering_reports_the_whole_length() {
        let mut buffer = [0 as c_char; 4];
        // SAFETY: a buffer of four bytes, described as four bytes.
        let n = unsafe {
            published_snprintf()(
                buffer.as_mut_ptr(),
                buffer.len(),
                c"%f!".as_ptr(),
                -f64::NAN,
            )
        };
        assert_eq!(n, 4);
        // SAFETY: `snprintf` terminated it within the four bytes.
        assert_eq!(
            unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes(),
            b"nan"
        );
    }

    #[test]
    fn asprintf_hands_back_a_nan_without_a_sign() {
        #[cfg(not(all(target_arch = "aarch64", not(target_vendor = "apple"))))]
        let asprintf = {
            type Asprintf = unsafe extern "C" fn(*mut *mut c_char, *const c_char, ...) -> c_int;
            // SAFETY: the export is a variadic function of this shape.
            let f: Asprintf =
                unsafe { std::mem::transmute(symbol("asprintf").expect("published")) };
            f
        };
        // The published entry takes Darwin's convention here; see above.
        #[cfg(all(target_arch = "aarch64", not(target_vendor = "apple")))]
        unsafe extern "C" fn asprintf(
            out: *mut *mut c_char,
            format: *const c_char,
            args: ...
        ) -> c_int {
            // SAFETY: the caller's obligations.
            unsafe { vasprintf(out, format, args) }
        }
        let mut out: *mut c_char = std::ptr::null_mut();
        // SAFETY: a writable slot and one double for one conversion.
        let n = unsafe { asprintf(&raw mut out, c"[%g]".as_ptr(), -f64::NAN) };
        assert_eq!(n, 5);
        // SAFETY: `asprintf` allocated and terminated it.
        unsafe {
            assert_eq!(CStr::from_ptr(out).to_bytes(), b"[nan]");
            crate::heap::free(out.cast());
        }
    }
}
