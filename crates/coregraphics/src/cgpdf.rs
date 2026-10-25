//! `CGPDFContext`: the PDF writer CoreGraphics would provide.

#![allow(non_snake_case)]

use corefoundation::base::CFTypeRef;

use crate::CGRect;

/// `CGContextRef CGPDFContextCreateWithURL(CFURLRef, const CGRect *,
/// CFDictionaryRef)`: NULL - there is no PDF writer, and a program printing to
/// a file gets the failure it already handles.
pub unsafe extern "C" fn CGPDFContextCreateWithURL(
    _url: CFTypeRef,
    _media: *const CGRect,
    _info: CFTypeRef,
) -> CFTypeRef {
    std::ptr::null()
}

exports::exports! {
    functions {
        "CGPDFContextCreateWithURL" => CGPDFContextCreateWithURL,
    }
}
