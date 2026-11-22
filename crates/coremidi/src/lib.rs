//! CoreMIDI: clients, ports and packet lists, with no MIDI devices.
//!
//! No host MIDI system is bridged, so there are no sources or destinations -
//! the state of a Mac with nothing plugged in. Clients and ports are real
//! objects with their names; the packet-list helpers are pure data structure
//! code and match macOS byte for byte, including the per-architecture packet
//! alignment. What each call answers for an endpoint that does not exist was
//! read off macOS, which accepts a send to one and refuses a connection.
#![allow(non_snake_case, clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use corefoundation::base::CFTypeRef;

type OSStatus = i32;
type MIDIObjectRef = u32;

const NO_ERR: OSStatus = 0;
/// `paramErr`: what macOS answers for a reference it does not know.
const PARAM_ERR: OSStatus = -50;
/// `kMIDIUnknownProperty`.
const UNKNOWN_PROPERTY: OSStatus = -10835;
/// `kMIDINotPermitted`.
const NOT_PERMITTED: OSStatus = -10844;

/// `kMIDIPropertyName`'s text, the one property a client or port has.
const PROPERTY_NAME: &str = "name";

/// `MIDIPacket`'s header: a `MIDITimeStamp` and a `UInt16` length, packed to
/// four bytes as `<CoreMIDI/MIDIServices.h>` declares it on every architecture.
const PACKET_HEADER: usize = 10;
const PACKET_LENGTH_OFFSET: usize = 8;
/// `MIDIPacketList.packet`, after the `UInt32` count.
const LIST_HEADER: usize = 4;
/// `MIDIPacketNext` rounds to a word on ARM and not on Intel.
const ARM_PACKET_ALIGNMENT: usize = 4;
/// A status byte that starts a system-exclusive message, which macOS never
/// merges with another event in one packet.
const SYSEX_START: u8 = 0xf0;

enum Object {
    Client {
        name: Option<String>,
    },
    Port {
        client: MIDIObjectRef,
        name: Option<String>,
    },
}

static OBJECTS: Mutex<Option<HashMap<MIDIObjectRef, Object>>> = Mutex::new(None);
static NEXT_REF: AtomicU32 = AtomicU32::new(1);

fn with_objects<R>(f: impl FnOnce(&mut HashMap<MIDIObjectRef, Object>) -> R) -> R {
    let mut objects = OBJECTS.lock().unwrap_or_else(|e| e.into_inner());
    f(objects.get_or_insert_with(HashMap::new))
}

/// # Safety
///
/// `name` is null or a CF object.
unsafe fn name_of(name: CFTypeRef) -> Option<String> {
    // SAFETY: as the caller states.
    let text = unsafe { corefoundation::string::as_string(name) }?;
    String::from_utf8(text.utf8()).ok()
}

/// Register `object`, writing its reference through `out`.
///
/// # Safety
///
/// `out` is null or writable.
unsafe fn publish(object: Object, out: *mut MIDIObjectRef) -> OSStatus {
    if out.is_null() {
        return PARAM_ERR;
    }
    let reference = NEXT_REF.fetch_add(1, Ordering::Relaxed);
    with_objects(|objects| objects.insert(reference, object));
    // SAFETY: the caller states it is writable.
    unsafe { *out = reference };
    NO_ERR
}

fn is_client(reference: MIDIObjectRef) -> bool {
    with_objects(|objects| matches!(objects.get(&reference), Some(Object::Client { .. })))
}

/// `OSStatus MIDIClientCreate(CFStringRef name, MIDINotifyProc notifyProc,
/// void *notifyRefCon, MIDIClientRef *outClient)`
///
/// The notification procedure is never called: with no devices, the MIDI
/// setup never changes.
pub unsafe extern "C" fn MIDIClientCreate(
    name: CFTypeRef,
    _notify: *const c_void,
    _context: *mut c_void,
    out: *mut MIDIObjectRef,
) -> OSStatus {
    // SAFETY: the caller's string and out-parameter.
    unsafe {
        let name = name_of(name);
        publish(Object::Client { name }, out)
    }
}

/// `OSStatus MIDIClientDispose(MIDIClientRef client)`, which disposes of the
/// client's ports with it.
pub extern "C" fn MIDIClientDispose(client: MIDIObjectRef) -> OSStatus {
    with_objects(|objects| {
        if !matches!(objects.get(&client), Some(Object::Client { .. })) {
            return PARAM_ERR;
        }
        objects.retain(|reference, object| {
            *reference != client
                && !matches!(object, Object::Port { client: c, .. } if *c == client)
        });
        NO_ERR
    })
}

/// # Safety
///
/// `name` is null or a CF object; `out` is null or writable.
unsafe fn create_port(client: MIDIObjectRef, name: CFTypeRef, out: *mut MIDIObjectRef) -> OSStatus {
    if !is_client(client) {
        return PARAM_ERR;
    }
    // SAFETY: the caller's string and out-parameter.
    unsafe {
        let name = name_of(name);
        publish(Object::Port { client, name }, out)
    }
}

/// `OSStatus MIDIInputPortCreate(MIDIClientRef client, CFStringRef portName,
/// MIDIReadProc readProc, void *refCon, MIDIPortRef *outPort)`
///
/// The read procedure is never called: no source can be connected.
pub unsafe extern "C" fn MIDIInputPortCreate(
    client: MIDIObjectRef,
    name: CFTypeRef,
    _read: *const c_void,
    _context: *mut c_void,
    out: *mut MIDIObjectRef,
) -> OSStatus {
    // SAFETY: the caller's.
    unsafe { create_port(client, name, out) }
}

/// `OSStatus MIDIOutputPortCreate(MIDIClientRef client, CFStringRef portName,
/// MIDIPortRef *outPort)`
pub unsafe extern "C" fn MIDIOutputPortCreate(
    client: MIDIObjectRef,
    name: CFTypeRef,
    out: *mut MIDIObjectRef,
) -> OSStatus {
    // SAFETY: the caller's.
    unsafe { create_port(client, name, out) }
}

/// `OSStatus MIDIPortDispose(MIDIPortRef port)`
pub extern "C" fn MIDIPortDispose(port: MIDIObjectRef) -> OSStatus {
    with_objects(|objects| match objects.get(&port) {
        Some(Object::Port { .. }) => {
            objects.remove(&port);
            NO_ERR
        }
        _ => PARAM_ERR,
    })
}

/// `OSStatus MIDIPortConnectSource(MIDIPortRef port, MIDIEndpointRef source,
/// void *connRefCon)`: there is no source to connect.
pub extern "C" fn MIDIPortConnectSource(
    _port: MIDIObjectRef,
    _source: MIDIObjectRef,
    _context: *mut c_void,
) -> OSStatus {
    PARAM_ERR
}

/// `OSStatus MIDIPortDisconnectSource(MIDIPortRef port, MIDIEndpointRef source)`
pub extern "C" fn MIDIPortDisconnectSource(
    _port: MIDIObjectRef,
    _source: MIDIObjectRef,
) -> OSStatus {
    PARAM_ERR
}

/// `ItemCount MIDIGetNumberOfSources(void)`
pub extern "C" fn MIDIGetNumberOfSources() -> usize {
    0
}

/// `ItemCount MIDIGetNumberOfDestinations(void)`
pub extern "C" fn MIDIGetNumberOfDestinations() -> usize {
    0
}

/// `MIDIEndpointRef MIDIGetSource(ItemCount sourceIndex)`: null for an index
/// out of range, which every index is.
pub extern "C" fn MIDIGetSource(_index: usize) -> MIDIObjectRef {
    0
}

/// `MIDIEndpointRef MIDIGetDestination(ItemCount destIndex)`
pub extern "C" fn MIDIGetDestination(_index: usize) -> MIDIObjectRef {
    0
}

/// Whether `object` is a client or a port this module made.
fn exists(object: MIDIObjectRef) -> bool {
    with_objects(|objects| objects.contains_key(&object))
}

/// `OSStatus MIDIObjectGetIntegerProperty(MIDIObjectRef obj,
/// CFStringRef propertyID, SInt32 *outValue)`
///
/// A client or port has no integer property; the value is zeroed, as macOS
/// zeroes it.
pub unsafe extern "C" fn MIDIObjectGetIntegerProperty(
    object: MIDIObjectRef,
    _property: CFTypeRef,
    out: *mut i32,
) -> OSStatus {
    if !exists(object) {
        return PARAM_ERR;
    }
    if !out.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { *out = 0 };
    }
    UNKNOWN_PROPERTY
}

/// `OSStatus MIDIObjectGetStringProperty(MIDIObjectRef obj,
/// CFStringRef propertyID, CFStringRef *str)`
///
/// The name a client or port was made with, as a string the caller owns.
pub unsafe extern "C" fn MIDIObjectGetStringProperty(
    object: MIDIObjectRef,
    property: CFTypeRef,
    out: *mut CFTypeRef,
) -> OSStatus {
    if out.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller states it is writable.
    unsafe { *out = std::ptr::null() };
    // SAFETY: null or a CF object, as the caller states.
    let property = unsafe { name_of(property) };
    let name = with_objects(|objects| match objects.get(&object) {
        None => Err(PARAM_ERR),
        Some(Object::Client { name } | Object::Port { name, .. }) => Ok(name.clone()),
    });
    let name = match name {
        Err(status) => return status,
        Ok(name) if property.as_deref() == Some(PROPERTY_NAME) => name,
        Ok(_) => None,
    };
    let Some(name) = name else {
        return UNKNOWN_PROPERTY;
    };
    // SAFETY: as above.
    unsafe { *out = corefoundation::string::from_bytes(name.as_bytes(), false) };
    NO_ERR
}

/// `OSStatus MIDISend(MIDIPortRef port, MIDIEndpointRef dest,
/// const MIDIPacketList *pktlist)`: accepted and dropped, as macOS accepts a
/// send to a destination that does not exist.
pub extern "C" fn MIDISend(
    _port: MIDIObjectRef,
    _destination: MIDIObjectRef,
    _list: *const c_void,
) -> OSStatus {
    NO_ERR
}

/// `MIDISysexSendRequest`.
#[repr(C)]
pub struct SysexSendRequest {
    destination: MIDIObjectRef,
    data: *const u8,
    bytes_to_send: u32,
    complete: u8,
    reserved: [u8; 3],
    completion: Option<unsafe extern "C" fn(*mut SysexSendRequest)>,
    context: *mut c_void,
}

/// `OSStatus MIDISendSysex(MIDISysexSendRequest *request)`
///
/// macOS finishes a request to a destination that does not exist at once and
/// reports it later, on a thread of its own, as complete with nothing left to
/// send.
pub unsafe extern "C" fn MIDISendSysex(request: *mut SysexSendRequest) -> OSStatus {
    if request.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's request, live until its completion runs.
    let request = unsafe { &mut *request };
    let host = match cfg!(target_arch = "aarch64") {
        true => darwin::sysctl::GuestArch::Arm64,
        false => darwin::sysctl::GuestArch::X86_64,
    };
    let completion = request.completion;
    // A completion read out of the request is the guest's own code, which only
    // a guest of this host's architecture can be called at.
    if completion.is_some() && darwin::sysctl::guest_arch() != host {
        eprintln!("rosemary: coremidi: MIDISendSysex completion of an emulated guest refused");
        return NOT_PERMITTED;
    }
    request.bytes_to_send = 0;
    request.complete = 1;
    if let Some(completion) = completion {
        // SAFETY: a global queue, and a function taking the request as its
        // context, which is `dispatch_function_t`'s shape.
        unsafe {
            let queue = darwin::dispatch::queue::get_global_queue(0, 0);
            darwin::dispatch::queue::async_f(
                queue,
                request as *mut SysexSendRequest as u64,
                completion as usize as u64,
            );
        }
    }
    NO_ERR
}

/// `MIDIPacket *MIDIPacketListInit(MIDIPacketList *pktlist)`
pub unsafe extern "C" fn MIDIPacketListInit(list: *mut u8) -> *mut u8 {
    // SAFETY: a packet list, whose count is its first word.
    unsafe {
        (list as *mut u32).write_unaligned(0);
        list.add(LIST_HEADER)
    }
}

/// Where the packet after the one at `packet` starts, as the guest's
/// `MIDIPacketNext` finds it.
///
/// # Safety
///
/// `packet` is a packet in a list.
unsafe fn next_packet(packet: *mut u8) -> *mut u8 {
    // SAFETY: as the caller states.
    let length = unsafe { packet_length(packet) };
    let end = packet as usize + PACKET_HEADER + length;
    let end = match darwin::sysctl::guest_arch() {
        darwin::sysctl::GuestArch::Arm64 => end.next_multiple_of(ARM_PACKET_ALIGNMENT),
        darwin::sysctl::GuestArch::X86_64 => end,
    };
    end as *mut u8
}

/// # Safety
///
/// `packet` is a packet in a list.
unsafe fn packet_length(packet: *const u8) -> usize {
    // SAFETY: as the caller states.
    usize::from(unsafe { (packet.add(PACKET_LENGTH_OFFSET) as *const u16).read_unaligned() })
}

/// `MIDIPacket *MIDIPacketListAdd(MIDIPacketList *pktlist, ByteCount listSize,
/// MIDIPacket *curPacket, MIDITimeStamp time, ByteCount nData, const Byte *data)`
///
/// An event at the current packet's time joins that packet unless either is
/// system-exclusive; otherwise it starts the next one. Null when the list has
/// no room for it.
pub unsafe extern "C" fn MIDIPacketListAdd(
    list: *mut u8,
    size: usize,
    current: *mut u8,
    time: u64,
    count: usize,
    data: *const u8,
) -> *mut u8 {
    let end = list as usize + size;
    // SAFETY: the caller's list, `size` bytes long, with `current` its last
    // packet, and `count` readable bytes at `data`.
    unsafe {
        let packets = (list as *const u32).read_unaligned();
        let bytes = std::slice::from_raw_parts(data, count);
        if packets > 0 && (current as *const u64).read_unaligned() == time {
            let length = packet_length(current);
            let sysex =
                *current.add(PACKET_HEADER) == SYSEX_START || bytes.first() == Some(&SYSEX_START);
            let merged = u16::try_from(length + count).ok();
            if let (false, Some(merged)) = (sysex, merged) {
                let at = current.add(PACKET_HEADER + length);
                if at as usize + count > end {
                    return std::ptr::null_mut();
                }
                std::ptr::copy_nonoverlapping(data, at, count);
                (current.add(PACKET_LENGTH_OFFSET) as *mut u16).write_unaligned(merged);
                return current;
            }
        }
        let packet = match packets {
            0 => current,
            _ => next_packet(current),
        };
        let Ok(length) = u16::try_from(count) else {
            return std::ptr::null_mut();
        };
        if packet as usize + PACKET_HEADER + count > end {
            return std::ptr::null_mut();
        }
        (packet as *mut u64).write_unaligned(time);
        (packet.add(PACKET_LENGTH_OFFSET) as *mut u16).write_unaligned(length);
        std::ptr::copy_nonoverlapping(data, packet.add(PACKET_HEADER), count);
        (list as *mut u32).write_unaligned(packets + 1);
        packet
    }
}

/// The `kMIDIProperty*` globals.
pub fn data_symbols() -> Vec<(String, u64)> {
    corefoundation::bootstrap();
    corefoundation::base::string_globals(&[
        ("_kMIDIPropertyName", PROPERTY_NAME),
        ("_kMIDIPropertyModel", "model"),
        ("_kMIDIPropertyDisplayName", "displayName"),
        ("_kMIDIPropertyUniqueID", "uniqueID"),
    ])
}

exports::exports! {
    functions {
        "MIDIClientCreate" => MIDIClientCreate,
        "MIDIClientDispose" => MIDIClientDispose,
        "MIDIInputPortCreate" => MIDIInputPortCreate,
        "MIDIOutputPortCreate" => MIDIOutputPortCreate,
        "MIDIPortDispose" => MIDIPortDispose,
        "MIDIPortConnectSource" => MIDIPortConnectSource,
        "MIDIPortDisconnectSource" => MIDIPortDisconnectSource,
        "MIDIGetNumberOfSources" => MIDIGetNumberOfSources,
        "MIDIGetNumberOfDestinations" => MIDIGetNumberOfDestinations,
        "MIDIGetSource" => MIDIGetSource,
        "MIDIGetDestination" => MIDIGetDestination,
        "MIDIObjectGetIntegerProperty" => MIDIObjectGetIntegerProperty,
        "MIDIObjectGetStringProperty" => MIDIObjectGetStringProperty,
        "MIDISend" => MIDISend,
        "MIDISendSysex" => MIDISendSysex,
        "MIDIPacketListInit" => MIDIPacketListInit,
        "MIDIPacketListAdd" => MIDIPacketListAdd,
    }
}

#[cfg(test)]
mod tests;
