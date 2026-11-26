//! `NSSplitView`: panes laid out side by side or one above another, and the
//! dividers between them - their style and colour, and how one is drawn.
//!
//! Qt's macOS style draws its splitter handles by asking a split view to draw
//! a divider into the style's own context.
//!
//! FIXME (ACCURACY): dividers cannot be dragged, holding priorities and
//! collapsing are not modelled, and positions are not autosaved.

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::aqua;
use crate::define::methods;

/// `NSSplitViewDividerStyle`.
const THICK: isize = 1;
const THIN: isize = 2;
const PANE_SPLITTER: isize = 3;

/// How wide each style's divider is, in points.
const THICK_WIDTH: f64 = 9.0;
const THIN_WIDTH: f64 = 1.0;
const PANE_SPLITTER_WIDTH: f64 = 10.0;

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

unsafe extern "C" fn divider_thickness(receiver: Id, sel: Sel) -> f64 {
    // SAFETY: the same receiver.
    match unsafe { divider_style(receiver, sel) } {
        THIN => THIN_WIDTH,
        PANE_SPLITTER => PANE_SPLITTER_WIDTH,
        _ => THICK_WIDTH,
    }
}

unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> u8 {
    1
}

fn is_vertical(receiver: Id) -> bool {
    crate::props::integer(receiver, "vertical") != 0
}

unsafe extern "C" fn set_vertical(receiver: Id, sel: Sel, vertical: u8) {
    // SAFETY: the property setter.
    unsafe { crate::props::set_bool(receiver, sel, vertical) };
    send(receiver, "adjustSubviews");
}

fn send(receiver: Id, sel: &str) {
    // SAFETY: `void (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel));
    }
}

/// `sizes` scaled to fill `available` in proportion, whole points each, the
/// last taking what rounding leaves; equal shares when all are empty.
fn proportional(sizes: &[f64], available: f64) -> Vec<f64> {
    let total: f64 = sizes.iter().sum();
    let available = available.max(0.0);
    let mut scaled: Vec<f64> = sizes
        .iter()
        .map(|size| match total > 0.0 {
            true => (size * available / total).floor(),
            false => (available / sizes.len() as f64).floor(),
        })
        .collect();
    let assigned: f64 = scaled.iter().sum();
    if let Some(last) = scaled.last_mut() {
        *last += available - assigned;
    }
    scaled
}

/// The visible panes, in order.
fn panes(receiver: Id) -> Vec<Id> {
    crate::view::subviews_of(receiver)
        .into_iter()
        .filter(|pane| !crate::view::is_hidden_or_has_hidden_ancestor(*pane))
        .collect()
}

fn thickness(receiver: Id) -> f64 {
    // SAFETY: `CGFloat (id, SEL)`, sent so a subclass's width is used.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("dividerThickness"))
    }
}

fn frame_of(view: Id) -> NSRect {
    // SAFETY: `NSRect (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSRect = std::mem::transmute(objc::msg::entry());
        f(view, objc::sel::get("frame"))
    }
}

/// `-adjustSubviews`: the panes resized in proportion so that they and the
/// dividers between them fill the split view, as AppKit's default does.
unsafe extern "C" fn adjust_subviews(receiver: Id, _sel: Sel) {
    let panes = panes(receiver);
    if panes.is_empty() {
        return;
    }
    let bounds = crate::view::bounds_of(receiver);
    let vertical = is_vertical(receiver);
    let along = |size: NSSize| if vertical { size.width } else { size.height };
    let divider = thickness(receiver);
    let available = along(bounds.size) - divider * (panes.len() - 1) as f64;
    let sizes: Vec<f64> = panes
        .iter()
        .map(|pane| along(frame_of(*pane).size))
        .collect();
    let mut position = 0.0;
    for (pane, size) in panes.iter().zip(proportional(&sizes, available)) {
        let frame = match vertical {
            true => NSRect {
                origin: NSPoint {
                    x: bounds.origin.x + position,
                    y: bounds.origin.y,
                },
                size: NSSize {
                    width: size,
                    height: bounds.size.height,
                },
            },
            false => NSRect {
                origin: NSPoint {
                    x: bounds.origin.x,
                    y: bounds.origin.y + position,
                },
                size: NSSize {
                    width: bounds.size.width,
                    height: size,
                },
            },
        };
        // SAFETY: `void (id, SEL, NSRect)`, sent so a pane's override runs.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
            f(*pane, objc::sel::get("setFrame:"), frame);
        }
        position += size + divider;
    }
    crate::backing::needs_display(receiver);
}

/// `-resizeSubviewsWithOldSize:`: the delegate's layout if it has one, or
/// `-adjustSubviews`.
unsafe extern "C" fn resize_subviews_with_old_size(receiver: Id, _sel: Sel, old: NSSize) {
    let delegate = crate::props::object(receiver, "delegate");
    let sel = objc::sel::get("splitView:resizeSubviewsWithOldSize:");
    // SAFETY: a live object or nil.
    let delegates = !delegate.is_null()
        && unsafe { objc::object::class_of(delegate) }.is_some_and(|c| c.method(sel).is_some());
    if !delegates {
        send(receiver, "adjustSubviews");
        return;
    }
    // SAFETY: `void (id, SEL, NSSplitView *, NSSize)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, NSSize) = std::mem::transmute(objc::msg::entry());
        f(delegate, sel, receiver, old);
    }
}

/// `-drawRect:`: each divider, through `-drawDividerInRect:`.
unsafe extern "C" fn draw_rect(receiver: Id, _sel: Sel, _dirty: NSRect) {
    let panes = panes(receiver);
    let bounds = crate::view::bounds_of(receiver);
    let divider = thickness(receiver);
    for pane in panes.iter().take(panes.len().saturating_sub(1)) {
        let frame = frame_of(*pane);
        let rect = match is_vertical(receiver) {
            true => NSRect {
                origin: NSPoint {
                    x: frame.origin.x + frame.size.width,
                    y: bounds.origin.y,
                },
                size: NSSize {
                    width: divider,
                    height: bounds.size.height,
                },
            },
            false => NSRect {
                origin: NSPoint {
                    x: bounds.origin.x,
                    y: frame.origin.y + frame.size.height,
                },
                size: NSSize {
                    width: bounds.size.width,
                    height: divider,
                },
            },
        };
        // SAFETY: `void (id, SEL, NSRect)`, sent so a subclass draws its own.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get("drawDividerInRect:"), rect);
        }
    }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSSplitView").expect("declared before its methods");
    crate::props::properties!(class, {
        assigned "delegate", "setDelegate:";
    });
    methods!(class, {
        "isFlipped" => yes, c"c16@0:8";
        "isVertical" => crate::props::get_bool, c"c16@0:8";
        "setVertical:" => set_vertical, c"v20@0:8c16";
        "dividerThickness" => divider_thickness, c"d16@0:8";
        "adjustSubviews" => adjust_subviews, c"v16@0:8";
        "resizeSubviewsWithOldSize:" => resize_subviews_with_old_size, c"v32@0:8{CGSize=dd}16";
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "dividerStyle" => divider_style, c"q16@0:8";
        "setDividerStyle:" => set_divider_style, c"v24@0:8q16";
        "dividerColor" => divider_color, c"@16@0:8";
        "drawDividerInRect:" => draw_divider_in_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        // FIXME (ACCURACY): kept only: divider positions are neither saved to
        // nor restored from the user defaults under this name.
        "autosaveName" => crate::props::get_object, c"@16@0:8";
        "setAutosaveName:" => crate::props::set_copied, c"v24@0:8@16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn panes_keep_their_proportions_and_fill_the_space() {
        assert_eq!(proportional(&[100.0, 300.0], 801.0), [200.0, 601.0]);
        assert_eq!(proportional(&[0.0, 0.0], 10.0), [5.0, 5.0]);
        assert_eq!(proportional(&[50.0], 120.0), [120.0]);
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
