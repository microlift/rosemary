#![cfg(not(target_os = "macos"))]

//! `copyfile(3)`, which has no host counterpart. Feral's games copy their
//! bundled `InputDevices` tree into a cache with it before opening a window.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const ACL: u32 = 1 << 0;
const STAT: u32 = 1 << 1;
const XATTR: u32 = 1 << 2;
const DATA: u32 = 1 << 3;
const ALL: u32 = ACL | STAT | XATTR | DATA;
const RECURSIVE: u32 = 1 << 15;
const CHECK: u32 = 1 << 16;
const EXCL: u32 = 1 << 17;
const NOFOLLOW_SRC: u32 = 1 << 18;
const MOVE: u32 = 1 << 20;
const UNLINK: u32 = 1 << 21;

/// The opaque `copyfile_state_t`. Nothing is kept in it: no callback or
/// progress query is supported, so it only has to be a distinct live block.
const STATE_SIZE: usize = 64;

fn host(path: *const c_char) -> Option<PathBuf> {
    if path.is_null() {
        crate::errno::set(libc::EINVAL);
        return None;
    }
    // SAFETY: non-null, and a `const char *` is NUL-terminated by contract.
    let host = prefix::ns::to_host(unsafe { CStr::from_ptr(path) });
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(host.to_bytes())))
}

fn may_write(path: &Path) -> bool {
    let Ok(c) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        crate::errno::set(libc::EINVAL);
        return false;
    };
    crate::stat::may_write(c.as_ptr())
}

fn fail(error: std::io::Error) -> c_int {
    crate::errno::set(error.raw_os_error().unwrap_or(libc::EIO));
    -1
}

/// Copy one file, directory entry or symlink - never a directory's contents.
fn copy_one(from: &Path, to: &Path, flags: u32) -> std::io::Result<()> {
    let meta = match flags & NOFOLLOW_SRC != 0 {
        true => std::fs::symlink_metadata(from)?,
        false => std::fs::metadata(from)?,
    };
    if !may_write(to) {
        return Err(std::io::Error::last_os_error());
    }
    let existing = std::fs::symlink_metadata(to).ok();
    if flags & EXCL != 0 && existing.is_some() {
        return Err(std::io::Error::from_raw_os_error(libc::EEXIST));
    }
    if flags & UNLINK != 0 && existing.as_ref().is_some_and(|m| !m.is_dir()) {
        std::fs::remove_file(to)?;
    }
    let kind = meta.file_type();
    if kind.is_symlink() {
        if existing.is_some() && flags & UNLINK == 0 {
            std::fs::remove_file(to)?;
        }
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)?;
        return Ok(());
    }
    if kind.is_dir() {
        match std::fs::create_dir(to) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            other => other?,
        }
    } else if flags & DATA != 0 {
        std::fs::copy(from, to)?;
    } else if existing.is_none() {
        std::fs::File::create(to)?;
    }
    if flags & STAT != 0 {
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(meta.mode() & 0o7777))?;
        let times = [
            libc::timespec {
                tv_sec: meta.atime() as _,
                tv_nsec: meta.atime_nsec() as _,
            },
            libc::timespec {
                tv_sec: meta.mtime() as _,
                tv_nsec: meta.mtime_nsec() as _,
            },
        ];
        if let Ok(c) = CString::new(to.as_os_str().as_encoded_bytes()) {
            // SAFETY: a NUL-terminated path and two timespecs.
            unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
        }
    }
    Ok(())
}

/// Copy the tree at `from` onto `to`, entries before their contents.
fn copy_tree(from: &Path, to: &Path, flags: u32) -> std::io::Result<()> {
    // A tree is walked physically, as `copyfile`'s own `fts` walk is: a link
    // inside it is copied as a link.
    copy_one(from, to, flags | NOFOLLOW_SRC)?;
    if !std::fs::symlink_metadata(from)?.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_tree(&entry.path(), &to.join(entry.file_name()), flags)?;
    }
    Ok(())
}

fn copy(from: &Path, to: &Path, flags: u32, from_text: &[u8]) -> std::io::Result<()> {
    let recursive = flags & RECURSIVE != 0 && std::fs::metadata(from)?.is_dir();
    if !recursive {
        return copy_one(from, to, flags);
    }
    // As with cp(1): a source directory without a trailing slash goes *inside*
    // a destination directory that already exists; with one, its contents do.
    let to = match std::fs::metadata(to) {
        Ok(m) if m.is_dir() && !from_text.ends_with(b"/") => match from.file_name() {
            Some(name) => to.join(name),
            None => to.to_path_buf(),
        },
        _ => to.to_path_buf(),
    };
    // The top directory follows a link given as the source, as `fts` does for
    // its root.
    copy_one(from, &to, flags & !NOFOLLOW_SRC)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_tree(&entry.path(), &to.join(entry.file_name()), flags)?;
    }
    Ok(())
}

/// `int copyfile(const char *from, const char *to, copyfile_state_t, copyfile_flags_t)`
///
/// ACLs and extended attributes are not copied: the host keeps neither in a
/// form a Darwin program could tell apart from their absence.
///
/// # Safety
///
/// `from` and `to` are NUL-terminated strings.
pub unsafe extern "C" fn copyfile(
    from: *const c_char,
    to: *const c_char,
    _state: *mut c_void,
    flags: u32,
) -> c_int {
    let (Some(source), Some(destination)) = (host(from), host(to)) else {
        return -1;
    };
    if flags & CHECK != 0 {
        // What would be copied: everything asked for that there is.
        return match std::fs::symlink_metadata(&source) {
            Ok(_) => (flags & ALL & !(ACL | XATTR)) as c_int,
            Err(e) => fail(e),
        };
    }
    // SAFETY: checked non-null by `host`.
    let text = unsafe { CStr::from_ptr(from) }.to_bytes();
    if let Err(e) = copy(&source, &destination, flags, text) {
        return fail(e);
    }
    if flags & MOVE != 0 {
        let removed = match std::fs::symlink_metadata(&source) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(&source),
            _ => std::fs::remove_file(&source),
        };
        if let Err(e) = removed {
            return fail(e);
        }
    }
    0
}

/// `int fcopyfile(int from, int to, copyfile_state_t, copyfile_flags_t)`
///
/// The whole file, from its start, as Darwin copies it.
///
/// # Safety
///
/// `from` and `to` are open descriptors.
pub unsafe extern "C" fn fcopyfile(from: c_int, to: c_int, _state: *mut c_void, flags: u32) -> c_int {
    use std::io::{Seek, SeekFrom};
    use std::os::fd::FromRawFd;

    // Borrowed, not owned: the caller closes them.
    // SAFETY: the caller's open descriptors.
    let mut source = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(from) });
    let mut destination = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(to) });
    let result = (|| -> std::io::Result<()> {
        if flags & DATA != 0 {
            source.seek(SeekFrom::Start(0))?;
            std::io::copy(&mut *source, &mut *destination)?;
        }
        if flags & STAT != 0 {
            let mode = source.metadata()?.mode() & 0o7777;
            destination.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// `copyfile_state_t copyfile_state_alloc(void)`
///
/// # Safety
///
/// None beyond the allocator's.
pub unsafe extern "C" fn copyfile_state_alloc() -> *mut c_void {
    // SAFETY: a plain size; the block is released by `copyfile_state_free`.
    let block = unsafe { crate::heap::malloc(STATE_SIZE) };
    if !block.is_null() {
        // SAFETY: a fresh block of `STATE_SIZE` bytes.
        unsafe { std::ptr::write_bytes(block.cast::<u8>(), 0, STATE_SIZE) };
    }
    block
}

/// `int copyfile_state_free(copyfile_state_t)`
///
/// # Safety
///
/// `state` is null or came from [`copyfile_state_alloc`].
pub unsafe extern "C" fn copyfile_state_free(state: *mut c_void) -> c_int {
    if !state.is_null() {
        // SAFETY: the caller's obligation.
        unsafe { crate::heap::free(state) };
    }
    0
}

exports::exports! {
    functions {
        "copyfile" => copyfile,
        "fcopyfile" => fcopyfile,
        "copyfile_state_alloc" => copyfile_state_alloc,
        "copyfile_state_free" => copyfile_state_free,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("darwin-copyfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(from: &Path, to: &Path, flags: u32) -> c_int {
        let from = CString::new(from.as_os_str().as_encoded_bytes()).unwrap();
        let to = CString::new(to.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: two live strings.
        unsafe { copyfile(from.as_ptr(), to.as_ptr(), std::ptr::null_mut(), flags) }
    }

    #[test]
    fn copies_a_tree_recursively_into_a_new_directory() {
        let dir = scratch("tree");
        std::fs::create_dir_all(dir.join("src/sub")).unwrap();
        std::fs::write(dir.join("src/a.plist"), b"a").unwrap();
        std::fs::write(dir.join("src/sub/b.plist"), b"b").unwrap();
        assert_eq!(call(&dir.join("src"), &dir.join("dst"), ALL | RECURSIVE), 0);
        assert_eq!(std::fs::read(dir.join("dst/a.plist")).unwrap(), b"a");
        assert_eq!(std::fs::read(dir.join("dst/sub/b.plist")).unwrap(), b"b");
    }

    #[test]
    fn copies_a_directory_inside_an_existing_one_like_cp() {
        let dir = scratch("inside");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        std::fs::write(dir.join("src/a"), b"a").unwrap();
        assert_eq!(call(&dir.join("src"), &dir.join("dst"), ALL | RECURSIVE), 0);
        assert_eq!(std::fs::read(dir.join("dst/src/a")).unwrap(), b"a");
    }

    #[test]
    fn exclusive_copy_refuses_an_existing_destination() {
        let dir = scratch("excl");
        std::fs::write(dir.join("a"), b"a").unwrap();
        std::fs::write(dir.join("b"), b"b").unwrap();
        assert_eq!(call(&dir.join("a"), &dir.join("b"), ALL | EXCL), -1);
        assert_eq!(crate::errno::get(), libc::EEXIST);
        assert_eq!(call(&dir.join("a"), &dir.join("c"), ALL), 0);
        assert_eq!(std::fs::read(dir.join("c")).unwrap(), b"a");
    }
}
