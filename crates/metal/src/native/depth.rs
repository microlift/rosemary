//! Native Metal depth/stencil state backing Rosemary's guest descriptors.
//!
//! Guest `MTLDepthStencilDescriptor` and `MTLStencilDescriptor` instances
//! belong to the built-in Objective-C runtime. Copy their scalar state into
//! newly allocated Apple descriptors, never pass guest pointers to Metal.

use super::*;

pub(super) struct NativeDepthStencilState {
    pub(super) host: HostId,
    pub(super) device: Id,
}

pub(super) fn install() {
    let state = object::register::<NativeDepthStencilState>(c"RosemaryNativeMTLDepthStencilState");
    methods!(state, {
        "device" => device, c"@16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
    });
}

fn set_host_stencil(
    host_descriptor: HostId,
    selector: &CStr,
    state: &resource::StencilState,
) -> bool {
    if state.compare > 7 || state.fail > 7 || state.depth_fail > 7 || state.pass > 7 {
        gpu::trace("native Metal rejected invalid stencil descriptor enum".to_string());
        return false;
    }
    let class = unsafe { host_get_class(c"MTLStencilDescriptor".as_ptr()) };
    if class.is_null() {
        return false;
    }
    let host_stencil = unsafe { host_send_id(class, sel(c"new")) };
    if host_stencil.is_null() {
        return false;
    }
    for (method, value) in [
        (c"setStencilCompareFunction:".as_ref(), state.compare),
        (c"setStencilFailureOperation:".as_ref(), state.fail),
        (c"setDepthFailureOperation:".as_ref(), state.depth_fail),
        (c"setDepthStencilPassOperation:".as_ref(), state.pass),
        (c"setReadMask:".as_ref(), state.read_mask as u64),
        (c"setWriteMask:".as_ref(), state.write_mask as u64),
    ] {
        unsafe { host_send_void_u64(host_stencil, sel(method), value) };
    }
    unsafe {
        host_send_void_id(host_descriptor, sel(selector), host_stencil);
        host_release(host_stencil);
    }
    true
}

pub(super) unsafe extern "C" fn new_state(receiver: Id, _sel: Sel, descriptor: Id) -> Id {
    if !object::is_kind(descriptor, c"MTLDepthStencilDescriptor") {
        gpu::trace("native Metal requires a guest MTLDepthStencilDescriptor".to_string());
        return std::ptr::null_mut();
    }
    let state = *unsafe { object::body::<resource::DepthStencilState>(descriptor) };
    if state.depth_compare > 7 {
        gpu::trace("native Metal rejected invalid depth compare function".to_string());
        return std::ptr::null_mut();
    }
    let _pool = HostPool::new();
    let class = unsafe { host_get_class(c"MTLDepthStencilDescriptor".as_ptr()) };
    if class.is_null() {
        return std::ptr::null_mut();
    }
    let host_descriptor = unsafe { host_send_id(class, sel(c"new")) };
    if host_descriptor.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        host_send_void_u64(
            host_descriptor,
            sel(c"setDepthCompareFunction:"),
            state.depth_compare,
        );
        host_send_void_bool(
            host_descriptor,
            sel(c"setDepthWriteEnabled:"),
            state.depth_write,
        );
    }
    let valid = !state.stencil_enabled
        || (set_host_stencil(host_descriptor, c"setFrontFaceStencil:", &state.front)
            && set_host_stencil(host_descriptor, c"setBackFaceStencil:", &state.back));
    if !valid {
        unsafe { host_release(host_descriptor) };
        return std::ptr::null_mut();
    }
    let host = unsafe {
        host_send_id_id(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newDepthStencilStateWithDescriptor:"),
            host_descriptor,
        )
    };
    unsafe { host_release(host_descriptor) };
    if host.is_null() {
        gpu::trace("native Metal could not create depth/stencil state".to_string());
        return std::ptr::null_mut();
    }
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeMTLDepthStencilState",
        NativeDepthStencilState {
            host,
            device: receiver,
        },
    );
    if guest.is_null() {
        unsafe { host_release(host) };
        objc::arc::release(receiver);
    }
    guest
}

unsafe extern "C" fn device(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeDepthStencilState>(receiver) }.device
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let state = unsafe { object::body::<NativeDepthStencilState>(receiver) };
    unsafe { host_release(state.host) };
    objc::arc::release(state.device);
    objc::arc::finalize(receiver);
}
