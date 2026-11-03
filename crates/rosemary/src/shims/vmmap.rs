use microlift::arch::x86_64::context::RegisterContext;
use static_init::dynamic;

use crate::abi::{GuestMemory, Result};

use super::set_int_return;
use crate::shims::bind;

/// `MAP_JIT`. Not in the `libc` crate's Darwin bindings, and it is the flag this
/// module exists to remove, so it is spelled out with the value from `mman.h`.
const MAP_JIT: i32 = darwin::vmmap::darwin::MAP_JIT;

/// The ranges the guest asked to be executable.
mod arenas {
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

    /// A published range, or a claimed-but-unfilled slot when `end` is zero.
    struct Slot {
        base: AtomicU64,
        end: AtomicU64,
    }

    impl Slot {
        fn empty() -> Self {
            Self {
                base: AtomicU64::new(0),
                end: AtomicU64::new(0),
            }
        }
    }

    const CHUNK: usize = 512;
    type Chunk = [Slot; CHUNK];

    /// 512 chunks of 512, which a guest that emits one arena per JIT method would
    /// still not reach.
    static DIRECTORY: [AtomicPtr<Chunk>; 512] = [const { AtomicPtr::new(null_mut()) }; 512];

    /// Slots claimed. Not slots *filled*: see [`Slot`].
    static COUNT: AtomicUsize = AtomicUsize::new(0);

    /// One past the index that answered for a page, or zero: a direct-mapped cache
    /// that turns the scan into two loads for an address the guest is executing.
    ///
    /// Keyed by page rather than kept as a single "last hit" so that two threads in
    /// two arenas do not write the same line back and forth; a slot settles on one
    /// value and is then only read.
    static RECENT: [AtomicUsize; 64] = [const { AtomicUsize::new(0) }; 64];

    fn recent(va: u64) -> &'static AtomicUsize {
        &RECENT[(va >> 12) as usize & (RECENT.len() - 1)]
    }

    /// The lowest and highest arena address, for the question asked on the hot path.
    static LO: AtomicU64 = AtomicU64::new(u64::MAX);
    static HI: AtomicU64 = AtomicU64::new(0);

    fn chunk(i: usize) -> Option<&'static Chunk> {
        let p = DIRECTORY.get(i)?.load(Ordering::Acquire);
        // SAFETY: a chunk is installed once, by `claim`, from a leaked `Box`.
        (!p.is_null()).then(|| unsafe { &*p })
    }

    /// Slot `i`, if it has been filled.
    fn get(i: usize) -> Option<(u64, u64)> {
        let slot = &chunk(i / CHUNK)?[i % CHUNK];
        // Pairs with the release in `push`: `base` is written first.
        match slot.end.load(Ordering::Acquire) {
            0 => None,
            end => Some((slot.base.load(Ordering::Relaxed), end)),
        }
    }

    /// Slot `i`, allocating its chunk if this is the first claim in it.
    fn claim(i: usize) -> Option<&'static Slot> {
        let cell = DIRECTORY.get(i / CHUNK)?;
        let mut p = cell.load(Ordering::Acquire);
        if p.is_null() {
            let fresh = Box::into_raw(Box::new(std::array::from_fn(|_| Slot::empty())));
            p = match cell.compare_exchange(null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => fresh,
                // SAFETY: the losing allocation was never published.
                Err(won) => {
                    drop(unsafe { Box::from_raw(fresh) });
                    won
                }
            };
        }
        // SAFETY: as `chunk`.
        Some(&unsafe { &*p }[i % CHUNK])
    }

    /// Record `[base, base + len)` as memory the guest means to execute.
    pub fn push(base: u64, len: u64) {
        let Some(end) = base.checked_add(len) else {
            return;
        };
        if base == 0 || len == 0 || find(base).is_some_and(|(_, e)| end <= e) {
            return;
        }
        // Before the entry is published, so a reader that sees the entry has
        // already seen a range wide enough to admit it.
        LO.fetch_min(base, Ordering::Relaxed);
        HI.fetch_max(end, Ordering::Relaxed);

        let i = COUNT.fetch_add(1, Ordering::Relaxed);
        let Some(slot) = claim(i) else {
            eprintln!("rosemary: out of arena slots, {base:#x}+{len:#x} not recorded");
            return;
        };
        slot.base.store(base, Ordering::Relaxed);
        slot.end.store(end, Ordering::Release);
    }

    /// The arena containing `va`, as `(base, end)`.
    pub fn find(va: u64) -> Option<(u64, u64)> {
        if va < LO.load(Ordering::Relaxed) || va >= HI.load(Ordering::Relaxed) {
            return None;
        }
        let cached = recent(va);
        let hint = cached.load(Ordering::Relaxed);
        if hint != 0
            && let Some((base, end)) = get(hint - 1)
            && (base..end).contains(&va)
        {
            return Some((base, end));
        }
        for i in 0..COUNT.load(Ordering::Relaxed) {
            if let Some((base, end)) = get(i)
                && (base..end).contains(&va)
            {
                if hint != i + 1 {
                    cached.store(i + 1, Ordering::Relaxed);
                }
                return Some((base, end));
            }
        }
        None
    }
}

/// The arena containing `va`, if any.
pub fn arena_at(va: u64) -> Option<(u64, u64)> {
    arenas::find(va).map(|(base, end)| (base, end - base))
}

/// Whether `va` is in memory the guest declared executable.
pub fn contains(va: u64) -> bool {
    arenas::find(va).is_some()
}

/// The shim for `name`, if this module has one.
pub fn shim_for(name: &str) -> Option<super::Shim> {
    match name {
        "mmap" => Some(bind!("mmap" => mmap)),
        "mprotect" => Some(bind!("mprotect" => mprotect)),
        _ => None,
    }
}

/// The protection to actually request for a guest's `prot`.
pub(crate) fn host_prot(prot: i32) -> i32 {
    match prot & libc::PROT_EXEC {
        0 => prot,
        _ => (prot & !libc::PROT_EXEC) | libc::PROT_READ,
    }
}

/// The flags to actually request for a guest's `flags`.
///
/// `MAP_JIT` is dropped here rather than in [`darwin::vmmap::map_flags`] because
/// it is this module's business and not the platform's: the guest is saying it
/// means to write code into the mapping, which is what [`arenas`] records. The
/// rest of the translation is the platform's, and off Darwin it is not a no-op -
/// `MAP_ANON` alone is a different bit on either side.
pub(crate) fn host_flags(flags: i32) -> i32 {
    darwin::vmmap::map_flags(flags & !MAP_JIT)
}

/// `void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t off)`
fn mmap(_symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let (addr, len) = (ctx.rdi as usize, ctx.rsi as usize);
    let (prot, flags) = (ctx.rdx as i32, ctx.rcx as i32);
    let (fd, off) = (ctx.r8 as i32, ctx.r9 as i64);
    let (want_prot, want_flags) = (host_prot(prot), host_flags(flags));

    // SAFETY: the guest's own request, with executable permission removed. The
    // address and length are the guest's to choose; the kernel judges both.
    let p = unsafe {
        libc::mmap(
            addr as *mut libc::c_void,
            len,
            want_prot,
            want_flags,
            // An anonymous mapping's `fd` on Darwin is a VM tag rather than a
            // descriptor; see [`darwin::vmmap::map_fd`].
            darwin::vmmap::map_fd(flags, fd),
            off,
        )
    };

    // The guest said it means to execute these bytes; see [`ARENAS`].
    if prot & libc::PROT_EXEC != 0 && p != libc::MAP_FAILED {
        arenas::push(p as usize as u64, len as u64);
        crate::smc::track(p as usize as u64, len as u64, prot);
    }

    if tracing() {
        eprintln!(
            "rosemary: mmap({addr:#x}, {len:#x}, {}, {}, {fd}, {off:#x}) as ({}, {}) -> {}",
            prot_name(prot),
            flags_name(flags),
            prot_name(want_prot),
            flags_name(want_flags),
            outcome(p as isize)
        );
    }
    ctx.rax = p as u64;
    Ok(())
}

/// Change a guest range's protection as the guest asked, keeping the code
/// cache's books: an executable range is an arena, and whatever guard the cache
/// held on these pages was just replaced. Answers `mprotect`'s return value.
///
/// Async-signal-safe once [`crate::smc::warm`] has run, but for the rare
/// allocation of another chunk of arena slots: `darwin`'s service handler
/// calls it for `vm_protect` from another task.
fn protect(addr: usize, len: usize, prot: i32) -> i32 {
    let want_prot = host_prot(prot);

    // SAFETY: the guest's own request, with executable permission removed.
    let rc = unsafe { libc::mprotect(addr as *mut libc::c_void, len, want_prot) };

    if prot & libc::PROT_EXEC != 0 && rc == 0 {
        arenas::push(addr as u64, len as u64);
    }
    // Any protection, not only an executable one. See [`crate::smc`].
    if rc == 0 && contains(addr as u64) {
        crate::smc::track(addr as u64, len as u64, prot);
    }
    rc
}

/// [`protect`] as `darwin::mach::local`'s hook for `vm_protect`: an `errno`, or 0.
///
/// # Safety
///
/// The range is the guest's to change.
pub(crate) unsafe fn protect_hook(address: u64, len: u64, prot: i32) -> i32 {
    match protect(address as usize, len as usize, prot) {
        0 => 0,
        _ => std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EINVAL),
    }
}

/// `int mprotect(void *addr, size_t len, int prot)`
fn mprotect(_symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let (addr, len, prot) = (ctx.rdi as usize, ctx.rsi as usize, ctx.rdx as i32);
    let want_prot = host_prot(prot);
    let rc = protect(addr, len, prot);

    if tracing() {
        eprintln!(
            "rosemary: mprotect({addr:#x}, {len:#x}, {}) as {} -> {}",
            prot_name(prot),
            prot_name(want_prot),
            outcome(rc as isize)
        );
    }
    set_int_return(ctx, rc);
    Ok(())
}

/// Whether `ROSEMARY_TRACE_VM` asked for the guest's use of memory to be reported:
///
/// Read once and cached, because some of its callers sit on the path taken for *every*
/// generated region and a `getenv` each time would be a cost paid by runs that never
/// look at the output.
#[inline(always)]
pub fn tracing() -> bool {
    #[dynamic]
    static ON: bool = std::env::var_os("ROSEMARY_TRACE_VM").is_some();
    *ON
}

/// A return value, with `errno` when it is a failure.
fn outcome(rc: isize) -> String {
    if rc == 0 || (rc != -1 && rc as usize != usize::MAX) {
        return format!("{rc:#x}");
    }
    let e = darwin::errno::get();
    format!("failed, errno {e}")
}

fn prot_name(prot: i32) -> String {
    if prot == 0 {
        return "PROT_NONE".into();
    }
    let mut parts = Vec::new();
    for (bit, name) in [
        (libc::PROT_READ, "READ"),
        (libc::PROT_WRITE, "WRITE"),
        (libc::PROT_EXEC, "EXEC"),
    ] {
        if prot & bit != 0 {
            parts.push(name);
        }
    }
    parts.join("|")
}

fn flags_name(flags: i32) -> String {
    let mut parts = Vec::new();
    for (bit, name) in [
        (libc::MAP_SHARED, "SHARED"),
        (libc::MAP_PRIVATE, "PRIVATE"),
        (libc::MAP_FIXED, "FIXED"),
        (libc::MAP_ANON, "ANON"),
        (MAP_JIT, "JIT"),
    ] {
        if flags & bit != 0 {
            parts.push(name);
        }
    }
    format!("{}({flags:#x})", parts.join("|"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the lock-free table rests on: a slot, once published, is the
    /// answer forever, and concurrent readers never see a half-written one.
    #[test]
    fn arenas_are_found_from_every_thread_while_they_are_being_appended() {
        const N: u64 = 4096;
        // Well above any address a real arena has, so this test's entries cannot
        // be confused with another test's.
        const BASE: u64 = 0x7000_0000_0000;

        std::thread::scope(|s| {
            s.spawn(|| {
                for i in 0..N {
                    arenas::push(BASE + i * 0x2000, 0x1000);
                }
            });
            for _ in 0..4 {
                s.spawn(|| {
                    for i in 0..N {
                        // A published arena never stops being found, and the gap
                        // between two of them is never claimed by either.
                        let va = BASE + i * 0x2000;
                        if let Some((base, len)) = arena_at(va) {
                            assert_eq!((base, len), (va, 0x1000));
                            assert!(arena_at(va + 0x1000).is_none());
                        }
                    }
                });
            }
        });

        for i in 0..N {
            assert_eq!(
                arena_at(BASE + i * 0x2000),
                Some((BASE + i * 0x2000, 0x1000))
            );
            assert!(!contains(BASE + i * 0x2000 + 0x1000));
        }
    }

    /// A range already covered costs no slot.
    #[test]
    fn a_subrange_of_a_recorded_arena_is_not_recorded_again() {
        const BASE: u64 = 0x7100_0000_0000;
        arenas::push(BASE, 0x4000);
        arenas::push(BASE + 0x1000, 0x1000);
        assert_eq!(arena_at(BASE + 0x1000), Some((BASE, 0x4000)));
    }

    #[test]
    fn executable_permission_becomes_readable_permission() {
        // The case that produced write-only memory the lifter could not decode.
        assert_eq!(
            host_prot(libc::PROT_WRITE | libc::PROT_EXEC),
            libc::PROT_WRITE | libc::PROT_READ
        );
        assert_eq!(
            host_prot(libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC),
            libc::PROT_READ | libc::PROT_WRITE
        );
        assert_eq!(host_prot(libc::PROT_EXEC), libc::PROT_READ);
    }

    #[test]
    fn a_request_without_executable_permission_is_passed_through_unchanged() {
        for prot in [
            libc::PROT_NONE,
            libc::PROT_READ,
            libc::PROT_WRITE,
            libc::PROT_READ | libc::PROT_WRITE,
        ] {
            assert_eq!(host_prot(prot), prot);
        }
    }

    /// The guest's flags are Darwin's, whatever host this is compiled for; see
    /// [`darwin::vmmap`], which is what `host_flags` defers the rest to.
    use darwin::vmmap::darwin as guest;

    #[test]
    fn map_jit_is_removed_and_nothing_else_is() {
        let flags = guest::MAP_PRIVATE | guest::MAP_ANON | MAP_JIT;
        assert_eq!(
            host_flags(flags),
            darwin::vmmap::map_flags(guest::MAP_PRIVATE | guest::MAP_ANON),
        );
        let flags = guest::MAP_PRIVATE | guest::MAP_ANON | guest::MAP_FIXED;
        assert_eq!(host_flags(flags), darwin::vmmap::map_flags(flags));
    }

    /// The fault this module exists for, reproduced and then not reproduced.
    #[test]
    fn the_rewritten_request_yields_memory_the_guest_can_write() {
        let (prot, flags) = (
            libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            guest::MAP_PRIVATE | guest::MAP_ANON | MAP_JIT,
        );
        // SAFETY: an anonymous mapping of one host page, unmapped below.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                0x4000,
                host_prot(prot),
                host_flags(flags),
                darwin::vmmap::map_fd(flags, -1),
                0,
            )
        };
        assert_ne!(p, libc::MAP_FAILED, "rewritten mmap must succeed");
        // SAFETY: `p` is a page this test just mapped readable and writable.
        unsafe {
            std::ptr::write_bytes(p as *mut u8, 0xc3, 0x4000);
            assert_eq!(*(p as *const u8), 0xc3);
            libc::munmap(p, 0x4000);
        }
    }
}
