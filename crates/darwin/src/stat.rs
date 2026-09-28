#![cfg(not(target_os = "macos"))]

use std::ffi::{CStr, c_char, c_int};

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

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}

const _: () = {
    assert!(size_of::<Stat>() == 144);
    // The two fields this host's own `struct stat` places somewhere else
    // entirely, so a fill that reached the guest's buffer unconverted would
    // pass every size check this block makes.
    assert!(std::mem::offset_of!(Stat, mode) == 4);
    assert!(std::mem::offset_of!(Stat, nlink) == 6);
    assert!(std::mem::offset_of!(Stat, ino) == 8);
    assert!(std::mem::offset_of!(Stat, rdev) == 24);
    assert!(std::mem::offset_of!(Stat, atime) == 32);
    assert!(std::mem::offset_of!(Stat, size) == 96);
    assert!(std::mem::offset_of!(Stat, blksize) == 112);
};

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
        birthtime: Timespec::default(),
        size: host.st_size as i64,
        blocks: host.st_blocks as i64,
        blksize: host.st_blksize as i32,
        flags: 0,
        generation: 0,
        lspare: 0,
        qspare: [0; 2],
    }
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

/// Whether an `fopen` mode string can change the file it opens.
///
/// # Safety
///
/// `mode` is null or a NUL-terminated string.
pub(crate) unsafe fn stdio_writes(mode: *const c_char) -> bool {
    if mode.is_null() {
        return false;
    }
    let mode = unsafe { CStr::from_ptr(mode) }.to_bytes();
    mode.iter().any(|b| matches!(b, b'w' | b'a' | b'+'))
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
            // SAFETY: the caller states `out` is writable for a Darwin `stat`.
            unsafe { *out = from_host(&host) };
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
        unsafe { *out = from_host(&host) };
    }
    rc
}

/// Darwin's `AT_SYMLINK_FOLLOW` and `AT_EMPTY_PATH`.
///
/// `AT_EMPTY_PATH` is the one Darwin flag whose number carries across intact.
/// `AT_SYMLINK_FOLLOW` is not, and the reason is worth having written down:
/// this host defines a bit of that name too, at `0x400`, and the call it would
/// be passed to answers `EINVAL` to it - a flag the host knows by name and
/// refuses by value. What the guest asks for is what the host does anyway, so
/// it becomes no bit at all; see [`fstatat_flags`].
const DARWIN_AT_SYMLINK_FOLLOW: c_int = 0x40;
const DARWIN_AT_EMPTY_PATH: c_int = 0x1000;

/// `int fstatat(int dirfd, const char *path, struct stat *buf, int flags)`
///
/// Darwin's spelling of glibc's `fstatat`, and the name a guest's file I/O is
/// linked against - Chromium's included, which is why this is here rather than
/// left to the host's own.
///
/// Three things do not survive being passed through, and only the first is
/// obvious. The structure is [`Stat`] and not the host's, which [`from_host`]
/// converts field by field; the two are the *same* 144 bytes, so a copy that
/// skipped the conversion would pass every size check there is and answer with
/// the mode where the guest reads its link count. The flags are Darwin's words
/// - see [`fstatat_flags`]. And `AT_FDCWD` is `-2` here against `-100` there,
/// so a `dirfd` passed through unchanged makes every relative lookup fail with
/// `EBADF`, an error a guest cannot tell from a descriptor of its own that it
/// has already closed.
///
/// # Safety
///
/// `path` is a NUL-terminated string, or null with Darwin's `AT_EMPTY_PATH`;
/// `out` is null or a writable Darwin `struct stat`; and `dirfd` is either a
/// directory descriptor or Darwin's `AT_FDCWD`.
pub unsafe extern "C" fn fstatat(
    dirfd: c_int,
    path: *const c_char,
    out: *mut Stat,
    flags: c_int,
) -> c_int {
    let Some(host_flags) = fstatat_flags(flags) else {
        return -1;
    };
    let dirfd = match dirfd {
        DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    let ask = |path| {
        // SAFETY: a zeroed `struct stat` is what the host is asked to fill.
        let mut host: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: the caller's obligation for `path` and `dirfd`, and `host` is
        // a live local of the host's own layout, which is what this asks it to
        // fill; the call itself is made as Darwin.
        let rc =
            unsafe { crate::call_as_darwin!(libc::fstatat(dirfd, path, &mut host, host_flags)) };
        if rc == 0 && !out.is_null() {
            // SAFETY: the caller states `out` is writable for a Darwin `stat`.
            unsafe { *out = from_host(&host) };
        }
        (rc, rc == 0)
    };
    // `AT_EMPTY_PATH` asks about the descriptor and says nothing about the
    // path, so the host is handed the one spelling of "no path" that reaches
    // it: this host's `fstatat` answers `ENOENT` for a null path even with the
    // flag set, where Darwin accepts the null one and a guest may pass it.
    //
    // Deliberately *not* through `with_fallback_at`: that translates the path
    // through the prefix namespace, and there is no path here to translate -
    // the answer is about `dirfd` whatever the caller passed.
    if flags & DARWIN_AT_EMPTY_PATH != 0 {
        return ask(c"".as_ptr()).0;
    }
    // Which passes a null path straight through, and otherwise does the
    // case-folding retry `access`, `openat` and `faccessat` all share.
    with_fallback_at(dirfd, path, ask)
}

/// Translate a Darwin `AT_` word for a [`fstatat`], refusing bits that cannot be
/// honoured here.
///
/// `AT_SYMLINK_NOFOLLOW` is this host's own bit and crosses as itself.
/// `AT_SYMLINK_FOLLOW` becomes no bit, which is neither a refusal nor a loss:
/// following the link is what the host does unless a bit says otherwise, so
/// the answer is the answer the guest asked for. `AT_EMPTY_PATH` crosses as
/// itself, and is the one flag whose *effect* also has to be respelled, since
/// the host will not take a null path for it - see [`fstatat`].
///
/// `AT_NO_AUTOMOUNT` is the fourth bit a Darwin caller may pass, and it is
/// refused. There is no automounter on this host for it to hold back, and
/// accepting it would be a claim about the host's mounts that nothing here
/// backs - which is the same reason `open` refuses a flag it cannot honour
/// rather than dropping it.
fn fstatat_flags(flags: c_int) -> Option<c_int> {
    let known = DARWIN_AT_SYMLINK_NOFOLLOW | DARWIN_AT_SYMLINK_FOLLOW | DARWIN_AT_EMPTY_PATH;
    if flags & !known != 0 {
        crate::errno::set(libc::EINVAL);
        return None;
    }
    let mut host_flags = 0;
    if flags & DARWIN_AT_SYMLINK_NOFOLLOW != 0 {
        host_flags |= libc::AT_SYMLINK_NOFOLLOW;
    }
    if flags & DARWIN_AT_EMPTY_PATH != 0 {
        host_flags |= libc::AT_EMPTY_PATH;
    }
    Some(host_flags)
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

/// The guest spelling of a host path this crate is about to hand back.
///
/// `None` for one no mount reaches, which the caller reports as `ENOENT`.
pub(crate) fn guest_path(host: &CStr) -> Option<std::ffi::CString> {
    let host = host.to_str().ok()?;
    let guest = prefix::ns::to_guest(std::path::Path::new(host))?;
    std::ffi::CString::new(guest.into_os_string().into_encoded_bytes()).ok()
}

/// `FILE *fopen(const char *, const char *)`, with the same fallback.
///
/// # Safety
///
/// Both arguments are NUL-terminated strings.
pub unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut libc::FILE {
    let writing = unsafe { stdio_writes(mode) };
    with_fallback(path, |path| {
        if writing && !may_write(path) {
            return (std::ptr::null_mut(), false);
        }
        let file = unsafe { libc::fopen(path, mode) };
        (file, !file.is_null())
    })
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
    let Some(host_flags) = open_flags(flags) else {
        return -1;
    };
    let mode = open_mode(flags, mode);
    with_fallback(path, |path| {
        if writes(host_flags) && !may_write(path) {
            return (-1, false);
        }
        let fd = unsafe { libc::open(path, host_flags, mode) };
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
    let Some(host_flags) = open_flags(flags) else {
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
        let fd = unsafe { libc::openat(dirfd, path, host_flags, mode) };
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
    eprintln!("rosetta3: open: no equivalent on this host for Darwin O_ flags {unknown:#x}");
}

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

/// macOS's `struct statvfs` — **64 bytes**, and nothing like the 2 KB shape
/// `struct statfs` has.
///
/// # There are no name buffers here, and that is the whole point
///
/// `f_mntonname`, `f_mntfromname` and `f_mountfromname` are fields of
/// **`struct statfs`**, not of `struct statvfs`. Apple's own `statvfs(3)` man
/// page enumerates the members — `f_bsize`, `f_frsize`, `f_blocks`, `f_bfree`,
/// `f_bavail`, `f_files`, `f_ffree`, `f_favail`, `f_fsid`, `f_flag`,
/// `f_namemax` — and no name, and closes with "IMPLEMENTATION NOTES: The
/// statvfs() and fstatvfs() functions are implemented as wrappers around the
/// statfs() and fstatfs() functions, respectively. **Not all the information
/// provided by those functions is made available through this interface.**"
/// A caller that wants a mount point on macOS asks `getmntinfo` or `statfs`;
/// there is no mount point in this structure to derive one into, and inventing
/// 3 KB of them would be worse than the absence.
///
/// # The size is measured, not remembered — and it is load-bearing
///
/// A previous account of this entry had this structure at "~2 KB" and
/// `f_mntonname` as one of its fields. Writing 2 KB into this guest's buffer
/// would **smash its frame by about 3 KB**, which is the fault this entry
/// exists to fix. Three independent measurements, all from the guest's own
/// `__text`, agree on 64:
///
/// * `base::SysInfo::AmountOfFreeDiskSpace` at `Framework+0x5d83b90` (and
///   `AmountOfTotalDiskSpace` at `+0x5d83ce0`) each declare
///   `struct statvfs stats;` at `[rbp-0x70]` and pattern-initialise it with
///   **exactly four** `movaps` of the `0xaa` literal at `0xd637790` — 64 bytes
///   at `+0x5d83c0e`, `+0x5d83c12`, `+0x5d83c16`, `+0x5d83c1a`, and nothing
///   after. The two bytes that follow are a `xchg ax,ax` NOP, not a store.
/// * The same idiom, for `struct statfs`, emits
///   `mov edx,0x878; mov esi,0xaa; call _memset` at `+0x7424afe` — **2168**
///   bytes, which is Darwin's `sizeof(struct statfs)`, and the store that
///   follows reads `f_fstypename` at base `+0x48` and compares it against
///   `"hfs"` and `"ufs"`. That is the positive control: the pattern fill is
///   this compiler's `-ftrivial-auto-var-init=pattern` and it covers whole
///   objects, so its extent is the object's `sizeof`. (80 599 `movaps`
///   references to that one literal in `__TEXT,__literals` are the second
///   control.)
/// * The frame arithmetic closes: `sub rsp,0xa0` after `push rbp/r14/rbx`
///   leaves 160 bytes at `[rbp-0xb0, rbp-0x10)`, and that is 64 (the
///   `ScopedBlockingCall`) + 64 (`stats`) + 24 (`base::Location`) + 8 of slack.
///   The buffer at `rbp-0x70` therefore has 96 bytes to the saved `rbx` and
///   could not have held 2 KB whatever the header says.
///
/// ## Which offsets are measured and which are derived
///
/// Measured, by the reads the two functions perform:
///
/// | offset | width | what it is | evidence |
/// |---:|---:|---|---|
/// | `0x08` | 64 | the block size | `imul rbx, QWORD PTR [rbp-0x68]`, **the same operand in both functions** |
/// | `0x10` | 32 | total blocks | `mov ebx, DWORD PTR [rbp-0x60]` in `AmountOfTotalDiskSpace` |
/// | `0x18` | 32 | blocks available to others | `mov ebx, DWORD PTR [rbp-0x58]` in `AmountOfFreeDiskSpace` |
///
/// The two functions are `base/system/sys_info_posix.cc:184` and `:195`
/// (`FROM_HERE` on lines 185 and 196, and the function names are at
/// `0xd8eef0e` and `0xd8eef48`) and each returns
/// `saturated_cast<int64_t>(stats.f_bavail * stats.f_frsize)` or
/// `… f_blocks * stats.f_frsize` after `HANDLE_EINTR`. So `0x08` is the one
/// factor both share and `0x10`/`0x18` are the counters that differ — and the
/// counters are read **32 bits at a time**, which is why they are `u32` here
/// even though this host reports them as `unsigned long`.
///
/// The rest follow from the POSIX declaration order, from `0x08` being
/// eight bytes (so `f_bsize` is eight bytes at `0x00` — there is no room for a
/// four-byte `f_bsize` with a hole at `0x04`), and from the total being 64.
/// That derivation is recorded rather than hidden, and the two offsets the
/// guest's disk-space code depends on are the two that are measured.
///
/// # The 32-bit counters are macOS's, not a shortcut
///
/// A count this host reports above `2^32` does not fit, and is truncated
/// rather than refused: the field is thirty-two bits wide in the guest's own
/// layout, so macOS truncates it too. Refusing would invent a capability gap
/// that does not exist, and inventing a 64-bit field at those offsets would
/// put `f_bfree` where the guest reads `f_bavail`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Statvfs {
    pub f_bsize: u64,
    pub f_frsize: u64,
    pub f_blocks: u32,
    pub f_bfree: u32,
    pub f_bavail: u32,
    pub f_files: u32,
    pub f_ffree: u32,
    pub f_favail: u32,
    pub f_fsid: u64,
    pub f_flag: u32,
    pub f_namemax: u32,
    pub f_type: u32,
    _pad: u32,
}

const _: () = {
    assert!(size_of::<Statvfs>() == 64);
    // The three the guest reads, and the two that fix the width of `f_bsize`.
    assert!(std::mem::offset_of!(Statvfs, f_bsize) == 0);
    assert!(std::mem::offset_of!(Statvfs, f_frsize) == 8);
    assert!(std::mem::offset_of!(Statvfs, f_blocks) == 16);
    assert!(std::mem::offset_of!(Statvfs, f_bavail) == 24);
    assert!(std::mem::offset_of!(Statvfs, f_fsid) == 40);
    assert!(std::mem::offset_of!(Statvfs, f_flag) == 48);
    // This host's own `struct statvfs` is 112 bytes with every one of these
    // fields at a *different* offset and a different width, so a copy that
    // skipped the conversion would pass every size check this block makes and
    // hand the guest `f_frsize` where it reads `f_bavail`.
    assert!(size_of::<libc::statvfs>() == 112);
    assert!(std::mem::offset_of!(libc::statvfs, f_blocks) != 16
        || std::mem::offset_of!(libc::statvfs, f_bavail) != 24);
    assert!(size_of::<libc::statvfs>() != size_of::<Statvfs>());
};

/// Darwin's `f_flag` bits, which are `ST_RDONLY` and `ST_NOSUID` and nothing
/// else (`statvfs(3)`: "There are two flags defined for the `f_flag` member").
/// This host defines the same two at the same values and then a dozen more,
/// none of which names a mount option Darwin has.
const DARWIN_ST_RDONLY: u32 = 0x1;
const DARWIN_ST_NOSUID: u32 = 0x2;

/// A count this host reports wider than Darwin's field.
fn count(count: libc::c_ulong) -> u32 {
    count as u32
}

/// Fill a Darwin `struct statvfs` from this host's.
///
/// Every field is the same fact under the name the two platforms give it,
/// except `f_type` and `f_flag`, and both of those are refusals rather than
/// guesses — see [`say_unmapped_fstype`] and `f_flag`.
///
/// # Safety
///
/// `out` is null or a writable Darwin `struct statvfs`; `magic` reports this
/// host's filesystem magic for the same target, and is only called if the
/// `f_type` refusal has not been said yet.
unsafe fn fill_statvfs(
    host: &libc::statvfs,
    magic: impl FnOnce() -> libc::c_long,
    out: *mut Statvfs,
) {
    if out.is_null() {
        return;
    }
    let value = Statvfs {
        f_bsize: host.f_bsize as u64,
        // This host's `f_frsize` is the *fragment* size and is 0 for a
        // filesystem that has no separate one, where macOS's `f_frsize` is the
        // fundamental allocation unit — `statvfs(3)`: "This corresponds to the
        // f_bsize member of struct statfs." Both platforms therefore answer
        // `f_bsize` for a filesystem with one block size, and taking that
        // member when the fragment size is absent is the same fact rather than
        // a substitute for it. This host reports 4096 for both on ext4 and
        // tmpfs, so the fallback is not what the answer is today.
        f_frsize: match host.f_frsize {
            0 => host.f_bsize as u64,
            fragment => fragment as u64,
        },
        f_blocks: count(host.f_blocks),
        f_bfree: count(host.f_bfree),
        f_bavail: count(host.f_bavail),
        f_files: count(host.f_files),
        f_ffree: count(host.f_ffree),
        f_favail: count(host.f_favail),
        // One `unsigned long` of opaque identifier on both sides; the 0x38 on
        // this host and the two `int`s Darwin's `fsid_t` is made of are the
        // same eight bytes, and nothing in either platform interprets them.
        f_fsid: host.f_fsid as u64,
        // Only the two bits Darwin names. This host's word also carries
        // `ST_NODEV`, `ST_NOEXEC`, `ST_SYNCHRONOUS`, `ST_RELATIME` and more,
        // and none of those has a Darwin name, so none is claimed — a caller
        // that tested a bit Darwin does not define would be reading a bit this
        // runtime invented.
        f_flag: host.f_flag as u32 & (DARWIN_ST_RDONLY | DARWIN_ST_NOSUID),
        f_namemax: host.f_namemax as u32,
        // Refused. See [`say_unmapped_fstype`].
        f_type: 0,
        _pad: 0,
    };
    if value.f_type == 0 {
        say_unmapped_fstype(magic);
    }
    // SAFETY: the caller's obligation; `out` is non-null and the guest sized it
    // at exactly 64 bytes, which is what `assert!` above pins.
    unsafe { out.write(value) };
}

/// This host's filesystem magic for `path`, or 0 if it cannot be read.
///
/// `libc::statvfs` has no `f_type` — glibc added it to `statvfs` in 2.36 and
/// the Rust binding still mirrors the older header — and `statfs` carries the
/// same number under the same name, so this is the same fact asked a second
/// way. Only the refusal message needs it, and that asks at most once per
/// distinct magic, so the extra call is not on any path a caller pays for.
fn fstype_of(path: *const c_char) -> libc::c_long {
    with_fallback(path, |path| {
        // SAFETY: a zeroed `struct statfs` is what the host is asked to fill.
        let mut host: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: the caller's obligation, and `host` is this host's own layout.
        let rc = unsafe { libc::statfs(path, &mut host) };
        (host.f_type, rc == 0)
    })
}

/// [`fstype_of`] for a descriptor, so the refusal names the filesystem the
/// caller actually asked about.
fn fstype_of_fd(fd: c_int) -> libc::c_long {
    // SAFETY: a zeroed `struct statfs` is what the host is asked to fill.
    let mut host: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `host` is this host's own layout and `fd` is the caller's.
    let rc = unsafe { libc::fstatfs(fd, &mut host) };
    let _ = rc;
    host.f_type
}

/// Name a filesystem whose magic has no Darwin `OSTYPE` once.
///
/// Darwin's `f_type` is an `OSTYPE` and this host's is a filesystem magic; the
/// two agree for no filesystem this host actually has — ext4 is `0xef53` here
/// and tmpfs is `0x01021994`, and neither is a four-character `OSTYPE`. The
/// field is therefore left zero and this says so, once, by name: a caller that
/// switches on `f_type` sees "not a filesystem I can name" instead of a magic
/// that means nothing on macOS and would match nothing there either.
fn say_unmapped_fstype(magic: impl FnOnce() -> libc::c_long) {
    use std::sync::Mutex;

    static SAID: Mutex<Vec<libc::c_long>> = Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    let magic = magic();
    if said.contains(&magic) {
        return;
    }
    said.push(magic);
    eprintln!(
        "rosetta3: statvfs: no Darwin f_type for this host's filesystem magic {magic:#x}; \
         the field is left zero rather than given a number that would name nothing on macOS"
    );
}

/// `int statvfs(const char *, struct statvfs *)`
///
/// # Safety
///
/// `path` is NUL-terminated and `out` is null or a writable Darwin
/// `struct statvfs`.
pub unsafe extern "C" fn statvfs(path: *const c_char, out: *mut Statvfs) -> c_int {
    with_fallback(path, |path| {
        // SAFETY: a zeroed `struct statvfs` is what the host is asked to fill.
        let mut host: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: the caller's obligation, and `host` is this host's own layout.
        let result = unsafe { libc::statvfs(path, &mut host) };
        if result == 0 {
            unsafe { fill_statvfs(&host, || fstype_of(path), out) };
        }
        (result, result == 0)
    })
}

/// `int fstatvfs(int, struct statvfs *)`
///
/// Asked about a **descriptor**, so the host is asked about the descriptor:
/// `fstatvfs` on this host takes an `int` too, and resolving the descriptor to
/// a path to reopen would answer about whatever now occupies that name, which
/// for an unlinked file is nothing. The test
/// `fstatvfs_asks_about_the_descriptor_and_not_the_name` holds that apart.
///
/// # Safety
///
/// As [`statvfs`].
pub unsafe extern "C" fn fstatvfs(fd: c_int, out: *mut Statvfs) -> c_int {
    // SAFETY: a zeroed `struct statvfs` is what the host is asked to fill.
    let mut host: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `host` is this host's own layout and `fd` is the caller's.
    let result = unsafe { libc::fstatvfs(fd, &mut host) };
    if result == 0 {
        unsafe { fill_statvfs(&host, || fstype_of_fd(fd), out) };
    }
    result
}

// Every name this module answers, in both spellings a Mach-O may import them
// by - see `crate::base_name` for why `$INODE64` is not a name of its own, and
// `crate::divergent` for the list that stops the host's own being bound
// instead where this crate has not answered it.
exports::exports! {
    functions {
        "stat" | "stat$INODE64" => stat,
        "lstat" | "lstat$INODE64" => lstat,
        "fstat" | "fstat$INODE64" => fstat,
        "fstatat" | "fstatat$INODE64" => fstatat,
        "statfs" | "statfs$INODE64" => statfs,
        "fstatfs" | "fstatfs$INODE64" => fstatfs,
        "statvfs" | "statvfs$INODE64" => statvfs,
        "fstatvfs" | "fstatvfs$INODE64" => fstatvfs,
        "access" => access,
        "faccessat" => faccessat,
        "realpath" => realpath,
        "readlink" => readlink,
        "fopen" | "fopen$UNIX2003" => fopen,
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

    #[test]
    fn every_listed_name_resolves_and_a_variant_is_its_base() {
        for name in names() {
            assert!(symbol(name).is_some(), "{name}");
            assert_eq!(symbol(name), symbol(base(name)), "{name}");
        }
    }

    /// The layout is the guest's, and the size a guest reads is the file's.
    #[test]
    fn a_size_survives_the_conversion() {
        let at = std::env::temp_dir().join(format!("r3-stat-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("r3-stat-ci-{}", std::process::id()));
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

    /// The claim of the whole entry, end to end: the name a Mach-O imports
    /// binds, with the underscore and with the variant suffix, and the refusal
    /// list that used to name it does not get to deny what is answered here.
    #[test]
    fn a_published_fstatat_binds_and_is_not_refused() {
        assert!(
            crate::symbol("_fstatat").is_some(),
            "_fstatat is published and unanswered"
        );
        assert!(crate::symbol("_fstatat$INODE64").is_some());
        assert_eq!(crate::divergent::reason("fstatat"), None);
    }

    /// The structure, then: a guest reads it at Darwin's offsets, and this
    /// host's own is the same 144 bytes rearranged. So the answer is checked as
    /// *bytes* at the offsets Darwin's headers put the fields at, and not only
    /// through the field names - a call that filled the guest's buffer with
    /// this host's layout would satisfy every name-based assertion below and put
    /// nothing where a guest reads its size.
    #[test]
    fn fstatat_agrees_with_stat_and_lands_its_fields_at_darwin_offsets() {
        const WRITTEN: &[u8] = b"fstatat";
        let at = std::env::temp_dir().join(format!("r3-fstatat-{}", std::process::id()));
        let _ = std::fs::remove_file(&at);
        std::fs::write(&at, WRITTEN).expect("a file");
        let path = CString::new(at.to_str().expect("utf-8")).expect("no NUL");

        let mut out = Stat::default();
        // SAFETY: a NUL-terminated path, Darwin's AT_FDCWD, no flags, and a
        // writable Darwin `struct stat`.
        let rc = unsafe { fstatat(DARWIN_AT_FDCWD, path.as_ptr(), &mut out, 0) };
        assert_eq!(rc, 0, "fstatat failed: {}", std::io::Error::last_os_error());

        // The same question the way `stat` asks it, which is the whole claim:
        // one structure, filled one way, whichever entry asks.
        let mut expected = Stat::default();
        // SAFETY: a NUL-terminated path and a writable Darwin `stat`.
        let rc = unsafe { stat(path.as_ptr(), &mut expected) };
        let _ = std::fs::remove_file(&at);
        assert_eq!(rc, 0);
        assert_eq!(out.ino, expected.ino);
        assert_eq!(out.mode, expected.mode);
        assert_eq!(out.size, expected.size);
        assert_eq!(out.size, WRITTEN.len() as i64);

        // And as the bytes a guest reading its own buffer would see. `Stat` has
        // no padding and every field in it is an integer, so all 144 bytes of
        // it are defined.
        let bytes = unsafe {
            std::slice::from_raw_parts((&out as *const Stat).cast::<u8>(), size_of::<Stat>())
        };
        let size = std::mem::offset_of!(Stat, size);
        let mode = std::mem::offset_of!(Stat, mode);
        assert_eq!(
            i64::from_le_bytes(bytes[size..size + 8].try_into().expect("eight bytes")),
            WRITTEN.len() as i64,
            "the size is not where Darwin's headers put it"
        );
        assert_eq!(
            u16::from_le_bytes(bytes[mode..mode + 2].try_into().expect("two bytes")) & 0o170000,
            libc::S_IFREG as u16,
            "the mode is not where Darwin's headers put it"
        );
        // Which the two layouts have nothing to do with: this host puts the mode
        // and the size at offsets of its own, so a copy in either direction
        // would have answered every assertion above.
        assert_ne!(std::mem::offset_of!(libc::stat, st_mode), mode);
        assert_ne!(std::mem::offset_of!(libc::stat, st_size), size);
    }

    /// The flag does the work: a link is not what it points at, and a caller
    /// that asked about the link is asking about a different file.
    #[test]
    fn fstatat_without_symlink_nofollow_reports_the_link() {
        let dir = std::env::temp_dir().join(format!("r3-fstatat-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory");
        let target = dir.join("target");
        std::fs::write(&target, b"x").expect("a file");
        std::os::unix::fs::symlink(&target, dir.join("link")).expect("a symlink");
        let link = CString::new(dir.join("link").to_str().expect("utf-8")).expect("no NUL");

        let mut of_link = Stat::default();
        // SAFETY: a NUL-terminated path, Darwin's AT_FDCWD, and a flag from the
        // set this entry answers.
        let rc = unsafe {
            fstatat(
                DARWIN_AT_FDCWD,
                link.as_ptr(),
                &mut of_link,
                DARWIN_AT_SYMLINK_NOFOLLOW,
            )
        };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        let mut of_target = Stat::default();
        // SAFETY: as above, with no flag: the same call, following the link.
        let rc = unsafe { fstatat(DARWIN_AT_FDCWD, link.as_ptr(), &mut of_target, 0) };
        assert_eq!(rc, 0);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            of_link.mode & 0o170000,
            libc::S_IFLNK as u16,
            "the link itself"
        );
        assert_eq!(
            of_target.mode & 0o170000,
            libc::S_IFREG as u16,
            "what it points at"
        );
        assert_ne!(of_link.ino, of_target.ino, "two different files");
    }

    /// The spelling a guest cannot get right by passing the path it happens to
    /// have: Darwin's `fstatat` takes a null path with `AT_EMPTY_PATH`, and
    /// this host's takes an empty one.
    #[test]
    fn fstatat_with_empty_path_reports_the_descriptor_itself() {
        const WRITTEN: &[u8] = b"descriptor";
        let at = std::env::temp_dir().join(format!("r3-fstatat-empty-{}", std::process::id()));
        let _ = std::fs::remove_file(&at);
        std::fs::write(&at, WRITTEN).expect("a file");
        let path = CString::new(at.to_str().expect("utf-8")).expect("no NUL");
        // SAFETY: a NUL-terminated path and Darwin scalar arguments.
        let fd = unsafe { open(path.as_ptr(), crate::fcntl::oflag::O_RDONLY, 0) };
        assert!(fd >= 0, "open failed: {}", std::io::Error::last_os_error());

        let mut by_descriptor = Stat::default();
        // SAFETY: a live descriptor, a null path with the one flag that allows
        // it, and a writable Darwin `struct stat`.
        let rc = unsafe {
            fstatat(
                fd,
                std::ptr::null(),
                &mut by_descriptor,
                DARWIN_AT_EMPTY_PATH,
            )
        };
        let mut by_fstat = Stat::default();
        // SAFETY: a live descriptor and a writable Darwin `struct stat`.
        let answered = unsafe { fstat(fd, &mut by_fstat) };
        // SAFETY: a descriptor this test just opened.
        unsafe { libc::close(fd) };
        let _ = std::fs::remove_file(&at);
        assert_eq!(
            rc,
            0,
            "AT_EMPTY_PATH failed: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(answered, 0);

        assert_eq!(by_descriptor.ino, by_fstat.ino);
        assert_eq!(by_descriptor.size, WRITTEN.len() as i64);
    }

    #[test]
    fn fstatat_refuses_a_flag_the_host_cannot_honor() {
        // Darwin's AT_NO_AUTOMOUNT asks an automounter to hold an unmount back.
        const AT_NO_AUTOMOUNT: c_int = 0x0080;
        let path = c"/dev/null";
        crate::errno::set(0);
        // SAFETY: a static NUL-terminated path, Darwin's AT_FDCWD, a flag, and a
        // null `struct stat` the call must not write through.
        let rc = unsafe {
            fstatat(
                DARWIN_AT_FDCWD,
                path.as_ptr(),
                std::ptr::null_mut(),
                AT_NO_AUTOMOUNT,
            )
        };
        assert_eq!(rc, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // AT_SYMLINK_FOLLOW is the bit that is *not* refused: it asks for what
        // this host does anyway, so it needs no bit of its own to get it.
        // SAFETY: as above.
        let rc = unsafe {
            fstatat(
                DARWIN_AT_FDCWD,
                path.as_ptr(),
                std::ptr::null_mut(),
                DARWIN_AT_SYMLINK_FOLLOW,
            )
        };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        crate::errno::set(0);
    }

    /// And the case-insensitive fallback stays relative to the descriptor, as
    /// [`openat`]'s does.
    #[test]
    fn fstatat_case_folds_relative_to_its_descriptor() {
        let dir = std::env::temp_dir().join(format!("r3-fstatat-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Contents")).expect("a directory");
        std::fs::write(dir.join("Contents").join("Game.ios"), b"x").expect("a file");
        let directory = CString::new(dir.to_str().expect("utf-8")).expect("no NUL");
        // SAFETY: a NUL-terminated directory path and Darwin scalar arguments.
        let dirfd = unsafe { open(directory.as_ptr(), crate::fcntl::oflag::O_RDONLY, 0) };
        assert!(dirfd >= 0);

        let mut out = Stat::default();
        // SAFETY: a live directory descriptor, a NUL-terminated relative path,
        // and no flags.
        let rc = unsafe { fstatat(dirfd, c"contents/game.ios".as_ptr(), &mut out, 0) };
        // SAFETY: a descriptor this test just opened.
        unsafe { libc::close(dirfd) };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            rc,
            0,
            "the lowercased path was not found: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(out.size, 1);
    }

    #[test]
    fn open_translates_darwin_status_flags() {
        let at = std::env::temp_dir().join(format!("r3-open-flags-{}", std::process::id()));
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
        let at = std::env::temp_dir().join(format!("r3-openat-cwd-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("r3-openat-dir-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("r3-openat-case-{}", std::process::id()));
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
        let at = std::env::temp_dir().join(format!("r3-fstatat-cwd-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("r3-fstatat-dir-{}", std::process::id()));
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
        assert_eq!(followed.mode as u32 & 0o170000, libc::S_IFREG);
        assert_eq!(followed.size, 3);
        let (rc, link) = stat_at(c"link", DARWIN_AT_SYMLINK_NOFOLLOW);
        assert_eq!(rc, 0);
        assert_eq!(link.mode as u32 & 0o170000, libc::S_IFLNK);
        let (rc, folded) = stat_at(c"contents/game.ios", 0);
        assert_eq!(rc, 0, "the case fold stays relative to the descriptor");
        assert_eq!(folded.size, 3);

        // SAFETY: a descriptor this test just opened.
        unsafe { libc::close(dirfd) };
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

    /// What the host says about `path`, for the tests below to compare against.
    fn host_statvfs(path: &[u8]) -> libc::statvfs {
        // SAFETY: a zeroed `struct statvfs` is what the host is asked to fill.
        let mut host: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is a NUL-terminated string the caller built.
        let rc = unsafe { libc::statvfs(cstr(path).as_ptr(), &mut host) };
        assert_eq!(rc, 0, "host statvfs: {}", std::io::Error::last_os_error());
        host
    }

    /// A NUL-terminated copy of a byte string, for a `*const c_char`.
    fn cstr(bytes: &[u8]) -> CString {
        CString::new(bytes).expect("no NUL")
    }

    /// The structure is 64 bytes and lands its fields where the guest reads
    /// them. This is the assertion that would fail if the 2 KB `statfs`-shaped
    /// structure were used here, and it fails *loudly and early* rather than by
    /// smashing a frame three kilobytes up: 64 is the size the guest's own
    /// compiler reserved, measured in the doc comment on [`Statvfs`].
    #[test]
    fn a_statvfs_is_sixty_four_bytes_with_its_fields_where_the_guest_reads_them() {
        assert_eq!(size_of::<Statvfs>(), 64);
        assert_eq!(std::mem::offset_of!(Statvfs, f_bsize), 0);
        assert_eq!(std::mem::offset_of!(Statvfs, f_frsize), 8);
        assert_eq!(std::mem::offset_of!(Statvfs, f_blocks), 16);
        assert_eq!(std::mem::offset_of!(Statvfs, f_bavail), 24);
        // And the host's is neither the size nor the layout, which is the whole
        // reason this function exists.
        assert_ne!(size_of::<libc::statvfs>(), size_of::<Statvfs>());
        assert_ne!(std::mem::offset_of!(libc::statvfs, f_bavail), 24);
    }

    /// A real path reports a block size and a non-zero block count — the two
    /// facts `base::SysInfo::AmountOfFreeDiskSpace` is asking for, and a
    /// refusal that returned zero for either would satisfy a caller that never
    /// looked.
    #[test]
    fn statvfs_on_a_real_path_reports_a_block_size_and_a_block_count() {
        let path = cstr(b"/");
        let mut out = Statvfs {
            f_bsize: 0,
            f_frsize: 0,
            f_blocks: 0,
            f_bfree: 0,
            f_bavail: 0,
            f_files: 0,
            f_ffree: 0,
            f_favail: 0,
            f_fsid: 0,
            f_flag: 0,
            f_namemax: 0,
            f_type: 0,
            _pad: 0,
        };
        // SAFETY: a NUL-terminated path and a writable Darwin `statvfs`.
        let rc = unsafe { statvfs(path.as_ptr(), &mut out) };
        assert_eq!(rc, 0, "statvfs failed: {}", std::io::Error::last_os_error());
        assert!(out.f_bsize > 0, "no block size");
        assert!(out.f_blocks > 0, "a filesystem with no blocks at all");
        assert!(out.f_bavail > 0, "no block available to this user");
        assert!(out.f_frsize > 0, "no fundamental block size");
        assert!(out.f_files > 0, "no file nodes at all");
    }

    /// Every field this entry claims to fill, asserted against the host's own
    /// answer for the same path — and asserted as **bytes at Darwin's offsets**,
    /// not through the field names, so a call that filled the guest's buffer
    /// with this host's layout could not satisfy them.
    #[test]
    fn every_field_matches_the_host_and_sits_at_its_offset() {
        let raw = b"/";
        let path = cstr(raw);
        let host = host_statvfs(raw);

        let mut out = Statvfs {
            f_bsize: 0,
            f_frsize: 0,
            f_blocks: 0,
            f_bfree: 0,
            f_bavail: 0,
            f_files: 0,
            f_ffree: 0,
            f_favail: 0,
            f_fsid: 0,
            f_flag: 0,
            f_namemax: 0,
            f_type: 0,
            _pad: 0,
        };
        // SAFETY: a NUL-terminated path and a writable Darwin `statvfs`.
        let rc = unsafe { statvfs(path.as_ptr(), &mut out) };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());

        // The counts, the two block sizes, the identifier, the name limit.
        assert_eq!(out.f_bsize, host.f_bsize as u64, "f_bsize");
        assert_eq!(
            out.f_frsize,
            match host.f_frsize {
                0 => host.f_bsize as u64,
                fragment => fragment as u64,
            },
            "f_frsize"
        );
        assert_eq!(out.f_blocks, host.f_blocks as u32, "f_blocks");
        assert_eq!(out.f_bfree, host.f_bfree as u32, "f_bfree");
        assert_eq!(out.f_bavail, host.f_bavail as u32, "f_bavail");
        assert_eq!(out.f_files, host.f_files as u32, "f_files");
        assert_eq!(out.f_ffree, host.f_ffree as u32, "f_ffree");
        assert_eq!(out.f_favail, host.f_favail as u32, "f_favail");
        assert_eq!(out.f_fsid, host.f_fsid as u64, "f_fsid");
        assert_eq!(out.f_namemax, host.f_namemax as u32, "f_namemax");
        // The two flag bits Darwin defines, and no other.
        assert_eq!(
            out.f_flag,
            host.f_flag as u32 & (DARWIN_ST_RDONLY | DARWIN_ST_NOSUID),
            "f_flag carries only the two bits Darwin names"
        );
        // `f_type` is the one field this entry refuses, and the refusal is
        // visible: zero, and said once by name on stderr.
        assert_eq!(out.f_type, 0, "f_type is a refusal, not an answer");

        // And as the bytes a guest reading its own buffer would see. Every
        // field in `Statvfs` is an integer, so all 64 bytes are defined.
        let bytes = unsafe {
            std::slice::from_raw_parts((&out as *const Statvfs).cast::<u8>(), size_of::<Statvfs>())
        };
        // `f_bavail` is four bytes wide, so it is read as four - which is the
        // point: the guest reads it with `mov ebx, DWORD PTR [rbp-0x58]`.
        let word = |offset: usize| {
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
        };
        assert_eq!(
            word(std::mem::offset_of!(Statvfs, f_bavail)),
            host.f_bavail as u32,
            "f_bavail is not four bytes wide where the guest reads it"
        );
        assert_eq!(
            word(std::mem::offset_of!(Statvfs, f_blocks)),
            host.f_blocks as u32,
            "f_blocks is not four bytes wide where the guest reads it"
        );
        assert_eq!(
            u64::from_le_bytes(
                bytes[std::mem::offset_of!(Statvfs, f_frsize)
                    ..std::mem::offset_of!(Statvfs, f_frsize) + 8]
                    .try_into()
                    .expect("eight bytes")
            ),
            match host.f_frsize {
                0 => host.f_bsize as u64,
                fragment => fragment as u64,
            },
            "f_frsize is not eight bytes wide at +8, where the guest's shared operand is"
        );
        // Which the host's own layout contradicts: on this host `f_bavail` is at
        // 32 and eight bytes wide, so a straight copy would have satisfied
        // every name-based assertion above and put nothing where the guest
        // looks.
        assert_ne!(std::mem::offset_of!(libc::statvfs, f_bavail), 24);
    }

    /// `fstatvfs` asks about the descriptor. The file is unlinked before the
    /// second call, so an implementation that resolved the descriptor to a path
    /// and reopened it would answer `ENOENT` — and a reopen of *that name*
    /// would answer about a different file entirely.
    #[test]
    fn fstatvfs_asks_about_the_descriptor_and_not_the_name() {
        let at = std::env::temp_dir().join(format!("r3-fstatvfs-{}", std::process::id()));
        let _ = std::fs::remove_file(&at);
        std::fs::write(&at, b"descriptor").expect("a file");
        let path = cstr(at.to_str().expect("utf-8").as_bytes());
        // SAFETY: a NUL-terminated path and Darwin scalar arguments.
        let fd = unsafe { open(path.as_ptr(), crate::fcntl::oflag::O_RDONLY, 0) };
        assert!(fd >= 0, "open failed: {}", std::io::Error::last_os_error());

        let mut by_name = Statvfs {
            f_bsize: 0,
            f_frsize: 0,
            f_blocks: 0,
            f_bfree: 0,
            f_bavail: 0,
            f_files: 0,
            f_ffree: 0,
            f_favail: 0,
            f_fsid: 0,
            f_flag: 0,
            f_namemax: 0,
            f_type: 0,
            _pad: 0,
        };
        // SAFETY: a NUL-terminated path and a writable Darwin `statvfs`.
        let before = unsafe { statvfs(path.as_ptr(), &mut by_name) };
        assert_eq!(before, 0, "{}", std::io::Error::last_os_error());

        // The name goes away; the descriptor does not.
        let _ = std::fs::remove_file(&at);

        let mut by_descriptor = by_name;
        // SAFETY: a live descriptor and a writable Darwin `statvfs`.
        let rc = unsafe { fstatvfs(fd, &mut by_descriptor) };
        // SAFETY: a descriptor this test just opened.
        unsafe { libc::close(fd) };
        crate::errno::set(0);
        assert_eq!(
            rc,
            0,
            "fstatvfs on an unlinked file failed: {}",
            std::io::Error::last_os_error()
        );
        assert!(by_descriptor.f_bsize > 0);
        assert!(by_descriptor.f_blocks > 0);
        // The same filesystem, so the same answer.
        assert_eq!(by_descriptor.f_bsize, by_name.f_bsize);
        assert_eq!(by_descriptor.f_blocks, by_name.f_blocks);
        assert_eq!(by_descriptor.f_bavail, by_name.f_bavail);
    }

    /// The claim of the whole entry, end to end: the name a Mach-O imports
    /// binds, with the underscore `exports!` publishes, and the refusal list
    /// that used to name it does not get to deny what is answered here.
    #[test]
    fn a_published_statvfs_binds_and_is_not_refused() {
        assert!(
            crate::symbol("_statvfs").is_some(),
            "_statvfs is published and unanswered"
        );
        assert!(crate::symbol("_fstatvfs").is_some());
        assert_eq!(crate::divergent::reason("statvfs"), None);
        assert_eq!(crate::divergent::reason("fstatvfs"), None);
    }

    /// And the answer is 64 bytes into a buffer the caller sized at 64 — the
    /// test that fails without the whole entry, and the one that would have
    /// caught the frame it is here to prevent. A canary immediately after the
    /// structure is what a 2 KB fill would step on.
    #[test]
    fn the_fill_does_not_step_outside_the_sixty_four_bytes() {
        const GUARD: u8 = 0xa5;
        let mut buffer = [GUARD; 96];
        // SAFETY: `buffer` is 96 bytes and the first 64 of it are handed over
        // as a Darwin `struct statvfs`, which is 64 bytes.
        let rc = unsafe { statvfs(c"/".as_ptr(), buffer.as_mut_ptr().cast::<Statvfs>()) };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        assert!(
            buffer[64..].iter().all(|b| *b == GUARD),
            "the fill wrote past the structure: {:?}",
            &buffer[64..]
        );
        assert!(
            buffer[..64].iter().any(|b| *b != 0),
            "nothing at all was written"
        );
    }
}
