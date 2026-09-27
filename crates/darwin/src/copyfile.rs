#![cfg(not(target_os = "macos"))]

//! `copyfile(3)`, which has no host counterpart. Feral's games copy their
//! bundled `InputDevices` tree into a cache with it before opening a window.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The flag values, from Apple's own `copyfile.h`
/// (`apple-oss-distributions/copyfile`), which is the authority rather than a
/// recollection:
///
/// ```c
/// #define COPYFILE_ACL	    (1<<0)
/// #define COPYFILE_STAT	    (1<<1)
/// #define COPYFILE_XATTR	    (1<<2)
/// #define COPYFILE_DATA	    (1<<3)
/// #define COPYFILE_SECURITY   (COPYFILE_STAT | COPYFILE_ACL)
/// #define COPYFILE_METADATA   (COPYFILE_SECURITY | COPYFILE_XATTR)
/// #define COPYFILE_ALL	    (COPYFILE_METADATA | COPYFILE_DATA)   /* 0x0f */
/// #define COPYFILE_RECURSIVE	(1<<15)
/// #define COPYFILE_CHECK		(1<<16)
/// #define COPYFILE_EXCL		(1<<17)
/// #define COPYFILE_NOFOLLOW_SRC	(1<<18)
/// #define COPYFILE_NOFOLLOW_DST	(1<<19)
/// #define COPYFILE_MOVE		(1<<20)
/// #define COPYFILE_UNLINK		(1<<21)
/// ```
///
/// `COPYFILE_DATA` being `1<<3` is the load-bearing one. A second `copyfile`
/// used to live in `crate::media_unavailable` with `DATA = 0x4` and `STAT = 0x8`,
/// and it won every guest import; see the note there. `base::CopyFile` is the
/// guest's only `_copyfile` call site and it passes `COPYFILE_DATA` and nothing
/// else, so this row is the one that has to be right.
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
/// ACLs and extended attributes are **refused**, with `EINVAL`, rather than
/// quietly skipped.
///
/// Everything else is answered for real: `COPYFILE_DATA`, `COPYFILE_STAT`,
/// `COPYFILE_RECURSIVE`, `COPYFILE_CHECK`, `COPYFILE_EXCL`,
/// `COPYFILE_NOFOLLOW_SRC`, `COPYFILE_MOVE` and `COPYFILE_UNLINK`. A caller
/// that asked for an ACL and got a data copy and a `0` back has been told a
/// thing that did not happen, which is the one outcome this tree may not
/// produce. The guest cannot reach that path - `base::CopyFile` passes
/// `COPYFILE_DATA` alone, at `Framework+0x5d94848` - but a program that asked
/// for one deserves to be told no.
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
    if flags & (ACL | XATTR) != 0 {
        return fail(std::io::Error::from_raw_os_error(libc::EINVAL));
    }
    if flags & CHECK != 0 {
        // What would be copied: everything asked for that there is, which after
        // the line above is never an ACL or an xattr.
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

    /// What this host can do, which is `COPYFILE_ALL` without the two flags it
    /// refuses. The three tests below used to pass `ALL` and pass only because
    /// the ACL and xattr bits were being ignored; they are spelled out now so
    /// that the refusal above is what the tests are measuring.
    const DOABLE: u32 = STAT | DATA;

    #[test]
    fn copies_a_tree_recursively_into_a_new_directory() {
        let dir = scratch("tree");
        std::fs::create_dir_all(dir.join("src/sub")).unwrap();
        std::fs::write(dir.join("src/a.plist"), b"a").unwrap();
        std::fs::write(dir.join("src/sub/b.plist"), b"b").unwrap();
        assert_eq!(call(&dir.join("src"), &dir.join("dst"), DOABLE | RECURSIVE), 0);
        assert_eq!(std::fs::read(dir.join("dst/a.plist")).unwrap(), b"a");
        assert_eq!(std::fs::read(dir.join("dst/sub/b.plist")).unwrap(), b"b");
    }

    #[test]
    fn copies_a_directory_inside_an_existing_one_like_cp() {
        let dir = scratch("inside");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        std::fs::write(dir.join("src/a"), b"a").unwrap();
        assert_eq!(call(&dir.join("src"), &dir.join("dst"), DOABLE | RECURSIVE), 0);
        assert_eq!(std::fs::read(dir.join("dst/src/a")).unwrap(), b"a");
    }

    #[test]
    fn exclusive_copy_refuses_an_existing_destination() {
        let dir = scratch("excl");
        std::fs::write(dir.join("a"), b"a").unwrap();
        std::fs::write(dir.join("b"), b"b").unwrap();
        assert_eq!(call(&dir.join("a"), &dir.join("b"), DOABLE | EXCL), -1);
        assert_eq!(crate::errno::get(), libc::EEXIST);
        assert_eq!(call(&dir.join("a"), &dir.join("c"), DOABLE), 0);
        assert_eq!(std::fs::read(dir.join("c")).unwrap(), b"a");
    }

    /// The guest's own call, with the guest's own flags.
    ///
    /// `base::CopyFile` in `base/files/file_util_apple.mm` is
    ///
    /// ```c
    /// return (copyfile(from_path.value().c_str(), to_path.value().c_str(),
    ///                  /*state=*/nullptr, COPYFILE_DATA) == 0);
    /// ```
    ///
    /// and the binary agrees: `Framework+0x5d94843` is `mov ecx,0x8` and
    /// `+0x5d94848` is `call _copyfile` - re-disassembled, and the only
    /// `_copyfile` call site in the guest (`test/tools/callers.py _copyfile`).
    /// `8` is `COPYFILE_DATA`, `(1<<3)`.
    ///
    /// So this is the assertion a second `copyfile` with `DATA = 0x4` would
    /// fail: it copied no bytes, silently rewrote the destination's mode and
    /// timestamps instead, and returned `0` - and `sete bl` at `+0x5d9484f`
    /// turned that into `base::CopyFile` returning **true**. A file reported
    /// copied that had not been. The constants above are quoted from Apple's
    /// `copyfile.h`; this test is what holds them to the guest's call.
    #[test]
    fn the_guests_only_call_copies_the_bytes() {
        let dir = scratch("guest");
        std::fs::write(dir.join("from"), b"the bytes the guest asked to copy").unwrap();
        assert_eq!(DATA, 8, "COPYFILE_DATA is (1<<3); the guest passes 8");
        assert_eq!(call(&dir.join("from"), &dir.join("to"), DATA), 0);
        assert_eq!(
            std::fs::read(dir.join("to")).unwrap(),
            b"the bytes the guest asked to copy",
            "`COPYFILE_DATA` alone must copy the data, not the stat"
        );
    }

    /// An ACL or an xattr is refused, and says so, rather than being skipped.
    ///
    /// Each of these returned `0` having copied only the data. That is the
    /// fabricated success this tree may not produce: a caller that asked for an
    /// ACL cannot tell an absent ACL from a copied one, and `0` says it was
    /// copied.
    #[test]
    fn an_acl_or_an_xattr_is_refused_rather_than_skipped() {
        let dir = scratch("refuse");
        std::fs::write(dir.join("from"), b"a").unwrap();
        for (name, flags) in [
            ("COPYFILE_ACL", ACL),
            ("COPYFILE_XATTR", XATTR),
            ("COPYFILE_ALL", ALL),
            ("COPYFILE_SECURITY", STAT | ACL),
            ("COPYFILE_METADATA", STAT | ACL | XATTR),
        ] {
            assert_eq!(
                call(&dir.join("from"), &dir.join("to"), flags),
                -1,
                "{name} is not something this host does, so it must not answer 0"
            );
            assert_eq!(crate::errno::get(), libc::EINVAL, "{name}");
            assert!(
                !dir.join("to").exists(),
                "{name}: nothing may be written when the request is refused"
            );
        }
    }
}
