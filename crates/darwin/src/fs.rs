use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::stat::{DARWIN_PATH_MAX, guest_path, may_write, with_fallback};

/// The host path `path` names, or `None` for a null pointer.
fn host(path: *const c_char) -> Option<CString> {
    if path.is_null() {
        return None;
    }
    // SAFETY: non-null, and a `const char *` is NUL-terminated by contract.
    Some(prefix::ns::to_host(unsafe { CStr::from_ptr(path) }).into_owned())
}

/// Resolve an at-style path to one the Rust filesystem APIs can use.
fn host_at(dirfd: c_int, path: *const c_char) -> Option<CString> {
    if path.is_null() {
        return None;
    }
    // SAFETY: non-null, and an at-style path is NUL-terminated by contract.
    let guest = unsafe { CStr::from_ptr(path) };
    let path = prefix::ns::to_host_at(dirfd, guest).into_owned();
    if dirfd == libc::AT_FDCWD || path.as_bytes().first() == Some(&b'/') {
        return Some(path);
    }
    // Relative paths under a real descriptor need an absolute spelling for
    // `std::fs`, anchored to that descriptor.
    let dir = starnix::fd::path(dirfd)?;
    CString::new([dir.as_os_str().as_encoded_bytes(), b"/", path.as_bytes()].concat()).ok()
}

/// Run `ask` on the host spelling of both paths, folding `from` on `ENOENT`.
///
/// Only `from` is folded: `to` is a name the call is about to create, and a
/// case-insensitive reading of a name that does not exist yet would either find
/// nothing or find the wrong neighbour.
fn with_pair(
    from: *const c_char,
    to: *const c_char,
    mut ask: impl FnMut(*const c_char, *const c_char) -> c_int,
) -> c_int {
    let (Some(from_host), Some(to_host)) = (host(from), host(to)) else {
        crate::errno::set(libc::EFAULT);
        return -1;
    };
    let rc = ask(from_host.as_ptr(), to_host.as_ptr());
    if rc == 0 || crate::errno::get() != libc::ENOENT {
        return rc;
    }
    let Some(found) = prefix::ns::fold(&from_host) else {
        return rc;
    };
    ask(found.as_ptr(), to_host.as_ptr())
}

/// `int chdir(const char *)`
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn chdir(path: *const c_char) -> c_int {
    with_fallback(path, |path| {
        let rc = unsafe { libc::chdir(path) };
        (rc, rc == 0)
    })
}

/// `char *getcwd(char *, size_t)`
///
/// The working directory is a host directory - `chdir` put it there - and the
/// guest has to be told the name it used. A directory outside every mount has
/// no guest spelling; `ENOENT` is what a Darwin kernel answers about a path
/// that is not in its filesystem, and is better than handing over a host path
/// the guest will keep.
///
/// # Safety
///
/// `buffer` is null or writable for `size` bytes.
pub unsafe extern "C" fn getcwd(buffer: *mut c_char, size: libc::size_t) -> *mut c_char {
    // Into this crate's own buffer first: the host's name and the guest's are
    // not the same length, so the caller's `size` cannot be measured yet.
    let mut here = [0 as c_char; libc::PATH_MAX as usize];
    // SAFETY: `here` is a live local of the host's `PATH_MAX`.
    if unsafe { libc::getcwd(here.as_mut_ptr(), here.len()) }.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `getcwd` wrote a NUL-terminated path into `here`.
    let Some(spelled) = guest_path(unsafe { CStr::from_ptr(here.as_ptr()) }) else {
        crate::errno::set(libc::ENOENT);
        return std::ptr::null_mut();
    };
    let bytes = spelled.as_bytes_with_nul();
    // A null buffer means "allocate one", and the caller frees it - so it has
    // to come from the allocator the guest's `free` uses.
    let (out, room) = match buffer.is_null() {
        // SAFETY: a plain size; the block is the caller's to free.
        true => (
            unsafe { crate::heap::malloc(DARWIN_PATH_MAX) }.cast::<c_char>(),
            DARWIN_PATH_MAX,
        ),
        false => (buffer, size),
    };
    if out.is_null() {
        crate::errno::set(libc::ENOMEM);
        return std::ptr::null_mut();
    }
    if bytes.len() > room {
        if buffer.is_null() {
            // SAFETY: a block this function just allocated and nothing else
            // holds.
            unsafe { crate::heap::free(out.cast()) };
        }
        crate::errno::set(libc::ERANGE);
        return std::ptr::null_mut();
    }
    // SAFETY: `out` is writable for `room` bytes and `bytes` has just been
    // checked to fit; the two do not overlap.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), out, bytes.len()) };
    out
}

/// `int unlink(const char *)`
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn unlink(path: *const c_char) -> c_int {
    with_fallback(path, |path| {
        if !may_write(path) {
            return (-1, false);
        }
        let rc = unsafe { libc::unlink(path) };
        (rc, rc == 0)
    })
}

/// `int rmdir(const char *)`
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn rmdir(path: *const c_char) -> c_int {
    with_fallback(path, |path| {
        if !may_write(path) {
            return (-1, false);
        }
        let rc = unsafe { libc::rmdir(path) };
        (rc, rc == 0)
    })
}

/// `int remove(const char *)`
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn remove(path: *const c_char) -> c_int {
    with_fallback(path, |path| {
        if !may_write(path) {
            return (-1, false);
        }
        let rc = unsafe { libc::remove(path) };
        (rc, rc == 0)
    })
}

/// `int unlinkat(int, const char *, int)`
///
/// Darwin spells `AT_FDCWD` and `AT_REMOVEDIR` its own way. A dataless
/// directory is one whose contents live elsewhere, which no host directory
/// is, so `AT_REMOVEDIR_DATALESS` is plain `AT_REMOVEDIR`.
///
/// # Safety
///
/// `path` is a NUL-terminated string and `dirfd` a directory descriptor or
/// Darwin's `AT_FDCWD`.
pub unsafe extern "C" fn unlinkat(dirfd: c_int, path: *const c_char, flag: c_int) -> c_int {
    const DARWIN_AT_REMOVEDIR: c_int = 0x0080;
    const DARWIN_AT_REMOVEDIR_DATALESS: c_int = 0x0100;
    let removes_dir = DARWIN_AT_REMOVEDIR | DARWIN_AT_REMOVEDIR_DATALESS;
    let beneath = crate::stat::beneath::SUPPORTED;
    if flag & !(removes_dir | beneath) != 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let dirfd = match dirfd {
        crate::stat::DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    let host_flag = match flag & removes_dir {
        0 => 0,
        _ => libc::AT_REMOVEDIR,
    };
    crate::stat::with_fallback_at(dirfd, path, |path| {
        if !crate::stat::may_write_at(dirfd, path) {
            return (-1, false);
        }
        let rc = match flag & beneath {
            // SAFETY: the caller's descriptor, and a translated path.
            0 => unsafe { libc::unlinkat(dirfd, path, host_flag) },
            _ => unsafe { crate::stat::beneath::unlinkat(dirfd, path, host_flag) },
        };
        (rc, rc == 0)
    })
}

/// `int mkdir(const char *, mode_t)`
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn mkdir(path: *const c_char, mode: libc::mode_t) -> c_int {
    let Some(path) = host(path) else {
        crate::errno::set(libc::EFAULT);
        return -1;
    };
    make_dir(libc::AT_FDCWD, &path, mode)
}

/// `int mkdirat(int, const char *, mode_t)`
///
/// The path stays relative to `dirfd`, so the directory is made where the
/// descriptor now is even after it was renamed, as the host's `mkdirat` would.
///
/// # Safety
///
/// `path` is a NUL-terminated string and `dirfd` a directory descriptor or
/// Darwin's `AT_FDCWD`.
pub unsafe extern "C" fn mkdirat(dirfd: c_int, path: *const c_char, mode: libc::mode_t) -> c_int {
    if path.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let dirfd = match dirfd {
        crate::stat::DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    // SAFETY: non-null, and a `const char *` is NUL-terminated by contract.
    let path = prefix::ns::to_host_at(dirfd, unsafe { CStr::from_ptr(path) });
    make_dir(dirfd, &path, mode)
}

/// Make the directory at host path `path`, relative to `dirfd`, and record it
/// as the guest's when it is acting as another user.
///
/// No fold: the directory is one this call is about to create, and a
/// case-insensitive reading of a name that does not exist yet would answer
/// about a different directory. No `may_write` either: a name the prefix's
/// base owns already exists, and the host answers `EEXIST` for it.
fn make_dir(dirfd: c_int, path: &CStr, mode: libc::mode_t) -> c_int {
    // SAFETY: a NUL-terminated path, and the caller's descriptor.
    let rc = unsafe { libc::mkdirat(dirfd, path.as_ptr(), mode) };
    if rc == 0 && crate::credentials::creates_as_another() {
        crate::credentials::created(dirfd, path);
    }
    rc
}

/// `int chmod(const char *, mode_t)`
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn chmod(path: *const c_char, mode: libc::mode_t) -> c_int {
    with_fallback(path, |path| {
        if !may_write(path) {
            return (-1, false);
        }
        let rc = unsafe { libc::chmod(path, mode) };
        (rc, rc == 0)
    })
}

/// `chown` as the guest's ids have it, on the file at `found`.
///
/// On the prefix's drive the host is not asked at all: who may give a file to
/// whom is decided by the guest's ids, by Darwin's rule - root may give
/// anything to anybody, an owner may change the group to one they are in - and
/// the new owner is recorded on the file, where `stat` finds it (see
/// `prefix::ownership`). That is `fakeroot`'s `chown`.
///
/// Off the drive the host's `chown` stands, except that a guest that is root
/// is told a refusal worked: there is nowhere to record it, and a program that
/// is root does not expect `chown` to fail.
///
/// # Safety
///
/// `found` names a file by a NUL-terminated path, or a descriptor.
unsafe fn chown_as_guest(
    found: crate::stat::Found<'_>,
    uid: libc::uid_t,
    gid: libc::gid_t,
    host_chown: impl FnOnce() -> c_int,
) -> c_int {
    use crate::stat::Found;
    let mut host: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        match found {
            Found::Fd(fd) => libc::fstat(fd, &mut host),
            Found::Path(path, true) => libc::stat(path.as_ptr(), &mut host),
            Found::Path(path, false) => libc::lstat(path.as_ptr(), &mut host),
            Found::At(dirfd, path, follow) => libc::fstatat(
                dirfd,
                path.as_ptr(),
                &mut host,
                if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW },
            ),
        }
    };
    if rc != 0 {
        return -1;
    }
    let me = crate::credentials::current();
    if !prefix::ownership::inside(found, host.st_dev as u64) {
        let rc = host_chown();
        if rc != 0 && crate::errno::get() == libc::EPERM && me.is_root() {
            return 0;
        }
        return rc;
    }
    let seen = crate::stat::as_seen(&host, found);
    let uid = if uid == u32::MAX { seen.uid } else { uid };
    let gid = if gid == u32::MAX { seen.gid } else { gid };
    let allowed = me.is_root() || (me.euid == seen.uid && uid == seen.uid && me.in_group(gid));
    if !allowed {
        crate::errno::set(libc::EPERM);
        return -1;
    }
    // FIXME (ACCURACY): a filesystem that keeps no user attributes, and a
    // symlink on Linux, which may not carry one, cannot hold the record; the
    // change is reported as made and the old owner stays.
    let _ = prefix::ownership::record(found, host.st_dev as u64, Some((uid, gid)));
    0
}

/// `int chown(const char *, uid_t, gid_t)`
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn chown(path: *const c_char, uid: libc::uid_t, gid: libc::gid_t) -> c_int {
    with_fallback(path, |path| {
        // SAFETY: a NUL-terminated host path.
        let found = crate::stat::Found::Path(unsafe { CStr::from_ptr(path) }, true);
        let rc = unsafe { chown_as_guest(found, uid, gid, || libc::chown(path, uid, gid)) };
        (rc, rc == 0)
    })
}

/// `int fchown(int, uid_t, gid_t)`
pub extern "C" fn fchown(fd: c_int, uid: libc::uid_t, gid: libc::gid_t) -> c_int {
    // SAFETY: the caller's descriptor.
    unsafe {
        chown_as_guest(crate::stat::Found::Fd(fd), uid, gid, || {
            libc::fchown(fd, uid, gid)
        })
    }
}

/// `int fchownat(int fd, const char *path, uid_t, gid_t, int flag)`
///
/// Darwin spells `AT_FDCWD` and `AT_SYMLINK_NOFOLLOW` its own way; any other
/// flag has no meaning here and is refused.
///
/// # Safety
///
/// `path` is a NUL-terminated string and `dirfd` a directory descriptor or
/// Darwin's `AT_FDCWD`.
pub unsafe extern "C" fn fchownat(
    dirfd: c_int,
    path: *const c_char,
    uid: libc::uid_t,
    gid: libc::gid_t,
    flag: c_int,
) -> c_int {
    const DARWIN_AT_SYMLINK_NOFOLLOW: c_int = 0x20;
    if flag & !DARWIN_AT_SYMLINK_NOFOLLOW != 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let follow = flag & DARWIN_AT_SYMLINK_NOFOLLOW == 0;
    let dirfd = match dirfd {
        crate::stat::DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    let host_flag = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    crate::stat::with_fallback_at(dirfd, path, |path| {
        // SAFETY: a NUL-terminated host path.
        let found = crate::stat::Found::At(dirfd, unsafe { CStr::from_ptr(path) }, follow);
        let rc = unsafe {
            chown_as_guest(found, uid, gid, || {
                libc::fchownat(dirfd, path, uid, gid, host_flag)
            })
        };
        (rc, rc == 0)
    })
}

/// `int lchown(const char *, uid_t, gid_t)`
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn lchown(path: *const c_char, uid: libc::uid_t, gid: libc::gid_t) -> c_int {
    with_fallback(path, |path| {
        // SAFETY: a NUL-terminated host path.
        let found = crate::stat::Found::Path(unsafe { CStr::from_ptr(path) }, false);
        let rc = unsafe { chown_as_guest(found, uid, gid, || libc::lchown(path, uid, gid)) };
        (rc, rc == 0)
    })
}

/// `int lchflags(const char *, uint32_t)`
///
/// The host has no equivalent operation for symlink flags. Still resolve the
/// guest path and report ordinary lookup errors before returning unsupported.
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn lchflags(path: *const c_char, _flags: u32) -> c_int {
    with_fallback(path, |path| {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `path` is a NUL-terminated path and `stat` is writable.
        let rc = unsafe { libc::lstat(path, stat.as_mut_ptr()) };
        if rc != 0 {
            return (rc, false);
        }
        crate::errno::set(libc::ENOTSUP);
        (-1, false)
    })
}

/// `int fchflags(int, uint32_t)`, refused as [`lchflags`] is once the
/// descriptor is known to be open.
pub extern "C" fn fchflags(fd: c_int, _flags: u32) -> c_int {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: a `stat` to fill, for the descriptor check alone.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return -1;
    }
    crate::errno::set(libc::ENOTSUP);
    -1
}

/// `int undelete(const char *)`
///
/// Recovery is not supported, but validate the path as an existing object so
/// missing paths retain the usual filesystem error. This never changes files.
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn undelete(path: *const c_char) -> c_int {
    with_fallback(path, |path| {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `path` is a NUL-terminated path and `stat` is writable.
        let rc = unsafe { libc::lstat(path, stat.as_mut_ptr()) };
        if rc != 0 {
            return (rc, false);
        }
        crate::errno::set(libc::ENOTSUP);
        (-1, false)
    })
}

/// `int truncate(const char *, off_t)`
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn truncate(path: *const c_char, length: libc::off_t) -> c_int {
    with_fallback(path, |path| {
        if !may_write(path) {
            return (-1, false);
        }
        let rc = unsafe { libc::truncate(path, length) };
        (rc, rc == 0)
    })
}

/// Darwin's `lseek` whence values. The first three are POSIX's everywhere, but
/// Linux and FreeBSD number `SEEK_DATA` and `SEEK_HOLE` the other way round.
mod whence {
    pub const SEEK_SET: i32 = 0;
    pub const SEEK_CUR: i32 = 1;
    pub const SEEK_END: i32 = 2;
    pub const SEEK_HOLE: i32 = 3;
    pub const SEEK_DATA: i32 = 4;
}

/// `off_t lseek(int fd, off_t offset, int whence)`
pub extern "C" fn lseek(fd: c_int, offset: libc::off_t, whence: c_int) -> libc::off_t {
    let host = match whence {
        whence::SEEK_SET => libc::SEEK_SET,
        whence::SEEK_CUR => libc::SEEK_CUR,
        whence::SEEK_END => libc::SEEK_END,
        whence::SEEK_HOLE => libc::SEEK_HOLE,
        whence::SEEK_DATA => libc::SEEK_DATA,
        _ => {
            crate::errno::set(libc::EINVAL);
            return -1;
        }
    };
    // SAFETY: `lseek` reads no memory of the caller's.
    unsafe { libc::lseek(fd, offset, host) }
}

/// `int link(const char *, const char *)`
///
/// # Safety
///
/// Both arguments are NUL-terminated strings.
pub unsafe extern "C" fn link(from: *const c_char, to: *const c_char) -> c_int {
    with_pair(from, to, |from, to| {
        if !may_write(to) {
            return -1;
        }
        unsafe { libc::link(from, to) }
    })
}

/// `int clonefileat(int, const char *, int, const char *, int)`
///
/// This host does not expose Darwin's copy-on-write operation, so files and
/// directories are copied independently. `CLONE_NOFOLLOW` preserves a source
/// link.
///
/// # Safety
///
/// Both paths are NUL-terminated and both descriptors are valid or Darwin's
/// `AT_FDCWD`.
pub unsafe extern "C" fn clonefileat(
    from_dirfd: c_int,
    from: *const c_char,
    to_dirfd: c_int,
    to: *const c_char,
    flags: c_int,
) -> c_int {
    const CLONE_NOFOLLOW: c_int = 1;
    const CLONE_NOOWNERCOPY: c_int = 2;
    const CLONE_ACL: c_int = 4;
    if flags & !(CLONE_NOFOLLOW | CLONE_NOOWNERCOPY | CLONE_ACL) != 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if flags & CLONE_ACL != 0 {
        crate::errno::set(libc::ENOTSUP);
        return -1;
    }
    if from.is_null() || to.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let from_dirfd = match from_dirfd {
        crate::stat::DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    let to_dirfd = match to_dirfd {
        crate::stat::DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    let (Some(from), Some(to)) = (host_at(from_dirfd, from), host_at(to_dirfd, to)) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    let mut from_resolved = from.clone();
    let from_path = crate::stat::with_fallback_at(libc::AT_FDCWD, from.as_ptr(), |path| {
        from_resolved = unsafe { CStr::from_ptr(path) }.to_owned();
        let source = unsafe { CStr::from_ptr(path) };
        let source = Path::new(std::ffi::OsStr::from_bytes(source.to_bytes()));
        let result = match flags & CLONE_NOFOLLOW != 0 {
            true => std::fs::symlink_metadata(source),
            false => std::fs::metadata(source),
        };
        match result {
            Ok(_) => (true, true),
            Err(error) => {
                crate::errno::set(error.raw_os_error().unwrap_or(libc::EIO));
                (false, false)
            }
        }
    });
    if !from_path {
        return -1;
    }
    let from_path = Path::new(std::ffi::OsStr::from_bytes(from_resolved.to_bytes()));
    crate::stat::with_fallback_at(libc::AT_FDCWD, to.as_ptr(), |destination| {
        if !crate::stat::may_write_at(libc::AT_FDCWD, destination) {
            return (-1, true);
        }
        let destination = Path::new(std::ffi::OsStr::from_bytes(
            unsafe { CStr::from_ptr(destination) }.to_bytes(),
        ));
        match crate::clonecopy::clonefile(from_path, destination, flags & CLONE_NOFOLLOW != 0) {
            Ok(()) => (0, true),
            Err(error) => {
                crate::errno::set(error.raw_os_error().unwrap_or(libc::EIO));
                (-1, true)
            }
        }
    })
}

/// `int clonefile(const char *, const char *, uint32_t)`
///
/// # Safety
///
/// Both paths are NUL-terminated strings.
pub unsafe extern "C" fn clonefile(from: *const c_char, to: *const c_char, flags: c_int) -> c_int {
    let here = crate::stat::DARWIN_AT_FDCWD;
    unsafe { clonefileat(here, from, here, to, flags) }
}

/// `int renameatx_np(int, const char *, int, const char *, unsigned int)`
///
/// `RENAME_SWAP` and `RENAME_EXCL` are Linux's `RENAME_EXCHANGE` and
/// `RENAME_NOREPLACE`; a host without `renameat2` refuses them rather than
/// rename without the guarantee asked for.
///
/// # Safety
///
/// Both paths are NUL-terminated strings.
pub unsafe extern "C" fn renameatx_np(
    from_dirfd: c_int,
    from: *const c_char,
    to_dirfd: c_int,
    to: *const c_char,
    flags: u32,
) -> c_int {
    const RENAME_SWAP: u32 = 2;
    const RENAME_EXCL: u32 = 4;
    if flags & !(RENAME_SWAP | RENAME_EXCL) != 0 || flags == RENAME_SWAP | RENAME_EXCL {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let dirfd = |fd| match fd {
        crate::stat::DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    let (Some(from), Some(to)) = (
        host_at(dirfd(from_dirfd), from),
        host_at(dirfd(to_dirfd), to),
    ) else {
        crate::errno::set(libc::EFAULT);
        return -1;
    };
    // Both ends, as `rename`: a system file renamed away is as gone as one deleted.
    if !may_write(from.as_ptr()) || !may_write(to.as_ptr()) {
        return -1;
    }
    #[cfg(target_os = "linux")]
    {
        let mut linux = 0;
        if flags & RENAME_SWAP != 0 {
            linux |= libc::RENAME_EXCHANGE;
        }
        if flags & RENAME_EXCL != 0 {
            linux |= libc::RENAME_NOREPLACE;
        }
        // SAFETY: two NUL-terminated host paths.
        unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                linux,
            )
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        if flags != 0 {
            crate::errno::set(libc::ENOTSUP);
            return -1;
        }
        // SAFETY: two NUL-terminated host paths.
        unsafe { libc::rename(from.as_ptr(), to.as_ptr()) }
    }
}

/// `int rename(const char *, const char *)`
///
/// # Safety
///
/// Both arguments are NUL-terminated strings.
pub unsafe extern "C" fn rename(from: *const c_char, to: *const c_char) -> c_int {
    with_pair(from, to, |from, to| {
        // Both ends: a system file renamed away is as gone as one deleted.
        if !may_write(from) || !may_write(to) {
            return -1;
        }
        unsafe { libc::rename(from, to) }
    })
}

/// `int symlink(const char *target, const char *link)`
///
/// The *target* is translated too, and deliberately: it is stored in the link
/// verbatim, and an absolute one is read back by [`crate::stat::readlink`],
/// which spells a host target the guest's way. Storing the host spelling is
/// what makes those two agree, and it is also what lets a host tool follow a
/// link a guest made.
///
/// # Safety
///
/// Both arguments are NUL-terminated strings.
pub unsafe extern "C" fn symlink(target: *const c_char, link: *const c_char) -> c_int {
    let (Some(target), Some(link)) = (host(target), host(link)) else {
        crate::errno::set(libc::EFAULT);
        return -1;
    };
    if !may_write(link.as_ptr()) {
        return -1;
    }
    // SAFETY: two NUL-terminated paths.
    unsafe { libc::symlink(target.as_ptr(), link.as_ptr()) }
}

/// `int mkstemp(char *template)`
///
/// The template is rewritten in place with the name that was created, and the
/// caller sized it. The host writes a *host* path there, so the guest spelling
/// goes back instead - which is the same length by construction, the two names
/// differing only in the six characters `mkstemp` replaced.
///
/// # Safety
///
/// `template` is a NUL-terminated string ending in `XXXXXX`, writable for its
/// own length.
pub unsafe extern "C" fn mkstemp(template: *mut c_char) -> c_int {
    let Some((mut host_template, room)) = (unsafe { host_template(template) }) else {
        return -1;
    };
    // SAFETY: a writable NUL-terminated template.
    let fd = unsafe { libc::mkstemp(host_template.as_mut_ptr().cast::<c_char>()) };
    if fd < 0 {
        return fd;
    }
    if crate::credentials::creates_as_another()
        && let Ok(made) = CStr::from_bytes_until_nul(&host_template)
    {
        crate::credentials::created(libc::AT_FDCWD, made);
    }
    // SAFETY: `mkstemp` rewrote the template in place, NUL-terminated.
    if !unsafe { write_back(template, &host_template, room) } {
        // The file exists and the caller will never learn its name, so it is
        // removed rather than left behind in the prefix.
        // SAFETY: a descriptor this call just opened and the path that made it.
        unsafe {
            libc::close(fd);
            libc::unlink(host_template.as_ptr().cast::<c_char>());
        }
        return -1;
    }
    fd
}

/// `char *mkdtemp(char *template)`
///
/// # Safety
///
/// As [`mkstemp`].
pub unsafe extern "C" fn mkdtemp(template: *mut c_char) -> *mut c_char {
    let Some((mut host_template, room)) = (unsafe { host_template(template) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a writable NUL-terminated template.
    if unsafe { libc::mkdtemp(host_template.as_mut_ptr().cast::<c_char>()) }.is_null() {
        return std::ptr::null_mut();
    }
    if crate::credentials::creates_as_another()
        && let Ok(made) = CStr::from_bytes_until_nul(&host_template)
    {
        crate::credentials::created(libc::AT_FDCWD, made);
    }
    // SAFETY: `mkdtemp` rewrote the template in place, NUL-terminated.
    if !unsafe { write_back(template, &host_template, room) } {
        // SAFETY: a directory this call just made and nothing has entered.
        unsafe { libc::rmdir(host_template.as_ptr().cast::<c_char>()) };
        return std::ptr::null_mut();
    }
    // Darwin answers with the caller's own buffer.
    template
}

/// `char *mkdtempat_np(int dfd, char *path)`: [`mkdtemp`] with a template
/// relative to a directory descriptor.
///
/// The host has no such call, so the name is made here: each trailing `X` is
/// replaced from the host's random source and `mkdirat` tried, until a name is
/// free. The template is changed in place, as `mkdtemp` changes it.
///
/// # Safety
///
/// `path` is a writable NUL-terminated template.
pub unsafe extern "C" fn mkdtempat_np(dfd: c_int, path: *mut c_char) -> *mut c_char {
    if path.is_null() {
        crate::errno::set(libc::EFAULT);
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's string.
    let relative = unsafe { *path } as u8 != b'/';
    if dfd == crate::stat::DARWIN_AT_FDCWD || !relative {
        // SAFETY: the caller's template.
        return unsafe { mkdtemp(path) };
    }
    // SAFETY: the caller's NUL-terminated, writable template.
    let template = unsafe { std::slice::from_raw_parts_mut(path.cast::<u8>(), libc::strlen(path)) };
    let xs = template.iter().rev().take_while(|b| **b == b'X').count();
    if xs == 0 {
        crate::errno::set(libc::EINVAL);
        return std::ptr::null_mut();
    }
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let start = template.len() - xs;
    for _ in 0..1000 {
        let mut random = vec![0u8; xs];
        if getrandom::fill(&mut random).is_err() {
            crate::errno::set(libc::EIO);
            return std::ptr::null_mut();
        }
        for (slot, byte) in template[start..].iter_mut().zip(random) {
            *slot = ALPHABET[byte as usize % ALPHABET.len()];
        }
        // SAFETY: the template, rewritten in place and still NUL-terminated.
        if make_dir(dfd, unsafe { CStr::from_ptr(path) }, 0o700) == 0 {
            return path;
        }
        if crate::errno::get() != libc::EEXIST {
            return std::ptr::null_mut();
        }
    }
    crate::errno::set(libc::EEXIST);
    std::ptr::null_mut()
}

/// Darwin's temporary-name alphabet, in its order: a collision advances a
/// character to the next one here.
const TEMP_PADCHAR: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `char *_mktemp(char *path)`, which Darwin's `mktemp` is.
///
/// Darwin's `_gettemp` without a descriptor: the trailing `X`s are randomised,
/// then the name is stepped through the alphabet until `lstat` finds nothing.
/// Only the guest's buffer is rewritten; each candidate is looked up by its
/// guest spelling, case-folded as every other lookup is.
///
/// # Safety
///
/// `path` is a writable NUL-terminated template.
pub unsafe extern "C" fn _mktemp(path: *mut c_char) -> *mut c_char {
    if path.is_null() {
        crate::errno::set(libc::EFAULT);
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's NUL-terminated string.
    let len = unsafe { libc::strlen(path) };
    if len >= DARWIN_PATH_MAX {
        crate::errno::set(libc::ENAMETOOLONG);
        return std::ptr::null_mut();
    }
    if len == 0 {
        crate::errno::set(libc::EINVAL);
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's template, writable for its own length.
    let template = unsafe { std::slice::from_raw_parts_mut(path.cast::<u8>(), len) };
    let start = len - template.iter().rev().take_while(|b| **b == b'X').count();
    let mut random = vec![0u8; (len - start) * size_of::<u32>()];
    if getrandom::fill(&mut random).is_err() {
        crate::errno::set(libc::EIO);
        return std::ptr::null_mut();
    }
    for (slot, word) in template[start..]
        .iter_mut()
        .zip(random.chunks_exact(size_of::<u32>()))
    {
        let word = u32::from_ne_bytes(word.try_into().unwrap());
        *slot = TEMP_PADCHAR[word as usize % TEMP_PADCHAR.len()];
    }
    let first = template[start..].to_vec();
    loop {
        // SAFETY: the template, rewritten in place and still NUL-terminated;
        // a null `struct stat` asks only whether the name exists.
        if unsafe { crate::stat::lstat(template.as_ptr().cast(), std::ptr::null_mut()) } != 0 {
            return match crate::errno::get() {
                libc::ENOENT => path,
                _ => std::ptr::null_mut(),
            };
        }
        // Count through every name the `X`s can spell, as an odometer started
        // at the first random draw, so each is tried once.
        let mut at = start;
        loop {
            if at == len {
                crate::errno::set(libc::EEXIST);
                return std::ptr::null_mut();
            }
            let next = TEMP_PADCHAR
                .iter()
                .position(|c| *c == template[at])
                .map_or(0, |i| (i + 1) % TEMP_PADCHAR.len());
            template[at] = TEMP_PADCHAR[next];
            if template[at] != first[at - start] {
                break;
            }
            at += 1;
        }
    }
}

/// The host spelling of a `XXXXXX` template, and how many bytes the caller's
/// buffer holds for the name.
///
/// # Safety
///
/// `template` is null or NUL-terminated.
unsafe fn host_template(template: *mut c_char) -> Option<(Vec<u8>, usize)> {
    if template.is_null() {
        crate::errno::set(libc::EFAULT);
        return None;
    }
    let guest = unsafe { CStr::from_ptr(template) };
    let room = guest.to_bytes().len();
    Some((
        prefix::ns::to_host(guest)
            .into_owned()
            .into_bytes_with_nul(),
        room,
    ))
}

/// Write the guest spelling of a rewritten template back into the caller's
/// buffer, answering whether it fitted.
///
/// # Safety
///
/// `template` is writable for `room` bytes plus its NUL, and `made` is a
/// NUL-terminated host path.
unsafe fn write_back(template: *mut c_char, made: &[u8], room: usize) -> bool {
    let made = unsafe { CStr::from_ptr(made.as_ptr().cast::<c_char>()) };
    let Some(spelled) = guest_path(made) else {
        crate::errno::set(libc::ENOENT);
        return false;
    };
    let bytes = spelled.to_bytes();
    // The two spellings are the same length by construction - the host call
    // only replaced the six `X`s - so this is a guard, not a path anything is
    // expected to take.
    if bytes.len() != room {
        crate::errno::set(libc::ENAMETOOLONG);
        return false;
    }
    // SAFETY: `template` is writable for `room` bytes and `bytes` is exactly
    // that many; the NUL already there stays where it is.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), template, room) };
    true
}

/// `FSIOC_SYNC_VOLUME`, `_IOW('A', 1, uint32_t)`.
const FSIOC_SYNC_VOLUME: u64 = 0x8004_4101;
/// `FSOPT_NOFOLLOW`.
const FSOPT_NOFOLLOW: u32 = 0x1;

/// `int fsctl(const char *path, unsigned long request, void *data,
/// unsigned int options)`
///
/// An `ioctl` on the volume holding `path`. Only the volume-independent
/// request, syncing it, is answered; the rest are each file system's own
/// (APFS snapshots, HFS allocation hints), and a host file system has none of
/// them, so they fail as a Darwin file system without the request fails.
///
/// # Safety
///
/// `path` is a NUL-terminated string, and `data` is what `request` says.
pub unsafe extern "C" fn fsctl(
    path: *const c_char,
    request: u64,
    data: *mut c_void,
    options: u32,
) -> c_int {
    if path.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let found = with_fallback(path, |path| {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: a host path and a `stat` to fill.
        let rc = unsafe {
            match options & FSOPT_NOFOLLOW {
                0 => libc::stat(path, st.as_mut_ptr()),
                _ => libc::lstat(path, st.as_mut_ptr()),
            }
        };
        (rc, rc == 0)
    });
    if found != 0 {
        return -1;
    }
    // SAFETY: the caller's.
    unsafe { volume_request(request, data) }
}

/// `int ffsctl(int fd, unsigned long request, void *data, unsigned int options)`
///
/// [`fsctl`] on the volume holding an open descriptor.
///
/// # Safety
///
/// `data` is what `request` says.
pub unsafe extern "C" fn ffsctl(
    fd: c_int,
    request: u64,
    data: *mut c_void,
    _options: u32,
) -> c_int {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: a `stat` to fill, for the descriptor check alone.
    if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
        return -1;
    }
    // SAFETY: the caller's.
    unsafe { volume_request(request, data) }
}

/// Answer an `fsctl` request on a volume that exists.
///
/// # Safety
///
/// `data` is what `request` says.
unsafe fn volume_request(request: u64, data: *mut c_void) -> c_int {
    match request {
        FSIOC_SYNC_VOLUME => {
            // The argument is `FSCTL_SYNC_WAIT` and `FSCTL_SYNC_FULLSYNC`
            // flags, which Darwin copies in before looking at.
            if data.is_null() {
                crate::errno::set(libc::EFAULT);
                return -1;
            }
            // Every volume rather than the one: no portable call syncs a
            // single file system.
            // SAFETY: `sync` has no preconditions.
            unsafe { libc::sync() };
            0
        }
        _ => {
            crate::errno::set(libc::ENOTTY);
            -1
        }
    }
}

exports::exports! {
    functions {
        "chdir" => chdir,
        "fsctl" => fsctl,
        "ffsctl" => ffsctl,
        "getcwd" => getcwd,
        "unlink" => unlink,
        "unlinkat" => unlinkat,
        "rmdir" => rmdir,
        "remove" => remove,
        "mkdir" => mkdir,
        "mkdirat" => mkdirat,
        "chmod" => chmod,
        "chown" => chown,
        "lchown" => lchown,
        "fchown" => fchown,
        "fchownat" => fchownat,
        "lchflags" => lchflags,
        "fchflags" => fchflags,
        "undelete" => undelete,
        "truncate" => truncate,
        "lseek" => lseek,
        "link" => link,
        "clonefileat" => clonefileat,
        "clonefile" => clonefile,
        "renameatx_np" => renameatx_np,
        "rename" => rename,
        "symlink" => symlink,
        "mkstemp" => mkstemp,
        "mkdtemp" => mkdtemp,
        "mkdtempat_np" => mkdtempat_np,
        "_mktemp" => _mktemp,
        "mktemp" => _mktemp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;

    /// A file with no holes has its data at the start and its only hole, the
    /// implicit one, at the end - whichever way the host numbers the two.
    #[test]
    fn lseek_finds_holes_and_data_by_darwins_numbering() {
        let at = std::env::temp_dir().join(format!("rosemary-lseek-{}", std::process::id()));
        std::fs::write(&at, vec![1u8; 4096]).unwrap();
        let file = std::fs::File::open(&at).unwrap();
        let _ = std::fs::remove_file(&at);
        let fd = file.as_raw_fd();
        assert_eq!(lseek(fd, 0, whence::SEEK_HOLE), 4096);
        assert_eq!(lseek(fd, 0, whence::SEEK_DATA), 0);
        assert_eq!(lseek(fd, 0, 5), -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
    }

    #[test]
    fn fsctl_syncs_a_volume_and_refuses_what_the_file_system_would() {
        let mut flags = 0u32;
        let data = (&raw mut flags).cast();
        let tmp = c"/tmp";
        assert_eq!(
            unsafe { fsctl(tmp.as_ptr(), FSIOC_SYNC_VOLUME, data, 0) },
            0
        );

        assert_eq!(unsafe { fsctl(tmp.as_ptr(), 0xc008_6801, data, 0) }, -1);
        assert_eq!(crate::errno::get(), libc::ENOTTY);

        let missing = c"/nonexistent-rosemary-fsctl";
        assert_eq!(
            unsafe { fsctl(missing.as_ptr(), FSIOC_SYNC_VOLUME, data, 0) },
            -1
        );
        assert_eq!(crate::errno::get(), libc::ENOENT);
    }

    #[test]
    fn fchflags_refuses_an_open_file_and_a_closed_descriptor() {
        let file = std::fs::File::open("/").unwrap();
        assert_eq!(fchflags(file.as_raw_fd(), 0), -1);
        assert_eq!(crate::errno::get(), libc::ENOTSUP);
        assert_eq!(fchflags(-1, 0), -1);
        assert_eq!(crate::errno::get(), libc::EBADF);
    }

    #[test]
    fn ffsctl_refuses_a_file_system_request_and_a_closed_descriptor() {
        let file = std::fs::File::open("/").unwrap();
        let mut flags = 0u32;
        let data = (&raw mut flags).cast();
        assert_eq!(
            unsafe { ffsctl(file.as_raw_fd(), 0xc008_6801, data, 0) },
            -1
        );
        assert_eq!(crate::errno::get(), libc::ENOTTY);
        assert_eq!(unsafe { ffsctl(-1, FSIOC_SYNC_VOLUME, data, 0) }, -1);
        assert_eq!(crate::errno::get(), libc::EBADF);
    }

    /// A directory under the host's temporary directory, removed on drop so
    /// a failed assertion does not leave it for the next run.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(test: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rosemary-fs-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temporary directory");
            Self(dir)
        }

        fn c_path(&self, name: &str) -> CString {
            let path = self.0.join(name);
            CString::new(path.as_os_str().as_bytes()).expect("no NUL")
        }

        /// A read-only descriptor for `name`, closed on drop.
        fn open(&self, name: &str) -> std::fs::File {
            std::fs::File::open(self.0.join(name)).expect("an openable path")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mkdirat_translates_darwin_at_fdcwd() {
        let scratch = Scratch::new("mkdirat-cwd");
        let path = scratch.c_path("made");
        // SAFETY: a NUL-terminated path and Darwin's AT_FDCWD.
        let rc = unsafe { mkdirat(crate::stat::DARWIN_AT_FDCWD, path.as_ptr(), 0o755) };
        assert_eq!(rc, 0, "mkdirat: {}", std::io::Error::last_os_error());
        assert!(scratch.0.join("made").is_dir());
    }

    #[test]
    fn mkdirat_resolves_a_relative_path_against_its_descriptor() {
        // SAFETY: takes no argument and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: root searches a locked directory anyway");
            return;
        }
        let scratch = Scratch::new("mkdirat-relative");
        std::fs::create_dir_all(scratch.0.join("locked/inner")).expect("a directory");
        let dir = scratch.open("locked/inner");
        let locked = scratch.0.join("locked");
        let permissions = |mode| std::fs::Permissions::from_mode(mode);
        // No search permission above `inner`: only a lookup that starts from
        // the descriptor, rather than from an absolute spelling of it, works.
        std::fs::set_permissions(&locked, permissions(0o000)).expect("a chmod");
        // SAFETY: a live directory descriptor and a NUL-terminated relative path.
        let rc = unsafe { mkdirat(dir.as_raw_fd(), c"child".as_ptr(), 0o755) };
        let error = std::io::Error::last_os_error();
        std::fs::set_permissions(&locked, permissions(0o755)).expect("a chmod");
        assert_eq!(rc, 0, "mkdirat: {error}");
        assert!(scratch.0.join("locked/inner/child").is_dir());
    }

    #[test]
    fn mkdirat_reports_a_bad_descriptor_as_the_host_does() {
        let scratch = Scratch::new("mkdirat-badfd");
        std::fs::write(scratch.0.join("file"), b"x").expect("a file");
        let file = scratch.open("file");
        crate::errno::set(0);
        // SAFETY: a NUL-terminated relative path; -5 is never a descriptor.
        assert_eq!(unsafe { mkdirat(-5, c"child".as_ptr(), 0o755) }, -1);
        assert_eq!(crate::errno::get(), libc::EBADF);
        // SAFETY: a live, non-directory descriptor and a NUL-terminated path.
        assert_eq!(
            unsafe { mkdirat(file.as_raw_fd(), c"child".as_ptr(), 0o755) },
            -1
        );
        assert_eq!(crate::errno::get(), libc::ENOTDIR);
        crate::errno::set(0);
    }

    const DARWIN_AT_REMOVEDIR: c_int = 0x0080;
    const DARWIN_AT_REMOVEDIR_DATALESS: c_int = 0x0100;

    #[test]
    fn unlinkat_translates_darwin_at_fdcwd_and_removedir() {
        let scratch = Scratch::new("unlinkat-cwd");
        std::fs::create_dir(scratch.0.join("dir")).expect("a directory");
        let path = scratch.c_path("dir");
        crate::errno::set(0);
        // SAFETY: a NUL-terminated path and Darwin's AT_FDCWD.
        unsafe {
            assert_eq!(unlinkat(crate::stat::DARWIN_AT_FDCWD, path.as_ptr(), 0), -1);
            assert_eq!(crate::errno::get(), libc::EISDIR);
            let flag = DARWIN_AT_REMOVEDIR;
            assert_eq!(
                unlinkat(crate::stat::DARWIN_AT_FDCWD, path.as_ptr(), flag),
                0
            );
        }
        assert!(!scratch.0.join("dir").exists());
        crate::errno::set(0);
    }

    #[test]
    fn unlinkat_removes_relative_to_its_descriptor() {
        let scratch = Scratch::new("unlinkat-relative");
        std::fs::create_dir(scratch.0.join("dir")).expect("a directory");
        std::fs::write(scratch.0.join("file"), b"x").expect("a file");
        let dir = scratch.open(".");
        let fd = dir.as_raw_fd();
        // SAFETY: a live directory descriptor and NUL-terminated relative paths.
        unsafe {
            assert_eq!(unlinkat(fd, c"file".as_ptr(), 0), 0);
            assert_eq!(
                unlinkat(fd, c"dir".as_ptr(), DARWIN_AT_REMOVEDIR_DATALESS),
                0
            );
        }
        assert!(!scratch.0.join("file").exists());
        assert!(!scratch.0.join("dir").exists());
    }

    #[test]
    fn unlinkat_refuses_a_flag_it_cannot_honour() {
        let scratch = Scratch::new("unlinkat-flags");
        std::fs::write(scratch.0.join("file"), b"x").expect("a file");
        let dir = scratch.open(".");
        // Darwin's AT_SYMLINK_NOFOLLOW_ANY, which has no host equivalent.
        const DARWIN_AT_SYMLINK_NOFOLLOW_ANY: c_int = 0x0800;
        crate::errno::set(0);
        // SAFETY: a live directory descriptor and a NUL-terminated relative path.
        let rc = unsafe {
            unlinkat(
                dir.as_raw_fd(),
                c"file".as_ptr(),
                DARWIN_AT_SYMLINK_NOFOLLOW_ANY,
            )
        };
        assert_eq!(rc, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        assert!(scratch.0.join("file").exists());
        crate::errno::set(0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn unlinkat_resolve_beneath_removes_inside_and_refuses_an_escape() {
        let flag = crate::stat::beneath::SUPPORTED;
        let scratch = Scratch::new("unlinkat-beneath");
        std::fs::create_dir_all(scratch.0.join("root/sub/dir")).expect("a directory");
        std::fs::write(scratch.0.join("root/sub/file"), b"x").expect("a file");
        std::fs::write(scratch.0.join("outside"), b"x").expect("a file");
        std::os::unix::fs::symlink("..", scratch.0.join("root/up")).expect("a symlink");
        let root = scratch.open("root");
        let fd = root.as_raw_fd();
        // SAFETY: a live directory descriptor and NUL-terminated relative paths.
        unsafe {
            assert_eq!(unlinkat(fd, c"sub/file".as_ptr(), flag), 0);
            assert_eq!(
                unlinkat(fd, c"sub/dir/".as_ptr(), flag | DARWIN_AT_REMOVEDIR),
                0
            );
            for escape in [c"../outside", c"up/outside", c"sub/../..", c"/tmp/x"] {
                crate::errno::set(0);
                assert_eq!(unlinkat(fd, escape.as_ptr(), flag), -1, "{escape:?}");
                assert_eq!(crate::errno::get(), libc::EXDEV, "{escape:?}");
            }
            // The symlink itself is inside, and removing it does not follow it.
            assert_eq!(unlinkat(fd, c"up".as_ptr(), flag), 0);
        }
        assert!(!scratch.0.join("root/sub/file").exists());
        assert!(!scratch.0.join("root/sub/dir").exists());
        assert!(scratch.0.join("outside").exists());
        assert!(!scratch.0.join("root/up").exists());
        crate::errno::set(0);
    }

    #[test]
    fn mktemp_names_a_free_file_without_creating_it() {
        let scratch = Scratch::new("mktemp-free");
        let template = scratch.c_path("name.XXXXXX");
        let mut buffer = template.into_bytes_with_nul();
        let path = buffer.as_mut_ptr().cast::<c_char>();
        // SAFETY: a writable NUL-terminated template.
        assert_eq!(unsafe { _mktemp(path) }, path);
        // SAFETY: rewritten in place, still NUL-terminated.
        let made = unsafe { CStr::from_ptr(path) }.to_str().unwrap();
        let name = made.rsplit('/').next().unwrap();
        assert_eq!(name.len(), "name.XXXXXX".len());
        assert!(name[5..].bytes().all(|b| TEMP_PADCHAR.contains(&b)));
        assert!(!Path::new(made).exists());
    }

    #[test]
    fn mktemp_steps_past_taken_names_and_reports_exhaustion() {
        let scratch = Scratch::new("mktemp-taken");
        // One `X` spells 62 names; with all but one taken, the free one is found.
        for &c in &TEMP_PADCHAR[1..] {
            std::fs::write(scratch.0.join(format!("f{}", c as char)), b"").unwrap();
        }
        let mut buffer = scratch.c_path("fX").into_bytes_with_nul();
        let path = buffer.as_mut_ptr().cast::<c_char>();
        // SAFETY: a writable NUL-terminated template.
        assert_eq!(unsafe { _mktemp(path) }, path);
        // SAFETY: rewritten in place, still NUL-terminated.
        assert!(unsafe { CStr::from_ptr(path) }.to_bytes().ends_with(b"/f0"));

        std::fs::write(scratch.0.join("f0"), b"").unwrap();
        let mut buffer = scratch.c_path("fX").into_bytes_with_nul();
        // SAFETY: as above.
        assert!(unsafe { _mktemp(buffer.as_mut_ptr().cast()) }.is_null());
        assert_eq!(crate::errno::get(), libc::EEXIST);
        crate::errno::set(0);
    }
}
