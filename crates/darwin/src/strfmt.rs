//! The wide string formatters left to the host's C library, `swprintf` and
//! `swscanf`. Neither touches a stream or a value whose meaning differs, so
//! each is the host's, reached through a `va_list` - with the format's
//! `long double`s narrowed where Darwin's is the host's `double`. `sscanf` is
//! [`crate::stdio`]'s scanner, with the stream one.

use std::ffi::{VaList, c_int};

use crate::vararg::{LONG_DOUBLE_DIFFERS, narrow_long_double};

mod host {
    use std::ffi::{VaList, c_int};

    unsafe extern "C" {
        pub fn vswscanf(input: *const u32, format: *const u32, ap: VaList) -> c_int;
        pub fn vswprintf(out: *mut u32, n: usize, format: *const u32, ap: VaList) -> c_int;
    }
}

/// Run `f` with `format`, narrowed if this host needs it.
///
/// # Safety
///
/// `format` is NUL-terminated.
unsafe fn with_format<T: Copy + Into<u32> + From<u8>, R>(
    format: *const T,
    f: impl FnOnce(*const T) -> R,
) -> R {
    // SAFETY: the caller's NUL-terminated format.
    match LONG_DOUBLE_DIFFERS
        .then(|| unsafe { narrow_long_double(format) })
        .flatten()
    {
        Some(narrowed) => f(narrowed.as_ptr()),
        None => f(format),
    }
}

/// `int vswscanf(const wchar_t *, const wchar_t *, va_list)`
///
/// # Safety
///
/// As [`vsscanf`].
unsafe extern "C" fn vswscanf(input: *const u32, format: *const u32, ap: VaList) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { with_format(format, |format| host::vswscanf(input, format, ap)) }
}

/// `int vswprintf(wchar_t *, size_t, const wchar_t *, va_list)`
///
/// # Safety
///
/// `out` holds `n` wide characters and `ap` matches `format`.
pub unsafe extern "C" fn vswprintf(
    out: *mut u32,
    n: usize,
    format: *const u32,
    ap: VaList,
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { with_format(format, |format| host::vswprintf(out, n, format, ap)) }
}

crate::variadic! {
    /// `int swscanf(const wchar_t *, const wchar_t *, ...)`
    fn swscanf(input: *const u32, format: *const u32) -> c_int => vswscanf;
    /// `int swprintf(wchar_t *, size_t, const wchar_t *, ...)`
    fn swprintf(out: *mut u32, n: usize, format: *const u32) -> c_int => vswprintf;
}

crate::va_list! {
    darwin_vswscanf = vswscanf, 2;
    darwin_vswprintf = vswprintf, 3;
}

exports::exports! {
    functions {
        "sscanf" => crate::stdio::sscanf,
        "vsscanf" => crate::stdio::darwin_vsscanf,
        "swscanf" => swscanf,
        "vswscanf" => darwin_vswscanf,
        "swprintf" => swprintf,
        "vswprintf" => darwin_vswprintf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_arch = "aarch64")]
    use std::ffi::c_char;

    fn narrowed(format: &std::ffi::CStr) -> Option<String> {
        // SAFETY: a NUL-terminated literal.
        let out = unsafe { narrow_long_double(format.as_ptr().cast::<u8>()) }?;
        Some(String::from_utf8(out[..out.len() - 1].to_vec()).expect("ASCII"))
    }

    #[test]
    fn a_long_double_conversion_becomes_a_double_one() {
        assert_eq!(
            narrowed(c"%Lf %5.2Lg %1$*2$LA").as_deref(),
            Some("%lf %5.2lg %1$*2$lA")
        );
    }

    #[test]
    fn other_uses_of_l_are_left_alone() {
        assert_eq!(narrowed(c"%Ld %%Lf %[Lf] %[]Lf] L"), None);
    }

    #[test]
    #[cfg(target_arch = "aarch64")]
    fn sscanf_reads_a_long_double_as_darwins_double() {
        let mut value = 0f64;
        // SAFETY: one `double *`, which is what Darwin's `%Lf` stores through.
        let n = unsafe { published_sscanf(c"2.5".as_ptr(), c"%Lf".as_ptr(), &raw mut value) };
        assert_eq!((n, value), (1, 2.5));
    }

    /// `sscanf` as a guest calls it, with one pointer.
    #[cfg(target_arch = "aarch64")]
    unsafe fn published_sscanf(input: *const c_char, format: *const c_char, at: *mut f64) -> c_int {
        let address = symbol("sscanf").expect("published");
        #[cfg(all(target_arch = "aarch64", not(target_vendor = "apple")))]
        // SAFETY: two named pointers and one pointer in the tail.
        return unsafe {
            crate::vararg::call_darwin(address, &[input as u64, format as u64], &[at as u64])
        } as c_int;
        #[cfg(not(all(target_arch = "aarch64", not(target_vendor = "apple"))))]
        {
            type Sscanf = unsafe extern "C" fn(*const c_char, *const c_char, ...) -> c_int;
            // SAFETY: the export is a variadic function of this shape.
            let f: Sscanf = unsafe { std::mem::transmute(address as usize) };
            // SAFETY: as above.
            unsafe { f(input, format, at) }
        }
    }
}
