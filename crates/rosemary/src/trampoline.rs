use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use microlift::core::bridge::ImportKind;
use static_init::dynamic;

use crate::abi::{Classification, FunctionAbi, Lookup};
use crate::program::{AddrMap, ImportSite};

/// `ROSEMARY_TRAMPOLINES=0` turns the whole thing off.
const ENV: &str = "ROSEMARY_TRAMPOLINES";

/// Bytes per entry. Five are used; the rest is `int3` padding, so that a walk
/// that peeks past the `jmp` reads something that is neither zero nor code.
const STRIDE: u64 = 16;

/// How many symbols may be trampolined.
const SLOTS: usize = 8192;

/// Bytes in each half of the reservation.
const HALF: u64 = SLOTS as u64 * STRIDE;

/// Base of the whole reservation, or zero before the arena exists.
static BASE: AtomicU64 = AtomicU64::new(0);

/// One trampolined symbol.
struct Entry {
    /// The site the entry stands for, leaked so it can be handed out with a `'static`
    /// lifetime - the same thing [`crate::dyld`] does with a `dlsym` result, and for
    /// the same reason.
    site: &'static ImportSite,
    /// Its signature, resolved once when the entry was made.
    abi: &'static FunctionAbi,
}

/// Everything that needs a lock: allocation, and the two directions of lookup.
///
/// Written once per symbol in a process's life and read at lift time only, so a mutex
/// is the whole of the concurrency design.
struct State {
    /// Entries in slot order.
    entries: Vec<Entry>,
    /// Host address to the trampoline that stands for it, so one symbol has one entry
    /// however many times it is asked for.
    by_target: AddrMap<u64>,
}

static STATE: RwLock<Option<State>> = RwLock::new(None);

/// Whether the run this is part of wants trampolines at all.
static ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Take trampolines out of the run, before any guest code has seen one.
pub fn disarm() {
    ARMED.store(false, Ordering::Relaxed);
}

/// Whether guest calls use the host instruction set and calling convention.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub(crate) fn running_natively() -> bool {
    !ARMED.load(Ordering::Relaxed)
}

/// Whether trampolines are in use.
fn enabled() -> bool {
    #[dynamic]
    static ON: bool = {
        std::env::var(ENV)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(1)
            != 0
    };
    ARMED.load(Ordering::Relaxed) && *ON
}

/// Entries made, slots substituted, and entries the guest actually reached.
static MADE: AtomicU64 = AtomicU64::new(0);
static SUBSTITUTED: AtomicU64 = AtomicU64::new(0);
static REACHED: AtomicU64 = AtomicU64::new(0);

/// What the trampolines did this run, or `None` if there were none.
pub fn counts() -> Option<(u64, u64, u64)> {
    let made = MADE.load(Ordering::Relaxed);
    (made != 0).then(|| {
        (
            made,
            SUBSTITUTED.load(Ordering::Relaxed),
            REACHED.load(Ordering::Relaxed),
        )
    })
}

/// Whether `va` is a trampoline, and so guest code.
pub fn contains(va: u64) -> bool {
    let base = BASE.load(Ordering::Relaxed);
    base != 0 && va >= base && va < base + HALF
}

/// Reserve the arena, once.
///
/// The code half is readable and writable *by this runtime* and never executable - the
/// guest is x86-64 and this CPU cannot run a byte of it, which is the same decision
/// every guest mapping is made under.
fn arena_locked() -> Option<u64> {
    let base = BASE.load(Ordering::Relaxed);
    if base != 0 {
        return Some(base);
    }
    // SAFETY: an anonymous private mapping of a fixed size at an address of the
    // kernel's choosing. Nothing is assumed about where it lands beyond the two
    // halves being adjacent, which one mapping guarantees.
    let base = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            (HALF * 2) as usize,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if base == libc::MAP_FAILED {
        return None;
    }
    let base = base as usize as u64;
    // SAFETY: the upper half of the mapping just returned, page-aligned because
    // `HALF` is a multiple of the page size.
    unsafe {
        libc::mprotect(
            (base + HALF) as usize as *mut libc::c_void,
            HALF as usize,
            libc::PROT_NONE,
        );
    }
    // `int3` everywhere, so a decode that runs past an entry's `jmp` reads a
    // one-byte trap rather than the zeroes [`crate::region`] treats as garbage
    // and the padding of the *next* entry as an instruction.
    //
    // SAFETY: the code half of the mapping, writable and owned by this module.
    unsafe { std::ptr::write_bytes(base as usize as *mut u8, 0xCC, HALF as usize) };

    // Released after the fill: a reader that sees a non-zero base must find the
    // bytes already in place.
    BASE.store(base, Ordering::Release);
    Some(base)
}

/// The trampoline standing for `site`, creating it if this is the first ask.
///
/// `None` - meaning "call it the way you always did" - for every symbol a fast import
/// cannot express, which is the list in the module note.
pub fn for_site(site: &ImportSite) -> Option<u64> {
    if !enabled() || site.resolved == 0 {
        return None;
    }
    // The same three questions `ImportPolicy::promotion_for` asks of a static
    // call site, asked here because the answer must be the same.
    if crate::trace::watching(&site.symbol) {
        return None;
    }
    if crate::abi::classify(site.tag, &site.symbol) != Classification::Fast {
        return None;
    }
    let Lookup::Known(abi) = crate::abi::table::lookup(site.tag, &site.symbol) else {
        return None;
    };

    let mut guard = STATE.write().unwrap_or_else(|e| e.into_inner());
    // Under the lock, so the reservation happens once however many threads
    // arrive together. See [`arena_locked`].
    let base = arena_locked()?;
    let state = guard.get_or_insert_with(|| State {
        entries: Vec::new(),
        by_target: AddrMap::default(),
    });
    if let Some(&existing) = state.by_target.get(&site.resolved) {
        return Some(existing);
    }
    let slot = state.entries.len();
    if slot >= SLOTS {
        // Out of arena. Not an error: the caller falls back to the marshaller,
        return None;
    }
    let va = base + slot as u64 * STRIDE;
    write_jmp(va);
    state.entries.push(Entry {
        site: Box::leak(Box::new(site.clone())),
        abi,
    });
    state.by_target.insert(site.resolved, va);
    MADE.fetch_add(1, Ordering::Relaxed);
    // Two addresses that meant nothing a moment ago - the stub and the import
    // address behind it. See [`crate::dispatch::invalidate`].
    crate::dispatch::invalidate();
    Some(va)
}

/// Encode `jmp rel32` at `va`, targeting the import address `HALF` bytes above.
///
/// The five bytes are written before the slot is published, and no other slot's
/// bytes are touched, so a concurrent lift of a *different* trampoline reads
/// only settled memory.
///
/// A tail `jmp` rather than `call S; ret`: a `CallAbi`'s `ArgLocation::Stack` offsets
/// are measured from the guest stack pointer *before the call pushes anything*, so the
/// seventh integer argument of a SysV call is at `rsp+0`. A `call` here would push a
/// second return address underneath the arguments and put every one of them eight
/// bytes off. Microlift pops the return address before the inline call, so a `jmp`
/// leaves the stack exactly as a direct call site would.
fn write_jmp(va: u64) {
    // `HALF` from the start of this entry, less the five bytes the instruction
    // itself occupies - a constant, since the two halves are the same size.
    let rel = (HALF - 5) as u32;
    let mut bytes = [0u8; 5];
    bytes[0] = 0xE9;
    bytes[1..].copy_from_slice(&rel.to_le_bytes());
    // SAFETY: `va` is the start of an unused slot in the code half of the arena
    // this module mapped read/write, so five bytes from it are in bounds and
    // owned here. No guest thread can hold this address yet - it is published by
    // the caller after this returns.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), va as usize as *mut u8, bytes.len()) };
}

/// The slot `va` belongs to, if `va` is the entry point of a trampoline.
///
/// Only the entry point, deliberately.
fn code_slot(va: u64) -> Option<usize> {
    if !contains(va) {
        return None;
    }
    let offset = va - BASE.load(Ordering::Relaxed);
    (offset.is_multiple_of(STRIDE)).then_some((offset / STRIDE) as usize)
}

/// The slot the import address `va` belongs to.
fn import_slot(va: u64) -> Option<usize> {
    let base = BASE.load(Ordering::Relaxed);
    if base == 0 || va < base + HALF || va >= base + HALF * 2 {
        return None;
    }
    let offset = va - (base + HALF);
    (offset.is_multiple_of(STRIDE)).then_some((offset / STRIDE) as usize)
}

/// Whatever `f` says about the entry in `slot`, if there is one.
fn with_entry<T>(slot: usize, f: impl FnOnce(&Entry) -> T) -> Option<T> {
    let guard = STATE.read().unwrap_or_else(|e| e.into_inner());
    guard.as_ref()?.entries.get(slot).map(f)
}

/// The region to lift in order to execute the trampoline at `va`.
pub fn lift_region(va: u64) -> Option<(crate::region::Region, Vec<u8>)> {
    let slot = code_slot(va)?;
    // An address in the arena that has not been handed out is not code.
    with_entry(slot, |_| ())?;
    let mut bytes = [0u8; STRIDE as usize];
    // SAFETY: `code_slot` placed `va` at the start of a slot inside the code
    // half, so `STRIDE` bytes from it are within the mapping. The slot's five
    // instruction bytes were written before it was published and the rest is
    // the `int3` fill from `arena`, so nothing here is in flux.
    unsafe {
        std::ptr::copy_nonoverlapping(va as usize as *const u8, bytes.as_mut_ptr(), bytes.len())
    };
    // Counted here rather than at every call.
    REACHED.fetch_add(1, Ordering::Relaxed);
    let region = crate::region::region_for(&bytes, va, va, false)?;
    let len = usize::try_from(region.range.end - va).ok()?;
    Some((region, bytes.get(..len)?.to_vec()))
}

/// How microlift should reach `va`, if `va` is a trampoline's import address.
pub fn import_kind(va: u64) -> Option<ImportKind> {
    let slot = import_slot(va)?;
    with_entry(slot, |entry| {
        ImportKind::Fast(crate::abi::fast_import(
            entry.site,
            &crate::abi::Promotion {
                abi: entry.abi,
                address: entry.site.resolved,
            },
        ))
    })
}

/// The site behind a trampoline's import address.
///
/// Nothing on the fast path needs this.
pub fn import_at(va: u64) -> Option<&'static ImportSite> {
    with_entry(import_slot(va)?, |entry| entry.site)
}

/// The host function a trampoline stands for, if `va` is one.
pub fn host_behind(va: u64) -> Option<u64> {
    with_entry(code_slot(va)?, |entry| entry.site.resolved)
}

/// Point every eligible binding in `imports` at a trampoline, in `memory`.
pub fn install<'a>(
    imports: impl IntoIterator<Item = (u64, &'a ImportSite)>,
    is_guest: &dyn Fn(u64) -> bool,
    memory: &mut dyn crate::abi::GuestMemory,
) -> usize {
    if !enabled() {
        return 0;
    }
    let mut installed = 0;
    for (slot_va, site) in imports {
        if site.is_stub || is_guest(site.resolved) {
            continue;
        }
        let Some(va) = for_site(site) else {
            continue;
        };
        if memory.write(slot_va, &va.to_le_bytes()).is_ok() {
            installed += 1;
            SUBSTITUTED.fetch_add(1, Ordering::Relaxed);
        }
    }
    installed
}

/// `va` as a name, for a diagnostic. See [`crate::Program::locate`].
pub fn locate(va: u64) -> Option<String> {
    let slot = code_slot(va).or_else(|| import_slot(va))?;
    with_entry(slot, |entry| {
        format!("<trampoline for {}>", entry.site.symbol)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(symbol: &str, resolved: u64) -> ImportSite {
        ImportSite {
            symbol: symbol.to_string(),
            library: None,
            resolved,
            tag: 0,
            is_stub: false,
            plan: Default::default(),
        }
    }

    /// Nothing is a trampoline until one is made, and the rejection is the
    /// answer `is_guest_address` gets for every host pointer in the process.
    #[test]
    fn an_address_outside_the_arena_is_not_one() {
        assert!(!contains(0));
        assert!(!contains(0x1000));
        assert!(lift_region(0x1000).is_none());
        assert!(import_kind(0x1000).is_none());
    }

    /// An unsatisfied import resolves to null, and a trampoline to null would be
    /// a `jmp` into a fast import whose address is zero.
    #[test]
    fn an_unresolved_import_gets_none() {
        assert!(for_site(&site("_strlen", 0)).is_none());
    }

    /// The whole exclusion list in one property: only a symbol that would have
    /// been a fast import gets an entry.
    #[test]
    fn only_a_fast_import_is_trampolined() {
        // No signature at all.
        assert!(for_site(&site("_no_such_symbol_anywhere", 0x1234)).is_none());
        // Variadic, and reached through a shim.
        assert!(for_site(&site("_printf", 0x1234)).is_none());
    }

    /// The entry is one `jmp rel32` whose displacement lands on the import
    /// address the registry answers for - which is the entire contract between
    /// the two halves of the arena.
    #[test]
    fn the_jump_lands_on_the_registered_import() {
        let Some(va) = for_site(&site("_strlen", 0x1234)) else {
            // `_strlen` has to be in the table for this to mean anything; if the
            // table ever stops describing it the other tests still hold.
            return;
        };
        assert!(contains(va));

        let mut bytes = [0u8; 5];
        // SAFETY: `va` is a published trampoline, so five bytes from it are the
        // instruction this module wrote.
        unsafe { std::ptr::copy_nonoverlapping(va as usize as *const u8, bytes.as_mut_ptr(), 5) };
        assert_eq!(bytes[0], 0xE9);
        let rel = i32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        let target = (va + 5).wrapping_add(rel as i64 as u64);

        assert!(matches!(import_kind(target), Some(ImportKind::Fast(_))));
        assert_eq!(
            import_at(target).map(|s| s.symbol.as_str()),
            Some("_strlen")
        );
        // And the import address is not itself guest code, so a transfer to it
        // is the diagnostic it should be rather than a decode of `PROT_NONE`.
        assert!(!contains(target));
    }

    /// One symbol, one trampoline: the guest may compare two pointers it
    /// obtained separately and must get the answer it would have got natively.
    #[test]
    fn a_symbol_gets_one_entry() {
        let (a, b) = (
            for_site(&site("_strlen", 0x1234)),
            for_site(&site("_strlen", 0x1234)),
        );
        assert_eq!(a, b);
    }

    /// The lifted region is the `jmp` and nothing else - in particular the walk
    /// must not follow it into the import half, which is unreadable.
    #[test]
    fn the_region_is_the_jump_alone() {
        let Some(va) = for_site(&site("_strlen", 0x1234)) else {
            return;
        };
        let (region, code) = lift_region(va).expect("a published trampoline is code");
        assert_eq!(region.range, va..va + 5);
        assert_eq!(code, vec![0xE9, code[1], code[2], code[3], code[4]]);
        assert_eq!(region.starts, vec![va]);
    }

    /// Threads arriving together must all get trampolines out of **one** arena.
    #[test]
    fn concurrent_first_callers_share_one_arena() {
        const NAMES: &[&str] = &[
            "_strlen", "_strcmp", "_memcpy", "_memset", "_strchr", "_strrchr", "_abs", "_atoi",
        ];
        // A barrier, so "together" is a fact rather than a hope.
        let gate = std::sync::Barrier::new(NAMES.len());
        let made: Vec<u64> = std::thread::scope(|s| {
            let handles: Vec<_> = NAMES
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let gate = &gate;
                    s.spawn(move || {
                        gate.wait();
                        for_site(&site(name, 0x9000 + i as u64 * 0x10))
                    })
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().unwrap())
                .collect()
        });
        assert!(!made.is_empty(), "these symbols are all in the table");
        for va in made {
            assert!(
                contains(va),
                "{va:#x} was handed out and is not in the arena `contains` answers for"
            );
            assert!(lift_region(va).is_some(), "and it must be liftable");
        }
    }

    /// The padding inside an entry is not an entry point. A request to execute
    /// it is a bug worth reporting rather than decoding.
    #[test]
    fn the_interior_of_an_entry_is_not_code() {
        let Some(va) = for_site(&site("_strlen", 0x1234)) else {
            return;
        };
        assert!(lift_region(va + 1).is_none());
        assert!(lift_region(va + 5).is_none());
    }
}
