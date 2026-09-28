#![cfg(not(target_os = "macos"))]

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

/// `void perror(const char *s)`
///
/// Reads the *guest's* `errno`, which is a different slot holding a different
/// numbering from the one this host's `perror` would read. Written out here
/// rather than forwarded, because forwarding it would print this host's last
/// error rather than the guest's.
///
/// # Safety
///
/// `s` is NUL-terminated or null.
pub unsafe extern "C" fn perror(s: *const c_char) {
    let errnum = crate::errno::to_host(crate::errno::guest_errno());
    // SAFETY: the host's own static message for a number it knows.
    let message = unsafe { std::ffi::CStr::from_ptr(libc::strerror(errnum)) };
    let prefix = match s.is_null() {
        true => None,
        false => Some(unsafe { std::ffi::CStr::from_ptr(s) }),
    };
    let line = match prefix.map(|p| p.to_bytes()).filter(|p| !p.is_empty()) {
        Some(p) => format!(
            "{}: {}\n",
            String::from_utf8_lossy(p),
            String::from_utf8_lossy(message.to_bytes())
        ),
        None => format!("{}\n", String::from_utf8_lossy(message.to_bytes())),
    };
    // SAFETY: a buffer this function owns, written to the descriptor Darwin's
    // `perror` writes to. `write` rather than `fputs(stderr)` because the guest's
    // `stderr` is `crate::stdio`'s and this is below it.
    unsafe {
        libc::write(2, line.as_ptr().cast(), line.len());
    }
}

exports::exports! {
    functions {
        "timingsafe_bcmp" => timingsafe_bcmp,
        "memset_pattern4" => memset_pattern4,
        "memset_pattern8" => memset_pattern8,
        "memset_pattern16" => memset_pattern16,
        "strerror" => strerror,
        "perror" => perror,
        "strerror_r" => strerror_r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timingsafe_bcmp_reports_equality_and_difference() {
        let left = b"abcdef";
        let same = b"abcdef";
        let first = b"zbcdef";
        let last = b"abcdez";
        for (right, expected) in [(same, 0), (first, 1), (last, 1)] {
            // SAFETY: both arrays contain six readable bytes.
            let result =
                unsafe { timingsafe_bcmp(left.as_ptr().cast(), right.as_ptr().cast(), left.len()) };
            assert_eq!(result, expected);
        }
        // SAFETY: a zero-length comparison reads neither pointer.
        assert_eq!(
            unsafe { timingsafe_bcmp(std::ptr::null(), std::ptr::null(), 0) },
            0
        );
        assert_eq!(
            crate::symbol("_timingsafe_bcmp"),
            Some(timingsafe_bcmp as *const () as usize as u64)
        );
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
