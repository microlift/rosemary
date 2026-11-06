use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// `OSStatus`/`OSErr`, as the File Manager spells its answers.
type OSStatus = i32;

const NO_ERR: OSStatus = 0;
/// `paramErr`: an argument this call cannot work from.
const PARAM_ERR: OSStatus = -50;
/// `fnfErr`: the file or folder is not there.
const FNF_ERR: OSStatus = -43;
/// `dirNFErr`: the directory is not there, and could not be made.
const DIR_NF_ERR: OSStatus = -120;
/// `dupFNErr`: something by that name is already there.
const DUP_FN_ERR: OSStatus = -48;
/// `ioErr`: the host refused for a reason the File Manager has no word for.
const IO_ERR: OSStatus = -36;
/// `errFSRefsDifferent`: the two references name different objects.
const REFS_DIFFERENT: OSStatus = -1420;
/// `permErr`: the host refused for lack of permission.
const PERM_ERR: OSStatus = -54;
/// `dskFulErr`: the volume has no room left.
const DSK_FUL_ERR: OSStatus = -34;
/// `eofErr`: a read could not fill the whole request before running out of
/// fork to read.
const EOF_ERR: OSStatus = -39;
/// `posErr`: the position asked for is before the start of the fork.
const POS_ERR: OSStatus = -40;
/// `rfNumErr`: the fork reference number names nothing this tree has open.
const RF_NUM_ERR: OSStatus = -51;
/// `errFSForkNotFound`: a named fork, and the only one this tree has is the
/// data fork - see [`is_data_fork`].
const FORK_NOT_FOUND: OSStatus = -1409;
/// `errFSNoMoreItems`: an iterator - a fork, a directory listing - is spent.
const NO_MORE_ITEMS: OSStatus = -1417;
/// `errFSBadIteratorFlags`: a reserved bit of `FSIteratorFlags` was set.
const BAD_ITERATOR_FLAGS: OSStatus = -1422;
/// `diffVolErr`: the two ends of a move are on different filesystems, which
/// is the same thing Carbon means by different volumes.
const DIFF_VOL_ERR: OSStatus = -1303;

/// Whether a fork name is the data fork's - the empty string, which is
/// [`FSGetDataForkName`]'s own answer. Any other name asks for a fork this
/// tree does not have, such as the resource fork.
fn is_data_fork(name_length: u64) -> bool {
    name_length == 0
}

/// Bytes of an `FSRef`, which is ABI: a guest declares one on its stack.
const FS_REF_BYTES: usize = 80;

/// The first eight bytes of every reference this tree makes.
///
/// A reference that does not carry it came from somewhere else - a guest that
/// zeroed its storage, or a structure that is not an `FSRef` at all - and is
/// refused rather than read as an index into the table below.
const MAGIC: u64 = u64::from_le_bytes(*b"RMFSRef\0");

/// Every path a reference has been made for, and the index of each.
///
/// Host paths, since every call here opens them; a guest's spelling is
/// translated on the way in ([`FSPathMakeRef`]) and out ([`FSRefMakePath`]).
static PATHS: Mutex<Option<Interned>> = Mutex::new(None);

#[derive(Default)]
struct Interned {
    by_index: Vec<PathBuf>,
    by_path: HashMap<PathBuf, u64>,
}

/// The index for `path`, making one if this is the first reference to it.
fn intern(path: PathBuf) -> u64 {
    let mut held = PATHS.lock().unwrap_or_else(|e| e.into_inner());
    let table = held.get_or_insert_with(Interned::default);
    if let Some(index) = table.by_path.get(&path) {
        return *index;
    }
    let index = table.by_index.len() as u64;
    table.by_index.push(path.clone());
    table.by_path.insert(path, index);
    index
}

/// The path `index` was interned for.
fn interned(index: u64) -> Option<PathBuf> {
    let held = PATHS.lock().unwrap_or_else(|e| e.into_inner());
    held.as_ref()?.by_index.get(index as usize).cloned()
}

/// Write `path` into the caller's `FSRef` storage, for another module in this
/// crate that has a path and a caller's `FSRef` to fill in.
///
/// # Safety
///
/// As [`store`].
pub unsafe fn store_path(out: *mut c_void, path: PathBuf) {
    unsafe { store(out, path) };
}

/// Write `path` into the caller's `FSRef` storage.
///
/// # Safety
///
/// `out` is writable for [`FS_REF_BYTES`].
unsafe fn store(out: *mut c_void, path: PathBuf) {
    let index = intern(path);
    let mut bytes = [0_u8; FS_REF_BYTES];
    bytes[..8].copy_from_slice(&MAGIC.to_le_bytes());
    bytes[8..16].copy_from_slice(&index.to_le_bytes());
    // SAFETY: the caller states `out` holds a whole `FSRef`.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), FS_REF_BYTES) };
}

/// The path a reference this tree made names, or `None` for anything else.
///
/// # Safety
///
/// `reference` is null or readable for [`FS_REF_BYTES`].
unsafe fn load(reference: *const c_void) -> Option<PathBuf> {
    if reference.is_null() {
        return None;
    }
    let mut bytes = [0_u8; 16];
    unsafe { std::ptr::copy_nonoverlapping(reference.cast::<u8>(), bytes.as_mut_ptr(), 16) };
    if u64::from_le_bytes(bytes[..8].try_into().ok()?) != MAGIC {
        return None;
    }
    interned(u64::from_le_bytes(bytes[8..].try_into().ok()?))
}

/// A `FSVolumeRefNum` domain, as the root its folders hang off.
///
/// `None` for a domain this host has nothing to offer - a Classic System Folder
/// is the whole of that list, and answering the user's home for it would put a
/// file where the guest will never look again.
fn domain_root(volume: i16) -> Option<String> {
    Some(match volume {
        // kOnSystemDisk, kOnAppropriateDisk, kLocalDomain: this machine's own
        // root, where Apple puts `/Library` and `/Applications`.
        -32768 | -32767 | -32765 => String::new(),
        // kSystemDomain.
        -32766 => "/System".to_string(),
        // kNetworkDomain.
        -32764 => "/Network".to_string(),
        // kUserDomain.
        -32763 => foundation::home_directory(),
        _ => return None,
    })
}

/// The domain the two legacy "system disk" volumes mean for `kind`.
///
/// Carbon answers `kOnSystemDisk` and `kOnAppropriateDisk` from whichever
/// domain holds that kind of folder: the user's for preferences and the other
/// per-user folders, and the system domain for frameworks:
/// Apple's CallMachOFramework sample finds `System.framework` through
/// `FSFindFolder(kOnAppropriateDisk, kFrameworksFolderType)`, and programs that
/// copied it look up `free` that way. Every other kind keeps the local domain.
fn system_disk(volume: i16, kind: u32) -> i16 {
    const SYSTEM_DOMAIN: i16 = -32766;
    const USER_DOMAIN: i16 = -32763;
    match (volume, &kind.to_be_bytes()) {
        (-32768 | -32767, b"fram") => SYSTEM_DOMAIN,
        // The folders only a user has: there is no `/Library/Preferences` a
        // Carbon program asking for "the" preferences folder means.
        (
            -32768 | -32767,
            b"pref" | b"desk" | b"trsh" | b"docs" | b"down" | b"mdoc" | b"\xb5doc" | b"pdoc"
            | b"pubb" | b"cusr",
        ) => USER_DOMAIN,
        _ => volume,
    }
}

/// The folder an `OSType` names, relative to a domain root.
///
/// Four-character codes, written as the integers a guest passes rather than as
/// text, because that is what a comparison here has to be. `None` for a code
/// this tree cannot place, which [`FSFindFolder`] reports as "not found" rather
/// than guessing - the same rule `foundation::paths::relative` follows, and for
/// the same reason.
fn folder(kind: u32) -> Option<&'static str> {
    Some(match &kind.to_be_bytes() {
        b"macs" => "System",
        b"desk" => "Desktop",
        b"trsh" => ".Trash",
        b"font" => "Library/Fonts",
        b"pref" => "Library/Preferences",
        b"asup" => "Library/Application Support",
        b"cach" => "Library/Caches",
        b"logs" => "Library/Logs",
        b"libr" => "Library",
        b"fram" => "Library/Frameworks",
        b"ilgc" => "Library/Internet Plug-Ins",
        b"scr " => "Library/Scripts",
        b"apps" => "Applications",
        b"util" => "Applications/Utilities",
        b"docs" => "Documents",
        b"down" => "Downloads",
        b"mdoc" => "Movies",
        b"\xb5doc" => "Music",
        b"pdoc" => "Pictures",
        b"pubb" => "Public",
        b"temp" => "",
        b"cusr" => "",
        _ => return None,
    })
}

/// `OSErr FSFindFolder(FSVolumeRefNum vRefNum, OSType folderType,
/// Boolean createFolder, FSRef *result)`
///
/// # Safety
///
/// `result` is null or storage for a whole `FSRef`.
pub unsafe extern "C" fn FSFindFolder(
    volume: i16,
    kind: u32,
    create: u8,
    result: *mut c_void,
) -> i16 {
    if result.is_null() {
        return PARAM_ERR as i16;
    }
    let (Some(root), Some(relative)) = (domain_root(system_disk(volume, kind)), folder(kind))
    else {
        return FNF_ERR as i16;
    };
    // The temporary folder is the one answer that does not hang off a domain:
    // Apple's is per-user and per-boot, and the guest's is `/tmp`.
    let guest = match &kind.to_be_bytes() {
        b"temp" => "/tmp".to_string(),
        _ if relative.is_empty() => root,
        _ => format!("{root}/{relative}"),
    };
    let path = prefix::ns::host_path(&guest);
    // A provided framework's folder exists without being on disk, and is not
    // made there either: that would put an empty directory where the guest
    // expects to find every system framework.
    let mut found = path.is_dir() || darwin::images::virtual_directory(&guest);
    if create != 0 && !found {
        // A failure here is reported rather than ignored: `/Library/Frameworks`
        // cannot be made by an unprivileged process, and a reference to a
        // directory that is not there is worse than saying so.
        let _ = std::fs::create_dir_all(&path);
        found = path.is_dir();
    }
    if !found {
        return DIR_NF_ERR as i16;
    }
    // SAFETY: checked non-null above.
    unsafe { store(result, path) };
    NO_ERR as i16
}

/// `OSStatus FSPathMakeRef(const UInt8 *path, FSRef *ref, Boolean *isDirectory)`
///
/// # Safety
///
/// `path` is null or a C string, `reference` storage for a whole `FSRef`, and
/// `is_directory` null or a writable `Boolean`.
pub unsafe extern "C" fn FSPathMakeRef(
    path: *const u8,
    reference: *mut c_void,
    is_directory: *mut u8,
) -> OSStatus {
    if path.is_null() || reference.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller states `path` is a C string.
    let text = unsafe { CStr::from_ptr(path.cast()) };
    let Ok(text) = text.to_str() else {
        return PARAM_ERR;
    };
    let path = prefix::ns::host_path(text);
    let directory = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.is_dir(),
        Err(_) if darwin::images::virtual_directory(text) => true,
        Err(_) => return FNF_ERR,
    };
    if !is_directory.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *is_directory = u8::from(directory) };
    }
    // SAFETY: checked non-null above.
    unsafe { store(reference, path) };
    NO_ERR
}

/// `OSStatus FSRefMakePath(const FSRef *ref, UInt8 *path, UInt32 maxPathSize)`
///
/// Not imported by the guest, but `CFURLCreateFromFSRef` is the other half of
/// every `FSFindFolder` a Cocoa-era program makes and it is the call that turns
/// the reference back into something openable.
///
/// # Safety
///
/// `reference` is null or a whole `FSRef`, and `out` is writable for `max`.
pub unsafe extern "C" fn FSRefMakePath(
    reference: *const c_void,
    out: *mut u8,
    max: u32,
) -> OSStatus {
    if out.is_null() || max == 0 {
        return PARAM_ERR;
    }
    let Some(host) = (unsafe { load(reference) }) else {
        return PARAM_ERR;
    };
    let path = prefix::ns::to_guest(&host).unwrap_or(host);
    let bytes = path.as_os_str().as_encoded_bytes();
    // Apple reports `pathTooLongErr` rather than truncating, and a truncated
    // path names a different file.
    if bytes.len() + 1 > max as usize {
        return -2110;
    }
    // SAFETY: the length was just checked against the caller's own capacity.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        *out.add(bytes.len()) = 0;
    }
    NO_ERR
}

/// `OSErr FSCompareFSRefs(const FSRef *ref1, const FSRef *ref2)`
///
/// # Safety
///
/// Both arguments are null or whole `FSRef`s.
pub unsafe extern "C" fn FSCompareFSRefs(a: *const c_void, b: *const c_void) -> OSStatus {
    let (Some(a), Some(b)) = (unsafe { load(a) }, unsafe { load(b) }) else {
        return PARAM_ERR;
    };
    match a == b {
        true => NO_ERR,
        false => REFS_DIFFERENT,
    }
}

/// Whether the host path a reference holds is a folder, counting the ones a
/// provided framework makes exist - see [`darwin::images::virtual_directory`].
fn is_folder(host: &std::path::Path) -> bool {
    host.is_dir()
        || prefix::ns::to_guest(host)
            .and_then(|guest| guest.to_str().map(darwin::images::virtual_directory))
            .unwrap_or(false)
}

/// `OSErr FSIsAliasFile(const FSRef *ref, Boolean *aliasFileFlag,
/// Boolean *folderFlag)`
///
/// An alias is a Finder structure this tree does not write and cannot read, so
/// nothing here is one. A symlink is *not* an alias - the File Manager reports
/// those through `kFSCatInfoNodeFlags` - so answering false is the truth rather
/// than a shortcut.
///
/// # Safety
///
/// `reference` is null or a whole `FSRef`; the two flags are null or writable.
pub unsafe extern "C" fn FSIsAliasFile(
    reference: *const c_void,
    alias: *mut u8,
    folder: *mut u8,
) -> OSStatus {
    let Some(path) = (unsafe { load(reference) }) else {
        return PARAM_ERR;
    };
    if !alias.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *alias = 0 };
    }
    if !folder.is_null() {
        unsafe { *folder = u8::from(is_folder(&path)) };
    }
    NO_ERR
}

/// `OSErr FSResolveAliasFile(FSRef *ref, Boolean resolveAliasChains,
/// Boolean *targetIsFolder, Boolean *wasAliased)`
///
/// Nothing here is an alias - see [`FSIsAliasFile`] - so the reference is left
/// as it was and `wasAliased` is false, which is what Apple answers for a plain
/// file.
///
/// # Safety
///
/// `reference` is null or a whole `FSRef`; the two flags are null or writable.
pub unsafe extern "C" fn FSResolveAliasFile(
    reference: *mut c_void,
    _chains: u8,
    target_is_folder: *mut u8,
    was_aliased: *mut u8,
) -> OSStatus {
    let Some(path) = (unsafe { load(reference) }) else {
        return PARAM_ERR;
    };
    if !target_is_folder.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *target_is_folder = u8::from(is_folder(&path)) };
    }
    if !was_aliased.is_null() {
        unsafe { *was_aliased = 0 };
    }
    NO_ERR
}

/// `OSErr FSCreateFileUnicode(const FSRef *parentRef, UniCharCount nameLength,
/// const UniChar *name, FSCatalogInfoBitmap whichInfo, const FSCatalogInfo
/// *catalogInfo, FSRef *newRef, FSSpecPtr newSpec)`
///
/// Creates an empty file; fails if one exists. The catalog info a caller asks
/// to set (Finder info, dates, permissions) is not applied - Unity passes none.
/// `newSpec` is left alone: an `FSSpec` has no 64-bit meaning.
///
/// # Safety
///
/// `parent` is a whole `FSRef`, `name` holds `length` UTF-16 units, and
/// `new_ref` is null or storage for a whole `FSRef`. `new_ref` may be `parent`.
pub unsafe extern "C" fn FSCreateFileUnicode(
    parent: *const c_void,
    length: u64,
    name: *const u16,
    _which_info: u32,
    _catalog_info: *const c_void,
    new_ref: *mut c_void,
    _new_spec: *mut c_void,
) -> OSStatus {
    if name.is_null() || length == 0 {
        return PARAM_ERR;
    }
    let Some(directory) = (unsafe { load(parent) }) else {
        return PARAM_ERR;
    };
    // SAFETY: the caller states `name` holds `length` units.
    let units = unsafe { std::slice::from_raw_parts(name, length as usize) };
    let Ok(name) = String::from_utf16(units) else {
        return PARAM_ERR;
    };
    if name.contains('/') {
        return PARAM_ERR;
    }
    let path = directory.join(name);
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path);
    match created {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return DUP_FN_ERR,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DIR_NF_ERR,
        Err(_) => return IO_ERR,
    }
    if !new_ref.is_null() {
        // SAFETY: the caller states it is a whole `FSRef`.
        unsafe { store(new_ref, path) };
    }
    NO_ERR
}

/// `FSForkInfo`'s 48 bytes, under the header's own `#pragma pack(2)`.
///
/// Written out in full for the reason [`CatalogInfo`] is: the guest reads
/// these offsets by name.
#[repr(C, packed(2))]
#[derive(Clone, Copy, Default)]
pub struct ForkInfo {
    flags: u8,
    permissions: i8,
    volume: i16,
    reserved2: u32,
    node_id: u32,
    fork_id: u32,
    current_position: u64,
    logical_eof: u64,
    physical_eof: u64,
    process: u64,
}

const _: () = assert!(size_of::<ForkInfo>() == 48);

/// `CatPositionRec`, the sixteen bytes [`FSIterateForks`] steps with.
///
/// Only `initialize` is read: there is at most one fork to report, so it
/// doubles as "have I already reported it" rather than a real cursor.
#[repr(C, packed(2))]
#[derive(Clone, Copy, Default)]
pub struct CatPosition {
    initialize: i32,
    reserved: [i16; 6],
}

const _: () = assert!(size_of::<CatPosition>() == 16);

/// One fork this tree has open - always the data fork, since no other kind
/// exists here. Kept behind an index rather than the pointer Carbon's
/// `FSIORefNum` suggests, so a bad guest value is a bounds check rather than
/// a dereference.
struct ForkEntry {
    file: std::fs::File,
    path: PathBuf,
    /// What [`FSOpenFork`] was asked for, echoed back by [`FSGetForkCBInfo`].
    permissions: i8,
}

/// Every fork this process has open, by `FSIORefNum` minus one - zero is
/// never a valid reference number, on Carbon or here.
static FORKS: Mutex<Vec<Option<ForkEntry>>> = Mutex::new(Vec::new());

/// Opens a new slot, reusing a closed one before growing the table.
fn fork_open(path: PathBuf, file: std::fs::File, permissions: i8) -> i32 {
    let mut held = FORKS.lock().unwrap_or_else(|e| e.into_inner());
    let entry = Some(ForkEntry {
        file,
        path,
        permissions,
    });
    match held.iter().position(Option::is_none) {
        Some(index) => {
            held[index] = entry;
            index as i32 + 1
        }
        None => {
            held.push(entry);
            held.len() as i32
        }
    }
}

/// Runs `f` against the fork `num` names, or answers `None` for a number
/// nothing is open under.
fn fork_with<T>(num: i32, f: impl FnOnce(&mut ForkEntry) -> T) -> Option<T> {
    let mut held = FORKS.lock().unwrap_or_else(|e| e.into_inner());
    let index = usize::try_from(num.checked_sub(1)?).ok()?;
    held.get_mut(index)?.as_mut().map(f)
}

/// Frees `num`'s slot, closing the file it held. `false` for a number
/// nothing was open under, which is what the caller answers `rfNumErr` for.
fn fork_close(num: i32) -> bool {
    let mut held = FORKS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(index) = num.checked_sub(1).and_then(|i| usize::try_from(i).ok()) else {
        return false;
    };
    match held.get_mut(index) {
        Some(slot @ Some(_)) => {
            *slot = None;
            true
        }
        _ => false,
    }
}

/// The absolute byte offset `mode` (`fsAtMark`/`fsFromStart`/`fsFromLEOF`/
/// `fsFromMark`, the low two bits - the bits above are caching hints this
/// host has no counterpart for) and `offset` select, relative to `file`'s
/// current mark and length.
fn resolved_offset(file: &mut std::fs::File, mode: u16, offset: i64) -> Result<u64, OSStatus> {
    let base = match mode & 0x0003 {
        0 => return file.stream_position().map_err(|_| IO_ERR), // fsAtMark: offset is not consulted.
        1 => 0i64,                                              // fsFromStart
        2 => file.metadata().map_err(|_| IO_ERR)?.len() as i64, // fsFromLEOF
        3 => file.stream_position().map_err(|_| IO_ERR)? as i64, // fsFromMark
        _ => unreachable!("masked to two bits"),
    };
    base.checked_add(offset)
        .filter(|at| *at >= 0)
        .map(|at| at as u64)
        .ok_or(POS_ERR)
}

/// `OSErr FSCreateFork(const FSRef *ref, UniCharCount forkNameLength, const
/// UniChar *forkName)`
///
/// The data fork exists from the moment the object does, so there is never
/// one left for this call to make; any other name is a fork this tree does
/// not have at all.
///
/// FIXME (ACCURACY): answered as `dupFNErr` by analogy with every other
/// "make a thing that is already there" call in this file - Apple's own
/// answer for this exact case is not confirmed.
///
/// # Safety
///
/// `reference` is a whole `FSRef`; `name` holds `length` units.
pub unsafe extern "C" fn FSCreateFork(
    reference: *const c_void,
    length: u64,
    _name: *const u16,
) -> OSStatus {
    let Some(path) = (unsafe { load(reference) }) else {
        return PARAM_ERR;
    };
    if !is_data_fork(length) {
        return FORK_NOT_FOUND;
    }
    if std::fs::symlink_metadata(&path).is_err() {
        return FNF_ERR;
    }
    DUP_FN_ERR
}

/// `OSErr FSOpenFork(const FSRef *ref, UniCharCount forkNameLength, const
/// UniChar *forkName, SInt8 permissions, FSIORefNum *forkRefNum)`
///
/// # Safety
///
/// `reference` is a whole `FSRef`; `name` holds `length` units; `out_ref_num`
/// is null or a writable `FSIORefNum`.
pub unsafe extern "C" fn FSOpenFork(
    reference: *const c_void,
    length: u64,
    _name: *const u16,
    permissions: i8,
    out_ref_num: *mut i32,
) -> OSStatus {
    if out_ref_num.is_null() {
        return PARAM_ERR;
    }
    let Some(path) = (unsafe { load(reference) }) else {
        return PARAM_ERR;
    };
    if !is_data_fork(length) {
        return FORK_NOT_FOUND;
    }
    // FIXME (ACCURACY): `fsRdWrShPerm` asks for deny-mode locking this host
    // does not enforce, and `fsCurPerm` asks to keep whatever a path already
    // open has - there is no such path on a fresh open, so it is answered as
    // a plain read.
    let (read, write) = match permissions {
        0 | 1 => (true, false),
        2 => (false, true),
        3 | 4 => (true, true),
        _ => return PARAM_ERR,
    };
    let file = std::fs::OpenOptions::new()
        .read(read)
        .write(write)
        .open(&path);
    let file = match file {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FNF_ERR,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return PERM_ERR,
        Err(_) => return IO_ERR,
    };
    let num = fork_open(path, file, permissions);
    // SAFETY: checked non-null above.
    unsafe { out_ref_num.write_unaligned(num) };
    NO_ERR
}

/// `OSErr FSCloseFork(FSIORefNum forkRefNum)`
pub extern "C" fn FSCloseFork(fork_ref_num: i32) -> OSStatus {
    match fork_close(fork_ref_num) {
        true => NO_ERR,
        false => RF_NUM_ERR,
    }
}

/// `OSErr FSReadFork(FSIORefNum forkRefNum, UInt16 positionMode, SInt64
/// positionOffset, ByteCount requestCount, void *buffer, ByteCount
/// *actualCount)`
///
/// A short read - the fork ran out before `requestCount` was reached - is
/// `eofErr`, as Carbon answers it; `actualCount` still holds what was read.
///
/// # Safety
///
/// `buffer` is writable for `request_count`, unless that is zero; `actual_count`
/// is null or writable.
pub unsafe extern "C" fn FSReadFork(
    fork_ref_num: i32,
    position_mode: u16,
    position_offset: i64,
    request_count: u64,
    buffer: *mut u8,
    actual_count: *mut u64,
) -> OSStatus {
    if request_count == 0 {
        if !actual_count.is_null() {
            unsafe { actual_count.write_unaligned(0) };
        }
        return NO_ERR;
    }
    if buffer.is_null() {
        return PARAM_ERR;
    }
    let outcome = fork_with(fork_ref_num, |entry| -> Result<u64, OSStatus> {
        let at = resolved_offset(&mut entry.file, position_mode, position_offset)?;
        entry.file.seek(SeekFrom::Start(at)).map_err(|_| IO_ERR)?;
        let want = request_count as usize;
        // SAFETY: the caller states `buffer` is writable for `want` bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(buffer, want) };
        let mut read = 0;
        while read < want {
            match entry.file.read(&mut out[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(IO_ERR),
            }
        }
        Ok(read as u64)
    });
    let Some(outcome) = outcome else {
        return RF_NUM_ERR;
    };
    let read = match outcome {
        Ok(read) => read,
        Err(status) => return status,
    };
    if !actual_count.is_null() {
        unsafe { actual_count.write_unaligned(read) };
    }
    match read < request_count {
        true => EOF_ERR,
        false => NO_ERR,
    }
}

/// `OSErr FSWriteFork(FSIORefNum forkRefNum, UInt16 positionMode, SInt64
/// positionOffset, ByteCount requestCount, const void *buffer, ByteCount
/// *actualCount)`
///
/// # Safety
///
/// `buffer` is readable for `request_count`, unless that is zero;
/// `actual_count` is null or writable.
pub unsafe extern "C" fn FSWriteFork(
    fork_ref_num: i32,
    position_mode: u16,
    position_offset: i64,
    request_count: u64,
    buffer: *const u8,
    actual_count: *mut u64,
) -> OSStatus {
    if request_count == 0 {
        if !actual_count.is_null() {
            unsafe { actual_count.write_unaligned(0) };
        }
        return NO_ERR;
    }
    if buffer.is_null() {
        return PARAM_ERR;
    }
    let outcome = fork_with(fork_ref_num, |entry| -> Result<u64, OSStatus> {
        let at = resolved_offset(&mut entry.file, position_mode, position_offset)?;
        entry.file.seek(SeekFrom::Start(at)).map_err(|_| IO_ERR)?;
        let want = request_count as usize;
        // SAFETY: the caller states `buffer` holds `want` bytes.
        let input = unsafe { std::slice::from_raw_parts(buffer, want) };
        let mut written = 0;
        while written < want {
            match entry.file.write(&input[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.raw_os_error() == Some(libc::ENOSPC) => return Err(DSK_FUL_ERR),
                Err(_) => return Err(IO_ERR),
            }
        }
        Ok(written as u64)
    });
    let Some(outcome) = outcome else {
        return RF_NUM_ERR;
    };
    match outcome {
        Ok(written) => {
            if !actual_count.is_null() {
                unsafe { actual_count.write_unaligned(written) };
            }
            NO_ERR
        }
        Err(status) => status,
    }
}

/// `OSErr FSGetForkPosition(FSIORefNum forkRefNum, SInt64 *position)`
///
/// # Safety
///
/// `position` is null or writable.
pub unsafe extern "C" fn FSGetForkPosition(fork_ref_num: i32, position: *mut i64) -> OSStatus {
    if position.is_null() {
        return PARAM_ERR;
    }
    match fork_with(fork_ref_num, |entry| entry.file.stream_position()) {
        Some(Ok(at)) => {
            unsafe { position.write_unaligned(at as i64) };
            NO_ERR
        }
        Some(Err(_)) => IO_ERR,
        None => RF_NUM_ERR,
    }
}

/// `OSErr FSSetForkPosition(FSIORefNum forkRefNum, UInt16 positionMode,
/// SInt64 positionOffset)`
pub extern "C" fn FSSetForkPosition(
    fork_ref_num: i32,
    position_mode: u16,
    position_offset: i64,
) -> OSStatus {
    let outcome = fork_with(fork_ref_num, |entry| -> Result<(), OSStatus> {
        let at = resolved_offset(&mut entry.file, position_mode, position_offset)?;
        entry.file.seek(SeekFrom::Start(at)).map_err(|_| IO_ERR)?;
        Ok(())
    });
    match outcome {
        Some(Ok(())) => NO_ERR,
        Some(Err(status)) => status,
        None => RF_NUM_ERR,
    }
}

/// `OSErr FSGetForkSize(FSIORefNum forkRefNum, SInt64 *forkSize)`
///
/// # Safety
///
/// `fork_size` is null or writable.
pub unsafe extern "C" fn FSGetForkSize(fork_ref_num: i32, fork_size: *mut i64) -> OSStatus {
    if fork_size.is_null() {
        return PARAM_ERR;
    }
    match fork_with(fork_ref_num, |entry| entry.file.metadata().map(|m| m.len())) {
        Some(Ok(len)) => {
            unsafe { fork_size.write_unaligned(len as i64) };
            NO_ERR
        }
        Some(Err(_)) => IO_ERR,
        None => RF_NUM_ERR,
    }
}

/// `OSErr FSSetForkSize(FSIORefNum forkRefNum, UInt16 positionMode, SInt64
/// positionOffset)`
pub extern "C" fn FSSetForkSize(
    fork_ref_num: i32,
    position_mode: u16,
    position_offset: i64,
) -> OSStatus {
    let outcome = fork_with(fork_ref_num, |entry| -> Result<(), OSStatus> {
        let at = resolved_offset(&mut entry.file, position_mode, position_offset)?;
        entry
            .file
            .set_len(at)
            .map_err(|e| match e.raw_os_error() == Some(libc::ENOSPC) {
                true => DSK_FUL_ERR,
                false => IO_ERR,
            })
    });
    match outcome {
        Some(Ok(())) => NO_ERR,
        Some(Err(status)) => status,
        None => RF_NUM_ERR,
    }
}

/// `OSErr FSGetForkCBInfo(FSIORefNum desiredRefNum, FSVolumeRefNum volume,
/// SInt16 *iterator, FSIORefNum *actualRefNum, FSForkInfo *forkInfo, FSRef
/// *ref, HFSUniStr255 *outForkName)`
///
/// `desiredRefNum` of zero walks every fork this process has open, starting
/// from `*iterator` (zero, the first time); any other value asks about that
/// one fork and ignores both `volume` and `iterator`.
///
/// # Safety
///
/// `iterator` is null or readable and writable when `desired_ref_num` is
/// zero; every other non-null output is writable for its own type.
pub unsafe extern "C" fn FSGetForkCBInfo(
    desired_ref_num: i32,
    volume: i16,
    iterator: *mut i16,
    actual_ref_num: *mut i32,
    fork_info: *mut ForkInfo,
    reference: *mut c_void,
    out_fork_name: *mut UniStr255,
) -> OSStatus {
    let mut held = FORKS.lock().unwrap_or_else(|e| e.into_inner());
    let num = if desired_ref_num != 0 {
        let Some(index) = desired_ref_num
            .checked_sub(1)
            .and_then(|i| usize::try_from(i).ok())
        else {
            return RF_NUM_ERR;
        };
        if !matches!(held.get(index), Some(Some(_))) {
            return RF_NUM_ERR;
        }
        desired_ref_num
    } else {
        if iterator.is_null() {
            return PARAM_ERR;
        }
        // SAFETY: checked non-null; a caller zeroes it before the first call.
        let mut at = unsafe { iterator.read_unaligned() }.max(0) as usize;
        let found = loop {
            match held.get(at) {
                None => return NO_MORE_ITEMS,
                Some(None) => at += 1,
                Some(Some(entry)) => {
                    let matches_volume = volume == 0 || {
                        use std::os::unix::fs::MetadataExt;
                        std::fs::metadata(&entry.path)
                            .map(|m| volume_of(m.dev(), &entry.path) == volume)
                            .unwrap_or(false)
                    };
                    if matches_volume {
                        break at;
                    }
                    at += 1;
                }
            }
        };
        // SAFETY: checked non-null above.
        unsafe { iterator.write_unaligned((found + 1) as i16) };
        (found + 1) as i32
    };
    let index = (num - 1) as usize;
    let entry = held[index].as_mut().expect("just found above");
    let Ok(metadata) = entry.file.metadata() else {
        return IO_ERR;
    };
    let position = entry.file.stream_position().unwrap_or(0);
    if !actual_ref_num.is_null() {
        unsafe { actual_ref_num.write_unaligned(num) };
    }
    if !fork_info.is_null() {
        use std::os::unix::fs::MetadataExt;
        let info = ForkInfo {
            flags: 0,
            permissions: entry.permissions,
            volume: volume_of(metadata.dev(), &entry.path),
            reserved2: 0,
            node_id: metadata.ino() as u32,
            // FIXME (ACCURACY): no named fork exists to tell apart from the
            // data fork, so its id doubles as the node's rather than a real
            // fork id.
            fork_id: metadata.ino() as u32,
            current_position: position,
            logical_eof: metadata.len(),
            physical_eof: metadata.blocks().saturating_mul(512),
            // "should be ProcessSerialNumber", in Apple's own header - this
            // host has no PSNs to answer with.
            process: 0,
        };
        unsafe { fork_info.write_unaligned(info) };
    }
    if !reference.is_null() {
        unsafe { store(reference, entry.path.clone()) };
    }
    if !out_fork_name.is_null() {
        unsafe {
            out_fork_name.write_unaligned(UniStr255 {
                length: 0,
                unicode: [0; 255],
            })
        };
    }
    NO_ERR
}

/// `OSErr FSIterateForks(const FSRef *ref, CatPositionRec *forkIterator,
/// HFSUniStr255 *forkName, SInt64 *forkSize, UInt64 *forkPhysicalSize)`
///
/// There is at most one fork to report - the data fork of a plain file - so
/// the first call answers it and every later one on the same iterator (or
/// any call on a directory, which has no forks at all) is `errFSNoMoreItems`.
///
/// # Safety
///
/// `reference` is a whole `FSRef`; `fork_iterator` is a whole
/// `CatPositionRec`, zeroed by the caller before the first call; every other
/// non-null output is writable for its own type.
pub unsafe extern "C" fn FSIterateForks(
    reference: *const c_void,
    fork_iterator: *mut CatPosition,
    fork_name: *mut UniStr255,
    fork_size: *mut i64,
    fork_physical_size: *mut u64,
) -> OSStatus {
    if fork_iterator.is_null() {
        return PARAM_ERR;
    }
    let Some(path) = (unsafe { load(reference) }) else {
        return PARAM_ERR;
    };
    // SAFETY: the caller states a whole `CatPositionRec`.
    let mut state = unsafe { fork_iterator.read_unaligned() };
    if state.initialize != 0 {
        return NO_MORE_ITEMS;
    }
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return FNF_ERR;
    };
    if metadata.is_dir() {
        return NO_MORE_ITEMS;
    }
    if !fork_name.is_null() {
        unsafe {
            fork_name.write_unaligned(UniStr255 {
                length: 0,
                unicode: [0; 255],
            })
        };
    }
    if !fork_size.is_null() {
        unsafe { fork_size.write_unaligned(metadata.len() as i64) };
    }
    if !fork_physical_size.is_null() {
        use std::os::unix::fs::MetadataExt;
        unsafe { fork_physical_size.write_unaligned(metadata.blocks().saturating_mul(512)) };
    }
    state.initialize = 1;
    // SAFETY: as the read above.
    unsafe { fork_iterator.write_unaligned(state) };
    NO_ERR
}

/// `OSErr FSGetDataForkName(HFSUniStr255 *dataForkName)`
///
/// The data fork's name is the empty string - Apple's own answer, and the
/// only fork name this tree ever has to give; see [`is_data_fork`].
///
/// # Safety
///
/// `name` is null or writable for a whole `HFSUniStr255`.
pub unsafe extern "C" fn FSGetDataForkName(name: *mut UniStr255) -> OSStatus {
    if name.is_null() {
        return PARAM_ERR;
    }
    unsafe {
        name.write_unaligned(UniStr255 {
            length: 0,
            unicode: [0; 255],
        })
    };
    NO_ERR
}

/// `OSErr FSCreateDirectoryUnicode(const FSRef *parentRef, UniCharCount
/// nameLength, const UniChar *name, FSCatalogInfoBitmap whichInfo, const
/// FSCatalogInfo *catalogInfo, FSRef *newRef, FSSpecPtr newSpec, UInt32
/// *newDirID)`
///
/// As [`FSCreateFileUnicode`]: the catalog info a caller asks to set is not
/// applied, and `newSpec` is left alone.
///
/// # Safety
///
/// `parent` is a whole `FSRef`; `name` holds `length` units; `new_ref` is
/// null or storage for a whole `FSRef` and may be `parent`; `new_dir_id` is
/// null or writable.
pub unsafe extern "C" fn FSCreateDirectoryUnicode(
    parent: *const c_void,
    length: u64,
    name: *const u16,
    _which_info: u32,
    _catalog_info: *const c_void,
    new_ref: *mut c_void,
    _new_spec: *mut c_void,
    new_dir_id: *mut u32,
) -> OSStatus {
    if name.is_null() || length == 0 {
        return PARAM_ERR;
    }
    let Some(directory) = (unsafe { load(parent) }) else {
        return PARAM_ERR;
    };
    // SAFETY: the caller states `name` holds `length` units.
    let units = unsafe { std::slice::from_raw_parts(name, length as usize) };
    let Ok(leaf) = String::from_utf16(units) else {
        return PARAM_ERR;
    };
    if leaf.contains('/') {
        return PARAM_ERR;
    }
    let path = directory.join(&leaf);
    match std::fs::create_dir(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return DUP_FN_ERR,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DIR_NF_ERR,
        Err(_) => return IO_ERR,
    }
    if !new_dir_id.is_null() {
        use std::os::unix::fs::MetadataExt;
        if let Ok(metadata) = std::fs::metadata(&path) {
            unsafe { new_dir_id.write_unaligned(metadata.ino() as u32) };
        }
    }
    if !new_ref.is_null() {
        // SAFETY: the caller states it is a whole `FSRef`.
        unsafe { store(new_ref, path) };
    }
    NO_ERR
}

/// `OSErr FSMakeFSRefUnicode(const FSRef *parentRef, UniCharCount
/// nameLength, const UniChar *name, TextEncoding textEncodingHint, FSRef
/// *newRef)`
///
/// The encoding hint is Carbon's fallback for a name that is not valid
/// Unicode; the name here always already is, so it is never consulted.
///
/// # Safety
///
/// `parent` is a whole `FSRef`; `name` holds `length` units; `new_ref` is
/// null or storage for a whole `FSRef`.
pub unsafe extern "C" fn FSMakeFSRefUnicode(
    parent: *const c_void,
    length: u64,
    name: *const u16,
    _text_encoding_hint: u32,
    new_ref: *mut c_void,
) -> OSStatus {
    if name.is_null() {
        return PARAM_ERR;
    }
    let Some(directory) = (unsafe { load(parent) }) else {
        return PARAM_ERR;
    };
    // SAFETY: the caller states `name` holds `length` units.
    let units = unsafe { std::slice::from_raw_parts(name, length as usize) };
    let Ok(leaf) = String::from_utf16(units) else {
        return PARAM_ERR;
    };
    let path = directory.join(leaf);
    if std::fs::symlink_metadata(&path).is_err() {
        return FNF_ERR;
    }
    if !new_ref.is_null() {
        // SAFETY: the caller states it is a whole `FSRef`.
        unsafe { store(new_ref, path) };
    }
    NO_ERR
}

/// `FSRefParam`, the parameter block [`PBMakeFSRefUnicodeSync`] reads and
/// writes.
///
/// The fields ahead of `reference` are the File Manager's own queue and trap
/// bookkeeping, which this call never looks at - but the *offsets* are what
/// the guest declared the structure at, so every one of them is here too.
#[repr(C, packed(2))]
#[derive(Default)]
struct RefParam {
    q_link: u64,
    q_type: i16,
    io_trap: i16,
    io_cmd_addr: u64,
    io_completion: u64,
    io_result: i16,
    io_name_ptr: u64,
    io_vref_num: i16,
    reserved1: i16,
    reserved2: u8,
    reserved3: u8,
    reference: u64,
    which_info: u32,
    cat_info: u64,
    name_length: u64,
    name: u64,
    io_dir_id: u32,
    spec: u64,
    parent_ref: u64,
    new_ref: u64,
    text_encoding_hint: u32,
    out_name: u64,
}

const _: () = {
    assert!(std::mem::offset_of!(RefParam, io_result) == 28);
    assert!(std::mem::offset_of!(RefParam, reference) == 44);
    assert!(std::mem::offset_of!(RefParam, name_length) == 64);
    assert!(std::mem::offset_of!(RefParam, name) == 72);
    assert!(std::mem::offset_of!(RefParam, new_ref) == 100);
    assert!(std::mem::offset_of!(RefParam, text_encoding_hint) == 108);
    assert!(size_of::<RefParam>() == 120);
};

/// `OSErr PBMakeFSRefUnicodeSync(FSRefParam *paramBlock)`
///
/// The parameter-block form of [`FSMakeFSRefUnicode`]: `ref` is the parent,
/// `name`/`nameLength` the child's Unicode name, and `newRef` the output -
/// the same fields the comment above `FSMakeFSRefUnicode` names them. As
/// every synchronous PB call, the result also lands in `ioResult`, since
/// older callers read it from there instead of the return value.
///
/// # Safety
///
/// `param_block` is a whole `FSRefParam`.
pub unsafe extern "C" fn PBMakeFSRefUnicodeSync(param_block: *mut c_void) -> OSStatus {
    if param_block.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller states a whole `FSRefParam`.
    let block = unsafe { param_block.cast::<RefParam>().read_unaligned() };
    let status = unsafe {
        FSMakeFSRefUnicode(
            block.reference as *const c_void,
            block.name_length,
            block.name as *const u16,
            block.text_encoding_hint,
            block.new_ref as *mut c_void,
        )
    };
    // SAFETY: the same whole `FSRefParam`; only `ioResult` is written.
    unsafe {
        param_block
            .cast::<u8>()
            .add(std::mem::offset_of!(RefParam, io_result))
            .cast::<i16>()
            .write_unaligned(status as i16)
    };
    status
}

/// `OSErr FSRenameUnicode(const FSRef *ref, UniCharCount nameLength, const
/// UniChar *name, TextEncoding textEncodingHint, FSRef *newRef)`
///
/// Renamed within its own directory; a name already taken there is
/// `dupFNErr` rather than the silent replacement a host `rename` would give.
///
/// FIXME (ACCURACY): the existence check and the rename are two calls, not
/// one - a racing writer between them can still make this overwrite.
///
/// # Safety
///
/// `reference` is a whole `FSRef`; `name` holds `length` units; `new_ref` is
/// null or storage for a whole `FSRef` and may be `reference`.
pub unsafe extern "C" fn FSRenameUnicode(
    reference: *const c_void,
    length: u64,
    name: *const u16,
    _text_encoding_hint: u32,
    new_ref: *mut c_void,
) -> OSStatus {
    if name.is_null() || length == 0 {
        return PARAM_ERR;
    }
    let Some(path) = (unsafe { load(reference) }) else {
        return PARAM_ERR;
    };
    let Some(parent) = path.parent().map(PathBuf::from) else {
        return PARAM_ERR;
    };
    // SAFETY: the caller states `name` holds `length` units.
    let units = unsafe { std::slice::from_raw_parts(name, length as usize) };
    let Ok(leaf) = String::from_utf16(units) else {
        return PARAM_ERR;
    };
    if leaf.contains('/') {
        return PARAM_ERR;
    }
    let target = parent.join(&leaf);
    if target != path && std::fs::symlink_metadata(&target).is_ok() {
        return DUP_FN_ERR;
    }
    match std::fs::rename(&path, &target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FNF_ERR,
        Err(_) => return IO_ERR,
    }
    if !new_ref.is_null() {
        // SAFETY: the caller states it is a whole `FSRef`.
        unsafe { store(new_ref, target) };
    }
    NO_ERR
}

/// `OSErr FSMoveObject(const FSRef *ref, const FSRef *destDirectory, FSRef
/// *newRef)`
///
/// Keeps the object's own name; a host `rename` across filesystems is
/// `EXDEV`, which is `diffVolErr` here just as it is on Carbon.
///
/// FIXME (ACCURACY): as [`FSRenameUnicode`], the existence check and the
/// move are two calls, not one.
///
/// # Safety
///
/// `reference` and `dest_directory` are whole `FSRef`s; `new_ref` is null or
/// storage for a whole `FSRef` and may be either argument.
pub unsafe extern "C" fn FSMoveObject(
    reference: *const c_void,
    dest_directory: *const c_void,
    new_ref: *mut c_void,
) -> OSStatus {
    let (Some(path), Some(directory)) =
        (unsafe { load(reference) }, unsafe { load(dest_directory) })
    else {
        return PARAM_ERR;
    };
    let Some(name) = path.file_name().map(std::ffi::OsStr::to_os_string) else {
        return PARAM_ERR;
    };
    let target = directory.join(&name);
    if target != path && std::fs::symlink_metadata(&target).is_ok() {
        return DUP_FN_ERR;
    }
    match std::fs::rename(&path, &target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FNF_ERR,
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => return DIFF_VOL_ERR,
        Err(_) => return IO_ERR,
    }
    if !new_ref.is_null() {
        // SAFETY: the caller states it is a whole `FSRef`.
        unsafe { store(new_ref, target) };
    }
    NO_ERR
}

/// `OSErr FSDeleteObject(const FSRef *ref)`
///
/// A non-empty directory is `fBsyErr`: Classic Mac OS has no separate
/// "directory not empty" error, and uses the same one it gives for a file
/// still open elsewhere.
///
/// # Safety
///
/// `reference` is a whole `FSRef`.
pub unsafe extern "C" fn FSDeleteObject(reference: *const c_void) -> OSStatus {
    const FBSY_ERR: OSStatus = -47;
    let Some(path) = (unsafe { load(reference) }) else {
        return PARAM_ERR;
    };
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return FNF_ERR;
    };
    let result = match metadata.is_dir() {
        true => std::fs::remove_dir(&path),
        false => std::fs::remove_file(&path),
    };
    match result {
        Ok(()) => NO_ERR,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => FNF_ERR,
        Err(e) if e.raw_os_error() == Some(libc::ENOTEMPTY) => FBSY_ERR,
        Err(_) => IO_ERR,
    }
}

/// Copies `source` to `dest`, recursing into a directory. `skip_perm_errors`
/// is `kFSFileOperationSkipSourcePermissionErrors`: a denied entry is left
/// out of the copy instead of failing the whole tree.
///
/// FIXME (ACCURACY): a skipped entry is silent. Apple's own implementation
/// can report which entries it left out; this one does not keep a list.
fn copy_recursive(
    source: &std::path::Path,
    dest: &std::path::Path,
    skip_perm_errors: bool,
) -> Result<(), OSStatus> {
    let metadata = std::fs::symlink_metadata(source).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => FNF_ERR,
        std::io::ErrorKind::PermissionDenied => PERM_ERR,
        _ => IO_ERR,
    })?;
    if metadata.is_dir() {
        std::fs::create_dir(dest).map_err(|_| IO_ERR)?;
        for entry in std::fs::read_dir(source).map_err(|_| IO_ERR)?.flatten() {
            let child = dest.join(entry.file_name());
            if let Err(status) = copy_recursive(&entry.path(), &child, skip_perm_errors)
                && !(skip_perm_errors && status == PERM_ERR)
            {
                return Err(status);
            }
        }
        return Ok(());
    }
    if metadata.is_symlink() {
        let link = std::fs::read_link(source).map_err(|_| IO_ERR)?;
        return std::os::unix::fs::symlink(&link, dest).map_err(|_| IO_ERR);
    }
    std::fs::copy(source, dest).map(|_| ()).map_err(|e| {
        match e.kind() == std::io::ErrorKind::PermissionDenied {
            true => PERM_ERR,
            false => IO_ERR,
        }
    })
}

/// `OSStatus FSCopyObjectSync(const FSRef *source, const FSRef *destDir,
/// CFStringRef destName, FSRef *target, OptionBits options)`
///
/// # Safety
///
/// `source` and `dest_dir` are whole `FSRef`s; `dest_name` is null or a
/// `CFStringRef`; `target` is null or storage for a whole `FSRef`.
pub unsafe extern "C" fn FSCopyObjectSync(
    source: *const c_void,
    dest_dir: *const c_void,
    dest_name: *const c_void,
    target: *mut c_void,
    options: u32,
) -> OSStatus {
    const OVERWRITE: u32 = 0x01;
    const SKIP_SOURCE_PERMISSION_ERRORS: u32 = 0x02;
    // `kFSFileOperationDoNotMoveAcrossVolumes` is a move option a copy has
    // no use for, and `kFSFileOperationSkipPreflight` only skips a free-space
    // check this call never does anyway - both are accepted and ignored.
    let (Some(source_path), Some(dest_dir_path)) =
        (unsafe { load(source) }, unsafe { load(dest_dir) })
    else {
        return PARAM_ERR;
    };
    let name = if dest_name.is_null() {
        source_path.file_name().map(std::ffi::OsStr::to_os_string)
    } else {
        // SAFETY: the caller states a `CFStringRef`.
        unsafe { corefoundation::string::as_string(dest_name as corefoundation::base::CFTypeRef) }
            .map(|s| std::ffi::OsString::from(String::from_utf8_lossy(&s.utf8()).into_owned()))
    };
    let Some(name) = name else {
        return PARAM_ERR;
    };
    let dest_path = dest_dir_path.join(&name);
    if let Ok(existing) = std::fs::symlink_metadata(&dest_path) {
        if options & OVERWRITE == 0 {
            return DUP_FN_ERR;
        }
        let removed = match existing.is_dir() {
            true => std::fs::remove_dir_all(&dest_path),
            false => std::fs::remove_file(&dest_path),
        };
        if removed.is_err() {
            return IO_ERR;
        }
    }
    let skip_perm_errors = options & SKIP_SOURCE_PERMISSION_ERRORS != 0;
    if let Err(status) = copy_recursive(&source_path, &dest_path, skip_perm_errors) {
        return status;
    }
    if !target.is_null() {
        // SAFETY: the caller states it is a whole `FSRef`.
        unsafe { store(target, dest_path) };
    }
    NO_ERR
}

/// One `FSOpenIterator` snapshot: the host paths still to report, taken
/// eagerly at open time rather than discovered as the guest asks for more -
/// which is also why `containerChanged` in [`FSGetCatalogInfoBulk`] never has
/// anything to report.
struct CatalogIterator {
    pending: Mutex<std::collections::VecDeque<PathBuf>>,
}

/// `kFSIterateSubtree`: every descendant, not just the immediate children.
const ITERATE_SUBTREE: u32 = 1;
/// `kFSIterateDelete`: a stronger delivery guarantee under concurrent
/// mutation, which this eager snapshot already gives for free.
const ITERATE_DELETE: u32 = 2;

fn collect_flat(root: &std::path::Path, out: &mut std::collections::VecDeque<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(root) {
        out.extend(entries.flatten().map(|entry| entry.path()));
    }
}

fn collect_subtree(root: &std::path::Path, out: &mut std::collections::VecDeque<PathBuf>) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(path.clone());
            }
            out.push_back(path);
        }
    }
}

/// `OSErr FSOpenIterator(const FSRef *container, FSIteratorFlags
/// iteratorFlags, FSIterator *iterator)`
///
/// # Safety
///
/// `container` is a whole `FSRef`; `iterator` is a whole, writable pointer.
pub unsafe extern "C" fn FSOpenIterator(
    container: *const c_void,
    iterator_flags: u32,
    iterator: *mut *mut c_void,
) -> OSStatus {
    if iterator.is_null() {
        return PARAM_ERR;
    }
    if iterator_flags & !(ITERATE_SUBTREE | ITERATE_DELETE) != 0 {
        return BAD_ITERATOR_FLAGS;
    }
    let Some(root) = (unsafe { load(container) }) else {
        return PARAM_ERR;
    };
    if !root.is_dir() {
        return PARAM_ERR;
    }
    let mut pending = std::collections::VecDeque::new();
    match iterator_flags & ITERATE_SUBTREE != 0 {
        true => collect_subtree(&root, &mut pending),
        false => collect_flat(&root, &mut pending),
    }
    let boxed = Box::new(CatalogIterator {
        pending: Mutex::new(pending),
    });
    // SAFETY: checked non-null above.
    unsafe { iterator.write_unaligned(Box::into_raw(boxed) as *mut c_void) };
    NO_ERR
}

/// `OSErr FSCloseIterator(FSIterator iterator)`
///
/// # Safety
///
/// `iterator` is null or exactly what [`FSOpenIterator`] returned, not
/// already closed.
pub unsafe extern "C" fn FSCloseIterator(iterator: *mut c_void) -> OSStatus {
    if iterator.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller states this is exactly what `FSOpenIterator` made.
    unsafe { drop(Box::from_raw(iterator as *mut CatalogIterator)) };
    NO_ERR
}

/// `OSErr FSGetCatalogInfoBulk(FSIterator iterator, ItemCount
/// maximumObjects, ItemCount *actualObjects, Boolean *containerChanged,
/// FSCatalogInfoBitmap whichInfo, FSCatalogInfo *catalogInfos, FSRef *refs,
/// FSSpecPtr specs, HFSUniStr255 *names)`
///
/// `specs` is Apple's own documented no-op on 64-bit - an `FSSpec` has no
/// 64-bit meaning - so it is accepted and ignored rather than refused the
/// way [`FSGetCatalogInfo`] refuses one.
///
/// # Safety
///
/// `iterator` is exactly what [`FSOpenIterator`] returned; every non-null
/// output is writable for `maximum_objects` of its own type.
pub unsafe extern "C" fn FSGetCatalogInfoBulk(
    iterator: *mut c_void,
    maximum_objects: u64,
    actual_objects: *mut u64,
    container_changed: *mut u8,
    which: u32,
    catalog_infos: *mut CatalogInfo,
    refs: *mut c_void,
    _specs: *mut c_void,
    names: *mut UniStr255,
) -> OSStatus {
    if iterator.is_null() {
        return PARAM_ERR;
    }
    if which & !info::ANSWERED != 0 {
        eprintln!(
            "rosemary: coreservices: FSGetCatalogInfoBulk: {:#x} of the catalog bitmap is not \
             something this tree can answer",
            which & !info::ANSWERED
        );
        return PARAM_ERR;
    }
    // SAFETY: the caller states this is exactly what `FSOpenIterator` made.
    let state = unsafe { &*(iterator as *const CatalogIterator) };
    let mut pending = state.pending.lock().unwrap_or_else(|e| e.into_inner());
    let mut count = 0u64;
    while count < maximum_objects {
        let Some(path) = pending.pop_front() else {
            break;
        };
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            // Vanished between the listing and the read; left out rather
            // than failing the whole batch over one entry.
            continue;
        };
        if !catalog_infos.is_null() && which != 0 {
            let mut out = CatalogInfo::default();
            fill(&mut out, which, &path, &metadata);
            unsafe { catalog_infos.add(count as usize).write_unaligned(out) };
        }
        if !refs.is_null() {
            // SAFETY: the caller states storage for `maximum_objects` whole
            // `FSRef`s.
            unsafe {
                store(
                    refs.cast::<u8>().add(count as usize * FS_REF_BYTES).cast(),
                    path.clone(),
                )
            };
        }
        if !names.is_null() {
            let leaf: Vec<u16> = path
                .file_name()
                .map(|n| n.to_string_lossy().encode_utf16().take(255).collect())
                .unwrap_or_default();
            let mut leaf_name = UniStr255 {
                length: leaf.len() as u16,
                unicode: [0; 255],
            };
            leaf_name.unicode[..leaf.len()].copy_from_slice(&leaf);
            unsafe { names.add(count as usize).write_unaligned(leaf_name) };
        }
        count += 1;
    }
    if !actual_objects.is_null() {
        unsafe { actual_objects.write_unaligned(count) };
    }
    if !container_changed.is_null() {
        // FIXME (ACCURACY): always false. A real change after
        // `FSOpenIterator` snapshotted the listing goes unnoticed, where
        // Apple's own implementation would report it.
        unsafe { *container_changed = 0 };
    }
    match count {
        0 => NO_MORE_ITEMS,
        _ => NO_ERR,
    }
}

exports::exports! {
    functions {
        "FSCreateFileUnicode" => FSCreateFileUnicode,
        "FSFindFolder" => FSFindFolder,
        "FSPathMakeRef" => FSPathMakeRef,
        "FSRefMakePath" => FSRefMakePath,
        "FSCompareFSRefs" => FSCompareFSRefs,
        "FSIsAliasFile" => FSIsAliasFile,
        "FSResolveAliasFile" => FSResolveAliasFile,
        "FSGetCatalogInfo" => FSGetCatalogInfo,
        "FSSetCatalogInfo" => FSSetCatalogInfo,
        "FSGetVolumeInfo" => FSGetVolumeInfo,
        "FNNotify" => FNNotify,
        "FSOpenFork" => FSOpenFork,
        "FSCreateFork" => FSCreateFork,
        "FSCloseFork" => FSCloseFork,
        "FSReadFork" => FSReadFork,
        "FSWriteFork" => FSWriteFork,
        "FSGetForkPosition" => FSGetForkPosition,
        "FSSetForkPosition" => FSSetForkPosition,
        "FSGetForkSize" => FSGetForkSize,
        "FSSetForkSize" => FSSetForkSize,
        "FSGetForkCBInfo" => FSGetForkCBInfo,
        "FSIterateForks" => FSIterateForks,
        "FSGetDataForkName" => FSGetDataForkName,
        "FSOpenIterator" => FSOpenIterator,
        "FSCloseIterator" => FSCloseIterator,
        "FSGetCatalogInfoBulk" => FSGetCatalogInfoBulk,
        "FSCreateDirectoryUnicode" => FSCreateDirectoryUnicode,
        "FSMakeFSRefUnicode" => FSMakeFSRefUnicode,
        "PBMakeFSRefUnicodeSync" => PBMakeFSRefUnicodeSync,
        "FSRenameUnicode" => FSRenameUnicode,
        "FSMoveObject" => FSMoveObject,
        "FSDeleteObject" => FSDeleteObject,
        "FSCopyObjectSync" => FSCopyObjectSync,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference survives the round trip, and two for the same path compare
    /// equal - which is the whole contract a program depends on, since it can
    /// see nothing else about the eighty bytes.
    #[test]
    fn a_reference_round_trips_and_compares_equal() {
        let dir = std::env::temp_dir();
        let path = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        let mut first = [0_u8; FS_REF_BYTES];
        let mut second = [0_u8; FS_REF_BYTES];
        let mut is_dir = 0_u8;
        // SAFETY: a C string and storage for a whole reference.
        unsafe {
            assert_eq!(
                FSPathMakeRef(path.as_ptr().cast(), first.as_mut_ptr().cast(), &mut is_dir),
                NO_ERR
            );
            assert_eq!(
                FSPathMakeRef(
                    path.as_ptr().cast(),
                    second.as_mut_ptr().cast(),
                    std::ptr::null_mut()
                ),
                NO_ERR
            );
        }
        assert_eq!(is_dir, 1);
        let mut out = [0_u8; 1024];
        // SAFETY: a reference this test made, and a buffer with its own length.
        unsafe {
            assert_eq!(
                FSRefMakePath(first.as_ptr().cast(), out.as_mut_ptr(), out.len() as u32),
                NO_ERR
            );
            assert_eq!(
                FSCompareFSRefs(first.as_ptr().cast(), second.as_ptr().cast()),
                NO_ERR
            );
        }
        let end = out.iter().position(|b| *b == 0).unwrap();
        assert_eq!(&out[..end], dir.to_str().unwrap().as_bytes());
    }

    /// Unity creates its preferences file this way; a second creation is
    /// `dupFNErr`, not a truncation.
    #[test]
    fn a_created_file_exists_and_cannot_be_created_twice() {
        let dir = std::env::temp_dir().join(format!("rosemary-fscreate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut parent = [0_u8; FS_REF_BYTES];
        // SAFETY: storage for a whole reference.
        unsafe { store(parent.as_mut_ptr().cast(), dir.clone()) };
        let name: Vec<u16> = "made.plist".encode_utf16().collect();
        let create = |out: *mut c_void| {
            // SAFETY: a reference made above, a name of the length given, and
            // null or storage for a whole reference.
            unsafe {
                FSCreateFileUnicode(
                    parent.as_ptr().cast(),
                    name.len() as u64,
                    name.as_ptr(),
                    0,
                    std::ptr::null(),
                    out,
                    std::ptr::null_mut(),
                )
            }
        };
        let mut made = [0_u8; FS_REF_BYTES];
        assert_eq!(create(made.as_mut_ptr().cast()), NO_ERR);
        assert!(dir.join("made.plist").is_file());
        // SAFETY: the reference just filled in.
        assert_eq!(
            unsafe { load(made.as_ptr().cast()) },
            Some(dir.join("made.plist"))
        );
        assert_eq!(create(std::ptr::null_mut()), DUP_FN_ERR);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Storage the guest merely zeroed is not a reference, and is refused
    /// rather than read as index zero.
    #[test]
    fn zeroed_storage_is_not_a_reference() {
        let zero = [0_u8; FS_REF_BYTES];
        // SAFETY: readable storage of the right size.
        assert_eq!(
            unsafe { FSCompareFSRefs(zero.as_ptr().cast(), zero.as_ptr().cast()) },
            PARAM_ERR
        );
    }

    /// `kOnAppropriateDisk` frameworks are the system's, which is where
    /// `System.framework` is; preferences and the other per-user folders are
    /// the user's; every other kind stays on the local domain.
    #[test]
    fn system_disk_frameworks_are_the_system_domains() {
        const K_ON_SYSTEM_DISK: i16 = -32768;
        const K_ON_APPROPRIATE_DISK: i16 = -32767;
        const K_SYSTEM_DOMAIN: i16 = -32766;
        const K_USER_DOMAIN: i16 = -32763;
        let fram = u32::from_be_bytes(*b"fram");
        let pref = u32::from_be_bytes(*b"pref");
        let asup = u32::from_be_bytes(*b"asup");
        assert_eq!(system_disk(K_ON_APPROPRIATE_DISK, fram), K_SYSTEM_DOMAIN);
        assert_eq!(system_disk(K_ON_SYSTEM_DISK, fram), K_SYSTEM_DOMAIN);
        assert_eq!(system_disk(K_ON_SYSTEM_DISK, pref), K_USER_DOMAIN);
        assert_eq!(system_disk(K_ON_APPROPRIATE_DISK, pref), K_USER_DOMAIN);
        assert_eq!(
            system_disk(K_ON_APPROPRIATE_DISK, asup),
            K_ON_APPROPRIATE_DISK
        );
        assert_eq!(
            domain_root(system_disk(K_ON_APPROPRIATE_DISK, fram)).as_deref(),
            Some("/System")
        );
    }

    /// The user's own Application Support is where `NSSearchPathForDirectories`
    /// would have put it, because a program writes through one and reads
    /// through the other.
    #[test]
    fn finds_the_users_application_support() {
        const K_USER_DOMAIN: i16 = -32763;
        let mut reference = [0_u8; FS_REF_BYTES];
        // SAFETY: storage for a whole reference.
        let status = unsafe {
            FSFindFolder(
                K_USER_DOMAIN,
                u32::from_be_bytes(*b"asup"),
                1,
                reference.as_mut_ptr().cast(),
            )
        };
        assert_eq!(status, NO_ERR as i16);
        // SAFETY: a reference the call above just made.
        let path = unsafe { load(reference.as_ptr().cast()) }.unwrap();
        assert_eq!(
            path,
            prefix::ns::host_path(&format!(
                "{}/Library/Application Support",
                foundation::home_directory()
            ))
        );
    }

    /// A code this tree cannot place is a "not found", not a guess.
    #[test]
    fn an_unknown_folder_type_is_not_found() {
        let mut reference = [0_u8; FS_REF_BYTES];
        // SAFETY: storage for a whole reference.
        let status = unsafe {
            FSFindFolder(
                -32763,
                u32::from_be_bytes(*b"zzzz"),
                0,
                reference.as_mut_ptr().cast(),
            )
        };
        assert_eq!(status, FNF_ERR as i16);
    }

    /// Made, written, read back and shrunk by one call each, and the data
    /// fork is the only one there is to open.
    #[test]
    fn a_fork_round_trips_through_write_read_and_resize() {
        let dir = std::env::temp_dir().join(format!("rosemary-fork-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.bin");
        std::fs::write(&file, b"").unwrap();
        let mut reference = [0_u8; FS_REF_BYTES];
        let cpath = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
        // SAFETY: a C string and storage for a whole reference.
        unsafe {
            FSPathMakeRef(
                cpath.as_ptr().cast(),
                reference.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };

        let mut fork_ref_num = 0_i32;
        // SAFETY: a reference made above, no fork name (the data fork),
        // fsRdWrPerm, and storage for the refnum.
        let open = unsafe {
            FSOpenFork(
                reference.as_ptr().cast(),
                0,
                std::ptr::null(),
                3,
                &mut fork_ref_num,
            )
        };
        assert_eq!(open, NO_ERR);

        let payload = b"hello fork";
        let mut written = 0_u64;
        // SAFETY: a fork this test opened, and a buffer of its own length.
        let write = unsafe {
            FSWriteFork(
                fork_ref_num,
                1, // fsFromStart
                0,
                payload.len() as u64,
                payload.as_ptr(),
                &mut written,
            )
        };
        assert_eq!(write, NO_ERR);
        assert_eq!(written, payload.len() as u64);

        let mut size = 0_i64;
        // SAFETY: the fork opened above.
        assert_eq!(unsafe { FSGetForkSize(fork_ref_num, &mut size) }, NO_ERR);
        assert_eq!(size, payload.len() as i64);

        let mut read_back = [0_u8; 32];
        let mut actual = 0_u64;
        // SAFETY: storage with its own length.
        let read = unsafe {
            FSReadFork(
                fork_ref_num,
                1,
                0,
                read_back.len() as u64,
                read_back.as_mut_ptr(),
                &mut actual,
            )
        };
        // Asking for more than the fork holds is a short read: `eofErr`,
        // with `actual` still telling the truth about what came back.
        assert_eq!(read, EOF_ERR);
        assert_eq!(actual, payload.len() as u64);
        assert_eq!(&read_back[..actual as usize], payload);

        // fsFromMark after that read is at end of fork; shrink it there.
        assert_eq!(FSSetForkSize(fork_ref_num, 1, 4), NO_ERR);
        assert_eq!(unsafe { FSGetForkSize(fork_ref_num, &mut size) }, NO_ERR);
        assert_eq!(size, 4);

        assert_eq!(FSCloseFork(fork_ref_num), NO_ERR);
        // A refnum already closed names nothing.
        assert_eq!(FSCloseFork(fork_ref_num), RF_NUM_ERR);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The only fork this tree has is the data fork - the empty name; asking
    /// for any other is `errFSForkNotFound`, the same answer Apple gives for
    /// a fork that genuinely is not there.
    #[test]
    fn a_named_fork_is_not_found() {
        let dir = std::env::temp_dir();
        let cpath = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        let mut reference = [0_u8; FS_REF_BYTES];
        unsafe {
            FSPathMakeRef(
                cpath.as_ptr().cast(),
                reference.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };
        let name: Vec<u16> = "RESOURCE_FORK".encode_utf16().collect();
        let mut fork_ref_num = 0_i32;
        let status = unsafe {
            FSOpenFork(
                reference.as_ptr().cast(),
                name.len() as u64,
                name.as_ptr(),
                1,
                &mut fork_ref_num,
            )
        };
        assert_eq!(status, FORK_NOT_FOUND);
        assert_eq!(
            unsafe { FSCreateFork(reference.as_ptr().cast(), name.len() as u64, name.as_ptr()) },
            FORK_NOT_FOUND
        );
    }

    /// The data fork's name is the empty string - Apple's own answer.
    #[test]
    fn the_data_fork_name_is_empty() {
        let mut name = UniStr255 {
            length: 0xffff,
            unicode: [1; 255],
        };
        assert_eq!(unsafe { FSGetDataForkName(&mut name) }, NO_ERR);
        assert_eq!(name.length, 0);
    }

    /// There is one fork to report - the data fork - and then the iterator
    /// is spent, on a fresh `CatPositionRec` or a directory alike.
    #[test]
    fn iterate_forks_reports_the_data_fork_once() {
        let dir = std::env::temp_dir().join(format!("rosemary-iterfork-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"abcde").unwrap();
        let mut reference = [0_u8; FS_REF_BYTES];
        let cpath = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
        unsafe {
            FSPathMakeRef(
                cpath.as_ptr().cast(),
                reference.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };

        let mut state = CatPosition::default();
        let mut size = -1_i64;
        let mut physical = 0_u64;
        let mut name = UniStr255 {
            length: 9,
            unicode: [9; 255],
        };
        let first = unsafe {
            FSIterateForks(
                reference.as_ptr().cast(),
                &mut state,
                &mut name,
                &mut size,
                &mut physical,
            )
        };
        assert_eq!(first, NO_ERR);
        assert_eq!(size, 5);
        assert_eq!(name.length, 0);

        let second = unsafe {
            FSIterateForks(
                reference.as_ptr().cast(),
                &mut state,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(second, NO_MORE_ITEMS);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A flat iterator lists exactly a directory's immediate children, and
    /// is spent once they have all been returned.
    #[test]
    fn catalog_info_bulk_lists_a_directorys_children() {
        let dir = std::env::temp_dir().join(format!("rosemary-bulk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("one.txt"), b"1").unwrap();
        std::fs::write(dir.join("two.txt"), b"22").unwrap();
        let mut reference = [0_u8; FS_REF_BYTES];
        let cpath = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        unsafe {
            FSPathMakeRef(
                cpath.as_ptr().cast(),
                reference.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };

        let mut iterator: *mut c_void = std::ptr::null_mut();
        assert_eq!(
            unsafe { FSOpenIterator(reference.as_ptr().cast(), 0, &mut iterator) },
            NO_ERR
        );
        assert!(!iterator.is_null());

        let mut names = [
            UniStr255 {
                length: 0,
                unicode: [0; 255],
            },
            UniStr255 {
                length: 0,
                unicode: [0; 255],
            },
        ];
        let mut actual = 0_u64;
        let mut changed = 0xff_u8;
        let status = unsafe {
            FSGetCatalogInfoBulk(
                iterator,
                2,
                &mut actual,
                &mut changed,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                names.as_mut_ptr(),
            )
        };
        assert_eq!(status, NO_ERR);
        assert_eq!(actual, 2);
        assert_eq!(changed, 0);
        let mut seen: Vec<String> = names
            .iter()
            .map(|n| String::from_utf16(&n.unicode[..n.length as usize]).unwrap())
            .collect();
        seen.sort();
        assert_eq!(seen, ["one.txt", "two.txt"]);

        let exhausted = unsafe {
            FSGetCatalogInfoBulk(
                iterator,
                2,
                &mut actual,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(exhausted, NO_MORE_ITEMS);
        assert_eq!(unsafe { FSCloseIterator(iterator) }, NO_ERR);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Renamed within its directory, then moved to another one, then
    /// deleted - each call's `newRef` feeding the next.
    #[test]
    fn rename_move_and_delete_an_object() {
        let dir = std::env::temp_dir().join(format!("rosemary-renmv-{}", std::process::id()));
        let other =
            std::env::temp_dir().join(format!("rosemary-renmv-dest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let file = dir.join("original.txt");
        std::fs::write(&file, b"x").unwrap();

        let mut reference = [0_u8; FS_REF_BYTES];
        let cpath = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
        unsafe {
            FSPathMakeRef(
                cpath.as_ptr().cast(),
                reference.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };

        let new_name: Vec<u16> = "renamed.txt".encode_utf16().collect();
        let mut renamed = [0_u8; FS_REF_BYTES];
        let rename = unsafe {
            FSRenameUnicode(
                reference.as_ptr().cast(),
                new_name.len() as u64,
                new_name.as_ptr(),
                0,
                renamed.as_mut_ptr().cast(),
            )
        };
        assert_eq!(rename, NO_ERR);
        assert!(dir.join("renamed.txt").is_file());
        assert!(!file.exists());

        let mut dest_dir_ref = [0_u8; FS_REF_BYTES];
        let other_cpath = std::ffi::CString::new(other.to_str().unwrap()).unwrap();
        unsafe {
            FSPathMakeRef(
                other_cpath.as_ptr().cast(),
                dest_dir_ref.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };

        let mut moved = [0_u8; FS_REF_BYTES];
        let move_status = unsafe {
            FSMoveObject(
                renamed.as_ptr().cast(),
                dest_dir_ref.as_ptr().cast(),
                moved.as_mut_ptr().cast(),
            )
        };
        assert_eq!(move_status, NO_ERR);
        assert!(other.join("renamed.txt").is_file());

        assert_eq!(unsafe { FSDeleteObject(moved.as_ptr().cast()) }, NO_ERR);
        assert!(!other.join("renamed.txt").exists());

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
    }

    /// A whole directory tree, copied recursively; copying it again without
    /// `kFSFileOperationOverwrite` is a duplicate name, and with it replaces
    /// what is there.
    #[test]
    fn copy_object_sync_copies_a_tree_and_refuses_to_overwrite() {
        let src = std::env::temp_dir().join(format!("rosemary-copysrc-{}", std::process::id()));
        let dest_parent =
            std::env::temp_dir().join(format!("rosemary-copydest-{}", std::process::id()));
        std::fs::create_dir_all(src.join("inner")).unwrap();
        std::fs::write(src.join("inner/leaf.txt"), b"leaf").unwrap();
        std::fs::create_dir_all(&dest_parent).unwrap();

        let mut source_ref = [0_u8; FS_REF_BYTES];
        let source_c = std::ffi::CString::new(src.to_str().unwrap()).unwrap();
        unsafe {
            FSPathMakeRef(
                source_c.as_ptr().cast(),
                source_ref.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };
        let mut dest_ref = [0_u8; FS_REF_BYTES];
        let dest_c = std::ffi::CString::new(dest_parent.to_str().unwrap()).unwrap();
        unsafe {
            FSPathMakeRef(
                dest_c.as_ptr().cast(),
                dest_ref.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };

        let mut target = [0_u8; FS_REF_BYTES];
        let first = unsafe {
            FSCopyObjectSync(
                source_ref.as_ptr().cast(),
                dest_ref.as_ptr().cast(),
                std::ptr::null(),
                target.as_mut_ptr().cast(),
                0,
            )
        };
        assert_eq!(first, NO_ERR);
        let copy_root = dest_parent.join(src.file_name().unwrap());
        assert_eq!(
            std::fs::read(copy_root.join("inner/leaf.txt")).unwrap(),
            b"leaf"
        );

        let duplicate = unsafe {
            FSCopyObjectSync(
                source_ref.as_ptr().cast(),
                dest_ref.as_ptr().cast(),
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(duplicate, DUP_FN_ERR);

        const OVERWRITE: u32 = 0x01;
        let overwritten = unsafe {
            FSCopyObjectSync(
                source_ref.as_ptr().cast(),
                dest_ref.as_ptr().cast(),
                std::ptr::null(),
                std::ptr::null_mut(),
                OVERWRITE,
            )
        };
        assert_eq!(overwritten, NO_ERR);

        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&dest_parent).unwrap();
    }

    /// A directory made by name, then resolved back by the same name - the
    /// two references name the same object.
    #[test]
    fn create_directory_unicode_and_resolve_it_by_name() {
        let dir = std::env::temp_dir().join(format!("rosemary-mkdiru-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut parent = [0_u8; FS_REF_BYTES];
        let cpath = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        unsafe {
            FSPathMakeRef(
                cpath.as_ptr().cast(),
                parent.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };

        let name: Vec<u16> = "child".encode_utf16().collect();
        let mut new_ref = [0_u8; FS_REF_BYTES];
        let mut new_dir_id = 0_u32;
        let create = unsafe {
            FSCreateDirectoryUnicode(
                parent.as_ptr().cast(),
                name.len() as u64,
                name.as_ptr(),
                0,
                std::ptr::null(),
                new_ref.as_mut_ptr().cast(),
                std::ptr::null_mut(),
                &mut new_dir_id,
            )
        };
        assert_eq!(create, NO_ERR);
        assert!(dir.join("child").is_dir());
        assert_ne!(new_dir_id, 0);

        let mut resolved = [0_u8; FS_REF_BYTES];
        let resolve = unsafe {
            FSMakeFSRefUnicode(
                parent.as_ptr().cast(),
                name.len() as u64,
                name.as_ptr(),
                0,
                resolved.as_mut_ptr().cast(),
            )
        };
        assert_eq!(resolve, NO_ERR);
        assert_eq!(
            unsafe { FSCompareFSRefs(new_ref.as_ptr().cast(), resolved.as_ptr().cast()) },
            NO_ERR
        );

        let missing: Vec<u16> = "ghost".encode_utf16().collect();
        let mut missing_ref = [0_u8; FS_REF_BYTES];
        let missing_status = unsafe {
            FSMakeFSRefUnicode(
                parent.as_ptr().cast(),
                missing.len() as u64,
                missing.as_ptr(),
                0,
                missing_ref.as_mut_ptr().cast(),
            )
        };
        assert_eq!(missing_status, FNF_ERR);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The parameter-block form answers the same `FSRef` as the plain call,
    /// and leaves its answer in `ioResult` as well as returning it.
    #[test]
    fn pb_make_fs_ref_unicode_sync_matches_the_plain_call() {
        let dir = std::env::temp_dir().join(format!("rosemary-pbref-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("here.txt"), b"x").unwrap();
        let mut parent = [0_u8; FS_REF_BYTES];
        let cpath = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        unsafe {
            FSPathMakeRef(
                cpath.as_ptr().cast(),
                parent.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };

        let name: Vec<u16> = "here.txt".encode_utf16().collect();
        let mut new_ref = [0_u8; FS_REF_BYTES];
        let mut block = RefParam {
            reference: parent.as_ptr() as u64,
            name_length: name.len() as u64,
            name: name.as_ptr() as u64,
            new_ref: new_ref.as_mut_ptr() as u64,
            ..Default::default()
        };

        let status = unsafe { PBMakeFSRefUnicodeSync((&mut block as *mut RefParam).cast()) };
        assert_eq!(status, NO_ERR);
        assert_eq!(block.io_result as i32, NO_ERR);

        let mut direct = [0_u8; FS_REF_BYTES];
        unsafe {
            FSMakeFSRefUnicode(
                parent.as_ptr().cast(),
                name.len() as u64,
                name.as_ptr(),
                0,
                direct.as_mut_ptr().cast(),
            )
        };
        assert_eq!(
            unsafe { FSCompareFSRefs(new_ref.as_ptr().cast(), direct.as_ptr().cast()) },
            NO_ERR
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Asking about an open fork by its refnum, and by walking the open
    /// forks from zero, answer the same thing.
    #[test]
    fn get_fork_cb_info_answers_by_refnum_and_by_iteration() {
        let dir = std::env::temp_dir().join(format!("rosemary-cbinfo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.bin");
        std::fs::write(&file, b"cbinfo").unwrap();
        let mut reference = [0_u8; FS_REF_BYTES];
        let cpath = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
        unsafe {
            FSPathMakeRef(
                cpath.as_ptr().cast(),
                reference.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };

        let mut fork_ref_num = 0_i32;
        unsafe {
            FSOpenFork(
                reference.as_ptr().cast(),
                0,
                std::ptr::null(),
                1,
                &mut fork_ref_num,
            )
        };

        let mut info = ForkInfo::default();
        let mut found_ref = [0_u8; FS_REF_BYTES];
        let by_refnum = unsafe {
            FSGetForkCBInfo(
                fork_ref_num,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut info,
                found_ref.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(by_refnum, NO_ERR);
        let logical_eof = info.logical_eof;
        assert_eq!(logical_eof, 6);
        assert_eq!(
            unsafe { FSCompareFSRefs(reference.as_ptr().cast(), found_ref.as_ptr().cast()) },
            NO_ERR
        );

        // Seeded at this fork's own slot rather than zero, so a fork another
        // test has open concurrently cannot make this one flaky.
        let mut iterator = (fork_ref_num - 1) as i16;
        let mut actual_ref_num = 0_i32;
        let by_walk = unsafe {
            FSGetForkCBInfo(
                0,
                0,
                &mut iterator,
                &mut actual_ref_num,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(by_walk, NO_ERR);
        assert_eq!(actual_ref_num, fork_ref_num);

        assert_eq!(FSCloseFork(fork_ref_num), NO_ERR);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// `FSCatalogInfo`, whose layout is ABI: a guest declares one on its stack and
/// reads the fields back by name.
///
/// Written out in full even though only part of it is ever filled, because the
/// *offsets* are what the guest reads at - a field left out would move every
/// field after it.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CatalogInfo {
    node_flags: u16,
    volume: i16,
    parent_dir_id: u32,
    node_id: u32,
    sharing_flags: u8,
    user_privileges: u8,
    _reserved1: u8,
    _reserved2: u8,
    create_date: UtcDateTime,
    content_mod_date: UtcDateTime,
    attribute_mod_date: UtcDateTime,
    access_date: UtcDateTime,
    backup_date: UtcDateTime,
    permissions: Permissions,
    finder_info: [u8; 16],
    ext_finder_info: [u8; 16],
    data_logical_size: u64,
    data_physical_size: u64,
    rsrc_logical_size: u64,
    rsrc_physical_size: u64,
    valence: u32,
    text_encoding: u32,
    _reserved3: i16,
    _reserved4: i16,
}

/// `UTCDateTime`: seconds since 1904, split the way the old File Manager
/// counted them, and a fraction of 1/65536 of a second.
///
/// Packed to two bytes, which is what `<MacTypes.h>` declares it under and what
/// makes it eight bytes rather than twelve. Getting that wrong moves every
/// field of [`CatalogInfo`] after the first date.
#[repr(C, packed(2))]
#[derive(Clone, Copy, Default)]
pub(crate) struct UtcDateTime {
    high_seconds: u16,
    low_seconds: u32,
    fraction: u16,
}

/// `FSPermissionInfo`, which is `kFSCatInfoPermissions`' sixteen bytes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Permissions {
    user_id: u32,
    group_id: u32,
    _reserved1: u8,
    user_access: u8,
    mode: u16,
    _reserved2: u32,
}

const _: () = {
    assert!(size_of::<UtcDateTime>() == 8);
    assert!(size_of::<Permissions>() == 16);
    assert!(std::mem::offset_of!(CatalogInfo, create_date) == 16);
    assert!(std::mem::offset_of!(CatalogInfo, permissions) == 56);
    assert!(std::mem::offset_of!(CatalogInfo, data_logical_size) == 104);
    assert!(std::mem::offset_of!(CatalogInfo, valence) == 136);
    assert!(size_of::<CatalogInfo>() == 152);
};

/// `HFSUniStr255`: a length and 255 UTF-16 units, which is what `outName` is.
#[repr(C)]
pub struct UniStr255 {
    length: u16,
    unicode: [u16; 255],
}

const _: () = assert!(size_of::<UniStr255>() == 512);

/// `kFSCatInfo*`, the bits of the bitmap.
mod info {
    pub const TEXT_ENCODING: u32 = 0x0000_0001;
    pub const NODE_FLAGS: u32 = 0x0000_0002;
    pub const VOLUME: u32 = 0x0000_0004;
    pub const PARENT_DIR_ID: u32 = 0x0000_0008;
    pub const NODE_ID: u32 = 0x0000_0010;
    pub const CREATE_DATE: u32 = 0x0000_0020;
    pub const CONTENT_MOD: u32 = 0x0000_0040;
    pub const ATTR_MOD: u32 = 0x0000_0080;
    pub const ACCESS_DATE: u32 = 0x0000_0100;
    pub const BACKUP_DATE: u32 = 0x0000_0200;
    pub const PERMISSIONS: u32 = 0x0000_0400;
    pub const FINDER_INFO: u32 = 0x0000_0800;
    pub const FINDER_X_INFO: u32 = 0x0000_1000;
    pub const VALENCE: u32 = 0x0000_2000;
    pub const DATA_SIZES: u32 = 0x0000_4000;
    pub const RSRC_SIZES: u32 = 0x0000_8000;
    pub const SHARING_FLAGS: u32 = 0x0001_0000;
    pub const USER_PRIVS: u32 = 0x0002_0000;
    /// Everything the walk below knows how to answer.
    pub const ANSWERED: u32 = TEXT_ENCODING
        | NODE_FLAGS
        | VOLUME
        | PARENT_DIR_ID
        | NODE_ID
        | CREATE_DATE
        | CONTENT_MOD
        | ATTR_MOD
        | ACCESS_DATE
        | BACKUP_DATE
        | PERMISSIONS
        | FINDER_INFO
        | FINDER_X_INFO
        | VALENCE
        | DATA_SIZES
        | RSRC_SIZES
        | SHARING_FLAGS
        | USER_PRIVS;
    /// What [`super::FSSetCatalogInfo`] can act on: the lock bit, the
    /// permissions, and the two dates a POSIX file lets anyone but the kernel
    /// change. The Finder info, the valence, the sizes and the other two dates
    /// are facts of the file this tree reads rather than fields it can write.
    pub const SETTABLE: u32 = NODE_FLAGS | PERMISSIONS | CONTENT_MOD | ACCESS_DATE;
}

/// `kFSNodeIsDirectoryMask`.
const NODE_IS_DIRECTORY: u16 = 0x0010;
/// `kFSNodeLockedMask`.
const NODE_LOCKED: u16 = 0x0001;

/// Seconds from 1904-01-01 - the File Manager's epoch - to the Unix one.
const UNIX_TO_1904: i64 = 2_082_844_800;

impl UtcDateTime {
    /// A Unix timestamp in the File Manager's own counting.
    pub(crate) fn from_unix(seconds: i64) -> Self {
        let since = seconds.saturating_add(UNIX_TO_1904).max(0) as u64;
        UtcDateTime {
            high_seconds: (since >> 32) as u16,
            low_seconds: since as u32,
            fraction: 0,
        }
    }
}

/// The `FSVolumeRefNum` for a device.
///
/// Interned the way paths are, because a volume number has to be the same on
/// every question about the same filesystem: a caller compares two of them to
/// decide whether a move is a rename or a copy. Negative and starting at -100,
/// which is the range a real one is in - a positive number there would read as
/// a drive number from the days when there were two floppies.
fn volume_of(device: u64, seen_at: &std::path::Path) -> i16 {
    let mut held = VOLUMES.lock().unwrap_or_else(|e| e.into_inner());
    let table = volumes(&mut held);
    let index = match table.iter().position(|known| known.device == device) {
        Some(index) => index,
        None => {
            table.push(Volume {
                device,
                root: mount_point(seen_at, device),
            });
            table.len() - 1
        }
    };
    -100_i16.saturating_sub(i16::try_from(index).unwrap_or(0))
}

/// One filesystem, as the volume calls see it.
struct Volume {
    device: u64,
    /// Where it is mounted, which is what `FSGetVolumeInfo` hands back as the
    /// root directory and takes the volume's name from.
    root: PathBuf,
}

static VOLUMES: Mutex<Option<Vec<Volume>>> = Mutex::new(None);

/// The table, with the root filesystem always first.
///
/// Seeded rather than discovered, so that the boot volume has the same number
/// whichever question is asked first and so that a caller walking the volumes
/// by index finds one at index 1 - which is where the walk starts.
fn volumes(held: &mut Option<Vec<Volume>>) -> &mut Vec<Volume> {
    use std::os::unix::fs::MetadataExt;
    let table = held.get_or_insert_with(Vec::new);
    if table.is_empty()
        && let Ok(metadata) = std::fs::metadata("/")
    {
        table.push(Volume {
            device: metadata.dev(),
            root: PathBuf::from("/"),
        });
    }
    table
}

/// The directory `path` is mounted at: the highest ancestor still on `device`.
///
/// Walked rather than read out of `/proc/mounts`, because the walk is the
/// definition - a mount point is exactly where the device number changes - and
/// it needs no second view of the filesystem to be kept in step with the first.
fn mount_point(path: &std::path::Path, device: u64) -> PathBuf {
    use std::os::unix::fs::MetadataExt;
    let mut at = path.to_path_buf();
    while let Some(parent) = at.parent().map(PathBuf::from) {
        match std::fs::metadata(&parent) {
            Ok(metadata) if metadata.dev() == device => at = parent,
            _ => break,
        }
    }
    at
}

/// The volume a reference number names, as `(number, root)`.
fn volume_by_number(number: i16) -> Option<(i16, PathBuf)> {
    let mut held = VOLUMES.lock().unwrap_or_else(|e| e.into_inner());
    let table = volumes(&mut held);
    let index = usize::try_from(-100_i16.saturating_sub(number)).ok()?;
    table.get(index).map(|volume| (number, volume.root.clone()))
}

/// The volume at a one-based index, in the order they were first seen.
fn volume_by_index(index: u64) -> Option<(i16, PathBuf)> {
    let mut held = VOLUMES.lock().unwrap_or_else(|e| e.into_inner());
    let table = volumes(&mut held);
    let at = usize::try_from(index.checked_sub(1)?).ok()?;
    let volume = table.get(at)?;
    Some((
        -100_i16.saturating_sub(i16::try_from(at).unwrap_or(0)),
        volume.root.clone(),
    ))
}

/// `OSErr FSGetVolumeInfo(FSVolumeRefNum volume, ItemCount volumeIndex,
/// FSVolumeRefNum *actualVolume, FSVolumeInfoBitmap whichInfo,
/// FSVolumeInfo *info, HFSUniStr255 *volumeName, FSRef *rootDirectory)`
///
/// `whichInfo` is refused unless it is `kFSVolInfoNone`: `FSVolumeInfo` is a
/// structure of block counts, HFS signatures and a Finder info block, and a
/// filled-in one would be a description of a volume format this host has not
/// got. The three outputs beside it - which volume it was, what it is called,
/// and where its root is - are real questions and are answered.
///
/// # Safety
///
/// Each non-null output is writable for its own type.
pub unsafe extern "C" fn FSGetVolumeInfo(
    volume: i16,
    volume_index: u64,
    actual_volume: *mut i16,
    which: u32,
    info: *mut c_void,
    volume_name: *mut UniStr255,
    root_directory: *mut c_void,
) -> i16 {
    if which != 0 || !info.is_null() {
        eprintln!(
            "rosemary: coreservices: FSGetVolumeInfo was asked for FSVolumeInfo ({which:#x}), \
             which describes a volume format this host has not got"
        );
        return PARAM_ERR as i16;
    }
    // A volume reference number names one; a zero one means "the volumeIndex-th",
    // which is how a caller walks the mounted volumes.
    let found = match volume {
        0 => volume_by_index(volume_index),
        number => volume_by_number(number),
    };
    let Some((number, root)) = found else {
        // `nsvErr`: no such volume.
        return -35;
    };
    if !actual_volume.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *actual_volume = number };
    }
    if !volume_name.is_null() {
        // The mount point's own name, and "/" for the root - which is what this
        // filesystem is called here, where a volume has no name of its own.
        let leaf = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".to_string());
        let units: Vec<u16> = leaf.encode_utf16().take(255).collect();
        let mut name = UniStr255 {
            length: units.len() as u16,
            unicode: [0; 255],
        };
        name.unicode[..units.len()].copy_from_slice(&units);
        unsafe { volume_name.write_unaligned(name) };
    }
    if !root_directory.is_null() {
        // SAFETY: the caller states it is storage for a whole `FSRef`.
        unsafe { store(root_directory, root) };
    }
    NO_ERR as i16
}

/// `OSErr FSGetCatalogInfo(const FSRef *ref, FSCatalogInfoBitmap whichInfo,
/// FSCatalogInfo *catalogInfo, HFSUniStr255 *outName, FSSpec *fsSpec,
/// FSRef *parentRef)`
///
/// A bit of the bitmap this tree cannot answer is refused by name rather than
/// left as the zero the caller's storage happened to hold: the whole point of
/// the bitmap is that the caller reads back exactly the fields it asked for.
///
/// `fsSpec` is the one output that is always refused. An `FSSpec` names a file
/// by a volume, a directory id and a 63-byte Pascal name, which is a naming
/// scheme this tree does not have and cannot invent one entry of.
///
/// # Safety
///
/// `reference` is a whole `FSRef`; each non-null output is writable for its own
/// type.
pub unsafe extern "C" fn FSGetCatalogInfo(
    reference: *const c_void,
    which: u32,
    catalog_info: *mut CatalogInfo,
    out_name: *mut UniStr255,
    fs_spec: *mut c_void,
    parent_ref: *mut c_void,
) -> i16 {
    let Some(path) = (unsafe { load(reference) }) else {
        return PARAM_ERR as i16;
    };
    if !fs_spec.is_null() {
        eprintln!(
            "rosemary: coreservices: FSGetCatalogInfo was asked for an FSSpec, which names a \
             file by a volume and a directory id this tree has not got"
        );
        return PARAM_ERR as i16;
    }
    if which & !info::ANSWERED != 0 {
        eprintln!(
            "rosemary: coreservices: FSGetCatalogInfo: {:#x} of the catalog bitmap is not \
             something this tree can answer",
            which & !info::ANSWERED
        );
        return PARAM_ERR as i16;
    }
    let Ok(metadata) = std::fs::symlink_metadata(&path) else {
        return FNF_ERR as i16;
    };
    if !catalog_info.is_null() && which != 0 {
        let mut out = CatalogInfo::default();
        fill(&mut out, which, &path, &metadata);
        // SAFETY: the caller states it is writable. Unaligned because the
        // guest's storage is wherever it declared it.
        unsafe { catalog_info.write_unaligned(out) };
    }
    if !out_name.is_null() {
        let leaf: Vec<u16> = path
            .file_name()
            .map(|name| name.to_string_lossy().encode_utf16().take(255).collect())
            .unwrap_or_default();
        let mut name = UniStr255 {
            length: leaf.len() as u16,
            unicode: [0; 255],
        };
        name.unicode[..leaf.len()].copy_from_slice(&leaf);
        unsafe { out_name.write_unaligned(name) };
    }
    if !parent_ref.is_null() {
        let Some(parent) = path.parent().map(PathBuf::from) else {
            return FNF_ERR as i16;
        };
        // SAFETY: the caller states it is storage for a whole `FSRef`.
        unsafe { store(parent_ref, parent) };
    }
    NO_ERR as i16
}

/// Fill the fields `which` asks for. Every one is derived from the POSIX
/// metadata; the ones the Mac keeps and this host does not - the Finder's own
/// sixteen bytes, a backup date - are zero, which is what an unset one is
/// there too.
fn fill(out: &mut CatalogInfo, which: u32, path: &std::path::Path, metadata: &std::fs::Metadata) {
    use std::os::unix::fs::MetadataExt;

    if which & info::NODE_FLAGS != 0 {
        out.node_flags = match metadata.is_dir() {
            true => NODE_IS_DIRECTORY,
            false => 0,
        };
        if metadata.permissions().readonly() {
            out.node_flags |= NODE_LOCKED;
        }
    }
    if which & info::VOLUME != 0 {
        out.volume = volume_of(metadata.dev(), path);
    }
    if which & info::NODE_ID != 0 {
        out.node_id = metadata.ino() as u32;
    }
    if which & info::PARENT_DIR_ID != 0 {
        out.parent_dir_id = path
            .parent()
            .and_then(|parent| std::fs::metadata(parent).ok())
            .map_or(0, |parent| parent.ino() as u32);
    }
    // The Mac keeps a creation date and this host does not always; `birthtime`
    // is the one to ask, and the modification time is *not* a substitute - a
    // file that was written yesterday was not created yesterday.
    if which & info::CREATE_DATE != 0 {
        out.create_date = metadata
            .created()
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(UtcDateTime::default, |since| {
                UtcDateTime::from_unix(since.as_secs() as i64)
            });
    }
    if which & info::CONTENT_MOD != 0 {
        out.content_mod_date = UtcDateTime::from_unix(metadata.mtime());
    }
    if which & info::ATTR_MOD != 0 {
        out.attribute_mod_date = UtcDateTime::from_unix(metadata.ctime());
    }
    if which & info::ACCESS_DATE != 0 {
        out.access_date = UtcDateTime::from_unix(metadata.atime());
    }
    if which & info::PERMISSIONS != 0 {
        out.permissions = Permissions {
            user_id: metadata.uid(),
            group_id: metadata.gid(),
            mode: metadata.mode() as u16,
            ..Permissions::default()
        };
    }
    if which & info::VALENCE != 0 && metadata.is_dir() {
        out.valence = std::fs::read_dir(path)
            .map(|entries| entries.count() as u32)
            .unwrap_or(0);
    }
    if which & info::DATA_SIZES != 0 {
        out.data_logical_size = metadata.len();
        out.data_physical_size = metadata.blocks().saturating_mul(512);
    }
    // A resource fork is an HFS+ named fork, and there are none here - so its
    // sizes are zero rather than unanswered.
    let _ = which & info::RSRC_SIZES;
}

impl UtcDateTime {
    /// The Unix timestamp `self` names, for a date this tree is about to hand
    /// to `futimens` through [`std::fs::File::set_times`].
    fn to_system_time(self) -> std::time::SystemTime {
        let since_1904 = (u64::from(self.high_seconds) << 32) | u64::from(self.low_seconds);
        let unix = since_1904 as i64 - UNIX_TO_1904;
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix.max(0) as u64)
    }
}

/// `OSErr FSSetCatalogInfo(const FSRef *ref, FSCatalogInfoBitmap whichInfo,
/// const FSCatalogInfo *catalogInfo)`
///
/// A bit this tree cannot act on is refused by name - the same rule
/// [`FSGetCatalogInfo`] follows for the bits it cannot answer - rather than
/// silently dropped, which would tell a caller its change took when it did not.
/// The Finder info, the valence and the sizes are read-only facts of the file
/// everywhere in this bitmap; only the three a POSIX file actually carries -
/// its lock bit, its permissions, and two of its four dates - are settable.
///
/// # Safety
///
/// `reference` is a whole `FSRef`; `catalog_info` is null or readable for a
/// whole `CatalogInfo` when `which` is non-zero.
pub unsafe extern "C" fn FSSetCatalogInfo(
    reference: *const c_void,
    which: u32,
    catalog_info: *const CatalogInfo,
) -> i16 {
    let Some(path) = (unsafe { load(reference) }) else {
        return PARAM_ERR as i16;
    };
    if which == 0 {
        return NO_ERR as i16;
    }
    if which & !info::SETTABLE != 0 {
        eprintln!(
            "rosemary: coreservices: FSSetCatalogInfo: {:#x} of the catalog bitmap is not \
             something this tree can act on",
            which & !info::SETTABLE
        );
        return PARAM_ERR as i16;
    }
    if catalog_info.is_null() {
        return PARAM_ERR as i16;
    }
    // SAFETY: the caller states it is readable; unaligned for the reason
    // `FSGetCatalogInfo` reads `FSCatalogInfo` unaligned.
    let wanted = unsafe { catalog_info.read_unaligned() };
    if which & info::PERMISSIONS != 0 {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::Permissions::from_mode(u32::from(wanted.permissions.mode));
        if std::fs::set_permissions(&path, mode).is_err() {
            return IO_ERR as i16;
        }
    }
    if which & info::NODE_FLAGS != 0 {
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            return FNF_ERR as i16;
        };
        let mut permissions = metadata.permissions();
        permissions.set_readonly(wanted.node_flags & NODE_LOCKED != 0);
        if std::fs::set_permissions(&path, permissions).is_err() {
            return IO_ERR as i16;
        }
    }
    if which & (info::CONTENT_MOD | info::ACCESS_DATE) != 0 {
        // Read-only is enough for `futimens`; opening for write would refuse a
        // directory, and a directory's dates are as settable as a file's.
        let Ok(file) = std::fs::File::open(&path) else {
            return IO_ERR as i16;
        };
        let mut times = std::fs::FileTimes::new();
        if which & info::CONTENT_MOD != 0 {
            times = times.set_modified(wanted.content_mod_date.to_system_time());
        }
        if which & info::ACCESS_DATE != 0 {
            times = times.set_accessed(wanted.access_date.to_system_time());
        }
        if file.set_times(times).is_err() {
            return IO_ERR as i16;
        }
    }
    NO_ERR as i16
}

#[cfg(test)]
mod set_catalog_info_tests {
    use super::*;

    /// The lock bit round-trips through `FSSetCatalogInfo` and
    /// `FSGetCatalogInfo`, and the file is still there to ask about.
    #[test]
    fn the_lock_bit_round_trips() {
        let path = std::env::temp_dir().join(format!(
            "rosemary-fssetcatalog-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::write(&path, b"x").unwrap();
        let mut reference = [0_u8; FS_REF_BYTES];
        let mut is_dir = 0_u8;
        let text = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: a C string and storage for a whole reference.
        unsafe {
            assert_eq!(
                FSPathMakeRef(
                    text.as_ptr().cast(),
                    reference.as_mut_ptr().cast(),
                    &mut is_dir
                ),
                NO_ERR
            );
        }
        let mut set = CatalogInfo {
            node_flags: NODE_LOCKED,
            ..CatalogInfo::default()
        };
        // SAFETY: a reference just made, and a whole `CatalogInfo` this test owns.
        unsafe {
            assert_eq!(
                FSSetCatalogInfo(reference.as_ptr().cast(), info::NODE_FLAGS, &raw const set),
                NO_ERR as i16
            );
        }
        let mut got = CatalogInfo::default();
        // SAFETY: as above, and storage for a whole `CatalogInfo`.
        unsafe {
            assert_eq!(
                FSGetCatalogInfo(
                    reference.as_ptr().cast(),
                    info::NODE_FLAGS,
                    &raw mut got,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ),
                NO_ERR as i16
            );
        }
        assert_eq!(got.node_flags & NODE_LOCKED, NODE_LOCKED);
        // Unlock again so `remove_file` below does not need to clear it itself.
        set.node_flags = 0;
        // SAFETY: as above.
        unsafe { FSSetCatalogInfo(reference.as_ptr().cast(), info::NODE_FLAGS, &raw const set) };
        std::fs::remove_file(&path).unwrap();
    }

    /// A bit this tree cannot write - the valence, say - is refused rather than
    /// silently ignored.
    #[test]
    fn an_unsettable_bit_is_refused() {
        let dir = std::env::temp_dir();
        let text = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        let mut reference = [0_u8; FS_REF_BYTES];
        // SAFETY: a C string and storage for a whole reference.
        unsafe {
            assert_eq!(
                FSPathMakeRef(
                    text.as_ptr().cast(),
                    reference.as_mut_ptr().cast(),
                    std::ptr::null_mut()
                ),
                NO_ERR
            );
        }
        let set = CatalogInfo::default();
        // SAFETY: a reference just made, and a whole `CatalogInfo`.
        let status =
            unsafe { FSSetCatalogInfo(reference.as_ptr().cast(), info::VALENCE, &raw const set) };
        assert_eq!(status, PARAM_ERR as i16);
    }

    /// Nothing subscribes, so there is nobody to tell - and nothing crashes
    /// telling them anyway.
    #[test]
    fn fn_notify_is_accepted_with_no_subscribers() {
        // SAFETY: a null `FSRef` is a documented "no particular item" case.
        let status = unsafe { FNNotify(std::ptr::null(), 1, 0) };
        assert_eq!(status, NO_ERR);
    }
}

/// `OSStatus FNNotify(const FSRef *fileOrDirectory, FNMessage message,
/// OptionBits flags)`
///
/// Tells every process that subscribed through `FNSubscribe` that a file or
/// directory changed, so the Finder can redraw it. Nothing in this tree
/// subscribes - there is no Finder here - so there is nobody to tell: accepted
/// and done, as `coreservices::events::SetSystemUIMode` answers for a menu bar
/// this host has not got either.
pub unsafe extern "C" fn FNNotify(
    _file_or_directory: *const c_void,
    _message: u32,
    _flags: u32,
) -> OSStatus {
    NO_ERR
}
