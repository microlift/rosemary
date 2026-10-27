#![allow(non_snake_case)]

use crate::types::OSStatus;
use crate::{
    constants::{kVTParameterErr, kVTPropertyNotSupportedErr},
    types::VTSessionRef,
};
use corefoundation::{
    api::CFAllocatorRef, base::CFTypeRef, collections::CFDictionaryRef, string::CFStringRef,
};

/// # Safety
///
/// The session, key and value must be valid Core Foundation objects when non-null.
pub unsafe extern "C" fn VTSessionSetProperty(
    _session: VTSessionRef,
    _propertyKey: CFStringRef,
    _propertyValue: CFTypeRef,
) -> OSStatus {
    kVTPropertyNotSupportedErr
}

/// # Safety
///
/// The session and key must be valid; the optional output pointer must be writable.
pub unsafe extern "C" fn VTSessionCopyProperty(
    _session: VTSessionRef,
    _propertyKey: CFStringRef,
    _allocator: CFAllocatorRef,
    _propertyValueOut: *mut CFTypeRef,
) -> OSStatus {
    kVTPropertyNotSupportedErr
}

/// # Safety
///
/// The session and property dictionary must be valid objects when non-null.
pub unsafe extern "C" fn VTSessionSetProperties(
    _session: VTSessionRef,
    _propertyDictionary: CFDictionaryRef,
) -> OSStatus {
    kVTPropertyNotSupportedErr
}

/// # Safety
///
/// The session must be live and the optional output pointer must be writable.
pub unsafe extern "C" fn VTSessionCopySupportedPropertyDictionary(
    _session: VTSessionRef,
    _supportedPropertyDictionaryOut: *mut CFDictionaryRef,
) -> OSStatus {
    kVTParameterErr
}
