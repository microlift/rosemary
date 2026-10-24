use std::cell::Cell;
use std::ffi::c_void;
use std::sync::Mutex;

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};
use crate::view;

#[repr(C)]
pub struct Constraint {
    isa: *const c_void,
    /// Unretained, as AppKit's are: a constraint does not keep its views alive,
    /// and [`forget`] drops it from the active set when one of them goes.
    first: Id,
    second: Id,
    first_attribute: isize,
    second_attribute: isize,
    relation: isize,
    multiplier: f64,
    constant: f64,
    priority: f32,
    active: usize,
}

pub const INSTANCE_SIZE: usize = size_of::<Constraint>();

/// An `NSLayoutAnchor`: one attribute of one item, which is all a constraint
/// made from anchors is built of.
#[repr(C)]
pub struct Anchor {
    isa: *const c_void,
    /// Unretained, as a constraint's items are.
    item: Id,
    attribute: isize,
}

pub const ANCHOR_INSTANCE_SIZE: usize = size_of::<Anchor>();

/// `NSLayoutRelation`'s third value.
const GREATER_OR_EQUAL: isize = 1;

/// `NSLayoutPriorityDefaultLow` and `NSLayoutPriorityDefaultHigh`, AppKit's
/// defaults for hugging and for compression resistance.
const DEFAULT_HUGGING: f32 = 250.0;
const DEFAULT_COMPRESSION_RESISTANCE: f32 = 750.0;

/// Each view's content hugging and compression resistance, horizontal then
/// vertical, for the views that were given any.
// FIXME (ACCURACY): recorded and answered, but the solver here has no notion
// of priorities between a view's content size and its constraints.
static CONTENT_PRIORITIES: Mutex<Vec<(usize, [f32; 4])>> = Mutex::new(Vec::new());

const REQUIRED: f32 = 1000.0;

/// `NSLayoutRelation`.
const LESS_OR_EQUAL: isize = -1;
const EQUAL: isize = 0;

/// The active constraints, each retained, as the views they constrain would
/// retain them in AppKit.
static ACTIVE: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// The active set and the views it moves are process-wide, so only one thread
/// may run a solve at a time. A re-entrant solve on the solving thread is
/// caught by [`SOLVING`] before it reaches this lock.
static SOLVE_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static SOLVING: Cell<bool> = const { Cell::new(false) };
    static DIRTY: Cell<bool> = const { Cell::new(false) };
}

fn constraint_of<'a>(receiver: Id) -> &'a mut Constraint {
    // SAFETY: a constraint of this module's, which is what every method here is
    // sent to.
    unsafe { &mut *(receiver as *mut Constraint) }
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    X,
    Y,
}

/// Which quantity along an axis an attribute names; the index into
/// [`Known::values`].
const MIN: usize = 0;
const MAX: usize = 1;
const CENTER: usize = 2;
const SIZE: usize = 3;

/// `NSLayoutAttribute`, along an axis of a view whose superview is `flipped`.
///
/// Margins and baselines are the edges they sit nearest: an AppKit view has no
/// layout margins of its own, and a view here has no text to measure a
/// baseline from.
fn classify(attribute: isize, flipped: bool) -> Option<(Axis, usize)> {
    let (top, bottom) = if flipped { (MIN, MAX) } else { (MAX, MIN) };
    Some(match attribute {
        1 | 5 | 13 | 17 => (Axis::X, MIN),
        2 | 6 | 14 | 18 => (Axis::X, MAX),
        9 | 19 => (Axis::X, CENTER),
        7 => (Axis::X, SIZE),
        3 | 12 | 15 => (Axis::Y, top),
        4 | 11 | 16 => (Axis::Y, bottom),
        10 | 20 => (Axis::Y, CENTER),
        8 => (Axis::Y, SIZE),
        _ => return None,
    })
}

fn along(rect: NSRect, axis: Axis) -> (f64, f64) {
    match axis {
        Axis::X => (rect.origin.x, rect.size.width),
        Axis::Y => (rect.origin.y, rect.size.height),
    }
}

fn measure(rect: NSRect, axis: Axis, what: usize) -> f64 {
    let (origin, size) = along(rect, axis);
    match what {
        MIN => origin,
        MAX => origin + size,
        CENTER => origin + size / 2.0,
        _ => size,
    }
}

/// `item`'s rectangle in the coordinates of `space`, a view's superview.
fn rect_in(item: Id, space: Id) -> Option<NSRect> {
    if !view::is_view(item) {
        return None;
    }
    let bounds = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: view::frame_of(item).size,
    };
    if item == space {
        return Some(bounds);
    }
    Some(view::convert_rect(bounds, item, space))
}

pub(crate) fn is_flipped(view: Id) -> bool {
    let sel = objc::sel::get("isFlipped");
    // SAFETY: `-isFlipped` is `BOOL (id, SEL)`, sent so a guest override answers.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(view, sel) != 0
    }
}

/// What the constraints say about one axis of one view.
#[derive(Default)]
struct Known {
    values: [Option<f64>; 4],
    smallest: Option<f64>,
    largest: Option<f64>,
}

impl Known {
    /// How many of the edges and centre along this axis are known.
    fn positions(&self) -> usize {
        let [min, max, center, _] = self.values;
        [min, max, center].iter().filter(|v| v.is_some()).count()
    }

    /// Whether both the position and the size along this axis are known.
    fn determined(&self) -> bool {
        let positions = self.positions();
        positions >= 2 || (positions == 1 && self.values[SIZE].is_some())
    }

    /// Take what `other` knows where this does not, unless this axis is
    /// already fixed.
    fn fill_from(&mut self, other: &Known) {
        if self.determined() {
            return;
        }
        for (value, fallback) in self.values.iter_mut().zip(other.values) {
            if value.is_none() {
                *value = fallback;
            }
        }
    }

    /// The view's new origin and size along this axis, from `current`; a
    /// size the constraints leave open is the view's `intrinsic` one, as
    /// AppKit's content-size constraints make it, else what it already has.
    fn place(&self, current: (f64, f64), intrinsic: Option<f64>) -> (f64, f64) {
        let [min, max, center, size] = self.values;
        let mut size = size
            .or(min.zip(max).map(|(a, b)| b - a))
            .or(min.zip(center).map(|(a, c)| 2.0 * (c - a)))
            .or(max.zip(center).map(|(b, c)| 2.0 * (b - c)))
            .or(intrinsic)
            .unwrap_or(current.1);
        if let Some(largest) = self.largest {
            size = size.min(largest);
        }
        if let Some(smallest) = self.smallest {
            size = size.max(smallest);
        }
        let size = size.max(0.0);
        let origin = min
            .or(max.map(|b| b - size))
            .or(center.map(|c| c - size / 2.0))
            .unwrap_or(current.0);
        (origin, size)
    }
}

/// The width and height `target` has from a label pinned inside it to both
/// edges of an axis: the label's own size and the two insets, which is what
/// AppKit's fitting size makes of a badge around its text.
fn enclosed_size(target: Id, constraints: &[&Constraint]) -> [Option<f64>; 2] {
    let flipped = is_flipped(target);
    let mut out = [None, None];
    // Per child and axis: the leading inset and the trailing one.
    let mut edges: Vec<(Id, Axis, Option<f64>, Option<f64>)> = Vec::new();
    for c in constraints {
        if c.second != target || c.first == target || c.relation != EQUAL || c.multiplier != 1.0 {
            continue;
        }
        let (Some((axis, what)), Some((other_axis, other))) = (
            classify(c.first_attribute, flipped),
            classify(c.second_attribute, flipped),
        ) else {
            continue;
        };
        if axis != other_axis || what != other || !(what == MIN || what == MAX) {
            continue;
        }
        let at = match edges
            .iter()
            .position(|(v, a, ..)| *v == c.first && *a == axis)
        {
            Some(at) => at,
            None => {
                edges.push((c.first, axis, None, None));
                edges.len() - 1
            }
        };
        match what {
            MIN => edges[at].2 = Some(c.constant),
            _ => edges[at].3 = Some(-c.constant),
        }
    }
    for (child, axis, lead, trail) in edges {
        let (Some(lead), Some(trail)) = (lead, trail) else {
            continue;
        };
        let Some(size) = crate::label::intrinsic_size(child) else {
            continue;
        };
        let (slot, length) = match axis {
            Axis::X => (0, size.width),
            Axis::Y => (1, size.height),
        };
        let total = lead + length + trail;
        out[slot] = Some(out[slot].map_or(total, |t: f64| t.max(total)));
    }
    out
}

/// Where `target`'s constraints put it, in its superview's coordinates.
fn place(target: Id, constraints: &[&Constraint]) -> Option<NSRect> {
    // SAFETY: a view of this crate's, as every constrained item is.
    let space = unsafe { view::superview_of(target) };
    if !view::is_view(space) {
        return None;
    }
    let flipped = is_flipped(space);
    let current = view::frame_of(target);
    let mut x = Known::default();
    let mut y = Known::default();
    // What constraints naming `target` second say about it, which only fills
    // what none naming it first does: a box pinned to the edges of a stack
    // inside it would otherwise size the stack from the box's own unsolved
    // size, and each would keep the other at zero.
    let mut inverse_x = Known::default();
    let mut inverse_y = Known::default();
    for c in constraints {
        let inverse = c.first != target;
        // The side `target` is on, and the value the other side gives it.
        let (attribute, relation, value) = if c.first == target {
            let value = if c.second.is_null() || c.second_attribute == 0 {
                c.constant
            } else {
                let Some((axis, what)) = classify(c.second_attribute, flipped) else {
                    continue;
                };
                let rect = if c.second == target {
                    Some(current)
                } else {
                    rect_in(c.second, space)
                };
                let Some(rect) = rect else {
                    continue;
                };
                c.multiplier * measure(rect, axis, what) + c.constant
            };
            (c.first_attribute, c.relation, value)
        } else if c.second == target && c.relation == EQUAL && c.multiplier != 0.0 {
            let Some((axis, what)) = classify(c.first_attribute, flipped) else {
                continue;
            };
            let Some(rect) = rect_in(c.first, space) else {
                continue;
            };
            let value = (measure(rect, axis, what) - c.constant) / c.multiplier;
            (c.second_attribute, EQUAL, value)
        } else {
            continue;
        };
        let Some((axis, what)) = classify(attribute, flipped) else {
            continue;
        };
        let known = match (axis, inverse) {
            (Axis::X, false) => &mut x,
            (Axis::Y, false) => &mut y,
            (Axis::X, true) => &mut inverse_x,
            (Axis::Y, true) => &mut inverse_y,
        };
        match (relation, what) {
            (EQUAL, _) => known.values[what] = Some(value),
            (LESS_OR_EQUAL, SIZE) => known.largest = Some(value),
            (_, SIZE) => known.smallest = Some(value),
            _ => {}
        }
    }
    // A view held by one edge of its own and nothing else along an axis is as
    // big as its content, before what other views' constraints say about it
    // fills the rest: that is what its content hugging does in AppKit, and it
    // is what lets a label beside a stretching one keep its own width.
    let intrinsic = match crate::label::intrinsic_size(target) {
        Some(size) => [Some(size.width), Some(size.height)],
        None => enclosed_size(target, constraints),
    };
    for (known, length) in [(&mut x, intrinsic[0]), (&mut y, intrinsic[1])] {
        if let Some(length) = length
            && known.values[SIZE].is_none()
            && known.positions() == 1
        {
            known.values[SIZE] = Some(length.max(known.smallest.unwrap_or(0.0)));
        }
    }
    x.fill_from(&inverse_x);
    y.fill_from(&inverse_y);
    let (ox, width) = x.place(along(current, Axis::X), intrinsic[0]);
    let (oy, height) = y.place(along(current, Axis::Y), intrinsic[1]);
    Some(NSRect {
        origin: NSPoint { x: ox, y: oy },
        size: NSSize { width, height },
    })
}

fn differs(a: NSRect, b: NSRect) -> bool {
    [
        a.origin.x - b.origin.x,
        a.origin.y - b.origin.y,
        a.size.width - b.size.width,
        a.size.height - b.size.height,
    ]
    .iter()
    .any(|d| d.abs() > 0.001)
}

pub(crate) fn set_frame(target: Id, frame: NSRect) -> bool {
    if !differs(view::frame_of(target), frame) {
        return false;
    }
    // Sent, so a view that tracks its own size - a web view resizing its
    // engine - hears about it.
    // SAFETY: `-setFrame:` is `void (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(target, objc::sel::get("setFrame:"), frame);
    }
    true
}

/// One pass over every constrained view; whether any of them moved.
fn pass() -> bool {
    let snapshot: Vec<usize> = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // Held for the pass: a guest's `-setFrame:` may deactivate one.
    for c in &snapshot {
        objc::arc::retain(*c as Id);
    }
    let mut constraints: Vec<&Constraint> =
        snapshot.iter().map(|c| &*constraint_of(*c as Id)).collect();
    // Stable, so that among equal priorities the later constraint wins.
    constraints.sort_by(|a, b| a.priority.total_cmp(&b.priority));
    let mut targets: Vec<Id> = Vec::new();
    for c in &constraints {
        for item in [c.first, c.second] {
            if view::is_constrained(item) && !targets.contains(&item) {
                targets.push(item);
            }
        }
    }
    let mut moved = false;
    for target in targets {
        // A stack places what it arranges; its constraints only size it.
        if crate::controls::is_arranged(target) {
            continue;
        }
        let related: Vec<&Constraint> = constraints
            .iter()
            .copied()
            .filter(|c| c.first == target || c.second == target)
            .collect();
        if let Some(frame) = place(target, &related) {
            moved |= set_frame(target, frame);
        }
    }
    moved |= crate::controls::arrange_stacks();
    moved |= fit_windows();
    for c in snapshot {
        objc::arc::release(c as Id);
    }
    moved
}

/// What `view`'s own constraints say its width and height are, each as
/// `(exact, least)`: the constant of an equality, and the largest constant
/// lower bound.
pub(crate) fn own_size(view: Id) -> [(Option<f64>, Option<f64>); 2] {
    let mut out = [(None, None); 2];
    let active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    for c in active.iter().map(|c| &*constraint_of(*c as Id)) {
        if c.first != view || !(c.second.is_null() || c.second_attribute == 0) {
            continue;
        }
        let slot = match c.first_attribute {
            7 => &mut out[0],
            8 => &mut out[1],
            _ => continue,
        };
        match c.relation {
            EQUAL => slot.0 = Some(c.constant),
            GREATER_OR_EQUAL => {
                slot.1 = Some(slot.1.map_or(c.constant, |l: f64| l.max(c.constant)))
            }
            _ => {}
        }
    }
    out
}

/// Grow each window whose content view's constraints need more room than the
/// window gives it, as AppKit does: a window's own size holds at a lower
/// priority than a required constraint inside it. Never shrinks one. Whether
/// any window changed.
fn fit_windows() -> bool {
    let mut moved = false;
    for window in crate::window::all() {
        let content = crate::window::content_view_of(window);
        let Some(need) = crate::controls::required_size(content) else {
            continue;
        };
        let have = view::frame_of(content).size;
        if need.width <= have.width + 0.5 && need.height <= have.height + 0.5 {
            continue;
        }
        let size = NSSize {
            width: have.width.max(need.width),
            height: have.height.max(need.height),
        };
        // SAFETY: `-setContentSize:` is `void (id, SEL, NSSize)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSSize) = std::mem::transmute(objc::msg::entry());
            f(window, objc::sel::get("setContentSize:"), size);
        }
        moved = true;
    }
    moved
}

/// Bring every constrained view to where its constraints put it.
///
/// Called on every geometry change. Placing one view changes others', which
/// calls back in; that re-entry only marks the running solve dirty.
pub(crate) fn solve() {
    if SOLVING.get() {
        DIRTY.set(true);
        return;
    }
    let _solving = SOLVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).is_empty() && !crate::controls::has_stacks()
    {
        return;
    }
    SOLVING.set(true);
    // Bounded, because a contradictory set of equalities never settles.
    for _ in 0..16 {
        DIRTY.set(false);
        if !pass() && !DIRTY.get() {
            break;
        }
    }
    SOLVING.set(false);
}

/// Drop every active constraint on a view that is going away.
pub(crate) fn forget(view: Id) {
    let gone: Vec<usize> = {
        let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
        let (gone, kept) = active.drain(..).partition(|c| {
            let c = constraint_of(*c as Id);
            c.first == view || c.second == view
        });
        *active = kept;
        gone
    };
    for c in gone {
        constraint_of(c as Id).active = 0;
        objc::arc::release(c as Id);
    }
    crate::controls::forget(view);
    crate::label::forget(view);
    CONTENT_PRIORITIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(v, _)| *v != view as usize);
}

fn set_active(constraint: Id, on: bool) -> bool {
    if constraint.is_null() {
        return false;
    }
    let c = constraint_of(constraint);
    if (c.active != 0) == on {
        return false;
    }
    c.active = usize::from(on);
    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    if on {
        active.push(objc::arc::retain(constraint) as usize);
    } else if let Some(at) = active.iter().position(|x| *x == constraint as usize) {
        active.remove(at);
        drop(active);
        objc::arc::release(constraint);
    }
    true
}

/// `+constraintWithItem:attribute:relatedBy:toItem:attribute:multiplier:constant:`.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn constraint_with_item(
    class: Id,
    _sel: Sel,
    first: Id,
    first_attribute: isize,
    relation: isize,
    second: Id,
    second_attribute: isize,
    multiplier: f64,
    constant: f64,
) -> Id {
    // SAFETY: the receiver is a class, as a class method's is.
    let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) }) else {
        return std::ptr::null_mut();
    };
    let object = objc::object::create_instance(class, 0);
    let c = constraint_of(object);
    c.first = first;
    c.second = second;
    c.first_attribute = first_attribute;
    c.second_attribute = second_attribute;
    c.relation = relation;
    c.multiplier = multiplier;
    c.constant = constant;
    c.priority = REQUIRED;
    objc::arc::autorelease(object)
}

/// A new constraint, autoreleased, as the anchor methods hand one back.
fn make(
    first: (Id, isize),
    relation: isize,
    second: (Id, isize),
    multiplier: f64,
    constant: f64,
) -> Id {
    let class = objc::class::lookup(c"NSLayoutConstraint").expect("declared before its methods");
    // SAFETY: a direct call of the class method above, with its receiver.
    unsafe {
        constraint_with_item(
            objc::class::ptr(class) as Id,
            std::ptr::null_mut(),
            first.0,
            first.1,
            relation,
            second.0,
            second.1,
            multiplier,
            constant,
        )
    }
}

fn anchor_of<'a>(receiver: Id) -> &'a Anchor {
    // SAFETY: an anchor of this module's, which is what every anchor method is
    // sent to and what a well-formed program passes as the other anchor.
    unsafe { &*(receiver as *const Anchor) }
}

/// `-leadingAnchor` and the rest, each by the attribute its selector names.
unsafe extern "C" fn view_anchor(receiver: Id, sel: Sel) -> Id {
    // SAFETY: a registered selector is a NUL-terminated interned name.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) }.to_bytes();
    let (attribute, class) = match name {
        b"leftAnchor" => (1, c"NSLayoutXAxisAnchor"),
        b"rightAnchor" => (2, c"NSLayoutXAxisAnchor"),
        b"topAnchor" => (3, c"NSLayoutYAxisAnchor"),
        b"bottomAnchor" => (4, c"NSLayoutYAxisAnchor"),
        b"leadingAnchor" => (5, c"NSLayoutXAxisAnchor"),
        b"trailingAnchor" => (6, c"NSLayoutXAxisAnchor"),
        b"widthAnchor" => (7, c"NSLayoutDimension"),
        b"heightAnchor" => (8, c"NSLayoutDimension"),
        b"centerXAnchor" => (9, c"NSLayoutXAxisAnchor"),
        b"centerYAnchor" => (10, c"NSLayoutYAxisAnchor"),
        b"lastBaselineAnchor" => (11, c"NSLayoutYAxisAnchor"),
        b"firstBaselineAnchor" => (12, c"NSLayoutYAxisAnchor"),
        _ => objc::fail!(
            "-[NSView {}]: not an anchor this runtime knows",
            String::from_utf8_lossy(name)
        ),
    };
    let class = objc::class::lookup(class).expect("declared before its methods");
    let object = objc::object::create_instance(class, 0);
    // SAFETY: an `Anchor`, just made.
    let anchor = unsafe { &mut *(object as *mut Anchor) };
    anchor.item = receiver;
    anchor.attribute = attribute;
    objc::arc::autorelease(object)
}

unsafe extern "C" fn anchor_item(receiver: Id, _sel: Sel) -> Id {
    anchor_of(receiver).item
}

fn to_anchor(receiver: Id, relation: isize, other: Id, multiplier: f64, constant: f64) -> Id {
    let a = anchor_of(receiver);
    let (item, attribute) = match other.is_null() {
        true => (std::ptr::null_mut(), 0),
        false => (anchor_of(other).item, anchor_of(other).attribute),
    };
    make(
        (a.item, a.attribute),
        relation,
        (item, attribute),
        multiplier,
        constant,
    )
}

unsafe extern "C" fn equal_to(receiver: Id, _sel: Sel, other: Id) -> Id {
    to_anchor(receiver, EQUAL, other, 1.0, 0.0)
}

unsafe extern "C" fn at_least(receiver: Id, _sel: Sel, other: Id) -> Id {
    to_anchor(receiver, GREATER_OR_EQUAL, other, 1.0, 0.0)
}

unsafe extern "C" fn at_most(receiver: Id, _sel: Sel, other: Id) -> Id {
    to_anchor(receiver, LESS_OR_EQUAL, other, 1.0, 0.0)
}

unsafe extern "C" fn equal_to_plus(receiver: Id, _sel: Sel, other: Id, constant: f64) -> Id {
    to_anchor(receiver, EQUAL, other, 1.0, constant)
}

unsafe extern "C" fn at_least_plus(receiver: Id, _sel: Sel, other: Id, constant: f64) -> Id {
    to_anchor(receiver, GREATER_OR_EQUAL, other, 1.0, constant)
}

unsafe extern "C" fn at_most_plus(receiver: Id, _sel: Sel, other: Id, constant: f64) -> Id {
    to_anchor(receiver, LESS_OR_EQUAL, other, 1.0, constant)
}

unsafe extern "C" fn equal_times(receiver: Id, _sel: Sel, other: Id, multiplier: f64) -> Id {
    to_anchor(receiver, EQUAL, other, multiplier, 0.0)
}

unsafe extern "C" fn at_least_times(receiver: Id, _sel: Sel, other: Id, multiplier: f64) -> Id {
    to_anchor(receiver, GREATER_OR_EQUAL, other, multiplier, 0.0)
}

unsafe extern "C" fn at_most_times(receiver: Id, _sel: Sel, other: Id, multiplier: f64) -> Id {
    to_anchor(receiver, LESS_OR_EQUAL, other, multiplier, 0.0)
}

unsafe extern "C" fn equal_times_plus(
    receiver: Id,
    _sel: Sel,
    other: Id,
    multiplier: f64,
    constant: f64,
) -> Id {
    to_anchor(receiver, EQUAL, other, multiplier, constant)
}

unsafe extern "C" fn at_least_times_plus(
    receiver: Id,
    _sel: Sel,
    other: Id,
    multiplier: f64,
    constant: f64,
) -> Id {
    to_anchor(receiver, GREATER_OR_EQUAL, other, multiplier, constant)
}

unsafe extern "C" fn at_most_times_plus(
    receiver: Id,
    _sel: Sel,
    other: Id,
    multiplier: f64,
    constant: f64,
) -> Id {
    to_anchor(receiver, LESS_OR_EQUAL, other, multiplier, constant)
}

unsafe extern "C" fn equal_to_constant(receiver: Id, _sel: Sel, constant: f64) -> Id {
    to_anchor(receiver, EQUAL, std::ptr::null_mut(), 0.0, constant)
}

unsafe extern "C" fn at_least_constant(receiver: Id, _sel: Sel, constant: f64) -> Id {
    to_anchor(
        receiver,
        GREATER_OR_EQUAL,
        std::ptr::null_mut(),
        0.0,
        constant,
    )
}

unsafe extern "C" fn at_most_constant(receiver: Id, _sel: Sel, constant: f64) -> Id {
    to_anchor(receiver, LESS_OR_EQUAL, std::ptr::null_mut(), 0.0, constant)
}

/// Which slot of [`CONTENT_PRIORITIES`] a selector and orientation name.
fn priority_slot(compression: bool, orientation: isize) -> usize {
    usize::from(compression) * 2 + usize::from(orientation != 0)
}

fn content_priority(view: Id, slot: usize) -> f32 {
    CONTENT_PRIORITIES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(v, _)| *v == view as usize)
        .map_or(
            match slot {
                0 | 1 => DEFAULT_HUGGING,
                _ => DEFAULT_COMPRESSION_RESISTANCE,
            },
            |(_, p)| p[slot],
        )
}

fn set_content_priority(view: Id, slot: usize, value: f32) {
    let mut table = CONTENT_PRIORITIES.lock().unwrap_or_else(|e| e.into_inner());
    match table.iter_mut().find(|(v, _)| *v == view as usize) {
        Some((_, p)) => p[slot] = value,
        None => {
            let mut p = [
                DEFAULT_HUGGING,
                DEFAULT_HUGGING,
                DEFAULT_COMPRESSION_RESISTANCE,
                DEFAULT_COMPRESSION_RESISTANCE,
            ];
            p[slot] = value;
            table.push((view as usize, p));
        }
    }
}

unsafe extern "C" fn set_hugging(receiver: Id, _sel: Sel, value: f32, orientation: isize) {
    set_content_priority(receiver, priority_slot(false, orientation), value);
}

unsafe extern "C" fn hugging(receiver: Id, _sel: Sel, orientation: isize) -> f32 {
    content_priority(receiver, priority_slot(false, orientation))
}

unsafe extern "C" fn set_resistance(receiver: Id, _sel: Sel, value: f32, orientation: isize) {
    set_content_priority(receiver, priority_slot(true, orientation), value);
}

unsafe extern "C" fn resistance(receiver: Id, _sel: Sel, orientation: isize) -> f32 {
    content_priority(receiver, priority_slot(true, orientation))
}

fn each_in(array: Id, mut f: impl FnMut(Id)) {
    // SAFETY: null or an array of this runtime's.
    let Some(array) = (unsafe { corefoundation::collections::array(array as *const c_void) })
    else {
        return;
    };
    let values: Vec<Id> = array.values.iter().map(|v| *v as Id).collect();
    for value in values {
        f(value);
    }
}

unsafe extern "C" fn activate_constraints(_class: Id, _sel: Sel, constraints: Id) {
    each_in(constraints, |c| {
        set_active(c, true);
    });
    solve();
}

unsafe extern "C" fn deactivate_constraints(_class: Id, _sel: Sel, constraints: Id) {
    each_in(constraints, |c| {
        set_active(c, false);
    });
    solve();
}

unsafe extern "C" fn set_active_method(receiver: Id, _sel: Sel, on: u8) {
    if set_active(receiver, on != 0) {
        solve();
    }
}

unsafe extern "C" fn is_active(receiver: Id, _sel: Sel) -> u8 {
    u8::from(constraint_of(receiver).active != 0)
}

unsafe extern "C" fn first_item(receiver: Id, _sel: Sel) -> Id {
    constraint_of(receiver).first
}

unsafe extern "C" fn second_item(receiver: Id, _sel: Sel) -> Id {
    constraint_of(receiver).second
}

unsafe extern "C" fn first_attribute(receiver: Id, _sel: Sel) -> isize {
    constraint_of(receiver).first_attribute
}

unsafe extern "C" fn second_attribute(receiver: Id, _sel: Sel) -> isize {
    constraint_of(receiver).second_attribute
}

unsafe extern "C" fn relation(receiver: Id, _sel: Sel) -> isize {
    constraint_of(receiver).relation
}

unsafe extern "C" fn multiplier(receiver: Id, _sel: Sel) -> f64 {
    constraint_of(receiver).multiplier
}

unsafe extern "C" fn constant(receiver: Id, _sel: Sel) -> f64 {
    constraint_of(receiver).constant
}

unsafe extern "C" fn set_constant(receiver: Id, _sel: Sel, value: f64) {
    constraint_of(receiver).constant = value;
    if constraint_of(receiver).active != 0 {
        solve();
    }
}

unsafe extern "C" fn priority(receiver: Id, _sel: Sel) -> f32 {
    constraint_of(receiver).priority
}

unsafe extern "C" fn set_priority(receiver: Id, _sel: Sel, value: f32) {
    constraint_of(receiver).priority = value;
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSLayoutConstraint").expect("declared before its methods");
    class_methods!(class, {
        "constraintWithItem:attribute:relatedBy:toItem:attribute:multiplier:constant:" =>
            constraint_with_item, c"@72@0:8@16q24q32@40q48d56d64";
        "activateConstraints:" => activate_constraints, c"v24@0:8@16";
        "deactivateConstraints:" => deactivate_constraints, c"v24@0:8@16";
    });
    methods!(class, {
        "isActive" => is_active, c"c16@0:8";
        "setActive:" => set_active_method, c"v20@0:8c16";
        "firstItem" => first_item, c"@16@0:8";
        "secondItem" => second_item, c"@16@0:8";
        "firstAttribute" => first_attribute, c"q16@0:8";
        "secondAttribute" => second_attribute, c"q16@0:8";
        "relation" => relation, c"q16@0:8";
        "multiplier" => multiplier, c"d16@0:8";
        "constant" => constant, c"d16@0:8";
        "setConstant:" => set_constant, c"v24@0:8d16";
        "priority" => priority, c"f16@0:8";
        "setPriority:" => set_priority, c"v20@0:8f16";
    });

    let anchor = objc::class::lookup(c"NSLayoutAnchor").expect("declared before its methods");
    methods!(anchor, {
        "item" => anchor_item, c"@16@0:8";
        "constraintEqualToAnchor:" => equal_to, c"@24@0:8@16";
        "constraintGreaterThanOrEqualToAnchor:" => at_least, c"@24@0:8@16";
        "constraintLessThanOrEqualToAnchor:" => at_most, c"@24@0:8@16";
        "constraintEqualToAnchor:constant:" => equal_to_plus, c"@32@0:8@16d24";
        "constraintGreaterThanOrEqualToAnchor:constant:" => at_least_plus, c"@32@0:8@16d24";
        "constraintLessThanOrEqualToAnchor:constant:" => at_most_plus, c"@32@0:8@16d24";
    });
    let dimension = objc::class::lookup(c"NSLayoutDimension").expect("declared before its methods");
    methods!(dimension, {
        "constraintEqualToConstant:" => equal_to_constant, c"@24@0:8d16";
        "constraintGreaterThanOrEqualToConstant:" => at_least_constant, c"@24@0:8d16";
        "constraintLessThanOrEqualToConstant:" => at_most_constant, c"@24@0:8d16";
        "constraintEqualToAnchor:multiplier:" => equal_times, c"@32@0:8@16d24";
        "constraintGreaterThanOrEqualToAnchor:multiplier:" => at_least_times, c"@32@0:8@16d24";
        "constraintLessThanOrEqualToAnchor:multiplier:" => at_most_times, c"@32@0:8@16d24";
        "constraintEqualToAnchor:multiplier:constant:" => equal_times_plus, c"@40@0:8@16d24d32";
        "constraintGreaterThanOrEqualToAnchor:multiplier:constant:"
            => at_least_times_plus, c"@40@0:8@16d24d32";
        "constraintLessThanOrEqualToAnchor:multiplier:constant:"
            => at_most_times_plus, c"@40@0:8@16d24d32";
    });
    let view = objc::class::lookup(c"NSView").expect("declared before its methods");
    methods!(view, {
        "leftAnchor" => view_anchor, c"@16@0:8";
        "rightAnchor" => view_anchor, c"@16@0:8";
        "topAnchor" => view_anchor, c"@16@0:8";
        "bottomAnchor" => view_anchor, c"@16@0:8";
        "leadingAnchor" => view_anchor, c"@16@0:8";
        "trailingAnchor" => view_anchor, c"@16@0:8";
        "widthAnchor" => view_anchor, c"@16@0:8";
        "heightAnchor" => view_anchor, c"@16@0:8";
        "centerXAnchor" => view_anchor, c"@16@0:8";
        "centerYAnchor" => view_anchor, c"@16@0:8";
        "lastBaselineAnchor" => view_anchor, c"@16@0:8";
        "firstBaselineAnchor" => view_anchor, c"@16@0:8";
        "setContentHuggingPriority:forOrientation:" => set_hugging, c"v28@0:8f16q20";
        "contentHuggingPriorityForOrientation:" => hugging, c"f24@0:8q16";
        "setContentCompressionResistancePriority:forOrientation:"
            => set_resistance, c"v28@0:8f16q20";
        "contentCompressionResistancePriorityForOrientation:" => resistance, c"f24@0:8q16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(frame: NSRect) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSView").expect("NSView");
        let object = objc::object::create_instance(class, 0);
        let sel = objc::sel::get("initWithFrame:");
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(object, sel, frame)
        }
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect {
            origin: NSPoint { x, y },
            size: NSSize { width, height },
        }
    }

    fn pin(first: Id, attribute: isize, second: Id, constant: f64) -> Id {
        relate(first, attribute, second, attribute, constant)
    }

    fn relate(first: Id, attribute: isize, second: Id, other: isize, constant: f64) -> Id {
        let class = objc::class::lookup(c"NSLayoutConstraint").expect("NSLayoutConstraint");
        // SAFETY: direct calls with the signature installed above.
        unsafe {
            constraint_with_item(
                objc::class::ptr(class) as Id,
                objc::sel::get("constraintWithItem:"),
                first,
                attribute,
                EQUAL,
                second,
                other,
                1.0,
                constant,
            )
        }
    }

    fn send(receiver: Id, selector: &str, argument: Id) {
        // SAFETY: every selector used here is `void (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector), argument);
        }
    }

    /// The active set is global and the solver's re-entry flag per thread,
    /// so tests that solve must not overlap.
    static SOLVING_TESTS: Mutex<()> = Mutex::new(());

    fn constrain(view: Id) {
        // SAFETY: `-setTranslatesAutoresizingMaskIntoConstraints:` takes a BOOL.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            f(
                view,
                objc::sel::get("setTranslatesAutoresizingMaskIntoConstraints:"),
                0,
            );
        }
    }

    fn activate(constraints: &[Id]) -> Id {
        let values: Vec<*const c_void> = constraints.iter().map(|c| *c as *const c_void).collect();
        let array = corefoundation::collections::new_array(
            &values,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id;
        let class = objc::class::lookup(c"NSLayoutConstraint").expect("NSLayoutConstraint");
        send(objc::class::ptr(class) as Id, "activateConstraints:", array);
        array
    }

    #[test]
    fn a_background_pinned_to_the_stack_inside_it_takes_the_stacks_place() {
        let _serial = SOLVING_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        // Roblox's web view header: a box whose content is a stack, the box
        // pinned to the stack and the stack to the root, then a container
        // below the stack.
        let root = view(rect(0.0, 0.0, 400.0, 300.0));
        let background = view(rect(0.0, 0.0, 0.0, 0.0));
        let stack = view(rect(0.0, 0.0, 0.0, 0.0));
        let container = view(rect(0.0, 0.0, 0.0, 0.0));
        send(root, "addSubview:", background);
        send(background, "addSubview:", stack);
        send(root, "addSubview:", container);
        for v in [background, stack, container] {
            constrain(v);
        }
        let mut constraints = vec![
            pin(stack, 1, root, 0.0),
            pin(stack, 2, root, 0.0),
            pin(stack, 3, root, 0.0),
            relate(stack, 8, std::ptr::null_mut(), 0, 48.0),
        ];
        constraints.extend((1..=4).map(|edge| pin(background, edge, stack, 0.0)));
        constraints.extend([
            pin(container, 1, stack, 0.0),
            pin(container, 2, stack, 0.0),
            relate(container, 3, stack, 4, 0.0),
            pin(container, 4, root, 0.0),
        ]);
        let array = activate(&constraints);
        assert_eq!(view::frame_of(background), rect(0.0, 252.0, 400.0, 48.0));
        assert_eq!(view::frame_of(stack), rect(0.0, 0.0, 400.0, 48.0));
        assert_eq!(view::frame_of(container), rect(0.0, 0.0, 400.0, 252.0));
        let class = objc::class::lookup(c"NSLayoutConstraint").expect("NSLayoutConstraint");
        send(
            objc::class::ptr(class) as Id,
            "deactivateConstraints:",
            array,
        );
    }

    #[test]
    fn a_view_pinned_to_its_superviews_edges_follows_it() {
        let _serial = SOLVING_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let parent = view(rect(0.0, 0.0, 400.0, 300.0));
        let child = view(rect(0.0, 0.0, 0.0, 0.0));
        send(parent, "addSubview:", child);
        // SAFETY: `-setTranslatesAutoresizingMaskIntoConstraints:` takes a BOOL.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            f(
                child,
                objc::sel::get("setTranslatesAutoresizingMaskIntoConstraints:"),
                0,
            );
        }
        let constraints = [
            pin(child, 1, parent, 10.0),
            pin(child, 2, parent, -10.0),
            pin(child, 3, parent, -20.0),
            pin(child, 4, parent, 0.0),
        ];
        let values: Vec<*const c_void> = constraints.iter().map(|c| *c as *const c_void).collect();
        let array = corefoundation::collections::new_array(
            &values,
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id;
        let class = objc::class::lookup(c"NSLayoutConstraint").expect("NSLayoutConstraint");
        send(objc::class::ptr(class) as Id, "activateConstraints:", array);
        assert_eq!(view::frame_of(child), rect(10.0, 0.0, 380.0, 280.0));

        // SAFETY: `-setFrame:` is `void (id, SEL, NSRect)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
            f(
                parent,
                objc::sel::get("setFrame:"),
                rect(0.0, 0.0, 200.0, 100.0),
            );
        }
        assert_eq!(view::frame_of(child), rect(10.0, 0.0, 180.0, 80.0));

        send(
            objc::class::ptr(class) as Id,
            "deactivateConstraints:",
            array,
        );
    }

    fn send_id(receiver: Id, selector: &str) -> Id {
        // SAFETY: every selector used here is `id (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector))
        }
    }

    fn anchored(first: Id, anchor: &str, second: Id, other: &str, constant: f64) -> Id {
        let (a, b) = (send_id(first, anchor), send_id(second, other));
        // SAFETY: `-constraintEqualToAnchor:constant:` is `id (id, SEL, id, CGFloat)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, f64) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                a,
                objc::sel::get("constraintEqualToAnchor:constant:"),
                b,
                constant,
            )
        }
    }

    #[test]
    fn anchors_pin_a_view_as_the_item_form_does() {
        let _serial = SOLVING_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let parent = view(rect(0.0, 0.0, 400.0, 300.0));
        let child = view(rect(0.0, 0.0, 0.0, 0.0));
        send(parent, "addSubview:", child);
        constrain(child);
        let height = send_id(child, "heightAnchor");
        // SAFETY: `-constraintEqualToConstant:` is `id (id, SEL, CGFloat)`.
        let fixed = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, f64) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(height, objc::sel::get("constraintEqualToConstant:"), 40.0)
        };
        let array = activate(&[
            anchored(child, "leadingAnchor", parent, "leadingAnchor", 10.0),
            anchored(child, "trailingAnchor", parent, "trailingAnchor", -10.0),
            anchored(child, "bottomAnchor", parent, "bottomAnchor", 0.0),
            fixed,
        ]);
        assert_eq!(view::frame_of(child), rect(10.0, 0.0, 380.0, 40.0));
        let class = objc::class::lookup(c"NSLayoutConstraint").expect("NSLayoutConstraint");
        send(
            objc::class::ptr(class) as Id,
            "deactivateConstraints:",
            array,
        );
    }

    #[test]
    fn a_stack_aligned_by_width_stretches_what_it_arranges() {
        let _serial = SOLVING_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let class = objc::class::lookup(c"NSStackView").expect("NSStackView");
        let stack = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
        let stack = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
                std::mem::transmute(objc::msg::entry());
            f(
                stack,
                objc::sel::get("initWithFrame:"),
                rect(0.0, 0.0, 200.0, 64.0),
            )
        };
        // SAFETY: both setters take an `NSInteger`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, isize) = std::mem::transmute(objc::msg::entry());
            f(stack, objc::sel::get("setOrientation:"), 1);
            f(stack, objc::sel::get("setAlignment:"), 7);
        }
        let rows = [
            view(rect(0.0, 0.0, 0.0, 32.0)),
            view(rect(0.0, 0.0, 0.0, 32.0)),
        ];
        for row in rows {
            send(stack, "addArrangedSubview:", row);
        }
        solve();
        for row in rows {
            assert_eq!(view::frame_of(row).size.width, 200.0);
        }
    }
}
