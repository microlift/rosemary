//! `NSStatusBar` and `NSStatusItem`: the menu bar extras, which off macOS live
//! in the desktop's notification area through `sdl3::tray`.
//!
//! A tray icon only ever opens a menu - the desktop does not report a plain
//! click on it. An item with a menu shows that menu's items; an item without
//! one is driven by its button's action, so its tray menu is a single entry
//! that sends that action, which is what clicking the extra does on a Mac.
//!
//! The button lives in a window of its own that is never shown, placed where
//! a Mac's menu bar would put it, because a program positions the panel it
//! opens from `[[button window] convertRectToScreen:[button frame]]`.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

use foundation::geometry::{CGFloat, NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// The menu bar's height on a Mac since Big Sur.
const THICKNESS: CGFloat = 24.0;
/// `NSVariableStatusItemLength` and `NSSquareStatusItemLength`.
const VARIABLE_LENGTH: CGFloat = -1.0;
const SQUARE_LENGTH: CGFloat = -2.0;
/// The largest icon handed to the desktop; trays draw at 16-32 pixels, and a
/// 1024-pixel application icon would be sent over D-Bus as it is.
const ICON_LIMIT: u32 = 64;

#[repr(C)]
struct StatusItem {
    isa: *const c_void,
    button: Id,
    window: Id,
    menu: Id,
    tool_tip: Id,
    length: CGFloat,
    visible: usize,
    /// The `sdl3::tray::Tray`, or 0 while there is none yet.
    tray: usize,
    /// What the tray was last built from; see [`signature`].
    built: u64,
    /// The image the tray's icon was last made from.
    icon_from: usize,
    /// Distinguishes this item's entries in the tray click queue.
    serial: u32,
}

pub const ITEM_INSTANCE_SIZE: usize = size_of::<StatusItem>();

fn item_of<'a>(receiver: Id) -> &'a mut StatusItem {
    // SAFETY: an `NSStatusItem` of this crate's.
    unsafe { &mut *(receiver as *mut StatusItem) }
}

/// Every item the bar holds, in the order made, retained until
/// `-removeStatusItem:`.
static ITEMS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// What each tray entry stands for, by the tag it was made with.
enum Target {
    /// The item's button: send its action.
    Button(usize),
    /// A menu item, retained while it is in here.
    MenuItem(usize),
}

/// Each entry's tag, to its label and what it stands for.
static TARGETS: Mutex<Option<HashMap<u64, (String, Target)>>> = Mutex::new(None);

/// Clicks a debug script made, by tag, delivered with the desktop's.
static SCRIPTED: Mutex<Vec<u64>> = Mutex::new(Vec::new());

fn class(name: &std::ffi::CStr) -> &'static objc::class::Class {
    objc::class::lookup(name).expect("declared before its methods")
}

/// `+[NSStatusBar systemStatusBar]`, the only bar there is.
unsafe extern "C" fn system_status_bar(_class: Id, _sel: Sel) -> Id {
    static BAR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BAR.get_or_init(|| {
        let bar = objc::object::create_instance(class(c"NSStatusBar"), 0);
        objc::arc::make_immortal(bar);
        bar as usize
    }) as Id
}

unsafe extern "C" fn thickness(_receiver: Id, _sel: Sel) -> CGFloat {
    THICKNESS
}

unsafe extern "C" fn is_vertical(_receiver: Id, _sel: Sel) -> u8 {
    0
}

fn send_rect(receiver: Id, selector: &str) -> NSRect {
    if receiver.is_null() {
        return NSRect::default();
    }
    // SAFETY: the selector is `NSRect (id, SEL)`, which returns through a
    // buffer, so through the `_stret` entry.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSRect =
            std::mem::transmute(objc::msg::entry_stret());
        f(receiver, objc::sel::get(selector))
    }
}

fn send_id(receiver: Id, selector: &str) -> Id {
    if receiver.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the selector is `id (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector))
    }
}

fn send_object(receiver: Id, selector: &str, argument: Id) {
    // SAFETY: the selector is `void (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), argument)
    }
}

fn send_frame(receiver: Id, selector: &str, frame: NSRect) {
    // SAFETY: the selector is `void (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), frame)
    }
}

/// How wide an item is on the bar: its length, or for a variable one its
/// image's width with the padding AppKit gives it.
fn width_of(item: &StatusItem) -> CGFloat {
    match item.length {
        SQUARE_LENGTH => THICKNESS,
        l if l == VARIABLE_LENGTH || l <= 0.0 => {
            let image = send_id(item.button, "image");
            match crate::image::declared_size(image).width {
                w if w > 0.0 => w + 8.0,
                _ => THICKNESS,
            }
        }
        l => l,
    }
}

/// Where the items' windows go: right to left from the main screen's top
/// right corner, newest leftmost, as a Mac lays out its extras.
fn lay_out() {
    let items = ITEMS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let screen = send_id(objc::class::ptr(class(c"NSScreen")) as Id, "mainScreen");
    let bounds = send_rect(screen, "frame");
    let mut right = bounds.origin.x + bounds.size.width - THICKNESS * 4.0;
    for item in items {
        let item = item_of(item as Id);
        if item.window.is_null() || item.visible == 0 {
            continue;
        }
        let width = width_of(item);
        right -= width;
        let frame = NSRect {
            origin: NSPoint {
                x: right,
                y: bounds.origin.y + bounds.size.height - THICKNESS,
            },
            size: NSSize {
                width,
                height: THICKNESS,
            },
        };
        // SAFETY: `-setFrame:display:` is `void (id, SEL, NSRect, BOOL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSRect, u8) =
                std::mem::transmute(objc::msg::entry());
            f(item.window, objc::sel::get("setFrame:display:"), frame, 0);
        }
        send_frame(
            item.button,
            "setFrame:",
            NSRect {
                origin: NSPoint::default(),
                size: frame.size,
            },
        );
    }
}

/// `-statusItemWithLength:`: a new item at the left of the others.
unsafe extern "C" fn status_item_with_length(_receiver: Id, _sel: Sel, length: CGFloat) -> Id {
    static SERIAL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let object = objc::object::create_instance(class(c"NSStatusItem"), 0);
    let item = item_of(object);
    item.length = length;
    item.visible = 1;
    item.serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let frame = NSRect {
        origin: NSPoint::default(),
        size: NSSize {
            width: THICKNESS,
            height: THICKNESS,
        },
    };
    let window = objc::object::create_instance(class(c"NSWindow"), 0);
    // SAFETY: `-initWithContentRect:styleMask:backing:defer:` is
    // `id (id, SEL, NSRect, NSUInteger, NSUInteger, BOOL)`; borderless and
    // buffered.
    item.window = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect, usize, usize, u8) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            window,
            objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
            frame,
            0,
            2,
            1,
        )
    };
    let button = objc::object::create_instance(class(c"NSStatusBarButton"), 0);
    // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
    item.button = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(button, objc::sel::get("initWithFrame:"), frame)
    };
    if !item.window.is_null() && !item.button.is_null() {
        send_object(item.window, "setContentView:", item.button);
    }
    ITEMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(object as usize);
    lay_out();
    objc::arc::autorelease(object)
}

/// `-removeStatusItem:`: off the bar and out of the notification area.
unsafe extern "C" fn remove_status_item(_receiver: Id, _sel: Sel, object: Id) {
    let mut items = ITEMS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(at) = items.iter().position(|i| *i == object as usize) else {
        return;
    };
    items.remove(at);
    drop(items);
    take_down(item_of(object));
    objc::arc::release(object);
    lay_out();
}

fn take_down(item: &mut StatusItem) {
    if item.tray != 0 {
        sdl3::tray::destroy(item.tray as sdl3::tray::Tray);
        item.tray = 0;
    }
    forget_targets(item.serial);
}

fn forget_targets(serial: u32) {
    let mut targets = TARGETS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(targets) = targets.as_mut() else {
        return;
    };
    targets.retain(|tag, (_, target)| {
        let mine = (tag >> 32) as u32 == serial;
        if mine && let Target::MenuItem(item) = target {
            objc::arc::release(*item as Id);
        }
        !mine
    });
}

unsafe extern "C" fn status_bar(_receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the class method takes nothing of its receiver.
    unsafe { system_status_bar(std::ptr::null_mut(), _sel) }
}

unsafe extern "C" fn button(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).button
}

unsafe extern "C" fn menu(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).menu
}

unsafe extern "C" fn set_menu(receiver: Id, _sel: Sel, menu: Id) {
    let item = item_of(receiver);
    let old = std::mem::replace(&mut item.menu, objc::arc::retain(menu));
    objc::arc::release(old);
}

unsafe extern "C" fn length(receiver: Id, _sel: Sel) -> CGFloat {
    item_of(receiver).length
}

unsafe extern "C" fn set_length(receiver: Id, _sel: Sel, length: CGFloat) {
    item_of(receiver).length = length;
    lay_out();
}

unsafe extern "C" fn is_visible(receiver: Id, _sel: Sel) -> u8 {
    (item_of(receiver).visible != 0) as u8
}

unsafe extern "C" fn set_visible(receiver: Id, _sel: Sel, visible: u8) {
    let item = item_of(receiver);
    item.visible = (visible != 0) as usize;
    if visible == 0 {
        take_down(item);
    }
    lay_out();
}

unsafe extern "C" fn tool_tip(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).tool_tip
}

unsafe extern "C" fn set_tool_tip(receiver: Id, _sel: Sel, text: Id) {
    let item = item_of(receiver);
    let copy = match text.is_null() {
        true => text,
        false => foundation::string::from_str(&string_of(text)),
    };
    let old = std::mem::replace(&mut item.tool_tip, copy);
    objc::arc::release(old);
    // The tooltip is part of what the tray was built from.
    item.built = 0;
}

/// The pre-10.10 spellings, which are the button's properties now.
fn selector_name(sel: Sel) -> String {
    // SAFETY: a registered selector's name is a NUL-terminated string that
    // lives as long as the selector.
    unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) }
        .to_string_lossy()
        .into_owned()
}

unsafe extern "C" fn forward_object(receiver: Id, sel: Sel, value: Id) {
    send_object(item_of(receiver).button, &selector_name(sel), value);
}

unsafe extern "C" fn forward_get(receiver: Id, sel: Sel) -> Id {
    send_id(item_of(receiver).button, &selector_name(sel))
}

unsafe extern "C" fn set_action(receiver: Id, _sel: Sel, action: Sel) {
    // SAFETY: `-setAction:` is `void (id, SEL, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Sel) = std::mem::transmute(objc::msg::entry());
        f(
            item_of(receiver).button,
            objc::sel::get("setAction:"),
            action,
        )
    }
}

unsafe extern "C" fn action(receiver: Id, _sel: Sel) -> Sel {
    let button = item_of(receiver).button;
    if button.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `-action` is `SEL (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Sel = std::mem::transmute(objc::msg::entry());
        f(button, objc::sel::get("action"))
    }
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let item = item_of(receiver);
    take_down(item);
    for object in [item.button, item.window, item.menu, item.tool_tip] {
        objc::arc::release(object);
    }
    objc::arc::finalize(receiver);
}

/// `-[NSStatusBarButton appearsDisabled]`, drawn the same either way here.
unsafe extern "C" fn appears_disabled(_receiver: Id, _sel: Sel) -> u8 {
    0
}

unsafe extern "C" fn set_appears_disabled(_receiver: Id, _sel: Sel, _value: u8) {}

fn string_of(object: Id) -> String {
    match object.is_null() {
        true => String::new(),
        // SAFETY: an `NSString`.
        false => {
            String::from_utf8_lossy(&unsafe { foundation::string::bytes(object) }).into_owned()
        }
    }
}

/// The tray's label for an item driven by its button: what the button says,
/// else its tooltip, else the application's name.
fn button_label(item: &StatusItem) -> String {
    let title = string_of(send_id(item.button, "title"));
    if !title.is_empty() {
        return title;
    }
    let tip = string_of(item.tool_tip);
    if !tip.is_empty() {
        return tip;
    }
    foundation::bundle::main()
        .and_then(|b| b.display_name().map(str::to_owned))
        .unwrap_or_else(|| "Open".to_owned())
}

/// A summary of everything a tray is built from, to rebuild it only when one
/// of those changes.
fn signature(item: &StatusItem) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    item.menu.hash(&mut hash);
    if item.menu.is_null() {
        button_label(item).hash(&mut hash);
    } else {
        menu_signature(item.menu, &mut hash, 0);
    }
    hash.finish() | 1
}

fn menu_signature(menu: Id, hash: &mut impl std::hash::Hasher, depth: usize) {
    use std::hash::Hash;
    if depth > 8 {
        return;
    }
    for entry in crate::menu::shown_items(menu) {
        crate::menu::display_title(entry).hash(hash);
        crate::menu::is_separator(entry).hash(hash);
        crate::menu::state_of(entry).hash(hash);
        crate::menu::is_item_enabled(entry).hash(hash);
        let submenu = crate::menu::submenu_of(entry);
        if !submenu.is_null() {
            menu_signature(submenu, hash, depth + 1);
        }
    }
}

/// An image's pixels, no larger than the tray needs.
fn icon_pixels(image: Id) -> Option<coregraphics::raster::Bitmap> {
    let bitmap = match image.is_null() {
        false => crate::image::bitmap(image),
        // An extra with no image shows its title on a Mac; a tray has no
        // room for text, and the application's icon is what names it.
        true => None,
    }
    .or_else(crate::icon::current)?;
    Some(shrink(bitmap, ICON_LIMIT))
}

/// Box-filter `bitmap` down until it fits in `limit` pixels.
fn shrink(bitmap: coregraphics::raster::Bitmap, limit: u32) -> coregraphics::raster::Bitmap {
    let mut b = bitmap;
    while b.width > limit && b.height > limit && b.width % 2 == 0 && b.height % 2 == 0 {
        let (w, h) = (b.width / 2, b.height / 2);
        let mut out = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                for c in 0..4 {
                    let at = |dx: u32, dy: u32| {
                        b.pixels[(((y * 2 + dy) * b.width + x * 2 + dx) * 4 + c) as usize] as u32
                    };
                    out[((y * w + x) * 4 + c) as usize] =
                        ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4) as u8;
                }
            }
        }
        b = coregraphics::raster::Bitmap {
            width: w,
            height: h,
            pixels: out,
        };
    }
    b
}

/// Fill `menu` from an `NSMenu`, recording what each entry stands for.
fn mirror(
    into: sdl3::tray::Menu,
    menu: Id,
    serial: u32,
    next: &mut u32,
    targets: &mut HashMap<u64, (String, Target)>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    for entry in crate::menu::prepare(menu) {
        if crate::menu::is_separator(entry) {
            sdl3::tray::append(into, None, sdl3::tray::flags::BUTTON, 0);
            objc::arc::release(entry);
            continue;
        }
        let title = crate::menu::display_title(entry);
        let submenu = crate::menu::submenu_of(entry);
        let mut flags = match (submenu.is_null(), crate::menu::state_of(entry)) {
            (false, _) => sdl3::tray::flags::SUBMENU,
            (true, 0) => sdl3::tray::flags::BUTTON,
            (true, 1) => sdl3::tray::flags::CHECKBOX | sdl3::tray::flags::CHECKED,
            // Mixed has no tray spelling; unchecked is the honest one.
            (true, _) => sdl3::tray::flags::CHECKBOX,
        };
        if !crate::menu::is_item_enabled(entry) {
            flags |= sdl3::tray::flags::DISABLED;
        }
        *next += 1;
        let tag = ((serial as u64) << 32) | *next as u64;
        let made = sdl3::tray::append(into, Some(&title), flags, tag);
        if submenu.is_null() {
            // The entry's reference moves into the table.
            targets.insert(tag, (title, Target::MenuItem(entry as usize)));
        } else {
            mirror(
                sdl3::tray::submenu(made),
                submenu,
                serial,
                next,
                targets,
                depth + 1,
            );
            objc::arc::release(entry);
        }
    }
}

/// Say once why an item has no tray icon: the desktop has no notification
/// area SDL can reach, which leaves the item unreachable.
fn say_no_tray() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        let why = match sdl3::tray::available() {
            true => sdl3::error(),
            false => "this SDL has no tray support".to_owned(),
        };
        eprintln!("rosemary: appkit: NSStatusItem: no notification area to show it in: {why}");
    });
}

/// Bring one item's tray up to date with the item.
fn refresh(object: Id) {
    let item = item_of(object);
    if item.visible == 0 {
        return;
    }
    let image = send_id(item.button, "image");
    if item.tray == 0 {
        let icon = icon_pixels(image);
        let tip = string_of(item.tool_tip);
        let tray = sdl3::tray::create(
            icon.as_ref()
                .map(|b| (b.width, b.height, b.pixels.as_slice())),
            &tip,
        );
        let Some(tray) = tray else {
            say_no_tray();
            return;
        };
        item.tray = tray as usize;
        item.icon_from = image as usize;
        item.built = 0;
    }
    let tray = item.tray as sdl3::tray::Tray;
    if item.icon_from != image as usize {
        let icon = icon_pixels(image);
        sdl3::tray::set_icon(
            tray,
            icon.as_ref()
                .map(|b| (b.width, b.height, b.pixels.as_slice())),
        );
        item.icon_from = image as usize;
    }
    let signature = signature(item);
    if signature == item.built {
        return;
    }
    item.built = signature;
    sdl3::tray::set_tooltip(tray, &string_of(item.tool_tip));
    forget_targets(item.serial);
    let into = sdl3::tray::menu(tray);
    sdl3::tray::clear(into);
    let mut targets = HashMap::new();
    if item.menu.is_null() {
        let tag = (item.serial as u64) << 32;
        let label = button_label(item);
        sdl3::tray::append(into, Some(&label), sdl3::tray::flags::BUTTON, tag);
        targets.insert(tag, (label, Target::Button(object as usize)));
    } else {
        let mut next = 0;
        mirror(into, item.menu, item.serial, &mut next, &mut targets, 0);
    }
    TARGETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_default()
        .extend(targets);
}

/// The event pump's turn: deliver the clicks the desktop reported, then bring
/// every tray up to date.
fn pump() {
    let scripted = std::mem::take(&mut *SCRIPTED.lock().unwrap_or_else(|e| e.into_inner()));
    for tag in sdl3::tray::clicked().into_iter().chain(scripted) {
        let target = {
            let targets = TARGETS.lock().unwrap_or_else(|e| e.into_inner());
            match targets.as_ref().and_then(|t| t.get(&tag)) {
                Some((_, Target::Button(item))) => Some(Target::Button(*item)),
                Some((_, Target::MenuItem(entry))) => Some(Target::MenuItem(*entry)),
                None => None,
            }
        };
        match target {
            Some(Target::Button(item)) => {
                let button = item_of(item as Id).button;
                if !button.is_null() {
                    crate::controls::send_action(button);
                }
            }
            Some(Target::MenuItem(entry)) if crate::menu::is_item_enabled(entry as Id) => {
                crate::menu::perform(entry as Id);
            }
            _ => {}
        }
    }
    let items = ITEMS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for item in items {
        refresh(item as Id);
    }
}

/// Click the tray entry labelled `label`, as the desktop would report it; for
/// `crate::script`. False when no tray has one.
pub(crate) fn click(label: &str) -> bool {
    let targets = TARGETS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(tag) = targets
        .as_ref()
        .and_then(|t| t.iter().find(|(_, (l, _))| l == label).map(|(tag, _)| *tag))
    else {
        return false;
    };
    SCRIPTED.lock().unwrap_or_else(|e| e.into_inner()).push(tag);
    true
}

pub(crate) fn install() {
    let bar = class(c"NSStatusBar");
    class_methods!(bar, {
        "systemStatusBar" => system_status_bar, c"@16@0:8";
    });
    methods!(bar, {
        "statusItemWithLength:" => status_item_with_length, c"@24@0:8d16";
        "removeStatusItem:" => remove_status_item, c"v24@0:8@16";
        "thickness" => thickness, c"d16@0:8";
        "isVertical" => is_vertical, c"c16@0:8";
    });
    let item = class(c"NSStatusItem");
    methods!(item, {
        "statusBar" => status_bar, c"@16@0:8";
        "button" => button, c"@16@0:8";
        "menu" => menu, c"@16@0:8";
        "setMenu:" => set_menu, c"v24@0:8@16";
        "length" => length, c"d16@0:8";
        "setLength:" => set_length, c"v24@0:8d16";
        "isVisible" => is_visible, c"c16@0:8";
        "setVisible:" => set_visible, c"v24@0:8c16";
        "toolTip" => tool_tip, c"@16@0:8";
        "setToolTip:" => set_tool_tip, c"v24@0:8@16";
        "image" => forward_get, c"@16@0:8";
        "setImage:" => forward_object, c"v24@0:8@16";
        "title" => forward_get, c"@16@0:8";
        "setTitle:" => forward_object, c"v24@0:8@16";
        "target" => forward_get, c"@16@0:8";
        "setTarget:" => forward_object, c"v24@0:8@16";
        "action" => action, c":16@0:8";
        "setAction:" => set_action, c"v24@0:8:16";
        "dealloc" => dealloc, c"v16@0:8";
    });
    let button = class(c"NSStatusBarButton");
    methods!(button, {
        "appearsDisabled" => appears_disabled, c"c16@0:8";
        "setAppearsDisabled:" => set_appears_disabled, c"v24@0:8c16";
    });
    crate::event::add_pump_hook(pump);
}
