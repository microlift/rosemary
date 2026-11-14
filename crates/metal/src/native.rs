//! Experimental guest-object bridge to the host macOS Metal framework.
//! Host Objective-C objects never cross into the built-in guest runtime.

use super::*;

mod blit;
mod callback;
mod compute;
mod depth;
mod layer;
mod render;
mod sampler;
mod texture;
use std::ffi::{CStr, c_char, c_void};

type HostId = *mut c_void;
type HostSel = *const c_void;

#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    #[link_name = "MTLCopyAllDevices"]
    fn host_copy_all_devices() -> HostId;
    #[link_name = "MTLCreateSystemDefaultDevice"]
    fn host_create_default_device() -> HostId;
}

// Native host libdispatch, not the guest's reimplemented dispatch_data_t.
// A NULL destructor requests a fully owned copy of the input buffer.
unsafe extern "C" {
    #[link_name = "dispatch_data_create"]
    fn host_dispatch_data_create(
        bytes: *const c_void,
        length: usize,
        queue: *mut c_void,
        destructor: *mut c_void,
    ) -> HostId;
    #[link_name = "dispatch_release"]
    fn host_dispatch_release(data: HostId);
}

#[link(name = "objc")]
unsafe extern "C" {
    #[link_name = "objc_getClass"]
    fn host_get_class(name: *const c_char) -> HostId;
    #[link_name = "sel_registerName"]
    fn host_sel_register_name(name: *const c_char) -> HostSel;
    #[link_name = "objc_msgSend"]
    fn host_msg_send();
    #[link_name = "objc_retain"]
    fn host_retain(object: HostId) -> HostId;
    #[link_name = "objc_release"]
    fn host_release(object: HostId);
    #[link_name = "objc_autoreleasePoolPush"]
    fn host_pool_push() -> *mut c_void;
    #[link_name = "objc_autoreleasePoolPop"]
    fn host_pool_pop(token: *mut c_void);
}

// Each selector is dispatched with its declared Objective-C method ABI.
// A single symbol declaration avoids conflicting extern signatures.
macro_rules! native_send {
    ($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty) => {
        unsafe fn $name(receiver: HostId, selector: HostSel, $($arg: $ty),*) -> $ret {
            let send: unsafe extern "C" fn(HostId, HostSel, $($ty),*) -> $ret =
                unsafe { std::mem::transmute(host_msg_send as unsafe extern "C" fn()) };
            unsafe { send(receiver, selector, $($arg),*) }
        }
    };
}

native_send!(host_send_id() -> HostId);
native_send!(host_send_id_id(argument: HostId) -> HostId);
native_send!(host_send_id_ptr(argument: *const c_void) -> HostId);
native_send!(host_send_id_id_ptr(argument: HostId, error: *mut HostId) -> HostId);
native_send!(host_send_id_id_id_ptr(source: HostId, options: HostId, error: *mut HostId) -> HostId);
native_send!(host_send_id_u64(value: u64) -> HostId);
native_send!(host_send_id_u64_u64(first: u64, second: u64) -> HostId);
native_send!(host_send_id_ptr_u64_u64(bytes: *const u8, length: u64, options: u64) -> HostId);
native_send!(host_send_u64() -> u64);
native_send!(host_send_bool() -> bool);
native_send!(host_send_bool_u64(value: u64) -> bool);
native_send!(host_send_bool_sel(value: HostSel) -> bool);
native_send!(host_send_ptr() -> *mut c_void);
native_send!(host_send_void() -> ());
native_send!(host_send_void_u64(value: u64) -> ());
native_send!(host_send_void_f32(value: f32) -> ());
native_send!(host_send_void_f64(value: f64) -> ());
native_send!(host_send_void_f32_f32_f32(first: f32, second: f32, third: f32) -> ());
native_send!(host_send_void_f32_f32_f32_f32(first: f32, second: f32, third: f32, fourth: f32) -> ());
native_send!(host_send_void_id_u64(argument: HostId, index: u64) -> ());
native_send!(host_send_void_u64_u64_u64(first: u64, second: u64, third: u64) -> ());
native_send!(host_send_void_u64_u64(first: u64, second: u64) -> ());
native_send!(host_send_void_u64_u64_u64_u64(first: u64, second: u64, third: u64, fourth: u64) -> ());
native_send!(host_send_void_u64_u64_u64_u64_u64(first: u64, second: u64, third: u64, fourth: u64, fifth: u64) -> ());
native_send!(host_send_void_indexed(
    primitive: u64, index_count: u64, index_type: u64, index_buffer: HostId,
    index_offset: u64, instance_count: u64
) -> ());
native_send!(host_send_void_indexed_base(
    primitive: u64, index_count: u64, index_type: u64, index_buffer: HostId,
    index_offset: u64, instance_count: u64, base_vertex: i64, base_instance: u64
) -> ());
native_send!(host_send_void_clear_color(color: encoder::ClearColor) -> ());
native_send!(host_send_void_viewport(viewport: encoder::Viewport) -> ());
native_send!(host_send_void_scissor(rect: encoder::ScissorRect) -> ());
native_send!(host_send_void_id(argument: HostId) -> ());
native_send!(host_send_void_id_id(first: HostId, second: HostId) -> ());
native_send!(host_send_void_size(size: foundation::geometry::NSSize) -> ());
native_send!(host_send_void_bool(value: bool) -> ());
native_send!(host_send_void_id_u64_u64(argument: HostId, offset: u64, index: u64) -> ());
native_send!(host_send_void_ptr_u64_u64(bytes: *const c_void, length: u64, index: u64) -> ());
native_send!(host_send_void_size_size(groups: encoder::MtlSize, threads: encoder::MtlSize) -> ());
native_send!(host_send_void_region_u64_ptr_u64(region: texture::Region, mip_level: u64, bytes: *const c_void, bytes_per_row: u64) -> ());
native_send!(host_send_void_region_u64_u64_ptr_u64_u64(
    region: texture::Region, mip_level: u64, slice: u64,
    bytes: *const c_void, bytes_per_row: u64, bytes_per_image: u64
) -> ());
native_send!(host_send_void_ptr_u64_u64_region_u64_u64(
    bytes: *mut c_void, bytes_per_row: u64, bytes_per_image: u64,
    region: texture::Region, mip_level: u64, slice: u64
) -> ());
native_send!(host_send_void_ptr_u64_region_u64(bytes: *mut c_void, bytes_per_row: u64, region: texture::Region, mip_level: u64) -> ());
native_send!(host_send_void_id_u64_id_u64_u64(source: HostId, source_offset: u64, destination: HostId, destination_offset: u64, size: u64) -> ());
native_send!(host_send_void_texture_to_texture(
    source: HostId, source_slice: u64, source_level: u64,
    source_origin: encoder::MtlOrigin, size: encoder::MtlSize,
    destination: HostId, destination_slice: u64, destination_level: u64,
    destination_origin: encoder::MtlOrigin
) -> ());
native_send!(host_send_void_texture_to_buffer(
    texture: HostId,
    slice: u64,
    mip_level: u64,
    origin: encoder::MtlOrigin,
    size: encoder::MtlSize,
    buffer: HostId,
    offset: u64,
    bytes_per_row: u64,
    bytes_per_image: u64
) -> ());

struct HostPool(*mut c_void);

impl HostPool {
    fn new() -> Self {
        Self(unsafe { host_pool_push() })
    }
}

impl Drop for HostPool {
    fn drop(&mut self) {
        unsafe { host_pool_pop(self.0) };
    }
}

fn sel(name: &CStr) -> HostSel {
    unsafe { host_sel_register_name(name.as_ptr()) }
}

struct NativeDevice {
    host: HostId,
    name: Id,
}

struct NativeQueue {
    host: HostId,
    device: Id,
}

struct NativeBuffer {
    host: HostId,
    device: Id,
}

struct NativeCommandBuffer {
    host: HostId,
    queue: Id,
    scheduled_handlers: Vec<usize>,
    completed_handlers: Vec<usize>,
}

pub(super) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("ROSEMARY_METAL_BACKEND").as_deref() == Ok("native"))
}

fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::bootstrap();
        let device = object::register::<NativeDevice>(c"RosemaryNativeMTLDevice");
        let queue = object::register::<NativeQueue>(c"RosemaryNativeMTLCommandQueue");
        let buffer = object::register::<NativeBuffer>(c"RosemaryNativeMTLBuffer");
        let command = object::register::<NativeCommandBuffer>(c"RosemaryNativeMTLCommandBuffer");
        methods!(device, {
            "name" => device_name, c"@16@0:8";
            "registryID" => device_registry_id, c"Q16@0:8";
            "hasUnifiedMemory" => device_unified, c"B16@0:8";
            "isLowPower" => device_low_power, c"B16@0:8";
            "isHeadless" => device_headless, c"B16@0:8";
            "recommendedMaxWorkingSetSize" => device_working_set, c"Q16@0:8";
            "maxBufferLength" => device_max_buffer_length, c"Q16@0:8";
            "supportsFamily:" => device_supports_family, c"B24@0:8Q16";
            "supportsFeatureSet:" => device_supports_feature_set, c"B24@0:8Q16";
            "supportsTextureSampleCount:" => device_supports_texture_sample_count, c"B24@0:8Q16";
            "argumentBuffersSupport" => device_argument_buffers_support, c"Q16@0:8";
            "isDepth24Stencil8PixelFormatSupported" => device_depth24_stencil8_supported, c"B16@0:8";
            "newCommandQueue" => new_queue, c"@16@0:8";
            "newCommandQueueWithMaxCommandBufferCount:" => new_queue_with_count, c"@24@0:8Q16";
            "newBufferWithLength:options:" => new_buffer, c"@32@0:8Q16Q24";
            "newBufferWithBytes:length:options:" => new_buffer_with_bytes, c"@40@0:8^v16Q24Q32";
            "newTextureWithDescriptor:" => texture::new_texture, c"@24@0:8@16";
            "newSamplerStateWithDescriptor:" => sampler::new_sampler, c"@24@0:8@16";
            "newDepthStencilStateWithDescriptor:" => depth::new_state, c"@24@0:8@16";
            "newLibraryWithSource:options:error:" => compute::new_library_with_source,
                c"@40@0:8@16@24^@32";
            "newDefaultLibrary" => compute::new_default_library, c"@16@0:8";
            "newLibraryWithData:error:" => compute::new_library_with_data, c"@32@0:8@16^@24";

            "newComputePipelineStateWithFunction:error:" => compute::new_compute_pipeline,
                c"@32@0:8@16^@24";
            "newRenderPipelineStateWithDescriptor:error:" => render::new_render_pipeline,
                c"@32@0:8@16^@24";
            "dealloc" => device_dealloc, c"v16@0:8";
        });
        methods!(queue, {
            "device" => queue_device, c"@16@0:8";
            "commandBuffer" => new_command_buffer, c"@16@0:8";
            "commandBufferWithUnretainedReferences" => new_command_buffer_with_unretained_references, c"@16@0:8";
            "dealloc" => queue_dealloc, c"v16@0:8";
        });
        methods!(buffer, {
            "device" => buffer_device, c"@16@0:8";
            "contents" => buffer_contents, c"^v16@0:8";
            "length" => buffer_length, c"Q16@0:8";
            "storageMode" => buffer_storage_mode, c"Q16@0:8";
            "dealloc" => buffer_dealloc, c"v16@0:8";
        });
        blit::install();
        compute::install();
        render::install();
        depth::install();
        sampler::install();
        texture::install();
        layer::install();
        methods!(command, {
            "device" => command_device, c"@16@0:8";
            "commandQueue" => command_queue, c"@16@0:8";
            "commit" => command_commit, c"v16@0:8";
            "waitUntilCompleted" => command_wait, c"v16@0:8";
            "addCompletedHandler:" => add_command_handler, c"v24@0:8@?16";
            "addScheduledHandler:" => add_command_handler, c"v24@0:8@?16";
            "status" => command_status, c"Q16@0:8";
            "presentDrawable:" => layer::present_drawable, c"v24@0:8@16";
            "blitCommandEncoder" => blit::new_blit_encoder, c"@16@0:8";
            "computeCommandEncoder" => compute::new_compute_encoder, c"@16@0:8";
            "renderCommandEncoderWithDescriptor:" => render::new_render_encoder, c"@24@0:8@16";
            "dealloc" => command_dealloc, c"v16@0:8";
        });
    });
}

// The caller transfers one host retain to the guest-facing wrapper.
fn wrap_device(host: HostId) -> Id {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let _pool = HostPool::new();
    let native_name = unsafe { host_send_id(host, sel(c"name")) };
    let name = if native_name.is_null() {
        std::ptr::null_mut()
    } else {
        let utf8 = unsafe { host_send_ptr(native_name, sel(c"UTF8String")) };
        if utf8.is_null() {
            std::ptr::null_mut()
        } else {
            let name = unsafe { CStr::from_ptr(utf8.cast()) };
            foundation::string::from_str(&name.to_string_lossy())
        }
    };
    let guest = object::make(c"RosemaryNativeMTLDevice", NativeDevice { host, name });
    if guest.is_null() {
        objc::arc::release(name);
        unsafe { host_release(host) };
    }
    guest
}

fn wrap_queue(host: HostId, device: Id) -> Id {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    objc::arc::retain(device);
    let guest = object::make(
        c"RosemaryNativeMTLCommandQueue",
        NativeQueue { host, device },
    );
    if guest.is_null() {
        objc::arc::release(device);
        unsafe { host_release(host) };
    }
    guest
}

fn wrap_buffer(host: HostId, device: Id) -> Id {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    objc::arc::retain(device);
    let guest = object::make(c"RosemaryNativeMTLBuffer", NativeBuffer { host, device });
    if guest.is_null() {
        objc::arc::release(device);
        unsafe { host_release(host) };
    }
    guest
}

pub(super) fn devices() -> &'static [usize] {
    static DEVICES: OnceLock<Vec<usize>> = OnceLock::new();
    DEVICES.get_or_init(|| {
        bootstrap();
        let _pool = HostPool::new();
        let array = unsafe { host_copy_all_devices() };
        if array.is_null() {
            return Vec::new();
        }
        let count = unsafe { host_send_u64(array, sel(c"count")) };
        let mut devices = Vec::with_capacity(count as usize);
        for i in 0..count {
            let host = unsafe { host_send_id_u64(array, sel(c"objectAtIndex:"), i) };
            if !host.is_null() {
                let retained = unsafe { host_retain(host) };
                let guest = wrap_device(retained);
                if !guest.is_null() {
                    devices.push(guest as usize);
                }
            }
        }
        unsafe { host_release(array) };
        devices
    })
}

pub(super) fn default_device() -> Id {
    static DEFAULT: OnceLock<usize> = OnceLock::new();
    *DEFAULT.get_or_init(|| {
        bootstrap();
        let _pool = HostPool::new();
        let default = unsafe { host_create_default_device() };
        if default.is_null() {
            return 0;
        }
        if let Some(&guest) = devices().iter().find(|&&guest| {
            let wrapped = unsafe { object::body::<NativeDevice>(guest as Id) };
            wrapped.host == default
        }) {
            unsafe { host_release(default) };
            guest
        } else {
            wrap_device(default) as usize
        }
    }) as Id
}

unsafe extern "C" fn device_name(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeDevice>(receiver) }.name
}

unsafe extern "C" fn device_registry_id(receiver: Id, _sel: Sel) -> u64 {
    let _pool = HostPool::new();
    unsafe {
        host_send_u64(
            object::body::<NativeDevice>(receiver).host,
            sel(c"registryID"),
        )
    }
}
unsafe extern "C" fn device_working_set(receiver: Id, _sel: Sel) -> u64 {
    let _pool = HostPool::new();
    unsafe {
        host_send_u64(
            object::body::<NativeDevice>(receiver).host,
            sel(c"recommendedMaxWorkingSetSize"),
        )
    }
}
unsafe extern "C" fn device_max_buffer_length(receiver: Id, _sel: Sel) -> u64 {
    let _pool = HostPool::new();
    unsafe {
        host_send_u64(
            object::body::<NativeDevice>(receiver).host,
            sel(c"maxBufferLength"),
        )
    }
}
unsafe extern "C" fn device_unified(receiver: Id, _sel: Sel) -> bool {
    let _pool = HostPool::new();
    unsafe {
        host_send_bool(
            object::body::<NativeDevice>(receiver).host,
            sel(c"hasUnifiedMemory"),
        )
    }
}
unsafe extern "C" fn device_low_power(receiver: Id, _sel: Sel) -> bool {
    let _pool = HostPool::new();
    unsafe {
        host_send_bool(
            object::body::<NativeDevice>(receiver).host,
            sel(c"isLowPower"),
        )
    }
}
unsafe extern "C" fn device_headless(receiver: Id, _sel: Sel) -> bool {
    let _pool = HostPool::new();
    unsafe {
        host_send_bool(
            object::body::<NativeDevice>(receiver).host,
            sel(c"isHeadless"),
        )
    }
}
/// The deprecated MTLFeatureSet check is still used by older Metal clients
/// such as Roblox. Ask the host driver directly; it may report a different
/// capability set on each supported device generation.
unsafe extern "C" fn device_supports_feature_set(receiver: Id, _sel: Sel, feature: u64) -> bool {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeDevice>(receiver) }.host;
    let query = sel(c"supportsFeatureSet:");
    if !unsafe { host_send_bool_sel(host, sel(c"respondsToSelector:"), query) } {
        gpu::trace("native Metal driver does not implement supportsFeatureSet:".to_string());
        return false;
    }
    unsafe { host_send_bool_u64(host, query, feature) }
}

unsafe extern "C" fn device_supports_texture_sample_count(
    receiver: Id,
    _sel: Sel,
    count: u64,
) -> bool {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeDevice>(receiver) }.host;
    unsafe { host_send_bool_u64(host, sel(c"supportsTextureSampleCount:"), count) }
}

unsafe extern "C" fn device_depth24_stencil8_supported(receiver: Id, _sel: Sel) -> bool {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeDevice>(receiver) }.host;
    let query = sel(c"isDepth24Stencil8PixelFormatSupported");
    if !unsafe { host_send_bool_sel(host, sel(c"respondsToSelector:"), query) } {
        gpu::trace("native Metal driver lacks depth24/stencil8 capability query".to_string());
        return false;
    }
    unsafe { host_send_bool(host, query) }
}

unsafe extern "C" fn device_argument_buffers_support(receiver: Id, _sel: Sel) -> u64 {
    let _pool = HostPool::new();
    let host = unsafe { object::body::<NativeDevice>(receiver) }.host;
    unsafe { host_send_u64(host, sel(c"argumentBuffersSupport")) }
}

unsafe extern "C" fn device_supports_family(receiver: Id, _sel: Sel, family: u64) -> bool {
    let _pool = HostPool::new();
    unsafe {
        host_send_bool_u64(
            object::body::<NativeDevice>(receiver).host,
            sel(c"supportsFamily:"),
            family,
        )
    }
}
unsafe extern "C" fn device_dealloc(receiver: Id, _sel: Sel) {
    let device = unsafe { object::body::<NativeDevice>(receiver) };
    objc::arc::release(device.name);
    unsafe { host_release(device.host) };
    objc::arc::finalize(receiver);
}
unsafe extern "C" fn new_queue(receiver: Id, _sel: Sel) -> Id {
    let _pool = HostPool::new();
    let host = unsafe {
        host_send_id(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newCommandQueue"),
        )
    };
    wrap_queue(host, receiver)
}
unsafe extern "C" fn new_queue_with_count(receiver: Id, _sel: Sel, count: u64) -> Id {
    let _pool = HostPool::new();
    let host = unsafe {
        host_send_id_u64(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newCommandQueueWithMaxCommandBufferCount:"),
            count,
        )
    };
    wrap_queue(host, receiver)
}
unsafe extern "C" fn new_buffer(receiver: Id, _sel: Sel, length: usize, options: u64) -> Id {
    let _pool = HostPool::new();
    let host = unsafe {
        host_send_id_u64_u64(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newBufferWithLength:options:"),
            length as u64,
            options,
        )
    };
    wrap_buffer(host, receiver)
}
unsafe extern "C" fn new_buffer_with_bytes(
    receiver: Id,
    _sel: Sel,
    bytes: *const u8,
    length: usize,
    options: u64,
) -> Id {
    if bytes.is_null() && length != 0 {
        return std::ptr::null_mut();
    }
    let _pool = HostPool::new();
    let host = unsafe {
        host_send_id_ptr_u64_u64(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newBufferWithBytes:length:options:"),
            bytes,
            length as u64,
            options,
        )
    };
    wrap_buffer(host, receiver)
}
unsafe extern "C" fn queue_device(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeQueue>(receiver) }.device
}
unsafe extern "C" fn queue_dealloc(receiver: Id, _sel: Sel) {
    let queue = unsafe { object::body::<NativeQueue>(receiver) };
    unsafe { host_release(queue.host) };
    objc::arc::release(queue.device);
    objc::arc::finalize(receiver);
}
unsafe extern "C" fn new_command_buffer(receiver: Id, _sel: Sel) -> Id {
    make_command_buffer(receiver, c"commandBuffer")
}

unsafe extern "C" fn new_command_buffer_with_unretained_references(receiver: Id, _sel: Sel) -> Id {
    let queue = unsafe { object::body::<NativeQueue>(receiver) };
    let requested = sel(c"commandBufferWithUnretainedReferences");
    let _pool = HostPool::new();
    if !unsafe { host_send_bool_sel(queue.host, sel(c"respondsToSelector:"), requested) } {
        // Older host Metal implementations can lack the unretained variant.
        // Retaining instead is safe, at the cost of a longer resource lifetime.
        return make_command_buffer(receiver, c"commandBuffer");
    }
    make_command_buffer(receiver, c"commandBufferWithUnretainedReferences")
}

fn make_command_buffer(receiver: Id, selector: &CStr) -> Id {
    let _pool = HostPool::new();
    let queue = unsafe { object::body::<NativeQueue>(receiver) };
    let host = unsafe { host_send_id(queue.host, sel(selector)) };
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let host = unsafe { host_retain(host) };
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeMTLCommandBuffer",
        NativeCommandBuffer {
            host,
            queue: receiver,
            scheduled_handlers: Vec::new(),
            completed_handlers: Vec::new(),
        },
    );
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(receiver);
    }
    objc::arc::autorelease(guest)
}
unsafe extern "C" fn buffer_device(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeBuffer>(receiver) }.device
}
unsafe extern "C" fn buffer_length(receiver: Id, _sel: Sel) -> u64 {
    let _pool = HostPool::new();
    unsafe { host_send_u64(object::body::<NativeBuffer>(receiver).host, sel(c"length")) }
}
unsafe extern "C" fn buffer_storage_mode(receiver: Id, _sel: Sel) -> u64 {
    let _pool = HostPool::new();
    unsafe {
        host_send_u64(
            object::body::<NativeBuffer>(receiver).host,
            sel(c"storageMode"),
        )
    }
}
unsafe extern "C" fn buffer_contents(receiver: Id, _sel: Sel) -> *mut c_void {
    let _pool = HostPool::new();
    unsafe {
        host_send_ptr(
            object::body::<NativeBuffer>(receiver).host,
            sel(c"contents"),
        )
    }
}
unsafe extern "C" fn buffer_dealloc(receiver: Id, _sel: Sel) {
    let buffer = unsafe { object::body::<NativeBuffer>(receiver) };
    unsafe { host_release(buffer.host) };
    objc::arc::release(buffer.device);
    objc::arc::finalize(receiver);
}
unsafe extern "C" fn command_queue(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeCommandBuffer>(receiver) }.queue
}
unsafe extern "C" fn command_device(receiver: Id, _sel: Sel) -> Id {
    let queue = unsafe { object::body::<NativeCommandBuffer>(receiver) }.queue;
    unsafe { object::body::<NativeQueue>(queue) }.device
}
unsafe extern "C" fn add_command_handler(receiver: Id, method: Sel, block: Id) {
    if block.is_null() {
        return;
    }
    let copied = objc::block::copy(block);
    if copied.is_null() {
        return;
    }
    let scheduled = unsafe { CStr::from_ptr(method) }.to_bytes() == b"addScheduledHandler:";
    let command = unsafe { object::body::<NativeCommandBuffer>(receiver) };
    if scheduled {
        command.scheduled_handlers.push(copied as usize);
    } else {
        command.completed_handlers.push(copied as usize);
    }
}

unsafe extern "C" fn command_commit(receiver: Id, _sel: Sel) {
    let _pool = HostPool::new();
    let command = unsafe { object::body::<NativeCommandBuffer>(receiver) };
    let host = command.host;
    for block in std::mem::take(&mut command.scheduled_handlers) {
        callback::register(host, receiver, block as Id, true);
    }
    for block in std::mem::take(&mut command.completed_handlers) {
        callback::register(host, receiver, block as Id, false);
    }
    unsafe { host_send_void(host, sel(c"commit")) };
}
unsafe extern "C" fn command_wait(receiver: Id, _sel: Sel) {
    let _pool = HostPool::new();
    unsafe {
        host_send_void(
            object::body::<NativeCommandBuffer>(receiver).host,
            sel(c"waitUntilCompleted"),
        )
    };
}
unsafe extern "C" fn command_status(receiver: Id, _sel: Sel) -> u64 {
    let _pool = HostPool::new();
    unsafe {
        host_send_u64(
            object::body::<NativeCommandBuffer>(receiver).host,
            sel(c"status"),
        )
    }
}
unsafe extern "C" fn command_dealloc(receiver: Id, _sel: Sel) {
    let command = unsafe { object::body::<NativeCommandBuffer>(receiver) };
    for block in command
        .scheduled_handlers
        .drain(..)
        .chain(command.completed_handlers.drain(..))
    {
        objc::block::release(block as Id);
    }
    unsafe { host_release(command.host) };
    objc::arc::release(command.queue);
    unsafe { std::ptr::drop_in_place(command as *mut NativeCommandBuffer) };
    objc::arc::finalize(receiver);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Never install native CAMetalLayer dispatch while testing the default
    /// Vulkan-backed Metal implementation in the same process.
    fn require_native_opt_in() -> bool {
        if enabled() {
            return true;
        }
        eprintln!("skipped: set ROSEMARY_METAL_BACKEND=native for host Metal tests");
        false
    }

    #[test]
    fn host_metal_queue_and_shared_buffer_roundtrip() {
        if !require_native_opt_in() {
            return;
        }
        const MTL_RESOURCE_STORAGE_MODE_SHARED: u64 = 0;
        const MTL_COMMAND_BUFFER_STATUS_COMPLETED: u64 = 4;
        let device = *devices()
            .first()
            .expect("macOS must provide a Metal device") as Id;
        assert!(!default_device().is_null());
        assert!(!unsafe { device_name(device, std::ptr::null()) }.is_null());
        let buffer = unsafe {
            new_buffer(
                device,
                std::ptr::null(),
                32,
                MTL_RESOURCE_STORAGE_MODE_SHARED,
            )
        };
        assert!(!buffer.is_null());
        assert_eq!(unsafe { buffer_length(buffer, std::ptr::null()) }, 32);
        let contents = unsafe { buffer_contents(buffer, std::ptr::null()) } as *mut u32;
        assert!(!contents.is_null());
        unsafe { *contents = 0x1234_abcd };
        assert_eq!(unsafe { *contents }, 0x1234_abcd);
        let queue = unsafe { new_queue(device, std::ptr::null()) };
        assert!(!queue.is_null());
        let guest_pool = objc::arc::pool_push();
        let command = unsafe { new_command_buffer(queue, std::ptr::null()) };
        assert!(!command.is_null());
        unsafe {
            command_commit(command, std::ptr::null());
            command_wait(command, std::ptr::null());
        }
        assert_eq!(
            unsafe { command_status(command, std::ptr::null()) },
            MTL_COMMAND_BUFFER_STATUS_COMPLETED
        );
        objc::arc::pool_pop(guest_pool);
        objc::arc::release(queue);
        objc::arc::release(buffer);
    }

    // Exercise the built-in objc_msgSend trampoline, not the Rust method
    // implementations, so this catches missing registrations and ABI mismatches.
    #[test]
    fn builtin_objc_dispatches_to_host_metal() {
        if !require_native_opt_in() {
            return;
        }
        const MTL_RESOURCE_STORAGE_MODE_SHARED: u64 = 0;
        const MTL_COMMAND_BUFFER_STATUS_COMPLETED: u64 = 4;
        let device = if enabled() {
            unsafe { crate::MTLCreateSystemDefaultDevice() }
        } else {
            default_device()
        };
        assert!(!device.is_null());
        assert!(object::is_kind(device, c"RosemaryNativeMTLDevice"));

        let guest_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let guest_u64: unsafe extern "C" fn(Id, Sel) -> u64 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let guest_ptr: unsafe extern "C" fn(Id, Sel) -> *mut c_void =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let guest_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let guest_buffer: unsafe extern "C" fn(Id, Sel, u64, u64) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let guest_buffer_bytes: unsafe extern "C" fn(Id, Sel, *const u32, u64, u64) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };

        let pool = objc::arc::pool_push();
        let name = unsafe { guest_id(device, objc::sel::get("name")) };
        assert!(!name.is_null());
        assert!(object::is_kind(name, c"NSString"));

        let queue = unsafe { guest_id(device, objc::sel::get("newCommandQueue")) };
        assert!(!queue.is_null());
        assert!(object::is_kind(queue, c"RosemaryNativeMTLCommandQueue"));
        assert_eq!(unsafe { guest_id(queue, objc::sel::get("device")) }, device);

        let buffer = unsafe {
            guest_buffer(
                device,
                objc::sel::get("newBufferWithLength:options:"),
                64,
                MTL_RESOURCE_STORAGE_MODE_SHARED,
            )
        };
        assert!(!buffer.is_null());
        assert_eq!(unsafe { guest_u64(buffer, objc::sel::get("length")) }, 64);
        let contents = unsafe { guest_ptr(buffer, objc::sel::get("contents")) } as *mut u32;
        assert!(!contents.is_null());
        unsafe { *contents = 0xdec0_1234 };
        assert_eq!(unsafe { *contents }, 0xdec0_1234);

        let initial = [0x0102_0304_u32, 0x0506_0708_u32, 0x090a_0b0c_u32];
        let copied = unsafe {
            guest_buffer_bytes(
                device,
                objc::sel::get("newBufferWithBytes:length:options:"),
                initial.as_ptr(),
                std::mem::size_of_val(&initial) as u64,
                MTL_RESOURCE_STORAGE_MODE_SHARED,
            )
        };
        assert!(!copied.is_null());
        let mapped = unsafe { guest_ptr(copied, objc::sel::get("contents")) } as *const u32;
        assert!(!mapped.is_null());
        assert_eq!(
            unsafe { std::slice::from_raw_parts(mapped, initial.len()) },
            &initial
        );

        let command = unsafe { guest_id(queue, objc::sel::get("commandBuffer")) };
        assert!(!command.is_null());
        assert_eq!(
            unsafe { guest_id(command, objc::sel::get("device")) },
            device
        );
        assert_eq!(
            unsafe { guest_id(command, objc::sel::get("commandQueue")) },
            queue
        );
        unsafe {
            guest_void(command, objc::sel::get("commit"));
            guest_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(
            unsafe { guest_u64(command, objc::sel::get("status")) },
            MTL_COMMAND_BUFFER_STATUS_COMPLETED
        );

        objc::arc::pool_pop(pool);
        objc::arc::release(copied);
        objc::arc::release(buffer);
        objc::arc::release(queue);
    }

    #[test]
    fn builtin_blit_encoder_copies_bytes_on_host_gpu() {
        if !require_native_opt_in() {
            return;
        }
        let device = default_device();
        assert!(!device.is_null());
        let pool = objc::arc::pool_push();
        let source_data = [0x63_u8; 256];
        let source = unsafe {
            new_buffer_with_bytes(
                device,
                std::ptr::null(),
                source_data.as_ptr(),
                source_data.len(),
                0,
            )
        };
        let target = unsafe { new_buffer(device, std::ptr::null(), source_data.len(), 0) };
        assert!(!source.is_null() && !target.is_null());
        let queue = unsafe { new_queue(device, std::ptr::null()) };
        let command = unsafe { new_command_buffer(queue, std::ptr::null()) };
        let guest_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let guest_blit_copy: unsafe extern "C" fn(Id, Sel, Id, u64, Id, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let guest_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let blit = unsafe { guest_id(command, objc::sel::get("blitCommandEncoder")) };
        assert!(!blit.is_null());
        assert!(object::is_kind(
            blit,
            c"RosemaryNativeMTLBlitCommandEncoder"
        ));
        let source_ptr = unsafe { buffer_contents(source, std::ptr::null()) } as *const u8;
        let target_ptr = unsafe { buffer_contents(target, std::ptr::null()) } as *const u8;
        assert!(!source_ptr.is_null() && !target_ptr.is_null());
        unsafe {
            guest_blit_copy(
                blit,
                objc::sel::get("copyFromBuffer:sourceOffset:toBuffer:destinationOffset:size:"),
                source,
                64,
                target,
                16,
                80,
            );
            guest_void(blit, objc::sel::get("endEncoding"));
            guest_void(command, objc::sel::get("commit"));
            guest_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(
            unsafe { std::slice::from_raw_parts(target_ptr.add(16), 80) },
            &source_data[64..144]
        );
        objc::arc::pool_pop(pool);
        objc::arc::release(queue);
        objc::arc::release(source);
        objc::arc::release(target);
    }

    #[test]
    fn builtin_texture_roundtrip_uses_native_metal() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let descriptor = object::make(
            c"MTLTextureDescriptor",
            resource::TextureDescriptor {
                texture_type: MTL_TEXTURE_TYPE_2D,
                pixel_format: MTL_PIXEL_FORMAT_RGBA8_UNORM,
                width: 2,
                height: 2,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 0,
                usage: 3,
            },
        );
        let texture = unsafe { texture::new_texture(device, std::ptr::null(), descriptor) };
        assert!(!texture.is_null());
        assert!(object::is_kind(texture, c"RosemaryNativeMTLTexture"));
        let region = texture::Region {
            origin: [0, 0, 0],
            size: [2, 2, 1],
        };
        let pixels = [
            255_u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let mut output = [0_u8; 16];
        unsafe {
            texture::replace_region(
                texture,
                std::ptr::null(),
                region,
                0,
                pixels.as_ptr().cast(),
                8,
            );
            texture::get_bytes(
                texture,
                std::ptr::null(),
                output.as_mut_ptr().cast(),
                8,
                region,
                0,
            );
        }
        assert_eq!(output, pixels);
        let getter: unsafe extern "C" fn(Id, Sel) -> u64 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        assert_eq!(unsafe { getter(texture, objc::sel::get("width")) }, 2);
        assert_eq!(unsafe { getter(texture, objc::sel::get("height")) }, 2);
        objc::arc::pool_pop(pool);
        objc::arc::release(texture);
        objc::arc::release(descriptor);
    }

    #[test]
    fn builtin_texture_layer_upload_and_readback() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let descriptor = object::make(
            c"MTLTextureDescriptor",
            resource::TextureDescriptor {
                texture_type: 3, // MTLTextureType2DArray
                pixel_format: MTL_PIXEL_FORMAT_RGBA8_UNORM,
                width: 2,
                height: 2,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 2,
                storage_mode: 0,
                usage: 3,
            },
        );
        let guest = unsafe { texture::new_texture(device, std::ptr::null(), descriptor) };
        assert!(!guest.is_null());
        let source = [
            11_u8, 22, 33, 255, 44, 55, 66, 255, 77, 88, 99, 255, 111, 122, 133, 255,
        ];
        let region = texture::Region {
            origin: [0, 0, 0],
            size: [2, 2, 1],
        };
        let set: unsafe extern "C" fn(Id, Sel, texture::Region, u64, u64, *const c_void, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let get: unsafe extern "C" fn(Id, Sel, *mut c_void, u64, u64, texture::Region, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut result = [0u8; 16];
        unsafe {
            set(
                guest,
                objc::sel::get(
                    "replaceRegion:mipmapLevel:slice:withBytes:bytesPerRow:bytesPerImage:",
                ),
                region,
                0,
                1,
                source.as_ptr().cast(),
                8,
                0, // A single slice does not need bytesPerImage.
            );
            get(
                guest,
                objc::sel::get("getBytes:bytesPerRow:bytesPerImage:fromRegion:mipmapLevel:slice:"),
                result.as_mut_ptr().cast(),
                8,
                0, // A single slice does not need bytesPerImage.
                region,
                0,
                1,
            );
        }
        assert_eq!(result, source);
        objc::arc::pool_pop(pool);
        objc::arc::release(guest);
        objc::arc::release(descriptor);
    }

    #[test]
    fn native_texture_rejects_impossible_guest_dimensions_before_apple_validation() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let original = resource::TextureDescriptor {
            texture_type: MTL_TEXTURE_TYPE_2D,
            pixel_format: MTL_PIXEL_FORMAT_RGBA8_UNORM,
            width: 16,
            height: 16,
            depth: 1,
            mipmap_level_count: 1,
            sample_count: 1,
            array_length: 1,
            storage_mode: 0,
            usage: 3,
        };
        for (width, height, depth) in [
            (u32::MAX as u64, u32::MAX as u64, 1),
            (u64::MAX, 16, 1),
            (0, 16, 1),
            (16, 0, 1),
            (16, 16, 0),
        ] {
            let descriptor = object::make(
                c"MTLTextureDescriptor",
                resource::TextureDescriptor {
                    width,
                    height,
                    depth,
                    ..original
                },
            );
            let texture = unsafe { texture::new_texture(device, std::ptr::null(), descriptor) };
            assert!(
                texture.is_null(),
                "invalid dimensions {width}x{height}x{depth} reached Apple Metal"
            );
            objc::arc::release(descriptor);
        }
        let valid = object::make(c"MTLTextureDescriptor", original);
        let texture = unsafe { texture::new_texture(device, std::ptr::null(), valid) };
        assert!(
            !texture.is_null(),
            "the size guard rejected a valid Metal texture"
        );
        objc::arc::release(texture);
        objc::arc::release(valid);
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn builtin_block_compressed_mip_tail_copies_reach_host_gpu() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_copy: unsafe extern "C" fn(
            Id,
            Sel,
            Id,
            u64,
            u64,
            encoder::MtlOrigin,
            encoder::MtlSize,
            Id,
            u64,
            u64,
            encoder::MtlOrigin,
        ) = unsafe { std::mem::transmute(objc::msg::entry()) };

        // Both compressed mip-tail cases occurred in the DOORS live trace.
        // A copy must be allowed to cover complete 4x4 BC blocks even when
        // the smaller mip's logical dimensions are not block-aligned.
        for (format, block_bytes) in [(130, 8usize), (134, 16usize)] {
            for (
                from_width,
                from_height,
                from_level,
                to_width,
                to_height,
                to_level,
                copy_width,
                copy_height,
            ) in [(48, 24, 2, 96, 48, 3, 12, 8), (32, 32, 4, 64, 64, 5, 4, 4)]
            {
                let make_texture = |width: u64, height: u64| -> Id {
                    let levels = u64::from(64 - width.max(height).leading_zeros());
                    let descriptor = object::make(
                        c"MTLTextureDescriptor",
                        resource::TextureDescriptor {
                            texture_type: MTL_TEXTURE_TYPE_2D,
                            pixel_format: format,
                            width,
                            height,
                            depth: 1,
                            mipmap_level_count: levels,
                            sample_count: 1,
                            array_length: 1,
                            storage_mode: 0,
                            usage: 3,
                        },
                    );
                    assert!(!descriptor.is_null());
                    let tex = unsafe { texture::new_texture(device, std::ptr::null(), descriptor) };
                    objc::arc::release(descriptor);
                    assert!(!tex.is_null(), "host lacks BC texture format {format}");
                    tex
                };
                let source = make_texture(from_width, from_height);
                let target = make_texture(to_width, to_height);
                let row_bytes = (copy_width / 4 * block_bytes as u64) as usize;
                let bytes = row_bytes * (copy_height / 4) as usize;
                let data: Vec<u8> = (0..bytes)
                    .map(|i| (i as u8).wrapping_mul(17).wrapping_add(23))
                    .collect();
                let region = texture::Region {
                    origin: [0, 0, 0],
                    size: [copy_width, copy_height, 1],
                };
                unsafe {
                    texture::replace_region(
                        source,
                        std::ptr::null(),
                        region,
                        from_level,
                        data.as_ptr().cast(),
                        row_bytes as u64,
                    );
                }
                let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
                let command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
                let blit = unsafe { send_id(command, objc::sel::get("blitCommandEncoder")) };
                assert!(!blit.is_null());
                unsafe {
                    send_copy(
                        blit,
                        objc::sel::get(
                            "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:",
                        ),
                        source,
                        0,
                        from_level,
                        encoder::MtlOrigin { x: 0, y: 0, z: 0 },
                        encoder::MtlSize {
                            width: copy_width,
                            height: copy_height,
                            depth: 1,
                        },
                        target,
                        0,
                        to_level,
                        encoder::MtlOrigin { x: 0, y: 0, z: 0 },
                    );
                    send_void(blit, objc::sel::get("endEncoding"));
                    send_void(command, objc::sel::get("commit"));
                    send_void(command, objc::sel::get("waitUntilCompleted"));
                }
                assert_eq!(
                    unsafe { command_status(command, std::ptr::null()) },
                    4,
                    "BC format {format} source level {from_level} to destination level {to_level}"
                );
                let mut result = vec![0_u8; bytes];
                unsafe {
                    texture::get_bytes(
                        target,
                        std::ptr::null(),
                        result.as_mut_ptr().cast(),
                        row_bytes as u64,
                        region,
                        to_level,
                    );
                }
                assert_eq!(
                    result, data,
                    "native BC format {format} mip {from_level}->{to_level} failed to copy the entire padded block"
                );
                for item in [target, source, queue] {
                    objc::arc::release(item);
                }
            }
        }
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn builtin_texture_to_texture_blit_and_mipmaps_reach_host_gpu() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let desc = object::make(
            c"MTLTextureDescriptor",
            resource::TextureDescriptor {
                texture_type: MTL_TEXTURE_TYPE_2D,
                pixel_format: MTL_PIXEL_FORMAT_RGBA8_UNORM,
                width: 4,
                height: 4,
                depth: 1,
                mipmap_level_count: 3,
                sample_count: 1,
                array_length: 1,
                storage_mode: 0,
                usage: 3,
            },
        );
        let source = unsafe { texture::new_texture(device, std::ptr::null(), desc) };
        let target = unsafe { texture::new_texture(device, std::ptr::null(), desc) };
        assert!(!source.is_null() && !target.is_null());
        let pixels = [32u8, 64, 128, 255].repeat(16);
        unsafe {
            texture::replace_region(
                source,
                std::ptr::null(),
                texture::Region {
                    origin: [0, 0, 0],
                    size: [4, 4, 1],
                },
                0,
                pixels.as_ptr().cast(),
                16,
            );
        }
        let queue = unsafe { new_queue(device, std::ptr::null()) };
        let command = unsafe { new_command_buffer(queue, std::ptr::null()) };
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let copy: unsafe extern "C" fn(
            Id,
            Sel,
            Id,
            u64,
            u64,
            encoder::MtlOrigin,
            encoder::MtlSize,
            Id,
            u64,
            u64,
            encoder::MtlOrigin,
        ) = unsafe { std::mem::transmute(objc::msg::entry()) };
        let blit = unsafe { send_id(command, objc::sel::get("blitCommandEncoder")) };
        assert!(!blit.is_null());
        unsafe {
            send_id_arg(blit, objc::sel::get("generateMipmapsForTexture:"), source);
            copy(
                blit,
                objc::sel::get(
                    "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:",
                ),
                source,
                0,
                1,
                encoder::MtlOrigin { x: 0, y: 0, z: 0 },
                encoder::MtlSize {
                    width: 2,
                    height: 2,
                    depth: 1,
                },
                target,
                0,
                1,
                encoder::MtlOrigin { x: 0, y: 0, z: 0 },
            );
            send_void(blit, objc::sel::get("endEncoding"));
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(unsafe { command_status(command, std::ptr::null()) }, 4);
        let mut output = [0u8; 16];
        unsafe {
            texture::get_bytes(
                target,
                std::ptr::null(),
                output.as_mut_ptr().cast(),
                8,
                texture::Region {
                    origin: [0, 0, 0],
                    size: [2, 2, 1],
                },
                1,
            )
        };
        assert_eq!(output.as_slice(), [32, 64, 128, 255].repeat(4).as_slice());
        objc::arc::pool_pop(pool);
        for object in [target, source, desc, queue] {
            objc::arc::release(object);
        }
    }

    #[test]
    fn builtin_texture_to_buffer_blit_verifies_gpu_copy() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let desc = object::make(
            c"MTLTextureDescriptor",
            resource::TextureDescriptor {
                texture_type: MTL_TEXTURE_TYPE_2D,
                pixel_format: MTL_PIXEL_FORMAT_RGBA8_UNORM,
                width: 2,
                height: 2,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 0,
                usage: 3,
            },
        );
        let texture = unsafe { texture::new_texture(device, std::ptr::null(), desc) };
        assert!(!texture.is_null());
        let pixels = [
            11_u8, 22, 33, 255, 44, 55, 66, 255, 77, 88, 99, 255, 111, 122, 133, 255,
        ];
        unsafe {
            texture::replace_region(
                texture,
                std::ptr::null(),
                texture::Region {
                    origin: [0, 0, 0],
                    size: [2, 2, 1],
                },
                0,
                pixels.as_ptr().cast(),
                8,
            );
        }
        let output = unsafe { new_buffer(device, std::ptr::null(), 512, 0) };
        let queue = unsafe { new_queue(device, std::ptr::null()) };
        let command = unsafe { new_command_buffer(queue, std::ptr::null()) };
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_copy: unsafe extern "C" fn(
            Id,
            Sel,
            Id,
            u64,
            u64,
            encoder::MtlOrigin,
            encoder::MtlSize,
            Id,
            u64,
            u64,
            u64,
        ) = unsafe { std::mem::transmute(objc::msg::entry()) };
        let blit = unsafe { send_id(command, objc::sel::get("blitCommandEncoder")) };
        assert!(!blit.is_null());
        unsafe {
            send_copy(
                blit,
                objc::sel::get(
                    "copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toBuffer:destinationOffset:destinationBytesPerRow:destinationBytesPerImage:",
                ),
                texture,
                0,
                0,
                encoder::MtlOrigin { x: 0, y: 0, z: 0 },
                encoder::MtlSize {
                    width: 2,
                    height: 2,
                    depth: 1,
                },
                output,
                0,
                256,
                512,
            );
            send_void(blit, objc::sel::get("endEncoding"));
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(unsafe { command_status(command, std::ptr::null()) }, 4);
        let memory = unsafe { buffer_contents(output, std::ptr::null()) } as *const u8;
        assert!(!memory.is_null());
        assert_eq!(
            unsafe { std::slice::from_raw_parts(memory, 8) },
            &pixels[..8]
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(memory.add(256), 8) },
            &pixels[8..]
        );
        objc::arc::pool_pop(pool);
        for object in [output, texture, desc, queue] {
            objc::arc::release(object);
        }
    }

    #[test]
    fn builtin_source_shader_dispatches_on_native_gpu() {
        if !require_native_opt_in() {
            return;
        }
        let device = default_device();
        assert!(!device.is_null());
        let pool = objc::arc::pool_push();
        let source = foundation::string::from_str(
            "#include <metal_stdlib>\nusing namespace metal;\n\
             kernel void fill_words(device uint* out [[buffer(0)]], uint id [[thread_position_in_grid]]) { out[id] = id * 3 + 1; }",
        );
        let function_name = foundation::string::from_str("fill_words");
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_library: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_pipeline: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_id: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_buffer: unsafe extern "C" fn(Id, Sel, Id, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_sizes: unsafe extern "C" fn(Id, Sel, encoder::MtlSize, encoder::MtlSize) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };

        let mut error = std::ptr::null_mut();
        let library = unsafe {
            send_library(
                device,
                objc::sel::get("newLibraryWithSource:options:error:"),
                source,
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(
            !library.is_null(),
            "native MSL compilation failed: {error:p}"
        );
        assert!(error.is_null());
        let function = unsafe {
            send_id_arg(
                library,
                objc::sel::get("newFunctionWithName:"),
                function_name,
            )
        };
        assert!(!function.is_null());
        let pipeline = unsafe {
            send_pipeline(
                device,
                objc::sel::get("newComputePipelineStateWithFunction:error:"),
                function,
                &mut error,
            )
        };
        assert!(
            !pipeline.is_null(),
            "native compute pipeline creation failed: {error:p}"
        );
        assert!(error.is_null());

        const WORD_COUNT: usize = 16;
        let buffer = unsafe { new_buffer(device, std::ptr::null(), WORD_COUNT * 4, 0) };
        assert!(!buffer.is_null());
        let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
        let command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
        let compute = unsafe { send_id(command, objc::sel::get("computeCommandEncoder")) };
        assert!(!compute.is_null());
        unsafe {
            send_void_id(
                compute,
                objc::sel::get("setComputePipelineState:"),
                pipeline,
            );
            send_void_buffer(
                compute,
                objc::sel::get("setBuffer:offset:atIndex:"),
                buffer,
                0,
                0,
            );
            send_void_sizes(
                compute,
                objc::sel::get("dispatchThreadgroups:threadsPerThreadgroup:"),
                encoder::MtlSize {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
                encoder::MtlSize {
                    width: WORD_COUNT as u64,
                    height: 1,
                    depth: 1,
                },
            );
            send_void(compute, objc::sel::get("endEncoding"));
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        let output = unsafe { buffer_contents(buffer, std::ptr::null()) } as *const u32;
        assert!(!output.is_null());
        for index in 0..WORD_COUNT {
            assert_eq!(
                unsafe { *output.add(index) },
                index as u32 * 3 + 1,
                "word {index}"
            );
        }

        objc::arc::pool_pop(pool);
        objc::arc::release(buffer);
        objc::arc::release(queue);
        objc::arc::release(pipeline);
        objc::arc::release(function);
        objc::arc::release(library);
        objc::arc::release(function_name);
        objc::arc::release(source);
    }

    #[test]
    fn native_command_callbacks_run_guest_blocks_after_host_submission() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SCHEDULED: AtomicUsize = AtomicUsize::new(0);
        static COMPLETED: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn did_schedule(_block: Id, _command: Id) {
            SCHEDULED.fetch_add(1, Ordering::SeqCst);
        }
        unsafe extern "C" fn did_complete(_block: Id, _command: Id) {
            COMPLETED.fetch_add(1, Ordering::SeqCst);
        }
        #[repr(C)]
        struct GlobalBlock {
            isa: *const c_void,
            flags: i32,
            reserved: i32,
            invoke: unsafe extern "C" fn(Id, Id),
            descriptor: *const [u64; 2],
        }
        static DESCRIPTOR: [u64; 2] = [0, std::mem::size_of::<GlobalBlock>() as u64];
        let class = objc::class::lookup(c"__NSGlobalBlock__").expect("guest global block class");
        let isa = objc::class::ptr(class) as *const c_void;
        let scheduled_block = GlobalBlock {
            isa,
            flags: 1 << 28,
            reserved: 0,
            invoke: did_schedule,
            descriptor: &DESCRIPTOR,
        };
        let completed_block = GlobalBlock {
            isa,
            flags: 1 << 28,
            reserved: 0,
            invoke: did_complete,
            descriptor: &DESCRIPTOR,
        };
        SCHEDULED.store(0, Ordering::SeqCst);
        COMPLETED.store(0, Ordering::SeqCst);
        let guest_pool = objc::arc::pool_push();
        let device = default_device();
        let queue = unsafe { new_queue(device, std::ptr::null()) };
        let command = unsafe { new_command_buffer(queue, std::ptr::null()) };
        assert!(!command.is_null());
        let send_block: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            send_block(
                command,
                objc::sel::get("addScheduledHandler:"),
                &scheduled_block as *const _ as Id,
            );
            send_block(
                command,
                objc::sel::get("addCompletedHandler:"),
                &completed_block as *const _ as Id,
            );
            command_commit(command, std::ptr::null());
            command_wait(command, std::ptr::null());
        }
        for _ in 0..200 {
            if SCHEDULED.load(Ordering::SeqCst) == 1 && COMPLETED.load(Ordering::SeqCst) == 1 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            SCHEDULED.load(Ordering::SeqCst),
            1,
            "native scheduled callback did not reach guest"
        );
        assert_eq!(
            COMPLETED.load(Ordering::SeqCst),
            1,
            "native completed callback did not reach guest"
        );
        objc::arc::pool_pop(guest_pool);
        objc::arc::release(queue);
    }

    #[test]
    fn native_depth_stencil_guest_descriptor_creates_apple_state() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let guest_pool = objc::arc::pool_push();
        let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let state = resource::DepthStencilState {
            depth_compare: 1,
            depth_write: true,
            front: resource::StencilState {
                compare: 2,
                fail: 1,
                depth_fail: 2,
                pass: 3,
                read_mask: 0x0f,
                write_mask: 0xf0,
            },
            back: resource::StencilState {
                compare: 6,
                fail: 4,
                depth_fail: 5,
                pass: 6,
                read_mask: 0xff,
                write_mask: 0xaa,
            },
            stencil_enabled: true,
        };
        let descriptor = object::make(c"MTLDepthStencilDescriptor", state);
        assert!(!descriptor.is_null());
        let guest = unsafe {
            send_id_arg(
                device,
                objc::sel::get("newDepthStencilStateWithDescriptor:"),
                descriptor,
            )
        };
        assert!(object::is_kind(
            guest,
            c"RosemaryNativeMTLDepthStencilState"
        ));
        assert_eq!(unsafe { send_id(guest, objc::sel::get("device")) }, device);
        let host = unsafe { object::body::<depth::NativeDepthStencilState>(guest) }.host;
        assert!(!host.is_null());
        let host_device = unsafe { host_send_id(host, sel(c"device")) };
        assert_eq!(
            host_device,
            unsafe { object::body::<NativeDevice>(device) }.host
        );
        objc::arc::release(guest);
        objc::arc::release(descriptor);
        objc::arc::pool_pop(guest_pool);
    }

    #[test]
    fn builtin_unretained_command_buffer_submits_to_host() {
        if !require_native_opt_in() {
            return;
        }
        let device = default_device();
        let pool = objc::arc::pool_push();
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
        assert!(!queue.is_null());
        let command = unsafe {
            send_id(
                queue,
                objc::sel::get("commandBufferWithUnretainedReferences"),
            )
        };
        assert!(object::is_kind(command, c"RosemaryNativeMTLCommandBuffer"));
        unsafe {
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(unsafe { command_status(command, std::ptr::null()) }, 4);
        objc::arc::pool_pop(pool);
        objc::arc::release(queue);
    }

    #[test]
    fn native_device_capabilities_match_host_metal() {
        if !require_native_opt_in() {
            return;
        }
        let guest = default_device();
        assert!(!guest.is_null());
        let host = unsafe { object::body::<NativeDevice>(guest) }.host;
        let send_bool: unsafe extern "C" fn(Id, Sel, u64) -> bool =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_u64: unsafe extern "C" fn(Id, Sel) -> u64 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let _pool = HostPool::new();
        for count in [1_u64, 2, 4, 8, 16] {
            assert_eq!(
                unsafe { send_bool(guest, objc::sel::get("supportsTextureSampleCount:"), count) },
                unsafe { host_send_bool_u64(host, sel(c"supportsTextureSampleCount:"), count) },
            );
        }
        let host_supports_depth = unsafe {
            host_send_bool_sel(
                host,
                sel(c"respondsToSelector:"),
                sel(c"isDepth24Stencil8PixelFormatSupported"),
            )
        };
        let send_bool_no_arg: unsafe extern "C" fn(Id, Sel) -> bool =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        assert_eq!(
            unsafe {
                send_bool_no_arg(
                    guest,
                    objc::sel::get("isDepth24Stencil8PixelFormatSupported"),
                )
            },
            if host_supports_depth {
                unsafe { host_send_bool(host, sel(c"isDepth24Stencil8PixelFormatSupported")) }
            } else {
                false
            },
        );
        assert_eq!(
            unsafe { send_u64(guest, objc::sel::get("argumentBuffersSupport")) },
            unsafe { host_send_u64(host, sel(c"argumentBuffersSupport")) },
        );
        if unsafe {
            host_send_bool_sel(
                host,
                sel(c"respondsToSelector:"),
                sel(c"supportsFeatureSet:"),
            )
        } {
            for feature in [0_u64, 10000, 10001, 10002] {
                assert_eq!(
                    unsafe { send_bool(guest, objc::sel::get("supportsFeatureSet:"), feature) },
                    unsafe { host_send_bool_u64(host, sel(c"supportsFeatureSet:"), feature) },
                );
            }
        }
    }

    #[test]
    fn builtin_compute_inline_bytes_reach_host_shader() {
        if !require_native_opt_in() {
            return;
        }
        let device = default_device();
        let pool = objc::arc::pool_push();
        let source = foundation::string::from_str(
            "#include <metal_stdlib>\nusing namespace metal;\n\
             kernel void add_inline(constant uint &input [[buffer(0)]],\n\
                                    device uint* output [[buffer(1)]]) { output[0] = input + 7; }",
        );
        let name = foundation::string::from_str("add_inline");
        let send_library: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_pipeline: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_void: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_buffer: unsafe extern "C" fn(Id, Sel, Id, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_bytes: unsafe extern "C" fn(Id, Sel, *const c_void, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_dispatch: unsafe extern "C" fn(Id, Sel, encoder::MtlSize, encoder::MtlSize) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut error = std::ptr::null_mut();
        let library = unsafe {
            send_library(
                device,
                objc::sel::get("newLibraryWithSource:options:error:"),
                source,
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(!library.is_null(), "inline shader compilation: {error:p}");
        let function =
            unsafe { send_id_arg(library, objc::sel::get("newFunctionWithName:"), name) };
        assert!(!function.is_null());
        let pipeline = unsafe {
            send_pipeline(
                device,
                objc::sel::get("newComputePipelineStateWithFunction:error:"),
                function,
                &mut error,
            )
        };
        assert!(!pipeline.is_null(), "inline compute pipeline: {error:p}");
        let buffer = unsafe { new_buffer(device, std::ptr::null(), 4, 0) };
        assert!(!buffer.is_null());
        let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
        let command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
        let encoder = unsafe { send_id(command, objc::sel::get("computeCommandEncoder")) };
        let value = 35u32;
        unsafe {
            send_id_void(
                encoder,
                objc::sel::get("setComputePipelineState:"),
                pipeline,
            );
            send_bytes(
                encoder,
                objc::sel::get("setBytes:length:atIndex:"),
                (&raw const value).cast(),
                4,
                0,
            );
            send_buffer(
                encoder,
                objc::sel::get("setBuffer:offset:atIndex:"),
                buffer,
                0,
                1,
            );
            send_dispatch(
                encoder,
                objc::sel::get("dispatchThreadgroups:threadsPerThreadgroup:"),
                encoder::MtlSize {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
                encoder::MtlSize {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
            );
            send_void(encoder, objc::sel::get("endEncoding"));
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(unsafe { command_status(command, std::ptr::null()) }, 4);
        let output = unsafe { buffer_contents(buffer, std::ptr::null()) } as *const u32;
        assert!(!output.is_null());
        assert_eq!(unsafe { *output }, 42);
        objc::arc::pool_pop(pool);
        for obj in [queue, buffer, pipeline, function, library, name, source] {
            objc::arc::release(obj);
        }
    }

    #[test]
    fn builtin_source_compile_options_and_function_names() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let opts = object::make(
            c"MTLCompileOptions",
            pipeline::CompileOptions {
                macros: vec![("WANT_SECOND".into(), "1".into())],
                fast_math: Some(false),
                preserve_invariance: Some(false),
                language_version: None,
            },
        );
        assert!(!opts.is_null());
        let source = foundation::string::from_str(
            "#include <metal_stdlib>\nusing namespace metal;\n\
             kernel void basic_kernel(device uint* output [[buffer(0)]]) { output[0] = 1; }\n\
             #if WANT_SECOND\n\
             kernel void second_kernel(device uint* output [[buffer(0)]]) { output[0] = 2; }\n\
             #endif",
        );
        let send_compile: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_count: unsafe extern "C" fn(Id, Sel) -> usize =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_index: unsafe extern "C" fn(Id, Sel, usize) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut error = std::ptr::null_mut();
        let library = unsafe {
            send_compile(
                device,
                objc::sel::get("newLibraryWithSource:options:error:"),
                source,
                opts,
                &mut error,
            )
        };
        assert!(
            !library.is_null(),
            "native source compilation with guest options failed: {error:p}"
        );
        assert!(error.is_null());
        let names = unsafe { send_id(library, objc::sel::get("functionNames")) };
        assert!(!names.is_null());
        let count = unsafe { send_count(names, objc::sel::get("count")) };
        assert_eq!(count, 2, "expected both compiled Metal kernels");
        let mut found = Vec::new();
        for i in 0..count {
            let item = unsafe { send_index(names, objc::sel::get("objectAtIndex:"), i) };
            assert!(object::is_kind(item, c"NSString"));
            found.push(
                String::from_utf8_lossy(&unsafe { foundation::string::bytes(item) }).into_owned(),
            );
        }
        found.sort();
        assert_eq!(found, ["basic_kernel", "second_kernel"]);
        objc::arc::pool_pop(pool);
        for obj in [library, source, opts] {
            objc::arc::release(obj);
        }
    }

    #[test]
    fn builtin_compute_samples_guest_texture_and_sampler() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let tex_desc = object::make(
            c"MTLTextureDescriptor",
            resource::TextureDescriptor {
                texture_type: MTL_TEXTURE_TYPE_2D,
                pixel_format: MTL_PIXEL_FORMAT_RGBA8_UNORM,
                width: 2,
                height: 2,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 0,
                usage: 1,
            },
        );
        let texture = unsafe { texture::new_texture(device, std::ptr::null(), tex_desc) };
        assert!(!texture.is_null());
        let colors = [
            255_u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        unsafe {
            texture::replace_region(
                texture,
                std::ptr::null(),
                texture::Region {
                    origin: [0, 0, 0],
                    size: [2, 2, 1],
                },
                0,
                colors.as_ptr().cast(),
                8,
            );
        }
        let samp_desc = object::make(
            c"MTLSamplerDescriptor",
            resource::SamplerDescriptor::default(),
        );
        let sampler = unsafe { sampler::new_sampler(device, std::ptr::null(), samp_desc) };
        assert!(!sampler.is_null());
        assert!(object::is_kind(sampler, c"RosemaryNativeMTLSamplerState"));

        let source = foundation::string::from_str(
            "#include <metal_stdlib>\nusing namespace metal;\n\
             kernel void fetch_tex(texture2d<float, access::sample> tex [[texture(0)]],\n\
                                   sampler sampling [[sampler(0)]],\n\
                                   device uint4* output [[buffer(0)]],\n\
                                   uint index [[thread_position_in_grid]]) {\n\
                 output[index] = uint4(round(tex.sample(sampling, float2(0.75, 0.25)) * 255.0));\n\
             }",
        );
        let name = foundation::string::from_str("fetch_tex");
        let send_library: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_pipeline: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_buffer: unsafe extern "C" fn(Id, Sel, Id, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_resource: unsafe extern "C" fn(Id, Sel, Id, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_void: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_dispatch: unsafe extern "C" fn(Id, Sel, encoder::MtlSize, encoder::MtlSize) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut error = std::ptr::null_mut();
        let library = unsafe {
            send_library(
                device,
                objc::sel::get("newLibraryWithSource:options:error:"),
                source,
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(
            !library.is_null(),
            "sampling shader compilation failed: {error:p}"
        );
        let function =
            unsafe { send_id_arg(library, objc::sel::get("newFunctionWithName:"), name) };
        assert!(!function.is_null());
        let pipeline = unsafe {
            send_pipeline(
                device,
                objc::sel::get("newComputePipelineStateWithFunction:error:"),
                function,
                &mut error,
            )
        };
        assert!(
            !pipeline.is_null(),
            "sampling shader pipeline failed: {error:p}"
        );

        let output = unsafe { new_buffer(device, std::ptr::null(), 16, 0) };
        assert!(!output.is_null());
        let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
        let command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
        let encoder = unsafe { send_id(command, objc::sel::get("computeCommandEncoder")) };
        assert!(!encoder.is_null());
        unsafe {
            send_id_void(
                encoder,
                objc::sel::get("setComputePipelineState:"),
                pipeline,
            );
            send_buffer(
                encoder,
                objc::sel::get("setBuffer:offset:atIndex:"),
                output,
                0,
                0,
            );
            send_resource(encoder, objc::sel::get("setTexture:atIndex:"), texture, 0);
            send_resource(
                encoder,
                objc::sel::get("setSamplerState:atIndex:"),
                sampler,
                0,
            );
            send_dispatch(
                encoder,
                objc::sel::get("dispatchThreadgroups:threadsPerThreadgroup:"),
                encoder::MtlSize {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
                encoder::MtlSize {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
            );
            send_void(encoder, objc::sel::get("endEncoding"));
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(unsafe { command_status(command, std::ptr::null()) }, 4);
        let result = unsafe { buffer_contents(output, std::ptr::null()) } as *const u32;
        assert!(!result.is_null());
        assert_eq!(
            unsafe { std::slice::from_raw_parts(result, 4) },
            &[0, 255, 0, 255]
        );
        objc::arc::pool_pop(pool);
        for object in [
            output, queue, pipeline, function, library, name, source, sampler, samp_desc, texture,
            tex_desc,
        ] {
            objc::arc::release(object);
        }
    }

    #[test]
    fn builtin_mrt_and_depth_render_pass_draws_to_two_color_targets() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let source = foundation::string::from_str(
            "#include <metal_stdlib>\nusing namespace metal;\n\
             struct Vout { float4 position [[position]]; };\n\
             vertex Vout mrt_vertex(uint index [[vertex_id]]) {\n\
                float2 points[3] = {float2(-1,-1), float2(3,-1), float2(-1,3)};\n\
                Vout result; result.position = float4(points[index], 0, 1); return result;\n\
             }\n\
             struct Fout { float4 a [[color(0)]]; float4 b [[color(1)]]; };\n\
             fragment Fout mrt_fragment() {\n\
                Fout r; r.a = float4(0.6, 0.1, 0.2, 1); r.b = float4(0.2, 0.6, 0.8, 1); return r;\n\
             }",
        );
        let vertex_name = foundation::string::from_str("mrt_vertex");
        let fragment_name = foundation::string::from_str("mrt_fragment");
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_library: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_pipeline: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_id: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let draw: unsafe extern "C" fn(Id, Sel, u64, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut error = std::ptr::null_mut();
        let library = unsafe {
            send_library(
                device,
                objc::sel::get("newLibraryWithSource:options:error:"),
                source,
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(
            !library.is_null(),
            "MRT shader compilation failed: {error:p}"
        );
        let vertex =
            unsafe { send_id_arg(library, objc::sel::get("newFunctionWithName:"), vertex_name) };
        let fragment = unsafe {
            send_id_arg(
                library,
                objc::sel::get("newFunctionWithName:"),
                fragment_name,
            )
        };
        assert!(!vertex.is_null() && !fragment.is_null());
        let mut colors = [pipeline::ColorAttachment::fresh(); pipeline::COLOR_ATTACHMENTS];
        colors[0].pixel_format = MTL_PIXEL_FORMAT_RGBA8_UNORM;
        colors[1].pixel_format = MTL_PIXEL_FORMAT_RGBA8_UNORM;
        let descriptor = object::make(
            c"MTLRenderPipelineDescriptor",
            pipeline::RenderPipelineDescriptor {
                vertex_function: objc::arc::retain(vertex),
                fragment_function: objc::arc::retain(fragment),
                vertex_descriptor: std::ptr::null_mut(),
                colors,
                depth_format: 252, // MTLPixelFormatDepth32Float
                stencil_format: 0,
                sample_count: 1,
                alpha_to_coverage: false,
                alpha_to_one: false,
                rasterization: true,
                input_topology: 3,
                color_objects: std::ptr::null_mut(),
            },
        );
        let pipeline = unsafe {
            send_pipeline(
                device,
                objc::sel::get("newRenderPipelineStateWithDescriptor:error:"),
                descriptor,
                &mut error,
            )
        };
        assert!(!pipeline.is_null(), "MRT/depth pipeline failed: {error:p}");
        const SIDE: usize = 16;
        let make_texture = |format: u64, storage: u64| {
            let desc = object::make(
                c"MTLTextureDescriptor",
                resource::TextureDescriptor {
                    texture_type: MTL_TEXTURE_TYPE_2D,
                    pixel_format: format,
                    width: SIDE as u64,
                    height: SIDE as u64,
                    depth: 1,
                    mipmap_level_count: 1,
                    sample_count: 1,
                    array_length: 1,
                    storage_mode: storage,
                    usage: 4,
                },
            );
            let tex = unsafe { texture::new_texture(device, std::ptr::null(), desc) };
            objc::arc::release(desc);
            assert!(!tex.is_null());
            tex
        };
        let output_a = make_texture(MTL_PIXEL_FORMAT_RGBA8_UNORM, 0);
        let output_b = make_texture(MTL_PIXEL_FORMAT_RGBA8_UNORM, 0);
        let depth_tex = make_texture(252, 2);
        let mut pass_body = encoder::RenderPassDescriptor::default();
        for (index, tex) in [output_a, output_b].into_iter().enumerate() {
            let attachment = &mut pass_body.colors[index];
            attachment.texture = objc::arc::retain(tex);
            attachment.load_action = 2;
            attachment.store_action = 1;
        }
        pass_body.depth.texture = objc::arc::retain(depth_tex);
        pass_body.depth.load_action = 2;
        pass_body.depth.store_action = 1;
        pass_body.depth.clear_depth = 1.0;
        let pass = object::make(c"MTLRenderPassDescriptor", pass_body);
        let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
        let command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
        let encoder = unsafe {
            send_id_arg(
                command,
                objc::sel::get("renderCommandEncoderWithDescriptor:"),
                pass,
            )
        };
        assert!(
            !encoder.is_null(),
            "host Metal failed MRT plus depth encoder creation"
        );
        let state = resource::DepthStencilState {
            depth_compare: 7,
            depth_write: true,
            ..resource::DepthStencilState::default()
        };
        let depth_descriptor = object::make(c"MTLDepthStencilDescriptor", state);
        let depth_state = unsafe {
            send_id_arg(
                device,
                objc::sel::get("newDepthStencilStateWithDescriptor:"),
                depth_descriptor,
            )
        };
        assert!(!depth_state.is_null());
        unsafe {
            send_void_id(encoder, objc::sel::get("setRenderPipelineState:"), pipeline);
            send_void_id(
                encoder,
                objc::sel::get("setDepthStencilState:"),
                depth_state,
            );
            draw(
                encoder,
                objc::sel::get("drawPrimitives:vertexStart:vertexCount:"),
                3,
                0,
                3,
            );
            send_void(encoder, objc::sel::get("endEncoding"));
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(unsafe { command_status(command, std::ptr::null()) }, 4);
        for (tex, expected) in [(output_a, [153, 26, 51]), (output_b, [51, 153, 204])] {
            let mut pixels = [0u8; SIDE * SIDE * 4];
            unsafe {
                texture::get_bytes(
                    tex,
                    std::ptr::null(),
                    pixels.as_mut_ptr().cast(),
                    (SIDE * 4) as u64,
                    texture::Region {
                        origin: [0, 0, 0],
                        size: [SIDE as u64, SIDE as u64, 1],
                    },
                    0,
                )
            };
            let middle = (SIDE / 2 * SIDE + SIDE / 2) * 4;
            for c in 0..3 {
                assert!(
                    (pixels[middle + c] as i32 - expected[c]).abs() <= 3,
                    "MRT target {:?} output {:?}",
                    expected,
                    &pixels[middle..middle + 4]
                );
            }
            assert_eq!(pixels[middle + 3], 255);
        }
        objc::arc::pool_pop(pool);
        for obj in [
            depth_state,
            depth_descriptor,
            pass,
            depth_tex,
            output_b,
            output_a,
            pipeline,
            descriptor,
            vertex,
            fragment,
            vertex_name,
            fragment_name,
            library,
            source,
            queue,
        ] {
            objc::arc::release(obj);
        }
    }

    #[test]
    fn builtin_vertex_layout_and_blend_pipeline_draws_native_gpu() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let source = foundation::string::from_str(
            "#include <metal_stdlib>\nusing namespace metal;\n\
             struct Vin { float2 p [[attribute(0)]]; };\n\
             struct Vout { float4 p [[position]]; };\n\
             vertex Vout vertex_attrib(Vin in [[stage_in]]) {\n\
                 Vout out; out.p = float4(in.p, 0, 1); return out;\n\
             }\n\
             fragment float4 fragment_half_red() { return float4(1, 0, 0, 0.5); }",
        );
        let vertex_name = foundation::string::from_str("vertex_attrib");
        let fragment_name = foundation::string::from_str("fragment_half_red");
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_library: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_pipeline: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_buffer: unsafe extern "C" fn(Id, Sel, Id, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_id: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_draw: unsafe extern "C" fn(Id, Sel, u64, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut error = std::ptr::null_mut();
        let library = unsafe {
            send_library(
                device,
                objc::sel::get("newLibraryWithSource:options:error:"),
                source,
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(
            !library.is_null(),
            "vertex attribute shader compilation failed: {error:p}"
        );
        let vertex =
            unsafe { send_id_arg(library, objc::sel::get("newFunctionWithName:"), vertex_name) };
        let fragment = unsafe {
            send_id_arg(
                library,
                objc::sel::get("newFunctionWithName:"),
                fragment_name,
            )
        };
        assert!(!vertex.is_null() && !fragment.is_null());
        let vertex_desc = resource::new_empty_vertex_descriptor();
        assert!(!vertex_desc.is_null());
        {
            let body = unsafe { object::body::<resource::VertexDescriptor>(vertex_desc) };
            body.attributes[0] = resource::VertexAttribute {
                format: 29,
                offset: 0,
                buffer_index: 0,
            }; // MTLVertexFormatFloat2
            body.layouts[0] = resource::VertexLayout {
                stride: 8,
                step_function: 1,
                step_rate: 1,
            };
        }
        let mut colors = [pipeline::ColorAttachment::fresh(); pipeline::COLOR_ATTACHMENTS];
        colors[0].pixel_format = MTL_PIXEL_FORMAT_RGBA8_UNORM;
        colors[0].blending = true;
        colors[0].source_rgb = 4; // MTLBlendFactorSourceAlpha
        colors[0].destination_rgb = 5; // OneMinusSourceAlpha
        let descriptor = object::make(
            c"MTLRenderPipelineDescriptor",
            pipeline::RenderPipelineDescriptor {
                vertex_function: objc::arc::retain(vertex),
                fragment_function: objc::arc::retain(fragment),
                vertex_descriptor: vertex_desc,
                colors,
                depth_format: 0,
                stencil_format: 0,
                sample_count: 1,
                alpha_to_coverage: false,
                alpha_to_one: false,
                rasterization: true,
                input_topology: 3, // Triangle
                color_objects: std::ptr::null_mut(),
            },
        );
        let pipeline = unsafe {
            send_pipeline(
                device,
                objc::sel::get("newRenderPipelineStateWithDescriptor:error:"),
                descriptor,
                &mut error,
            )
        };
        assert!(
            !pipeline.is_null(),
            "native vertex/blend pipeline failed: {error:p}"
        );
        const SIDE: usize = 16;
        let tex_desc = object::make(
            c"MTLTextureDescriptor",
            resource::TextureDescriptor {
                texture_type: MTL_TEXTURE_TYPE_2D,
                pixel_format: MTL_PIXEL_FORMAT_RGBA8_UNORM,
                width: SIDE as u64,
                height: SIDE as u64,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 0,
                usage: 4,
            },
        );
        let target = unsafe { texture::new_texture(device, std::ptr::null(), tex_desc) };
        assert!(!target.is_null());
        let mut pass_body = encoder::RenderPassDescriptor::default();
        pass_body.colors[0].texture = objc::arc::retain(target);
        pass_body.colors[0].load_action = 2;
        pass_body.colors[0].store_action = 1;
        pass_body.colors[0].clear_color = [0.0, 0.0, 0.0, 0.0];
        let pass = object::make(c"MTLRenderPassDescriptor", pass_body);
        let vertices = [-1.0f32, -1.0, 3.0, -1.0, -1.0, 3.0];
        let vertex_buffer = unsafe {
            new_buffer_with_bytes(
                device,
                std::ptr::null(),
                vertices.as_ptr().cast(),
                std::mem::size_of_val(&vertices),
                0,
            )
        };
        assert!(!vertex_buffer.is_null());
        let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
        let command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
        let encoder = unsafe {
            send_id_arg(
                command,
                objc::sel::get("renderCommandEncoderWithDescriptor:"),
                pass,
            )
        };
        assert!(!encoder.is_null());
        unsafe {
            send_void_id(encoder, objc::sel::get("setRenderPipelineState:"), pipeline);
            send_buffer(
                encoder,
                objc::sel::get("setVertexBuffer:offset:atIndex:"),
                vertex_buffer,
                0,
                0,
            );
            send_draw(
                encoder,
                objc::sel::get("drawPrimitives:vertexStart:vertexCount:"),
                3,
                0,
                3,
            );
            send_void(encoder, objc::sel::get("endEncoding"));
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(unsafe { command_status(command, std::ptr::null()) }, 4);
        let mut pixels = [0u8; SIDE * SIDE * 4];
        unsafe {
            texture::get_bytes(
                target,
                std::ptr::null(),
                pixels.as_mut_ptr().cast(),
                (SIDE * 4) as u64,
                texture::Region {
                    origin: [0, 0, 0],
                    size: [SIDE as u64, SIDE as u64, 1],
                },
                0,
            )
        };
        let center = (SIDE / 2 * SIDE + SIDE / 2) * 4;
        assert!(
            (110..145).contains(&pixels[center]),
            "expected source-alpha blend, got {:?}",
            &pixels[center..center + 4]
        );
        objc::arc::pool_pop(pool);
        for object in [
            vertex_buffer,
            pass,
            target,
            tex_desc,
            pipeline,
            descriptor,
            vertex,
            fragment,
            vertex_name,
            fragment_name,
            library,
            source,
            queue,
        ] {
            objc::arc::release(object);
        }
    }

    #[test]
    fn builtin_offscreen_render_draws_native_metal_triangle() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        let pool = objc::arc::pool_push();
        let source = foundation::string::from_str(
            "#include <metal_stdlib>\nusing namespace metal;\n\
             struct TriangleVertex { float4 position [[position]]; };\n\
             vertex TriangleVertex triangle_vertex(uint i [[vertex_id]]) {\n\
               float2 p[3] = {float2(-1,-1), float2(3,-1), float2(-1,3)};\n\
               TriangleVertex v; v.position = float4(p[i], 0, 1); return v;\n\
             }\n\
             fragment float4 triangle_fragment() { return float4(0.9, 0.15, 0.25, 1); }\n\
             fragment float4 sample_fragment(texture2d<float, access::sample> image [[texture(0)]],\n\
                                             sampler sampling [[sampler(0)]]) {\n\
                 return image.sample(sampling, float2(0.75, 0.25));\n\
             }",
        );
        let vertex_name = foundation::string::from_str("triangle_vertex");
        let fragment_name = foundation::string::from_str("triangle_fragment");
        let sample_name = foundation::string::from_str("sample_fragment");
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_library: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_pipeline: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_id: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_resource: unsafe extern "C" fn(Id, Sel, Id, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let draw_indexed: unsafe extern "C" fn(Id, Sel, u64, u64, u64, Id, u64, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let draw_indexed_base: unsafe extern "C" fn(
            Id,
            Sel,
            u64,
            u64,
            u64,
            Id,
            u64,
            u64,
            i64,
            u64,
        ) = unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_u64: unsafe extern "C" fn(Id, Sel, u64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_scissor: unsafe extern "C" fn(Id, Sel, encoder::ScissorRect) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };

        let mut error = std::ptr::null_mut();
        let library = unsafe {
            send_library(
                device,
                objc::sel::get("newLibraryWithSource:options:error:"),
                source,
                std::ptr::null_mut(),
                &mut error,
            )
        };
        assert!(
            !library.is_null(),
            "native render shader library failed: {error:p}"
        );
        let vertex =
            unsafe { send_id_arg(library, objc::sel::get("newFunctionWithName:"), vertex_name) };
        let fragment = unsafe {
            send_id_arg(
                library,
                objc::sel::get("newFunctionWithName:"),
                fragment_name,
            )
        };
        assert!(!vertex.is_null() && !fragment.is_null());
        let sampled_fragment =
            unsafe { send_id_arg(library, objc::sel::get("newFunctionWithName:"), sample_name) };
        assert!(!sampled_fragment.is_null());
        let mut colors = [pipeline::ColorAttachment::fresh(); pipeline::COLOR_ATTACHMENTS];
        colors[0].pixel_format = MTL_PIXEL_FORMAT_RGBA8_UNORM;
        let pipeline_desc = object::make(
            c"MTLRenderPipelineDescriptor",
            pipeline::RenderPipelineDescriptor {
                vertex_function: objc::arc::retain(vertex),
                fragment_function: objc::arc::retain(fragment),
                vertex_descriptor: std::ptr::null_mut(),
                colors,
                depth_format: 0,
                stencil_format: 0,
                sample_count: 1,
                alpha_to_coverage: false,
                alpha_to_one: false,
                rasterization: true,
                input_topology: 0,
                color_objects: std::ptr::null_mut(),
            },
        );
        let pipeline = unsafe {
            send_pipeline(
                device,
                objc::sel::get("newRenderPipelineStateWithDescriptor:error:"),
                pipeline_desc,
                &mut error,
            )
        };
        assert!(
            !pipeline.is_null(),
            "native render pipeline failed: {error:p}"
        );
        const SIDE: usize = 16;
        let texture_desc = object::make(
            c"MTLTextureDescriptor",
            resource::TextureDescriptor {
                texture_type: MTL_TEXTURE_TYPE_2D,
                pixel_format: MTL_PIXEL_FORMAT_RGBA8_UNORM,
                width: SIDE as u64,
                height: SIDE as u64,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 0,
                usage: 4,
            },
        );
        let target = unsafe {
            send_id_arg(
                device,
                objc::sel::get("newTextureWithDescriptor:"),
                texture_desc,
            )
        };
        assert!(!target.is_null());
        let mut pass_body = encoder::RenderPassDescriptor::default();
        pass_body.colors[0].texture = objc::arc::retain(target);
        pass_body.colors[0].load_action = 2;
        pass_body.colors[0].store_action = 1;
        let pass = object::make(c"MTLRenderPassDescriptor", pass_body);

        let indices = [0u16, 1, 2];
        let index_buffer = unsafe {
            new_buffer_with_bytes(
                device,
                std::ptr::null(),
                indices.as_ptr().cast(),
                std::mem::size_of_val(&indices),
                0,
            )
        };
        assert!(!index_buffer.is_null());
        let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
        let command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
        let depth_desc = object::make(
            c"MTLDepthStencilDescriptor",
            resource::DepthStencilState::default(),
        );
        let depth_state = unsafe {
            send_id_arg(
                device,
                objc::sel::get("newDepthStencilStateWithDescriptor:"),
                depth_desc,
            )
        };
        assert!(!depth_state.is_null());
        let render = unsafe {
            send_id_arg(
                command,
                objc::sel::get("renderCommandEncoderWithDescriptor:"),
                pass,
            )
        };
        assert!(!render.is_null());
        assert!(object::is_kind(
            render,
            c"RosemaryNativeMTLRenderCommandEncoder"
        ));
        unsafe {
            send_void_id(render, objc::sel::get("setRenderPipelineState:"), pipeline);
            send_void_id(render, objc::sel::get("setDepthStencilState:"), depth_state);
            send_u64(render, objc::sel::get("setStencilReferenceValue:"), 0);
            send_u64(render, objc::sel::get("setCullMode:"), 0);
            send_u64(render, objc::sel::get("setFrontFacingWinding:"), 1);
            send_u64(render, objc::sel::get("setTriangleFillMode:"), 0);
            // An empty scissor is a valid Metal state and must suppress every
            // fragment, not leave an earlier scissor enabled. DOORS uses this.
            for empty in [
                encoder::ScissorRect {
                    x: 5,
                    y: 6,
                    width: 8,
                    height: 0,
                },
                encoder::ScissorRect {
                    x: 5,
                    y: 6,
                    width: 0,
                    height: 8,
                },
            ] {
                send_scissor(render, objc::sel::get("setScissorRect:"), empty);
                draw_indexed(
                    render,
                    objc::sel::get(
                        "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:",
                    ),
                    3,
                    3,
                    0,
                    index_buffer,
                    0,
                    1,
                );
            }
            // A following nonempty scissor must still work: the lower right
            // is colored, while the corner outside it remains clear.
            send_scissor(
                render,
                objc::sel::get("setScissorRect:"),
                encoder::ScissorRect {
                    x: 8,
                    y: 8,
                    width: 8,
                    height: 8,
                },
            );
            draw_indexed(
                render,
                objc::sel::get(
                    "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:",
                ),
                3,
                3,
                0,
                index_buffer,
                0,
                1,
            );
            send_void(render, objc::sel::get("endEncoding"));
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        let region = texture::Region {
            origin: [0, 0, 0],
            size: [SIDE as u64, SIDE as u64, 1],
        };
        let mut pixels = [0_u8; SIDE * SIDE * 4];
        unsafe {
            texture::get_bytes(
                target,
                std::ptr::null(),
                pixels.as_mut_ptr().cast(),
                (SIDE * 4) as u64,
                region,
                0,
            );
        }
        let center = (SIDE / 2 * SIDE + SIDE / 2) * 4;
        let pixel = &pixels[center..center + 4];
        assert!(pixel[0] > 200, "red {pixel:?}");
        assert!((20..70).contains(&pixel[1]), "green {pixel:?}");
        assert!((35..95).contains(&pixel[2]), "blue {pixel:?}");
        assert_eq!(pixel[3], 255);
        assert!(
            pixels[..3].iter().all(|value| *value < 5),
            "empty scissors must not render outside the subsequent clipped region: {:?}",
            &pixels[..4]
        );

        // Second render pass: bind an actual guest texture and sampler to a
        // fragment shader. This is also the ObjC selector path Roblox's
        // Metal renderer uses to source color from texture resources.
        let source_desc = object::make(
            c"MTLTextureDescriptor",
            resource::TextureDescriptor {
                texture_type: MTL_TEXTURE_TYPE_2D,
                pixel_format: MTL_PIXEL_FORMAT_RGBA8_UNORM,
                width: 2,
                height: 2,
                depth: 1,
                mipmap_level_count: 1,
                sample_count: 1,
                array_length: 1,
                storage_mode: 0,
                usage: 1,
            },
        );
        let source_tex = unsafe { texture::new_texture(device, std::ptr::null(), source_desc) };
        assert!(!source_tex.is_null());
        let colors = [
            255_u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        unsafe {
            texture::replace_region(
                source_tex,
                std::ptr::null(),
                texture::Region {
                    origin: [0, 0, 0],
                    size: [2, 2, 1],
                },
                0,
                colors.as_ptr().cast(),
                8,
            );
        }
        let sampler_desc = object::make(
            c"MTLSamplerDescriptor",
            resource::SamplerDescriptor::default(),
        );
        let sampler = unsafe { sampler::new_sampler(device, std::ptr::null(), sampler_desc) };
        assert!(!sampler.is_null());
        let mut color_formats = [pipeline::ColorAttachment::fresh(); pipeline::COLOR_ATTACHMENTS];
        color_formats[0].pixel_format = MTL_PIXEL_FORMAT_RGBA8_UNORM;
        let sampled_desc = object::make(
            c"MTLRenderPipelineDescriptor",
            pipeline::RenderPipelineDescriptor {
                vertex_function: objc::arc::retain(vertex),
                fragment_function: objc::arc::retain(sampled_fragment),
                vertex_descriptor: std::ptr::null_mut(),
                colors: color_formats,
                depth_format: 0,
                stencil_format: 0,
                sample_count: 1,
                alpha_to_coverage: false,
                alpha_to_one: false,
                rasterization: true,
                input_topology: 0,
                color_objects: std::ptr::null_mut(),
            },
        );
        let sampled_pipeline = unsafe {
            send_pipeline(
                device,
                objc::sel::get("newRenderPipelineStateWithDescriptor:error:"),
                sampled_desc,
                &mut error,
            )
        };
        assert!(
            !sampled_pipeline.is_null(),
            "native sampled render pipeline failed: {error:p}"
        );
        let second_command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
        let second_render = unsafe {
            send_id_arg(
                second_command,
                objc::sel::get("renderCommandEncoderWithDescriptor:"),
                pass,
            )
        };
        assert!(!second_render.is_null());
        unsafe {
            send_void_id(
                second_render,
                objc::sel::get("setRenderPipelineState:"),
                sampled_pipeline,
            );
            send_resource(
                second_render,
                objc::sel::get("setFragmentTexture:atIndex:"),
                source_tex,
                0,
            );
            send_resource(
                second_render,
                objc::sel::get("setFragmentSamplerState:atIndex:"),
                sampler,
                0,
            );
            draw_indexed_base(
                second_render,
                objc::sel::get(
                    "drawIndexedPrimitives:indexCount:indexType:indexBuffer:indexBufferOffset:instanceCount:baseVertex:baseInstance:",
                ),
                3,
                3,
                0,
                index_buffer,
                0,
                1,
                0,
                0,
            );
            send_void(second_render, objc::sel::get("endEncoding"));
            send_void(second_command, objc::sel::get("commit"));
            send_void(second_command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(
            unsafe { command_status(second_command, std::ptr::null()) },
            4
        );
        let mut sampled_pixels = [0_u8; SIDE * SIDE * 4];
        unsafe {
            texture::get_bytes(
                target,
                std::ptr::null(),
                sampled_pixels.as_mut_ptr().cast(),
                (SIDE * 4) as u64,
                region,
                0,
            );
        }
        assert_eq!(
            &sampled_pixels[center..center + 4],
            &[0, 255, 0, 255],
            "native fragment texture and sampler binding did not reach the GPU",
        );
        objc::arc::pool_pop(pool);
        for object in [
            index_buffer,
            depth_state,
            depth_desc,
            source_tex,
            source_desc,
            sampler,
            sampler_desc,
            sampled_pipeline,
            sampled_desc,
            sampled_fragment,
            sample_name,
        ] {
            objc::arc::release(object);
        }
        for object in [
            pass,
            target,
            texture_desc,
            pipeline,
            pipeline_desc,
            fragment,
            vertex,
            library,
            fragment_name,
            vertex_name,
            source,
            queue,
        ] {
            objc::arc::release(object);
        }
    }

    #[test]
    fn builtin_metal_layer_vends_native_drawable_and_accepts_presentation() {
        if !require_native_opt_in() {
            return;
        }
        crate::bootstrap();
        let device = default_device();
        assert!(!device.is_null());
        let pool = objc::arc::pool_push();
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id_arg: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_id: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_size: unsafe extern "C" fn(Id, Sel, foundation::geometry::NSSize) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let cls = objc::class::lookup(c"CAMetalLayer").expect("guest CAMetalLayer class");
        let guest_layer = unsafe { send_id(objc::class::ptr(cls) as Id, objc::sel::get("layer")) };
        assert!(!guest_layer.is_null());
        unsafe {
            send_void_id(guest_layer, objc::sel::get("setDevice:"), device);
            send_void_size(
                guest_layer,
                objc::sel::get("setDrawableSize:"),
                foundation::geometry::NSSize {
                    width: 48.0,
                    height: 32.0,
                },
            );
        }
        let drawable = unsafe { send_id(guest_layer, objc::sel::get("nextDrawable")) };
        assert!(
            !drawable.is_null(),
            "host CAMetalLayer did not vend a drawable without a window"
        );
        assert!(object::is_kind(drawable, c"RosemaryNativeCAMetalDrawable"));
        assert_eq!(
            unsafe { send_id(drawable, objc::sel::get("layer")) },
            guest_layer
        );
        let texture = unsafe { send_id(drawable, objc::sel::get("texture")) };
        assert!(object::is_kind(texture, c"RosemaryNativeMTLTexture"));
        let texture_width: unsafe extern "C" fn(Id, Sel) -> u64 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        assert_eq!(
            unsafe { texture_width(texture, objc::sel::get("width")) },
            48
        );
        assert_eq!(
            unsafe { texture_width(texture, objc::sel::get("height")) },
            32
        );
        let queue = unsafe { send_id(device, objc::sel::get("newCommandQueue")) };
        let command = unsafe { send_id(queue, objc::sel::get("commandBuffer")) };
        let mut described = encoder::RenderPassDescriptor::default();
        described.colors[0].texture = objc::arc::retain(texture);
        described.colors[0].load_action = 2;
        described.colors[0].store_action = 1;
        described.colors[0].clear_color = [0.1, 0.5, 0.7, 1.0];
        let pass = object::make(c"MTLRenderPassDescriptor", described);
        let render = unsafe {
            send_id_arg(
                command,
                objc::sel::get("renderCommandEncoderWithDescriptor:"),
                pass,
            )
        };
        assert!(!render.is_null());
        unsafe {
            send_void(render, objc::sel::get("endEncoding"));
            send_void_id(command, objc::sel::get("presentDrawable:"), drawable);
            send_void(command, objc::sel::get("commit"));
            send_void(command, objc::sel::get("waitUntilCompleted"));
        }
        assert_eq!(unsafe { command_status(command, std::ptr::null()) }, 4);
        objc::arc::pool_pop(pool);
        objc::arc::release(pass);
        objc::arc::release(queue);
        assert_eq!(
            layer::tracked_layer_count(),
            0,
            "guest CAMetalLayer destruction did not release native host layer"
        );
    }

    #[test]
    fn precompiled_library_rejects_invalid_guest_dispatch_data() {
        if !require_native_opt_in() {
            return;
        }
        let device = default_device();
        assert!(!device.is_null());
        let pool = objc::arc::pool_push();
        let invalid = b"this is not a valid compiled metallib";
        let data =
            unsafe { darwin::dispatch::data::create(invalid.as_ptr().cast(), invalid.len(), 0, 0) };
        assert_ne!(data, 0);
        let send_library: unsafe extern "C" fn(Id, Sel, u64, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut error = std::ptr::null_mut();
        let library = unsafe {
            send_library(
                device,
                objc::sel::get("newLibraryWithData:error:"),
                data,
                &mut error,
            )
        };
        assert!(
            library.is_null(),
            "Apple Metal unexpectedly accepted corrupt metallib data"
        );
        assert!(
            !error.is_null(),
            "rejected guest metallib must receive an NSError"
        );
        unsafe { darwin::dispatch::object::release_entry(data) };
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn precompiled_library_accepts_macos_framework_metallib() {
        if !require_native_opt_in() {
            return;
        }
        // macOS provides a signed precompiled Metal library with a stable
        // metallib header; use it as a test fixture without modifying it.
        let path =
            "/System/Library/Frameworks/Metal.framework/Versions/A/Resources/default.metallib";
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("skipped: macOS Metal framework has no default.metallib");
            return;
        };
        let device = default_device();
        assert!(!device.is_null());
        let pool = objc::arc::pool_push();
        let data =
            unsafe { darwin::dispatch::data::create(bytes.as_ptr().cast(), bytes.len(), 0, 0) };
        assert_ne!(data, 0);
        let send_library: unsafe extern "C" fn(Id, Sel, u64, *mut Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut error = std::ptr::null_mut();
        let library = unsafe {
            send_library(
                device,
                objc::sel::get("newLibraryWithData:error:"),
                data,
                &mut error,
            )
        };
        assert!(
            !library.is_null(),
            "native Metal rejected macOS default.metallib (guest error: {error:p})"
        );
        assert!(error.is_null());
        assert!(object::is_kind(library, c"RosemaryNativeMTLLibrary"));
        objc::arc::release(library);
        unsafe { darwin::dispatch::object::release_entry(data) };
        objc::arc::pool_pop(pool);
    }
}
