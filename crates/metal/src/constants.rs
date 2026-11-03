use super::*;

pub(crate) const MTL_PIXEL_FORMAT_INVALID: u64 = 0;
pub(crate) const MTL_PIXEL_FORMAT_RGBA8_UNORM: u64 = 70;
pub(crate) const MTL_PIXEL_FORMAT_BGRA8_UNORM: u64 = 80;

pub(crate) const MTL_TEXTURE_TYPE_2D: u64 = 2;
pub(crate) const MTL_TEXTURE_TYPE_CUBE: u64 = 5;
pub(crate) const MTL_TEXTURE_TYPE_TEXTURE_BUFFER: u64 = 9;

pub(crate) const MTL_STORAGE_MODE_MANAGED: u64 = 1;

pub(crate) const MTL_GPU_FAMILY_MAC_1: u64 = 2001;
pub(crate) const MTL_GPU_FAMILY_MAC_2: u64 = 2002;
pub(crate) const MTL_GPU_FAMILY_COMMON_1: u64 = 3001;
pub(crate) const MTL_GPU_FAMILY_COMMON_3: u64 = 3003;
pub(crate) const MTL_GPU_FAMILY_METAL_3: u64 = 5001;
pub(crate) const MTL_FEATURE_SET_MACOS_GPU_FAMILY_1_V1: u64 = 10000;
pub(crate) const MTL_FEATURE_SET_MACOS_GPU_FAMILY_2_V1: u64 = 10005;

pub(crate) const MTL_ARGUMENT_BUFFERS_TIER_1: u64 = 0;

pub(crate) const MTL_READ_WRITE_TEXTURE_TIER_1: u64 = 1;

pub(crate) const MTL_DEVICE_LOCATION_UNSPECIFIED: u64 = u64::MAX;

pub(crate) const MTL_COLOR_WRITE_MASK_ALL: u64 = 0xf;
pub(crate) const MTL_DEPTH_CLIP_MODE_CLAMP: u64 = 1;

pub(super) unsafe extern "C" fn constant_values_init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `MTLFunctionConstantValues` straight from `+alloc`, whose
    // body is this call's to establish.
    unsafe {
        object::init(
            receiver,
            ConstantValues {
                values: metal_translate::FunctionConstantValues::new(),
            },
        )
    };
    receiver
}

pub(super) unsafe extern "C" fn constant_values_reset(receiver: Id, _sel: Sel) {
    // SAFETY: an initialized `MTLFunctionConstantValues`.
    unsafe { object::body::<ConstantValues>(receiver) }.values =
        metal_translate::FunctionConstantValues::new();
}

pub(super) unsafe extern "C" fn constant_values_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: the body `-init` established, dropped once as the object goes.
    unsafe { std::ptr::drop_in_place(object::body::<ConstantValues>(receiver)) };
    objc::arc::finalize(receiver);
}

/// Read one value of an `MTLDataType` from where the guest keeps it.
///
/// # Safety
///
/// `value` must be null or point at a value of `data_type`.
pub(super) unsafe fn constant_bytes<'a>(value: *const u8, data_type: u64) -> Option<&'a [u8]> {
    let Some(size) = metal_translate::metal_data_type_size(data_type) else {
        report_library_error(format!(
            "MTLFunctionConstantValues was given MTLDataType {data_type}, which no function constant has"
        ));
        return None;
    };
    if value.is_null() {
        return None;
    }
    // SAFETY: the caller's obligation, for exactly the value's size.
    Some(unsafe { std::slice::from_raw_parts(value, size) })
}

pub(super) unsafe extern "C" fn set_constant_at_index(
    receiver: Id,
    _sel: Sel,
    value: *const u8,
    data_type: u64,
    index: u64,
) {
    // SAFETY: Metal reads one value of `data_type` from `value`.
    let Some(bytes) = (unsafe { constant_bytes(value, data_type) }) else {
        return;
    };
    // SAFETY: an initialized `MTLFunctionConstantValues`.
    let body = unsafe { object::body::<ConstantValues>(receiver) };
    if let Err(message) = body.values.set_index(index as u32, data_type, bytes) {
        report_library_error(message);
    }
}

pub(super) unsafe extern "C" fn set_constant_with_name(
    receiver: Id,
    _sel: Sel,
    value: *const u8,
    data_type: u64,
    name: Id,
) {
    // SAFETY: Metal reads one value of `data_type` from `value`.
    let Some(bytes) = (unsafe { constant_bytes(value, data_type) }) else {
        return;
    };
    // SAFETY: a guest `NSString`.
    let name = unsafe { foundation::string::bytes(name) };
    let Ok(name) = std::str::from_utf8(&name) else {
        return;
    };
    // SAFETY: an initialized `MTLFunctionConstantValues`.
    let body = unsafe { object::body::<ConstantValues>(receiver) };
    if let Err(message) = body.values.set_name(name, data_type, bytes) {
        report_library_error(message);
    }
}

/// `-setConstantValues:type:withRange:`: an array of values for consecutive
/// indices. An `NSRange` is two words and arrives in two registers.
pub(super) unsafe extern "C" fn set_constants_in_range(
    receiver: Id,
    sel: Sel,
    values: *const u8,
    data_type: u64,
    location: u64,
    length: u64,
) {
    let Some(size) = metal_translate::metal_data_type_size(data_type) else {
        report_library_error(format!(
            "MTLFunctionConstantValues was given MTLDataType {data_type}, which no function constant has"
        ));
        return;
    };
    // A C array of a three-lane vector is laid out at four lanes, like every
    // Metal vector type; each size here rounds up to the type's stride.
    let stride = size.next_power_of_two();
    for offset in 0..length {
        // SAFETY: the guest's array holds `length` values of `data_type`.
        let value = unsafe { values.add(offset as usize * stride) };
        unsafe { set_constant_at_index(receiver, sel, value, data_type, location + offset) };
    }
}
