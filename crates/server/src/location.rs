//! Where a server listens.
//!
//! One server per prefix, as one wineserver per Wine prefix: processes in
//! different prefixes are different machines and must not see each other.
//! The socket is named for the prefix directory's device and inode rather than
//! its path, so two spellings of one prefix meet the same server and a prefix
//! moved elsewhere does not.

use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// The mode Rosemary creates its directories with: this user's alone.
const PRIVATE_MODE: u32 = 0o700;
/// Group and other permission bits, none of which a trusted directory has.
const SHARED_BITS: u32 = 0o077;

/// Where this user's runtime directory for Rosemary is.
///
/// Only a path: nothing found there is trusted until [`prepare`] has checked
/// it, since anyone able to listen on a socket in it would be the server.
pub fn runtime_directory() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => PathBuf::from(runtime).join("rosemary"),
        None => fallback_base(),
    }
}

/// macOS gives each user a private temporary directory; a shared `/tmp` is
/// only the last resort, and [`prepare`] refuses one another user got to first.
#[cfg(target_vendor = "apple")]
fn fallback_base() -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: `buf` is writable for the length passed.
    let len = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    // `len` counts the NUL; 0 is failure, and more than `buf` a truncation.
    if len == 0 || len > buf.len() {
        return shared_tmp_base();
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len - 1])).join("rosemary")
}

#[cfg(not(target_vendor = "apple"))]
fn fallback_base() -> PathBuf {
    shared_tmp_base()
}

fn shared_tmp_base() -> PathBuf {
    // SAFETY: `getuid` has no preconditions.
    PathBuf::from(format!("/tmp/rosemary-{}", unsafe { libc::getuid() }))
}

/// `name` in the runtime directory, made and checked by [`prepare`].
pub fn private_runtime_directory(name: &str) -> io::Result<PathBuf> {
    let directory = runtime_directory().join(name);
    prepare(&directory)?;
    Ok(directory)
}

/// The directory a server for `domain` keeps its socket and lock in; `None`
/// is the processes that run outside any prefix.
pub fn directory_for(domain: Option<&Path>) -> PathBuf {
    let leaf = match domain.and_then(|dir| std::fs::metadata(dir).ok()) {
        Some(meta) => format!("server-{:x}-{:x}", meta.dev(), meta.ino()),
        None => "server-host".to_string(),
    };
    runtime_directory().join(leaf)
}

pub fn socket_in(directory: &Path) -> PathBuf {
    directory.join("socket")
}

pub fn lock_in(directory: &Path) -> PathBuf {
    directory.join("lock")
}

/// Make `directory`, or find it already made, and refuse it unless it is this
/// user's alone. Inside the runtime directory every level from the runtime
/// directory down is checked, so none can be swapped from outside; a directory
/// elsewhere was chosen by the user, who answers for its parents.
pub fn prepare(directory: &Path) -> io::Result<()> {
    let runtime = runtime_directory();
    match directory.strip_prefix(&runtime) {
        Ok(inner) => {
            let mut level = runtime;
            ensure_private(&level)?;
            for part in inner.components() {
                level.push(part);
                ensure_private(&level)?;
            }
        }
        Err(_) => {
            if let Some(parent) = directory.parent() {
                std::fs::create_dir_all(parent)?;
            }
            ensure_private(directory)?;
        }
    }
    Ok(())
}

/// Create `directory` private, or check that the one already there is: a real
/// directory, not a link, owned by this user, and closed to everyone else.
fn ensure_private(directory: &Path) -> io::Result<()> {
    match std::fs::DirBuilder::new()
        .mode(PRIVATE_MODE)
        .create(directory)
    {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(directory)?;
    // SAFETY: `getuid` has no preconditions.
    let me = unsafe { libc::getuid() };
    let problem = if !meta.file_type().is_dir() {
        "is not a directory"
    } else if meta.uid() != me {
        "belongs to another user"
    } else if meta.mode() & SHARED_BITS != 0 {
        "is open to other users"
    } else {
        return Ok(());
    };
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} {problem}", directory.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rosemary-location-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_runtime_directory_others_can_write_is_refused() {
        let parent = scratch("open");
        let directory = parent.join("server");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777)).unwrap();
        let error = prepare(&directory).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn a_runtime_directory_that_is_a_symlink_is_refused() {
        let parent = scratch("link");
        let target = parent.join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        let directory = parent.join("server");
        std::os::unix::fs::symlink(&target, &directory).unwrap();
        let error = prepare(&directory).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn a_private_directory_already_there_is_accepted() {
        let parent = scratch("again");
        let directory = parent.join("server");
        prepare(&directory).unwrap();
        prepare(&directory).unwrap();
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn a_custom_directory_leaves_its_parent_permissions_alone() {
        let parent =
            std::env::temp_dir().join(format!("rosemary-server-parent-{}", std::process::id()));
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o750)).unwrap();
        let directory = parent.join("server");
        prepare(&directory).unwrap();
        assert_eq!(std::fs::metadata(&parent).unwrap().mode() & 0o777, 0o750);
        assert_eq!(std::fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
        std::fs::remove_dir_all(parent).unwrap();
    }
}
