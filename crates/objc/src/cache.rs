use std::sync::OnceLock;

/// The Mach-O spelling, verbatim - the two leading underscores are part of the
/// name and adding a third would be a different symbol that binds to nothing.
///
/// `exports!`'s `data` section is used verbatim for the same reason: this is not
/// a C identifier, so there is no bare spelling to derive a Mach-O one from.
pub const EMPTY_CACHE: &str = "__objc_empty_cache";

/// One page, which is both the size of the mapping and the unit the answer is
/// measured in. See [`empty_cache`].
const PAGE: usize = 4096;

/// The address a guest's `__objc_empty_cache` import binds to: one writable page,
/// for the whole process.
///
/// # What the import is
///
/// A guest's `struct objc_class` is five words on this ABI - `isa`, `superclass`,
/// two cache words, and the `class_data_bits_t` - and `machish`'s Objective-C
/// decoder says exactly that when it sizes the structure. The compiler emits
/// `__objc_empty_cache` as the value of the *third* word of every class it
/// declares, once per class and once per metaclass, and the loader binds each
/// of those import slots to this address.
///
/// Nothing about the import is a call: the binding report lists these slots with
/// no stub beside them, the way every other data import is listed, and in the
/// Chromium Framework 342 of them name this one symbol - more than any other
/// name in the file. So this is a value the guest dereferences, and it is the
/// only data import in a real binary that cannot be given a stand-in.
///
/// # Why it has to be writable
///
/// The third word is a *method cache*, and filling a method cache is a store
/// through the pointer: a bucket, a selector and the `IMP` it resolved to.
/// Nothing reads the page and asks a question of it, which is exactly why the
/// `ROSETTA3_FILL_UNBOUND_DATA` escape hatch is wrong for this name - that page
/// is mapped `PROT_READ` on purpose, and pointing a read-only page at a field
/// the runtime writes turns one missing symbol into a fault on the first store
/// instead of a fault on the first load, with the same absence of a name
/// attached. `is_framework_constant` refuses a `__`-prefixed symbol for the same
/// reason, and correctly. Bound to zero, the first class in the process whose
/// cache is filled stores through null.
///
/// # Why one page, and not one per class
///
/// There is one binding per **symbol**, not per slot. All 342 slots resolve to
/// this one address, and nothing chosen here can make the seventh class point
/// somewhere the eighth does not - so a second page would not give any class a
/// cache of its own, only move the first collision further along. The symbol is
/// singular and *empty*: it names one object that every class starts out
/// pointing at, and a guest that tests its class's cache pointer for "still the
/// empty one" is only getting a meaningful answer while all the classes share
/// it. One page is what the ABI describes, not a compromise on it.
///
/// # Why the shared page is safe
///
/// Nothing in this tree ever *reads* it. This runtime's own `Class` has no cache
/// field at all - `class::Class` keeps its methods in a `HashMap` behind an
/// `RwLock` and resolves by selector - so a class this runtime built has nothing
/// to fill and nothing to look up, and the page is a write target and nothing
/// more. A colliding or stale entry in it is therefore invisible from here; the
/// only consumer of a class cache is a class structure in a guest's own
/// `__objc_data`.
///
/// What can tear is one guest thread's store landing in the middle of another's,
/// and that is a race between two stores into the *same* bucket. The guest's
/// runtime has that race on macOS too, against Apple's own copy of the symbol, so
/// no page size would have removed it - and giving each class a private page is
/// not available, as above. Two threads filling *different* buckets write
/// disjoint, naturally aligned words and cannot tear each other, and a page is
/// 256 sixteen-byte buckets with a fill storing whole buckets, so one class's
/// fill has somewhere to land.
///
/// # Why `mmap` and not a `static`
///
/// Two reasons, both about what Rust is entitled to assume. A `static` array
/// lives in `.bss` as `&[u8; N]`, and a reference to it aliases stores the
/// guest makes behind Rust's back - a soundness hole the compiler may act on
/// even though nothing in this tree ever touches the bytes. And `.bss` does not
/// lay out individual objects page-aligned, which a page-sized object handed to
/// another runtime's cache ought to be. An anonymous mapping has no Rust owner
/// at all: it is a raw address, and no reference to it is ever formed.
///
/// `MAP_PRIVATE` rather than `MAP_SHARED` because there is no file behind it for
/// the distinction to be about; every thread in the process sees the same
/// writable mapping either way. Never `munmap`ed, for the reason every address
/// handed to a guest outlives the guest.
pub fn empty_cache() -> u64 {
    static AT: OnceLock<u64> = OnceLock::new();
    *AT.get_or_init(|| {
        // SAFETY: an anonymous private mapping of one page, readable and
        // writable, and never unmapped - the address is published to every
        // `__objc_empty_cache` slot in the guest and unmapping it would turn
        // each of them into a fault.
        let at = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                PAGE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if at == libc::MAP_FAILED {
            // Not zero, and not a fallback: the whole point of this page is that
            // the import stops being zero. A runtime that cannot give the guest
            // somewhere to fill a cache cannot run it, and it says so here
            // rather than four hundred slots later as a fault at address nil.
            crate::fail!("objc: no writable page for {}", EMPTY_CACHE);
        }
        at as usize as u64
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word no zeroed page holds, so a store that went nowhere reads as zero.
    const MARKER: u64 = 0x0bad_c0de_dead_f00d;

    /// A bucket, which is the unit a cache fill writes: a selector and an `IMP`.
    const BUCKET: usize = 16;

    /// The one test this module exists for.
    ///
    /// Non-zero *and* writable, in that order of importance: a page that is
    /// merely present still faults the store, and a page that is merely
    /// readable still faults it too. The store is put back so a test running
    /// beside this one sees the page as a runtime would hand it over - zeroed.
    #[test]
    fn a_store_through_the_published_address_lands_and_reads_back() {
        let at = empty_cache();
        assert_ne!(at, 0, "an unbound data import is a store through nil");
        assert_eq!(
            at as usize % PAGE,
            0,
            "a cache the guest may align to a page should start on one"
        );
        // SAFETY: `at` is this module's own mapping, `PAGE` bytes of it are
        // mapped, and it is the first eight of them. The mapping is never
        // unmapped, so the pointer outlives the store and the load.
        unsafe {
            let page = at as *mut u64;
            assert_eq!(*page, 0, "the page is handed over empty");
            *page = MARKER;
            assert_eq!(*page, MARKER, "a cache fill has to land");
            *page = 0;
        }
    }

    /// Every slot binds to the same address, which is the property the whole
    /// design rests on: `OnceLock`, not a fresh mapping per ask. If this ever
    /// regressed, two classes in the same process would not be able to agree on
    /// what "not realized yet" means.
    #[test]
    fn the_same_page_answers_every_ask() {
        assert_eq!(empty_cache(), empty_cache());
    }

    /// The page is shared, so it has to survive being written from several
    /// threads at once - which is what a program does when two classes are first
    /// messaged on two cores.
    ///
    /// One thread per bucket, and each reads back its own: disjoint, naturally
    /// aligned words, so this is the case that has to be clean. Two threads
    /// filling the *same* bucket is the guest's own race and is deliberately not
    /// tested, because a test that lost would be reporting a property the ABI
    /// cannot give.
    #[test]
    fn buckets_filled_from_several_threads_all_survive() {
        const WRITERS: usize = 8;
        // The address goes across as a `usize`, not as a pointer. A raw pointer
        // is not `Send`, and that refusal is this module's whole design stated
        // as a type error: the page has no Rust owner, so there is nothing to
        // hand a thread but a number.
        let at = empty_cache() as usize;
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                scope.spawn(move || {
                    // SAFETY: `at` is the one page [`empty_cache`] mapped, still
                    // mapped, and `WRITERS` buckets of `BUCKET` bytes are inside
                    // it. Each thread owns its bucket outright, so no two stores
                    // alias.
                    unsafe {
                        let bucket = (at as *mut u64).add(writer * BUCKET / size_of::<u64>());
                        bucket.write(MARKER ^ writer as u64);
                        assert_eq!(bucket.read(), MARKER ^ writer as u64);
                        bucket.write(0);
                    }
                });
            }
        });
    }

    /// The page has to be able to hold a whole bucket array, and buckets are two
    /// words each. A page that could not is too small to be a cache.
    #[test]
    fn a_page_holds_a_bucket_array() {
        const _: () = assert!(
            PAGE.is_multiple_of(BUCKET),
            "buckets must not straddle the end"
        );
        const _: () = assert!(
            PAGE / BUCKET >= 128,
            "a cache narrower than this is not one"
        );
    }
}
