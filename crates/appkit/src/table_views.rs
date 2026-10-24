//! The views a view-based `NSTableView` is made of: `NSTableCellView`, the
//! usual cell, and `NSTableRowView`, which draws a row's background and
//! selection behind its cells.

use foundation::geometry::NSRect;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::props::{self, properties};

/// `NSBackgroundStyle`.
const BACKGROUND_NORMAL: usize = 0;
const BACKGROUND_EMPHASIZED: usize = 1;

/// `NSTableViewSelectionHighlightStyle`.
const HIGHLIGHT_NONE: usize = usize::MAX;

/// `-setBackgroundStyle:` on a cell view: told to its controls too, as
/// AppKit does, so their text turns white on a dark selection.
unsafe extern "C" fn cell_set_background_style(receiver: Id, sel: Sel, style: usize) {
    unsafe { props::set_integer(receiver, sel, style) };
    for name in ["textField", "imageView"] {
        let control = props::object(receiver, name);
        if control.is_null() {
            continue;
        }
        // SAFETY: a cell-owning control; `-cell` and `-setBackgroundStyle:`
        // are `NSControl`'s and `NSCell`'s.
        unsafe {
            let cell: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            let cell = cell(control, objc::sel::get("cell"));
            if cell.is_null() || !responds(cell, "setBackgroundStyle:") {
                continue;
            }
            let set: unsafe extern "C" fn(Id, Sel, usize) = std::mem::transmute(objc::msg::entry());
            set(cell, objc::sel::get("setBackgroundStyle:"), style);
        }
    }
}

fn responds(receiver: Id, selector: &str) -> bool {
    // SAFETY: a live object.
    unsafe { objc::object::class_of(receiver) }
        .is_some_and(|c| c.method(objc::sel::get(selector)).is_some())
}

/// `-interiorBackgroundStyle`: what the row's cells should draw against.
unsafe extern "C" fn interior_background_style(receiver: Id, _sel: Sel) -> usize {
    let selected = props::integer(receiver, "selected") != 0;
    let emphasized = props::integer(receiver, "emphasized") != 0;
    match selected && emphasized {
        true => BACKGROUND_EMPHASIZED,
        false => BACKGROUND_NORMAL,
    }
}

/// `-drawRect:` on a row view: background, then selection, as AppKit splits
/// it so a subclass can override either.
unsafe extern "C" fn row_draw_rect(receiver: Id, _sel: Sel, dirty: NSRect) {
    // SAFETY: both are `void (id, SEL, NSRect)`, sent so an override runs.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("drawBackgroundInRect:"), dirty);
        if props::integer(receiver, "selected") != 0 {
            f(receiver, objc::sel::get("drawSelectionInRect:"), dirty);
        }
    }
}

/// `-drawBackgroundInRect:`: the row's background colour, if it has one.
unsafe extern "C" fn row_draw_background(receiver: Id, _sel: Sel, dirty: NSRect) {
    let color = props::object(receiver, "backgroundColor");
    if color.is_null() {
        return;
    }
    // SAFETY: an NSColor, sent `-set`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        f(color, objc::sel::get("set"));
    }
    crate::color::NSRectFill(dirty);
}

/// `-drawSelectionInRect:`: the selection colour across the row's bounds,
/// the accent colour when emphasized and grey when not.
unsafe extern "C" fn row_draw_selection(receiver: Id, _sel: Sel, _dirty: NSRect) {
    if props::integer(receiver, "selectionHighlightStyle") == HIGHLIGHT_NONE {
        return;
    }
    let name = match props::integer(receiver, "emphasized") != 0 {
        true => "selectedContentBackgroundColor",
        false => "unemphasizedSelectedContentBackgroundColor",
    };
    // SAFETY: `+[NSColor ...]` answers a colour, sent `-set`.
    unsafe {
        let class = objc::class::lookup(c"NSColor").expect("NSColor");
        let get: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        let color = get(objc::class::ptr(class) as Id, objc::sel::get(name));
        if color.is_null() {
            return;
        }
        let set: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        set(color, objc::sel::get("set"));
    }
    crate::color::NSRectFill(crate::view::bounds_of(receiver));
}

unsafe extern "C" fn row_set_selected(receiver: Id, sel: Sel, selected: u8) {
    unsafe { props::set_bool(receiver, sel, selected) };
    crate::backing::needs_display(receiver);
}

unsafe extern "C" fn nothing_in_rect(_receiver: Id, _sel: Sel, _rect: NSRect) {}

pub(crate) fn install() {
    let cell = objc::class::lookup(c"NSTableCellView").expect("declared before its methods");
    properties!(cell, {
        assigned "textField", "setTextField:";
        assigned "imageView", "setImageView:";
        retained "objectValue", "setObjectValue:";
        integer "rowSizeStyle", "setRowSizeStyle:";
    });
    methods!(cell, {
        "backgroundStyle" => props::get_integer, c"q16@0:8";
        "setBackgroundStyle:" => cell_set_background_style, c"v24@0:8q16";
    });

    let row = objc::class::lookup(c"NSTableRowView").expect("declared before its methods");
    properties!(row, {
        bool "isEmphasized", "setEmphasized:";
        bool "isGroupRowStyle", "setGroupRowStyle:";
        bool "isFloating", "setFloating:";
        bool "isTargetForDropOperation", "setTargetForDropOperation:";
        bool "isNextRowSelected", "setNextRowSelected:";
        bool "isPreviousRowSelected", "setPreviousRowSelected:";
        integer "selectionHighlightStyle", "setSelectionHighlightStyle:";
        integer "draggingDestinationFeedbackStyle", "setDraggingDestinationFeedbackStyle:";
        retained "backgroundColor", "setBackgroundColor:";
    });
    methods!(row, {
        "isSelected" => props::get_bool, c"c16@0:8";
        "setSelected:" => row_set_selected, c"v20@0:8c16";
        "interiorBackgroundStyle" => interior_background_style, c"q16@0:8";
        "drawRect:" => row_draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "drawBackgroundInRect:" => row_draw_background, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "drawSelectionInRect:" => row_draw_selection, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "drawSeparatorInRect:" => nothing_in_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "drawDraggingDestinationFeedbackInRect:" => nothing_in_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
    });
}
