//! The `completionHandler:` forms of library, function and pipeline creation.
//!
//! Each reads its arguments on the calling thread, as Metal copies them when
//! the call is made, then compiles on a global dispatch queue and hands the
//! result to the handler there. The caller is free at once, which is what a
//! game making its pipelines this way is after.

use super::*;

/// DISPATCH_QUEUE_PRIORITY_DEFAULT.
const DISPATCH_QUEUE_PRIORITY_DEFAULT: i64 = 0;

/// One creation in flight: the work, and the handler it is delivered to.
struct Creation {
    /// Makes the object (owned, or nil) and fills in the error slot.
    compile: Box<dyn FnOnce(&mut Id) -> Id + Send>,
    block: usize,
    /// Whether the handler takes a reflection object between the two, which
    /// is always nil: no reflection was asked of the synchronous form.
    reflection: bool,
}

/// Retains an object for work on another thread; an `Id` is not `Send`.
struct Held(usize);

impl Held {
    fn new(object: Id) -> Self {
        Self(objc::arc::retain(object) as usize)
    }

    /// Hold an object the caller owns, without another reference.
    fn owned(object: Id) -> Self {
        Self(object as usize)
    }

    fn get(&self) -> Id {
        self.0 as Id
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        objc::arc::release(self.0 as Id);
    }
}

/// Run `compile` on a global queue, then `block` with what it made.
///
/// # Safety
///
/// `block` is a block whose signature takes the result, optionally a nil
/// reflection, then the error.
unsafe fn create(
    block: Id,
    reflection: bool,
    compile: impl FnOnce(&mut Id) -> Id + Send + 'static,
) {
    // SAFETY: the guest passed a block.
    let block = unsafe { objc::shorthand::objc_retainBlock(block) };
    let context = Box::into_raw(Box::new(Creation {
        compile: Box::new(compile),
        block: block as usize,
        reflection,
    }));
    // SAFETY: a global queue, and `deliver` takes back the box it is handed.
    unsafe {
        let queue = darwin::dispatch::queue::get_global_queue(DISPATCH_QUEUE_PRIORITY_DEFAULT, 0);
        darwin::dispatch::queue::async_f(queue, context as u64, deliver as *const () as u64);
    }
}

unsafe extern "C" fn deliver(context: u64) {
    // SAFETY: the box `create` leaked for this call.
    let creation = unsafe { Box::from_raw(context as *mut Creation) };
    let pool = objc::arc::pool_push();
    let mut error = std::ptr::null_mut();
    let object = (creation.compile)(&mut error);
    let block = creation.block as Id;
    // SAFETY: a block `create` copied, alive until released below; a nil one
    // is still compiled for, as a guest may rely on the object being made.
    if let Some(invoke) = (!block.is_null())
        .then(|| unsafe { objc::block::invoke(block) })
        .flatten()
    {
        // SAFETY: the handler signatures `create` was given.
        unsafe {
            if creation.reflection {
                let call: unsafe extern "C" fn(Id, Id, Id, Id) = std::mem::transmute(invoke);
                darwin::call_as_darwin!(call(block, object, std::ptr::null_mut(), error));
            } else {
                let call: unsafe extern "C" fn(Id, Id, Id) = std::mem::transmute(invoke);
                darwin::call_as_darwin!(call(block, object, error));
            }
        }
    }
    if !block.is_null() {
        objc::block::release(block);
    }
    objc::arc::release(object);
    objc::arc::pool_pop(pool);
}

/// `-[MTLDevice newLibraryWithSource:options:completionHandler:]`.
pub(super) unsafe extern "C" fn new_library_with_source(
    receiver: Id,
    _sel: Sel,
    source: Id,
    options: Id,
    handler: Id,
) {
    // SAFETY: a guest `NSString`, and an options object or nil.
    let (source, macros) = unsafe { library::source_inputs(source, options) };
    let device = Held::new(receiver);
    // SAFETY: `void (^)(id<MTLLibrary>, NSError *)`.
    unsafe {
        create(handler, false, move |error| {
            // SAFETY: a device, held until the library is made.
            library::library_from_source(device.get(), source.as_deref(), &macros, error)
        })
    };
}

/// `-[MTLLibrary newFunctionWithName:constantValues:completionHandler:]`.
pub(super) unsafe extern "C" fn new_function_with_constants(
    receiver: Id,
    _sel: Sel,
    name: Id,
    constants: Id,
    handler: Id,
) {
    // SAFETY: a guest `NSString`, and a constant values object or nil.
    let (name, values) = unsafe { library::constant_inputs(name, constants) };
    let library = Held::new(receiver);
    // SAFETY: `void (^)(id<MTLFunction>, NSError *)`.
    unsafe {
        create(handler, false, move |error| {
            // SAFETY: a library, held until the function is made.
            library::function_with_constants(library.get(), &name, values, error)
        })
    };
}

/// `-[MTLDevice newRenderPipelineStateWithDescriptor:completionHandler:]`.
pub(super) unsafe extern "C" fn new_render_pipeline(
    receiver: Id,
    _sel: Sel,
    descriptor: Id,
    handler: Id,
) {
    let snapshot = if descriptor.is_null() {
        std::ptr::null_mut()
    } else {
        // SAFETY: `MTLRenderPipelineDescriptor` is this crate's class.
        unsafe { pipeline::copy_render_pipeline_descriptor(descriptor) }
    };
    let descriptor = Held::owned(snapshot);
    let device = Held::new(receiver);
    // SAFETY: `void (^)(id<MTLRenderPipelineState>, NSError *)`.
    unsafe {
        create(handler, false, move |error| {
            let sel = objc::sel::get("newRenderPipelineStateWithDescriptor:error:");
            // SAFETY: the synchronous form, over a descriptor nothing else
            // can change.
            device::new_render_pipeline(device.get(), sel, descriptor.get(), error)
        })
    };
}

/// `-[MTLDevice newComputePipelineStateWithDescriptor:options:completionHandler:]`.
pub(super) unsafe extern "C" fn new_compute_pipeline_with_descriptor(
    receiver: Id,
    _sel: Sel,
    descriptor: Id,
    _options: u64,
    handler: Id,
) {
    let function = Held::new(pipeline::compute_descriptor_function(descriptor));
    let device = Held::new(receiver);
    // SAFETY: `void (^)(id<MTLComputePipelineState>,
    // MTLComputePipelineReflection *, NSError *)`.
    unsafe {
        create(handler, true, move |error| {
            let sel =
                objc::sel::get("newComputePipelineStateWithDescriptor:options:reflection:error:");
            // SAFETY: the synchronous form with the descriptor's function as
            // it was when the call was made.
            device::new_compute_pipeline(device.get(), sel, function.get(), error)
        })
    };
}
