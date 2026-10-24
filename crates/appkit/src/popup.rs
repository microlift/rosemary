//! `NSPopUpButton`: a button showing one item of its menu, which opens the
//! menu when pressed.
//!
//! The menu is an ordinary `NSMenu`, opened by the chrome's menu tracker
//! (see [`crate::decor::pop_up`]); choosing an item selects it here and sends
//! the item's action, or the button's when the item has none, as AppKit does.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::view;

#[derive(Clone, Copy)]
struct Popup {
    /// Retained.
    menu: usize,
    /// -1 for none.
    selected: isize,
    pulls_down: bool,
}

fn popups() -> &'static Mutex<HashMap<usize, Popup>> {
    static POPUPS: OnceLock<Mutex<HashMap<usize, Popup>>> = OnceLock::new();
    POPUPS.get_or_init(Default::default)
}

/// The button's state, made with an empty menu on first use.
fn state(button: Id) -> Popup {
    if let Some(popup) = popups()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(button as usize))
    {
        return *popup;
    }
    let menu = new_menu();
    let popup = Popup {
        menu: menu as usize,
        selected: -1,
        pulls_down: false,
    };
    popups()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(button as usize, popup);
    popup
}

fn update(button: Id, change: impl FnOnce(&mut Popup)) {
    state(button);
    if let Some(popup) = popups()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&(button as usize))
    {
        change(popup);
    }
    crate::backing::needs_display(button);
}

fn new_menu() -> Id {
    let class = objc::class::lookup(c"NSMenu").expect("declared before its methods");
    let menu = objc::object::create_instance(class, 0);
    let title = foundation::string::from_str("");
    // SAFETY: `-initWithTitle:` is `id (id, SEL, id)` on a menu.
    let menu = unsafe { send_id(menu, "initWithTitle:", title) };
    objc::arc::release(title);
    menu
}

/// Whether `menu` is a pop-up button's, whose items are enabled without an
/// action: the button is what answers for them.
pub(crate) fn owns_menu(menu: Id) -> bool {
    popups()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .any(|popup| popup.menu == menu as usize)
}

/// Drop the state of a button that is going away.
pub(crate) fn forget(button: Id) {
    let gone = popups()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(button as usize));
    if let Some(popup) = gone {
        objc::arc::release(popup.menu as Id);
    }
}

/// `id (id, SEL, id)`.
unsafe fn send_id(receiver: Id, selector: &str, argument: Id) -> Id {
    // SAFETY: the caller names a method of this prototype.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), argument)
    }
}

fn send_get(receiver: Id, selector: &str) -> Id {
    // SAFETY: every selector passed here is a getter answering an object.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector))
    }
}

fn menu_of(button: Id) -> Id {
    state(button).menu as Id
}

fn items(button: Id) -> Vec<Id> {
    let array = send_get(menu_of(button), "itemArray");
    // SAFETY: an array of items, or nil.
    unsafe { corefoundation::collections::array(array as _) }
        .map(|a| a.values.iter().map(|v| *v as Id).collect())
        .unwrap_or_default()
}

fn title_of(item: Id) -> String {
    if item.is_null() {
        return String::new();
    }
    // SAFETY: a string, or nil.
    String::from_utf8_lossy(&unsafe { foundation::string::bytes(send_get(item, "title")) })
        .into_owned()
}

fn selected_item(button: Id) -> Id {
    let popup = state(button);
    usize::try_from(popup.selected)
        .ok()
        .and_then(|at| items(button).get(at).copied())
        .unwrap_or(std::ptr::null_mut())
}

/// Keep the selection pointing at an item, as AppKit's does after a change:
/// the first item when there was none and one was added.
fn settle_selection(button: Id) {
    let count = items(button).len() as isize;
    update(button, |popup| {
        if popup.selected >= count {
            popup.selected = count - 1;
        }
        if popup.selected < 0 && count > 0 {
            popup.selected = 0;
        }
    });
}

unsafe extern "C" fn init_with_frame_pulls_down(
    receiver: Id,
    _sel: Sel,
    frame: NSRect,
    pulls_down: u8,
) -> Id {
    // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)` on every view.
    let button = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("initWithFrame:"), frame)
    };
    update(button, |popup| popup.pulls_down = pulls_down != 0);
    button
}

unsafe extern "C" fn set_pulls_down(receiver: Id, _sel: Sel, on: u8) {
    update(receiver, |popup| popup.pulls_down = on != 0);
}

unsafe extern "C" fn pulls_down(receiver: Id, _sel: Sel) -> u8 {
    u8::from(state(receiver).pulls_down)
}

unsafe extern "C" fn menu(receiver: Id, _sel: Sel) -> Id {
    menu_of(receiver)
}

unsafe extern "C" fn set_menu(receiver: Id, _sel: Sel, menu: Id) {
    let menu = match menu.is_null() {
        true => new_menu(),
        false => objc::arc::retain(menu),
    };
    let mut old = 0;
    update(receiver, |popup| {
        old = std::mem::replace(&mut popup.menu, menu as usize);
        popup.selected = -1;
    });
    objc::arc::release(old as Id);
    settle_selection(receiver);
}

unsafe extern "C" fn add_item_with_title(receiver: Id, sel: Sel, title: Id) {
    let count = items(receiver).len() as isize;
    // SAFETY: the same button.
    unsafe { insert_item_with_title(receiver, sel, title, count) };
}

unsafe extern "C" fn add_items_with_titles(receiver: Id, sel: Sel, titles: Id) {
    // SAFETY: an array of strings, or nil.
    let titles = unsafe { corefoundation::collections::array(titles as _) }
        .map(|a| a.values.clone())
        .unwrap_or_default();
    for title in titles {
        // SAFETY: the same button, and a string.
        unsafe { add_item_with_title(receiver, sel, title as Id) };
    }
}

/// `-insertItemWithTitle:atIndex:`. AppKit removes an item of the same title
/// first: a pop-up's titles are unique.
unsafe extern "C" fn insert_item_with_title(receiver: Id, _sel: Sel, title: Id, index: isize) {
    let text = title_of_string(title);
    if let Some(existing) = items(receiver)
        .iter()
        .position(|item| title_of(*item) == text)
    {
        // SAFETY: the same button, at an index it has.
        unsafe {
            remove_item_at_index(
                receiver,
                objc::sel::get("removeItemAtIndex:"),
                existing as isize,
            )
        };
    }
    let index = index.clamp(0, items(receiver).len() as isize);
    let empty = foundation::string::from_str("");
    let title = if title.is_null() { empty } else { title };
    // SAFETY: the menu's own insert, `id (id, SEL, id, SEL, id, NSInteger)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Sel, Id, isize) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            menu_of(receiver),
            objc::sel::get("insertItemWithTitle:action:keyEquivalent:atIndex:"),
            title,
            std::ptr::null(),
            empty,
            index,
        );
    }
    objc::arc::release(empty);
    let selected = state(receiver).selected;
    if selected >= index && selected >= 0 {
        update(receiver, |popup| popup.selected += 1);
    }
    settle_selection(receiver);
}

fn title_of_string(title: Id) -> String {
    // SAFETY: a string, or nil.
    String::from_utf8_lossy(&unsafe { foundation::string::bytes(title) }).into_owned()
}

unsafe extern "C" fn remove_all_items(receiver: Id, _sel: Sel) {
    let menu = menu_of(receiver);
    // SAFETY: `-removeAllItems` is `void (id, SEL)` on a menu.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        f(menu, objc::sel::get("removeAllItems"));
    }
    update(receiver, |popup| popup.selected = -1);
}

unsafe extern "C" fn remove_item_at_index(receiver: Id, _sel: Sel, index: isize) {
    if index < 0 || index as usize >= items(receiver).len() {
        return;
    }
    // SAFETY: `-removeItemAtIndex:` is `void (id, SEL, NSInteger)` on a menu.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, isize) = std::mem::transmute(objc::msg::entry());
        f(
            menu_of(receiver),
            objc::sel::get("removeItemAtIndex:"),
            index,
        );
    }
    update(receiver, |popup| {
        if popup.selected > index || (popup.selected == index && index > 0) {
            popup.selected -= 1;
        }
    });
    settle_selection(receiver);
}

unsafe extern "C" fn remove_item_with_title(receiver: Id, sel: Sel, title: Id) {
    let text = title_of_string(title);
    if let Some(at) = items(receiver)
        .iter()
        .position(|item| title_of(*item) == text)
    {
        // SAFETY: the same button, at an index it has.
        unsafe { remove_item_at_index(receiver, sel, at as isize) };
    }
}

unsafe extern "C" fn item_array(receiver: Id, _sel: Sel) -> Id {
    send_get(menu_of(receiver), "itemArray")
}

unsafe extern "C" fn number_of_items(receiver: Id, _sel: Sel) -> isize {
    items(receiver).len() as isize
}

unsafe extern "C" fn item_at_index(receiver: Id, _sel: Sel, index: isize) -> Id {
    usize::try_from(index)
        .ok()
        .and_then(|at| items(receiver).get(at).copied())
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn item_title_at_index(receiver: Id, sel: Sel, index: isize) -> Id {
    // SAFETY: the same button.
    let item = unsafe { item_at_index(receiver, sel, index) };
    autoreleased(&title_of(item))
}

unsafe extern "C" fn item_titles(receiver: Id, _sel: Sel) -> Id {
    let titles: Vec<Id> = items(receiver)
        .iter()
        .map(|item| foundation::string::from_str(&title_of(*item)))
        .collect();
    let values: Vec<*const std::ffi::c_void> = titles.iter().map(|t| *t as *const _).collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    titles.into_iter().for_each(objc::arc::release);
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn last_item(receiver: Id, _sel: Sel) -> Id {
    items(receiver)
        .last()
        .copied()
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn item_with_title(receiver: Id, _sel: Sel, title: Id) -> Id {
    let text = title_of_string(title);
    items(receiver)
        .into_iter()
        .find(|item| title_of(*item) == text)
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn index_of_item_with_title(receiver: Id, _sel: Sel, title: Id) -> isize {
    let text = title_of_string(title);
    items(receiver)
        .iter()
        .position(|item| title_of(*item) == text)
        .map_or(-1, |at| at as isize)
}

unsafe extern "C" fn index_of_item(receiver: Id, _sel: Sel, item: Id) -> isize {
    items(receiver)
        .iter()
        .position(|candidate| *candidate == item)
        .map_or(-1, |at| at as isize)
}

fn tag_of(item: Id) -> isize {
    // SAFETY: `-tag` is `NSInteger (id, SEL)` on a menu item.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
        f(item, objc::sel::get("tag"))
    }
}

unsafe extern "C" fn index_of_item_with_tag(receiver: Id, _sel: Sel, tag: isize) -> isize {
    items(receiver)
        .iter()
        .position(|item| tag_of(*item) == tag)
        .map_or(-1, |at| at as isize)
}

unsafe extern "C" fn select_item_at_index(receiver: Id, _sel: Sel, index: isize) {
    let count = items(receiver).len() as isize;
    update(receiver, |popup| {
        popup.selected = if (0..count).contains(&index) {
            index
        } else {
            -1
        };
    });
}

unsafe extern "C" fn select_item(receiver: Id, sel: Sel, item: Id) {
    // SAFETY: the same button.
    let index = unsafe { index_of_item(receiver, sel, item) };
    // SAFETY: as above.
    unsafe { select_item_at_index(receiver, sel, index) };
}

/// `-selectItemWithTitle:`: nothing selected when no item has that title.
unsafe extern "C" fn select_item_with_title(receiver: Id, sel: Sel, title: Id) {
    // SAFETY: the same button.
    let index = unsafe { index_of_item_with_title(receiver, sel, title) };
    // SAFETY: as above.
    unsafe { select_item_at_index(receiver, sel, index) };
}

unsafe extern "C" fn select_item_with_tag(receiver: Id, sel: Sel, tag: isize) -> u8 {
    // SAFETY: the same button.
    let index = unsafe { index_of_item_with_tag(receiver, sel, tag) };
    if index < 0 {
        return 0;
    }
    // SAFETY: as above.
    unsafe { select_item_at_index(receiver, sel, index) };
    1
}

unsafe extern "C" fn selected_item_method(receiver: Id, _sel: Sel) -> Id {
    selected_item(receiver)
}

unsafe extern "C" fn index_of_selected_item(receiver: Id, _sel: Sel) -> isize {
    state(receiver).selected
}

unsafe extern "C" fn title_of_selected_item(receiver: Id, _sel: Sel) -> Id {
    let item = selected_item(receiver);
    match item.is_null() {
        true => std::ptr::null_mut(),
        false => autoreleased(&title_of(item)),
    }
}

unsafe extern "C" fn selected_tag(receiver: Id, _sel: Sel) -> isize {
    let item = selected_item(receiver);
    match item.is_null() {
        true => -1,
        false => tag_of(item),
    }
}

/// `-title`: the selected item's, or the first item's for a pull-down, whose
/// first item is its title.
unsafe extern "C" fn title(receiver: Id, _sel: Sel) -> Id {
    let item = match state(receiver).pulls_down {
        true => items(receiver)
            .first()
            .copied()
            .unwrap_or(std::ptr::null_mut()),
        false => selected_item(receiver),
    };
    autoreleased(&title_of(item))
}

/// `-setTitle:`: selects the item of that title, adding one when there is
/// none, as AppKit does for a pop-up that is not a pull-down.
unsafe extern "C" fn set_title(receiver: Id, sel: Sel, title: Id) {
    // SAFETY: the same button.
    if unsafe { index_of_item_with_title(receiver, sel, title) } < 0 {
        // SAFETY: as above.
        unsafe { add_item_with_title(receiver, sel, title) };
    }
    // SAFETY: as above.
    unsafe { select_item_with_title(receiver, sel, title) };
}

unsafe extern "C" fn set_autoenables_items(receiver: Id, _sel: Sel, on: u8) {
    // SAFETY: `-setAutoenablesItems:` is `void (id, SEL, BOOL)` on a menu.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
        f(
            menu_of(receiver),
            objc::sel::get("setAutoenablesItems:"),
            on,
        );
    }
}

unsafe extern "C" fn autoenables_items(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: `-autoenablesItems` is `BOOL (id, SEL)` on a menu.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(menu_of(receiver), objc::sel::get("autoenablesItems"))
    }
}

unsafe extern "C" fn synchronize_title(receiver: Id, _sel: Sel) {
    crate::backing::needs_display(receiver);
}

fn autoreleased(text: &str) -> Id {
    objc::arc::autorelease(foundation::string::from_str(text))
}

/// `-mouseDown:`: the menu, under the button, with the selected item marked.
unsafe extern "C" fn mouse_down(receiver: Id, _sel: Sel, _event: Id) {
    if !crate::controls::is_enabled(receiver) {
        return;
    }
    // SAFETY: a view of this crate's.
    let window = unsafe { view::window_of(receiver) };
    // SAFETY: as above.
    let Some(frame) = (unsafe { view::presentation_frame(receiver) }) else {
        return;
    };
    let density = f64::from(frame.window_width)
        / f64::from(sdl3::video::window_size(frame.window).map_or(1, |(w, _)| w.max(1)));
    let density = if density > 0.0 { density } else { 1.0 };
    let x = (f64::from(frame.x) / density) as f32;
    let below = ((f64::from(frame.y) + f64::from(frame.height)) / density) as f32;
    let popup = state(receiver);
    // The selected item checked, as a pop-up's menu shows it.
    for (at, item) in items(receiver).into_iter().enumerate() {
        let on = !popup.pulls_down && at as isize == popup.selected;
        // SAFETY: `-setState:` is `void (id, SEL, NSInteger)` on an item.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, isize) = std::mem::transmute(objc::msg::entry());
            f(item, objc::sel::get("setState:"), isize::from(on));
        }
    }
    crate::decor::pop_up(window, popup.menu as Id, x, below, receiver);
}

/// An item of `button`'s menu was chosen in the tracker: select it, then send
/// the item's action, or the button's when the item has none.
pub(crate) fn chose(button: Id, item: Id) {
    // SAFETY: the button the tracker was opened for, and an item of its menu.
    let index = unsafe { index_of_item(button, objc::sel::get("indexOfItem:"), item) };
    if !state(button).pulls_down {
        update(button, |popup| popup.selected = index);
    }
    // SAFETY: `-action` is `SEL (id, SEL)` on an item.
    let has_action = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Sel = std::mem::transmute(objc::msg::entry());
        !f(item, objc::sel::get("action")).is_null()
    };
    match has_action {
        true => crate::menu::perform(item),
        // SAFETY: `-performClick:` is `void (id, SEL, id)` on a button.
        false => crate::controls::send_action(button),
    }
}

/// `-intrinsicContentSize`: wide enough for the widest item's title and the
/// arrows, as tall as AppKit's pop-up bezel at the control's size.
unsafe extern "C" fn intrinsic_content_size(receiver: Id, _sel: Sel) -> NSSize {
    let responds = |sel: &str| {
        // SAFETY: a live object.
        unsafe { objc::object::class_of(receiver) }
            .is_some_and(|c| c.method(objc::sel::get(sel)).is_some())
    };
    let control_size = match responds("controlSize") {
        // SAFETY: `NSControlSize (id, SEL)`.
        true => unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get("controlSize"))
        },
        false => 0,
    };
    // `NSControlSizeRegular`, `Small` and `Mini`.
    let height = match control_size {
        1 => 17.0,
        2 => 14.0,
        _ => 20.0,
    };
    let Some((font, size)) = crate::label::chrome_font(send_get(receiver, "font")) else {
        return NSSize {
            width: -1.0,
            height,
        };
    };
    let titles: Vec<String> = match state(receiver).pulls_down {
        true => items(receiver)
            .first()
            .map(|i| title_of(*i))
            .into_iter()
            .collect(),
        false => items(receiver).iter().map(|i| title_of(*i)).collect(),
    };
    let widest = titles
        .iter()
        .map(|t| font.measure(t, size as f32))
        .fold(0.0f32, f32::max);
    // The title's inset, the arrows, and the gap before them; see `draw_rect`.
    NSSize {
        width: (f64::from(widest) + 8.0 + 16.0 + 12.0).ceil(),
        height,
    }
}

/// The bezel, the selected item's title, and the arrows that say this button
/// opens a menu.
unsafe extern "C" fn draw_rect(receiver: Id, _sel: Sel, _dirty: NSRect) {
    let bounds = view::bounds_of(receiver);
    let density = crate::backing::focus_density();
    let width = (bounds.size.width * density).ceil() as usize;
    let height = (bounds.size.height * density).ceil() as usize;
    if width == 0 || height == 0 {
        return;
    }
    let mut pixels = vec![0u8; width * height * 4];
    let mut canvas = chrome::Canvas::new(&mut pixels, width, height, width * 4, density as f32);
    let (w, h) = (bounds.size.width as f32, bounds.size.height as f32);
    // A borderless pop-up is its title and arrows on whatever is behind it,
    // in the appearance's label colour.
    // SAFETY: `-isBordered` is `BOOL (id, SEL)`.
    let bordered = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("isBordered")) != 0
    };
    if bordered {
        canvas.fill_rounded_rect(
            chrome::Rect::new(1.0, 1.0, w - 2.0, h - 3.0),
            4.0,
            chrome::Color::rgba(255, 255, 255, 1.0),
        );
        canvas.stroke_rounded_rect(
            chrome::Rect::new(1.5, 1.5, w - 3.0, h - 4.0),
            4.0,
            1.0,
            chrome::Color::rgba(0, 0, 0, 0.3),
        );
    }
    let ink = match !bordered && crate::appearance::is_dark() {
        true => 255,
        false => 0,
    };
    let enabled = crate::controls::is_enabled(receiver);
    let alpha = if enabled { 0.85 } else { 0.35 };
    let font: Option<(Arc<chrome::Font>, f64)> =
        crate::label::chrome_font(send_get(receiver, "font"));
    if let Some((font, size)) = font {
        let size = size as f32;
        let arrows = 16.0;
        let title = title_of(match state(receiver).pulls_down {
            true => items(receiver)
                .first()
                .copied()
                .unwrap_or(std::ptr::null_mut()),
            false => selected_item(receiver),
        });
        let line = font.truncate(&title, size, (w - 12.0 - arrows).max(0.0));
        let baseline = font.centered_baseline(size, 0.0, h - 1.0);
        let color = chrome::Color::rgba(ink, ink, ink, alpha);
        canvas.draw_text(&line, &font, size, chrome::Point::new(8.0, baseline), color);
        let glyph = if state(receiver).pulls_down {
            "\u{25BE}"
        } else {
            "\u{2195}"
        };
        let advance = font.measure(glyph, size);
        canvas.draw_text(
            glyph,
            &font,
            size,
            chrome::Point::new(w - 6.0 - advance, baseline),
            color,
        );
    }
    crate::backing::draw_image(
        &pixels,
        width,
        height,
        bounds.size,
        bounds,
        NSRect {
            origin: NSPoint::default(),
            size: bounds.size,
        },
        // Source-over.
        2,
        1.0,
        true,
    );
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSPopUpButton").expect("declared in crate::classes");
    methods!(class, {
        "initWithFrame:pullsDown:" => init_with_frame_pulls_down,
            c"@52@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16c48";
        "setPullsDown:" => set_pulls_down, c"v20@0:8c16";
        "pullsDown" => pulls_down, c"c16@0:8";
        "menu" => menu, c"@16@0:8";
        "setMenu:" => set_menu, c"v24@0:8@16";
        "addItemWithTitle:" => add_item_with_title, c"v24@0:8@16";
        "addItemsWithTitles:" => add_items_with_titles, c"v24@0:8@16";
        "insertItemWithTitle:atIndex:" => insert_item_with_title, c"v32@0:8@16q24";
        "removeAllItems" => remove_all_items, c"v16@0:8";
        "removeItemAtIndex:" => remove_item_at_index, c"v24@0:8q16";
        "removeItemWithTitle:" => remove_item_with_title, c"v24@0:8@16";
        "itemArray" => item_array, c"@16@0:8";
        "numberOfItems" => number_of_items, c"q16@0:8";
        "itemAtIndex:" => item_at_index, c"@24@0:8q16";
        "itemTitleAtIndex:" => item_title_at_index, c"@24@0:8q16";
        "itemTitles" => item_titles, c"@16@0:8";
        "lastItem" => last_item, c"@16@0:8";
        "itemWithTitle:" => item_with_title, c"@24@0:8@16";
        "indexOfItemWithTitle:" => index_of_item_with_title, c"q24@0:8@16";
        "indexOfItem:" => index_of_item, c"q24@0:8@16";
        "indexOfItemWithTag:" => index_of_item_with_tag, c"q24@0:8q16";
        "selectItem:" => select_item, c"v24@0:8@16";
        "selectItemAtIndex:" => select_item_at_index, c"v24@0:8q16";
        "selectItemWithTitle:" => select_item_with_title, c"v24@0:8@16";
        "selectItemWithTag:" => select_item_with_tag, c"c24@0:8q16";
        "selectedItem" => selected_item_method, c"@16@0:8";
        "indexOfSelectedItem" => index_of_selected_item, c"q16@0:8";
        "titleOfSelectedItem" => title_of_selected_item, c"@16@0:8";
        "selectedTag" => selected_tag, c"q16@0:8";
        "title" => title, c"@16@0:8";
        "setTitle:" => set_title, c"v24@0:8@16";
        "stringValue" => title_of_selected_item, c"@16@0:8";
        "setAutoenablesItems:" => set_autoenables_items, c"v20@0:8c16";
        "autoenablesItems" => autoenables_items, c"c16@0:8";
        "synchronizeTitleAndSelectedItem" => synchronize_title, c"v16@0:8";
        "mouseDown:" => mouse_down, c"v24@0:8@16";
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "intrinsicContentSize" => intrinsic_content_size, c"{CGSize=dd}16@0:8";
    });
}
