//! `-[NSControl cell]` and the `NSCell` family.
//!
//! Every control here keeps its own state, where Apple's keeps it in a cell and
//! forwards to it. A cell is therefore the reverse: a small object that names
//! its control and forwards the shared accessors to it, so a program that
//! reaches through `-cell` reads and writes the same state the control shows.
//! A button cell a program allocates itself is given a button of its own to
//! keep that state, which it owns rather than the other way round.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// `isa`, then the control. Not retained unless `owns_control` is set: a
/// control owns its cell, and the cell is dropped in [`forget`] when the
/// control goes.
#[repr(C)]
struct Cell {
    isa: *const std::ffi::c_void,
    control: Id,
    /// Nonzero for a cell a program made, which made its control to back it.
    owns_control: usize,
}

pub const INSTANCE_SIZE: usize = size_of::<Cell>();

/// The cell class for each control class, most specific first.
const KINDS: &[(&std::ffi::CStr, &std::ffi::CStr)] = &[
    (c"NSPopUpButton", c"NSPopUpButtonCell"),
    (c"NSButton", c"NSButtonCell"),
    (c"NSSearchField", c"NSSearchFieldCell"),
    (c"NSTextField", c"NSTextFieldCell"),
    (c"NSSegmentedControl", c"NSSegmentedCell"),
    (c"NSImageView", c"NSImageCell"),
    (c"NSControl", c"NSActionCell"),
];

/// What a cell answers for its control. Only methods returning in registers:
/// the forwarder cannot move a struct-return buffer's receiver.
const FORWARDED: &[&str] = &[
    "tag",
    "setTag:",
    "stringValue",
    "setStringValue:",
    "objectValue",
    "setObjectValue:",
    "intValue",
    "setIntValue:",
    "integerValue",
    "setIntegerValue:",
    "doubleValue",
    "setDoubleValue:",
    "floatValue",
    "setFloatValue:",
    "attributedStringValue",
    "setAttributedStringValue:",
    "title",
    "setTitle:",
    "attributedTitle",
    "setAttributedTitle:",
    "font",
    "setFont:",
    "alignment",
    "setAlignment:",
    "textColor",
    "setTextColor:",
    "backgroundColor",
    "setBackgroundColor:",
    "drawsBackground",
    "setDrawsBackground:",
    "isBordered",
    "setBordered:",
    "isEnabled",
    "setEnabled:",
    "isEditable",
    "setEditable:",
    "isSelectable",
    "setSelectable:",
    "lineBreakMode",
    "setLineBreakMode:",
    "wraps",
    "setWraps:",
    "state",
    "setState:",
    "image",
    "setImage:",
    "alternateImage",
    "setAlternateImage:",
    "keyEquivalent",
    "setKeyEquivalent:",
    "target",
    "setTarget:",
    "action",
    "setAction:",
    "formatter",
    "setFormatter:",
    "isHighlighted",
    "setHighlighted:",
    "bezelStyle",
    "setBezelStyle:",
    "segmentCount",
    "setSegmentCount:",
    "selectedSegment",
    "setSelectedSegment:",
    "selectSegmentWithTag:",
    "tagForSegment:",
    "setTag:forSegment:",
    "labelForSegment:",
    "setLabel:forSegment:",
    "isSelectedForSegment:",
    "setSelected:forSegment:",
    "isEnabledForSegment:",
    "setEnabled:forSegment:",
    "trackingMode",
    "setTrackingMode:",
    "buttonType",
    "setButtonType:",
    "highlightsBy",
    "setHighlightsBy:",
    "showsStateBy",
    "setShowsStateBy:",
    "imagePosition",
    "setImagePosition:",
    "imageScaling",
    "setImageScaling:",
    "allowsMixedState",
    "setAllowsMixedState:",
    "nextState",
    "setNextState",
    "alternateTitle",
    "setAlternateTitle:",
    "isTransparent",
    "setTransparent:",
    "keyEquivalentModifierMask",
    "setKeyEquivalentModifierMask:",
    "performClick:",
    // `NSPopUpButtonCell`'s item list, which its control keeps.
    "pullsDown",
    "setPullsDown:",
    "menu",
    "setMenu:",
    "addItemWithTitle:",
    "addItemsWithTitles:",
    "insertItemWithTitle:atIndex:",
    "removeAllItems",
    "removeItemAtIndex:",
    "removeItemWithTitle:",
    "itemArray",
    "numberOfItems",
    "itemAtIndex:",
    "itemTitleAtIndex:",
    "itemTitles",
    "lastItem",
    "itemWithTitle:",
    "indexOfItemWithTitle:",
    "indexOfItem:",
    "indexOfItemWithTag:",
    "selectItem:",
    "selectItemAtIndex:",
    "selectItemWithTitle:",
    "selectItemWithTag:",
    "selectedItem",
    "indexOfSelectedItem",
    "titleOfSelectedItem",
    "selectedTag",
    "setAutoenablesItems:",
    "autoenablesItems",
    "synchronizeTitleAndSelectedItem",
];

fn cells() -> &'static Mutex<HashMap<usize, usize>> {
    static TABLE: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

fn kind_of(control: Id) -> &'static objc::class::Class {
    // SAFETY: a control this crate's classes describe, or a program's subclass.
    let chain = unsafe { objc::object::class_of(control) };
    let class_name = KINDS
        .iter()
        .find(|(control_class, _)| {
            let Some(wanted) = objc::class::lookup(control_class) else {
                return false;
            };
            chain.is_some_and(|c| c.chain().any(|k| std::ptr::eq(k, wanted)))
        })
        .map_or(c"NSActionCell", |(_, cell)| *cell);
    objc::class::lookup(class_name).expect("declared in crate::classes")
}

/// The control's cell, made on first use and owned by the control.
pub(crate) fn cell_of(control: Id) -> Id {
    if control.is_null() {
        return std::ptr::null_mut();
    }
    let mut table = cells().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&cell) = table.get(&(control as usize)) {
        return cell as Id;
    }
    let cell = objc::object::create_instance(kind_of(control), 0);
    // SAFETY: a fresh instance of a class laid out as `Cell`.
    unsafe { (*(cell as *mut Cell)).control = control };
    table.insert(control as usize, cell as usize);
    cell
}

/// Drop the cell of a control that is going away.
pub(crate) fn forget(control: Id) {
    let cell = cells()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(control as usize));
    if let Some(cell) = cell {
        // SAFETY: the cell this module made; nothing else points at its control.
        unsafe { (*(cell as *mut Cell)).control = std::ptr::null_mut() };
        objc::arc::release(cell as Id);
    }
}

/// Give a cell a program allocated a control of its kind to keep its state,
/// so that what it is sent is kept, and so that it can be the prototype a
/// matrix copies its cells from.
fn back_with_control(cell: Id) -> Id {
    if cell.is_null() {
        return cell;
    }
    // SAFETY: an instance of a class laid out as `Cell`.
    if unsafe { !(*(cell as *const Cell)).control.is_null() } {
        return cell;
    }
    // SAFETY: an object this runtime allocated.
    let chain = unsafe { objc::object::class_of(cell) };
    let Some(control_class) = KINDS
        .iter()
        .find(|(_, cell_class)| {
            objc::class::lookup(cell_class).is_some_and(|wanted| {
                chain.is_some_and(|c| c.chain().any(|k| std::ptr::eq(k, wanted)))
            })
        })
        .and_then(|(control_class, _)| objc::class::lookup(control_class))
    else {
        return cell;
    };
    let control = objc::object::create_instance(control_class, 0);
    // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)` on every view.
    let control = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, foundation::geometry::NSRect) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            control,
            objc::sel::get("initWithFrame:"),
            foundation::geometry::NSRect::default(),
        )
    };
    if control.is_null() {
        return cell;
    }
    // SAFETY: as above; the cell keeps the reference `-initWithFrame:` gave.
    unsafe {
        (*(cell as *mut Cell)).control = control;
        (*(cell as *mut Cell)).owns_control = 1;
    }
    cells()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(control as usize, cell as usize);
    cell
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    back_with_control(receiver)
}

/// `-initTextCell:`: a button's title is the string, and any other cell's
/// value is.
unsafe extern "C" fn init_text_cell(receiver: Id, _sel: Sel, text: Id) -> Id {
    let cell = back_with_control(receiver);
    let setter = match is_button_cell(cell) {
        true => "setTitle:",
        false => "setStringValue:",
    };
    if responds(cell, setter) {
        // SAFETY: both setters take one string.
        unsafe { send_object(cell, setter, text) };
    }
    cell
}

fn is_button_cell(cell: Id) -> bool {
    let Some(button) = objc::class::lookup(c"NSButtonCell") else {
        return false;
    };
    // SAFETY: an object this runtime allocated.
    unsafe { objc::object::class_of(cell) }
        .is_some_and(|c| c.chain().any(|k| std::ptr::eq(k, button)))
}

fn responds(object: Id, selector: &str) -> bool {
    // SAFETY: an object this runtime allocated.
    unsafe { objc::object::class_of(object) }
        .is_some_and(|c| c.method(objc::sel::get(selector)).is_some())
}

/// `-initImageCell:`: an image and no title.
unsafe extern "C" fn init_image_cell(receiver: Id, _sel: Sel, image: Id) -> Id {
    let cell = back_with_control(receiver);
    if responds(cell, "setImage:") {
        // SAFETY: `-setImage:` takes one image.
        unsafe { send_object(cell, "setImage:", image) };
    }
    cell
}

/// # Safety
///
/// `selector` must name a method taking one object and returning nothing.
unsafe fn send_object(receiver: Id, selector: &str, value: Id) {
    // SAFETY: the caller's obligation.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), value);
    }
}

/// `-dealloc`: a cell that made its control releases it.
unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    forget_state(receiver);
    // SAFETY: an instance of a class laid out as `Cell`.
    let cell = unsafe { &mut *(receiver as *mut Cell) };
    if cell.owns_control != 0 && !cell.control.is_null() {
        let control = std::mem::replace(&mut cell.control, std::ptr::null_mut());
        cell.owns_control = 0;
        {
            let mut table = cells().lock().unwrap_or_else(|e| e.into_inner());
            if table.get(&(control as usize)) == Some(&(receiver as usize)) {
                table.remove(&(control as usize));
            }
        }
        // Something else - a matrix keeping it as a prototype - may still
        // hold the control; it is then given a new cell when one is asked for.
        objc::arc::release(control);
    }
    objc::arc::finalize(receiver);
}

/// `-copyWithZone:`: a cell of the same class with a control of its own,
/// given the original's configuration, so that a program can measure a
/// different string with it without touching the original's control.
unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *mut std::ffi::c_void) -> Id {
    // SAFETY: an object this runtime allocated.
    let Some(class) = (unsafe { objc::object::class_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    let copy = back_with_control(objc::object::create_instance(class, 0));
    // SAFETY: an instance of a class laid out as `Cell`.
    let (from, to) = unsafe {
        (
            (*(receiver as *const Cell)).control,
            (*(copy as *const Cell)).control,
        )
    };
    if !from.is_null() && !to.is_null() && from != to {
        copy_configuration(from, to);
    }
    let own: Vec<(Slot, usize)> = state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|((owner, slot), _)| *owner == receiver as usize && *slot != Slot::MenuItem)
        .map(|((_, slot), value)| (*slot, *value))
        .collect();
    for (slot, value) in own {
        set(copy, slot, value);
    }
    copy
}

/// Copy what a cell shows from one control to another of the same kind:
/// each getter and setter pair both controls answer.
fn copy_configuration(from: Id, to: Id) {
    // The button type first: it sets the defaults the rest override.
    const WORDS: &[(&str, &str)] = &[
        ("buttonType", "setButtonType:"),
        ("highlightsBy", "setHighlightsBy:"),
        ("showsStateBy", "setShowsStateBy:"),
        ("imagePosition", "setImagePosition:"),
        ("bezelStyle", "setBezelStyle:"),
        ("alignment", "setAlignment:"),
        ("lineBreakMode", "setLineBreakMode:"),
        ("maximumNumberOfLines", "setMaximumNumberOfLines:"),
        ("controlSize", "setControlSize:"),
        ("state", "setState:"),
        ("tag", "setTag:"),
        ("action", "setAction:"),
    ];
    const OBJECTS: &[(&str, &str)] = &[
        ("font", "setFont:"),
        ("textColor", "setTextColor:"),
        ("backgroundColor", "setBackgroundColor:"),
        ("title", "setTitle:"),
        ("alternateTitle", "setAlternateTitle:"),
        ("image", "setImage:"),
        ("alternateImage", "setAlternateImage:"),
        ("formatter", "setFormatter:"),
        ("target", "setTarget:"),
        ("stringValue", "setStringValue:"),
    ];
    const FLAGS: &[(&str, &str)] = &[
        ("drawsBackground", "setDrawsBackground:"),
        ("isBordered", "setBordered:"),
        ("isBezeled", "setBezeled:"),
        ("usesSingleLineMode", "setUsesSingleLineMode:"),
        ("isEditable", "setEditable:"),
        ("isSelectable", "setSelectable:"),
        ("isEnabled", "setEnabled:"),
        ("allowsMixedState", "setAllowsMixedState:"),
    ];
    let both = |getter: &str, setter: &str| responds(from, getter) && responds(to, setter);
    // SAFETY: each pair is a getter and setter of the same word-sized or
    // object type, sent to two controls of the same kind.
    unsafe {
        for (getter, setter) in WORDS.iter().chain(OBJECTS) {
            if both(getter, setter) {
                let get: unsafe extern "C" fn(Id, Sel) -> usize =
                    std::mem::transmute(objc::msg::entry());
                let set: unsafe extern "C" fn(Id, Sel, usize) =
                    std::mem::transmute(objc::msg::entry());
                set(
                    to,
                    objc::sel::get(setter),
                    get(from, objc::sel::get(getter)),
                );
            }
        }
        for (getter, setter) in FLAGS {
            if both(getter, setter) {
                let get: unsafe extern "C" fn(Id, Sel) -> u8 =
                    std::mem::transmute(objc::msg::entry());
                let set: unsafe extern "C" fn(Id, Sel, u8) =
                    std::mem::transmute(objc::msg::entry());
                set(
                    to,
                    objc::sel::get(setter),
                    get(from, objc::sel::get(getter)),
                );
            }
        }
    }
}

/// `-cellSizeForBounds:`: what the control needs, a wrapping text field's
/// text broken to the width of `bounds`.
unsafe extern "C" fn cell_size_for_bounds(
    receiver: Id,
    _sel: Sel,
    bounds: foundation::geometry::NSRect,
) -> foundation::geometry::NSSize {
    // SAFETY: an instance of a class laid out as `Cell`.
    let control = unsafe { (*(receiver as *const Cell)).control };
    if control.is_null() {
        return foundation::geometry::NSSize::default();
    }
    if let Some(size) = crate::label::cell_size(control, bounds) {
        return size;
    }
    // SAFETY: `-intrinsicContentSize` is `NSSize (id, SEL)` on every view.
    let size = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> foundation::geometry::NSSize =
            std::mem::transmute(objc::msg::entry());
        f(control, objc::sel::get("intrinsicContentSize"))
    };
    foundation::geometry::NSSize {
        width: size.width.max(0.0),
        height: size.height.max(0.0),
    }
}

unsafe extern "C" fn cell_size(receiver: Id, sel: Sel) -> foundation::geometry::NSSize {
    let unbounded = foundation::geometry::NSRect {
        origin: Default::default(),
        size: foundation::geometry::NSSize {
            width: f64::MAX,
            height: f64::MAX,
        },
    };
    // SAFETY: the same receiver.
    unsafe { cell_size_for_bounds(receiver, sel, unbounded) }
}

unsafe extern "C" fn cell(receiver: Id, _sel: Sel) -> Id {
    cell_of(receiver)
}

unsafe extern "C" fn control_view(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a cell this module made.
    unsafe { (*(receiver as *const Cell)).control }
}

/// `-accessibilitySetOverrideValue:forAttribute:`, an `NSObject` category.
/// Nothing here serves accessibility, so the override has no reader.
unsafe extern "C" fn accessibility_set_override(
    _receiver: Id,
    _sel: Sel,
    _value: Id,
    _attribute: Id,
) -> u8 {
    1
}

/// State a cell keeps itself rather than in a control, by `(cell, slot)`.
///
/// Mostly what only a cell has - control size, the first-responder ring, a
/// text cell's bezel - which a program sets on a standalone cell it draws with.
/// A cell backed by a control forwards what the control has (see
/// [`FORWARDED`]); these answer for the rest, and for a program's own `NSCell`
/// subclass, which has no control.
fn state() -> &'static Mutex<HashMap<(usize, Slot), usize>> {
    static TABLE: OnceLock<Mutex<HashMap<(usize, Slot), usize>>> = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Slot {
    ControlSize,
    ShowsFirstResponder,
    Bezeled,
    BezelStyle,
    FocusRingType,
    ControlTint,
    /// Stored inverted, so a fresh cell is enabled.
    Disabled,
    State,
    Highlighted,
    /// An `f64`'s bits.
    DoubleValue,
    Editable,
    ArrowPosition,
    UsesItemFromMenu,
    /// Retained.
    MenuItem,
}

fn get(cell: Id, slot: Slot) -> usize {
    state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(cell as usize, slot))
        .copied()
        .unwrap_or(0)
}

fn set(cell: Id, slot: Slot, value: usize) {
    state()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert((cell as usize, slot), value);
}

/// Drop everything [`state`] holds for a cell, releasing its menu item.
fn forget_state(cell: Id) {
    let item = {
        let mut table = state().lock().unwrap_or_else(|e| e.into_inner());
        table.retain(|(owner, slot), _| *owner != cell as usize || *slot == Slot::MenuItem);
        table.remove(&(cell as usize, Slot::MenuItem))
    };
    if let Some(item) = item {
        objc::arc::release(item as Id);
    }
}

macro_rules! cell_accessors {
    ($($get:ident / $set:ident : $slot:expr;)*) => {$(
        unsafe extern "C" fn $get(receiver: Id, _sel: Sel) -> usize {
            get(receiver, $slot)
        }
        unsafe extern "C" fn $set(receiver: Id, _sel: Sel, value: usize) {
            set(receiver, $slot, value);
        }
    )*};
}

cell_accessors! {
    control_size / set_control_size: Slot::ControlSize;
    bezel_style / set_bezel_style: Slot::BezelStyle;
    focus_ring_type / set_focus_ring_type: Slot::FocusRingType;
    control_tint / set_control_tint: Slot::ControlTint;
    cell_state / set_cell_state: Slot::State;
    arrow_position / set_arrow_position: Slot::ArrowPosition;
}

macro_rules! cell_flags {
    ($($get:ident / $set:ident : $slot:expr;)*) => {$(
        unsafe extern "C" fn $get(receiver: Id, _sel: Sel) -> u8 {
            u8::from(get(receiver, $slot) != 0)
        }
        unsafe extern "C" fn $set(receiver: Id, _sel: Sel, value: u8) {
            set(receiver, $slot, usize::from(value != 0));
        }
    )*};
}

cell_flags! {
    shows_first_responder / set_shows_first_responder: Slot::ShowsFirstResponder;
    is_bezeled / set_bezeled: Slot::Bezeled;
    is_highlighted / set_highlighted: Slot::Highlighted;
    is_editable / set_editable: Slot::Editable;
    uses_item_from_menu / set_uses_item_from_menu: Slot::UsesItemFromMenu;
}

unsafe extern "C" fn is_enabled(receiver: Id, _sel: Sel) -> u8 {
    u8::from(get(receiver, Slot::Disabled) == 0)
}

unsafe extern "C" fn set_enabled(receiver: Id, _sel: Sel, value: u8) {
    set(receiver, Slot::Disabled, usize::from(value == 0));
}

unsafe extern "C" fn double_value(receiver: Id, _sel: Sel) -> f64 {
    f64::from_bits(get(receiver, Slot::DoubleValue) as u64)
}

unsafe extern "C" fn set_double_value(receiver: Id, _sel: Sel, value: f64) {
    set(receiver, Slot::DoubleValue, value.to_bits() as usize);
}

unsafe extern "C" fn menu_item(receiver: Id, _sel: Sel) -> Id {
    get(receiver, Slot::MenuItem) as Id
}

unsafe extern "C" fn set_menu_item(receiver: Id, _sel: Sel, item: Id) {
    let old = get(receiver, Slot::MenuItem) as Id;
    set(receiver, Slot::MenuItem, objc::arc::retain(item) as usize);
    objc::arc::release(old);
}

/// A word-sized answer from a method taking no arguments, or `default` when
/// the receiver has no such method.
fn ask(receiver: Id, selector: &str, default: usize) -> usize {
    if !responds(receiver, selector) {
        return default;
    }
    // SAFETY: every selector asked here returns an integer, BOOL or object
    // and takes no arguments.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector))
    }
}

fn is_a(object: Id, class: &std::ffi::CStr) -> bool {
    let Some(wanted) = objc::class::lookup(class) else {
        return false;
    };
    // SAFETY: an object this runtime allocated.
    unsafe { objc::object::class_of(object) }
        .is_some_and(|c| c.chain().any(|k| std::ptr::eq(k, wanted)))
}

/// The current graphics context's `CGContext`, and whether its user space
/// grows downward.
fn current_port() -> Option<(corefoundation::base::CFTypeRef, bool)> {
    let class = objc::class::lookup(c"NSGraphicsContext")?;
    let context = ask(class as *const _ as Id, "currentContext", 0) as Id;
    if context.is_null() {
        return None;
    }
    let port = ask(context, "CGContext", 0) as corefoundation::base::CFTypeRef;
    let flipped = ask(context, "isFlipped", 0) as u8 != 0;
    (!port.is_null()).then_some((port, flipped))
}

/// What a standalone cell draws as.
enum Drawn {
    Push,
    Bevel,
    Round,
    Check,
    Radio,
    Disclosure,
    Popup { pulls_down: bool },
    Combo,
    Search,
    Field,
}

fn drawn_as(cell: Id) -> Option<Drawn> {
    if is_a(cell, c"NSPopUpButtonCell") {
        return Some(Drawn::Popup {
            pulls_down: ask(cell, "pullsDown", 0) as u8 != 0,
        });
    }
    if is_a(cell, c"NSSearchFieldCell") {
        return Some(Drawn::Search);
    }
    if is_a(cell, c"NSComboBoxCell") {
        return Some(Drawn::Combo);
    }
    if is_a(cell, c"NSTextFieldCell") {
        return (ask(cell, "isBezeled", 0) as u8 != 0).then_some(Drawn::Field);
    }
    if !is_a(cell, c"NSButtonCell") {
        return None;
    }
    // NSSwitchButton, NSRadioButton.
    match ask(cell, "buttonType", 0) {
        3 => return Some(Drawn::Check),
        4 => return Some(Drawn::Radio),
        _ => {}
    }
    if ask(cell, "isBordered", 1) as u8 == 0 {
        return None;
    }
    Some(match ask(cell, "bezelStyle", 1) {
        // NSRoundedBezelStyle
        1 => Drawn::Push,
        // NSDisclosureBezelStyle
        5 => Drawn::Disclosure,
        // Circular, help, textured rounded, round rect, recessed, inline.
        7 | 9 | 11 | 12 | 13 | 15 => Drawn::Round,
        // The square and bevel styles.
        _ => Drawn::Bevel,
    })
}

/// `-drawWithFrame:inView:`: the cell's control, as `crate::aqua` paints it,
/// into the current graphics context - for a program drawing native widgets,
/// its own bitmap context.
///
/// FIXME (ACCURACY): a cell's title and image are not drawn; the programs
/// seen to draw standalone cells draw their labels themselves.
unsafe extern "C" fn draw_with_frame(
    receiver: Id,
    _sel: Sel,
    frame: foundation::geometry::NSRect,
    _view: Id,
) {
    draw_cell(receiver, frame, false);
}

/// `-drawInteriorWithFrame:inView:`: only what sits inside the bezel, which
/// for the cells drawn here is a check box's or radio button's glyph.
unsafe extern "C" fn draw_interior_with_frame(
    receiver: Id,
    _sel: Sel,
    frame: foundation::geometry::NSRect,
    _view: Id,
) {
    draw_cell(receiver, frame, true);
}

fn draw_cell(cell: Id, frame: foundation::geometry::NSRect, interior: bool) {
    let Some(drawn) = drawn_as(cell) else {
        return;
    };
    if interior && !matches!(drawn, Drawn::Check | Drawn::Radio | Drawn::Disclosure) {
        return;
    }
    let Some((port, flipped)) = current_port() else {
        return;
    };
    let Some(p) = crate::aqua::Painter::new(port, frame, flipped) else {
        return;
    };
    let size = crate::aqua::Size::from_control_size(get(cell, Slot::ControlSize));
    let key = ask(cell, "keyEquivalent", 0) as Id;
    let look = crate::aqua::Look {
        pressed: ask(cell, "isHighlighted", 0) as u8 != 0,
        disabled: ask(cell, "isEnabled", 1) as u8 == 0,
        focused: get(cell, Slot::ShowsFirstResponder) != 0,
        // SAFETY: a string or nil.
        default: !key.is_null() && unsafe { foundation::string::bytes(key) } == b"\r",
    };
    let value = match ask(cell, "state", 0) as isize {
        0 => crate::aqua::Value::Off,
        -1 => crate::aqua::Value::Mixed,
        _ => crate::aqua::Value::On,
    };
    use crate::aqua;
    match drawn {
        Drawn::Push => aqua::push_button(&p, frame, size, look),
        Drawn::Bevel => aqua::bevel_button(&p, frame, look),
        Drawn::Round => aqua::round_button(&p, frame, look),
        Drawn::Check => aqua::check_box(&p, frame, size, value, look),
        Drawn::Radio => aqua::radio_button(&p, frame, size, value, look),
        Drawn::Disclosure => {
            let direction = if value == aqua::Value::Off { 0 } else { 1 };
            aqua::disclosure(&p, frame, direction, look)
        }
        Drawn::Popup { pulls_down } => aqua::popup_button(&p, frame, size, look, pulls_down),
        Drawn::Combo => aqua::combo_box(&p, frame, size, look),
        Drawn::Search => aqua::round_field(&p, frame, look.focused),
        Drawn::Field => aqua::text_field(&p, frame, look.focused),
    }
}

/// `-[NSPopUpButtonCell initTextCell:pullsDown:]`
unsafe extern "C" fn init_text_cell_pulls_down(
    receiver: Id,
    sel: Sel,
    text: Id,
    pulls_down: u8,
) -> Id {
    // SAFETY: the same receiver and string.
    let cell = unsafe { init_text_cell(receiver, sel, text) };
    if responds(cell, "setPullsDown:") {
        // SAFETY: `-setPullsDown:` takes one BOOL.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            f(cell, objc::sel::get("setPullsDown:"), pulls_down);
        }
    }
    cell
}

pub(crate) fn install() {
    let control = objc::class::lookup(c"NSControl").expect("declared before its methods");
    methods!(control, {
        "cell" => cell, c"@16@0:8";
    });
    let object = objc::class::lookup(c"NSObject").expect("the runtime's own");
    methods!(object, {
        "accessibilitySetOverrideValue:forAttribute:" => accessibility_set_override, c"c32@0:8@16@24";
    });
    let cell_class = objc::class::lookup(c"NSCell").expect("declared before its methods");
    methods!(cell_class, {
        "init" => init, c"@16@0:8";
        "initTextCell:" => init_text_cell, c"@24@0:8@16";
        "initImageCell:" => init_image_cell, c"@24@0:8@16";
        "dealloc" => dealloc, c"v16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
        "cellSizeForBounds:" => cell_size_for_bounds,
            c"{CGSize=dd}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "cellSize" => cell_size, c"{CGSize=dd}16@0:8";
        "controlSize" => control_size, c"Q16@0:8";
        "setControlSize:" => set_control_size, c"v24@0:8Q16";
        "bezelStyle" => bezel_style, c"Q16@0:8";
        "setBezelStyle:" => set_bezel_style, c"v24@0:8Q16";
        "focusRingType" => focus_ring_type, c"Q16@0:8";
        "setFocusRingType:" => set_focus_ring_type, c"v24@0:8Q16";
        "controlTint" => control_tint, c"Q16@0:8";
        "setControlTint:" => set_control_tint, c"v24@0:8Q16";
        "state" => cell_state, c"q16@0:8";
        "setState:" => set_cell_state, c"v24@0:8q16";
        "showsFirstResponder" => shows_first_responder, c"c16@0:8";
        "setShowsFirstResponder:" => set_shows_first_responder, c"v20@0:8c16";
        "isBezeled" => is_bezeled, c"c16@0:8";
        "setBezeled:" => set_bezeled, c"v20@0:8c16";
        "isHighlighted" => is_highlighted, c"c16@0:8";
        "setHighlighted:" => set_highlighted, c"v20@0:8c16";
        "isEditable" => is_editable, c"c16@0:8";
        "setEditable:" => set_editable, c"v20@0:8c16";
        "isEnabled" => is_enabled, c"c16@0:8";
        "setEnabled:" => set_enabled, c"v20@0:8c16";
        "doubleValue" => double_value, c"d16@0:8";
        "setDoubleValue:" => set_double_value, c"v24@0:8d16";
        "drawWithFrame:inView:" => draw_with_frame, c"v56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "drawInteriorWithFrame:inView:" => draw_interior_with_frame, c"v56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
    });
    let popup_cell =
        objc::class::lookup(c"NSPopUpButtonCell").expect("declared before its methods");
    methods!(popup_cell, {
        "initTextCell:pullsDown:" => init_text_cell_pulls_down, c"@28@0:8@16c24";
        "arrowPosition" => arrow_position, c"Q16@0:8";
        "setArrowPosition:" => set_arrow_position, c"v24@0:8Q16";
        "usesItemFromMenu" => uses_item_from_menu, c"c16@0:8";
        "setUsesItemFromMenu:" => set_uses_item_from_menu, c"v20@0:8c16";
    });
    methods!(cell_class, {
        "menuItem" => menu_item, c"@16@0:8";
        "setMenuItem:" => set_menu_item, c"v24@0:8@16";
    });
    let forward = objc::msg::entry_forward_to_first_word();
    for (control_class, cell_class) in KINDS {
        let (Some(control_class), Some(cell_class)) = (
            objc::class::lookup(control_class),
            objc::class::lookup(cell_class),
        ) else {
            continue;
        };
        methods!(cell_class, {
            "controlView" => control_view, c"@16@0:8";
        });
        for name in FORWARDED {
            let sel = objc::sel::get(name);
            let Some(method) = control_class.method(sel) else {
                continue;
            };
            // SAFETY: a registered method's types are a NUL-terminated string.
            let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
            if types.to_bytes().first() == Some(&b'{') {
                continue;
            }
            cell_class.replace_method(sel, forward, types);
        }
    }
}

#[cfg(test)]
mod tests {
    use objc::object::Id;
    use objc::sel::Sel;

    unsafe fn send(receiver: Id, selector: &str) -> usize {
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector))
        }
    }

    unsafe fn send_word(receiver: Id, selector: &str, value: usize) -> usize {
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, usize) -> usize =
                std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector), value)
        }
    }

    fn new_button_cell(title: &str) -> Id {
        let class = objc::class::lookup(c"NSButtonCell").expect("NSButtonCell");
        let cell = objc::object::create_instance(class, 0);
        let title = foundation::string::from_str(title);
        // SAFETY: `-initTextCell:` takes one string.
        let cell = unsafe { send_word(cell, "initTextCell:", title as usize) } as Id;
        objc::arc::release(title);
        cell
    }

    #[test]
    fn a_copied_text_field_cell_measures_without_changing_the_original() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSTextField").expect("NSTextField");
        let field = objc::object::create_instance(class, 0);
        let short = foundation::string::from_str("ab");
        let long = foundation::string::from_str("a much longer string than that");
        // SAFETY: each selector is one a text field or its cell answers, with
        // the argument and return types used here.
        unsafe {
            send_word(field, "setStringValue:", short as usize);
            let cell = send(field, "cell") as Id;
            let copy = send(cell, "copy") as Id;
            assert!(!copy.is_null() && copy != cell);
            send_word(copy, "setStringValue:", long as usize);
            let measure = |cell: Id| {
                let f: unsafe extern "C" fn(Id, Sel) -> foundation::geometry::NSSize =
                    std::mem::transmute(objc::msg::entry());
                f(cell, objc::sel::get("cellSize"))
            };
            assert!(measure(copy).width > measure(cell).width);
            let kept = send(field, "stringValue") as Id;
            assert_eq!(foundation::string::bytes(kept), b"ab");
            objc::arc::release(copy);
        }
        objc::arc::release(short);
        objc::arc::release(long);
    }

    #[test]
    fn a_program_made_button_cell_keeps_its_button_type() {
        crate::bootstrap();
        let cell = new_button_cell("Remember me");
        // SAFETY: each selector is one the button cell answers, with the
        // argument and return types used here.
        unsafe {
            assert!(!(send(cell, "controlView") as Id).is_null());
            assert_eq!(send(cell, "highlightsBy"), 6);
            send_word(cell, "setButtonType:", 3);
            assert_eq!(send(cell, "buttonType"), 3);
            assert_eq!(send(cell, "highlightsBy"), 1);
            assert_eq!(send(cell, "showsStateBy"), 1);
            assert_eq!(send(cell, "imagePosition"), 2);
            assert_eq!(send(cell, "isBordered") as u8, 0);
            let title = send(cell, "title") as Id;
            assert_eq!(foundation::string::bytes(title), b"Remember me");
        }
        objc::arc::release(cell);
    }

    #[test]
    fn a_button_cell_allowing_mixed_state_cycles_through_it() {
        crate::bootstrap();
        let cell = new_button_cell("");
        // SAFETY: as above.
        unsafe {
            send_word(cell, "setButtonType:", 3);
            assert_eq!(send(cell, "nextState") as isize, 1);
            send_word(cell, "setAllowsMixedState:", 1);
            assert_eq!(send(cell, "allowsMixedState") as u8, 1);
            send(cell, "setNextState");
            assert_eq!(send(cell, "state") as isize, -1);
            send(cell, "setNextState");
            assert_eq!(send(cell, "state") as isize, 1);
            send(cell, "setNextState");
            assert_eq!(send(cell, "state") as isize, 0);
        }
        objc::arc::release(cell);
    }

    #[test]
    fn a_button_cell_set_as_a_prototype_shapes_new_matrix_cells() {
        crate::bootstrap();
        let cell = new_button_cell("Option");
        // SAFETY: as above, and `-setPrototype:` takes one cell.
        let made = unsafe {
            send_word(cell, "setButtonType:", 4);
            let class = objc::class::lookup(c"NSMatrix").expect("NSMatrix");
            let matrix = objc::object::create_instance(class, 0);
            let f: unsafe extern "C" fn(Id, Sel, foundation::geometry::NSRect) -> Id =
                std::mem::transmute(objc::msg::entry());
            let matrix = f(
                matrix,
                objc::sel::get("initWithFrame:"),
                foundation::geometry::NSRect::default(),
            );
            send_word(matrix, "setPrototype:", cell as usize);
            assert_eq!(send(matrix, "prototype") as Id, cell);
            let made = crate::matrix::new_cell(matrix);
            objc::arc::release(cell);
            objc::arc::release(matrix);
            made
        };
        // SAFETY: a button.
        unsafe {
            assert_eq!(send(made, "buttonType"), 4);
            assert_eq!(
                foundation::string::bytes(send(made, "title") as Id),
                b"Option"
            );
        }
        objc::arc::release(made);
    }

    #[test]
    fn a_text_field_cell_answers_what_its_field_does() {
        crate::bootstrap();
        let cell = objc::class::lookup(c"NSTextFieldCell").expect("NSTextFieldCell");
        for name in [
            "setFont:",
            "font",
            "setAlignment:",
            "setDrawsBackground:",
            "setEditable:",
        ] {
            assert!(cell.method(objc::sel::get(name)).is_some(), "{name}");
        }
    }
}
