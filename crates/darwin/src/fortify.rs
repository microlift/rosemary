use std::ffi::{c_char, c_int, c_void};

use libc::size_t;

#[inline(always)]
fn checked(len: usize, capacity: usize) {
    if len > capacity {
        unsafe { libc::abort() }
    }
}

unsafe extern "C" fn memcpy_chk(
    dst: *mut c_void,
    src: *const c_void,
    len: usize,
    capacity: usize,
) -> *mut c_void {
    checked(len, capacity);
    unsafe { std::ptr::copy_nonoverlapping(src.cast::<u8>(), dst.cast::<u8>(), len) };
    dst
}

unsafe extern "C" fn memmove_chk(
    dst: *mut c_void,
    src: *const c_void,
    len: usize,
    capacity: usize,
) -> *mut c_void {
    checked(len, capacity);
    unsafe { std::ptr::copy(src.cast::<u8>(), dst.cast::<u8>(), len) };
    dst
}

unsafe extern "C" fn memset_chk(
    dst: *mut c_void,
    byte: c_int,
    len: usize,
    capacity: usize,
) -> *mut c_void {
    checked(len, capacity);
    unsafe { std::ptr::write_bytes(dst, byte as u8, len) };
    dst
}

unsafe fn c_len(s: *const c_char) -> usize {
    unsafe { libc::strlen(s) }
}

unsafe extern "C" fn strcpy_chk(
    dst: *mut c_char,
    src: *const c_char,
    capacity: usize,
) -> *mut c_char {
    let len = unsafe { c_len(src) } + 1;
    checked(len, capacity);
    unsafe { std::ptr::copy_nonoverlapping(src, dst, len) };
    dst
}

unsafe extern "C" fn strcat_chk(
    dst: *mut c_char,
    src: *const c_char,
    capacity: usize,
) -> *mut c_char {
    let used = unsafe { c_len(dst) };
    let added = unsafe { c_len(src) };
    checked(used.saturating_add(added).saturating_add(1), capacity);
    unsafe { std::ptr::copy_nonoverlapping(src, dst.add(used), added + 1) };
    dst
}

unsafe extern "C" fn strncpy_chk(
    dst: *mut c_char,
    src: *const c_char,
    len: usize,
    capacity: usize,
) -> *mut c_char {
    checked(len, capacity);
    // strncpy copies at most len bytes and zero-pads the remainder of the
    // destination, so it never reads past the source terminator.
    let copied = unsafe { libc::strnlen(src, len) };
    unsafe {
        std::ptr::copy_nonoverlapping(src, dst, copied);
        std::ptr::write_bytes(dst.add(copied), 0, len - copied);
    }
    dst
}

unsafe extern "C" fn strlcpy_chk(
    dst: *mut c_char,
    src: *const c_char,
    size: usize,
    capacity: usize,
) -> usize {
    checked(size, capacity);
    let len = unsafe { c_len(src) };
    if size != 0 {
        let copied = len.min(size - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(src, dst, copied);
            *dst.add(copied) = 0;
        }
    }
    len
}

unsafe extern "C" fn strlcat_chk(
    dst: *mut c_char,
    src: *const c_char,
    size: usize,
    capacity: usize,
) -> usize {
    checked(size, capacity);
    let src_len = unsafe { c_len(src) };
    let dst_len = unsafe { libc::strnlen(dst, size) };
    if dst_len == size {
        return size.saturating_add(src_len);
    }
    let available = size - dst_len;
    if available != 0 {
        let copied = src_len.min(available - 1);
        unsafe {
            std::ptr::copy_nonoverlapping(src, dst.add(dst_len), copied);
            *dst.add(dst_len + copied) = 0;
        }
    }
    dst_len + src_len
}

unsafe extern "C" {
    fn rosemary_sprintf_chk(s: *mut c_char, flags: c_int, slen: size_t, format: *const c_char, ...);
    fn rosemary_vsprintf_chk(
        s: *mut c_char,
        flags: c_int,
        slen: size_t,
        format: *const c_char,
        ap: std::ffi::VaList,
    ) -> c_int;
    fn rosemary_snprintf_chk(
        s: *mut c_char,
        maxlen: size_t,
        flags: c_int,
        slen: size_t,
        format: *const c_char,
        ...
    ) -> c_int;
    fn rosemary_vsnprintf_chk(
        s: *mut c_char,
        maxlen: size_t,
        flags: c_int,
        slen: size_t,
        format: *const c_char,
        ap: std::ffi::VaList,
    ) -> c_int;
}

exports::exports! {
    functions {
        "__memcpy_chk" => memcpy_chk,
        "__memmove_chk" => memmove_chk,
        "__memset_chk" => memset_chk,
        "__sprintf_chk" => rosemary_sprintf_chk,
        "__snprintf_chk" => rosemary_snprintf_chk,
        "__vsprintf_chk" => rosemary_vsprintf_chk,
        "__vsnprintf_chk" => rosemary_vsnprintf_chk,
        "__strcat_chk" => strcat_chk,
        "__strcpy_chk" => strcpy_chk,
        "__strlcat_chk" => strlcat_chk,
        "__strlcpy_chk" => strlcpy_chk,
        "__strncpy_chk" => strncpy_chk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_string_functions_have_darwin_results() {
        let mut dst = [0i8; 8];
        unsafe {
            assert_eq!(strlcpy_chk(dst.as_mut_ptr(), c"abcdef".as_ptr(), 4, 8), 6);
            assert_eq!(std::ffi::CStr::from_ptr(dst.as_ptr()).to_bytes(), b"abc");
            assert_eq!(strlcat_chk(dst.as_mut_ptr(), c"XYZ".as_ptr(), 8, 8), 6);
            assert_eq!(std::ffi::CStr::from_ptr(dst.as_ptr()).to_bytes(), b"abcXYZ");
        }
    }

    #[test]
    fn strncpy_chk_pads_the_destination() {
        let mut dst = [b'!' as c_char; 8];
        unsafe {
            assert_eq!(
                strncpy_chk(dst.as_mut_ptr(), c"ab".as_ptr(), 6, 8),
                dst.as_mut_ptr()
            );
        }
        assert_eq!(
            dst,
            [
                b'a' as c_char,
                b'b' as c_char,
                0,
                0,
                0,
                0,
                b'!' as c_char,
                b'!' as c_char
            ]
        );
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn sprintf_chk_forwards_variadic_arguments() {
        type Sprintf = unsafe extern "C" fn(*mut c_char, c_int, usize, *const c_char, ...) -> c_int;
        let function: Sprintf =
            unsafe { std::mem::transmute(symbol("__sprintf_chk").expect("published") as usize) };
        let mut dst = [0i8; 32];
        let written = unsafe {
            function(
                dst.as_mut_ptr(),
                0,
                dst.len(),
                c"%s:%d".as_ptr(),
                c"ok".as_ptr(),
                42,
            )
        };
        assert_eq!(written, 5);
        assert_eq!(
            unsafe { std::ffi::CStr::from_ptr(dst.as_ptr()) }.to_bytes(),
            b"ok:42"
        );
    }
}
