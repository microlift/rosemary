use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

/// Resolve existing ancestors while preserving the spelling of a new suffix.
pub(crate) fn for_creation(path: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let _errno = starnix::errno::Held::now();
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        if ancestor.symlink_metadata().is_ok() {
            break;
        }
        if let Some(found) = CString::new(ancestor.as_os_str().as_bytes())
            .ok()
            .and_then(|name| insensitive(&name))
        {
            let mut resolved = PathBuf::from(std::ffi::OsStr::from_bytes(found.as_bytes()));
            for part in suffix.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        let (Some(name), Some(parent)) = (ancestor.file_name(), ancestor.parent()) else {
            break;
        };
        suffix.push(name);
        ancestor = parent;
    }
    path.to_path_buf()
}

/// `path`, spelled the way the filesystem spells it, or `None` if no
/// case-insensitive reading of it exists either.
///
/// `None` for a path that already exists, because the caller is expected to have
/// tried it: this is the answer to "the exact name failed", not a replacement
/// for it.
///
/// `errno` is put back before returning. The caller has already failed a call
/// and is about to report that failure, so the walk's own syscalls must not be
/// what the guest reads: Rust's `read_dir` clears `errno` before each `readdir`
/// to tell the end of a directory from an error, so a walk that finds nothing
/// turns the caller's `ENOENT` into a failure with `errno == 0`. Boost reads
/// that as "not a not-found error" and throws `status: Success`.
pub(crate) fn insensitive(path: &CStr) -> Option<CString> {
    let _errno = starnix::errno::Held::now();
    let text = path.to_str().ok()?;
    if text.is_empty() {
        return None;
    }
    let wanted = Path::new(text);
    if wanted.symlink_metadata().is_ok() {
        return None;
    }
    let mut at = match wanted.is_absolute() {
        true => PathBuf::from("/"),
        false => PathBuf::from("."),
    };
    for part in wanted.components() {
        use std::path::Component;
        match part {
            Component::RootDir => continue,
            Component::CurDir => continue,
            Component::ParentDir => {
                at.push("..");
                continue;
            }
            Component::Prefix(_) => return None,
            Component::Normal(name) => {
                let exact = at.join(name);
                if exact.symlink_metadata().is_ok() {
                    at = exact;
                    continue;
                }
                at = sole_match(&at, name.to_str()?)?;
            }
        }
    }
    // A walk that ended where it started found nothing worth reporting.
    let found = at.to_str()?;
    (found != text).then(|| CString::new(found).ok())?
}

/// [`insensitive`] for a path interpreted relative to an open directory.
///
/// The descriptor's path preserves `openat`'s starting point without changing
/// the process working directory.
pub(crate) fn insensitive_at(dirfd: libc::c_int, path: &CStr) -> Option<CString> {
    if path.to_bytes().starts_with(b"/") || dirfd == libc::AT_FDCWD {
        return insensitive(path);
    }
    let mut anchored = super::guard::directory_of(dirfd)?
        .into_os_string()
        .into_vec();
    anchored.push(b'/');
    anchored.extend_from_slice(path.to_bytes());
    insensitive(&CString::new(anchored).ok()?)
}

/// The one entry of `directory` whose name equals `name` ignoring case.
///
/// `None` for none and for more than one. Two entries differing only in case is
/// a directory this cannot speak for, and picking either would be a coin toss
/// the caller has no way to notice.
fn sole_match(directory: &Path, name: &str) -> Option<PathBuf> {
    let mut found = None;
    for entry in std::fs::read_dir(directory).ok()? {
        // An incomplete enumeration cannot prove that a match is unique. In
        // particular, silently skipping an unreadable entry could make one of
        // two case-folded names look like the sole match.
        let entry = entry.ok()?;
        let candidate = entry.file_name();
        let candidate = candidate.to_str()?;
        if !candidate.eq_ignore_ascii_case(name) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(entry.path());
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory with a known mixed-case name in it, removed when the test
    /// ends whichever way it ends.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let at = std::env::temp_dir().join(format!("rosemary-path-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("Contents")).expect("a scratch directory");
        std::fs::write(at.join("Contents").join("Game.ios"), b"x").expect("a file");
        Scratch(at)
    }

    #[test]
    fn a_lowercased_path_finds_the_file() {
        let scratch = scratch("lower");
        let lowered = CString::new(
            scratch
                .0
                .join("contents")
                .join("game.ios")
                .to_str()
                .expect("utf-8"),
        )
        .expect("no NUL");
        let found = insensitive(&lowered);
        if scratch.0.join("contents/game.ios").exists() {
            assert_eq!(found, None);
            return;
        }
        let found = found.expect("a case-insensitive match");
        assert_eq!(
            found.to_str().expect("utf-8"),
            scratch
                .0
                .join("Contents")
                .join("Game.ios")
                .to_str()
                .unwrap()
        );
    }

    /// A path that is already right is not "resolved" to itself: the caller has
    /// tried it, and answering would send it round the same failure twice.
    #[test]
    fn an_exact_path_is_not_a_match() {
        let scratch = scratch("exact");
        let exact = CString::new(
            scratch
                .0
                .join("Contents")
                .join("Game.ios")
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(insensitive(&exact), None);
    }

    #[test]
    fn creation_uses_existing_parent_case_and_keeps_new_names() {
        let scratch = scratch("create");
        let wanted = scratch.0.join("contents/NewDirectory/Child");
        let resolved = for_creation(&wanted);
        std::fs::create_dir_all(&resolved).unwrap();
        assert!(scratch.0.join("Contents/NewDirectory/Child").is_dir());
    }

    #[test]
    fn creation_does_not_create_missing_parents_or_choose_ambiguous_names() {
        let scratch = scratch("create-ambiguous");
        let missing = for_creation(&scratch.0.join("contents/Missing/Child"));
        assert!(std::fs::create_dir(&missing).is_err());
        if scratch.0.join("contents").exists() {
            return; // A case-insensitive volume cannot hold an ambiguous pair.
        }
        std::fs::create_dir(scratch.0.join("CONTENTS")).unwrap();
        let wanted = scratch.0.join("contents/NewDirectory");
        assert_eq!(for_creation(&wanted), wanted);
    }

    #[test]
    fn a_path_that_is_not_there_in_any_case_is_none() {
        let scratch = scratch("absent");
        let absent =
            CString::new(scratch.0.join("contents").join("nothing").to_str().unwrap()).unwrap();
        assert_eq!(insensitive(&absent), None);
    }

    /// The walk reads directories, and `read_dir` clears `errno` on the way. The
    /// caller is reporting the failure that sent it here, so that must not show.
    #[test]
    fn the_walk_leaves_errno_where_it_found_it() {
        let scratch = scratch("errno");
        let absent =
            CString::new(scratch.0.join("Contents").join("nothing").to_str().unwrap()).unwrap();
        starnix::errno::set(libc::ENOENT);
        assert_eq!(insensitive(&absent), None);
        assert_eq!(starnix::errno::get(), libc::ENOENT);
        starnix::errno::set(0);
    }

    #[test]
    fn a_relative_path_can_start_at_a_directory_descriptor() {
        let scratch = scratch("at");
        let directory = CString::new(scratch.0.to_str().expect("utf-8")).expect("no NUL");
        // SAFETY: a NUL-terminated path and native flags.
        let dirfd = unsafe { libc::open(directory.as_ptr(), libc::O_RDONLY) };
        assert!(dirfd >= 0);
        let found = insensitive_at(dirfd, c"contents/game.ios");
        // SAFETY: a descriptor this test just opened.
        unsafe { libc::close(dirfd) };

        if scratch.0.join("contents/game.ios").exists() {
            assert_eq!(found, None);
        } else {
            assert!(found.unwrap().to_bytes().ends_with(b"/Contents/Game.ios"));
        }
    }

    /// Darwin accepts paths deeper than an arbitrary implementation limit as
    /// long as the byte length remains within the filesystem's limit.
    #[test]
    fn a_valid_path_with_more_than_sixty_four_components_is_walked() {
        let at = std::env::temp_dir().join(format!("rosemary-path-deep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        let mut directory = at.clone();
        for _ in 0..70 {
            directory.push("D");
        }
        std::fs::create_dir_all(&directory).expect("a deep directory");
        std::fs::write(directory.join("Leaf"), b"x").expect("a leaf");

        let mut wanted = at;
        for _ in 0..70 {
            wanted.push("d");
        }
        wanted.push("leaf");
        let wanted = CString::new(wanted.to_str().expect("utf-8")).expect("no NUL");
        let found = insensitive(&wanted);
        if Path::new(wanted.to_str().unwrap()).exists() {
            assert_eq!(found, None);
        } else {
            assert!(found.unwrap().to_bytes().ends_with(b"/D/D/D/D/D/Leaf"));
        }

        // The resolved path owns no resources, so the tree can be removed now.
        let root = std::env::temp_dir().join(format!("rosemary-path-deep-{}", std::process::id()));
        std::fs::remove_dir_all(root).expect("the fixture is removed");
    }
}
