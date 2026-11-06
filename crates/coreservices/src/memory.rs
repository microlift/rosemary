//! The Memory Manager's `Ptr`/`Handle` allocator.
//!
//! There is no heap zone here, no compaction, and nothing purges - this tree
//! has an ordinary host allocator behind it, which never needs any of that.
//! What it does need, and what the host allocator will not do for a bare
//! pointer, is remembering each block's size: `GetPtrSize`/`GetHandleSize` and
//! the `realloc`-based resizers all depend on recovering it, so every block
//! this module hands out is tracked by address until it is disposed.
//!
//! A `Handle` is a master pointer to a block - `*mut *mut c_void` - kept
//! stable across `SetHandleSize` the way Apple's own are: the master pointer
//! itself never moves, but what it points at can, and every accessor reads
//! the block through it rather than caching the block address across a call
//! that might resize it.

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc, realloc};
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

type OSErr = i16;
type Ptr = *mut c_void;
type Handle = *mut Ptr;

const NO_ERR: OSErr = 0;
/// `memFullErr`.
const MEM_FULL_ERR: OSErr = -108;
/// `nilHandleErr`.
const NIL_HANDLE_ERR: OSErr = -109;
/// `paramErr`.
const PARAM_ERR: OSErr = -50;

/// Every block's alignment. Nothing here knows the guest type behind a block,
/// so this is large enough for anything a guest is likely to cast a `Ptr` or a
/// dereferenced `Handle` to and read through directly.
const ALIGN: usize = 16;

thread_local! {
    /// What `MemError` reports: the last call's result, per thread - the way
    /// Carbon has kept it since Mac OS X 10.3 (see `MacMemory.h`). Before
    /// that it was one global for the whole application; nothing here needs
    /// to reproduce that older, less safe behavior.
    static MEM_ERROR: Cell<OSErr> = const { Cell::new(NO_ERR) };
}

fn set_error(err: OSErr) -> OSErr {
    MEM_ERROR.with(|e| e.set(err));
    err
}

/// `OSErr MemError(void)`
pub extern "C" fn MemError() -> OSErr {
    MEM_ERROR.with(Cell::get)
}

/// The layout a logical `size` is actually allocated with. `size.max(1)`
/// because `GlobalAlloc` requires a nonzero size; `NewPtr(0)`/`NewHandle(0)`
/// still answer a valid, distinct, zero-length block, as Apple's do.
fn layout_for(size: usize) -> Layout {
    Layout::from_size_align(size.max(1), ALIGN).unwrap_or(Layout::new::<u8>())
}

/// Large enough that no real guest allocation reaches it, small enough that
/// [`capacity_for`] rounding it up to the next power of two cannot overflow
/// `usize`.
const MAX_SIZE: usize = 1 << 40;

/// The capacity actually reserved for a block of `size` bytes: a power of
/// two, so an ordinary [`SetPtrSize`] grow usually fits inside it without
/// touching the host allocator at all - which is what lets that call keep a
/// nonrelocatable `Ptr` at the same address every time it succeeds, the one
/// thing it must never fail to do. `None` for a `size` too large to be a
/// genuine request.
fn capacity_for(size: usize) -> Option<usize> {
    (size <= MAX_SIZE).then(|| size.max(16).next_power_of_two())
}

/// Nonrelocatable blocks, by address: `(capacity actually allocated, logical
/// size [`GetPtrSize`] answers)`. The host allocator does not let a bare
/// pointer answer either, so this is what [`GetPtrSize`]/[`SetPtrSize`]/
/// [`DisposePtr`] recover them from.
static PTRS: Mutex<Option<HashMap<usize, (usize, usize)>>> = Mutex::new(None);

fn new_ptr(byte_count: i64, zeroed: bool) -> Ptr {
    let Ok(size) = usize::try_from(byte_count) else {
        set_error(PARAM_ERR);
        return std::ptr::null_mut();
    };
    let Some(capacity) = capacity_for(size) else {
        set_error(MEM_FULL_ERR);
        return std::ptr::null_mut();
    };
    let layout = layout_for(capacity);
    let raw = match zeroed {
        // SAFETY: `layout` has a nonzero size.
        true => unsafe { alloc_zeroed(layout) },
        false => unsafe { alloc(layout) },
    };
    if raw.is_null() {
        set_error(MEM_FULL_ERR);
        return std::ptr::null_mut();
    }
    let mut held = PTRS.lock().unwrap_or_else(|e| e.into_inner());
    held.get_or_insert_with(HashMap::default)
        .insert(raw as usize, (capacity, size));
    set_error(NO_ERR);
    raw.cast()
}

/// `Ptr NewPtr(Size byteCount)`
pub extern "C" fn NewPtr(byte_count: i64) -> Ptr {
    new_ptr(byte_count, false)
}

/// `Ptr NewPtrClear(Size byteCount)`
pub extern "C" fn NewPtrClear(byte_count: i64) -> Ptr {
    new_ptr(byte_count, true)
}

/// `void DisposePtr(Ptr p)`. `NULL` is accepted, as Apple's is, and disposes
/// nothing.
///
/// # Safety
///
/// `p` is null or a live [`NewPtr`]/[`NewPtrClear`] result not yet disposed.
pub unsafe extern "C" fn DisposePtr(p: Ptr) {
    if p.is_null() {
        set_error(NO_ERR);
        return;
    }
    let mut held = PTRS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = held.as_mut() else {
        set_error(PARAM_ERR);
        return;
    };
    let Some((capacity, _size)) = map.remove(&(p as usize)) else {
        set_error(PARAM_ERR);
        return;
    };
    // SAFETY: `p` was allocated with `layout_for(capacity)`, and is removed
    // from `PTRS` exactly once, so this runs at most once per allocation.
    unsafe { dealloc(p.cast(), layout_for(capacity)) };
    set_error(NO_ERR);
}

/// `Size GetPtrSize(Ptr p)`
pub extern "C" fn GetPtrSize(p: Ptr) -> i64 {
    let held = PTRS.lock().unwrap_or_else(|e| e.into_inner());
    match held.as_ref().and_then(|m| m.get(&(p as usize))) {
        Some(&(_capacity, size)) => {
            set_error(NO_ERR);
            size as i64
        }
        None => {
            set_error(PARAM_ERR);
            0
        }
    }
}

/// `void SetPtrSize(Ptr p, Size newSize)`.
///
/// A real nonrelocatable block never moves - growing it either succeeds in
/// place or fails - and since this call has no way to hand the caller a new
/// address even if it wanted to, it never tries: `p` fits within the
/// capacity [`NewPtr`]/[`NewPtrClear`] already reserved (see
/// [`capacity_for`]) or the call fails with `memFullErr`, exactly as a real
/// block boxed in by its neighbors would, rather than silently relocating
/// out from under a caller who kept using the address they were given.
///
/// # Safety
///
/// `p` is a live [`NewPtr`]/[`NewPtrClear`] result not yet disposed.
pub unsafe extern "C" fn SetPtrSize(p: Ptr, new_size: i64) {
    let Ok(new_size) = usize::try_from(new_size) else {
        set_error(PARAM_ERR);
        return;
    };
    let mut held = PTRS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = held.as_mut() else {
        set_error(PARAM_ERR);
        return;
    };
    let Some(entry) = map.get_mut(&(p as usize)) else {
        set_error(PARAM_ERR);
        return;
    };
    if new_size > entry.0 {
        set_error(MEM_FULL_ERR);
        return;
    }
    entry.1 = new_size;
    set_error(NO_ERR);
}

/// Live handles, by the master pointer's own address: `(block address,
/// logical size)`. The master pointer is the `Handle` a guest holds, so
/// looking one up by its address is looking up by the `Handle` value itself.
/// The block is kept as a bare `usize`, not a `Ptr`, so the map can live in a
/// `static` - a raw pointer is not `Send`/`Sync`, an address is.
static HANDLES: Mutex<Option<HashMap<usize, (usize, usize)>>> = Mutex::new(None);

fn new_handle(byte_count: i64, zeroed: bool) -> Handle {
    let Ok(size) = usize::try_from(byte_count) else {
        set_error(PARAM_ERR);
        return std::ptr::null_mut();
    };
    let layout = layout_for(size);
    let block = match zeroed {
        // SAFETY: `layout` has a nonzero size.
        true => unsafe { alloc_zeroed(layout) },
        false => unsafe { alloc(layout) },
    };
    if block.is_null() {
        set_error(MEM_FULL_ERR);
        return std::ptr::null_mut();
    }
    let master = Box::into_raw(Box::new(block.cast::<c_void>()));
    let mut held = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    held.get_or_insert_with(HashMap::default)
        .insert(master as usize, (block as usize, size));
    set_error(NO_ERR);
    master
}

/// `Handle NewHandle(Size byteCount)`
pub extern "C" fn NewHandle(byte_count: i64) -> Handle {
    new_handle(byte_count, false)
}

/// `Handle NewHandleClear(Size byteCount)`
pub extern "C" fn NewHandleClear(byte_count: i64) -> Handle {
    new_handle(byte_count, true)
}

/// `void DisposeHandle(Handle h)`. `NULL` is accepted and disposes nothing.
///
/// # Safety
///
/// `h` is null or a live [`NewHandle`]/[`NewHandleClear`] result not yet
/// disposed.
pub unsafe extern "C" fn DisposeHandle(h: Handle) {
    if h.is_null() {
        set_error(NO_ERR);
        return;
    }
    let mut held = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = held.as_mut() else {
        set_error(NIL_HANDLE_ERR);
        return;
    };
    let Some((block, size)) = map.remove(&(h as usize)) else {
        set_error(NIL_HANDLE_ERR);
        return;
    };
    // SAFETY: `block` was allocated with `layout_for(size)` (by `new_handle`,
    // or most recently by `SetHandleSize`/`PtrAndHand`), and `h` is removed
    // from `HANDLES` exactly once, so this runs at most once per allocation.
    unsafe {
        dealloc(block as *mut u8, layout_for(size));
        drop(Box::from_raw(h));
    }
    set_error(NO_ERR);
}

/// `Size GetHandleSize(Handle h)`
///
/// # Safety
///
/// `h` is null or a live [`NewHandle`]/[`NewHandleClear`] result.
pub unsafe extern "C" fn GetHandleSize(h: Handle) -> i64 {
    let held = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    match held.as_ref().and_then(|m| m.get(&(h as usize))) {
        Some(&(_, size)) => {
            set_error(NO_ERR);
            size as i64
        }
        None => {
            set_error(NIL_HANDLE_ERR);
            0
        }
    }
}

/// `void SetHandleSize(Handle h, Size newSize)`. Unlike [`SetPtrSize`], moving
/// the block on a resize is exactly what a `Handle`'s indirection exists for -
/// not an approximation - so the master pointer at `h` is updated to the
/// block's new address, which is real Memory Manager behavior.
///
/// # Safety
///
/// `h` is a live [`NewHandle`]/[`NewHandleClear`] result not yet disposed.
pub unsafe extern "C" fn SetHandleSize(h: Handle, new_size: i64) {
    let Ok(new_size) = usize::try_from(new_size) else {
        set_error(PARAM_ERR);
        return;
    };
    let mut held = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = held.as_mut() else {
        set_error(NIL_HANDLE_ERR);
        return;
    };
    let Some(entry) = map.get_mut(&(h as usize)) else {
        set_error(NIL_HANDLE_ERR);
        return;
    };
    let (block, old_size) = *entry;
    // SAFETY: `block` was allocated with `layout_for(old_size)`, and
    // `new_size.max(1)` is nonzero.
    let raw = unsafe { realloc(block as *mut u8, layout_for(old_size), new_size.max(1)) };
    if raw.is_null() {
        set_error(MEM_FULL_ERR);
        return;
    }
    *entry = (raw as usize, new_size);
    // SAFETY: `h` is the master pointer `new_handle` boxed; the Memory
    // Manager updates it in place on every move, which is the point of a
    // master pointer.
    unsafe { *h = raw.cast() };
    set_error(NO_ERR);
}

/// `SInt8 HGetState(Handle h)`. Always unlocked and unpurgeable - the state
/// every handle this module hands out is born with, and nothing here ever
/// locks or purges one.
///
/// # Safety
///
/// `h` is null or a live [`NewHandle`]/[`NewHandleClear`] result.
pub unsafe extern "C" fn HGetState(h: Handle) -> i8 {
    let held = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    match held.as_ref().is_some_and(|m| m.contains_key(&(h as usize))) {
        true => {
            set_error(NO_ERR);
            0
        }
        false => {
            set_error(NIL_HANDLE_ERR);
            0
        }
    }
}

/// `OSErr PtrAndHand(const void *ptr1, Handle hand2, long size)`: appends
/// `size` bytes starting at `ptr1` onto the end of `hand2`'s block, growing it
/// first - equivalent to `SetHandleSize(hand2, GetHandleSize(hand2) + size)`
/// followed by a copy into the new room.
///
/// # Safety
///
/// `ptr1` is null, when `size` is zero, or readable for `size` bytes; `hand2`
/// is a live [`NewHandle`]/[`NewHandleClear`] result not yet disposed.
pub unsafe extern "C" fn PtrAndHand(ptr1: *const c_void, hand2: Handle, size: i64) -> OSErr {
    let Ok(add) = usize::try_from(size) else {
        return set_error(PARAM_ERR);
    };
    if ptr1.is_null() && add != 0 {
        return set_error(PARAM_ERR);
    }
    let mut held = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = held.as_mut() else {
        return set_error(NIL_HANDLE_ERR);
    };
    let Some(entry) = map.get_mut(&(hand2 as usize)) else {
        return set_error(NIL_HANDLE_ERR);
    };
    let (block, old_size) = *entry;
    let new_size = old_size + add;
    // SAFETY: `block` was allocated with `layout_for(old_size)`, and
    // `new_size.max(1)` is nonzero.
    let raw = unsafe { realloc(block as *mut u8, layout_for(old_size), new_size.max(1)) };
    if raw.is_null() {
        return set_error(MEM_FULL_ERR);
    }
    if add != 0 {
        // SAFETY: `raw` now has room for `old_size + add` bytes, and the
        // caller states `ptr1` is readable for `add` of them.
        unsafe { std::ptr::copy_nonoverlapping(ptr1.cast::<u8>(), raw.add(old_size), add) };
    }
    *entry = (raw as usize, new_size);
    // SAFETY: as `SetHandleSize`.
    unsafe { *hand2 = raw.cast() };
    set_error(NO_ERR)
}

exports::exports! {
    functions {
        "NewPtr" => NewPtr,
        "NewPtrClear" => NewPtrClear,
        "DisposePtr" => DisposePtr,
        "GetPtrSize" => GetPtrSize,
        "SetPtrSize" => SetPtrSize,
        "NewHandle" => NewHandle,
        "NewHandleClear" => NewHandleClear,
        "DisposeHandle" => DisposeHandle,
        "GetHandleSize" => GetHandleSize,
        "SetHandleSize" => SetHandleSize,
        "HGetState" => HGetState,
        "PtrAndHand" => PtrAndHand,
        "MemError" => MemError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_round_trips_its_size_and_bytes() {
        let p = NewPtr(4);
        assert!(!p.is_null());
        assert_eq!(GetPtrSize(p), 4);
        assert_eq!(MemError(), NO_ERR);
        // SAFETY: `p` is live and sized for 4 bytes.
        unsafe { p.cast::<u8>().write_bytes(0xaa, 4) };
        // SAFETY: `p` is live, not yet disposed.
        unsafe { SetPtrSize(p, 8) };
        assert_eq!(GetPtrSize(p), 8);
        // SAFETY: `p` is live.
        unsafe { DisposePtr(p) };
        assert_eq!(GetPtrSize(p), 0);
        assert_eq!(MemError(), PARAM_ERR);
    }

    #[test]
    fn a_zero_length_pointer_is_valid_and_not_null() {
        let p = NewPtr(0);
        assert!(!p.is_null());
        assert_eq!(GetPtrSize(p), 0);
        // SAFETY: `p` is live.
        unsafe { DisposePtr(p) };
    }

    #[test]
    fn a_negative_size_pointer_is_refused() {
        assert!(NewPtr(-1).is_null());
        assert_eq!(MemError(), PARAM_ERR);
    }

    #[test]
    fn new_ptr_clear_zeroes_the_block() {
        let p = NewPtrClear(8);
        // SAFETY: `p` is live and sized for 8 bytes.
        let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), 8) };
        assert_eq!(bytes, [0; 8]);
        // SAFETY: `p` is live.
        unsafe { DisposePtr(p) };
    }

    #[test]
    fn a_handle_round_trips_through_resizes() {
        let h = NewHandle(3);
        assert!(!h.is_null());
        assert_eq!(unsafe { GetHandleSize(h) }, 3);
        assert_eq!(unsafe { HGetState(h) }, 0);
        // SAFETY: the master pointer `NewHandle` returned, live.
        let block = unsafe { *h };
        // SAFETY: the block `NewHandle` sized for 3 bytes.
        unsafe { block.cast::<u8>().write_bytes(b'x', 3) };
        // SAFETY: `h` is live, not yet disposed.
        unsafe { SetHandleSize(h, 6) };
        assert_eq!(unsafe { GetHandleSize(h) }, 6);
        // The master pointer `h` itself never moves - only what it points at
        // might - so re-reading through it still finds the live block.
        // SAFETY: `h` is live.
        let moved = unsafe { *h };
        // SAFETY: the (possibly moved) block, now sized for 6 bytes.
        let bytes = unsafe { std::slice::from_raw_parts(moved.cast::<u8>(), 3) };
        assert_eq!(bytes, [b'x'; 3]);
        // SAFETY: `h` is live.
        unsafe { DisposeHandle(h) };
        assert_eq!(unsafe { GetHandleSize(h) }, 0);
        assert_eq!(MemError(), NIL_HANDLE_ERR);
    }

    #[test]
    fn disposing_a_null_handle_is_harmless() {
        // SAFETY: null is an accepted no-op.
        unsafe { DisposeHandle(std::ptr::null_mut()) };
        assert_eq!(MemError(), NO_ERR);
    }

    #[test]
    fn ptr_and_hand_appends_and_grows() {
        let h = NewHandle(0);
        let tail = b"tail";
        // SAFETY: `tail` is readable for its own length; `h` is live.
        let status = unsafe { PtrAndHand(tail.as_ptr().cast(), h, tail.len() as i64) };
        assert_eq!(status, NO_ERR);
        assert_eq!(unsafe { GetHandleSize(h) }, 4);
        // SAFETY: `h` is live.
        let block = unsafe { *h };
        // SAFETY: the block, now sized for 4 bytes.
        let bytes = unsafe { std::slice::from_raw_parts(block.cast::<u8>(), 4) };
        assert_eq!(bytes, tail);
        // SAFETY: `h` is live.
        unsafe { DisposeHandle(h) };
    }
}
