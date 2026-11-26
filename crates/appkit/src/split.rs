//! `NSSplitView`: panes laid out side by side or one above another, and the
//! dividers between them - their style and colour, and how one is drawn.
//!
//! Qt's macOS style draws its splitter handles by asking a split view to draw
//! a divider into the style's own context.
//!
//! FIXME (ACCURACY): dividers cannot be dragged, collapsing is not modelled,
//! and positions are not autosaved.

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::aqua;
use crate::define::methods;

/// `NSSplitViewDividerStyle`.
const THICK: isize = 1;
const THIN: isize = 2;
const PANE_SPLITTER: isize = 3;

/// `NSLayoutPriorityDefaultLow`: a pane's holding priority until one is set.
const DEFAULT_HOLDING_PRIORITY: f32 = 250.0;

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
    indexed_panes(receiver)
        .into_iter()
        .map(|(_, pane)| pane)
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

/// Whether `view` is a pane of a split view, which places it.
pub(crate) fn is_pane(view: Id) -> bool {
    let Some(split) = objc::class::lookup(c"NSSplitView") else {
        return false;
    };
    // SAFETY: a view of this crate's.
    let parent = unsafe { crate::view::superview_of(view) };
    // SAFETY: a live view or nil.
    crate::view::is_view(parent) && unsafe { corefoundation::base::is_kind_of(parent.cast(), split) }
}

/// The property a subview's holding priority is kept under, by its index.
fn holding_key(index: usize) -> String {
    format!("holdingPriorityForSubview{index}")
}

unsafe extern "C" fn holding_priority(receiver: Id, _sel: Sel, index: isize) -> f32 {
    holding_priority_of(receiver, index as usize)
}

fn holding_priority_of(receiver: Id, index: usize) -> f32 {
    match crate::props::integer(receiver, &holding_key(index)) {
        0 => DEFAULT_HOLDING_PRIORITY,
        bits => f32::from_bits(bits as u32),
    }
}

unsafe extern "C" fn set_holding_priority(receiver: Id, _sel: Sel, priority: f32, index: isize) {
    crate::props::set_integer_of(
        receiver,
        &holding_key(index as usize),
        priority.to_bits() as usize,
    );
}

/// The visible panes, each with its index among the subviews, which is what a
/// holding priority is given for.
fn indexed_panes(receiver: Id) -> Vec<(usize, Id)> {
    crate::view::subviews_of(receiver)
        .into_iter()
        .enumerate()
        .filter(|(_, pane)| !crate::view::is_hidden_or_has_hidden_ancestor(*pane))
        .collect()
}

/// `sizes` grown or shrunk by `delta` in all, the panes of the lowest holding
/// priority first and shared evenly among equals; a pane given more than it
/// has to lose passes the rest to the next priority.
fn held(sizes: &[f64], priorities: &[f32], mut delta: f64) -> Vec<f64> {
    let mut sizes = sizes.to_vec();
    let mut levels: Vec<f32> = priorities.to_vec();
    levels.sort_by(f32::total_cmp);
    levels.dedup();
    for level in levels {
        let mut group: Vec<usize> = (0..sizes.len())
            .filter(|i| priorities[*i] == level)
            .collect();
        while delta.abs() > 0.001 && !group.is_empty() {
            let share = delta / group.len() as f64;
            let mut exhausted = Vec::new();
            for &i in &group {
                let change = share.max(-sizes[i]);
                sizes[i] += change;
                delta -= change;
                if sizes[i] <= 0.0 {
                    exhausted.push(i);
                }
            }
            if exhausted.is_empty() {
                break;
            }
            group.retain(|i| !exhausted.contains(i));
        }
    }
    sizes
}

/// `-adjustSubviews`: the panes resized so that they and the dividers between
/// them fill the split view, those of the lowest holding priority taking the
/// change.
///
/// FIXME (ACCURACY): panes that all hold at one priority share the change in
/// proportion to their sizes, as AppKit did before holding priorities; which
/// of the two AppKit does then is not observed.
unsafe extern "C" fn adjust_subviews(receiver: Id, _sel: Sel) {
    let indexed = indexed_panes(receiver);
    if indexed.is_empty() {
        return;
    }
    let along = along_axis(receiver);
    let panes: Vec<Id> = indexed.iter().map(|(_, pane)| *pane).collect();
    let sizes: Vec<f64> = panes
        .iter()
        .map(|pane| along(frame_of(*pane).size))
        .collect();
    let priorities: Vec<f32> = indexed
        .iter()
        .map(|(index, _)| holding_priority_of(receiver, *index))
        .collect();
    let available = available(receiver, panes.len());
    let sizes = match priorities.windows(2).all(|pair| pair[0] == pair[1]) {
        true => proportional(&sizes, available),
        false => held(&sizes, &priorities, available - sizes.iter().sum::<f64>()),
    };
    place(receiver, &panes, &sizes);
}

/// The panes' extent along the axis the split view divides.
fn along_axis(receiver: Id) -> impl Fn(NSSize) -> f64 {
    let vertical = is_vertical(receiver);
    move |size: NSSize| if vertical { size.width } else { size.height }
}

/// The room the panes have between the dividers.
fn available(receiver: Id, panes: usize) -> f64 {
    let along = along_axis(receiver);
    along(crate::view::bounds_of(receiver).size) - thickness(receiver) * (panes - 1) as f64
}

/// Frame each pane at its size along the axis, across the whole split view.
fn place(receiver: Id, panes: &[Id], sizes: &[f64]) {
    let bounds = crate::view::bounds_of(receiver);
    let vertical = is_vertical(receiver);
    let divider = thickness(receiver);
    let mut position = 0.0;
    for (pane, size) in panes.iter().zip(sizes.iter().copied()) {
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
        "holdingPriorityForSubviewAtIndex:" => holding_priority, c"f24@0:8q16";
        "setHoldingPriority:forSubviewAtIndex:" => set_holding_priority, c"v28@0:8f16q20";
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
    fn the_pane_holding_lowest_takes_the_change() {
        assert_eq!(held(&[261.0, 910.0], &[470.0, 250.0], -22.0), [261.0, 888.0]);
        assert_eq!(held(&[100.0, 100.0], &[250.0, 250.0], 50.0), [125.0, 125.0]);
        // A pane that cannot shrink further passes the rest on.
        assert_eq!(held(&[100.0, 10.0], &[470.0, 250.0], -30.0), [80.0, 0.0]);
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
