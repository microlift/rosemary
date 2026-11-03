//! `NSVisualEffectView`, the translucent material behind sidebars, titlebars
//! and status bars.
//!
//! FIXME (ACCURACY): nothing behind a window can be seen through it here, so
//! a material is drawn as the opaque colour it averages to.

use foundation::geometry::NSRect;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::props::{self, properties};

/// `NSVisualEffectMaterial`s that are not the window background's grey.
const MATERIAL_TITLEBAR: usize = 3;
const MATERIAL_SELECTION: usize = 4;
const MATERIAL_MENU: usize = 5;
const MATERIAL_SIDEBAR: usize = 7;
const MATERIAL_HEADER_VIEW: usize = 10;
const MATERIAL_CONTENT_BACKGROUND: usize = 18;

unsafe extern "C" fn draw_rect(receiver: Id, _sel: Sel, dirty: NSRect) {
    let name = match props::integer(receiver, "material") {
        MATERIAL_SELECTION => "selectedContentBackgroundColor",
        MATERIAL_CONTENT_BACKGROUND => "controlBackgroundColor",
        MATERIAL_MENU => "controlBackgroundColor",
        MATERIAL_TITLEBAR | MATERIAL_SIDEBAR | MATERIAL_HEADER_VIEW => "windowBackgroundColor",
        _ => "windowBackgroundColor",
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
    crate::color::NSRectFill(dirty);
}

/// `-interiorBackgroundStyle`: light for every material drawn here.
unsafe extern "C" fn interior_background_style(_receiver: Id, _sel: Sel) -> usize {
    0
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSVisualEffectView").expect("declared before its methods");
    properties!(class, {
        integer "material", "setMaterial:";
        integer "blendingMode", "setBlendingMode:";
        integer "state", "setState:";
        bool "isEmphasized", "setEmphasized:";
        retained "maskImage", "setMaskImage:";
    });
    methods!(class, {
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "interiorBackgroundStyle" => interior_background_style, c"q16@0:8";
    });
}
