//! `fsctl` and `ffsctl`, xnu's filesystem control calls (`bsd/vfs/vfs_syscalls.c`).
//!
//! A request is an ioctl number against the volume a file is on. xnu handles a
//! few generic requests itself and hands every other one to the filesystem's
//! `VNOP_IOCTL`, which answers `ENOTTY` for a request it does not know. The
//! requests that belong to particular Apple filesystems (APFS, HFS) and those
//! that need an entitlement have no host filesystem to answer them, so they
//! take that same path: the call fails with `ENOTTY`, as it does on a volume
//! whose filesystem implements none of them.

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};

use crate::stat::with_fallback;

const IOC_IN: c_ulong = 0x8000_0000;
const IOCPARM_MASK: c_ulong = 0x1fff;

/// `IOCBASECMD`: the request without its parameter size.
const fn base(request: c_ulong) -> c_ulong {
    request & !(IOCPARM_MASK << 16)
}

/// `IOCPARM_LEN`
const fn parameter_size(request: c_ulong) -> usize {
    ((request >> 16) & IOCPARM_MASK) as usize
}

/// `FSIOC_SYNC_VOLUME`, `_IOW('A', 1, uint32_t)`.
const FSIOC_SYNC_VOLUME: c_ulong = 0x8004_4101;
/// `FSIOC_FD_ONLY_OPEN_ONCE`, `_IOWR('A', 21, uint32_t)`, which `fsctl`
/// refuses: a file reached by path is not open at all.
const FSIOC_FD_ONLY_OPEN_ONCE: c_ulong = 0xc004_4115;
/// `FSCTL_SYNC_WAIT`: wait for the sync to finish.
const FSCTL_SYNC_WAIT: u32 = 1 << 1;
const FSOPT_NOFOLLOW: c_uint = 0x1;

/// `fsctl_bogus_command_compat`: callers that spelled a generic request
/// without its size, as the old `FSCTL_*` names do, still reach it.
fn compat(request: c_ulong) -> c_ulong {
    if request == base(FSIOC_SYNC_VOLUME) {
        FSIOC_SYNC_VOLUME
    } else {
        request
    }
}

/// What the request is to act on: the descriptor it was given, or the path,
/// which is opened only for a request that needs it.
enum Target {
    Path(*const c_char),
    Fd(c_int),
}

/// `fsctl_internal`, for a file `st` describes. `Err` is the errno.
///
/// # Safety
///
/// `data` must be readable for the request's parameter size when the request
/// has `IOC_IN`.
unsafe fn internal(
    target: Target,
    st: &libc::stat,
    request: c_ulong,
    data: *mut c_void,
) -> Result<(), c_int> {
    let kind = st.st_mode & libc::S_IFMT;
    if kind == libc::S_IFCHR || kind == libc::S_IFBLK {
        return Err(libc::ENOTTY);
    }
    let request = compat(request);
    let size = parameter_size(request);
    if request & IOC_IN != 0 && size != 0 && data.is_null() {
        return Err(libc::EFAULT);
    }
    match request {
        FSIOC_SYNC_VOLUME => {
            // SAFETY: the request has `IOC_IN` and a 4-byte parameter, which
            // the caller vouched for and which is not null.
            let flags = unsafe { data.cast::<u32>().read_unaligned() };
            sync_volume(target, flags & FSCTL_SYNC_WAIT != 0);
            Ok(())
        }
        _ => Err(libc::ENOTTY),
    }
}

/// `handle_sync_volume`: write out the volume the file is on. xnu ignores
/// what the sync itself returns, and so does this.
fn sync_volume(target: Target, _wait: bool) {
    // Linux's `syncfs` waits for the writes it starts, which a `MNT_NOWAIT`
    // sync need not do, but may.
    let fd = match target {
        Target::Fd(fd) => fd,
        Target::Path(host) => {
            // SAFETY: `host` is a NUL-terminated host path.
            let fd =
                unsafe { libc::open(host, libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK) };
            if fd < 0 {
                // A file that cannot be read still names a volume; without a
                // descriptor for it, every volume is written out.
                // SAFETY: `sync` takes no arguments.
                unsafe { libc::sync() };
                return;
            }
            sync_descriptor(fd);
            // SAFETY: `fd` was opened above and is closed once.
            unsafe { libc::close(fd) };
            return;
        }
    };
    sync_descriptor(fd);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn sync_descriptor(fd: c_int) {
    // SAFETY: `syncfs` only reads the descriptor.
    unsafe { libc::syncfs(fd) };
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn sync_descriptor(_fd: c_int) {
    // SAFETY: `sync` takes no arguments.
    unsafe { libc::sync() };
}

/// `int fsctl(const char *path, unsigned long request, void *data, unsigned int options)`
///
/// # Safety
///
/// `path` must be null or NUL-terminated, and `data` must be what `request`
/// says it is.
pub unsafe extern "C" fn fsctl(
    path: *const c_char,
    request: c_ulong,
    data: *mut c_void,
    options: c_uint,
) -> c_int {
    if request == FSIOC_FD_ONLY_OPEN_ONCE {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if path.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let follow = options & FSOPT_NOFOLLOW == 0;
    let result = with_fallback(path, |host| {
        // SAFETY: a zeroed `struct stat` is what the host is asked to fill.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `host` is a NUL-terminated host path and `st` a live local.
        let rc = unsafe {
            if follow {
                libc::stat(host, &mut st)
            } else {
                libc::lstat(host, &mut st)
            }
        };
        if rc != 0 {
            return (Err(crate::errno::get()), false);
        }
        // SAFETY: the caller's obligation for `data`.
        (
            unsafe { internal(Target::Path(host), &st, request, data) },
            true,
        )
    });
    finish(result)
}

/// `int ffsctl(int fd, unsigned long request, void *data, unsigned int options)`
///
/// # Safety
///
/// `data` must be what `request` says it is.
pub unsafe extern "C" fn ffsctl(
    fd: c_int,
    request: c_ulong,
    data: *mut c_void,
    _options: c_uint,
) -> c_int {
    // SAFETY: a zeroed `struct stat` is what the host is asked to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a live local; a bad descriptor is the host's `EBADF`.
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return -1;
    }
    // SAFETY: the caller's obligation for `data`.
    finish(unsafe { internal(Target::Fd(fd), &st, request, data) })
}

fn finish(result: Result<(), c_int>) -> c_int {
    match result {
        Ok(()) => 0,
        Err(errno) => {
            crate::errno::set(errno);
            -1
        }
    }
}

exports::exports! {
    functions {
        "fsctl" => fsctl,
        "ffsctl" => ffsctl,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn errno() -> c_int {
        crate::errno::get()
    }

    /// The generic sync request succeeds, spelled with or without its size.
    #[test]
    fn a_volume_syncs_by_path_and_by_descriptor() {
        let dir = CString::new(std::env::temp_dir().to_str().unwrap()).unwrap();
        let mut flags = FSCTL_SYNC_WAIT;
        let data = (&raw mut flags).cast::<c_void>();
        // SAFETY: a NUL-terminated path and a live `uint32_t`.
        assert_eq!(
            unsafe { fsctl(dir.as_ptr(), FSIOC_SYNC_VOLUME, data, 0) },
            0
        );
        // SAFETY: as above.
        assert_eq!(
            unsafe { fsctl(dir.as_ptr(), base(FSIOC_SYNC_VOLUME), data, 0) },
            0
        );
        // SAFETY: a NUL-terminated path.
        let fd = unsafe { libc::open(dir.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        assert!(fd >= 0);
        // SAFETY: an open descriptor and a live `uint32_t`.
        assert_eq!(unsafe { ffsctl(fd, FSIOC_SYNC_VOLUME, data, 0) }, 0);
        // SAFETY: `fd` was opened above.
        unsafe { libc::close(fd) };
    }

    /// What xnu refuses before and after it finds the file.
    #[test]
    fn requests_xnu_refuses_fail_as_xnu_fails_them() {
        let dir = CString::new(std::env::temp_dir().to_str().unwrap()).unwrap();
        let mut buf = [0u8; 32];
        let data = buf.as_mut_ptr().cast::<c_void>();
        // A request no host filesystem implements: the filesystem's ENOTTY,
        // with nothing written back.
        buf.fill(0xa5);
        // SAFETY: a NUL-terminated path and a 32-byte buffer.
        assert_eq!(unsafe { fsctl(dir.as_ptr(), 0xc020_7a17, data, 0) }, -1);
        assert_eq!(errno(), libc::ENOTTY);
        assert!(buf.iter().all(|&b| b == 0xa5));

        // SAFETY: as above.
        assert_eq!(
            unsafe { fsctl(dir.as_ptr(), FSIOC_FD_ONLY_OPEN_ONCE, data, 0) },
            -1
        );
        assert_eq!(errno(), libc::EINVAL);

        let missing = c"/nonexistent/rosemary-fsctl";
        // SAFETY: as above.
        assert_eq!(
            unsafe { fsctl(missing.as_ptr(), FSIOC_SYNC_VOLUME, data, 0) },
            -1
        );
        assert_eq!(errno(), libc::ENOENT);

        // SAFETY: a NUL-terminated path; the request's `IOC_IN` parameter is null.
        let rc = unsafe { fsctl(dir.as_ptr(), FSIOC_SYNC_VOLUME, std::ptr::null_mut(), 0) };
        assert_eq!((rc, errno()), (-1, libc::EFAULT));

        // A character device is not on a volume the request can reach.
        // SAFETY: as above.
        assert_eq!(
            unsafe { fsctl(c"/dev/null".as_ptr(), FSIOC_SYNC_VOLUME, data, 0) },
            -1
        );
        assert_eq!(errno(), libc::ENOTTY);

        // SAFETY: a descriptor that is not open.
        assert_eq!(unsafe { ffsctl(-1, FSIOC_SYNC_VOLUME, data, 0) }, -1);
        assert_eq!(errno(), libc::EBADF);
    }
}
