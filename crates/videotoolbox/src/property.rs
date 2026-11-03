#![allow(non_snake_case)]

use crate::types::OSStatus;
use crate::{
    constants::{kVTParameterErr, kVTPropertyNotSupportedErr},
    types::VTSessionRef,
};
use corefoundation::{
    api::CFAllocatorRef, base::CFTypeRef, collections::CFDictionaryRef, string::CFStringRef,
};

pub unsafe extern "C" fn VTSessionSetProperty(
    _session: VTSessionRef,
    _propertyKey: CFStringRef,
    _propertyValue: CFTypeRef,
) -> OSStatus {
    kVTPropertyNotSupportedErr
}

pub unsafe extern "C" fn VTSessionCopyProperty(
    _session: VTSessionRef,
    _propertyKey: CFStringRef,
    _allocator: CFAllocatorRef,
    _propertyValueOut: *mut CFTypeRef,
) -> OSStatus {
    kVTPropertyNotSupportedErr
}

pub unsafe extern "C" fn VTSessionSetProperties(
    _session: VTSessionRef,
    _propertyDictionary: CFDictionaryRef,
) -> OSStatus {
    kVTPropertyNotSupportedErr
}

pub unsafe extern "C" fn VTSessionCopySupportedPropertyDictionary(
    _session: VTSessionRef,
    _supportedPropertyDictionaryOut: *mut CFDictionaryRef,
) -> OSStatus {
    kVTParameterErr
}
