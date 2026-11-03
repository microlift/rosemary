#![cfg(not(target_os = "macos"))]

use std::ffi::{c_char, c_int, c_void};

/// The answer for every file: it has no extended ACL.
fn no_acl() -> *mut c_void {
    crate::errno::set(libc::ENOENT);
    std::ptr::null_mut()
}

/// A call given an `acl_t` or entry this runtime never made.
fn not_an_acl() -> c_int {
    crate::errno::set(libc::EINVAL);
    -1
}

/// `acl_t acl_get_link_np(const char *path, acl_type_t type)`
pub extern "C" fn acl_get_link_np(_path: *const c_char, _kind: c_int) -> *mut c_void {
    no_acl()
}

/// `acl_t acl_get_file(const char *path, acl_type_t type)`
pub extern "C" fn acl_get_file(_path: *const c_char, _kind: c_int) -> *mut c_void {
    no_acl()
}

/// `acl_t acl_get_fd_np(int fd, acl_type_t type)`
pub extern "C" fn acl_get_fd_np(_fd: c_int, _kind: c_int) -> *mut c_void {
    no_acl()
}

/// `int acl_free(void *obj)`: null is accepted, as it is everywhere else.
pub extern "C" fn acl_free(obj: *mut c_void) -> c_int {
    match obj.is_null() {
        true => 0,
        false => not_an_acl(),
    }
}

/// `int acl_get_entry(acl_t, int, acl_entry_t *)`
pub extern "C" fn acl_get_entry(_acl: *mut c_void, _id: c_int, _entry: *mut *mut c_void) -> c_int {
    not_an_acl()
}

/// `int acl_get_tag_type(acl_entry_t, acl_tag_t *)`
pub extern "C" fn acl_get_tag_type(_entry: *mut c_void, _tag: *mut c_int) -> c_int {
    not_an_acl()
}

/// `int acl_get_permset(acl_entry_t, acl_permset_t *)`
pub extern "C" fn acl_get_permset(_entry: *mut c_void, _set: *mut *mut c_void) -> c_int {
    not_an_acl()
}

/// `int acl_get_flagset_np(void *, acl_flagset_t *)`
pub extern "C" fn acl_get_flagset_np(_obj: *mut c_void, _set: *mut *mut c_void) -> c_int {
    not_an_acl()
}

/// `int acl_get_perm_np(acl_permset_t, acl_perm_t)`
pub extern "C" fn acl_get_perm_np(_set: *mut c_void, _perm: c_int) -> c_int {
    not_an_acl()
}

/// `int acl_get_flag_np(acl_flagset_t, acl_flag_t)`
pub extern "C" fn acl_get_flag_np(_set: *mut c_void, _flag: c_int) -> c_int {
    not_an_acl()
}

/// `void *acl_get_qualifier(acl_entry_t)`
pub extern "C" fn acl_get_qualifier(_entry: *mut c_void) -> *mut c_void {
    not_an_acl();
    std::ptr::null_mut()
}

/// The answer for a call that would build an ACL.
///
/// FIXME (ACCURACY): Darwin builds one in memory whatever the file system
/// can store. None could ever be applied to a host file, so none is built.
fn cannot_build() -> *mut c_void {
    crate::errno::set(libc::ENOTSUP);
    std::ptr::null_mut()
}

/// `acl_t acl_init(int count)`
pub extern "C" fn acl_init(_count: c_int) -> *mut c_void {
    cannot_build()
}

/// `acl_t acl_from_text(const char *buf_p)`
pub extern "C" fn acl_from_text(_text: *const c_char) -> *mut c_void {
    cannot_build()
}

/// `char *acl_to_text(acl_t, ssize_t *len_p)`
pub extern "C" fn acl_to_text(_acl: *mut c_void, _len: *mut isize) -> *mut c_char {
    not_an_acl();
    std::ptr::null_mut()
}

/// `int acl_create_entry(acl_t *acl_p, acl_entry_t *entry_p)`
pub extern "C" fn acl_create_entry(_acl: *mut *mut c_void, _entry: *mut *mut c_void) -> c_int {
    not_an_acl()
}

/// `int acl_create_entry_np(acl_t *acl_p, acl_entry_t *entry_p, int index)`
pub extern "C" fn acl_create_entry_np(
    _acl: *mut *mut c_void,
    _entry: *mut *mut c_void,
    _index: c_int,
) -> c_int {
    not_an_acl()
}

/// `int acl_copy_entry(acl_entry_t dest, acl_entry_t src)`
pub extern "C" fn acl_copy_entry(_dest: *mut c_void, _src: *mut c_void) -> c_int {
    not_an_acl()
}

/// `int acl_delete_entry(acl_t, acl_entry_t)`
pub extern "C" fn acl_delete_entry(_acl: *mut c_void, _entry: *mut c_void) -> c_int {
    not_an_acl()
}

/// `int acl_set_tag_type(acl_entry_t, acl_tag_t)`
pub extern "C" fn acl_set_tag_type(_entry: *mut c_void, _tag: c_int) -> c_int {
    not_an_acl()
}

/// `int acl_set_qualifier(acl_entry_t, const void *)`
pub extern "C" fn acl_set_qualifier(_entry: *mut c_void, _qualifier: *const c_void) -> c_int {
    not_an_acl()
}

/// `int acl_set_permset(acl_entry_t, acl_permset_t)`
pub extern "C" fn acl_set_permset(_entry: *mut c_void, _set: *mut c_void) -> c_int {
    not_an_acl()
}

/// `int acl_add_perm(acl_permset_t, acl_perm_t)`
pub extern "C" fn acl_add_perm(_set: *mut c_void, _perm: c_int) -> c_int {
    not_an_acl()
}

/// `int acl_clear_perms(acl_permset_t)`
pub extern "C" fn acl_clear_perms(_set: *mut c_void) -> c_int {
    not_an_acl()
}

/// `int acl_set_fd(int fd, acl_t)`
pub extern "C" fn acl_set_fd(_fd: c_int, _acl: *mut c_void) -> c_int {
    not_an_acl()
}

/// `int mbr_identifier_translate(int id_type, const void *identifier,
/// size_t identifier_size, int target_type, void **result, int *rec_type)`
///
/// Returns an error number rather than setting `errno`. There is no directory
/// service behind this host's users to translate through, so every identifier
/// is one it has no record of.
pub extern "C" fn mbr_identifier_translate(
    _id_type: c_int,
    _identifier: *const c_void,
    _identifier_size: usize,
    _target_type: c_int,
    _result: *mut *mut c_void,
    _rec_type: *mut c_int,
) -> c_int {
    libc::ENOENT
}

/// `int mbr_uid_to_uuid(uid_t, uuid_t)`, which is [`mbr_identifier_translate`]
/// from a uid, and so has no record either.
pub extern "C" fn mbr_uid_to_uuid(_uid: u32, _uuid: *mut u8) -> c_int {
    libc::ENOENT
}

/// `int mbr_gid_to_uuid(gid_t, uuid_t)`, as [`mbr_uid_to_uuid`].
pub extern "C" fn mbr_gid_to_uuid(_gid: u32, _uuid: *mut u8) -> c_int {
    libc::ENOENT
}

exports::exports! {
    functions {
        "acl_get_link_np" => acl_get_link_np,
        "acl_get_file" => acl_get_file,
        "acl_get_fd_np" => acl_get_fd_np,
        "acl_free" => acl_free,
        "acl_get_entry" => acl_get_entry,
        "acl_get_tag_type" => acl_get_tag_type,
        "acl_get_permset" => acl_get_permset,
        "acl_get_flagset_np" => acl_get_flagset_np,
        "acl_get_perm_np" => acl_get_perm_np,
        "acl_get_flag_np" => acl_get_flag_np,
        "acl_get_qualifier" => acl_get_qualifier,
        "acl_init" => acl_init,
        "acl_from_text" => acl_from_text,
        "acl_to_text" => acl_to_text,
        "acl_create_entry" => acl_create_entry,
        "acl_create_entry_np" => acl_create_entry_np,
        "acl_copy_entry" => acl_copy_entry,
        "acl_delete_entry" => acl_delete_entry,
        "acl_set_tag_type" => acl_set_tag_type,
        "acl_set_qualifier" => acl_set_qualifier,
        "acl_set_permset" => acl_set_permset,
        "acl_add_perm" => acl_add_perm,
        "acl_clear_perms" => acl_clear_perms,
        "acl_set_fd" => acl_set_fd,
        "mbr_identifier_translate" => mbr_identifier_translate,
        "mbr_uid_to_uuid" => mbr_uid_to_uuid,
        "mbr_gid_to_uuid" => mbr_gid_to_uuid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_has_no_extended_acl() {
        assert!(acl_get_link_np(c"/".as_ptr(), 0x100).is_null());
        assert_eq!(crate::errno::get(), libc::ENOENT);
        assert_eq!(acl_free(std::ptr::null_mut()), 0);
    }

    #[test]
    fn no_acl_is_built_or_applied() {
        assert!(acl_init(1).is_null());
        assert_eq!(crate::errno::get(), libc::ENOTSUP);
        assert_eq!(acl_set_fd(0, std::ptr::null_mut()), -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
    }
}
