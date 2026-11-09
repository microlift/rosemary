//! `MTLComputePipelineDescriptor` and the descriptor-based creation path.

use objc::object::Id;
use objc::sel::Sel;

use crate::object::{self, methods};

pub(crate) struct ComputeDescriptor {
    function: Id,
    stage_input: Id,
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a freshly allocated descriptor.
    unsafe {
        object::init(
            receiver,
            ComputeDescriptor {
                function: std::ptr::null_mut(),
                stage_input: std::ptr::null_mut(),
            },
        )
    };
    receiver
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a descriptor `-init` established.
    let descriptor = unsafe { object::body::<ComputeDescriptor>(receiver) };
    objc::arc::release(std::mem::replace(
        &mut descriptor.function,
        std::ptr::null_mut(),
    ));
    objc::arc::release(std::mem::replace(
        &mut descriptor.stage_input,
        std::ptr::null_mut(),
    ));
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn object_value(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a descriptor of this crate's.
    let descriptor = unsafe { object::body::<ComputeDescriptor>(receiver) };
    // SAFETY: a selector the runtime owns.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"computeFunction" => descriptor.function,
        _ => descriptor.stage_input,
    }
}

unsafe extern "C" fn set_object_value(receiver: Id, sel: Sel, value: Id) {
    // SAFETY: a descriptor of this crate's.
    let descriptor = unsafe { object::body::<ComputeDescriptor>(receiver) };
    // SAFETY: a selector the runtime owns.
    let slot = match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"setComputeFunction:" => &mut descriptor.function,
        _ => &mut descriptor.stage_input,
    };
    objc::arc::release(std::mem::replace(slot, objc::arc::retain(value)));
}

/// `-[MTLDevice newComputePipelineStateWithDescriptor:options:reflection:error:]`.
///
/// No reflection object is made. A stage input descriptor describes a
/// kernel's `[[stage_in]]`, which no translated kernel has, so it has nothing
/// to configure.
pub(crate) unsafe extern "C" fn new_with_descriptor(
    device: Id,
    sel: Sel,
    descriptor: Id,
    _options: u64,
    reflection: *mut Id,
    error: *mut Id,
) -> Id {
    if !reflection.is_null() {
        // SAFETY: the guest passed a writable pointer.
        unsafe { *reflection = std::ptr::null_mut() };
    }
    let function = descriptor_function(descriptor);
    // SAFETY: the same call with the descriptor's function.
    unsafe { crate::device::new_compute_pipeline(device, sel, function, error) }
}

/// The function a compute pipeline descriptor names, or nil.
pub(crate) fn descriptor_function(descriptor: Id) -> Id {
    if object::is_kind(descriptor, c"MTLComputePipelineDescriptor") {
        // SAFETY: a descriptor of this crate's.
        unsafe { object::body::<ComputeDescriptor>(descriptor) }.function
    } else {
        std::ptr::null_mut()
    }
}

/// `-newComputePipelineStateWithFunction:options:reflection:error:`.
pub(crate) unsafe extern "C" fn new_with_function_reflection(
    device: Id,
    sel: Sel,
    function: Id,
    _options: u64,
    reflection: *mut Id,
    error: *mut Id,
) -> Id {
    if !reflection.is_null() {
        // SAFETY: the guest passed a writable pointer.
        unsafe { *reflection = std::ptr::null_mut() };
    }
    // SAFETY: the same call without reflection.
    unsafe { crate::device::new_compute_pipeline(device, sel, function, error) }
}

pub(super) fn install() {
    let class = object::register::<ComputeDescriptor>(c"MTLComputePipelineDescriptor");
    methods!(class, {
        "init" => init, c"@16@0:8";
        "reset" => init, c"@16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
        "computeFunction" => object_value, c"@16@0:8";
        "setComputeFunction:" => set_object_value, c"v24@0:8@16";
        "stageInputDescriptor" => object_value, c"@16@0:8";
        "setStageInputDescriptor:" => set_object_value, c"v24@0:8@16";
        // A hint for the compiler, which the driver makes its own choice
        // about.
        "setMaxTotalThreadsPerThreadgroup:" => crate::no_op, c"v24@0:8Q16";
        "setThreadGroupSizeIsMultipleOfThreadExecutionWidth:" => crate::no_op, c"v20@0:8c16";
        "label" => crate::nil, c"@16@0:8";
        "setLabel:" => crate::no_op, c"v24@0:8@16";
    });
}
