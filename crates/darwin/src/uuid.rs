#![cfg(not(target_os = "macos"))]

use std::ffi::c_char;

/// `uuid_t`: sixteen bytes, on both platforms.
const LEN: usize = 16;

/// UUID generation has no error return; never manufacture a predictable UUID.
fn random() -> [u8; LEN] {
    let mut bytes = [0u8; LEN];
    if getrandom::fill(&mut bytes).is_err() {
        std::process::abort();
    }
    bytes
}

/// `void uuid_generate_random(uuid_t out)`
///
/// A version-4 UUID: sixteen random bytes with the version and variant fields
/// overwritten, which is what makes it recognisable as one rather than as
/// sixteen bytes.
///
/// # Safety
///
/// `out` is sixteen writable bytes.
unsafe extern "C" fn generate_random(out: *mut u8) {
    if out.is_null() {
        return;
    }
    let mut bytes = random();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    // SAFETY: the caller's sixteen bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, LEN) };
}

/// `void uuid_clear(uuid_t)`
///
/// # Safety
///
/// As [`generate_random`].
unsafe extern "C" fn clear(out: *mut u8) {
    if out.is_null() {
        return;
    }
    // SAFETY: the caller's sixteen bytes.
    unsafe { core::ptr::write_bytes(out, 0, LEN) };
}

/// `void uuid_copy(uuid_t dst, const uuid_t src)`
///
/// # Safety
///
/// `dst` is sixteen writable bytes and `src` sixteen readable ones.
unsafe extern "C" fn copy(dst: *mut u8, src: *const u8) {
    if dst.is_null() || src.is_null() {
        return;
    }
    // SAFETY: the caller's two buffers, which the caller states do not overlap
    // any more than `uuid_copy`'s own do.
    unsafe { core::ptr::copy(src, dst, LEN) };
}

/// `int uuid_compare(const uuid_t, const uuid_t)`
///
/// # Safety
///
/// Both are sixteen readable bytes.
unsafe extern "C" fn compare(a: *const u8, b: *const u8) -> i32 {
    if a.is_null() || b.is_null() {
        return 0;
    }
    let (a, b) = unsafe {
        (
            core::slice::from_raw_parts(a, LEN),
            core::slice::from_raw_parts(b, LEN),
        )
    };
    match a.cmp(b) {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    }
}

/// `int uuid_is_null(const uuid_t)`
///
/// # Safety
///
/// `at` is sixteen readable bytes.
unsafe extern "C" fn is_null(at: *const u8) -> i32 {
    if at.is_null() {
        return 1;
    }
    let bytes = unsafe { core::slice::from_raw_parts(at, LEN) };
    i32::from(bytes.iter().all(|b| *b == 0))
}

/// The thirty-six characters plus a NUL that Darwin calls a `uuid_string_t`.
const TEXT_LEN: usize = 37;

/// Write `bytes` into `out` as `8-4-4-4-12` hexadecimal.
///
/// # Safety
///
/// `out` is [`TEXT_LEN`] writable bytes.
unsafe fn format(bytes: &[u8; LEN], out: *mut c_char, upper: bool) {
    const LOWER: &[u8; 16] = b"0123456789abcdef";
    const UPPER: &[u8; 16] = b"0123456789ABCDEF";
    let digits = match upper {
        true => UPPER,
        false => LOWER,
    };
    let mut text = [0u8; TEXT_LEN];
    let mut at = 0;
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            text[at] = b'-';
            at += 1;
        }
        text[at] = digits[usize::from(byte >> 4)];
        text[at + 1] = digits[usize::from(byte & 0xf)];
        at += 2;
    }
    // SAFETY: the caller's buffer, which is `TEXT_LEN` bytes and is written in
    // full including the trailing NUL left by the initialiser.
    unsafe { core::ptr::copy_nonoverlapping(text.as_ptr().cast::<c_char>(), out, TEXT_LEN) };
}

/// `void uuid_unparse_lower(const uuid_t, uuid_string_t out)`
///
/// # Safety
///
/// `at` is sixteen readable bytes and `out` thirty-seven writable ones.
unsafe extern "C" fn unparse_lower(at: *const u8, out: *mut c_char) {
    unsafe { unparse(at, out, false) };
}

/// `void uuid_unparse_upper(const uuid_t, uuid_string_t out)`
///
/// Darwin's plain `uuid_unparse` is this one, not the lower-case one - the
/// opposite of the Linux library's default, and visible to anyone comparing the
/// string against one written earlier.
///
/// # Safety
///
/// As [`unparse_lower`].
unsafe extern "C" fn unparse_upper(at: *const u8, out: *mut c_char) {
    unsafe { unparse(at, out, true) };
}

/// # Safety
///
/// As [`unparse_lower`].
unsafe fn unparse(at: *const u8, out: *mut c_char, upper: bool) {
    if at.is_null() || out.is_null() {
        return;
    }
    let mut bytes = [0u8; LEN];
    // SAFETY: the caller's sixteen readable bytes.
    unsafe { core::ptr::copy_nonoverlapping(at, bytes.as_mut_ptr(), LEN) };
    // SAFETY: the caller's thirty-seven writable bytes.
    unsafe { format(&bytes, out, upper) };
}

/// `int uuid_parse(const uuid_string_t in, uuid_t out)`
///
/// `-1` for anything that is not thirty-six characters in the `8-4-4-4-12`
/// shape, which is what both platforms' do.
///
/// # Safety
///
/// `text` is NUL-terminated and `out` is sixteen writable bytes.
unsafe extern "C" fn parse(text: *const c_char, out: *mut u8) -> i32 {
    if text.is_null() || out.is_null() {
        return -1;
    }
    let text = unsafe { core::ffi::CStr::from_ptr(text) }.to_bytes();
    if text.len() != TEXT_LEN - 1 {
        return -1;
    }
    let mut bytes = [0u8; LEN];
    let mut at = 0;
    let mut written = 0;
    for i in 0..LEN {
        if matches!(i, 4 | 6 | 8 | 10) {
            if text[at] != b'-' {
                return -1;
            }
            at += 1;
        }
        let (Some(hi), Some(lo)) = (
            (text[at] as char).to_digit(16),
            (text[at + 1] as char).to_digit(16),
        ) else {
            return -1;
        };
        bytes[written] = ((hi << 4) | lo) as u8;
        written += 1;
        at += 2;
    }
    // SAFETY: the caller's sixteen writable bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, LEN) };
    0
}

/// `int gethostuuid(uuid_t id, const struct timespec *wait)`
///
/// The host's machine id, which like Darwin's hardware UUID is the same for
/// every process on the machine and across boots. `wait` bounds Darwin's wait
/// for the kernel to learn the UUID; here it is known or it is not, and a
/// host without one fails as a Darwin wait that ran out does.
///
/// # Safety
///
/// `out` is sixteen writable bytes.
unsafe extern "C" fn gethostuuid(out: *mut u8, _wait: *const libc::timespec) -> i32 {
    if out.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let Some(bytes) = starnix::system::host_uuid() else {
        crate::errno::set(libc::EWOULDBLOCK);
        return -1;
    };
    // SAFETY: the caller's sixteen writable bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, LEN) };
    0
}

exports::exports! {
    functions {
        "gethostuuid" | "___gethostuuid" => gethostuuid,
        "uuid_clear" => clear,
        "uuid_compare" => compare,
        "uuid_copy" => copy,
        /// All three generate a version-4 UUID; see the module note.
        "uuid_generate" | "uuid_generate_random" | "uuid_generate_time" => generate_random,
        "uuid_is_null" => is_null,
        "uuid_parse" => parse,
        /// Darwin's plain spelling is the upper-case one.
        "uuid_unparse" | "uuid_unparse_upper" => unparse_upper,
        "uuid_unparse_lower" => unparse_lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generated() -> [u8; LEN] {
        let mut out = [0u8; LEN];
        // SAFETY: sixteen writable bytes.
        unsafe { generate_random(out.as_mut_ptr()) };
        out
    }

    /// A generated value must be a version-4 UUID, not merely sixteen random
    /// bytes: a caller that parses the version field would otherwise read a
    /// random one.
    #[test]
    fn a_generated_uuid_carries_its_version_and_variant() {
        let bytes = generated();
        assert_eq!(bytes[6] & 0xf0, 0x40, "version 4");
        assert_eq!(bytes[8] & 0xc0, 0x80, "the RFC 4122 variant");
        // SAFETY: sixteen readable bytes.
        assert_eq!(unsafe { is_null(bytes.as_ptr()) }, 0);
    }

    #[test]
    fn two_generated_uuids_differ() {
        assert_ne!(generated(), generated());
    }

    /// Darwin's plain `uuid_unparse` is the upper-case one, which is the
    /// opposite of this host's library. A guest that writes a string out and
    /// compares it later notices.
    #[test]
    fn the_plain_spelling_is_upper_case() {
        let bytes: [u8; LEN] = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ];
        let mut text = [0u8; TEXT_LEN];
        // SAFETY: sixteen readable bytes and thirty-seven writable ones.
        unsafe { unparse_upper(bytes.as_ptr(), text.as_mut_ptr().cast()) };
        assert_eq!(
            std::ffi::CStr::from_bytes_until_nul(&text)
                .unwrap()
                .to_str()
                .unwrap(),
            "01234567-89AB-CDEF-FEDC-BA9876543210"
        );
    }

    /// Formatted and parsed back is the value it started as, which is the whole
    /// contract the two halves have with each other.
    #[test]
    fn a_uuid_round_trips_through_its_text() {
        let bytes = generated();
        for upper in [false, true] {
            let mut text = [0u8; TEXT_LEN];
            // SAFETY: sixteen readable bytes and thirty-seven writable ones.
            unsafe { unparse(bytes.as_ptr(), text.as_mut_ptr().cast(), upper) };
            let mut back = [0u8; LEN];
            // SAFETY: a NUL-terminated string and sixteen writable bytes.
            assert_eq!(
                unsafe { parse(text.as_ptr().cast(), back.as_mut_ptr()) },
                0,
                "{}",
                String::from_utf8_lossy(&text)
            );
            assert_eq!(back, bytes);
        }
    }

    #[test]
    fn text_that_is_not_a_uuid_is_refused() {
        let mut back = [0u8; LEN];
        for bad in [
            c"".as_ptr(),
            c"not-a-uuid".as_ptr(),
            // The right length, with a separator in the wrong place.
            c"01234567+89ab-cdef-fedc-ba9876543210".as_ptr(),
            // The right shape, with a non-hexadecimal digit.
            c"0123456z-89ab-cdef-fedc-ba9876543210".as_ptr(),
        ] {
            // SAFETY: a static NUL-terminated string and sixteen writable bytes.
            assert_eq!(unsafe { parse(bad, back.as_mut_ptr()) }, -1);
        }
    }

    #[test]
    fn clearing_and_comparing_agree_about_the_null_uuid() {
        let mut a = generated();
        let b = [0u8; LEN];
        // SAFETY: sixteen writable bytes.
        unsafe { clear(a.as_mut_ptr()) };
        // SAFETY: two sixteen-byte buffers.
        assert_eq!(unsafe { compare(a.as_ptr(), b.as_ptr()) }, 0);
        // SAFETY: sixteen readable bytes.
        assert_eq!(unsafe { is_null(a.as_ptr()) }, 1);
    }

    #[test]
    fn the_host_uuid_is_the_same_each_time() {
        let read = || {
            let mut out = [0u8; LEN];
            let rc = unsafe { gethostuuid(out.as_mut_ptr(), core::ptr::null()) };
            (rc == 0).then_some(out)
        };
        let first = read();
        assert_eq!(first, read());
        assert_eq!(first.is_some(), starnix::system::host_uuid().is_some());
    }
}
