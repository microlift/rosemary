//! Out-of-line memory, and how it crosses between tasks.
//!
//! Mach maps a sender's pages into the receiver's address space. Two host
//! processes share no address space, so a message's out-of-line regions are
//! gathered into one anonymous memory object that travels beside the message as
//! a descriptor, and the receiver copies each region into fresh pages of its own.

use std::os::fd::{AsRawFd, OwnedFd};

use super::kern::*;
use super::local;

/// A shared mapping of a memory object, unmapped when dropped.
struct Mapping {
    base: *mut u8,
    len: usize,
}

impl Mapping {
    fn new(object: &OwnedFd, len: u64, prot: libc::c_int) -> Option<Mapping> {
        let len = usize::try_from(len).ok()?;
        // SAFETY: a new mapping at no fixed address cannot displace anything.
        let base = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                len,
                prot,
                libc::MAP_SHARED,
                object.as_raw_fd(),
                0,
            )
        };
        (base != libc::MAP_FAILED).then_some(Mapping {
            base: base.cast(),
            len,
        })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: `len` mapped, readable bytes, alive as long as `self`.
        unsafe { core::slice::from_raw_parts(self.base, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping this value made and nothing else refers to.
        unsafe { libc::munmap(self.base.cast(), self.len) };
    }
}

/// The regions, as `(address, size)`, back to back in one new memory object:
/// `None` when there is nothing to carry.
///
/// A region this task cannot read is `MACH_SEND_INVALID_MEMORY`, as Mach's own
/// copy-in answers it.
pub fn gather(regions: &[(u64, u64)]) -> Result<Option<OwnedFd>, i32> {
    let total = regions
        .iter()
        .try_fold(0u64, |total, (_, size)| total.checked_add(*size))
        .ok_or(MACH_SEND_TOO_LARGE)?;
    if total == 0 {
        return Ok(None);
    }
    let object = starnix::memory::object(total).map_err(|_| MACH_SEND_NO_BUFFER)?;
    let mapping = Mapping::new(&object, total, libc::PROT_READ | libc::PROT_WRITE)
        .ok_or(MACH_SEND_NO_BUFFER)?;
    let mut at = 0usize;
    for &(address, size) in regions {
        let size = size as usize;
        // SAFETY: `[at, at + size)` lies within the mapping, whose length is
        // the sum of the sizes, and this is the only view of it.
        let into = unsafe { core::slice::from_raw_parts_mut(mapping.base.add(at), size) };
        local::read(address, into).map_err(|_| MACH_SEND_INVALID_MEMORY)?;
        at += size;
    }
    Ok(Some(object))
}

/// The out-of-line memory that arrived with a message.
pub struct Arrived(Mapping);

impl Arrived {
    /// Map `object`, which its sender said holds `len` bytes; `None` when it
    /// does not, since reading past its end would fault rather than fail.
    pub fn map(object: OwnedFd, len: u64) -> Option<Arrived> {
        // SAFETY: `stat` is plain data the call fills in.
        let mut stat: libc::stat = unsafe { core::mem::zeroed() };
        // SAFETY: an open descriptor and a live `stat` to fill.
        if unsafe { libc::fstat(object.as_raw_fd(), &mut stat) } != 0
            || u64::try_from(stat.st_size).ok()? < len
        {
            return None;
        }
        Mapping::new(&object, len, libc::PROT_READ).map(Arrived)
    }

    /// `size` bytes from `at`, which the caller has checked lie within.
    pub fn bytes(&self, at: u64, size: u64) -> &[u8] {
        &self.0.bytes()[at as usize..(at + size) as usize]
    }
}

/// Fresh pages of this task holding `bytes`, which is where a receiver finds
/// out-of-line data and what it later hands to `vm_deallocate`. Zero for no
/// bytes, as Mach gives an empty region no address.
pub fn place(bytes: &[u8]) -> Result<u64, i32> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let address = local::allocate(0, bytes.len() as u64, local::VM_FLAGS_ANYWHERE)?;
    // SAFETY: fresh, writable pages at least `bytes.len()` long, which nothing
    // else knows of yet.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len()) };
    Ok(address)
}
