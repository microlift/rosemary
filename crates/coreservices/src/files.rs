use std::collections::HashMap;
use std::ffi::{CStr, c_void};
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
/// `errFSRefsDifferent`: the two references name different objects.
const REFS_DIFFERENT: OSStatus = -1420;

/// Bytes of an `FSRef`, which is ABI: a guest declares one on its stack.
const FS_REF_BYTES: usize = 80;

/// The first eight bytes of every reference this tree makes.
///
/// A reference that does not carry it came from somewhere else - a guest that
/// zeroed its storage, or a structure that is not an `FSRef` at all - and is
/// refused rather than read as an index into the table below.
const MAGIC: u64 = u64::from_le_bytes(*b"R3FSRef\0");

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
    let (Some(root), Some(relative)) = (domain_root(volume), folder(kind)) else {
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
    if create != 0 && !path.is_dir() {
        // A failure here is reported rather than ignored: `/Library/Frameworks`
        // cannot be made by an unprivileged process, and a reference to a
        // directory that is not there is worse than saying so.
        let _ = std::fs::create_dir_all(&path);
    }
    if !path.is_dir() {
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
    let Ok(metadata) = std::fs::metadata(&path) else {
        return FNF_ERR;
    };
    if !is_directory.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *is_directory = u8::from(metadata.is_dir()) };
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
        unsafe { *folder = u8::from(path.is_dir()) };
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
        unsafe { *target_is_folder = u8::from(path.is_dir()) };
    }
    if !was_aliased.is_null() {
        unsafe { *was_aliased = 0 };
    }
    NO_ERR
}

exports::exports! {
    functions {
        "FSFindFolder" => FSFindFolder,
        "FSPathMakeRef" => FSPathMakeRef,
        "FSRefMakePath" => FSRefMakePath,
        "FSCompareFSRefs" => FSCompareFSRefs,
        "FSIsAliasFile" => FSIsAliasFile,
        "FSResolveAliasFile" => FSResolveAliasFile,
        "FSGetCatalogInfo" => FSGetCatalogInfo,
        "FSGetVolumeInfo" => FSGetVolumeInfo,
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
struct UtcDateTime {
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
}

/// `kFSNodeIsDirectoryMask`.
const NODE_IS_DIRECTORY: u16 = 0x0010;
/// `kFSNodeLockedMask`.
const NODE_LOCKED: u16 = 0x0001;

/// Seconds from 1904-01-01 - the File Manager's epoch - to the Unix one.
const UNIX_TO_1904: i64 = 2_082_844_800;

impl UtcDateTime {
    /// A Unix timestamp in the File Manager's own counting.
    fn from_unix(seconds: i64) -> Self {
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
            "rosetta3: coreservices: FSGetVolumeInfo was asked for FSVolumeInfo ({which:#x}), \
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
            "rosetta3: coreservices: FSGetCatalogInfo was asked for an FSSpec, which names a \
             file by a volume and a directory id this tree has not got"
        );
        return PARAM_ERR as i16;
    }
    if which & !info::ANSWERED != 0 {
        eprintln!(
            "rosetta3: coreservices: FSGetCatalogInfo: {:#x} of the catalog bitmap is not \
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
