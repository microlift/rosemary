//! Self-modifying code in the guest's own arenas: noticing a write to bytes a
//! translation was made from.
//!
//! A JIT patches code it already emitted - Mono rewrites a `call rel32` from its
//! magic trampoline to the method once that method is compiled - and a translation
//! of the old bytes keeps doing what they said. So every host page a published
//! arena translation was lifted from is write-protected (guest code bytes are never
//! executed natively, so `PROT_READ` is all the host needs), and a write to one is
//! a fault:
//!
//! 1. [`on_fault`], in the signal handler, makes the page writable again, queues
//!    it, and returns so the write retries. No locks and no allocation: the write
//!    may be lifted code or a host shim such as `memcpy`.
//! 2. [`crate::cache::CodeCache::drain_writes`], at dispatch, re-protects each
//!    queued page and compares it against the bytes every translation on it was
//!    lifted from. Only a translation whose bytes really changed is retired, so a
//!    JIT emitting a new method beside old ones costs one fault, not a relift.
//! 3. The addresses retired go into a log every thread replays into its own
//!    window and link table before its next lookup. See [`retracted_since`].
//!
//! Only arenas recorded by [`crate::shims::vmmap`] take part. Image code is not
//! memory the guest writes, and a write the kernel makes on the guest's behalf
//! (a `read(2)` into an arena) returns `EFAULT` rather than faulting - acceptable
//! for code memory, which no guest fills that way.

use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence};

/// Guest wants the page writable: its last `mmap`/`mprotect` asked for it.
const WRITABLE: u32 = 1;
/// The host protection is `PROT_READ` because a translation was lifted from it.
const GUARDED: u32 = 2;
/// On the pending list, waiting for a drain.
const QUEUED: u32 = 4;

/// Host pages tracked. 16 bytes each, allocated zeroed on first use and touched
/// only where pages hash, so the untouched majority costs no memory.
const CAPACITY: usize = 1 << 20;

/// How far a probe goes before the table is treated as full for that page.
const MAX_PROBE: usize = 256;

struct Page {
    /// Page number plus one; zero is an empty slot.
    key: AtomicU64,
    state: AtomicU32,
    /// The next entry on the pending list, as an index plus one.
    next: AtomicU32,
}

static TABLE: AtomicPtr<Page> = AtomicPtr::new(std::ptr::null_mut());

/// Log2 of the host page size, set with the table.
static PAGE_SHIFT: AtomicU32 = AtomicU32::new(0);

/// Head of the pending list, as an index plus one. Pushed with a CAS from the
/// handler, taken whole with a swap by a drain - which is what keeps it free of
/// ABA: nothing ever pops a single node.
static PENDING: AtomicU32 = AtomicU32::new(0);

fn table() -> Option<&'static [Page]> {
    let p = TABLE.load(Ordering::Acquire);
    // SAFETY: published once, by `init`, from a leaked zeroed allocation of
    // `CAPACITY` pages; an all-zero `Page` is a valid empty one.
    (!p.is_null()).then(|| unsafe { std::slice::from_raw_parts(p, CAPACITY) })
}

fn init() -> &'static [Page] {
    if let Some(t) = table() {
        return t;
    }
    // SAFETY: `sysconf` has no preconditions.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
    PAGE_SHIFT.store(size.trailing_zeros(), Ordering::Relaxed);
    let layout = std::alloc::Layout::array::<Page>(CAPACITY).expect("page table layout");
    // SAFETY: a non-zero layout; zeroed memory is a table of empty slots.
    let fresh = unsafe { std::alloc::alloc_zeroed(layout) }.cast::<Page>();
    assert!(!fresh.is_null(), "out of memory for the arena page table");
    if TABLE
        .compare_exchange(
            std::ptr::null_mut(),
            fresh,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        // SAFETY: the losing allocation was never published.
        unsafe { std::alloc::dealloc(fresh.cast(), layout) };
    }
    table().expect("just published")
}

/// The host page size in bytes, once anything has been tracked.
pub fn page_size() -> u64 {
    match PAGE_SHIFT.load(Ordering::Relaxed) {
        0 => 0,
        shift => 1 << shift,
    }
}

fn hash(page: u64) -> usize {
    (page.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 24) as usize & (CAPACITY - 1)
}

/// The slot for `page`, if it is tracked. Async-signal-safe.
fn find(table: &'static [Page], page: u64) -> Option<(usize, &'static Page)> {
    let key = page + 1;
    let mut at = hash(page);
    for _ in 0..MAX_PROBE {
        let slot = &table[at];
        match slot.key.load(Ordering::Acquire) {
            0 => return None,
            k if k == key => return Some((at, slot)),
            _ => at = (at + 1) & (CAPACITY - 1),
        }
    }
    None
}

/// The slot for `page`, claiming one if it is new. Never from a signal handler.
fn claim(table: &'static [Page], page: u64) -> Option<&'static Page> {
    let key = page + 1;
    let mut at = hash(page);
    for _ in 0..MAX_PROBE {
        let slot = &table[at];
        match slot
            .key
            .compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return Some(slot),
            Err(k) if k == key => return Some(slot),
            Err(_) => at = (at + 1) & (CAPACITY - 1),
        }
    }
    None
}

fn pages_of(start: u64, end: u64) -> std::ops::RangeInclusive<u64> {
    let shift = PAGE_SHIFT.load(Ordering::Relaxed);
    (start >> shift)..=(end.saturating_sub(1) >> shift)
}

/// Record that the guest just set `[base, base + len)` of an arena to `prot`.
///
/// Whatever guard this runtime held there is gone - the guest's own `mprotect`
/// or `mmap` replaced the host protection - so each page is queued for a drain to
/// re-guard and recheck.
pub fn track(base: u64, len: u64, prot: i32) {
    if len == 0 {
        return;
    }
    let table = init();
    let writable = if prot & libc::PROT_WRITE != 0 {
        WRITABLE
    } else {
        0
    };
    for page in pages_of(base, base.saturating_add(len)) {
        let Some(slot) = claim(table, page) else {
            continue;
        };
        let mut state = slot.state.load(Ordering::Acquire);
        loop {
            let want = (state & QUEUED) | writable;
            match slot
                .state
                .compare_exchange(state, want, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(seen) => state = seen,
            }
        }
        if state & GUARDED != 0 {
            queue(table, slot);
        }
    }
}

/// Put `slot` on the pending list unless it is already there. Async-signal-safe.
fn queue(table: &'static [Page], slot: &'static Page) {
    if slot.state.fetch_or(QUEUED, Ordering::AcqRel) & QUEUED != 0 {
        return;
    }
    // SAFETY: `slot` is an element of `table`.
    let index = unsafe { (slot as *const Page).offset_from(table.as_ptr()) } as u32 + 1;
    let mut head = PENDING.load(Ordering::Acquire);
    loop {
        slot.next.store(head, Ordering::Relaxed);
        match PENDING.compare_exchange_weak(head, index, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(seen) => head = seen,
        }
    }
}

/// Queue the tracked pages of `ranges` for a drain to recheck, as if written.
pub fn requeue(ranges: &[std::ops::Range<u64>]) {
    let Some(table) = table() else {
        return;
    };
    for r in ranges.iter().filter(|r| r.start < r.end) {
        for page in pages_of(r.start, r.end) {
            if let Some((_, slot)) = find(table, page) {
                queue(table, slot);
            }
        }
    }
}

/// Make the page table now, so nothing that may run in a signal handler -
/// [`before_write`] - is first to allocate it.
pub fn warm() {
    init();
}

/// `darwin::mach::local`'s write barrier: a write about to be made to
/// `[address, address + len)` on another task's behalf, through the kernel,
/// which a guarded page would refuse with `EFAULT` rather than fault on. Each
/// guarded page is unguarded and queued for a drain, as a faulting store
/// would have it. Async-signal-safe.
pub fn before_write(address: u64, len: u64) {
    let Some(table) = table() else {
        return;
    };
    for page in pages_of(address, address.saturating_add(len)) {
        if find(table, page).is_some() {
            on_fault(page << PAGE_SHIFT.load(Ordering::Relaxed));
        }
    }
}

/// Whether any page is waiting for a drain. One relaxed load, for the run loop.
#[inline(always)]
pub fn dirty() -> bool {
    PENDING.load(Ordering::Relaxed) != 0
}

/// A write fault at `addr`: whether it was a guarded arena page, now writable
/// again so the faulting store can retry.
///
/// Async-signal-safe: atomics and one `mprotect`.
pub fn on_fault(addr: u64) -> bool {
    let Some(table) = table() else {
        return false;
    };
    let shift = PAGE_SHIFT.load(Ordering::Relaxed);
    let Some((_, slot)) = find(table, addr >> shift) else {
        return false;
    };
    if slot.state.load(Ordering::Acquire) & WRITABLE == 0 {
        // The guest itself made it read-only. Its fault, not ours.
        return false;
    }
    let _errno = darwin::errno::Held::now();
    slot.state.fetch_and(!GUARDED, Ordering::AcqRel);
    let page = (addr >> shift) << shift;
    // SAFETY: a page of an arena the guest mapped and wants writable.
    let rc = unsafe {
        libc::mprotect(
            page as usize as *mut libc::c_void,
            1 << shift,
            libc::PROT_READ | libc::PROT_WRITE,
        )
    };
    if rc != 0 {
        // Unmapped underneath the record: a real fault.
        return false;
    }
    // After the unprotect, so a drain that takes the page off the list has
    // already seen it writable and its re-guard wins.
    queue(table, slot);
    true
}

/// Write-protect every page of `ranges` the guest has writable, so the next
/// store to one reaches [`on_fault`]. Called with the cache's write lock held,
/// before the snapshot it guards is compared and published.
pub fn guard(ranges: &[std::ops::Range<u64>]) {
    let Some(table) = table() else {
        return;
    };
    let shift = PAGE_SHIFT.load(Ordering::Relaxed);
    for r in ranges.iter().filter(|r| r.start < r.end) {
        for page in pages_of(r.start, r.end) {
            if let Some((_, slot)) = find(table, page) {
                protect(slot, page << shift, shift);
            }
        }
    }
    // The comparison that follows must read memory after the protection is in
    // place, or a store between the two would be seen by neither.
    fence(Ordering::SeqCst);
}

fn protect(slot: &Page, at: u64, shift: u32) {
    if slot.state.load(Ordering::Acquire) & WRITABLE == 0 {
        return;
    }
    slot.state.fetch_or(GUARDED, Ordering::AcqRel);
    // SAFETY: an arena page; dropping write permission is always allowed and is
    // undone by `on_fault` on the first store.
    unsafe {
        libc::mprotect(
            at as usize as *mut libc::c_void,
            1 << shift,
            libc::PROT_READ,
        )
    };
}

/// A page taken off the pending list by [`take_pending`].
pub struct Pending(&'static Page);

impl Pending {
    /// Where the page starts.
    pub fn start(&self) -> u64 {
        (self.0.key.load(Ordering::Relaxed) - 1) << PAGE_SHIFT.load(Ordering::Relaxed)
    }

    /// Write-protect the page again, before what is on it is compared: a store
    /// before the protection is seen by the comparison, one after it faults and
    /// queues the page again.
    pub fn guard(&self) {
        let shift = PAGE_SHIFT.load(Ordering::Relaxed);
        protect(self.0, self.start(), shift);
        fence(Ordering::SeqCst);
    }
}

/// Every queued page, off the list. Called with the cache's write lock held.
///
/// A page is left as it is - writable - unless the caller guards it, so a page
/// no translation lives on any more is not faulted on again as a JIT fills it.
pub fn take_pending() -> Vec<Pending> {
    let Some(table) = table() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut at = PENDING.swap(0, Ordering::AcqRel);
    while at != 0 {
        let slot = &table[at as usize - 1];
        // Read before the flag is cleared: once it is, a handler may push the
        // page again and overwrite the link.
        at = slot.next.load(Ordering::Relaxed);
        slot.state.fetch_and(!QUEUED, Ordering::AcqRel);
        out.push(Pending(slot));
    }
    out
}

/// Every address retired so far, in order. Threads replay it from their own
/// cursor; see [`retracted_since`].
static LOG: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static LOGGED: AtomicUsize = AtomicUsize::new(0);

/// Append retired addresses to the log every thread replays.
pub fn retire(addresses: &[u64]) {
    if addresses.is_empty() {
        return;
    }
    let mut log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    log.extend_from_slice(addresses);
    LOGGED.store(log.len(), Ordering::Release);
}

/// How long the log is. A new thread starts here: it has nothing to retract.
pub fn logged() -> usize {
    LOGGED.load(Ordering::Acquire)
}

/// The addresses retired since `cursor`, and the cursor to resume from - or
/// `None` without taking the lock when nothing was.
#[inline(always)]
pub fn retracted_since(cursor: usize) -> Option<(Vec<u64>, usize)> {
    if LOGGED.load(Ordering::Acquire) == cursor {
        return None;
    }
    let log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    Some((log[cursor.min(log.len())..].to_vec(), log.len()))
}

/// The bytes one translation was lifted from, kept to tell a write that changed
/// them from one that landed beside them.
pub struct Snapshot {
    /// The address the translation was published under first; it is live while
    /// the cache still maps this address to it.
    pub entry: u64,
    pub ranges: Vec<(u64, Box<[u8]>)>,
}

impl Snapshot {
    /// The runs of bytes that no longer match, as `(start, len)`.
    pub fn changed(&self) -> Vec<(u64, u64)> {
        changed(&self.ranges)
    }

    /// The host pages this snapshot covers.
    pub fn pages(&self) -> impl Iterator<Item = u64> + '_ {
        let shift = PAGE_SHIFT.load(Ordering::Relaxed);
        self.ranges.iter().flat_map(move |(start, bytes)| {
            pages_of(*start, start + bytes.len() as u64).map(move |p| p << shift)
        })
    }

    pub fn spans(&self) -> Vec<std::ops::Range<u64>> {
        self.ranges
            .iter()
            .map(|(s, b)| *s..s + b.len() as u64)
            .collect()
    }
}

/// The runs of `ranges` whose live bytes differ from the copies, as `(start, len)`.
fn changed(ranges: &[(u64, Box<[u8]>)]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for (start, bytes) in ranges {
        let live = *start as usize as *const u8;
        let mut run: Option<usize> = None;
        for i in 0..=bytes.len() {
            // SAFETY: arena memory, never unmapped (see `crate::shims::vmmap`).
            // Volatile because another thread may be storing to it.
            let differs = i < bytes.len() && unsafe { live.add(i).read_volatile() } != bytes[i];
            match (differs, run) {
                (true, None) => run = Some(i),
                (false, Some(from)) => {
                    out.push((start + from as u64, (i - from) as u64));
                    run = None;
                }
                _ => {}
            }
        }
    }
    out
}

/// Whether `[start, start + len)` lies in tracked arena memory at all.
pub fn tracked(start: u64, len: u64) -> bool {
    let Some(table) = table() else {
        return false;
    };
    pages_of(start, start.saturating_add(len)).all(|p| find(table, p).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(len: usize) -> *mut u8 {
        // SAFETY: a fresh anonymous mapping.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert_ne!(p, libc::MAP_FAILED);
        p.cast()
    }

    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "guard accepts a slice of address ranges, not a collected list of addresses"
    )]
    fn a_fault_on_a_guarded_page_unprotects_and_queues_it() {
        let _ = init();
        let size = page_size() as usize;
        let p = map(size * 2);
        track(
            p as u64,
            (size * 2) as u64,
            libc::PROT_READ | libc::PROT_WRITE,
        );
        // One guarded range, not a vector of addresses within the range.
        let ranges: [std::ops::Range<u64>; 1] = [p as u64..p as u64 + 8];
        guard(&ranges);
        // Not the second page: it holds nothing.
        assert!(on_fault(p as u64 + 3));
        // SAFETY: writable again after `on_fault`.
        unsafe { *p = 0x90 };
        // Other tests may have queued pages of their own.
        let pending = take_pending();
        let mine = pending
            .iter()
            .find(|q| q.start() == p as u64)
            .expect("the faulted page is queued");
        mine.guard();
        // Re-guarded by the drain.
        assert!(on_fault(p as u64));
    }

    #[test]
    fn a_page_the_guest_made_read_only_is_not_ours_to_unprotect() {
        let _ = init();
        let size = page_size() as usize;
        let p = map(size);
        track(p as u64, size as u64, libc::PROT_READ);
        assert!(!on_fault(p as u64));
    }

    #[test]
    fn a_snapshot_reports_only_the_bytes_that_changed() {
        let mut bytes = vec![0u8; 64];
        let base = bytes.as_ptr() as u64;
        let ranges = vec![(base, bytes.clone().into_boxed_slice())];
        assert!(changed(&ranges).is_empty());
        bytes[10] = 1;
        bytes[11] = 1;
        bytes[40] = 1;
        assert_eq!(changed(&ranges), vec![(base + 10, 2), (base + 40, 1)]);
    }
}
