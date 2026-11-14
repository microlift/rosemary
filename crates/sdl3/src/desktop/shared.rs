//! A window's pixels, in memory its guest and the desktop's host both map.
//!
//! The guest makes a file in the desktop's private directory, maps it, names
//! it to the host, and unlinks it once the host has it open: from then on the
//! memory lives exactly as long as the two mappings.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) const BYTES_PER_PIXEL: usize = 4;

/// The mode of a buffer's file: this user's alone, as its directory is.
const PRIVATE_MODE: u32 = 0o600;

/// A side longer than this is not a window's.
const MAX_SIDE: i32 = 1 << 15;

pub(crate) struct Shared {
    pixels: NonNull<u8>,
    len: usize,
    pub width: i32,
    pub height: i32,
}

// SAFETY: the mapping is this value's alone in this process.
unsafe impl Send for Shared {}

fn len_of(width: i32, height: i32) -> io::Result<usize> {
    if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a window's size",
        ));
    }
    Ok(width as usize * height as usize * BYTES_PER_PIXEL)
}

impl Shared {
    /// A new buffer in `directory`, all zeroes, and the name of its file
    /// there, which is the caller's to remove.
    pub(crate) fn create(
        directory: &Path,
        width: i32,
        height: i32,
    ) -> io::Result<(Shared, PathBuf)> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let len = len_of(width, height)?;
        let name = format!(
            "buffer-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = directory.join(&name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(PRIVATE_MODE)
            .open(&path)?;
        let made = file
            .set_len(len as u64)
            .and_then(|()| Shared::map(&file, len, width, height));
        match made {
            Ok(shared) => Ok((shared, PathBuf::from(name))),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(e)
            }
        }
    }

    /// The buffer a guest made at `path`, which must hold the pixels it says.
    pub(crate) fn open(path: &Path, width: i32, height: i32) -> io::Result<Shared> {
        let len = len_of(width, height)?;
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        if file.metadata()?.len() < len as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a buffer shorter than its size",
            ));
        }
        Shared::map(&file, len, width, height)
    }

    fn map(file: &File, len: usize, width: i32, height: i32) -> io::Result<Shared> {
        // SAFETY: a fresh shared mapping of a file at least `len` long.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let pixels = NonNull::new(address.cast()).ok_or_else(io::Error::last_os_error)?;
        Ok(Shared {
            pixels,
            len,
            width,
            height,
        })
    }

    pub(crate) fn pitch(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL
    }

    pub(crate) fn pointer(&mut self) -> *mut u8 {
        self.pixels.as_ptr()
    }

    /// The pixels as they are now. The other side may be writing them: what
    /// is read is bytes, and nothing here depends on which.
    pub(crate) fn pixels(&self) -> &[u8] {
        // SAFETY: `len` mapped bytes, for as long as `self`.
        unsafe { std::slice::from_raw_parts(self.pixels.as_ptr(), self.len) }
    }

    pub(crate) fn pixels_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `pixels`.
        unsafe { std::slice::from_raw_parts_mut(self.pixels.as_ptr(), self.len) }
    }

    /// Copy `from`'s top-left into this one's, as far as both reach.
    pub(crate) fn keep(&mut self, from: &Shared) {
        let bytes = self.pitch().min(from.pitch());
        let (to_pitch, from_pitch) = (self.pitch(), from.pitch());
        for row in 0..self.height.min(from.height) as usize {
            let source = &from.pixels()[row * from_pitch..row * from_pitch + bytes];
            self.pixels_mut()[row * to_pitch..row * to_pitch + bytes].copy_from_slice(source);
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: the mapping made in `map`, unmapped once.
        unsafe { libc::munmap(self.pixels.as_ptr().cast(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rosemary-shared-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn what_one_mapping_writes_the_other_reads_after_the_file_is_gone() {
        let directory = scratch("both");
        let (mut ours, name) = Shared::create(&directory, 3, 2).unwrap();
        let theirs = Shared::open(&directory.join(&name), 3, 2).unwrap();
        std::fs::remove_file(directory.join(&name)).unwrap();
        ours.pixels_mut()[5] = 0x7f;
        assert_eq!(theirs.pixels()[5], 0x7f);
        assert_eq!(theirs.pixels().len(), 3 * 2 * BYTES_PER_PIXEL);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_buffer_opened_as_larger_than_its_file_is_refused() {
        let directory = scratch("short");
        let (_ours, name) = Shared::create(&directory, 2, 2).unwrap();
        assert!(Shared::open(&directory.join(&name), 4, 4).is_err());
        assert!(Shared::open(&directory.join(&name), 0, 4).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_resized_buffer_keeps_its_top_left() {
        let directory = scratch("keep");
        let (mut old, _) = Shared::create(&directory, 2, 2).unwrap();
        old.pixels_mut().fill(0xaa);
        let (mut new, _) = Shared::create(&directory, 3, 1).unwrap();
        new.keep(&old);
        assert_eq!(new.pixels()[..8], [0xaa; 8]);
        assert_eq!(new.pixels()[8..], [0; 4]);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
