use std::ffi::c_void;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Menu {
    isa: *const c_void,
    title: Id,
    /// A mutable `NSArray` of items, made on first use.
    items: Id,
    /// Unretained, as every AppKit delegate is.
    delegate: Id,
    /// The item this menu is the submenu of. `-supermenu` answers that item's
    /// menu rather than the item, so it follows the item when it moves.
    supermenu: Id,
    /// `-autoenablesItems`, inverted so that a fresh menu's zero is AppKit's
    /// `YES`.
    no_autoenable: usize,
}

#[repr(C)]
pub struct MenuItem {
    isa: *const c_void,
    title: Id,
    submenu: Id,
    /// Unretained: a target owns the item, not the other way round.
    target: Id,
    key_equivalent: Id,
    menu: Id,
    action: Sel,
    tag: isize,
    state: isize,
    enabled: usize,
    hidden: usize,
    /// Set only by `+separatorItem`. A separator is an ordinary item as far as
    /// this bookkeeping goes; what makes it one is that it answers
    /// `-isSeparatorItem`, which is how a caller walking a menu tells a divider
    /// from something it should read a title off.
    separator: usize,
    /// Kept rather than ignored, because a caller that sets a key equivalent's
    /// modifiers reads them back to build its own shortcut table.
    modifier_mask: usize,
    tool_tip: Id,
    image: Id,
    /// A custom view for the item. Kept, and not drawn: the item's title is
    /// shown in its place.
    view: Id,
    /// `-isAlternate`: shown only while its extra modifier is held, which a
    /// menu here never does, so an alternate is never shown.
    alternate: usize,
    indentation: isize,
}

pub const MENU_INSTANCE_SIZE: usize = size_of::<Menu>();
pub const ITEM_INSTANCE_SIZE: usize = size_of::<MenuItem>();

fn menu_of<'a>(receiver: Id) -> &'a mut Menu {
    // SAFETY: a menu of this crate's.
    unsafe { &mut *(receiver as *mut Menu) }
}

fn item_of<'a>(receiver: Id) -> &'a mut MenuItem {
    // SAFETY: a menu item of this crate's.
    unsafe { &mut *(receiver as *mut MenuItem) }
}

/// The item list, made on first use.
fn items_of(receiver: Id) -> &'static mut corefoundation::collections::ArrayContents {
    let menu = menu_of(receiver);
    if menu.items.is_null() {
        let array = corefoundation::collections::new_array(
            &[],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            true,
        );
        menu.items = array as Id;
    }
    // SAFETY: an array this function made.
    unsafe { corefoundation::collections::array(menu.items as *const c_void) }
        .expect("the array this function just made")
}

unsafe extern "C" fn init_with_title(receiver: Id, _sel: Sel, title: Id) -> Id {
    menu_of(receiver).title = objc::arc::retain(title);
    receiver
}

unsafe extern "C" fn menu_title(receiver: Id, _sel: Sel) -> Id {
    menu_of(receiver).title
}

unsafe extern "C" fn set_menu_title(receiver: Id, _sel: Sel, title: Id) {
    let menu = menu_of(receiver);
    objc::arc::release(menu.title);
    menu.title = objc::arc::retain(title);
}

unsafe extern "C" fn menu_items(receiver: Id, _sel: Sel) -> Id {
    items_of(receiver);
    menu_of(receiver).items
}

/// `-setItemArray:`, and the `NSMenuItems` key a nib carries.
unsafe extern "C" fn set_menu_items(receiver: Id, _sel: Sel, items: Id) {
    let list = items_of(receiver);
    list.values.clear();
    // SAFETY: null or an array.
    let Some(source) = (unsafe { corefoundation::collections::array(items as *const c_void) })
    else {
        return;
    };
    for value in source.values.clone() {
        list.values.push(list.policy.retain(value));
        item_of(value as Id).menu = receiver;
    }
}

unsafe extern "C" fn number_of_items(receiver: Id, _sel: Sel) -> isize {
    items_of(receiver).values.len() as isize
}

unsafe extern "C" fn item_at_index(receiver: Id, _sel: Sel, index: isize) -> Id {
    let list = items_of(receiver);
    match corefoundation::collections::at(index, list.values.len()) {
        Some(at) => list.values[at] as Id,
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn add_item(receiver: Id, _sel: Sel, item: Id) {
    if item.is_null() {
        return;
    }
    let list = items_of(receiver);
    list.values.push(list.policy.retain(item as *const c_void));
    item_of(item).menu = receiver;
}

unsafe extern "C" fn insert_item(receiver: Id, _sel: Sel, item: Id, index: isize) {
    if item.is_null() {
        return;
    }
    let list = items_of(receiver);
    let at = (index.max(0) as usize).min(list.values.len());
    let retained = list.policy.retain(item as *const c_void);
    list.values.insert(at, retained);
    item_of(item).menu = receiver;
}

/// Take the item at `at` out of the menu, which then no longer owns it.
fn detach(receiver: Id, at: usize) {
    let list = items_of(receiver);
    let value = list.values.remove(at);
    let item = item_of(value as Id);
    if item.menu == receiver {
        item.menu = std::ptr::null_mut();
    }
    list.policy.release(value);
}

unsafe extern "C" fn remove_item(receiver: Id, sel: Sel, item: Id) {
    // SAFETY: `-indexOfItem:` is `NSInteger (id, SEL, id)`.
    let at = unsafe { index_of_item(receiver, sel, item) };
    if at >= 0 {
        detach(receiver, at as usize);
    }
}

unsafe extern "C" fn remove_item_at_index(receiver: Id, _sel: Sel, index: isize) {
    let list = items_of(receiver);
    if let Some(at) = corefoundation::collections::at(index, list.values.len()) {
        detach(receiver, at);
    }
}

unsafe extern "C" fn remove_all_items(receiver: Id, _sel: Sel) {
    while !items_of(receiver).values.is_empty() {
        detach(receiver, items_of(receiver).values.len() - 1);
    }
}

unsafe extern "C" fn index_of_item(receiver: Id, _sel: Sel, item: Id) -> isize {
    let list = items_of(receiver);
    list.values
        .iter()
        .position(|v| *v == item as *const c_void)
        .map_or(-1, |at| at as isize)
}

/// `-itemWithTag:`, searching this menu and then its submenus.
///
/// Recursive, as Apple's is: a program tags an item deep in the hierarchy and
/// looks for it from the main menu.
unsafe extern "C" fn item_with_tag(receiver: Id, sel: Sel, tag: isize) -> Id {
    for value in items_of(receiver).values.clone() {
        let item = value as Id;
        if item_of(item).tag == tag {
            return item;
        }
        let submenu = item_of(item).submenu;
        if !submenu.is_null() {
            // SAFETY: `-itemWithTag:` is `id (id, SEL, NSInteger)`.
            let found = unsafe { item_with_tag(submenu, sel, tag) };
            if !found.is_null() {
                return found;
            }
        }
    }
    std::ptr::null_mut()
}

/// `-itemWithTitle:`, which is **not** recursive - unlike `-itemWithTag:` above.
///
/// Apple's searches this menu's own items only, and the difference is not an
/// oversight in either place: a tag is unique across a hierarchy by convention
/// and a title is not, so a recursive search by title would answer with an item
/// from a submenu the caller was not asking about.
unsafe extern "C" fn item_with_title(receiver: Id, _sel: Sel, title: Id) -> Id {
    // SAFETY: null or an `NSString`.
    let wanted = unsafe { foundation::string::bytes(title) };
    for value in items_of(receiver).values.clone() {
        let item = value as Id;
        // SAFETY: an item of this menu, whose title is null or an `NSString`.
        if unsafe { foundation::string::bytes(item_of(item).title) } == wanted {
            return item;
        }
    }
    std::ptr::null_mut()
}

/// `-indexOfItemWithTitle:`, the same search reported as a position.
unsafe extern "C" fn index_of_item_with_title(receiver: Id, sel: Sel, title: Id) -> isize {
    // SAFETY: `-itemWithTitle:` is `id (id, SEL, id)`.
    let found = unsafe { item_with_title(receiver, sel, title) };
    match found.is_null() {
        // `NSNotFound` is `NSIntegerMax` and not -1, which is the value a
        // caller compares against.
        true => isize::MAX,
        // SAFETY: `-indexOfItem:` is `NSInteger (id, SEL, id)`, and `found` is
        // an item of this menu.
        false => unsafe { index_of_item(receiver, sel, found) },
    }
}

unsafe extern "C" fn menu_delegate(receiver: Id, _sel: Sel) -> Id {
    menu_of(receiver).delegate
}

unsafe extern "C" fn set_menu_delegate(receiver: Id, _sel: Sel, value: Id) {
    menu_of(receiver).delegate = value;
}

unsafe extern "C" fn supermenu(receiver: Id, _sel: Sel) -> Id {
    match menu_of(receiver).supermenu {
        holder if holder.is_null() => holder,
        holder => item_of(holder).menu,
    }
}

unsafe extern "C" fn init_item(
    receiver: Id,
    _sel: Sel,
    title: Id,
    action: Sel,
    key_equivalent: Id,
) -> Id {
    let item = item_of(receiver);
    item.title = objc::arc::retain(title);
    item.action = action;
    item.key_equivalent = objc::arc::retain(key_equivalent);
    item.enabled = 1;
    item.modifier_mask = crate::keymap::flags::COMMAND as usize;
    receiver
}

/// `-init`: an enabled item whose key equivalent, once set, takes Command -
/// AppKit's defaults, which a nib and `-initWithTitle:...` both assume.
unsafe extern "C" fn init_plain_item(receiver: Id, _sel: Sel) -> Id {
    let item = item_of(receiver);
    item.enabled = 1;
    item.modifier_mask = crate::keymap::flags::COMMAND as usize;
    receiver
}

unsafe extern "C" fn item_title(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).title
}

unsafe extern "C" fn set_item_title(receiver: Id, _sel: Sel, title: Id) {
    let item = item_of(receiver);
    objc::arc::release(item.title);
    item.title = objc::arc::retain(title);
}

unsafe extern "C" fn item_action(receiver: Id, _sel: Sel) -> Sel {
    item_of(receiver).action
}

unsafe extern "C" fn set_item_action(receiver: Id, _sel: Sel, action: Sel) {
    item_of(receiver).action = action;
}

unsafe extern "C" fn item_target(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).target
}

unsafe extern "C" fn set_item_target(receiver: Id, _sel: Sel, target: Id) {
    item_of(receiver).target = target;
}

unsafe extern "C" fn item_submenu(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).submenu
}

unsafe extern "C" fn set_item_submenu(receiver: Id, _sel: Sel, submenu: Id) {
    let item = item_of(receiver);
    if !item.submenu.is_null() && menu_of(item.submenu).supermenu == receiver {
        menu_of(item.submenu).supermenu = std::ptr::null_mut();
    }
    objc::arc::release(item.submenu);
    item.submenu = objc::arc::retain(submenu);
    if !submenu.is_null() {
        menu_of(submenu).supermenu = receiver;
    }
}

unsafe extern "C" fn item_menu(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).menu
}

unsafe extern "C" fn set_item_menu(receiver: Id, _sel: Sel, menu: Id) {
    item_of(receiver).menu = menu;
}

unsafe extern "C" fn key_equivalent(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).key_equivalent
}

unsafe extern "C" fn set_key_equivalent(receiver: Id, _sel: Sel, key: Id) {
    let item = item_of(receiver);
    objc::arc::release(item.key_equivalent);
    item.key_equivalent = objc::arc::retain(key);
}

/// The plain integer accessors, each reading or writing one field.
macro_rules! integer_accessors {
    ($($get:ident / $set:ident => $field:ident),* $(,)?) => {
        $(
            unsafe extern "C" fn $get(receiver: Id, _sel: Sel) -> isize {
                item_of(receiver).$field as isize
            }
            unsafe extern "C" fn $set(receiver: Id, _sel: Sel, value: isize) {
                item_of(receiver).$field = value as _;
            }
        )*
    };
}

integer_accessors! {
    item_tag / set_item_tag => tag,
    item_state / set_item_state => state,
}

unsafe extern "C" fn is_enabled(receiver: Id, _sel: Sel) -> u8 {
    (item_of(receiver).enabled != 0) as u8
}

unsafe extern "C" fn set_enabled(receiver: Id, _sel: Sel, enabled: u8) {
    item_of(receiver).enabled = enabled as usize;
}

unsafe extern "C" fn is_hidden(receiver: Id, _sel: Sel) -> u8 {
    (item_of(receiver).hidden != 0) as u8
}

unsafe extern "C" fn set_hidden(receiver: Id, _sel: Sel, hidden: u8) {
    item_of(receiver).hidden = hidden as usize;
}

/// The requests that describe how a menu *looks*, which nothing here draws.
/// `+[NSMenuItem separatorItem]`, the divider in a menu.
///
/// A fresh item each time, as Apple's is: a separator goes into exactly one menu
/// at one index, and a shared singleton put into two menus would be at one index
/// in both.
unsafe extern "C" fn separator_item(_class: Id, _sel: Sel) -> Id {
    let class = objc::class::lookup(c"NSMenuItem").expect("declared before its methods");
    let object = objc::object::create_instance(class, 0);
    item_of(object).separator = 1;
    objc::arc::autorelease(object)
}

unsafe extern "C" fn is_separator_item(receiver: Id, _sel: Sel) -> u8 {
    item_of(receiver).separator as u8
}

/// `-hasSubmenu`, which a caller asks before descending rather than reading
/// `-submenu` and testing it for nil.
unsafe extern "C" fn has_submenu(receiver: Id, _sel: Sel) -> u8 {
    !item_of(receiver).submenu.is_null() as u8
}

unsafe extern "C" fn key_equivalent_modifier_mask(receiver: Id, _sel: Sel) -> usize {
    item_of(receiver).modifier_mask
}

unsafe extern "C" fn set_key_equivalent_modifier_mask(receiver: Id, _sel: Sel, mask: usize) {
    item_of(receiver).modifier_mask = mask;
}

unsafe extern "C" fn item_tool_tip(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).tool_tip
}

unsafe extern "C" fn set_item_tool_tip(receiver: Id, _sel: Sel, text: Id) {
    let item = item_of(receiver);
    objc::arc::release(item.tool_tip);
    item.tool_tip = objc::arc::retain(text);
}

unsafe extern "C" fn item_image(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).image
}

unsafe extern "C" fn set_item_image(receiver: Id, _sel: Sel, image: Id) {
    let item = item_of(receiver);
    objc::arc::release(item.image);
    item.image = objc::arc::retain(image);
}

unsafe extern "C" fn item_view(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).view
}

unsafe extern "C" fn set_item_view(receiver: Id, _sel: Sel, view: Id) {
    let item = item_of(receiver);
    objc::arc::release(item.view);
    item.view = objc::arc::retain(view);
}

/// `-indexOfItemWithSubmenu:`, `-indexOfItem:` asked the other way round.
///
/// How a caller finds where a submenu was attached so it can replace or remove it
/// - `-1` for a submenu this menu does not hold, which is the convention every
/// `indexOf` on `NSMenu` uses.
unsafe extern "C" fn index_of_item_with_submenu(receiver: Id, _sel: Sel, submenu: Id) -> isize {
    items_of(receiver)
        .values
        .iter()
        .position(|value| item_of(*value as Id).submenu == submenu)
        .map_or(-1, |at| at as isize)
}

unsafe extern "C" fn autoenables_items(receiver: Id, _sel: Sel) -> u8 {
    (menu_of(receiver).no_autoenable == 0) as u8
}

unsafe extern "C" fn set_autoenables_items(receiver: Id, _sel: Sel, value: u8) {
    menu_of(receiver).no_autoenable = (value == 0) as usize;
}

unsafe extern "C" fn is_alternate(receiver: Id, _sel: Sel) -> u8 {
    item_of(receiver).alternate as u8
}

unsafe extern "C" fn set_alternate(receiver: Id, _sel: Sel, value: u8) {
    item_of(receiver).alternate = value as usize;
}

unsafe extern "C" fn indentation_level(receiver: Id, _sel: Sel) -> isize {
    item_of(receiver).indentation
}

unsafe extern "C" fn set_indentation_level(receiver: Id, _sel: Sel, value: isize) {
    item_of(receiver).indentation = value.clamp(0, 15);
}

/// `-[NSMenu update]`: validation happens as a menu opens, so there is
/// nothing to bring up to date ahead of that.
unsafe extern "C" fn update(_receiver: Id, _sel: Sel) {}

/// `-[NSMenu performKeyEquivalent:]`: find the item whose key equivalent the
/// event is, here or in a submenu, and choose it if it is enabled.
unsafe extern "C" fn perform_key_equivalent(receiver: Id, _sel: Sel, event: Id) -> u8 {
    // SAFETY: an event, which answers both; sent rather than read so a
    // guest's own `NSEvent` subclass answers for itself.
    let (characters, modifiers) = unsafe {
        let text: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        let flags: unsafe extern "C" fn(Id, Sel) -> u64 = std::mem::transmute(objc::msg::entry());
        (
            text(event, objc::sel::get("charactersIgnoringModifiers")),
            flags(event, objc::sel::get("modifierFlags")),
        )
    };
    if characters.is_null() {
        return 0;
    }
    // SAFETY: an `NSString`.
    let characters =
        String::from_utf8_lossy(&unsafe { foundation::string::bytes(characters) }).into_owned();
    let Some(item) = find_key_equivalent(receiver, &characters, modifiers, 0) else {
        return 0;
    };
    if !is_item_enabled(item) {
        return 0;
    }
    perform(item);
    1
}

/// `-performActionForItemAtIndex:`.
unsafe extern "C" fn perform_action_for_item_at_index(receiver: Id, _sel: Sel, index: isize) {
    let list = items_of(receiver);
    if let Some(at) = corefoundation::collections::at(index, list.values.len()) {
        perform(list.values[at] as Id);
    }
}

unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _object: Id) {}

pub(crate) fn install() {
    let menu = objc::class::lookup(c"NSMenu").expect("declared before its methods");
    methods!(menu, {
        "initWithTitle:" => init_with_title, c"@24@0:8@16";
        "title" => menu_title, c"@16@0:8";
        "setTitle:" => set_menu_title, c"v24@0:8@16";
        "itemArray" => menu_items, c"@16@0:8";
        "setItemArray:" => set_menu_items, c"v24@0:8@16";
        "numberOfItems" => number_of_items, c"q16@0:8";
        "itemAtIndex:" => item_at_index, c"@24@0:8q16";
        "addItem:" => add_item, c"v24@0:8@16";
        "insertItem:atIndex:" => insert_item, c"v32@0:8@16q24";
        "removeItem:" => remove_item, c"v24@0:8@16";
        "removeItemAtIndex:" => remove_item_at_index, c"v24@0:8q16";
        "removeAllItems" => remove_all_items, c"v16@0:8";
        "indexOfItem:" => index_of_item, c"q24@0:8@16";
        "itemWithTag:" => item_with_tag, c"@24@0:8q16";
        "itemWithTitle:" => item_with_title, c"@24@0:8@16";
        "indexOfItemWithTitle:" => index_of_item_with_title, c"q24@0:8@16";
        "delegate" => menu_delegate, c"@16@0:8";
        "setDelegate:" => set_menu_delegate, c"v24@0:8@16";
        "supermenu" => supermenu, c"@16@0:8";
        "autoenablesItems" => autoenables_items, c"c16@0:8";
        "setAutoenablesItems:" => set_autoenables_items, c"v20@0:8c16";
        "update" => update, c"v16@0:8";
        "performKeyEquivalent:" => perform_key_equivalent, c"c24@0:8@16";
        "performActionForItemAtIndex:" => perform_action_for_item_at_index, c"v24@0:8q16";
        "setSubmenu:forItem:" => ignored_object, c"v32@0:8@16@24";
        "indexOfItemWithSubmenu:" => index_of_item_with_submenu, c"q24@0:8@16";
    });

    let item = objc::class::lookup(c"NSMenuItem").expect("declared before its methods");
    class_methods!(item, {
        "separatorItem" => separator_item, c"@16@0:8";
    });
    methods!(item, {
        "init" => init_plain_item, c"@16@0:8";
        "initWithTitle:action:keyEquivalent:" => init_item, c"@40@0:8@16:24@32";
        "isAlternate" => is_alternate, c"c16@0:8";
        "setAlternate:" => set_alternate, c"v20@0:8c16";
        "indentationLevel" => indentation_level, c"q16@0:8";
        "setIndentationLevel:" => set_indentation_level, c"v24@0:8q16";
        "title" => item_title, c"@16@0:8";
        "setTitle:" => set_item_title, c"v24@0:8@16";
        "action" => item_action, c":16@0:8";
        "setAction:" => set_item_action, c"v24@0:8:16";
        "target" => item_target, c"@16@0:8";
        "setTarget:" => set_item_target, c"v24@0:8@16";
        "submenu" => item_submenu, c"@16@0:8";
        "setSubmenu:" => set_item_submenu, c"v24@0:8@16";
        "menu" => item_menu, c"@16@0:8";
        "setMenu:" => set_item_menu, c"v24@0:8@16";
        "keyEquivalent" => key_equivalent, c"@16@0:8";
        "setKeyEquivalent:" => set_key_equivalent, c"v24@0:8@16";
        "tag" => item_tag, c"q16@0:8";
        "setTag:" => set_item_tag, c"v24@0:8q16";
        "state" => item_state, c"q16@0:8";
        "setState:" => set_item_state, c"v24@0:8q16";
        "isEnabled" => is_enabled, c"c16@0:8";
        "setEnabled:" => set_enabled, c"v20@0:8c16";
        "isHidden" => is_hidden, c"c16@0:8";
        "setHidden:" => set_hidden, c"v20@0:8c16";
        "setKeyEquivalentModifierMask:" => set_key_equivalent_modifier_mask, c"v24@0:8Q16";
        "keyEquivalentModifierMask" => key_equivalent_modifier_mask, c"Q16@0:8";
        "isSeparatorItem" => is_separator_item, c"c16@0:8";
        "hasSubmenu" => has_submenu, c"c16@0:8";
        "toolTip" => item_tool_tip, c"@16@0:8";
        "setToolTip:" => set_item_tool_tip, c"v24@0:8@16";
        "image" => item_image, c"@16@0:8";
        "setImage:" => set_item_image, c"v24@0:8@16";
        "view" => item_view, c"@16@0:8";
        "setView:" => set_item_view, c"v24@0:8@16";
        "setOnStateImage:" => ignored_object, c"v24@0:8@16";
        "setMixedStateImage:" => ignored_object, c"v24@0:8@16";
        "setRepresentedObject:" => ignored_object, c"v24@0:8@16";
    });
}

// What the chrome reads off a menu, and what choosing an item does.

/// `[NSApp mainMenu]`, or nil.
pub(crate) fn main_menu() -> Id {
    let app = crate::application::shared();
    if app.is_null() {
        return app;
    }
    // SAFETY: `-mainMenu` is `id (id, SEL)` on the application.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(app, objc::sel::get("mainMenu"))
    }
}

fn is_menu_item(object: Id) -> bool {
    // SAFETY: null or an object this runtime allocated.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSMenuItem").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

fn is_menu(object: Id) -> bool {
    // SAFETY: null or an object this runtime allocated.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSMenu").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

/// A menu's items that are shown: neither hidden nor alternates.
pub(crate) fn shown_items(menu: Id) -> Vec<Id> {
    if !is_menu(menu) {
        return Vec::new();
    }
    items_of(menu)
        .values
        .iter()
        .map(|value| *value as Id)
        .filter(|item| is_menu_item(*item))
        .filter(|item| item_of(*item).hidden == 0 && item_of(*item).alternate == 0)
        .collect()
}

fn string_of(object: Id) -> String {
    match object.is_null() {
        true => String::new(),
        // SAFETY: an `NSString`.
        false => {
            String::from_utf8_lossy(&unsafe { foundation::string::bytes(object) }).into_owned()
        }
    }
}

/// The title an item shows: its own, or its submenu's when it has none.
pub(crate) fn display_title(item: Id) -> String {
    let own = string_of(item_of(item).title);
    match own.is_empty() && !item_of(item).submenu.is_null() {
        true => string_of(menu_of(item_of(item).submenu).title),
        false => own,
    }
}

pub(crate) fn submenu_of(item: Id) -> Id {
    item_of(item).submenu
}

pub(crate) fn is_separator(item: Id) -> bool {
    item_of(item).separator != 0
}

pub(crate) fn state_of(item: Id) -> isize {
    item_of(item).state
}

pub(crate) fn indentation(item: Id) -> isize {
    item_of(item).indentation
}

/// `-setSeparatorItem` has no setter of AppKit's; a nib says so with a flag.
pub(crate) fn mark_separator(item: Id) {
    item_of(item).separator = 1;
}

/// The item's key equivalent as a Mac menu shows it.
pub(crate) fn shortcut_label(item: Id) -> String {
    let item = item_of(item);
    let key = string_of(item.key_equivalent);
    let mask = item.modifier_mask as u64;
    use crate::keymap::flags;
    chrome::menu::shortcut_label(
        &key,
        chrome::menu::Modifiers {
            command: mask & flags::COMMAND != 0,
            shift: mask & flags::SHIFT != 0,
            option: mask & flags::OPTION != 0,
            control: mask & flags::CONTROL != 0,
        },
    )
}

/// The object an item's action goes to: its target, or the first responder
/// in the application's chain that implements the action.
fn target_of(item: Id) -> Id {
    let item = item_of(item);
    if item.action.is_null() {
        return std::ptr::null_mut();
    }
    crate::responder::target_for_action(item.action, item.target)
}

/// Whether `object` implements `sel`.
fn implements(object: Id, sel: Sel) -> bool {
    // SAFETY: a live object this runtime allocated.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| c.method(sel).is_some())
}

/// Whether choosing the item would do anything, as AppKit decides it: an
/// item in a menu that enables its items is enabled when something takes
/// its action and, if that asks to, agrees.
pub(crate) fn is_item_enabled(item: Id) -> bool {
    let body = item_of(item);
    let menu = body.menu;
    let automatic = is_menu(menu) && menu_of(menu).no_autoenable == 0;
    if !automatic {
        return body.enabled != 0;
    }
    if !body.submenu.is_null() {
        return true;
    }
    let target = target_of(item);
    if target.is_null() {
        return false;
    }
    for validator in ["validateMenuItem:", "validateUserInterfaceItem:"] {
        let sel = objc::sel::get(validator);
        if implements(target, sel) {
            // SAFETY: both validators are `BOOL (id, SEL, id)`.
            return unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                    std::mem::transmute(objc::msg::entry());
                f(target, sel, item) != 0
            };
        }
    }
    true
}

/// Whether the item is enabled as it was last set, with no validation:
/// what a menu bar title shows, which is asked every frame.
pub(crate) fn is_enabled_flag(item: Id) -> bool {
    item_of(item).enabled != 0 || !item_of(item).submenu.is_null()
}

/// Send the menu's delegate `selector` with the menu, if it implements it.
fn tell_delegate(menu: Id, selector: &str) {
    if !is_menu(menu) {
        return;
    }
    let delegate = menu_of(menu).delegate;
    let sel = objc::sel::get(selector);
    if delegate.is_null() || !implements(delegate, sel) {
        return;
    }
    // SAFETY: each delegate message here is `void (id, SEL, NSMenu *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(delegate, sel, menu);
    }
}

/// The items of a menu about to open, after its delegate has had the chance
/// to rebuild them, each retained for the caller - see [`release_items`].
pub(crate) fn prepare(menu: Id) -> Vec<Id> {
    tell_delegate(menu, "menuNeedsUpdate:");
    shown_items(menu)
        .into_iter()
        .map(objc::arc::retain)
        .collect()
}

pub(crate) fn release_items(items: Vec<Id>) {
    items.into_iter().for_each(objc::arc::release);
}

pub(crate) fn will_open(menu: Id) {
    tell_delegate(menu, "menuWillOpen:");
}

pub(crate) fn did_close(menu: Id) {
    tell_delegate(menu, "menuDidClose:");
}

/// Choose `item`: its action is sent to its target, bracketed by the two
/// notifications AppKit posts about it.
pub(crate) fn perform(item: Id) {
    let body = item_of(item);
    if body.action.is_null() {
        return;
    }
    let (menu, action, target) = (body.menu, body.action, body.target);
    foundation::notification::post(
        "NSMenuWillSendActionNotification",
        menu,
        std::ptr::null_mut(),
    );
    let app = crate::application::shared();
    if !app.is_null() {
        // SAFETY: `-sendAction:to:from:` is `BOOL (id, SEL, SEL, id, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Sel, Id, Id) -> u8 =
                std::mem::transmute(objc::msg::entry());
            f(
                app,
                objc::sel::get("sendAction:to:from:"),
                action,
                target,
                item,
            );
        }
    }
    foundation::notification::post(
        "NSMenuDidSendActionNotification",
        menu,
        std::ptr::null_mut(),
    );
}

/// The modifiers a key equivalent is compared by.
const EQUIVALENT_MODIFIERS: u64 = crate::keymap::flags::SHIFT
    | crate::keymap::flags::CONTROL
    | crate::keymap::flags::OPTION
    | crate::keymap::flags::COMMAND;

/// Whether an item with key equivalent `key` and modifier mask `mask` is what
/// a key press of `characters` (ignoring modifiers) with `modifiers` held
/// means.
pub(crate) fn matches_key_equivalent(
    key: &str,
    mask: u64,
    characters: &str,
    modifiers: u64,
) -> bool {
    let mut chars = key.chars();
    let (Some(wanted), None) = (chars.next(), chars.next()) else {
        return false;
    };
    let mut required = mask & EQUIVALENT_MODIFIERS;
    // An upper-case equivalent is the shifted key.
    if wanted.is_alphabetic() && wanted.is_uppercase() {
        required |= crate::keymap::flags::SHIFT;
    }
    let mut pressed = characters.chars();
    let (Some(pressed), None) = (pressed.next(), pressed.next()) else {
        return false;
    };
    let same_key = wanted.to_lowercase().eq(pressed.to_lowercase())
        // Return and Enter are one key equivalent.
        || (matches!(wanted, '\r' | '\u{3}') && matches!(pressed, '\r' | '\u{3}'));
    same_key && modifiers & EQUIVALENT_MODIFIERS == required
}

/// The first item, depth first, whose key equivalent is this key press.
fn find_key_equivalent(menu: Id, characters: &str, modifiers: u64, depth: usize) -> Option<Id> {
    if depth > 16 {
        return None;
    }
    for item in shown_items(menu) {
        let body = item_of(item);
        if !body.submenu.is_null() {
            if let Some(found) = find_key_equivalent(body.submenu, characters, modifiers, depth + 1)
            {
                return Some(found);
            }
            continue;
        }
        let key = string_of(body.key_equivalent);
        if matches_key_equivalent(&key, body.modifier_mask as u64, characters, modifiers) {
            return Some(item);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(title: &str) -> Id {
        crate::bootstrap();
        let object =
            objc::object::create_instance(objc::class::lookup(c"NSMenu").expect("NSMenu"), 0);
        // SAFETY: `-initWithTitle:` is `id (id, SEL, id)`.
        unsafe {
            init_with_title(
                object,
                objc::sel::get("initWithTitle:"),
                foundation::string::from_str(title),
            )
        }
    }

    fn item(tag: isize) -> Id {
        crate::bootstrap();
        let object = objc::object::create_instance(
            objc::class::lookup(c"NSMenuItem").expect("NSMenuItem"),
            0,
        );
        // SAFETY: `-setTag:` is `void (id, SEL, NSInteger)`.
        unsafe { set_item_tag(object, objc::sel::get("setTag:"), tag) };
        object
    }

    #[test]
    fn a_key_equivalent_is_its_key_and_exactly_its_modifiers() {
        use crate::keymap::flags::{CAPS_LOCK, COMMAND, CONTROL, OPTION, SHIFT};
        assert!(matches_key_equivalent("s", COMMAND, "s", COMMAND));
        assert!(!matches_key_equivalent("s", COMMAND, "s", COMMAND | SHIFT));
        assert!(!matches_key_equivalent("s", COMMAND, "s", CONTROL));
        assert!(!matches_key_equivalent("s", COMMAND, "d", COMMAND));
        // An upper-case equivalent is the shifted key.
        assert!(matches_key_equivalent("S", COMMAND, "s", COMMAND | SHIFT));
        assert!(matches_key_equivalent(
            "z",
            COMMAND | OPTION,
            "z",
            COMMAND | OPTION
        ));
        // Caps lock is not a modifier an equivalent is chosen by.
        assert!(matches_key_equivalent(
            "s",
            COMMAND,
            "s",
            COMMAND | CAPS_LOCK
        ));
        assert!(!matches_key_equivalent("", COMMAND, "s", COMMAND));
    }

    static SAVES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static ALLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

    unsafe extern "C" fn save(_receiver: Id, _sel: Sel, _sender: Id) {
        SAVES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    unsafe extern "C" fn validate(_receiver: Id, _sel: Sel, _item: Id) -> u8 {
        ALLOWED.load(std::sync::atomic::Ordering::SeqCst) as u8
    }

    /// Command-S reaches a Save item two menus down, is refused while the
    /// target says the item is disabled, and is not taken by another key.
    #[test]
    fn a_key_equivalent_chooses_its_item_when_the_target_agrees() {
        crate::bootstrap();
        let class = objc::class::ptr(objc::class::lookup(c"NSApplication").expect("NSApplication"));
        // SAFETY: `+sharedApplication` is `id (Class, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(class as Id, objc::sel::get("sharedApplication"));
        }
        let target_class =
            objc::class::allocate_pair(objc::class::lookup(c"NSObject"), c"R3TestMenuTarget", 0)
                .expect("a class of this name is not declared elsewhere");
        target_class.add_method(
            objc::sel::get("r3TestSave:"),
            save as objc::class::Imp,
            c"v24@0:8@16",
        );
        target_class.add_method(
            objc::sel::get("validateMenuItem:"),
            validate as objc::class::Imp,
            c"c24@0:8@16",
        );
        objc::class::register_pair(target_class);
        let target = objc::object::create_instance(target_class, 0);

        let root = menu("Main");
        let file = menu("File");
        let holder = item(0);
        let save_item = item(1);
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            init_plain_item(save_item, objc::sel::get("init"));
            set_item_action(
                save_item,
                objc::sel::get("setAction:"),
                objc::sel::get("r3TestSave:"),
            );
            set_item_target(save_item, objc::sel::get("setTarget:"), target);
            set_key_equivalent(
                save_item,
                objc::sel::get("setKeyEquivalent:"),
                foundation::string::from_str("s"),
            );
            set_item_submenu(holder, objc::sel::get("setSubmenu:"), file);
            add_item(root, objc::sel::get("addItem:"), holder);
            add_item(file, objc::sel::get("addItem:"), save_item);
        }
        let press = |characters: &str, modifiers: u64| {
            let event = crate::nsevent::make(crate::nsevent::Fields {
                kind: crate::nsevent::kind::KEY_DOWN,
                modifiers,
                characters: foundation::string::from_str(characters),
                ..crate::nsevent::Fields::default()
            });
            // SAFETY: `-performKeyEquivalent:` is `BOOL (id, SEL, NSEvent *)`.
            unsafe { perform_key_equivalent(root, objc::sel::get("performKeyEquivalent:"), event) }
        };
        let command = crate::keymap::flags::COMMAND;
        let before = SAVES.load(std::sync::atomic::Ordering::SeqCst);
        ALLOWED.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(press("s", command), 1);
        assert_eq!(SAVES.load(std::sync::atomic::Ordering::SeqCst), before + 1);
        assert_eq!(press("d", command), 0, "no item has Command-D");

        ALLOWED.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(!is_item_enabled(save_item));
        assert_eq!(press("s", command), 0, "a disabled item is not chosen");
        assert_eq!(SAVES.load(std::sync::atomic::Ordering::SeqCst), before + 1);

        // A menu that does not enable its own items goes by the flag alone.
        // SAFETY: `-setAutoenablesItems:` is `void (id, SEL, BOOL)`.
        unsafe { set_autoenables_items(file, objc::sel::get("setAutoenablesItems:"), 0) };
        assert!(is_item_enabled(save_item));
        ALLOWED.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// An item nothing can take the action of is disabled, as AppKit greys
    /// out a menu item with nowhere to send it.
    #[test]
    fn an_action_nothing_implements_disables_its_item() {
        let file = menu("Edit");
        let orphan = item(3);
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            init_plain_item(orphan, objc::sel::get("init"));
            set_item_action(
                orphan,
                objc::sel::get("setAction:"),
                objc::sel::get("r3TestNobodyImplementsThis:"),
            );
            add_item(file, objc::sel::get("addItem:"), orphan);
        }
        assert!(!is_item_enabled(orphan));
        assert!(is_enabled_flag(orphan), "the flag itself is still set");
    }

    /// A separator answers `-isSeparatorItem` and an ordinary item does not,
    /// which is how a caller walking a menu tells a divider from something with a
    /// title to read.
    #[test]
    fn a_separator_says_so_and_an_ordinary_item_does_not() {
        crate::bootstrap();
        let class = objc::class::ptr(objc::class::lookup(c"NSMenuItem").expect("NSMenuItem")) as Id;
        // SAFETY: `+separatorItem` is `id (Class, SEL)`.
        let separator = unsafe { separator_item(class, objc::sel::get("separatorItem")) };
        let plain = item(1);
        // SAFETY: `-isSeparatorItem` is `BOOL (id, SEL)` on items of this crate's.
        unsafe {
            let sel = objc::sel::get("isSeparatorItem");
            assert_eq!(is_separator_item(separator, sel), 1);
            assert_eq!(is_separator_item(plain, sel), 0);
        }
        // And two separators are two items, so each can sit at its own index.
        let second = unsafe { separator_item(class, objc::sel::get("separatorItem")) };
        assert_ne!(separator, second);
    }

    /// `-hasSubmenu` and `-indexOfItemWithSubmenu:` are the pair a caller uses to
    /// find where a submenu was attached.
    #[test]
    fn a_submenu_is_found_by_the_item_holding_it() {
        let parent = menu("Parent");
        let child = menu("Child");
        let holder = item(7);
        let plain = item(8);
        // SAFETY: menus and items this test made.
        unsafe {
            assert_eq!(has_submenu(holder, objc::sel::get("hasSubmenu")), 0);
            set_item_submenu(holder, objc::sel::get("setSubmenu:"), child);
            assert_eq!(has_submenu(holder, objc::sel::get("hasSubmenu")), 1);
            add_item(parent, objc::sel::get("addItem:"), plain);
            add_item(parent, objc::sel::get("addItem:"), holder);
            assert_eq!(
                index_of_item_with_submenu(
                    parent,
                    objc::sel::get("indexOfItemWithSubmenu:"),
                    child
                ),
                1
            );
            // A submenu this menu does not hold is -1, not 0.
            assert_eq!(
                index_of_item_with_submenu(
                    parent,
                    objc::sel::get("indexOfItemWithSubmenu:"),
                    menu("Elsewhere")
                ),
                -1
            );
        }
    }

    /// A tag is found through a submenu, which is how a program reaches the item
    /// it wants from the main menu.
    #[test]
    fn an_item_is_found_by_tag_through_a_submenu() {
        let root = menu("Main");
        let file = menu("File");
        let holder = item(0);
        let wanted = item(42);

        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            set_item_submenu(holder, objc::sel::get("setSubmenu:"), file);
            add_item(root, objc::sel::get("addItem:"), holder);
            add_item(file, objc::sel::get("addItem:"), wanted);

            assert_eq!(
                item_with_tag(root, objc::sel::get("itemWithTag:"), 42),
                wanted
            );
            assert!(item_with_tag(root, objc::sel::get("itemWithTag:"), 7).is_null());
            // And the submenu knows the menu it hangs from, not the item.
            assert_eq!(supermenu(file, objc::sel::get("supermenu")), root);
        }
    }

    /// Adding, indexing and removing agree with each other.
    #[test]
    fn a_menus_items_are_the_ones_added_to_it() {
        let menu = menu("Edit");
        let first = item(1);
        let second = item(2);
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            add_item(menu, objc::sel::get("addItem:"), first);
            insert_item(menu, objc::sel::get("insertItem:atIndex:"), second, 0);
            assert_eq!(number_of_items(menu, objc::sel::get("numberOfItems")), 2);
            assert_eq!(
                item_at_index(menu, objc::sel::get("itemAtIndex:"), 0),
                second
            );
            assert_eq!(
                index_of_item(menu, objc::sel::get("indexOfItem:"), first),
                1
            );

            remove_item(menu, objc::sel::get("removeItem:"), second);
            assert_eq!(number_of_items(menu, objc::sel::get("numberOfItems")), 1);
            assert_eq!(
                item_at_index(menu, objc::sel::get("itemAtIndex:"), 0),
                first
            );
            // An index outside the list is nil rather than a panic.
            assert!(item_at_index(menu, objc::sel::get("itemAtIndex:"), 9).is_null());
            // A removed item no longer names the menu it left.
            assert!(item_menu(second, objc::sel::get("menu")).is_null());
            remove_item_at_index(menu, objc::sel::get("removeItemAtIndex:"), 0);
            assert_eq!(number_of_items(menu, objc::sel::get("numberOfItems")), 0);
        }
    }
}
