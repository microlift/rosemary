//! Guest MTLSamplerDescriptor -> host MTLSamplerState conversion.
//! Copy each descriptor field into an Apple-owned descriptor rather than
//! handing a built-in Objective-C instance to the host Metal runtime.

use super::*;

pub(super) struct NativeSampler {
    pub(super) host: HostId,
    pub(super) device: Id,
}

pub(super) fn install() {
    let class = object::register::<NativeSampler>(c"RosemaryNativeMTLSamplerState");
    methods!(class, {
        "device" => device, c"@16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
    });
}

pub(super) unsafe extern "C" fn new_sampler(receiver: Id, _sel: Sel, descriptor: Id) -> Id {
    if !object::is_kind(descriptor, c"MTLSamplerDescriptor") {
        gpu::trace("native Metal sampler requires a guest MTLSamplerDescriptor".to_string());
        return std::ptr::null_mut();
    }
    let values = *unsafe { object::body::<resource::SamplerDescriptor>(descriptor) };
    // Passing an invalid enum to an Apple Metal setter can raise a host ObjC
    // exception, which cannot safely unwind into the guest runtime.
    let valid = values.min_filter <= MTL_SAMPLER_MIN_MAG_FILTER_LINEAR
        && values.mag_filter <= MTL_SAMPLER_MIN_MAG_FILTER_LINEAR
        && values.mip_filter <= MTL_SAMPLER_MIP_FILTER_LINEAR
        && values
            .address
            .iter()
            .all(|&mode| mode <= MTL_SAMPLER_ADDRESS_MODE_CLAMP_TO_BORDER_COLOR)
        && (1..=MAX_SAMPLER_ANISOTROPY).contains(&values.max_anisotropy)
        && values.compare_function <= MTL_COMPARE_FUNCTION_ALWAYS
        && values.border_color <= MTL_SAMPLER_BORDER_COLOR_OPAQUE_WHITE
        && values.lod_min_clamp.is_finite()
        && values.lod_min_clamp >= 0.0
        && !values.lod_max_clamp.is_nan()
        && values.lod_max_clamp >= values.lod_min_clamp;
    if !valid {
        gpu::trace("native Metal rejected unsupported sampler descriptor".to_string());
        return std::ptr::null_mut();
    }
    let _pool = HostPool::new();
    let class = unsafe { host_get_class(c"MTLSamplerDescriptor".as_ptr()) };
    if class.is_null() {
        return std::ptr::null_mut();
    }
    let host_desc = unsafe { host_send_id(class, sel(c"new")) };
    if host_desc.is_null() {
        return std::ptr::null_mut();
    }
    for (selector, value) in [
        (c"setMinFilter:".as_ref(), values.min_filter),
        (c"setMagFilter:".as_ref(), values.mag_filter),
        (c"setMipFilter:".as_ref(), values.mip_filter),
        (c"setSAddressMode:".as_ref(), values.address[0]),
        (c"setTAddressMode:".as_ref(), values.address[1]),
        (c"setRAddressMode:".as_ref(), values.address[2]),
        (c"setMaxAnisotropy:".as_ref(), values.max_anisotropy),
        (c"setCompareFunction:".as_ref(), values.compare_function),
        (c"setBorderColor:".as_ref(), values.border_color),
    ] {
        unsafe { host_send_void_u64(host_desc, sel(selector), value) };
    }
    unsafe {
        host_send_void_f32(host_desc, sel(c"setLodMinClamp:"), values.lod_min_clamp);
        host_send_void_f32(host_desc, sel(c"setLodMaxClamp:"), values.lod_max_clamp);
        host_send_void_bool(
            host_desc,
            sel(c"setNormalizedCoordinates:"),
            values.normalized_coordinates,
        );
    }
    let host = unsafe {
        host_send_id_id(
            object::body::<NativeDevice>(receiver).host,
            sel(c"newSamplerStateWithDescriptor:"),
            host_desc,
        )
    };
    unsafe { host_release(host_desc) };
    if host.is_null() {
        gpu::trace("native Metal could not create sampler state".to_string());
        return std::ptr::null_mut();
    }
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeMTLSamplerState",
        NativeSampler {
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
    unsafe { object::body::<NativeSampler>(receiver) }.device
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let sampler = unsafe { object::body::<NativeSampler>(receiver) };
    unsafe { host_release(sampler.host) };
    objc::arc::release(sampler.device);
    objc::arc::finalize(receiver);
}
