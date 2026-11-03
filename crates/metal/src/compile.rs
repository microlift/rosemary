//! The `completionHandler:` forms of library, function and pipeline creation.
//!
//! Each compiles on the calling thread, as its synchronous form does, and
//! hands the result to the handler on a global dispatch queue - Metal's
//! handlers run on a thread of its own, and a caller may be holding a lock
//! the handler takes.

use super::*;

/// DISPATCH_QUEUE_PRIORITY_DEFAULT.
const DISPATCH_QUEUE_PRIORITY_DEFAULT: i64 = 0;

/// The handler's arguments, each owned by this entry; addresses because an
/// `Id` is not `Send`.
struct Completion {
    block: usize,
    object: usize,
    error: usize,
    /// Whether the handler takes a reflection object between the two, which
    /// is always nil: no reflection was asked of the synchronous form.
    reflection: bool,
}

/// Run `block` with `object` (owned) and `error` (autoreleased, or nil).
///
/// # Safety
///
/// `block` is a block whose signature takes the result, optionally a nil
/// reflection, then the error.
unsafe fn complete(block: Id, object: Id, error: Id, reflection: bool) {
    // SAFETY: the guest passed a block.
    let block = unsafe { objc::shorthand::objc_retainBlock(block) };
    if block.is_null() {
        objc::arc::release(object);
        return;
    }
    let context = Box::into_raw(Box::new(Completion {
        block: block as usize,
        object: object as usize,
        error: objc::arc::retain(error) as usize,
        reflection,
    }));
    // SAFETY: a global queue, and `deliver` takes back the box it is handed.
    unsafe {
        let queue = darwin::dispatch::queue::get_global_queue(DISPATCH_QUEUE_PRIORITY_DEFAULT, 0);
        darwin::dispatch::queue::async_f(queue, context as u64, deliver as *const () as u64);
    }
}

unsafe extern "C" fn deliver(context: u64) {
    // SAFETY: the box `complete` leaked for this call.
    let completion = unsafe { Box::from_raw(context as *mut Completion) };
    let (block, object, error) = (
        completion.block as Id,
        completion.object as Id,
        completion.error as Id,
    );
    // SAFETY: a block `complete` copied, alive until released below.
    if let Some(invoke) = unsafe { objc::block::invoke(block) } {
        // SAFETY: the handler signatures `complete` was given.
        unsafe {
            if completion.reflection {
                let call: unsafe extern "C" fn(Id, Id, Id, Id) = std::mem::transmute(invoke);
                call(block, object, std::ptr::null_mut(), error);
            } else {
                let call: unsafe extern "C" fn(Id, Id, Id) = std::mem::transmute(invoke);
                call(block, object, error);
            }
        }
    }
    objc::block::release(block);
    objc::arc::release(object);
    objc::arc::release(error);
}

/// `-[MTLDevice newLibraryWithSource:options:completionHandler:]`.
pub(super) unsafe extern "C" fn new_library_with_source(
    receiver: Id,
    _sel: Sel,
    source: Id,
    options: Id,
    handler: Id,
) {
    let mut error = std::ptr::null_mut();
    let sel = objc::sel::get("newLibraryWithSource:options:error:");
    // SAFETY: the synchronous form, with a local error slot.
    let library =
        unsafe { library::new_library_with_source(receiver, sel, source, options, &mut error) };
    // SAFETY: `void (^)(id<MTLLibrary>, NSError *)`.
    unsafe { complete(handler, library, error, false) };
}

/// `-[MTLLibrary newFunctionWithName:constantValues:completionHandler:]`.
pub(super) unsafe extern "C" fn new_function_with_constants(
    receiver: Id,
    _sel: Sel,
    name: Id,
    constants: Id,
    handler: Id,
) {
    let mut error = std::ptr::null_mut();
    let sel = objc::sel::get("newFunctionWithName:constantValues:error:");
    // SAFETY: the synchronous form, with a local error slot.
    let function =
        unsafe { library::new_function_with_constants(receiver, sel, name, constants, &mut error) };
    // SAFETY: `void (^)(id<MTLFunction>, NSError *)`.
    unsafe { complete(handler, function, error, false) };
}

/// `-[MTLDevice newRenderPipelineStateWithDescriptor:completionHandler:]`.
pub(super) unsafe extern "C" fn new_render_pipeline(
    receiver: Id,
    _sel: Sel,
    descriptor: Id,
    handler: Id,
) {
    let mut error = std::ptr::null_mut();
    let sel = objc::sel::get("newRenderPipelineStateWithDescriptor:error:");
    // SAFETY: the synchronous form, with a local error slot.
    let state = unsafe { device::new_render_pipeline(receiver, sel, descriptor, &mut error) };
    // SAFETY: `void (^)(id<MTLRenderPipelineState>, NSError *)`.
    unsafe { complete(handler, state, error, false) };
}

/// `-[MTLDevice newComputePipelineStateWithDescriptor:options:completionHandler:]`.
pub(super) unsafe extern "C" fn new_compute_pipeline_with_descriptor(
    receiver: Id,
    _sel: Sel,
    descriptor: Id,
    options: u64,
    handler: Id,
) {
    let mut error = std::ptr::null_mut();
    let sel = objc::sel::get("newComputePipelineStateWithDescriptor:options:reflection:error:");
    // SAFETY: the synchronous form, with no reflection and a local error slot.
    let state = unsafe {
        pipeline::new_compute_pipeline_with_descriptor(
            receiver,
            sel,
            descriptor,
            options,
            std::ptr::null_mut(),
            &mut error,
        )
    };
    // SAFETY: `void (^)(id<MTLComputePipelineState>,
    // MTLComputePipelineReflection *, NSError *)`.
    unsafe { complete(handler, state, error, true) };
}
