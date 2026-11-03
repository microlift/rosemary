//! `filesec_t`, a file's owner, group, mode, owner and group UUIDs and ACL in
//! one object, and the `statx_np` and `chmodx_np` calls that read and apply
//! one. `libcopyfile` moves a file's security through these.
#![cfg(not(target_os = "macos"))]

use std::ffi::{c_char, c_int, c_void};

use crate::stat::Stat;

/// `filesec_property_t`.
mod property {
    use std::ffi::c_int;

    pub const OWNER: c_int = 1;
    pub const GROUP: c_int = 2;
    pub const UUID: c_int = 3;
    pub const MODE: c_int = 4;
    pub const ACL: c_int = 5;
    pub const GRPUUID: c_int = 6;
    pub const ACL_RAW: c_int = 100;
    pub const ACL_ALLOCSIZE: c_int = 101;
}

/// `_FILESEC_UNSET_PROPERTY` and `_FILESEC_REMOVE_ACL`, which stand in for a
/// property's value.
const UNSET_PROPERTY: usize = 0;
const REMOVE_ACL: usize = 1;

/// `ALLPERMS`: what `chmodx_np` applies of a mode.
const ALLPERMS: u16 = 0o7777;

type Uuid = [u8; 16];

/// No host file has a Darwin ACL, so the only ACL a filesec can carry is the
/// request to remove one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Acl {
    Unset,
    Remove,
}

/// `struct _filesec`, whose layout is libc's own business: callers hold only
/// the pointer.
#[derive(Clone, Debug)]
pub struct Filesec {
    owner: Option<u32>,
    group: Option<u32>,
    uuid: Option<Uuid>,
    grpuuid: Option<Uuid>,
    mode: Option<u16>,
    acl: Acl,
}

impl Filesec {
    const EMPTY: Filesec = Filesec {
        owner: None,
        group: None,
        uuid: None,
        grpuuid: None,
        mode: None,
        acl: Acl::Unset,
    };

    /// What `statx_np` records: the stat's ids and mode. The UUIDs and ACL
    /// come from the kernel's `kauth_filesec`, which no host file has.
    fn record_stat(&mut self, sb: &Stat) {
        *self = Filesec {
            owner: Some(sb.uid),
            group: Some(sb.gid),
            mode: Some(sb.mode),
            ..Filesec::EMPTY
        };
    }
}

fn fail(error: c_int) -> c_int {
    crate::errno::set(error);
    -1
}

/// `filesec_t filesec_init(void)`
pub extern "C" fn filesec_init() -> *mut Filesec {
    Box::into_raw(Box::new(Filesec::EMPTY))
}

/// `filesec_t filesec_dup(filesec_t)`
///
/// # Safety
///
/// `fsec` came from [`filesec_init`] or [`filesec_dup`] and is live.
pub unsafe extern "C" fn filesec_dup(fsec: *const Filesec) -> *mut Filesec {
    // SAFETY: the caller's.
    Box::into_raw(Box::new(unsafe { (*fsec).clone() }))
}

/// `void filesec_free(filesec_t)`
///
/// # Safety
///
/// `fsec` is null or a live filesec, which this call ends.
pub unsafe extern "C" fn filesec_free(fsec: *mut Filesec) {
    if !fsec.is_null() {
        // SAFETY: the caller's; it came from `Box::into_raw`.
        drop(unsafe { Box::from_raw(fsec) });
    }
}

/// `int filesec_get_property(filesec_t, filesec_property_t, void *propptr)`
///
/// A null `propptr` asks only whether the property is set.
///
/// # Safety
///
/// `fsec` is a live filesec, and `propptr` is null or as wide as `property`'s
/// type.
pub unsafe extern "C" fn filesec_get_property(
    fsec: *const Filesec,
    property: c_int,
    propptr: *mut c_void,
) -> c_int {
    // SAFETY: the caller's.
    let fsec = unsafe { &*fsec };
    let acl_set = fsec.acl == Acl::Remove;
    // SAFETY, for each write: `propptr` is non-null and the type `property`
    // names, by the caller's contract.
    let found = match property {
        property::OWNER => fsec
            .owner
            .map(|v| unsafe { propptr.cast::<u32>().write_unaligned(v) }),
        property::GROUP => fsec
            .group
            .map(|v| unsafe { propptr.cast::<u32>().write_unaligned(v) }),
        property::UUID => fsec
            .uuid
            .map(|v| unsafe { propptr.cast::<Uuid>().write_unaligned(v) }),
        property::GRPUUID => fsec
            .grpuuid
            .map(|v| unsafe { propptr.cast::<Uuid>().write_unaligned(v) }),
        property::MODE => fsec
            .mode
            .map(|v| unsafe { propptr.cast::<u16>().write_unaligned(v) }),
        // The ACL, or the raw buffer behind it, is the removal marker itself.
        property::ACL | property::ACL_RAW => {
            acl_set.then(|| unsafe { propptr.cast::<usize>().write_unaligned(REMOVE_ACL) })
        }
        property::ACL_ALLOCSIZE => {
            acl_set.then(|| unsafe { propptr.cast::<usize>().write_unaligned(0) })
        }
        _ => return fail(libc::EINVAL),
    };
    match found {
        Some(()) => 0,
        None => fail(libc::ENOENT),
    }
}

/// Read one property's value, or `None` for `_FILESEC_UNSET_PROPERTY`.
///
/// # Safety
///
/// `propptr` is null or points at a `T`.
unsafe fn value<T: Copy>(propptr: *const c_void) -> Option<T> {
    // SAFETY: the caller's.
    (!propptr.is_null()).then(|| unsafe { propptr.cast::<T>().read_unaligned() })
}

/// `int filesec_set_property(filesec_t, filesec_property_t, const void *)`
///
/// # Safety
///
/// `fsec` is a live filesec, and `propptr` is a marker or points at a value
/// of `property`'s type.
pub unsafe extern "C" fn filesec_set_property(
    fsec: *mut Filesec,
    property: c_int,
    propptr: *const c_void,
) -> c_int {
    // SAFETY: the caller's, for this and each `value`.
    let fsec = unsafe { &mut *fsec };
    unsafe {
        match property {
            property::OWNER => fsec.owner = value(propptr),
            property::GROUP => fsec.group = value(propptr),
            property::UUID => fsec.uuid = value(propptr),
            property::GRPUUID => fsec.grpuuid = value(propptr),
            property::MODE => fsec.mode = value(propptr),
            property::ACL => match propptr as usize {
                UNSET_PROPERTY => fsec.acl = Acl::Unset,
                REMOVE_ACL => fsec.acl = Acl::Remove,
                // A pointer to an `acl_t`, and this runtime never makes one.
                _ => return fail(libc::EINVAL),
            },
            property::ACL_RAW => {
                fsec.acl = Acl::Unset;
                // A `kauth_filesec` the caller hands over: an ACL, which no
                // host file can be given.
                if value::<*const c_void>(propptr).is_some_and(|raw| !raw.is_null()) {
                    return fail(libc::ENOTSUP);
                }
            }
            // The size of a raw buffer, which there never is.
            property::ACL_ALLOCSIZE if propptr.is_null() => return fail(libc::EINVAL),
            property::ACL_ALLOCSIZE => {}
            _ => return fail(libc::EINVAL),
        }
    }
    0
}

/// `int filesec_unset_property(filesec_t, filesec_property_t)`
///
/// # Safety
///
/// As [`filesec_set_property`].
pub unsafe extern "C" fn filesec_unset_property(fsec: *mut Filesec, property: c_int) -> c_int {
    // SAFETY: the caller's.
    unsafe { filesec_set_property(fsec, property, UNSET_PROPERTY as *const c_void) }
}

/// `int filesec_query_property(filesec_t, filesec_property_t, int *validptr)`
///
/// # Safety
///
/// `fsec` is a live filesec and `validptr` is writable.
pub unsafe extern "C" fn filesec_query_property(
    fsec: *const Filesec,
    property: c_int,
    validptr: *mut c_int,
) -> c_int {
    // SAFETY: the caller's.
    let fsec = unsafe { &*fsec };
    let valid = match property {
        property::OWNER => fsec.owner.is_some(),
        property::GROUP => fsec.group.is_some(),
        property::UUID => fsec.uuid.is_some(),
        property::GRPUUID => fsec.grpuuid.is_some(),
        property::MODE => fsec.mode.is_some(),
        property::ACL | property::ACL_RAW | property::ACL_ALLOCSIZE => fsec.acl == Acl::Remove,
        _ => return fail(libc::EINVAL),
    };
    // SAFETY: the caller's.
    unsafe { validptr.write(c_int::from(valid)) };
    0
}

/// Stat through `stat`, then record what it found in `fsec` unless that is
/// null, in which case this is the plain call.
///
/// # Safety
///
/// `fsec` is null or a live filesec.
unsafe fn statx(fsec: *mut Filesec, sb: *mut Stat, stat: impl FnOnce(*mut Stat) -> c_int) -> c_int {
    let rc = stat(sb);
    if rc == 0 && !fsec.is_null() {
        // SAFETY: `stat` filled `sb`, and `fsec` is the caller's.
        unsafe { (*fsec).record_stat(&*sb) };
    }
    rc
}

/// `int statx_np(const char *, struct stat *, filesec_t)`, in the `$INODE64`
/// layout.
///
/// # Safety
///
/// As [`crate::stat::stat`], with `fsec` null or a live filesec.
pub unsafe extern "C" fn statx_np(path: *const c_char, sb: *mut Stat, fsec: *mut Filesec) -> c_int {
    // SAFETY: the caller's.
    unsafe { statx(fsec, sb, |sb| crate::stat::stat(path, sb)) }
}

/// `int lstatx_np(const char *, struct stat *, filesec_t)`
///
/// # Safety
///
/// As [`statx_np`].
pub unsafe extern "C" fn lstatx_np(
    path: *const c_char,
    sb: *mut Stat,
    fsec: *mut Filesec,
) -> c_int {
    // SAFETY: the caller's.
    unsafe { statx(fsec, sb, |sb| crate::stat::lstat(path, sb)) }
}

/// `int fstatx_np(int, struct stat *, filesec_t)`
///
/// # Safety
///
/// As [`crate::stat::fstat`], with `fsec` null or a live filesec.
pub unsafe extern "C" fn fstatx_np(fd: c_int, sb: *mut Stat, fsec: *mut Filesec) -> c_int {
    // SAFETY: the caller's.
    unsafe { statx(fsec, sb, |sb| crate::stat::fstat(fd, sb)) }
}

/// The unsuffixed `statx_np`, as [`crate::stat::stat_unsuffixed`].
///
/// # Safety
///
/// As [`statx_np`], with `sb` laid out as the unsuffixed `stat` is.
pub unsafe extern "C" fn statx_np_unsuffixed(
    path: *const c_char,
    sb: *mut c_void,
    fsec: *mut Filesec,
) -> c_int {
    let legacy = crate::stat::unsuffixed_is_legacy();
    // SAFETY: the caller's.
    unsafe { crate::stat::unsuffixed(sb, legacy, |sb| statx_np(path, sb, fsec)) }
}

/// The unsuffixed `lstatx_np`.
///
/// # Safety
///
/// As [`statx_np_unsuffixed`].
pub unsafe extern "C" fn lstatx_np_unsuffixed(
    path: *const c_char,
    sb: *mut c_void,
    fsec: *mut Filesec,
) -> c_int {
    let legacy = crate::stat::unsuffixed_is_legacy();
    // SAFETY: the caller's.
    unsafe { crate::stat::unsuffixed(sb, legacy, |sb| lstatx_np(path, sb, fsec)) }
}

/// The unsuffixed `fstatx_np`.
///
/// # Safety
///
/// As [`fstatx_np`], with `sb` laid out as the unsuffixed `stat` is.
pub unsafe extern "C" fn fstatx_np_unsuffixed(
    fd: c_int,
    sb: *mut c_void,
    fsec: *mut Filesec,
) -> c_int {
    let legacy = crate::stat::unsuffixed_is_legacy();
    // SAFETY: the caller's.
    unsafe { crate::stat::unsuffixed(sb, legacy, |sb| fstatx_np(fd, sb, fsec)) }
}

/// Apply `fsec` as `__chmod_extended` does: whichever of owner, group and
/// mode it sets, in one step that changes nothing if the ids cannot be.
///
/// UUIDs travel with the ACL in a `kauth_filesec`, which a host file system
/// cannot store, so a filesec that sets either is refused before anything is
/// changed, as a Darwin volume without extended security refuses it. Removing
/// an ACL that cannot exist succeeds.
///
/// # Safety
///
/// `fsec` is null or a live filesec.
unsafe fn chmodx(
    fsec: *const Filesec,
    chown: impl FnOnce(u32, u32) -> c_int,
    chmod: impl FnOnce(u16) -> c_int,
) -> c_int {
    if fsec.is_null() {
        return fail(libc::EINVAL);
    }
    // SAFETY: the caller's.
    let fsec = unsafe { &*fsec };
    if fsec.uuid.is_some() || fsec.grpuuid.is_some() {
        return fail(libc::ENOTSUP);
    }
    // `KAUTH_UID_NONE` and `KAUTH_GID_NONE` are `chown`'s "leave it" too.
    if (fsec.owner.is_some() || fsec.group.is_some())
        && chown(
            fsec.owner.unwrap_or(u32::MAX),
            fsec.group.unwrap_or(u32::MAX),
        ) != 0
    {
        return -1;
    }
    match fsec.mode {
        Some(mode) => chmod(mode & ALLPERMS),
        None => 0,
    }
}

/// `int chmodx_np(const char *, filesec_t)`
///
/// # Safety
///
/// `path` is a NUL-terminated string and `fsec` null or a live filesec.
pub unsafe extern "C" fn chmodx_np(path: *const c_char, fsec: *const Filesec) -> c_int {
    // SAFETY: the caller's, for all three.
    unsafe {
        chmodx(
            fsec,
            |uid, gid| crate::fs::chown(path, uid, gid),
            |mode| crate::fs::chmod(path, libc::mode_t::from(mode)),
        )
    }
}

/// `int fchmodx_np(int, filesec_t)`
///
/// # Safety
///
/// `fsec` is null or a live filesec.
pub unsafe extern "C" fn fchmodx_np(fd: c_int, fsec: *const Filesec) -> c_int {
    // SAFETY: the caller's; `fchmod` reads no memory.
    unsafe {
        chmodx(
            fsec,
            |uid, gid| crate::fs::fchown(fd, uid, gid),
            |mode| libc::fchmod(fd, libc::mode_t::from(mode)),
        )
    }
}

exports::exports! {
    functions {
        "filesec_init" => filesec_init,
        "filesec_dup" => filesec_dup,
        "filesec_free" => filesec_free,
        "filesec_get_property" => filesec_get_property,
        "filesec_set_property" => filesec_set_property,
        "filesec_unset_property" => filesec_unset_property,
        "filesec_query_property" => filesec_query_property,
        "statx_np" => statx_np_unsuffixed,
        "statx_np$INODE64" | "statx64_np" => statx_np,
        "lstatx_np" => lstatx_np_unsuffixed,
        "lstatx_np$INODE64" | "lstatx64_np" => lstatx_np,
        "fstatx_np" => fstatx_np_unsuffixed,
        "fstatx_np$INODE64" | "fstatx64_np" => fstatx_np,
        "chmodx_np" => chmodx_np,
        "fchmodx_np" => fchmodx_np,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> std::path::PathBuf {
        let at =
            std::env::temp_dir().join(format!("rosemary-filesec-{name}-{}", std::process::id()));
        std::fs::write(&at, b"x").unwrap();
        at
    }

    #[test]
    fn an_unset_property_is_enoent_and_a_set_one_reads_back() {
        let fsec = filesec_init();
        let mut uid = 0u32;
        unsafe {
            assert_eq!(
                filesec_get_property(fsec, property::OWNER, (&raw mut uid).cast()),
                -1
            );
            assert_eq!(crate::errno::get(), libc::ENOENT);
            let want = 501u32;
            assert_eq!(
                filesec_set_property(fsec, property::OWNER, (&raw const want).cast()),
                0
            );
            assert_eq!(
                filesec_get_property(fsec, property::OWNER, (&raw mut uid).cast()),
                0
            );
            assert_eq!(uid, 501);
            assert_eq!(filesec_unset_property(fsec, property::OWNER), 0);
            assert_eq!(
                filesec_get_property(fsec, property::OWNER, std::ptr::null_mut()),
                -1
            );
            filesec_free(fsec);
        }
    }

    #[test]
    fn a_removed_acl_reads_back_as_the_marker_and_survives_a_dup() {
        let fsec = filesec_init();
        unsafe {
            assert_eq!(
                filesec_set_property(fsec, property::ACL, REMOVE_ACL as *const c_void),
                0
            );
            let copy = filesec_dup(fsec);
            let mut acl = 0usize;
            assert_eq!(
                filesec_get_property(copy, property::ACL, (&raw mut acl).cast()),
                0
            );
            assert_eq!(acl, REMOVE_ACL);
            let mut valid = 0;
            assert_eq!(
                filesec_query_property(copy, property::ACL_RAW, &mut valid),
                0
            );
            assert_eq!(valid, 1);
            filesec_free(copy);
            filesec_free(fsec);
        }
    }

    #[test]
    fn an_acl_object_is_refused_since_none_was_ever_made() {
        let fsec = filesec_init();
        let acl = 0x1000usize as *const c_void;
        unsafe {
            assert_eq!(
                filesec_set_property(fsec, property::ACL, (&raw const acl).cast()),
                -1
            );
            assert_eq!(crate::errno::get(), libc::EINVAL);
            filesec_free(fsec);
        }
    }

    /// The stat's ids and mode, and none of what a `kauth_filesec` carries.
    #[test]
    fn fstatx_records_owner_group_and_mode_but_no_uuids_or_acl() {
        let at = scratch("fstatx");
        let file = std::fs::File::open(&at).unwrap();
        let _ = std::fs::remove_file(&at);
        let fsec = filesec_init();
        let mut sb = Stat::default();
        unsafe {
            assert_eq!(fstatx_np(file.as_raw_fd(), &mut sb, fsec), 0);
            let mut mode = 0u16;
            assert_eq!(
                filesec_get_property(fsec, property::MODE, (&raw mut mode).cast()),
                0
            );
            assert_eq!(mode, sb.mode);
            let mut uid = 0u32;
            assert_eq!(
                filesec_get_property(fsec, property::OWNER, (&raw mut uid).cast()),
                0
            );
            assert_eq!(uid, sb.uid);
            assert_eq!(
                filesec_get_property(fsec, property::UUID, std::ptr::null_mut()),
                -1
            );
            assert_eq!(
                filesec_get_property(fsec, property::ACL, std::ptr::null_mut()),
                -1
            );
            assert_eq!(crate::errno::get(), libc::ENOENT);
            filesec_free(fsec);
        }
    }

    #[test]
    fn fchmodx_applies_the_mode_and_refuses_uuids_without_changing_it() {
        let at = scratch("fchmodx");
        let file = std::fs::OpenOptions::new().write(true).open(&at).unwrap();
        let fsec = filesec_init();
        unsafe {
            let mode = 0o600u16;
            filesec_set_property(fsec, property::MODE, (&raw const mode).cast());
            assert_eq!(fchmodx_np(file.as_raw_fd(), fsec), 0);
            assert_eq!(
                std::fs::metadata(&at).unwrap().permissions().mode() & 0o7777,
                0o600
            );

            let other = 0o644u16;
            let uuid: Uuid = [7; 16];
            filesec_set_property(fsec, property::MODE, (&raw const other).cast());
            filesec_set_property(fsec, property::UUID, (&raw const uuid).cast());
            assert_eq!(fchmodx_np(file.as_raw_fd(), fsec), -1);
            assert_eq!(crate::errno::get(), libc::ENOTSUP);
            assert_eq!(
                std::fs::metadata(&at).unwrap().permissions().mode() & 0o7777,
                0o600
            );
            filesec_free(fsec);
        }
        let _ = std::fs::remove_file(&at);
    }
}
