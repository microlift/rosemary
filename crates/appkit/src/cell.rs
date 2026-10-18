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

/// `-initTextCell:`: the title is the string.
unsafe extern "C" fn init_text_cell(receiver: Id, _sel: Sel, text: Id) -> Id {
    let cell = back_with_control(receiver);
    // SAFETY: `-setTitle:` takes one string on every button.
    unsafe { send_object(cell, "setTitle:", text) };
    cell
}

/// `-initImageCell:`: an image and no title.
unsafe extern "C" fn init_image_cell(receiver: Id, _sel: Sel, image: Id) -> Id {
    let cell = back_with_control(receiver);
    // SAFETY: `-setImage:` takes one image on every button.
    unsafe { send_object(cell, "setImage:", image) };
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

pub(crate) fn install() {
    let control = objc::class::lookup(c"NSControl").expect("declared before its methods");
    methods!(control, {
        "cell" => cell, c"@16@0:8";
    });
    let object = objc::class::lookup(c"NSObject").expect("the runtime's own");
    methods!(object, {
        "accessibilitySetOverrideValue:forAttribute:" => accessibility_set_override, c"c32@0:8@16@24";
    });
    let button_cell = objc::class::lookup(c"NSButtonCell").expect("declared before its methods");
    methods!(button_cell, {
        "init" => init, c"@16@0:8";
        "initTextCell:" => init_text_cell, c"@24@0:8@16";
        "initImageCell:" => init_image_cell, c"@24@0:8@16";
    });
    let cell_class = objc::class::lookup(c"NSCell").expect("declared before its methods");
    methods!(cell_class, {
        "dealloc" => dealloc, c"v16@0:8";
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
