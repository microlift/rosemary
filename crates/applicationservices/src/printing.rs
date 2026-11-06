#![allow(non_snake_case)]

use std::ffi::c_void;

/// `OSStatus`.
type OSStatus = i32;

/// `kPMNoDefaultPrinter`: this runtime never configures a default printer.
const NO_DEFAULT_PRINTER: OSStatus = -30872;
/// `kPMInvalidPrinter`: every `PMPrinter` a guest could hold is one this
/// runtime never made, since nothing here ever answers with a real one.
const INVALID_PRINTER: OSStatus = -30880;

/// `PMResolution`: a printer's horizontal and vertical resolution, in dots
/// per inch.
#[repr(C)]
pub(crate) struct Resolution {
    h_res: f64,
    v_res: f64,
}

/// `OSStatus PMSessionGetCurrentPrinter(PMPrintSession, PMPrinter *)`.
///
/// This runtime behaves like a Mac with no printers configured: the output
/// printer is always null, and the session is told so.
pub unsafe extern "C" fn PMSessionGetCurrentPrinter(
    _session: *mut c_void,
    current_printer: *mut *mut c_void,
) -> OSStatus {
    if !current_printer.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { current_printer.write(std::ptr::null_mut()) };
    }
    NO_DEFAULT_PRINTER
}

/// `OSStatus PMPrinterGetPrinterResolutionCount(PMPrinter, UInt32 *)`.
///
/// Nothing here ever vends a real `PMPrinter`, so every one a guest could
/// pass in is already invalid.
pub unsafe extern "C" fn PMPrinterGetPrinterResolutionCount(
    _printer: *mut c_void,
    count: *mut u32,
) -> OSStatus {
    if !count.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { count.write(0) };
    }
    INVALID_PRINTER
}

/// `OSStatus PMPrinterGetIndexedPrinterResolution(PMPrinter, UInt32,
/// PMResolution *)`.
pub unsafe extern "C" fn PMPrinterGetIndexedPrinterResolution(
    _printer: *mut c_void,
    _resolution_index: u32,
    resolution: *mut Resolution,
) -> OSStatus {
    if !resolution.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe {
            resolution.write(Resolution {
                h_res: 0.0,
                v_res: 0.0,
            })
        };
    }
    INVALID_PRINTER
}

/// `OSStatus PMPrinterGetOutputResolution(PMPrinter, PMPrintSettings,
/// PMResolution *)`.
pub unsafe extern "C" fn PMPrinterGetOutputResolution(
    _printer: *mut c_void,
    _print_settings: *mut c_void,
    resolution: *mut Resolution,
) -> OSStatus {
    if !resolution.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe {
            resolution.write(Resolution {
                h_res: 0.0,
                v_res: 0.0,
            })
        };
    }
    INVALID_PRINTER
}

/// `OSStatus PMPrinterSetOutputResolution(PMPrinter, PMPrintSettings, const
/// PMResolution *)`.
pub unsafe extern "C" fn PMPrinterSetOutputResolution(
    _printer: *mut c_void,
    _print_settings: *mut c_void,
    _resolution: *const Resolution,
) -> OSStatus {
    INVALID_PRINTER
}

exports::exports! {
    functions {
        "PMSessionGetCurrentPrinter" => PMSessionGetCurrentPrinter,
        "PMPrinterGetPrinterResolutionCount" => PMPrinterGetPrinterResolutionCount,
        "PMPrinterGetIndexedPrinterResolution" => PMPrinterGetIndexedPrinterResolution,
        "PMPrinterGetOutputResolution" => PMPrinterGetOutputResolution,
        "PMPrinterSetOutputResolution" => PMPrinterSetOutputResolution,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_default_printer_leaves_the_output_printer_null() {
        let mut printer: *mut c_void = std::ptr::dangling_mut::<c_void>();
        // SAFETY: `printer` is a writable out-parameter.
        let status = unsafe { PMSessionGetCurrentPrinter(std::ptr::null_mut(), &mut printer) };
        assert_eq!(status, NO_DEFAULT_PRINTER);
        assert!(printer.is_null());
    }

    #[test]
    fn resolution_queries_on_a_runtime_with_no_printers_refuse_and_zero_the_count() {
        let mut count = 7u32;
        // SAFETY: `count` is a writable out-parameter.
        let status =
            unsafe { PMPrinterGetPrinterResolutionCount(std::ptr::null_mut(), &mut count) };
        assert_eq!(status, INVALID_PRINTER);
        assert_eq!(count, 0);
    }

    #[test]
    fn an_indexed_resolution_query_refuses_and_zeroes_its_output() {
        let mut resolution = Resolution {
            h_res: 300.0,
            v_res: 300.0,
        };
        // SAFETY: `resolution` is a writable out-parameter.
        let status = unsafe {
            PMPrinterGetIndexedPrinterResolution(std::ptr::null_mut(), 1, &mut resolution)
        };
        assert_eq!(status, INVALID_PRINTER);
        assert_eq!(resolution.h_res, 0.0);
        assert_eq!(resolution.v_res, 0.0);
    }
}
