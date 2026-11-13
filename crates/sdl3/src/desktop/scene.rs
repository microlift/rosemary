//! The desktop's window list: where every window is, how they stack, and
//! which of them has the keyboard and the pointer.
//!
//! This is the part of a Mac's window server that a program's windows are
//! known to each other through. A window there is a server-side object with a
//! number, a level and a frame in one coordinate space every window shares;
//! the server keeps the single stacking order, finds the window under the
//! pointer from it, and sends each input event to one window. Nothing here
//! calls SDL, so the list does not care which process a window belongs to.

use std::collections::VecDeque;

use crate::Rect;
use crate::video::{Event, HitTestResult, flags};

/// `kCGPopUpMenuWindowLevel`: the lowest a menu stacks, whatever its level.
const MENU_LEVEL: i32 = 101;
/// `kCGHelpWindowLevel`, likewise for a tooltip.
const TOOLTIP_LEVEL: i32 = 200;

/// SDL's number for the left button, the one that moves and resizes.
const LEFT_BUTTON: u8 = 1;

/// The states in which the desktop, not the program, chooses the frame.
const COVERING: u64 = flags::FULLSCREEN | flags::MAXIMIZED;

/// SDL's id for the window, which is also what an event names it by.
pub(crate) type WindowId = u32;

struct Window {
    id: WindowId,
    /// The window this one stays in front of and is hidden with: the one a
    /// popup is placed against, or the one a modal window is modal for.
    parent: Option<WindowId>,
    /// Whether the parent takes no input while this window is shown.
    modal: bool,
    /// `SDL_WindowFlags`, without the bits [`Scene::flags`] works out.
    flags: u64,
    /// In desktop coordinates, a popup's included.
    frame: Rect,
    /// The frame to go back to from full screen or maximized.
    restore: Rect,
    shown: bool,
    level: i32,
    /// When it was last raised: the order within a level.
    raised: u64,
    opacity: f32,
    minimum: (i32, i32),
}

impl Window {
    fn is_popup(&self) -> bool {
        self.flags & (flags::POPUP_MENU | flags::TOOLTIP) != 0
    }

    /// The frame the program gave it, which is not the one it has while it
    /// fills the desktop.
    fn placed(&self) -> Rect {
        match self.flags & COVERING != 0 {
            true => self.restore,
            false => self.frame,
        }
    }

    fn takes_pointer(&self) -> bool {
        self.flags & flags::TOOLTIP == 0
    }

    fn takes_keyboard(&self) -> bool {
        self.flags & (flags::TOOLTIP | flags::NOT_FOCUSABLE) == 0
    }
}

/// An interactive move or resize, from the press that began it.
struct Session {
    window: WindowId,
    grip: HitTestResult,
    pointer: (f32, f32),
    frame: Rect,
}

pub(crate) struct Scene {
    size: (i32, i32),
    windows: Vec<Window>,
    raises: u64,
    /// The window keys go to, while the desktop itself has the host's focus.
    focus: Option<WindowId>,
    active: bool,
    pointer: (f32, f32),
    /// Whether the pointer is over the desktop at all.
    inside: bool,
    buttons: u32,
    /// The window the pointer was last reported to be in.
    over: Option<WindowId>,
    /// The window a press went to, which keeps the pointer until the release.
    capture: Option<WindowId>,
    session: Option<Session>,
    events: VecDeque<Event>,
    /// What changed on the screen since [`Scene::take_damage`].
    damage: Option<Rect>,
}

fn union(damage: Option<Rect>, rect: Rect) -> Option<Rect> {
    match damage {
        Some(damage) => Some(damage.bounding(rect)),
        None => (!rect.is_empty()).then_some(rect),
    }
}

/// `frame` after the pointer moved by `(dx, dy)` holding `grip`.
fn dragged(frame: Rect, grip: HitTestResult, dx: i32, dy: i32, minimum: (i32, i32)) -> Rect {
    use HitTestResult as Grip;
    let (left, top, right, bottom) = match grip {
        Grip::Normal => return frame,
        Grip::Draggable => {
            return Rect {
                x: frame.x + dx,
                y: frame.y + dy,
                ..frame
            };
        }
        Grip::ResizeTopLeft => (true, true, false, false),
        Grip::ResizeTop => (false, true, false, false),
        Grip::ResizeTopRight => (false, true, true, false),
        Grip::ResizeRight => (false, false, true, false),
        Grip::ResizeBottomRight => (false, false, true, true),
        Grip::ResizeBottom => (false, false, false, true),
        Grip::ResizeBottomLeft => (true, false, false, true),
        Grip::ResizeLeft => (true, false, false, false),
    };
    let (minimum_w, minimum_h) = (minimum.0.max(1), minimum.1.max(1));
    let mut out = frame;
    if left {
        out.w = (frame.w - dx).max(minimum_w);
        out.x = frame.x + frame.w - out.w;
    }
    if right {
        out.w = (frame.w + dx).max(minimum_w);
    }
    if top {
        out.h = (frame.h - dy).max(minimum_h);
        out.y = frame.y + frame.h - out.h;
    }
    if bottom {
        out.h = (frame.h + dy).max(minimum_h);
    }
    out
}

impl Scene {
    pub(crate) fn new(size: (i32, i32)) -> Scene {
        Scene {
            size,
            windows: Vec::new(),
            raises: 0,
            focus: None,
            active: false,
            pointer: (0.0, 0.0),
            inside: false,
            buttons: 0,
            over: None,
            capture: None,
            session: None,
            events: VecDeque::new(),
            damage: None,
        }
    }

    pub(crate) fn size(&self) -> (i32, i32) {
        self.size
    }

    pub(crate) fn bounds(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.size.0,
            h: self.size.1,
        }
    }

    /// The host gave the desktop a new size. Windows that fill it follow.
    pub(crate) fn resize(&mut self, size: (i32, i32)) {
        self.size = size;
        self.damage = Some(self.bounds());
        let covering: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|window| window.flags & COVERING != 0)
            .map(|window| window.id)
            .collect();
        for id in covering {
            self.set_frame(id, self.bounds());
        }
    }

    fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|window| window.id == id)
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|window| window.id == id)
    }

    pub(crate) fn contains(&self, id: WindowId) -> bool {
        self.window(id).is_some()
    }

    /// `offset` is from the parent's origin for a popup, as SDL places one,
    /// and from the desktop's otherwise.
    pub(crate) fn add(
        &mut self,
        id: WindowId,
        parent: Option<WindowId>,
        flags: u64,
        offset: (i32, i32),
        size: (i32, i32),
    ) {
        let origin = parent
            .and_then(|parent| self.window(parent))
            .map_or((0, 0), |parent| (parent.frame.x, parent.frame.y));
        let parent = parent.filter(|parent| self.contains(*parent));
        let frame = Rect {
            x: origin.0 + offset.0,
            y: origin.1 + offset.1,
            w: size.0.max(1),
            h: size.1.max(1),
        };
        self.windows.push(Window {
            id,
            parent,
            modal: false,
            flags: flags & !(flags::HIDDEN | flags::INPUT_FOCUS | COVERING),
            frame,
            restore: frame,
            shown: false,
            level: 0,
            raised: 0,
            opacity: 1.0,
            minimum: (1, 1),
        });
        if flags & flags::HIDDEN == 0 {
            self.show(id);
        }
    }

    pub(crate) fn remove(&mut self, id: WindowId) {
        self.hide(id);
        self.windows.retain(|window| window.id != id);
        for window in &mut self.windows {
            if window.parent == Some(id) {
                window.parent = None;
            }
        }
    }

    /// `id` and every window hanging off it, parents first.
    fn family(&self, id: WindowId) -> Vec<WindowId> {
        self.descendants(id, false)
    }

    /// `id` and the popups placed against it, which go where it goes.
    fn attached(&self, id: WindowId) -> Vec<WindowId> {
        self.descendants(id, true)
    }

    fn descendants(&self, id: WindowId, popups_only: bool) -> Vec<WindowId> {
        let mut family = vec![id];
        let mut next = 0;
        while next < family.len() {
            let parent = family[next];
            let mut children: Vec<&Window> = self
                .windows
                .iter()
                .filter(|window| window.parent == Some(parent) && !family.contains(&window.id))
                .filter(|window| window.is_popup() || !popups_only)
                .collect();
            children.sort_by_key(|window| window.raised);
            family.extend(children.into_iter().map(|window| window.id));
            next += 1;
        }
        family
    }

    /// Shown, and so is everything it hangs off: SDL hides a popup with its
    /// parent.
    fn visible(&self, window: &Window) -> bool {
        window.shown
            && window
                .parent
                .and_then(|parent| self.window(parent))
                .is_none_or(|parent| self.visible(parent))
    }

    /// The level a window stacks at: a popup is never under what it hangs off.
    fn stacking_level(&self, window: &Window) -> i32 {
        let floor = match window.flags {
            flags if flags & flags::TOOLTIP != 0 => TOOLTIP_LEVEL,
            flags if flags & flags::POPUP_MENU != 0 => MENU_LEVEL,
            _ => i32::MIN,
        };
        let parent = window
            .parent
            .and_then(|parent| self.window(parent))
            .map_or(i32::MIN, |parent| self.stacking_level(parent));
        window.level.max(floor).max(parent)
    }

    /// The windows on the screen, back to front.
    pub(crate) fn stack(&self) -> Vec<WindowId> {
        let mut stack: Vec<&Window> = self
            .windows
            .iter()
            .filter(|window| self.visible(window))
            .collect();
        stack.sort_by_key(|window| (self.stacking_level(window), window.raised));
        stack.into_iter().map(|window| window.id).collect()
    }

    /// The frontmost window that takes the pointer at a desktop point.
    pub(crate) fn window_at(&self, x: f32, y: f32) -> Option<WindowId> {
        let (x, y) = (x.floor() as i32, y.floor() as i32);
        self.stack().into_iter().rev().find(|id| {
            self.window(*id)
                .is_some_and(|window| window.takes_pointer() && window.frame.contains(x, y))
        })
    }

    /// Everywhere [`Scene::window_at`] finds a window, as rectangles in no
    /// order that means anything: the pointer is nobody's anywhere else.
    pub(crate) fn pointer_region(&self) -> Vec<Rect> {
        let mut region: Vec<Rect> = self
            .windows
            .iter()
            .filter(|window| self.visible(window) && window.takes_pointer())
            .map(|window| window.frame.intersection(self.bounds()))
            .filter(|frame| !frame.is_empty())
            .collect();
        region.sort_by_key(|frame| (frame.x, frame.y, frame.w, frame.h));
        region
    }

    pub(crate) fn frame(&self, id: WindowId) -> Option<Rect> {
        self.window(id).map(|window| window.frame)
    }

    pub(crate) fn parent(&self, id: WindowId) -> Option<WindowId> {
        self.window(id)?.parent
    }

    /// Where SDL would say the window is: from its parent's origin for a
    /// popup, and on the desktop otherwise.
    pub(crate) fn position(&self, id: WindowId) -> Option<(i32, i32)> {
        let window = self.window(id)?;
        let origin = self.parent_origin(window);
        Some((window.frame.x - origin.0, window.frame.y - origin.1))
    }

    fn parent_origin(&self, window: &Window) -> (i32, i32) {
        window
            .parent
            .filter(|_| window.is_popup())
            .and_then(|parent| self.window(parent))
            .map_or((0, 0), |parent| (parent.frame.x, parent.frame.y))
    }

    /// Make `id` modal for `parent`: in front of it, and taking its input.
    pub(crate) fn set_modal_for(&mut self, id: WindowId, parent: WindowId) {
        if !self.contains(parent) || self.family(id).contains(&parent) {
            return;
        }
        if let Some(window) = self.window_mut(id) {
            window.parent = Some(parent);
            window.modal = true;
            self.damage_window(id);
        }
    }

    /// The shown window that is modal for `id`, or for the one that is.
    fn blocker(&self, id: WindowId) -> Option<WindowId> {
        let modal = self
            .windows
            .iter()
            .find(|window| window.modal && window.parent == Some(id) && self.visible(window))?;
        Some(self.blocker(modal.id).unwrap_or(modal.id))
    }

    /// The window the pointer's events go to at a desktop point: the one
    /// there, unless a modal window has taken its input.
    fn target_at(&self, x: f32, y: f32) -> Option<WindowId> {
        self.window_at(x, y)
            .filter(|window| self.blocker(*window).is_none())
    }

    pub(crate) fn flags(&self, id: WindowId) -> Option<u64> {
        let window = self.window(id)?;
        let mut flags = window.flags;
        if !window.shown {
            flags |= flags::HIDDEN;
        }
        if self.active && self.focus == Some(id) {
            flags |= flags::INPUT_FOCUS;
        }
        Some(flags)
    }

    pub(crate) fn opacity(&self, id: WindowId) -> Option<f32> {
        self.window(id).map(|window| window.opacity)
    }

    fn damage_window(&mut self, id: WindowId) {
        if let Some(frame) = self.frame(id) {
            self.damage = union(self.damage, frame);
        }
    }

    fn damage_family(&mut self, id: WindowId) {
        for member in self.family(id) {
            self.damage_window(member);
        }
    }

    /// Move and resize a window, taking its popups along, and say so when it
    /// is on the screen - which is when SDL does.
    fn set_frame(&mut self, id: WindowId, frame: Rect) {
        let Some(window) = self.window(id) else {
            return;
        };
        let (old, shown) = (window.frame, window.shown);
        if old == frame {
            return;
        }
        self.damage_family(id);
        let (dx, dy) = (frame.x - old.x, frame.y - old.y);
        for member in self.attached(id) {
            let Some(window) = self.window_mut(member) else {
                continue;
            };
            match member == id {
                true => window.frame = frame,
                false => {
                    window.frame.x += dx;
                    window.frame.y += dy;
                }
            }
        }
        self.damage_family(id);
        if shown && (dx, dy) != (0, 0) {
            self.events.push_back(Event::WindowMoved { window: id });
        }
        if shown && (frame.w, frame.h) != (old.w, old.h) {
            self.events.push_back(Event::WindowResized {
                window: id,
                width: frame.w,
                height: frame.h,
            });
        }
        self.settle_pointer();
    }

    pub(crate) fn set_position(&mut self, id: WindowId, x: i32, y: i32) {
        let Some(window) = self.window(id) else {
            return;
        };
        let origin = self.parent_origin(window);
        let frame = Rect {
            x: origin.0 + x,
            y: origin.1 + y,
            ..window.placed()
        };
        self.place(id, frame);
    }

    pub(crate) fn set_size(&mut self, id: WindowId, width: i32, height: i32) {
        let Some(window) = self.window(id) else {
            return;
        };
        let frame = Rect {
            w: width.max(1),
            h: height.max(1),
            ..window.placed()
        };
        self.place(id, frame);
    }

    /// A frame the program asked for. While the window fills the desktop it
    /// is where the window goes back to.
    fn place(&mut self, id: WindowId, frame: Rect) {
        let Some(window) = self.window_mut(id) else {
            return;
        };
        if window.flags & COVERING != 0 {
            window.restore = frame;
            return;
        }
        self.set_frame(id, frame);
    }

    pub(crate) fn set_minimum(&mut self, id: WindowId, width: i32, height: i32) {
        if let Some(window) = self.window_mut(id) {
            window.minimum = (width, height);
        }
    }

    pub(crate) fn set_flag(&mut self, id: WindowId, flag: u64, on: bool) {
        if let Some(window) = self.window_mut(id) {
            window.flags = match on {
                true => window.flags | flag,
                false => window.flags & !flag,
            };
        }
    }

    pub(crate) fn set_opacity(&mut self, id: WindowId, opacity: f32) {
        if let Some(window) = self.window_mut(id) {
            window.opacity = opacity.clamp(0.0, 1.0);
            self.damage_window(id);
        }
    }

    pub(crate) fn set_level(&mut self, id: WindowId, level: i32) {
        if let Some(window) = self.window_mut(id) {
            window.level = level;
            self.damage_family(id);
            self.settle_pointer();
        }
    }

    /// Enter or leave full screen or maximized, `state` being that flag.
    pub(crate) fn set_covering(&mut self, id: WindowId, state: u64, on: bool) {
        let bounds = self.bounds();
        let Some(window) = self.window_mut(id) else {
            return;
        };
        if (window.flags & state != 0) == on {
            return;
        }
        if on && window.flags & COVERING == 0 {
            window.restore = window.frame;
        }
        // The frame is the desktop's now, not a drag's.
        self.session.take_if(|session| session.window == id);
        let Some(window) = self.window_mut(id) else {
            return;
        };
        window.flags = match on {
            true => window.flags | state,
            false => window.flags & !state,
        };
        let frame = match window.flags & COVERING != 0 {
            true => bounds,
            false => window.restore,
        };
        self.events.push_back(Event::WindowState { window: id });
        self.set_frame(id, frame);
    }

    pub(crate) fn show(&mut self, id: WindowId) {
        let Some(window) = self.window_mut(id) else {
            return;
        };
        if window.shown {
            return;
        }
        window.shown = true;
        self.raise(id, true);
    }

    pub(crate) fn hide(&mut self, id: WindowId) {
        let Some(window) = self.window(id).filter(|window| window.shown) else {
            return;
        };
        let parent = window.parent;
        self.damage_family(id);
        if let Some(window) = self.window_mut(id) {
            window.shown = false;
        }
        let gone = |scene: &Scene, held: Option<WindowId>| {
            held.and_then(|held| scene.window(held))
                .is_some_and(|held| !scene.visible(held))
        };
        if gone(self, self.capture) {
            self.capture = None;
        }
        if self
            .session
            .as_ref()
            .is_some_and(|session| gone(self, Some(session.window)))
        {
            self.session = None;
        }
        if gone(self, self.focus) {
            let heir = parent.filter(|parent| self.can_focus(*parent)).or_else(|| {
                self.stack()
                    .into_iter()
                    .rev()
                    .find(|id| self.can_focus(*id))
            });
            self.set_focus(heir);
        }
        self.settle_pointer();
    }

    fn can_focus(&self, id: WindowId) -> bool {
        self.window(id)
            .is_some_and(|window| self.visible(window) && window.takes_keyboard())
    }

    /// Bring a window and its popups to the front of their levels, and give
    /// it the keyboard when `focus` says to and it takes it.
    pub(crate) fn raise(&mut self, id: WindowId, focus: bool) {
        for member in self.family(id) {
            self.raises += 1;
            let raised = self.raises;
            if let Some(window) = self.window_mut(member) {
                window.raised = raised;
            }
        }
        self.damage_family(id);
        if focus && self.can_focus(id) {
            self.set_focus(Some(id));
        }
        self.settle_pointer();
    }

    fn set_focus(&mut self, focus: Option<WindowId>) {
        if self.focus == focus {
            return;
        }
        let old = std::mem::replace(&mut self.focus, focus);
        if !self.active {
            return;
        }
        for (window, gained) in [(old, false), (focus, true)] {
            if let Some(window) = window {
                self.events.push_back(Event::WindowFocus { window, gained });
            }
        }
    }

    /// The window keys go to, or SDL's zero for none.
    pub(crate) fn keyboard(&self) -> WindowId {
        self.focus.filter(|_| self.active).unwrap_or(0)
    }

    /// The host gave the desktop its keyboard focus, or took it away.
    pub(crate) fn host_focus(&mut self, gained: bool) {
        if self.active == gained {
            return;
        }
        self.active = gained;
        if let Some(window) = self.focus {
            self.events.push_back(Event::WindowFocus { window, gained });
        }
    }

    pub(crate) fn pointer(&self) -> (f32, f32) {
        self.pointer
    }

    pub(crate) fn buttons(&self) -> u32 {
        self.buttons
    }

    /// The window pointer events are going to.
    pub(crate) fn pointer_window(&self) -> Option<WindowId> {
        self.capture.or(self.over)
    }

    /// The pointer as `id` sees it.
    fn local(&self, id: WindowId) -> (f32, f32) {
        let origin = self.frame(id).map_or((0, 0), |frame| (frame.x, frame.y));
        (
            self.pointer.0 - origin.0 as f32,
            self.pointer.1 - origin.1 as f32,
        )
    }

    /// Tell the windows concerned when the one under the pointer changed,
    /// which a window moving does as well as the pointer.
    fn settle_pointer(&mut self) {
        if self.capture.is_some() || self.session.is_some() {
            return;
        }
        let over = match self.inside {
            true => self.target_at(self.pointer.0, self.pointer.1),
            false => None,
        };
        if over == self.over {
            return;
        }
        let old = std::mem::replace(&mut self.over, over);
        for (window, entered) in [(old, false), (over, true)] {
            if let Some(window) = window.filter(|window| self.contains(*window)) {
                self.events
                    .push_back(Event::WindowMouse { window, entered });
            }
        }
    }

    /// The pointer is at a desktop point, having moved by `(dx, dy)`.
    pub(crate) fn pointer_moved(&mut self, x: f32, y: f32, dx: f32, dy: f32) {
        // A screen's pointer stops at its edges.
        self.pointer = (
            x.clamp(0.0, (self.size.0 - 1).max(0) as f32),
            y.clamp(0.0, (self.size.1 - 1).max(0) as f32),
        );
        self.inside = true;
        if let Some(session) = &self.session {
            let (window, grip, frame) = (session.window, session.grip, session.frame);
            let moved_x = (self.pointer.0 - session.pointer.0).round() as i32;
            let moved_y = (self.pointer.1 - session.pointer.1).round() as i32;
            let minimum = self.window(window).map_or((1, 1), |window| window.minimum);
            self.set_frame(window, dragged(frame, grip, moved_x, moved_y, minimum));
            return;
        }
        self.settle_pointer();
        if let Some(window) = self.pointer_window() {
            let (x, y) = self.local(window);
            self.events.push_back(Event::MouseMotion {
                window,
                x,
                y,
                dx,
                dy,
                buttons: self.buttons,
            });
        }
    }

    /// The pointer left the desktop.
    pub(crate) fn pointer_left(&mut self) {
        self.inside = false;
        self.settle_pointer();
    }

    /// Where a left press now would land, for the window's hit test: the
    /// window and the point in its coordinates.
    pub(crate) fn press_point(&self) -> Option<(WindowId, (i32, i32))> {
        if self.capture.is_some() || self.session.is_some() {
            return None;
        }
        let window = self.target_at(self.pointer.0, self.pointer.1)?;
        let (x, y) = self.local(window);
        Some((window, (x.floor() as i32, y.floor() as i32)))
    }

    /// A button went down or came up where the pointer is. `hit` is what the
    /// window's hit test made of the point, for the press that asks.
    pub(crate) fn button(&mut self, button: u8, down: bool, clicks: u8, hit: HitTestResult) {
        let mask = 1 << button.saturating_sub(1).min(31);
        match down {
            true => self.pressed(button, mask, clicks, hit),
            false => self.released(button, mask, clicks),
        }
    }

    fn pressed(&mut self, button: u8, mask: u32, clicks: u8, hit: HitTestResult) {
        self.buttons |= mask;
        let window = match self.capture {
            Some(window) => window,
            None => {
                let Some(window) = self.window_at(self.pointer.0, self.pointer.1) else {
                    return;
                };
                // A click orders a window front and makes it key - or the
                // window that is modal for it, which then keeps the press.
                if let Some(modal) = self.blocker(window) {
                    self.raise(window, false);
                    self.raise(modal, true);
                    return;
                }
                self.raise(window, true);
                if button == LEFT_BUTTON && hit != HitTestResult::Normal {
                    // A window that fills the desktop stays where it is.
                    let held = self
                        .window(window)
                        .filter(|window| window.flags & COVERING == 0)
                        .map(|window| window.frame);
                    self.session = held.map(|frame| Session {
                        window,
                        grip: hit,
                        pointer: self.pointer,
                        frame,
                    });
                    self.events.push_back(Event::WindowHitTest { window });
                    return;
                }
                self.capture = Some(window);
                window
            }
        };
        let (x, y) = self.local(window);
        self.events.push_back(Event::MouseButton {
            window,
            button,
            down: true,
            clicks,
            x,
            y,
        });
    }

    fn released(&mut self, button: u8, mask: u32, clicks: u8) {
        self.buttons &= !mask;
        if button == LEFT_BUTTON && self.session.take().is_some() {
            self.settle_pointer();
            return;
        }
        let Some(window) = self.capture else {
            return;
        };
        let (x, y) = self.local(window);
        self.events.push_back(Event::MouseButton {
            window,
            button,
            down: false,
            clicks,
            x,
            y,
        });
        if self.buttons == 0 {
            self.capture = None;
            self.settle_pointer();
        }
    }

    /// An event the driver made, in its turn among this list's own.
    pub(crate) fn push(&mut self, event: Event) {
        self.events.push_back(event);
    }

    pub(crate) fn take_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// The part of the desktop that no longer shows what it should.
    pub(crate) fn take_damage(&mut self) -> Option<Rect> {
        self.damage.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESKTOP: (i32, i32) = (800, 600);

    fn scene() -> Scene {
        let mut scene = Scene::new(DESKTOP);
        scene.host_focus(true);
        scene
    }

    fn add(scene: &mut Scene, id: WindowId, at: (i32, i32)) {
        scene.add(id, None, flags::HIDDEN, at, (200, 100));
        scene.show(id);
    }

    fn events(scene: &mut Scene) -> Vec<Event> {
        std::iter::from_fn(|| scene.take_event()).collect()
    }

    fn click(scene: &mut Scene, at: (f32, f32), hit: HitTestResult) {
        scene.pointer_moved(at.0, at.1, 0.0, 0.0);
        scene.button(LEFT_BUTTON, true, 1, hit);
    }

    #[test]
    fn the_pointer_region_is_the_shown_windows_inside_the_desktop() {
        let mut scene = scene();
        add(&mut scene, 1, (700, 550));
        add(&mut scene, 2, (10, 20));
        add(&mut scene, 3, (300, 300));
        scene.hide(3);
        scene.add(4, Some(2), flags::TOOLTIP | flags::HIDDEN, (5, 5), (50, 20));
        scene.show(4);
        let rect = |x, y, w, h| Rect { x, y, w, h };
        assert_eq!(
            scene.pointer_region(),
            [rect(10, 20, 200, 100), rect(700, 550, 100, 50)]
        );
    }

    #[test]
    fn a_shown_window_goes_in_front_and_takes_the_keyboard() {
        let mut scene = scene();
        add(&mut scene, 1, (0, 0));
        add(&mut scene, 2, (50, 50));
        assert_eq!(scene.stack(), [1, 2]);
        assert_eq!(scene.keyboard(), 2);
        assert_eq!(
            events(&mut scene),
            [
                Event::WindowFocus {
                    window: 1,
                    gained: true
                },
                Event::WindowFocus {
                    window: 1,
                    gained: false
                },
                Event::WindowFocus {
                    window: 2,
                    gained: true
                },
            ]
        );
    }

    #[test]
    fn a_higher_level_stays_in_front_of_a_raised_window() {
        let mut scene = scene();
        add(&mut scene, 1, (0, 0));
        add(&mut scene, 2, (0, 0));
        scene.set_level(1, 3);
        scene.raise(2, true);
        assert_eq!(scene.stack(), [2, 1]);
        assert_eq!(scene.window_at(10.0, 10.0), Some(1));
    }

    #[test]
    fn a_popup_is_placed_against_its_parent_and_moves_with_it() {
        let mut scene = scene();
        add(&mut scene, 1, (100, 100));
        scene.add(2, Some(1), flags::POPUP_MENU, (10, 20), (50, 50));
        assert_eq!(
            scene.frame(2).map(|frame| (frame.x, frame.y)),
            Some((110, 120))
        );
        scene.set_position(1, 300, 200);
        assert_eq!(
            scene.frame(2).map(|frame| (frame.x, frame.y)),
            Some((310, 220))
        );
        assert_eq!(scene.position(2), Some((10, 20)));
        // Raising the parent leaves its popup above it.
        scene.raise(1, true);
        assert_eq!(scene.stack(), [1, 2]);
    }

    #[test]
    fn a_click_raises_the_window_under_it_and_keeps_the_drag() {
        let mut scene = scene();
        add(&mut scene, 1, (0, 0));
        add(&mut scene, 2, (300, 0));
        events(&mut scene);
        click(&mut scene, (20.0, 30.0), HitTestResult::Normal);
        assert_eq!(scene.stack(), [2, 1]);
        assert_eq!(scene.keyboard(), 1);
        // Over the other window, with the button still held.
        scene.pointer_moved(320.0, 30.0, 300.0, 0.0);
        scene.button(LEFT_BUTTON, false, 1, HitTestResult::Normal);
        let pointer: Vec<Event> = events(&mut scene)
            .into_iter()
            .filter(|event| matches!(event, Event::MouseMotion { .. } | Event::MouseButton { .. }))
            .collect();
        assert_eq!(
            pointer,
            [
                Event::MouseMotion {
                    window: 1,
                    x: 20.0,
                    y: 30.0,
                    dx: 0.0,
                    dy: 0.0,
                    buttons: 0
                },
                Event::MouseButton {
                    window: 1,
                    button: LEFT_BUTTON,
                    down: true,
                    clicks: 1,
                    x: 20.0,
                    y: 30.0
                },
                Event::MouseMotion {
                    window: 1,
                    x: 320.0,
                    y: 30.0,
                    dx: 300.0,
                    dy: 0.0,
                    buttons: 1
                },
                Event::MouseButton {
                    window: 1,
                    button: LEFT_BUTTON,
                    down: false,
                    clicks: 1,
                    x: 320.0,
                    y: 30.0
                },
            ]
        );
        assert_eq!(scene.pointer_window(), Some(2));
    }

    #[test]
    fn a_press_on_a_draggable_point_moves_the_window_instead_of_reaching_it() {
        let mut scene = scene();
        add(&mut scene, 1, (100, 100));
        events(&mut scene);
        click(&mut scene, (150.0, 110.0), HitTestResult::Draggable);
        scene.pointer_moved(180.0, 150.0, 30.0, 40.0);
        scene.button(LEFT_BUTTON, false, 1, HitTestResult::Normal);
        assert_eq!(
            scene.frame(1),
            Some(Rect {
                x: 130,
                y: 140,
                w: 200,
                h: 100
            })
        );
        let seen = events(&mut scene);
        assert!(seen.contains(&Event::WindowHitTest { window: 1 }));
        assert!(seen.contains(&Event::WindowMoved { window: 1 }));
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, Event::MouseButton { .. }))
        );
    }

    #[test]
    fn a_resize_from_the_top_left_keeps_the_opposite_corner_and_the_minimum() {
        let frame = Rect {
            x: 100,
            y: 100,
            w: 200,
            h: 100,
        };
        assert_eq!(
            dragged(frame, HitTestResult::ResizeTopLeft, 50, 20, (1, 1)),
            Rect {
                x: 150,
                y: 120,
                w: 150,
                h: 80
            }
        );
        assert_eq!(
            dragged(frame, HitTestResult::ResizeTopLeft, 500, 500, (120, 60)),
            Rect {
                x: 180,
                y: 140,
                w: 120,
                h: 60
            }
        );
    }

    #[test]
    fn a_tooltip_takes_neither_the_pointer_nor_the_keyboard() {
        let mut scene = scene();
        add(&mut scene, 1, (0, 0));
        scene.add(2, Some(1), flags::TOOLTIP, (10, 10), (50, 20));
        assert_eq!(scene.stack(), [1, 2]);
        assert_eq!(scene.window_at(20.0, 20.0), Some(1));
        assert_eq!(scene.keyboard(), 1);
    }

    #[test]
    fn a_modal_window_takes_its_parents_clicks_without_moving_with_it() {
        let mut scene = scene();
        add(&mut scene, 1, (0, 0));
        add(&mut scene, 2, (500, 400));
        scene.set_modal_for(2, 1);
        events(&mut scene);
        click(&mut scene, (20.0, 30.0), HitTestResult::Normal);
        assert_eq!(scene.stack(), [1, 2]);
        assert_eq!(scene.keyboard(), 2);
        assert!(
            !events(&mut scene)
                .iter()
                .any(|event| matches!(event, Event::MouseButton { .. }))
        );
        scene.set_position(1, 100, 100);
        assert_eq!(scene.position(2), Some((500, 400)));
    }

    #[test]
    fn hiding_the_focused_window_hands_the_keyboard_to_the_next_one() {
        let mut scene = scene();
        add(&mut scene, 1, (0, 0));
        add(&mut scene, 2, (0, 0));
        scene.hide(2);
        assert_eq!(scene.keyboard(), 1);
        assert_eq!(
            scene.flags(2).map(|flags| flags & flags::HIDDEN),
            Some(flags::HIDDEN)
        );
        assert_eq!(
            scene.flags(1).map(|flags| flags & flags::INPUT_FOCUS),
            Some(flags::INPUT_FOCUS)
        );
    }

    #[test]
    fn full_screen_fills_the_desktop_and_gives_the_frame_back() {
        let mut scene = scene();
        add(&mut scene, 1, (40, 30));
        events(&mut scene);
        scene.set_covering(1, flags::FULLSCREEN, true);
        assert_eq!(scene.frame(1), Some(scene.bounds()));
        assert_eq!(
            events(&mut scene),
            [
                Event::WindowState { window: 1 },
                Event::WindowMoved { window: 1 },
                Event::WindowResized {
                    window: 1,
                    width: DESKTOP.0,
                    height: DESKTOP.1
                },
            ]
        );
        // Asked for while covering, a frame is where the window returns to.
        scene.set_size(1, 300, 200);
        assert_eq!(scene.frame(1), Some(scene.bounds()));
        scene.set_covering(1, flags::FULLSCREEN, false);
        assert_eq!(
            scene.frame(1),
            Some(Rect {
                x: 40,
                y: 30,
                w: 300,
                h: 200
            })
        );
    }

    #[test]
    fn keys_go_nowhere_while_the_host_has_the_focus_elsewhere() {
        let mut scene = scene();
        add(&mut scene, 1, (0, 0));
        events(&mut scene);
        scene.host_focus(false);
        assert_eq!(scene.keyboard(), 0);
        assert_eq!(
            events(&mut scene),
            [Event::WindowFocus {
                window: 1,
                gained: false
            }]
        );
    }

    #[test]
    fn a_window_moved_under_a_still_pointer_is_entered() {
        let mut scene = scene();
        add(&mut scene, 1, (400, 400));
        scene.pointer_moved(10.0, 10.0, 0.0, 0.0);
        events(&mut scene);
        scene.set_position(1, 0, 0);
        assert!(events(&mut scene).contains(&Event::WindowMouse {
            window: 1,
            entered: true
        }));
    }

    #[test]
    fn damage_covers_where_a_window_was_and_where_it_went() {
        let mut scene = scene();
        add(&mut scene, 1, (0, 0));
        scene.take_damage();
        scene.set_position(1, 300, 200);
        assert_eq!(
            scene.take_damage(),
            Some(Rect {
                x: 0,
                y: 0,
                w: 500,
                h: 300
            })
        );
        assert_eq!(scene.take_damage(), None);
    }
}
