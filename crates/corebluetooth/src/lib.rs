//! A CoreBluetooth surface for hosts without a supported Bluetooth transport.
//!
//! API references: Apple's CoreBluetooth documentation for CBManager, CBUUID,
//! CBCentralManager, CBPeripheralManager, CBMutableService and
//! CBMutableCharacteristic: <https://developer.apple.com/documentation/corebluetooth>.
//! Delegate queues: <https://developer.apple.com/documentation/corebluetooth/cbcentralmanager/init(delegate:queue:options:)>.
//! Authorization: <https://developer.apple.com/documentation/corebluetooth/cbmanager/authorization-swift.type.property>.
//! UUID forms: <https://developer.apple.com/documentation/corebluetooth/cbuuid/init(string:)>.
//! Local attributes: <https://developer.apple.com/documentation/corebluetooth/cbmutablecharacteristic/init(type:properties:value:permissions:)>.
//! All entry points here are Objective-C methods; there are no C function imports.
use std::ffi::{CStr, c_void};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicI64, Ordering};

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

const UNSUPPORTED: i64 = 2;
const CLASSES: &[&CStr] = &[
    c"CBManager",
    c"CBCentralManager",
    c"CBPeripheralManager",
    c"CBUUID",
    c"CBAttribute",
    c"CBService",
    c"CBMutableService",
    c"CBCharacteristic",
    c"CBMutableCharacteristic",
];
const STRINGS: &[(&str, &str)] = &[
    ("_CBAdvertisementDataLocalNameKey", "kCBAdvDataLocalName"),
    (
        "_CBAdvertisementDataServiceUUIDsKey",
        "kCBAdvDataServiceUUIDs",
    ),
    (
        "_CBCentralManagerOptionShowPowerAlertKey",
        "kCBInitOptionShowPowerAlert",
    ),
    (
        "_CBPeripheralManagerOptionShowPowerAlertKey",
        "kCBInitOptionShowPowerAlert",
    ),
];

macro_rules! methods {
    ($class:expr, { $($name:literal => $function:ident, $encoding:literal;)* }) => {
        $( $class.replace_method(objc::sel::get($name), $function as *const c_void, $encoding); )*
    };
}

fn class(name: &CStr) -> &'static Class {
    objc::class::lookup(name).expect("CoreBluetooth is bootstrapped")
}

#[repr(C)]
struct Manager {
    isa: *const Class,
    delegate: Id,
    state: AtomicI64,
    queue: u64,
}

unsafe extern "C" fn state(object: Id, _: Sel) -> i64 {
    unsafe { (*object.cast::<Manager>()).state.load(Ordering::Acquire) }
}

unsafe extern "C" fn authorization(_: Id, _: Sel) -> i64 {
    // FIXME (ACCURACY): No host privacy service; no permission prompt is issued.
    // Apple documents notDetermined as the initial authorization status.
    0
}

unsafe extern "C" fn legacy_authorization(_: Id, _: Sel) -> i64 {
    // FIXME (ACCURACY): No background-sharing privacy service or prompt.
    0
}

unsafe extern "C" fn delegate(object: Id, _: Sel) -> Id {
    let slot = unsafe { &raw mut (*object.cast::<Manager>()).delegate };
    objc::arc::autorelease(objc::arc::load_weak_retained(slot))
}

unsafe extern "C" fn set_delegate(object: Id, _: Sel, value: Id) {
    let slot = unsafe { &raw mut (*object.cast::<Manager>()).delegate };
    objc::arc::store_weak(slot, value);
}

unsafe extern "C" fn manager_dealloc(object: Id, _: Sel) {
    unsafe {
        let manager = object.cast::<Manager>();
        objc::arc::destroy_weak(&raw mut (*manager).delegate);
        darwin::dispatch::object::release((*manager).queue);
    }
    objc::arc::finalize(object);
}

unsafe extern "C" fn update_state(context: *mut c_void) {
    let object = context as Id;
    let manager = object.cast::<Manager>();
    unsafe { (*manager).state.store(UNSUPPORTED, Ordering::Release) };
    let target = objc::arc::load_weak_retained(unsafe { &raw mut (*manager).delegate });
    let peripheral =
        unsafe { corefoundation::base::is_kind_of(object, class(c"CBPeripheralManager")) };
    let selector = objc::sel::get(if peripheral {
        "peripheralManagerDidUpdateState:"
    } else {
        "centralManagerDidUpdateState:"
    });
    if let Some(method) = unsafe { objc::object::class_of(target) }.and_then(|c| c.method(selector))
    {
        let call: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(method.implementation()) };
        unsafe { call(target, selector, object) };
    }
    objc::arc::release(target);
    objc::arc::release(object);
}

unsafe extern "C" fn manager_init_options(object: Id, _: Sel, target: Id, queue: u64, _: Id) -> Id {
    if object.is_null() {
        return object;
    }
    // FIXME (ACCURACY): No Bluetooth transport; only the unsupported transition exists.
    let queue = if queue == 0 {
        darwin::dispatch::queue::main_queue()
    } else {
        queue
    };
    unsafe {
        let manager = object.cast::<Manager>();
        (*manager).queue = queue;
        darwin::dispatch::object::retain(queue);
        objc::arc::init_weak(&raw mut (*manager).delegate, target);
        objc::arc::retain(object);
        darwin::dispatch::queue::async_f(
            queue,
            object as u64,
            update_state as *const c_void as u64,
        );
    }
    object
}

unsafe extern "C" fn manager_init_queue(object: Id, sel: Sel, target: Id, queue: u64) -> Id {
    unsafe { manager_init_options(object, sel, target, queue, null_mut()) }
}
unsafe extern "C" fn manager_init(object: Id, sel: Sel) -> Id {
    unsafe { manager_init_queue(object, sel, null_mut(), 0) }
}
// FIXME (ACCURACY): No transport, so radio operations never start or succeed.
unsafe extern "C" fn inactive(_: Id, _: Sel) -> bool {
    false
}
unsafe extern "C" fn noop(_: Id, _: Sel) {}
unsafe extern "C" fn noop_one(_: Id, _: Sel, _: Id) {}
unsafe extern "C" fn noop_two(_: Id, _: Sel, _: Id, _: Id) {}
unsafe extern "C" fn update_value(_: Id, _: Sel, _: Id, _: Id, _: Id) -> bool {
    false
}
unsafe extern "C" fn empty_array(_: Id, _: Sel) -> Id {
    objc::arc::autorelease(corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    ) as Id)
}
// FIXME (ACCURACY): There are no discovered or connected peripherals without a transport.
unsafe extern "C" fn retrieve(_: Id, _: Sel, _: Id) -> Id {
    unsafe { empty_array(null_mut(), std::ptr::null()) }
}

#[repr(C)]
struct Uuid {
    isa: *const Class,
    bytes: [u8; 16],
    len: u8,
}

fn parse_uuid(text: &str) -> Option<Vec<u8>> {
    let valid = match text.len() {
        4 | 8 => text.bytes().all(|b| b.is_ascii_hexdigit()),
        36 => text.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        }),
        _ => false,
    };
    if !valid {
        // FIXME (ACCURACY): Invalid input returns nil rather than raising NSInvalidArgumentException.
        return None;
    }
    let hex: Vec<u8> = text.bytes().filter(|b| *b != b'-').collect();
    Some(
        hex.as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let digit = |b: u8| (b as char).to_digit(16).unwrap() as u8;
                digit(pair[0]) * 16 + digit(pair[1])
            })
            .collect(),
    )
}

fn make_uuid(bytes: &[u8]) -> Id {
    if !matches!(bytes.len(), 2 | 4 | 16) {
        return null_mut();
    }
    let object: *mut Uuid = unsafe { corefoundation::base::allocate(class(c"CBUUID")) };
    if object.is_null() {
        return null_mut();
    }
    unsafe {
        (&mut (*object).bytes)[..bytes.len()].copy_from_slice(bytes);
        (*object).len = bytes.len() as u8;
    }
    objc::arc::autorelease(object.cast())
}

unsafe fn uuid_bytes<'a>(object: Id) -> &'a [u8] {
    unsafe {
        let uuid = &*object.cast::<Uuid>();
        &uuid.bytes[..uuid.len as usize]
    }
}

fn expanded(bytes: &[u8]) -> [u8; 16] {
    let mut value = [
        0, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0x80, 0x5f, 0x9b, 0x34, 0xfb,
    ];
    match bytes.len() {
        2 => value[2..4].copy_from_slice(bytes),
        4 => value[..4].copy_from_slice(bytes),
        16 => value.copy_from_slice(bytes),
        _ => {}
    }
    value
}

unsafe extern "C" fn uuid_string_factory(_: Id, _: Sel, text: Id) -> Id {
    let Some(string) = (unsafe { corefoundation::string::as_string(text) }) else {
        return null_mut();
    };
    String::from_utf8(string.utf8())
        .ok()
        .and_then(|s| parse_uuid(&s))
        .map_or(null_mut(), |bytes| make_uuid(&bytes))
}
unsafe extern "C" fn uuid_data_factory(_: Id, _: Sel, data: Id) -> Id {
    unsafe { corefoundation::collections::data(data) }.map_or(null_mut(), |bytes| make_uuid(bytes))
}
unsafe extern "C" fn uuid_cf_factory(_: Id, _: Sel, uuid: Id) -> Id {
    unsafe { corefoundation::uuid::bytes_of(uuid) }.map_or(null_mut(), |bytes| make_uuid(&bytes))
}
unsafe extern "C" fn uuid_ns_factory(receiver: Id, sel: Sel, uuid: Id) -> Id {
    let selector = objc::sel::get("UUIDString");
    let Some(method) = (unsafe { objc::object::class_of(uuid) }).and_then(|c| c.method(selector))
    else {
        return null_mut();
    };
    let call: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(method.implementation()) };
    let text = unsafe { call(uuid, selector) };
    unsafe { uuid_string_factory(receiver, sel, text) }
}
unsafe extern "C" fn uuid_string(object: Id, _: Sel) -> Id {
    let bytes = unsafe { uuid_bytes(object) };
    let text = if bytes.len() == 16 {
        corefoundation::uuid::format(&expanded(bytes))
    } else {
        bytes.iter().map(|b| format!("{b:02X}")).collect()
    };
    objc::arc::autorelease(corefoundation::string::from_bytes(text.as_bytes(), false) as Id)
}
unsafe extern "C" fn uuid_data(object: Id, _: Sel) -> Id {
    objc::arc::autorelease(corefoundation::collections::new_data(
        unsafe { uuid_bytes(object) }.to_vec(),
    ) as Id)
}
unsafe extern "C" fn uuid_equal(object: Id, _: Sel, other: Id) -> bool {
    (unsafe { corefoundation::base::is_kind_of(other, class(c"CBUUID")) })
        && expanded(unsafe { uuid_bytes(object) }) == expanded(unsafe { uuid_bytes(other) })
}
unsafe extern "C" fn uuid_hash(object: Id, _: Sel) -> usize {
    expanded(unsafe { uuid_bytes(object) })
        .iter()
        .fold(0usize, |hash, b| {
            hash.wrapping_mul(31).wrapping_add(*b as usize)
        })
}
unsafe extern "C" fn uuid_copy(object: Id, _: Sel, _: Id) -> Id {
    objc::arc::retain(object)
}

#[repr(C)]
struct Service {
    isa: *const Class,
    uuid: Id,
    primary: bool,
    characteristics: Id,
    included: Id,
}
#[repr(C)]
struct Characteristic {
    isa: *const Class,
    uuid: Id,
    properties: u64,
    permissions: u64,
    value: Id,
    descriptors: Id,
    service: Id,
}

unsafe extern "C" fn service_init(object: Id, _: Sel, uuid: Id, primary: bool) -> Id {
    if !object.is_null() {
        unsafe {
            (*object.cast::<Service>()).uuid = objc::arc::retain(uuid);
            (*object.cast::<Service>()).primary = primary;
        }
    }
    object
}
unsafe extern "C" fn characteristic_init(
    object: Id,
    _: Sel,
    uuid: Id,
    properties: u64,
    value: Id,
    permissions: u64,
) -> Id {
    if !object.is_null() {
        unsafe {
            let body = &mut *object.cast::<Characteristic>();
            body.uuid = objc::arc::retain(uuid);
            body.properties = properties;
            body.permissions = permissions;
            body.value = objc::arc::retain(value);
        }
    }
    object
}
unsafe extern "C" fn attribute_uuid(object: Id, _: Sel) -> Id {
    // Both attribute layouts start with isa and UUID.
    unsafe { (*object.cast::<Service>()).uuid }
}
unsafe extern "C" fn primary(object: Id, _: Sel) -> bool {
    unsafe { (*object.cast::<Service>()).primary }
}
unsafe extern "C" fn nil(_: Id, _: Sel) -> Id {
    null_mut()
}

macro_rules! property {
    ($get:ident, $set:ident, $body:ty, $field:ident) => {
        unsafe extern "C" fn $get(object: Id, _: Sel) -> Id {
            unsafe { (*object.cast::<$body>()).$field }
        }
        unsafe extern "C" fn $set(object: Id, _: Sel, value: Id) {
            let value = objc::arc::retain(value);
            let old = unsafe { std::mem::replace(&mut (*object.cast::<$body>()).$field, value) };
            objc::arc::release(old);
        }
    };
}
unsafe extern "C" fn characteristics(object: Id, _: Sel) -> Id {
    unsafe { (*object.cast::<Service>()).characteristics }
}
unsafe extern "C" fn set_characteristics(object: Id, _: Sel, value: Id) {
    let value = objc::arc::retain(value);
    let old = unsafe { std::mem::replace(&mut (*object.cast::<Service>()).characteristics, value) };
    for (array, adding) in [(old, false), (value, true)] {
        let items = unsafe { corefoundation::collections::array(array) }
            .map(|array| array.values.clone())
            .unwrap_or_default();
        for item in items {
            if unsafe { corefoundation::base::is_kind_of(item, class(c"CBCharacteristic")) } {
                let slot = unsafe { &raw mut (*(item as *mut Characteristic)).service };
                if adding || unsafe { *slot == object } {
                    objc::arc::store_weak(slot, if adding { object } else { null_mut() });
                }
            }
        }
    }
    objc::arc::release(old);
}
unsafe extern "C" fn characteristic_service(object: Id, _: Sel) -> Id {
    let slot = unsafe { &raw mut (*object.cast::<Characteristic>()).service };
    objc::arc::autorelease(objc::arc::load_weak_retained(slot))
}
property!(included, set_included, Service, included);
property!(value, set_value, Characteristic, value);
property!(descriptors, set_descriptors, Characteristic, descriptors);
unsafe extern "C" fn properties(object: Id, _: Sel) -> u64 {
    unsafe { (*object.cast::<Characteristic>()).properties }
}
unsafe extern "C" fn set_properties(object: Id, _: Sel, value: u64) {
    unsafe { (*object.cast::<Characteristic>()).properties = value }
}
unsafe extern "C" fn permissions(object: Id, _: Sel) -> u64 {
    unsafe { (*object.cast::<Characteristic>()).permissions }
}
unsafe extern "C" fn set_permissions(object: Id, _: Sel, value: u64) {
    unsafe { (*object.cast::<Characteristic>()).permissions = value }
}
unsafe extern "C" fn service_dealloc(object: Id, _: Sel) {
    unsafe {
        let body = &*object.cast::<Service>();
        for item in [body.uuid, body.characteristics, body.included] {
            objc::arc::release(item);
        }
    }
    objc::arc::finalize(object);
}
unsafe extern "C" fn characteristic_dealloc(object: Id, _: Sel) {
    unsafe {
        let body = &mut *object.cast::<Characteristic>();
        objc::arc::destroy_weak(&raw mut body.service);
        for item in [body.uuid, body.value, body.descriptors] {
            objc::arc::release(item);
        }
    }
    objc::arc::finalize(object);
}

/// Register classes and their Objective-C method encodings once.
pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        let manager = corefoundation::base::register(c"CBManager", None, size_of::<Manager>(), false);
        methods!(manager, {
            "state" => state, c"q16@0:8";
            "authorization" => authorization, c"q16@0:8";
            "dealloc" => manager_dealloc, c"v16@0:8";
        });
        methods!(manager.metaclass(), { "authorization" => authorization, c"q16@0:8"; });
        for name in [c"CBCentralManager", c"CBPeripheralManager"] {
            let subclass = corefoundation::base::register(name, Some(manager), size_of::<Manager>(), false);
            methods!(subclass, {
                "init" => manager_init, c"@16@0:8";
                "initWithDelegate:queue:" => manager_init_queue, c"@32@0:8@16@24";
                "initWithDelegate:queue:options:" => manager_init_options, c"@40@0:8@16@24@32";
                "delegate" => delegate, c"@16@0:8";
                "setDelegate:" => set_delegate, c"v24@0:8@16";
            });
        }
        methods!(class(c"CBCentralManager"), {
            "isScanning" => inactive, c"c16@0:8";
            "scanForPeripheralsWithServices:options:" => noop_two, c"v32@0:8@16@24";
            "stopScan" => noop, c"v16@0:8";
            "retrievePeripheralsWithIdentifiers:" => retrieve, c"@24@0:8@16";
            "retrieveConnectedPeripheralsWithServices:" => retrieve, c"@24@0:8@16";
            "connectPeripheral:options:" => noop_two, c"v32@0:8@16@24";
            "cancelPeripheralConnection:" => noop_one, c"v24@0:8@16";
        });
        methods!(class(c"CBPeripheralManager"), {
            "isAdvertising" => inactive, c"c16@0:8";
            "startAdvertising:" => noop_one, c"v24@0:8@16";
            "stopAdvertising" => noop, c"v16@0:8";
            "addService:" => noop_one, c"v24@0:8@16";
            "removeService:" => noop_one, c"v24@0:8@16";
            "removeAllServices" => noop, c"v16@0:8";
            "updateValue:forCharacteristic:onSubscribedCentrals:" => update_value, c"c40@0:8@16@24@32";
        });
        methods!(class(c"CBPeripheralManager").metaclass(), {
            "authorizationStatus" => legacy_authorization, c"q16@0:8";
        });
        let uuid = corefoundation::base::register(c"CBUUID", None, size_of::<Uuid>(), false);
        methods!(uuid.metaclass(), {
            "UUIDWithString:" => uuid_string_factory, c"@24@0:8@16";
            "UUIDWithData:" => uuid_data_factory, c"@24@0:8@16";
            "UUIDWithCFUUID:" => uuid_cf_factory, c"@24@0:8^{__CFUUID=}16";
            "UUIDWithNSUUID:" => uuid_ns_factory, c"@24@0:8@16";
        });
        methods!(uuid, {
            "UUIDString" => uuid_string, c"@16@0:8";
            "data" => uuid_data, c"@16@0:8";
            "isEqual:" => uuid_equal, c"c24@0:8@16";
            "hash" => uuid_hash, c"Q16@0:8";
            "copyWithZone:" => uuid_copy, c"@24@0:8^{_NSZone=}16";
        });
        let attribute = corefoundation::base::register(c"CBAttribute", None, 2 * size_of::<usize>(), false);
        methods!(attribute, { "UUID" => attribute_uuid, c"@16@0:8"; });
        let service = corefoundation::base::register(c"CBService", Some(attribute), size_of::<Service>(), false);
        methods!(service, {
            "isPrimary" => primary, c"c16@0:8";
            "characteristics" => characteristics, c"@16@0:8";
            "includedServices" => included, c"@16@0:8";
            "peripheral" => nil, c"@16@0:8";
            "dealloc" => service_dealloc, c"v16@0:8";
        });
        let mutable = corefoundation::base::register(c"CBMutableService", Some(service), size_of::<Service>(), false);
        methods!(mutable, {
            "initWithType:primary:" => service_init, c"@28@0:8@16c24";
            "setCharacteristics:" => set_characteristics, c"v24@0:8@16";
            "setIncludedServices:" => set_included, c"v24@0:8@16";
        });
        let characteristic = corefoundation::base::register(c"CBCharacteristic", Some(attribute), size_of::<Characteristic>(), false);
        methods!(characteristic, {
            "properties" => properties, c"Q16@0:8";
            "value" => value, c"@16@0:8";
            "descriptors" => descriptors, c"@16@0:8";
            "isNotifying" => inactive, c"c16@0:8";
            "service" => characteristic_service, c"@16@0:8";
            "dealloc" => characteristic_dealloc, c"v16@0:8";
        });
        let mutable = corefoundation::base::register(c"CBMutableCharacteristic", Some(characteristic), size_of::<Characteristic>(), false);
        methods!(mutable, {
            "initWithType:properties:value:permissions:" => characteristic_init, c"@48@0:8@16Q24@32Q40";
            "permissions" => permissions, c"Q16@0:8";
            "setPermissions:" => set_permissions, c"v24@0:8Q16";
            "setProperties:" => set_properties, c"v24@0:8Q16";
            "setValue:" => set_value, c"v24@0:8@16";
            "setDescriptors:" => set_descriptors, c"v24@0:8@16";
            "subscribedCentrals" => empty_array, c"@16@0:8";
        });
    });
}

/// Class objects and NSString pointer cells exported by CoreBluetooth.framework.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut symbols = corefoundation::base::string_globals(STRINGS);
    for name in CLASSES {
        let class = class(name);
        for (prefix, value) in [
            ("_OBJC_CLASS_$_", class),
            ("_OBJC_METACLASS_$_", class.metaclass()),
        ] {
            symbols.push((
                format!("{prefix}{}", name.to_string_lossy()),
                objc::class::ptr(value) as u64,
            ));
        }
    }
    symbols
}

pub fn symbol(name: &str) -> Option<u64> {
    let normalized = if name.starts_with('_') {
        name.to_string()
    } else {
        format!("_{name}")
    };
    data_symbols()
        .into_iter()
        .find(|(symbol, _)| symbol == &normalized)
        .map(|(_, value)| value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn sel() -> Sel {
        std::ptr::null()
    }
    fn allocate(name: &CStr) -> Id {
        objc::object::create_instance(class(name), 0)
    }
    unsafe fn text(object: Id) -> String {
        String::from_utf8(
            unsafe { corefoundation::string::as_string(object) }
                .unwrap()
                .utf8(),
        )
        .unwrap()
    }
    unsafe fn from_string(text: &str) -> Id {
        let string = corefoundation::string::from_bytes(text.as_bytes(), false) as Id;
        let selector = objc::sel::get("UUIDWithString:");
        let uuid_class = class(c"CBUUID");
        let method = uuid_class.metaclass().method(selector).unwrap();
        let call: unsafe extern "C" fn(Id, Sel, Id) -> Id =
            unsafe { std::mem::transmute(method.implementation()) };
        let result = unsafe { call(objc::class::ptr(uuid_class) as Id, selector, string) };
        objc::arc::release(string);
        result
    }

    #[test]
    fn uuids_round_trip_and_compare_expanded_short_forms() {
        bootstrap();
        let pool = objc::arc::pool_push();
        unsafe {
            let short = from_string("180d");
            let wide = from_string("0000180D");
            let full = from_string("0000180d-0000-1000-8000-00805f9b34fb");
            assert_eq!(text(uuid_string(short, sel())), "180D");
            assert_eq!(
                text(uuid_string(full, sel())),
                "0000180D-0000-1000-8000-00805F9B34FB"
            );
            assert!(uuid_equal(short, sel(), full));
            assert!(uuid_equal(short, sel(), wide));
            assert_eq!(uuid_hash(short, sel()), uuid_hash(full, sel()));
            let cf = corefoundation::uuid::new(expanded(uuid_bytes(short)));
            assert!(uuid_equal(
                short,
                sel(),
                uuid_cf_factory(null_mut(), sel(), cf as Id)
            ));
            objc::arc::release(cf as Id);
            for uuid in [short, wide, full] {
                let data = uuid_data(uuid, sel());
                assert_eq!(
                    corefoundation::collections::data(data).unwrap().len(),
                    uuid_bytes(uuid).len()
                );
                assert!(uuid_equal(
                    uuid,
                    sel(),
                    uuid_data_factory(null_mut(), sel(), data)
                ));
            }
            for len in [0, 1, 3, 8, 15, 17] {
                let data = corefoundation::collections::new_data(vec![0; len]) as Id;
                assert!(uuid_data_factory(null_mut(), sel(), data).is_null());
                objc::arc::release(data);
            }
            let data = uuid_data(short, sel());
            assert_eq!(
                corefoundation::collections::data(data).unwrap(),
                &[0x18, 0x0d]
            );
            assert!(uuid_equal(
                short,
                sel(),
                uuid_data_factory(null_mut(), sel(), data)
            ));
            let custom = from_string("71DA3FD1-7E10-41C1-B16F-4430B506CDE7");
            assert!(!uuid_equal(short, sel(), custom));
            assert!(!uuid_equal(short, sel(), null_mut()));
            assert!(!uuid_equal(
                short,
                sel(),
                corefoundation::string::immortal("180D") as Id
            ));
            assert_eq!(uuid_bytes(custom).len(), 16);
            assert!(uuid_equal(
                custom,
                sel(),
                uuid_data_factory(null_mut(), sel(), uuid_data(custom, sel()))
            ));
            let copy = uuid_copy(short, sel(), null_mut());
            assert_eq!(short, copy);
            objc::arc::release(copy);
            for invalid in [
                "",
                "180",
                "180G",
                "0000180D00001000800000805F9B34FB",
                "0000180D_0000-1000-8000-00805F9B34FB",
            ] {
                assert!(from_string(invalid).is_null(), "{invalid}");
            }
        }
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn mutable_services_and_characteristics_keep_their_properties_alive() {
        bootstrap();
        let pool = objc::arc::pool_push();
        unsafe {
            let uuid = from_string("180D");
            let service = service_init(allocate(c"CBMutableService"), sel(), uuid, true);
            let data = corefoundation::collections::new_data(vec![1, 2]) as Id;
            let characteristic = characteristic_init(
                allocate(c"CBMutableCharacteristic"),
                sel(),
                uuid,
                0x12,
                data,
                1,
            );
            objc::arc::release(data);
            assert_eq!(attribute_uuid(service, sel()), uuid);
            assert!(primary(service, sel()));
            assert_eq!(attribute_uuid(characteristic, sel()), uuid);
            assert_eq!(properties(characteristic, sel()), 0x12);
            assert_eq!(permissions(characteristic, sel()), 1);
            assert_eq!(
                corefoundation::collections::data(value(characteristic, sel())).unwrap(),
                &[1, 2]
            );
            let array = corefoundation::collections::new_array(
                &[characteristic],
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            ) as Id;
            set_characteristics(service, sel(), array);
            objc::arc::release(array);
            assert_eq!(
                corefoundation::collections::array(characteristics(service, sel()))
                    .unwrap()
                    .values,
                vec![characteristic as *const c_void]
            );
            assert_eq!(characteristic_service(characteristic, sel()), service);
            set_characteristics(service, sel(), null_mut());
            assert!(characteristic_service(characteristic, sel()).is_null());
            set_properties(characteristic, sel(), 2);
            set_permissions(characteristic, sel(), 3);
            assert_eq!(properties(characteristic, sel()), 2);
            assert_eq!(permissions(characteristic, sel()), 3);
            set_value(characteristic, sel(), null_mut());
            assert!(value(characteristic, sel()).is_null());
            objc::arc::release(characteristic);
            objc::arc::release(service);
        }
        objc::arc::pool_pop(pool);
    }

    #[derive(Debug)]
    struct Event {
        manager: usize,
        selector: String,
        state: i64,
        thread: std::thread::ThreadId,
        queue: String,
    }
    static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());
    unsafe extern "C" fn did_update(_: Id, selector: Sel, manager: Id) {
        EVENTS.lock().unwrap().push(Event {
            manager: manager as usize,
            selector: unsafe { CStr::from_ptr(selector) }
                .to_str()
                .unwrap()
                .to_string(),
            state: unsafe { state(manager, sel()) },
            thread: std::thread::current().id(),
            queue: unsafe { CStr::from_ptr(darwin::dispatch::queue::queue_get_label(0)) }
                .to_str()
                .unwrap()
                .to_string(),
        });
    }
    unsafe extern "C" fn barrier(_: *mut c_void) {}

    #[test]
    fn managers_notify_asynchronously_on_main_or_the_supplied_queue() {
        bootstrap();
        let delegate_class =
            corefoundation::base::abstract_class(c"RosemaryBluetoothTestDelegate", None);
        methods!(delegate_class, {
            "centralManagerDidUpdateState:" => did_update, c"v24@0:8@16";
            "peripheralManagerDidUpdateState:" => did_update, c"v24@0:8@16";
        });
        let target = objc::object::create_instance(delegate_class, 0);
        let main_thread = std::thread::current().id();
        unsafe {
            for (name, selector) in [
                (c"CBCentralManager", "centralManagerDidUpdateState:"),
                (c"CBPeripheralManager", "peripheralManagerDidUpdateState:"),
            ] {
                let object = manager_init_options(allocate(name), sel(), target, 0, null_mut());
                assert_eq!(state(object, sel()), 0);
                assert!(EVENTS.lock().unwrap().is_empty());
                darwin::dispatch::queue::drain_main();
                let events = std::mem::take(&mut *EVENTS.lock().unwrap());
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].manager, object as usize);
                assert_eq!(events[0].selector, selector);
                assert_eq!(events[0].state, UNSUPPORTED);
                assert_eq!(events[0].thread, main_thread);
                assert_eq!(authorization(object, sel()), 0);
                assert!(!inactive(object, sel()));
                noop_two(object, sel(), null_mut(), null_mut());
                assert!(!update_value(
                    object,
                    sel(),
                    null_mut(),
                    null_mut(),
                    null_mut()
                ));
                objc::arc::release(object);
            }
            let queue = darwin::dispatch::queue::create_serial("corebluetooth.tests");
            let object = manager_init_queue(allocate(c"CBCentralManager"), sel(), target, queue);
            darwin::dispatch::queue::sync_f(queue, 0, barrier as *const c_void as u64);
            let events = std::mem::take(&mut *EVENTS.lock().unwrap());
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].manager, object as usize);
            assert_eq!(events[0].state, UNSUPPORTED);
            assert_ne!(events[0].thread, main_thread);
            assert_eq!(events[0].queue, "corebluetooth.tests");
            // The worker drops the delivered item's queue reference after the
            // barrier above may already have returned on this thread.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while darwin::dispatch::object::retain_count(queue) > 2
                && std::time::Instant::now() < deadline
            {
                std::thread::yield_now();
            }
            assert_eq!(darwin::dispatch::object::retain_count(queue), 2);
            objc::arc::release(object);
            assert_eq!(darwin::dispatch::object::retain_count(queue), 1);
            darwin::dispatch::object::release(queue);
            let object = manager_init_queue(allocate(c"CBCentralManager"), sel(), target, 0);
            objc::arc::release(target);
            assert!(delegate(object, sel()).is_null());
            darwin::dispatch::queue::drain_main();
            assert!(EVENTS.lock().unwrap().is_empty());
            assert_eq!(state(object, sel()), UNSUPPORTED);
            objc::arc::release(object);
        }
    }

    #[test]
    fn exported_classes_and_string_cells_are_stable() {
        let symbols = data_symbols();
        assert_eq!(symbols, data_symbols());
        assert!(symbols.iter().all(|(_, address)| *address != 0));
        for name in CLASSES {
            assert!(symbol(&format!("_OBJC_CLASS_$_{}", name.to_string_lossy())).is_some());
        }
        for (name, value) in STRINGS {
            let cell = symbol(name).unwrap();
            assert_eq!(symbol(&name[1..]), Some(cell));
            assert_eq!(unsafe { text(*(cell as *const Id)) }, *value);
        }
    }
}
