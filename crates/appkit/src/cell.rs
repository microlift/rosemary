//! `-[NSControl cell]` and the `NSCell` family.
//!
//! Every control here keeps its own state, where Apple's keeps it in a cell and
//! forwards to it. A cell is therefore the reverse: a small object that names
//! its control and forwards the shared accessors to it, so a program that
//! reaches through `-cell` reads and writes the same state the control shows.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// `isa`, then the control. Not retained: a control owns its cell, and the
/// cell is dropped in [`forget`] when the control goes.
#[repr(C)]
struct Cell {
    isa: *const std::ffi::c_void,
    control: Id,
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
