//! The `MTLResource` protocol methods buffers and textures share.

use super::*;

const MTL_PURGEABLE_STATE_NON_VOLATILE: u64 = 2;

/// `-setPurgeableState:` answers the state before the call. Nothing here ever
/// discards a resource's contents, so every resource stays non-volatile; a
/// guest which marks one volatile or empty is allowed to find its contents
/// kept.
pub(super) unsafe extern "C" fn set_purgeable_state(_receiver: Id, _sel: Sel, _state: u64) -> u64 {
    MTL_PURGEABLE_STATE_NON_VOLATILE
}

pub(super) unsafe extern "C" fn buffer_allocated_size(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a buffer of this crate's.
    unsafe { object::body::<Buffer>(receiver) }.inner.length
}

pub(super) unsafe extern "C" fn buffer_resource_options(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a buffer of this crate's.
    unsafe { object::body::<Buffer>(receiver) }.options
}

pub(super) unsafe extern "C" fn texture_allocated_size(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a texture of this crate's.
    unsafe { object::body_ref::<Texture>(receiver) }.accounted() as u64
}

pub(super) unsafe extern "C" fn texture_resource_options(receiver: Id, _sel: Sel) -> u64 {
    // SAFETY: a texture of this crate's.
    unsafe { object::body_ref::<Texture>(receiver) }
        .descriptor
        .storage_mode
        << 4
}

unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> bool {
    false
}

unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> bool {
    true
}

/// Methods every resource class answers alike.
pub(super) fn install(class: &'static objc::class::Class) {
    methods!(class, {
        "setPurgeableState:" => set_purgeable_state, c"Q24@0:8Q16";
        "heap" => heap::resource_heap, c"@16@0:8";
        "heapOffset" => crate::zero, c"Q16@0:8";
        "isAliasable" => heap::resource_is_aliasable, c"B16@0:8";
        "makeAliasable" => heap::resource_make_aliasable, c"v16@0:8";
        "cpuCacheMode" => crate::zero, c"Q16@0:8";
        "hazardTrackingMode" => crate::zero, c"Q16@0:8";
    });
}

/// Texture-only answers for the views, buffers and surfaces nothing here
/// makes a texture from.
pub(super) fn install_texture(class: &'static objc::class::Class) {
    methods!(class, {
        "allocatedSize" => texture_allocated_size, c"Q16@0:8";
        "resourceOptions" => texture_resource_options, c"Q16@0:8";
        "isFramebufferOnly" => no, c"B16@0:8";
        "allowGPUOptimizedContents" => yes, c"B16@0:8";
        "isShareable" => no, c"B16@0:8";
        "parentTexture" => crate::nil, c"@16@0:8";
        "rootResource" => crate::nil, c"@16@0:8";
        "buffer" => crate::nil, c"@16@0:8";
        "iosurface" => crate::nil, c"^{__IOSurface=}16@0:8";
        "parentRelativeLevel" => crate::zero, c"Q16@0:8";
        "parentRelativeSlice" => crate::zero, c"Q16@0:8";
    });
}

pub(super) fn install_buffer(class: &'static objc::class::Class) {
    methods!(class, {
        "allocatedSize" => buffer_allocated_size, c"Q16@0:8";
        "resourceOptions" => buffer_resource_options, c"Q16@0:8";
    });
}
