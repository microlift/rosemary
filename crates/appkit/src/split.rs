//! `NSSplitView`'s dividers: their style and colour, and how one is drawn.
//!
//! Qt's macOS style draws its splitter handles by asking a split view to draw
//! a divider into the style's own context.
//!
//! FIXME (ACCURACY): a split view does not lay out or resize its subviews.

use foundation::geometry::NSRect;
use objc::object::Id;
use objc::sel::Sel;

use crate::aqua;
use crate::define::methods;

/// `NSSplitViewDividerStyle`.
const THICK: isize = 1;

/// `-dividerStyle`, thick until set, as AppKit's default is.
unsafe extern "C" fn divider_style(receiver: Id, _sel: Sel) -> isize {
    match crate::props::integer(receiver, "dividerStyle") as isize {
        0 => THICK,
        style => style,
    }
}

unsafe extern "C" fn set_divider_style(receiver: Id, _sel: Sel, style: isize) {
    crate::props::set_integer_of(receiver, "dividerStyle", style as usize);
    crate::backing::needs_display(receiver);
}

/// `-dividerColor`: clear for thick dividers, which show only their dimple.
///
/// FIXME (ACCURACY): thin and pane-splitter dividers use the separator colour,
/// and a thin divider in a textured window is not made clear.
unsafe extern "C" fn divider_color(receiver: Id, sel: Sel) -> Id {
    // SAFETY: the same receiver.
    let name = match unsafe { divider_style(receiver, sel) } {
        THICK => "clearColor",
        _ => "separatorColor",
    };
    crate::color::named_color(name).expect("a named colour of this runtime")
}

/// `-drawDividerInRect:`: the divider colour over `rect`, and a thick
/// divider's dimple at its centre.
unsafe extern "C" fn draw_divider_in_rect(receiver: Id, sel: Sel, rect: NSRect) {
    let Some((port, flipped)) = crate::cell::current_port() else {
        return;
    };
    let Some(p) = aqua::Painter::new(port, rect, flipped) else {
        return;
    };
    // Sent rather than called, so a subclass's colour is the one drawn.
    // SAFETY: `-dividerColor` is `id (id, SEL)`.
    let color = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("dividerColor"))
    };
    if !color.is_null() {
        // SAFETY: an `NSColor`.
        let (r, g, b, a) = unsafe { crate::color::components(color) };
        aqua::fill(&p, rect, [r, g, b, a]);
    }
    // SAFETY: the same receiver.
    if unsafe { divider_style(receiver, sel) } == THICK {
        aqua::split_dimple(&p, rect);
    }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSSplitView").expect("declared before its methods");
    methods!(class, {
        "dividerStyle" => divider_style, c"q16@0:8";
        "setDividerStyle:" => set_divider_style, c"v24@0:8q16";
        "dividerColor" => divider_color, c"@16@0:8";
        "drawDividerInRect:" => draw_divider_in_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `NSSplitViewDividerStyleThin`.
    const THIN: isize = 2;

    fn new_split_view() -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSSplitView").expect("NSSplitView");
        objc::object::create_instance(class, 0)
    }

    fn alpha_of_divider(view: Id) -> f64 {
        // SAFETY: `-dividerColor` takes only the receiver, and answers a colour.
        unsafe { crate::color::components(divider_color(view, std::ptr::null())).3 }
    }

    #[test]
    fn a_new_split_view_has_clear_thick_dividers() {
        let view = new_split_view();
        // SAFETY: the accessor takes only the receiver.
        assert_eq!(unsafe { divider_style(view, std::ptr::null()) }, THICK);
        assert_eq!(alpha_of_divider(view), 0.0);
    }

    #[test]
    fn a_thin_divider_is_drawn_in_a_visible_colour() {
        let view = new_split_view();
        // SAFETY: the accessors take the receiver and the style.
        unsafe { set_divider_style(view, std::ptr::null(), THIN) };
        // SAFETY: as above.
        assert_eq!(unsafe { divider_style(view, std::ptr::null()) }, THIN);
        assert!(alpha_of_divider(view) > 0.0);
    }
}
