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
    /// The view `-addConstraint:` installed it on, unretained; nil for one
    /// activated directly.
    owner: Id,
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

thread_local! {
    static SOLVING: Cell<bool> = const { Cell::new(false) };
    static DIRTY: Cell<bool> = const { Cell::new(false) };
}

fn constraint_of<'a>(receiver: Id) -> &'a mut Constraint {
    // SAFETY: a constraint of this module's, which is what every method here is
    // sent to.
    unsafe { &mut *(receiver as *mut Constraint) }
}

pub(crate) fn is_flipped(view: Id) -> bool {
    let sel = objc::sel::get("isFlipped");
    // SAFETY: `-isFlipped` is `BOOL (id, SEL)`, sent so a guest override answers.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(view, sel) != 0
    }
}

use crate::cassowary::{Expression, REQUIRED as SOLVER_REQUIRED, Relation, Solver};

/// A constraint priority as a solver strength. Each hundred points of
/// priority is a factor of ten, so that many constraints of one priority do
/// not outweigh a single one of a higher priority, which is how Auto Layout
/// treats them.
fn strength(priority: f32) -> f64 {
    match priority >= REQUIRED {
        true => SOLVER_REQUIRED,
        false => 10f64.powf(f64::from(priority.max(0.0)) / 100.0),
    }
}

/// What keeps a view the constraints leave free where it already is.
const STAY: f64 = 1e-3;

/// The views of one solve and their variables: each view's left, top, width
/// and height, in the top-down coordinates of the tree's root.
struct System {
    views: Vec<Id>,
    index: std::collections::HashMap<usize, usize>,
    /// The view whose fitting size is being found, which is then the top of
    /// the system; none for a whole window's solve.
    root: Option<Id>,
}

impl System {
    fn left(i: usize) -> usize {
        4 * i
    }
    fn top(i: usize) -> usize {
        4 * i + 1
    }
    fn width(i: usize) -> usize {
        4 * i + 2
    }
    fn height(i: usize) -> usize {
        4 * i + 3
    }

    /// Add `view` and every view above it, up to the root.
    fn add(&mut self, view: Id) {
        let mut at = view;
        while view::is_view(at) && !self.index.contains_key(&(at as usize)) {
            self.index.insert(at as usize, self.views.len());
            self.views.push(at);
            if Some(at) == self.root {
                break;
            }
            // SAFETY: a view of this crate's.
            at = unsafe { view::superview_of(at) };
        }
    }

    /// Whether `view` is the root or under it; every view is, with no root.
    fn within(&self, view: Id) -> bool {
        let Some(root) = self.root else {
            return true;
        };
        let mut at = view;
        while view::is_view(at) {
            if at == root {
                return true;
            }
            // SAFETY: as above.
            at = unsafe { view::superview_of(at) };
        }
        false
    }

    fn superview(&self, view: Id) -> Option<usize> {
        if Some(view) == self.root {
            return None;
        }
        // SAFETY: a view of this crate's.
        let parent = unsafe { view::superview_of(view) };
        self.index.get(&(parent as usize)).copied()
    }

    /// The nearest view both are in, each counting as in itself.
    fn common_ancestor(&self, a: Id, b: Id) -> Option<usize> {
        let mut chain = Vec::new();
        let mut at = a;
        while view::is_view(at) {
            chain.push(at);
            // SAFETY: as above.
            at = unsafe { view::superview_of(at) };
        }
        let mut at = b;
        while view::is_view(at) {
            if chain.contains(&at) {
                return self.index.get(&(at as usize)).copied();
            }
            // SAFETY: as above.
            at = unsafe { view::superview_of(at) };
        }
        None
    }

    /// `view`'s `attribute` as an expression, a position measured from the
    /// origin of `space`, the view the constraint is expressed in.
    ///
    /// FIXME (ACCURACY): baselines are the bottom and top edges, and the
    /// margin attributes are the edges, as a view here has no text metrics
    /// to place a baseline by and no layout margins.
    fn attribute(&self, view: usize, attribute: isize, space: usize) -> Option<Expression> {
        let var = |v| Expression::var(v);
        let left = var(Self::left(view)).plus(&var(Self::left(space)), -1.0);
        let top = var(Self::top(view)).plus(&var(Self::top(space)), -1.0);
        let width = var(Self::width(view));
        let height = var(Self::height(view));
        Some(match attribute {
            1 | 5 | 13 | 17 => left,
            2 | 6 | 14 | 18 => left.plus(&width, 1.0),
            3 | 12 | 15 => top,
            4 | 11 | 16 => top.plus(&height, 1.0),
            7 => width,
            8 => height,
            9 | 19 => left.plus(&width, 0.5),
            10 | 20 => top.plus(&height, 0.5),
            _ => return None,
        })
    }
}

/// `-intrinsicContentSize`, sent so a guest's override answers; `None` when
/// the class has no such method.
fn intrinsic_content_size(view: Id) -> Option<NSSize> {
    let sel = objc::sel::get("intrinsicContentSize");
    // SAFETY: a live object.
    let class = unsafe { objc::object::class_of(view) }?;
    class.method(sel)?;
    // SAFETY: `NSSize (id, SEL)`, two doubles returned in registers.
    Some(unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSSize = std::mem::transmute(objc::msg::entry());
        f(view, sel)
    })
}

/// Whether the solver leaves `view` alone: its frame is set by its program,
/// by autoresizing, or by the stack that arranges it.
fn pinned(view: Id) -> bool {
    !view::is_constrained(view) || crate::controls::is_arranged(view)
}

/// What pulls a view towards its smallest size when its fitting size is
/// asked for: above the stays, below every real priority.
const FIT: f64 = 1e-2;

/// Solve `constraints` and the frames they leave fixed. Answers each view of
/// the system with where the solution puts it, in its superview, and the
/// root's size when `root` asks for a fitting size.
fn solve_system(constraints: &[&Constraint], root: Option<Id>) -> (Vec<(Id, NSRect)>, NSSize) {
    let mut system = System {
        views: Vec::new(),
        index: std::collections::HashMap::new(),
        root,
    };
    if let Some(root) = root {
        system.add(root);
    }
    let constraints: Vec<&Constraint> = constraints
        .iter()
        .copied()
        .filter(|c| system.within(c.first) && (!view::is_view(c.second) || system.within(c.second)))
        .collect();
    for c in &constraints {
        for item in [c.first, c.second] {
            if view::is_view(item) {
                system.add(item);
            }
        }
    }
    let mut solver = Solver::new();
    let add = |solver: &mut Solver, e: Expression, relation: Relation, strength: f64| {
        if solver.add(&e, relation, strength).is_err() {
            // AppKit breaks one of the conflicting constraints and carries
            // on; this keeps it, at the highest strength that can lose.
            let _ = solver.add(&e, relation, self::strength(999.0));
        }
    };
    let flipped: Vec<bool> = system.views.iter().map(|v| is_flipped(*v)).collect();
    for (i, &view) in system.views.iter().enumerate() {
        let frame = view::frame_of(view);
        let (l, t, w, h) = (
            System::left(i),
            System::top(i),
            System::width(i),
            System::height(i),
        );
        let var = Expression::var;
        let pin = |solver: &mut Solver, v, value: f64, strength| {
            add(
                solver,
                var(v).add_constant(-value),
                Relation::Equal,
                strength,
            );
        };
        let parent = system.superview(view);
        let fitting = Some(view) == system.root;
        let fixed = (pinned(view) || parent.is_none()) && !fitting;
        let strength = if fixed { SOLVER_REQUIRED } else { STAY };
        // A view its mask places moves with its superview, as the
        // constraints AppKit makes of the mask say, rather than staying put.
        let rates = match parent {
            Some(p) if fixed && !crate::controls::is_arranged(view) => {
                view::autoresizing_rates(view).map(|rates| (p, rates))
            }
            _ => None,
        };
        if let Some((p, [(kx, kw), (ky, kh)])) = rates {
            let old = view::frame_of(system.views[p]).size;
            let follow =
                |solver: &mut Solver, own: Expression, rate: f64, along: usize, was: f64| {
                    let e = own.plus(&var(along), -rate).add_constant(rate * was);
                    add(solver, e, Relation::Equal, SOLVER_REQUIRED);
                };
            let width = var(w).add_constant(-frame.size.width);
            follow(&mut solver, width, kw, System::width(p), old.width);
            let height = var(h).add_constant(-frame.size.height);
            follow(&mut solver, height, kh, System::height(p), old.height);
            let left = var(l)
                .plus(&var(System::left(p)), -1.0)
                .add_constant(-frame.origin.x);
            follow(&mut solver, left, kx, System::width(p), old.width);
            // The mask's y is the frame's, so from the top in a flipped
            // superview and from the bottom otherwise.
            let (top, rate) = match flipped[p] {
                true => (
                    var(t)
                        .plus(&var(System::top(p)), -1.0)
                        .add_constant(-frame.origin.y),
                    ky,
                ),
                false => (
                    var(t)
                        .plus(&var(System::top(p)), -1.0)
                        .plus(&var(System::height(p)), -1.0)
                        .plus(&var(h), 1.0)
                        .add_constant(frame.origin.y),
                    -ky,
                ),
            };
            follow(&mut solver, top, rate, System::height(p), old.height);
            continue;
        }
        match fitting {
            true => {
                pin(&mut solver, w, 0.0, FIT);
                pin(&mut solver, h, 0.0, FIT);
            }
            false => {
                pin(&mut solver, w, frame.size.width, strength);
                pin(&mut solver, h, frame.size.height, strength);
            }
        }
        match parent {
            // The top of the tree is the origin of the system.
            None => {
                pin(&mut solver, l, 0.0, SOLVER_REQUIRED);
                pin(&mut solver, t, 0.0, SOLVER_REQUIRED);
            }
            Some(p) => {
                // left = parent.left + x, and top from the parent's top, or
                // from its bottom when the parent counts y upwards.
                let left = var(l)
                    .plus(&var(System::left(p)), -1.0)
                    .add_constant(-frame.origin.x);
                add(&mut solver, left, Relation::Equal, strength);
                let top = match flipped[p] {
                    true => var(t)
                        .plus(&var(System::top(p)), -1.0)
                        .add_constant(-frame.origin.y),
                    false => var(t)
                        .plus(&var(System::top(p)), -1.0)
                        .plus(&var(System::height(p)), -1.0)
                        .add_constant(frame.origin.y + frame.size.height),
                };
                // A free view's top follows its height when the parent counts
                // upwards: what stays put on screen is its bottom edge.
                let top = match (fixed, flipped[p]) {
                    (false, false) => top.plus(&var(h), 1.0).add_constant(-frame.size.height),
                    _ => top,
                };
                add(&mut solver, top, Relation::Equal, strength);
            }
        }
        if fixed {
            continue;
        }
        // Content hugging and compression resistance, at their priorities.
        if let Some(size) = intrinsic_content_size(view) {
            for (v, length, orientation) in [(w, size.width, 0), (h, size.height, 1)] {
                if length < 0.0 {
                    continue;
                }
                let hugging = content_priority(view, priority_slot(false, orientation));
                let resistance = content_priority(view, priority_slot(true, orientation));
                let e = var(v).add_constant(-length);
                add(
                    &mut solver,
                    e.clone(),
                    Relation::LessOrEqual,
                    self::strength(hugging),
                );
                add(
                    &mut solver,
                    e,
                    Relation::GreaterOrEqual,
                    self::strength(resistance),
                );
            }
        }
        // No negative sizes.
        add(
            &mut solver,
            var(w),
            Relation::GreaterOrEqual,
            SOLVER_REQUIRED,
        );
        add(
            &mut solver,
            var(h),
            Relation::GreaterOrEqual,
            SOLVER_REQUIRED,
        );
    }
    for c in &constraints {
        let Some(&first) = system.index.get(&(c.first as usize)) else {
            continue;
        };
        let has_second = view::is_view(c.second) && c.second_attribute != 0;
        let space = match has_second {
            true => system.common_ancestor(c.first, c.second),
            false => system.superview(c.first).or(Some(first)),
        };
        let Some(space) = space else {
            continue;
        };
        let Some(lhs) = system.attribute(first, c.first_attribute, space) else {
            continue;
        };
        let rhs = match has_second {
            true => {
                let Some(&second) = system.index.get(&(c.second as usize)) else {
                    continue;
                };
                let Some(e) = system.attribute(second, c.second_attribute, space) else {
                    continue;
                };
                Expression::constant(0.0).plus(&e, c.multiplier)
            }
            false => Expression::constant(0.0),
        }
        .add_constant(c.constant);
        let relation = match c.relation {
            LESS_OR_EQUAL => Relation::LessOrEqual,
            GREATER_OR_EQUAL => Relation::GreaterOrEqual,
            _ => Relation::Equal,
        };
        add(
            &mut solver,
            lhs.plus(&rhs, -1.0),
            relation,
            strength(c.priority),
        );
    }
    // Each free view's frame, in its superview, with its edges on whole
    // points as AppKit puts them.
    let mut out = Vec::new();
    for (i, &view) in system.views.iter().enumerate() {
        let Some(p) = system.superview(view) else {
            continue;
        };
        if pinned(view) {
            continue;
        }
        let value = |v| solver.value(v);
        let (pl, pt, ph) = (
            value(System::left(p)),
            value(System::top(p)),
            value(System::height(p)),
        );
        let left = (value(System::left(i)) - pl).round();
        let right = (value(System::left(i)) + value(System::width(i)) - pl).round();
        let top = (value(System::top(i)) - pt).round();
        let bottom = (value(System::top(i)) + value(System::height(i)) - pt).round();
        let height = (bottom - top).max(0.0);
        let y = match flipped[p] {
            true => top,
            false => ph.round() - bottom,
        };
        out.push((
            view,
            NSRect {
                origin: NSPoint { x: left, y },
                size: NSSize {
                    width: (right - left).max(0.0),
                    height,
                },
            },
        ));
    }
    let size = match root.and_then(|r| system.index.get(&(r as usize)).copied()) {
        Some(i) => NSSize {
            width: solver.value(System::width(i)).max(0.0).ceil(),
            height: solver.value(System::height(i)).max(0.0).ceil(),
        },
        None => NSSize::default(),
    };
    (out, size)
}

/// `-[NSView fittingSize]`: the smallest size the view's own constraints and
/// its subviews' allow.
unsafe extern "C" fn fitting_size(receiver: Id, _sel: Sel) -> NSSize {
    if !SOLVING.get() {
        update_constraints();
    }
    let snapshot: Vec<usize> = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for c in &snapshot {
        objc::arc::retain(*c as Id);
    }
    let constraints: Vec<&Constraint> =
        snapshot.iter().map(|c| &*constraint_of(*c as Id)).collect();
    let (_, size) = solve_system(&constraints, Some(receiver));
    for c in snapshot {
        objc::arc::release(c as Id);
    }
    size
}

/// `-[NSView intrinsicContentSize]`: a plain view has none of its own.
unsafe extern "C" fn no_intrinsic_size(_receiver: Id, _sel: Sel) -> NSSize {
    NSSize {
        width: -1.0,
        height: -1.0,
    }
}

unsafe extern "C" fn invalidate_intrinsic_size(_receiver: Id, _sel: Sel) {
    solve();
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

/// One solve of every active constraint; whether any view moved.
fn pass() -> bool {
    let snapshot: Vec<usize> = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // Held for the pass: a guest's `-setFrame:` may deactivate one.
    for c in &snapshot {
        objc::arc::retain(*c as Id);
    }
    let constraints: Vec<&Constraint> =
        snapshot.iter().map(|c| &*constraint_of(*c as Id)).collect();
    let (mut placed, _) = solve_system(&constraints, None);
    // Parents first: a parent's new size resizes its autoresizing children,
    // which the next pass then holds where that left them.
    // SAFETY: views of this crate's.
    placed.sort_by_key(|(view, _)| depth(*view));
    let mut moved = false;
    for (view, frame) in placed {
        moved |= set_frame(view, frame);
    }
    moved |= crate::controls::arrange_stacks();
    moved |= fit_windows();
    for c in snapshot {
        objc::arc::release(c as Id);
    }
    moved
}

/// How many superviews `view` has.
fn depth(view: Id) -> usize {
    let mut depth = 0;
    // SAFETY: views of this crate's.
    let mut at = unsafe { view::superview_of(view) };
    while view::is_view(at) {
        depth += 1;
        at = unsafe { view::superview_of(at) };
    }
    depth
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

/// Set by every change a layout pass would act on.
static LAYOUT_NEEDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Note that the constraints or the geometry they depend on changed.
///
/// The solve itself waits for the next layout pass, before the windows draw,
/// or for a program that asks for one: AppKit defers layout the same way,
/// and a view is routinely given its constraints in its own initializer,
/// which a solve run from inside it would reach half made.
pub(crate) fn solve() {
    LAYOUT_NEEDED.store(true, std::sync::atomic::Ordering::Release);
}

/// The layout pass: views owed `-updateConstraints` get it, then every
/// constrained view goes where its constraints put it, if anything changed.
pub(crate) fn layout_now() {
    if SOLVING.get() {
        DIRTY.set(true);
        return;
    }
    update_constraints();
    if !LAYOUT_NEEDED.swap(false, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    if ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).is_empty() && !crate::controls::has_stacks()
    {
        return;
    }
    if SOLVING.get() {
        DIRTY.set(true);
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
    // What placing views changed is settled by the passes above.
    LAYOUT_NEEDED.store(false, std::sync::atomic::Ordering::Release);
}

/// Drop every active constraint on a view that is going away.
pub(crate) fn forget(view: Id) {
    if let Some(introduced) = INTRODUCED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        introduced.remove(&(view as usize));
    }
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

/// A new constraint, autoreleased, for another module of this crate.
pub(crate) fn new_constraint(
    first: (Id, isize),
    relation: isize,
    second: (Id, isize),
    multiplier: f64,
    constant: f64,
) -> Id {
    make(first, relation, second, multiplier, constant)
}

pub(crate) fn set_priority_of(constraint: Id, priority: f32) {
    if !constraint.is_null() {
        constraint_of(constraint).priority = priority;
    }
}

/// `-[NSView addConstraint:]`: active, and listed by the view's
/// `-constraints`.
unsafe extern "C" fn add_constraint(receiver: Id, _sel: Sel, constraint: Id) {
    if constraint.is_null() {
        return;
    }
    constraint_of(constraint).owner = receiver;
    if set_active(constraint, true) {
        solve();
    }
}

unsafe extern "C" fn add_constraints(receiver: Id, _sel: Sel, constraints: Id) {
    each_in(constraints, |c| {
        constraint_of(c).owner = receiver;
        set_active(c, true);
    });
    solve();
}

unsafe extern "C" fn remove_constraint(_receiver: Id, _sel: Sel, constraint: Id) {
    if constraint.is_null() {
        return;
    }
    constraint_of(constraint).owner = std::ptr::null_mut();
    if set_active(constraint, false) {
        solve();
    }
}

unsafe extern "C" fn remove_constraints(_receiver: Id, _sel: Sel, constraints: Id) {
    each_in(constraints, |c| {
        constraint_of(c).owner = std::ptr::null_mut();
        set_active(c, false);
    });
    solve();
}

/// `-[NSView constraints]`: the active ones installed on this view.
unsafe extern "C" fn view_constraints(receiver: Id, _sel: Sel) -> Id {
    let owned: Vec<corefoundation::base::CFTypeRef> = ACTIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|c| constraint_of(**c as Id).owner == receiver)
        .map(|c| *c as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &owned,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    objc::arc::autorelease(array as Id)
}

/// Views owed an `-updateConstraints`, each retained.
static NEEDS_UPDATE: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// `-setNeedsUpdateConstraints:`: the view's `-updateConstraints`, which a
/// subclass overrides to make its constraints, is sent before the next solve.
unsafe extern "C" fn set_needs_update_constraints(receiver: Id, _sel: Sel, needed: u8) {
    if needed == 0 {
        return;
    }
    let mut pending = NEEDS_UPDATE.lock().unwrap_or_else(|e| e.into_inner());
    if !pending.contains(&(receiver as usize)) {
        pending.push(objc::arc::retain(receiver) as usize);
    }
}

unsafe extern "C" fn needs_update_constraints(receiver: Id, _sel: Sel) -> u8 {
    NEEDS_UPDATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&(receiver as usize)) as u8
}

/// Send `-updateConstraints` to every view owed one. A view may ask again
/// from inside it, which is honoured on the next round, as AppKit's own
/// loop does.
pub(crate) fn update_constraints() {
    for _ in 0..8 {
        let pending: Vec<usize> =
            std::mem::take(&mut *NEEDS_UPDATE.lock().unwrap_or_else(|e| e.into_inner()));
        if pending.is_empty() {
            return;
        }
        for view in pending {
            // SAFETY: a view, sent its `void (id, SEL)` hook.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
                f(view as Id, objc::sel::get("updateConstraints"));
            }
            objc::arc::release(view as Id);
        }
    }
}

/// Views that have had the `-updateConstraints` every new view is owed when
/// it first appears in a window, as AppKit's first layout pass sends it.
static INTRODUCED: Mutex<Option<std::collections::HashSet<usize>>> = Mutex::new(None);

/// Set when a view joins a window, so the next [`prepare`] looks for views
/// that have not been introduced.
static TREE_CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub(crate) fn note_tree_changed() {
    TREE_CHANGED.store(true, std::sync::atomic::Ordering::Release);
}

/// Before the windows draw: every view in a shown one not yet sent
/// `-updateConstraints` is owed one, and then the constraints are solved.
pub(crate) fn prepare() {
    if !TREE_CHANGED.swap(false, std::sync::atomic::Ordering::AcqRel) {
        layout_now();
        return;
    }
    let mut stack: Vec<Id> = crate::window::all()
        .into_iter()
        .filter(|w| crate::window::is_shown(*w))
        .map(crate::window::content_view_of)
        .collect();
    let mut owed = Vec::new();
    {
        let mut introduced = INTRODUCED.lock().unwrap_or_else(|e| e.into_inner());
        let introduced = introduced.get_or_insert_default();
        while let Some(view) = stack.pop() {
            if !view::is_view(view) {
                continue;
            }
            if introduced.insert(view as usize) {
                owed.push(view);
            }
            stack.extend(view::subviews_of(view));
        }
    }
    {
        let mut pending = NEEDS_UPDATE.lock().unwrap_or_else(|e| e.into_inner());
        // Deepest first, as AppKit updates a subtree from its leaves up.
        for view in owed.into_iter().rev() {
            if !pending.contains(&(view as usize)) {
                pending.push(objc::arc::retain(view) as usize);
            }
        }
    }
    solve();
    layout_now();
}

/// `-[NSView updateConstraints]`: the base class has none of its own.
unsafe extern "C" fn base_update_constraints(_receiver: Id, _sel: Sel) {}

/// `-setNeedsLayout:`: solving is done on every change here, so it runs now.
unsafe extern "C" fn set_needs_layout(_receiver: Id, _sel: Sel, needed: u8) {
    if needed != 0 {
        solve();
    }
}

/// `-layoutSubtreeIfNeeded` and the like: the layout pass, now.
unsafe extern "C" fn solve_now(_receiver: Id, _sel: Sel) {
    layout_now();
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
        "constraintsWithVisualFormat:options:metrics:views:" =>
            crate::visual_format::constraints_with_visual_format, c"@48@0:8@16Q24@32@40";
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
        "fittingSize" => fitting_size, c"{CGSize=dd}16@0:8";
        "intrinsicContentSize" => no_intrinsic_size, c"{CGSize=dd}16@0:8";
        "invalidateIntrinsicContentSize" => invalidate_intrinsic_size, c"v16@0:8";
        "addConstraint:" => add_constraint, c"v24@0:8@16";
        "addConstraints:" => add_constraints, c"v24@0:8@16";
        "removeConstraint:" => remove_constraint, c"v24@0:8@16";
        "removeConstraints:" => remove_constraints, c"v24@0:8@16";
        "constraints" => view_constraints, c"@16@0:8";
        "setNeedsUpdateConstraints:" => set_needs_update_constraints, c"v20@0:8c16";
        "needsUpdateConstraints" => needs_update_constraints, c"c16@0:8";
        "updateConstraintsForSubtreeIfNeeded" => solve_now, c"v16@0:8";
        "updateConstraints" => base_update_constraints, c"v16@0:8";
        "setNeedsLayout:" => set_needs_layout, c"v20@0:8c16";
        "layoutSubtreeIfNeeded" => solve_now, c"v16@0:8";
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
        layout_now();
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
    fn a_constraint_through_an_autoresizing_view_sizes_its_superview() {
        let _serial = SOLVING_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        // TextMate's gutter: a scroll view as wide as the document view in
        // its clip view, which its mask sizes to the scroll view.
        let root = view(rect(0.0, 0.0, 400.0, 300.0));
        let scroll = view(rect(0.0, 0.0, 0.0, 300.0));
        let clip = view(rect(0.0, 0.0, 0.0, 300.0));
        let document = view(rect(0.0, 0.0, 0.0, 300.0));
        send(root, "addSubview:", scroll);
        send(scroll, "addSubview:", clip);
        send(clip, "addSubview:", document);
        constrain(scroll);
        constrain(document);
        // SAFETY: `-setAutoresizingMask:` is `void (id, SEL, NSUInteger)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, usize) = std::mem::transmute(objc::msg::entry());
            f(clip, objc::sel::get("setAutoresizingMask:"), 0x12);
        }
        let constraints = [
            pin(scroll, 1, root, 0.0),
            pin(scroll, 3, root, 0.0),
            pin(scroll, 4, root, 0.0),
            pin(document, 1, clip, 0.0),
            pin(document, 2, clip, 0.0),
            pin(document, 3, clip, 0.0),
            relate(document, 7, std::ptr::null_mut(), 0, 41.0),
        ];
        let array = activate(&constraints);
        layout_now();
        assert_eq!(view::frame_of(scroll), rect(0.0, 0.0, 41.0, 300.0));
        assert_eq!(view::frame_of(clip), rect(0.0, 0.0, 41.0, 300.0));
        assert_eq!(view::frame_of(document).size.width, 41.0);
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
        // Vertical attributes grow downwards whichever way the superview
        // counts, as `V:|-20-[child]` makes `child.top = parent.top + 20`.
        let constraints = [
            pin(child, 1, parent, 10.0),
            pin(child, 2, parent, -10.0),
            pin(child, 3, parent, 20.0),
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
        layout_now();
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
        layout_now();
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
        layout_now();
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
        layout_now();
        for row in rows {
            assert_eq!(view::frame_of(row).size.width, 200.0);
        }
    }
}
