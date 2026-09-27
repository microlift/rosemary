use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// A four-character code as the `u32` an `AEEventClass` or `DescType` is.
const fn four(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

pub const CORE_EVENT_CLASS: u32 = four(b"aevt");
pub const INTERNET_EVENT_CLASS: u32 = four(b"GURL");
pub const OPEN_APPLICATION: u32 = four(b"oapp");
pub const REOPEN_APPLICATION: u32 = four(b"rapp");
pub const OPEN_DOCUMENTS: u32 = four(b"odoc");
pub const QUIT_APPLICATION: u32 = four(b"quit");
pub const GET_URL: u32 = four(b"GURL");
pub const ANSWER: u32 = four(b"ansr");

const TYPE_NULL: u32 = four(b"null");
const TYPE_LIST: u32 = four(b"list");
const TYPE_RECORD: u32 = four(b"reco");
const TYPE_APPLE_EVENT: u32 = four(b"aevt");
const TYPE_UNICODE_TEXT: u32 = four(b"utxt");
const TYPE_UTF8_TEXT: u32 = four(b"utf8");
const TYPE_CHAR: u32 = four(b"TEXT");
const TYPE_FILE_URL: u32 = four(b"furl");
const TYPE_TYPE: u32 = four(b"type");
const TYPE_ENUMERATED: u32 = four(b"enum");
const TYPE_BOOLEAN: u32 = four(b"bool");
const TYPE_TRUE: u32 = four(b"true");
const TYPE_FALSE: u32 = four(b"fals");
const TYPE_SINT16: u32 = four(b"shor");
const TYPE_SINT32: u32 = four(b"long");
const TYPE_SINT64: u32 = four(b"comp");
const TYPE_DOUBLE: u32 = four(b"doub");
const TYPE_PROCESS_SERIAL_NUMBER: u32 = four(b"psn ");

/// `keyDirectObject`, the parameter a URL or a document list arrives in.
pub const KEY_DIRECT_OBJECT: u32 = four(b"----");
const KEY_EVENT_CLASS_ATTR: u32 = four(b"evcl");
const KEY_EVENT_ID_ATTR: u32 = four(b"evid");

/// What a descriptor holds: an `AEDesc`, in the three shapes one comes in.
///
/// A list and a record are kept apart rather than folded into one keyed vector,
/// because `-numberOfItems` and `-descriptorAtIndex:` mean different things to
/// each and a descriptor is only ever one of them.
#[derive(Default)]
struct Desc {
    kind: u32,
    /// The bytes of a leaf descriptor, in the byte order a guest reading them
    /// through `-data` expects.
    data: Vec<u8>,
    /// A list's items, retained, in order.
    list: Vec<Id>,
    /// A record's or an event's keyword-addressed items, retained.
    record: Vec<(u32, Id)>,
    /// An event's attributes, retained. Separate from [`Desc::record`] because
    /// `-attributeDescriptorForKeyword:` and `-paramDescriptorForKeyword:` are
    /// two name spaces: `keyDirectObject` in one is not the other's.
    attributes: Vec<(u32, Id)>,
    event_class: u32,
    event_id: u32,
    return_id: i16,
    transaction_id: i32,
}

// SAFETY: the ids are retained by the descriptor that holds them, and a
// descriptor is only touched by the thread holding it.
unsafe impl Send for Desc {}

impl Desc {
    fn leaf(kind: u32, data: Vec<u8>) -> Self {
        Self {
            kind,
            data,
            ..Self::default()
        }
    }

    /// Whether items are addressed by keyword rather than by position.
    fn keyed(&self) -> bool {
        matches!(self.kind, TYPE_RECORD | TYPE_APPLE_EVENT)
    }
}

/// An `NSAppleEventDescriptor`, laid out by this crate.
#[repr(C)]
struct Descriptor {
    isa: *const c_void,
    desc: *mut Desc,
}

fn descriptor_class() -> &'static Class {
    corefoundation::base::register(
        c"NSAppleEventDescriptor",
        None,
        size_of::<Descriptor>(),
        false,
    )
}

/// Wrap `desc` in a fresh, +1 descriptor object.
fn wrap(desc: Desc) -> Id {
    crate::bootstrap();
    // SAFETY: `Descriptor` is the layout `descriptor_class` was registered
    // with, and its first field is the `isa`.
    let object: *mut Descriptor = unsafe { corefoundation::base::allocate(descriptor_class()) };
    // SAFETY: freshly allocated; the field is written before it is read.
    unsafe { (*object).desc = Box::into_raw(Box::new(desc)) };
    object as Id
}

fn autoreleased(desc: Desc) -> Id {
    objc::arc::autorelease(wrap(desc))
}

/// The payload of `object`, or `None` for null and for anything that is not one
/// of this module's descriptors.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn desc_of<'a>(object: Id) -> Option<&'a mut Desc> {
    if !unsafe {
        corefoundation::base::is_kind_of(
            object as corefoundation::base::CFTypeRef,
            descriptor_class(),
        )
    } {
        return None;
    }
    // SAFETY: an instance of this module's class, whose payload it wrote.
    unsafe { (*(object as *mut Descriptor)).desc.as_mut() }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: `-dealloc` is reached with the last reference gone, and the
    // receiver is a `Descriptor` whose payload came from `Box::into_raw`.
    unsafe {
        let object = receiver as *mut Descriptor;
        if !(*object).desc.is_null() {
            let desc = Box::from_raw((*object).desc);
            (*object).desc = std::ptr::null_mut();
            for item in desc.list.iter().copied() {
                objc::arc::release(item);
            }
            for (_, item) in desc.record.iter().chain(desc.attributes.iter()) {
                objc::arc::release(*item);
            }
        }
        objc::arc::finalize(receiver);
    }
}

// -- building descriptors ----------------------------------------------------

/// A string descriptor, autoreleased, as `+descriptorWithString:` makes one.
///
/// `typeUnicodeText` rather than `typeUTF8Text` because that is what Apple's
/// own `+descriptorWithString:` produces, and a guest that switches on
/// `-descriptorType` before reading the bytes is entitled to see it.
pub fn string(text: &str) -> Id {
    let mut data = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        data.extend_from_slice(&unit.to_ne_bytes());
    }
    autoreleased(Desc::leaf(TYPE_UNICODE_TEXT, data))
}

/// An Apple Event with no parameters, autoreleased.
///
/// The target is this process, which is the only one an event raised in this
/// tree can have come from or be going to.
pub fn event(event_class: u32, event_id: u32) -> Id {
    let mut desc = Desc {
        kind: TYPE_APPLE_EVENT,
        event_class,
        event_id,
        ..Desc::default()
    };
    for (keyword, code) in [
        (KEY_EVENT_CLASS_ATTR, event_class),
        (KEY_EVENT_ID_ATTR, event_id),
    ] {
        // `wrap` rather than `autoreleased`: the attribute table owns these,
        // and a pool release of the extra reference would be the only thing
        // between them and the event they describe.
        desc.attributes.push((
            keyword,
            wrap(Desc::leaf(TYPE_TYPE, code.to_ne_bytes().to_vec())),
        ));
    }
    autoreleased(desc)
}

/// Add `value` to `event` under `keyword`, as `-setParamDescriptor:forKeyword:`
/// would.
///
/// # Safety
///
/// Both must be descriptors this module made.
pub unsafe fn set_param(event: Id, keyword: u32, value: Id) {
    unsafe { set_keyed(event, keyword, value, Slot::Param) };
}

/// Which of an event's two keyword name spaces a call means.
#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Param,
    Attribute,
}

/// # Safety
///
/// `receiver` and `value` must be null or objects this runtime allocated.
unsafe fn set_keyed(receiver: Id, keyword: u32, value: Id, slot: Slot) {
    let Some(desc) = (unsafe { desc_of(receiver) }) else {
        return;
    };
    let table = match slot {
        Slot::Param => &mut desc.record,
        Slot::Attribute => &mut desc.attributes,
    };
    if let Some(existing) = table.iter_mut().find(|(k, _)| *k == keyword) {
        objc::arc::release(existing.1);
        match value.is_null() {
            true => table.retain(|(k, _)| *k != keyword),
            false => existing.1 = objc::arc::retain(value),
        }
        return;
    }
    if !value.is_null() {
        table.push((keyword, objc::arc::retain(value)));
    }
}

/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn keyed(receiver: Id, keyword: u32, slot: Slot) -> Id {
    let Some(desc) = (unsafe { desc_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    let table = match slot {
        Slot::Param => &desc.record,
        Slot::Attribute => &desc.attributes,
    };
    table
        .iter()
        .find(|(k, _)| *k == keyword)
        .map_or(std::ptr::null_mut(), |(_, v)| *v)
}

// -- reading descriptors -----------------------------------------------------

/// The text a descriptor holds, for the types that hold text.
///
/// A type that is not text answers `None` rather than its bytes read as
/// characters: `-stringValue` on a `typeSInt32` is nil on Apple's, and bytes of
/// an integer decoded as UTF-8 would be a plausible wrong string instead.
fn text_of(desc: &Desc) -> Option<String> {
    match desc.kind {
        TYPE_UNICODE_TEXT => {
            let units: Vec<u16> = desc
                .data
                .chunks_exact(2)
                .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        TYPE_UTF8_TEXT | TYPE_CHAR | TYPE_FILE_URL => {
            Some(String::from_utf8_lossy(&desc.data).into_owned())
        }
        _ => None,
    }
}

/// The integer a descriptor holds, widened to 64 bits.
fn integer_of(desc: &Desc) -> Option<i64> {
    let data = desc.data.as_slice();
    match desc.kind {
        TYPE_SINT16 if data.len() >= 2 => Some(i16::from_ne_bytes([data[0], data[1]]) as i64),
        TYPE_SINT32 | TYPE_TYPE | TYPE_ENUMERATED if data.len() >= 4 => {
            Some(i32::from_ne_bytes([data[0], data[1], data[2], data[3]]) as i64)
        }
        TYPE_SINT64 if data.len() >= 8 => Some(i64::from_ne_bytes(data[..8].try_into().ok()?)),
        TYPE_DOUBLE if data.len() >= 8 => {
            Some(f64::from_ne_bytes(data[..8].try_into().ok()?) as i64)
        }
        TYPE_BOOLEAN if !data.is_empty() => Some((data[0] != 0) as i64),
        TYPE_TRUE => Some(1),
        TYPE_FALSE => Some(0),
        _ => None,
    }
}

fn real_of(desc: &Desc) -> Option<f64> {
    match desc.kind {
        TYPE_DOUBLE if desc.data.len() >= 8 => {
            Some(f64::from_ne_bytes(desc.data[..8].try_into().ok()?))
        }
        _ => integer_of(desc).map(|n| n as f64),
    }
}

/// The four-character code a `typeType` or `typeEnumerated` holds.
///
/// The bytes are the code in the machine's byte order, which is how an `AEDesc`
/// carries an `OSType` and how a guest that built one with `AECreateDesc` wrote
/// it.
fn code_of(desc: &Desc) -> u32 {
    match desc.data.len() >= 4 {
        true => u32::from_ne_bytes([desc.data[0], desc.data[1], desc.data[2], desc.data[3]]),
        false => 0,
    }
}

// -- NSAppleEventDescriptor --------------------------------------------------

unsafe extern "C" fn null_descriptor(_class: Id, _sel: Sel) -> Id {
    autoreleased(Desc::leaf(TYPE_NULL, Vec::new()))
}

unsafe extern "C" fn list_descriptor(_class: Id, _sel: Sel) -> Id {
    autoreleased(Desc {
        kind: TYPE_LIST,
        ..Desc::default()
    })
}

unsafe extern "C" fn record_descriptor(_class: Id, _sel: Sel) -> Id {
    autoreleased(Desc {
        kind: TYPE_RECORD,
        ..Desc::default()
    })
}

unsafe extern "C" fn descriptor_with_string(_class: Id, _sel: Sel, value: Id) -> Id {
    // SAFETY: `value` is null or a string.
    string(&String::from_utf8_lossy(&unsafe { text_bytes(value) }))
}

/// # Safety
///
/// `string` must be null or a string object.
unsafe fn text_bytes(string: Id) -> Vec<u8> {
    match string.is_null() {
        true => Vec::new(),
        false => unsafe { crate::string::bytes(string) },
    }
}

unsafe extern "C" fn descriptor_with_int32(_class: Id, _sel: Sel, value: i32) -> Id {
    autoreleased(Desc::leaf(TYPE_SINT32, value.to_ne_bytes().to_vec()))
}

unsafe extern "C" fn descriptor_with_double(_class: Id, _sel: Sel, value: f64) -> Id {
    autoreleased(Desc::leaf(TYPE_DOUBLE, value.to_ne_bytes().to_vec()))
}

unsafe extern "C" fn descriptor_with_boolean(_class: Id, _sel: Sel, value: u8) -> Id {
    let kind = match value != 0 {
        true => TYPE_TRUE,
        false => TYPE_FALSE,
    };
    autoreleased(Desc::leaf(kind, Vec::new()))
}

unsafe extern "C" fn descriptor_with_type_code(_class: Id, _sel: Sel, code: u32) -> Id {
    autoreleased(Desc::leaf(TYPE_TYPE, code.to_ne_bytes().to_vec()))
}

unsafe extern "C" fn descriptor_with_enum_code(_class: Id, _sel: Sel, code: u32) -> Id {
    autoreleased(Desc::leaf(TYPE_ENUMERATED, code.to_ne_bytes().to_vec()))
}

unsafe extern "C" fn descriptor_with_bytes(
    _class: Id,
    _sel: Sel,
    kind: u32,
    bytes: *const u8,
    length: u32,
) -> Id {
    let data = match bytes.is_null() {
        true => Vec::new(),
        // SAFETY: the caller declared `length` bytes at `bytes`.
        false => unsafe { std::slice::from_raw_parts(bytes, length as usize) }.to_vec(),
    };
    autoreleased(Desc::leaf(kind, data))
}

unsafe extern "C" fn descriptor_with_data(_class: Id, _sel: Sel, kind: u32, data: Id) -> Id {
    // SAFETY: `data` is null or an `NSData`.
    autoreleased(Desc::leaf(kind, unsafe { data_bytes(data) }))
}

/// # Safety
///
/// `data` must be null or an object this runtime allocated.
unsafe fn data_bytes(data: Id) -> Vec<u8> {
    // SAFETY: the caller's obligation; both answer for a null or foreign object.
    unsafe {
        let bytes = corefoundation::api::CFDataGetBytePtr(data as corefoundation::base::CFTypeRef);
        let length = corefoundation::api::CFDataGetLength(data as corefoundation::base::CFTypeRef);
        match bytes.is_null() || length <= 0 {
            true => Vec::new(),
            false => std::slice::from_raw_parts(bytes, length as usize).to_vec(),
        }
    }
}

/// `+currentProcessDescriptor`, this process as a target.
///
/// `kCurrentProcess`, the process serial number every application may use for
/// itself, because there is no other process here for an event to address.
unsafe extern "C" fn current_process_descriptor(_class: Id, _sel: Sel) -> Id {
    const CURRENT_PROCESS: [u32; 2] = [0, 2];
    let mut data = Vec::with_capacity(8);
    for word in CURRENT_PROCESS {
        data.extend_from_slice(&word.to_ne_bytes());
    }
    autoreleased(Desc::leaf(TYPE_PROCESS_SERIAL_NUMBER, data))
}

unsafe extern "C" fn apple_event_with(
    _class: Id,
    _sel: Sel,
    event_class: u32,
    event_id: u32,
    target: Id,
    return_id: i16,
    transaction_id: i32,
) -> Id {
    let e = event(event_class, event_id);
    // SAFETY: a descriptor this call just made.
    if let Some(desc) = unsafe { desc_of(e) } {
        desc.return_id = return_id;
        desc.transaction_id = transaction_id;
    }
    if !target.is_null() {
        // SAFETY: `target` is null or a descriptor, and `e` is one.
        unsafe { set_keyed(e, four(b"addr"), target, Slot::Attribute) };
    }
    e
}

unsafe extern "C" fn descriptor_type(receiver: Id, _sel: Sel) -> u32 {
    // SAFETY: a descriptor.
    unsafe { desc_of(receiver) }.map_or(TYPE_NULL, |d| d.kind)
}

unsafe extern "C" fn data(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a descriptor.
    let bytes = unsafe { desc_of(receiver) }.map_or_else(Vec::new, |d| d.data.clone());
    let data = corefoundation::collections::new_data(bytes);
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(data) as Id }
}

unsafe extern "C" fn event_class(receiver: Id, _sel: Sel) -> u32 {
    // SAFETY: a descriptor.
    unsafe { desc_of(receiver) }.map_or(0, |d| d.event_class)
}

unsafe extern "C" fn event_id(receiver: Id, _sel: Sel) -> u32 {
    // SAFETY: a descriptor.
    unsafe { desc_of(receiver) }.map_or(0, |d| d.event_id)
}

unsafe extern "C" fn return_id(receiver: Id, _sel: Sel) -> i16 {
    // SAFETY: a descriptor.
    unsafe { desc_of(receiver) }.map_or(0, |d| d.return_id)
}

unsafe extern "C" fn transaction_id(receiver: Id, _sel: Sel) -> i32 {
    // SAFETY: a descriptor.
    unsafe { desc_of(receiver) }.map_or(0, |d| d.transaction_id)
}

unsafe extern "C" fn string_value(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a descriptor.
    let Some(text) = (unsafe { desc_of(receiver) }).and_then(|d| text_of(d)) else {
        return std::ptr::null_mut();
    };
    objc::arc::autorelease(crate::string::from_str(&text))
}

unsafe extern "C" fn int32_value(receiver: Id, _sel: Sel) -> i32 {
    // SAFETY: a descriptor.
    unsafe { desc_of(receiver) }
        .and_then(|d| integer_of(d))
        .unwrap_or(0) as i32
}

unsafe extern "C" fn double_value(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: a descriptor.
    unsafe { desc_of(receiver) }
        .and_then(|d| real_of(d))
        .unwrap_or(0.0)
}

unsafe extern "C" fn boolean_value(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a descriptor.
    u8::from(
        unsafe { desc_of(receiver) }
            .and_then(|d| integer_of(d))
            .unwrap_or(0)
            != 0,
    )
}

unsafe extern "C" fn type_code_value(receiver: Id, _sel: Sel) -> u32 {
    // SAFETY: a descriptor.
    unsafe { desc_of(receiver) }.map_or(0, |d| code_of(d))
}

unsafe extern "C" fn is_record_descriptor(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a descriptor.
    u8::from(unsafe { desc_of(receiver) }.is_some_and(|d| d.keyed()))
}

unsafe extern "C" fn number_of_items(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: a descriptor.
    unsafe { desc_of(receiver) }.map_or(0, |d| match d.keyed() {
        true => d.record.len() as i64,
        false => d.list.len() as i64,
    })
}

/// `-descriptorAtIndex:`, one-based, as every AE list index is.
unsafe extern "C" fn descriptor_at_index(receiver: Id, _sel: Sel, index: i64) -> Id {
    // SAFETY: a descriptor.
    let Some(desc) = (unsafe { desc_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    let Ok(at) = usize::try_from(index - 1) else {
        return std::ptr::null_mut();
    };
    match desc.keyed() {
        true => desc
            .record
            .get(at)
            .map_or(std::ptr::null_mut(), |(_, v)| *v),
        false => desc.list.get(at).copied().unwrap_or(std::ptr::null_mut()),
    }
}

unsafe extern "C" fn keyword_for_descriptor_at_index(receiver: Id, _sel: Sel, index: i64) -> u32 {
    // SAFETY: a descriptor.
    let Some(desc) = (unsafe { desc_of(receiver) }) else {
        return 0;
    };
    usize::try_from(index - 1)
        .ok()
        .and_then(|at| desc.record.get(at))
        .map_or(0, |(k, _)| *k)
}

/// `-insertDescriptor:atIndex:`. Index 0 means "append", which is what Apple's
/// documents and what `-[NSAppleEventDescriptor listDescriptor]` callers use to
/// build a list in order.
unsafe extern "C" fn insert_descriptor_at_index(receiver: Id, _sel: Sel, value: Id, index: i64) {
    // SAFETY: a descriptor.
    let Some(desc) = (unsafe { desc_of(receiver) }) else {
        return;
    };
    if value.is_null() {
        return;
    }
    let value = objc::arc::retain(value);
    let at = match index <= 0 {
        true => desc.list.len(),
        false => ((index - 1) as usize).min(desc.list.len()),
    };
    desc.list.insert(at, value);
}

unsafe extern "C" fn remove_descriptor_at_index(receiver: Id, _sel: Sel, index: i64) {
    // SAFETY: a descriptor.
    let Some(desc) = (unsafe { desc_of(receiver) }) else {
        return;
    };
    let Ok(at) = usize::try_from(index - 1) else {
        return;
    };
    match desc.keyed() {
        true if at < desc.record.len() => objc::arc::release(desc.record.remove(at).1),
        false if at < desc.list.len() => objc::arc::release(desc.list.remove(at)),
        _ => {}
    }
}

unsafe extern "C" fn descriptor_for_keyword(receiver: Id, _sel: Sel, keyword: u32) -> Id {
    // SAFETY: a descriptor.
    unsafe { keyed(receiver, keyword, Slot::Param) }
}

unsafe extern "C" fn set_descriptor_for_keyword(receiver: Id, _sel: Sel, value: Id, keyword: u32) {
    // SAFETY: both are descriptors.
    unsafe { set_keyed(receiver, keyword, value, Slot::Param) };
}

unsafe extern "C" fn remove_descriptor_with_keyword(receiver: Id, _sel: Sel, keyword: u32) {
    // SAFETY: a descriptor; a null value removes.
    unsafe { set_keyed(receiver, keyword, std::ptr::null_mut(), Slot::Param) };
}

unsafe extern "C" fn attribute_descriptor_for_keyword(receiver: Id, _sel: Sel, keyword: u32) -> Id {
    // SAFETY: a descriptor.
    unsafe { keyed(receiver, keyword, Slot::Attribute) }
}

unsafe extern "C" fn set_attribute_descriptor_for_keyword(
    receiver: Id,
    _sel: Sel,
    value: Id,
    keyword: u32,
) {
    // SAFETY: both are descriptors.
    unsafe { set_keyed(receiver, keyword, value, Slot::Attribute) };
}

/// `-coerceToDescriptorType:`, between the leaf types this module can express.
///
/// Nil for a coercion it cannot do, which is Apple's own answer for one the
/// Apple Event Manager has no coercion handler for - and the only honest one,
/// since the alternative is bytes reinterpreted as a type they never held.
unsafe extern "C" fn coerce_to_descriptor_type(receiver: Id, _sel: Sel, kind: u32) -> Id {
    // SAFETY: a descriptor.
    let Some(desc) = (unsafe { desc_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    if desc.kind == kind {
        return receiver;
    }
    coerced(desc, kind).map_or(std::ptr::null_mut(), autoreleased)
}

/// `desc` as `kind`, or `None` for a coercion this module cannot do.
fn coerced(desc: &Desc, kind: u32) -> Option<Desc> {
    let data = match kind {
        TYPE_UNICODE_TEXT => {
            let text = text_of(desc).or_else(|| integer_of(desc).map(|n| n.to_string()))?;
            text.encode_utf16().flat_map(u16::to_ne_bytes).collect()
        }
        TYPE_UTF8_TEXT | TYPE_CHAR => text_of(desc)
            .or_else(|| integer_of(desc).map(|n| n.to_string()))?
            .into_bytes(),
        TYPE_SINT16 => (integer_of(desc)? as i16).to_ne_bytes().to_vec(),
        TYPE_SINT32 => (integer_of(desc)? as i32).to_ne_bytes().to_vec(),
        TYPE_SINT64 => integer_of(desc)?.to_ne_bytes().to_vec(),
        TYPE_DOUBLE => real_of(desc)?.to_ne_bytes().to_vec(),
        TYPE_BOOLEAN => vec![u8::from(integer_of(desc)? != 0)],
        TYPE_TYPE | TYPE_ENUMERATED => code_of(desc).to_ne_bytes().to_vec(),
        _ => return None,
    };
    Some(Desc::leaf(kind, data))
}

/// `-description`, for a program that logs the event it was handed.
unsafe extern "C" fn description(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a descriptor.
    let text = match unsafe { desc_of(receiver) } {
        None => "<NSAppleEventDescriptor: nil>".to_string(),
        Some(desc) => match desc.kind {
            TYPE_APPLE_EVENT => format!(
                "<NSAppleEventDescriptor: {}\\{}, {} parameter(s)>",
                spelling(desc.event_class),
                spelling(desc.event_id),
                desc.record.len()
            ),
            kind => format!(
                "<NSAppleEventDescriptor: '{}', {} byte(s)>",
                spelling(kind),
                desc.data.len()
            ),
        },
    };
    objc::arc::autorelease(crate::string::from_str(&text))
}

/// A four-character code as the four characters it is, for a diagnostic.
fn spelling(code: u32) -> String {
    String::from_utf8_lossy(&code.to_be_bytes()).into_owned()
}

// -- NSAppleEventManager -----------------------------------------------------

/// One `-setEventHandler:andSelector:forEventClass:andEventID:`.
///
/// The handler is *not* retained, which is Apple's rule and the reason
/// `-removeEventHandlerForEventClass:andEventID:` exists: a manager that
/// retained one would keep the application delegate alive forever.
struct Handler {
    event_class: u32,
    event_id: u32,
    handler: Id,
    selector: Sel,
}

// SAFETY: the id is only used on the thread that dispatches, and the mutex is
// what serializes the table itself.
unsafe impl Send for Handler {}

static HANDLERS: std::sync::Mutex<Vec<Handler>> = std::sync::Mutex::new(Vec::new());

/// The one manager, which `+sharedAppleEventManager` must answer with every
/// time - a guest caches the pointer and compares against it.
static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

// The event being handled and the reply being built, for `-currentAppleEvent`
// and `-currentReplyAppleEvent`. Per thread, not global: a handler running on
// one thread must not see the event another thread is dispatching, and Apple's
// is per thread for the same reason.
thread_local! {
    static CURRENT: std::cell::Cell<(Id, Id)> =
        const { std::cell::Cell::new((std::ptr::null_mut(), std::ptr::null_mut())) };
}

unsafe extern "C" fn shared_manager(class: Id, _sel: Sel) -> Id {
    *SHARED.get_or_init(|| {
        // SAFETY: `class` is `NSAppleEventManager`, which this module
        // registered.
        let object = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const Class).expect("a class"),
                0,
            )
        };
        // Retained forever, as every singleton in this tree is: one a guest can
        // release is one it can make dangle by releasing what it never owned.
        objc::arc::retain(object);
        object as usize
    }) as Id
}

unsafe extern "C" fn set_event_handler(
    _receiver: Id,
    _sel: Sel,
    handler: Id,
    selector: Sel,
    event_class: u32,
    event_id: u32,
) {
    if handler.is_null() {
        return;
    }
    let mut handlers = HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    handlers.retain(|h| (h.event_class, h.event_id) != (event_class, event_id));
    handlers.push(Handler {
        event_class,
        event_id,
        handler,
        selector,
    });
    if tracing() {
        eprintln!(
            "rosetta3: foundation: handler for {}/{} is -[{} {}]",
            spelling(event_class),
            spelling(event_id),
            // SAFETY: a live object the guest handed over.
            unsafe { objc::object::class_of(handler) }.map_or("nil", Class::name_str),
            selector_name(selector),
        );
    }
}

unsafe extern "C" fn remove_event_handler(
    _receiver: Id,
    _sel: Sel,
    event_class: u32,
    event_id: u32,
) {
    HANDLERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|h| (h.event_class, h.event_id) != (event_class, event_id));
}

unsafe extern "C" fn current_apple_event(_receiver: Id, _sel: Sel) -> Id {
    CURRENT.with(|c| c.get().0)
}

unsafe extern "C" fn current_reply_apple_event(_receiver: Id, _sel: Sel) -> Id {
    CURRENT.with(|c| c.get().1)
}

/// A suspended dispatch: the event and the reply, kept alive until the handler
/// resumes it.
///
/// The suspension ID a guest is handed is the index into this table plus one,
/// so that zero is never a valid one - a caller that stores the ID in a zeroed
/// struct would otherwise resume an event it never suspended.
static SUSPENDED: std::sync::Mutex<Vec<Option<(usize, usize)>>> = std::sync::Mutex::new(Vec::new());

unsafe extern "C" fn suspend_current_apple_event(_receiver: Id, _sel: Sel) -> *const c_void {
    let (event, reply) = CURRENT.with(|c| c.get());
    if event.is_null() {
        return std::ptr::null();
    }
    objc::arc::retain(event);
    objc::arc::retain(reply);
    let mut table = SUSPENDED.lock().unwrap_or_else(|e| e.into_inner());
    table.push(Some((event as usize, reply as usize)));
    table.len() as *const c_void
}

/// The pair a suspension ID names, or `None` if it names nothing.
fn suspended(id: *const c_void) -> Option<(Id, Id)> {
    let index = (id as usize).checked_sub(1)?;
    let table = SUSPENDED.lock().unwrap_or_else(|e| e.into_inner());
    let (event, reply) = (*table.get(index)?)?;
    Some((event as Id, reply as Id))
}

unsafe extern "C" fn apple_event_for_suspension_id(
    _receiver: Id,
    _sel: Sel,
    id: *const c_void,
) -> Id {
    suspended(id).map_or(std::ptr::null_mut(), |(event, _)| event)
}

unsafe extern "C" fn reply_apple_event_for_suspension_id(
    _receiver: Id,
    _sel: Sel,
    id: *const c_void,
) -> Id {
    suspended(id).map_or(std::ptr::null_mut(), |(_, reply)| reply)
}

unsafe extern "C" fn set_current_with_suspension_id(_receiver: Id, _sel: Sel, id: *const c_void) {
    if let Some(pair) = suspended(id) {
        CURRENT.with(|c| c.set(pair));
    }
}

/// `-resumeWithSuspensionID:`, which ends the suspension and lets the event and
/// its reply go.
unsafe extern "C" fn resume_with_suspension_id(_receiver: Id, _sel: Sel, id: *const c_void) {
    let Some(index) = (id as usize).checked_sub(1) else {
        return;
    };
    let taken = SUSPENDED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(index)
        .and_then(Option::take);
    let Some((event, reply)) = taken else {
        return;
    };
    CURRENT.with(|c| {
        if c.get() == (event as Id, reply as Id) {
            c.set((std::ptr::null_mut(), std::ptr::null_mut()));
        }
    });
    objc::arc::release(event as Id);
    objc::arc::release(reply as Id);
}

// -- dispatch ----------------------------------------------------------------

fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(crate::notification::TRACE_ENV).is_some())
}

/// Hand `event` to the handler registered for its class and ID, and answer with
/// the reply descriptor the handler was given to fill in.
///
/// An event nobody registered for is not an error: on a real Mac the Apple
/// Event Manager answers `errAEEventNotHandled` and the sender carries on, and
/// every application receives events it does not handle.
///
/// # Safety
///
/// `event` must be a descriptor this module made.
pub unsafe fn dispatch(event: Id) -> Id {
    let Some(desc) = (unsafe { desc_of(event) }) else {
        return std::ptr::null_mut();
    };
    let (event_class, event_id) = (desc.event_class, desc.event_id);
    let reply = self::event(CORE_EVENT_CLASS, ANSWER);
    let found = HANDLERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|h| (h.event_class, h.event_id) == (event_class, event_id))
        .map(|h| (h.handler, h.selector));
    if tracing() {
        eprintln!(
            "rosetta3: foundation: dispatch {}/{} to {} handler(s)",
            spelling(event_class),
            spelling(event_id),
            found.is_some() as u8,
        );
    }
    let Some((handler, selector)) = found else {
        // SAFETY: descriptors this module made.
        unsafe { dispatch_to_c(event, reply, event_class, event_id) };
        return reply;
    };
    let previous = CURRENT.with(|c| c.replace((event, reply)));
    // SAFETY: a handler is `void (id, SEL, NSAppleEventDescriptor *,
    // NSAppleEventDescriptor *)` by the contract of
    // `-setEventHandler:andSelector:forEventClass:andEventID:`.
    unsafe { call_handler(handler, selector, event, reply) };
    CURRENT.with(|c| c.set(previous));
    reply
}

// -- The Apple Event Manager's C interface -----------------------------------

/// `AEDesc`: a type code and an opaque handle. The handle here is one of this
/// module's descriptor objects, retained, so the C calls and the Objective-C
/// ones read the same payload.
#[repr(C)]
pub struct AeDesc {
    descriptor_type: u32,
    data_handle: Id,
}

/// `OSErr (*AEEventHandlerProcPtr)(const AppleEvent *, AppleEvent *, SRefCon)`
type CHandlerProc = unsafe extern "C" fn(*const AeDesc, *mut AeDesc, usize) -> i16;

/// One `AEInstallEventHandler`, keyed as the Objective-C table is.
struct CHandler {
    event_class: u32,
    event_id: u32,
    proc_: usize,
    refcon: usize,
}

static C_HANDLERS: std::sync::Mutex<Vec<CHandler>> = std::sync::Mutex::new(Vec::new());

/// `errAEEventNotHandled`, `errAEHandlerNotFound` and `paramErr`.
const EVENT_NOT_HANDLED: i16 = -1708;
const HANDLER_NOT_FOUND: i16 = -1717;
const PARAM_ERR: i16 = -50;

/// Hand an event no Objective-C handler took to a C one, as the Apple Event
/// Manager consults one table for both.
///
/// # Safety
///
/// `event` and `reply` are descriptors this module made.
unsafe fn dispatch_to_c(event: Id, reply: Id, event_class: u32, event_id: u32) {
    let found = C_HANDLERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|h| (h.event_class, h.event_id) == (event_class, event_id))
        .map(|h| (h.proc_, h.refcon));
    let Some((proc_, refcon)) = found else {
        return;
    };
    let event = AeDesc {
        descriptor_type: TYPE_APPLE_EVENT,
        data_handle: event,
    };
    let mut reply = AeDesc {
        descriptor_type: TYPE_APPLE_EVENT,
        data_handle: reply,
    };
    // SAFETY: the guest registered a function of this type; a callable thunk
    // under emulation, as the ABI table describes the argument.
    let f: CHandlerProc = unsafe { std::mem::transmute(proc_) };
    unsafe { f(&event, &mut reply, refcon) };
}

/// `OSErr AEInstallEventHandler(AEEventClass, AEEventID, AEEventHandlerUPP,
/// SRefCon, Boolean isSysHandler)`
#[allow(non_snake_case)]
pub unsafe extern "C" fn AEInstallEventHandler(
    event_class: u32,
    event_id: u32,
    handler: usize,
    refcon: usize,
    _system: u8,
) -> i16 {
    if handler == 0 {
        return PARAM_ERR;
    }
    let mut handlers = C_HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    handlers.retain(|h| (h.event_class, h.event_id) != (event_class, event_id));
    handlers.push(CHandler {
        event_class,
        event_id,
        proc_: handler,
        refcon,
    });
    0
}

/// `OSErr AERemoveEventHandler(AEEventClass, AEEventID, AEEventHandlerUPP,
/// Boolean isSysHandler)`
#[allow(non_snake_case)]
pub unsafe extern "C" fn AERemoveEventHandler(
    event_class: u32,
    event_id: u32,
    handler: usize,
    _system: u8,
) -> i16 {
    let mut handlers = C_HANDLERS.lock().unwrap_or_else(|e| e.into_inner());
    let before = handlers.len();
    handlers.retain(|h| {
        (h.event_class, h.event_id) != (event_class, event_id)
            || (handler != 0 && h.proc_ != handler)
    });
    match handlers.len() == before {
        true => HANDLER_NOT_FOUND,
        false => 0,
    }
}

/// `OSErr AECreateDesc(DescType, const void *, Size, AEDesc *)`
#[allow(non_snake_case)]
pub unsafe extern "C" fn AECreateDesc(
    kind: u32,
    bytes: *const u8,
    size: isize,
    out: *mut AeDesc,
) -> i16 {
    if out.is_null() || size < 0 || (bytes.is_null() && size > 0) {
        return PARAM_ERR;
    }
    let data = match size {
        0 => Vec::new(),
        // SAFETY: the caller says `bytes` covers `size` bytes.
        _ => unsafe { std::slice::from_raw_parts(bytes, size as usize) }.to_vec(),
    };
    // SAFETY: the caller's writable `AEDesc`.
    unsafe {
        out.write(AeDesc {
            descriptor_type: kind,
            data_handle: wrap(Desc::leaf(kind, data)),
        })
    };
    0
}

/// `OSErr AEDisposeDesc(AEDesc *)`, which leaves the null descriptor behind.
#[allow(non_snake_case)]
pub unsafe extern "C" fn AEDisposeDesc(desc: *mut AeDesc) -> i16 {
    if desc.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's `AEDesc`, whose handle is one of this module's or null.
    unsafe {
        objc::arc::release((*desc).data_handle);
        desc.write(AeDesc {
            descriptor_type: TYPE_NULL,
            data_handle: std::ptr::null_mut(),
        });
    }
    0
}

/// `Size AEGetDescDataSize(const AEDesc *)`
#[allow(non_snake_case)]
pub unsafe extern "C" fn AEGetDescDataSize(desc: *const AeDesc) -> isize {
    // SAFETY: the caller's `AEDesc`.
    let handle = match desc.is_null() {
        true => return 0,
        false => unsafe { (*desc).data_handle },
    };
    unsafe { desc_of(handle) }.map_or(0, |d| d.data.len() as isize)
}

/// `OSErr AEGetDescData(const AEDesc *, void *, Size)`: as much as fits.
#[allow(non_snake_case)]
pub unsafe extern "C" fn AEGetDescData(desc: *const AeDesc, out: *mut u8, max: isize) -> i16 {
    if desc.is_null() || (out.is_null() && max > 0) {
        return PARAM_ERR;
    }
    // SAFETY: the caller's `AEDesc`.
    let Some(d) = (unsafe { desc_of((*desc).data_handle) }) else {
        return EVENT_NOT_HANDLED;
    };
    let count = d.data.len().min(max.max(0) as usize);
    // SAFETY: the caller says `out` holds `max` bytes.
    unsafe { std::ptr::copy_nonoverlapping(d.data.as_ptr(), out, count) };
    0
}

exports::exports! {
    functions {
        "AEInstallEventHandler" => AEInstallEventHandler,
        "AERemoveEventHandler" => AERemoveEventHandler,
        "AECreateDesc" => AECreateDesc,
        "AEDisposeDesc" => AEDisposeDesc,
        "AEGetDescDataSize" => AEGetDescDataSize,
        "AEGetDescData" => AEGetDescData,
    }
}

/// Build and dispatch a parameterless event, for the ones this runtime raises.
pub fn raise(event_class: u32, event_id: u32) {
    crate::bootstrap();
    let e = event(event_class, event_id);
    // SAFETY: a descriptor this call just made.
    unsafe { dispatch(e) };
}

/// # Safety
///
/// `handler` must be live and `selector` must name a method taking two objects.
unsafe fn call_handler(handler: Id, selector: Sel, event: Id, reply: Id) {
    let Some(method) =
        (unsafe { objc::object::class_of(handler) }).and_then(|c| c.method(selector))
    else {
        // Apple raises here. A named abort instead, because a handler method
        // that is not there is an event that silently never arrives, which is
        // indistinguishable from an event that was never sent.
        objc::fail!(
            "-[NSAppleEventManager dispatch]: the handler does not respond to {}, the selector \
             it registered",
            selector_name(selector)
        );
    };
    let f: unsafe extern "C" fn(Id, Sel, Id, Id) = unsafe {
        std::mem::transmute::<*const c_void, unsafe extern "C" fn(Id, Sel, Id, Id)>(
            method.implementation(),
        )
    };
    unsafe { f(handler, selector, event, reply) };
}

fn selector_name(selector: Sel) -> String {
    // SAFETY: a registered selector's name is a live C string.
    unsafe { std::ffi::CStr::from_ptr(objc::sel::name(selector)) }
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn install() {
    let descriptor = descriptor_class();
    descriptor.replace_method(
        objc::sel::get("dealloc"),
        dealloc as *const c_void,
        c"v16@0:8",
    );
    class_methods!(descriptor, {
        "nullDescriptor" => null_descriptor, c"@16@0:8";
        "listDescriptor" => list_descriptor, c"@16@0:8";
        "recordDescriptor" => record_descriptor, c"@16@0:8";
        "currentProcessDescriptor" => current_process_descriptor, c"@16@0:8";
        "descriptorWithString:" => descriptor_with_string, c"@24@0:8@16";
        "descriptorWithInt32:" => descriptor_with_int32, c"@24@0:8i16";
        "descriptorWithDouble:" => descriptor_with_double, c"@24@0:8d16";
        "descriptorWithBoolean:" => descriptor_with_boolean, c"@24@0:8C16";
        "descriptorWithTypeCode:" => descriptor_with_type_code, c"@24@0:8I16";
        "descriptorWithEnumCode:" => descriptor_with_enum_code, c"@24@0:8I16";
        "descriptorWithDescriptorType:bytes:length:" => descriptor_with_bytes, c"@36@0:8I16^v20I28";
        "descriptorWithDescriptorType:data:" => descriptor_with_data, c"@32@0:8I16@24";
        "appleEventWithEventClass:eventID:targetDescriptor:returnID:transactionID:"
            => apple_event_with, c"@42@0:8I16I20@24s32i34";
    });
    methods!(descriptor, {
        "descriptorType" => descriptor_type, c"I16@0:8";
        "data" => data, c"@16@0:8";
        "eventClass" => event_class, c"I16@0:8";
        "eventID" => event_id, c"I16@0:8";
        "returnID" => return_id, c"s16@0:8";
        "transactionID" => transaction_id, c"i16@0:8";
        "stringValue" => string_value, c"@16@0:8";
        "int32Value" => int32_value, c"i16@0:8";
        "doubleValue" => double_value, c"d16@0:8";
        "booleanValue" => boolean_value, c"C16@0:8";
        "typeCodeValue" => type_code_value, c"I16@0:8";
        "enumCodeValue" => type_code_value, c"I16@0:8";
        "isRecordDescriptor" => is_record_descriptor, c"c16@0:8";
        "numberOfItems" => number_of_items, c"q16@0:8";
        "descriptorAtIndex:" => descriptor_at_index, c"@24@0:8q16";
        "keywordForDescriptorAtIndex:" => keyword_for_descriptor_at_index, c"I24@0:8q16";
        "insertDescriptor:atIndex:" => insert_descriptor_at_index, c"v32@0:8@16q24";
        "removeDescriptorAtIndex:" => remove_descriptor_at_index, c"v24@0:8q16";
        "descriptorForKeyword:" => descriptor_for_keyword, c"@24@0:8I16";
        "setDescriptor:forKeyword:" => set_descriptor_for_keyword, c"v28@0:8@16I24";
        "removeDescriptorWithKeyword:" => remove_descriptor_with_keyword, c"v24@0:8I16";
        "paramDescriptorForKeyword:" => descriptor_for_keyword, c"@24@0:8I16";
        "setParamDescriptor:forKeyword:" => set_descriptor_for_keyword, c"v28@0:8@16I24";
        "removeParamDescriptorWithKeyword:" => remove_descriptor_with_keyword, c"v24@0:8I16";
        "attributeDescriptorForKeyword:" => attribute_descriptor_for_keyword, c"@24@0:8I16";
        "setAttributeDescriptor:forKeyword:"
            => set_attribute_descriptor_for_keyword, c"v28@0:8@16I24";
        "coerceToDescriptorType:" => coerce_to_descriptor_type, c"@24@0:8I16";
        "description" => description, c"@16@0:8";
    });

    let manager =
        corefoundation::base::register(c"NSAppleEventManager", None, size_of::<usize>(), false);
    class_methods!(manager, {
        "sharedAppleEventManager" => shared_manager, c"@16@0:8";
    });
    methods!(manager, {
        "setEventHandler:andSelector:forEventClass:andEventID:"
            => set_event_handler, c"v40@0:8@16:24I32I36";
        "removeEventHandlerForEventClass:andEventID:" => remove_event_handler, c"v24@0:8I16I20";
        "currentAppleEvent" => current_apple_event, c"@16@0:8";
        "currentReplyAppleEvent" => current_reply_apple_event, c"@16@0:8";
        "suspendCurrentAppleEvent" => suspend_current_apple_event, c"^v16@0:8";
        "appleEventForSuspensionID:" => apple_event_for_suspension_id, c"@24@0:8^v16";
        "replyAppleEventForSuspensionID:" => reply_apple_event_for_suspension_id, c"@24@0:8^v16";
        "setCurrentAppleEventAndReplyEventWithSuspensionID:"
            => set_current_with_suspension_id, c"v24@0:8^v16";
        "resumeWithSuspensionID:" => resume_with_suspension_id, c"v24@0:8^v16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the handler below was called with, so a test can tell an event that
    /// arrived from one that never did.
    ///
    /// Tests share a process and run at once, so each reads back only its own
    /// event class rather than clearing the table under another test's feet.
    static SEEN: std::sync::Mutex<Vec<(u32, u32, String)>> = std::sync::Mutex::new(Vec::new());

    fn seen(event_class: u32) -> Vec<(u32, u32, String)> {
        SEEN.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(class, _, _)| *class == event_class)
            .cloned()
            .collect()
    }

    unsafe extern "C" fn record(_receiver: Id, _sel: Sel, event: Id, reply: Id) {
        // SAFETY: the descriptors `dispatch` hands a handler.
        let url = unsafe {
            let direct = descriptor_for_keyword(
                event,
                objc::sel::get("paramDescriptorForKeyword:"),
                KEY_DIRECT_OBJECT,
            );
            let text = string_value(direct, objc::sel::get("stringValue"));
            String::from_utf8_lossy(&text_bytes(text)).into_owned()
        };
        // SAFETY: an event descriptor.
        let (class, id) = unsafe {
            (
                event_class(event, objc::sel::get("eventClass")),
                event_id(event, objc::sel::get("eventID")),
            )
        };
        assert!(!reply.is_null(), "a handler was given no reply descriptor");
        SEEN.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((class, id, url));
    }

    fn install_handler(class: u32, id: u32) -> Id {
        crate::bootstrap();
        let handler =
            corefoundation::base::register(c"R3AppleEventHandler", None, size_of::<usize>(), false);
        handler.replace_method(
            objc::sel::get("handle:withReply:"),
            record as *const c_void,
            c"v32@0:8@16@24",
        );
        let object = objc::object::create_instance(handler, 0);
        // SAFETY: neither of the first two arguments is read.
        unsafe {
            set_event_handler(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                object,
                objc::sel::get("handle:withReply:"),
                class,
                id,
            )
        };
        object
    }

    /// The whole point of the class: an event raised in this tree reaches the
    /// handler the guest registered, carrying the parameter it was built with.
    #[test]
    fn a_registered_handler_is_given_the_event() {
        let handler = install_handler(INTERNET_EVENT_CLASS, GET_URL);
        let e = event(INTERNET_EVENT_CLASS, GET_URL);
        // SAFETY: two descriptors this call made.
        unsafe { set_param(e, KEY_DIRECT_OBJECT, string("r3://open")) };
        let reply = unsafe { dispatch(e) };
        assert!(!reply.is_null());

        assert_eq!(
            seen(INTERNET_EVENT_CLASS),
            [(INTERNET_EVENT_CLASS, GET_URL, "r3://open".to_string())]
        );

        // SAFETY: neither of the first two arguments is read.
        unsafe {
            remove_event_handler(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                INTERNET_EVENT_CLASS,
                GET_URL,
            )
        };
        objc::arc::release(handler);
    }

    /// An event with no handler is answered rather than refused, because every
    /// application receives events it does not handle.
    #[test]
    fn an_event_nobody_registered_for_is_not_an_error() {
        crate::bootstrap();
        let e = event(four(b"r3ev"), four(b"none"));
        // SAFETY: a descriptor this call made.
        let reply = unsafe { dispatch(e) };
        // SAFETY: the reply is a descriptor this module made.
        assert_eq!(
            unsafe { event_class(reply, objc::sel::get("eventClass")) },
            CORE_EVENT_CLASS
        );
    }

    /// A handler registered twice for one event is one handler: Apple's
    /// replaces, and two would deliver the event twice.
    #[test]
    fn registering_twice_replaces_the_handler() {
        let a = install_handler(four(b"r3tw"), four(b"twic"));
        let b = install_handler(four(b"r3tw"), four(b"twic"));
        raise(four(b"r3tw"), four(b"twic"));
        assert_eq!(seen(four(b"r3tw")).len(), 1);
        // SAFETY: neither of the first two arguments is read.
        unsafe {
            remove_event_handler(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                four(b"r3tw"),
                four(b"twic"),
            )
        };
        objc::arc::release(a);
        objc::arc::release(b);
    }

    /// A removed handler stops hearing its event, which is what makes
    /// `-removeEventHandlerForEventClass:andEventID:` worth having.
    #[test]
    fn a_removed_handler_hears_nothing() {
        let handler = install_handler(four(b"r3rm"), four(b"gone"));
        // SAFETY: neither of the first two arguments is read.
        unsafe {
            remove_event_handler(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                four(b"r3rm"),
                four(b"gone"),
            )
        };
        raise(four(b"r3rm"), four(b"gone"));
        assert!(seen(four(b"r3rm")).is_empty());
        objc::arc::release(handler);
    }

    /// A string round-trips through `typeUnicodeText`, which is the one path
    /// every URL handler in the world takes.
    #[test]
    fn a_string_descriptor_reads_back_as_its_string() {
        crate::bootstrap();
        let d = string("https://example.invalid/x");
        // SAFETY: a descriptor this call made.
        unsafe {
            assert_eq!(
                descriptor_type(d, objc::sel::get("descriptorType")),
                TYPE_UNICODE_TEXT
            );
            let text = string_value(d, objc::sel::get("stringValue"));
            assert_eq!(
                String::from_utf8_lossy(&text_bytes(text)),
                "https://example.invalid/x"
            );
        }
    }

    /// A list is one-based and keeps the order items were inserted in, because
    /// an `odoc` event's file list is read back by index.
    #[test]
    fn a_list_is_indexed_from_one_in_insertion_order() {
        crate::bootstrap();
        // SAFETY: neither argument is read.
        let list = unsafe { list_descriptor(std::ptr::null_mut(), objc::sel::get("x")) };
        // SAFETY: descriptors this module made.
        unsafe {
            for text in ["first", "second"] {
                insert_descriptor_at_index(
                    list,
                    objc::sel::get("insertDescriptor:atIndex:"),
                    string(text),
                    0,
                );
            }
            assert_eq!(number_of_items(list, objc::sel::get("numberOfItems")), 2);
            let second = descriptor_at_index(list, objc::sel::get("descriptorAtIndex:"), 2);
            let text = string_value(second, objc::sel::get("stringValue"));
            assert_eq!(String::from_utf8_lossy(&text_bytes(text)), "second");
        }
    }

    /// Parameters and attributes are two name spaces, so the same keyword in
    /// each is two different descriptors.
    #[test]
    fn a_parameter_is_not_an_attribute() {
        crate::bootstrap();
        let e = event(CORE_EVENT_CLASS, OPEN_APPLICATION);
        // SAFETY: descriptors this module made.
        unsafe {
            set_param(e, KEY_DIRECT_OBJECT, string("parameter"));
            set_attribute_descriptor_for_keyword(
                e,
                objc::sel::get("setAttributeDescriptor:forKeyword:"),
                string("attribute"),
                KEY_DIRECT_OBJECT,
            );
            let read = |slot| {
                let d = keyed(e, KEY_DIRECT_OBJECT, slot);
                let text = string_value(d, objc::sel::get("stringValue"));
                String::from_utf8_lossy(&text_bytes(text)).into_owned()
            };
            assert_eq!(read(Slot::Param), "parameter");
            assert_eq!(read(Slot::Attribute), "attribute");
        }
    }

    /// The event's class and ID are readable as attributes too, which is how a
    /// handler registered for a wildcard would tell events apart.
    #[test]
    fn an_event_carries_its_class_as_an_attribute() {
        crate::bootstrap();
        let e = event(CORE_EVENT_CLASS, OPEN_DOCUMENTS);
        // SAFETY: a descriptor this module made.
        let code = unsafe {
            let attr = attribute_descriptor_for_keyword(
                e,
                objc::sel::get("attributeDescriptorForKeyword:"),
                KEY_EVENT_ID_ATTR,
            );
            type_code_value(attr, objc::sel::get("typeCodeValue"))
        };
        assert_eq!(code, OPEN_DOCUMENTS);
    }

    /// A coercion this module can do produces the type asked for, and one it
    /// cannot is nil rather than reinterpreted bytes.
    #[test]
    fn a_coercion_either_converts_or_answers_nil() {
        crate::bootstrap();
        // SAFETY: neither of the first two arguments is read.
        let number =
            unsafe { descriptor_with_int32(std::ptr::null_mut(), objc::sel::get("x"), -7) };
        // SAFETY: a descriptor this call made.
        unsafe {
            let text = coerce_to_descriptor_type(
                number,
                objc::sel::get("coerceToDescriptorType:"),
                TYPE_UNICODE_TEXT,
            );
            let value = string_value(text, objc::sel::get("stringValue"));
            assert_eq!(String::from_utf8_lossy(&text_bytes(value)), "-7");
            assert!(
                coerce_to_descriptor_type(
                    number,
                    objc::sel::get("coerceToDescriptorType:"),
                    TYPE_LIST,
                )
                .is_null()
            );
        }
    }

    /// `+sharedAppleEventManager` is a singleton, which is what makes a guest's
    /// cached pointer to it stay valid.
    #[test]
    fn the_manager_is_one_object() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSAppleEventManager").expect("registered by `install`");
        let class = objc::class::ptr(class) as Id;
        // SAFETY: the class this module registered.
        let (a, b) = unsafe {
            (
                shared_manager(class, objc::sel::get("sharedAppleEventManager")),
                shared_manager(class, objc::sel::get("sharedAppleEventManager")),
            )
        };
        assert!(!a.is_null());
        assert_eq!(a, b);
    }

    /// A suspended event outlives the handler that suspended it, and the
    /// suspension ID reads it back - which is the whole reason a handler
    /// suspends rather than replying inline.
    #[test]
    fn a_suspended_event_is_readable_until_it_is_resumed() {
        crate::bootstrap();
        let e = event(four(b"r3su"), four(b"susp"));
        let reply = event(CORE_EVENT_CLASS, ANSWER);
        CURRENT.with(|c| c.set((e, reply)));
        // SAFETY: neither argument is read.
        let id = unsafe { suspend_current_apple_event(std::ptr::null_mut(), objc::sel::get("x")) };
        assert!(!id.is_null(), "a suspension ID of zero is not a valid one");
        CURRENT.with(|c| c.set((std::ptr::null_mut(), std::ptr::null_mut())));

        // SAFETY: neither of the first two arguments is read.
        unsafe {
            assert_eq!(
                apple_event_for_suspension_id(std::ptr::null_mut(), objc::sel::get("x"), id),
                e
            );
            resume_with_suspension_id(std::ptr::null_mut(), objc::sel::get("x"), id);
            assert!(
                apple_event_for_suspension_id(std::ptr::null_mut(), objc::sel::get("x"), id)
                    .is_null()
            );
        }
    }
}
