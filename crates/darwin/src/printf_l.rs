use std::ffi::{VaList, c_char, c_int};

use crate::sprintf::{rosemary_vsnprintf, vasprintf_into_guest};

/// `int vsnprintf_l(char *, size_t, locale_t, const char *, va_list)`
///
/// # Safety
///
/// `buffer` holds `size` bytes, `locale` is null or a `locale_t` this process
/// made, and the arguments match `format`.
unsafe extern "C" fn vsnprintf_l(
    buffer: *mut c_char,
    size: usize,
    locale: libc::locale_t,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    let _switched = crate::xlocale::switch(locale);
    // SAFETY: the caller's buffer, format and arguments.
    unsafe { rosemary_vsnprintf(buffer, size, format, ap) }
}

/// `int vasprintf_l(char **, locale_t, const char *, va_list)`
///
/// The result is in the guest's allocator, as [`vasprintf_into_guest`] says.
///
/// # Safety
///
/// As [`vsnprintf_l`].
unsafe extern "C" fn vasprintf_l(
    out: *mut *mut c_char,
    locale: libc::locale_t,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    let _switched = crate::xlocale::switch(locale);
    // SAFETY: the caller's obligations.
    unsafe { vasprintf_into_guest(out, format, ap) }
}

crate::variadic! {
    /// `int snprintf_l(char *, size_t, locale_t, const char *, ...)`
    fn snprintf_l(
        buffer: *mut c_char,
        size: usize,
        locale: libc::locale_t,
        format: *const c_char,
    ) -> c_int => vsnprintf_l;
    /// `int asprintf_l(char **, locale_t, const char *, ...)`
    fn asprintf_l(out: *mut *mut c_char, locale: libc::locale_t, format: *const c_char) -> c_int
        => vasprintf_l;
}

crate::va_list! {
    darwin_vsnprintf_l = vsnprintf_l, 4;
    darwin_vasprintf_l = vasprintf_l, 3;
}

exports::exports! {
    functions {
        "snprintf_l" => snprintf_l,
        "sscanf_l" => crate::stdio::sscanf_l,
        "asprintf_l" => asprintf_l,
        "vsnprintf_l" => darwin_vsnprintf_l,
        "vsscanf_l" => crate::stdio::darwin_vsscanf_l,
        "vasprintf_l" => darwin_vasprintf_l,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    type SnprintfL =
        unsafe extern "C" fn(*mut c_char, usize, libc::locale_t, *const c_char, ...) -> c_int;

    /// Called through the published symbol, as a guest would reach it.
    #[cfg(not(all(target_arch = "aarch64", not(target_vendor = "apple"))))]
    fn published_snprintf_l() -> SnprintfL {
        // SAFETY: the export is a variadic function of this shape.
        unsafe { std::mem::transmute(symbol("snprintf_l").expect("published")) }
    }

    /// The published symbol takes Darwin's convention here, so this host's own
    /// ellipsis reaches the same `va_list` form; the entry has a test of its own.
    #[cfg(all(target_arch = "aarch64", not(target_vendor = "apple")))]
    fn published_snprintf_l() -> SnprintfL {
        unsafe extern "C" fn host(
            buffer: *mut c_char,
            size: usize,
            locale: libc::locale_t,
            format: *const c_char,
            args: ...
        ) -> c_int {
            // SAFETY: the caller's obligations.
            unsafe { vsnprintf_l(buffer, size, locale, format, args) }
        }
        host
    }

    /// Nine integer arguments, as Darwin passes them: the four named ones in
    /// registers and every variadic one on the stack.
    #[test]
    #[cfg(target_arch = "aarch64")]
    fn a_darwin_caller_reaches_the_published_entry() {
        let mut buffer = [0 as c_char; 64];
        let address = symbol("snprintf_l").expect("published");
        let named = [
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
            c"%d %s %.1f".as_ptr() as u64,
        ];
        let tail = [42, c"x".as_ptr() as u64, 1.5f64.to_bits()];
        // SAFETY: a buffer this test sized, and a tail matching the format.
        let written = unsafe { crate::vararg::call_darwin(address, &named, &tail) };
        assert_eq!(written as c_int, 8);
        // SAFETY: `vsnprintf` terminated it.
        assert_eq!(
            unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_bytes(),
            b"42 x 1.5"
        );
    }

    /// Six integer arguments and one past them: the seventh is on the caller's
    /// stack, and reading it is the whole reason this is not a register shuffle.
    #[test]
    fn an_argument_past_the_registers_is_still_read() {
        let mut buffer = [0 as c_char; 64];
        // SAFETY: a buffer this test sized, and a format matching the
        // arguments after it.
        let written = unsafe {
            published_snprintf_l()(
                buffer.as_mut_ptr(),
                buffer.len(),
                std::ptr::null_mut(),
                c"%d %d %d %d %d %d %d".as_ptr(),
                1,
                2,
                3,
                4,
                5,
                6,
                7,
            )
        };
        assert_eq!(written, 13);
        // SAFETY: `vsnprintf` terminated it.
        let text = unsafe { CStr::from_ptr(buffer.as_ptr()) };
        assert_eq!(text.to_bytes(), b"1 2 3 4 5 6 7");
    }

    /// And a floating-point argument, which travels in a vector register and is
    /// read out of the other half of the save area.
    #[test]
    fn a_double_is_read_from_the_vector_registers() {
        let mut buffer = [0 as c_char; 64];
        unsafe {
            published_snprintf_l()(
                buffer.as_mut_ptr(),
                buffer.len(),
                std::ptr::null_mut(),
                c"%s=%.2f".as_ptr(),
                c"x".as_ptr(),
                9.25f64,
            );
            assert_eq!(CStr::from_ptr(buffer.as_ptr()).to_bytes(), b"x=9.25");
        }
    }

    /// The locale is switched for the call and put back after it, which is what
    /// the whole `_l` family is for: the same number formats differently.
    #[test]
    fn the_locale_argument_is_the_one_the_number_is_formatted_in() {
        // SAFETY: a locale this test makes and frees.
        let c_locale = unsafe {
            libc::newlocale(
                crate::locale::all_mask(),
                c"C".as_ptr(),
                std::ptr::null_mut(),
            )
        };
        if c_locale.is_null() {
            eprintln!("skipped: this host has no C locale");
            return;
        }
        let mut buffer = [0 as c_char; 32];
        // SAFETY: as above, with a real locale rather than a null one.
        unsafe {
            let before = libc::uselocale(std::ptr::null_mut());
            published_snprintf_l()(
                buffer.as_mut_ptr(),
                buffer.len(),
                c_locale,
                c"%.1f".as_ptr(),
                0.5f64,
            );
            assert_eq!(CStr::from_ptr(buffer.as_ptr()).to_bytes(), b"0.5");
            assert_eq!(
                libc::uselocale(std::ptr::null_mut()),
                before,
                "the thread is left where it was"
            );
            libc::freelocale(c_locale);
        }
    }
}
