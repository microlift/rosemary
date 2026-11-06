#![cfg(not(target_os = "macos"))]

use std::ffi::{CStr, CString, c_char, c_int};

use crate::{
    fcntl::{FcntlFlags, oflag},
    translation::DarwinTranslation,
};

const HONORED: c_int = oflag::O_CREAT | oflag::O_EXCL | oflag::O_TRUNC | oflag::O_CLOEXEC;

/// A guest segment name as a name glibc will accept.
///
/// `None` for a name that is not a valid C string, or one that is empty once
/// the leading separators are off - glibc answers `EINVAL` for both, and so
/// does Darwin for the empty one.
///
/// The leading `/` is kept, because glibc skips exactly that; every `/` after
/// it becomes `%2F`, and a literal `%` becomes `%25` so the mapping stays
/// injective. Two rosemary processes sharing a segment agree on the host name
/// because they run the same translation.
fn to_host_name(name: &CStr) -> Option<CString> {
    let text = name.to_str().ok()?;
    let body = text.trim_start_matches('/');
    if body.is_empty() {
        return None;
    }
    let mut host = String::with_capacity(1 + body.len());
    host.push('/');
    for c in body.chars() {
        match c {
            '/' => host.push_str("%2F"),
            '%' => host.push_str("%25"),
            _ => host.push(c),
        }
    }
    CString::new(host).ok()
}

/// The name as a host name, or `EINVAL` set and `None`.
///
/// Deliberately *not* a prefix translation. A POSIX shared-memory name looks
/// like a path and is not one: it names an object in the kernel's own
/// namespace, which a prefix does not hold and cannot heal. `to_host_name`
/// above is the escaping that namespace needs.
///
/// # Safety
///
/// `name` is null or a NUL-terminated string.
pub(crate) unsafe fn host_name(name: *const c_char) -> Option<CString> {
    if name.is_null() {
        crate::errno::set(libc::EINVAL);
        return None;
    }
    // SAFETY: non-null, and a `const char *` is NUL-terminated by contract.
    let translated = to_host_name(unsafe { CStr::from_ptr(name) });
    if translated.is_none() {
        crate::errno::set(libc::EINVAL);
    }
    translated
}

pub unsafe extern "C" fn shm_open(name: *const c_char, flags: FcntlFlags, mode: u32) -> c_int {
    let Some(name) = (unsafe { host_name(name) }) else {
        return -1;
    };
    let host_flags = flags.honoring(HONORED).to_host();
    let mode = match crate::fcntl::creates(flags.0) {
        true => mode as libc::mode_t,
        false => 0,
    };
    // SAFETY: a live NUL-terminated name, and flags this host understands.
    let fd = unsafe { libc::shm_open(name.as_ptr(), host_flags, mode) };
    if fd >= 0 && flags.0 & oflag::O_CLOEXEC == 0 {
        // glibc set it unasked; Darwin would not have.
        // SAFETY: `fd` is the descriptor just opened.
        unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
    }
    fd
}

pub unsafe extern "C" fn shm_unlink(name: *const c_char) -> c_int {
    let Some(name) = (unsafe { host_name(name) }) else {
        return -1;
    };
    // SAFETY: a live NUL-terminated name.
    unsafe { libc::shm_unlink(name.as_ptr()) }
}

exports::exports! {
    functions {
        "shm_open" => shm_open,
        "shm_unlink" => shm_unlink,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The word `shm_open` hands the host, for a Darwin word the guest gave.
    fn to_host_flags(guest: c_int) -> c_int {
        FcntlFlags(guest).honoring(HONORED).to_host()
    }

    /// The bug this module exists for: Darwin's create-exclusive word is this
    /// host's truncate-nonblock word, which asks for the opposite thing.
    #[test]
    fn creating_a_segment_is_not_truncating_one() {
        let host = to_host_flags(oflag::O_RDWR | oflag::O_CREAT | oflag::O_EXCL);
        assert_eq!(host & libc::O_ACCMODE, libc::O_RDWR);
        assert_eq!(host & libc::O_CREAT, libc::O_CREAT);
        assert_eq!(host & libc::O_EXCL, libc::O_EXCL);
        assert_eq!(host & libc::O_TRUNC, 0);
    }

    #[test]
    fn a_flag_shm_open_ignores_is_dropped_rather_than_forwarded() {
        let host = to_host_flags(oflag::O_RDONLY | oflag::O_APPEND | oflag::O_NONBLOCK);
        assert_eq!(host, libc::O_RDONLY);
    }

    #[test]
    fn close_on_exec_survives_the_translation() {
        let host = to_host_flags(oflag::O_RDWR | oflag::O_CLOEXEC);
        assert_eq!(host & libc::O_CLOEXEC, libc::O_CLOEXEC);
    }

    #[test]
    fn a_plain_name_keeps_its_spelling() {
        let name = to_host_name(c"/com.example.cache").unwrap();
        assert_eq!(name.as_c_str(), c"/com.example.cache");
    }

    /// glibc refuses a separator past the first; Darwin's flat namespace does
    /// not, so it has to survive as something.
    #[test]
    fn a_separator_inside_the_name_is_escaped_injectively() {
        assert_eq!(
            to_host_name(c"/com.example/cache").unwrap().as_c_str(),
            c"/com.example%2Fcache"
        );
        assert_eq!(
            to_host_name(c"/com.example%2Fcache").unwrap().as_c_str(),
            c"/com.example%252Fcache"
        );
    }

    /// A name with nothing but separators is `EINVAL` on both platforms.
    #[test]
    fn an_empty_name_is_refused() {
        assert!(to_host_name(c"").is_none());
        assert!(to_host_name(c"/").is_none());
    }

    /// Darwin accepts a name that does not start with one; glibc wants it.
    #[test]
    fn a_name_without_a_leading_separator_gets_one() {
        assert_eq!(to_host_name(c"cache").unwrap().as_c_str(), c"/cache");
    }

    /// End to end, on the host's own shared memory: create, reopen, unlink.
    #[test]
    fn a_segment_created_with_darwin_flags_can_be_reopened() {
        let name = CString::new(format!("/rosemary-shm-test-{}", std::process::id())).unwrap();
        // SAFETY: a live name, and a mode that matters because `O_CREAT` is set.
        let fd = unsafe {
            shm_open(
                name.as_ptr(),
                FcntlFlags(oflag::O_RDWR | oflag::O_CREAT | oflag::O_EXCL),
                0o600,
            )
        };
        assert!(
            fd >= 0,
            "create failed: {}",
            std::io::Error::last_os_error()
        );
        // Darwin does not set close-on-exec unless asked.
        // SAFETY: `fd` is open.
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        // SAFETY: a live name; no mode, because there is no `O_CREAT`.
        let again = unsafe { shm_open(name.as_ptr(), FcntlFlags(oflag::O_RDONLY), 0) };
        assert!(
            again >= 0,
            "reopen failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: both descriptors are open.
        unsafe {
            libc::close(fd);
            libc::close(again);
        }
        // SAFETY: a live name.
        assert_eq!(unsafe { shm_unlink(name.as_ptr()) }, 0);
    }
}
