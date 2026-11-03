// FIXME (PORTABILITY): entire module is unportable and contains too much #[cfg] - prefer cfg!() where platform specificity is truly needed
// and build abstractions

use std::ffi::{CStr, CString, c_char, c_void};

const NOFOLLOW: i32 = 0x0001;
const CREATE: i32 = 0x0002;
const REPLACE: i32 = 0x0004;
const ENOATTR: i32 = 93;

/// Whether this host's `errno` means the attribute named is not on the file.
///
/// Linux has no `ENOATTR` and reports a missing attribute as `ENODATA`.
pub(crate) fn is_absent(errno: i32) -> bool {
    #[cfg(target_os = "linux")]
    return errno == libc::ENODATA;
    #[cfg(not(target_os = "linux"))]
    return errno == libc::ENOATTR;
}

/// `rc` from one of the attribute calls, as the guest should see its failure.
///
/// Linux's `ENODATA` is a different error on Darwin (96, "no message available
/// on STREAM"). From these calls it can only mean the attribute is absent, so
/// it is reported as Darwin's number for that - the one a guest compares
/// against. The host slot keeps `ENODATA`, so [`is_absent`] still answers.
#[cfg(target_os = "linux")]
fn absent_as_enoattr<T: Copy + PartialOrd + From<i8>>(rc: T) -> T {
    if rc < T::from(0) && is_absent(crate::errno::get()) {
        crate::errno::set_guest(ENOATTR);
    }
    rc
}

/// Elsewhere the host has its own number, which [`crate::errno`] maps.
#[cfg(not(target_os = "linux"))]
fn absent_as_enoattr<T>(rc: T) -> T {
    rc
}

/// The file an attribute call is about: a host path, and whether a final
/// symlink is the file rather than the way to it, or an open descriptor.
#[derive(Clone, Copy)]
enum File {
    Path(*const c_char, bool),
    Fd(libc::c_int),
}

/// This host's extended-attribute calls, over the guest's own flat names.
///
/// Every function here takes the name the guest wrote and answers in Darwin's
/// terms - a count and `errno`, `ERANGE` for a buffer too small, `ENOATTR` for
/// an attribute that is not there. Putting the name in the user namespace, and
/// getting it back out, happens inside.
#[cfg(target_os = "linux")]
mod host {
    use std::ffi::{CStr, CString, c_char, c_void};

    use super::File;

    /// Linux's own attribute namespaces, which are not the guest's to use.
    ///
    /// A guest name is prefixed with `user.`; one that already reads as one of
    /// these is left alone, because the guest cannot have meant one but a name
    /// that happens to start that way must still round trip to itself.
    const NAMESPACES: &[&str] = &["user.", "security.", "system.", "trusted."];

    /// The guest's name as this host's, or `None` for one it cannot be put in.
    pub fn name(guest: &CStr) -> Option<CString> {
        let text = guest.to_str().ok()?;
        if NAMESPACES.iter().any(|ns| text.starts_with(ns)) {
            return CString::new(text).ok();
        }
        CString::new(format!("user.{text}")).ok()
    }

    /// # Safety
    ///
    /// A path in `file` is NUL-terminated, and `value` is `size` writable
    /// bytes, or `size` is zero.
    pub unsafe fn get(file: File, name: &CStr, value: *mut c_void, size: usize) -> isize {
        // SAFETY: the caller's file and buffer, with a name this module owns.
        unsafe {
            match file {
                File::Path(path, false) => libc::getxattr(path, name.as_ptr(), value, size),
                File::Path(path, true) => libc::lgetxattr(path, name.as_ptr(), value, size),
                File::Fd(fd) => libc::fgetxattr(fd, name.as_ptr(), value, size),
            }
        }
    }

    /// # Safety
    ///
    /// A path in `file` is NUL-terminated, and `value` is `size` readable bytes.
    pub unsafe fn set(
        file: File,
        name: &CStr,
        value: *const c_void,
        size: usize,
        create: bool,
        replace: bool,
    ) -> i32 {
        let mut flags = 0;
        if create {
            flags |= libc::XATTR_CREATE;
        }
        if replace {
            flags |= libc::XATTR_REPLACE;
        }
        let name = name.as_ptr();
        // SAFETY: the caller's file and buffer, with a name this module owns.
        unsafe {
            match file {
                File::Path(path, false) => libc::setxattr(path, name, value, size, flags),
                File::Path(path, true) => libc::lsetxattr(path, name, value, size, flags),
                File::Fd(fd) => libc::fsetxattr(fd, name, value, size, flags),
            }
        }
    }

    /// # Safety
    ///
    /// A path in `file` is NUL-terminated.
    pub unsafe fn remove(file: File, name: &CStr) -> i32 {
        // SAFETY: the caller's file, with a name this module owns.
        unsafe {
            match file {
                File::Path(path, false) => libc::removexattr(path, name.as_ptr()),
                File::Path(path, true) => libc::lremovexattr(path, name.as_ptr()),
                File::Fd(fd) => libc::fremovexattr(fd, name.as_ptr()),
            }
        }
    }

    /// Every attribute on `file` the guest could have written, in its own
    /// spelling. `None` leaves `errno` set.
    ///
    /// # Safety
    ///
    /// A path in `file` is NUL-terminated.
    pub unsafe fn list(file: File) -> Option<Vec<Vec<u8>>> {
        let mut buffer = vec![0u8; 4096];
        loop {
            // SAFETY: the caller's file, and a buffer this function owns.
            let rc = unsafe {
                let (p, n) = (buffer.as_mut_ptr().cast::<c_char>(), buffer.len());
                match file {
                    File::Path(path, false) => libc::listxattr(path, p, n),
                    File::Path(path, true) => libc::llistxattr(path, p, n),
                    File::Fd(fd) => libc::flistxattr(fd, p, n),
                }
            };
            if rc >= 0 {
                buffer.truncate(rc as usize);
                break;
            }
            if crate::errno::get() != libc::ERANGE || buffer.len() >= 1 << 20 {
                return None;
            }
            buffer.resize(buffer.len() * 2, 0);
        }
        Some(
            buffer
                .split(|b| *b == 0)
                .filter(|n| !n.is_empty())
                // Anything outside `user.` is one of this host's own namespaces
                // and not something the guest wrote.
                .filter_map(|n| n.strip_prefix(b"user.").map(<[u8]>::to_vec))
                .collect(),
        )
    }
}

#[cfg(target_os = "freebsd")]
mod host {
    use std::ffi::{CStr, CString, c_void};

    use super::File;

    /// Every guest attribute lives in the user namespace. The guest cannot have
    /// meant one of the kernel's, and `EXTATTR_NAMESPACE_SYSTEM` needs a
    /// privilege Darwin never asked the caller for.
    const NAMESPACE: libc::c_int = libc::EXTATTR_NAMESPACE_USER;

    /// The guest's name unchanged: FreeBSD carries the namespace beside the
    /// name rather than inside it.
    pub fn name(guest: &CStr) -> Option<CString> {
        CString::new(guest.to_bytes()).ok()
    }

    /// `extattr_get_*` on `file`: up to `n` bytes into `p`, or the length for a
    /// null `p`.
    ///
    /// # Safety
    ///
    /// A path in `file` is NUL-terminated, and `p` is null or `n` writable bytes.
    unsafe fn read(file: File, name: &CStr, p: *mut c_void, n: usize) -> isize {
        // SAFETY: the caller's.
        unsafe {
            match file {
                File::Path(path, false) => {
                    libc::extattr_get_file(path, NAMESPACE, name.as_ptr(), p, n)
                }
                File::Path(path, true) => {
                    libc::extattr_get_link(path, NAMESPACE, name.as_ptr(), p, n)
                }
                File::Fd(fd) => libc::extattr_get_fd(fd, NAMESPACE, name.as_ptr(), p, n),
            }
        }
    }

    /// The attribute's length, or `None` with `errno` set.
    ///
    /// # Safety
    ///
    /// A path in `file` is NUL-terminated.
    unsafe fn length(file: File, name: &CStr) -> Option<usize> {
        // SAFETY: the caller's file. A null buffer asks for the length.
        let rc = unsafe { read(file, name, core::ptr::null_mut(), 0) };
        match rc < 0 {
            true => None,
            false => Some(rc as usize),
        }
    }

    /// # Safety
    ///
    /// A path in `file` is NUL-terminated, and `value` is `size` writable
    /// bytes, or `size` is zero.
    pub unsafe fn get(file: File, name: &CStr, value: *mut c_void, size: usize) -> isize {
        // Read through a buffer of this module's own rather than the caller's,
        // because FreeBSD truncates into a short buffer where Darwin refuses
        // with `ERANGE` - and a truncated attribute reported as a whole one is
        // the answer a caller cannot tell is wrong.
        // SAFETY: the caller's file.
        let Some(length) = (unsafe { length(file, name) }) else {
            return -1;
        };
        if size == 0 {
            return length as isize;
        }
        if length > size {
            crate::errno::set(libc::ERANGE);
            return -1;
        }
        let mut owned = vec![0u8; length];
        // SAFETY: the caller's file, into a buffer of exactly the length the
        // kernel just reported.
        let rc = unsafe { read(file, name, owned.as_mut_ptr().cast(), owned.len()) };
        if rc < 0 {
            return -1;
        }
        let rc = (rc as usize).min(size);
        // SAFETY: the caller's buffer, and `rc` bytes is within both.
        unsafe { core::ptr::copy_nonoverlapping(owned.as_ptr(), value.cast::<u8>(), rc) };
        rc as isize
    }

    /// # Safety
    ///
    /// A path in `file` is NUL-terminated, and `value` is `size` readable bytes.
    pub unsafe fn set(
        file: File,
        name: &CStr,
        value: *const c_void,
        size: usize,
        create: bool,
        replace: bool,
    ) -> i32 {
        // FreeBSD has no counterpart to `XATTR_CREATE`/`XATTR_REPLACE` - its
        // set always overwrites - so the condition is checked here. That is a
        // test and a write rather than one atomic call: two guests racing for
        // the same attribute can both see it absent and both succeed, where
        // Darwin would fail one of them.
        if create || replace {
            // SAFETY: the caller's file.
            let present = unsafe { length(file, name) }.is_some();
            if create && present {
                crate::errno::set(libc::EEXIST);
                return -1;
            }
            if replace && !present {
                crate::errno::set(libc::ENOATTR);
                return -1;
            }
        }
        let name = name.as_ptr();
        // SAFETY: the caller's file and buffer, with a name this module owns.
        let rc = unsafe {
            match file {
                File::Path(path, false) => {
                    libc::extattr_set_file(path, NAMESPACE, name, value, size)
                }
                File::Path(path, true) => {
                    libc::extattr_set_link(path, NAMESPACE, name, value, size)
                }
                File::Fd(fd) => libc::extattr_set_fd(fd, NAMESPACE, name, value, size),
            }
        };
        // Darwin's `setxattr` answers zero, not a count.
        match rc < 0 {
            true => -1,
            false => 0,
        }
    }

    /// # Safety
    ///
    /// A path in `file` is NUL-terminated.
    pub unsafe fn remove(file: File, name: &CStr) -> i32 {
        // SAFETY: the caller's file, with a name this module owns.
        unsafe {
            match file {
                File::Path(path, false) => {
                    libc::extattr_delete_file(path, NAMESPACE, name.as_ptr())
                }
                File::Path(path, true) => libc::extattr_delete_link(path, NAMESPACE, name.as_ptr()),
                File::Fd(fd) => libc::extattr_delete_fd(fd, NAMESPACE, name.as_ptr()),
            }
        }
    }

    /// Every attribute on `file` the guest could have written. `None` leaves
    /// `errno` set.
    ///
    /// # Safety
    ///
    /// A path in `file` is NUL-terminated.
    pub unsafe fn list(file: File) -> Option<Vec<Vec<u8>>> {
        let mut buffer = vec![0u8; 4096];
        loop {
            // SAFETY: the caller's file, and a buffer this function owns.
            let rc = unsafe {
                let (p, n) = (buffer.as_mut_ptr().cast::<c_void>(), buffer.len());
                match file {
                    File::Path(path, false) => libc::extattr_list_file(path, NAMESPACE, p, n),
                    File::Path(path, true) => libc::extattr_list_link(path, NAMESPACE, p, n),
                    File::Fd(fd) => libc::extattr_list_fd(fd, NAMESPACE, p, n),
                }
            };
            if rc >= 0 && (rc as usize) < buffer.len() {
                buffer.truncate(rc as usize);
                break;
            }
            // A full buffer is indistinguishable from an exactly-full one here,
            // so it is grown and asked again rather than assumed complete.
            if rc < 0 && crate::errno::get() != libc::ERANGE {
                return None;
            }
            if buffer.len() >= 1 << 20 {
                crate::errno::set(libc::ERANGE);
                return None;
            }
            buffer.resize(buffer.len() * 2, 0);
        }

        // FreeBSD's list is not NUL-separated: each name is a one-byte length
        // followed by exactly that many bytes, with no terminator.
        let mut names = Vec::new();
        let mut rest = buffer.as_slice();
        while let Some((len, tail)) = rest.split_first() {
            let len = *len as usize;
            if tail.len() < len {
                break;
            }
            names.push(tail[..len].to_vec());
            rest = &tail[len..];
        }
        Some(names)
    }
}

#[cfg(target_os = "macos")]
mod host {
    use super::*;

    pub fn name(name: &CStr) -> Option<CString> {
        Some(name.to_owned())
    }

    pub unsafe fn remove(file: File, name: &CStr) -> i32 {
        unsafe {
            match file {
                File::Path(path, nofollow) => {
                    libc::removexattr(path, name.as_ptr(), if nofollow { NOFOLLOW } else { 0 })
                }
                File::Fd(fd) => libc::fremovexattr(fd, name.as_ptr(), 0),
            }
        }
    }

    pub unsafe fn set_fd(fd: libc::c_int, name: &CStr, value: *const c_void, size: usize) -> i32 {
        unsafe { libc::fsetxattr(fd, name.as_ptr(), value, size, 0, 0) }
    }

    pub unsafe fn list(file: File) -> Option<Vec<Vec<u8>>> {
        let mut bytes = vec![0u8; 256];
        loop {
            let (p, n) = (bytes.as_mut_ptr().cast(), bytes.len());
            let got = unsafe {
                match file {
                    File::Path(path, nofollow) => {
                        libc::listxattr(path, p, n, if nofollow { NOFOLLOW } else { 0 })
                    }
                    File::Fd(fd) => libc::flistxattr(fd, p, n, 0),
                }
            };
            if got >= 0 {
                bytes.truncate(got as usize);
                return Some(
                    bytes
                        .split(|b| *b == 0)
                        .filter(|n| !n.is_empty())
                        .map(<[u8]>::to_vec)
                        .collect(),
                );
            }
            if crate::errno::get() != libc::ERANGE || bytes.len() >= 1 << 20 {
                return None;
            }
            bytes.resize(bytes.len() * 4, 0);
        }
    }
}

/// `int getxattr(const char *, const char *, void *, size_t, u_int32_t, int)`
///
/// `position` is Darwin's offset into a resource fork and is refused when it is
/// not zero: this host has no way to read an attribute from part-way through,
/// and answering from the start would be a short read reported as a whole one.
///
/// # Safety
///
/// `path` and `name` are NUL-terminated; `value` is `size` writable bytes, or
/// `size` is zero.
unsafe extern "C" fn getxattr(
    path: *const c_char,
    name: *const c_char,
    value: *mut c_void,
    size: usize,
    position: u32,
    options: i32,
) -> isize {
    // SAFETY: the caller's, for each.
    let Some(host) = (unsafe { checked_name(name, position) }) else {
        return -1;
    };
    let Some(path) = (unsafe { host_file(path) }) else {
        return -1;
    };
    let file = File::Path(path.as_ptr(), options & NOFOLLOW != 0);
    unsafe { get(file, &host, value, size, position, options) }
}

/// `ssize_t fgetxattr(int, const char *, void *, size_t, u_int32_t, int)`
///
/// # Safety
///
/// As [`getxattr`], on an open descriptor.
unsafe extern "C" fn fgetxattr(
    fd: libc::c_int,
    name: *const c_char,
    value: *mut c_void,
    size: usize,
    position: u32,
    options: i32,
) -> isize {
    // SAFETY: the caller's, for each.
    let Some(host) = (unsafe { checked_name(name, position) }) else {
        return -1;
    };
    unsafe { get(File::Fd(fd), &host, value, size, position, options) }
}

/// The guest's attribute `name` in this host's namespace, refusing what
/// [`getxattr`] and [`setxattr`] refuse before they look for the file.
///
/// # Safety
///
/// `name` is NUL-terminated or null.
unsafe fn checked_name(name: *const c_char, position: u32) -> Option<CString> {
    #[cfg(not(target_os = "macos"))]
    if position != 0 {
        crate::errno::set(libc::EINVAL);
        return None;
    }
    #[cfg(target_os = "macos")]
    let _ = position;
    // SAFETY: the caller's.
    let host = unsafe { host_name(name) };
    if host.is_none() {
        crate::errno::set(libc::EINVAL);
    }
    host
}

/// # Safety
///
/// A path in `file` is NUL-terminated; `value` is `size` writable bytes, or
/// `size` is zero.
unsafe fn get(
    file: File,
    host: &CStr,
    value: *mut c_void,
    size: usize,
    position: u32,
    options: i32,
) -> isize {
    // SAFETY: the caller's file and buffer.
    #[cfg(target_os = "macos")]
    return unsafe {
        match file {
            File::Path(path, _) => {
                libc::getxattr(path, host.as_ptr(), value, size, position, options)
            }
            File::Fd(fd) => libc::fgetxattr(fd, host.as_ptr(), value, size, position, options),
        }
    };
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (position, options);
        absent_as_enoattr(unsafe { host::get(file, host, value, size) })
    }
}

/// `int setxattr(const char *, const char *, const void *, size_t, u_int32_t, int)`
///
/// # Safety
///
/// As [`getxattr`], with `value` readable rather than writable.
unsafe extern "C" fn setxattr(
    path: *const c_char,
    name: *const c_char,
    value: *const c_void,
    size: usize,
    position: u32,
    options: i32,
) -> i32 {
    // SAFETY: the caller's, for each.
    let Some(host) = (unsafe { checked_name(name, position) }) else {
        return -1;
    };
    let Some(path) = (unsafe { host_file(path) }) else {
        return -1;
    };
    let file = File::Path(path.as_ptr(), options & NOFOLLOW != 0);
    unsafe { set(file, &host, value, size, position, options) }
}

/// `int fsetxattr(int, const char *, const void *, size_t, u_int32_t, int)`
///
/// # Safety
///
/// As [`setxattr`], on an open descriptor.
unsafe extern "C" fn fsetxattr(
    fd: libc::c_int,
    name: *const c_char,
    value: *const c_void,
    size: usize,
    position: u32,
    options: i32,
) -> i32 {
    // SAFETY: the caller's, for each.
    let Some(host) = (unsafe { checked_name(name, position) }) else {
        return -1;
    };
    unsafe { set(File::Fd(fd), &host, value, size, position, options) }
}

/// # Safety
///
/// A path in `file` is NUL-terminated, and `value` is `size` readable bytes.
unsafe fn set(
    file: File,
    host: &CStr,
    value: *const c_void,
    size: usize,
    position: u32,
    options: i32,
) -> i32 {
    // SAFETY: the caller's file and buffer.
    #[cfg(target_os = "macos")]
    return unsafe {
        match file {
            File::Path(path, _) => {
                libc::setxattr(path, host.as_ptr(), value, size, position, options)
            }
            File::Fd(fd) => libc::fsetxattr(fd, host.as_ptr(), value, size, position, options),
        }
    };
    #[cfg(not(target_os = "macos"))]
    {
        let _ = position;
        let (create, replace) = (options & CREATE != 0, options & REPLACE != 0);
        absent_as_enoattr(unsafe { host::set(file, host, value, size, create, replace) })
    }
}

/// `int removexattr(const char *, const char *, int)`
///
/// # Safety
///
/// `path` and `name` are NUL-terminated.
unsafe extern "C" fn removexattr(path: *const c_char, name: *const c_char, options: i32) -> i32 {
    let Some(host) = (unsafe { host_name(name) }) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    let Some(path) = (unsafe { host_file(path) }) else {
        return -1;
    };
    let file = File::Path(path.as_ptr(), options & NOFOLLOW != 0);
    // SAFETY: a translated path.
    absent_as_enoattr(unsafe { host::remove(file, &host) })
}

/// `int fremovexattr(int, const char *, int)`
///
/// # Safety
///
/// `name` is NUL-terminated.
unsafe extern "C" fn fremovexattr(fd: libc::c_int, name: *const c_char, _options: i32) -> i32 {
    let Some(host) = (unsafe { host_name(name) }) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    // SAFETY: a descriptor names no memory.
    absent_as_enoattr(unsafe { host::remove(File::Fd(fd), &host) })
}

/// `ssize_t listxattr(const char *, char *, size_t, int)`
///
/// Darwin's answer is a run of NUL-terminated names, in the guest's own flat
/// spelling - which is what [`host::list`] hands back, whatever this host's
/// list looks like.
///
/// # Safety
///
/// `path` is NUL-terminated and `list` is `size` writable bytes, or `size` is
/// zero.
unsafe extern "C" fn listxattr(
    path: *const c_char,
    list: *mut c_char,
    size: usize,
    options: i32,
) -> isize {
    let Some(path) = (unsafe { host_file(path) }) else {
        return -1;
    };
    // SAFETY: a translated path, and the caller's buffer.
    unsafe {
        list_into(
            File::Path(path.as_ptr(), options & NOFOLLOW != 0),
            list,
            size,
        )
    }
}

/// `ssize_t flistxattr(int, char *, size_t, int)`
///
/// # Safety
///
/// `list` is `size` writable bytes, or `size` is zero.
unsafe extern "C" fn flistxattr(
    fd: libc::c_int,
    list: *mut c_char,
    size: usize,
    _options: i32,
) -> isize {
    // SAFETY: the caller's buffer.
    unsafe { list_into(File::Fd(fd), list, size) }
}

/// The names on `file` as `listxattr` answers them.
///
/// # Safety
///
/// A path in `file` is NUL-terminated and `list` is `size` writable bytes, or
/// `size` is zero.
unsafe fn list_into(file: File, list: *mut c_char, size: usize) -> isize {
    // SAFETY: the caller's file.
    let Some(names) = (unsafe { host::list(file) }) else {
        return -1;
    };
    let mut out: Vec<u8> = Vec::new();
    for name in names.into_iter().filter(|name| !reserved(name)) {
        out.extend_from_slice(&name);
        out.push(0);
    }
    // A zero size is the guest asking how much room it needs.
    if size == 0 {
        return out.len() as isize;
    }
    if out.len() > size {
        crate::errno::set(libc::ERANGE);
        return -1;
    }
    // SAFETY: the caller's buffer, which was just checked to be big enough.
    unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), list.cast::<u8>(), out.len()) };
    out.len() as isize
}

/// `name` in this host's namespace, or `None` for a name it cannot be put in.
///
/// # Safety
///
/// `name` is NUL-terminated or null.
unsafe fn host_name(name: *const c_char) -> Option<CString> {
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) };
    if reserved(name.to_bytes()) {
        return None;
    }
    host::name(name)
}

/// Set the attribute the guest calls `name` on the file at the *host* path
/// `path` to `value`, or remove it for `None`: for a call that has already found
/// the file, such as `setattrlist` writing Finder info. Answers as `setxattr`
/// and `removexattr` do.
///
/// # Safety
///
/// `path` and `name` are NUL-terminated.
pub(crate) unsafe fn set_on_host(
    path: &CStr,
    name: &CStr,
    value: Option<&[u8]>,
    nofollow: bool,
) -> i32 {
    let Some(host) = (unsafe { host_name(name.as_ptr()) }) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    let rc = match value {
        // SAFETY: the caller's path, and a buffer of the length given.
        #[cfg(target_os = "macos")]
        Some(value) => unsafe {
            let options = if nofollow { NOFOLLOW } else { 0 };
            libc::setxattr(
                path.as_ptr(),
                host.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                options,
            )
        },
        // SAFETY: as above.
        #[cfg(not(target_os = "macos"))]
        Some(value) => unsafe {
            let file = File::Path(path.as_ptr(), nofollow);
            host::set(
                file,
                &host,
                value.as_ptr().cast(),
                value.len(),
                false,
                false,
            )
        },
        // SAFETY: the caller's path.
        None => unsafe { host::remove(File::Path(path.as_ptr(), nofollow), &host) },
    };
    absent_as_enoattr(rc)
}

/// [`set_on_host`] for the file open on `fd`: the descriptor itself, so the
/// attribute lands on that file whatever it is now called.
///
/// # Safety
///
/// `name` is NUL-terminated.
pub(crate) unsafe fn set_on_fd(fd: libc::c_int, name: &CStr, value: Option<&[u8]>) -> i32 {
    let Some(host) = (unsafe { host_name(name.as_ptr()) }) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    let rc = match value {
        // SAFETY: the caller's descriptor, and a buffer of the length given.
        #[cfg(target_os = "macos")]
        Some(value) => unsafe { host::set_fd(fd, &host, value.as_ptr().cast(), value.len()) },
        #[cfg(not(target_os = "macos"))]
        Some(value) => unsafe {
            host::set(
                File::Fd(fd),
                &host,
                value.as_ptr().cast(),
                value.len(),
                false,
                false,
            )
        },
        // SAFETY: a descriptor names no memory.
        None => unsafe { host::remove(File::Fd(fd), &host) },
    };
    absent_as_enoattr(rc)
}

/// Whether `name`, as the guest spells it, is the runtime's own record of who
/// owns a file (`prefix::ownership::RECORD`), which a guest neither sees nor
/// writes: it would be a `chown` that no permission check saw.
fn reserved(name: &[u8]) -> bool {
    let record = prefix::ownership::RECORD.to_bytes();
    let bare = record.strip_prefix(b"user.").unwrap_or(record);
    name == record || name == bare
}

/// The host path the guest's `path` names.
///
/// No case fold: these calls are about a file the guest has already opened or
/// listed, so a spelling that does not exist is the guest's answer, not a
/// lookup to retry. See [`crate::fs`] for the translation itself.
///
/// # Safety
///
/// `path` is null or a NUL-terminated string.
unsafe fn host_file(path: *const c_char) -> Option<CString> {
    if path.is_null() {
        crate::errno::set(libc::EFAULT);
        return None;
    }
    // SAFETY: non-null, and a `const char *` is NUL-terminated by contract.
    Some(prefix::ns::to_host(unsafe { CStr::from_ptr(path) }).into_owned())
}

exports::exports! {
    functions {
        "getxattr" => getxattr,
        "setxattr" => setxattr,
        "removexattr" => removexattr,
        "listxattr" => listxattr,
        "fgetxattr" => fgetxattr,
        "fsetxattr" => fsetxattr,
        "fremovexattr" => fremovexattr,
        "flistxattr" => flistxattr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule this module exists for: a flat Darwin name is not a name Linux
    /// will store on a regular file. FreeBSD carries the namespace separately,
    /// so there the name crosses unchanged.
    #[test]
    fn a_guest_name_is_put_in_this_hosts_namespace() {
        let host = host::name(c"org.chromium.crashpad.database.initialized").expect("a valid name");
        #[cfg(target_os = "linux")]
        assert_eq!(
            host.to_str().unwrap(),
            "user.org.chromium.crashpad.database.initialized"
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            host.to_str().unwrap(),
            "org.chromium.crashpad.database.initialized"
        );
    }

    /// A name that already reads as one of Linux's namespaces is left alone, so
    /// that it still round trips to itself.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_name_already_in_a_host_namespace_is_not_prefixed_twice() {
        assert_eq!(
            host::name(c"user.thing").unwrap().to_str().unwrap(),
            "user.thing"
        );
        assert_eq!(
            host::name(c"security.thing").unwrap().to_str().unwrap(),
            "security.thing"
        );
    }

    /// Through the real filesystem: set, read back, list, remove - all in the
    /// guest's flat spelling.
    ///
    /// Skipped rather than failed where the filesystem has no extended
    /// attributes, which is a property of the host and not of this code.
    #[test]
    fn an_attribute_round_trips_in_the_guests_spelling() {
        let path = std::env::temp_dir().join(format!("rosemary-xattr-{}", std::process::id()));
        std::fs::write(&path, b"x").expect("a scratch file");
        let c = CString::new(path.to_str().unwrap()).unwrap();

        // SAFETY: a path and name this test owns, with a buffer of its own.
        let rc = unsafe {
            setxattr(
                c.as_ptr(),
                c"com.example.tag".as_ptr(),
                c"7".as_ptr().cast(),
                1,
                0,
                0,
            )
        };
        if rc != 0 {
            let _ = std::fs::remove_file(&path);
            eprintln!("skipping: this filesystem has no extended attributes");
            return;
        }

        let mut value = [0u8; 8];
        // SAFETY: as above, with eight writable bytes.
        let got = unsafe {
            getxattr(
                c.as_ptr(),
                c"com.example.tag".as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(got, 1);
        assert_eq!(value[0], b'7');

        let mut names = [0u8; 64];
        let listed = unsafe { listxattr(c.as_ptr(), names.as_mut_ptr().cast(), names.len(), 0) };
        assert!(listed > 0);
        assert!(
            names[..listed as usize]
                .split(|b| *b == 0)
                .any(|n| n == b"com.example.tag"),
            "the guest's own spelling is what comes back"
        );

        assert_eq!(
            unsafe { removexattr(c.as_ptr(), c"com.example.tag".as_ptr(), 0) },
            0
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The descriptor calls speak the guest's spelling too, which is what
    /// `libcopyfile` copies attributes through.
    #[test]
    fn an_attribute_round_trips_through_a_descriptor() {
        use std::os::fd::AsRawFd;
        let path = std::env::temp_dir().join(format!("rosemary-fxattr-{}", std::process::id()));
        let file = std::fs::File::create(&path).expect("a scratch file");
        let _ = std::fs::remove_file(&path);
        let fd = file.as_raw_fd();
        let name = c"com.example.tag";

        // SAFETY: a descriptor and name this test owns, with buffers of its own.
        unsafe {
            if fsetxattr(fd, name.as_ptr(), c"7".as_ptr().cast(), 1, 0, 0) != 0 {
                eprintln!("skipping: this filesystem has no extended attributes");
                return;
            }
            assert_eq!(
                fsetxattr(fd, name.as_ptr(), c"8".as_ptr().cast(), 1, 0, CREATE),
                -1
            );
            assert_eq!(crate::errno::get(), libc::EEXIST);

            let mut value = [0u8; 8];
            let got = fgetxattr(
                fd,
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                0,
            );
            assert_eq!((got, value[0]), (1, b'7'));

            let mut names = [0u8; 64];
            let listed = flistxattr(fd, names.as_mut_ptr().cast(), names.len(), 0);
            assert_eq!(&names[..listed as usize], b"com.example.tag\0");

            assert_eq!(fremovexattr(fd, name.as_ptr(), 0), 0);
            assert_eq!(
                fgetxattr(fd, name.as_ptr(), core::ptr::null_mut(), 0, 0, 0),
                -1
            );
        }
    }

    /// A missing attribute is Darwin's `ENOATTR` (93) whatever this host calls
    /// it - on Linux it is `ENODATA`, which the guest would read as 96.
    #[test]
    fn a_missing_attribute_is_enoattr() {
        let path = std::env::temp_dir().join(format!("rosemary-noattr-{}", std::process::id()));
        std::fs::write(&path, b"x").expect("a scratch file");
        let c = CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: a path and name this test owns.
        let supported = unsafe {
            setxattr(
                c.as_ptr(),
                c"com.example.probe".as_ptr(),
                c"1".as_ptr().cast(),
                1,
                0,
                0,
            )
        } == 0;
        if !supported {
            let _ = std::fs::remove_file(&path);
            eprintln!("skipping: this filesystem has no extended attributes");
            return;
        }
        // SAFETY: as above; a zero size asks for the length alone.
        let got = unsafe {
            getxattr(
                c.as_ptr(),
                c"com.example.absent".as_ptr(),
                core::ptr::null_mut(),
                0,
                0,
                0,
            )
        };
        assert_eq!(got, -1);
        assert_eq!(crate::errno::guest_errno(), 93);
        // SAFETY: as above.
        let removed = unsafe { removexattr(c.as_ptr(), c"com.example.absent".as_ptr(), 0) };
        assert_eq!(removed, -1);
        assert_eq!(crate::errno::guest_errno(), 93);
        // XATTR_REPLACE of an attribute that is not there.
        // SAFETY: as above.
        let replaced = unsafe {
            setxattr(
                c.as_ptr(),
                c"com.example.absent".as_ptr(),
                c"1".as_ptr().cast(),
                1,
                0,
                0x0004,
            )
        };
        assert_eq!(replaced, -1);
        assert_eq!(crate::errno::guest_errno(), 93);
        let _ = std::fs::remove_file(&path);
    }

    /// Darwin's resource-fork offset has no host equivalent and is refused
    /// rather than ignored: ignoring it answers from the start of the attribute
    /// and reports the count as if it came from the offset asked for.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_non_zero_position_is_refused_rather_than_ignored() {
        // SAFETY: nothing is dereferenced on the refusing path.
        let rc = unsafe { getxattr(c"/".as_ptr(), c"x".as_ptr(), core::ptr::null_mut(), 0, 4, 0) };
        assert_eq!(rc, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        crate::errno::set(0);
    }
}
