//! Cache files on disk: written whole and renamed into place, read by mapping.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Writes `bytes` to `path` through a temporary file in the same directory.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = AtomicFile::create(path)?;
    file.write_all(bytes)?;
    file.commit()
}

/// A file written under a temporary name in its directory and renamed to its own
/// name by [`AtomicFile::commit`]; dropped uncommitted, it is removed.
///
/// Several processes may build the same image, so the file appears at its name only
/// by a rename, which is atomic within a filesystem; two writers write equivalent
/// bytes, so whichever rename lands last is correct.
pub struct AtomicFile {
    file: File,
    temporary: PathBuf,
    path: PathBuf,
    committed: bool,
}

impl AtomicFile {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let directory = path
            .parent()
            .ok_or_else(|| std::io::Error::other("a cache file needs a directory"))?;
        std::fs::create_dir_all(directory)?;
        let temporary = directory.join(format!(
            ".{}.{}.tmp",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        Ok(AtomicFile {
            file: File::create(&temporary)?,
            temporary,
            path: path.to_owned(),
            committed: false,
        })
    }

    pub fn commit(mut self) -> std::io::Result<()> {
        self.file.sync_all()?;
        std::fs::rename(&self.temporary, &self.path)?;
        self.committed = true;
        // The rename itself is durable only once the directory is.
        let directory = self.path.parent().expect("created with a directory");
        File::open(directory)?.sync_all()
    }
}

impl Write for AtomicFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.file.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl std::io::Seek for AtomicFile {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.file.seek(to)
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.temporary);
        }
    }
}

/// A cache file mapped read-only. A rename over it later leaves this mapping intact.
pub struct Mapped {
    ptr: *mut libc::c_void,
    len: usize,
}

// SAFETY: a private read-only mapping no one writes through.
unsafe impl Send for Mapped {}
unsafe impl Sync for Mapped {}

impl std::ops::Deref for Mapped {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: `ptr` maps `len` readable bytes until `drop`.
        unsafe { std::slice::from_raw_parts(self.ptr.cast(), self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        if self.len != 0 {
            // SAFETY: unmapping exactly what `map` mapped.
            unsafe { libc::munmap(self.ptr, self.len) };
        }
    }
}

/// Maps `path`, or `None` if it does not exist.
pub fn map(path: &Path) -> std::io::Result<Option<Mapped>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let len = usize::try_from(file.metadata()?.len()).map_err(std::io::Error::other)?;
    if len == 0 {
        return Ok(Some(Mapped {
            ptr: std::ptr::null_mut(),
            len,
        }));
    }
    // SAFETY: a fresh private read-only mapping of a file this process opened.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    Ok(Some(Mapped { ptr, len }))
}

/// Held while one process builds a cache file; released when dropped.
#[derive(Debug)]
pub struct BuildLock {
    _file: File,
}

/// The sidecar beside `path` that [`try_lock`] takes.
pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

/// Takes `path`'s build lock without waiting, or `None` if another process holds it.
///
/// Avoids duplicated work only: readers never take it, and a writer that ignored it
/// would still only ever rename a whole file into place.
pub fn try_lock(path: &Path) -> std::io::Result<Option<BuildLock>> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path(path))?;
    // SAFETY: `flock` on a descriptor this function owns.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(BuildLock { _file: file }));
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EWOULDBLOCK) => Ok(None),
        _ => Err(error),
    }
}

/// Removes every entry of `base` not named in `keep`.
///
/// A process of another build may still be using one; it keeps its mappings, and a
/// write it attempts afterwards fails and is dropped like any other cache write.
pub fn prune(base: &Path, keep: &[&str]) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if keep.iter().any(|k| name == **k) {
            continue;
        }
        let path = entry.path();
        let _ = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rosemary-aot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_written_file_maps_back_whole() {
        let dir = scratch("write");
        let path = dir.join("a/b.rmc");
        write_atomic(&path, b"first").unwrap();
        assert_eq!(&*map(&path).unwrap().unwrap(), b"first");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["b.rmc"], "a temporary file was left behind");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_uncommitted_file_leaves_nothing_behind() {
        let dir = scratch("uncommitted");
        let path = dir.join("e.rmc");
        let mut file = AtomicFile::create(&path).unwrap();
        file.write_all(b"partial").unwrap();
        drop(file);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_mapping_survives_a_rename_over_it() {
        let dir = scratch("rename");
        let path = dir.join("c.rmc");
        write_atomic(&path, b"old").unwrap();
        let mapped = map(&path).unwrap().unwrap();
        write_atomic(&path, b"new!").unwrap();
        assert_eq!(&*mapped, b"old");
        assert_eq!(&*map(&path).unwrap().unwrap(), b"new!");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_file_maps_to_none() {
        assert!(map(&scratch("missing").join("x")).unwrap().is_none());
    }

    #[test]
    fn a_second_build_lock_is_refused_until_the_first_drops() {
        let dir = scratch("lock");
        let path = dir.join("d.rmc");
        let first = try_lock(&path).unwrap();
        assert!(first.is_some());
        // `flock` locks belong to the open file, so a second open contends even in one process.
        assert!(try_lock(&path).unwrap().is_none());
        drop(first);
        assert!(try_lock(&path).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pruning_keeps_only_what_it_is_told_to() {
        let dir = scratch("prune");
        for name in ["current", "old", "profile"] {
            std::fs::create_dir_all(dir.join(name).join("x")).unwrap();
        }
        std::fs::write(dir.join("stray"), b"").unwrap();
        prune(&dir, &["current", "profile"]);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["current", "profile"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
