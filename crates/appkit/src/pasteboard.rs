use std::sync::{Mutex, OnceLock};

use corefoundation::api;
use corefoundation::base::CFTypeRef;
use corefoundation::callbacks::Policy;
use corefoundation::collections;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// `NSGeneralPboard`'s value, so `+generalPasteboard` and
/// `+pasteboardWithName:NSGeneralPboard` are one board, as on a Mac.
const GENERAL: &str = "Apple CFPasteboard general";
const STRING: &str = "public.utf8-plain-text";

#[derive(Default)]
struct Entry {
    ty: Vec<u8>,
    data: Option<Vec<u8>>,
    property_list: usize,
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        objc::arc::retain(self.property_list as Id);
        Self {
            ty: self.ty.clone(),
            data: self.data.clone(),
            property_list: self.property_list,
        }
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        objc::arc::release(self.property_list as Id);
    }
}

struct Board {
    object: usize,
    change_count: isize,
    /// The first item's entries, which the declare-and-set API writes to.
    entries: Vec<Entry>,
    /// Every later item `-writeObjects:` wrote.
    more_items: Vec<Vec<Entry>>,
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
        more_items: Vec::new(),
    }
}

fn board_mut(all: &mut Boards, receiver: Id) -> Option<&mut Board> {
    all.boards
        .iter_mut()
        .find_map(|(_, board)| ((board.object as Id) == receiver).then_some(board))
}

fn is_general(all: &Boards, receiver: Id) -> bool {
    all.boards
        .iter()
        .any(|(name, board)| name == GENERAL.as_bytes() && board.object as Id == receiver)
}

/// Pull a text change made by another desktop application into the general
/// board before answering a read. SDL is the authority for that one board;
/// named boards remain process-local AppKit objects.
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
    board.more_items.clear();
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

const HTML: &str = "public.html";

/// The general pasteboard's plain text and HTML, with its change count: for a
/// toolkit that keeps a clipboard of its own and has to be handed ours.
pub fn general_text_and_html() -> (isize, Option<Vec<u8>>, Option<Vec<u8>>) {
    let count = change_count_of(CLIPBOARD);
    (
        count,
        flavor_data(CLIPBOARD, STRING.as_bytes()),
        flavor_data(CLIPBOARD, HTML.as_bytes()),
    )
}

/// Make the general pasteboard hold this text and HTML and nothing else, as
/// a copy in another toolkit does; its new change count.
pub fn set_general_text_and_html(text: Option<Vec<u8>>, html: Option<Vec<u8>>) -> isize {
    clear_named(CLIPBOARD);
    if let Some(text) = text {
        put_flavor(CLIPBOARD, STRING.as_bytes(), text);
    }
    if let Some(html) = html {
        put_flavor(CLIPBOARD, HTML.as_bytes(), html);
    }
    change_count_of(CLIPBOARD)
}

/// The board `PasteboardCreate(kPasteboardClipboard)` means.
///
/// The same board as `+[NSPasteboard generalPasteboard]`, because on a Mac the
/// two APIs are two vocabularies for one clipboard - a program that copies
/// through Cocoa and pastes through the Pasteboard Manager has to see its own
/// text. See [`crate::pasteboard_manager`].
pub(crate) const CLIPBOARD: &[u8] = GENERAL.as_bytes();

/// `NSPasteboardNameDrag`, the board a dragging session carries.
const DRAG: &str = "Apple CFPasteboard drag";

/// The drag pasteboard, which is the same object every time it is asked for.
pub(crate) fn drag_board() -> Id {
    named(DRAG.as_bytes())
}

/// The types a pasteboard object holds, for matching a drag against the types
/// a destination registered.
pub(crate) fn types_of(receiver: Id) -> Vec<Vec<u8>> {
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    if is_general(&all, receiver)
        && let Some(board) = board_mut(&mut all, receiver)
    {
        sync_system_text(board);
    }
    board_mut(&mut all, receiver)
        .map(|board| board.entries.iter().map(|entry| entry.ty.clone()).collect())
        .unwrap_or_default()
}

/// The flavour types a named board currently holds.
///
/// The general board is synchronised with the desktop's clipboard first, for the
/// reason [`sync_system_text`] gives: a read has to see what another application
/// copied.
pub(crate) fn flavors(name: &[u8]) -> Vec<Vec<u8>> {
    let general = name == CLIPBOARD;
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
    let general = name == CLIPBOARD;
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
    let general = name == CLIPBOARD;
    let receiver = named(name);
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = board_mut(&mut all, receiver) else {
        return;
    };
    let entry = entry_mut(board, ty);
    objc::arc::release(entry.property_list as Id);
    entry.property_list = 0;
    entry.data = Some(bytes.clone());
    board.more_items.clear();
    board.change_count += 1;
    // Out to the desktop as well, so another application can paste it - the same
    // thing `-setData:forType:` does for the same board and the same flavour.
    if general && ty == STRING.as_bytes() {
        let _ = sdl3::clipboard::set_text(&bytes);
    }
}

/// Empty a named board, and return its new change count.
pub(crate) fn clear_named(name: &[u8]) -> isize {
    let general = name == CLIPBOARD;
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
    let general = name == CLIPBOARD;
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

pub(crate) fn strings(values: impl IntoIterator<Item = Vec<u8>>) -> Id {
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

/// `-changeCount`, which a program polls to notice another application's
/// copy, so the general board takes in the desktop's first.
unsafe extern "C" fn change_count(receiver: Id, _sel: Sel) -> isize {
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let general = is_general(&all, receiver);
    board_mut(&mut all, receiver).map_or(0, |board| {
        if general {
            sync_system_text(board);
        }
        board.change_count
    })
}

fn clear(board: &mut Board) -> isize {
    board.entries.clear();
    board.more_items.clear();
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
    board.more_items.clear();
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
    board.more_items.clear();
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
    board.more_items.clear();
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

/// One `-writeObjects:` argument as the item it becomes on the board.
fn written_item(object: Id) -> Option<Vec<Entry>> {
    if is_string(object) {
        return Some(vec![Entry {
            ty: STRING.as_bytes().to_vec(),
            data: Some(type_bytes(object)),
            property_list: 0,
        }]);
    }
    if !is_item(object) {
        return None;
    }
    // SAFETY: an `NSPasteboardItem`, which this module laid out.
    let item = unsafe { &*(object as *const Item) };
    // An item belongs to at most one board; one already written, or read
    // from a board, cannot be written again.
    if !item.board.is_null() {
        return None;
    }
    // SAFETY: null or the item's own contents, from `Box::into_raw`.
    Some(unsafe { item.own.as_ref() }.cloned().unwrap_or_default())
}

/// `-writeObjects:` for strings and `NSPasteboardItem`s, each one item.
///
/// Other objects need an `NSPasteboardWriting` implementation; returning `NO`
/// for one is the API's ordinary way to say it cannot be written, rather than
/// claiming success and losing its contents.
unsafe extern "C" fn write_objects(receiver: Id, _sel: Sel, objects: Id) -> u8 {
    let Some(objects) = (unsafe { collections::array(objects as CFTypeRef) }) else {
        return 0;
    };
    let objects: Vec<Id> = objects.values.iter().map(|object| *object as Id).collect();
    let Some(mut items) = objects
        .iter()
        .map(|object| written_item(*object))
        .collect::<Option<Vec<_>>>()
    else {
        return 0;
    };
    if items.is_empty() {
        return 0;
    }
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let general = is_general(&all, receiver);
    let Some(board) = board_mut(&mut all, receiver) else {
        return 0;
    };
    clear(board);
    board.entries = items.remove(0);
    board.more_items = items;
    let change_count = board.change_count;
    let system_text = general.then(|| {
        board
            .entries
            .iter()
            .find(|entry| entry.ty == STRING.as_bytes())
            .and_then(|entry| entry.data.clone())
            .unwrap_or_default()
    });
    drop(all);
    for (index, object) in objects.into_iter().enumerate() {
        if is_item(object) {
            bind_item(object, receiver, change_count, index);
        }
    }
    if let Some(text) = system_text {
        let _ = sdl3::clipboard::set_text(&text);
    }
    1
}

/// The plain text of every item that has some, in item order.
fn item_strings(board: &Board) -> Vec<Vec<u8>> {
    std::iter::once(&board.entries)
        .chain(&board.more_items)
        .filter_map(|entries| {
            entries
                .iter()
                .find(|entry| entry.ty == STRING.as_bytes())
                .and_then(|entry| entry.data.clone())
        })
        .collect()
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
    board_mut(&mut all, receiver).is_some_and(|board| !item_strings(board).is_empty()) as u8
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
    let Some(values) = board_mut(&mut all, receiver).map(|board| item_strings(board)) else {
        return std::ptr::null_mut();
    };
    strings(values)
}

/// An `NSPasteboardItem`.
///
/// One made with `+alloc`/`-init` keeps its own contents until it is written.
/// A written item, or one `-pasteboardItems` handed out, reads its board live
/// and answers nothing once the board's change count moves on, as an item on
/// a Mac's board does.
#[repr(C)]
struct Item {
    isa: *const std::ffi::c_void,
    board: Id,
    change_count: isize,
    index: usize,
    own: *mut Vec<Entry>,
}

pub const ITEM_INSTANCE_SIZE: usize = size_of::<Item>();

fn item_class() -> &'static objc::class::Class {
    objc::class::lookup(c"NSPasteboardItem").expect("declared before its methods")
}

fn is_item(object: Id) -> bool {
    let class = item_class();
    // SAFETY: null or an object this runtime allocated.
    unsafe { objc::object::class_of(object) }
        .is_some_and(|have| have.chain().any(|ancestor| std::ptr::eq(ancestor, class)))
}

/// Tie an item to its place on a board, dropping any contents of its own.
fn bind_item(object: Id, board: Id, change_count: isize, index: usize) {
    // SAFETY: an `NSPasteboardItem`, which this module laid out.
    let item = unsafe { &mut *(object as *mut Item) };
    if !item.board.is_null() {
        return;
    }
    item.board = objc::arc::retain(board);
    item.change_count = change_count;
    item.index = index;
    if !item.own.is_null() {
        // SAFETY: from `Box::into_raw`, owned by the item until now.
        drop(unsafe { Box::from_raw(item.own) });
        item.own = std::ptr::null_mut();
    }
}

/// The number of items a board holds.
fn item_count(board: &Board) -> usize {
    if board.entries.is_empty() && board.more_items.is_empty() {
        0
    } else {
        1 + board.more_items.len()
    }
}

/// Item `index`'s entries; the first item is the board's own entries, which
/// the declare-and-set API writes to.
fn item_entries(board: &Board, index: usize) -> &[Entry] {
    match index {
        0 => &board.entries,
        _ => board.more_items.get(index - 1).map_or(&[], Vec::as_slice),
    }
}

/// The item's entries, or nothing once its board has changed.
fn current_flavors(receiver: Id) -> Vec<Entry> {
    // SAFETY: an `NSPasteboardItem`, which this module laid out.
    let item = unsafe { &*(receiver as *const Item) };
    if item.board.is_null() {
        // SAFETY: null or the item's own contents, from `Box::into_raw`.
        return unsafe { item.own.as_ref() }.cloned().unwrap_or_default();
    }
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let general = is_general(&all, item.board);
    let Some(board) = board_mut(&mut all, item.board) else {
        return Vec::new();
    };
    if general {
        sync_system_text(board);
    }
    if board.change_count != item.change_count {
        return Vec::new();
    }
    // Cloned, so a property list stays held past the lock, as
    // `-propertyListForType:` on the board does.
    item_entries(board, item.index).to_vec()
}

unsafe extern "C" fn pasteboard_items(receiver: Id, _sel: Sel) -> Id {
    let mut all = boards().lock().unwrap_or_else(|e| e.into_inner());
    let general = is_general(&all, receiver);
    let Some(board) = board_mut(&mut all, receiver) else {
        return std::ptr::null_mut();
    };
    if general {
        sync_system_text(board);
    }
    let change_count = board.change_count;
    let count = item_count(board);
    drop(all);
    let items: Vec<CFTypeRef> = (0..count)
        .map(|index| {
            let object = objc::object::create_instance(item_class(), 0);
            bind_item(object, receiver, change_count, index);
            object as CFTypeRef
        })
        .collect();
    let array = collections::new_array(&items, Policy::from_type_callbacks(), false);
    for item in items {
        objc::arc::release(item as Id);
    }
    // SAFETY: a new Objective-C object.
    unsafe { api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn item_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: an `NSPasteboardItem`, which this module laid out.
    let item = unsafe { &mut *(receiver as *mut Item) };
    objc::arc::release(item.board);
    item.board = std::ptr::null_mut();
    if !item.own.is_null() {
        // SAFETY: from `Box::into_raw`, owned by the item until `-dealloc`.
        drop(unsafe { Box::from_raw(item.own) });
        item.own = std::ptr::null_mut();
    }
    objc::arc::finalize(receiver);
}

/// Store one flavour in an item that is not yet on a board; `NO` for one
/// that is, whose contents are the board's.
fn set_item_flavor(receiver: Id, ty: Id, data: Option<Vec<u8>>, property_list: Id) -> u8 {
    // SAFETY: an `NSPasteboardItem`, which this module laid out.
    let item = unsafe { &mut *(receiver as *mut Item) };
    if !item.board.is_null() {
        return 0;
    }
    if item.own.is_null() {
        item.own = Box::into_raw(Box::default());
    }
    // SAFETY: from `Box::into_raw`, owned by the item.
    let own = unsafe { &mut *item.own };
    let ty = type_bytes(ty);
    let entry = match own.iter().position(|entry| entry.ty == ty) {
        Some(at) => &mut own[at],
        None => {
            own.push(Entry {
                ty,
                data: None,
                property_list: 0,
            });
            own.last_mut().expect("entry just pushed")
        }
    };
    objc::arc::release(entry.property_list as Id);
    entry.property_list = objc::arc::retain(property_list) as usize;
    entry.data = data;
    1
}

unsafe extern "C" fn item_set_data(receiver: Id, _sel: Sel, data: Id, ty: Id) -> u8 {
    let Some(data) = (unsafe { collections::data(data as CFTypeRef) }).map(|bytes| bytes.clone())
    else {
        return 0;
    };
    set_item_flavor(receiver, ty, Some(data), std::ptr::null_mut())
}

unsafe extern "C" fn item_set_string(receiver: Id, _sel: Sel, string: Id, ty: Id) -> u8 {
    if string.is_null() {
        return 0;
    }
    set_item_flavor(receiver, ty, Some(type_bytes(string)), std::ptr::null_mut())
}

unsafe extern "C" fn item_set_property_list(receiver: Id, _sel: Sel, value: Id, ty: Id) -> u8 {
    if value.is_null() {
        return 0;
    }
    set_item_flavor(receiver, ty, None, value)
}

unsafe extern "C" fn item_types(receiver: Id, _sel: Sel) -> Id {
    strings(
        current_flavors(receiver)
            .iter()
            .map(|entry| entry.ty.clone()),
    )
}

fn item_data(receiver: Id, ty: Id) -> Option<Vec<u8>> {
    let ty = type_bytes(ty);
    current_flavors(receiver)
        .iter()
        .find(|entry| entry.ty == ty)
        .and_then(|entry| entry.data.clone())
}

unsafe extern "C" fn item_data_for_type(receiver: Id, _sel: Sel, ty: Id) -> Id {
    let Some(data) = item_data(receiver, ty) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a new data object.
    unsafe { api::CFAutorelease(collections::new_data(data)) as Id }
}

unsafe extern "C" fn item_string_for_type(receiver: Id, _sel: Sel, ty: Id) -> Id {
    let Some(value) = item_data(receiver, ty) else {
        return std::ptr::null_mut();
    };
    let string = corefoundation::string::from_bytes(&value, false);
    // SAFETY: a new string object.
    unsafe { api::CFAutorelease(string) as Id }
}

unsafe extern "C" fn item_property_list_for_type(receiver: Id, _sel: Sel, ty: Id) -> Id {
    let ty = type_bytes(ty);
    current_flavors(receiver)
        .iter()
        .find(|entry| entry.ty == ty)
        .map_or(std::ptr::null_mut(), |entry| {
            // SAFETY: held by the clone until the caller's pool drains.
            unsafe { api::CFAutorelease(objc::arc::retain(entry.property_list as Id)) as Id }
        })
}

unsafe extern "C" fn item_available_type_from_array(receiver: Id, _sel: Sel, types: Id) -> Id {
    let requested = unsafe { collections::array(types as CFTypeRef) }
        .map(|array| {
            array
                .values
                .iter()
                .map(|value| type_bytes(*value as Id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let flavors = current_flavors(receiver);
    let Some(ty) = requested
        .into_iter()
        .find(|ty| flavors.iter().any(|entry| entry.ty == *ty))
    else {
        return std::ptr::null_mut();
    };
    let result = corefoundation::string::from_bytes(&ty, false);
    // SAFETY: a new string object.
    unsafe { api::CFAutorelease(result) as Id }
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
        "pasteboardItems" => pasteboard_items, c"@16@0:8";
    });
    methods!(item_class(), {
        "dealloc" => item_dealloc, c"v16@0:8";
        "types" => item_types, c"@16@0:8";
        "dataForType:" => item_data_for_type, c"@24@0:8@16";
        "stringForType:" => item_string_for_type, c"@24@0:8@16";
        "propertyListForType:" => item_property_list_for_type, c"@24@0:8@16";
        "availableTypeFromArray:" => item_available_type_from_array, c"@24@0:8@16";
        "setData:forType:" => item_set_data, c"c32@0:8@16@24";
        "setString:forType:" => item_set_string, c"c32@0:8@16@24";
        "setPropertyList:forType:" => item_set_property_list, c"c32@0:8@16@24";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `NSGeneralPboard` names the board `+generalPasteboard` answers.
    #[test]
    fn the_general_board_is_the_one_its_name_constant_names() {
        if !sdl3::isolated::run_here_with_dummy_video() {
            return;
        }
        crate::bootstrap();
        let name = corefoundation::string::from_bytes(b"Apple CFPasteboard general", false) as Id;
        // SAFETY: the two class methods' signatures.
        let (general, by_name) = unsafe {
            (
                general_pasteboard(std::ptr::null_mut(), objc::sel::get("generalPasteboard")),
                pasteboard_with_name(
                    std::ptr::null_mut(),
                    objc::sel::get("pasteboardWithName:"),
                    name,
                ),
            )
        };
        assert_eq!(general, by_name);
    }

    /// Another application's copy shows in the general board's change count,
    /// which is how a program polling it notices.
    #[test]
    fn change_count_notices_a_copy_made_elsewhere() {
        if !sdl3::isolated::run_here_with_dummy_video() {
            return;
        }
        crate::bootstrap();
        // The desktop clipboard is the video subsystem's.
        if !sdl3::may_pump() || !sdl3::video::init_video() {
            return;
        }
        let sel = objc::sel::get("changeCount");
        // SAFETY: `+generalPasteboard` is `id (Class, SEL)`.
        let board = unsafe {
            general_pasteboard(std::ptr::null_mut(), objc::sel::get("generalPasteboard"))
        };
        assert!(sdl3::clipboard::set_text(b"first copy elsewhere"));
        // SAFETY: `-changeCount` on the general board.
        let before = unsafe { change_count(board, sel) };
        assert!(sdl3::clipboard::set_text(b"second copy elsewhere"));
        let after = unsafe { change_count(board, sel) };
        assert!(after > before, "{before} -> {after}");
        let ty = corefoundation::string::from_bytes(STRING.as_bytes(), false) as Id;
        // SAFETY: `-stringForType:` on the general board.
        let read = unsafe { string_for_type(board, objc::sel::get("stringForType:"), ty) };
        assert_eq!(
            unsafe { foundation::string::bytes(read) },
            b"second copy elsewhere"
        );
    }

    #[test]
    fn general_pasteboard_keeps_string_data_and_type_order() {
        if !sdl3::isolated::run_here_with_dummy_video() {
            return;
        }
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

    fn item_string(item: Id) -> Option<Vec<u8>> {
        let ty = corefoundation::string::from_bytes(STRING.as_bytes(), false) as Id;
        // SAFETY: `-stringForType:` on an `NSPasteboardItem`.
        let read = unsafe { item_string_for_type(item, objc::sel::get("stringForType:"), ty) };
        (!read.is_null()).then(|| unsafe { foundation::string::bytes(read) })
    }

    /// Each written string is its own item, and an item stops answering once
    /// its board is cleared.
    #[test]
    fn pasteboard_items_split_written_strings_and_go_stale_on_change() {
        crate::bootstrap();
        let name = corefoundation::string::from_bytes(b"pasteboard-items-test", false) as Id;
        let board = unsafe {
            pasteboard_with_name(
                std::ptr::null_mut(),
                objc::sel::get("pasteboardWithName:"),
                name,
            )
        };
        let sel = objc::sel::get("pasteboardItems");
        unsafe { clear_contents(board, objc::sel::get("clearContents")) };
        let empty = unsafe { pasteboard_items(board, sel) };
        assert!(
            unsafe { collections::array(empty as CFTypeRef) }
                .expect("array")
                .values
                .is_empty()
        );

        let first = corefoundation::string::from_bytes(b"first", false);
        let second = corefoundation::string::from_bytes(b"second", false);
        let objects =
            collections::new_array(&[first, second], Policy::from_type_callbacks(), false) as Id;
        assert_eq!(
            unsafe { write_objects(board, objc::sel::get("writeObjects:"), objects) },
            1
        );
        let items = unsafe { pasteboard_items(board, sel) };
        let items = unsafe { collections::array(items as CFTypeRef) }.expect("array");
        let items: Vec<Id> = items.values.iter().map(|item| *item as Id).collect();
        assert_eq!(items.len(), 2);
        assert_eq!(item_string(items[0]).as_deref(), Some(&b"first"[..]));
        assert_eq!(item_string(items[1]).as_deref(), Some(&b"second"[..]));

        unsafe { clear_contents(board, objc::sel::get("clearContents")) };
        assert_eq!(item_string(items[0]), None);
        let types = unsafe { item_types(items[1], objc::sel::get("types")) };
        assert!(
            unsafe { collections::array(types as CFTypeRef) }
                .expect("array")
                .values
                .is_empty()
        );
    }

    /// An item made by the program carries all its flavours onto the board,
    /// and is the board's from then on.
    #[test]
    fn written_items_keep_their_flavours_and_become_read_only() {
        crate::bootstrap();
        let name = corefoundation::string::from_bytes(b"pasteboard-write-items", false) as Id;
        let board = unsafe {
            pasteboard_with_name(
                std::ptr::null_mut(),
                objc::sel::get("pasteboardWithName:"),
                name,
            )
        };
        let text = corefoundation::string::from_bytes(STRING.as_bytes(), false) as Id;
        let html = corefoundation::string::from_bytes(HTML.as_bytes(), false) as Id;
        let item = objc::object::create_instance(item_class(), 0);
        let set_string = objc::sel::get("setString:forType:");
        let value = corefoundation::string::from_bytes(b"plain", false) as Id;
        assert_eq!(unsafe { item_set_string(item, set_string, value, text) }, 1);
        let markup = collections::new_data(b"<b>bold</b>".to_vec()) as Id;
        assert_eq!(
            unsafe { item_set_data(item, objc::sel::get("setData:forType:"), markup, html) },
            1
        );
        let second = corefoundation::string::from_bytes(b"second", false);
        let objects = collections::new_array(
            &[item as CFTypeRef, second],
            Policy::from_type_callbacks(),
            false,
        ) as Id;
        assert_eq!(
            unsafe { write_objects(board, objc::sel::get("writeObjects:"), objects) },
            1
        );

        let read = unsafe { data_for_type(board, objc::sel::get("dataForType:"), html) };
        assert_eq!(
            unsafe { collections::data(read as CFTypeRef) }.expect("data"),
            b"<b>bold</b>"
        );
        let items = unsafe { pasteboard_items(board, objc::sel::get("pasteboardItems")) };
        let items = unsafe { collections::array(items as CFTypeRef) }.expect("array");
        assert_eq!(items.values.len(), 2);
        assert_eq!(
            item_string(items.values[1] as Id).as_deref(),
            Some(&b"second"[..])
        );
        assert_eq!(item_string(item).as_deref(), Some(&b"plain"[..]));
        assert_eq!(unsafe { item_set_string(item, set_string, value, html) }, 0);
        // Already on a board, so it cannot be written again.
        assert_eq!(
            unsafe { write_objects(board, objc::sel::get("writeObjects:"), objects) },
            0
        );
    }
}
