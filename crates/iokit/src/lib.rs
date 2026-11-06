#![allow(clippy::missing_safety_doc, clippy::not_unsafe_ptr_arg_deref)]

pub mod api;
pub mod hid;
pub mod manager;
pub mod power;
pub mod queue;
pub mod registry;
mod schedule;
pub mod value;

/// Emit HID discovery diagnostics when `ROSEMARY_TRACE_HID` is set.
pub(crate) fn trace_hid(message: impl std::fmt::Display) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("ROSEMARY_TRACE_HID").is_some()) {
        eprintln!("rosemary: HID: {message}");
    }
}

/// Whether the SDL3-backed IOKit services are available.
pub fn available() -> bool {
    sdl3::available()
}

exports::exports! {
    functions {
        "IOHIDManagerCreate" => api::IOHIDManagerCreate,
        "IOHIDQueueCreate" => queue::IOHIDQueueCreate,
        "IOHIDQueueAddElement" => queue::IOHIDQueueAddElement,
        "IOHIDQueueRemoveElement" => queue::IOHIDQueueRemoveElement,
        "IOHIDQueueContainsElement" => queue::IOHIDQueueContainsElement,
        "IOHIDQueueStart" => queue::IOHIDQueueStart,
        "IOHIDQueueStop" => queue::IOHIDQueueStop,
        "IOHIDQueueCopyNextValue" => queue::IOHIDQueueCopyNextValue,
        "IOHIDQueueCopyNextValueWithTimeout" => queue::IOHIDQueueCopyNextValueWithTimeout,
        "IOHIDDeviceSetProperty" => queue::IOHIDDeviceSetProperty,
        "IOHIDManagerOpen" => api::IOHIDManagerOpen,
        "IOHIDManagerClose" => api::IOHIDManagerClose,
        "IOHIDManagerGetTypeID" => api::IOHIDManagerGetTypeID,
        "IOHIDManagerSetDeviceMatching" => api::IOHIDManagerSetDeviceMatching,
        "IOHIDManagerSetDeviceMatchingMultiple" => api::IOHIDManagerSetDeviceMatchingMultiple,
        "IOHIDManagerRegisterDeviceMatchingCallback" => api::IOHIDManagerRegisterDeviceMatchingCallback,
        "IOHIDManagerRegisterDeviceRemovalCallback" => api::IOHIDManagerRegisterDeviceRemovalCallback,
        "IOHIDManagerRegisterInputValueCallback" => api::IOHIDManagerRegisterInputValueCallback,
        "IOHIDManagerScheduleWithRunLoop" => api::IOHIDManagerScheduleWithRunLoop,
        "IOHIDManagerUnscheduleFromRunLoop" => api::IOHIDManagerUnscheduleFromRunLoop,
        "IOHIDDeviceRegisterInputValueCallback" => api::IOHIDDeviceRegisterInputValueCallback,
        "IOHIDDeviceCreate" => api::IOHIDDeviceCreate,
        "IOHIDDeviceGetProperty" => api::IOHIDDeviceGetProperty,
        "IOHIDDeviceCopyMatchingElements" => api::IOHIDDeviceCopyMatchingElements,
        "IOHIDDeviceOpen" => api::IOHIDDeviceOpen,
        "IOHIDDeviceClose" => api::IOHIDDeviceClose,
        "IOHIDDeviceGetTypeID" => api::IOHIDDeviceGetTypeID,
        "IOHIDDeviceGetValue" => api::IOHIDDeviceGetValue,
        "IOHIDElementGetDevice" => api::IOHIDElementGetDevice,
        "IOHIDElementGetParent" => api::IOHIDElementGetParent,
        "IOHIDElementHasNullState" => api::IOHIDElementHasNullState,
        "IOHIDElementGetCookie" => api::IOHIDElementGetCookie,
        "IOHIDElementGetUsagePage" => api::IOHIDElementGetUsagePage,
        "IOHIDElementGetUsage" => api::IOHIDElementGetUsage,
        "IOHIDElementGetType" => api::IOHIDElementGetType,
        "IOHIDElementGetLogicalMin" => api::IOHIDElementGetLogicalMin,
        "IOHIDElementGetLogicalMax" => api::IOHIDElementGetLogicalMax,
        "IOHIDElementGetReportSize" => api::IOHIDElementGetReportSize,
        "IOHIDElementGetReportCount" => api::IOHIDElementGetReportCount,
        "IOHIDElementGetChildren" => api::IOHIDElementGetChildren,
        "IOHIDElementGetTypeID" => api::IOHIDElementGetTypeID,
        "IOHIDValueGetElement" => api::IOHIDValueGetElement,
        "IOHIDValueGetIntegerValue" => api::IOHIDValueGetIntegerValue,
        "IOHIDValueGetLength" => api::IOHIDValueGetLength,
        "IOHIDValueGetScaledValue" => api::IOHIDValueGetScaledValue,
        "IOHIDValueGetTypeID" => api::IOHIDValueGetTypeID,
        "IOHIDManagerCopyDevices" => api::IOHIDManagerCopyDevices,
        "IOHIDDeviceGetService" => api::IOHIDDeviceGetService,
        "IOHIDDeviceScheduleWithRunLoop" => api::IOHIDDeviceScheduleWithRunLoop,
        "IOHIDDeviceUnscheduleFromRunLoop" => api::IOHIDDeviceUnscheduleFromRunLoop,
        "IOHIDDeviceRegisterRemovalCallback" => api::IOHIDDeviceRegisterRemovalCallback,
        "IOHIDDeviceGetReport" => api::IOHIDDeviceGetReport,
        "IOHIDDeviceSetReport" => api::IOHIDDeviceSetReport,
        "IOHIDDeviceRegisterInputReportCallback" => api::IOHIDDeviceRegisterInputReportCallback,
    }
    groups {
        power,
        registry,
    }
}

/// The `kIOHID*Key` globals and [`registry`]'s master port, as
/// `(Mach-O name, address)`.
///
/// Each key is a `CFStringRef` **variable**: the guest loads the symbol's
/// address and then loads the string out of it, so the symbol is a cell holding
/// the object rather than the object. The string itself still has to be the
/// same one every time, because a matching dictionary compares its keys by
/// identity.
pub fn data_symbols() -> Vec<(String, u64)> {
    corefoundation::bootstrap();
    let mut out = registry::data_symbols();
    out.extend(api::key_strings().into_iter().map(|(symbol, text)| {
        (
            symbol.to_string(),
            corefoundation::base::global_cell(symbol, corefoundation::base::interned_name(text)),
        )
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key a guest builds a matching dictionary from is the *same object*
    /// each time it is read, which is what makes it work as a key at all.
    #[test]
    fn a_key_global_is_one_object() {
        let first = data_symbols();
        let second = data_symbols();
        assert_eq!(first, second);
        assert!(first.iter().all(|(_, addr)| *addr != 0));
    }
}
