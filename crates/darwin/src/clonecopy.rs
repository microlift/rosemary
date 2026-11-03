//! The copy `clonefile` makes when the host cannot clone: data, mode and
//! times, a directory with everything under it.

use std::ffi::CString;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

const STAT: u32 = 1 << 1;
const DATA: u32 = 1 << 3;
const RECURSIVE: u32 = 1 << 15;
const EXCL: u32 = 1 << 17;
const NOFOLLOW_SRC: u32 = 1 << 18;

fn may_write(path: &Path) -> bool {
    let Ok(c) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        crate::errno::set(libc::EINVAL);
        return false;
    };
    crate::stat::may_write(c.as_ptr())
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
    let kind = meta.file_type();
    if kind.is_symlink() {
        if existing.is_some() {
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
            .truncate(false) // truncated below, after the same-file check
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

/// Copy `from` to `to`, which does not exist yet.
fn copy(from: &Path, to: &Path, flags: u32) -> std::io::Result<()> {
    let meta = if flags & NOFOLLOW_SRC != 0 {
        std::fs::symlink_metadata(from)?
    } else {
        std::fs::metadata(from)?
    };
    let recursive = flags & RECURSIVE != 0 && meta.is_dir();
    if !recursive {
        return copy_one(from, to, flags);
    }
    // The top directory follows a link given as the source, as `fts` does for
    // its root.
    let canonical_source = std::fs::canonicalize(from)?;
    let mut ancestor = to;
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
    copy_one(from, to, flags & !(NOFOLLOW_SRC | STAT))?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_tree(&entry.path(), &to.join(entry.file_name()), flags)?;
    }
    if flags & STAT != 0 {
        copy_one(from, to, STAT)?;
    }
    Ok(())
}

/// Make the independent copy used when the host cannot create an APFS clone.
pub(crate) fn clonefile(from: &Path, to: &Path, nofollow: bool) -> std::io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::Error::from_raw_os_error(libc::EEXIST));
    }
    let flags = STAT | DATA | RECURSIVE | EXCL | if nofollow { NOFOLLOW_SRC } else { 0 };
    copy(from, to, flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("darwin-clonecopy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copies_a_tree_recursively_into_a_new_directory() {
        let dir = scratch("tree");
        std::fs::create_dir_all(dir.join("src/sub")).unwrap();
        std::fs::write(dir.join("src/a.plist"), b"a").unwrap();
        std::fs::write(dir.join("src/sub/b.plist"), b"b").unwrap();
        clonefile(&dir.join("src"), &dir.join("dst"), false).unwrap();
        assert_eq!(std::fs::read(dir.join("dst/a.plist")).unwrap(), b"a");
        assert_eq!(std::fs::read(dir.join("dst/sub/b.plist")).unwrap(), b"b");
    }

    #[test]
    fn refuses_an_existing_destination() {
        let dir = scratch("excl");
        std::fs::write(dir.join("a"), b"a").unwrap();
        std::fs::write(dir.join("b"), b"b").unwrap();
        let error = clonefile(&dir.join("a"), &dir.join("b"), false).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
        clonefile(&dir.join("a"), &dir.join("c"), false).unwrap();
        assert_eq!(std::fs::read(dir.join("c")).unwrap(), b"a");
    }

    #[test]
    fn refuses_to_copy_a_directory_into_itself() {
        let dir = scratch("into");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let error = clonefile(&dir.join("src"), &dir.join("src/inner"), false).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
    }
}
