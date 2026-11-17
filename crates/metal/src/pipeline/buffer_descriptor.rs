//! Per-stage buffer mutability promises, independent of the bound buffers.

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use objc::object::Id;
use objc::sel::Sel;

use crate::gpu;
use crate::object::{self, methods};

const BUFFER_COUNT: usize = 31;
const MUTABILITY_DEFAULT: u64 = 0;
const MUTABILITY_IMMUTABLE: u64 = 2;

struct BufferDescriptor {
    mutability: AtomicU64,
}

struct BufferArray {
    elements: [AtomicPtr<c_void>; BUFFER_COUNT],
}

impl BufferArray {
    fn new() -> Self {
        Self {
            elements: std::array::from_fn(|_| AtomicPtr::new(std::ptr::null_mut())),
        }
    }
}

unsafe extern "C" fn descriptor_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated `MTLPipelineBufferDescriptor`.
    unsafe {
        object::init(
            receiver,
            BufferDescriptor {
                mutability: AtomicU64::new(MUTABILITY_DEFAULT),
            },
        )
    };
    receiver
}

unsafe extern "C" fn mutability(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a buffer descriptor of this crate's; the field is atomic.
    unsafe { object::body_ref::<BufferDescriptor>(receiver) }
        .mutability
        .load(Ordering::Relaxed)
}

unsafe extern "C" fn set_mutability(receiver: Id, _sel: Sel, value: u64) {
    if value > MUTABILITY_IMMUTABLE {
        gpu::trace(format!(
            "setMutability: rejects invalid MTLMutability {value}"
        ));
        return;
    }
    // SAFETY: a buffer descriptor of this crate's; the field is atomic.
    unsafe { object::body_ref::<BufferDescriptor>(receiver) }
        .mutability
        .store(value, Ordering::Relaxed);
}

unsafe fn object_message(receiver: Id, selector: &str) -> Id {
    // SAFETY: these messages return an object and take no explicit arguments.
    let send: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    // SAFETY: the caller passes a live object or nil.
    unsafe { darwin::call_as_darwin!(send(receiver, objc::sel::get(selector))) }
}

unsafe extern "C" fn descriptor_copy(receiver: Id, _sel: Sel, zone: *mut c_void) -> Id {
    // The native copy allocates the dynamic class and runs its initializer.
    // SAFETY: a live receiver of this crate's class or a subclass of it.
    let Some(class) = (unsafe { objc::object::class_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `allocWithZone:` takes a zone and returns an object.
    let allocate: unsafe extern "C" fn(Id, Sel, *mut c_void) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    // SAFETY: as above, sent to the receiver's own class.
    let copied = unsafe {
        darwin::call_as_darwin!(allocate(
            objc::class::ptr(class) as Id,
            objc::sel::get("allocWithZone:"),
            zone
        ))
    };
    // SAFETY: a freshly allocated instance of the receiver's class.
    let copied = unsafe { object_message(copied, "init") };
    if !copied.is_null() {
        // SAFETY: both are buffer descriptors of this crate's.
        let value = unsafe { mutability(receiver, std::ptr::null()) };
        // SAFETY: as above; the field is atomic.
        unsafe { object::body_ref::<BufferDescriptor>(copied) }
            .mutability
            .store(value, Ordering::Relaxed);
    }
    copied
}

unsafe extern "C" fn array_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated `MTLPipelineBufferDescriptorArray`.
    unsafe { object::init(receiver, BufferArray::new()) };
    receiver
}

pub(super) fn new_array() -> Id {
    object::make(c"MTLPipelineBufferDescriptorArray", BufferArray::new())
}

unsafe extern "C" fn element(receiver: Id, _sel: Sel, index: u64) -> Id {
    // SAFETY: an array of this crate's; its slots are atomic.
    let array = unsafe { object::body_ref::<BufferArray>(receiver) };
    let Some(slot) = usize::try_from(index)
        .ok()
        .and_then(|index| array.elements.get(index))
    else {
        gpu::trace(format!(
            "pipeline buffer index {index} is outside 0..{BUFFER_COUNT}"
        ));
        return std::ptr::null_mut();
    };
    let current = slot.load(Ordering::Acquire);
    if !current.is_null() {
        return current.cast();
    }
    let created = object::make(
        c"MTLPipelineBufferDescriptor",
        BufferDescriptor {
            mutability: AtomicU64::new(MUTABILITY_DEFAULT),
        },
    );
    match slot.compare_exchange(
        std::ptr::null_mut(),
        created.cast(),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => created,
        Err(existing) => {
            objc::arc::release(created);
            existing.cast()
        }
    }
}

unsafe extern "C" fn assign(receiver: Id, _sel: Sel, value: Id, index: u64) {
    if !value.is_null() && !object::is_kind(value, c"MTLPipelineBufferDescriptor") {
        gpu::trace("pipeline buffer assignment requires a buffer descriptor");
        return;
    }
    // SAFETY: an array of this crate's; its slots are atomic.
    let array = unsafe { object::body_ref::<BufferArray>(receiver) };
    let Some(slot) = usize::try_from(index)
        .ok()
        .and_then(|index| array.elements.get(index))
    else {
        gpu::trace(format!(
            "pipeline buffer index {index} is outside 0..{BUFFER_COUNT}"
        ));
        return;
    };
    if slot.load(Ordering::Acquire) == value.cast() {
        return;
    }
    // SAFETY: a buffer descriptor checked above, or nil.
    let copy = unsafe { object_message(value, "copy") };
    objc::arc::release(slot.swap(copy.cast(), Ordering::AcqRel).cast());
}

pub(super) unsafe fn reset(array: Id) {
    if array.is_null() {
        return;
    }
    // SAFETY: the caller passes an array of this crate's.
    for slot in &unsafe { object::body_ref::<BufferArray>(array) }.elements {
        objc::arc::release(slot.swap(std::ptr::null_mut(), Ordering::AcqRel).cast());
    }
}

pub(super) unsafe fn copy_array(source: Id) -> Id {
    let result = new_array();
    if source.is_null() || result.is_null() {
        return result;
    }
    // SAFETY: the caller passes an array of this crate's, and `result` is one.
    let (source, destination) = unsafe {
        (
            object::body_ref::<BufferArray>(source),
            object::body_ref::<BufferArray>(result),
        )
    };
    for (source, destination) in source.elements.iter().zip(&destination.elements) {
        // SAFETY: each slot holds a buffer descriptor or nil.
        let copy = unsafe { object_message(source.load(Ordering::Acquire).cast(), "copy") };
        destination.store(copy.cast(), Ordering::Release);
    }
    result
}

unsafe extern "C" fn array_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: the array's last reference is going away.
    unsafe { reset(receiver) };
    objc::arc::finalize(receiver);
}

pub(super) fn install() {
    let descriptor = object::register::<BufferDescriptor>(c"MTLPipelineBufferDescriptor");
    methods!(descriptor, {
        "init" => descriptor_init, c"@16@0:8";
        "mutability" => mutability, c"Q16@0:8";
        "setMutability:" => set_mutability, c"v24@0:8Q16";
        "copyWithZone:" => descriptor_copy, c"@24@0:8^{_NSZone=}16";
    });
    let array = object::register::<BufferArray>(c"MTLPipelineBufferDescriptorArray");
    methods!(array, {
        "init" => array_init, c"@16@0:8";
        "objectAtIndexedSubscript:" => element, c"@24@0:8Q16";
        "setObject:atIndexedSubscript:" => assign, c"v32@0:8@16Q24";
        "dealloc" => array_dealloc, c"v16@0:8";
    });
}
