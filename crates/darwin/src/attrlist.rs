use std::ffi::{CStr, c_char, c_int, c_void};

use crate::stat::Stat;

/// `struct attrlist`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AttrList {
    pub bitmapcount: u16,
    pub reserved: u16,
    pub commonattr: u32,
    pub volattr: u32,
    pub dirattr: u32,
    pub fileattr: u32,
    pub forkattr: u32,
}

const ATTR_BIT_MAP_COUNT: u16 = 5;

const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_CHGTIME: u32 = 0x0000_0800;
const ATTR_CMN_ACCTIME: u32 = 0x0000_1000;
const ATTR_CMN_OWNERID: u32 = 0x0000_8000;
const ATTR_CMN_GRPID: u32 = 0x0001_0000;
const ATTR_CMN_ACCESSMASK: u32 = 0x0002_0000;
const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;

const ATTR_DIR_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_DIR_ENTRYCOUNT: u32 = 0x0000_0002;

const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0002;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
const ATTR_FILE_IOBLOCKSIZE: u32 = 0x0000_0008;
const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
const ATTR_FILE_DATAALLOCSIZE: u32 = 0x0000_0400;

const COMMON: u32 = ATTR_CMN_DEVID
    | ATTR_CMN_OBJTYPE
    | ATTR_CMN_MODTIME
    | ATTR_CMN_CHGTIME
    | ATTR_CMN_ACCTIME
    | ATTR_CMN_OWNERID
    | ATTR_CMN_GRPID
    | ATTR_CMN_ACCESSMASK
    | ATTR_CMN_FLAGS
    | ATTR_CMN_FILEID
    | ATTR_CMN_RETURNED_ATTRS;
/// What only `getattrlistbulk` answers: an entry's name is the one thing a
/// listing does not already know, and its error is per entry.
const BULK_ONLY: u32 = ATTR_CMN_NAME | ATTR_CMN_ERROR;
const DIR: u32 = ATTR_DIR_LINKCOUNT | ATTR_DIR_ENTRYCOUNT;
const FILE: u32 = ATTR_FILE_LINKCOUNT
    | ATTR_FILE_TOTALSIZE
    | ATTR_FILE_ALLOCSIZE
    | ATTR_FILE_IOBLOCKSIZE
    | ATTR_FILE_DATALENGTH
    | ATTR_FILE_DATAALLOCSIZE;

const FSOPT_NOFOLLOW: u64 = 0x1;
const FSOPT_REPORT_FULLSIZE: u64 = 0x4;
const FSOPT_PACK_INVAL_ATTRS: u64 = 0x8;
const OPTIONS: u64 = FSOPT_NOFOLLOW | FSOPT_REPORT_FULLSIZE | FSOPT_PACK_INVAL_ATTRS;

/// `enum vtype`, which `ATTR_CMN_OBJTYPE` reports.
fn object_type(mode: u16) -> u32 {
    match libc::mode_t::from(mode) & libc::S_IFMT {
        libc::S_IFREG => 1,
        libc::S_IFDIR => 2,
        libc::S_IFBLK => 3,
        libc::S_IFCHR => 4,
        libc::S_IFLNK => 5,
        libc::S_IFSOCK => 6,
        libc::S_IFIFO => 7,
        _ => 0,
    }
}

/// Why a request was refused, before anything was looked up.
fn check(list: &AttrList, options: u64, bulk: bool) -> Result<(), c_int> {
    if list.bitmapcount != ATTR_BIT_MAP_COUNT || options & !OPTIONS != 0 {
        return Err(libc::EINVAL);
    }
    // A bulk listing is unreadable without the returned set, so Darwin insists.
    if bulk && list.commonattr & ATTR_CMN_RETURNED_ATTRS == 0 {
        return Err(libc::EINVAL);
    }
    let common = match bulk {
        true => COMMON | BULK_ONLY,
        false => COMMON,
    };
    let unsupported = [
        ("common", list.commonattr & !common),
        ("volume", list.volattr),
        ("directory", list.dirattr & !DIR),
        ("file", list.fileattr & !FILE),
        ("fork", list.forkattr),
    ];
    if unsupported.iter().all(|(_, bits)| *bits == 0) {
        return Ok(());
    }
    let named: Vec<String> = unsupported
        .iter()
        .filter(|(_, bits)| *bits != 0)
        .map(|(what, bits)| format!("{what} {bits:#x}"))
        .collect();
    eprintln!(
        "rosemary: getattrlist: attributes not implemented: {}",
        named.join(", ")
    );
    Err(libc::ENOTSUP)
}

/// Lay out the answer for an object `found` describes, or failed to. `entries`
/// is the number of directory entries, asked for only when it is wanted.
///
/// `name` is given only for a `getattrlistbulk` entry, which also pads the
/// entry to 8 bytes so the next one starts aligned.
fn pack(
    list: &AttrList,
    options: u64,
    found: Result<&Stat, c_int>,
    name: Option<&[u8]>,
    entries: impl FnOnce() -> u32,
) -> Vec<u8> {
    let zero = Stat::default();
    let (st, error) = match found {
        Ok(st) => (st, 0),
        Err(error) => (&zero, error),
    };
    let is_dir = error == 0 && object_type(st.mode) == 2;
    let pack_invalid = options & FSOPT_PACK_INVAL_ATTRS != 0;
    // Darwin returns directory attributes only for a directory and file
    // attributes only for anything else. The others are left out, or packed as
    // zero with FSOPT_PACK_INVAL_ATTRS so the layout is the one asked for. An
    // entry that could not be looked up has only its name and its error.
    let common_asked = list.commonattr & !ATTR_CMN_RETURNED_ATTRS;
    let (common_valid, dir_valid, file_valid) = match (error, is_dir) {
        (0, true) => (common_asked & !ATTR_CMN_ERROR, list.dirattr, 0),
        (0, false) => (common_asked & !ATTR_CMN_ERROR, 0, list.fileattr),
        _ => (common_asked & (ATTR_CMN_NAME | ATTR_CMN_ERROR), 0, 0),
    };
    let (common, dir, file) = match pack_invalid {
        true => (common_asked, list.dirattr, list.fileattr),
        false => (common_valid, dir_valid, file_valid),
    };

    let mut out = vec![0u8; 4];
    if list.commonattr & ATTR_CMN_RETURNED_ATTRS != 0 {
        // `attribute_set_t`: what is actually valid, then its own padding
        // word. The flag itself is not listed as returned.
        for bits in [common_valid, 0, dir_valid, file_valid, 0] {
            out.extend_from_slice(&bits.to_ne_bytes());
        }
    }
    // An `attrreference_t`, filled in once the name's place is known.
    let name_ref = (common & ATTR_CMN_NAME != 0).then(|| {
        out.extend_from_slice(&[0; 8]);
        out.len() - 8
    });
    let time = |out: &mut Vec<u8>, t: crate::stat::Timespec| {
        out.extend_from_slice(&t.sec.to_ne_bytes());
        out.extend_from_slice(&t.nsec.to_ne_bytes());
    };
    let u32_ = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_ne_bytes());
    let i64_ = |out: &mut Vec<u8>, v: i64| out.extend_from_slice(&v.to_ne_bytes());
    if common & ATTR_CMN_DEVID != 0 {
        u32_(&mut out, st.dev as u32);
    }
    if common & ATTR_CMN_OBJTYPE != 0 {
        u32_(&mut out, object_type(st.mode));
    }
    if common & ATTR_CMN_MODTIME != 0 {
        time(&mut out, st.mtime);
    }
    if common & ATTR_CMN_CHGTIME != 0 {
        time(&mut out, st.ctime);
    }
    if common & ATTR_CMN_ACCTIME != 0 {
        time(&mut out, st.atime);
    }
    if common & ATTR_CMN_OWNERID != 0 {
        u32_(&mut out, st.uid);
    }
    if common & ATTR_CMN_GRPID != 0 {
        u32_(&mut out, st.gid);
    }
    if common & ATTR_CMN_ACCESSMASK != 0 {
        u32_(&mut out, u32::from(st.mode) & 0o7777);
    }
    if common & ATTR_CMN_FLAGS != 0 {
        u32_(&mut out, st.flags);
    }
    if common & ATTR_CMN_FILEID != 0 {
        i64_(&mut out, st.ino as i64);
    }
    if common & ATTR_CMN_ERROR != 0 {
        u32_(&mut out, error as u32);
    }
    if dir & ATTR_DIR_LINKCOUNT != 0 {
        // Hard links *to the directory*, not `st_nlink`: a Linux directory's
        // link count also counts its subdirectories' `..`, and `du` would take
        // every directory with a subdirectory for a hard-linked one. Neither
        // this host nor APFS can hard-link a directory, so the answer is 1.
        u32_(&mut out, u32::from(is_dir));
    }
    if dir & ATTR_DIR_ENTRYCOUNT != 0 {
        u32_(&mut out, if is_dir { entries() } else { 0 });
    }
    let alloc = st.blocks * 512;
    if file & ATTR_FILE_LINKCOUNT != 0 {
        u32_(&mut out, if is_dir { 0 } else { u32::from(st.nlink) });
    }
    if file & ATTR_FILE_TOTALSIZE != 0 {
        i64_(&mut out, if is_dir { 0 } else { st.size });
    }
    if file & ATTR_FILE_ALLOCSIZE != 0 {
        i64_(&mut out, if is_dir { 0 } else { alloc });
    }
    if file & ATTR_FILE_IOBLOCKSIZE != 0 {
        u32_(&mut out, if is_dir { 0 } else { st.blksize as u32 });
    }
    if file & ATTR_FILE_DATALENGTH != 0 {
        i64_(&mut out, if is_dir { 0 } else { st.size });
    }
    if file & ATTR_FILE_DATAALLOCSIZE != 0 {
        i64_(&mut out, if is_dir { 0 } else { alloc });
    }
    if let Some(at) = name_ref {
        let name = name.unwrap_or_default();
        let offset = (out.len() - at) as u32;
        out[at..at + 4].copy_from_slice(&offset.to_ne_bytes());
        out[at + 4..at + 8].copy_from_slice(&(name.len() as u32 + 1).to_ne_bytes());
        out.extend_from_slice(name);
        out.push(0);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    if name.is_some() {
        out.resize(out.len().next_multiple_of(8), 0);
    }
    let len = out.len() as u32;
    out[..4].copy_from_slice(&len.to_ne_bytes());
    out
}

/// Entries in the directory at host path `path`, not counting `.` and `..`.
fn count_entries(path: &CStr) -> u32 {
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()));
    std::fs::read_dir(path).map_or(0, |d| d.count() as u32)
}

/// Copy `answer` out, truncated to `size`, and report its length as Darwin
/// does: what was written, or the whole with FSOPT_REPORT_FULLSIZE.
///
/// # Safety
///
/// `buf` is writable for `size` bytes.
unsafe fn copy_out(mut answer: Vec<u8>, options: u64, buf: *mut c_void, size: usize) -> c_int {
    let n = answer.len().min(size);
    if options & FSOPT_REPORT_FULLSIZE == 0 && n >= 4 {
        answer[..4].copy_from_slice(&(n as u32).to_ne_bytes());
    }
    // SAFETY: `n <= size`, the caller's obligation.
    unsafe { std::ptr::copy_nonoverlapping(answer.as_ptr(), buf.cast::<u8>(), n) };
    0
}

/// `int getattrlist(const char *, struct attrlist *, void *, size_t, unsigned
/// int options)`. `options` is an `unsigned long` in the header the SDK
/// ships and an `unsigned int` in the man page; only its low bits mean anything.
///
/// # Safety
///
/// `path` is NUL-terminated, `list` is readable, and `buf` is writable for
/// `size` bytes.
pub unsafe extern "C" fn getattrlist(
    path: *const c_char,
    list: *const AttrList,
    buf: *mut c_void,
    size: usize,
    options: u64,
) -> c_int {
    let options = options & u64::from(u32::MAX);
    if path.is_null() || list.is_null() || buf.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let list = unsafe { list.read_unaligned() };
    if let Err(errno) = check(&list, options, false) {
        crate::errno::set(errno);
        return -1;
    }
    let found = crate::stat::with_fallback(path, |host| {
        // SAFETY: a zeroed `struct stat` for the host to fill, and the path
        // `with_fallback` translated.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            match options & FSOPT_NOFOLLOW != 0 {
                true => libc::lstat(host, &mut st),
                false => libc::stat(host, &mut st),
            }
        };
        // SAFETY: `host` is the NUL-terminated path just used.
        let answer = (rc == 0).then(|| {
            let st = crate::stat::from_host(&st);
            pack(&list, options, Ok(&st), None, || {
                count_entries(unsafe { CStr::from_ptr(host) })
            })
        });
        (answer, rc == 0)
    });
    match found {
        Some(answer) => unsafe { copy_out(answer, options, buf, size) },
        None => -1,
    }
}

/// `int fgetattrlist(int, struct attrlist *, void *, size_t, unsigned int)`
///
/// # Safety
///
/// As [`getattrlist`], with `fd` in place of the path.
pub unsafe extern "C" fn fgetattrlist(
    fd: c_int,
    list: *const AttrList,
    buf: *mut c_void,
    size: usize,
    options: u64,
) -> c_int {
    let options = options & u64::from(u32::MAX);
    if list.is_null() || buf.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let list = unsafe { list.read_unaligned() };
    if let Err(errno) = check(&list, options, false) {
        crate::errno::set(errno);
        return -1;
    }
    // SAFETY: a zeroed `struct stat` for the host to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return -1;
    }
    let st = crate::stat::from_host(&st);
    let answer = pack(&list, options, Ok(&st), None, || {
        let path = starnix::fd::path(fd).unwrap_or_default();
        count_entries(
            &std::ffi::CString::new(path.into_os_string().into_encoded_bytes()).unwrap_or_default(),
        )
    });
    unsafe { copy_out(answer, options, buf, size) }
}

/// A directory `getattrlistbulk` is part-way through, by the guest's descriptor.
///
/// Darwin keeps the place in the descriptor's own offset. A host offset cannot
/// hold the mount points a listing shows after the directory's own entries, so
/// the place is kept here instead, in a handle of its own that leaves the
/// guest's offset alone. Dropped by [`forget`] when the descriptor is closed.
struct Bulk {
    dir: *mut libc::DIR,
    /// The entry that did not fit in the last caller's buffer.
    pending: Option<crate::dirent::Dirent>,
}

// SAFETY: the handle is used by one caller at a time, out of the map.
unsafe impl Send for Bulk {}

static BULK: std::sync::Mutex<Option<std::collections::HashMap<c_int, Bulk>>> =
    std::sync::Mutex::new(None);

fn bulk<T>(with: impl FnOnce(&mut std::collections::HashMap<c_int, Bulk>) -> T) -> T {
    let mut guard = BULK.lock().unwrap_or_else(|e| e.into_inner());
    with(guard.get_or_insert_with(Default::default))
}

/// Drop the listing of `fd`, which is closed or about to be replaced.
pub(crate) fn forget(fd: c_int) {
    if let Some(gone) = bulk(|open| open.remove(&fd)) {
        // SAFETY: the handle this module opened, used by nothing else.
        unsafe { crate::dirent::closedir(gone.dir) };
    }
}

/// A fresh handle on the directory `fd` names, starting at its beginning.
fn open_bulk(fd: c_int) -> Result<Bulk, c_int> {
    // SAFETY: a relative path under the caller's descriptor.
    let own = unsafe {
        libc::openat(
            fd,
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if own < 0 {
        return Err(crate::errno::get());
    }
    // SAFETY: a descriptor this function owns, handed to the handle.
    let dir = unsafe { libc::fdopendir(own) };
    if dir.is_null() {
        let error = crate::errno::get();
        // SAFETY: still ours, since `fdopendir` failed.
        unsafe { libc::close(own) };
        return Err(error);
    }
    Ok(Bulk { dir, pending: None })
}

/// `int getattrlistbulk(int dirfd, struct attrlist *, void *attrBuf, size_t
/// attrBufSize, uint64_t options)`: as many of the directory's next entries as
/// fit, each laid out as [`getattrlist`] would and prefixed by its length.
/// Returns how many, and 0 at the end.
///
/// # Safety
///
/// `list` is readable and `buf` is writable for `size` bytes.
pub unsafe extern "C" fn getattrlistbulk(
    fd: c_int,
    list: *const AttrList,
    buf: *mut c_void,
    size: usize,
    options: u64,
) -> c_int {
    if list.is_null() || buf.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let list = unsafe { list.read_unaligned() };
    if let Err(errno) = check(&list, options, true) {
        crate::errno::set(errno);
        return -1;
    }
    // Out of the map while it is read, so one listing never waits on another's
    // `stat`s.
    let mut state = match bulk(|open| open.remove(&fd)) {
        Some(state) => state,
        None => match open_bulk(fd) {
            Ok(state) => state,
            Err(errno) => {
                crate::errno::set(errno);
                return -1;
            }
        },
    };
    let mut out = Vec::new();
    let mut count = 0;
    let mut failed = None;
    loop {
        let entry = match state.pending.take() {
            Some(entry) => entry,
            // SAFETY: the handle `open_bulk` made.
            None => match unsafe { crate::dirent::next_listed(state.dir) } {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(errno) => {
                    failed = Some(errno);
                    break;
                }
            },
        };
        // SAFETY: NUL-terminated within the entry.
        let name = unsafe { CStr::from_ptr(entry.name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let mut st = Stat::default();
        // Never followed: an entry is the link itself, as `readdir` reports it.
        // SAFETY: the caller's directory and a NUL-terminated name under it.
        let rc = unsafe { crate::stat::fstatat(fd, name.as_ptr(), &mut st, 0x20) };
        let found = match rc {
            0 => Ok(&st),
            _ => Err(crate::errno::from_host(crate::errno::get())),
        };
        let packed = pack(&list, options, found, Some(name.to_bytes()), || {
            // SAFETY: as for the `stat` above.
            let at = unsafe { libc::openat(fd, name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) };
            let n = match at < 0 {
                true => 0,
                false => count_entries(
                    &std::ffi::CString::new(
                        starnix::fd::path(at)
                            .unwrap_or_default()
                            .into_os_string()
                            .into_encoded_bytes(),
                    )
                    .unwrap_or_default(),
                ),
            };
            if at >= 0 {
                // SAFETY: opened just above.
                unsafe { libc::close(at) };
            }
            n
        });
        if out.len() + packed.len() > size {
            state.pending = Some(entry);
            if count == 0 {
                failed = Some(libc::ERANGE);
            }
            break;
        }
        out.extend_from_slice(&packed);
        count += 1;
    }
    bulk(|open| open.insert(fd, state));
    if let Some(errno) = failed.filter(|_| count == 0) {
        crate::errno::set(errno);
        return -1;
    }
    // SAFETY: `out` is at most `size` bytes, the caller's obligation.
    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), buf.cast::<u8>(), out.len()) };
    count
}

// What `setattrlist` reads, beyond the attributes `getattrlist` answers.
const ATTR_CMN_SCRIPT: u32 = 0x0000_0100;
const ATTR_CMN_CRTIME: u32 = 0x0000_0200;
const ATTR_CMN_BKUPTIME: u32 = 0x0000_2000;
const ATTR_CMN_FNDRINFO: u32 = 0x0000_4000;
const ATTR_CMN_EXTENDED_SECURITY: u32 = 0x0040_0000;
const ATTR_CMN_UUID: u32 = 0x0080_0000;
const ATTR_CMN_GRPUUID: u32 = 0x0100_0000;
const ATTR_CMN_ADDEDTIME: u32 = 0x1000_0000;
const ATTR_CMN_DATA_PROTECT_FLAGS: u32 = 0x4000_0000;
const ATTR_VOL_NAME: u32 = 0x0000_2000;
const ATTR_VOL_INFO: u32 = 0x8000_0000;
const ATTR_FILE_DEVTYPE: u32 = 0x0000_0020;

// `sys/attr.h`'s `ATTR_*_SETMASK`: what may be set at all. Anything outside is
// `EINVAL`, before anything is read.
const ATTR_CMN_SETMASK: u32 = 0x51C7_FF00;
const ATTR_CMN_VOLSETMASK: u32 = 0x0000_6700;
const ATTR_VOL_SETMASK: u32 = 0x8000_2000;
const ATTR_FILE_SETMASK: u32 = 0x0000_0020;

/// `ATTR_MAX_BUFFER`: a larger buffer is `ENOMEM`, as xnu will not copy it in.
const ATTR_MAX_BUFFER: usize = 8192;

/// The name Darwin keeps a file's Finder info under.
const FINDER_INFO: &CStr = c"com.apple.FinderInfo";

/// What a `setattrlist` buffer asks to change, read in the order xnu's
/// `setattrlist_internal` reads it.
///
/// Only what this host can keep is kept. The rest - a creation, backup or
/// added time, a text encoding, file flags, a protection class - is read past
/// and dropped, which is what xnu does on a volume that does not support one:
/// `vnode_setattr` succeeds with the attribute left unsupported.
#[derive(Default)]
struct Changes {
    modified: Option<libc::timespec>,
    accessed: Option<libc::timespec>,
    finder_info: Option<[u8; 32]>,
    owner: Option<u32>,
    group: Option<u32>,
    mode: Option<u32>,
}

/// The fixed-size values at the front of `buf`, one after another with no
/// padding, as `attrlist_unpack_fixed` takes them.
struct Unpack<'a>(&'a [u8]);

impl Unpack<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], c_int> {
        let (head, rest) = self.0.split_first_chunk::<N>().ok_or(libc::EINVAL)?;
        self.0 = rest;
        Ok(*head)
    }
    fn u32(&mut self) -> Result<u32, c_int> {
        self.take::<4>().map(u32::from_ne_bytes)
    }
    /// A `struct user64_timespec`: two 64-bit words.
    fn time(&mut self) -> Result<libc::timespec, c_int> {
        let sec = i64::from_ne_bytes(self.take::<8>()?);
        let nsec = i64::from_ne_bytes(self.take::<8>()?);
        Ok(libc::timespec {
            tv_sec: sec as libc::time_t,
            tv_nsec: nsec as _,
        })
    }
    /// A time that is set rather than read past. The host takes two
    /// out-of-range nanosecond values as `UTIME_NOW` and `UTIME_OMIT`, so
    /// every one outside a second is refused before it can reach it.
    fn kept_time(&mut self) -> Result<libc::timespec, c_int> {
        let time = self.time()?;
        match (0..1_000_000_000).contains(&time.tv_nsec) {
            true => Ok(time),
            false => Err(libc::EINVAL),
        }
    }
}

/// Whether `list` asks for anything, or why it may not ask for what it does -
/// the checks `setattrlist_internal` makes before it reads the buffer.
fn validate(list: &AttrList) -> Result<bool, c_int> {
    if list.bitmapcount != ATTR_BIT_MAP_COUNT {
        return Err(libc::EINVAL);
    }
    let invalid = match list.volattr != 0 {
        true => {
            list.volattr & !ATTR_VOL_SETMASK != 0
                || list.commonattr & !ATTR_CMN_VOLSETMASK != 0
                || list.fileattr != 0
                || list.forkattr != 0
        }
        false => {
            list.commonattr & !ATTR_CMN_SETMASK != 0
                || list.fileattr & !ATTR_FILE_SETMASK != 0
                || list.dirattr != 0
                || list.forkattr != 0
        }
    };
    if invalid {
        return Err(libc::EINVAL);
    }
    Ok(list.commonattr != 0
        || list.volattr & !ATTR_VOL_INFO != 0
        || list.dirattr != 0
        || list.fileattr != 0
        || list.forkattr != 0)
}

/// Read what `buf` holds for the attributes a [`validate`]d `list` names.
fn changes(list: &AttrList, buf: &[u8]) -> Result<Changes, c_int> {
    let common = list.commonattr;
    let mut at = Unpack(buf);
    let mut out = Changes::default();
    if common & ATTR_CMN_SCRIPT != 0 {
        at.u32()?;
    }
    if common & ATTR_CMN_CRTIME != 0 {
        at.time()?;
    }
    if common & ATTR_CMN_MODTIME != 0 {
        out.modified = Some(at.kept_time()?);
    }
    // Read and ignored by xnu too: the change time is the kernel's to keep.
    if common & ATTR_CMN_CHGTIME != 0 {
        at.time()?;
    }
    if common & ATTR_CMN_ACCTIME != 0 {
        out.accessed = Some(at.kept_time()?);
    }
    if common & ATTR_CMN_BKUPTIME != 0 {
        at.time()?;
    }
    if common & ATTR_CMN_FNDRINFO != 0 {
        out.finder_info = Some(at.take::<32>()?);
    }
    if common & ATTR_CMN_OWNERID != 0 {
        out.owner = Some(at.u32()?);
    }
    if common & ATTR_CMN_GRPID != 0 {
        out.group = Some(at.u32()?);
    }
    if common & ATTR_CMN_ACCESSMASK != 0 {
        out.mode = Some(at.u32()?);
    }
    if common & ATTR_CMN_FLAGS != 0 {
        at.u32()?;
    }
    // An ACL, or the owner and group UUIDs: extended security, which a volume
    // without it refuses (`vnode_setattr`'s `ENOTSUP`) and this host has no
    // place for.
    if common & (ATTR_CMN_EXTENDED_SECURITY | ATTR_CMN_UUID | ATTR_CMN_GRPUUID) != 0 {
        return Err(libc::ENOTSUP);
    }
    if common & ATTR_CMN_ADDEDTIME != 0 {
        at.time()?;
    }
    if common & ATTR_CMN_DATA_PROTECT_FLAGS != 0 {
        at.u32()?;
    }
    // Renaming the volume, or an HFS volume's boot information (Finder info
    // with `ATTR_VOL_INFO`), are what a volume that cannot do them refuses.
    if list.volattr & ATTR_VOL_NAME != 0
        || (list.volattr & ATTR_VOL_INFO != 0 && out.finder_info.is_some())
    {
        return Err(libc::EINVAL);
    }
    if list.fileattr & ATTR_FILE_DEVTYPE != 0 {
        return Err(libc::EINVAL);
    }
    Ok(out)
}

/// Whether host path `path` is itself a symlink.
#[cfg(target_os = "linux")]
fn is_symlink(path: *const c_char) -> bool {
    // SAFETY: a zeroed `struct stat` for the host to fill, and the caller's path.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    unsafe { libc::lstat(path, &mut st) == 0 && st.st_mode & libc::S_IFMT == libc::S_IFLNK }
}

/// Whether `st` is the root of a volume: the guest's `/`, the one mount
/// `getmntinfo` reports.
fn is_volume_root(st: &Stat) -> bool {
    let mut root = Stat::default();
    // SAFETY: a NUL-terminated path and a live local.
    let rc = unsafe { crate::stat::stat(c"/".as_ptr(), &mut root) };
    rc == 0 && (root.dev, root.ino) == (st.dev, st.ino)
}

/// The file a `setattrlist` changes.
#[derive(Clone, Copy)]
enum Target {
    /// The guest's path, and whether a final symlink is followed.
    Path(*const c_char, bool),
    Fd(c_int),
}

impl Target {
    /// The file as `stat` shows it to the guest, or `None` with `errno` set.
    fn stat(self) -> Option<Stat> {
        let mut st = Stat::default();
        // SAFETY: the guest's own path or descriptor, and a live local.
        let rc = unsafe {
            match self {
                Target::Path(path, true) => crate::stat::stat(path, &mut st),
                Target::Path(path, false) => crate::stat::lstat(path, &mut st),
                Target::Fd(fd) => crate::stat::fstat(fd, &mut st),
            }
        };
        (rc == 0).then_some(st)
    }

    /// Finder info, as `setattrlist_setfinderinfo` writes it: an extended
    /// attribute, which thirty-two zero bytes remove.
    fn set_finder_info(self, info: &[u8; 32]) -> c_int {
        let value = (info != &[0; 32]).then_some(&info[..]);
        // Nothing to remove is the state asked for.
        let removed_nothing =
            |rc: c_int| rc != 0 && value.is_none() && crate::xattr::is_absent(crate::errno::get());
        match self {
            Target::Path(path, follow) => crate::stat::with_fallback(path, |host| {
                if !crate::stat::may_write(host) {
                    return (-1, false);
                }
                // SAFETY: the path `with_fallback` translated, and a name this
                // module owns.
                let rc = unsafe {
                    crate::xattr::set_on_host(CStr::from_ptr(host), FINDER_INFO, value, !follow)
                };
                let rc = if removed_nothing(rc) { 0 } else { rc };
                (rc, rc == 0)
            }),
            Target::Fd(fd) => {
                // SAFETY: the caller's descriptor, and a name this module owns.
                let rc = unsafe { crate::xattr::set_on_fd(fd, FINDER_INFO, value) };
                if removed_nothing(rc) { 0 } else { rc }
            }
        }
    }

    fn set_owner(self, owner: Option<u32>, group: Option<u32>) -> c_int {
        let (uid, gid) = (owner.unwrap_or(u32::MAX), group.unwrap_or(u32::MAX));
        // SAFETY: the guest's own path or descriptor, as `chown` takes them.
        unsafe {
            match self {
                Target::Path(path, true) => crate::fs::chown(path, uid, gid),
                Target::Path(path, false) => crate::fs::lchown(path, uid, gid),
                Target::Fd(fd) => crate::fs::fchown(fd, uid, gid),
            }
        }
    }

    fn set_mode(self, mode: u32) -> c_int {
        let mode = (mode & 0o7777) as libc::mode_t;
        match self {
            // SAFETY: the guest's path, as `chmod` takes it.
            Target::Path(path, true) => unsafe { crate::fs::chmod(path, mode) },
            Target::Path(path, false) => crate::stat::with_fallback(path, |host| {
                if !crate::stat::may_write(host) {
                    return (-1, false);
                }
                // SAFETY: the path `with_fallback` translated.
                let rc = unsafe {
                    libc::fchmodat(libc::AT_FDCWD, host, mode, libc::AT_SYMLINK_NOFOLLOW)
                };
                // FIXME (ACCURACY): Linux keeps no mode on a symlink and never
                // consults one, so it refuses to set it. Darwin's `lchmod`
                // succeeds; the link is reported as done, its mode still 0777.
                #[cfg(target_os = "linux")]
                if rc != 0 && crate::errno::get() == libc::EOPNOTSUPP && is_symlink(host) {
                    return (0, true);
                }
                (rc, rc == 0)
            }),
            // SAFETY: the caller's descriptor.
            Target::Fd(fd) => unsafe { libc::fchmod(fd, mode) },
        }
    }

    /// The modification and access times, each left alone when not given.
    fn set_times(
        self,
        modified: Option<libc::timespec>,
        accessed: Option<libc::timespec>,
    ) -> c_int {
        let omit = libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        };
        let times = [accessed.unwrap_or(omit), modified.unwrap_or(omit)];
        match self {
            Target::Path(path, follow) => crate::stat::with_fallback(path, |host| {
                if !crate::stat::may_write(host) {
                    return (-1, false);
                }
                let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
                // SAFETY: the path `with_fallback` translated, and two times.
                let rc = unsafe { libc::utimensat(libc::AT_FDCWD, host, times.as_ptr(), flags) };
                (rc, rc == 0)
            }),
            // SAFETY: the caller's descriptor, and two times.
            Target::Fd(fd) => unsafe { libc::futimens(fd, times.as_ptr()) },
        }
    }

    /// Make the changes, in xnu's order: Finder info first when the new mode
    /// takes away the owner's write permission, so that it can still be
    /// written, and last otherwise, so that a mode that grants it comes first.
    ///
    /// xnu makes them in one `vnode_setattr`; here each is its own call, so
    /// one that fails puts back the owner, mode and times already changed,
    /// as `before` had them, and the guest's -1 means nothing changed.
    fn apply(self, changes: Changes, before: &Stat) -> c_int {
        let mut done = Done::default();
        if self.try_apply(&changes, &mut done) {
            return 0;
        }
        self.undo(done, before);
        -1
    }

    fn try_apply(self, changes: &Changes, done: &mut Done) -> bool {
        let finder_first = changes.mode.is_some_and(|mode| mode & 0o200 == 0);
        if finder_first
            && let Some(info) = &changes.finder_info
            && self.set_finder_info(info) != 0
        {
            return false;
        }
        if changes.owner.is_some() || changes.group.is_some() {
            if self.set_owner(changes.owner, changes.group) != 0 {
                return false;
            }
            done.owner = true;
        }
        if let Some(mode) = changes.mode {
            if self.set_mode(mode) != 0 {
                return false;
            }
            done.mode = true;
        }
        if changes.modified.is_some() || changes.accessed.is_some() {
            if self.set_times(changes.modified, changes.accessed) != 0 {
                return false;
            }
            done.times = true;
        }
        // FIXME (ACCURACY): Finder info written first is not put back if a
        // later change fails; nothing after it is expected to.
        if !finder_first
            && let Some(info) = &changes.finder_info
            && self.set_finder_info(info) != 0
        {
            return false;
        }
        true
    }

    /// Put back what `done` says was changed, latest first, leaving `errno`
    /// as the failure that made it necessary.
    fn undo(self, done: Done, before: &Stat) {
        let _errno = crate::errno::Held::now();
        let time = |t: crate::stat::Timespec| libc::timespec {
            tv_sec: t.sec as libc::time_t,
            tv_nsec: t.nsec as _,
        };
        if done.times {
            self.set_times(Some(time(before.mtime)), Some(time(before.atime)));
        }
        if done.mode {
            self.set_mode(u32::from(before.mode));
        }
        if done.owner {
            self.set_owner(Some(before.uid), Some(before.gid));
        }
    }
}

/// Which of [`Target::apply`]'s changes have been made.
#[derive(Default)]
struct Done {
    owner: bool,
    mode: bool,
    times: bool,
}

/// The shared body of [`setattrlist`] and [`fsetattrlist`].
///
/// # Safety
///
/// `list` is readable and `buf` is readable for `size` bytes.
unsafe fn set(target: Target, list: *const AttrList, buf: *const c_void, size: usize) -> c_int {
    if list.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let list = unsafe { list.read_unaligned() };
    let fail = |errno| {
        crate::errno::set(errno);
        -1
    };
    match validate(&list) {
        Err(errno) => return fail(errno),
        Ok(false) => return 0,
        Ok(true) => {}
    }
    if size > ATTR_MAX_BUFFER {
        return fail(libc::ENOMEM);
    }
    if buf.is_null() && size != 0 {
        return fail(libc::EFAULT);
    }
    let bytes = match size {
        0 => &[][..],
        // SAFETY: the caller's buffer of `size` bytes.
        _ => unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), size) },
    };
    let changes = match changes(&list, bytes) {
        Ok(changes) => changes,
        Err(errno) => return fail(errno),
    };
    // What a failed change is put back to, and the lookup that fails before
    // anything is changed.
    let Some(before) = target.stat() else {
        return -1;
    };
    // Volume attributes, the common ones among them, are the volume's, and
    // xnu refuses them on anything but its root.
    if list.volattr != 0 && !is_volume_root(&before) {
        return fail(libc::EINVAL);
    }
    target.apply(changes, &before)
}

/// `int setattrlist(const char *path, struct attrlist *attrList, void
/// *attrBuf, size_t attrBufSize, unsigned long options)`
///
/// xnu's: the attributes `attrList` names, read from `attrBuf` in their
/// canonical order and set on the file at `path` - the link itself with
/// `FSOPT_NOFOLLOW`.
///
/// # Safety
///
/// `path` is NUL-terminated, `list` is readable, and `buf` is readable for
/// `size` bytes.
pub unsafe extern "C" fn setattrlist(
    path: *const c_char,
    list: *const AttrList,
    buf: *const c_void,
    size: usize,
    options: u64,
) -> c_int {
    if path.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let follow = options & FSOPT_NOFOLLOW == 0;
    // SAFETY: the caller's obligations, passed on.
    unsafe { set(Target::Path(path, follow), list, buf, size) }
}

/// `int fsetattrlist(int fd, struct attrlist *attrList, void *attrBuf, size_t
/// attrBufSize, unsigned long options)`
///
/// # Safety
///
/// As [`setattrlist`], with `fd` in place of the path.
pub unsafe extern "C" fn fsetattrlist(
    fd: c_int,
    list: *const AttrList,
    buf: *const c_void,
    size: usize,
    _options: u64,
) -> c_int {
    // SAFETY: the caller's obligations, passed on.
    unsafe { set(Target::Fd(fd), list, buf, size) }
}

exports::exports! {
    functions {
        "getattrlist" => getattrlist,
        "fgetattrlist" => fgetattrlist,
        "getattrlistbulk" => getattrlistbulk,
        "setattrlist" => setattrlist,
        "fsetattrlist" => fsetattrlist,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(common: u32, dir: u32, file: u32) -> AttrList {
        AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: common,
            volattr: 0,
            dirattr: dir,
            fileattr: file,
            forkattr: 0,
        }
    }

    fn ask(path: &CStr, l: AttrList, options: u64) -> Result<Vec<u8>, c_int> {
        let mut buf = [0u8; 256];
        // SAFETY: live locals of the sizes passed.
        let rc = unsafe {
            getattrlist(
                path.as_ptr(),
                &l,
                buf.as_mut_ptr().cast(),
                buf.len(),
                options,
            )
        };
        if rc != 0 {
            return Err(crate::errno::get());
        }
        let len = u32::from_ne_bytes(buf[..4].try_into().unwrap()) as usize;
        Ok(buf[..len].to_vec())
    }

    fn word(b: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes(b[at..at + 4].try_into().unwrap())
    }

    /// Two `user64_timespec`s, as a `setattrlist` buffer holds them.
    fn times(times: &[(i64, i64)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (sec, nsec) in times {
            out.extend_from_slice(&sec.to_ne_bytes());
            out.extend_from_slice(&nsec.to_ne_bytes());
        }
        out
    }

    fn scratch_file(name: &str) -> (std::path::PathBuf, std::ffi::CString) {
        let file = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let path = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
        (file, path)
    }

    /// The attributes are read in xnu's order - modification time, then
    /// access time, then the mode - and each lands where `stat` sees it.
    #[test]
    fn setattrlist_sets_the_times_and_the_mode() {
        use std::os::unix::fs::MetadataExt;
        let (file, path) = scratch_file("setattrlist");
        let mut buf = times(&[(1_000_000_000, 5), (1_100_000_000, 7)]);
        buf.extend_from_slice(&0o600u32.to_ne_bytes());
        let l = list(
            ATTR_CMN_MODTIME | ATTR_CMN_ACCTIME | ATTR_CMN_ACCESSMASK,
            0,
            0,
        );
        // SAFETY: live locals of the sizes passed.
        let rc = unsafe { setattrlist(path.as_ptr(), &l, buf.as_ptr().cast(), buf.len(), 0) };
        let meta = std::fs::metadata(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(rc, 0, "errno {}", crate::errno::get());
        assert_eq!((meta.mtime(), meta.mtime_nsec()), (1_000_000_000, 5));
        assert_eq!((meta.atime(), meta.atime_nsec()), (1_100_000_000, 7));
        assert_eq!(meta.mode() & 0o7777, 0o600);
    }

    /// With FSOPT_NOFOLLOW a symlink's mode is the link's, and setting it
    /// leaves the target alone and does not stop the times that follow it.
    #[test]
    fn setattrlist_sets_a_symlinks_mode_without_following_it() {
        use std::os::unix::fs::MetadataExt;
        let (file, _) = scratch_file("setattrlist-link-target");
        std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .unwrap();
        let link = file.with_extension("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let path = std::ffi::CString::new(link.to_str().unwrap()).unwrap();
        let mut buf = times(&[(1_000_000_000, 0)]);
        buf.extend_from_slice(&0o600u32.to_ne_bytes());
        let l = list(ATTR_CMN_MODTIME | ATTR_CMN_ACCESSMASK, 0, 0);
        // SAFETY: live locals of the sizes passed.
        let rc = unsafe {
            setattrlist(
                path.as_ptr(),
                &l,
                buf.as_ptr().cast(),
                buf.len(),
                FSOPT_NOFOLLOW,
            )
        };
        let errno = crate::errno::get();
        let link_meta = std::fs::symlink_metadata(&link).unwrap();
        let target_meta = std::fs::metadata(&file).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(rc, 0, "errno {errno}");
        assert_eq!(target_meta.mode() & 0o7777, 0o644);
        assert_eq!(link_meta.mtime(), 1_000_000_000);
    }

    #[test]
    fn fsetattrlist_sets_a_time_through_a_descriptor() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let (file, _) = scratch_file("fsetattrlist");
        let handle = std::fs::File::open(&file).unwrap();
        let buf = times(&[(1_234_567_890, 0)]);
        let l = list(ATTR_CMN_MODTIME, 0, 0);
        // SAFETY: an open descriptor and live locals of the sizes passed.
        let rc = unsafe { fsetattrlist(handle.as_raw_fd(), &l, buf.as_ptr().cast(), buf.len(), 0) };
        let meta = std::fs::metadata(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(rc, 0);
        assert_eq!(meta.mtime(), 1_234_567_890);
    }

    /// Finder info is the `com.apple.FinderInfo` attribute, as on Darwin, and
    /// thirty-two zero bytes take it away again.
    #[test]
    #[cfg(target_os = "linux")]
    fn setattrlist_writes_finder_info_as_an_attribute() {
        let (file, path) = scratch_file("finderinfo");
        let mut info = [0u8; 32];
        info[..8].copy_from_slice(b"TEXTttxt");
        let l = list(ATTR_CMN_FNDRINFO, 0, 0);
        let set = |info: &[u8; 32]| {
            // SAFETY: live locals of the sizes passed.
            unsafe { setattrlist(path.as_ptr(), &l, info.as_ptr().cast(), 32, 0) }
        };
        if set(&info) != 0 {
            let errno = crate::errno::get();
            std::fs::remove_file(&file).unwrap();
            assert_eq!(
                errno,
                libc::EOPNOTSUPP,
                "only a filesystem without attributes refuses"
            );
            return;
        }
        let mut read = [0u8; 32];
        // SAFETY: the host's own call, on a path and buffer of this test's.
        let got = unsafe {
            libc::getxattr(
                path.as_ptr(),
                c"user.com.apple.FinderInfo".as_ptr(),
                read.as_mut_ptr().cast(),
                read.len(),
            )
        };
        let cleared = set(&[0; 32]);
        // SAFETY: as above, asking only for the length.
        let after = unsafe {
            libc::getxattr(
                path.as_ptr(),
                c"user.com.apple.FinderInfo".as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        };
        std::fs::remove_file(&file).unwrap();
        assert_eq!((got, read), (32, info));
        assert_eq!((cleared, after), (0, -1));
    }

    /// A change that fails puts back the ones made before it: a link's new
    /// time is undone when the Finder info after it cannot be written, which
    /// Linux refuses on a symlink.
    #[test]
    #[cfg(target_os = "linux")]
    fn setattrlist_puts_back_what_it_changed_when_a_later_change_fails() {
        use std::os::unix::fs::MetadataExt;
        let (file, _) = scratch_file("setattrlist-undo-target");
        let link = file.with_extension("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let before = std::fs::symlink_metadata(&link).unwrap();
        let path = std::ffi::CString::new(link.to_str().unwrap()).unwrap();
        let mut buf = times(&[(1_000_000_000, 0)]);
        buf.extend_from_slice(b"TEXTttxt");
        buf.extend_from_slice(&[0; 24]);
        let l = list(ATTR_CMN_MODTIME | ATTR_CMN_FNDRINFO, 0, 0);
        // SAFETY: live locals of the sizes passed.
        let rc = unsafe {
            setattrlist(
                path.as_ptr(),
                &l,
                buf.as_ptr().cast(),
                buf.len(),
                FSOPT_NOFOLLOW,
            )
        };
        let errno = crate::errno::get();
        let after = std::fs::symlink_metadata(&link).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(rc, -1, "Linux keeps no user attribute on a symlink");
        assert_eq!(errno, libc::EPERM);
        assert_eq!(
            (after.mtime(), after.mtime_nsec()),
            (before.mtime(), before.mtime_nsec())
        );
    }

    /// Volume attributes, common ones included, are refused on a file that
    /// is not the volume's root rather than applied to it.
    #[test]
    fn setattrlist_refuses_volume_attributes_below_the_root() {
        use std::os::unix::fs::MetadataExt;
        let (file, path) = scratch_file("setattrlist-volume");
        let before = std::fs::metadata(&file).unwrap().mtime();
        let buf = times(&[(1_000_000_000, 0)]);
        let mut l = list(ATTR_CMN_MODTIME, 0, 0);
        l.volattr = ATTR_VOL_INFO;
        // SAFETY: live locals of the sizes passed.
        let rc = unsafe { setattrlist(path.as_ptr(), &l, buf.as_ptr().cast(), buf.len(), 0) };
        let errno = crate::errno::get();
        let after = std::fs::metadata(&file).unwrap().mtime();
        std::fs::remove_file(&file).unwrap();
        assert_eq!((rc, errno), (-1, libc::EINVAL));
        assert_eq!(after, before);
    }

    /// Through a descriptor the Finder info lands on the open file, even once
    /// the name it was opened by is gone.
    #[test]
    #[cfg(target_os = "linux")]
    fn fsetattrlist_writes_finder_info_on_a_renamed_file() {
        use std::os::fd::AsRawFd;
        let (file, _) = scratch_file("fsetattrlist-finderinfo");
        let moved = file.with_extension("moved");
        let handle = std::fs::File::open(&file).unwrap();
        std::fs::rename(&file, &moved).unwrap();
        let mut info = [0u8; 32];
        info[..8].copy_from_slice(b"TEXTttxt");
        let l = list(ATTR_CMN_FNDRINFO, 0, 0);
        let set = |info: &[u8; 32]| {
            // SAFETY: an open descriptor and live locals of the sizes passed.
            unsafe { fsetattrlist(handle.as_raw_fd(), &l, info.as_ptr().cast(), 32, 0) }
        };
        if set(&info) != 0 {
            let errno = crate::errno::get();
            std::fs::remove_file(&moved).unwrap();
            assert_eq!(
                errno,
                libc::EOPNOTSUPP,
                "only a filesystem without attributes refuses"
            );
            return;
        }
        let path = std::ffi::CString::new(moved.to_str().unwrap()).unwrap();
        let mut read = [0u8; 32];
        // SAFETY: the host's own call, on a path and buffer of this test's.
        let got = unsafe {
            libc::getxattr(
                path.as_ptr(),
                c"user.com.apple.FinderInfo".as_ptr(),
                read.as_mut_ptr().cast(),
                read.len(),
            )
        };
        let cleared = set(&[0; 32]);
        let cleared_again = set(&[0; 32]);
        std::fs::remove_file(&moved).unwrap();
        assert_eq!((got, read), (32, info));
        assert_eq!((cleared, cleared_again), (0, 0));
    }

    /// xnu's checks, in xnu's order: an attribute that cannot be set is
    /// `EINVAL` before anything else, a request for nothing succeeds whatever
    /// the buffer, an oversized buffer is `ENOMEM`, and a short one `EINVAL`.
    #[test]
    fn setattrlist_refuses_what_xnu_refuses() {
        let (file, path) = scratch_file("setattrlist-refusals");
        let call = |l: AttrList, buf: &[u8], size: usize| {
            // SAFETY: `size` is the buffer's length, except where the test
            // means the size to be refused before the buffer is read.
            let rc = unsafe { setattrlist(path.as_ptr(), &l, buf.as_ptr().cast(), size, 0) };
            match rc {
                0 => Ok(()),
                _ => Err(crate::errno::get()),
            }
        };
        let results = [
            call(list(ATTR_CMN_NAME, 0, 0), &[], 0),
            call(list(0, ATTR_DIR_LINKCOUNT, 0), &[], 0),
            call(list(0, 0, 0), &[], 0),
            call(list(ATTR_CMN_MODTIME, 0, 0), &[], ATTR_MAX_BUFFER + 1),
            call(list(ATTR_CMN_MODTIME, 0, 0), &[0; 8], 8),
            call(list(ATTR_CMN_UUID, 0, 0), &[0; 16], 16),
            call(list(ATTR_CMN_CRTIME | ATTR_CMN_FLAGS, 0, 0), &[0; 20], 20),
            // Linux's UTIME_OMIT, which must not be taken as "leave it".
            call(
                list(ATTR_CMN_MODTIME, 0, 0),
                &times(&[(5, (1 << 30) - 2)]),
                16,
            ),
            call(list(ATTR_CMN_ACCTIME, 0, 0), &times(&[(5, -1)]), 16),
        ];
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            results,
            [
                Err(libc::EINVAL),
                Err(libc::EINVAL),
                Ok(()),
                Err(libc::ENOMEM),
                Err(libc::EINVAL),
                Err(libc::ENOTSUP),
                // Kept nowhere here, as on a volume that has neither.
                Ok(()),
                Err(libc::EINVAL),
                Err(libc::EINVAL),
            ]
        );
    }

    /// `du`'s question: a directory is never reported as hard-linked, however
    /// many subdirectories give it a host link count above two.
    #[test]
    fn a_directory_has_one_link() {
        let dir = std::env::temp_dir().join(format!("attrlist-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let path = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        let b = ask(
            &path,
            list(0, ATTR_DIR_LINKCOUNT | ATTR_DIR_ENTRYCOUNT, 0),
            0,
        )
        .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(b.len(), 12);
        assert_eq!((word(&b, 4), word(&b, 8)), (1, 1));
    }

    #[test]
    fn attributes_are_packed_in_bit_order() {
        let mut st = Stat::default();
        st.mode = libc::S_IFREG as u16 | 0o644;
        st.nlink = 3;
        st.uid = 501;
        st.size = 1000;
        st.blocks = 8;
        let l = list(
            ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_OWNERID | ATTR_CMN_OBJTYPE,
            ATTR_DIR_LINKCOUNT,
            ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE,
        );
        let b = pack(&l, 0, Ok(&st), None, || unreachable!());
        // Length, returned set, OBJTYPE, OWNERID, FILE_LINKCOUNT, ALLOCSIZE;
        // the directory attribute is left out for a file.
        assert_eq!(b.len(), 4 + 20 + 4 + 4 + 4 + 8);
        assert_eq!(word(&b, 4), ATTR_CMN_OWNERID | ATTR_CMN_OBJTYPE);
        assert_eq!(word(&b, 12), 0);
        assert_eq!(word(&b, 16), ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE);
        assert_eq!((word(&b, 24), word(&b, 28), word(&b, 32)), (1, 501, 3));
        assert_eq!(i64::from_ne_bytes(b[36..44].try_into().unwrap()), 4096);
    }

    /// What Unity's player asks for when it scans its data directory.
    fn unity() -> AttrList {
        list(
            ATTR_CMN_RETURNED_ATTRS
                | ATTR_CMN_ERROR
                | ATTR_CMN_FLAGS
                | ATTR_CMN_MODTIME
                | ATTR_CMN_OBJTYPE
                | ATTR_CMN_NAME,
            0,
            ATTR_FILE_TOTALSIZE,
        )
    }

    /// Every entry `fd` lists, as (name, object type, size), reading `size`
    /// bytes at a time.
    fn listing(fd: c_int, size: usize) -> Vec<(String, u32, i64)> {
        let mut found = Vec::new();
        let mut buf = vec![0u8; size];
        loop {
            // SAFETY: live locals of the sizes passed.
            let n = unsafe { getattrlistbulk(fd, &unity(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            assert!(n >= 0, "errno {}", crate::errno::get());
            if n == 0 {
                return found;
            }
            let mut at = 0;
            for _ in 0..n {
                let e = &buf[at..];
                let len = word(e, 0) as usize;
                assert_eq!(len % 8, 0, "entries stay 8-byte aligned");
                assert_eq!(word(e, 4) & ATTR_CMN_ERROR, 0);
                let name_at = 0x18 + word(e, 0x18) as usize;
                let name_len = word(e, 0x1c) as usize;
                let name = String::from_utf8(e[name_at..name_at + name_len - 1].to_vec()).unwrap();
                let kind = word(e, 0x20);
                let total = match word(e, 0x10) & ATTR_FILE_TOTALSIZE {
                    0 => -1,
                    _ => i64::from_ne_bytes(e[0x38..0x40].try_into().unwrap()),
                };
                found.push((name, kind, total));
                at += len;
            }
        }
    }

    fn scratch(what: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("attrlist-{what}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("five"), b"12345").unwrap();
        dir
    }

    fn open_dir(dir: &std::path::Path) -> c_int {
        let path = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        // SAFETY: a NUL-terminated path.
        unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY) }
    }

    #[test]
    fn a_bulk_listing_names_every_entry_once() {
        let dir = scratch("bulk");
        let fd = open_dir(&dir);
        let mut found = listing(fd, 4096);
        found.sort();
        assert_eq!(
            found,
            [("five".to_string(), 1, 5), ("sub".to_string(), 2, -1)]
        );
        // At the end it stays at the end.
        assert!(listing(fd, 4096).is_empty());
        // SAFETY: the descriptor opened above.
        unsafe { crate::net::descriptor::close(fd) };
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bulk_listing_resumes_where_the_buffer_ran_out() {
        let dir = scratch("resume");
        let fd = open_dir(&dir);
        // Room for one entry a call, so the second waits for the next call.
        let mut found = listing(fd, 72);
        found.sort();
        assert_eq!(found.len(), 2, "{found:?}");
        let mut tiny = [0u8; 8];
        // SAFETY: the descriptor above and a live local.
        let n = unsafe { getattrlistbulk(fd, &unity(), tiny.as_mut_ptr().cast(), 8, 0) };
        assert_eq!(n, 0, "the end, not a buffer too small for nothing");
        unsafe { crate::net::descriptor::close(fd) };

        // A closed descriptor's place is forgotten: the next one starts over.
        let fd = open_dir(&dir);
        // SAFETY: as above.
        let n = unsafe { getattrlistbulk(fd, &unity(), tiny.as_mut_ptr().cast(), 8, 0) };
        assert_eq!((n, crate::errno::get()), (-1, libc::ERANGE));
        assert_eq!(listing(fd, 4096).len(), 2);
        unsafe { crate::net::descriptor::close(fd) };
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bulk_listing_needs_the_returned_set() {
        let mut l = unity();
        l.commonattr &= !ATTR_CMN_RETURNED_ATTRS;
        let mut buf = [0u8; 64];
        // SAFETY: a live local; the descriptor is never reached.
        let n = unsafe { getattrlistbulk(0, &l, buf.as_mut_ptr().cast(), buf.len(), 0) };
        assert_eq!((n, crate::errno::get()), (-1, libc::EINVAL));
    }

    #[test]
    fn a_name_is_refused_outside_a_bulk_listing() {
        assert_eq!(ask(c"/", list(ATTR_CMN_NAME, 0, 0), 0), Err(libc::ENOTSUP));
    }

    #[test]
    fn an_unimplemented_attribute_is_refused() {
        let root = c"/";
        assert_eq!(ask(root, list(0x1, 0, 0), 0), Err(libc::ENOTSUP));
        let mut l = list(ATTR_CMN_OBJTYPE, 0, 0);
        l.volattr = 0x8000_0000;
        assert_eq!(ask(root, l, 0), Err(libc::ENOTSUP));
        l = list(ATTR_CMN_OBJTYPE, 0, 0);
        l.bitmapcount = 4;
        assert_eq!(ask(root, l, 0), Err(libc::EINVAL));
    }
}
