#![allow(non_snake_case)]

//! `VTSession` properties. The only sessions there are decompression ones.

use corefoundation::{
    api::CFAllocatorRef, base::CFTypeRef, collections::CFDictionaryRef, string::CFStringRef,
};

use crate::constants::{kVTParameterErr, kVTPropertyNotSupportedErr};
use crate::decompression;
use crate::types::{OSStatus, VTSessionRef};

/// `OSStatus VTSessionSetProperty(VTSessionRef session, CFStringRef
/// propertyKey, CFTypeRef propertyValue)`
pub unsafe extern "C" fn VTSessionSetProperty(
    session: VTSessionRef,
    property_key: CFStringRef,
    property_value: CFTypeRef,
) -> OSStatus {
    decompression::set_property(session.cast_const(), property_key.cast(), property_value)
}

/// `OSStatus VTSessionCopyProperty(VTSessionRef session, CFStringRef
/// propertyKey, CFAllocatorRef allocator, void *propertyValueOut)`
pub unsafe extern "C" fn VTSessionCopyProperty(
    session: VTSessionRef,
    property_key: CFStringRef,
    _allocator: CFAllocatorRef,
    property_value_out: *mut CFTypeRef,
) -> OSStatus {
    decompression::copy_property(
        session.cast_const(),
        property_key.cast(),
        property_value_out,
    )
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

exports::exports! {
    functions {
        "VTSessionSetProperty" => VTSessionSetProperty,
        "VTSessionCopyProperty" => VTSessionCopyProperty,
    }
}
