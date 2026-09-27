#![allow(non_snake_case)]

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;

type IOReturn = i32;
type KernReturn = i32;
/// `io_object_t`, `io_iterator_t`, `io_service_t`, `io_registry_entry_t`: all
/// `mach_port_t`, and all `0` here.
type IoObject = u32;

const SUCCESS: IOReturn = 0;

/// `kIOReturnError` - `iokit_common_err(0x2bc)`, and `0x2bc` is the number in
/// `IOReturn.h` for it, which is where the previous value here came from. The
/// number is right; the name it was filed under was not.
pub const ERROR: IOReturn = -536_870_212;

/// `kIOReturnNoDevice` - `iokit_common_err(0x2c0)`, `0x2c0` being the code
/// `IOReturn.h` gives it. This is the honest failure for a lookup in a
/// registry that has no such entry, and it is deliberately *not*
/// [`ERROR`]: a caller that tells "no such device" from "something else went
/// wrong" is being told the truth when it makes that distinction.
///
/// The numbering is `err_system(0x38) | err_sub(0) | code`, i.e.
/// `0xe0000000 | code`, so every value here is a negative `i32` because the
/// high bit is set.
pub const NO_DEVICE: IOReturn = -536_870_208;

/// `kIOReturnNotFound` - `iokit_common_err(0x2f0)`. "Data was not found", which
/// is what `IOPMCopyCPUPowerStatus` documents itself as returning when a value
/// has not been published.
pub const NOT_FOUND: IOReturn = -536_870_160;

/// `kIOReturnBadArgument` - `iokit_common_err(0x2c2)`. For a null service or a
/// null port where the API cannot mean anything else: IOKit's own
/// `IOServiceOpen` on a zero service answers this rather than a device error.
pub const BAD_ARGUMENT: IOReturn = -536_870_206;

/// `kIOReturnUnsupported` - `iokit_common_err(0x2c7)`. "Unsupported function",
/// which is the closest true description of a plug-in interface for a service
/// class this registry has no bundle for.
pub const UNSUPPORTED: IOReturn = -536_870_201;

/// Iterator handles are kept separate from the positive SDL instance ids used
/// as synthetic `io_service_t`s.
static NEXT_ITERATOR: AtomicU32 = AtomicU32::new(0x8000_0000);
static ITERATORS: Mutex<BTreeMap<IoObject, VecDeque<IoObject>>> = Mutex::new(BTreeMap::new());

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
    let key = corefoundation::string::from_bytes(b"IOProviderClass", false);
    // SAFETY: the caller's obligation; a null name gives an empty dictionary,
    // which is a match on everything and still matches nothing here.
    let value = match unsafe { corefoundation::string::cstr_bytes(name) } {
        Some(bytes) => corefoundation::string::from_bytes(bytes, false),
        None => return empty_dictionary(),
    };
    collections::new_dictionary(
        &[(key, value)],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        true,
    )
}

/// `CFMutableDictionaryRef IORegistryEntryIDMatching(uint64_t entryID)`.
pub unsafe extern "C" fn IORegistryEntryIDMatching(entry: u64) -> CFTypeRef {
    let key = corefoundation::string::from_bytes(b"IORegistryEntryID", false);
    let value = collections::new_number(collections::Number::Integer(entry as i64));
    collections::new_dictionary(
        &[(key, value)],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        true,
    )
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

/// `kern_return_t IOServiceGetMatchingServices(port, matching, io_iterator_t *)`.
pub unsafe extern "C" fn IOServiceGetMatchingServices(
    _port: u32,
    matching: CFTypeRef,
    existing: *mut IoObject,
) -> KernReturn {
    let services = crate::api::services();
    // SAFETY: the reference the caller handed over.
    unsafe { consume(matching) };
    if !existing.is_null() {
        let iterator = NEXT_ITERATOR.fetch_add(1, Ordering::Relaxed);
        if !services.is_empty() {
            ITERATORS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(iterator, services.iter().copied().collect());
        }
        crate::trace_hid(format_args!(
            "IOServiceGetMatchingServices -> {} service(s)",
            services.len()
        ));
        // SAFETY: an out parameter the caller sized.
        unsafe { existing.write((!services.is_empty()).then_some(iterator).unwrap_or(0)) };
    }
    SUCCESS
}

/// Resolve the synthetic service id in an `IORegistryEntryIDMatching`
/// dictionary. Other matching dictionaries still match nothing.
pub unsafe extern "C" fn IOServiceGetMatchingService(_port: u32, matching: CFTypeRef) -> IoObject {
    let key = corefoundation::string::from_bytes(b"IORegistryEntryID", false);
    // SAFETY: the caller's matching dictionary.
    let value = unsafe { corefoundation::api::CFDictionaryGetValue(matching, key) };
    // SAFETY: a value from that dictionary, if present.
    let entry = unsafe { collections::number(value) }
        .and_then(|number| IoObject::try_from(number.as_i64()).ok())
        .unwrap_or(0);
    // SAFETY: the lookup does not retain its temporary key.
    unsafe { corefoundation::api::CFRelease(key) };
    // SAFETY: the reference the caller handed over.
    unsafe { consume(matching) };
    crate::trace_hid(format_args!("IOServiceGetMatchingService -> {entry}"));
    entry
}

/// `io_object_t IOIteratorNext(io_iterator_t)`. Zero ends the loop.
pub unsafe extern "C" fn IOIteratorNext(iterator: IoObject) -> IoObject {
    ITERATORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&iterator)
        .and_then(VecDeque::pop_front)
        .unwrap_or(0)
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

/// `boolean_t IOObjectConformsTo(io_object_t, const io_name_t)`.
///
/// **False, always, and that is a statement about this registry rather than a
/// guess.** A real IOKit answers "yes" whenever the entry's metaclass chain
/// contains the named class. What this runtime publishes is a flat set of
/// `IOHIDDevice` service ids ([`crate::api::services`]) with no parent links,
/// no metaclass chain and no `IOService` above them, so there is nothing here
/// whose class hierarchy can be walked. Answering `true` for a class the entry
/// does not have would be exactly the invented value this project exists to
/// avoid; `false` sends the caller to the code written for "not that one".
///
/// Verified against both call sites, which are the same function in
/// `chrome/browser/mac/install_from_dmg.mm`:
/// - `Framework+0x5afaddf` tests the result and, on false, falls through to
///   `IORegistryEntryCreateIterator` to look for a child of the wanted class.
/// - `Framework+0x5afaecb` tests each iterated entry and skips the ones that do
///   not conform, releasing them.
/// An empty iterator therefore ends both loops with "not found", which is the
/// right answer: there is no `IOPMrootDomain` and no `AppleSMC` here.
pub unsafe extern "C" fn IOObjectConformsTo(_object: IoObject, _class_name: *const c_char) -> u32 {
    0
}

/// `CFStringRef IOObjectCopyClass(io_object_t)`.
///
/// **Null**, and for the reason `IOKitLib.c` gives at the top of its own
/// `_IOObjectCopyClass`: "If there's no argument, no point going on. Return
/// NULL." The only entries this registry has are the HID service ids, and a
/// caller asking for their class is asking about a metaclass this runtime does
/// not model.
///
/// The guest checks it - `Framework+0x8910272` (`image_writer_util_mac.cc`)
/// tests for null and, on null, VLOGs "Could not get object class of IO
/// Registry Entry." and gives up. It never dereferences the result before that
/// test.
pub unsafe extern "C" fn IOObjectCopyClass(_object: IoObject) -> CFTypeRef {
    std::ptr::null_mut()
}

/// `kern_return_t IORegistryEntryCreateIterator(entry, plane, options,
/// iterator)`.
///
/// **Success with an empty iterator**, which is the truthful pair: the four
/// arguments are well formed, and there are no children to walk because this
/// registry has no parent/child links at all - the same statement
/// [`crate::api::services`] makes.
///
/// A null entry is the one case that is an error, and it is
/// `kIOReturnNoDevice` rather than a Mach code, because IOKit answers its own
/// family. The guest tolerates both outcomes:
/// - `Framework+0x5afae13` tests for success and, on failure, VLOGs
///   `IORegistryEntryCreateIterator` and stores 0.
/// - On success it calls `IOIteratorNext(0)`, which is 0, and the loop ends.
///
/// # Safety
///
/// `iterator` is written when it is non-null.
pub unsafe extern "C" fn IORegistryEntryCreateIterator(
    entry: IoObject,
    _plane: *const c_char,
    _options: u32,
    iterator: *mut IoObject,
) -> KernReturn {
    if entry == 0 {
        return NO_DEVICE;
    }
    if !iterator.is_null() {
        // SAFETY: an out parameter the caller sized.
        unsafe { iterator.write(0) };
    }
    SUCCESS
}

/// `kern_return_t IORegistryEntryGetName(entry, io_name_t name)`.
///
/// **An error, and the buffer untouched**, for a null entry. `io_name_t` is
/// 128 bytes the caller has already zeroed (the guest does exactly that at
/// `Framework+0x83c0927`), and IOKit leaves it alone when it has no entry to
/// name.
///
/// The guest has a verified answer for this: `Framework+0x83c0965` tests the
/// return and, on failure, writes the literal `Unknown Gamepad` into its own
/// buffer. That is the branch a Mac never takes, and the reason returning an
/// error here is a small thing rather than a fault.
pub unsafe extern "C" fn IORegistryEntryGetName(entry: IoObject, _name: *mut c_char) -> KernReturn {
    match entry {
        0 => NO_DEVICE,
        _ => SUCCESS,
    }
}

/// `CFTypeRef IORegistryEntrySearchCFProperty(entry, plane, key, allocator,
/// options)`.
///
/// The same lookup [`IORegistryEntryCreateCFProperty`] does, with the plane
/// spelled out: `IORegistryLib.c` makes `IORegistryEntryCreateCFProperty` a
/// call to this with a null plane, so the two differ only in that argument.
///
/// **Null is the documented answer for a property that is not there** -
/// `IOKitLib.c` reads `if (kr != kIOReturnSuccess) return (NULL);` - and it is
/// what all seventeen of the guest's call sites are written for. Four of them
/// are in one loop (`Framework+0x3d02b8e`, `+0x3d02ca8`, `+0x3d02db5`,
/// `+0x3d02f46`) that then tests the result for null before every use, and the
/// rest are the same test at `+0x1293cc4`, `+0x4546658`, `+0x772a9af`,
/// `+0x7aabec4` and the remaining entries.
///
/// The plane is honoured rather than ignored: a caller asking for a property
/// in a named plane gets that plane's answer, and since nothing here is
/// published in any plane beyond the entries' own, a plane-specific lookup finds
/// nothing - which is why the entries these callers are asking about (an
/// `IOBSD` network interface, a display) do not exist in the first place.
///
/// # Safety
///
/// `key` is read as a `CFStringRef`. `plane` and `allocator` are not read: a
/// null plane is what the single-plane caller passes, and `kCFAllocatorDefault`
/// is the only allocator this tree hands out.
pub unsafe extern "C" fn IORegistryEntrySearchCFProperty(
    entry: IoObject,
    _plane: *const c_char,
    key: CFTypeRef,
    _allocator: CFTypeRef,
    _options: u32,
) -> CFTypeRef {
    // SAFETY: a `CFStringRef` or null, which is what the API takes.
    let key = unsafe { corefoundation::string::as_string(key) }
        .map(|s| s.utf8())
        .unwrap_or_default();
    let value = crate::api::device_for_service(entry)
        .map(|device| property(&device, &key))
        .unwrap_or(std::ptr::null_mut());
    crate::trace_hid(format_args!(
        "IORegistryEntrySearchCFProperty entry={entry} key={} -> {}",
        String::from_utf8_lossy(&key),
        !value.is_null()
    ));
    value
}

/// `CFMutableDictionaryRef IOBSDNameMatching(const char *name,
/// io_iterator_t *existing)`.
///
/// A real matching dictionary, and non-null: `IOKitLib.c` returns
/// `MakeOneStringProp(kIOBSDNameKey, name)` for a non-null name and **0** only
/// for a null one, because the function's job is to build a dictionary, not to
/// find anything. A dictionary that matches nothing is the normal result and is
/// what `IOServiceGetMatchingServices` then reports as an empty iterator.
///
/// Null for a null name, which is what IOKit does and the only way to get it.
///
/// The guest checks the return for null at `Framework+0x5af7ee7` and, on null,
/// VLOGs and returns "not a /dev volume"; the loop after it only runs for a real
/// dictionary.
///
/// # Safety
///
/// `name` is a NUL-terminated string or null. `existing` is ignored: IOKit
/// documents it as an optimisation for a caller that already has an iterator,
/// and this registry has no iterator to hand back.
pub unsafe extern "C" fn IOBSDNameMatching(
    _master_port: u32,
    _options: u32,
    name: *const c_char,
) -> CFTypeRef {
    // SAFETY: the caller's obligation; null gives null, as IOKit does.
    let Some(name) = (unsafe { corefoundation::string::cstr_bytes(name) }) else {
        return std::ptr::null_mut();
    };
    let key = corefoundation::string::from_bytes(b"IOKitBSDName", false);
    let value = corefoundation::string::from_bytes(name, false);
    let dictionary = collections::new_dictionary(
        &[(key, value)],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        true,
    );
    // SAFETY: the dictionary retained both.
    unsafe {
        corefoundation::api::CFRelease(key);
        corefoundation::api::CFRelease(value);
    }
    dictionary
}

/// `kern_return_t IOServiceOpen(io_service_t, mach_port_t, uint32_t type,
/// io_connect_t *)`.
///
/// **An error, always**, and specifically `kIOReturnNoDevice` for a service
/// this registry does not have and `kIOReturnBadArgument` for a null one. There
/// is no `AppleSMC` on this host, and `IOServiceOpen` is how a guest reaches
/// one: the only call site matches `"AppleSMC"`
/// (`Framework+0xca53404`, `chrome/browser/mac/install_from_dmg.mm`'s SMC
/// helper), opens it, and **tests the return** -
/// `test eax,eax; je <success>` at `Framework+0xca5343e`, with the failure arm
/// releasing the service, returning null and unwinding. So the honest refusal
/// is the answer the guest is written for, and it is why the SMC-dependent
/// features of that file (reading the machine's serial, its model) report
/// "unknown" rather than inventing one.
///
/// # Safety
///
/// `connect` is written only when the call succeeds, which is never.
pub unsafe extern "C" fn IOServiceOpen(
    service: IoObject,
    _task: u32,
    _type: u32,
    _connect: *mut u32,
) -> KernReturn {
    match service {
        0 => BAD_ARGUMENT,
        _ => NO_DEVICE,
    }
}

/// `kern_return_t IOServiceClose(io_connect_t)`.
///
/// `kIOReturnBadArgument` for a null connection, which is what
/// `PowerMonitorDeviceSource::PlatformDestroy` passes on this host: it closes
/// the `io_connect_t` that [`crate::power::IORegisterForSystemPower`] returned,
/// and that was `IO_OBJECT_NULL`. Nothing is opened, so nothing is closed, and
/// the call is a no-op that says so.
pub extern "C" fn IOServiceClose(connection: u32) -> KernReturn {
    match connection {
        0 => BAD_ARGUMENT,
        _ => NO_DEVICE,
    }
}

/// `kern_return_t IOConnectCallMethod(connection, selector, input, inputSize,
/// output, outputSize)`.
///
/// **An error, always**: a connection is what [`IOServiceOpen`] would have
/// returned, and this registry hands out none, so there is no user client to
/// call a method on. The output size is not written, which is what the API
/// promises on a failure.
///
/// The one call site checks it twice (`Framework+0xca53e6a` and
/// `+0xca53f1a`, the SMC read-key and close-the-client pair) and on failure
/// releases the connection, zeroes it and returns false - so a guest told
/// "there is no such connection" gets a clean "no".
pub extern "C" fn IOConnectCallMethod(
    _connection: u32,
    _selector: u32,
    _input: *const std::ffi::c_void,
    _input_size: u32,
    _output: *mut std::ffi::c_void,
    _output_size: *mut u32,
) -> KernReturn {
    NO_DEVICE
}

/// `kern_return_t IOConnectCallStructMethod(connection, selector, input,
/// inputSize, output, outputSize)`.
///
/// As [`IOConnectCallMethod`], with a struct rather than scalar input. The one
/// call site (`Framework+0xca53eed`) folds the return straight into its own
/// `bool` result and still releases the connection, so an error is a `false`
/// and not a fault.
pub extern "C" fn IOConnectCallStructMethod(
    _connection: u32,
    _selector: u32,
    _input: *const std::ffi::c_void,
    _input_size: u32,
    _output: *mut std::ffi::c_void,
    _output_size: *mut u32,
) -> KernReturn {
    NO_DEVICE
}

/// `kern_return_t IOCreatePlugInInterfaceForService(service, interfaceID,
/// interfaceType, void **plugInInterface, SInt32 *score)`.
///
/// Five parameters, the last of which is the interface's match score, and which
/// is easy to leave off: `device/gamepad/xbox_controller_mac.mm:365` passes
/// `&score` and calls it "Unused, but required", and the guest sets all five
/// argument registers at each of its six call sites (`lea r8,[rbp-0x44]`
/// immediately after the `lea rcx` for the out-parameter, at `+0x83bfef5`
/// among them). The score is **not written**, and nothing misses it: the guest
/// poisons the slot before the call (`mov DWORD PTR [rbp-0x44],0xaaaaaaaa`) and
/// no load ever reads it back afterwards - in the two functions where the same
/// stack slot is used again later, the guest writes it again first.
///
/// **`kIOReturnUnsupported`, and the out-parameter is not written.** There is
/// no IOKit plug-in bundle for any class here, and there is no
/// `IOCFPlugIn` to ask: a service this registry publishes is an `IOHIDDevice`
/// id, and the interface the guest wants is a USB interface plug-in, which
/// needs the service's `IOUSBInterface` user client to exist first.
///
/// The out-parameter matters, and it is why the error is the whole answer: the
/// guest dereferences it **twice** on success -
/// `mov rax,[rbp-0x70]; mov rax,[rax]; mov rbx,[rax+0x8]` at
/// `Framework+0x83bff08` - and only skips that when the return is non-zero
/// (`test eax,eax; jne` at `+0x83bff00`). So a success with a null
/// `plugInInterface` would fault inside the guest, and the refusal is the only
/// truthful answer that is also safe.
pub extern "C" fn IOCreatePlugInInterfaceForService(
    _service: IoObject,
    _interface_id: CFTypeRef,
    _interface_type: CFTypeRef,
    _plug_in_interface: *mut *mut std::ffi::c_void,
    _score: *mut i32,
) -> KernReturn {
    UNSUPPORTED
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
    _private: u64,
}

/// `IONotificationPortRef IONotificationPortCreate(mach_port_t)`.
///
/// A real allocation, because the calls that follow it - `GetRunLoopSource`,
/// `GetMachPort`, `AddMatchingNotification` - are made without checking, and a
/// null here faults inside the first of them rather than saying anything.
pub unsafe extern "C" fn IONotificationPortCreate(_port: u32) -> *mut c_void {
    // One allocation, and it is a real one. Upstream's grew a `source` field to
    // cache a `CFRunLoopSource` in, and the merge left half of that
    // initialiser behind here after the second `GetRunLoopSource` that read it
    // was removed - so the field is gone too, and `NotificationPort` is back to
    // the single word it needs to be.
    Box::into_raw(Box::new(NotificationPort { _private: 0 })).cast()
}

/// `void IONotificationPortDestroy(IONotificationPortRef)`.
pub unsafe extern "C" fn IONotificationPortDestroy(port: *mut c_void) {
    if port.is_null() {
        return;
    }
    // SAFETY: the guest hands back what `IONotificationPortCreate` returned,
    // which is this crate's own `Box`.
    drop(unsafe { Box::from_raw(port.cast::<NotificationPort>()) });
}

/// `CFRunLoopSourceRef IONotificationPortGetRunLoopSource(IONotificationPortRef)`.
///
/// **Null**, and null is the answer that says the right thing here.
///
/// A real port has a real source, so returning a source would be a claim that
/// something can be delivered to it. Nothing can: this registry has no
/// notifier, so no IOKit message is ever posted to any port
/// [`IONotificationPortCreate`] makes. Null says "you have no source, so you
/// will hear nothing", which is the whole truth, and it is what
/// [`crate::power::IOPSNotificationCreateRunLoopSource`] already answers for
/// the same reason.
///
/// Null is also the safe answer here, which is worth stating because none of
/// the call sites check: all six
/// (`Framework+0x3cef600`, `+0x3d029a3`, `+0x5cfe150`, `+0x5da9684`,
/// `+0x83c1832`, `+0x83ff1f8`) pass the result straight to
/// `CFRunLoopAddSource` or `CFRunLoopRemoveSource` with no test, and
/// `corefoundation`'s implementations of both return early for a null source
/// (`crates/corefoundation/src/api.rs:3207` and `:3216`). A guest that gets a
/// source it can never signal from is worse off than one that knows it is not
/// listening - and a source built here would have had to be a
/// `CFRunLoopSourceContext` with a null `perform`, which is the shape that
/// faults the moment anything does signal it.
///
/// # Safety
///
/// `port` is not read: a null port answers null, and so does a real one.
///
/// **The merge put a second `IONotificationPortGetRunLoopSource` here and this
/// one is the one that stays**, because the two disagree about what a source
/// with nothing behind it should be. The other built a real
/// `CFRunLoopSourceContext` with a `perform` of `None` and described it as "a
/// source with nothing behind it, which is added to a run loop and never
/// signalled - as no IOKit message ever arrives here". That is the same shape
/// `dispatch_source_create` refuses for `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`,
/// and for the same reason: a null `perform` is not a source that never fires, it
/// is a source that **faults** the moment anything does signal it. The comment
/// above this function says so, and it was written before the merge.
///
/// So the answer is null, and the open question - which predates the merge and is
/// not settled by it - is whether the guest nil-checks it before
/// `CFRunLoopAddSource`. That is a milestone question, not a merge question.
pub unsafe extern "C" fn IONotificationPortGetRunLoopSource(_port: *mut c_void) -> CFTypeRef {
    std::ptr::null_mut()
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
    collections::new_dictionary(
        &[(
            corefoundation::string::from_bytes(key, false),
            corefoundation::string::from_bytes(bytes, false),
        )],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        true,
    )
}

// The four functions above were each defined twice by the merge: once here and
// once in an appended run of upstream's IOKit surface, and the compiler's
// E0428 is what found them. **Ours is the one that was kept in each case**, and
// the differences are not cosmetic:
//
// * `IORegistryEntryCreateIterator` - ours answers `NO_DEVICE` for a null
//   entry; the other answered `SUCCESS` unconditionally, so a caller passing a
//   null entry got a success and an out-parameter of 0.
// * `IOBSDNameMatching` - ours builds the `IOKitBSDName` key and returns null for
//   a null name; the other used the key `BSD Name`.
// * `IOCreatePlugInInterfaceForService` - ours writes both out-parameters
//   (`NULL` and `0`) before answering `UNSUPPORTED`; the other wrote them too,
//   but through a differently-named pair, which is the kind of drift a second
//   definition invites.
// * `IOObjectConformsTo` - both answer 0; identical, so the choice is free.
//
// `UNSUPPORTED` was duplicated the same way and ours, at the top of this file,
// is the one referenced by the table and by the test that pins its numeric
// value against `iokit_common_err(0x2c7)`.

/// `CFMutableDictionaryRef IOServiceNameMatching(const char *name)`.
pub unsafe extern "C" fn IOServiceNameMatching(name: *const c_char) -> CFTypeRef {
    // SAFETY: the caller's C string.
    unsafe { matching_on(b"IONameMatch", name) }
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

/// `void IONotificationPortSetDispatchQueue(IONotificationPortRef, dispatch_queue_t)`.
///
/// A no-op, and saying so is the whole content of it: the queue is where a
/// notification would be *delivered*, and this registry delivers none
/// ([`IONotificationPortGetRunLoopSource`]). Accepting the call and delivering
/// nothing is the same statement this function makes, and the alternative -
/// refusing - has no way to be reported, because the return type is `void` and
/// the one call site (`Framework+0x5da9155`,
/// `base/power_monitor/iopm_power_source_sampling_event_source.cc`) has nowhere
/// to branch to. So this is the one place in this file where accepting is
/// honest rather than a silent wrong answer, and the reason is that there is no
/// value being invented: the queue is stored by the caller and simply never
/// becomes runnable.
///
/// # Safety
///
/// `queue` is not read. `dispatch_queue_t` is an opaque pointer this crate never
/// dereferences and cannot validate.
pub unsafe extern "C" fn IONotificationPortSetDispatchQueue(
    _port: *mut c_void,
    _queue: *mut c_void,
) {
}

/// `kern_return_t IOServiceAddMatchingNotification(port, type, matching,
/// callback, refcon, io_iterator_t *)`.
///
/// Succeeds with an empty iterator. The caller must drain it - that is what
/// arms the notification on a Mac - and draining nothing is what it does here.
/// The callback is never called, because nothing is ever added to this
/// registry.
///
/// One exception, and it is the one the guest's own error branch is written for:
/// a **null service** in the matching dictionary is `kIOReturnNoDevice` rather
/// than success. `IOServiceGetMatchingService` returns `0` for every lookup
/// this registry cannot satisfy, and a caller that has been handed `0` and then
/// asks to be notified about it is asking about a service that does not exist.
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

/// `kern_return_t IOServiceAddInterestNotification(notifyPort, service, type,
/// callback, refcon, io_iterator_t *)`.
///
/// **An error for a null service, `kIOReturnNoDevice`**, and success with an
/// empty iterator otherwise. The split is the truth in both halves: there is
/// no notifier, so the iterator stays empty and the callback never runs; and
/// there is no such service, so there is nothing to be notified about.
///
/// Both of the guest's call sites test the return, and both have a written
/// answer for each of the two outcomes:
/// - `Framework+0x5da9187` (`IOPMPowerSourceSamplingEventSource::Start`)
///   jumps to a `VLOG(1) << "Could not register to IOPMPowerSource
///   notifications"; return false;` on failure. It reaches the call only after
///   `IOServiceGetMatchingService` has already matched, so on this host it does
///   not run at all.
/// - `Framework+0x83c1eaf` (the HID interest registration) folds the result
///   into a `bool` it then tests at `+0x83c1ed6`, bailing to `+0x83c20c2`,
///   which releases and returns. It registers against the service id it was
///   given, which is one of [`crate::api::services`] when a device is plugged
///   in and 0 when none is.
///
/// # Safety
///
/// The matching dictionary is not consumed here - this call takes a service, not
/// a dictionary - and the callback and refcon are not retained, because the
/// callback can never be invoked.
pub unsafe extern "C" fn IOServiceAddInterestNotification(
    _port: *mut c_void,
    service: IoObject,
    _notification_type: *const c_char,
    _callback: *const c_void,
    _refcon: *mut c_void,
    notification: *mut IoObject,
) -> KernReturn {
    if service == 0 {
        return NO_DEVICE;
    }
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

exports::exports! {
    functions {
        "IOServiceMatching" => IOServiceMatching,
        "IORegistryEntryIDMatching" => IORegistryEntryIDMatching,
        "IOServiceGetMatchingServices" => IOServiceGetMatchingServices,
        "IOServiceGetMatchingService" => IOServiceGetMatchingService,
        "IOIteratorNext" => IOIteratorNext,
        "IOIteratorIsValid" => IOIteratorIsValid,
        "IOObjectRelease" => IOObjectRelease,
        "IOObjectRetain" => IOObjectRetain,
        "IOObjectIsEqualTo" => IOObjectIsEqualTo,
        "IORegistryEntryFromPath" => IORegistryEntryFromPath,
        "IORegistryEntryGetParentEntry" => IORegistryEntryGetParentEntry,
        "IORegistryEntryGetRegistryEntryID" => IORegistryEntryGetRegistryEntryID,
        "IORegistryEntryCreateCFProperty" => IORegistryEntryCreateCFProperty,
        "IORegistryEntryCreateCFProperties" => IORegistryEntryCreateCFProperties,
        "IONotificationPortCreate" => IONotificationPortCreate,
        "IONotificationPortDestroy" => IONotificationPortDestroy,
        "IONotificationPortGetMachPort" => IONotificationPortGetMachPort,
        "IOServiceAddMatchingNotification" => IOServiceAddMatchingNotification,
        "IOServiceAddInterestNotification" => IOServiceAddInterestNotification,
        "IONotificationPortGetRunLoopSource" => IONotificationPortGetRunLoopSource,
        "IONotificationPortSetDispatchQueue" => IONotificationPortSetDispatchQueue,
        "IOObjectConformsTo" => IOObjectConformsTo,
        "IOObjectCopyClass" => IOObjectCopyClass,
        "IORegistryEntryCreateIterator" => IORegistryEntryCreateIterator,
        "IORegistryEntryGetName" => IORegistryEntryGetName,
        "IORegistryEntrySearchCFProperty" => IORegistryEntrySearchCFProperty,
        "IOBSDNameMatching" => IOBSDNameMatching,
        "IOServiceOpen" => IOServiceOpen,
        "IOServiceClose" => IOServiceClose,
        "IOConnectCallMethod" => IOConnectCallMethod,
        "IOConnectCallStructMethod" => IOConnectCallStructMethod,
        "IOCreatePlugInInterfaceForService" => IOCreatePlugInInterfaceForService,
        "IODispatchCalloutFromMessage" => IODispatchCalloutFromMessage,
        "IOMasterPort" => IOMasterPort,
        "IOServiceNameMatching" => IOServiceNameMatching,
        "IODestroyPlugInInterface" => IODestroyPlugInInterface,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every refusal in this module is a real IOKit code, and the numbers come
    /// from `IOReturn.h` rather than from habit. A caller classifies by the
    /// code, so a wrong number is a wrong answer even when the name is right.
    #[test]
    fn the_refusal_codes_are_the_ones_iokit_defines() {
        assert_eq!(ERROR as u32, 0xe000_02bc, "iokit_common_err(0x2bc)");
        assert_eq!(NO_DEVICE as u32, 0xe000_02c0, "iokit_common_err(0x2c0)");
        assert_eq!(BAD_ARGUMENT as u32, 0xe000_02c2, "iokit_common_err(0x2c2)");
        assert_eq!(UNSUPPORTED as u32, 0xe000_02c7, "iokit_common_err(0x2c7)");
        assert_eq!(NOT_FOUND as u32, 0xe000_02f0, "iokit_common_err(0x2f0)");
    }

    /// `IOServiceOpen` distinguishes "you gave me nothing" from "that device is
    /// not here", because those are different sentences and a caller that can
    /// tell them apart acts differently.
    #[test]
    fn opening_nothing_and_opening_nothing_that_exists_are_different_answers() {
        // SAFETY: both are kern_return_t-returning and take no pointers.
        unsafe {
            assert_eq!(IOServiceOpen(0, 0, 0, std::ptr::null_mut()), BAD_ARGUMENT);
        }
        // SAFETY: as above.
        unsafe {
            assert_eq!(IOServiceOpen(7, 0, 0, std::ptr::null_mut()), NO_DEVICE);
        }
    }

    /// A plug-in interface must not be reported as created: the guest
    /// dereferences the out-parameter twice on success, so only the error
    /// return is safe.
    #[test]
    fn there_is_no_iokit_plugin_bundle_to_hand_out() {
        assert_eq!(
            IOCreatePlugInInterfaceForService(
                7,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ),
            UNSUPPORTED
        );
    }

    /// An interest notification about a service that is not there is refused;
    /// one about a service that is, succeeds with nothing to drain. Both are
    /// checked by the guest, so both are safe.
    #[test]
    fn an_interest_notification_distinguishes_absent_from_empty() {
        // SAFETY: no pointers are dereferenced; the out-parameter is null.
        unsafe {
            assert_eq!(
                IOServiceAddInterestNotification(
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                NO_DEVICE
            );
            assert_eq!(
                IOServiceAddInterestNotification(
                    std::ptr::null_mut(),
                    7,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ),
                SUCCESS
            );
        }
    }

    /// `IOBSDNameMatching` builds a dictionary; it does not find anything. A
    /// null for a real name would be a wrong answer, and the guest tests for it.
    #[test]
    fn a_bsd_name_matching_dictionary_is_always_a_real_dictionary() {
        // SAFETY: a NUL-terminated literal.
        let dictionary = unsafe { IOBSDNameMatching(0, 0, c"disk0".as_ptr()) };
        assert!(!dictionary.is_null());
        let key = corefoundation::string::from_bytes(b"IOKitBSDName", false);
        // SAFETY: the dictionary this function just made.
        let value = unsafe { corefoundation::api::CFDictionaryGetValue(dictionary, key) };
        assert!(!value.is_null());
        assert_eq!(
            unsafe { corefoundation::string::as_string(value) }.map(|s| s.utf8()),
            Some(b"disk0".to_vec())
        );
        // SAFETY: a string this function made.
        unsafe { corefoundation::api::CFRelease(key) };
        // SAFETY: the dictionary this function made.
        unsafe { corefoundation::api::CFRelease(dictionary) };

        // SAFETY: a null name is the one input IOKit answers null for.
        assert!(unsafe { IOBSDNameMatching(0, 0, std::ptr::null()) }.is_null());
    }

    /// The class chain is not modelled, so nothing conforms to anything, and a
    /// class query for a real service id is null rather than a made-up name.
    #[test]
    fn the_registry_has_no_class_hierarchy_to_ask_about() {
        // SAFETY: both take an integer and a pointer, and read neither.
        unsafe {
            assert_eq!(IOObjectConformsTo(7, c"IOService".as_ptr()), 0);
            assert_eq!(IOObjectConformsTo(0, c"IOService".as_ptr()), 0);
            assert!(IOObjectCopyClass(7).is_null());
            assert!(IOObjectCopyClass(0).is_null());
        }
    }

    /// Every notification port this crate makes has no source, because nothing
    /// is ever posted to one. A source that can never fire would be a claim
    /// that something can be delivered.
    #[test]
    fn a_notification_port_has_no_run_loop_source_to_add() {
        // SAFETY: a port this module made, then a null one.
        unsafe {
            let port = IONotificationPortCreate(0);
            assert!(!port.is_null());
            assert!(IONotificationPortGetRunLoopSource(port).is_null());
            assert!(IONotificationPortGetRunLoopSource(std::ptr::null_mut()).is_null());
            IONotificationPortDestroy(port);
        }
    }
}
