use alloc::vec::Vec;
use core::ffi::c_void;
use core::ptr;

use crate::error::{Error, Result};
use crate::memory::{AddressSpace, Placement, Prot, host_page_size};
use crate::raw::Word;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sys_icache_invalidate(start: *mut c_void, len: usize);
}

struct Region {
    base: *mut u8,
    len: usize,
}

impl Region {
    fn contains(&self, addr: usize, len: usize) -> Option<*mut u8> {
        let off = addr.checked_sub(self.base as usize)?;
        if off.checked_add(len)? > self.len {
            return None;
        }
        // SAFETY: `off + len` was just checked against this region's length.
        Some(unsafe { self.base.add(off) })
    }

    /// The region's bytes.
    ///
    /// The slice aliases memory the guest may be executing against - see
    /// [`Regions`](crate::memory::Regions) for why that is inherent to an
    /// identity-mapped space rather than something this could guard.
    fn as_slice(&self) -> &[u8] {
        // SAFETY: `base..base+len` is a live mapping this space owns and keeps
        // readable for its whole lifetime; the borrow is tied to `self`.
        unsafe { core::slice::from_raw_parts(self.base as *const u8, self.len) }
    }
}

/// `mmap`-backed host memory owning its mappings; all unmapped on drop.
///
/// Holds one region per [`reserve`](AddressSpace::reserve), because a link maps
/// one region per image and a program with a bundled dylib has more than one.
pub struct MmapSpace {
    regions: Vec<Region>,
    /// Whether the caller asked for host-executable memory.
    want_exec: bool,
    /// Whether the live mappings are `MAP_JIT` (implies `want_exec` on macOS).
    jit: bool,
    /// Whether [`map_file`](AddressSpace::map_file) maps rather than declines.
    file_backed: bool,
}

impl MmapSpace {
    pub fn new() -> Self {
        MmapSpace {
            regions: Vec::new(),
            want_exec: false,
            jit: false,
            file_backed: false,
        }
    }

    /// Request memory the host CPU may execute.
    ///
    /// Only meaningful when loading an image of the *host's* architecture. A
    /// guest x86-64 image must leave this off: the pages would gain `PROT_EXEC`
    /// that nothing can use, and on macOS the `MAP_JIT` mapping it implies
    /// makes per-segment protection unenforceable.
    pub fn executable(mut self, yes: bool) -> Self {
        self.want_exec = yes;
        self
    }

    /// Map image segments from their files instead of copying them in, so
    /// the page cache shares them across processes and `/proc/<pid>/maps`
    /// names them.
    ///
    /// Off by default: a recompiled guest's image is only ever data to the
    /// recompiler, and it keeps the anonymous mappings it was written against.
    pub fn file_backed(mut self, yes: bool) -> Self {
        self.file_backed = yes;
        self
    }

    /// Replace `[dst, dst+len)` with fresh anonymous memory, as `reserve` left it.
    fn remap_anonymous(&self, dst: *mut u8, len: usize) -> Result<()> {
        let (prot, flags) = self.mmap_flags();
        // SAFETY: `dst..dst+len` lies inside a region this space owns, so
        // MAP_FIXED replaces only our own pages.
        let raw = unsafe {
            libc::mmap(
                dst as *mut c_void,
                len,
                prot,
                flags | libc::MAP_FIXED,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(Error::Memory("mmap failed"));
        }
        Ok(())
    }

    fn in_bounds(&self, addr: Word, len: usize) -> Result<*mut u8> {
        let a = usize::try_from(addr).map_err(|_| Error::OutOfBounds { addr, len })?;
        self.regions
            .iter()
            .find_map(|r| r.contains(a, len))
            .ok_or(Error::OutOfBounds { addr, len })
    }

    /// Total bytes mapped across every region.
    pub fn len(&self) -> usize {
        self.regions.iter().map(|r| r.len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// How many regions have been reserved.
    pub fn region_count(&self) -> usize {
        self.regions.len()
    }

    /// Whether `[base, base+len)` overlaps a region already mapped *by us*.
    ///
    /// Only our own regions: the kernel is the authority on everything else,
    /// and `try_reserve_at` already defers to it by checking where the
    /// non-`MAP_FIXED` hint actually landed.
    fn collides(&self, base: usize, len: usize) -> bool {
        self.regions
            .iter()
            .any(|r| base < r.base as usize + r.len && (r.base as usize) < base + len)
    }

    fn mmap_flags(&self) -> (libc::c_int, libc::c_int) {
        if self.jit {
            (
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANON | map_jit(),
            )
        } else {
            (
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
            )
        }
    }

    /// Map `size` bytes wherever the kernel likes, trimming to `align`.
    /// Mirrors `selfish/src/memory/host_mmap.rs:50-91`.
    fn reserve_anywhere(&mut self, size: usize, align: usize) -> Result<Word> {
        let align = align.max(1).next_power_of_two();
        let over = size
            .checked_add(align)
            .ok_or(Error::Memory("reserve overflow"))?;
        let (prot, flags) = self.mmap_flags();

        // SAFETY: standard anonymous mmap; the result is checked against MAP_FAILED.
        let raw = unsafe { libc::mmap(ptr::null_mut(), over, prot, flags, -1, 0) };
        if raw == libc::MAP_FAILED {
            return Err(Error::Memory("mmap failed"));
        }
        let raw = raw as usize;
        let aligned = (raw + (align - 1)) & !(align - 1);

        let head = aligned - raw;
        if head > 0 {
            // SAFETY: unmapping only the leading slack we just mapped ourselves.
            unsafe { libc::munmap(raw as *mut c_void, head) };
        }
        let tail = over - head - size;
        if tail > 0 {
            // SAFETY: unmapping only the trailing slack we just mapped ourselves.
            unsafe { libc::munmap((aligned + size) as *mut c_void, tail) };
        }

        self.regions.push(Region {
            base: aligned as *mut u8,
            len: size,
        });
        Ok(aligned as Word)
    }

    /// Try to map `size` bytes exactly at `want`, without `MAP_FIXED`.
    ///
    /// Returns `Ok(None)` if the kernel placed us elsewhere, having already
    /// released the mapping it gave us - the caller decides whether that is a
    /// slide to accept or an error. With `exact`, the kernel is asked for
    /// `want` or nothing where it can be: Linux moves a bare hint below the
    /// lowest address it hands out on its own (0x8000 on arm64), though it
    /// maps lower when asked outright, as Wine's loader needs from 0x1000.
    fn try_reserve_at(&mut self, size: usize, want: Word, exact: bool) -> Result<Option<Word>> {
        let hint = usize::try_from(want).map_err(|_| Error::Memory("placement too high"))?;
        let (prot, mut flags) = self.mmap_flags();
        if exact {
            flags |= map_exact();
        }

        // SAFETY: anonymous mmap with an *advisory* hint, or one that fails
        // rather than replace a mapping - never MAP_FIXED, so nothing already
        // at `hint` is unmapped. The result is checked against MAP_FAILED.
        let raw = unsafe { libc::mmap(hint as *mut c_void, size, prot, flags, -1, 0) };
        if raw == libc::MAP_FAILED {
            if exact && std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
                return Ok(None);
            }
            return Err(Error::Memory("mmap failed"));
        }
        if raw as usize != hint {
            // SAFETY: unmapping exactly the region the kernel just handed us.
            unsafe { libc::munmap(raw, size) };
            return Ok(None);
        }

        self.regions.push(Region {
            base: raw as *mut u8,
            len: size,
        });
        Ok(Some(want))
    }
}

impl crate::memory::Regions for MmapSpace {
    fn region_at(&self, addr: Word) -> Option<(Word, &[u8])> {
        let a = usize::try_from(addr).ok()?;
        let r = self
            .regions
            .iter()
            .find(|r| a >= r.base as usize && a < r.base as usize + r.len)?;
        Some((r.base as Word, r.as_slice()))
    }

    fn regions(&self) -> Vec<(Word, &[u8])> {
        self.regions
            .iter()
            .map(|r| (r.base as Word, r.as_slice()))
            .collect()
    }
}

impl AddressSpace for MmapSpace {
    fn reserve(&mut self, size: usize, align: usize, placement: Placement) -> Result<Word> {
        if size == 0 {
            return Err(Error::Memory("zero-length reservation"));
        }
        // MAP_JIT from the start, because a page that was ever writable cannot
        // later become executable under the hardened runtime. Without it the
        // transition does work - measured on Apple Silicon, an anonymous RW
        // mapping takes `mprotect(PROT_READ|PROT_EXEC)` and runs - which is why
        // a loaded program, whose data segments it must keep writable, asks
        // for a plain space instead and protects per segment.
        self.jit = self.want_exec && cfg!(target_os = "macos");

        // Guard our own regions before asking the kernel. Without this a second
        // image preferring the same address as the first would get the hint
        // honoured - the range is genuinely mapped, just by us - and the two
        // images would be written over each other.
        if let Some(want) = placement.wanted()
            && let Ok(want) = usize::try_from(want)
            && self.collides(want, size)
        {
            return match placement {
                Placement::Fixed(_) => Err(Error::Memory("fixed placement already reserved")),
                _ => self.reserve_anywhere(size, align),
            };
        }

        match placement {
            Placement::Anywhere => self.reserve_anywhere(size, align),
            Placement::Preferred(want) => match self.try_reserve_at(size, want, false)? {
                Some(base) => Ok(base),
                None => self.reserve_anywhere(size, align),
            },
            Placement::Fixed(want) => self
                .try_reserve_at(size, want, true)?
                .ok_or(Error::Memory("fixed placement unavailable")),
        }
    }

    fn write(&mut self, addr: Word, data: &[u8]) -> Result<()> {
        let dst = self.in_bounds(addr, data.len())?;
        if self.jit {
            // SAFETY: toggling this thread's JIT write protection off, storing
            // within the owned mapping, re-enabling it, then flushing the
            // instruction cache for exactly the range written.
            unsafe {
                jit_write_protect(false);
                ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
                jit_write_protect(true);
                icache_flush(dst as *mut c_void, data.len());
            }
        } else {
            // SAFETY: `dst` is within the owned mapping, which is currently writable.
            unsafe { ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len()) };
        }
        Ok(())
    }

    fn map_file(&mut self, addr: Word, path: &str, offset: u64, data: &[u8]) -> Result<usize> {
        use std::os::fd::AsRawFd;

        // macOS checks executable file pages against a code signature only
        // dyld registers, and MAP_JIT memory cannot be file-backed at all.
        if !self.file_backed || self.jit || cfg!(target_os = "macos") {
            return Ok(0);
        }
        // Whole pages only, so a mapping never reaches into a page another
        // segment or the zero-fill shares; the caller copies the remainder.
        let page = host_page_size();
        let len = data.len() & !(page as usize - 1);
        let Ok(off) = libc::off_t::try_from(offset) else {
            return Ok(0);
        };
        if len == 0 || !addr.is_multiple_of(page) || !offset.is_multiple_of(page) {
            return Ok(0);
        }
        let dst = self.in_bounds(addr, len)?;
        let Ok(file) = std::fs::File::open(path) else {
            return Ok(0);
        };
        // Past the end of the file a mapping raises SIGBUS instead of reading.
        match file.metadata() {
            Ok(m) if m.is_file() && m.len() >= offset.saturating_add(len as u64) => {}
            _ => return Ok(0),
        }

        let (prot, _) = self.mmap_flags();
        // SAFETY: MAP_FIXED over a range inside a region this space owns,
        // checked by `in_bounds`; private, so writes never reach the file.
        let raw = unsafe {
            libc::mmap(
                dst as *mut c_void,
                len,
                prot,
                libc::MAP_PRIVATE | libc::MAP_FIXED,
                file.as_raw_fd(),
                off,
            )
        };
        if raw == libc::MAP_FAILED {
            // A failed MAP_FIXED may already have unmapped the range.
            self.remap_anonymous(dst, len)?;
            return Ok(0);
        }
        // SAFETY: `dst..dst+len` was just mapped readable.
        let mapped = unsafe { core::slice::from_raw_parts(dst as *const u8, len) };
        if mapped != &data[..len] {
            self.remap_anonymous(dst, len)?;
            return Ok(0);
        }
        Ok(len)
    }

    fn read(&self, addr: Word, out: &mut [u8]) -> Result<()> {
        let src = self.in_bounds(addr, out.len())?;
        // SAFETY: `src` is within the owned mapping, which is readable.
        unsafe { ptr::copy_nonoverlapping(src, out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    fn protect(&mut self, addr: Word, len: usize, prot: Prot) -> Result<()> {
        // Under MAP_JIT the region is RWX at the hardware level with W^X
        // enforced dynamically by the write-protect toggle, so per-range
        // mprotect is both unnecessary and counterproductive. This is the
        // behaviour `selfish` has unconditionally on macOS; here it applies
        // only to the opt-in executable mode.
        if self.jit {
            return Ok(());
        }
        if len == 0 {
            return Ok(());
        }
        let ptr = self.in_bounds(addr, len)? as *mut c_void;
        let mut flags = 0;
        if prot.read {
            flags |= libc::PROT_READ;
        }
        if prot.write {
            flags |= libc::PROT_WRITE;
        }
        if prot.exec {
            flags |= libc::PROT_EXEC;
        }
        // SAFETY: protecting a sub-range of the owned mapping, bounds-checked above.
        let rc = unsafe { libc::mprotect(ptr, len, flags) };
        if rc != 0 {
            return Err(Error::Memory("mprotect failed"));
        }
        Ok(())
    }

    fn protect_granularity(&self) -> u64 {
        host_page_size()
    }

    fn host_ptr(&self, addr: Word) -> Option<*mut u8> {
        self.in_bounds(addr, 1).ok()
    }
}

impl Default for MmapSpace {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MmapSpace {
    fn drop(&mut self) {
        for r in &self.regions {
            if !r.base.is_null() && r.len != 0 {
                // SAFETY: unmapping exactly a region we reserved and still own.
                unsafe { libc::munmap(r.base as *mut c_void, r.len) };
            }
        }
    }
}

/// The flag that makes an mmap hint exact without replacing what is there.
/// Kernels too old to know it treat it as a plain hint, which the caller's
/// address check still catches.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn map_exact() -> libc::c_int {
    libc::MAP_FIXED_NOREPLACE
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn map_exact() -> libc::c_int {
    0
}

#[cfg(target_os = "macos")]
fn map_jit() -> libc::c_int {
    libc::MAP_JIT
}

#[cfg(not(target_os = "macos"))]
fn map_jit() -> libc::c_int {
    0
}

#[cfg(target_os = "macos")]
unsafe fn jit_write_protect(enable: bool) {
    // SAFETY: FFI to a thread-local toggle with no memory effects of its own.
    unsafe { libc::pthread_jit_write_protect_np(enable as libc::c_int) };
}

#[cfg(target_os = "macos")]
unsafe fn icache_flush(p: *mut c_void, len: usize) {
    // SAFETY: flushing the instruction cache over a range we just wrote.
    unsafe { sys_icache_invalidate(p, len) };
}

#[cfg(not(target_os = "macos"))]
unsafe fn jit_write_protect(_enable: bool) {}

#[cfg(not(target_os = "macos"))]
unsafe fn icache_flush(_p: *mut c_void, _len: usize) {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;
    use alloc::{format, vec};

    #[test]
    fn reserve_anywhere_is_zeroed_and_aligned() {
        let mut mem = MmapSpace::new();
        let base = mem.reserve(0x8000, 0x4000, Placement::Anywhere).unwrap();
        assert_eq!(base % 0x4000, 0);

        let mut out = [0xffu8; 16];
        mem.read(base, &mut out).unwrap();
        assert_eq!(out, [0u8; 16]);
    }

    #[test]
    fn write_read_round_trip() {
        let mut mem = MmapSpace::new();
        let base = mem.reserve(0x4000, 0x1000, Placement::Anywhere).unwrap();
        mem.write(base + 0x100, b"machish").unwrap();
        let mut out = [0u8; 7];
        mem.read(base + 0x100, &mut out).unwrap();
        assert_eq!(&out, b"machish");
    }

    #[test]
    fn bounds_are_enforced() {
        let mut mem = MmapSpace::new();
        let base = mem.reserve(0x4000, 0x1000, Placement::Anywhere).unwrap();
        assert!(mem.write(base - 1, &[0]).is_err());
        assert!(mem.write(base + 0x4000, &[0]).is_err());
        assert!(mem.read(base + 0x3fff, &mut [0u8; 2]).is_err());
        assert!(mem.host_ptr(base + 0x4000).is_none());
        assert!(mem.host_ptr(base).is_some());
    }

    #[test]
    fn preferred_placement_falls_back_without_clobbering() {
        // Reserve a region, then ask a second space to land on top of it. The
        // request must be denied and relocated, and the first space must be
        // completely intact afterward - that is the MAP_FIXED hazard.
        let mut first = MmapSpace::new();
        let base = first.reserve(0x4000, 0x1000, Placement::Anywhere).unwrap();
        first.write(base, b"sentinel").unwrap();

        let mut second = MmapSpace::new();
        let got = second
            .reserve(0x4000, 0x1000, Placement::Preferred(base))
            .unwrap();
        assert_ne!(got, base, "the occupied range must not have been taken");

        let mut out = [0u8; 8];
        first.read(base, &mut out).unwrap();
        assert_eq!(&out, b"sentinel", "the existing mapping was clobbered");
    }

    #[test]
    fn fixed_placement_fails_rather_than_clobbering() {
        let mut first = MmapSpace::new();
        let base = first.reserve(0x4000, 0x1000, Placement::Anywhere).unwrap();
        first.write(base, b"sentinel").unwrap();

        let mut second = MmapSpace::new();
        assert!(
            second
                .reserve(0x4000, 0x1000, Placement::Fixed(base))
                .is_err()
        );

        let mut out = [0u8; 8];
        first.read(base, &mut out).unwrap();
        assert_eq!(&out, b"sentinel");
    }

    #[test]
    fn fixed_placement_when_it_succeeds_is_exact() {
        // 0x1_0000_0000 is where an x86-64 Mach-O executable wants to land, and
        // in a normal host process that range is free. Whether it *is* free is
        // not ours to guarantee, so the assertion is conditional - but the
        // guarantee that matters is unconditional: `Fixed` either lands on the
        // exact address or fails. It must never quietly slide, because a
        // non-PIE image bound to a slid base would be silently wrong.
        let mut mem = MmapSpace::new();
        match mem.reserve(0x8000, 0x4000, Placement::Fixed(0x1_0000_0000)) {
            Ok(base) => {
                assert_eq!(base, 0x1_0000_0000);
                mem.write(base, b"guest").unwrap();
            }
            Err(e) => assert_eq!(e, Error::Memory("fixed placement unavailable")),
        }
    }

    #[test]
    fn guest_mapping_is_never_executable() {
        // The §4.4 invariant: a default (guest) space never asks for PROT_EXEC.
        let mut mem = MmapSpace::new();
        let base = mem.reserve(0x4000, 0x1000, Placement::Anywhere).unwrap();
        assert!(!mem.jit, "guest mappings must not be MAP_JIT");
        // Protecting read-execute is still a legal request the caller may make;
        // the loader is what declines to make it for a non-native spec.
        mem.protect(base, 0x1000, Prot::RO).unwrap();
    }

    #[test]
    fn several_reservations_coexist_without_clobbering_each_other() {
        // The multi-image case. Two reservations must both stay live and stay
        // independently addressable - a second `reserve` replacing the first
        // would leak one mapping and make every address in it invalid.
        let mut mem = MmapSpace::new();
        let a = mem.reserve(0x8000, 0x4000, Placement::Anywhere).unwrap();
        let b = mem.reserve(0x8000, 0x4000, Placement::Anywhere).unwrap();
        assert_eq!(mem.region_count(), 2);
        assert_ne!(a, b);

        mem.write(a, &[0xaa; 8]).unwrap();
        mem.write(b, &[0xbb; 8]).unwrap();
        let mut buf = [0u8; 8];
        mem.read(a, &mut buf).unwrap();
        assert_eq!(buf, [0xaa; 8]);
        mem.read(b, &mut buf).unwrap();
        assert_eq!(buf, [0xbb; 8]);
    }

    #[test]
    fn a_second_image_preferring_an_address_we_already_hold_slides() {
        // Without the self-collision check this is the dangerous case: the
        // kernel would honour the hint, because the range *is* mapped - by us -
        // and the second image would be written over the first.
        let mut mem = MmapSpace::new();
        let a = mem.reserve(0x8000, 0x4000, Placement::Anywhere).unwrap();
        let b = mem
            .reserve(0x4000, 0x4000, Placement::Preferred(a))
            .unwrap();
        assert_ne!(b, a, "a colliding preferred placement must slide");

        mem.write(a, &[0x11; 8]).unwrap();
        let mut buf = [0u8; 8];
        mem.read(b, &mut buf).unwrap();
        assert_eq!(buf, [0u8; 8], "the second reservation overlapped the first");
    }

    /// A file of `pages` host pages plus a partial one, each byte distinct
    /// from its neighbours so a misplaced page cannot compare equal.
    fn patterned_file(name: &str, pages: usize) -> (std::path::PathBuf, Vec<u8>) {
        let page = host_page_size() as usize;
        let data: Vec<u8> = (0..pages * page + 100).map(|i| (i % 251) as u8).collect();
        let path = std::env::temp_dir().join(format!("machish-{name}-{}", std::process::id()));
        std::fs::write(&path, &data).unwrap();
        (path, data)
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn maps_whole_pages_of_a_matching_file() {
        let page = host_page_size() as usize;
        let (path, data) = patterned_file("match", 3);
        let mut mem = MmapSpace::new().file_backed(true);
        let base = mem.reserve(4 * page, page, Placement::Anywhere).unwrap();

        let mapped = mem
            .map_file(base, path.to_str().unwrap(), 0, &data)
            .unwrap();
        assert_eq!(mapped, 3 * page, "only whole pages are mapped");
        mem.write(base + mapped as Word, &data[mapped..]).unwrap();

        let mut out = vec![0u8; data.len()];
        mem.read(base, &mut out).unwrap();
        assert_eq!(out, data);

        // Private: a fixup-style write must not reach the file.
        mem.write(base, &[0xee; 8]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), data);

        #[cfg(target_os = "linux")]
        {
            let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
            let name = path.to_str().unwrap();
            assert!(
                maps.lines().any(|l| l.ends_with(name)),
                "the mapping should name its file"
            );
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn declines_bytes_the_file_does_not_hold() {
        let page = host_page_size() as usize;
        let (path, mut data) = patterned_file("mismatch", 2);
        data[page + 1] ^= 0xff;
        let mut mem = MmapSpace::new().file_backed(true);
        let base = mem.reserve(4 * page, page, Placement::Anywhere).unwrap();

        let mapped = mem
            .map_file(base, path.to_str().unwrap(), 0, &data)
            .unwrap();
        assert_eq!(mapped, 0);
        // The file's pages must not have been left behind.
        let mut out = vec![0xffu8; 2 * page];
        mem.read(base, &mut out).unwrap();
        assert!(out.iter().all(|&b| b == 0));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn declines_unless_asked_or_misaligned() {
        let page = host_page_size() as usize;
        let (path, data) = patterned_file("decline", 2);
        let path = path.to_str().unwrap().to_owned();

        let mut plain = MmapSpace::new();
        let base = plain.reserve(4 * page, page, Placement::Anywhere).unwrap();
        assert_eq!(plain.map_file(base, &path, 0, &data).unwrap(), 0);

        let mut mem = MmapSpace::new().file_backed(true);
        let base = mem.reserve(4 * page, page, Placement::Anywhere).unwrap();
        assert_eq!(mem.map_file(base + 1, &path, 0, &data[1..]).unwrap(), 0);
        assert_eq!(mem.map_file(base, &path, 1, &data[1..]).unwrap(), 0);
        assert_eq!(
            mem.map_file(base, "/nonexistent/machish", 0, &data)
                .unwrap(),
            0
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_fixed_placement_onto_our_own_region_is_refused() {
        let mut mem = MmapSpace::new();
        let a = mem.reserve(0x8000, 0x4000, Placement::Anywhere).unwrap();
        assert!(mem.reserve(0x4000, 0x4000, Placement::Fixed(a)).is_err());
    }
}
