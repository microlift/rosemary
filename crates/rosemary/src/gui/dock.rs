use std::sync::Mutex;

use super::draw::{DrawList, Input, Rect, Ui, color, hit};
use super::panel::{Model, View};

/// How many views there are, which is the width of the hidden-flag array.
const VIEWS: usize = View::ALL.len();

/// The thickness of a splitter, and of the border a float is resized by.
const SPLIT_W: f32 = 5.0;
const GRIP: f32 = 7.0;

/// The smallest a float, or an edge strip, may be dragged to.
const MIN_W: f32 = 160.0;
const MIN_H: f32 = 64.0;

/// How close to the surface edge a drop has to be to dock to it.
const EDGE_ZONE: f32 = 22.0;

/// How far a press has to travel before it is a drag rather than a click.
const SLOP: f32 = 4.0;

/// Which side of the surface an edge container is pinned to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// In carve order: the left and right strips run the full height, and the
    /// top and bottom ones fill what is left between them.
    const ALL: [Side; 4] = [Side::Left, Side::Right, Side::Top, Side::Bottom];

    fn index(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
            Side::Top => 2,
            Side::Bottom => 3,
        }
    }

    /// Whether a second window snapped to this side goes below the first
    /// rather than beside it. A tall strip stacks; a wide one lines up.
    fn stacks(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }
}

/// A tabbed group of views, and the one of them that is showing.
#[derive(Clone)]
pub struct Leaf {
    id: u32,
    tabs: Vec<View>,
    active: View,
    scroll: f32,
    reach: f32,
}

impl Leaf {
    fn new(id: u32, tabs: Vec<View>) -> Leaf {
        let active = tabs[0];
        Leaf {
            id,
            tabs,
            active,
            scroll: 0.0,
            reach: 0.0,
        }
    }

    /// The tabs that are not hidden, which is what the strip draws.
    fn shown(&self, hidden: &[bool; VIEWS]) -> Vec<View> {
        self.tabs
            .iter()
            .copied()
            .filter(|v| !hidden[v.slot()])
            .collect()
    }

    /// The view to draw: the active one, or the first that is still shown.
    fn current(&self, hidden: &[bool; VIEWS]) -> Option<View> {
        let shown = self.shown(hidden);
        match shown.contains(&self.active) {
            true => Some(self.active),
            false => shown.first().copied(),
        }
    }
}

#[derive(Clone)]
struct Split {
    id: u32,
    /// Whether `a` is above `b` rather than left of it.
    vertical: bool,
    ratio: f32,
    a: Box<Node>,
    b: Box<Node>,
}

#[derive(Clone)]
enum Node {
    Leaf(Leaf),
    Split(Split),
}

impl Node {
    fn visible(&self, hidden: &[bool; VIEWS]) -> bool {
        match self {
            Node::Leaf(l) => l.current(hidden).is_some(),
            Node::Split(s) => s.a.visible(hidden) || s.b.visible(hidden),
        }
    }

    fn holds(&self, view: View) -> bool {
        match self {
            Node::Leaf(l) => l.tabs.contains(&view),
            Node::Split(s) => s.a.holds(view) || s.b.holds(view),
        }
    }
}

/// A window that is docked to nothing.
struct Float {
    rect: Rect,
    node: Node,
}

/// What the mouse is in the middle of doing.
#[derive(Clone, Copy)]
enum Hold {
    Resize {
        float: usize,
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
    },
    /// A splitter inside a container, and the pixels its two children share.
    Splitter {
        id: u32,
        vertical: bool,
        span: f32,
    },
    EdgeSize(Side),
    /// A press on a header or tab that has not yet moved far enough to be a
    /// drag. Held rather than acted on so that a click can still be a click.
    Grab {
        leaf: u32,
        tab: Option<View>,
        from: (f32, f32),
    },
    Drag,
}

/// A window in flight between two places in the tree.
struct Drag {
    leaf: Leaf,
    size: (f32, f32),
    /// Where inside the window the pointer took hold of it.
    grab: (f32, f32),
    at: (f32, f32),
}

/// Where a dropped window would land.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Edge(Side),
    Onto { leaf: u32, zone: Zone },
    Free,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    Center,
}

/// A row of the Windows menu.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Toggle(View),
    ShowAll,
    HideAll,
    Reset,
}

/// Everything the overlay remembers about its own windows.
pub struct Dock {
    edges: [Option<Node>; 4],
    /// Each edge strip's thickness in pixels, or zero for "not sized yet".
    extents: [f32; 4],
    /// Back to front: the last one is on top and gets the click.
    floats: Vec<Float>,
    hidden: [bool; VIEWS],
    hold: Option<Hold>,
    drag: Option<Drag>,
    menu: bool,
    menu_scroll: f32,
    next_id: u32,
    /// The surface the last frame was laid out against, for a gesture that has
    /// to be given up without one in hand.
    surface: (f32, f32),
}

impl Default for Dock {
    fn default() -> Self {
        let mut dock = Dock {
            edges: [None, None, None, None],
            extents: [0.0; 4],
            floats: Vec::new(),
            hidden: [false; VIEWS],
            hold: None,
            drag: None,
            menu: false,
            menu_scroll: 0.0,
            next_id: 0,
            surface: (1280.0, 720.0),
        };
        dock.reset();
        dock
    }
}

impl Dock {
    fn next(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    /// The layout the overlay starts with, and the one the menu comes back to.
    ///
    /// A left-hand column, which is where the single panel this replaced used
    /// to be: an overlay that opens covering the guest's whole window is one
    /// people close before reading.
    fn reset(&mut self) {
        self.edges = [None, None, None, None];
        self.extents = [0.0; 4];
        self.floats.clear();
        self.hidden = [false; VIEWS];
        self.drag = None;
        self.hold = None;

        let top = Node::Leaf(Leaf::new(self.next(), vec![View::Frames, View::Trace]));
        let middle = Node::Leaf(Leaf::new(self.next(), vec![View::Time, View::HostSymbols]));
        let bottom = Node::Leaf(Leaf::new(
            self.next(),
            vec![
                View::Regions,
                View::Instructions,
                View::Entries,
                View::Translation,
                View::Footprint,
                View::Keys,
            ],
        ));
        let lower = Node::Split(Split {
            id: self.next(),
            vertical: true,
            ratio: 0.42,
            a: Box::new(middle),
            b: Box::new(bottom),
        });
        self.edges[Side::Left.index()] = Some(Node::Split(Split {
            id: self.next(),
            vertical: true,
            ratio: 0.30,
            a: Box::new(top),
            b: Box::new(lower),
        }));
    }

    /// Whether any node anywhere still holds `view`.
    fn holds(&self, view: View) -> bool {
        self.edges
            .iter()
            .flatten()
            .chain(self.floats.iter().map(|f| &f.node))
            .any(|n| n.holds(view))
    }

    /// Show or hide one window.
    fn toggle(&mut self, view: View, surface: (f32, f32)) {
        let slot = view.slot();
        self.hidden[slot] = !self.hidden[slot];
        if !self.hidden[slot] && !self.holds(view) {
            let id = self.next();
            self.float_at(Leaf::new(id, vec![view]), surface);
        }
    }

    /// Put a leaf down as a float, cascaded so a run of them is not one pile.
    fn float_at(&mut self, leaf: Leaf, surface: (f32, f32)) {
        let step = 24.0 * (self.floats.len() % 8) as f32;
        let size = (
            (surface.0 * 0.30).clamp(MIN_W, 560.0),
            (surface.1 * 0.34).clamp(MIN_H, 460.0),
        );
        let rect = (
            (surface.0 * 0.34 + step)
                .min(surface.0 - size.0 - 8.0)
                .max(8.0),
            (72.0 + step).min(surface.1 - size.1 - 8.0).max(8.0),
            size.0,
            size.1,
        );
        self.floats.push(Float {
            rect,
            node: Node::Leaf(leaf),
        });
    }

    /// Pull a whole leaf, or one of its tabs, out of wherever it is.
    fn detach(&mut self, id: u32, tab: Option<View>) -> Option<Leaf> {
        // One tab out of a group of several: the group survives.
        if let Some(view) = tab
            && self.find(id).is_some_and(|l| l.tabs.len() > 1)
        {
            let taken = self.remove_view(view)?;
            let new = self.next();
            return Some(Leaf::new(new, vec![taken]));
        }
        let mut out = None;
        self.rebuild(|node| take_leaf(node, id, &mut out));
        out
    }

    /// Take one view out of its group, leaving the group behind.
    fn remove_view(&mut self, view: View) -> Option<View> {
        let mut out = None;
        self.rebuild(|node| take_view(node, view, &mut out));
        out
    }

    /// Apply a by-value transform to every container, dropping the empty ones.
    fn rebuild(&mut self, mut f: impl FnMut(Node) -> Option<Node>) {
        for slot in self.edges.iter_mut() {
            if let Some(node) = slot.take() {
                *slot = f(node);
            }
        }
        let mut kept = Vec::with_capacity(self.floats.len());
        for float in std::mem::take(&mut self.floats) {
            if let Some(node) = f(float.node) {
                kept.push(Float {
                    rect: float.rect,
                    node,
                });
            }
        }
        self.floats = kept;
    }

    fn find(&mut self, id: u32) -> Option<&mut Leaf> {
        self.edges
            .iter_mut()
            .flatten()
            .chain(self.floats.iter_mut().map(|f| &mut f.node))
            .find_map(|n| find_leaf(n, id))
    }

    /// Put a dragged window down.
    fn place(&mut self, drag: Drag, target: Target, surface: (f32, f32)) {
        match target {
            Target::Edge(side) => {
                let slot = side.index();
                let node = match self.edges[slot].take() {
                    None => Node::Leaf(drag.leaf),
                    Some(existing) => {
                        let id = self.next();
                        Node::Split(Split {
                            id,
                            vertical: side.stacks(),
                            ratio: 0.5,
                            a: Box::new(existing),
                            b: Box::new(Node::Leaf(drag.leaf)),
                        })
                    }
                };
                self.edges[slot] = Some(node);
                if self.extents[slot] <= 0.0 {
                    self.extents[slot] = match side.stacks() {
                        true => (surface.0 * 0.30).clamp(MIN_W, 560.0),
                        false => (surface.1 * 0.30).clamp(MIN_H, 460.0),
                    };
                }
            }
            Target::Onto { leaf, zone } => {
                let mut payload = Some(drag.leaf);
                let mut ids = self.next_id;
                self.rebuild(|node| Some(insert(node, leaf, zone, &mut payload, &mut ids)));
                self.next_id = ids;
                // Nothing matched, which means the target went away between the
                // drag starting and the drop. Better a float than a lost window.
                if let Some(leaf) = payload {
                    self.float_at(leaf, surface);
                }
            }
            Target::Free => {
                let rect = (
                    (drag.at.0 - drag.grab.0).clamp(-drag.size.0 * 0.5, surface.0 - 40.0),
                    (drag.at.1 - drag.grab.1).clamp(0.0, surface.1 - 24.0),
                    drag.size.0,
                    drag.size.1,
                );
                self.floats.push(Float {
                    rect,
                    node: Node::Leaf(drag.leaf),
                });
            }
        }
    }
}

/// Remove the leaf `id` names, collapsing the splits it leaves behind.
fn take_leaf(node: Node, id: u32, out: &mut Option<Leaf>) -> Option<Node> {
    match node {
        Node::Leaf(l) if l.id == id => {
            *out = Some(l);
            None
        }
        Node::Leaf(l) => Some(Node::Leaf(l)),
        Node::Split(s) => collapse(s, |n| take_leaf(n, id, out)),
    }
}

/// Remove one view from whatever group holds it.
fn take_view(node: Node, view: View, out: &mut Option<View>) -> Option<Node> {
    match node {
        Node::Leaf(mut l) => {
            if let Some(at) = l.tabs.iter().position(|v| *v == view) {
                *out = Some(l.tabs.remove(at));
                if l.tabs.is_empty() {
                    return None;
                }
                if l.active == view {
                    l.active = l.tabs[at.min(l.tabs.len() - 1)];
                }
            }
            Some(Node::Leaf(l))
        }
        Node::Split(s) => collapse(s, |n| take_view(n, view, out)),
    }
}

/// Transform both children, and dissolve the split if it is left with one.
fn collapse(s: Split, mut f: impl FnMut(Node) -> Option<Node>) -> Option<Node> {
    let a = f(*s.a);
    let b = f(*s.b);
    match (a, b) {
        (Some(a), Some(b)) => Some(Node::Split(Split {
            a: Box::new(a),
            b: Box::new(b),
            ..s
        })),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// Put `payload` where `zone` says, relative to the leaf `at` names.
fn insert(node: Node, at: u32, zone: Zone, payload: &mut Option<Leaf>, ids: &mut u32) -> Node {
    match node {
        Node::Leaf(mut l) if l.id == at && payload.is_some() => {
            let dropped = payload.take().expect("checked just above");
            match zone {
                Zone::Center => {
                    let active = dropped.active;
                    l.tabs.extend(dropped.tabs);
                    l.active = active;
                    Node::Leaf(l)
                }
                _ => {
                    *ids += 1;
                    let vertical = matches!(zone, Zone::Top | Zone::Bottom);
                    let first = matches!(zone, Zone::Left | Zone::Top);
                    let (a, b) = match first {
                        true => (Node::Leaf(dropped), Node::Leaf(l)),
                        false => (Node::Leaf(l), Node::Leaf(dropped)),
                    };
                    Node::Split(Split {
                        id: *ids,
                        vertical,
                        ratio: 0.5,
                        a: Box::new(a),
                        b: Box::new(b),
                    })
                }
            }
        }
        Node::Leaf(l) => Node::Leaf(l),
        Node::Split(s) => Node::Split(Split {
            a: Box::new(insert(*s.a, at, zone, payload, ids)),
            b: Box::new(insert(*s.b, at, zone, payload, ids)),
            ..s
        }),
    }
}

fn find_leaf(node: &mut Node, id: u32) -> Option<&mut Leaf> {
    match node {
        Node::Leaf(l) if l.id == id => Some(l),
        Node::Leaf(_) => None,
        Node::Split(s) => find_leaf(&mut s.a, id).or_else(|| find_leaf(&mut s.b, id)),
    }
}

/// Move one splitter's ratio, wherever in the tree it is.
///
/// A nudge rather than a lookup returning `&mut Split`: a conditional return of
/// a borrow out of a recursive walk is the one shape the borrow checker will
/// not see through.
fn nudge(node: &mut Node, id: u32, by: f32) -> bool {
    match node {
        Node::Leaf(_) => false,
        Node::Split(s) => {
            if s.id == id {
                s.ratio = (s.ratio + by).clamp(0.06, 0.94);
                return true;
            }
            nudge(&mut s.a, id, by) || nudge(&mut s.b, id, by)
        }
    }
}

/// One frame's geometry. Computed before anything is drawn, so that a click can
/// be answered by the layout it actually landed on.
struct Geom {
    bar: Rect,
    button: Rect,
    menu: Option<Rect>,
    items: Vec<(Item, Rect)>,
    /// Topmost first, which is hit-test order and the reverse of draw order.
    containers: Vec<Cont>,
}

struct Cont {
    which: Which,
    rect: Rect,
    leaves: Vec<LeafGeom>,
    /// Splitter handles: the split's id, its rectangle, and its direction.
    splits: Vec<(u32, Rect, bool)>,
    /// An edge strip's own resize handle.
    handle: Option<Rect>,
}

#[derive(Clone, Copy)]
enum Which {
    Float(usize),
    Edge(Side),
}

struct LeafGeom {
    id: u32,
    rect: Rect,
    header: Rect,
    tabs: Vec<(View, Rect)>,
    close: Rect,
    content: Rect,
    view: View,
}

impl Dock {
    fn layout(&self, list: &DrawList) -> Geom {
        let line = list.atlas().line_h;
        let em = list.atlas().advance;
        let bar_h = line + 10.0;
        let bar = (0.0, 0.0, list.width, bar_h);
        let button = (8.0, 3.0, list.measure("Windows") + em * 3.0, bar_h - 6.0);

        let menu = self.menu.then(|| {
            let w = (em * 30.0).min(list.width - 16.0);
            let rows = View::ALL.len() as f32 + 3.5;
            let h = (rows * (line + 6.0) + 12.0).min(list.height - bar_h - 16.0);
            (button.0, bar_h + 2.0, w, h)
        });
        let items = match menu {
            None => Vec::new(),
            Some(panel) => {
                let step = line + 6.0;
                let mut rows = Vec::with_capacity(View::ALL.len() + 3);
                let mut y = panel.1 + 6.0 - self.menu_scroll;
                for view in View::ALL {
                    rows.push((Item::Toggle(view), (panel.0 + 4.0, y, panel.2 - 8.0, step)));
                    y += step;
                }
                y += step * 0.5;
                for item in [Item::ShowAll, Item::HideAll, Item::Reset] {
                    rows.push((item, (panel.0 + 4.0, y, panel.2 - 8.0, step)));
                    y += step;
                }
                rows
            }
        };

        // The dockspace is everything the menu bar is not.
        let mut work = (0.0, bar_h, list.width, (list.height - bar_h).max(0.0));
        let mut containers = Vec::new();
        let mut edges = Vec::new();
        for side in Side::ALL {
            let slot = side.index();
            let Some(node) = self.edges[slot].as_ref() else {
                continue;
            };
            if !node.visible(&self.hidden) {
                continue;
            }
            let want = match self.extents[slot] > 0.0 {
                true => self.extents[slot],
                false => match side.stacks() {
                    true => (list.width * 0.30).clamp(MIN_W, 560.0),
                    false => (list.height * 0.30).clamp(MIN_H, 460.0),
                },
            };
            let (rect, handle) = match side {
                Side::Left => {
                    let w = want.clamp(MIN_W, (work.2 - MIN_W).max(MIN_W));
                    let r = (work.0, work.1, w, work.3);
                    work = (work.0 + w + SPLIT_W, work.1, work.2 - w - SPLIT_W, work.3);
                    (r, (r.0 + r.2, r.1, SPLIT_W, r.3))
                }
                Side::Right => {
                    let w = want.clamp(MIN_W, (work.2 - MIN_W).max(MIN_W));
                    let r = (work.0 + work.2 - w, work.1, w, work.3);
                    work = (work.0, work.1, work.2 - w - SPLIT_W, work.3);
                    (r, (r.0 - SPLIT_W, r.1, SPLIT_W, r.3))
                }
                Side::Top => {
                    let h = want.clamp(MIN_H, (work.3 - MIN_H).max(MIN_H));
                    let r = (work.0, work.1, work.2, h);
                    work = (work.0, work.1 + h + SPLIT_W, work.2, work.3 - h - SPLIT_W);
                    (r, (r.0, r.1 + r.3, r.2, SPLIT_W))
                }
                Side::Bottom => {
                    let h = want.clamp(MIN_H, (work.3 - MIN_H).max(MIN_H));
                    let r = (work.0, work.1 + work.3 - h, work.2, h);
                    work = (work.0, work.1, work.2, work.3 - h - SPLIT_W);
                    (r, (r.0, r.1 - SPLIT_W, r.2, SPLIT_W))
                }
            };
            let mut cont = Cont {
                which: Which::Edge(side),
                rect,
                leaves: Vec::new(),
                splits: Vec::new(),
                handle: Some(handle),
            };
            self.layout_node(node, rect, list, &mut cont);
            edges.push(cont);
        }

        // Floats first in the hit-test order, topmost of them first of all.
        for (index, float) in self.floats.iter().enumerate().rev() {
            if !float.node.visible(&self.hidden) {
                continue;
            }
            let rect = clamp_rect(float.rect, list.width, list.height, bar_h);
            let mut cont = Cont {
                which: Which::Float(index),
                rect,
                leaves: Vec::new(),
                splits: Vec::new(),
                handle: None,
            };
            self.layout_node(&float.node, rect, list, &mut cont);
            containers.push(cont);
        }
        containers.extend(edges);

        Geom {
            bar,
            button,
            menu,
            items,
            containers,
        }
    }

    fn layout_node(&self, node: &Node, rect: Rect, list: &DrawList, out: &mut Cont) {
        match node {
            Node::Leaf(leaf) => {
                let Some(view) = leaf.current(&self.hidden) else {
                    return;
                };
                let line = list.atlas().line_h;
                let em = list.atlas().advance;
                let head_h = (line + 8.0).min(rect.3);
                let header = (rect.0, rect.1, rect.2, head_h);
                let close_w = em * 2.5;
                let close = (
                    rect.0 + rect.2 - close_w - 2.0,
                    rect.1 + 2.0,
                    close_w,
                    head_h - 4.0,
                );

                let shown = leaf.shown(&self.hidden);
                let room = (rect.2 - close_w - 8.0).max(em * 4.0);
                let natural: Vec<f32> = shown
                    .iter()
                    .map(|v| list.measure(v.title()) + em * 2.0)
                    .collect();
                let total: f32 = natural.iter().sum();
                let scale = match total > room {
                    true => room / total,
                    false => 1.0,
                };
                let mut tabs = Vec::with_capacity(shown.len());
                let mut x = rect.0 + 2.0;
                for (view, width) in shown.iter().zip(natural) {
                    let w = (width * scale).max(em * 3.0);
                    tabs.push((*view, (x, rect.1 + 2.0, w - 2.0, head_h - 4.0)));
                    x += w;
                }
                let content = (
                    rect.0 + 1.0,
                    rect.1 + head_h,
                    (rect.2 - 2.0).max(0.0),
                    (rect.3 - head_h - 1.0).max(0.0),
                );
                out.leaves.push(LeafGeom {
                    id: leaf.id,
                    rect,
                    header,
                    tabs,
                    close,
                    content,
                    view,
                });
            }
            Node::Split(split) => {
                let (av, bv) = (split.a.visible(&self.hidden), split.b.visible(&self.hidden));
                match (av, bv) {
                    (false, false) => {}
                    (true, false) => self.layout_node(&split.a, rect, list, out),
                    (false, true) => self.layout_node(&split.b, rect, list, out),
                    (true, true) => {
                        let span = match split.vertical {
                            true => rect.3,
                            false => rect.2,
                        };
                        let first = ((span - SPLIT_W) * split.ratio).round().max(0.0);
                        let second = (span - SPLIT_W - first).max(0.0);
                        let (ra, rb, handle) = match split.vertical {
                            true => (
                                (rect.0, rect.1, rect.2, first),
                                (rect.0, rect.1 + first + SPLIT_W, rect.2, second),
                                (rect.0, rect.1 + first, rect.2, SPLIT_W),
                            ),
                            false => (
                                (rect.0, rect.1, first, rect.3),
                                (rect.0 + first + SPLIT_W, rect.1, second, rect.3),
                                (rect.0 + first, rect.1, SPLIT_W, rect.3),
                            ),
                        };
                        out.splits.push((split.id, handle, split.vertical));
                        self.layout_node(&split.a, ra, list, out);
                        self.layout_node(&split.b, rb, list, out);
                    }
                }
            }
        }
    }
}

/// Keep a float reachable however far it has been dragged.
fn clamp_rect(rect: Rect, width: f32, height: f32, bar: f32) -> Rect {
    let w = rect.2.clamp(MIN_W, width.max(MIN_W));
    let h = rect.3.clamp(MIN_H, (height - bar).max(MIN_H));
    let x = rect.0.clamp(24.0 - w, (width - 24.0).max(0.0));
    let y = rect.1.clamp(bar, (height - 24.0).max(bar));
    (x, y, w, h)
}

/// One frame of input, drained from [`super::input`].
#[derive(Default)]
pub struct Events {
    /// Where the pointer is, or `None` while the guest has the mouse.
    pub at: Option<(f32, f32)>,
    pub press: Option<(f32, f32)>,
    pub released: bool,
    /// How far the pointer moved since the last frame, in pixels.
    pub delta: (f32, f32),
    /// `(x, y, pixels)`, positive upwards.
    pub wheel: Option<(f32, f32, f32)>,
    /// Typed characters, as ASCII. The atlas has no others.
    pub keys: Vec<u8>,
}

fn grow(r: Rect, by: f32) -> Rect {
    (r.0 - by, r.1 - by, r.2 + by * 2.0, r.3 + by * 2.0)
}

/// The leaf whose content is under `(x, y)`, topmost first.
fn content_at(g: &Geom, x: f32, y: f32) -> Option<u32> {
    g.containers
        .iter()
        .flat_map(|c| c.leaves.iter())
        .find(|l| hit(l.content, x, y))
        .map(|l| l.id)
}

/// Which fifth of a window a drop at `(x, y)` is aiming at.
fn zone_of(r: Rect, x: f32, y: f32) -> Zone {
    let fx = ((x - r.0) / r.2.max(1.0)).clamp(0.0, 1.0);
    let fy = ((y - r.1) / r.3.max(1.0)).clamp(0.0, 1.0);
    if (0.30..0.70).contains(&fx) && (0.30..0.70).contains(&fy) {
        return Zone::Center;
    }
    [
        (fx, Zone::Left),
        (1.0 - fx, Zone::Right),
        (fy, Zone::Top),
        (1.0 - fy, Zone::Bottom),
    ]
    .into_iter()
    .fold((f32::MAX, Zone::Center), |best, next| {
        match next.0 < best.0 {
            true => next,
            false => best,
        }
    })
    .1
}

impl Dock {
    /// Where a window dropped at `(x, y)` would go.
    fn target_at(&self, g: &Geom, x: f32, y: f32, surface: (f32, f32)) -> Target {
        let bar = g.bar.3;
        // The surface's own edges win over whatever is drawn against them:
        // this is how a strip gains its second window.
        if x <= EDGE_ZONE {
            return Target::Edge(Side::Left);
        }
        if x >= surface.0 - EDGE_ZONE {
            return Target::Edge(Side::Right);
        }
        if y <= bar + EDGE_ZONE {
            return Target::Edge(Side::Top);
        }
        if y >= surface.1 - EDGE_ZONE {
            return Target::Edge(Side::Bottom);
        }
        for leaf in g.containers.iter().flat_map(|c| c.leaves.iter()) {
            if hit(leaf.rect, x, y) {
                return Target::Onto {
                    leaf: leaf.id,
                    zone: zone_of(leaf.rect, x, y),
                };
            }
        }
        Target::Free
    }

    /// The rectangle to shade so the drop is not a guess.
    fn preview(&self, g: &Geom, target: Target, drag: &Drag, surface: (f32, f32)) -> Rect {
        let bar = g.bar.3;
        let work = (0.0, bar, surface.0, (surface.1 - bar).max(0.0));
        match target {
            Target::Edge(side) => {
                let slot = side.index();
                let want = match self.extents[slot] > 0.0 {
                    true => self.extents[slot],
                    false => match side.stacks() {
                        true => (surface.0 * 0.30).clamp(MIN_W, 560.0),
                        false => (surface.1 * 0.30).clamp(MIN_H, 460.0),
                    },
                };
                match side {
                    Side::Left => (work.0, work.1, want, work.3),
                    Side::Right => (work.0 + work.2 - want, work.1, want, work.3),
                    Side::Top => (work.0, work.1, work.2, want),
                    Side::Bottom => (work.0, work.1 + work.3 - want, work.2, want),
                }
            }
            Target::Onto { leaf, zone } => {
                let Some(lg) = g
                    .containers
                    .iter()
                    .flat_map(|c| c.leaves.iter())
                    .find(|l| l.id == leaf)
                else {
                    return (drag.at.0, drag.at.1, drag.size.0, drag.size.1);
                };
                let r = lg.rect;
                match zone {
                    Zone::Center => r,
                    Zone::Left => (r.0, r.1, r.2 * 0.5, r.3),
                    Zone::Right => (r.0 + r.2 * 0.5, r.1, r.2 * 0.5, r.3),
                    Zone::Top => (r.0, r.1, r.2, r.3 * 0.5),
                    Zone::Bottom => (r.0, r.1 + r.3 * 0.5, r.2, r.3 * 0.5),
                }
            }
            Target::Free => (
                drag.at.0 - drag.grab.0,
                drag.at.1 - drag.grab.1,
                drag.size.0,
                drag.size.1,
            ),
        }
    }

    fn nudge_split(&mut self, id: u32, by: f32) {
        self.edges
            .iter_mut()
            .flatten()
            .chain(self.floats.iter_mut().map(|f| &mut f.node))
            .any(|n| nudge(n, id, by));
    }

    /// Move a float to the top of the pile. Returns its new index.
    fn raise(&mut self, index: usize) -> usize {
        if index + 1 < self.floats.len() {
            let float = self.floats.remove(index);
            self.floats.push(float);
        }
        self.floats.len().saturating_sub(1)
    }

    fn act(&mut self, item: Item, surface: (f32, f32)) {
        match item {
            Item::Toggle(view) => self.toggle(view, surface),
            Item::ShowAll => {
                for view in View::ALL {
                    if self.hidden[view.slot()] {
                        self.toggle(view, surface);
                    }
                }
            }
            Item::HideAll => self.hidden = [true; VIEWS],
            Item::Reset => self.reset(),
        }
    }

    /// Give up whatever gesture was in flight, putting its window down where it
    /// is. Called when the mouse goes back to the guest mid-drag.
    fn cancel(&mut self, surface: (f32, f32)) {
        if let Some(drag) = self.drag.take() {
            self.place(drag, Target::Free, surface);
        }
        self.hold = None;
    }

    fn interact(&mut self, g: &Geom, ev: &Events, input: &mut Input, surface: (f32, f32)) {
        if let Some((wx, wy, pixels)) = ev.wheel {
            match g.menu.filter(|m| hit(*m, wx, wy)) {
                Some(panel) => {
                    let rows = g.items.len() as f32;
                    let step = g.items.first().map(|(_, r)| r.3).unwrap_or(1.0);
                    let reach = (rows * step + 12.0 - panel.3).max(0.0);
                    self.menu_scroll = (self.menu_scroll - pixels).clamp(0.0, reach);
                }
                None => {
                    if let Some(id) = content_at(g, wx, wy)
                        && let Some(leaf) = self.find(id)
                    {
                        leaf.scroll = (leaf.scroll - pixels).clamp(0.0, leaf.reach);
                    }
                }
            }
        }

        // A gesture in progress owns the mouse until the button comes up.
        if let Some(hold) = self.hold {
            let (dx, dy) = ev.delta;
            match hold {
                Hold::Resize {
                    float,
                    left,
                    right,
                    top,
                    bottom,
                } => {
                    if let Some(f) = self.floats.get_mut(float) {
                        // A left or top edge moves the origin and takes the same
                        // amount off the size, which is what keeps the opposite
                        // edge still - the property that makes this a resize.
                        if left {
                            let w = (f.rect.2 - dx).max(MIN_W);
                            f.rect.0 += f.rect.2 - w;
                            f.rect.2 = w;
                        }
                        if right {
                            f.rect.2 = (f.rect.2 + dx).max(MIN_W);
                        }
                        if top {
                            let h = (f.rect.3 - dy).max(MIN_H);
                            f.rect.1 += f.rect.3 - h;
                            f.rect.3 = h;
                        }
                        if bottom {
                            f.rect.3 = (f.rect.3 + dy).max(MIN_H);
                        }
                    }
                }
                Hold::Splitter { id, vertical, span } => {
                    let step = match vertical {
                        true => dy,
                        false => dx,
                    } / span.max(1.0);
                    self.nudge_split(id, step);
                }
                Hold::EdgeSize(side) => {
                    let slot = side.index();
                    let step = match side {
                        Side::Left => dx,
                        Side::Right => -dx,
                        Side::Top => dy,
                        Side::Bottom => -dy,
                    };
                    let floor = match side.stacks() {
                        true => MIN_W,
                        false => MIN_H,
                    };
                    self.extents[slot] = (self.extents[slot] + step).max(floor);
                }
                Hold::Grab { leaf, tab, from } => {
                    if let Some(at) = ev.at
                        && (at.0 - from.0).hypot(at.1 - from.1) > SLOP
                    {
                        self.begin_drag(g, leaf, tab, at);
                    }
                }
                Hold::Drag => {
                    if let Some(at) = ev.at
                        && let Some(drag) = self.drag.as_mut()
                    {
                        drag.at = at;
                    }
                }
            }
            if ev.released {
                if let Some(drag) = self.drag.take() {
                    let target = self.target_at(g, drag.at.0, drag.at.1, surface);
                    self.place(drag, target, surface);
                }
                self.hold = None;
            }
            input.press = None;
            return;
        }

        let Some((px, py)) = input.press else {
            return;
        };

        if let Some(panel) = g.menu
            && hit(panel, px, py)
        {
            input.press = None;
            for (item, rect) in &g.items {
                // Clipped rows are drawn nowhere and must not be clickable.
                if hit(*rect, px, py) && rect.1 >= panel.1 && rect.1 + rect.3 <= panel.1 + panel.3 {
                    self.act(*item, surface);
                    break;
                }
            }
            return;
        }
        if hit(g.button, px, py) {
            self.menu = !self.menu;
            self.menu_scroll = 0.0;
            input.press = None;
            end_edit(true);
            return;
        }
        self.menu = false;
        if hit(g.bar, px, py) {
            input.press = None;
            end_edit(true);
            return;
        }

        for cont in g.containers.iter() {
            let float = match cont.which {
                Which::Float(index) => Some(index),
                Which::Edge(_) => None,
            };
            // A float's border reaches a little outside it, so a window flush
            // against another is still grabbable.
            if let Some(index) = float
                && hit(grow(cont.rect, GRIP), px, py)
            {
                let (x, y, w, h) = cont.rect;
                let (left, right) = (px <= x + GRIP, px >= x + w - GRIP);
                let (top, bottom) = (py <= y + GRIP, py >= y + h - GRIP);
                if left || right || top || bottom {
                    let index = self.raise(index);
                    self.hold = Some(Hold::Resize {
                        float: index,
                        left,
                        right,
                        top,
                        bottom,
                    });
                    input.press = None;
                    end_edit(true);
                    return;
                }
            }
            for (id, handle, vertical) in &cont.splits {
                if hit(grow(*handle, 2.0), px, py) {
                    let span = match vertical {
                        true => cont.rect.3,
                        false => cont.rect.2,
                    };
                    self.hold = Some(Hold::Splitter {
                        id: *id,
                        vertical: *vertical,
                        span,
                    });
                    input.press = None;
                    end_edit(true);
                    return;
                }
            }
            if let Some(handle) = cont.handle
                && hit(grow(handle, 2.0), px, py)
                && let Which::Edge(side) = cont.which
            {
                // The handle is where the strip actually ended up, which is also
                // the number a drag has to start from.
                self.extents[side.index()] = match side.stacks() {
                    true => cont.rect.2,
                    false => cont.rect.3,
                };
                self.hold = Some(Hold::EdgeSize(side));
                input.press = None;
                end_edit(true);
                return;
            }
            if !hit(cont.rect, px, py) {
                continue;
            }
            if let Some(index) = float {
                self.raise(index);
            }
            for leaf in &cont.leaves {
                if hit(leaf.close, px, py) {
                    self.hidden[leaf.view.slot()] = true;
                    input.press = None;
                    end_edit(true);
                    return;
                }
                let tab = leaf
                    .tabs
                    .iter()
                    .find(|(_, r)| hit(*r, px, py))
                    .map(|(v, _)| *v);
                if let Some(view) = tab {
                    if let Some(l) = self.find(leaf.id) {
                        l.active = view;
                    }
                    self.hold = Some(Hold::Grab {
                        leaf: leaf.id,
                        tab: Some(view),
                        from: (px, py),
                    });
                    input.press = None;
                    end_edit(true);
                    return;
                }
                if hit(leaf.header, px, py) {
                    self.hold = Some(Hold::Grab {
                        leaf: leaf.id,
                        tab: None,
                        from: (px, py),
                    });
                    input.press = None;
                    end_edit(true);
                    return;
                }
            }
            // Inside a window but not on its chrome: the press belongs to
            // whatever the view drew there, and nothing below this one.
            return;
        }
    }

    /// Turn a held press into a window in flight.
    fn begin_drag(&mut self, g: &Geom, leaf: u32, tab: Option<View>, at: (f32, f32)) {
        let rect = g
            .containers
            .iter()
            .flat_map(|c| c.leaves.iter())
            .find(|l| l.id == leaf)
            .map(|l| l.rect)
            .unwrap_or((at.0, at.1, 320.0, 240.0));
        let Some(taken) = self.detach(leaf, tab) else {
            self.hold = None;
            return;
        };
        self.drag = Some(Drag {
            leaf: taken,
            size: (rect.2, rect.3),
            grab: (
                (at.0 - rect.0).clamp(0.0, rect.2),
                (at.1 - rect.1).clamp(0.0, 24.0),
            ),
            at,
        });
        self.hold = Some(Hold::Drag);
    }
}

impl Dock {
    fn draw(&mut self, list: &mut DrawList, model: &Model, g: &Geom, mut input: Input) {
        // Only the topmost window under the pointer may take a click, so a
        // button on a window behind another is not pressed through it.
        let hot = input
            .at
            .and_then(|(x, y)| g.containers.iter().position(|c| hit(c.rect, x, y)));
        for (index, cont) in g.containers.iter().enumerate().rev() {
            list.fill(cont.rect, color::PANEL);
            for (_, handle, _) in &cont.splits {
                list.fill(*handle, color::SPLIT);
            }
            if let Some(handle) = cont.handle {
                list.fill(handle, color::SPLIT);
            }
            for leaf in &cont.leaves {
                let feed = match Some(index) == hot {
                    true => input,
                    false => Input::default(),
                };
                let left = self.draw_leaf(list, model, leaf, feed);
                if Some(index) == hot {
                    input = left;
                }
            }
            list.outline(cont.rect, color::EDGE);
        }
        self.draw_bar(list, g, input);
        self.draw_drag(list, g);
    }

    /// One window: its tab strip, and as much of its view as fits.
    fn draw_leaf(
        &mut self,
        list: &mut DrawList,
        model: &Model,
        lg: &LeafGeom,
        input: Input,
    ) -> Input {
        let em = list.atlas().advance;
        list.fill(lg.header, color::HEADER);
        for (view, rect) in &lg.tabs {
            let on = *view == lg.view;
            let fill = match (on, input.over(*rect)) {
                (true, _) => color::TAB_ACTIVE,
                (false, true) => color::BUTTON_HOT,
                (false, false) => color::TAB,
            };
            list.fill(*rect, fill);
            list.elided(
                rect.0 + em * 0.6,
                rect.1 + 3.0,
                view.title(),
                rect.2 - em * 1.2,
                match on {
                    true => color::VALUE,
                    false => color::LABEL,
                },
            );
        }
        if input.over(lg.close) {
            list.fill(lg.close, color::BUTTON_HOT);
        }
        list.text(lg.close.0 + em * 0.75, lg.close.1 + 3.0, "x", color::LABEL);

        let scroll = self.find(lg.id).map(|l| l.scroll).unwrap_or(0.0);
        let inner = (
            lg.content.0 + 8.0,
            lg.content.1 + 6.0,
            (lg.content.2 - 20.0).max(em * 8.0),
        );
        // Laid out once against the list to learn how tall the content wants to
        // be, then thrown away and laid out again for real. Measuring is what
        // the scrollbar and the wheel's limit are both made of.
        let mark = list.vertices.len();
        let mut probe = Ui::new(list, inner.0, inner.1, inner.2);
        super::panel::body(&mut probe, model, lg.view);
        let wanted = probe.y - inner.1;
        list.vertices.truncate(mark);

        let visible = lg.content.3 - 12.0;
        let reach = (wanted - visible).max(0.0);
        let scroll = scroll.clamp(0.0, reach);
        if let Some(leaf) = self.find(lg.id) {
            leaf.reach = reach;
            leaf.scroll = scroll;
        }

        list.push_clip(lg.content);
        let mut ui = Ui::new(list, inner.0, inner.1 - scroll, inner.2);
        ui.input = input;
        super::panel::body(&mut ui, model, lg.view);
        let left = ui.input;
        list.pop_clip();

        if reach > 0.0 && visible > 0.0 {
            let track = lg.content;
            let thumb = (track.3 * visible / wanted.max(1.0)).max(12.0);
            let travel = track.3 - thumb;
            let at = scroll / reach;
            list.rect(track.0 + track.2 - 4.0, track.1, 3.0, track.3, color::TRACK);
            list.rect(
                track.0 + track.2 - 4.0,
                track.1 + travel * at,
                3.0,
                thumb,
                color::EDGE,
            );
        }
        left
    }

    /// The menu bar, and the Windows list when it is down.
    fn draw_bar(&mut self, list: &mut DrawList, g: &Geom, input: Input) {
        let em = list.atlas().advance;
        list.fill(g.bar, color::HEADER);
        list.rect(0.0, g.bar.3 - 1.0, g.bar.2, 1.0, color::EDGE);

        let fill = match (self.menu, input.over(g.button)) {
            (true, _) => color::TAB_ACTIVE,
            (false, true) => color::BUTTON_HOT,
            (false, false) => color::BUTTON,
        };
        list.fill(g.button, fill);
        list.outline(g.button, color::EDGE);
        list.text(g.button.0 + em, g.button.1 + 3.0, "Windows", color::VALUE);

        let mut x = g.button.0 + g.button.2 + em * 2.0;
        x += list.text(x, 5.0, "rosemary", color::HEADING) + em;
        // The main executable is the name the person watching recognises.
        let name = super::program_name().unwrap_or("no program");
        list.text(x, 5.0, name, color::VALUE);

        // Said in the loudest colour available, because while the overlay has
        // the mouse the guest is being denied it - and a game that has silently
        // stopped responding to the mouse is a bug report about the game.
        let (hint, colour) = match super::input::focused() {
            true => ("shift+F8 releases the mouse", color::ALERT),
            false => ("shift+F8 gives the overlay the mouse", color::MUTED),
        };
        let width = list.measure(hint);
        if g.bar.2 - width - 12.0 > x + em * 2.0 {
            list.text(g.bar.2 - width - 12.0, 5.0, hint, colour);
        }

        let Some(panel) = g.menu else {
            return;
        };
        list.fill(panel, color::PANEL);
        list.outline(panel, color::EDGE);
        list.push_clip(panel);
        for (item, rect) in &g.items {
            if input.over(*rect) {
                list.fill(*rect, color::BUTTON_HOT);
            }
            let (mark, label, colour) = match item {
                Item::Toggle(view) => (
                    match self.hidden[view.slot()] {
                        true => "[ ]",
                        false => "[x]",
                    },
                    view.title(),
                    match self.hidden[view.slot()] {
                        true => color::MUTED,
                        false => color::VALUE,
                    },
                ),
                Item::ShowAll => ("   ", "show all", color::LABEL),
                Item::HideAll => ("   ", "hide all", color::LABEL),
                Item::Reset => ("   ", "reset layout", color::LABEL),
            };
            list.text(rect.0 + 6.0, rect.1 + 3.0, mark, color::HEADING);
            list.text(rect.0 + 6.0 + em * 4.0, rect.1 + 3.0, label, colour);
        }
        list.pop_clip();
    }

    /// The window in flight, and where it would land.
    fn draw_drag(&self, list: &mut DrawList, g: &Geom) {
        let Some(drag) = self.drag.as_ref() else {
            return;
        };
        let surface = (list.width, list.height);
        let target = self.target_at(g, drag.at.0, drag.at.1, surface);
        let preview = self.preview(g, target, drag, surface);
        list.fill(preview, color::DROP);
        list.outline(preview, color::DROP_EDGE);

        let em = list.atlas().advance;
        let title = drag.leaf.active.title();
        let ghost = (
            drag.at.0 - drag.grab.0,
            drag.at.1 - drag.grab.1,
            list.measure(title) + em * 3.0,
            list.atlas().line_h + 8.0,
        );
        list.fill(ghost, color::TAB_ACTIVE);
        list.outline(ghost, color::DROP_EDGE);
        list.text(ghost.0 + em, ghost.1 + 4.0, title, color::VALUE);
    }
}

/// The one dock, and the rectangles it drew last frame.
static DOCK: Mutex<Option<Dock>> = Mutex::new(None);
static OCCUPIED: Mutex<Vec<Rect>> = Mutex::new(Vec::new());

/// Whether `(x, y)` is over anything the overlay drew.
///
/// Read by the event monitor, which has to answer synchronously and cannot wait
/// on whatever the render thread is doing to the dock itself.
pub fn occupied(x: f32, y: f32) -> bool {
    let guard = OCCUPIED.lock().unwrap_or_else(|e| e.into_inner());
    guard.iter().any(|r| hit(*r, x, y))
}

/// A text box the keyboard can be pointed at.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    TraceDir,
}

static EDIT: Mutex<Option<(Field, String)>> = Mutex::new(None);

/// The text being typed into `field`, if it is the one being edited.
pub fn editing(field: Field) -> Option<String> {
    let guard = EDIT.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .as_ref()
        .filter(|(f, _)| *f == field)
        .map(|(_, text)| text.clone())
}

/// Whether the keyboard is going to a text box rather than to the guest.
pub fn typing() -> bool {
    EDIT.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Point the keyboard at `field`, starting from `initial`.
pub fn begin_edit(field: Field, initial: &str) {
    let mut guard = EDIT.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some((field, initial.to_string()));
}

/// Finish editing, keeping what was typed or throwing it away.
fn end_edit(commit: bool) {
    let taken = {
        let mut guard = EDIT.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    // Outside the lock: applying the value takes another one, and a fixed order
    // between two locks is the only thing that makes a deadlock impossible.
    if let (true, Some((Field::TraceDir, text))) = (commit, taken) {
        super::trace::set_directory(&text);
    }
}

/// Feed typed characters to whatever text box is open.
fn apply_keys(keys: &[u8]) {
    if keys.is_empty() {
        return;
    }
    let mut done = None;
    {
        let mut guard = EDIT.lock().unwrap_or_else(|e| e.into_inner());
        let Some((_, text)) = guard.as_mut() else {
            return;
        };
        for &key in keys {
            match key {
                0x08 | 0x7f => {
                    text.pop();
                }
                b'\r' | b'\n' => done = Some(true),
                0x1b => done = Some(false),
                // The atlas is printable ASCII and nothing else draws.
                0x20..=0x7e => text.push(key as char),
                _ => {}
            }
        }
    }
    if let Some(commit) = done {
        end_edit(commit);
    }
}

/// Lay the windows out, act on this frame's input, and draw them.
pub fn run(list: &mut DrawList, model: &Model, events: Events) {
    apply_keys(&events.keys);

    let mut guard = DOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dock = guard.get_or_insert_with(Dock::default);
    let surface = (list.width, list.height);
    dock.surface = surface;
    let geom = dock.layout(list);
    let mut input = Input {
        at: events.at,
        press: events.press,
    };
    dock.interact(&geom, &events, &mut input, surface);
    dock.draw(list, model, &geom, input);

    let mut occupied = Vec::with_capacity(geom.containers.len() + 2);
    occupied.push(geom.bar);
    if let Some(panel) = geom.menu {
        occupied.push(panel);
    }
    // Grown by the grip, because the border a float is resized by is outside it.
    occupied.extend(geom.containers.iter().map(|c| grow(c.rect, GRIP)));
    *OCCUPIED.lock().unwrap_or_else(|e| e.into_inner()) = occupied;
}

/// The overlay is closed: nothing of it is on screen, so nothing of it should
/// be taking the guest's clicks either.
pub fn dismiss() {
    OCCUPIED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    end_edit(false);
}

/// Give up whatever gesture is in flight. Called when the mouse goes back to
/// the guest, which can happen in the middle of a drag.
pub fn abandon() {
    let mut guard = DOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(dock) = guard.as_mut() {
        let surface = dock.surface;
        dock.cancel(surface);
    }
}

/// Serializes every test that drives the one shared [`DOCK`], including the
/// ones in [`super`] that reach it through `compose`.
#[cfg(test)]
pub fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where a view's tab ended up, for a test that wants to press on it.
#[cfg(test)]
fn tab_at(list: &DrawList, view: View) -> Option<Rect> {
    let guard = DOCK.lock().unwrap_or_else(|e| e.into_inner());
    let geom = guard.as_ref()?.layout(list);
    geom.containers
        .iter()
        .flat_map(|c| c.leaves.iter())
        .flat_map(|l| l.tabs.iter())
        .find(|(v, _)| *v == view)
        .map(|(_, r)| *r)
}

/// Forget every window's position. For the tests, and for a guest whose surface
/// changed size so drastically that nothing is reachable.
#[cfg(test)]
pub fn reset() {
    let mut guard = DOCK.lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(Dock::default).reset();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dock() -> Dock {
        Dock::default()
    }

    /// The default layout is one edge strip holding every view.
    #[test]
    fn every_view_starts_somewhere() {
        let dock = dock();
        for view in View::ALL {
            assert!(dock.holds(view), "{view:?} is in no window");
        }
    }

    /// Hiding a window and showing it again gets it back, wherever it ends up.
    #[test]
    fn a_hidden_window_comes_back() {
        let mut dock = dock();
        dock.toggle(View::Time, (1920.0, 1080.0));
        assert!(dock.hidden[View::Time.slot()]);
        dock.toggle(View::Time, (1920.0, 1080.0));
        assert!(!dock.hidden[View::Time.slot()]);
        assert!(dock.holds(View::Time));
    }

    /// A view dragged out of its group leaves the group behind, and a group
    /// dragged out whole takes its tabs with it.
    #[test]
    fn detaching_takes_a_tab_or_the_whole_group() {
        let mut dock = dock();
        let id = match dock.edges[Side::Left.index()].as_ref() {
            Some(Node::Split(s)) => match s.a.as_ref() {
                Node::Leaf(l) => l.id,
                _ => panic!("the default layout's first child is a leaf"),
            },
            _ => panic!("the default layout is a split"),
        };
        let taken = dock
            .detach(id, Some(View::Trace))
            .expect("one tab out of two");
        assert_eq!(taken.tabs, vec![View::Trace]);
        assert!(dock.holds(View::Frames), "the group survived");
        assert!(!dock.holds(View::Trace), "the tab left");

        let rest = dock.detach(id, None).expect("the whole group");
        assert_eq!(rest.tabs, vec![View::Frames]);
        assert!(!dock.holds(View::Frames));
    }

    /// Dropping onto the centre of a window tabs into it; dropping onto a side
    /// splits it.
    #[test]
    fn a_drop_tabs_or_splits() {
        let mut dock = dock();
        let surface = (1920.0, 1080.0);
        let id = dock.next();
        let payload = Leaf::new(id, vec![View::Keys]);
        let target = match dock.edges[Side::Left.index()].as_ref() {
            Some(Node::Split(s)) => match s.a.as_ref() {
                Node::Leaf(l) => l.id,
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        dock.remove_view(View::Keys);
        dock.place(
            Drag {
                leaf: payload,
                size: (200.0, 200.0),
                grab: (0.0, 0.0),
                at: (0.0, 0.0),
            },
            Target::Onto {
                leaf: target,
                zone: Zone::Center,
            },
            surface,
        );
        let group = dock.find(target).expect("the target group");
        assert!(group.tabs.contains(&View::Keys), "the drop did not tab in");
        assert_eq!(
            group.active,
            View::Keys,
            "the dropped tab is the one showing"
        );
    }

    /// A window dropped on a surface edge with nothing there becomes that
    /// edge's strip; a second one lines up inside it.
    #[test]
    fn edges_line_windows_up() {
        let mut dock = dock();
        let surface = (1920.0, 1080.0);
        for view in [View::Frames, View::Time] {
            let id = dock.next();
            dock.remove_view(view);
            dock.place(
                Drag {
                    leaf: Leaf::new(id, vec![view]),
                    size: (200.0, 200.0),
                    grab: (0.0, 0.0),
                    at: (0.0, 0.0),
                },
                Target::Edge(Side::Bottom),
                surface,
            );
        }
        let Some(Node::Split(split)) = dock.edges[Side::Bottom.index()].as_ref() else {
            panic!("two windows on one edge are a split");
        };
        assert!(!split.vertical, "a bottom strip lines up side by side");
        assert!(
            dock.extents[Side::Bottom.index()] > 0.0,
            "the strip was sized"
        );
    }

    /// Removing the last view of a split dissolves the split rather than
    /// leaving a node with one child, which would take half the space and draw
    /// nothing in it.
    #[test]
    fn a_split_with_one_child_left_dissolves() {
        let mut dock = dock();
        for view in [View::Time, View::HostSymbols] {
            dock.remove_view(view);
        }
        let Some(Node::Split(top)) = dock.edges[Side::Left.index()].as_ref() else {
            panic!("the left strip is a split");
        };
        assert!(
            matches!(top.b.as_ref(), Node::Leaf(_)),
            "the emptied split did not dissolve",
        );
    }

    /// A tab pressed, dragged to the far side of the surface and let go ends up
    /// docked to that edge.
    ///
    /// End to end through [`run`], because every part of the gesture depends on
    /// geometry that only the real layout produces: which tab the press landed
    /// on, whether the movement passed the slop, and which drop zone the
    /// release was over.
    #[test]
    fn a_tab_dragged_to_an_edge_docks_there() {
        let Some(atlas) = super::super::font::atlas(13.0) else {
            // No CoreText, no layout - a supported configuration.
            return;
        };
        let _serial = serial();
        reset();
        let model = Model::default();
        let surface = (1920.0, 1080.0);
        let frame = |events: Events| {
            let mut list = DrawList::new(surface.0, surface.1, atlas);
            run(&mut list, &model, events);
            list
        };

        let list = frame(Events::default());
        let tab = tab_at(&list, View::Trace).expect("the xctrace tab is on screen");
        let from = (tab.0 + tab.2 * 0.5, tab.1 + tab.3 * 0.5);
        frame(Events {
            at: Some(from),
            press: Some(from),
            ..Events::default()
        });
        let to = (surface.0 - 6.0, 500.0);
        frame(Events {
            at: Some(to),
            delta: (to.0 - from.0, to.1 - from.1),
            ..Events::default()
        });
        frame(Events {
            at: Some(to),
            released: true,
            ..Events::default()
        });

        let guard = DOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dock = guard.as_ref().expect("the dock exists by now");
        assert!(
            dock.edges[Side::Right.index()]
                .as_ref()
                .is_some_and(|n| n.holds(View::Trace)),
            "the dragged tab did not dock to the right edge",
        );
        assert!(
            dock.edges[Side::Left.index()]
                .as_ref()
                .is_some_and(|n| n.holds(View::Frames)),
            "the group it came from went with it",
        );
    }

    /// The five drop zones, from the middle outwards.
    #[test]
    fn the_centre_tabs_and_the_sides_split() {
        let rect = (0.0, 0.0, 100.0, 100.0);
        assert_eq!(zone_of(rect, 50.0, 50.0), Zone::Center);
        assert_eq!(zone_of(rect, 4.0, 50.0), Zone::Left);
        assert_eq!(zone_of(rect, 96.0, 50.0), Zone::Right);
        assert_eq!(zone_of(rect, 50.0, 4.0), Zone::Top);
        assert_eq!(zone_of(rect, 50.0, 96.0), Zone::Bottom);
    }
}
