#![allow(non_snake_case)]

use std::collections::BTreeMap;
use std::ffi::{c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;

type IOReturn = i32;
type KernReturn = i32;
/// `io_object_t`, `io_iterator_t`, `io_service_t`, `io_registry_entry_t`: all
/// `mach_port_t`, represented by SDL instance ids or synthetic iterator handles.
type IoObject = u32;

const SUCCESS: IOReturn = 0;
/// `kIOReturnNoDevice`. The honest failure for a lookup in an empty registry,
/// and distinct from `kIOReturnError` so a caller that tells them apart can.
const NO_DEVICE: IOReturn = 0xe000_02c0u32 as i32;

/// Iterator handles are kept separate from the positive SDL instance ids used
/// as synthetic `io_service_t`s.
static NEXT_ITERATOR: AtomicU32 = AtomicU32::new(0x8000_0000);
/// An iterator's services and how far through them it is. Kept whole rather
/// than consumed, so `IOIteratorReset` can start it over.
struct Listing {
    services: Vec<IoObject>,
    next: usize,
}

static ITERATORS: Mutex<BTreeMap<IoObject, Listing>> = Mutex::new(BTreeMap::new());

/// `mach_port_t kIOMasterPortDefault` - and `kIOMainPortDefault`, its current
/// spelling.
///
/// `MACH_PORT_NULL`, which is its value on a Mac too: the constant means "ask
/// for the default", not "here is the master port". A guest loads it and hands
/// it straight to a lookup, so the only thing that matters is that the *cell*
/// exists - an unbound data import here is a load from address zero, which is
/// the fault this symbol was found by.
pub fn data_symbols() -> Vec<(String, u64)> {
    ["_kIOMasterPortDefault", "_kIOMainPortDefault"]
        .into_iter()
        .map(|name| (name.to_string(), corefoundation::base::global_cell(name, 0)))
        .collect()
}

/// `CFMutableDictionaryRef IOServiceMatching(const char *name)`.
///
/// The dictionary really does carry `IOProviderClass`, because a caller is
/// entitled to add keys to it and to read back what it asked for.
pub unsafe extern "C" fn IOServiceMatching(name: *const c_char) -> CFTypeRef {
    // SAFETY: the caller's obligation; a null name gives an empty dictionary,
    // which matches every discoverable HID device.
    let value = match unsafe { corefoundation::string::cstr_bytes(name) } {
        Some(bytes) => corefoundation::string::from_bytes(bytes, false),
        None => return empty_dictionary(),
    };
    let key = corefoundation::string::from_bytes(b"IOProviderClass", false);
    matching_dictionary(key, value)
}

/// `CFMutableDictionaryRef IORegistryEntryIDMatching(uint64_t entryID)`.
pub unsafe extern "C" fn IORegistryEntryIDMatching(entry: u64) -> CFTypeRef {
    let key = corefoundation::string::from_bytes(b"IORegistryEntryID", false);
    let value = collections::new_number(collections::Number::Integer(entry as i64));
    matching_dictionary(key, value)
}

fn matching_dictionary(key: CFTypeRef, value: CFTypeRef) -> CFTypeRef {
    let dictionary = collections::new_dictionary(
        &[(key, value)],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        true,
    );
    unsafe {
        corefoundation::api::CFRelease(key);
        corefoundation::api::CFRelease(value);
    }
    dictionary
}

fn empty_dictionary() -> CFTypeRef {
    collections::new_dictionary(
        &[],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        true,
    )
}

/// The matching dictionary is **consumed** by every `IOServiceGetMatching*`
/// call, which is the one piece of IOKit ownership a caller does not do itself.
///
/// # Safety
///
/// `matching` must be null or a CFTypeRef.
unsafe fn consume(matching: CFTypeRef) {
    if !matching.is_null() {
        // SAFETY: the caller's obligation, and the call is a release of a
        // reference the caller has just handed over.
        unsafe { corefoundation::api::CFRelease(matching) };
    }
}

unsafe fn matching_services(matching: CFTypeRef) -> Vec<u32> {
    if matching.is_null() {
        return crate::api::services();
    }
    let get = |name: &[u8]| {
        let key = corefoundation::string::from_bytes(name, false);
        let value = unsafe { corefoundation::api::CFDictionaryGetValue(matching, key) };
        unsafe { corefoundation::api::CFRelease(key) };
        value
    };
    let provider = get(b"IOProviderClass");
    if let Some(provider) = unsafe { corefoundation::string::as_string(provider) }
        && ![b"IOHIDDevice".as_slice(), b"IOHIDDevice2".as_slice()]
            .contains(&provider.utf8().as_slice())
    {
        return Vec::new();
    }
    crate::hid::Device::discover()
        .into_iter()
        .filter(|device| {
            for (key, actual) in [
                (b"IORegistryEntryID".as_slice(), device.id as i64),
                (b"VendorID".as_slice(), device.vendor as i64),
                (b"ProductID".as_slice(), device.product as i64),
                (
                    b"DeviceUsagePage".as_slice(),
                    device.elements[0].page as i64,
                ),
                (b"DeviceUsage".as_slice(), device.elements[0].usage as i64),
            ] {
                if let Some(value) = unsafe { collections::number(get(key)) }
                    && value.as_i64() != actual
                {
                    return false;
                }
            }
            for key in [b"IONameMatch".as_slice(), b"BSD Name".as_slice()] {
                if let Some(value) = unsafe { corefoundation::string::as_string(get(key)) }
                    && value.utf8() != device.name.as_bytes()
                {
                    return false;
                }
            }
            true
        })
        .map(|d| d.id)
        .collect()
}

/// `kern_return_t IOServiceGetMatchingServices(port, matching, io_iterator_t *)`.
pub unsafe extern "C" fn IOServiceGetMatchingServices(
    _port: u32,
    matching: CFTypeRef,
    existing: *mut IoObject,
) -> KernReturn {
    let services = unsafe { matching_services(matching) };
    // SAFETY: the reference the caller handed over.
    unsafe { consume(matching) };
    if !existing.is_null() {
        let iterator = NEXT_ITERATOR.fetch_add(1, Ordering::Relaxed);
        {
            ITERATORS.lock().unwrap_or_else(|e| e.into_inner()).insert(
                iterator,
                Listing {
                    services: services.to_vec(),
                    next: 0,
                },
            );
        }
        crate::trace_hid(format_args!(
            "IOServiceGetMatchingServices -> {} service(s)",
            services.len()
        ));
        // SAFETY: an out parameter the caller sized.
        unsafe { existing.write(iterator) };
    }
    SUCCESS
}

/// Resolve the synthetic service id in an `IORegistryEntryIDMatching`
/// dictionary, including HID class, identity, usage, and registry-id filters.
pub unsafe extern "C" fn IOServiceGetMatchingService(_port: u32, matching: CFTypeRef) -> IoObject {
    let entry = unsafe { matching_services(matching) }
        .first()
        .copied()
        .unwrap_or(0);
    unsafe {
        consume(matching);
    }
    entry
}

/// `io_object_t IOIteratorNext(io_iterator_t)`. Zero ends the loop.
pub unsafe extern "C" fn IOIteratorNext(iterator: IoObject) -> IoObject {
    ITERATORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&iterator)
        .and_then(|listing| {
            let service = listing.services.get(listing.next).copied()?;
            listing.next += 1;
            Some(service)
        })
        .unwrap_or(0)
}

/// `void IOIteratorReset(io_iterator_t)`: back to the first service.
pub unsafe extern "C" fn IOIteratorReset(iterator: IoObject) {
    if let Some(listing) = ITERATORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&iterator)
    {
        listing.next = 0;
    }
}

/// `boolean_t IOIteratorIsValid(io_iterator_t)`.
///
/// True, and that is not the same as "there is something in it": invalid means
/// the registry changed under the iteration and the caller must start again, so
/// answering false would put a program that checks into a loop.
pub unsafe extern "C" fn IOIteratorIsValid(_iterator: IoObject) -> u32 {
    1
}

pub unsafe extern "C" fn IOObjectRelease(object: IoObject) -> KernReturn {
    ITERATORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&object);
    SUCCESS
}

pub unsafe extern "C" fn IOObjectRetain(_object: IoObject) -> KernReturn {
    SUCCESS
}

pub unsafe extern "C" fn IOObjectIsEqualTo(a: IoObject, b: IoObject) -> u32 {
    u32::from(a == b)
}

/// `io_registry_entry_t IORegistryEntryFromPath(port, const io_string_t path)`.
pub unsafe extern "C" fn IORegistryEntryFromPath(_port: u32, _path: *const c_char) -> IoObject {
    0
}

pub unsafe extern "C" fn IORegistryEntryGetParentEntry(
    _entry: IoObject,
    _plane: *const c_char,
    parent: *mut IoObject,
) -> KernReturn {
    if !parent.is_null() {
        // SAFETY: an out parameter the caller sized.
        unsafe { parent.write(0) };
    }
    NO_DEVICE
}

pub unsafe extern "C" fn IORegistryEntryGetRegistryEntryID(
    entry: IoObject,
    id: *mut u64,
) -> KernReturn {
    if !id.is_null() {
        // SAFETY: an out parameter the caller sized.
        unsafe { id.write(entry as u64) };
    }
    match entry {
        0 => NO_DEVICE,
        _ => SUCCESS,
    }
}

fn property(device: &crate::hid::Device, key: &[u8]) -> CFTypeRef {
    let number = |value: i64| collections::new_number(collections::Number::Integer(value));
    match key {
        b"VendorID" => number(device.vendor as i64),
        b"ProductID" => number(device.product as i64),
        b"VersionNumber" => number(device.version as i64),
        b"PrimaryUsagePage" | b"DeviceUsagePage" => number(device.elements[0].page as i64),
        b"PrimaryUsage" | b"DeviceUsage" => number(device.elements[0].usage as i64),
        b"LocationID" | b"IOHIDLocationID" => number(device.id as i64),
        b"MaxInputReportSize" => number(device.max_input_report_size() as i64),
        b"ReportDescriptor" | b"IOHIDReportDescriptorKey" => {
            collections::new_data(device.report_descriptor())
        }
        b"DeviceUsagePairs" => device_usage_pairs(device),
        b"Product" | b"Manufacturer" => {
            corefoundation::string::from_bytes(device.name.as_bytes(), false)
        }
        b"Transport" => corefoundation::string::from_bytes(b"USB", false),
        b"bInterfaceNumber" => number(0),
        _ => std::ptr::null_mut(),
    }
}

fn device_usage_pairs(device: &crate::hid::Device) -> CFTypeRef {
    let page = corefoundation::string::from_bytes(b"DeviceUsagePage", false);
    let usage = corefoundation::string::from_bytes(b"DeviceUsage", false);
    let page_value =
        collections::new_number(collections::Number::Integer(device.elements[0].page as i64));
    let usage_value = collections::new_number(collections::Number::Integer(
        device.elements[0].usage as i64,
    ));
    let pair = collections::new_dictionary(
        &[(page, page_value), (usage, usage_value)],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: the dictionary retained its entries.
    unsafe {
        corefoundation::api::CFRelease(page);
        corefoundation::api::CFRelease(usage);
        corefoundation::api::CFRelease(page_value);
        corefoundation::api::CFRelease(usage_value);
    }
    let pairs = collections::new_array(&[pair], Policy::from_type_callbacks(), false);
    // SAFETY: the array retained the dictionary.
    unsafe { corefoundation::api::CFRelease(pair) };
    pairs
}

fn registry_properties(device: &crate::hid::Device) -> CFTypeRef {
    let entries = [
        (
            b"IOProviderClass".as_slice(),
            corefoundation::string::from_bytes(b"IOHIDDevice", false),
        ),
        (b"VendorID".as_slice(), property(device, b"VendorID")),
        (b"ProductID".as_slice(), property(device, b"ProductID")),
        (
            b"VersionNumber".as_slice(),
            property(device, b"VersionNumber"),
        ),
        (
            b"PrimaryUsagePage".as_slice(),
            property(device, b"PrimaryUsagePage"),
        ),
        (
            b"PrimaryUsage".as_slice(),
            property(device, b"PrimaryUsage"),
        ),
        (b"LocationID".as_slice(), property(device, b"LocationID")),
        (
            b"MaxInputReportSize".as_slice(),
            property(device, b"MaxInputReportSize"),
        ),
        (
            b"ReportDescriptor".as_slice(),
            property(device, b"ReportDescriptor"),
        ),
        (
            b"DeviceUsagePairs".as_slice(),
            property(device, b"DeviceUsagePairs"),
        ),
        (b"Product".as_slice(), property(device, b"Product")),
        (
            b"Manufacturer".as_slice(),
            property(device, b"Manufacturer"),
        ),
        (b"Transport".as_slice(), property(device, b"Transport")),
        (
            b"bInterfaceNumber".as_slice(),
            property(device, b"bInterfaceNumber"),
        ),
    ];
    let keys: Vec<_> = entries
        .iter()
        .map(|(key, _)| corefoundation::string::from_bytes(key, false))
        .collect();
    let pairs: Vec<_> = keys
        .iter()
        .zip(entries)
        .map(|(key, (_, value))| (*key, value))
        .collect();
    let properties = collections::new_dictionary(
        &pairs,
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    );
    for key in keys {
        // SAFETY: the dictionary retained each key.
        unsafe { corefoundation::api::CFRelease(key) };
    }
    for (_, value) in entries {
        // SAFETY: the dictionary retained every property value.
        unsafe { corefoundation::api::CFRelease(value) };
    }
    properties
}

/// `CFTypeRef IORegistryEntryCreateCFProperty(entry, key, allocator, options)`.
pub unsafe extern "C" fn IORegistryEntryCreateCFProperty(
    entry: IoObject,
    key: CFTypeRef,
    _allocator: CFTypeRef,
    _options: u32,
) -> CFTypeRef {
    let key = unsafe { corefoundation::string::as_string(key) }
        .map(|s| s.utf8())
        .unwrap_or_default();
    let value = crate::api::device_for_service(entry)
        .map(|device| property(&device, &key))
        .unwrap_or(std::ptr::null_mut());
    crate::trace_hid(format_args!(
        "IORegistryEntryCreateCFProperty entry={entry} key={} -> {}",
        String::from_utf8_lossy(&key),
        !value.is_null()
    ));
    value
}

/// `CFTypeRef IORegistryEntrySearchCFProperty(entry, plane, key, allocator,
/// options)`.
///
/// The search would go on to the entry's parents or children in `plane`; the
/// entries here have no relatives that carry properties, so it ends at the
/// entry itself.
pub unsafe extern "C" fn IORegistryEntrySearchCFProperty(
    entry: IoObject,
    _plane: *const std::ffi::c_char,
    key: CFTypeRef,
    allocator: CFTypeRef,
    options: u32,
) -> CFTypeRef {
    // SAFETY: the caller's own arguments.
    unsafe { IORegistryEntryCreateCFProperty(entry, key, allocator, options) }
}

pub unsafe extern "C" fn IORegistryEntryCreateCFProperties(
    entry: IoObject,
    properties: *mut CFTypeRef,
    _allocator: CFTypeRef,
    _options: u32,
) -> KernReturn {
    let values = crate::api::device_for_service(entry).map(|device| registry_properties(&device));
    crate::trace_hid(format_args!(
        "IORegistryEntryCreateCFProperties entry={entry} -> {}",
        values.is_some()
    ));
    if let Some(values) = values {
        if !properties.is_null() {
            // SAFETY: an out parameter the caller sized.
            unsafe { properties.write(values) };
        }
        SUCCESS
    } else if !properties.is_null() {
        // SAFETY: an out parameter the caller sized.
        unsafe { properties.write(std::ptr::null_mut()) };
        NO_DEVICE
    } else {
        NO_DEVICE
    }
}

/// An `IONotificationPortRef`. Opaque to the guest and never dereferenced by
/// it; it exists so that the pointer is not null.
struct NotificationPort {
    /// The run loop source `IONotificationPortGetRunLoopSource` answers,
    /// made on first request and owned by the port.
    source: CFTypeRef,
}

/// `IONotificationPortRef IONotificationPortCreate(mach_port_t)`.
///
/// A real allocation, because the calls that follow it - `GetRunLoopSource`,
/// `GetMachPort`, `AddMatchingNotification` - are made without checking, and a
/// null here faults inside the first of them rather than saying anything.
pub unsafe extern "C" fn IONotificationPortCreate(_port: u32) -> *mut c_void {
    Box::into_raw(Box::new(NotificationPort {
        source: std::ptr::null(),
    }))
    .cast()
}

/// `void IONotificationPortDestroy(IONotificationPortRef)`.
pub unsafe extern "C" fn IONotificationPortDestroy(port: *mut c_void) {
    if port.is_null() {
        return;
    }
    // SAFETY: the guest hands back what `IONotificationPortCreate` returned,
    // which is this crate's own `Box`.
    let port = unsafe { Box::from_raw(port.cast::<NotificationPort>()) };
    if !port.source.is_null() {
        // SAFETY: the source this port made and owns.
        unsafe {
            corefoundation::api::CFRunLoopSourceInvalidate(port.source);
            corefoundation::api::CFRelease(port.source);
        }
    }
}

/// `CFRunLoopSourceRef IONotificationPortGetRunLoopSource(IONotificationPortRef)`.
///
/// A source with nothing behind it, which is added to a run loop and never
/// signalled - as no IOKit message ever arrives here.
pub unsafe extern "C" fn IONotificationPortGetRunLoopSource(port: *mut c_void) -> CFTypeRef {
    if port.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a port `IONotificationPortCreate` made.
    let port = unsafe { &mut *port.cast::<NotificationPort>() };
    if port.source.is_null() {
        let context = corefoundation::api::CFRunLoopSourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
            equal: std::ptr::null(),
            hash: std::ptr::null(),
            schedule: std::ptr::null(),
            cancel: std::ptr::null(),
            perform: None,
        };
        // SAFETY: a context that outlives the call, which copies it.
        port.source =
            unsafe { corefoundation::api::CFRunLoopSourceCreate(std::ptr::null(), 0, &context) };
    }
    port.source
}

/// `kern_return_t IOMasterPort(mach_port_t bootstrap, mach_port_t *master)`:
/// `MACH_PORT_NULL`, the default the constant `kIOMasterPortDefault` names.
pub unsafe extern "C" fn IOMasterPort(_bootstrap: u32, master: *mut u32) -> KernReturn {
    if !master.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { master.write(0) };
    }
    SUCCESS
}

/// A one-key matching dictionary, as each `IO...Matching` builds.
///
/// # Safety
///
/// `name` is null or a C string.
unsafe fn matching_on(key: &[u8], name: *const c_char) -> CFTypeRef {
    // SAFETY: the caller's obligation.
    let Some(bytes) = (unsafe { corefoundation::string::cstr_bytes(name) }) else {
        return empty_dictionary();
    };
    matching_dictionary(
        corefoundation::string::from_bytes(key, false),
        corefoundation::string::from_bytes(bytes, false),
    )
}

/// `CFMutableDictionaryRef IOServiceNameMatching(const char *name)`.
pub unsafe extern "C" fn IOServiceNameMatching(name: *const c_char) -> CFTypeRef {
    // SAFETY: the caller's C string.
    unsafe { matching_on(b"IONameMatch", name) }
}

/// `CFMutableDictionaryRef IOBSDNameMatching(mach_port_t, uint32_t, const char *)`.
pub unsafe extern "C" fn IOBSDNameMatching(
    _port: u32,
    _options: u32,
    name: *const c_char,
) -> CFTypeRef {
    // SAFETY: the caller's C string.
    unsafe { matching_on(b"BSD Name", name) }
}

/// `kern_return_t IORegistryEntryCreateIterator(entry, plane, options, io_iterator_t *)`:
/// an empty iterator, as the registry here has no children to walk.
pub unsafe extern "C" fn IORegistryEntryCreateIterator(
    _entry: IoObject,
    _plane: *const c_char,
    _options: u32,
    iterator: *mut IoObject,
) -> KernReturn {
    if !iterator.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { iterator.write(0) };
    }
    SUCCESS
}

/// `kern_return_t IORegistryEntryGetChildIterator(entry, plane, io_iterator_t *)`:
/// an empty iterator, for the same reason [`IORegistryEntryCreateIterator`]'s
/// is - there are no parent/child links in this registry, only a flat list of
/// HID devices, and [`IORegistryEntryGetParentEntry`] already answers
/// `NO_DEVICE` for every one of them.
pub unsafe extern "C" fn IORegistryEntryGetChildIterator(
    entry: IoObject,
    _plane: *const c_char,
    iterator: *mut IoObject,
) -> KernReturn {
    // SAFETY: as `IORegistryEntryCreateIterator`.
    unsafe { IORegistryEntryCreateIterator(entry, std::ptr::null(), 0, iterator) }
}

/// `boolean_t IOObjectConformsTo(io_object_t, const io_name_t className)`:
/// reports HID classes for live controller services.
pub unsafe extern "C" fn IOObjectConformsTo(object: IoObject, class: *const c_char) -> u32 {
    let class = unsafe { corefoundation::string::cstr_bytes(class) };
    (class.is_some_and(|c| c == b"IOHIDDevice" || c == b"IOHIDDevice2")
        && crate::api::device_for_service(object).is_some()) as u32
}

/// `kIOReturnUnsupported`.
const UNSUPPORTED: IOReturn = -536_870_201;

/// `IOReturn IOCreatePlugInInterfaceForService(service, pluginType,
/// interfaceType, IOCFPlugInInterface ***, SInt32 *score)`: refused, as no
/// service here has a user-space driver plug-in.
pub unsafe extern "C" fn IOCreatePlugInInterfaceForService(
    _service: IoObject,
    _plugin_type: CFTypeRef,
    _interface_type: CFTypeRef,
    interface: *mut *mut c_void,
    score: *mut i32,
) -> IOReturn {
    // SAFETY: the caller's out-parameters.
    unsafe {
        if !interface.is_null() {
            interface.write(std::ptr::null_mut());
        }
        if !score.is_null() {
            score.write(0);
        }
    }
    UNSUPPORTED
}

/// `kern_return_t IODestroyPlugInInterface(IOCFPlugInInterface **)`.
pub unsafe extern "C" fn IODestroyPlugInInterface(_interface: *mut c_void) -> KernReturn {
    SUCCESS
}

/// The `io_connect_t` `IORegisterForSystemPower` answers: any live-looking
/// port name, since the calls it is passed to accept and ignore it.
const ROOT_POWER_DOMAIN: IoObject = 0x5057_5200;

/// `io_connect_t IORegisterForSystemPower(void *refcon, IONotificationPortRef *,
/// IOServiceInterestCallback, io_object_t *notifier)`
///
/// Registered, and never called back: this host's sleep and wake do not reach
/// the guest, so a program is not told to prepare for either.
pub unsafe extern "C" fn IORegisterForSystemPower(
    _refcon: *mut c_void,
    port: *mut *mut c_void,
    _callback: *const c_void,
    notifier: *mut IoObject,
) -> IoObject {
    // SAFETY: the caller's out-parameters.
    unsafe {
        if !port.is_null() {
            port.write(IONotificationPortCreate(0));
        }
        if !notifier.is_null() {
            notifier.write(ROOT_POWER_DOMAIN);
        }
    }
    ROOT_POWER_DOMAIN
}

/// `IOReturn IOAllowPowerChange(io_connect_t, intptr_t)` and
/// `IOCancelPowerChange`: answers to a notification that never comes.
pub unsafe extern "C" fn IOAllowPowerChange(
    _kernel_port: IoObject,
    _notification: isize,
) -> IOReturn {
    SUCCESS
}

/// `mach_port_t IONotificationPortGetMachPort(IONotificationPortRef)`.
///
/// `MACH_PORT_NULL`. A port from `darwin::mach` would be one nothing ever sends
/// to, and a caller that adds it to a run loop would then wait on a port that
/// cannot receive - which is the same nothing, arrived at more expensively.
pub unsafe extern "C" fn IONotificationPortGetMachPort(_port: *mut c_void) -> u32 {
    0
}

/// `kern_return_t IOServiceAddMatchingNotification(port, type, matching,
/// callback, refcon, io_iterator_t *)`.
///
/// Succeeds with an empty iterator. The caller must drain it - that is what
/// arms the notification on a Mac - and draining nothing is what it does here.
/// The callback is never called, because nothing is ever added to this
/// registry.
pub unsafe extern "C" fn IOServiceAddMatchingNotification(
    _port: *mut c_void,
    _notification_type: *const c_char,
    matching: CFTypeRef,
    _callback: *const c_void,
    _refcon: *mut c_void,
    notification: *mut IoObject,
) -> KernReturn {
    // SAFETY: the reference the caller handed over.
    unsafe { consume(matching) };
    if !notification.is_null() {
        // SAFETY: an out parameter the caller sized.
        unsafe { notification.write(0) };
    }
    SUCCESS
}

/// `void IODispatchCalloutFromMessage(void *unused, mach_msg_header_t *, void *reference)`.
///
/// Nothing here delivers an IOKit message, so there is never one to dispatch.
pub unsafe extern "C" fn IODispatchCalloutFromMessage(
    _unused: *mut c_void,
    _message: *mut c_void,
    _reference: *mut c_void,
) {
}

/// `CFDictionaryRef KextManagerCopyLoadedKextInfo(CFArrayRef ids, CFArrayRef keys)`.
///
/// Empty: there is no kernel extension loaded on a host that has no XNU. A
/// caller looking for a particular bundle id finds it absent, which is the true
/// answer, where a null return would be read as the query itself failing.
pub unsafe extern "C" fn KextManagerCopyLoadedKextInfo(
    _ids: CFTypeRef,
    _keys: CFTypeRef,
) -> CFTypeRef {
    empty_dictionary()
}

/// `CFDictionaryRef IODisplayCreateInfoDictionary(io_service_t, IOOptionBits)`.
///
/// Empty, which is what IOGraphicsLib answers when no display service stands
/// behind the framebuffer - and `CGDisplayIOServicePort` names none here.
/// Never null, as on a Mac.
pub unsafe extern "C" fn IODisplayCreateInfoDictionary(
    _framebuffer: u32,
    _options: u32,
) -> CFTypeRef {
    empty_dictionary()
}

exports::exports! {
    functions {
        "KextManagerCopyLoadedKextInfo" => KextManagerCopyLoadedKextInfo,
        "IODisplayCreateInfoDictionary" => IODisplayCreateInfoDictionary,
        "IOServiceMatching" => IOServiceMatching,
        "IORegistryEntryIDMatching" => IORegistryEntryIDMatching,
        "IOServiceGetMatchingServices" => IOServiceGetMatchingServices,
        "IOServiceGetMatchingService" => IOServiceGetMatchingService,
        "IOIteratorNext" => IOIteratorNext,
        "IOIteratorIsValid" => IOIteratorIsValid,
        "IOIteratorReset" => IOIteratorReset,
        "IOObjectRelease" => IOObjectRelease,
        "IOObjectRetain" => IOObjectRetain,
        "IOObjectIsEqualTo" => IOObjectIsEqualTo,
        "IORegistryEntryFromPath" => IORegistryEntryFromPath,
        "IORegistryEntryGetParentEntry" => IORegistryEntryGetParentEntry,
        "IORegistryEntryGetChildIterator" => IORegistryEntryGetChildIterator,
        "IORegistryEntryGetRegistryEntryID" => IORegistryEntryGetRegistryEntryID,
        "IORegistryEntryCreateCFProperty" => IORegistryEntryCreateCFProperty,
        "IORegistryEntryCreateCFProperties" => IORegistryEntryCreateCFProperties,
        "IORegistryEntrySearchCFProperty" => IORegistryEntrySearchCFProperty,
        "IONotificationPortCreate" => IONotificationPortCreate,
        "IONotificationPortDestroy" => IONotificationPortDestroy,
        "IONotificationPortGetMachPort" => IONotificationPortGetMachPort,
        "IOServiceAddMatchingNotification" => IOServiceAddMatchingNotification,
        "IODispatchCalloutFromMessage" => IODispatchCalloutFromMessage,
        "IONotificationPortGetRunLoopSource" => IONotificationPortGetRunLoopSource,
        // `IOMainPort` is the same call under the name macOS 12 gave it.
        "IOMasterPort" | "IOMainPort" => IOMasterPort,
        "IOServiceNameMatching" => IOServiceNameMatching,
        "IOBSDNameMatching" => IOBSDNameMatching,
        "IORegistryEntryCreateIterator" => IORegistryEntryCreateIterator,
        "IOObjectConformsTo" => IOObjectConformsTo,
        "IOCreatePlugInInterfaceForService" => IOCreatePlugInInterfaceForService,
        "IODestroyPlugInInterface" => IODestroyPlugInInterface,
        "IORegisterForSystemPower" => IORegisterForSystemPower,
        "IOAllowPowerChange" | "IOCancelPowerChange" => IOAllowPowerChange,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A child iterator always comes back empty and already-ended, since
    /// nothing in this registry has children to walk.
    #[test]
    fn a_child_iterator_is_empty() {
        let mut iterator = 0xdead_beef;
        let status = unsafe { IORegistryEntryGetChildIterator(1, std::ptr::null(), &mut iterator) };
        assert_eq!(status, SUCCESS);
        assert_eq!(unsafe { IOIteratorNext(iterator) }, 0);
    }
}
