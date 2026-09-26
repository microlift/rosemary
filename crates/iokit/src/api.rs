#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use corefoundation::api as cf;
use corefoundation::base::{CFIndex, CFTypeID, CFTypeRef};
use corefoundation::collections::{self, Number};

use crate::hid::{Device, RemovalCallback, ReportCallback};
use crate::manager::{Criterion, DeviceCallback, Manager, ValueCallback};
use crate::value;

type IOReturn = i32;
const SUCCESS: IOReturn = 0;
const ERROR: IOReturn = -536_870_212;

static MANAGERS: Mutex<Vec<Arc<Manager>>> = Mutex::new(Vec::new());

/// The SDL instance ids which currently back an `IOHIDDeviceRef`.
pub(crate) fn services() -> Vec<u32> {
    let mut services = MANAGERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .flat_map(|manager| manager.devices().into_iter().map(|device| device.id))
        .collect::<Vec<_>>();
    services.sort_unstable();
    services.dedup();
    services
}

/// Find a device by the SDL instance id used as its IORegistry service id.
pub(crate) fn device_for_service(service: u32) -> Option<Arc<Device>> {
    MANAGERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find_map(|manager| manager.device(service))
}

/// # Safety
///
/// `manager` must be null or a pointer this module handed out.
unsafe fn manager<'a>(manager: CFTypeRef) -> Option<&'a Manager> {
    // SAFETY: the caller's obligation; managers are never freed.
    (!manager.is_null()).then(|| unsafe { &*(manager as *const Manager) })
}

/// # Safety
///
/// `device` must be null or an `IOHIDDeviceRef` this module handed out.
unsafe fn device<'a>(device: CFTypeRef) -> Option<&'a Device> {
    (!device.is_null()).then(|| unsafe { &*(device as *const Device) })
}

pub unsafe extern "C" fn IOHIDManagerCreate(_allocator: CFTypeRef, _options: u32) -> CFTypeRef {
    let manager = Arc::new(Manager::new());
    let handle = Arc::as_ptr(&manager) as CFTypeRef;
    // A guest may `CFRetain` it, and this handle has no isa to retain through.
    corefoundation::base::note_typed_opaque(handle, crate::manager::type_id());
    MANAGERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(manager);
    crate::trace_hid(format_args!("IOHIDManagerCreate -> {handle:p}"));
    handle
}

/// Reopen a device from the IORegistry service id HIDAPI saved in its path.
pub unsafe extern "C" fn IOHIDDeviceCreate(_allocator: CFTypeRef, service: u32) -> CFTypeRef {
    for manager in MANAGERS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        if let Some(device) = manager.device(service) {
            crate::trace_hid(format_args!(
                "IOHIDDeviceCreate service={service} -> device"
            ));
            return Arc::as_ptr(&device) as CFTypeRef;
        }
    }
    std::ptr::null()
}

pub unsafe extern "C" fn IOHIDManagerOpen(manager: CFTypeRef, _options: u32) -> IOReturn {
    let result = match unsafe { self::manager(manager) }.is_some_and(|m| m.open()) {
        true => SUCCESS,
        false => ERROR,
    };
    crate::trace_hid(format_args!(
        "IOHIDManagerOpen manager={manager:p} -> {result}"
    ));
    result
}

pub unsafe extern "C" fn IOHIDManagerClose(manager: CFTypeRef, _options: u32) -> IOReturn {
    if let Some(m) = unsafe { self::manager(manager) } {
        m.close();
    }
    SUCCESS
}

pub unsafe extern "C" fn IOHIDManagerGetTypeID() -> CFTypeID {
    crate::manager::type_id()
}

pub unsafe extern "C" fn IOHIDDeviceGetTypeID() -> CFTypeID {
    crate::hid::device_type_id()
}

pub unsafe extern "C" fn IOHIDElementGetTypeID() -> CFTypeID {
    crate::hid::element_type_id()
}

pub unsafe extern "C" fn IOHIDValueGetTypeID() -> CFTypeID {
    value::type_id()
}

/// Read one matching dictionary into a [`Criterion`].
///
/// # Safety
///
/// `dictionary` must be null or a `CFDictionaryRef`.
unsafe fn criterion_of(dictionary: CFTypeRef) -> Criterion {
    unsafe fn number_for(dictionary: CFTypeRef, key: &std::ffi::CStr) -> Option<u32> {
        let name = corefoundation::string::from_bytes(key.to_bytes(), false);
        // SAFETY: the caller's obligation, and a string this call just made.
        let found = unsafe { cf::CFDictionaryGetValue(dictionary, name) };
        unsafe { cf::CFRelease(name) };
        unsafe { collections::number(found) }.map(|n| n.as_i64() as u32)
    }
    unsafe {
        Criterion {
            page: number_for(dictionary, c"DeviceUsagePage"),
            usage: number_for(dictionary, c"DeviceUsage"),
            vendor: number_for(dictionary, c"VendorID"),
            product: number_for(dictionary, c"ProductID"),
        }
    }
}

pub unsafe extern "C" fn IOHIDManagerSetDeviceMatching(manager: CFTypeRef, matching: CFTypeRef) {
    let Some(m) = (unsafe { self::manager(manager) }) else {
        return;
    };
    // Null means "match everything", which is what an empty criteria list is.
    let criteria = match matching.is_null() {
        true => Vec::new(),
        false => vec![unsafe { criterion_of(matching) }],
    };
    m.set_criteria(criteria);
}

pub unsafe extern "C" fn IOHIDManagerSetDeviceMatchingMultiple(
    manager: CFTypeRef,
    matching: CFTypeRef,
) {
    let Some(m) = (unsafe { self::manager(manager) }) else {
        return;
    };
    // SAFETY: as above - a `CFArrayRef` of dictionaries, or null.
    let count = unsafe { cf::CFArrayGetCount(matching) };
    let mut criteria = Vec::new();
    for i in 0..count {
        let entry = unsafe { cf::CFArrayGetValueAtIndex(matching, i) };
        criteria.push(unsafe { criterion_of(entry) });
    }
    m.set_criteria(criteria);
}

pub unsafe extern "C" fn IOHIDManagerRegisterDeviceMatchingCallback(
    manager: CFTypeRef,
    callback: Option<DeviceCallback>,
    context: *mut c_void,
) {
    if let Some(m) = unsafe { self::manager(manager) } {
        m.set_matched(callback, context as usize);
    }
}

pub unsafe extern "C" fn IOHIDManagerRegisterDeviceRemovalCallback(
    manager: CFTypeRef,
    callback: Option<DeviceCallback>,
    context: *mut c_void,
) {
    if let Some(m) = unsafe { self::manager(manager) } {
        m.set_removed(callback, context as usize);
    }
}

pub unsafe extern "C" fn IOHIDManagerRegisterInputValueCallback(
    manager: CFTypeRef,
    callback: Option<ValueCallback>,
    context: *mut c_void,
) {
    if let Some(manager) = unsafe { self::manager(manager) } {
        manager.set_values(callback, context as usize);
    }
}

pub unsafe extern "C" fn IOHIDDeviceRegisterInputValueCallback(
    device: CFTypeRef,
    callback: Option<ValueCallback>,
    context: *mut c_void,
) {
    let _ = device;
    for m in MANAGERS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        m.set_values(callback, context as usize);
    }
}

pub unsafe extern "C" fn IOHIDManagerScheduleWithRunLoop(
    manager: CFTypeRef,
    run_loop: CFTypeRef,
    mode: CFTypeRef,
) {
    let Some(m) = (unsafe { self::manager(manager) }) else {
        return;
    };
    // HIDAPI schedules a manager and immediately copies its devices without an
    // explicit IOHIDManagerOpen. Scheduling is enough to activate discovery.
    if !m.open() {
        return;
    }
    crate::trace_hid(format_args!(
        "IOHIDManagerScheduleWithRunLoop manager={manager:p}"
    ));
    schedule(m as *const Manager as usize, run_loop, mode);
}

pub unsafe extern "C" fn IOHIDManagerUnscheduleFromRunLoop(
    _manager: CFTypeRef,
    _run_loop: CFTypeRef,
    _mode: CFTypeRef,
) {
    // no-op: The source stays and finds no manager to poll once the
    // manager is closed, which is the same observable behaviour and avoids a
    // registry of sources keyed on a pair.
}

fn schedule(manager: usize, run_loop: CFTypeRef, mode: CFTypeRef) {
    unsafe extern "C" fn poll_one(info: *mut c_void) {
        // SAFETY: `info` is a `&Manager`'s address, and managers are never freed.
        if let Some(m) = unsafe { (info as *const Manager).as_ref() } {
            m.poll();
        }
    }
    unsafe extern "C" fn wake(_timer: CFTypeRef, info: *mut c_void) {
        if let Some(m) = unsafe { (info as *const Manager).as_ref() } {
            m.poll();
        }
    }

    let context = cf::CFRunLoopSourceContext {
        version: 0,
        info: manager as *mut c_void,
        retain: std::ptr::null(),
        release: std::ptr::null(),
        copy_description: std::ptr::null(),
        equal: std::ptr::null(),
        hash: std::ptr::null(),
        schedule: std::ptr::null(),
        cancel: std::ptr::null(),
        perform: Some(poll_one),
    };
    let mut timer_context = cf::CFRunLoopTimerContext {
        version: 0,
        info: manager as *mut c_void,
        retain: std::ptr::null(),
        release: std::ptr::null(),
        copy_description: std::ptr::null(),
    };
    // SAFETY: both contexts are this function's own and outlive the calls, which
    // copy what they need.
    unsafe {
        let source = cf::CFRunLoopSourceCreate(std::ptr::null(), 0, &context);
        cf::CFRunLoopAddSource(run_loop, source, mode);
        cf::CFRunLoopSourceSignal(source);

        let timer = cf::CFRunLoopTimerCreate(
            std::ptr::null(),
            cf::CFAbsoluteTimeGetCurrent(),
            1.0 / 60.0,
            0,
            0,
            Some(wake),
            &mut timer_context,
        );
        cf::CFRunLoopAddTimer(run_loop, timer, mode);
    }
}

/// `IOHIDDeviceGetProperty`, which is where a guest's GUID comes from.
///
/// The runner builds the GUID `SGamepadMapping::FindFromGUID` matches on out of
/// the vendor, product and version keys - so these are answered from SDL's own
/// USB identifiers rather than from anything SDL derived. Feeding it
/// `SDL_GetJoystickGUID` would be the wrong direction: that string is *made
/// from* these numbers.
pub unsafe extern "C" fn IOHIDDeviceGetProperty(device: CFTypeRef, key: CFTypeRef) -> CFTypeRef {
    let Some(d) = (unsafe { self::device(device) }) else {
        return std::ptr::null();
    };
    // SAFETY: as above - a `CFStringRef` or null.
    let key = unsafe { corefoundation::string::as_string(key) }
        .map(|s| s.utf8())
        .unwrap_or_default();
    crate::trace_hid(format_args!(
        "IOHIDDeviceGetProperty id={} key={}",
        d.id,
        String::from_utf8_lossy(&key)
    ));
    let number = |value: i64| {
        let n = collections::new_number(Number::Integer(value));
        // SAFETY: a fresh object handed straight back; the caller owns nothing.
        unsafe { cf::CFAutorelease(n) }
    };
    match key.as_slice() {
        b"VendorID" => number(d.vendor as i64),
        b"ProductID" => number(d.product as i64),
        b"VersionNumber" => number(d.version as i64),
        b"PrimaryUsagePage" => number(d.elements[0].page as i64),
        b"PrimaryUsage" => number(d.elements[0].usage as i64),
        b"MaxInputReportSize" => number(d.max_input_report_size() as i64),
        b"ReportDescriptor" => {
            let data = collections::new_data(d.report_descriptor());
            // SAFETY: a fresh property value valid through the caller's pool.
            unsafe { cf::CFAutorelease(data) }
        }
        b"DeviceUsagePairs" => {
            let page = corefoundation::string::from_bytes(b"DeviceUsagePage", false);
            let usage = corefoundation::string::from_bytes(b"DeviceUsage", false);
            let page_value = collections::new_number(Number::Integer(d.elements[0].page as i64));
            let usage_value = collections::new_number(Number::Integer(d.elements[0].usage as i64));
            let pair = collections::new_dictionary(
                &[(page, page_value), (usage, usage_value)],
                corefoundation::callbacks::Policy::from_key_callbacks(),
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            // SAFETY: the dictionary retained its four entries.
            unsafe {
                cf::CFRelease(page);
                cf::CFRelease(usage);
                cf::CFRelease(page_value);
                cf::CFRelease(usage_value);
            }
            let pairs = collections::new_array(
                &[pair],
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            );
            // SAFETY: the array retained its dictionary.
            unsafe { cf::CFRelease(pair) };
            // SAFETY: a fresh property value valid through the caller's pool.
            unsafe { cf::CFAutorelease(pairs) }
        }
        b"Transport" => {
            let s = corefoundation::string::from_bytes(b"USB", false);
            // SAFETY: a fresh property value valid through the caller's pool.
            unsafe { cf::CFAutorelease(s) }
        }
        b"SerialNumber" => std::ptr::null(),
        b"bInterfaceNumber" => number(0),
        b"Product" | b"Manufacturer" => {
            let s = corefoundation::string::from_bytes(d.name.as_bytes(), false);
            unsafe { cf::CFAutorelease(s) }
        }
        // A key this runtime does not answer is null, which is what a real
        // device that does not report it gives - and what a caller checks for.
        _ => std::ptr::null(),
    }
}

/// `IOHIDDeviceCopyMatchingElements`, which a guest walks to build its mapping.
///
/// The matching dictionary is ignored: every guest in this tree passes null,
/// which means "all of them", and a filter nobody exercises is a filter nobody
/// has checked.
pub unsafe extern "C" fn IOHIDDeviceCopyMatchingElements(
    device: CFTypeRef,
    _matching: CFTypeRef,
    _options: u32,
) -> CFTypeRef {
    let Some(d) = (unsafe { self::device(device) }) else {
        return std::ptr::null();
    };
    let handles: Vec<CFTypeRef> = d
        .elements
        .iter()
        .map(|e| d.handle(e.cookie) as CFTypeRef)
        .collect();
    crate::trace_hid(format_args!(
        "IOHIDDeviceCopyMatchingElements id={} -> {} elements",
        d.id,
        handles.len()
    ));
    // The elements are not CoreFoundation objects, so the array holds them with
    // no callbacks: nothing to retain, nothing to release, and identity by
    // pointer - which is what a guest comparing two element references wants.
    collections::new_array(&handles, corefoundation::callbacks::Policy::NONE, false)
}

pub unsafe extern "C" fn IOHIDDeviceOpen(_device: CFTypeRef, _options: u32) -> IOReturn {
    // The device was opened when it was matched; there is nothing left to do.
    SUCCESS
}

pub unsafe extern "C" fn IOHIDDeviceClose(_device: CFTypeRef, _options: u32) -> IOReturn {
    SUCCESS
}

/// Poll one element from a device.
pub unsafe extern "C" fn IOHIDDeviceGetValue(
    device: CFTypeRef,
    element: *const c_void,
    out: *mut CFTypeRef,
) -> IOReturn {
    if out.is_null() {
        return ERROR;
    }
    // SAFETY: the caller's two handles must be a device and one of its
    // elements.
    let Some(device) = (unsafe { self::device(device) }) else {
        return ERROR;
    };
    let Some((owner, cookie)) = (unsafe { value::element(element) }) else {
        return ERROR;
    };
    if !std::ptr::eq(device, owner) {
        return ERROR;
    }
    let Some(element) = device.element(cookie) else {
        return ERROR;
    };
    let integer = device.value(element);
    crate::trace_hid(format_args!(
        "IOHIDDeviceGetValue id={} cookie={cookie} value={integer}",
        device.id
    ));
    let value = value::new(device, cookie, integer);
    // `GetValue` has no Create/Copy ownership; keep the value valid through the
    // caller's autorelease pool.
    let value = unsafe { cf::CFAutorelease(value) };
    // SAFETY: checked non-null above.
    unsafe { out.write(value) };
    SUCCESS
}

/// Read one field of an element.
macro_rules! element_field {
    ($(#[$meta:meta])* $name:ident, $ty:ty, $read:expr, $default:expr) => {
        $(#[$meta])*
        pub unsafe extern "C" fn $name(element: *const c_void) -> $ty {
            // SAFETY: an `IOHIDElementRef` this
            // module handed out, whose device is still open.
            match unsafe { value::element(element) }
                .and_then(|(device, cookie)| device.element(cookie))
            {
                Some(e) => $read(e),
                None => $default,
            }
        }
    };
}

element_field!(/// The element's opaque identity, which a guest stores and looks up.
    IOHIDElementGetCookie, u32, |e: &crate::hid::Element| e.cookie, 0);
element_field!(/// `kHIDPage_GenericDesktop`, `kHIDPage_Button`, and so on.
    IOHIDElementGetUsagePage, u32, |e: &crate::hid::Element| e.page, 0);
element_field!(/// Which control on that page.
    IOHIDElementGetUsage, u32, |e: &crate::hid::Element| e.usage, 0);
element_field!(/// Axis, button, hat or collection.
    IOHIDElementGetType, u32, |e: &crate::hid::Element| e.kind, 0);
element_field!(/// The lowest value this element reports.
    IOHIDElementGetLogicalMin, CFIndex, |e: &crate::hid::Element| e.logical_min as CFIndex, 0);
element_field!(/// The highest.
    IOHIDElementGetLogicalMax, CFIndex, |e: &crate::hid::Element| e.logical_max as CFIndex, 0);

/// `IOHIDElementRef IOHIDElementGetParent(IOHIDElementRef)`.
///
/// The tree this module publishes is one collection deep, and
/// [`crate::hid::describe`] says so where it builds it: element zero is the
/// root collection and every other element is its child. So the answer is the
/// root for everything and null for the root itself, which is what "the root has
/// no parent" means.
///
/// That is not a convenience answer, it is the tree: a guest that walks up from
/// an axis to find the `kHIDPage_GenericDesktop`/`kHIDUsage_Gamepad` collection
/// above it gets there, and one that walks up from the root stops. The guest
/// does exactly that at `Framework+0x83bd5d3`, testing the result for null
/// (`test rax,rax; je <next element>`) so the walk terminates either way - but
/// with no parent at all it would never find the usage it is looking for and
/// would silently report no buttons and no axes.
pub unsafe extern "C" fn IOHIDElementGetParent(element: *const c_void) -> CFTypeRef {
    let Some((device, cookie)) = (unsafe { value::element(element) }) else {
        return std::ptr::null();
    };
    match cookie {
        0 => std::ptr::null(),
        _ => device.handle(0) as CFTypeRef,
    }
}

/// `IOHIDDeviceRef IOHIDElementGetDevice(IOHIDElementRef)`.
///
/// The `IOHIDDeviceRef` that owns the element, which is the device itself: in
/// this crate an `IOHIDDeviceRef` *is* the `*const Device`
/// ([`IOHIDDeviceGetService`] reads it that way), and
/// [`crate::value::ElementRef`] records the owning device for exactly this
/// answer.
///
/// The guest compares the result for pointer identity against the devices it
/// enumerated - `cmp QWORD [r15+0x28], rax` at `Framework+0x83be8bc` - so
/// returning the same address the enumeration handed out is what makes the
/// comparison succeed. Returning a fresh object per call would match nothing
/// and silently drop every value.
pub unsafe extern "C" fn IOHIDElementGetDevice(element: *const c_void) -> CFTypeRef {
    let Some((device, _cookie)) = (unsafe { value::element(element) }) else {
        return std::ptr::null();
    };
    std::ptr::from_ref(device) as CFTypeRef
}

/// `uint32_t IOHIDElementGetReportSize(IOHIDElementRef, IOOptionBits options)`.
///
/// The width of the element's own value in the device's report - which is what
/// [`IOHIDValueGetLength`] already answers for the same element, from the same
/// model, and is the only report this crate produces. See that function for why
/// a `Collection` is zero bytes wide.
///
/// **32-bit, not `CFIndex`.** The guest zero-extends the result itself -
/// `mov eax, eax` at `Framework+0x83bd7a0`, immediately after the call - which
/// only a compiler that believed the return was 32 bits wide would emit. The
/// binary is the authority on the width, and `IOHIDValueGetLength`'s `CFIndex`
/// is a different function with a different declared type.
///
/// The options are reserved and ignored; the guest passes none.
pub unsafe extern "C" fn IOHIDElementGetReportSize(
    element: *const c_void,
    _options: u32,
) -> u32 {
    // SAFETY: forwarded unchanged - the same element pointer and the same
    // precondition, and `IOHIDValueGetLength` reads nothing `GetReportSize`
    // does not. The width is this function's own, narrowed on purpose.
    unsafe { IOHIDValueGetLength(element) as u32 }
}

/// `CFIndex IOHIDElementGetReportID(IOHIDElementRef)`.
///
/// **0**, the report id of the single synthetic report
/// [`crate::hid::Device::input_report`] describes, and the one
/// [`IOHIDDeviceGetReport`] answers for `kIOHIDReportTypeInput` with an id of
/// 0. The guest uses only the low byte of this (`movzx esi, BYTE PTR
/// [rbp-0x2c]` at `Framework+0x83bd8e2`) as the report id to hand the raw
/// bytes to, so 0 is the id those bytes actually have.
pub unsafe extern "C" fn IOHIDElementGetReportID(_element: *const c_void) -> u32 {
    0
}

/// `const uint8_t *IOHIDValueGetBytePtr(IOHIDValueRef)`.
///
/// A pointer into the report the device delivered, and **null** when this
/// runtime has no such report for it.
///
/// The distinction is the API's own: `IOHIDValueGetBytePtr` is documented as
/// returning "a pointer to the value's data, or NULL if the value has no data",
/// and a value synthesised from SDL's per-element getters has no report behind
/// it - there is no byte in any buffer that means "this axis is at 12000"
/// unless a device delivered a real HID report, and inventing one would be a
/// plausible-looking number for a value nobody measured.
///
/// Where a real report *does* exist the pointer is into the caller's own
/// registered buffer: [`crate::hid::Device::set_report_callback`] records the
/// `report`/`length` pair the guest handed
/// `IOHIDDeviceRegisterInputReportCallback`, and that buffer is filled from the
/// device before the callback runs. Handing back its address is then literally
/// true - it is the same memory the guest wrote.
///
/// # Safety
///
/// `value` must be an `IOHIDValueRef` this module made, or null.
pub unsafe extern "C" fn IOHIDValueGetBytePtr(v: CFTypeRef) -> *const u8 {
    let Some(value) = (unsafe { value::borrow(v) }) else {
        return std::ptr::null();
    };
    let Some(device) = (unsafe { value::device(value) }) else {
        return std::ptr::null();
    };
    device.report_bytes()
}

/// `IOHIDElementGetChildren`: everything under a collection.
///
/// Only the root collection has children here, and its children are every other
/// element - which is the shape a guest walking from the top expects, and the
/// shape a real gamepad's report descriptor mostly has.
pub unsafe extern "C" fn IOHIDElementGetChildren(element: *const c_void) -> CFTypeRef {
    let Some((device, cookie)) = (unsafe { value::element(element) }) else {
        return std::ptr::null();
    };
    if cookie != 0 {
        return std::ptr::null();
    }
    let handles: Vec<CFTypeRef> = device
        .elements
        .iter()
        .skip(1)
        .map(|e| device.handle(e.cookie) as CFTypeRef)
        .collect();
    collections::new_array(&handles, corefoundation::callbacks::Policy::NONE, false)
}

pub unsafe extern "C" fn IOHIDValueGetElement(v: CFTypeRef) -> *const c_void {
    // SAFETY: a value handed to a callback, read
    // during that callback.
    let Some(v) = (unsafe { value::borrow(v) }) else {
        return std::ptr::null();
    };
    match unsafe { value::device(v) } {
        Some(device) => device.handle(v.cookie),
        None => std::ptr::null(),
    }
}

pub unsafe extern "C" fn IOHIDValueGetIntegerValue(v: CFTypeRef) -> CFIndex {
    unsafe { value::borrow(v) }.map_or(0, |v| v.integer as CFIndex)
}

/// The width of the element's value in its HID report.
pub unsafe extern "C" fn IOHIDValueGetLength(v: CFTypeRef) -> CFIndex {
    let Some(value) = (unsafe { value::borrow(v) }) else {
        return 0;
    };
    // SAFETY: as above; the value keeps the originating device and cookie.
    let Some(element) = (unsafe { value::device(value) }).and_then(|d| d.element(value.cookie))
    else {
        return 0;
    };
    match element.source {
        crate::hid::Source::Axis(_) | crate::hid::Source::GamepadAxis(_, _) => {
            size_of::<i16>() as CFIndex
        }
        crate::hid::Source::Button(_)
        | crate::hid::Source::GamepadButton(_)
        | crate::hid::Source::Hat(_) => size_of::<u8>() as CFIndex,
        crate::hid::Source::Collection => 0,
    }
}

/// Unused, but part of the shape: a caller may ask for a scaled reading.
pub unsafe extern "C" fn IOHIDValueGetScaledValue(v: CFTypeRef, _kind: u32) -> f64 {
    unsafe { value::borrow(v) }.map_or(0.0, |v| v.integer as f64)
}

/// Keys a guest builds a matching dictionary from, exported as data.
///
/// `CFStringRef` globals, so the same object every time - a guest stores one and
/// uses it as a dictionary key, and two different objects would not match.
pub fn key_strings() -> Vec<(&'static str, &'static str)> {
    vec![
        ("_kIOHIDDeviceUsagePageKey", "DeviceUsagePage"),
        ("_kIOHIDDeviceUsageKey", "DeviceUsage"),
        ("_kIOHIDVendorIDKey", "VendorID"),
        ("_kIOHIDProductIDKey", "ProductID"),
        ("_kIOHIDVersionNumberKey", "VersionNumber"),
        ("_kIOHIDPrimaryUsagePageKey", "PrimaryUsagePage"),
        ("_kIOHIDPrimaryUsageKey", "PrimaryUsage"),
        ("_kIOHIDProductKey", "Product"),
        ("_kIOHIDManufacturerKey", "Manufacturer"),
        ("_kIOHIDMaxInputReportSizeKey", "MaxInputReportSize"),
        ("_kIOHIDReportDescriptorKey", "ReportDescriptor"),
        ("_kIOHIDDeviceUsagePairsKey", "DeviceUsagePairs"),
        ("_kIOHIDTransportKey", "Transport"),
        ("_kIOHIDSerialNumberKey", "SerialNumber"),
    ]
}

/// `CFSetRef IOHIDManagerCopyDevices(IOHIDManagerRef)`.
///
/// Null when no devices have matched, otherwise a retained set of the devices
/// the manager currently owns.
pub unsafe extern "C" fn IOHIDManagerCopyDevices(manager: CFTypeRef) -> CFTypeRef {
    let Some(manager) = (unsafe { self::manager(manager) }) else {
        return std::ptr::null();
    };
    // Populate synchronously: Some apps HIDAPI copies immediately after
    // scheduling, before its run loop source has had a chance to perform.
    manager.poll();
    let devices = manager.devices();
    if devices.is_empty() {
        return std::ptr::null();
    }
    let handles: Vec<CFTypeRef> = devices
        .iter()
        .map(|device| Arc::as_ptr(device) as CFTypeRef)
        .collect();
    collections::new_set(
        &handles,
        corefoundation::callbacks::Policy::from_type_callbacks(),
    )
}

/// `io_service_t IOHIDDeviceGetService(IOHIDDeviceRef)`.
///
/// SDL's stable joystick instance id doubles as the synthetic IORegistry
/// service id. HIDAPI saves it in a path and later gives it to
/// [`IOHIDDeviceCreate`] to reopen this exact device.
pub unsafe extern "C" fn IOHIDDeviceGetService(device: CFTypeRef) -> u32 {
    let service = unsafe { self::device(device) }.map_or(0, |device| device.id);
    crate::trace_hid(format_args!("IOHIDDeviceGetService -> {service}"));
    service
}

/// `IOHIDDeviceScheduleWithRunLoop` / `IOHIDDeviceUnscheduleFromRunLoop`.
///
/// Accepted and ignored. Scheduling is what makes callbacks arrive on a run
/// loop, and this crate's arrive from SDL's own polling instead - so there is
/// nothing to attach, and refusing would stop a program that only wanted its
/// callbacks to work.
pub unsafe extern "C" fn IOHIDDeviceScheduleWithRunLoop(
    device: CFTypeRef,
    _run_loop: CFTypeRef,
    _mode: CFTypeRef,
) {
    if let Some(device) = unsafe { self::device(device) } {
        crate::trace_hid(format_args!(
            "IOHIDDeviceScheduleWithRunLoop id={}",
            device.id
        ));
    }
}

pub unsafe extern "C" fn IOHIDDeviceUnscheduleFromRunLoop(
    _device: CFTypeRef,
    _run_loop: CFTypeRef,
    _mode: CFTypeRef,
) {
}

/// `IOHIDDeviceRegisterRemovalCallback(device, callback, context)`.
///
/// SDL reports removal while polling the manager, which delivers both the
/// device callback registered here and the manager-level callback.
pub unsafe extern "C" fn IOHIDDeviceRegisterRemovalCallback(
    device: CFTypeRef,
    callback: Option<RemovalCallback>,
    context: *mut c_void,
) {
    if let Some(device) = unsafe { self::device(device) } {
        device.set_removal_callback(callback, context as usize);
    }
}

/// The raw-report family: `IOHIDDeviceGetReport`, `IOHIDDeviceSetReport`,
/// `IOHIDDeviceRegisterInputReportCallback`.
pub unsafe extern "C" fn IOHIDDeviceGetReport(
    device: CFTypeRef,
    report_type: u32,
    report_id: CFIndex,
    report: *mut u8,
    length: *mut CFIndex,
) -> IOReturn {
    if report.is_null()
        || length.is_null()
        || !(report_type == 0 || report_type == 2)
        || !(0..=u8::MAX as CFIndex).contains(&report_id)
    {
        return ERROR;
    }
    // SAFETY: the caller's obligation and the out pointer checked above.
    let Some(device) = (unsafe { self::device(device) }) else {
        return ERROR;
    };
    // SAFETY: the caller supplies the buffer capacity through `length`.
    let capacity = unsafe { length.read() };
    if capacity < 0 {
        return ERROR;
    }
    if device.has_raw_reports() {
        let extra_id = usize::from(report_id == 0);
        let Some(bytes) = device.raw_get_report(
            report_type == 2,
            report_id as u8,
            capacity as usize + extra_id,
        ) else {
            return ERROR;
        };
        let bytes = match report_id == 0 {
            true => bytes.get(1..).unwrap_or_default(),
            false => bytes.as_slice(),
        };
        let copied = bytes.len().min(capacity as usize);
        // SAFETY: the caller promises `capacity` writable bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), report, copied);
            length.write(copied as CFIndex);
        }
        return SUCCESS;
    }
    if report_type != 0 || report_id != 0 {
        return ERROR;
    }
    let bytes = device.input_report();
    let copied = bytes.len().min(capacity as usize);
    // SAFETY: the caller promises `capacity` writable bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), report, copied);
        length.write(copied as CFIndex);
    }
    SUCCESS
}

pub unsafe extern "C" fn IOHIDDeviceSetReport(
    device: CFTypeRef,
    report_type: u32,
    report_id: CFIndex,
    report: *const u8,
    length: CFIndex,
) -> IOReturn {
    if report.is_null()
        || length < 0
        || !(report_type == 1 || report_type == 2)
        || !(0..=u8::MAX as CFIndex).contains(&report_id)
    {
        return ERROR;
    }
    let Some(device) = (unsafe { self::device(device) }) else {
        return ERROR;
    };
    // SAFETY: the caller promises `length` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(report, length as usize) };
    if device.has_raw_reports() {
        let mut with_id;
        let bytes = if report_id == 0 {
            with_id = Vec::with_capacity(bytes.len() + 1);
            with_id.push(0);
            with_id.extend_from_slice(bytes);
            with_id.as_slice()
        } else {
            bytes
        };
        return match device.raw_set_report(report_type == 2, bytes) {
            Some(true) => SUCCESS,
            _ => ERROR,
        };
    }
    if report_type != 1 {
        return ERROR;
    }
    // SAFETY: the device is open and the bytes belong to this call.
    match unsafe { sdl3::send_effect(device.joystick, bytes) } {
        true => SUCCESS,
        false => ERROR,
    }
}

pub unsafe extern "C" fn IOHIDDeviceRegisterInputReportCallback(
    device: CFTypeRef,
    report: *mut u8,
    length: CFIndex,
    callback: Option<ReportCallback>,
    context: *mut c_void,
) {
    if length < 0 {
        return;
    }
    if let Some(device) = unsafe { self::device(device) } {
        crate::trace_hid(format_args!(
            "IOHIDDeviceRegisterInputReportCallback id={} length={length}",
            device.id
        ));
        device.set_report_callback(report, length as usize, callback, context as usize);
    }
}
