//! CoreMedia contiguous byte buffers. Backing storage never moves while referenced.

#![allow(non_snake_case)]

use core::ffi::c_void;
use objc::class::Class;
use objc::object::Id;
use std::sync::{Arc, Mutex, OnceLock};

const BAD_CUSTOM_SOURCE: i32 = -12702;
const BAD_OFFSET: i32 = -12703;
const BAD_LENGTH: i32 = -12704;
const BAD_POINTER: i32 = -12705;
const EMPTY_BUFFER: i32 = -12706;
const UNALLOCATED_BLOCK: i32 = -12707;
const ALLOC_FAILED: i32 = -12701;
const STRUCT_ALLOC_FAILED: i32 = -12700;
const ASSURE_NOW: u32 = 1;
const ALWAYS_COPY_DATA: u32 = 1 << 1;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(pointer: *mut c_void);
}

/// Opaque until embedded guest callbacks can be thunked by the ABI bridge.
#[repr(C)]
pub struct CustomBlockSource {
    _private: [u8; 0],
}

struct Allocation {
    memory: *mut u8,
    capacity: usize,
    /// Whether `memory` is this module's to free. Memory lent with
    /// `kCFAllocatorNull` as its block allocator is the caller's.
    owned: bool,
}

// The backing pointer and all framework-mediated byte accesses are protected
// by the shared Mutex<Allocation>. Distinct views have independent Storage
// locks, which are NOT sufficient to synchronize overlapping byte accesses.
unsafe impl Send for Allocation {}

impl Allocation {
    fn allocate(&mut self) -> i32 {
        if !self.memory.is_null() {
            return 0;
        }
        // SAFETY: only malloc-backed storage is supported.
        let raw = unsafe { malloc(self.capacity) };
        if raw.is_null() {
            return ALLOC_FAILED;
        }
        self.memory = raw.cast();
        0
    }
}

impl Drop for Allocation {
    fn drop(&mut self) {
        if self.owned && !self.memory.is_null() {
            // SAFETY: the last buffer retaining the allocation owns the free.
            unsafe { free(self.memory.cast()) };
        }
    }
}

struct Storage {
    allocation: Arc<Mutex<Allocation>>,
    start: usize,
    length: usize,
}

impl Storage {
    fn allocate(&mut self) -> i32 {
        self.allocation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allocate()
    }

    // Reading must not change a deferred block's allocation state. Apple
    // returns kCMBlockBufferUnallocatedBlockErr until memory is explicitly
    // assured or an operation such as ReplaceDataBytes creates its storage.
    fn pointer(&self, offset: usize) -> Result<*mut u8, i32> {
        let shared = self.allocation.lock().unwrap_or_else(|e| e.into_inner());
        if shared.memory.is_null() {
            return Err(UNALLOCATED_BLOCK);
        }
        // SAFETY: creation validates start+length within allocation capacity;
        // all aliases retain this allocation and its bytes never move.
        Ok(unsafe { shared.memory.add(self.start + offset) })
    }

    /// Copy bytes while holding the allocation lock for the entire operation.
    /// All views acquire locks in the same order: Storage -> Allocation.
    ///
    /// # Safety
    /// `destination` must address `length` writable bytes and must not be
    /// concurrently accessed except through this allocation's serialized APIs.
    unsafe fn copy_bytes(&self, offset: usize, length: usize, destination: *mut u8) -> i32 {
        let shared = self.allocation.lock().unwrap_or_else(|e| e.into_inner());
        if shared.memory.is_null() {
            return UNALLOCATED_BLOCK;
        }
        // SAFETY: bounds were checked by the caller and the shared allocation
        // cannot be freed or modified by another view while this lock is held.
        // ptr::copy has memmove semantics, including overlapping guest slices.
        unsafe {
            core::ptr::copy(shared.memory.add(self.start + offset), destination, length);
        }
        0
    }

    /// Replacement is allowed to allocate deferred memory, and serializes the
    /// complete copy with reads/writes through any other view of the allocation.
    ///
    /// # Safety
    /// `source` must address `length` readable bytes and must not be
    /// concurrently modified outside this allocation's serialized APIs.
    unsafe fn replace_bytes(&self, offset: usize, length: usize, source: *const u8) -> i32 {
        let mut shared = self.allocation.lock().unwrap_or_else(|e| e.into_inner());
        let status = shared.allocate();
        if status != 0 {
            return status;
        }
        // SAFETY: the destination extent was checked by the caller; source is
        // caller-validated memory. ptr::copy is overlap-safe under this lock.
        unsafe {
            core::ptr::copy(source, shared.memory.add(self.start + offset), length);
        }
        0
    }
}

#[repr(C)]
struct Body {
    isa: *const Class,
    contents: *mut Mutex<Storage>,
}

fn class() -> &'static Class {
    static CLASS: OnceLock<usize> = OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let class =
            corefoundation::base::register(c"__NSCMBlockBuffer", None, size_of::<Body>(), true);
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: class registration persists for the life of the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("CMBlockBuffer class") }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: objc::sel::Sel) {
    // SAFETY: the final release owns the boxed storage exactly once.
    unsafe {
        let body = receiver as *mut Body;
        if !(*body).contents.is_null() {
            drop(Box::from_raw((*body).contents));
            (*body).contents = core::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

fn new_buffer(storage: Storage) -> *const c_void {
    // SAFETY: registered class is allocated with Body's exact layout.
    let body: *mut Body = unsafe { corefoundation::base::allocate(class()) };
    if body.is_null() {
        return core::ptr::null();
    }
    // SAFETY: the allocation's contents field is owned by the object.
    unsafe { (*body).contents = Box::into_raw(Box::new(Mutex::new(storage))) };
    body.cast()
}

/// # Safety
/// Handle must be null or a live CF object from this runtime.
unsafe fn storage<'a>(handle: *const c_void) -> Option<&'a Mutex<Storage>> {
    if handle.is_null() || !unsafe { corefoundation::base::is_kind_of(handle, class()) } {
        return None;
    }
    // SAFETY: instances of class() contain a live Mutex<Storage>.
    unsafe { (*(handle as *const Body)).contents.as_ref() }
}

pub extern "C" fn CMBlockBufferGetTypeID() -> usize {
    corefoundation::base::type_id(class())
}

/// # Safety
/// Handle must be null or a live builtin CoreFoundation object.
pub(crate) unsafe fn is_buffer(handle: *const c_void) -> bool {
    unsafe { storage(handle) }.is_some()
}

/// Creates a single-block buffer. Caller memory is taken only with
/// `kCFAllocatorNull` as its block allocator, which says nobody frees it: the
/// caller keeps it alive and the buffer only reads and writes it. Memory some
/// allocator should free at release, and custom block sources, whose
/// callbacks are guest code, are refused.
///
/// # Safety
/// Output must be writable. Embedded callbacks are explicitly rejected
/// because guest function pointers need ABI substitution before invocation.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CMBlockBufferCreateWithMemoryBlock(
    allocator: *const c_void,
    memory: *mut c_void,
    block_length: usize,
    block_allocator: *const c_void,
    custom_source: *const CustomBlockSource,
    offset: usize,
    length: usize,
    flags: u32,
    out: *mut *const c_void,
) -> i32 {
    if out.is_null() {
        return BAD_POINTER;
    }
    // SAFETY: out is a writable result slot.
    unsafe { *out = core::ptr::null() };
    if allocator
        == corefoundation::api::standard_allocator(corefoundation::api::StandardAllocator::Null)
    {
        return STRUCT_ALLOC_FAILED;
    }
    if !corefoundation::api::supports_malloc_allocator(allocator) {
        return BAD_CUSTOM_SOURCE;
    }
    if block_length == 0 || block_length > isize::MAX as usize {
        return BAD_LENGTH;
    }
    if offset > block_length {
        return BAD_OFFSET;
    }
    if length == 0 || length > block_length - offset {
        return BAD_LENGTH;
    }
    // Guest embedded CustomBlockSource callback pointers are not ABI-thunked.
    if !custom_source.is_null() {
        return BAD_CUSTOM_SOURCE;
    }
    let null_allocator =
        corefoundation::api::standard_allocator(corefoundation::api::StandardAllocator::Null);
    if !memory.is_null() {
        // Memory another allocator is to free at release is refused rather
        // than guessing whose heap it came from.
        if block_allocator != null_allocator {
            return BAD_CUSTOM_SOURCE;
        }
        let lent = Storage {
            allocation: Arc::new(Mutex::new(Allocation {
                memory: memory.cast(),
                capacity: block_length,
                owned: false,
            })),
            start: offset,
            length,
        };
        let result = new_buffer(lent);
        if result.is_null() {
            return STRUCT_ALLOC_FAILED;
        }
        // SAFETY: out is a writable result slot.
        unsafe { *out = result };
        return 0;
    }
    if block_allocator == null_allocator {
        // NULL supplied memory necessarily requires backing allocation, which
        // the distinguished null allocator explicitly cannot provide.
        return ALLOC_FAILED;
    }
    if !corefoundation::api::supports_malloc_allocator(block_allocator) {
        return BAD_CUSTOM_SOURCE;
    }
    let mut storage = Storage {
        allocation: Arc::new(Mutex::new(Allocation {
            memory: core::ptr::null_mut(),
            capacity: block_length,
            owned: true,
        })),
        start: offset,
        length,
    };
    if flags & ASSURE_NOW != 0 {
        let status = storage.allocate();
        if status != 0 {
            return status;
        }
    }
    let result = new_buffer(storage);
    if result.is_null() {
        return STRUCT_ALLOC_FAILED;
    }
    unsafe { *out = result };
    0
}

/// # Safety
/// The source must be a valid retained block buffer; out must be writable.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CMBlockBufferCreateContiguous(
    allocator: *const c_void,
    source: *const c_void,
    block_allocator: *const c_void,
    custom_source: *const CustomBlockSource,
    offset: usize,
    length: usize,
    flags: u32,
    out: *mut *const c_void,
) -> i32 {
    if out.is_null() {
        return BAD_POINTER;
    }
    unsafe { *out = core::ptr::null() };
    if allocator
        == corefoundation::api::standard_allocator(corefoundation::api::StandardAllocator::Null)
    {
        return STRUCT_ALLOC_FAILED;
    }
    if !corefoundation::api::supports_malloc_allocator(allocator) {
        return BAD_CUSTOM_SOURCE;
    }
    let Some(original) = (unsafe { storage(source) }) else {
        return BAD_POINTER;
    };
    let source_len = original.lock().unwrap_or_else(|e| e.into_inner()).length;
    if source_len == 0 {
        return EMPTY_BUFFER;
    }
    if offset >= source_len {
        return BAD_OFFSET;
    }
    let length = if length == 0 {
        source_len - offset
    } else {
        length
    };
    if length > source_len - offset {
        return BAD_LENGTH;
    }
    if !custom_source.is_null() {
        return BAD_CUSTOM_SOURCE;
    }
    let block_allocator_is_null = block_allocator
        == corefoundation::api::standard_allocator(corefoundation::api::StandardAllocator::Null);
    if !block_allocator_is_null && !corefoundation::api::supports_malloc_allocator(block_allocator)
    {
        return BAD_CUSTOM_SOURCE;
    }
    // Contiguity cannot be established by allocating an uninitialized source.
    // Even the force-copy/assure flags must reject a deferred source until its
    // actual bytes have become available.
    {
        let guard = original.lock().unwrap_or_else(|e| e.into_inner());
        if guard
            .allocation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .memory
            .is_null()
        {
            return UNALLOCATED_BLOCK;
        }
    }
    // The force-copy flag explicitly overrides the aliasing fast path. This
    // new owned allocation does not change if the original is later modified.
    if flags & ALWAYS_COPY_DATA != 0 {
        if block_allocator_is_null {
            return ALLOC_FAILED;
        }
        let mut copied = core::ptr::null();
        let status = unsafe {
            CMBlockBufferCreateWithMemoryBlock(
                allocator,
                core::ptr::null_mut(),
                length,
                block_allocator,
                custom_source,
                0,
                length,
                flags | ASSURE_NOW,
                &mut copied,
            )
        };
        if status != 0 {
            return status;
        }
        let mut pointer = core::ptr::null_mut();
        let status = unsafe {
            CMBlockBufferGetDataPointer(
                copied,
                0,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                &mut pointer,
            )
        };
        let status = if status == 0 {
            unsafe { CMBlockBufferCopyDataBytes(source, offset, length, pointer.cast()) }
        } else {
            status
        };
        if status != 0 {
            unsafe { corefoundation::api::CFRelease(copied) };
            return status;
        }
        unsafe { *out = copied };
        return 0;
    }
    // Every represented block is already contiguous. A new view retains its
    // allocation; releasing the original cannot invalidate the view.
    let (allocation, start) = {
        let guard = original.lock().unwrap_or_else(|e| e.into_inner());
        (Arc::clone(&guard.allocation), guard.start + offset)
    };
    let mut view = Storage {
        allocation,
        start,
        length,
    };
    if flags & ASSURE_NOW != 0 {
        let status = view.allocate();
        if status != 0 {
            return status;
        }
    }
    let result = new_buffer(view);
    if result.is_null() {
        return STRUCT_ALLOC_FAILED;
    }
    unsafe { *out = result };
    0
}

/// # Safety
/// The handle must refer to a live builtin CoreFoundation object.
pub unsafe extern "C" fn CMBlockBufferGetDataLength(handle: *const c_void) -> usize {
    let Some(contents) = (unsafe { storage(handle) }) else {
        return 0;
    };
    contents.lock().unwrap_or_else(|e| e.into_inner()).length
}

/// # Safety
/// Output slots must be writable. The returned pointer is borrowed until the
/// final CF release of the buffer and must not be used after that release.
pub unsafe extern "C" fn CMBlockBufferGetDataPointer(
    handle: *const c_void,
    offset: usize,
    length_at_offset_out: *mut usize,
    total_length_out: *mut usize,
    data_pointer_out: *mut *mut u8,
) -> i32 {
    let Some(contents) = (unsafe { storage(handle) }) else {
        return BAD_POINTER;
    };
    let guard = contents.lock().unwrap_or_else(|e| e.into_inner());
    if guard.length == 0 {
        return EMPTY_BUFFER;
    }
    if offset >= guard.length {
        return BAD_OFFSET;
    }
    let pointer = match guard.pointer(offset) {
        Ok(pointer) => pointer,
        Err(status) => return status,
    };
    // SAFETY: caller-provided slots are valid or null.
    unsafe {
        if !length_at_offset_out.is_null() {
            *length_at_offset_out = guard.length - offset;
        }
        if !total_length_out.is_null() {
            *total_length_out = guard.length;
        }
        if !data_pointer_out.is_null() {
            *data_pointer_out = pointer;
        }
    }
    0
}

/// # Safety
/// Destination must point to at least length writable bytes.
pub unsafe extern "C" fn CMBlockBufferCopyDataBytes(
    handle: *const c_void,
    offset: usize,
    length: usize,
    destination: *mut c_void,
) -> i32 {
    let Some(contents) = (unsafe { storage(handle) }) else {
        return BAD_POINTER;
    };
    let guard = contents.lock().unwrap_or_else(|e| e.into_inner());
    if offset > guard.length {
        return BAD_OFFSET;
    }
    if length == 0 || length > guard.length - offset {
        return BAD_LENGTH;
    }
    if destination.is_null() {
        return BAD_POINTER;
    }
    // SAFETY: caller supplies destination storage; bounds were checked above.
    // The shared allocation lock stays held across the *entire* read.
    unsafe { guard.copy_bytes(offset, length, destination.cast()) }
}

/// # Safety
/// Source must address at least length readable bytes.
pub unsafe extern "C" fn CMBlockBufferReplaceDataBytes(
    source: *const c_void,
    handle: *const c_void,
    offset: usize,
    length: usize,
) -> i32 {
    let Some(contents) = (unsafe { storage(handle) }) else {
        return BAD_POINTER;
    };
    let guard = contents.lock().unwrap_or_else(|e| e.into_inner());
    if offset > guard.length {
        return BAD_OFFSET;
    }
    if length == 0 || length > guard.length - offset {
        return BAD_LENGTH;
    }
    if source.is_null() {
        return BAD_POINTER;
    }
    // SAFETY: caller supplies source storage; bounds were checked above.
    // Allocation and the *entire* replacement happen under the shared lock.
    unsafe { guard.replace_bytes(offset, length, source.cast()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_unsupported_block_structure_allocator() {
        let mut out = core::ptr::dangling::<c_void>();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::dangling(),
                    core::ptr::null_mut(),
                    16,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    16,
                    0,
                    &mut out,
                ),
                BAD_CUSTOM_SOURCE
            );
        }
        assert!(out.is_null());
    }

    #[test]
    fn accepts_standard_malloc_allocator_but_rejects_null_allocator() {
        use corefoundation::api::{StandardAllocator, standard_allocator};
        let mut out = core::ptr::null();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    standard_allocator(StandardAllocator::Malloc),
                    core::ptr::null_mut(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    0,
                    &mut out,
                ),
                0
            );
            assert_eq!(CMBlockBufferGetDataLength(out), 4);
            corefoundation::api::CFRelease(out);
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    standard_allocator(StandardAllocator::Null),
                    core::ptr::null_mut(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    0,
                    &mut out,
                ),
                STRUCT_ALLOC_FAILED
            );
            assert!(out.is_null());
        }
    }

    #[test]
    fn uses_standard_backing_allocator_for_allocated_and_contiguous_blocks() {
        use corefoundation::api::{StandardAllocator, standard_allocator};
        for allocator in [
            core::ptr::null(),
            standard_allocator(StandardAllocator::Malloc),
            standard_allocator(StandardAllocator::SystemDefault),
            standard_allocator(StandardAllocator::MallocZone),
        ] {
            let mut source = core::ptr::null();
            unsafe {
                assert_eq!(
                    CMBlockBufferCreateWithMemoryBlock(
                        core::ptr::null(),
                        core::ptr::null_mut(),
                        4,
                        allocator,
                        core::ptr::null(),
                        0,
                        4,
                        ASSURE_NOW,
                        &mut source
                    ),
                    0
                );
                assert_eq!(CMBlockBufferGetDataLength(source), 4);
                assert_eq!(
                    CMBlockBufferReplaceDataBytes(b"abcd".as_ptr().cast(), source, 0, 4),
                    0
                );
                let mut contiguous = core::ptr::null();
                assert_eq!(
                    CMBlockBufferCreateContiguous(
                        core::ptr::null(),
                        source,
                        allocator,
                        core::ptr::null(),
                        0,
                        4,
                        0,
                        &mut contiguous
                    ),
                    0
                );
                let mut copied = [0u8; 4];
                assert_eq!(
                    CMBlockBufferCopyDataBytes(contiguous, 0, 4, copied.as_mut_ptr().cast()),
                    0
                );
                assert_eq!(&copied, b"abcd");
                corefoundation::api::CFRelease(contiguous);
                corefoundation::api::CFRelease(source);
            }
        }
        let mut output = core::ptr::dangling::<c_void>();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    4,
                    standard_allocator(StandardAllocator::Null),
                    core::ptr::null(),
                    0,
                    4,
                    ASSURE_NOW,
                    &mut output
                ),
                ALLOC_FAILED
            );
            assert!(output.is_null());
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::dangling_mut(),
                    4,
                    standard_allocator(StandardAllocator::Malloc),
                    core::ptr::null(),
                    0,
                    4,
                    0,
                    &mut output
                ),
                BAD_CUSTOM_SOURCE
            );
            assert!(output.is_null());
        }
    }

    #[test]
    fn native_null_allocator_matrix_for_block_creation_and_contiguous_views() {
        use corefoundation::api::{StandardAllocator, standard_allocator};
        let unsupported = standard_allocator(StandardAllocator::Null);
        let mut source = core::ptr::null();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    ASSURE_NOW,
                    &mut source,
                ),
                0
            );
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"abcd".as_ptr().cast(), source, 0, 4),
                0
            );
            // No backing allocation is necessary for an already contiguous view.
            for flags in [0u32, ASSURE_NOW] {
                let mut view = core::ptr::null();
                assert_eq!(
                    CMBlockBufferCreateContiguous(
                        core::ptr::null(),
                        source,
                        unsupported,
                        core::ptr::null(),
                        0,
                        4,
                        flags,
                        &mut view,
                    ),
                    0
                );
                let mut p = core::ptr::null_mut();
                let mut q = core::ptr::null_mut();
                assert_eq!(
                    CMBlockBufferGetDataPointer(
                        source,
                        0,
                        core::ptr::null_mut(),
                        core::ptr::null_mut(),
                        &mut p
                    ),
                    0
                );
                assert_eq!(
                    CMBlockBufferGetDataPointer(
                        view,
                        0,
                        core::ptr::null_mut(),
                        core::ptr::null_mut(),
                        &mut q
                    ),
                    0
                );
                assert_eq!(p, q);
                corefoundation::api::CFRelease(view);
            }
            let mut out = core::ptr::dangling::<c_void>();
            for flags in [ALWAYS_COPY_DATA, ALWAYS_COPY_DATA | ASSURE_NOW] {
                assert_eq!(
                    CMBlockBufferCreateContiguous(
                        core::ptr::null(),
                        source,
                        unsupported,
                        core::ptr::null(),
                        0,
                        4,
                        flags,
                        &mut out,
                    ),
                    ALLOC_FAILED
                );
                assert!(out.is_null());
            }
            // NULL structure allocator cannot allocate the CF block object.
            assert_eq!(
                CMBlockBufferCreateContiguous(
                    unsupported,
                    source,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    0,
                    &mut out,
                ),
                STRUCT_ALLOC_FAILED
            );
            assert!(out.is_null());
            for flags in [0, ASSURE_NOW] {
                assert_eq!(
                    CMBlockBufferCreateWithMemoryBlock(
                        core::ptr::null(),
                        core::ptr::null_mut(),
                        4,
                        unsupported,
                        core::ptr::null(),
                        0,
                        4,
                        flags,
                        &mut out,
                    ),
                    ALLOC_FAILED
                );
                assert!(out.is_null());
                assert_eq!(
                    CMBlockBufferCreateWithMemoryBlock(
                        unsupported,
                        core::ptr::null_mut(),
                        4,
                        core::ptr::null(),
                        core::ptr::null(),
                        0,
                        4,
                        flags,
                        &mut out,
                    ),
                    STRUCT_ALLOC_FAILED
                );
                assert!(out.is_null());
            }
            corefoundation::api::CFRelease(source);
        }
    }

    #[test]
    fn delayed_allocation_bytes_and_contiguous_copy() {
        let mut source = core::ptr::null();
        // SAFETY: out slot is writable.
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    8,
                    core::ptr::null(),
                    core::ptr::null(),
                    2,
                    4,
                    0,
                    &mut source,
                ),
                0
            );
            assert_eq!(
                corefoundation::api::CFGetTypeID(source),
                CMBlockBufferGetTypeID()
            );
            assert_eq!(CMBlockBufferGetDataLength(source), 4);
            let input = *b"ABCD";
            assert_eq!(
                CMBlockBufferReplaceDataBytes(input.as_ptr().cast(), source, 0, 4),
                0
            );
            let mut ptr = core::ptr::null_mut();
            let mut available = 0;
            let mut total = 0;
            assert_eq!(
                CMBlockBufferGetDataPointer(source, 1, &mut available, &mut total, &mut ptr),
                0
            );
            assert_eq!((available, total), (3, 4));
            assert_eq!(core::slice::from_raw_parts(ptr, available), b"BCD");
            assert_eq!(
                CMBlockBufferGetDataPointer(
                    source,
                    4,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    &mut ptr
                ),
                BAD_OFFSET
            );
            assert_eq!(
                CMBlockBufferCopyDataBytes(source, 3, 2, ptr.cast()),
                BAD_LENGTH
            );
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"a".as_ptr().cast(), source, 4, 1),
                BAD_LENGTH
            );
            let mut destination = [0; 4];
            assert_eq!(
                CMBlockBufferCopyDataBytes(source, 0, 4, destination.as_mut_ptr().cast()),
                0
            );
            assert_eq!(destination, input);

            let mut copy = core::ptr::null();
            assert_eq!(
                CMBlockBufferCreateContiguous(
                    core::ptr::null(),
                    source,
                    core::ptr::null(),
                    core::ptr::null(),
                    1,
                    0,
                    0,
                    &mut copy
                ),
                0
            );
            assert_eq!(CMBlockBufferGetDataLength(copy), 3);
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"xyz".as_ptr().cast(), copy, 0, 3),
                0
            );
            assert_eq!(core::slice::from_raw_parts(ptr, available), b"xyz");
            let mut bytes = [0; 3];
            assert_eq!(
                CMBlockBufferCopyDataBytes(copy, 0, 3, bytes.as_mut_ptr().cast()),
                0
            );
            assert_eq!(&bytes, b"xyz");

            corefoundation::api::CFRetain(copy);
            corefoundation::api::CFRelease(copy);
            assert_eq!(CMBlockBufferGetDataLength(copy), 3);
            corefoundation::api::CFRelease(copy);
            corefoundation::api::CFRelease(source);
        }
    }

    #[test]
    fn concurrent_alias_reads_and_replacements_are_serialized() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        const LEN: usize = 128 * 1024;
        const ITERATIONS: usize = 800;
        let mut source = core::ptr::null();
        let mut alias = core::ptr::null();
        let mut second_alias = core::ptr::null();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    LEN,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    LEN,
                    ASSURE_NOW,
                    &mut source,
                ),
                0
            );
            for target in [&mut alias, &mut second_alias] {
                assert_eq!(
                    CMBlockBufferCreateContiguous(
                        core::ptr::null(),
                        source,
                        core::ptr::null(),
                        core::ptr::null(),
                        0,
                        LEN,
                        0,
                        target,
                    ),
                    0
                );
            }
        }
        // The reader may acquire the first lock before either writer runs.
        // Initialize the allocation so its first snapshot is well-defined.
        let initial = vec![0x11u8; LEN];
        assert_eq!(
            unsafe { CMBlockBufferReplaceDataBytes(initial.as_ptr().cast(), source, 0, LEN) },
            0
        );
        // Hold all CF refs through the scoped workers. Pass the pointer bits,
        // not Rust references to the foreign CF objects, to worker threads.
        let (source, alias, second_alias) =
            (source as usize, alias as usize, second_alias as usize);
        let barrier = Arc::new(Barrier::new(3));
        thread::scope(|scope| {
            let writer_barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                let a = vec![0x55u8; LEN];
                let b = vec![0xaau8; LEN];
                writer_barrier.wait();
                for i in 0..ITERATIONS {
                    let bytes = if i % 2 == 0 { &a } else { &b };
                    assert_eq!(
                        unsafe {
                            CMBlockBufferReplaceDataBytes(
                                bytes.as_ptr().cast(),
                                source as *const c_void,
                                0,
                                LEN,
                            )
                        },
                        0
                    );
                }
            });
            let second_writer_barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                let a = vec![0x33u8; LEN];
                let b = vec![0xccu8; LEN];
                second_writer_barrier.wait();
                for i in 0..ITERATIONS {
                    let bytes = if i % 2 == 0 { &a } else { &b };
                    assert_eq!(
                        unsafe {
                            CMBlockBufferReplaceDataBytes(
                                bytes.as_ptr().cast(),
                                alias as *const c_void,
                                0,
                                LEN,
                            )
                        },
                        0
                    );
                }
            });
            let mut copied = vec![0u8; LEN];
            barrier.wait();
            for _ in 0..ITERATIONS {
                assert_eq!(
                    unsafe {
                        CMBlockBufferCopyDataBytes(
                            second_alias as *const c_void,
                            0,
                            LEN,
                            copied.as_mut_ptr().cast(),
                        )
                    },
                    0
                );
                // Each snapshot must be one complete replacement; the shared
                // allocation mutex forbids torn snapshots across views.
                assert!(
                    copied.iter().all(|&b| b == copied[0]),
                    "torn cross-view read"
                );
            }
        });
        unsafe {
            corefoundation::api::CFRelease(second_alias as *const c_void);
            corefoundation::api::CFRelease(alias as *const c_void);
            corefoundation::api::CFRelease(source as *const c_void);
        }
    }

    #[test]
    fn overlapping_copy_between_aliases_uses_memmove_semantics() {
        let mut source = core::ptr::null();
        let mut view = core::ptr::null();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    8,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    8,
                    ASSURE_NOW,
                    &mut source,
                ),
                0
            );
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"abcdefgh".as_ptr().cast(), source, 0, 8),
                0
            );
            assert_eq!(
                CMBlockBufferCreateContiguous(
                    core::ptr::null(),
                    source,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    8,
                    0,
                    &mut view,
                ),
                0
            );
            let mut ptr = core::ptr::null_mut();
            assert_eq!(
                CMBlockBufferGetDataPointer(
                    view,
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    &mut ptr
                ),
                0
            );
            assert_eq!(
                CMBlockBufferCopyDataBytes(source, 0, 6, ptr.add(2).cast()),
                0
            );
            let mut observed = [0u8; 8];
            assert_eq!(
                CMBlockBufferCopyDataBytes(view, 0, 8, observed.as_mut_ptr().cast()),
                0
            );
            assert_eq!(&observed, b"ababcdef");
            assert_eq!(
                CMBlockBufferReplaceDataBytes(ptr.add(2).cast(), view, 0, 6),
                0
            );
            assert_eq!(
                CMBlockBufferCopyDataBytes(source, 0, 8, observed.as_mut_ptr().cast()),
                0
            );
            assert_eq!(&observed, b"abcdefef");
            corefoundation::api::CFRelease(view);
            corefoundation::api::CFRelease(source);
        }
    }

    #[test]
    fn aliased_contiguous_views_share_bytes_and_keep_allocation_alive() {
        let mut source = core::ptr::null();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    8,
                    core::ptr::null(),
                    core::ptr::null(),
                    2,
                    4,
                    ASSURE_NOW,
                    &mut source
                ),
                0
            );
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"abcd".as_ptr().cast(), source, 0, 4),
                0
            );
            let mut full = core::ptr::null();
            assert_eq!(
                CMBlockBufferCreateContiguous(
                    core::ptr::null(),
                    source,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    0,
                    &mut full
                ),
                0
            );
            assert_ne!(full, source);
            let mut source_ptr = core::ptr::null_mut();
            let mut full_ptr = core::ptr::null_mut();
            assert_eq!(
                CMBlockBufferGetDataPointer(
                    source,
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    &mut source_ptr
                ),
                0
            );
            assert_eq!(
                CMBlockBufferGetDataPointer(
                    full,
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    &mut full_ptr
                ),
                0
            );
            assert_eq!(source_ptr, full_ptr);
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"xxxx".as_ptr().cast(), full, 0, 4),
                0
            );
            let mut readback = [0; 4];
            assert_eq!(
                CMBlockBufferCopyDataBytes(source, 0, 4, readback.as_mut_ptr().cast()),
                0
            );
            assert_eq!(&readback, b"xxxx");

            let mut slice = core::ptr::null();
            assert_eq!(
                CMBlockBufferCreateContiguous(
                    core::ptr::null(),
                    full,
                    core::ptr::null(),
                    core::ptr::null(),
                    1,
                    2,
                    0,
                    &mut slice
                ),
                0
            );
            let mut slice_ptr = core::ptr::null_mut();
            assert_eq!(
                CMBlockBufferGetDataPointer(
                    slice,
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    &mut slice_ptr
                ),
                0
            );
            assert_eq!(slice_ptr, source_ptr.add(1));
            corefoundation::api::CFRelease(source);
            corefoundation::api::CFRelease(full);
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"yz".as_ptr().cast(), slice, 0, 2),
                0
            );
            let mut readback = [0; 2];
            assert_eq!(
                CMBlockBufferCopyDataBytes(slice, 0, 2, readback.as_mut_ptr().cast()),
                0
            );
            assert_eq!(&readback, b"yz");
            corefoundation::api::CFRelease(slice);
        }
    }

    #[test]
    fn always_copy_data_flag_prevents_aliases_and_keeps_independent_ownership() {
        let mut source = core::ptr::null();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    ASSURE_NOW,
                    &mut source,
                ),
                0
            );
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"abcd".as_ptr().cast(), source, 0, 4),
                0
            );
            let mut independent = core::ptr::null();
            assert_eq!(
                CMBlockBufferCreateContiguous(
                    core::ptr::null(),
                    source,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    ALWAYS_COPY_DATA,
                    &mut independent,
                ),
                0
            );
            assert_ne!(source, independent);
            let mut source_ptr = core::ptr::null_mut();
            let mut independent_ptr = core::ptr::null_mut();
            assert_eq!(
                CMBlockBufferGetDataPointer(
                    source,
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    &mut source_ptr
                ),
                0
            );
            assert_eq!(
                CMBlockBufferGetDataPointer(
                    independent,
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    &mut independent_ptr
                ),
                0
            );
            assert_ne!(source_ptr, independent_ptr);
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"xxxx".as_ptr().cast(), independent, 0, 4),
                0
            );
            let mut original = [0u8; 4];
            assert_eq!(
                CMBlockBufferCopyDataBytes(source, 0, 4, original.as_mut_ptr().cast()),
                0
            );
            assert_eq!(&original, b"abcd");
            corefoundation::api::CFRelease(source);
            let mut readback = [0u8; 4];
            assert_eq!(
                CMBlockBufferCopyDataBytes(independent, 0, 4, readback.as_mut_ptr().cast()),
                0
            );
            assert_eq!(&readback, b"xxxx");
            corefoundation::api::CFRelease(independent);
        }
    }

    #[test]
    fn deferred_blocks_reject_access_until_replace_allocates_backing() {
        for flags in [0u32, ASSURE_NOW] {
            let mut source = core::ptr::null();
            unsafe {
                assert_eq!(
                    CMBlockBufferCreateWithMemoryBlock(
                        core::ptr::null(),
                        core::ptr::null_mut(),
                        4,
                        core::ptr::null(),
                        core::ptr::null(),
                        0,
                        4,
                        flags,
                        &mut source,
                    ),
                    0
                );
                let expected = if flags == 0 { UNALLOCATED_BLOCK } else { 0 };
                let mut pointer = core::ptr::null_mut();
                assert_eq!(
                    CMBlockBufferGetDataPointer(
                        source,
                        0,
                        core::ptr::null_mut(),
                        core::ptr::null_mut(),
                        &mut pointer
                    ),
                    expected
                );
                let mut copied = [0u8; 4];
                assert_eq!(
                    CMBlockBufferCopyDataBytes(source, 0, 4, copied.as_mut_ptr().cast()),
                    expected
                );
                for copy_flags in [
                    0u32,
                    ASSURE_NOW,
                    ALWAYS_COPY_DATA,
                    ASSURE_NOW | ALWAYS_COPY_DATA,
                ] {
                    let mut contiguous = core::ptr::null();
                    let status = CMBlockBufferCreateContiguous(
                        core::ptr::null(),
                        source,
                        core::ptr::null(),
                        core::ptr::null(),
                        0,
                        4,
                        copy_flags,
                        &mut contiguous,
                    );
                    if flags == 0 {
                        assert_eq!(status, UNALLOCATED_BLOCK, "flags {copy_flags}");
                        assert!(contiguous.is_null());
                    } else {
                        assert_eq!(status, 0, "flags {copy_flags}");
                        corefoundation::api::CFRelease(contiguous);
                    }
                }
                assert_eq!(
                    CMBlockBufferReplaceDataBytes(b"ABCD".as_ptr().cast(), source, 0, 4),
                    0
                );
                assert_eq!(
                    CMBlockBufferCopyDataBytes(source, 0, 4, copied.as_mut_ptr().cast()),
                    0
                );
                assert_eq!(&copied, b"ABCD");
                corefoundation::api::CFRelease(source);
            }
        }
    }

    #[test]
    fn contiguous_invalid_length_is_distinct_from_invalid_starting_offset() {
        let mut source = core::ptr::null();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    ASSURE_NOW,
                    &mut source,
                ),
                0
            );
            for (offset, length, expected) in
                [(3, 2, BAD_LENGTH), (0, 5, BAD_LENGTH), (4, 1, BAD_OFFSET)]
            {
                let mut contiguous = core::ptr::dangling::<c_void>();
                assert_eq!(
                    CMBlockBufferCreateContiguous(
                        core::ptr::null(),
                        source,
                        core::ptr::null(),
                        core::ptr::null(),
                        offset,
                        length,
                        0,
                        &mut contiguous,
                    ),
                    expected
                );
                assert!(contiguous.is_null());
            }
            corefoundation::api::CFRelease(source);
        }
    }

    #[test]
    fn zero_length_operations_and_out_of_range_classification_match_native() {
        let mut block = core::ptr::dangling::<c_void>();
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    0,
                    0,
                    &mut block
                ),
                BAD_LENGTH
            );
            assert!(block.is_null());
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    ASSURE_NOW,
                    &mut block
                ),
                0
            );
            let mut bytes = [0u8; 4];
            assert_eq!(
                CMBlockBufferCopyDataBytes(block, 0, 0, bytes.as_mut_ptr().cast()),
                BAD_LENGTH
            );
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"a".as_ptr().cast(), block, 0, 0),
                BAD_LENGTH
            );
            assert_eq!(
                CMBlockBufferCopyDataBytes(block, 3, 2, bytes.as_mut_ptr().cast()),
                BAD_LENGTH
            );
            assert_eq!(
                CMBlockBufferReplaceDataBytes(b"ab".as_ptr().cast(), block, 3, 2),
                BAD_LENGTH
            );
            assert_eq!(
                CMBlockBufferCopyDataBytes(block, 5, 1, bytes.as_mut_ptr().cast()),
                BAD_OFFSET
            );
            corefoundation::api::CFRelease(block);
        }
    }

    #[test]
    fn rejects_invalid_lengths_offsets_and_unsafe_ownership() {
        let mut out = core::ptr::dangling::<c_void>();
        // SAFETY: all failures are rejected before invalid pointers are used.
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    0,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    0,
                    0,
                    &mut out
                ),
                BAD_LENGTH
            );
            assert!(out.is_null());
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    3,
                    2,
                    0,
                    &mut out
                ),
                BAD_LENGTH
            );
            let memory = [0u8; 4];
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    memory.as_ptr().cast_mut().cast(),
                    4,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    4,
                    0,
                    &mut out
                ),
                BAD_CUSTOM_SOURCE
            );
            assert_eq!(CMBlockBufferGetDataLength(core::ptr::null()), 0);
            assert_eq!(
                CMBlockBufferGetDataPointer(
                    core::ptr::null(),
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    core::ptr::null_mut()
                ),
                BAD_POINTER
            );
        }
    }

    #[test]
    fn memory_lent_with_the_null_allocator_is_read_in_place_and_never_freed() {
        use corefoundation::api::{StandardAllocator, standard_allocator};
        let mut memory = *b"..abcd..";
        let (mut lent, mut copy) = (core::ptr::null(), core::ptr::null());
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    memory.as_mut_ptr().cast(),
                    memory.len(),
                    standard_allocator(StandardAllocator::Null),
                    core::ptr::null(),
                    2,
                    4,
                    0,
                    &mut lent,
                ),
                0
            );
            assert_eq!(CMBlockBufferGetDataLength(lent), 4);
            let mut pointer = core::ptr::null_mut();
            assert_eq!(
                CMBlockBufferGetDataPointer(
                    lent,
                    0,
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    &mut pointer
                ),
                0
            );
            assert_eq!(pointer.cast::<u8>(), memory.as_mut_ptr().add(2));
            assert_eq!(
                CMBlockBufferCreateContiguous(
                    core::ptr::null(),
                    lent,
                    core::ptr::null(),
                    core::ptr::null(),
                    0,
                    0,
                    ALWAYS_COPY_DATA,
                    &mut copy,
                ),
                0
            );
            corefoundation::api::CFRelease(lent);
            memory.fill(b'x');
            let mut bytes = [0u8; 4];
            assert_eq!(
                CMBlockBufferCopyDataBytes(copy, 0, 4, bytes.as_mut_ptr().cast()),
                0
            );
            assert_eq!(&bytes, b"abcd", "the copy is its own");
            corefoundation::api::CFRelease(copy);
        }
        // Still the caller's to use, and to drop.
        assert_eq!(memory, *b"xxxxxxxx");
    }

    #[test]
    fn refuses_unbridged_custom_callbacks_without_dereferencing_them() {
        let fake_source = core::ptr::dangling::<CustomBlockSource>();
        let mut buffer = core::ptr::dangling::<c_void>();
        // SAFETY: custom_source is never dereferenced on the rejection path.
        unsafe {
            assert_eq!(
                CMBlockBufferCreateWithMemoryBlock(
                    core::ptr::null(),
                    core::ptr::null_mut(),
                    16,
                    core::ptr::null(),
                    fake_source,
                    0,
                    16,
                    0,
                    &mut buffer
                ),
                BAD_CUSTOM_SOURCE
            );
            assert!(buffer.is_null());
        }
    }
}

exports::exports! {
    functions {
        "CMBlockBufferGetTypeID" => CMBlockBufferGetTypeID,
        "CMBlockBufferCreateWithMemoryBlock" => CMBlockBufferCreateWithMemoryBlock,
        "CMBlockBufferCreateContiguous" => CMBlockBufferCreateContiguous,
        "CMBlockBufferGetDataPointer" => CMBlockBufferGetDataPointer,
        "CMBlockBufferGetDataLength" => CMBlockBufferGetDataLength,
        "CMBlockBufferCopyDataBytes" => CMBlockBufferCopyDataBytes,
        "CMBlockBufferReplaceDataBytes" => CMBlockBufferReplaceDataBytes,
    }
}
