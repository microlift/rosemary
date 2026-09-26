use std::sync::{Mutex, OnceLock};

use corefoundation::api;
use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

const GENERAL: &str = "NSGeneralPboard";
const STRING: &str = "public.utf8-plain-text";

#[derive(Default)]
struct Entry {
    ty: Vec<u8>,
    data: Option<Vec<u8>>,
    property_list: usize,
}

impl Drop for Entry {
    fn drop(&mut self) {
        objc::arc::release(self.property_list as Id);
    }
}

struct Board {
    object: usize,
    change_count: isize,
    entries: Vec<Entry>,
    object_strings: Vec<Vec<u8>>,
}

#[derive(Default)]
struct Boards {
    boards: Vec<(Vec<u8>, Board)>,
    next_unique: usize,
}

fn boards() -> &'static Mutex<Boards> {
    static BOARDS: OnceLock<Mutex<Boards>> = OnceLock::new();
    BOARDS.get_or_init(Default::default)
}

fn class() -> &'static objc::class::Class {
    objc::class::lookup(c"NSPasteboard").expect("declared before its methods")
}

fn make_board() -> Board {
    let object = objc::object::create_instance(class(), 0);
    // Named pasteboards outlive every temporary autorelease pool.  The table
    // owns this reference for as long as the board is registered.
    objc::arc::retain(object);
    Board {
        object: object as usize,
        change_count: 0,
        entries: Vec::new(),
        object_strings: Vec::new(),
    }
}

fn board_mut(all: &mut Boards, receiver: Id) -> Option<&mut Board> {
    all.boards
        .iter_mut()
        .find_map(|(_, board)| ((board.object as Id) == receiver).then_some(board))
}

/// Whether the general board is the desktop's own clipboard, rather than one
/// more board of this process like every named one.
///
/// SDL is that desktop clipboard wherever it can be given one as well as read,
/// which is everywhere except its X11 backend - where handing SDL the clipboard
/// costs the process its life, for the reasons
/// [`sdl3::clipboard::may_own_selection`] sets out. There the general board
/// keeps its name and its change count and nothing else: it is process-local,
/// exactly like a named board, rather than half of a desktop clipboard that
/// this process is unable to publish to.
fn shares_desktop(name: &[u8]) -> bool {
    name == CLIPBOARD && sdl3::clipboard::may_own_selection()
}

fn is_general(all: &Boards, receiver: Id) -> bool {
    all.boards
        .iter()
        .any(|(name, board)| shares_desktop(name) && (board.object as Id) == receiver)
}

/// Pull a text change made by another desktop application into the general
/// board before answering a read. SDL is the authority for that one board;
/// named boards remain process-local AppKit objects, and so does the general
/// board wherever [`shares_desktop`] says SDL cannot speak for it.
fn sync_system_text(board: &mut Board) {
    if sdl3::clipboard::has_text() != Some(true) {
        return;
    }
    let Some(text) = sdl3::clipboard::text() else {
        return;
    };
    let existing = board
        .entries
        .iter()
        .find(|entry| entry.ty == STRING.as_bytes())
        .and_then(|entry| entry.data.as_ref());
    if existing == Some(&text) {
        return;
    }
    let entry = entry_mut(board, STRING.as_bytes());
    objc::arc::release(entry.property_list as Id);
    entry.property_list = 0;
    entry.data = Some(text);
    board.object_strings.clear();
    board.change_count += 1;
}

/// The current plain text from the desktop-backed general pasteboard.
///
/// Text controls use this rather than reading SDL directly so text copied by
/// this process and text supplied by another desktop application behave the
/// same way.
pub(crate) fn general_text() -> Option<Vec<u8>> {
    let receiver = named(GENERAL.as_bytes());
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let board = board_mut(&mut all, receiver)?;
    sync_system_text(board);
    board
        .entries
        .iter()
        .find(|entry| entry.ty == STRING.as_bytes())
        .and_then(|entry| entry.data.clone())
}

/// The board `PasteboardCreate(kPasteboardClipboard)` means.
///
/// The same board as `+[NSPasteboard generalPasteboard]`, because on a Mac the
/// two APIs are two vocabularies for one clipboard - a program that copies
/// through Cocoa and pastes through the Pasteboard Manager has to see its own
/// text. See [`crate::pasteboard_manager`].
pub(crate) const CLIPBOARD: &[u8] = GENERAL.as_bytes();

/// The flavour types a named board currently holds.
///
/// The general board is synchronised with the desktop's clipboard first, where
/// it is the desktop's clipboard at all - see [`shares_desktop`] - for the
/// reason [`sync_system_text`] gives: a read has to see what another
/// application copied.
pub(crate) fn flavors(name: &[u8]) -> Vec<Vec<u8>> {
    let general = shares_desktop(name);
    let receiver = named(name);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = board_mut(&mut all, receiver) else {
        return Vec::new();
    };
    if general {
        sync_system_text(board);
    }
    board.entries.iter().map(|entry| entry.ty.clone()).collect()
}

/// One flavour's bytes, or `None` for a flavour the board does not hold.
pub(crate) fn flavor_data(name: &[u8], ty: &[u8]) -> Option<Vec<u8>> {
    let general = shares_desktop(name);
    let receiver = named(name);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let board = board_mut(&mut all, receiver)?;
    if general {
        sync_system_text(board);
    }
    board
        .entries
        .iter()
        .find(|entry| entry.ty == ty)
        .and_then(|entry| entry.data.clone())
}

/// Write one flavour, replacing whatever that flavour held.
pub(crate) fn put_flavor(name: &[u8], ty: &[u8], bytes: Vec<u8>) {
    let general = shares_desktop(name);
    let receiver = named(name);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = board_mut(&mut all, receiver) else {
        return;
    };
    let entry = entry_mut(board, ty);
    objc::arc::release(entry.property_list as Id);
    entry.property_list = 0;
    entry.data = Some(bytes.clone());
    board.object_strings.clear();
    board.change_count += 1;
    // Out to the desktop as well, so another application can paste it - the same
    // thing `-setData:forType:` does for the same board and the same flavour.
    if general && ty == STRING.as_bytes() {
        let _ = sdl3::clipboard::set_text(&bytes);
    }
}

/// Empty a named board, and return its new change count.
pub(crate) fn clear_named(name: &[u8]) -> isize {
    let general = shares_desktop(name);
    let receiver = named(name);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let count = board_mut(&mut all, receiver).map_or(0, clear);
    if general {
        let _ = sdl3::clipboard::set_text(b"");
    }
    count
}

/// A named board's change count, after taking in anything the desktop changed.
pub(crate) fn change_count_of(name: &[u8]) -> isize {
    let general = shares_desktop(name);
    let receiver = named(name);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = board_mut(&mut all, receiver) else {
        return 0;
    };
    if general {
        sync_system_text(board);
    }
    board.change_count
}

fn named(name: &[u8]) -> Id {
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, board)) = all.boards.iter().find(|(n, _)| n == name) {
        return board.object as Id;
    }
    let board = make_board();
    let object = board.object as Id;
    all.boards.push((name.to_vec(), board));
    object
}

fn type_bytes(object: Id) -> Vec<u8> {
    // SAFETY: a string or nil supplied by the guest.
    unsafe { foundation::string::bytes(object) }
}

fn entry_mut<'a>(board: &'a mut Board, ty: &[u8]) -> &'a mut Entry {
    if let Some(at) = board.entries.iter().position(|entry| entry.ty == ty) {
        return &mut board.entries[at];
    }
    board.entries.push(Entry {
        ty: ty.to_vec(),
        data: None,
        property_list: 0,
    });
    board.entries.last_mut().expect("entry just pushed")
}

fn strings(values: impl IntoIterator<Item = Vec<u8>>) -> Id {
    let values: Vec<CFTypeRef> = values
        .into_iter()
        .map(|value| corefoundation::string::from_bytes(&value, false))
        .collect();
    let array = collections::new_array(&values, Policy::from_type_callbacks(), false);
    // The array retained its members.  Balance the construction references.
    for value in values {
        objc::arc::release(value as Id);
    }
    // SAFETY: a new Objective-C object.
    unsafe { api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn general_pasteboard(_class: Id, _sel: Sel) -> Id {
    named(GENERAL.as_bytes())
}

unsafe extern "C" fn pasteboard_with_name(_class: Id, _sel: Sel, name: Id) -> Id {
    named(&type_bytes(name))
}

unsafe extern "C" fn pasteboard_with_unique_name(_class: Id, _sel: Sel) -> Id {
    let name = {
        let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
        all.next_unique += 1;
        format!("NSPasteboard-{}", all.next_unique).into_bytes()
    };
    named(&name)
}

/// `-initWithName:` joins the same named-board registry as the factory.
unsafe extern "C" fn init_with_name(receiver: Id, _sel: Sel, name: Id) -> Id {
    let board = named(&type_bytes(name));
    // The object `alloc` made is only a shell; the registered board is the
    // object's identity.  Give the initializer's caller its owning reference.
    objc::arc::release(receiver);
    objc::arc::retain(board)
}

unsafe extern "C" fn remove_pasteboard_with_name(_class: Id, _sel: Sel, name: Id) {
    let name = type_bytes(name);
    if name == GENERAL.as_bytes() {
        return;
    }
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = all.boards.iter().position(|(n, _)| *n == name) {
        let (_, board) = all.boards.remove(at);
        objc::arc::release(board.object as Id);
    }
}

unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    let all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some((name, _)) = all
        .boards
        .iter()
        .find(|(_, board)| (board.object as Id) == receiver)
    else {
        return std::ptr::null_mut();
    };
    let result = corefoundation::string::from_bytes(name, false);
    // SAFETY: a new Objective-C object.
    unsafe { api::CFAutorelease(result) as Id }
}

unsafe extern "C" fn types(receiver: Id, _sel: Sel) -> Id {
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let general = is_general(&all, receiver);
    if general && let Some(board) = board_mut(&mut all, receiver) {
        sync_system_text(board);
    }
    let values: Vec<Vec<u8>> = board_mut(&mut all, receiver)
        .map(|board| board.entries.iter().map(|entry| entry.ty.clone()).collect())
        .unwrap_or_default();
    strings(values)
}

unsafe extern "C" fn change_count(receiver: Id, _sel: Sel) -> isize {
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    board_mut(&mut all, receiver).map_or(0, |board| board.change_count)
}

fn clear(board: &mut Board) -> isize {
    board.entries.clear();
    board.object_strings.clear();
    board.change_count += 1;
    board.change_count
}

unsafe extern "C" fn clear_contents(receiver: Id, _sel: Sel) -> isize {
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let general = is_general(&all, receiver);
    let count = board_mut(&mut all, receiver).map_or(0, clear);
    if general {
        let _ = sdl3::clipboard::set_text(b"");
    }
    count
}

unsafe extern "C" fn declare_types(receiver: Id, _sel: Sel, types: Id, _owner: Id) -> isize {
    let requested = unsafe { collections::array(types as CFTypeRef) }
        .map(|array| {
            array
                .values
                .iter()
                .map(|value| type_bytes(*value as Id))
                .collect::<Vec<Vec<u8>>>()
        })
        .unwrap_or_default();
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = board_mut(&mut all, receiver) else {
        return 0;
    };
    clear(board);
    for ty in requested {
        entry_mut(board, &ty);
    }
    board.change_count
}

unsafe extern "C" fn add_types(receiver: Id, _sel: Sel, types: Id, _owner: Id) -> isize {
    let requested = unsafe { collections::array(types as CFTypeRef) }
        .map(|array| {
            array
                .values
                .iter()
                .map(|value| type_bytes(*value as Id))
                .collect::<Vec<Vec<u8>>>()
        })
        .unwrap_or_default();
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = board_mut(&mut all, receiver) else {
        return 0;
    };
    for ty in requested {
        entry_mut(board, &ty);
    }
    board.change_count += 1;
    board.change_count
}

unsafe extern "C" fn set_data(receiver: Id, _sel: Sel, data: Id, ty: Id) -> u8 {
    let ty = type_bytes(ty);
    let data = unsafe { collections::data(data as CFTypeRef) }.map(|bytes| bytes.clone());
    let Some(data) = data else { return 0 };
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let general = is_general(&all, receiver);
    let system_text = (general && ty == STRING.as_bytes()).then(|| data.clone());
    let Some(board) = board_mut(&mut all, receiver) else {
        return 0;
    };
    let entry = entry_mut(board, &ty);
    objc::arc::release(entry.property_list as Id);
    entry.property_list = 0;
    entry.data = Some(data);
    board.object_strings.clear();
    board.change_count += 1;
    if let Some(text) = system_text {
        let _ = sdl3::clipboard::set_text(&text);
    }
    1
}

unsafe extern "C" fn data_for_type(receiver: Id, _sel: Sel, ty: Id) -> Id {
    let ty = type_bytes(ty);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    if is_general(&all, receiver)
        && let Some(board) = board_mut(&mut all, receiver)
    {
        sync_system_text(board);
    }
    let Some(data) = board_mut(&mut all, receiver)
        .and_then(|board| board.entries.iter().find(|entry| entry.ty == ty))
        .and_then(|entry| entry.data.clone())
    else {
        return std::ptr::null_mut();
    };
    // SAFETY: a new data object.
    unsafe { api::CFAutorelease(collections::new_data(data)) as Id }
}

unsafe extern "C" fn set_string(receiver: Id, _sel: Sel, string: Id, ty: Id) -> u8 {
    let value = type_bytes(string);
    let ty = type_bytes(ty);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let general = is_general(&all, receiver);
    let system_text = (general && ty == STRING.as_bytes()).then(|| value.clone());
    let Some(board) = board_mut(&mut all, receiver) else {
        return 0;
    };
    let entry = entry_mut(board, &ty);
    objc::arc::release(entry.property_list as Id);
    entry.property_list = 0;
    entry.data = Some(value);
    board.object_strings.clear();
    board.change_count += 1;
    if let Some(text) = system_text {
        let _ = sdl3::clipboard::set_text(&text);
    }
    1
}

unsafe extern "C" fn string_for_type(receiver: Id, _sel: Sel, ty: Id) -> Id {
    let ty = type_bytes(ty);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    if is_general(&all, receiver)
        && let Some(board) = board_mut(&mut all, receiver)
    {
        sync_system_text(board);
    }
    let Some(value) = board_mut(&mut all, receiver)
        .and_then(|board| board.entries.iter().find(|entry| entry.ty == ty))
        .and_then(|entry| entry.data.clone())
    else {
        return std::ptr::null_mut();
    };
    let string = corefoundation::string::from_bytes(&value, false);
    // SAFETY: a new string object.
    unsafe { api::CFAutorelease(string) as Id }
}

unsafe extern "C" fn set_property_list(receiver: Id, _sel: Sel, value: Id, ty: Id) -> u8 {
    if value.is_null() {
        return 0;
    }
    let ty = type_bytes(ty);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = board_mut(&mut all, receiver) else {
        return 0;
    };
    let entry = entry_mut(board, &ty);
    objc::arc::release(entry.property_list as Id);
    entry.property_list = objc::arc::retain(value) as usize;
    entry.data = None;
    board.object_strings.clear();
    board.change_count += 1;
    1
}

unsafe extern "C" fn property_list_for_type(receiver: Id, _sel: Sel, ty: Id) -> Id {
    let ty = type_bytes(ty);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    board_mut(&mut all, receiver)
        .and_then(|board| board.entries.iter().find(|entry| entry.ty == ty))
        .map_or(std::ptr::null_mut(), |entry| {
            // A setter can replace the stored object on another thread after
            // this lock is released, so hand the caller an autoreleased hold.
            unsafe { api::CFAutorelease(objc::arc::retain(entry.property_list as Id)) as Id }
        })
}

unsafe extern "C" fn available_type_from_array(receiver: Id, _sel: Sel, types: Id) -> Id {
    let requested = unsafe { collections::array(types as CFTypeRef) }
        .map(|array| {
            array
                .values
                .iter()
                .map(|value| type_bytes(*value as Id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = board_mut(&mut all, receiver) else {
        return std::ptr::null_mut();
    };
    let Some(ty) = requested
        .into_iter()
        .find(|ty| board.entries.iter().any(|entry| entry.ty == *ty))
    else {
        return std::ptr::null_mut();
    };
    let result = corefoundation::string::from_bytes(&ty, false);
    // SAFETY: a new string object.
    unsafe { api::CFAutorelease(result) as Id }
}

unsafe extern "C" fn prepare_for_new_contents(receiver: Id, _sel: Sel, _options: usize) -> isize {
    unsafe { clear_contents(receiver, objc::sel::get("clearContents")) }
}

fn is_string(object: Id) -> bool {
    let Some(string) = objc::class::lookup(c"NSString") else {
        return false;
    };
    // SAFETY: null or an object this runtime allocated.
    unsafe { objc::object::class_of(object) }
        .is_some_and(|class| class.chain().any(|ancestor| std::ptr::eq(ancestor, string)))
}

fn asks_for_strings(classes: Id) -> bool {
    let Some(string) = objc::class::lookup(c"NSString") else {
        return false;
    };
    // SAFETY: an array of class objects, or an object that is not an array.
    unsafe { collections::array(classes as CFTypeRef) }.is_some_and(|classes| {
        classes
            .values
            .iter()
            .any(|class| (*class as usize) == (objc::class::ptr(string) as usize))
    })
}

/// `-writeObjects:` for the standard object this runtime can serialize.
///
/// `NSString` is the common AppKit pasteboard payload.  Other objects need an
/// `NSPasteboardWriting` implementation; returning `NO` for one is the API's
/// ordinary way to say it cannot be written, rather than claiming success and
/// losing its contents.
unsafe extern "C" fn write_objects(receiver: Id, _sel: Sel, objects: Id) -> u8 {
    let Some(objects) = (unsafe { collections::array(objects as CFTypeRef) }) else {
        return 0;
    };
    if objects.values.is_empty() || !objects.values.iter().all(|object| is_string(*object as Id)) {
        return 0;
    }
    let values: Vec<Vec<u8>> = objects
        .values
        .iter()
        .map(|object| type_bytes(*object as Id))
        .collect();
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let general = is_general(&all, receiver);
    let system_text = general.then(|| values[0].clone());
    let Some(board) = board_mut(&mut all, receiver) else {
        return 0;
    };
    clear(board);
    let entry = entry_mut(board, STRING.as_bytes());
    entry.data = values.first().cloned();
    board.object_strings = values;
    if let Some(text) = system_text {
        let _ = sdl3::clipboard::set_text(&text);
    }
    1
}

unsafe extern "C" fn can_read_objects(receiver: Id, _sel: Sel, classes: Id, _options: Id) -> u8 {
    if !asks_for_strings(classes) {
        return 0;
    }
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    if is_general(&all, receiver)
        && let Some(board) = board_mut(&mut all, receiver)
    {
        sync_system_text(board);
    }
    board_mut(&mut all, receiver).is_some_and(|board| {
        !board.object_strings.is_empty()
            || board
                .entries
                .iter()
                .any(|entry| entry.ty == STRING.as_bytes() && entry.data.is_some())
    }) as u8
}

unsafe extern "C" fn read_objects(receiver: Id, _sel: Sel, classes: Id, _options: Id) -> Id {
    if !asks_for_strings(classes) {
        return std::ptr::null_mut();
    }
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    if is_general(&all, receiver)
        && let Some(board) = board_mut(&mut all, receiver)
    {
        sync_system_text(board);
    }
    let Some(values) = board_mut(&mut all, receiver).map(|board| {
        if board.object_strings.is_empty() {
            board
                .entries
                .iter()
                .find(|entry| entry.ty == STRING.as_bytes())
                .and_then(|entry| entry.data.clone())
                .into_iter()
                .collect()
        } else {
            board.object_strings.clone()
        }
    }) else {
        return std::ptr::null_mut();
    };
    strings(values)
}

unsafe extern "C" fn release_globally(_receiver: Id, _sel: Sel) {}

pub(crate) fn install() {
    let class = class();
    class_methods!(class, {
        "generalPasteboard" => general_pasteboard, c"@16@0:8";
        "pasteboardWithName:" => pasteboard_with_name, c"@24@0:8@16";
        "pasteboardWithUniqueName" => pasteboard_with_unique_name, c"@16@0:8";
        "removePasteboardWithName:" => remove_pasteboard_with_name, c"v24@0:8@16";
    });
    methods!(class, {
        "name" => name, c"@16@0:8";
        "initWithName:" => init_with_name, c"@24@0:8@16";
        "types" => types, c"@16@0:8";
        "changeCount" => change_count, c"q16@0:8";
        "clearContents" => clear_contents, c"q16@0:8";
        "declareTypes:owner:" => declare_types, c"q32@0:8@16@24";
        "addTypes:owner:" => add_types, c"q32@0:8@16@24";
        "setData:forType:" => set_data, c"c32@0:8@16@24";
        "dataForType:" => data_for_type, c"@24@0:8@16";
        "setString:forType:" => set_string, c"c32@0:8@16@24";
        "stringForType:" => string_for_type, c"@24@0:8@16";
        "setPropertyList:forType:" => set_property_list, c"c32@0:8@16@24";
        "propertyListForType:" => property_list_for_type, c"@24@0:8@16";
        "availableTypeFromArray:" => available_type_from_array, c"@24@0:8@16";
        "prepareForNewContentsWithOptions:" => prepare_for_new_contents, c"q24@0:8Q16";
        "writeObjects:" => write_objects, c"c24@0:8@16";
        "canReadObjectForClasses:options:" => can_read_objects, c"c32@0:8@16@24";
        "readObjectsForClasses:options:" => read_objects, c"@32@0:8@16@24";
        "releaseGlobally" => release_globally, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_pasteboard_keeps_string_data_and_type_order() {
        crate::bootstrap();
        let board = unsafe {
            general_pasteboard(std::ptr::null_mut(), objc::sel::get("generalPasteboard"))
        };
        let ty = corefoundation::string::from_bytes(STRING.as_bytes(), false) as Id;
        let text = corefoundation::string::from_bytes(b"paste me", false) as Id;
        assert_eq!(
            unsafe { set_string(board, objc::sel::get("setString:forType:"), text, ty) },
            1
        );
        let read = unsafe { string_for_type(board, objc::sel::get("stringForType:"), ty) };
        assert_eq!(unsafe { foundation::string::bytes(read) }, b"paste me");
        let types = unsafe { types(board, objc::sel::get("types")) };
        let array = unsafe { collections::array(types as CFTypeRef) }.expect("array");
        assert_eq!(array.values.len(), 1);
        assert_eq!(
            unsafe { foundation::string::bytes(array.values[0] as Id) },
            STRING.as_bytes()
        );
    }

    #[test]
    fn named_pasteboards_keep_data_and_support_object_reading() {
        crate::bootstrap();
        let name = corefoundation::string::from_bytes(b"pasteboard-test", false) as Id;
        let board = unsafe {
            pasteboard_with_name(
                std::ptr::null_mut(),
                objc::sel::get("pasteboardWithName:"),
                name,
            )
        };
        let ty = corefoundation::string::from_bytes(STRING.as_bytes(), false) as Id;
        let data = collections::new_data(b"bytes".to_vec()) as Id;
        assert_eq!(
            unsafe { set_data(board, objc::sel::get("setData:forType:"), data, ty) },
            1
        );
        let copied = unsafe { data_for_type(board, objc::sel::get("dataForType:"), ty) };
        assert_eq!(
            unsafe { collections::data(copied as CFTypeRef) }.expect("data"),
            b"bytes"
        );

        let string = corefoundation::string::from_bytes(b"object", false) as Id;
        let objects =
            collections::new_array(&[string as CFTypeRef], Policy::from_type_callbacks(), false)
                as Id;
        assert_eq!(
            unsafe { write_objects(board, objc::sel::get("writeObjects:"), objects) },
            1
        );
        let string_class = objc::class::ptr(objc::class::lookup(c"NSString").expect("NSString"));
        let classes = collections::new_array(
            &[string_class as CFTypeRef],
            Policy::from_type_callbacks(),
            false,
        ) as Id;
        assert_eq!(
            unsafe {
                can_read_objects(
                    board,
                    objc::sel::get("canReadObjectForClasses:options:"),
                    classes,
                    std::ptr::null_mut(),
                )
            },
            1
        );
        let read = unsafe {
            read_objects(
                board,
                objc::sel::get("readObjectsForClasses:options:"),
                classes,
                std::ptr::null_mut(),
            )
        };
        let read = unsafe { collections::array(read as CFTypeRef) }.expect("array");
        assert_eq!(
            unsafe { foundation::string::bytes(read.values[0] as Id) },
            b"object"
        );
    }
}
