//! `NSVisualEffectView`, the translucent material behind sidebars, titlebars
//! and status bars.
//!
//! FIXME (ACCURACY): nothing behind a window can be seen through it here, so
//! a material is drawn as the opaque colour it averages to.

use foundation::geometry::{NSPoint, NSRect};
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

/// `NSVisualEffectBlendingModeBehindWindow`, which is also the default.
const BEHIND_WINDOW: usize = 0;

/// Whether `view` shows what is behind its window rather than what is behind
/// it in the window.
pub(crate) fn blends_behind_window(view: Id) -> bool {
    let Some(class) = objc::class::lookup(c"NSVisualEffectView") else {
        return false;
    };
    // SAFETY: a live view.
    let is_effect_view = unsafe { corefoundation::base::is_kind_of(view.cast(), class) };
    is_effect_view && props::integer(view, "blendingMode") == BEHIND_WINDOW
}

/// `-hitTest:`: a view blending behind its window is the window's background,
/// so a point that is in none of its subviews passes through to what is
/// drawn over it.
///
/// FIXME (ACCURACY): inferred from CrossOver, whose source lists are below
/// such a view in their pane and take clicks on macOS.
unsafe extern "C" fn hit_test(receiver: Id, _sel: Sel, point: NSPoint) -> Id {
    // SAFETY: a view of this crate's, and a point in its superview's space.
    let hit = unsafe { crate::view::hit_base(receiver, point) };
    match hit == receiver && blends_behind_window(receiver) {
        true => std::ptr::null_mut(),
        false => hit,
    }
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
        "hitTest:" => hit_test, c"@32@0:8{CGPoint=dd}16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(class: &std::ffi::CStr, frame: NSRect) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(class).expect("a view class");
        let object = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(object, objc::sel::get("initWithFrame:"), frame)
        }
    }

    fn rect(width: f64, height: f64) -> NSRect {
        NSRect {
            origin: NSPoint::default(),
            size: foundation::geometry::NSSize { width, height },
        }
    }

    fn add(parent: Id, child: Id) {
        // SAFETY: `-addSubview:` is `void (id, SEL, NSView *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(parent, objc::sel::get("addSubview:"), child);
        }
    }

    /// A view blending behind the window is the window's background: a click
    /// over it reaches the sibling drawn under it in the tree.
    #[test]
    fn a_behind_window_effect_view_passes_a_click_to_what_is_under_it() {
        let pane = view(c"NSView", rect(100.0, 100.0));
        let list = view(c"NSView", rect(100.0, 100.0));
        let effect = view(c"NSVisualEffectView", rect(100.0, 100.0));
        add(pane, list);
        add(pane, effect);
        let point = NSPoint { x: 50.0, y: 50.0 };
        // SAFETY: views of this crate's.
        assert_eq!(unsafe { crate::view::hit_test(pane, point) }, list);
        props::set_integer_of(effect, "blendingMode", 1);
        // SAFETY: as above.
        assert_eq!(unsafe { crate::view::hit_test(pane, point) }, effect);
    }
}
