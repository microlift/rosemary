use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};

use machish::memory::Regions;
use machish::report::LinkReport;

/// Set to skip every interposition, leaving each image bound to the host's own.
///
/// A debugging lever, and the one worth reaching for first when a program
/// corrupts its heap. The interposition a guest declares is nearly always an
/// allocator replacing `malloc`/`free`, and a replacement allocator is silent
/// about a double free where glibc's own aborts with a sentence naming it - so
/// turning the replacement off does not fix the bug, it moves the report from
/// "a corrupted free list, thousands of allocations later" to "free(): invalid
/// pointer, here".
///
/// Not safe in general: an image that interposes for a reason other than
/// allocation gets its replacement dropped.
pub const NO_INTERPOSE_ENV: &str = "ROSETTA3_NO_INTERPOSE";

/// Set to print every binding an interposition moves.
///
/// The question it answers is "did the replacement allocator reach *this*
/// image": a program with two allocators is one where some image's `free` was
/// missed, and [`missed`] can only see the ones that stayed pointing at the
/// host under a symbol something else moved.
pub const TRACE_ENV: &str = "ROSETTA3_TRACE_INTERPOSE";

/// Set to leave this tree's frameworks on the host's allocator, rather than the
/// guest's.
///
/// Narrower than [`NO_INTERPOSE_ENV`] and useful for the opposite reason: the
/// guest keeps its own allocator throughout, and the only thing that changes is
/// whether `objc::allocator` and `darwin::heap` call into it. So a heap
/// corruption that survives this is not one of *these* crossings, and one that
/// does not is.
///
/// Not a fix either way: with it set, the handful of blocks this tree hands the
/// guest to `free` come from the wrong allocator instead. It is an A/B, not a
/// mode to run in.
pub const NO_ADOPT_ENV: &str = "ROSETTA3_NO_ADOPT_ALLOCATOR";

fn tracing() -> bool {
    std::env::var_os(TRACE_ENV).is_some()
}

fn disabled() -> bool {
    std::env::var_os(NO_INTERPOSE_ENV).is_some_and(|v| v != "0")
}

/// Apply every interposition the link's images declare.
pub fn apply<M: Regions>(report: &mut LinkReport, memory: &mut M) -> Result<usize, machish::Error> {
    if disabled() {
        eprintln!(
            "rosetta3: interpose: every interposition skipped ({NO_INTERPOSE_ENV} is set) - the \
             program is using the host's allocator throughout"
        );
        return Ok(0);
    }
    // Gathered before the loop because the loop mutates `report.images`, and
    // because an image may interpose a function another image also interposes -
    // in which case dyld applies both, in load order, and so does this.
    let requests = remember(report, true);
    if requests.is_empty() {
        return Ok(0);
    }

    // Which symbols actually moved. Kept so the split check below can ask the
    // one question that matters about a partial interposition - see [`missed`].
    let mut moved: BTreeSet<String> = BTreeSet::new();
    let rebound = rebind(report, memory, &requests, &mut moved)?;
    let interposers: BTreeSet<u64> = report
        .images
        .iter()
        .filter(|i| !i.interpositions.is_empty())
        .map(|i| i.base)
        .collect();
    for line in missed(report, &moved, &interposers) {
        eprintln!("rosetta3: interpose: {line}");
    }
    Ok(rebound)
}

/// Every interposition an image loaded so far declared, as `(interposer, pair)`.
///
/// Kept because dyld applies an interposition to images loaded *later* too: a
/// plugin `dlopen`ed after the program started binds `free` like any other
/// image, and left on the host's it frees blocks the replacement made. Qt's
/// `libqcocoa` is the case in hand - its `operator delete` of a `QPointer`'s
/// control block, allocated in `QtCore` by Studio's mimalloc, faulted in the
/// host's mimalloc.
static ACTIVE: std::sync::Mutex<Vec<(u64, machish::report::Interposition)>> =
    std::sync::Mutex::new(Vec::new());

/// Add `report`'s own interpositions to [`ACTIVE`] and return all of them.
/// `program` starts the list afresh, as a program's link is the first.
fn remember(report: &LinkReport, program: bool) -> Vec<(u64, machish::report::Interposition)> {
    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    if program {
        active.clear();
    }
    active.extend(
        report
            .images
            .iter()
            .flat_map(|i| i.interpositions.iter().map(move |p| (i.base, *p))),
    );
    active.clone()
}

/// Point every binding in `report` that resolved to a replacee at its
/// replacement, except in the image that asked for it.
fn rebind<M: Regions>(
    report: &mut LinkReport,
    memory: &mut M,
    requests: &[(u64, machish::report::Interposition)],
    moved: &mut BTreeSet<String>,
) -> Result<usize, machish::Error> {
    let mut rebound = 0;
    for (from, pair) in requests {
        for image in &mut report.images {
            if image.base == *from {
                continue;
            }
            for binding in &mut image.bindings {
                if binding.resolved != pair.replacee {
                    continue;
                }
                if tracing() {
                    eprintln!(
                        "rosetta3: interpose: {} in {} -> {:#x}",
                        binding.symbol,
                        image.name.rsplit('/').next().unwrap_or(&image.name),
                        pair.replacement
                    );
                }
                memory.write(binding.slot_va, &pair.replacement.to_le_bytes())?;
                binding.resolved = pair.replacement;
                // The tag names the *provider* that answered, and the provider
                // that answered was the host's.
                binding.tag = 0;
                moved.insert(binding.symbol.clone());
                rebound += 1;
            }
        }
    }
    Ok(rebound)
}

/// [`apply`] for an image the guest `dlopen`ed: the interpositions already in
/// effect, and any the new image declares, applied to its bindings.
pub fn apply_to_loaded<M: Regions>(
    report: &mut LinkReport,
    memory: &mut M,
) -> Result<usize, machish::Error> {
    if disabled() {
        return Ok(0);
    }
    let requests = remember(report, false);
    if requests.is_empty() {
        return Ok(0);
    }
    rebind(report, memory, &requests, &mut BTreeSet::new())
}

/// The `malloc`/`free` the guest's own images call, once every interposition
/// has been applied.
///
/// Not the same pair this process uses. A guest that ships an allocator has
/// replaced them for its own images, and the frameworks in this tree are *not*
/// among those images - they are host code, linked against the host's libc
/// before the guest existed. So a block one of them hands the guest to `free`
/// is freed by an allocator that never made it; see
/// [`objc::allocator`] for what that looks like when it goes wrong.
///
/// Read out of the bindings rather than by name, because the answer wanted is
/// "what does the guest's code actually call", which is exactly what a binding
/// records - after [`apply`] has had its say.
pub fn guest_allocator(report: &LinkReport) -> Option<(u64, u64)> {
    let resolved = |name: &str| {
        report
            .images
            .iter()
            .flat_map(|image| &image.bindings)
            .find(|binding| darwin::base_name(&binding.symbol) == name && binding.resolved != 0)
            .map(|binding| binding.resolved)
    };
    Some((resolved("malloc")?, resolved("free")?))
}

/// Point this tree's frameworks at the guest's allocator, so a block they hand
/// it can be freed by the allocator that made it.
///
/// The **native backend only**. Under emulation the guest's allocator is
/// x86-64 this process cannot enter without the run loop, so calling it from a
/// framework would be a jump into unlifted bytes; nothing is registered there
/// and the host's own is used, which is what every other host call in that
/// backend already does.
pub fn adopt_guest_allocator(report: &LinkReport) {
    if std::env::var_os(NO_ADOPT_ENV).is_some_and(|v| v != "0") {
        eprintln!(
            "rosetta3: interpose: this tree's frameworks will keep the host's allocator \
             ({NO_ADOPT_ENV} is set)"
        );
        return;
    }
    let Some((malloc, free)) = guest_allocator(report) else {
        return;
    };
    GUEST_MALLOC.store(malloc as usize, Ordering::Release);
    GUEST_FREE.store(free as usize, Ordering::Release);
    let malloc_entry = malloc_as_darwin as *const () as usize;
    let free_entry = free_as_darwin as *const () as usize;
    objc::allocator::register(malloc_entry, free_entry);
    let owns = if heap_region_describes_the_host_allocator() {
        owns_block as *const () as usize
    } else {
        // Named rather than installed. A predicate that answers about a
        // different allocator would turn every live block into a zero, which
        // is the failure this whole mechanism exists to prevent, and it would
        // do it silently. With none installed, `darwin::heap::owns` answers
        // `None` and `darwin::malloc::malloc_size` behaves as it did before.
        eprintln!(
            "rosetta3: interpose: mimalloc does not own a block this host's own malloc just \
             returned, so no ownership predicate is registered - `darwin::malloc::malloc_size` \
             will have no way to tell a live block from a foreign pointer, and a foreign pointer \
             will reach the host's malloc_usable_size undefined, which is what it did before this \
             was added"
        );
        0
    };
    darwin::heap::register(malloc_entry, free_entry, owns);
    if tracing() {
        eprintln!("rosetta3: interpose: guest allocator malloc {malloc:#x}, free {free:#x}");
        // The fact the ownership predicate above rests on, printed when asked
        // for rather than assumed. `guest_allocator` reads whatever the guest's
        // images resolved to, and that is a second allocator if it is not the
        // host's own.
        let host_malloc = libc::malloc as *const () as usize;
        eprintln!(
            "rosetta3: interpose: the guest's malloc {} the host's malloc {host_malloc:#x}, and \
             the ownership predicate is {}",
            if malloc as usize == host_malloc {
                "is the same function as"
            } else {
                "is NOT the same function as"
            },
            if owns == 0 { "NOT installed" } else { "installed" },
        );
    }
}

unsafe extern "C" {
    /// mimalloc v3's `bool mi_is_in_heap_region(const void *p)`.
    ///
    /// **Not exported by the `mimalloc` crate.** It is declared in
    /// `libmimalloc_sys::extended`, which that crate gates behind a feature
    /// this one does not enable (`crates/rosetta3/Cargo.toml:47`), so there is
    /// no `mimalloc::mi_is_in_heap_region` to call. `dlsym` cannot find it
    /// either, and not because the archive is statically linked: `MI_OVERRIDE`
    /// makes this executable *define* the libc names, so `malloc`, `free`,
    /// `realloc`, `calloc` and `malloc_usable_size` are all in its `.dynsym`
    /// (measured, entries 400-419), and the `mi_`-prefixed spellings are not
    /// exported at all - so `dlsym` has nowhere to look for this one. It is in
    /// `libmimalloc.a` as its own section `.text.mi_is_in_heap_region` (0x14
    /// bytes), compiled with `-ffunction-sections` and dropped by
    /// `--gc-sections` precisely because nothing referenced it; naming it here
    /// is what keeps it, and it is in no symbol table of the built binary until
    /// this line exists.
    ///
    /// Defined for **every** pointer, foreign included: it is
    /// `_mi_safe_ptr_page(p) != NULL` (mimalloc v3 `page-map.c:443`), the
    /// checked lookup whose submap test `_mi_ptr_page` skips when `MI_DEBUG`
    /// is 0 and the target is not Apple. That is the predicate the fix needs and
    /// the one the unchecked build cannot do for itself.
    fn mi_is_in_heap_region(p: *const std::ffi::c_void) -> bool;
}

/// `mi_is_in_heap_region`, as `darwin::heap`'s ownership predicate.
///
/// The same answer for a pointer inside a live segment but not at a block start
/// - `true`, and then `malloc_usable_size` sizes the segment's block, which is
/// what the host's own allocator says for freed memory and what Darwin says
/// through its zone's `size` slot. The predicate answers "is this pointer in
/// this allocator at all", which is the question that has a false answer, not
/// "is this a live block", which is one only a heap can enumerate.
unsafe extern "C" fn owns_block(p: *const std::ffi::c_void) -> bool {
    // SAFETY: `mi_is_in_heap_region` reads only mimalloc's own page map and is
    // defined for any pointer; see the declaration above.
    unsafe { mi_is_in_heap_region(p) }
}

/// Whether the host's `malloc_usable_size` is mimalloc's, asked as a question
/// with an answer rather than assumed from the dependency list.
///
/// The control that has to pass before the predicate is installed: allocate one
/// block from **the host's own `malloc`** - the function `malloc_size` will go
/// on to call `malloc_usable_size` about - and ask mimalloc whether it owns it.
/// If it does not, then asking mimalloc about a pointer is asking a different
/// allocator than the one being sized, every live block would come back "not
/// owned", and the zero would be a wrong answer wearing a correct-looking
/// name.
///
/// A block is also asked about, so this fails in the other direction too: a
/// predicate that says `false` to the host's own block would answer `malloc_size`
/// 0 for it, which is as wrong as the other case and is caught here.
fn heap_region_describes_the_host_allocator() -> bool {
    // SAFETY: a size this function asks for itself and frees before returning.
    let block = unsafe { libc::malloc(64) };
    if block.is_null() {
        eprintln!(
            "rosetta3: interpose: the host's own malloc returned null for a 64-byte probe, so no \
             ownership predicate is registered - there is no block to check the predicate against"
        );
        return false;
    }
    // SAFETY: `block` is a live block of this host's allocator.
    let says_yes = unsafe { mi_is_in_heap_region(block) };
    // SAFETY: `block` came from `malloc` and is not used again.
    unsafe { libc::free(block) };
    if !says_yes {
        return false;
    }
    // The other direction, on an address that is certainly not a block.
    let stack = &0u8 as *const u8 as *const std::ffi::c_void;
    !unsafe { mi_is_in_heap_region(stack) }
}

/// The guest's own `malloc` and `free`, behind [`malloc_as_darwin`].
static GUEST_MALLOC: AtomicUsize = AtomicUsize::new(0);
static GUEST_FREE: AtomicUsize = AtomicUsize::new(0);

/// The guest's allocator, entered with this thread's Darwin thread pointer.
///
/// Registered in place of the allocator itself because the allocator is Darwin
/// machine code and reads the thread pointer directly: mimalloc's `free` decides
/// between its unsynchronised local free list and the atomic cross-thread one by
/// comparing the owning thread's identity against `%gs:0`. A pool worker or any
/// other thread this runtime started has the host's pointer there, so the
/// comparison is between a real thread and whatever that address holds - and the
/// allocator either faults on a low address or takes the local path on a page it
/// does not own. The guard costs nothing on a thread that is inside Darwin code
/// already, which every guest thread is.
unsafe extern "C" fn malloc_as_darwin(size: usize) -> *mut std::ffi::c_void {
    // SAFETY: `GUEST_MALLOC` holds the address of the guest's own `malloc`.
    let malloc: unsafe extern "C" fn(usize) -> *mut std::ffi::c_void =
        unsafe { std::mem::transmute(GUEST_MALLOC.load(Ordering::Acquire)) };
    // SAFETY: the caller's size, answered by the allocator the guest also frees with.
    unsafe { darwin::call_as_darwin!(malloc(size)) }
}

/// As [`malloc_as_darwin`], for `free`.
unsafe extern "C" fn free_as_darwin(block: *mut std::ffi::c_void) {
    if quarantine_free() {
        quarantine::defer(block);
        return;
    }
    // SAFETY: `GUEST_FREE` holds the address of the guest's own `free`.
    let free: unsafe extern "C" fn(*mut std::ffi::c_void) =
        unsafe { std::mem::transmute(GUEST_FREE.load(Ordering::Acquire)) };
    // SAFETY: the caller states the block came from the guest's allocator.
    unsafe { darwin::call_as_darwin!(free(block)) };
}

/// Whether freed blocks are held back instead of returned to the allocator.
///
/// `ROSETTA3_QUARANTINE_FREE=1`. **Off by default, and a diagnostic, not a
/// fix.** The reasoning is in [`quarantine`]; the short version is that the
/// guest has a use-after-free at
/// `blink/renderer/platform/scheduler/worker/non_main_thread_impl.cc`
/// `+0x4b4814b` - a `lock dec` whose result is never tested, immediately
/// followed by `call [rax+0x10]` on the same pointer. Whether that is *fatal*
/// depends entirely on whether the allocator hands the same block straight back,
/// and it does: the thread that was just destroyed is the one running the code.
///
/// Withholding freed blocks makes the use visible without changing it - the
/// stale memory keeps whatever it last held, so the fault moves somewhere real
/// instead of into recycled space, and the guest gets far enough to show what it
/// was trying to do. It costs memory, so it is not something to leave on.
fn quarantine_free() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let on = std::env::var("ROSETTA3_QUARANTINE_FREE")
            .is_ok_and(|v| !v.is_empty() && v != "0");
        if on {
            eprintln!(
                "rosetta3: interpose: ROSETTA3_QUARANTINE_FREE is set - freed blocks are held \
                 back, not returned to the allocator"
            );
        }
        on
    })
}

/// Freed blocks this run is holding back.
mod quarantine {
    use std::sync::Mutex;

    /// `ROSETTA3_QUARANTINE_FREE=N` caps it. The default is generous: the point
    /// is to survive a start-up, not to be a leak.
    const DEFAULT_LIMIT: usize = 1 << 20;
    const ENV_LIMIT: &str = "ROSETTA3_QUARANTINE_FREE";

    /// Held, and never freed. A `Vec` rather than a set: addresses repeat, and
    /// holding the same block twice is harmless here in a way that deduplicating
    /// would not be - the count is the budget, not the contents.
    static HELD: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    fn limit() -> usize {
        static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *LIMIT.get_or_init(|| {
            std::env::var(ENV_LIMIT)
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_LIMIT)
        })
    }

    /// Withhold `block`. The pointer is only stored, never dereferenced, so this
    /// is safe whatever the guest thinks it is doing.
    pub(super) fn defer(block: *mut std::ffi::c_void) {
        if block.is_null() {
            return;
        }
        let Ok(mut held) = HELD.lock() else {
            return;
        };
        if held.len() >= limit() {
            return;
        }
        held.push(block as usize);
    }
}

/// Bindings that name a symbol some *other* binding had interposed, and were left
/// pointing at the host anyway.
fn missed(
    report: &LinkReport,
    moved: &BTreeSet<String>,
    interposers: &BTreeSet<u64>,
) -> Vec<String> {
    let guest: Vec<(u64, u64)> = report
        .images
        .iter()
        .flat_map(|i| &i.segments)
        .map(|s| (s.vmaddr, s.vmaddr.wrapping_add(s.vmsize)))
        .collect();
    let in_guest = |addr: u64| guest.iter().any(|(lo, hi)| addr >= *lo && addr < *hi);

    let mut out = Vec::new();
    for image in &report.images {
        if interposers.contains(&image.base) {
            continue;
        }
        for binding in &image.bindings {
            if !moved.contains(&binding.symbol) || in_guest(binding.resolved) {
                continue;
            }
            out.push(format!(
                "{} in {} still resolves to {:#x}, outside the guest - \
                 this program has two allocators",
                binding.symbol,
                image.name.rsplit('/').next().unwrap_or(&image.name),
                binding.resolved
            ));
        }
    }
    out
}

/// One line per interposing image, for the inspect transcript.
pub fn describe(report: &LinkReport) -> Vec<String> {
    report
        .images
        .iter()
        .filter(|i| !i.interpositions.is_empty())
        .map(|i| {
            format!(
                "{} interposes {} function(s)",
                i.name,
                i.interpositions.len()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use machish::fixups::BindKind;
    use machish::memory::{AddressSpace, BufferSpace, Placement};
    use machish::report::{BindingRecord, ImageReport, Interposition};

    use super::*;

    const HOST_FREE: u64 = 0xaaaa_0000;
    const GUEST_FREE: u64 = 0xbbbb_0000;

    fn image(name: &str, base: u64, slot: Option<u64>) -> ImageReport {
        ImageReport {
            name: name.to_string(),
            base,
            bindings: slot
                .map(|slot_va| BindingRecord {
                    slot_va,
                    stub_va: None,
                    library: None,
                    symbol: "_free".to_string(),
                    resolved: HOST_FREE,
                    kind: BindKind::NonLazy,
                    weak_import: false,
                    tag: 0,
                })
                .into_iter()
                .collect(),
            ..ImageReport::default()
        }
    }

    #[test]
    fn a_loaded_image_gets_the_programs_interpositions() {
        let mut memory = BufferSpace::new(0x10_0000);
        let slots = memory.reserve(0x1000, 0x1000, Placement::Anywhere).unwrap();

        let mut allocator = image("libmimalloc.dylib", 0x1000, None);
        allocator.interpositions.push(Interposition {
            replacement: GUEST_FREE,
            replacee: HOST_FREE,
        });
        let mut program = LinkReport {
            images: vec![image("Program", 0x2000, Some(slots)), allocator],
        };
        apply(&mut program, &mut memory).unwrap();

        let mut plugin = LinkReport {
            images: vec![image("libplugin.dylib", 0x3000, Some(slots + 8))],
        };
        assert_eq!(apply_to_loaded(&mut plugin, &mut memory).unwrap(), 1);
        assert_eq!(plugin.images[0].bindings[0].resolved, GUEST_FREE);
        let mut slot = [0u8; 8];
        memory.read(slots + 8, &mut slot).unwrap();
        assert_eq!(u64::from_le_bytes(slot), GUEST_FREE);
    }
}
