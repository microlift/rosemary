use std::{
    ffi::{CStr, c_char, c_int, c_void},
    mem::MaybeUninit,
};

use crate::translation::DarwinTranslation;

/// Darwin's `struct stat` as an x86-64 guest sees it - the `_DARWIN_FEATURE_64_BIT_INODE`
/// layout, which is the only one arm64 and modern x86-64 code has.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Stat {
    pub dev: i32,
    pub mode: u16,
    pub nlink: u16,
    pub ino: u64,
    pub uid: u32,
    pub gid: u32,
    pub rdev: i32,
    _pad: i32,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub birthtime: Timespec,
    pub size: i64,
    pub blocks: i64,
    pub blksize: i32,
    pub flags: u32,
    /// `st_gen`, spelled out because `gen` is a reserved word now.
    pub generation: u32,
    pub lspare: i32,
    pub qspare: [i64; 2],
}

impl DarwinTranslation<libc::stat> for Stat {
    fn to_host(&self) -> libc::stat {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        stat.st_dev = self.dev as _;
        stat.st_mode = self.mode.into();
        stat.st_nlink = self.nlink.into();
        stat.st_ino = self.ino;
        stat.st_uid = self.uid;
        stat.st_gid = self.gid;
        stat.st_rdev = self.rdev as _;
        stat.st_atime = self.atime.sec;
        stat.st_atime_nsec = self.atime.nsec;
        stat.st_mtime = self.mtime.sec;
        stat.st_mtime_nsec = self.mtime.nsec;
        stat.st_ctime = self.ctime.sec;
        stat.st_ctime_nsec = self.ctime.nsec;
        stat.st_size = self.size;
        stat.st_blocks = self.blocks;
        stat.st_blksize = self.blksize.into();
        #[cfg(target_os = "macos")]
        {
            stat.st_birthtime = self.birthtime.sec;
            stat.st_birthtime_nsec = self.birthtime.nsec;
            stat.st_flags = self.flags;
            stat.st_gen = self.generation;
            stat.st_lspare = self.lspare;
            stat.st_qspare = self.qspare;
        }
        stat
    }

    fn from_host(host: &libc::stat) -> Self {
        from_host(host)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}

impl DarwinTranslation<libc::timespec> for Timespec {
    fn to_host(&self) -> libc::timespec {
        libc::timespec {
            tv_sec: self.sec,
            tv_nsec: self.nsec,
        }
    }

    fn from_host(host: &libc::timespec) -> Self {
        Self {
            sec: host.tv_sec,
            nsec: host.tv_nsec,
        }
    }
}

const _: () = {
    assert!(size_of::<Stat>() == 144);
    assert!(std::mem::offset_of!(Stat, ino) == 8);
    assert!(std::mem::offset_of!(Stat, rdev) == 24);
    assert!(std::mem::offset_of!(Stat, atime) == 32);
    assert!(std::mem::offset_of!(Stat, size) == 96);
    assert!(std::mem::offset_of!(Stat, blksize) == 112);
};

/// x86-64 Darwin's `struct stat` without `_DARWIN_FEATURE_64_BIT_INODE`: what
/// the unsuffixed `stat`, `lstat` and `fstat` fill there, `$INODE64` naming the
/// [`Stat`] ones. A program built for 10.5 and earlier imports these, and its
/// buffer is 24 bytes short of a [`Stat`] - writing one smashes its stack.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LegacyStat {
    pub dev: i32,
    pub ino: u32,
    pub mode: u16,
    pub nlink: u16,
    pub uid: u32,
    pub gid: u32,
    pub rdev: i32,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub size: i64,
    pub blocks: i64,
    pub blksize: i32,
    pub flags: u32,
    pub generation: u32,
    pub lspare: i32,
    pub qspare: [i64; 2],
}

const _: () = {
    assert!(size_of::<LegacyStat>() == 120);
    assert!(std::mem::offset_of!(LegacyStat, atime) == 24);
    assert!(std::mem::offset_of!(LegacyStat, size) == 72);
    assert!(std::mem::offset_of!(LegacyStat, blksize) == 88);
};

impl From<&Stat> for LegacyStat {
    fn from(stat: &Stat) -> Self {
        LegacyStat {
            dev: stat.dev,
            // Truncated, as the 32-bit `ino_t` of these callers always was.
            ino: stat.ino as u32,
            mode: stat.mode,
            nlink: stat.nlink,
            uid: stat.uid,
            gid: stat.gid,
            rdev: stat.rdev,
            atime: stat.atime,
            mtime: stat.mtime,
            ctime: stat.ctime,
            size: stat.size,
            blocks: stat.blocks,
            blksize: stat.blksize,
            flags: stat.flags,
            generation: stat.generation,
            lspare: stat.lspare,
            qspare: stat.qspare,
        }
    }
}

/// Whether an unsuffixed `stat` family name means [`LegacyStat`]: only an
/// x86-64 guest ever had the 32-bit-inode layout; arm64's plain names are the
/// 64-bit one.
pub(crate) fn unsuffixed_is_legacy() -> bool {
    crate::sysctl::guest_arch() == crate::sysctl::GuestArch::X86_64
}

/// Fill `out` through `fill`, as [`LegacyStat`] when `legacy` and as [`Stat`]
/// otherwise.
///
/// # Safety
///
/// `out` is null or a writable `struct stat` of that layout.
unsafe fn unsuffixed(
    out: *mut c_void,
    legacy: bool,
    fill: impl FnOnce(*mut Stat) -> c_int,
) -> c_int {
    if out.is_null() || !legacy {
        return fill(out.cast());
    }
    let mut full = Stat::default();
    let rc = fill(&mut full);
    if rc == 0 {
        // SAFETY: the caller states `out` is a writable legacy `struct stat`.
        unsafe {
            out.cast::<LegacyStat>()
                .write_unaligned(LegacyStat::from(&full))
        };
    }
    rc
}

/// The unsuffixed `int stat(const char *, struct stat *)`.
///
/// # Safety
///
/// As [`stat`], with `out` laid out as [`unsuffixed_is_legacy`] says.
pub unsafe extern "C" fn stat_unsuffixed(path: *const c_char, out: *mut c_void) -> c_int {
    unsafe { unsuffixed(out, unsuffixed_is_legacy(), |out| stat(path, out)) }
}

/// The unsuffixed `lstat`, as [`stat_unsuffixed`].
///
/// # Safety
///
/// As [`stat_unsuffixed`].
pub unsafe extern "C" fn lstat_unsuffixed(path: *const c_char, out: *mut c_void) -> c_int {
    unsafe { unsuffixed(out, unsuffixed_is_legacy(), |out| lstat(path, out)) }
}

/// The unsuffixed `fstat`, as [`stat_unsuffixed`].
///
/// # Safety
///
/// As [`fstat`], with `out` laid out as [`unsuffixed_is_legacy`] says.
pub unsafe extern "C" fn fstat_unsuffixed(fd: c_int, out: *mut c_void) -> c_int {
    unsafe { unsuffixed(out, unsuffixed_is_legacy(), |out| fstat(fd, out)) }
}

/// Convert what the host filled in.
///
/// `st_birthtimespec` is the creation time, which glibc's `stat` does not carry:
/// it is left zero rather than filled from `st_ctime`, because a guest comparing
/// the two to decide whether a file was replaced would get "yes" every time from
/// a copy of the change time.
pub(crate) fn from_host(host: &libc::stat) -> Stat {
    Stat {
        dev: host.st_dev as i32,
        mode: host.st_mode as u16,
        nlink: host.st_nlink as u16,
        ino: host.st_ino as u64,
        uid: host.st_uid,
        gid: host.st_gid,
        rdev: host.st_rdev as i32,
        _pad: 0,
        atime: Timespec {
            sec: host.st_atime as i64,
            nsec: host.st_atime_nsec as i64,
        },
        mtime: Timespec {
            sec: host.st_mtime as i64,
            nsec: host.st_mtime_nsec as i64,
        },
        ctime: Timespec {
            sec: host.st_ctime as i64,
            nsec: host.st_ctime_nsec as i64,
        },
        #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
        birthtime: Timespec::default(),
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        birthtime: Timespec {
            sec: host.st_birthtime,
            nsec: host.st_birthtime_nsec,
        },
        size: host.st_size as i64,
        blocks: host.st_blocks as i64,
        blksize: host.st_blksize as i32,
        #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
        flags: 0,
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        flags: host.st_flags,
        #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
        generation: 0,
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        generation: host.st_gen as u32,
        lspare: 0,
        qspare: [0; 2],
    }
}

pub(crate) use prefix::ownership::Found;

/// What the guest is told about the file the host described as `host`: Darwin's
/// layout, with the owner and mode the prefix reports for it - root for the
/// base's files, whoever a guest recorded, and set-id bits for its set-id
/// programs. See `prefix::ownership`.
pub(crate) fn as_seen(host: &libc::stat, found: Found<'_>) -> Stat {
    let mut stat = from_host(host);
    if let Some(owner) = prefix::ownership::of(found, host.st_dev as u64, host.st_ino as u64) {
        stat.uid = owner.uid;
        stat.gid = owner.gid;
        if let Some(mode) = owner.mode {
            stat.mode = (stat.mode & !0o7777) | mode as u16;
        }
    }
    stat
}

/// Run `ask` on the host path `path` names, and again on the case-insensitive
/// reading of it if the first attempt says the file is not there.
///
/// The mapping is unconditional and costs no syscall. The fold is tried only on
/// `ENOENT`: a path that exists and cannot be read is a permission problem, and
/// looking for a differently-cased neighbour would answer about a different
/// file.
pub(crate) fn with_fallback<T>(
    path: *const c_char,
    mut ask: impl FnMut(*const c_char) -> (T, bool),
) -> T {
    if path.is_null() {
        return ask(path).0;
    }
    // SAFETY: non-null, and a `const char *` is NUL-terminated by contract.
    let host = prefix::ns::to_host(unsafe { CStr::from_ptr(path) });
    let (result, ok) = ask(host.as_ptr());
    if ok || crate::errno::get() != libc::ENOENT {
        return result;
    }
    let Some(found) = prefix::ns::fold(&host) else {
        return result;
    };
    ask(found.as_ptr()).0
}

/// Whether the guest may write the host path `path`, which is about to be
/// written. A write to a file the prefix's base owns is recorded, or refused
/// with `EROFS` under `system_files: immutable` - see `prefix::system`.
///
/// `false` means refused, with `errno` set for the caller to return.
pub(crate) fn may_write(path: *const c_char) -> bool {
    may_write_at(libc::AT_FDCWD, path)
}

/// [`may_write`] for a path interpreted against an open directory.
pub(crate) fn may_write_at(dirfd: c_int, path: *const c_char) -> bool {
    if path.is_null() {
        return true;
    }
    // SAFETY: non-null, and a `const char *` is NUL-terminated by contract.
    match prefix::ns::may_write_at(dirfd, unsafe { CStr::from_ptr(path) }) {
        Ok(()) => true,
        Err(errno) => {
            crate::errno::set(errno);
            false
        }
    }
}

/// Whether host `open` flags can change the file they open.
fn writes(host_flags: c_int) -> bool {
    host_flags & libc::O_ACCMODE != libc::O_RDONLY
        || host_flags & (libc::O_CREAT | libc::O_TRUNC) != 0
}

/// `int stat(const char *, struct stat *)`
///
/// # Safety
///
/// `path` is a NUL-terminated string and `out` is null or a writable Darwin
/// `struct stat`.
pub unsafe extern "C" fn stat(path: *const c_char, out: *mut Stat) -> c_int {
    unsafe { at_path(path, out, false) }
}

/// `int lstat(const char *, struct stat *)`
///
/// # Safety
///
/// As [`stat`].
pub unsafe extern "C" fn lstat(path: *const c_char, out: *mut Stat) -> c_int {
    unsafe { at_path(path, out, true) }
}

/// # Safety
///
/// As [`stat`].
pub(crate) unsafe fn at_path(path: *const c_char, out: *mut Stat, symlink: bool) -> c_int {
    with_fallback(path, |path| {
        // SAFETY: a zeroed `struct stat` is what the host is asked to fill.
        let mut host: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: the caller's obligation for `path`, and `host` is a live local.
        let rc = unsafe {
            match symlink {
                true => libc::lstat(path, &mut host),
                false => libc::stat(path, &mut host),
            }
        };
        if rc == 0 && !out.is_null() {
            // SAFETY: `path` is the host path just stat'ed, and the caller
            // states `out` is writable for a Darwin `stat`.
            unsafe { *out = as_seen(&host, Found::Path(CStr::from_ptr(path), !symlink)) };
        }
        (rc, rc == 0)
    })
}

/// `int fstat(int, struct stat *)`
///
/// # Safety
///
/// `out` is null or a writable Darwin `struct stat`.
pub unsafe extern "C" fn fstat(fd: c_int, out: *mut Stat) -> c_int {
    // SAFETY: a zeroed `struct stat` is what the host is asked to fill.
    let mut host: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `host` is a live local; `fd` is the caller's.
    let rc = unsafe { libc::fstat(fd, &mut host) };
    if rc == 0 && !out.is_null() {
        // SAFETY: the caller states `out` is writable.
        unsafe { *out = as_seen(&host, Found::Fd(fd)) };
    }
    rc
}

/// `int access(const char *, int)`, with the same fallback.
///
/// Not divergent - the flags agree - but a program that lowercases a path asks
/// this about it before opening it.
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn access(path: *const c_char, mode: c_int) -> c_int {
    with_fallback(path, |path| {
        let rc = unsafe { libc::access(path, mode) };
        (rc, rc == 0)
    })
}

/// Darwin's `PATH_MAX`, which is **not** the host's.
///
/// A guest sizes its `realpath` buffer with its own headers, so 1024 bytes is all
/// the room there is - and Linux's `PATH_MAX` is 4096. Resolving into the caller's
/// buffer directly would let a path longer than a guest expects overrun it, which
/// is why [`realpath`] resolves into its own and copies.
pub(crate) const DARWIN_PATH_MAX: usize = 1024;

/// Resolve a guest path with case fallback and translate the result back to a
/// guest path. Refuse results longer than Darwin's `PATH_MAX`.
///
/// # Safety
///
/// `path` is a NUL-terminated string, and `resolved` is null or writable for
/// `PATH_MAX` bytes as the *guest* counts them.
pub unsafe extern "C" fn realpath(path: *const c_char, resolved: *mut c_char) -> *mut c_char {
    let found = with_fallback(path, |path| {
        // Resolved into this crate's own buffer rather than the caller's: the
        // host may write up to its own `PATH_MAX`, which is larger than the
        // caller's array.
        let mut buffer = [0 as c_char; libc::PATH_MAX as usize];
        // SAFETY: the caller's obligation for `path`; `buffer` is a live local
        // of the host's `PATH_MAX`.
        let got = unsafe { libc::realpath(path, buffer.as_mut_ptr()) };
        match got.is_null() {
            true => (None, false),
            // SAFETY: `realpath` wrote a NUL-terminated path into `buffer`.
            false => (
                Some(unsafe { CStr::from_ptr(buffer.as_ptr()) }.to_owned()),
                true,
            ),
        }
    });
    let Some(found) = found else {
        return std::ptr::null_mut();
    };
    let Some(found) = guest_path(&found) else {
        crate::errno::set(libc::ENOENT);
        return std::ptr::null_mut();
    };
    let bytes = found.as_bytes_with_nul();
    if bytes.len() > DARWIN_PATH_MAX {
        crate::errno::set(libc::ENAMETOOLONG);
        return std::ptr::null_mut();
    }
    // A null `resolved` means "allocate one", and the caller frees it - so it has
    // to come from the allocator the guest's `free` uses.
    let out = match resolved.is_null() {
        // SAFETY: a plain size; the block is the caller's to free.
        true => unsafe { crate::heap::malloc(DARWIN_PATH_MAX) }.cast::<c_char>(),
        false => resolved,
    };
    if out.is_null() {
        crate::errno::set(libc::ENOMEM);
        return std::ptr::null_mut();
    }
    // SAFETY: `out` is the caller's `PATH_MAX` array or a fresh block of that
    // size, and `bytes` has just been checked to fit in it.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), out, bytes.len()) };
    out
}

/// `ssize_t readlink(const char *, char *, size_t)`, with the same fallback.
///
/// Here for the same reason as [`access`]: a program that built a path in the
/// wrong case asks about it, and the host answers `ENOENT` for a link that is
/// there under another spelling.
///
/// # Safety
///
/// `path` is a NUL-terminated string and `buffer` is writable for `size` bytes.
pub unsafe extern "C" fn readlink(
    path: *const c_char,
    buffer: *mut c_char,
    size: libc::size_t,
) -> libc::ssize_t {
    // Read into this crate's own buffer rather than the caller's: an absolute
    // target is a host path, and the guest spelling of it is not necessarily
    // the same length.
    let mut target = [0u8; libc::PATH_MAX as usize];
    let read = with_fallback(path, |path| {
        // SAFETY: the caller's obligation for `path`; `target` is a live local
        // of the host's `PATH_MAX`.
        let n = unsafe { libc::readlink(path, target.as_mut_ptr().cast::<c_char>(), target.len()) };
        (n, n >= 0)
    });
    if read < 0 {
        return read;
    }
    let target = &target[..read as usize];
    // A relative target is resolved against the link's own directory, which is
    // already inside the namespace, so it is its own guest spelling.
    let spelled;
    let target = match target.first() {
        Some(b'/') => {
            let Some(guest) = std::str::from_utf8(target)
                .ok()
                .and_then(|host| prefix::ns::to_guest(std::path::Path::new(host)))
            else {
                crate::errno::set(libc::ENOENT);
                return -1;
            };
            spelled = guest.into_os_string().into_encoded_bytes();
            spelled.as_slice()
        }
        _ => target,
    };
    // Darwin truncates rather than failing, and answers with what it wrote.
    let copied = target.len().min(size);
    // SAFETY: the caller's buffer is writable for `size` bytes and `copied` is
    // no larger; the two do not overlap, `target` being this crate's own.
    unsafe { std::ptr::copy_nonoverlapping(target.as_ptr().cast::<c_char>(), buffer, copied) };
    copied as libc::ssize_t
}

/// Whether creating a file now has to be recorded as made by somebody the host
/// would not name - see `crate::credentials::created` - and, if so, whether
/// the file is there already, in which case nothing is created.
///
/// `None` when there is nothing to record.
fn creation(create: bool, exclusive: bool, dirfd: c_int, path: *const c_char) -> Option<bool> {
    if create && crate::credentials::creates_as_another() {
        if exclusive {
            return Some(false);
        }
        let saved = crate::errno::get();
        // SAFETY: a zeroed `struct stat` for the host to fill, and the
        // caller's NUL-terminated path.
        let mut host: libc::stat = unsafe { std::mem::zeroed() };
        let there =
            unsafe { libc::fstatat(dirfd, path, &mut host, libc::AT_SYMLINK_NOFOLLOW) } == 0;
        crate::errno::set(saved);
        return Some(there);
    }
    let _ = (create, exclusive, dirfd, path);
    None
}

/// Record what [`creation`] said was about to be made, if it was.
fn created(existed: Option<bool>, made: bool, dirfd: c_int, path: *const c_char) {
    if made && existed == Some(false) {
        // SAFETY: the caller's NUL-terminated path.
        crate::credentials::created(dirfd, unsafe { CStr::from_ptr(path) });
    }
    let _ = (existed, made, dirfd, path);
}

/// The guest spelling of a host path this crate is about to hand back.
///
/// `None` for one no mount reaches, which the caller reports as `ENOENT`.
pub(crate) fn guest_path(host: &CStr) -> Option<std::ffi::CString> {
    let host = host.to_str().ok()?;
    let guest = prefix::ns::to_guest(std::path::Path::new(host))?;
    std::ffi::CString::new(guest.into_os_string().into_encoded_bytes()).ok()
}

/// `int open(const char *, int, ...)`, with the same fallback.
///
/// This entry has a fixed third argument so it can also be called by an ABI
/// bridge. Only `O_CREAT` makes that argument meaningful; without it the guest
/// may have left the corresponding register untouched.
///
/// # Safety
///
/// `path` is a NUL-terminated string.
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: u32) -> c_int {
    if flags & beneath::O_SUPPORTED != 0 {
        // SAFETY: the caller's obligations, unchanged.
        return unsafe { openat(DARWIN_AT_FDCWD, path, flags, mode) };
    }
    let Some(host_flags) = open_flags(flags) else {
        return -1;
    };
    let mode = open_mode(flags, mode);
    with_fallback(path, |path| {
        if writes(host_flags) && !may_write(path) {
            return (-1, false);
        }
        let existed = creation(
            host_flags & libc::O_CREAT != 0,
            host_flags & libc::O_EXCL != 0,
            libc::AT_FDCWD,
            path,
        );
        let fd = unsafe { libc::open(path, host_flags, mode) };
        created(existed, fd >= 0, libc::AT_FDCWD, path);
        (fd, fd >= 0)
    })
}

/// `int openat(int, const char *, int, ...)`, with Darwin's flag word.
///
/// The case-insensitive fallback remains relative to `dirfd`, including when it
/// is a real directory descriptor rather than `AT_FDCWD`.
///
/// # Safety
///
/// `path` is a NUL-terminated string and `dirfd` is either a directory
/// descriptor or Darwin's `AT_FDCWD`.
pub unsafe extern "C" fn openat(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mode: u32,
) -> c_int {
    let resolve_beneath = flags & beneath::O_SUPPORTED != 0;
    let Some(host_flags) = open_flags(flags & !beneath::O_SUPPORTED) else {
        return -1;
    };
    let dirfd = match dirfd {
        DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    let mode = open_mode(flags, mode);
    with_fallback_at(dirfd, path, |path| {
        if writes(host_flags) && !may_write_at(dirfd, path) {
            return (-1, false);
        }
        let existed = creation(
            host_flags & libc::O_CREAT != 0,
            host_flags & libc::O_EXCL != 0,
            dirfd,
            path,
        );
        let fd = match resolve_beneath {
            // SAFETY: the caller's descriptor, and a translated path.
            true => unsafe { beneath::open(dirfd, path, host_flags, mode) },
            false => unsafe { libc::openat(dirfd, path, host_flags, mode) },
        };
        created(existed, fd >= 0, dirfd, path);
        (fd, fd >= 0)
    })
}

/// [`with_fallback`] for a path interpreted against `dirfd`, a host descriptor
/// or the host's `AT_FDCWD`.
///
/// The fold stays relative to `dirfd`: a relative path under a real directory
/// descriptor is already a host path, and mapping it again would apply the
/// prefix twice.
pub(crate) fn with_fallback_at<T>(
    dirfd: c_int,
    path: *const c_char,
    mut ask: impl FnMut(*const c_char) -> (T, bool),
) -> T {
    if path.is_null() {
        return ask(path).0;
    }
    // SAFETY: non-null, and a `const char *` is NUL-terminated by contract.
    let host = prefix::ns::to_host_at(dirfd, unsafe { CStr::from_ptr(path) });
    let (result, ok) = ask(host.as_ptr());
    if ok || crate::errno::get() != libc::ENOENT {
        return result;
    }
    let Some(found) = prefix::ns::fold_at(dirfd, &host) else {
        return result;
    };
    ask(found.as_ptr()).0
}

/// Darwin spells `AT_FDCWD` as `-2`; Linux spells it as `-100`.
pub(crate) const DARWIN_AT_FDCWD: c_int = -2;

/// Darwin's `AT_EACCESS` and `AT_SYMLINK_NOFOLLOW`, which are Linux's `0x200`
/// and `0x100`.
const DARWIN_AT_EACCESS: c_int = 0x10;
const DARWIN_AT_SYMLINK_NOFOLLOW: c_int = 0x20;

/// `int faccessat(int fd, const char *path, int mode, int flag)`
///
/// Passed through, Darwin's `AT_FDCWD` is a descriptor number the host has
/// never heard of and every relative lookup fails with `EBADF`. The access
/// modes agree; the directory, the flags and the path are translated.
///
/// # Safety
///
/// `path` is a NUL-terminated string and `dirfd` is either a directory
/// descriptor or Darwin's `AT_FDCWD`.
pub unsafe extern "C" fn faccessat(
    dirfd: c_int,
    path: *const c_char,
    mode: c_int,
    flag: c_int,
) -> c_int {
    let known = DARWIN_AT_EACCESS | DARWIN_AT_SYMLINK_NOFOLLOW;
    if flag & !known != 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let mut host_flag = 0;
    if flag & DARWIN_AT_EACCESS != 0 {
        host_flag |= libc::AT_EACCESS;
    }
    if flag & DARWIN_AT_SYMLINK_NOFOLLOW != 0 {
        host_flag |= libc::AT_SYMLINK_NOFOLLOW;
    }
    let dirfd = match dirfd {
        DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    with_fallback_at(dirfd, path, |path| {
        let rc = unsafe { libc::faccessat(dirfd, path, mode, host_flag) };
        (rc, rc == 0)
    })
}

/// `int fstatat(int fd, const char *path, struct stat *buf, int flag)`
///
/// Only `AT_SYMLINK_NOFOLLOW` has a host equivalent. `AT_REALDEV`,
/// `AT_FDONLY` and `AT_SYMLINK_NOFOLLOW_ANY` change what is looked up or what
/// `st_dev` means, so they are refused with `EINVAL` rather than dropped.
///
/// # Safety
///
/// `path` is a NUL-terminated string, `dirfd` is either a directory descriptor
/// or Darwin's `AT_FDCWD`, and `out` is null or a writable Darwin `struct stat`.
pub unsafe extern "C" fn fstatat(
    dirfd: c_int,
    path: *const c_char,
    out: *mut Stat,
    flag: c_int,
) -> c_int {
    let known = DARWIN_AT_SYMLINK_NOFOLLOW | beneath::SUPPORTED;
    if flag & !known != 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let nofollow = flag & DARWIN_AT_SYMLINK_NOFOLLOW != 0;
    let host_flag = match nofollow {
        true => libc::AT_SYMLINK_NOFOLLOW,
        false => 0,
    };
    let dirfd = match dirfd {
        DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    if flag & beneath::SUPPORTED != 0 {
        return with_fallback_at(dirfd, path, |path| {
            // SAFETY: the caller's path and descriptor; `out` as for this function.
            let rc = unsafe { beneath::fstatat(dirfd, path, out, nofollow) };
            (rc, rc == 0)
        });
    }
    with_fallback_at(dirfd, path, |path| {
        let mut host = MaybeUninit::<libc::stat>::uninit();
        let rc = unsafe { libc::fstatat(dirfd, path, host.as_mut_ptr(), host_flag) };
        if rc == 0 && !out.is_null() {
            // SAFETY: filled by the host, and `path` is the one it was given.
            unsafe {
                *out = as_seen(
                    &host.assume_init(),
                    Found::At(dirfd, CStr::from_ptr(path), !nofollow),
                )
            };
        }
        (rc, rc == 0)
    })
}

/// Darwin's `AT_RESOLVE_BENEATH`, which fails a lookup that would leave the
/// directory it starts from, by `..`, an absolute path or a symlink. Safe
/// archive extraction (`unzip`) relies on it, so it is kept rather than dropped.
mod beneath {
    use super::Stat;
    #[cfg(target_os = "linux")]
    use super::{Found, as_seen};
    #[cfg(target_os = "linux")]
    use std::ffi::CStr;
    use std::ffi::{c_char, c_int};

    /// The `AT_` flag where the host can honour it, and no bits elsewhere, so
    /// the caller's check refuses it by `EINVAL` there.
    #[cfg(target_os = "linux")]
    pub const SUPPORTED: c_int = 0x2000;
    #[cfg(not(target_os = "linux"))]
    pub const SUPPORTED: c_int = 0;

    /// `O_RESOLVE_BENEATH`, likewise: elsewhere it stays an unknown bit that
    /// `open` refuses by name.
    #[cfg(target_os = "linux")]
    pub const O_SUPPORTED: c_int = 0x1000;
    #[cfg(not(target_os = "linux"))]
    pub const O_SUPPORTED: c_int = 0;

    /// `openat2` beneath `dirfd` with host `flags`.
    ///
    /// # Safety
    ///
    /// `path` is a NUL-terminated host path.
    #[cfg(target_os = "linux")]
    pub unsafe fn open(dirfd: c_int, path: *const c_char, flags: c_int, mode: c_int) -> c_int {
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = flags as u64;
        if flags & (libc::O_CREAT | libc::O_TMPFILE) != 0 {
            how.mode = mode as u64;
        }
        how.resolve = libc::RESOLVE_BENEATH;
        // SAFETY: a live `open_how` of the size passed, and the caller's path.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dirfd,
                path,
                &how,
                size_of::<libc::open_how>(),
            )
        };
        fd as c_int
    }

    #[cfg(not(target_os = "linux"))]
    pub unsafe fn open(_: c_int, _: *const c_char, _: c_int, _: c_int) -> c_int {
        unreachable!("O_SUPPORTED is zero here, so nothing asks for it")
    }

    /// `fstatat` beneath `dirfd`: `openat2` with `RESOLVE_BENEATH` to an
    /// `O_PATH` descriptor, then that descriptor's own `stat`.
    ///
    /// FIXME (ACCURACY): an escape fails with this host's `EXDEV`, which the
    /// guest sees as `EXDEV`; Darwin answers `ENOTCAPABLE`, which has no host
    /// counterpart in `crate::errno`.
    ///
    /// # Safety
    ///
    /// `path` is a NUL-terminated host path and `out` is null or a writable
    /// Darwin `struct stat`.
    #[cfg(target_os = "linux")]
    pub unsafe fn fstatat(
        dirfd: c_int,
        path: *const c_char,
        out: *mut Stat,
        nofollow: bool,
    ) -> c_int {
        let flags = libc::O_PATH | libc::O_CLOEXEC | if nofollow { libc::O_NOFOLLOW } else { 0 };
        // SAFETY: the caller's path.
        let fd = unsafe { open(dirfd, path, flags, 0) };
        if fd < 0 {
            return -1;
        }
        let mut host: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: an `O_PATH` descriptor this call owns; an empty path asks
        // about the descriptor itself, which is the symlink under `O_NOFOLLOW`.
        let rc = unsafe { libc::fstatat(fd, c"".as_ptr(), &mut host, libc::AT_EMPTY_PATH) };
        if rc == 0 && !out.is_null() {
            // SAFETY: the caller states `out` is writable, and `path` was
            // found beneath `dirfd`.
            unsafe { *out = as_seen(&host, Found::At(dirfd, CStr::from_ptr(path), !nofollow)) };
        }
        let saved = crate::errno::get();
        // SAFETY: closed exactly once.
        unsafe { libc::close(fd) };
        crate::errno::set(saved);
        rc
    }

    #[cfg(not(target_os = "linux"))]
    pub unsafe fn fstatat(_: c_int, _: *const c_char, _: *mut Stat, _: bool) -> c_int {
        unreachable!("SUPPORTED is zero here, so nothing asks for it")
    }
}

/// Translate a Darwin `O_` word, refusing bits whose semantics cannot be kept.
fn open_flags(flags: c_int) -> Option<c_int> {
    match crate::fcntl::flags_to_host(flags) {
        Ok(host) => Some(host),
        Err(unknown) => {
            say_unknown_open_flags(unknown);
            crate::errno::set(libc::EINVAL);
            None
        }
    }
}

/// Narrow a real mode and discard the stale register when there is no mode.
fn open_mode(flags: c_int, mode: u32) -> c_int {
    match crate::fcntl::creates(flags) {
        true => mode as libc::mode_t as c_int,
        false => 0,
    }
}

/// Name an unsupported flag set once; `EINVAL` alone blames the guest.
fn say_unknown_open_flags(unknown: c_int) {
    use std::sync::Mutex;

    static SAID: Mutex<Vec<c_int>> = Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.contains(&unknown) {
        return;
    }
    said.push(unknown);
    eprintln!("rosemary: open: no equivalent on this host for Darwin O_ flags {unknown:#x}");
}

/// Every name this module answers, in both spellings a Mach-O may import them
/// by - see [`LegacyStat`] on why `$INODE64` is not the same name.
/// Darwin's `struct statfs`, which is 2168 bytes to this host's 120.
///
/// The two do not merely differ in layout, they differ in *kind*: Darwin
/// carries the mount point and the device inside the structure, as two
/// 1024-byte paths, where Linux answers only numbers and expects a caller who
/// wants the path to have asked `/proc/mounts`. So the size difference is the
/// dangerous part - the host's `statfs` writes 120 bytes into a buffer the
/// guest sized at 2168 and leaves the rest as whatever was on its stack, and a
/// caller reading `f_mntonname` gets that.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Statfs {
    pub bsize: u32,
    pub iosize: i32,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub fsid: [i32; 2],
    pub owner: u32,
    pub fstype: u32,
    pub flags: u32,
    pub fssubtype: u32,
    pub fstypename: [u8; 16],
    pub mntonname: [u8; 1024],
    pub mntfromname: [u8; 1024],
    pub flags_ext: u32,
    pub reserved: [u32; 7],
}

/// Fill a Darwin `struct statfs` from this host's.
///
/// The counts are the whole answer and they carry across exactly: both
/// platforms report blocks of `f_bsize` bytes. What does **not** carry across
/// is everything textual - `f_fstypename`, `f_mntonname` and `f_mntfromname`
/// are left empty rather than filled with a plausible `"apfs"` and `"/"`,
/// because a caller that reads them is one deciding something about the volume
/// and an invented mount point is a wrong answer where an empty one is a
/// visible gap. Every caller seen so far wants `f_bsize * f_bavail`.
///
/// # Safety
///
/// `out` is null or a writable Darwin `struct statfs`.
/// A count this host reports signed, as Darwin's unsigned one.
///
/// FreeBSD's `f_bavail` and `f_ffree` are signed, because the root reserve can
/// push them below zero; Darwin has no way to say that, so it says none left.
#[cfg(target_os = "freebsd")]
fn unsigned(count: i64) -> u64 {
    count.max(0) as u64
}

#[cfg(not(target_os = "freebsd"))]
fn unsigned(count: u64) -> u64 {
    count
}

unsafe fn fill_statfs(host: &libc::statfs, out: *mut Statfs) {
    if out.is_null() {
        return;
    }
    let value = Statfs {
        bsize: host.f_bsize as u32,
        iosize: host.f_bsize as i32,
        blocks: host.f_blocks,
        bfree: host.f_bfree,
        bavail: unsigned(host.f_bavail),
        files: host.f_files,
        ffree: unsigned(host.f_ffree),
        // Two 32-bit halves on both sides, and opaque on both: it is only ever
        // compared against another `f_fsid`.
        fsid: unsafe { std::mem::transmute::<libc::fsid_t, [i32; 2]>(host.f_fsid) },
        owner: 0,
        fstype: 0,
        // This host's `struct statfs` carries no mount-flag word at all - the
        // read-only bit is `statvfs`'s `f_flag`, a different call - so nothing
        // is claimed rather than `MNT_RDONLY` being guessed at.
        flags: 0,
        fssubtype: 0,
        fstypename: [0; 16],
        mntonname: [0; 1024],
        mntfromname: [0; 1024],
        flags_ext: 0,
        reserved: [0; 7],
    };
    // SAFETY: the caller's obligation; `out` is non-null and sized by the guest
    // at exactly this structure.
    unsafe { out.write(value) };
}

/// `int statfs(const char *, struct statfs *)`
///
/// # Safety
///
/// `path` is NUL-terminated and `out` is null or a writable Darwin
/// `struct statfs`.
pub unsafe extern "C" fn statfs(path: *const c_char, out: *mut Statfs) -> c_int {
    with_fallback(path, |path| {
        let mut host: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: the caller's obligation, and `host` is this host's own layout.
        let result = unsafe { libc::statfs(path, &mut host) };
        if result == 0 {
            unsafe { fill_statfs(&host, out) };
        }
        (result, result == 0)
    })
}

/// `int fstatfs(int, struct statfs *)`
///
/// # Safety
///
/// As [`statfs`].
pub unsafe extern "C" fn fstatfs(fd: c_int, out: *mut Statfs) -> c_int {
    let mut host: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `host` is this host's own layout and `fd` is the caller's.
    let result = unsafe { libc::fstatfs(fd, &mut host) };
    if result == 0 {
        unsafe { fill_statfs(&host, out) };
    }
    result
}

/// `int getmntinfo(struct statfs **, int)`.
///
/// The prefix is one host filesystem view for now. Keep the result in static
/// storage because Darwin returns a pointer owned by the C library.
///
/// # Safety
///
/// `out` is a writable pointer slot or null.
pub unsafe extern "C" fn getmntinfo(out: *mut *mut Statfs, _flags: c_int) -> c_int {
    if out.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: the caller provided a writable result slot and `root_mount` is static.
    unsafe { out.write(root_mount() as *const Statfs as *mut Statfs) };
    1
}

/// The one mount this runtime reports, as [`getmntinfo`] and [`getfsstat`] both
/// answer it.
fn root_mount() -> &'static Statfs {
    static ROOT: std::sync::OnceLock<Statfs> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let path = c"/";
        let mut value = Statfs {
            bsize: 0,
            iosize: 0,
            blocks: 0,
            bfree: 0,
            bavail: 0,
            files: 0,
            ffree: 0,
            fsid: [0; 2],
            owner: 0,
            fstype: 0,
            flags: 0,
            fssubtype: 0,
            fstypename: [0; 16],
            mntonname: [0; 1024],
            mntfromname: [0; 1024],
            flags_ext: 0,
            reserved: [0; 7],
        };
        let mut host: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(path.as_ptr(), &mut host) } == 0 {
            unsafe { fill_statfs(&host, &mut value) };
        }
        value.mntonname[0] = b'/';
        value.fstypename[..5].copy_from_slice(b"other");
        value
    })
}

/// `int getfsstat(struct statfs *, int, int)`.
///
/// The same single mount [`getmntinfo`] reports. A null buffer asks only how many
/// there are; a buffer too small for one entry gets none, as Darwin answers.
///
/// # Safety
///
/// `buffer` is null or writable for `size` bytes.
pub unsafe extern "C" fn getfsstat(buffer: *mut Statfs, size: c_int, _flags: c_int) -> c_int {
    if buffer.is_null() {
        return 1;
    }
    if (size.max(0) as usize) < size_of::<Statfs>() {
        return 0;
    }
    // SAFETY: the buffer holds at least one entry, by the check above.
    unsafe { buffer.write(*root_mount()) };
    1
}

/// `char *devname(dev_t, mode_t)`.
///
/// There is no host-wide device database to translate Darwin's device number
/// into. The empty name is the C library's missing-device spelling and keeps
/// callers from dereferencing an unresolved import.
pub extern "C" fn devname(_device: i32, _mode: i32) -> *const c_char {
    c"".as_ptr()
}

exports::exports! {
    functions {
        "stat" => stat_unsuffixed,
        "stat$INODE64" => stat,
        "lstat" => lstat_unsuffixed,
        "lstat$INODE64" => lstat,
        "fstat" => fstat_unsuffixed,
        "fstat$INODE64" => fstat,
        "fstatat" | "fstatat$INODE64" => fstatat,
        "statfs" | "statfs$INODE64" => statfs,
        "fstatfs" | "fstatfs$INODE64" => fstatfs,
        "getmntinfo" | "getmntinfo$INODE64" => getmntinfo,
        "getfsstat" | "getfsstat$INODE64" => getfsstat,
        "devname" => devname,
        "access" => access,
        "faccessat" => faccessat,
        "realpath" => realpath,
        "readlink" => readlink,
        "open" => open,
        "openat" => openat,
    }
}

/// The name a `$`-suffixed spelling is a variant of.
///
/// Unused outside the tests, and here so the list above cannot drift from the
/// match below.
#[cfg(test)]
fn base(name: &str) -> &str {
    name.split('$').next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::fs::PermissionsExt;

    /// Except the three whose plain names are the legacy layout on x86-64.
    #[test]
    fn every_listed_name_resolves_and_a_variant_is_its_base() {
        for name in names() {
            assert!(symbol(name).is_some(), "{name}");
            match base(name) {
                "stat" | "lstat" | "fstat" => {}
                base => assert_eq!(symbol(name), symbol(base), "{name}"),
            }
        }
    }

    /// A legacy caller's buffer is 120 bytes, and what follows it is its own.
    #[test]
    fn a_legacy_stat_stays_inside_its_buffer() {
        let at = std::env::temp_dir().join(format!("rosemary-ostat-{}", std::process::id()));
        std::fs::write(&at, vec![0u8; 4321]).expect("a file");
        let file = std::fs::File::open(&at).expect("open");
        let _ = std::fs::remove_file(&at);
        let mut buffer = [0xa5u8; 144];
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
        // SAFETY: an open descriptor and a buffer larger than a `LegacyStat`.
        let rc = unsafe { unsuffixed(buffer.as_mut_ptr().cast(), true, |out| fstat(fd, out)) };
        assert_eq!(rc, 0);
        assert!(
            buffer[120..].iter().all(|b| *b == 0xa5),
            "wrote past 120 bytes"
        );
        let size = i64::from_le_bytes(buffer[72..80].try_into().expect("eight bytes"));
        assert_eq!(size, 4321);
    }

    /// The layout is the guest's, and the size a guest reads is the file's.
    #[test]
    fn a_size_survives_the_conversion() {
        let at = std::env::temp_dir().join(format!("rosemary-stat-{}", std::process::id()));
        std::fs::write(&at, vec![0u8; 1234]).expect("a file");
        let path = CString::new(at.to_str().expect("utf-8")).expect("no NUL");
        let mut out = Stat::default();
        // SAFETY: a NUL-terminated path and a writable Darwin `stat`.
        let rc = unsafe { stat(path.as_ptr(), &mut out) };
        let _ = std::fs::remove_file(&at);
        assert_eq!(rc, 0);
        assert_eq!(out.size, 1234);
        assert_ne!(out.mode & 0o170000, 0, "a file type");
    }

    /// And the case-insensitive fallback reaches through it.
    #[test]
    fn a_lowercased_path_still_stats() {
        let dir = std::env::temp_dir().join(format!("rosemary-stat-ci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Contents")).expect("a directory");
        std::fs::write(dir.join("Contents").join("Game.ios"), b"xyz").expect("a file");
        let lowered = CString::new(
            dir.join("contents")
                .join("game.ios")
                .to_str()
                .expect("utf-8"),
        )
        .expect("no NUL");
        let mut out = Stat::default();
        let rc = unsafe { stat(lowered.as_ptr(), &mut out) };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rc, 0, "the lowercased path was not found");
        assert_eq!(out.size, 3);
    }

    #[test]
    fn open_translates_darwin_status_flags() {
        let at = std::env::temp_dir().join(format!("rosemary-open-flags-{}", std::process::id()));
        std::fs::write(&at, b"x").expect("a file");
        let path = CString::new(at.to_str().expect("utf-8")).expect("no NUL");

        // SAFETY: a NUL-terminated path and Darwin scalar arguments.
        let fd = unsafe {
            open(
                path.as_ptr(),
                crate::fcntl::oflag::O_RDONLY | crate::fcntl::oflag::O_NONBLOCK,
                0,
            )
        };
        assert!(fd >= 0, "open failed: {}", std::io::Error::last_os_error());
        // SAFETY: a descriptor this test just opened; F_GETFL has no third argument.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        // SAFETY: a descriptor this test just opened.
        unsafe { libc::close(fd) };
        let _ = std::fs::remove_file(&at);

        assert_ne!(flags & libc::O_NONBLOCK, 0);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn open_refuses_a_flag_the_host_cannot_honor() {
        // Darwin's O_SHLOCK has no Linux equivalent. Silently dropping it
        // would claim a lock that was never acquired.
        const O_SHLOCK: c_int = 0x0010;
        let path = c"/dev/null";
        crate::errno::set(0);
        // SAFETY: a static NUL-terminated path and Darwin scalar arguments.
        let fd = unsafe { open(path.as_ptr(), crate::fcntl::oflag::O_RDONLY | O_SHLOCK, 0) };
        assert_eq!(fd, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        crate::errno::set(0);
    }

    #[test]
    fn openat_translates_at_fdcwd_flags_and_mode() {
        let at = std::env::temp_dir().join(format!("rosemary-openat-cwd-{}", std::process::id()));
        let _ = std::fs::remove_file(&at);
        let path = CString::new(at.to_str().expect("utf-8")).expect("no NUL");
        let guest_flags = crate::fcntl::oflag::O_WRONLY
            | crate::fcntl::oflag::O_CREAT
            | crate::fcntl::oflag::O_EXCL;

        // SAFETY: a NUL-terminated path, Darwin's AT_FDCWD and scalar arguments.
        let fd = unsafe { openat(DARWIN_AT_FDCWD, path.as_ptr(), guest_flags, 0o604) };
        assert!(
            fd >= 0,
            "openat(AT_FDCWD) failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: a descriptor this test just opened.
        unsafe { libc::close(fd) };

        let permissions = std::fs::metadata(&at)
            .expect("the created file")
            .permissions()
            .mode()
            & 0o777;
        let _ = std::fs::remove_file(&at);
        assert_eq!(permissions & 0o604, 0o604 & !umask());
    }

    #[test]
    fn openat_resolves_a_relative_path_against_its_descriptor() {
        let dir = std::env::temp_dir().join(format!("rosemary-openat-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory");
        let directory = CString::new(dir.to_str().expect("utf-8")).expect("no NUL");
        // SAFETY: a NUL-terminated directory path and Darwin scalar arguments.
        let dirfd = unsafe { open(directory.as_ptr(), crate::fcntl::oflag::O_RDONLY, 0) };
        assert!(dirfd >= 0);

        let name = c"relative";
        let guest_flags = crate::fcntl::oflag::O_WRONLY
            | crate::fcntl::oflag::O_CREAT
            | crate::fcntl::oflag::O_EXCL;
        // SAFETY: a live directory descriptor, a NUL-terminated relative path
        // and Darwin scalar arguments.
        let fd = unsafe { openat(dirfd, name.as_ptr(), guest_flags, 0o600) };
        assert!(
            fd >= 0,
            "openat failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: two descriptors this test just opened.
        unsafe {
            libc::close(fd);
            libc::close(dirfd);
        }

        assert!(dir.join("relative").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn openat_case_folds_relative_to_its_descriptor() {
        let dir = std::env::temp_dir().join(format!("rosemary-openat-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Contents")).expect("a directory");
        std::fs::write(dir.join("Contents").join("Game.ios"), b"x").expect("a file");
        let directory = CString::new(dir.to_str().expect("utf-8")).expect("no NUL");
        // SAFETY: a NUL-terminated directory path and Darwin scalar arguments.
        let dirfd = unsafe { open(directory.as_ptr(), crate::fcntl::oflag::O_RDONLY, 0) };
        assert!(dirfd >= 0);

        // SAFETY: a live directory descriptor, a NUL-terminated relative path
        // and Darwin scalar arguments.
        let fd = unsafe {
            openat(
                dirfd,
                c"contents/game.ios".as_ptr(),
                crate::fcntl::oflag::O_RDONLY,
                0,
            )
        };
        assert!(
            fd >= 0,
            "openat failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: two descriptors this test just opened.
        unsafe {
            libc::close(fd);
            libc::close(dirfd);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fstatat_translates_at_fdcwd() {
        let at = std::env::temp_dir().join(format!("rosemary-fstatat-cwd-{}", std::process::id()));
        std::fs::write(&at, vec![0u8; 321]).expect("a file");
        let path = CString::new(at.to_str().expect("utf-8")).expect("no NUL");
        let mut out = Stat::default();
        // SAFETY: a NUL-terminated path, Darwin's AT_FDCWD and a writable `stat`.
        let rc = unsafe { fstatat(DARWIN_AT_FDCWD, path.as_ptr(), &mut out, 0) };
        let _ = std::fs::remove_file(&at);
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        assert_eq!(out.size, 321);
    }

    #[test]
    fn fstatat_resolves_relative_to_its_descriptor_and_honours_nofollow() {
        let dir = std::env::temp_dir().join(format!("rosemary-fstatat-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Contents")).expect("a directory");
        std::fs::write(dir.join("Contents").join("Game.ios"), b"xyz").expect("a file");
        std::os::unix::fs::symlink("Contents/Game.ios", dir.join("link")).expect("a symlink");
        let directory = CString::new(dir.to_str().expect("utf-8")).expect("no NUL");
        // SAFETY: a NUL-terminated directory path and Darwin scalar arguments.
        let dirfd = unsafe { open(directory.as_ptr(), crate::fcntl::oflag::O_RDONLY, 0) };
        assert!(dirfd >= 0);

        let stat_at = |name: &CStr, flag| {
            let mut out = Stat::default();
            // SAFETY: a live directory descriptor, a NUL-terminated relative
            // path and a writable `stat`.
            let rc = unsafe { fstatat(dirfd, name.as_ptr(), &mut out, flag) };
            (rc, out)
        };
        let (rc, followed) = stat_at(c"link", 0);
        assert_eq!(rc, 0);
        assert_eq!(followed.mode as u32 & 0o170000, libc::S_IFREG.into());
        assert_eq!(followed.size, 3);
        let (rc, link) = stat_at(c"link", DARWIN_AT_SYMLINK_NOFOLLOW);
        assert_eq!(rc, 0);
        assert_eq!(link.mode as u32 & 0o170000, libc::S_IFLNK.into());
        let (rc, folded) = stat_at(c"contents/game.ios", 0);
        assert_eq!(rc, 0, "the case fold stays relative to the descriptor");
        assert_eq!(folded.size, 3);

        // SAFETY: a descriptor this test just opened.
        unsafe { libc::close(dirfd) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `unzip` looks up and opens every path it extracts beneath its target
    /// directory this way, and refuses to extract when the lookup fails.
    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_beneath_finds_what_is_inside_and_refuses_what_is_not() {
        const AT_RESOLVE_BENEATH: c_int = 0x2000;
        const O_RESOLVE_BENEATH: c_int = 0x1000;
        let dir = std::env::temp_dir().join(format!("rosemary-beneath-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inside")).expect("a directory");
        std::fs::write(dir.join("inside").join("file"), b"ab").expect("a file");
        let directory = CString::new(dir.join("inside").to_str().expect("utf-8")).expect("no NUL");
        // SAFETY: a NUL-terminated directory path and Darwin scalar arguments.
        let dirfd = unsafe { open(directory.as_ptr(), crate::fcntl::oflag::O_RDONLY, 0) };
        assert!(dirfd >= 0);

        let mut out = Stat::default();
        let flags = AT_RESOLVE_BENEATH | DARWIN_AT_SYMLINK_NOFOLLOW;
        // SAFETY: a live directory descriptor, NUL-terminated relative paths
        // and a writable `stat`.
        unsafe {
            assert_eq!(fstatat(dirfd, c"file".as_ptr(), &mut out, flags), 0);
            assert_eq!(out.size, 2);
            assert_eq!(
                fstatat(dirfd, c"../inside/file".as_ptr(), &mut out, flags),
                -1
            );
            let fd = openat(dirfd, c"file".as_ptr(), O_RESOLVE_BENEATH, 0);
            assert!(fd >= 0, "a file beneath the descriptor opens");
            libc::close(fd);
            assert_eq!(
                openat(dirfd, c"../inside/file".as_ptr(), O_RESOLVE_BENEATH, 0),
                -1
            );
            libc::close(dirfd);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fstatat_refuses_flags_without_a_host_equivalent() {
        const DARWIN_AT_REALDEV: c_int = 0x200;
        let mut out = Stat::default();
        crate::errno::set(0);
        // SAFETY: a static NUL-terminated path, Darwin's AT_FDCWD and a
        // writable `stat`.
        let rc = unsafe { fstatat(DARWIN_AT_FDCWD, c"/".as_ptr(), &mut out, DARWIN_AT_REALDEV) };
        assert_eq!(rc, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        crate::errno::set(0);
    }

    /// The process umask, restored immediately after reading it.
    fn umask() -> u32 {
        // SAFETY: `umask` cannot fail and the second call restores the first.
        unsafe {
            let previous = libc::umask(0o777);
            libc::umask(previous);
            previous as u32
        }
    }
}
