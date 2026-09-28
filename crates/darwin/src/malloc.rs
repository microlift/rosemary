#![cfg(not(target_os = "macos"))]

use std::ffi::{c_char, c_void};
use std::sync::RwLock;

use crate::mach::kern::{KERN_INVALID_ADDRESS, KERN_INVALID_TASK, KERN_NO_SPACE, KERN_SUCCESS};
use crate::mach::space::{Name, TASK_SELF};

/// Darwin's `malloc_statistics_t`. Four fields, and the layout is ABI: a guest
/// reads `size_in_use` out of a structure it allocated on its own stack.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Statistics {
    pub blocks_in_use: u32,
    _pad: u32,
    pub size_in_use: usize,
    pub max_size_in_use: usize,
    pub size_allocated: usize,
}

const _: () = {
    assert!(size_of::<Statistics>() == 32);
    assert!(std::mem::offset_of!(Statistics, size_in_use) == 8);
};

/// Darwin's `malloc_zone_t`, laid out exactly as `<malloc/malloc.h>` declares
/// it, at version 13.
///
/// **Every offset is ABI and not a choice**, because a zone is not opaque to a
/// guest. Chromium is the proof, and each offset below is one it was seen to
/// use in its own binary:
///
/// - its `AllocatorShimOverrideAppleDefaultZone` constructor builds its own
///   zone at `__DATA,0xe7341a0` in Chromium Framework and writes `size` at
///   `+0x10`, `malloc` `+0x18`, `calloc` `+0x20`, `valloc` `+0x28`, `free`
///   `+0x30`, `realloc` `+0x38`, `destroy` `+0x40`, the name `+0x48`,
///   `batch_malloc` `+0x50`, `batch_free` `+0x58`, `introspect` `+0x60`,
///   `version = 13` at `+0x68`, `memalign` `+0x70`, `free_definite_size`
///   `+0x78`, a null `pressure_relief` `+0x80`, `claimed_address` `+0x88` and
///   `try_free_default` `+0x90` (Framework `0x5decf3a..0x5ded101`);
/// - it walks every zone `malloc_get_all_zones` returns and `strcmp`s the word
///   at `+0x48` against `"PartitionAlloc"`;
/// - `StoreZoneFunctions` (Framework `0x5ded7a0`) copies `malloc`, `calloc`,
///   `valloc`, `free`, `realloc`, `size`, `batch_*`, and `introspect->good_size`
///   out of the default and purgeable zones, reads `version` to decide which of
///   `memalign`, `free_definite_size`, `claimed_address` and `try_free_default`
///   exist, and **traps unless every one it copies is non-null**.
///
/// The first version of this type was seven function pointers in an invented
/// order; the second added a name at `+0x48` but kept the invented order, so
/// `zone->malloc` read `realloc` and `zone->introspect` read 16 bytes past the
/// end of the object. The integer `0x17` that a neighbouring static happened
/// to hold there is the fault address both of those versions crashed on.
///
/// The zones this crate owns live in one writable page (see [`page`]) rather
/// than in `static`s, because a guest writes into them: Chromium's
/// `InterceptAllocationsMac` reads each zone's page protection with
/// `vm_region_64`, makes it writable if it is not, and stores its own function
/// pointers over the originals - as it must on Darwin, where the default zone
/// is a structure libmalloc owns and Chromium patches. A `static` of function
/// pointers is in RELRO, read-only after relocation, and the guest's store
/// would fault.
#[repr(C)]
pub struct Zone {
    /// `reserved1` and `reserved2`, which `<malloc/malloc.h>` reserves for
    /// CoreFoundation's own use. Null, which is what CoreFoundation finds in
    /// every zone it did not make.
    reserved: [*mut c_void; 2],
    /// `size_t (*size)(zone, const void *ptr)`
    pub size: unsafe extern "C" fn(*mut c_void, *const c_void) -> usize,
    /// `void *(*malloc)(zone, size_t)`
    pub malloc: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void,
    /// `void *(*calloc)(zone, size_t, size_t)`
    pub calloc: unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void,
    /// `void *(*valloc)(zone, size_t)`
    pub valloc: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void,
    /// `void (*free)(zone, void *)`
    pub free: unsafe extern "C" fn(*mut c_void, *mut c_void),
    /// `void *(*realloc)(zone, void *, size_t)`
    pub realloc: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void,
    /// `void (*destroy)(zone)`
    pub destroy: unsafe extern "C" fn(*mut c_void),
    /// `const char *zone_name`, at **offset 0x48** - the field Chromium's
    /// constructor reads to ask whether PartitionAlloc is already installed.
    pub name: *const c_char,
    /// `unsigned (*batch_malloc)(zone, size_t, void **results, unsigned n)`
    pub batch_malloc: unsafe extern "C" fn(*mut c_void, usize, *mut *mut c_void, u32) -> u32,
    /// `void (*batch_free)(zone, void **to_be_freed, unsigned n)`
    pub batch_free: unsafe extern "C" fn(*mut c_void, *mut *mut c_void, u32),
    /// `struct malloc_introspection_t *introspect`
    pub introspect: *const Introspection,
    /// `unsigned version`: which of the slots below exist.
    pub version: u32,
    /// `void *(*memalign)(zone, size_t alignment, size_t size)` - version 5.
    pub memalign: unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void,
    /// `void (*free_definite_size)(zone, void *, size_t)` - version 6.
    pub free_definite_size: unsafe extern "C" fn(*mut c_void, *mut c_void, usize),
    /// `size_t (*pressure_relief)(zone, size_t goal)` - version 8.
    pub pressure_relief: unsafe extern "C" fn(*mut c_void, usize) -> usize,
    /// `boolean_t (*claimed_address)(zone, void *)` - version 10.
    pub claimed_address: unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32,
    /// `void (*try_free_default)(zone, void *)` - version 13.
    pub try_free_default: unsafe extern "C" fn(*mut c_void, *mut c_void),
}

/// The version every zone this crate makes declares: the one whose slots end
/// at `try_free_default`, which is also the version Chromium gives its own.
/// A higher one would promise `malloc_with_options` and the typed-malloc slots
/// past `+0x98`, which this structure does not have.
const VERSION: u32 = 13;

const _: () = {
    use std::mem::offset_of;
    // Chromium's own offsets, one by one - see the type's documentation.
    assert!(offset_of!(Zone, size) == 0x10);
    assert!(offset_of!(Zone, malloc) == 0x18);
    assert!(offset_of!(Zone, calloc) == 0x20);
    assert!(offset_of!(Zone, valloc) == 0x28);
    assert!(offset_of!(Zone, free) == 0x30);
    assert!(offset_of!(Zone, realloc) == 0x38);
    assert!(offset_of!(Zone, destroy) == 0x40);
    assert!(offset_of!(Zone, name) == 0x48);
    assert!(offset_of!(Zone, batch_malloc) == 0x50);
    assert!(offset_of!(Zone, batch_free) == 0x58);
    assert!(offset_of!(Zone, introspect) == 0x60);
    assert!(offset_of!(Zone, version) == 0x68);
    assert!(offset_of!(Zone, memalign) == 0x70);
    assert!(offset_of!(Zone, free_definite_size) == 0x78);
    assert!(offset_of!(Zone, pressure_relief) == 0x80);
    assert!(offset_of!(Zone, claimed_address) == 0x88);
    assert!(offset_of!(Zone, try_free_default) == 0x90);
    assert!(size_of::<Zone>() == 0x98);
};

/// `memory_reader_t`: `int (*)(task_t, vm_address_t, vm_size_t, void *,
/// vm_size_t)`, the callback Darwin reads a *foreign* task's memory through.
///
/// A parameter here rather than a type alias used nowhere: it is the shape of
/// the argument the guest hands over, and saying so is the only place the
/// choice not to call it is visible.
pub type MemoryReader = unsafe extern "C" fn(Name, u64, u64, *mut c_void, u64) -> i32;

/// Darwin's `malloc_introspection_t`, through `task_statistics` (version 12 of
/// the introspection table; `zone_type` came later and is not read by anything
/// that reads a version-13 zone's table).
///
/// Chromium reads `good_size` out of it in `StoreZoneFunctions` and traps if it
/// is null; its own table, at Framework `__DATA,0xe734238`, fills all sixteen
/// slots, which is the layout asserted below. Every slot here is filled too,
/// for the reason every zone slot is.
#[repr(C)]
pub struct Introspection {
    /// `kern_return_t (*enumerator)(task_t, void *, unsigned type_mask,
    /// vm_address_t zone, memory_reader_t, vm_range_recorder_t)`
    pub enumerator:
        unsafe extern "C" fn(Name, *mut c_void, u32, u64, Option<MemoryReader>, *mut c_void) -> i32,
    /// `size_t (*good_size)(zone, size_t)`
    pub good_size: unsafe extern "C" fn(*mut c_void, usize) -> usize,
    /// `boolean_t (*check)(zone)`
    pub check: unsafe extern "C" fn(*mut c_void) -> i32,
    /// `void (*print)(zone, boolean_t verbose)`
    pub print: unsafe extern "C" fn(*mut c_void, i32),
    /// `void (*log)(zone, void *address)`
    pub log: unsafe extern "C" fn(*mut c_void, *mut c_void),
    /// `void (*force_lock)(zone)`
    pub force_lock: unsafe extern "C" fn(*mut c_void),
    /// `void (*force_unlock)(zone)`
    pub force_unlock: unsafe extern "C" fn(*mut c_void),
    /// `void (*statistics)(zone, malloc_statistics_t *)`
    pub statistics: unsafe extern "C" fn(*mut c_void, *mut Statistics),
    /// `boolean_t (*zone_locked)(zone)`
    pub zone_locked: unsafe extern "C" fn(*mut c_void) -> i32,
    /// `boolean_t (*enable_discharge_checking)(zone)`
    pub enable_discharge_checking: unsafe extern "C" fn(*mut c_void) -> i32,
    /// `void (*disable_discharge_checking)(zone)`
    pub disable_discharge_checking: unsafe extern "C" fn(*mut c_void),
    /// `void (*discharge)(zone, void *memory)`
    pub discharge: unsafe extern "C" fn(*mut c_void, *mut c_void),
    /// `void (*enumerate_discharged_pointers)(zone, void (^)(void *, void *))`
    pub enumerate_discharged_pointers: unsafe extern "C" fn(*mut c_void, *mut c_void),
    /// `void (*reinit_lock)(zone)`
    pub reinit_lock: unsafe extern "C" fn(*mut c_void),
    /// `void (*print_task)(task_t, unsigned level, vm_address_t zone,
    /// memory_reader_t, print_task_printer_t)`
    pub print_task: unsafe extern "C" fn(Name, u32, u64, Option<MemoryReader>, *mut c_void),
    /// `void (*task_statistics)(task_t, vm_address_t zone, memory_reader_t,
    /// malloc_statistics_t *)`
    pub task_statistics: unsafe extern "C" fn(Name, u64, Option<MemoryReader>, *mut Statistics),
}

const _: () = {
    use std::mem::offset_of;
    assert!(offset_of!(Introspection, good_size) == 0x08);
    assert!(offset_of!(Introspection, statistics) == 0x38);
    assert!(offset_of!(Introspection, zone_locked) == 0x40);
    assert!(offset_of!(Introspection, reinit_lock) == 0x68);
    assert!(offset_of!(Introspection, task_statistics) == 0x78);
    assert!(size_of::<Introspection>() == 0x80);
};

/// The slots of every zone this crate makes.
///
/// They route to [`crate::heap`] rather than to `libc::malloc`, and that is the
/// whole point of them: a block a zone allocated has to be freeable by the
/// plain `free` the guest's own image bound, and the two are the same allocator
/// only when the allocation goes through the indirection that adopts the
/// guest's. Calling `libc::malloc` here would answer a zone's `malloc` from one
/// allocator and its `free` from another whenever a guest allocator is
/// registered.
///
/// `zone` is ignored in all of them, which is correct: every zone this crate
/// makes is a view of the one allocator, so which of them a guest reached is
/// not a question the answer depends on.
///
/// # Safety
///
/// Every one of these is a zone slot, so the caller is guest code invoking the
/// pointer it read out of a [`Zone`]. The obligation is the C prototype's: a
/// size or a count for the allocating slots, and - for the freeing and
/// resizing ones - a block this allocator returned and that is not freed twice.
mod vtable {
    use std::ffi::c_void;

    use super::{Introspection, MemoryReader, Statistics};
    use crate::mach::space::{Name, TASK_SELF};

    /// The alignment every block [`crate::heap`] returns already has: glibc's
    /// and musl's `MALLOC_ALIGNMENT` on a 64-bit host, and Darwin's quantum.
    const QUANTUM: usize = 16;

    /// `zone->malloc`. # Safety: as this module.
    pub(super) unsafe extern "C" fn malloc(_zone: *mut c_void, size: usize) -> *mut c_void {
        // SAFETY: the caller's size, and a fresh uninitialized block or null.
        unsafe { crate::heap::malloc(size) }
    }

    /// `zone->calloc`. # Safety: as this module.
    ///
    /// The two counts are multiplied, and a product that does not fit is null
    /// rather than a wrapped-around block - which is what `calloc` is for.
    pub(super) unsafe extern "C" fn calloc(
        _zone: *mut c_void,
        num: usize,
        size: usize,
    ) -> *mut c_void {
        let Some(bytes) = num.checked_mul(size) else {
            return std::ptr::null_mut();
        };
        // SAFETY: the caller's size, and a fresh uninitialized block or null.
        let block = unsafe { crate::heap::malloc(bytes) };
        if !block.is_null() {
            // SAFETY: `heap::malloc` returned `bytes` writable bytes, or null,
            // which the branch has already excluded.
            unsafe { std::ptr::write_bytes(block, 0, bytes) };
        }
        block
    }

    /// An aligned block, or null where this allocator cannot give one.
    ///
    /// Only the host's own allocator can: `posix_memalign`'s block is one its
    /// `free` takes. A guest allocator adopted through [`crate::heap`] has no
    /// aligned entry point this crate can reach, and over-allocating and
    /// offsetting would hand out a pointer that allocator's `free` does not
    /// recognise - so that case is the null a failed allocation is, rather
    /// than a block that faults when freed.
    fn aligned(alignment: usize, size: usize) -> *mut c_void {
        if alignment <= QUANTUM {
            // SAFETY: the caller's size; every block is already this aligned.
            return unsafe { crate::heap::malloc(size) };
        }
        if crate::heap::is_registered() || !alignment.is_power_of_two() {
            return std::ptr::null_mut();
        }
        let mut block = std::ptr::null_mut();
        // SAFETY: a writable out-pointer; `alignment` is a power of two at
        // least the size of a pointer.
        match unsafe { libc::posix_memalign(&mut block, alignment, size) } {
            0 => block,
            _ => std::ptr::null_mut(),
        }
    }

    /// `zone->valloc`: page-aligned. # Safety: as this module.
    pub(super) unsafe extern "C" fn valloc(_zone: *mut c_void, size: usize) -> *mut c_void {
        // SAFETY: always sound.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        aligned(page, size)
    }

    /// `zone->memalign`. # Safety: as this module.
    pub(super) unsafe extern "C" fn memalign(
        _zone: *mut c_void,
        alignment: usize,
        size: usize,
    ) -> *mut c_void {
        aligned(alignment, size)
    }

    /// `zone->realloc`. # Safety: as this module.
    ///
    /// There is no `realloc` in [`crate::heap`] to delegate to, so this is
    /// allocate, copy, free. How much to copy is the one number a resize needs
    /// and cannot get, and it is taken from [`crate::malloc::malloc_size`] - the
    /// same answer this tree already publishes for every other "how big is this
    /// block" question, with the same caveat, which `malloc_size` states in its
    /// own contract.
    pub(super) unsafe extern "C" fn realloc(
        _zone: *mut c_void,
        ptr: *mut c_void,
        size: usize,
    ) -> *mut c_void {
        if ptr.is_null() {
            // SAFETY: as [`malloc`], with the caller's size.
            return unsafe { malloc(std::ptr::null_mut(), size) };
        }
        if size == 0 {
            // SAFETY: a block this allocator returned, freed once, and C's rule
            // that a zero-size `realloc` frees and returns null.
            unsafe { free(std::ptr::null_mut(), ptr) };
            return std::ptr::null_mut();
        }
        // SAFETY: the caller's size, and a fresh uninitialized block or null.
        let grown = unsafe { malloc(std::ptr::null_mut(), size) };
        if grown.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: the caller states `ptr` is this allocator's own block and the
        // two allocations are distinct, so they do not overlap.
        unsafe {
            let live = crate::malloc::malloc_size(ptr);
            std::ptr::copy_nonoverlapping(ptr.cast::<u8>(), grown.cast::<u8>(), size.min(live));
            free(std::ptr::null_mut(), ptr);
        }
        grown
    }

    /// `zone->free`. # Safety: as this module.
    pub(super) unsafe extern "C" fn free(_zone: *mut c_void, ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }
        // SAFETY: a block [`crate::heap::malloc`] returned.
        unsafe { crate::heap::free(ptr) }
    }

    /// `zone->free_definite_size`: `free`, with a size this allocator does not
    /// need. # Safety: as this module.
    pub(super) unsafe extern "C" fn free_definite_size(
        zone: *mut c_void,
        ptr: *mut c_void,
        _size: usize,
    ) {
        // SAFETY: the caller's block.
        unsafe { free(zone, ptr) }
    }

    /// `zone->try_free_default`: free a block that the zone it was handed to
    /// passed on because it was not its own. Every zone this crate makes is the
    /// one allocator, so the default zone's block is freed here as any other.
    /// # Safety: as this module.
    pub(super) unsafe extern "C" fn try_free_default(zone: *mut c_void, ptr: *mut c_void) {
        // SAFETY: the caller's block.
        unsafe { free(zone, ptr) }
    }

    /// `zone->size`. # Safety: as this module.
    ///
    /// The usable size of a block this allocator returned, which is
    /// [`crate::malloc::malloc_size`] with the zone argument the ABI makes it
    /// take.
    pub(super) unsafe extern "C" fn size(_zone: *mut c_void, ptr: *const c_void) -> usize {
        // SAFETY: the caller's block, which is what `malloc_size` asks about.
        unsafe { crate::malloc::malloc_size(ptr) }
    }

    /// `zone->destroy`. # Safety: as this module.
    ///
    /// Nothing is released. Darwin frees every block a destroyed zone ever
    /// allocated; here those blocks belong to the one allocator every zone
    /// shares and stay valid, which is a leak where Darwin would not leak, and
    /// never a block freed under a caller that still holds it. Unregistering
    /// is `malloc_destroy_zone`'s job, not this slot's, as it is on Darwin.
    pub(super) unsafe extern "C" fn destroy(_zone: *mut c_void) {}

    /// `zone->batch_malloc`: up to `count` blocks of `size`, and how many.
    /// # Safety: as this module; `results` has room for `count` pointers.
    pub(super) unsafe extern "C" fn batch_malloc(
        zone: *mut c_void,
        size: usize,
        results: *mut *mut c_void,
        count: u32,
    ) -> u32 {
        let mut made = 0;
        while made < count {
            // SAFETY: the caller's size.
            let block = unsafe { malloc(zone, size) };
            if block.is_null() {
                break;
            }
            // SAFETY: `results` has room for `count` pointers.
            unsafe { results.add(made as usize).write(block) };
            made += 1;
        }
        made
    }

    /// `zone->batch_free`. # Safety: as this module; `blocks` holds `count`
    /// pointers, each null or this allocator's.
    pub(super) unsafe extern "C" fn batch_free(
        zone: *mut c_void,
        blocks: *mut *mut c_void,
        count: u32,
    ) {
        for i in 0..count as usize {
            // SAFETY: the caller's array of `count` blocks.
            unsafe { free(zone, blocks.add(i).read()) };
        }
    }

    /// `zone->pressure_relief`: bytes released. There is nothing to purge from
    /// a block this allocator produced, so none. # Safety: as this module.
    pub(super) unsafe extern "C" fn pressure_relief(_zone: *mut c_void, _goal: usize) -> usize {
        0
    }

    /// `zone->claimed_address`. # Safety: as this module.
    ///
    /// True for any non-null address. `<malloc/malloc.h>` allows a false
    /// positive and forbids a false negative, and this allocator cannot tell
    /// its blocks from anyone else's - so "it may be mine" is the only answer
    /// that is never wrong in the direction that matters.
    pub(super) unsafe extern "C" fn claimed_address(_zone: *mut c_void, ptr: *mut c_void) -> i32 {
        i32::from(!ptr.is_null())
    }

    // The introspection table.

    /// `enumerator`: `KERN_FAILURE`. The host allocator does not enumerate its
    /// blocks, and "this zone has none" - a success with no ranges - would be
    /// a heap-inspection tool's wrong answer rather than its missing one.
    unsafe extern "C" fn enumerator(
        _task: Name,
        _context: *mut c_void,
        _type_mask: u32,
        _zone: u64,
        _reader: Option<MemoryReader>,
        _recorder: *mut c_void,
    ) -> i32 {
        crate::mach::kern::KERN_FAILURE
    }

    /// `good_size`: the size a request of `size` really gets, which is at least
    /// `size` and a whole number of quanta.
    unsafe extern "C" fn good_size(_zone: *mut c_void, size: usize) -> usize {
        size.max(1).next_multiple_of(QUANTUM)
    }

    /// `check`: true. There is no zone-private state here to be inconsistent.
    unsafe extern "C" fn check(_zone: *mut c_void) -> i32 {
        1
    }

    /// `print`, `log`, `discharge`, `disable_discharge_checking`,
    /// `enumerate_discharged_pointers`: nothing to report and nothing tracked.
    unsafe extern "C" fn print(_zone: *mut c_void, _verbose: i32) {}
    unsafe extern "C" fn log(_zone: *mut c_void, _address: *mut c_void) {}
    unsafe extern "C" fn discharge(_zone: *mut c_void, _memory: *mut c_void) {}
    unsafe extern "C" fn disable_discharge_checking(_zone: *mut c_void) {}
    unsafe extern "C" fn enumerate_discharged_pointers(_zone: *mut c_void, _block: *mut c_void) {}

    /// `force_lock`, `force_unlock`, `reinit_lock`: the `fork` protocol. The
    /// host allocator takes and resets its own locks around `fork`, so there is
    /// no zone lock for these to hold.
    unsafe extern "C" fn force_lock(_zone: *mut c_void) {}
    unsafe extern "C" fn force_unlock(_zone: *mut c_void) {}
    unsafe extern "C" fn reinit_lock(_zone: *mut c_void) {}

    /// `zone_locked`: false, as there is no zone lock - see `force_lock`.
    unsafe extern "C" fn zone_locked(_zone: *mut c_void) -> i32 {
        0
    }

    /// `enable_discharge_checking`: false, which is how a zone says it does
    /// not support it.
    unsafe extern "C" fn enable_discharge_checking(_zone: *mut c_void) -> i32 {
        0
    }

    /// `statistics`: [`super::malloc_zone_statistics`].
    unsafe extern "C" fn statistics(zone: *mut c_void, stats: *mut Statistics) {
        // SAFETY: the caller's writable statistics.
        unsafe { super::malloc_zone_statistics(zone, stats) }
    }

    /// `print_task`: nothing, as `print`.
    unsafe extern "C" fn print_task(
        _task: Name,
        _level: u32,
        _zone: u64,
        _reader: Option<MemoryReader>,
        _printer: *mut c_void,
    ) {
    }

    /// `task_statistics`: this task's, as `statistics`; zeroes for another,
    /// which this process cannot read.
    unsafe extern "C" fn task_statistics(
        task: Name,
        _zone: u64,
        _reader: Option<MemoryReader>,
        stats: *mut Statistics,
    ) {
        if stats.is_null() {
            return;
        }
        if task == TASK_SELF {
            // SAFETY: the caller's writable statistics.
            unsafe { super::malloc_zone_statistics(std::ptr::null_mut(), stats) };
        } else {
            // SAFETY: as above.
            unsafe { stats.write(Statistics::default()) };
        }
    }

    pub(super) const INTROSPECTION: Introspection = Introspection {
        enumerator,
        good_size,
        check,
        print,
        log,
        force_lock,
        force_unlock,
        statistics,
        zone_locked,
        enable_discharge_checking,
        disable_discharge_checking,
        discharge,
        enumerate_discharged_pointers,
        reinit_lock,
        print_task,
        task_statistics,
    };
}

impl Zone {
    /// A zone of this crate's, answering through [`vtable`], named `name`.
    fn ours(name: *const c_char, introspect: *const Introspection) -> Zone {
        Zone {
            reserved: [std::ptr::null_mut(); 2],
            size: vtable::size,
            malloc: vtable::malloc,
            calloc: vtable::calloc,
            valloc: vtable::valloc,
            free: vtable::free,
            realloc: vtable::realloc,
            destroy: vtable::destroy,
            name,
            batch_malloc: vtable::batch_malloc,
            batch_free: vtable::batch_free,
            introspect,
            version: VERSION,
            memalign: vtable::memalign,
            free_definite_size: vtable::free_definite_size,
            pressure_relief: vtable::pressure_relief,
            claimed_address: vtable::claimed_address,
            try_free_default: vtable::try_free_default,
        }
    }
}

/// Where the default zone, the purgeable zone and their introspection table
/// sit inside [`page`]. Each is 16-byte aligned and the three do not overlap.
const DEFAULT_AT: usize = 0x000;
const PURGEABLE_AT: usize = 0x100;
const INTROSPECTION_AT: usize = 0x200;

const _: () = {
    assert!(DEFAULT_AT + size_of::<Zone>() <= PURGEABLE_AT);
    assert!(PURGEABLE_AT + size_of::<Zone>() <= INTROSPECTION_AT);
    assert!(INTROSPECTION_AT + size_of::<Introspection>() <= 4096);
};

/// The one writable, page-aligned page the default and purgeable zones live in,
/// filled in on first use.
///
/// An anonymous mapping rather than a `static` or a heap block, for three
/// reasons. It must be writable - see [`Zone`]. It must be **a page of its
/// own**, because a guest that finds it read-only makes the whole page
/// writable and back, and doing that to a page shared with Rust statics or
/// heap blocks would briefly change their protection under code that did not
/// ask. And a mapping has no Rust owner, so the guest's stores into it alias
/// nothing Rust holds a reference to; the address crosses threads as a
/// `usize`.
///
/// The names point at `&'static CStr` literals: `AppleDefaultZone` and
/// `ApplePurgeableZone`, the names Darwin gives them. The name is not
/// decoration - a guest asking "is PartitionAlloc already installed?" reads
/// it, and is entitled to be told "no, this is the system's zone".
fn page() -> usize {
    static PAGE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: a fresh anonymous mapping, owned by nothing else.
        let page = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(
            page != libc::MAP_FAILED,
            "no page for the default malloc zones"
        );
        let page = page as usize;
        let introspect = (page + INTROSPECTION_AT) as *mut Introspection;
        // SAFETY: three non-overlapping, aligned objects inside a writable page
        // that nothing else has seen yet.
        unsafe {
            introspect.write(vtable::INTROSPECTION);
            ((page + DEFAULT_AT) as *mut Zone)
                .write(Zone::ours(c"AppleDefaultZone".as_ptr(), introspect));
            ((page + PURGEABLE_AT) as *mut Zone)
                .write(Zone::ours(c"ApplePurgeableZone".as_ptr(), introspect));
        }
        page
    })
}

/// `malloc_zone_t *malloc_default_zone()`
///
/// The zone a plain `malloc` belongs to, and this tree's own rather than the
/// host's.
///
/// It used to be answered by passing the name through to the host, on the
/// reasoning that a guest asking for the default zone wants *this process's*
/// zone, whose vtable is a different structure. That reasoning holds on macOS
/// and nowhere else: off Darwin the name resolves to nothing, the import goes
/// unbound, and a guest's call jumps through a zero slot. Chromium's
/// `AllocatorShimOverrideAppleDefaultZone` constructor calls this immediately
/// after [`malloc_get_all_zones`], so it is not a name it can do without.
///
/// Answering with this crate's zone is the honest answer on any host: it is the
/// zone whose `malloc` this tree's `malloc` is, because both go through
/// [`crate::heap`]. Handing a guest the *host's* zone instead would be worse
/// than useless - a block from one allocator and a `free` to another.
pub extern "C" fn malloc_default_zone() -> *mut c_void {
    (page() + DEFAULT_AT) as *mut c_void
}

/// `malloc_zone_t *malloc_default_purgeable_zone()`
///
/// The same zone every time, not null, and not the default zone: a guest that
/// has asked for purgeable memory can only know it has a zone of its own by
/// asking and comparing addresses, so an alias of the default zone would make
/// every use of this name a lie.
pub extern "C" fn malloc_default_purgeable_zone() -> *mut c_void {
    (page() + PURGEABLE_AT) as *mut c_void
}

/// Whether `zone` is one this crate made, whose slots it filled in.
fn is_ours(zone: *mut c_void) -> bool {
    let at = zone as usize;
    at == page() + DEFAULT_AT
        || at == page() + PURGEABLE_AT
        || created()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&at)
}

/// The zones [`malloc_create_zone`] made, which are this crate's as much as the
/// two in [`page`] are.
fn created() -> &'static RwLock<Vec<usize>> {
    static CREATED: std::sync::OnceLock<RwLock<Vec<usize>>> = std::sync::OnceLock::new();
    CREATED.get_or_init(|| RwLock::new(Vec::new()))
}

/// The zones a guest has handed [`malloc_zone_register`], as addresses.
///
/// The zone's *contents* are never read from here, and that is the invariant
/// this registry exists to keep.
///
/// The zones in existence, oldest registration first.
///
/// A zone a guest registers is a structure of **the guest's** function
/// pointers, and under emulation those are addresses in another instruction
/// set - a call to one from host code is a jump into the middle of nothing. So
/// the registry records *that a zone exists* and never what it does. The
/// `malloc_zone_*` calls are the one place a zone's slots are called, because
/// dispatching through them is what those calls are; [`callable`] is where
/// that is allowed only when the guest's code is this host's.
///
/// A raw address rather than a `*mut Zone` for that reason too: a pointer to
/// the structure would invite exactly the load that starts the whole problem,
/// and the value is only ever compared and handed back to the guest that
/// registered it.
///
/// **The two zones this crate owns are in here from the start**, rather than
/// being prepended by whoever answers a query. That is not a detail: a guest
/// that installs its own zone unregisters the ones it displaces, and a list
/// that put them back on every call would report the zone it had just removed -
/// so the delegating zone would never install, and the caller would abort on a
/// list that plainly contained its zone.
///
/// The order is **oldest first**, which is what makes registration mean
/// anything: a caller that registers a zone in order to make allocations go
/// through it reads `zones[0]` to confirm it won, and appending is what puts a
/// newly registered zone ahead of the ones registered before it.
fn registry() -> &'static RwLock<Vec<usize>> {
    static ZONES: std::sync::OnceLock<RwLock<Vec<usize>>> = std::sync::OnceLock::new();
    ZONES.get_or_init(|| {
        RwLock::new(vec![
            malloc_default_zone() as usize,
            malloc_default_purgeable_zone() as usize,
        ])
    })
}

/// `void malloc_zone_register(malloc_zone_t *)`
///
/// Records `zone` at the end of the list, so that [`malloc_get_all_zones`]
/// reports it and a later [`malloc_zone_unregister`] can find it. **The vtable
/// inside is not called, now or later**: see [`registry`].
///
/// Not `unsafe` because nothing here dereferences the pointer, which is the
/// same reason this call can be honest on a host that shares the guest's
/// architecture. Darwin faults on a null zone; treating one as a no-op is the
/// behaviour of a function that had nothing to record anyway.
pub extern "C" fn malloc_zone_register(zone: *mut c_void) {
    if zone.is_null() {
        return;
    }
    let at = zone as usize;
    let mut zones = registry().write().unwrap_or_else(|e| e.into_inner());
    // Re-registering moves a zone to the end rather than adding a second copy of
    // it: a guest that registers on every thread start would otherwise grow
    // this list without bound, and "most recently registered" would stop meaning
    // anything.
    zones.retain(|held| *held != at);
    zones.push(at);
}

/// `void malloc_zone_unregister(malloc_zone_t *)`
///
/// Removes `zone` from the list. A zone that was never registered, and a null
/// one, are both no-ops: the mistake this call catches is a zone being torn
/// down twice, and the second teardown is not the one that should fault.
pub extern "C" fn malloc_zone_unregister(zone: *mut c_void) {
    if zone.is_null() {
        return;
    }
    let at = zone as usize;
    let mut zones = registry().write().unwrap_or_else(|e| e.into_inner());
    zones.retain(|held| *held != at);
}

/// Every zone in existence, in the order [`registry`] holds them.
///
/// The copy is deliberate: the lock is released before the caller's block is
/// allocated and written, and the set is small enough that a registration racing
/// this read costs one more entry in the next call's list.
fn all_zones() -> Vec<u64> {
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|at| *at as u64)
        .collect()
}

/// `kern_return_t malloc_get_all_zones(task_t, memory_reader_t, vm_address_t **,
/// unsigned *)`
///
/// **The array is the callee's to allocate.** `zones` is a `vm_address_t **` and
/// goes in holding whatever the caller had - in practice a null pointer it has
/// not looked at - and comes out holding a fresh array the caller owns and
/// frees. Filling a buffer the caller had sized would be a different function
/// with a different name, and answering this one that way leaves `*zones` null
/// for a caller that goes straight on to dereference it.
///
/// The array comes from [`crate::heap`] for the reason every other block handed
/// to a guest here does: the caller frees it with whichever `free` its own image
/// bound, and that is the same allocator only if the allocation went through the
/// indirection that adopts the guest's.
///
/// ## The order is a contract
///
/// **Registration order, oldest first**, with the two zones this crate owns
/// already at the front before any registration. A caller that registers a zone
/// in order to make it the one allocations go through - which is the whole
/// reason to call this - reads `zones[0]` back to confirm it won, so a
/// prepended list would report the winner last. [`REGISTERED`] is appended to
/// for exactly this reason.
///
/// ## What is refused, and why it is not a stub
///
/// Only `TASK_SELF`. This runtime hosts one process, so a foreign task is a
/// capability it does not have, and `KERN_INVALID_TASK` - the same answer
/// `crate::mach::info` gives for a task other than this one - is the truth
/// rather than an empty list that reads as "that task has no zones".
///
/// ## Why `reader` is never called
///
/// Darwin needs the callback because the `zones` pointer is an address *in the
/// target's* address space, and reading it needs that task's `mach_vm_read`. For
/// `TASK_SELF` the target's address space *is* this one, so the caller's pointer
/// is used directly - which is what Darwin's own fast path does. For any other
/// task this call refuses, so there is never a moment where calling a guest
/// function pointer from host code would be the only way to answer, and it is
/// never done: the same invariant [`malloc_zone_register`] keeps, and for the
/// same reason.
///
/// # Safety
///
/// `zones` is a writable `vm_address_t *` and `zone_count` a writable
/// `unsigned`. On success `*zones` is a block from [`crate::heap`] that the
/// caller owns and must free.
pub unsafe extern "C" fn malloc_get_all_zones(
    task: Name,
    _reader: Option<MemoryReader>,
    zones: *mut *mut u64,
    zone_count: *mut u32,
) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    if zones.is_null() || zone_count.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    let all = all_zones();
    // SAFETY: a plain allocation of a computed length, null-checked below. Never
    // zero bytes: a caller that dereferences `*zones` has to be able to.
    let array = unsafe { crate::heap::malloc((all.len() * size_of::<u64>()).max(1)) }.cast::<u64>();
    if array.is_null() {
        // Nothing handed over, and the count says nothing was: a caller that
        // allocated from the count on a previous call learns it must retry.
        // SAFETY: the caller's `unsigned`, writable.
        unsafe { *zone_count = 0 };
        return KERN_NO_SPACE;
    }
    // SAFETY: `array` is `all.len()` writable `u64`s - it is non-null and was
    // allocated for exactly that many - and the two do not overlap.
    unsafe { std::ptr::copy_nonoverlapping(all.as_ptr(), array, all.len()) };
    // SAFETY: the caller's `vm_address_t *` slot and its `unsigned`.
    unsafe {
        *zones = array;
        *zone_count = all.len() as u32;
    }
    KERN_SUCCESS
}

/// libsystem_malloc's assertion reporter, and the one variadic in this file.
///
/// C-variadic is a shape Rust cannot write, and the tail is the whole of what
/// this function has to forward: the guest's format string names a `%s` and a
/// `%#016zx` that mean nothing without the arguments behind them. So this is
/// [`crate::printf_l`]'s arrangement again - a `va_list` built in the callee's
/// own frame by a hand-written trampoline, handed to the host's `vsnprintf` -
/// rather than a `va_list` bridge of this crate's own.
///
/// x86-64 only, as that arrangement is: the register save area, the offsets
/// inside it and the `al` convention are all SysV AMD64's. On another
/// architecture this name is not published, which is where it started - unbound
/// rather than bound to something that would read the wrong registers.
#[cfg(target_arch = "x86_64")]
mod abort_report {
    use std::ffi::{c_char, c_int, c_void};

    /// The SysV `va_list`, which is one of these and not a pointer.
    ///
    /// `#[repr(C)]` because the trampoline writes it by offset and glibc reads
    /// it by offset; the two have to agree on all twenty-four bytes. `Copy` for
    /// the same reason `va_copy` exists: the walk consumes it, so a second pass
    /// over the same arguments needs its own copy.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VaList {
        gp_offset: u32,
        fp_offset: u32,
        overflow_arg_area: *mut c_void,
        reg_save_area: *mut c_void,
    }

    unsafe extern "C" {
        fn vsnprintf(
            buffer: *mut c_char,
            size: usize,
            format: *const c_char,
            ap: *mut VaList,
        ) -> c_int;
    }

    /// How the message is rendered before the first heap fallback. Long enough
    /// for every assertion in libsystem_malloc, and a truncation is not
    /// something a crash report should be able to cause.
    const FIRST_TRY: usize = 512;

    /// `abort_report_np`'s arguments, as the trampoline leaves them: the format
    /// and a `va_list` describing the rest of this frame.
    ///
    /// # Safety
    ///
    /// The trampoline's: `format` is the caller's NUL-terminated string and `ap`
    /// is a `va_list` built for this frame by [`trampoline`]. Both are read, and
    /// nothing is written through either.
    unsafe extern "C" fn report(format: *const c_char, ap: *mut VaList) -> ! {
        let message = match (format.is_null(), ap.is_null()) {
            // SAFETY: the caller's format and a `va_list` for this frame, which
            // is what [`report`] documents and what the trampoline built.
            (false, false) => unsafe { render(format, ap) },
            // A caller with no format still gets a line, because a bare
            // "libsystem_malloc asserted" is more use than silence.
            _ => b"libsystem_malloc: an assertion with no format\n".to_vec(),
        };
        write_stderr(&message);
        // SAFETY: ends the process, which is what this call is for. The write
        // above is `write(2)` rather than stdio, so the message does not sit in
        // a buffer `abort` would not flush.
        unsafe { libc::abort() }
    }

    /// `format` rendered against the tail, or empty if it could not be read.
    ///
    /// # Safety
    ///
    /// As [`report`].
    unsafe fn render(format: *const c_char, ap: *mut VaList) -> Vec<u8> {
        let mut stack = [0u8; FIRST_TRY];
        // SAFETY: [`report`]'s, and a `va_copy`: the call consumes the list it
        // is given, so a second pass needs one of its own.
        let mut first: VaList = unsafe { *ap };
        // SAFETY: the caller's format and a `va_list` for this frame, into a
        // buffer of exactly `FIRST_TRY` bytes.
        let needed =
            unsafe { vsnprintf(stack.as_mut_ptr().cast(), FIRST_TRY, format, &raw mut first) };
        if needed < 0 {
            return Vec::new();
        }
        let needed = needed as usize;
        if needed < FIRST_TRY {
            return stack[..needed].to_vec();
        }
        // Longer than the stack attempt: the message is kept anyway, because a
        // report that stops mid-sentence at the point of the failure is the one
        // thing worse than no report.
        let Some(room) = needed.checked_add(1) else {
            return Vec::new();
        };
        // SAFETY: a plain allocation of `room` bytes, checked for null below.
        let heap = unsafe { libc::malloc(room) }.cast::<u8>();
        if heap.is_null() {
            return Vec::new();
        }
        // SAFETY: [`report`]'s, and the second `va_copy`.
        let mut again: VaList = unsafe { *ap };
        // SAFETY: the caller's format, a list for this frame, and a buffer of
        // exactly the `room` bytes the sizing pass asked for.
        let written = unsafe { vsnprintf(heap.cast(), room, format, &raw mut again) };
        let out = match written {
            w if w < 0 => Vec::new(),
            w => {
                // SAFETY: `vsnprintf` wrote `w + 1` bytes of `room` at most,
                // all of which it initialized.
                unsafe { std::slice::from_raw_parts(heap, (w as usize).min(room - 1)) }.to_vec()
            }
        };
        // SAFETY: the `malloc` above, not freed twice.
        unsafe { libc::free(heap.cast()) };
        out
    }

    /// Write `bytes` to the process's standard error, as the report is.
    ///
    /// `write(2)` and not a `FILE *`, for two reasons: this is about to abort,
    /// and an `abort` does not flush; and a guest's `stderr` is a host stream
    /// only by the arrangement `crate::stdio` documents, while descriptor 2 is
    /// the same file either way.
    fn write_stderr(bytes: &[u8]) {
        let mut at = 0;
        while at < bytes.len() {
            // SAFETY: `bytes` is live for the length passed, and `write` only
            // reads it.
            let written = unsafe { libc::write(2, bytes[at..].as_ptr().cast(), bytes.len() - at) };
            if written > 0 {
                at += written as usize;
                continue;
            }
            // SAFETY: reads the thread's last `errno`, which the call above set.
            if written < 0 && unsafe { *libc::__errno_location() } == libc::EINTR {
                continue;
            }
            // A full pipe, or a closed descriptor: there is nowhere left to
            // report to, and the abort below is the part that matters.
            break;
        }
    }

    // One named integer argument - the format in `rdi` - so the tail starts at
    // `rsi`, which is `gp_offset` of 8, and the list itself is the fifth
    // argument register.
    core::arch::global_asm!(
        ".text",
        ".globl rosetta3_abort_report_np",
        ".hidden rosetta3_abort_report_np",
        ".type rosetta3_abort_report_np, @function",
        "rosetta3_abort_report_np:",
        "push rbp",
        "mov rbp, rsp",
        // 176 bytes of register save area, then the 24-byte `va_list`, rounded
        // up to keep the stack sixteen-aligned for the call.
        "sub rsp, 208",
        "mov [rsp], rdi",
        "mov [rsp + 8], rsi",
        "mov [rsp + 16], rdx",
        "mov [rsp + 24], rcx",
        "mov [rsp + 32], r8",
        "mov [rsp + 40], r9",
        "movaps [rsp + 48], xmm0",
        "movaps [rsp + 64], xmm1",
        "movaps [rsp + 80], xmm2",
        "movaps [rsp + 96], xmm3",
        "movaps [rsp + 112], xmm4",
        "movaps [rsp + 128], xmm5",
        "movaps [rsp + 144], xmm6",
        "movaps [rsp + 160], xmm7",
        "mov dword ptr [rsp + 176], 8",
        "mov dword ptr [rsp + 180], 48",
        "lea rax, [rbp + 16]",
        "mov [rsp + 184], rax",
        "mov rax, rsp",
        "mov [rsp + 192], rax",
        "lea rsi, [rsp + 176]",
        "call {body}",
        "mov rsp, rbp",
        "pop rbp",
        "ret",
        ".size rosetta3_abort_report_np, . - rosetta3_abort_report_np",
        body = sym report,
    );

    unsafe extern "C" {
        /// The trampoline, which calls [`report`] and so does not come back.
        fn rosetta3_abort_report_np() -> !;
    }

    /// The trampoline's address, for the [`exports!`] below - which sits outside
    /// this module, and which has to name a symbol the assembler produced.
    pub(super) const TRAMPOLINE: unsafe extern "C" fn() -> ! = rosetta3_abort_report_np;
}

/// The address in `zone`'s slot at `offset`, read as a word.
///
/// A word rather than a typed field, because the zone may be a guest's and the
/// guest may have left the slot null, which a Rust function-pointer type cannot
/// hold.
///
/// # Safety
///
/// `zone` is a live `malloc_zone_t` at least `offset + 8` bytes long.
unsafe fn slot(zone: *mut c_void, offset: usize) -> usize {
    // SAFETY: the caller's contract.
    unsafe { zone.cast::<u8>().add(offset).cast::<usize>().read() }
}

/// Make `slot` - the value in `zone`'s slot named `what` - callable, or stop
/// the process by name.
///
/// Darwin's `malloc_zone_malloc` and the rest are one-line dispatches through
/// the zone's own slots, and a guest relies on exactly that: Chromium stores
/// its own function pointers into the default zone and expects the zone's
/// callers to reach them, and a block its slot allocated must be freed by its
/// slot. So these dispatch the same way, whoever filled the slot.
///
/// The one refusal is the invariant [`registry`] exists to keep. A slot a
/// guest filled is guest code, and calling it from here is only a call when
/// the guest's instructions are this host's - the native backend. Under
/// emulation it is an address in another instruction set, so unless the slot
/// still holds this crate's own function the call is refused by name, not made.
fn callable(zone: *mut c_void, slot: usize, ours: usize, what: &str) -> usize {
    if slot == 0 {
        abort_naming(zone, what, "is null");
    }
    if slot != ours && !crate::sysctl::runs_host_instructions() {
        abort_naming(zone, what, "is guest code this host cannot call");
    }
    slot
}

fn abort_naming(zone: *mut c_void, what: &str, why: &str) -> ! {
    let line = format!("rosetta3: malloc zone {zone:p}: its {what} slot {why}\n");
    // SAFETY: a live buffer; a short write loses the message, not the abort.
    unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
    // SAFETY: always sound; it does not return.
    unsafe { libc::abort() }
}

/// `void *malloc_zone_malloc(malloc_zone_t *zone, size_t size)`
///
/// # Safety
///
/// `zone` is a live zone: one of this crate's or one a guest registered.
pub unsafe extern "C" fn malloc_zone_malloc(zone: *mut c_void, size: usize) -> *mut c_void {
    // SAFETY: the caller's zone; `+0x18` is `malloc` in every version.
    let f = callable(
        zone,
        unsafe { slot(zone, 0x18) },
        vtable::malloc as *const () as usize,
        "malloc",
    );
    // SAFETY: a zone's `malloc` has this type.
    let f: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void =
        unsafe { std::mem::transmute(f) };
    // SAFETY: the zone's own slot, with its own zone and the caller's size.
    unsafe { f(zone, size) }
}

/// `void *malloc_zone_calloc(malloc_zone_t *zone, size_t n, size_t size)`
///
/// # Safety
///
/// As [`malloc_zone_malloc`].
pub unsafe extern "C" fn malloc_zone_calloc(
    zone: *mut c_void,
    count: usize,
    size: usize,
) -> *mut c_void {
    // SAFETY: the caller's zone; `+0x20` is `calloc` in every version.
    let f = callable(
        zone,
        unsafe { slot(zone, 0x20) },
        vtable::calloc as *const () as usize,
        "calloc",
    );
    // SAFETY: a zone's `calloc` has this type.
    let f: unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void =
        unsafe { std::mem::transmute(f) };
    // SAFETY: as [`malloc_zone_malloc`].
    unsafe { f(zone, count, size) }
}

/// `void *malloc_zone_valloc(malloc_zone_t *zone, size_t size)`
///
/// # Safety
///
/// As [`malloc_zone_malloc`].
pub unsafe extern "C" fn malloc_zone_valloc(zone: *mut c_void, size: usize) -> *mut c_void {
    // SAFETY: the caller's zone; `+0x28` is `valloc` in every version.
    let f = callable(
        zone,
        unsafe { slot(zone, 0x28) },
        vtable::valloc as *const () as usize,
        "valloc",
    );
    // SAFETY: a zone's `valloc` has this type.
    let f: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void =
        unsafe { std::mem::transmute(f) };
    // SAFETY: as [`malloc_zone_malloc`].
    unsafe { f(zone, size) }
}

/// `void malloc_zone_free(malloc_zone_t *zone, void *ptr)`
///
/// # Safety
///
/// As [`malloc_zone_malloc`], and `ptr` is null or a block `zone` allocated.
pub unsafe extern "C" fn malloc_zone_free(zone: *mut c_void, ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: the caller's zone; `+0x30` is `free` in every version.
    let f = callable(
        zone,
        unsafe { slot(zone, 0x30) },
        vtable::free as *const () as usize,
        "free",
    );
    // SAFETY: a zone's `free` has this type.
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void) = unsafe { std::mem::transmute(f) };
    // SAFETY: as [`malloc_zone_malloc`], with the caller's block.
    unsafe { f(zone, ptr) }
}

/// `void *malloc_zone_realloc(malloc_zone_t *zone, void *ptr, size_t size)`
///
/// # Safety
///
/// As [`malloc_zone_free`].
pub unsafe extern "C" fn malloc_zone_realloc(
    zone: *mut c_void,
    ptr: *mut c_void,
    size: usize,
) -> *mut c_void {
    // SAFETY: the caller's zone; `+0x38` is `realloc` in every version.
    let f = callable(
        zone,
        unsafe { slot(zone, 0x38) },
        vtable::realloc as *const () as usize,
        "realloc",
    );
    // SAFETY: a zone's `realloc` has this type.
    let f: unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void =
        unsafe { std::mem::transmute(f) };
    // SAFETY: as [`malloc_zone_free`].
    unsafe { f(zone, ptr, size) }
}

/// `void *malloc_zone_memalign(malloc_zone_t *zone, size_t alignment, size_t size)`
///
/// Null for a zone older than version 5, which has no `memalign` slot to call:
/// Darwin's answer too.
///
/// # Safety
///
/// As [`malloc_zone_malloc`].
pub unsafe extern "C" fn malloc_zone_memalign(
    zone: *mut c_void,
    alignment: usize,
    size: usize,
) -> *mut c_void {
    // SAFETY: the caller's zone; `version` is at `+0x68` in every version.
    let version = unsafe { zone.cast::<u8>().add(0x68).cast::<u32>().read() };
    if version < 5 {
        return std::ptr::null_mut();
    }
    // SAFETY: a version-5 zone has `memalign` at `+0x70`.
    let f = callable(
        zone,
        unsafe { slot(zone, 0x70) },
        vtable::memalign as *const () as usize,
        "memalign",
    );
    // SAFETY: a zone's `memalign` has this type.
    let f: unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void =
        unsafe { std::mem::transmute(f) };
    // SAFETY: as [`malloc_zone_malloc`].
    unsafe { f(zone, alignment, size) }
}

/// `malloc_zone_t *malloc_create_zone(vm_size_t start_size, unsigned flags)`
///
/// A new zone of this crate's, registered - Darwin's `malloc_create_zone`
/// registers what it creates - and unnamed until [`malloc_set_zone_name`]
/// names it. Both arguments are sizing hints for an allocator of its own that
/// this zone does not have: its blocks come from the one allocator every zone
/// here shares, which is what keeps them freeable by a plain `free`.
///
/// Leaked on purpose, as Darwin's are until `malloc_destroy_zone`: its address
/// is in the registry and in the guest's hands for the rest of the process.
pub extern "C" fn malloc_create_zone(_start_size: u64, _flags: u32) -> *mut c_void {
    let introspect = (page() + INTROSPECTION_AT) as *const Introspection;
    let zone = Box::into_raw(Box::new(Zone::ours(std::ptr::null(), introspect))).cast::<c_void>();
    created()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push(zone as usize);
    malloc_zone_register(zone);
    zone
}

/// `void malloc_set_zone_name(malloc_zone_t *zone, const char *name)`
///
/// Writes a copy of `name` - or null - into the zone's name at `+0x48`, which
/// is where every reader of a zone's name looks. Darwin makes the zone
/// writable first and so does this, for a zone that is not this crate's: the
/// structure is the guest's own data, and its name is a field of it rather than
/// code, so writing it is sound on either backend.
///
/// The previous name is not freed. Darwin frees it, on the assumption that it
/// was the copy an earlier call made; here a zone's first name is a string
/// literal, and a leaked copy per rename is a cost where a `free` of a literal
/// would be a fault.
///
/// # Safety
///
/// `zone` is a live zone; `name` is null or NUL-terminated.
pub unsafe extern "C" fn malloc_set_zone_name(zone: *mut c_void, name: *const c_char) {
    if zone.is_null() {
        return;
    }
    let copy = if name.is_null() {
        std::ptr::null()
    } else {
        // SAFETY: the caller's NUL-terminated string.
        let text = unsafe { std::ffi::CStr::from_ptr(name) };
        text.to_owned().into_raw().cast_const()
    };
    let field = zone.cast::<u8>().wrapping_add(0x48).cast::<*const c_char>();
    if !is_ours(zone) {
        // SAFETY: always sound: an `mprotect` of pages that are the guest's
        // own and mapped, since the zone it named is in them.
        unsafe {
            let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
            let start = (zone as usize) & !(page - 1);
            let end = (zone as usize + size_of::<Zone>()).next_multiple_of(page);
            libc::mprotect(
                start as *mut c_void,
                end - start,
                libc::PROT_READ | libc::PROT_WRITE,
            );
        }
    }
    // SAFETY: the zone's name field, writable - ours always are, and a guest's
    // was made so above.
    unsafe { field.write(copy) };
}

/// `void malloc_zone_statistics(malloc_zone_t *zone, malloc_statistics_t *stats)`
///
/// # Safety
///
/// `stats` is null or a writable Darwin `malloc_statistics_t`.
pub unsafe extern "C" fn malloc_zone_statistics(_zone: *mut c_void, stats: *mut Statistics) {
    if stats.is_null() {
        return;
    }
    #[cfg(target_env = "gnu")]
    {
        let info = unsafe { libc::mallinfo2() };
        // SAFETY: the caller states `stats` is writable.
        unsafe {
            *stats = Statistics {
                // glibc counts no blocks, and a count invented from the byte total
                // would be a number with no meaning. Zero says "not reported",
                // which is what a caller that divides by it needs to see.
                blocks_in_use: 0,
                _pad: 0,
                // `uordblks` counts only what came out of the heap. A block past
                // `M_MMAP_THRESHOLD` - which is most of a game's memory - is mapped
                // instead and counted in `hblkhd`, so reporting the first alone says
                // a program using two hundred megabytes is using sixteen kilobytes.
                size_in_use: info.uordblks + info.hblkhd,
                // The high-water mark is not kept either. Reporting the current
                // total would say the program has never freed anything.
                max_size_in_use: 0,
                size_allocated: info.arena + info.hblkhd,
            };
        }
    }
    #[cfg(not(target_env = "gnu"))]
    {
        unsafe {
            *stats = Statistics {
                blocks_in_use: 0,
                _pad: 0,
                size_in_use: 0,
                max_size_in_use: 0,
                size_allocated: 0,
            };
        }
    }
}

/// `size_t malloc_size(const void *ptr)`
///
/// Darwin answers the usable size of a block, and **zero for a pointer its
/// allocator does not own**. The second half is not a detail of the signature,
/// it is the question the function exists to answer safely: `malloc_zone_from_ptr`
/// returns NULL for a pointer in no registered malloc zone, and Apple's own
/// test for this function
/// (`compiler-rt/test/tsan/Darwin/malloc_size.mm`) asserts `size = 0x0` with
/// `zone = no zone` for a pointer that is a global and for one that came from
/// `mmap`. An mmap'd region and a guest arena are in that same class.
///
/// glibc and musl answer the first question as `malloc_usable_size` and do not
/// answer the second at all, so a pointer that is not a live block is undefined
/// here - and undefined is a **fault**, not a zero, on a host whose
/// `malloc_usable_size` is mimalloc's: with `MI_DEBUG=0` and not
/// `defined(__APPLE__)`, mimalloc's `internal.h:668` compiles
/// `_mi_unchecked_ptr_page`, so `_mi_page_map[p >> 29]` is dereferenced
/// without being tested. For a pointer in no heap that index is null and the
/// load faults at `0x00 + ((p >> 16) & 0x1fff) * 8`. Measured: `0x348` and
/// `0x350` on two runs, from `Framework+0x680e10`'s `_malloc_size`.
///
/// So the ownership question is asked first, through [`crate::heap`], and a
/// pointer that is not a block is answered 0 - **and named**, because a zero
/// nobody printed is the failure this project exists to prevent. Which is not
/// a substitute: it is the number Darwin returns for this pointer, and the
/// pointer in the measurement above was inside a 64 GiB `PROT_NONE` cage this
/// runtime mapped for the guest, not inside a heap.
///
/// # Safety
///
/// `ptr` is null, a live block of this host's allocator, or a pointer the
/// registered predicate answers `Some(false)` for. A pointer that is neither,
/// and that the predicate answers `Some(true)` for, is a freed block in a live
/// segment - which `malloc_usable_size` answers for, as the host's allocator
/// does, and which Darwin would answer for through its zone's `size` slot.
pub unsafe extern "C" fn malloc_size(ptr: *const c_void) -> usize {
    if ptr.is_null() {
        return 0;
    }
    if let Some(false) = crate::heap::owns(ptr) {
        // Not a block, so there is no size. Named and counted, then answered
        // with the zero Darwin gives.
        report_not_owned(ptr);
        return 0;
    }
    // SAFETY: the caller states `ptr` is a live block of this allocator, or
    // that no predicate is registered - in which case there is one allocator
    // and it is this host's, which is the case `malloc_usable_size` is for.
    unsafe { libc::malloc_usable_size(ptr.cast_mut()) }
}

/// Distinct foreign pointers [`report_not_owned`] has written down.
static NOT_OWNED: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// Every foreign pointer this run has been asked about, named or not.
static NOT_OWNED_TOTAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Whether the count above has been reported without the pointers it counts.
static NOT_OWNED_OVERFLOW_SAID: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// How many distinct foreign pointers are written down before the rest are only
/// counted. A bounded list and an unbounded count, because the count is what a
/// reader needs to trust and a list of ten thousand addresses is not a log
/// anyone reads.
const NOT_OWNED_NAMED: usize = 8;

/// Write `ptr` down, once, as a pointer this host's allocator does not own.
///
/// The alternative - handing it to `malloc_usable_size` and letting the
/// allocator decide what a null first-level page-map entry means - produces a
/// `SIGSEGV` with no name in it, which is the one outcome worse than a wrong
/// answer, because it says nothing at all.
///
/// Two guards, and each is for a real reason rather than tidiness:
///
/// - **One line per distinct address.** A guest asking about the same arena
///   block in a loop must not produce a log to end all logs.
/// - **The lock is dropped before the line is printed.** Printing allocates, and
///   holding a `Mutex` across an allocation in a function a guest can reach is
///   how a lock ends up held by a thread that then blocks on it. Two guest
///   threads asking at once serialise on the set and both get counted; neither
///   holds the lock while the other's event is in flight.
fn report_not_owned(ptr: *const c_void) {
    use std::sync::atomic::Ordering;

    NOT_OWNED_TOTAL.fetch_add(1, Ordering::Relaxed);
    let Ok(mut named) = NOT_OWNED.lock() else {
        // A poisoned lock means another thread panicked holding it, which means
        // this process is already in a state the guest will hear about. The
        // count above still went up, so the zero is accounted for.
        return;
    };
    let at = ptr as usize;
    if named.contains(&at) {
        return;
    }
    if named.len() < NOT_OWNED_NAMED {
        named.push(at);
        drop(named);
        eprintln!(
            "rosetta3: malloc_size: {at:#x} is not a block of this host's allocator, so it has \
             no size and Darwin's answer is 0 - malloc_zone_from_ptr answers NULL for a pointer \
             in no malloc zone on macOS, and a region this runtime mapped for the guest (a \
             vm_allocate arena, a guest-side pool) is in that class, not in a heap. Answered 0 \
             and named, because the same pointer handed to the host's malloc_usable_size faults \
             at {:#x} instead: with MI_DEBUG=0 and not __APPLE__, mimalloc tests no first-level \
             page-map entry and this load is from a null submap.",
            (at >> 16 & 0x1fff) * 8
        );
        return;
    }
    if !NOT_OWNED_OVERFLOW_SAID.swap(true, Ordering::Relaxed) {
        drop(named);
        eprintln!(
            "rosetta3: malloc_size: {NOT_OWNED_NAMED} distinct foreign pointers have been \
             answered 0 and written down individually, and {total} have now been answered 0 in \
             total - the addresses past those {named_at} are counted and not listed, and this \
             count is a running one, not a final total",
            total = NOT_OWNED_TOTAL.load(Ordering::Relaxed),
            named_at = NOT_OWNED_NAMED,
        );
    }
}

exports::exports! {
    functions {
        "malloc_size" => malloc_size,
        "malloc_zone_statistics" => malloc_zone_statistics,
        "malloc_default_purgeable_zone" => malloc_default_purgeable_zone,
        "malloc_default_zone" => malloc_default_zone,
        "malloc_zone_register" => malloc_zone_register,
        "malloc_zone_unregister" => malloc_zone_unregister,
        "malloc_get_all_zones" => malloc_get_all_zones,
        "malloc_zone_malloc" => malloc_zone_malloc,
        "malloc_zone_calloc" => malloc_zone_calloc,
        "malloc_zone_valloc" => malloc_zone_valloc,
        "malloc_zone_free" => malloc_zone_free,
        "malloc_zone_realloc" => malloc_zone_realloc,
        "malloc_zone_memalign" => malloc_zone_memalign,
        "malloc_create_zone" => malloc_create_zone,
        "malloc_set_zone_name" => malloc_set_zone_name,
        // The trampoline above, which is the only shape that can be this
        // symbol. Absent on an architecture whose `va_list` this file does not
        // build; see [`abort_report`].
        #[cfg(target_arch = "x86_64")]
        "abort_report_np" => abort_report::TRAMPOLINE,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::c_char;

    use super::*;

    /// The zone list is process-global state, so the tests that read or change
    /// it have to be one at a time.
    ///
    /// Not a lock the production code takes: this is a test harness detail, and
    /// a `Mutex` in [`registry`] would serialise a guest's registrations against
    /// each other for no reason a real process has. Each test that takes this
    /// puts the list back before it drops it.
    static ONE_ZONE_LIST_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A block's reported size covers what was asked for, and a null pointer
    /// owns nothing.
    #[test]
    fn reports_the_size_of_a_block() {
        // SAFETY: a block from this host's allocator, freed below.
        let block = unsafe { libc::malloc(4096) };
        assert!(!block.is_null());
        // SAFETY: `block` is live.
        assert!(unsafe { malloc_size(block) } >= 4096);
        // SAFETY: `block` came from `malloc` and is not used again.
        unsafe { libc::free(block) };
        // SAFETY: null is the documented no-block case.
        assert_eq!(unsafe { malloc_size(std::ptr::null()) }, 0);
    }

    /// A registered ownership predicate, and the previous registration restored
    /// when the guard drops.
    ///
    /// The registration is process-global - `crate::heap::register` writes three
    /// statics - so these tests take the same lock `heap`'s own do, and a test
    /// running beside one of them is not decided by it either way. The predicate
    /// answers about **one** address for the same reason.
    struct OneRefused(
        Option<(
            (usize, usize, usize),
            std::sync::MutexGuard<'static, ()>,
        )>,
    );

    impl OneRefused {
        /// A real, mapped, non-null address that is certainly not a block of
        /// anything: a leaked byte, so it is still mapped when the test has
        /// finished and is never dereferenced.
        ///
        /// **One address for the whole binary, not one per call.** Leaking a
        /// fresh byte each time would hand back a different address every call,
        /// and a predicate comparing against it would answer `true` to
        /// everything - which is a test that passes while checking nothing,
        /// which is the failure this exists to catch.
        fn foreign() -> *const c_void {
            static FOREIGN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
            *FOREIGN.get_or_init(|| Box::leak(Box::new(0u8)) as *const u8 as usize) as *const c_void
        }

        /// `bool(const void *)`: owned, except for [`OneRefused::foreign`].
        // SAFETY: the signature `crate::heap::register` documents.
        unsafe extern "C" fn owns(p: *const c_void) -> bool {
            p != OneRefused::foreign()
        }

        /// Install that predicate. `0` for `owns` installs none at all.
        fn install(owns: usize) -> Self {
            let one_at_a_time = crate::heap::ONE_REGISTRATION_AT_A_TIME
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let was = crate::heap::registration();
            crate::heap::register(
                libc::malloc as *const () as usize,
                libc::free as *const () as usize,
                owns,
            );
            Self(Some((was, one_at_a_time)))
        }
    }

    impl Drop for OneRefused {
        fn drop(&mut self) {
            if let Some((was, _lock)) = self.0.take() {
                crate::heap::register(was.0, was.1, was.2);
            }
        }
    }

    /// A pointer this host's allocator does not own is answered 0 - and a real
    /// block is still answered its real size, in the same process and under the
    /// same predicate.
    ///
    /// The second half is what stops the first from being a blanket zero, and
    /// the first is what a reader of a log can check: without it the address
    /// goes to `malloc_usable_size`, which on a host whose `malloc_usable_size`
    /// is mimalloc's faults inside the allocator rather than answering.
    #[test]
    fn a_pointer_the_allocator_does_not_own_has_no_size_and_a_real_block_still_does() {
        let _guard = OneRefused::install(OneRefused::owns as *const () as usize);
        // SAFETY: the predicate installed above says this is not a block, which
        // is the case the function now handles.
        assert_eq!(unsafe { malloc_size(OneRefused::foreign()) }, 0);
        // SAFETY: a block from this host's allocator, freed below.
        let block = unsafe { libc::malloc(4096) };
        assert!(!block.is_null());
        // SAFETY: `block` is live and the predicate says it is one.
        assert!(unsafe { malloc_size(block) } >= 4096);
        // SAFETY: `block` came from `malloc` and is not used again.
        unsafe { libc::free(block) };
    }

    /// Every zero [`malloc_size`] returns for a pointer nobody owns is counted,
    /// so a reader can tell "there was no such pointer" from "there were many".
    #[test]
    fn a_foreign_pointer_is_counted_as_well_as_answered() {
        use std::sync::atomic::Ordering;
        let _guard = OneRefused::install(OneRefused::owns as *const () as usize);
        let before = NOT_OWNED_TOTAL.load(Ordering::Relaxed);
        // SAFETY: the installed predicate says it is not a block.
        unsafe { malloc_size(OneRefused::foreign()) };
        assert_eq!(NOT_OWNED_TOTAL.load(Ordering::Relaxed), before + 1);
    }

    /// With no predicate registered the answer is still the host's own, which is
    /// the case [`crate::heap::owns`] calls `None` and must not be read as "no".
    #[test]
    fn with_no_ownership_predicate_the_hosts_own_answer_is_unchanged() {
        let _guard = OneRefused::install(0);
        // SAFETY: a block from this host's allocator, freed below.
        let block = unsafe { libc::malloc(2048) };
        // SAFETY: `block` is live, and no predicate was installed to say
        // otherwise.
        assert!(unsafe { malloc_size(block) } >= 2048);
        // SAFETY: `block` came from `malloc` and is not used again.
        unsafe { libc::free(block) };
    }

    /// The total is a real number and the fields nothing can answer are zero,
    /// which is the difference between "not reported" and "nothing allocated".
    #[test]
    fn the_total_in_use_is_reported() {
        // Two allocations, because glibc counts them in different fields: a
        // small one comes out of the heap and a large one is mapped, and a
        // report that missed either would still look plausible.
        // Through `black_box`, because in a release build an allocation whose
        // only use is its own length is one the optimiser deletes - and the test
        // then measures a program holding nothing.
        let small = std::hint::black_box(vec![0u8; 8 << 10]);
        let large = std::hint::black_box(vec![0u8; 4 << 20]);
        let mut stats = Statistics::default();
        // SAFETY: a writable structure of the declared layout.
        unsafe { malloc_zone_statistics(std::ptr::null_mut(), &mut stats) };
        let held = small.len() + large.len();
        assert!(
            stats.size_in_use >= held,
            "{} bytes in use with {held} held",
            stats.size_in_use,
        );
        assert!(stats.size_allocated >= stats.size_in_use);
        assert_eq!(stats.max_size_in_use, 0, "a high-water mark nothing keeps");
        drop(std::hint::black_box((small, large)));
    }

    /// The answer `malloc_default_purgeable_zone` gives is a zone: a real
    /// object, the same one every time, and not the default zone - which is the
    /// one thing a caller comparing the two is asking.
    #[test]
    fn the_purgeable_zone_is_a_real_zone_of_its_own() {
        let zone = malloc_default_purgeable_zone();
        assert!(!zone.is_null(), "a null zone is not a zone");
        assert_eq!(
            zone,
            malloc_default_purgeable_zone(),
            "the same zone every time, as on Darwin"
        );
        assert_ne!(
            zone,
            malloc_default_zone(),
            "the whole point of the name: separate from the default zone"
        );
        // And the size slot answers about a *block*, not about the zone, which
        // is what `malloc_size` with the zone argument dropped gives.
        // SAFETY: this crate's own zone, read at the offsets
        // `<malloc/malloc.h>` declares.
        let vtable = unsafe { &*zone.cast::<Zone>() };
        // SAFETY: a block this zone's `malloc` allocated, below.
        let block = unsafe { (vtable.malloc)(zone, 96) };
        assert!(!block.is_null());
        // SAFETY: `block` is live and was asked for as 96 bytes.
        assert!(unsafe { (vtable.size)(zone, block) } >= 96);
        // SAFETY: a block this zone allocated, freed once.
        unsafe { (vtable.free)(zone, block) };
    }

    /// The word at `offset` from `base`, read the way a guest reads it: at a
    /// fixed displacement, not through this crate's field names.
    fn word(base: *mut c_void, offset: usize) -> usize {
        // SAFETY: every caller passes one of this crate's zones or its
        // introspection table and an offset inside it.
        unsafe { base.cast::<u8>().add(offset).cast::<usize>().read() }
    }

    /// Exactly what Chromium's `StoreZoneFunctions` (Framework `0x5ded7a0`)
    /// does to the default and purgeable zones, and what it traps on.
    ///
    /// It copies `malloc` `+0x18`, `calloc` `+0x20`, `valloc` `+0x28`, `free`
    /// `+0x30`, `realloc` `+0x38`, `size` `+0x10` and `introspect->good_size`
    /// (`*(+0x60) + 8`), and `int3`s unless all seven are non-null; then
    /// `batch_malloc` `+0x50` and `batch_free` `+0x58`; then, by `version` at
    /// `+0x68`, `memalign` `+0x70` (5), `free_definite_size` `+0x78` (6),
    /// `claimed_address` `+0x88` (10) and `try_free_default` `+0x90` (13). The
    /// zone this replaced was 0x50 bytes long, so the `introspect` load read a
    /// neighbouring static's `0x17` and the `good_size` load faulted at `0x1f`.
    #[test]
    fn every_slot_chromium_copies_is_where_it_reads_it() {
        for zone in [malloc_default_zone(), malloc_default_purgeable_zone()] {
            for (offset, what) in [
                (0x10, "size"),
                (0x18, "malloc"),
                (0x20, "calloc"),
                (0x28, "valloc"),
                (0x30, "free"),
                (0x38, "realloc"),
                (0x40, "destroy"),
                (0x50, "batch_malloc"),
                (0x58, "batch_free"),
                (0x70, "memalign"),
                (0x78, "free_definite_size"),
                (0x80, "pressure_relief"),
                (0x88, "claimed_address"),
                (0x90, "try_free_default"),
            ] {
                assert_ne!(word(zone, offset), 0, "{what} at {offset:#x} is null");
            }
            // SAFETY: `version` is a 32-bit field at `+0x68`.
            let version = unsafe { zone.cast::<u8>().add(0x68).cast::<u32>().read() };
            assert_eq!(
                version, 13,
                "the version whose slots end at try_free_default"
            );
            let introspect = word(zone, 0x60) as *mut c_void;
            assert!(!introspect.is_null());
            // Every one of the sixteen slots Chromium's own table fills.
            for slot in 0..16 {
                assert_ne!(
                    word(introspect, slot * 8),
                    0,
                    "introspection slot {slot} is null"
                );
            }
            // And `good_size` answers at least what was asked, in whole quanta.
            // SAFETY: the introspection table's `good_size`.
            let good_size: unsafe extern "C" fn(*mut c_void, usize) -> usize =
                unsafe { std::mem::transmute(word(introspect, 8)) };
            // SAFETY: this crate's own slot.
            assert_eq!(unsafe { good_size(zone, 1) }, 16);
            // SAFETY: as above.
            assert_eq!(unsafe { good_size(zone, 17) }, 32);
        }
    }

    /// Chromium patches the default zone: `DeprotectMallocZone` asks
    /// `vm_region_64` for the page's protection, and writes its own function
    /// pointers over the zone's once the page is writable. A zone in a `static`
    /// would be in read-only RELRO; this one is in a writable page of its own,
    /// so the guest needs no `vm_protect` and changes no one else's page.
    #[test]
    fn the_default_zones_are_in_a_writable_page_of_their_own() {
        let zone = malloc_default_zone();
        let mut address = zone as u64;
        let mut size = 0_u64;
        let mut info = [0_i32; 10];
        let mut count = 10_u32;
        let mut object = 0;
        // SAFETY: writable out-parameters and a 40-byte info buffer, which is
        // `VM_REGION_BASIC_INFO_64` (flavour 9).
        let status = unsafe {
            crate::mach::vm::vm_region_64(
                TASK_SELF,
                &mut address,
                &mut size,
                9,
                info.as_mut_ptr().cast(),
                &mut count,
                &mut object,
            )
        };
        assert_eq!(status, KERN_SUCCESS);
        assert!(address <= zone as u64 && zone as u64 + 0x98 <= address + size);
        // `protection` is the first field; `VM_PROT_WRITE` is 2.
        assert_ne!(info[0] & 2, 0, "the guest must be able to write its slots");
        // Both zones and their table share the page, and nothing else does.
        assert_eq!(
            zone as usize & !4095,
            malloc_default_purgeable_zone() as usize & !4095
        );
        assert_eq!(word(zone, 0x60) & !4095, zone as usize & !4095);
    }

    /// A guest reading a zone's **name** at offset `0x48` gets a readable,
    /// NUL-terminated string - and not merely a readable word.
    ///
    /// This is the test for the defect that first faulted Chromium: its
    /// `AllocatorShimOverrideAppleDefaultZone` constructor walks the zones from
    /// `malloc_get_all_zones` and `strcmp`s the word at `+0x48` against
    /// `"PartitionAlloc"`. A zone too short to have that field handed it
    /// another Rust object's data, and `strcmp` faulted at `0x17`.
    #[test]
    fn a_zone_carries_a_name_where_a_guest_reads_one() {
        let names: Vec<_> = [malloc_default_zone(), malloc_default_purgeable_zone()]
            .into_iter()
            .map(|zone| {
                let name = word(zone, 0x48) as *const c_char;
                assert!(!name.is_null());
                // SAFETY: a zone name this crate published: a `&'static CStr`.
                unsafe { std::ffi::CStr::from_ptr(name) }.to_owned()
            })
            .collect();
        assert_eq!(names[0].to_str(), Ok("AppleDefaultZone"));
        assert_eq!(names[1].to_str(), Ok("ApplePurgeableZone"));
    }

    /// The `malloc_zone_*` calls dispatch through the zone's own slots, as
    /// Darwin's do, and a zone `malloc_create_zone` made is registered and can
    /// be renamed where a guest reads names.
    #[test]
    fn the_zone_calls_dispatch_through_the_zone() {
        let _serialised = ONE_ZONE_LIST_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let zone = malloc_create_zone(0, 0);
        assert!(
            all_zones().contains(&(zone as u64)),
            "created zones are registered"
        );
        assert_eq!(word(zone, 0x48), 0, "and unnamed until named");
        // SAFETY: a zone this crate made, and a NUL-terminated name.
        unsafe { malloc_set_zone_name(zone, c"MyZone".as_ptr()) };
        // SAFETY: the name just written.
        let name = unsafe { std::ffi::CStr::from_ptr(word(zone, 0x48) as *const c_char) };
        assert_eq!(name.to_str(), Ok("MyZone"));

        // SAFETY: this crate's zone throughout; every block is freed through it.
        unsafe {
            let block = malloc_zone_malloc(zone, 40);
            assert!(!block.is_null());
            block.cast::<u8>().write_bytes(0xa5, 40);
            let grown = malloc_zone_realloc(zone, block, 4000);
            assert!(!grown.is_null());
            assert!(
                std::slice::from_raw_parts(grown.cast::<u8>(), 40)
                    .iter()
                    .all(|b| *b == 0xa5)
            );
            malloc_zone_free(zone, grown);

            let zeroed = malloc_zone_calloc(zone, 4, 8);
            assert!(
                std::slice::from_raw_parts(zeroed.cast::<u8>(), 32)
                    .iter()
                    .all(|b| *b == 0)
            );
            malloc_zone_free(zone, zeroed);

            let paged = malloc_zone_valloc(zone, 10);
            assert_eq!(paged as usize % 4096, 0, "valloc is page-aligned");
            malloc_zone_free(zone, paged);
            let aligned = malloc_zone_memalign(zone, 256, 10);
            assert_eq!(aligned as usize % 256, 0);
            malloc_zone_free(zone, aligned);
            malloc_zone_free(zone, std::ptr::null_mut());
        }
        malloc_zone_unregister(zone);
    }

    const DISPATCH_CHILD: &str = "ROSETTA3_TEST_ZONE_DISPATCH";

    /// Re-run the calling test in a child with [`DISPATCH_CHILD`] set.
    fn in_child() -> Option<std::process::Output> {
        let exe = std::env::current_exe().ok()?;
        let name = std::thread::current().name()?.to_string();
        std::process::Command::new(exe)
            .args(["--exact", &name, "--test-threads=1"])
            .env(DISPATCH_CHILD, "1")
            .output()
            .ok()
    }

    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "C" fn counting_malloc(zone: *mut c_void, size: usize) -> *mut c_void {
        CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: this crate's own slot, with the caller's arguments.
        unsafe { vtable::malloc(zone, size) }
    }

    /// A slot a guest replaced is the one called - Chromium's interception
    /// depends on it - when the guest's code is this host's.
    #[test]
    fn a_replaced_slot_is_called_when_the_guest_runs_natively() {
        if std::env::var_os(DISPATCH_CHILD).is_some() {
            crate::sysctl::set_runs_host_instructions(true);
            let zone = malloc_create_zone(0, 0);
            // SAFETY: the `malloc` slot of a zone this test owns, written as a
            // guest writes it.
            unsafe {
                zone.cast::<u8>()
                    .add(0x18)
                    .cast::<usize>()
                    .write(counting_malloc as *const () as usize)
            };
            // SAFETY: a zone whose `malloc` slot allocates from this allocator.
            let block = unsafe { malloc_zone_malloc(zone, 8) };
            assert!(!block.is_null());
            assert_eq!(CALLS.load(std::sync::atomic::Ordering::Relaxed), 1);
            // SAFETY: the block just allocated, freed through the zone.
            unsafe { malloc_zone_free(zone, block) };
            return;
        }
        let Some(out) = in_child() else {
            eprintln!("skipping: cannot re-exec this test binary");
            return;
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Under emulation the same call is refused by name rather than made: a
    /// replaced slot is then an address in another instruction set.
    #[test]
    fn a_replaced_slot_is_refused_under_emulation() {
        if std::env::var_os(DISPATCH_CHILD).is_some() {
            crate::sysctl::set_runs_host_instructions(false);
            let zone = malloc_create_zone(0, 0);
            // SAFETY: as above.
            unsafe {
                zone.cast::<u8>()
                    .add(0x18)
                    .cast::<usize>()
                    .write(counting_malloc as *const () as usize)
            };
            // SAFETY: a live zone; this call aborts.
            unsafe { malloc_zone_malloc(zone, 8) };
            return;
        }
        let Some(out) = in_child() else {
            eprintln!("skipping: cannot re-exec this test binary");
            return;
        };
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(out.status.signal(), Some(libc::SIGABRT));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("its malloc slot is guest code"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A zone's `malloc` and the plain `free` the guest's own image bound are
    /// one allocator, which is the property a guest frees a zone's block on.
    #[test]
    fn a_block_allocated_through_a_zone_is_freeable_by_plain_free() {
        let zone = malloc_default_purgeable_zone();
        // SAFETY: a pointer to this crate's own `Zone` (see above); the block
        // it allocates here is freed through the plain `free` below.
        let vtable = unsafe { &*zone.cast::<Zone>() };
        // SAFETY: a size this test asks for, and the block is freed below.
        let block = unsafe { (vtable.malloc)(zone, 64) };
        assert!(!block.is_null());
        // SAFETY: the block has 64 writable bytes.
        unsafe { block.cast::<u8>().write(0x5a) };
        // SAFETY: a block [`crate::heap::malloc`] returned, freed once, and
        // dropped here without being touched again.
        unsafe { crate::heap::free(block) };
    }

    /// The same through `calloc`, which has to zero, and through a `realloc`
    /// that has to preserve the bytes it copied - the two slots a caller
    /// exercises that plain `malloc`/`free` never reaches.
    #[test]
    fn the_other_three_allocating_slots_do_what_they_say() {
        let zone = malloc_default_purgeable_zone();
        // SAFETY: a pointer to this crate's own `Zone`; every block allocated
        // through it below is freed through it too.
        let vtable = unsafe { &*zone.cast::<Zone>() };

        // SAFETY: 8 x 8 bytes, freed below.
        let zeroed = unsafe { (vtable.calloc)(zone, 8, 8) };
        assert!(!zeroed.is_null());
        // SAFETY: `zeroed` holds 64 writable bytes this call just had zeroed.
        assert!(
            unsafe { std::slice::from_raw_parts(zeroed.cast::<u8>(), 64) }
                .iter()
                .all(|byte| *byte == 0),
            "calloc returned memory that was not zeroed"
        );

        // SAFETY: `zeroed` is this vtable's own block of 64 bytes.
        let grown = unsafe { (vtable.realloc)(zone, zeroed, 256) };
        assert!(
            !grown.is_null(),
            "a resize that fails is a lie about memory"
        );
        // SAFETY: the first 64 bytes of `grown` are the copy of `zeroed`.
        assert!(
            unsafe { std::slice::from_raw_parts(grown.cast::<u8>(), 64) }
                .iter()
                .all(|byte| *byte == 0),
            "a resize dropped the bytes it was asked to keep"
        );
        // SAFETY: a block this vtable allocated, freed once.
        unsafe { (vtable.free)(zone, grown) };

        // A null pointer is `malloc`'s null, and a zero size frees.
        // SAFETY: the null-pointer case `realloc` is defined for.
        let fresh = unsafe { (vtable.realloc)(zone, std::ptr::null_mut(), 32) };
        assert!(!fresh.is_null());
        // SAFETY: a block this vtable allocated, freed once.
        unsafe { (vtable.free)(zone, fresh) };
    }

    /// Register and unregister are a pair, and both ends tolerate being wrong:
    /// a zone that was never registered, and a null one.
    ///
    /// The zone is leaked rather than freed, because the registry outlives the
    /// test that put an address in it: a freed block's address could be handed
    /// to the next allocation and make a stale entry look current.
    #[test]
    fn registering_and_unregistering_a_zone_round_trips() {
        let _serialised = ONE_ZONE_LIST_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // A `Zone` of the declared layout, leaked so the address the registry
        // keeps stays meaningful. Its contents are this crate's own vtable, so
        // nothing here depends on what a guest would put in one.
        let zone = Box::into_raw(Box::new(Zone::ours(
            c"AZoneSomeGuestRegistered".as_ptr(),
            std::ptr::null(),
        )));

        assert!(!all_zones().contains(&(zone as u64)));
        malloc_zone_register(zone.cast());
        assert!(
            all_zones().contains(&(zone as u64)),
            "a registered zone is not reported"
        );
        // Registering twice is one zone, not two.
        malloc_zone_register(zone.cast());
        assert_eq!(
            all_zones()
                .iter()
                .filter(|held| **held == zone as u64)
                .count(),
            1
        );

        malloc_zone_unregister(zone.cast());
        assert!(!all_zones().contains(&(zone as u64)));
        // And the two that cannot be right are no-ops rather than faults.
        malloc_zone_unregister(std::ptr::null_mut());
        malloc_zone_unregister(zone.cast());
        malloc_zone_register(std::ptr::null_mut());
        assert!(!all_zones().contains(&0), "a null zone was recorded");
    }

    /// The list this hands back, in the order a caller reads it to decide
    /// whether its own registration won.
    ///
    /// The call is `vm_address_t **`: the array is the **callee's** to allocate,
    /// and the caller goes straight on to dereference what comes back. An
    /// implementation that filled a buffer the caller had sized instead would
    /// leave that pointer as the null the caller passed in - which is a fault at
    /// the next instruction, with the allocator's own `zones[0]` check never
    /// reached and nothing in the log to say why.
    #[test]
    fn the_zone_list_answers_for_this_task() {
        let _serialised = ONE_ZONE_LIST_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut array: *mut u64 = std::ptr::null_mut();
        let mut count = 0;
        // SAFETY: a writable `vm_address_t *` slot, deliberately null as the
        // caller has it, and a writable `unsigned`.
        let rc = unsafe { malloc_get_all_zones(TASK_SELF, None, &mut array, &mut count) };
        assert_eq!(rc, KERN_SUCCESS);
        assert!(!array.is_null(), "the caller dereferences what comes back");
        assert!(count >= 2, "the default and purgeable zones at least");
        assert_eq!(array as usize % size_of::<u64>(), 0, "and it is an array");
        // SAFETY: the block above, `count` `u64`s, read before it is freed.
        let listed = unsafe { std::slice::from_raw_parts(array, count as usize) }.to_vec();
        // SAFETY: the block above, from this allocator, freed once.
        unsafe { crate::heap::free(array.cast()) };

        assert!(listed.contains(&(malloc_default_purgeable_zone() as u64)));
        assert!(listed.contains(&(malloc_default_zone() as u64)));
    }

    /// The order is the contract, and the sequence is the one a guest performs.
    ///
    /// This is not a test invented for the shape of the API. A guest installing
    /// its own zone - to route allocations through a delegating vtable - reads
    /// `zones[0]` to learn which zone it is displacing, registers its own,
    /// unregisters and re-registers the two it found, and then reads `zones[0]`
    /// again to confirm its own won, aborting by name if it did not.
    ///
    /// That whole dance is what fixes the order. The zone registered *before*
    /// the others are re-registered is the one that ends up first, so the list is
    /// oldest-registration-first and appending is what puts it there. A
    /// prepended list would report the last-registered zone - one the guest has
    /// already dealt with - as the winner.
    #[test]
    fn the_zone_registered_before_the_others_is_reported_first() {
        let _serialised = ONE_ZONE_LIST_AT_A_TIME
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let listed = || {
            let mut array: *mut u64 = std::ptr::null_mut();
            let mut count = 0;
            // SAFETY: a writable slot and count; the block is freed below.
            assert_eq!(
                // SAFETY: as above.
                unsafe { malloc_get_all_zones(TASK_SELF, None, &mut array, &mut count) },
                KERN_SUCCESS
            );
            // SAFETY: the block just returned, `count` `u64`s, read before the free.
            let zones = unsafe { std::slice::from_raw_parts(array, count as usize) }.to_vec();
            // SAFETY: the block above, from this allocator, freed once.
            unsafe { crate::heap::free(array.cast()) };
            zones
        };

        // The two zones the guest finds, and the first of them is the default.
        let before = listed();
        let displaced = malloc_default_zone();
        let purgeable = malloc_default_purgeable_zone();
        assert_eq!(before.first(), Some(&(displaced as u64)));

        // A `Zone` of the declared layout, leaked so the address the registry
        // keeps stays meaningful, as the round-trip test explains.
        // The name Chromium's own delegating zone carries. It is the string a
        // guest comparing zone names is looking for when it wants to know
        // whether a delegating allocator is already installed, so a test that
        // walks names should use one that is recognisable.
        let delegating = Box::into_raw(Box::new(Zone::ours(
            c"DelegatingDefaultZoneForPartitionAlloc".as_ptr(),
            std::ptr::null(),
        )));

        // The guest's sequence, in the order it performs it.
        malloc_zone_register(delegating.cast());
        malloc_zone_unregister(displaced);
        malloc_zone_register(displaced);
        malloc_zone_unregister(purgeable);
        malloc_zone_register(purgeable);

        let after = listed();
        assert_eq!(
            after.first(),
            Some(&(delegating as u64)),
            "a guest reads zones[0] to confirm the zone it installed won"
        );
        assert_eq!(
            after.len(),
            3,
            "and the two it re-registered are still there"
        );

        // Put the process back the way it was, so a test running after this one
        // sees the two-zone list it expects.
        malloc_zone_register(displaced);
        malloc_zone_register(purgeable);
        malloc_zone_unregister(delegating.cast());
        assert_eq!(all_zones().len(), 2, "the list is back to the two it owns");
    }

    /// Any other task is a capability this runtime does not have, and says so
    /// with the code Darwin uses for it. See `malloc_get_all_zones`.
    #[test]
    fn another_task_is_refused_rather_than_answered_empty() {
        let mut array: *mut u64 = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: a writable slot and count, with a task that is not this one.
        let rc =
            unsafe { malloc_get_all_zones(Name::from(0x9abcu32), None, &mut array, &mut count) };
        assert_eq!(rc, KERN_INVALID_TASK);
        // Nothing was handed over and no count was invented, so a caller that
        // ignored the code cannot read the untouched pointer as an answer.
        assert!(array.is_null());
        assert_eq!(count, 0);

        // The two null out-parameters are the address error, as everywhere in
        // Mach: with nowhere to put the answer, no question is being asked.
        // SAFETY: a null slot with a live count, which is what this is testing.
        assert_eq!(
            // SAFETY: as above.
            unsafe { malloc_get_all_zones(TASK_SELF, None, std::ptr::null_mut(), &mut count) },
            KERN_INVALID_ADDRESS
        );
        // SAFETY: a live slot with a null count.
        assert_eq!(
            unsafe { malloc_get_all_zones(TASK_SELF, None, &mut array, std::ptr::null_mut()) },
            KERN_INVALID_ADDRESS
        );
    }

    /// The one function here that has to end the process is tested in a child
    /// of this one, because it is not returning. What is being checked is the
    /// whole path: the trampoline's register save area, the guest's format
    /// string, a vararg in a vector register, one past the integer registers on
    /// the stack, the write to descriptor 2, and the `SIGABRT`.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn an_assertion_reaches_stderr_with_its_arguments_and_aborts() {
        use std::os::unix::process::ExitStatusExt;

        const CHILD: &str = "ROSETTA3_TEST_ABORT_REPORT";
        if std::env::var_os(CHILD).is_some() {
            // The child: a variadic call is not a shape Rust can *write*, so it
            // goes through the published address with the prototype it has.
            let call: unsafe extern "C" fn(*const c_char, ...) -> ! =
                // SAFETY: `abort_report_np` is a variadic function of this shape
                // that never returns, which is this type.
                unsafe { std::mem::transmute(crate::symbol("abort_report_np").expect("published")) };
            // SAFETY: a NUL-terminated format, and six arguments after it: one
            // string, one pointer, one double, and three integers that carry
            // the tail past `r9` and onto the caller's stack.
            unsafe {
                call(
                    c"abort_report_np: %s %p %.1f %d %d %d\n".as_ptr(),
                    c"zone".as_ptr(),
                    std::ptr::null::<c_void>(),
                    0.5f64,
                    1,
                    2,
                    3,
                )
            };
        }

        let Ok(exe) = std::env::current_exe() else {
            eprintln!("skipping: no path to this test binary");
            return;
        };
        // The child's own filter, from the name libtest gave this thread: a
        // hand-written name would stop matching the day this test is renamed,
        // and a child that ran nothing would look exactly like a child whose
        // report never reached `stderr`.
        let Some(name) = std::thread::current().name().map(str::to_string) else {
            eprintln!("skipping: this harness does not name its threads");
            return;
        };
        let out = std::process::Command::new(exe)
            .args(["--exact", &name, "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .expect("the child runs this same test");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("abort_report_np: zone (nil) 0.5 1 2 3"),
            "the format string and every argument reached the report; got:\n{stderr}"
        );
        assert!(!out.status.success(), "the child was supposed to die");
        assert_eq!(
            out.status.signal(),
            Some(libc::SIGABRT),
            "and to die of SIGABRT rather than of its own accord"
        );
    }
}
