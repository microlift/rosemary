use std::ffi::{c_char, c_void};

/// Compare `len` bytes without stopping at the first difference.
///
/// # Safety
///
/// Both pointers must be readable for `len` bytes, unless `len` is zero.
pub unsafe extern "C" fn timingsafe_bcmp(a: *const c_void, b: *const c_void, len: usize) -> i32 {
    let a = a.cast::<u8>();
    let b = b.cast::<u8>();
    let mut difference = 0u8;
    for offset in 0..len {
        // Volatile reads keep the full comparison observable to the optimizer.
        difference |= unsafe { a.add(offset).read_volatile() ^ b.add(offset).read_volatile() };
    }
    ((difference as u32).wrapping_neg() >> 31) as i32
}

/// Fill `len` bytes at `b` by repeating the `n`-byte pattern at `pattern`.
///
/// A trailing partial copy is written, which is what Apple's does: the length is
/// in bytes and is not required to be a multiple of the pattern.
///
/// # Safety
///
/// `b` is `len` writable bytes and `pattern` is `n` readable ones; the two do
/// not overlap.
unsafe fn fill(b: *mut c_void, pattern: *const c_void, n: usize, len: usize) {
    if b.is_null() || pattern.is_null() || len == 0 {
        return;
    }
    let dst = b.cast::<u8>();
    let src = pattern.cast::<u8>();
    let mut written = 0;
    while written < len {
        let step = n.min(len - written);
        // SAFETY: `written + step <= len`, so the destination is inside the
        // caller's buffer, and `step <= n`, so the source is inside the pattern.
        // The caller states they do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(src, dst.add(written), step) };
        written += step;
    }
}

/// `void memset_pattern4(void *b, const void *pattern4, size_t len)`
///
/// # Safety
///
/// As [`fill`], with a four-byte pattern.
pub unsafe extern "C" fn memset_pattern4(b: *mut c_void, pattern: *const c_void, len: usize) {
    // SAFETY: the caller's own buffer and a four-byte pattern.
    unsafe { fill(b, pattern, 4, len) };
}

/// `void memset_pattern8(void *b, const void *pattern8, size_t len)`
///
/// # Safety
///
/// As [`fill`], with an eight-byte pattern.
pub unsafe extern "C" fn memset_pattern8(b: *mut c_void, pattern: *const c_void, len: usize) {
    // SAFETY: the caller's own buffer and an eight-byte pattern.
    unsafe { fill(b, pattern, 8, len) };
}

/// `void memset_pattern16(void *b, const void *pattern16, size_t len)`
///
/// # Safety
///
/// As [`fill`], with a sixteen-byte pattern.
pub unsafe extern "C" fn memset_pattern16(b: *mut c_void, pattern: *const c_void, len: usize) {
    // SAFETY: the caller's own buffer and a sixteen-byte pattern.
    unsafe { fill(b, pattern, 16, len) };
}

/// `char *strerror(int errnum)`
///
/// Wrapped only for the number: `errnum` is the guest's, and the guest's are not
/// this host's above 34. Handing this host's `strerror` a Darwin `ENOTSUP` (45)
/// gets "Level 2 not synchronized", which is a real message about a real error
/// and the wrong one. See [`crate::errno`].
pub unsafe extern "C" fn strerror(errnum: i32) -> *mut c_char {
    // SAFETY: no caller memory is touched; the string is the host library's own,
    // valid until this thread calls `strerror` again, which is Darwin's contract
    // for it too.
    unsafe { libc::strerror(crate::errno::to_host(errnum)) }
}

/// `int strerror_r(int errnum, char *buf, size_t buflen)`
///
/// Two divergences at once. Darwin's is POSIX's: it writes the message into the
/// caller's buffer and returns 0, `EINVAL` for a number it does not know or
/// `ERANGE` for a buffer too small. glibc's `strerror_r` is the GNU one - it
/// *returns* the message and is entitled to leave the buffer untouched - so a
/// guest that prints its buffer after the call prints whatever was there before,
/// which is uninitialised stack rather than an error. glibc does publish the
/// POSIX form, under `__xpg_strerror_r`, which is what `libc::strerror_r` binds
/// to here.
///
/// And `errnum` is a guest's error number, translated as in [`strerror`].
///
/// # Safety
///
/// `buf` is `buflen` writable bytes, or `buflen` is zero.
pub unsafe extern "C" fn strerror_r(errnum: i32, buf: *mut c_char, buflen: usize) -> i32 {
    if buf.is_null() || buflen == 0 {
        return libc::ERANGE;
    }
    // SAFETY: the caller's buffer, with the length they gave for it.
    unsafe { libc::strerror_r(crate::errno::to_host(errnum), buf, buflen) }
}

/// `char *strnstr(const char *big, const char *little, size_t len)`
///
/// BSD's, which glibc lacks: find `little` in at most the first `len` bytes of
/// `big`, stopping early at its terminator.
///
/// # Safety
///
/// Both are NUL-terminated strings, or `big` is readable for `len` bytes.
pub unsafe extern "C" fn strnstr(
    big: *const c_char,
    little: *const c_char,
    len: usize,
) -> *mut c_char {
    let needle = unsafe { std::ffi::CStr::from_ptr(little) }.to_bytes();
    if needle.is_empty() {
        return big.cast_mut();
    }
    // SAFETY: stops at the terminator or `len`, whichever comes first.
    let haystack_len = unsafe { libc::strnlen(big, len) };
    let haystack = unsafe { std::slice::from_raw_parts(big.cast::<u8>(), haystack_len) };
    match haystack.windows(needle.len()).position(|w| w == needle) {
        Some(at) => unsafe { big.add(at).cast_mut() },
        None => std::ptr::null_mut(),
    }
}

exports::exports! {
    functions {
        "strnstr" => strnstr,
        "timingsafe_bcmp" => timingsafe_bcmp,
        "memset_pattern4" => memset_pattern4,
        "memset_pattern8" => memset_pattern8,
        "memset_pattern16" => memset_pattern16,
        "strerror" => strerror,
        "strerror_r" => strerror_r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strnstr_searches_only_the_first_len_bytes() {
        let big = c"Foo Bar Baz";
        let found = |little: &std::ffi::CStr, len| {
            let at = unsafe { strnstr(big.as_ptr(), little.as_ptr(), len) };
            (!at.is_null()).then(|| at as usize - big.as_ptr() as usize)
        };
        assert_eq!(found(c"Bar", 11), Some(4));
        assert_eq!(found(c"Bar", 7), Some(4));
        assert_eq!(found(c"Bar", 6), None);
        assert_eq!(found(c"Baz", 100), Some(8));
        assert_eq!(found(c"", 0), Some(0));
    }

    #[test]
    fn timingsafe_bcmp_reports_equality_and_difference() {
        let mut implementations: Vec<
            unsafe extern "C" fn(*const c_void, *const c_void, usize) -> i32,
        > = vec![timingsafe_bcmp];
        if crate::libc_backend::using_builtin() {
            let addr = crate::symbol("_timingsafe_bcmp").expect("timingsafe_bcmp is exported");
            // SAFETY: the exported function has this prototype. Function addresses
            // can differ across codegen units, so check its behavior instead.
            implementations.push(unsafe {
                std::mem::transmute::<
                    *const (),
                    unsafe extern "C" fn(*const c_void, *const c_void, usize) -> i32,
                >(addr as usize as *const ())
            });
        }
        let left = b"abcdef";
        let same = b"abcdef";
        let first = b"zbcdef";
        let last = b"abcdez";
        for compare in implementations {
            for (right, expected) in [(same, 0), (first, 1), (last, 1)] {
                // SAFETY: both arrays contain six readable bytes.
                let result =
                    unsafe { compare(left.as_ptr().cast(), right.as_ptr().cast(), left.len()) };
                assert_eq!(result, expected);
            }
            // SAFETY: a zero-length comparison reads neither pointer.
            assert_eq!(unsafe { compare(std::ptr::null(), std::ptr::null(), 0) }, 0);
        }
    }

    fn filled(n: usize, pattern: &[u8], len: usize) -> Vec<u8> {
        let mut buf = vec![0xAAu8; len + 8];
        // SAFETY: `buf` has `len` writable bytes at its start and `pattern` has
        // `n`; they are separate allocations.
        unsafe { fill(buf.as_mut_ptr().cast(), pattern.as_ptr().cast(), n, len) };
        buf
    }

    #[test]
    fn the_pattern_repeats_across_the_buffer() {
        let out = filled(4, b"abcd", 12);
        assert_eq!(&out[..12], b"abcdabcdabcd");
        assert!(out[12..].iter().all(|b| *b == 0xAA), "wrote past `len`");
    }

    /// The length is in bytes and need not be a multiple of the pattern; the
    /// last copy is cut short rather than rounded up.
    #[test]
    fn a_partial_final_copy_is_written_and_nothing_beyond_it() {
        let out = filled(16, b"0123456789abcdef", 20);
        assert_eq!(&out[..20], b"0123456789abcdef0123");
        assert!(out[20..].iter().all(|b| *b == 0xAA), "wrote past `len`");
    }

    #[test]
    fn a_zero_length_writes_nothing() {
        let out = filled(8, b"01234567", 0);
        assert!(out.iter().all(|b| *b == 0xAA));
    }

    /// The property glibc's own `strerror_r` does not have: the message ends up
    /// in the caller's buffer, and the return is the status rather than a
    /// pointer.
    #[test]
    fn strerror_r_fills_the_callers_buffer() {
        let mut buf = [0u8; 128];
        // SAFETY: 128 writable bytes, and the length matches.
        let rc = unsafe { strerror_r(libc::ENOENT, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(rc, 0);
        let text = std::ffi::CStr::from_bytes_until_nul(&buf).expect("a C string");
        assert!(
            !text.to_bytes().is_empty(),
            "the buffer was left untouched, which is the GNU behaviour"
        );
    }

    #[test]
    fn a_buffer_too_small_is_reported_rather_than_overrun() {
        let mut buf = [0u8; 2];
        // SAFETY: two writable bytes, and the length matches.
        let rc = unsafe { strerror_r(libc::ENOENT, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(rc, libc::ERANGE);
    }

    /// The published entry points, through the addresses a guest is handed.
    #[test]
    fn the_exported_entry_points_are_the_ones_they_claim_to_be() {
        for (name, n) in [
            ("memset_pattern4", 4usize),
            ("memset_pattern8", 8),
            ("memset_pattern16", 16),
        ] {
            let addr = symbol(name).expect(name);
            // SAFETY: each of the three has this prototype.
            let f: unsafe extern "C" fn(*mut c_void, *const c_void, usize) =
                unsafe { std::mem::transmute(addr as usize as *const ()) };
            let pattern = vec![b'z'; n];
            let mut buf = vec![0u8; n * 3];
            // SAFETY: `buf` is `n * 3` writable bytes and `pattern` is `n`.
            unsafe { f(buf.as_mut_ptr().cast(), pattern.as_ptr().cast(), buf.len()) };
            assert!(buf.iter().all(|b| *b == b'z'), "{name}");
        }
    }
}
