//! `copyfile(3)`, which has no host counterpart. Feral's games copy their
//! bundled `InputDevices` tree into a cache with it before opening a window.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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

/// State values retain their ABI width and own filename storage.
enum StateValue {
    Word(usize),
    Name(CString),
}
const STATE_SIZE: usize = 1;
static STATE_VALUES: OnceLock<Mutex<HashMap<(usize, u32), StateValue>>> = OnceLock::new();

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
    let existing = match std::fs::symlink_metadata(to) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if std::fs::metadata(to).is_ok_and(|dst| dst.dev() == meta.dev() && dst.ino() == meta.ino()) {
        return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
    }
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
        let mut source = std::fs::File::open(from)?;
        let mut destination = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            // Check for source/destination identity before truncating the destination.
            .truncate(false)
            .create_new(flags & EXCL != 0)
            .open(to)?;
        let dst = destination.metadata()?;
        if dst.dev() == meta.dev() && dst.ino() == meta.ino() {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        }
        destination.set_len(0)?;
        std::io::copy(&mut source, &mut destination)?;
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
            if unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Copy the tree at `from` onto `to`, entries before their contents.
fn copy_tree(from: &Path, to: &Path, flags: u32) -> std::io::Result<()> {
    // A tree is walked physically, as `copyfile`'s own `fts` walk is: a link
    // inside it is copied as a link.
    if !std::fs::symlink_metadata(from)?.is_dir() {
        return copy_one(from, to, flags | NOFOLLOW_SRC);
    }
    copy_one(from, to, (flags | NOFOLLOW_SRC) & !STAT)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_tree(&entry.path(), &to.join(entry.file_name()), flags)?;
    }
    if flags & STAT != 0 {
        copy_one(from, to, STAT | NOFOLLOW_SRC)?;
    }
    Ok(())
}

fn copy(from: &Path, to: &Path, flags: u32, from_text: &[u8]) -> std::io::Result<()> {
    let meta = if flags & NOFOLLOW_SRC != 0 {
        std::fs::symlink_metadata(from)?
    } else {
        std::fs::metadata(from)?
    };
    let recursive = flags & RECURSIVE != 0 && meta.is_dir();
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
    let canonical_source = std::fs::canonicalize(from)?;
    let mut ancestor = to.as_path();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(path) => {
                if path.starts_with(&canonical_source) {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                }
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
            }
            Err(e) => return Err(e),
        }
    }
    copy_one(from, &to, flags & !(NOFOLLOW_SRC | STAT))?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_tree(&entry.path(), &to.join(entry.file_name()), flags)?;
    }
    if flags & STAT != 0 {
        copy_one(from, &to, STAT)?;
    }
    Ok(())
}

/// Make the independent copy used when the host cannot create an APFS clone.
pub(crate) fn clonefile(from: &Path, to: &Path, nofollow: bool) -> std::io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::Error::from_raw_os_error(libc::EEXIST));
    }
    let flags = ALL | RECURSIVE | EXCL | if nofollow { NOFOLLOW_SRC } else { 0 };
    copy(from, to, flags, from.as_os_str().as_bytes())
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
    let Some(source) = host(from) else {
        return -1;
    };
    if flags & CHECK != 0 {
        // What would be copied: everything asked for that there is.
        return match std::fs::symlink_metadata(&source) {
            Ok(_) => 0,
            Err(e) => fail(e),
        };
    }
    let Some(destination) = host(to) else {
        return -1;
    };
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
pub unsafe extern "C" fn fcopyfile(
    from: c_int,
    to: c_int,
    _state: *mut c_void,
    flags: u32,
) -> c_int {
    use std::io::{Seek, SeekFrom};
    use std::os::fd::FromRawFd;

    if from < 0 || to < 0 {
        crate::errno::set(libc::EBADF);
        return -1;
    }
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
        if let Some(values) = STATE_VALUES.get() {
            values
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retain(|(owner, _), _| *owner != state as usize);
        }
        // SAFETY: the caller's obligation.
        unsafe { crate::heap::free(state) };
    }
    0
}

/// Configure a copy state using the flag-specific Darwin argument convention.
pub unsafe extern "C" fn copyfile_state_set(
    state: *mut c_void,
    flag: u32,
    source: *const c_void,
) -> c_int {
    if state.is_null() || source.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let value = match flag {
        1 | 3 | 11 | 12 | 13 | 15 | 16 | 18 => {
            StateValue::Word(unsafe { source.cast::<u32>().read_unaligned() } as usize)
        }
        2 | 4 => StateValue::Name(unsafe { CStr::from_ptr(source.cast()) }.to_owned()),
        6 | 7 => StateValue::Word(source as usize),
        14 => StateValue::Word(unsafe { source.cast::<u8>().read() } as usize),
        _ => {
            crate::errno::set(libc::EINVAL);
            return -1;
        }
    };
    let mut values = STATE_VALUES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if flag == 13 {
        let StateValue::Word(value) = value else {
            unreachable!()
        };
        values.insert((state as usize, 11), StateValue::Word(value));
        values.insert((state as usize, 12), StateValue::Word(value));
    } else {
        values.insert((state as usize, flag), value);
    }
    0
}

pub unsafe extern "C" fn copyfile_state_get(
    state: *mut c_void,
    flag: u32,
    destination: *mut c_void,
) -> c_int {
    if state.is_null() || destination.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let values = STATE_VALUES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let value = match values.get(&(state as usize, flag)) {
        Some(StateValue::Word(value)) => *value,
        Some(StateValue::Name(name)) => name.as_ptr() as usize,
        None if matches!(flag, 1 | 3) => (-2i32) as u32 as usize,
        None => 0,
    };
    unsafe {
        match flag {
            1 | 3 | 11 | 12 | 15 | 16 | 18 => {
                destination.cast::<u32>().write_unaligned(value as u32)
            }
            10 | 14 => destination.cast::<u8>().write(value as u8),
            2 | 4 | 5 | 6 | 7 | 8 | 9 | 17 => destination.cast::<usize>().write_unaligned(value),
            _ => {
                crate::errno::set(libc::EINVAL);
                return -1;
            }
        }
    }
    0
}

exports::exports! {
    functions {
        "copyfile" => copyfile,
        "fcopyfile" => fcopyfile,
        "copyfile_state_alloc" => copyfile_state_alloc,
        "copyfile_state_free" => copyfile_state_free,
        "copyfile_state_set" => copyfile_state_set,
        "copyfile_state_get" => copyfile_state_get,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("darwin-copyfile-{name}-{}", std::process::id()));
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
