//! Dragging sessions inside this application.
//!
//! `-dragImage:at:offset:event:pasteboard:source:slideBack:` runs the session
//! before it returns; `-beginDraggingSessionWithItems:event:source:` returns
//! the session and it runs from the next turn of the event loop, as AppKit's
//! does. Either way the session follows the pointer through the window
//! system's input, drives `NSDraggingDestination` on the registered view or
//! window under it, and ends with the source's end callback.
//!
//! Only this application's windows can be destinations: no window server here
//! hands a drag to another program, so a drop anywhere else ends the session
//! with `NSDragOperationNone`, as a drop on nothing does on a Mac.
//!
//! FIXME (ACCURACY): the dragged image is not drawn and does not slide back;
//! the pointer, with whatever cursor the program sets, is all that moves.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::Mutex;

use foundation::geometry::{NSPoint, NSRect};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::nsevent::kind;

/// `NSDragOperation`.
pub mod operation {
    pub const NONE: u64 = 0;
    pub const COPY: u64 = 1;
    pub const LINK: u64 = 2;
    pub const GENERIC: u64 = 4;
    pub const PRIVATE: u64 = 8;
    pub const MOVE: u64 = 16;
    pub const DELETE: u64 = 32;
    pub const EVERY: u64 = u64::MAX;
}

/// `NSDraggingContextWithinApplication`: every destination here is.
const WITHIN_APPLICATION: isize = 1;

/// `NSDraggingFormationDefault`.
const DEFAULT_FORMATION: isize = 0;

const TRACKING_MODE: &str = "NSEventTrackingRunLoopMode";

/// How often a destination that wants periodic updates is sent one while the
/// pointer rests. AppKit does not document its own interval.
const PERIODIC_UPDATE: std::time::Duration = std::time::Duration::from_millis(50);

/// What a session's loop takes from the queue; everything else waits there
/// for the program, after the drag.
const TRACKED: u64 = (1 << kind::LEFT_MOUSE_UP)
    | (1 << kind::RIGHT_MOUSE_UP)
    | (1 << kind::OTHER_MOUSE_UP)
    | (1 << kind::MOUSE_MOVED)
    | (1 << kind::LEFT_MOUSE_DRAGGED)
    | (1 << kind::RIGHT_MOUSE_DRAGGED)
    | (1 << kind::OTHER_MOUSE_DRAGGED)
    | (1 << kind::KEY_DOWN)
    | (1 << kind::FLAGS_CHANGED);

// Registration.

/// The types each view or window registered, by address. A view or window
/// leaves the table in its `-dealloc`.
static REGISTERED: Mutex<Vec<(usize, Vec<Vec<u8>>)>> = Mutex::new(Vec::new());

fn registered_types(object: Id) -> Vec<Vec<u8>> {
    REGISTERED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(address, _)| *address == object as usize)
        .map(|(_, types)| types.clone())
        .unwrap_or_default()
}

/// Whether a drag of a pasteboard holding `types` is one `object` takes.
fn accepts(object: Id, types: &[Vec<u8>]) -> bool {
    registered_types(object).iter().any(|ty| types.contains(ty))
}

pub(crate) fn forget(object: Id) {
    REGISTERED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(address, _)| *address != object as usize);
}

fn type_list(array: Id) -> Vec<Vec<u8>> {
    // SAFETY: an array of strings, or an object that is not an array.
    unsafe { corefoundation::collections::array(array as *const c_void) }
        .map(|array| {
            array
                .values
                .iter()
                // SAFETY: the array's members are strings.
                .map(|value| unsafe { foundation::string::bytes(*value as Id) })
                .collect()
        })
        .unwrap_or_default()
}

/// `-registerForDraggedTypes:`. Registering again adds to the types already
/// registered; `-registeredDraggedTypes` never repeats one.
unsafe extern "C" fn register_for_dragged_types(receiver: Id, _sel: Sel, types: Id) {
    let added = type_list(types);
    let mut table = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
    let at = match table.iter().position(|(a, _)| *a == receiver as usize) {
        Some(at) => at,
        None => {
            table.push((receiver as usize, Vec::new()));
            table.len() - 1
        }
    };
    let registered = &mut table[at].1;
    for ty in added {
        if !registered.contains(&ty) {
            registered.push(ty);
        }
    }
}

unsafe extern "C" fn unregister_dragged_types(receiver: Id, _sel: Sel) {
    forget(receiver);
}

unsafe extern "C" fn registered_dragged_types(receiver: Id, _sel: Sel) -> Id {
    crate::pasteboard::strings(registered_types(receiver))
}

// Messages.

/// The object that answers `selector` for `target`: the target, or for a
/// window that does not implement one of the destination messages, its
/// delegate, which is where NSWindow's own implementation forwards it.
fn implementer(target: Id, selector: &str) -> Option<(Id, *const c_void)> {
    let sel = objc::sel::get(selector);
    // SAFETY: null or a live object.
    if let Some(method) = unsafe { objc::object::class_of(target) }.and_then(|c| c.method(sel)) {
        return Some((target, method.implementation()));
    }
    const FORWARDED: [&str; 6] = [
        "draggingEntered:",
        "draggingUpdated:",
        "draggingExited:",
        "prepareForDragOperation:",
        "performDragOperation:",
        "concludeDragOperation:",
    ];
    if !FORWARDED.contains(&selector) || !is_window(target) {
        return None;
    }
    let delegate = crate::window::delegate_of(target);
    // SAFETY: null or the window's live delegate.
    let method = unsafe { objc::object::class_of(delegate) }.and_then(|c| c.method(sel))?;
    Some((delegate, method.implementation()))
}

/// Send a destination message that answers an `NSDragOperation`, or `None`
/// when nothing implements it.
fn send_operation(target: Id, selector: &str, info: Id) -> Option<u64> {
    let (receiver, imp) = implementer(target, selector)?;
    // SAFETY: every operation-returning destination message is
    // `NSDragOperation (id, SEL, id<NSDraggingInfo>)`.
    Some(unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> u64 = std::mem::transmute(imp);
        f(receiver, objc::sel::get(selector), info)
    })
}

/// The same for a `BOOL` message.
fn send_flag(target: Id, selector: &str, info: Id) -> Option<bool> {
    let (receiver, imp) = implementer(target, selector)?;
    // SAFETY: `BOOL (id, SEL, id<NSDraggingInfo>)`.
    Some(unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 = std::mem::transmute(imp);
        f(receiver, objc::sel::get(selector), info) != 0
    })
}

/// The same for a `void` message.
fn send_void(target: Id, selector: &str, info: Id) {
    if let Some((receiver, imp)) = implementer(target, selector) {
        // SAFETY: `void (id, SEL, id<NSDraggingInfo>)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(imp);
            f(receiver, objc::sel::get(selector), info);
        }
    }
}

fn is_window(object: Id) -> bool {
    // SAFETY: null or a live object.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSWindow").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

fn method_of(target: Id, selector: &str) -> Option<*const c_void> {
    // SAFETY: null or a live object.
    unsafe { objc::object::class_of(target) }
        .and_then(|c| c.method(objc::sel::get(selector)))
        .map(|m| m.implementation())
}

// The session.

/// What the session and its two faces - `NSDraggingSession` for the source,
/// `NSDraggingInfo` for a destination - answer about the drag.
struct Session {
    sequence: isize,
    /// The `NSDraggingSession` and the `NSDraggingInfo`, retained.
    session: Id,
    info: Id,
    /// Retained, as are the source and the image.
    pasteboard: Id,
    source: Id,
    image: Id,
    /// The image's bottom-left, relative to the pointer.
    image_offset: NSPoint,
    /// The pointer, in screen coordinates.
    location: NSPoint,
    /// The window of the destination the pointer is over, unretained: the
    /// tracker retains the destination, and the destination is in it.
    destination_window: Id,
    /// What the source allows, before the modifier keys have their say.
    mask: u64,
    ignores_modifiers: bool,
    modifiers: u64,
    formation: isize,
    animates: bool,
    valid_items: isize,
    leader_index: isize,
    /// The NSEvent number of the button the drag follows.
    button: i64,
    /// Whether the source answers the `NSDraggingSource` protocol rather than
    /// the informal one `-dragImage:...` used before it.
    session_source: bool,
}

thread_local! {
    static ACTIVE: RefCell<Option<Session>> = const { RefCell::new(None) };
    /// A session `-beginDraggingSessionWithItems:` made, waiting for the loop.
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
    /// The drag's button was let go where no event reports it - over the
    /// decorations this runtime draws.
    static RELEASED_UNREPORTED: Cell<bool> = const { Cell::new(false) };
}

static SEQUENCE: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

struct Pending {
    event: Id,
    /// Why the session cannot run, which ends it as soon as it starts.
    refused: Option<String>,
}

fn with_session<R: Default>(face: Id, read: impl FnOnce(&mut Session) -> R) -> R {
    // SAFETY: both faces have this layout.
    let sequence = unsafe { (*(face as *const Face)).sequence };
    ACTIVE.with_borrow_mut(|active| match active {
        Some(session) if session.sequence == sequence => read(session),
        _ => R::default(),
    })
}

/// The layout of both faces: the session they answer for. A face kept past
/// its session answers zero for everything.
#[repr(C)]
struct Face {
    isa: *const c_void,
    sequence: isize,
}

pub const SESSION_INSTANCE_SIZE: usize = size_of::<Face>();

fn face(class: &std::ffi::CStr, sequence: isize) -> Id {
    let class = objc::class::lookup(class).expect("declared before its methods");
    let object = objc::object::create_instance(class, 0);
    // SAFETY: a fresh instance of a class with `Face`'s layout.
    unsafe { (*(object as *mut Face)).sequence = sequence };
    object
}

/// The operations the source allows with the modifier keys held now.
///
/// Control asks for a link, Option for a copy and Command for a generic
/// operation, and the source's mask is narrowed to that one - unless the
/// source asked for the keys to be ignored.
fn effective_mask(mask: u64, modifiers: u64, ignores_modifiers: bool) -> u64 {
    use crate::keymap::flags;
    if ignores_modifiers {
        return mask;
    }
    let wanted = [
        (flags::CONTROL, operation::LINK),
        (flags::OPTION, operation::COPY),
        (flags::COMMAND, operation::GENERIC),
    ]
    .into_iter()
    .find(|(flag, _)| modifiers & flag != 0);
    match wanted {
        Some((_, op)) => mask & op,
        None => mask,
    }
}

fn current_mask() -> u64 {
    ACTIVE.with_borrow(|active| {
        active.as_ref().map_or(operation::NONE, |s| {
            effective_mask(s.mask, s.modifiers, s.ignores_modifiers)
        })
    })
}

fn change_session(change: impl FnOnce(&mut Session)) {
    ACTIVE.with_borrow_mut(|active| {
        if let Some(session) = active {
            change(session);
        }
    });
}

/// Where a drag is over something that takes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Target {
    destination: Id,
    window: Id,
}

/// The nearest of `view` and its superviews registered for one of `types`.
fn registered_ancestor(view: Id, types: &[Vec<u8>]) -> Option<Id> {
    let mut view = view;
    // Bounded for the reason `view::window_of` is.
    for _ in 0..64 {
        if !crate::view::is_view(view) {
            return None;
        }
        if accepts(view, types) {
            return Some(view);
        }
        // SAFETY: a view of this crate's.
        view = unsafe { crate::view::superview_of(view) };
    }
    None
}

/// The destination under a screen point, the pointer's events reported against
/// `reporting`: the registered view nearest the one the point hits, or else
/// the window, if it registered.
fn target_at(reporting: Id, point: NSPoint, types: &[Vec<u8>]) -> Option<Target> {
    let window = crate::window::window_at_screen_point(reporting, point);
    if window.is_null() {
        return None;
    }
    let base = crate::window::from_screen(window, point);
    // SAFETY: null or the content view of a window of this crate's.
    let hit = unsafe { crate::view::hit_test(crate::window::content_view_of(window), base) };
    let destination =
        registered_ancestor(hit, types).or_else(|| accepts(window, types).then_some(window))?;
    Some(Target {
        destination,
        window,
    })
}

/// The destination half of the protocol: which destination the drag is over
/// and what it last said it would do.
#[derive(Default)]
struct Tracker {
    /// Retained while the drag is over it.
    target: Option<Target>,
    /// The last operation entered or updated answered, within the source's mask.
    operation: u64,
}

impl Tracker {
    /// The pointer is over `target`, which may be the destination it was over
    /// already: that one is sent `-draggingUpdated:` if `update` says so.
    fn track(&mut self, info: Id, target: Option<Target>, update: bool) {
        if target == self.target {
            if update
                && let Some(current) = self.target
                && let Some(answer) = send_operation(current.destination, "draggingUpdated:", info)
            {
                self.operation = answer & current_mask();
            }
            return;
        }
        self.leave(info);
        let Some(target) = target else {
            return;
        };
        objc::arc::retain(target.destination);
        self.target = Some(target);
        change_session(|s| s.destination_window = target.window);
        self.operation = send_operation(target.destination, "draggingEntered:", info)
            .map_or(operation::NONE, |answer| answer & current_mask());
    }

    /// The destination the drag was over is told it left, and let go of.
    fn leave(&mut self, info: Id) {
        if let Some(previous) = self.target {
            send_void(previous.destination, "draggingExited:", info);
        }
        self.release();
    }

    fn release(&mut self) {
        if let Some(previous) = self.target.take() {
            objc::arc::release(previous.destination);
        }
        self.operation = operation::NONE;
        change_session(|s| s.destination_window = std::ptr::null_mut());
    }

    /// The button was let go: the destination performs the drop or refuses
    /// it. The operation the drop ended with.
    fn drop(&mut self, info: Id) -> u64 {
        let Some(target) = self.target else {
            return operation::NONE;
        };
        let operation = self.operation;
        // A drop nothing was offered for is the drag leaving; an
        // unimplemented `-prepareForDragOperation:` agrees, and an
        // unimplemented `-performDragOperation:` refuses.
        let performed = match operation == operation::NONE {
            true => {
                send_void(target.destination, "draggingExited:", info);
                false
            }
            false => {
                send_flag(target.destination, "prepareForDragOperation:", info) != Some(false)
                    && send_flag(target.destination, "performDragOperation:", info) == Some(true)
            }
        };
        if performed {
            send_void(target.destination, "concludeDragOperation:", info);
        }
        send_void(target.destination, "draggingEnded:", info);
        self.release();
        match performed {
            true => operation,
            false => operation::NONE,
        }
    }

    /// The drag was cancelled.
    fn cancel(&mut self, info: Id) {
        self.leave(info);
    }

    /// Whether the destination wants `-draggingUpdated:` while the pointer
    /// rests, which it does unless it says otherwise.
    fn wants_periodic_updates(&self) -> bool {
        let Some(target) = self.target else {
            return false;
        };
        let Some(imp) = method_of(target.destination, "wantsPeriodicDraggingUpdates") else {
            return true;
        };
        // SAFETY: `BOOL (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(imp);
            f(
                target.destination,
                objc::sel::get("wantsPeriodicDraggingUpdates"),
            ) != 0
        }
    }
}

// Starting and ending.

/// Make the active session. The source is asked what it allows here, as
/// AppKit asks when the drag starts.
fn begin(pasteboard: Id, source: Id, image: Id, image_offset: NSPoint, location: NSPoint) -> Id {
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let session = face(c"NSDraggingSession", sequence);
    let info = face(c"NSDraggingDestinationInfo", sequence);
    let session_source = method_of(
        source,
        "draggingSession:sourceOperationMaskForDraggingContext:",
    )
    .is_some();
    ACTIVE.with_borrow_mut(|active| {
        *active = Some(Session {
            sequence,
            session,
            info,
            pasteboard: objc::arc::retain(pasteboard),
            source: objc::arc::retain(source),
            image: objc::arc::retain(image),
            image_offset,
            location,
            destination_window: std::ptr::null_mut(),
            mask: operation::NONE,
            ignores_modifiers: false,
            modifiers: 0,
            formation: DEFAULT_FORMATION,
            animates: false,
            valid_items: 1,
            leader_index: 0,
            button: 0,
            session_source,
        })
    });
    let (mask, ignores) = match session_source {
        true => (
            source_mask(source, session),
            source_flag(
                source,
                "ignoreModifierKeysForDraggingSession:",
                Some(session),
            ),
        ),
        false => (
            legacy_source_mask(source),
            source_flag(source, "ignoreModifierKeysWhileDragging", None),
        ),
    };
    change_session(|s| {
        s.mask = mask;
        s.ignores_modifiers = ignores;
    });
    session
}

fn source_mask(source: Id, session: Id) -> u64 {
    let selector = "draggingSession:sourceOperationMaskForDraggingContext:";
    let Some(imp) = method_of(source, selector) else {
        return operation::NONE;
    };
    // SAFETY: `NSDragOperation (id, SEL, NSDraggingSession *, NSDraggingContext)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, isize) -> u64 = std::mem::transmute(imp);
        f(
            source,
            objc::sel::get(selector),
            session,
            WITHIN_APPLICATION,
        )
    }
}

/// `-draggingSourceOperationMaskForLocal:`, the informal protocol's question.
fn legacy_source_mask(source: Id) -> u64 {
    let selector = "draggingSourceOperationMaskForLocal:";
    let Some(imp) = method_of(source, selector) else {
        // FIXME (ACCURACY): AppKit does not document what it assumes of a
        // source that does not answer; every operation lets the destination
        // choose.
        return operation::EVERY;
    };
    // SAFETY: `NSDragOperation (id, SEL, BOOL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, u8) -> u64 = std::mem::transmute(imp);
        f(source, objc::sel::get(selector), 1)
    }
}

/// A source's `BOOL` answer to a question about `session`, or with no
/// argument for the informal protocol's.
fn source_flag(source: Id, selector: &str, session: Option<Id>) -> bool {
    let Some(imp) = method_of(source, selector) else {
        return false;
    };
    let sel = objc::sel::get(selector);
    // SAFETY: `BOOL (id, SEL, NSDraggingSession *)`, or `BOOL (id, SEL)`.
    unsafe {
        match session {
            Some(session) => {
                let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 = std::mem::transmute(imp);
                f(source, sel, session) != 0
            }
            None => {
                let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(imp);
                f(source, sel) != 0
            }
        }
    }
}

/// Tell the source where the drag is, through the selector its protocol
/// spells the stage with.
fn tell_source(session_selector: &str, image_selector: &str) {
    let Some((source, session, image, location, image_location, session_source)) = ACTIVE
        .with_borrow(|active| {
            active.as_ref().map(|s| {
                (
                    s.source,
                    s.session,
                    s.image,
                    s.location,
                    offset(s.location, s.image_offset),
                    s.session_source,
                )
            })
        })
    else {
        return;
    };
    let (selector, first, point) = match session_source {
        true => (session_selector, session, location),
        false => (image_selector, image, image_location),
    };
    if let Some(imp) = method_of(source, selector) {
        // SAFETY: `void (id, SEL, id, NSPoint)` in both protocols.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, NSPoint) = std::mem::transmute(imp);
            f(source, objc::sel::get(selector), first, point);
        }
    }
}

/// End the active session with `result`: the source hears how it ended, and
/// the session's objects are let go.
fn finish(result: u64) {
    let Some(session) = ACTIVE.with_borrow(|active| {
        active.as_ref().map(|s| {
            (
                s.source,
                s.session,
                s.image,
                s.location,
                offset(s.location, s.image_offset),
                s.session_source,
            )
        })
    }) else {
        return;
    };
    let (source, object, image, location, image_location, session_source) = session;
    let (selector, first, point) = match session_source {
        true => ("draggingSession:endedAtPoint:operation:", object, location),
        false => ("draggedImage:endedAt:operation:", image, image_location),
    };
    if let Some(imp) = method_of(source, selector) {
        // SAFETY: `void (id, SEL, id, NSPoint, NSDragOperation)` in both.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, NSPoint, u64) = std::mem::transmute(imp);
            f(source, objc::sel::get(selector), first, point, result);
        }
    }
    if let Some(session) = ACTIVE.with_borrow_mut(Option::take) {
        objc::arc::release(session.pasteboard);
        objc::arc::release(session.source);
        objc::arc::release(session.image);
        objc::arc::release(session.info);
        objc::arc::release(session.session);
    }
}

fn offset(point: NSPoint, by: NSPoint) -> NSPoint {
    NSPoint {
        x: point.x + by.x,
        y: point.y + by.y,
    }
}

fn window_of_event(event: Id) -> Id {
    crate::nsevent::fields_of(event).map_or(std::ptr::null_mut(), |f| f.window)
}

/// An event's location in screen coordinates.
fn screen_location(event: Id) -> NSPoint {
    let at = crate::nsevent::location_of(event);
    let window = window_of_event(event);
    match window.is_null() {
        true => at,
        false => crate::window::to_screen(window, at),
    }
}

/// The NSEvent button number a drag started by `event` ends with.
fn drag_button(event: Id) -> i64 {
    let fields = crate::nsevent::fields_of(event).unwrap_or_default();
    match fields.kind {
        kind::LEFT_MOUSE_DOWN | kind::LEFT_MOUSE_DRAGGED => 0,
        kind::RIGHT_MOUSE_DOWN | kind::RIGHT_MOUSE_DRAGGED => 1,
        _ => fields.button,
    }
}

fn is_mouse_up(kind: u64) -> bool {
    matches!(
        kind,
        kind::LEFT_MOUSE_UP | kind::RIGHT_MOUSE_UP | kind::OTHER_MOUSE_UP
    )
}

/// Track the active session until it is dropped or cancelled, and end it.
fn run(start: Id) {
    struct Pool(*mut c_void);
    impl Drop for Pool {
        fn drop(&mut self) {
            objc::arc::pool_pop(self.0);
        }
    }
    let app = crate::application::shared();
    let Some(info) = ACTIVE.with_borrow(|a| a.as_ref().map(|s| s.info)) else {
        return;
    };
    if app.is_null() {
        finish(operation::NONE);
        return;
    }
    let button = drag_button(start);
    change_session(|s| s.button = button);
    // The pointer's last position, and the window that reported it.
    let mut reporting = window_of_event(start);
    let target = |reporting: Id| {
        ACTIVE.with_borrow(|a| {
            a.as_ref().and_then(|s| {
                target_at(
                    reporting,
                    s.location,
                    &crate::pasteboard::types_of(s.pasteboard),
                )
            })
        })
    };
    RELEASED_UNREPORTED.set(false);
    tell_source("draggingSession:willBeginAtPoint:", "draggedImage:beganAt:");
    let mut tracker = Tracker::default();
    crate::application::with_current_event(start, || tracker.track(info, target(reporting), true));
    // Keys pressed during the drag reach the program after it, in order, so
    // none is left half-pressed.
    let mut keys = Vec::new();
    let mut last_update = std::time::Instant::now();
    let result = loop {
        let _pool = Pool(objc::arc::pool_push());
        if RELEASED_UNREPORTED.take() {
            tracker.cancel(info);
            break operation::NONE;
        }
        // SAFETY: the main thread, with the application.
        let event = unsafe {
            crate::application::next_event_in_mode(app, TRACKED, 0.002, TRACKING_MODE, true)
        };
        // SAFETY: as above.
        let event = unsafe { newest_motion(app, event) };
        let Some(fields) = crate::nsevent::fields_of(event) else {
            if last_update.elapsed() >= PERIODIC_UPDATE && tracker.wants_periodic_updates() {
                tracker.track(info, target(reporting), true);
                last_update = std::time::Instant::now();
            }
            continue;
        };
        change_session(|s| s.modifiers = fields.modifiers);
        let outcome = crate::application::with_current_event(event, || match fields.kind {
            kind::KEY_DOWN if fields.key_code == crate::keymap::vk::ESCAPE => {
                tracker.cancel(info);
                Some(operation::NONE)
            }
            kind::KEY_DOWN => {
                keys.push(objc::arc::retain(event));
                None
            }
            kind::FLAGS_CHANGED => {
                keys.push(objc::arc::retain(event));
                tracker.track(info, target(reporting), true);
                None
            }
            up if is_mouse_up(up) && fields.button == button => {
                reporting = fields.window;
                change_session(|s| s.location = screen_location(event));
                tracker.track(info, target(reporting), false);
                Some(tracker.drop(info))
            }
            up if is_mouse_up(up) => None,
            _ => {
                reporting = fields.window;
                change_session(|s| s.location = screen_location(event));
                tracker.track(info, target(reporting), true);
                tell_source("draggingSession:movedToPoint:", "draggedImage:movedTo:");
                None
            }
        });
        last_update = std::time::Instant::now();
        if let Some(result) = outcome {
            break result;
        }
    };
    crate::application::with_current_event(start, || finish(result));
    crate::application::requeue_front(keys);
}

/// The newest of the motions queued right behind `event`, which a Mac hands
/// over in place of the ones it skipped: a destination can take longer to
/// answer an update than the pointer takes to move (a 3D editor that hit-tests
/// its scene on each), and following every queued motion leaves the drag
/// further behind with each one. Anything other than a motion ends the run, so
/// buttons and keys keep their order.
///
/// # Safety
///
/// On the main thread with `app` the application, inside an autorelease pool.
unsafe fn newest_motion(app: Id, mut event: Id) -> Id {
    let is_motion = |event: Id| {
        crate::nsevent::fields_of(event).is_some_and(|f| {
            matches!(
                f.kind,
                kind::MOUSE_MOVED
                    | kind::LEFT_MOUSE_DRAGGED
                    | kind::RIGHT_MOUSE_DRAGGED
                    | kind::OTHER_MOUSE_DRAGGED
            )
        })
    };
    if !is_motion(event) {
        return event;
    }
    loop {
        // SAFETY: the caller's thread and application; a zero wait only looks.
        let next = unsafe {
            crate::application::next_event_in_mode(app, TRACKED, 0.0, TRACKING_MODE, false)
        };
        if next.is_null() || !is_motion(next) {
            return event;
        }
        // SAFETY: as above; this takes the event just looked at.
        event = unsafe {
            crate::application::next_event_in_mode(app, TRACKED, 0.0, TRACKING_MODE, true)
        };
    }
}

/// The button a drag is following was let go over the decorations, where no
/// event reports it: the session ends as a drop on nothing.
pub(crate) fn release_unreported(button: i64) {
    if ACTIVE.with_borrow(|a| a.as_ref().is_some_and(|s| s.button == button)) {
        RELEASED_UNREPORTED.set(true);
    }
}

/// Run the session `-beginDraggingSessionWithItems:` left for the event loop.
pub(crate) fn run_pending() {
    let Some(pending) = PENDING.with_borrow_mut(Option::take) else {
        return;
    };
    match pending.refused {
        Some(reason) => {
            eprintln!("rosemary: appkit: the dragging session is refused: {reason}");
            finish(operation::NONE);
        }
        None => run(pending.event),
    }
    objc::arc::release(pending.event);
}

fn busy() -> bool {
    ACTIVE.with_borrow(Option::is_some) || PENDING.with_borrow(Option::is_some)
}

/// `-[NSWindow dragImage:at:offset:event:pasteboard:source:slideBack:]`: `at`
/// is where the image's bottom-left goes, in the window's base coordinates;
/// the offset has been ignored since 10.0.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn window_drag_image(
    receiver: Id,
    _sel: Sel,
    image: Id,
    at: NSPoint,
    _offset: foundation::geometry::NSSize,
    event: Id,
    pasteboard: Id,
    source: Id,
    _slide_back: u8,
) {
    drag_image(receiver, image, at, event, pasteboard, source);
}

/// `-[NSView dragImage:at:offset:event:pasteboard:source:slideBack:]`, with
/// `at` in the view's coordinates.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn view_drag_image(
    receiver: Id,
    _sel: Sel,
    image: Id,
    at: NSPoint,
    _offset: foundation::geometry::NSSize,
    event: Id,
    pasteboard: Id,
    source: Id,
    _slide_back: u8,
) {
    // SAFETY: a view of this crate's.
    let window = unsafe { crate::view::window_of(receiver) };
    let at = crate::view::point_to_window(receiver, at);
    drag_image(window, image, at, event, pasteboard, source);
}

fn drag_image(window: Id, image: Id, at: NSPoint, event: Id, pasteboard: Id, source: Id) {
    if busy() {
        eprintln!("rosemary: appkit: -dragImage: refused: a dragging session is already running");
        return;
    }
    let pointer = crate::nsevent::location_of(event);
    let image_offset = NSPoint {
        x: at.x - pointer.x,
        y: at.y - pointer.y,
    };
    let location = match window.is_null() {
        true => screen_location(event),
        false => crate::window::to_screen(window, pointer),
    };
    let pasteboard = match pasteboard.is_null() {
        true => crate::pasteboard::drag_board(),
        false => pasteboard,
    };
    begin(pasteboard, source, image, image_offset, location);
    run(event);
}

/// `-[NSView beginDraggingSessionWithItems:event:source:]`.
///
/// The items' pasteboard writers go onto the drag pasteboard now, through
/// `-writeObjects:`; a writer it cannot write leaves the pasteboard as it was
/// and refuses the session, which still ends through the source's callback so
/// a caller waiting for that callback stops waiting.
unsafe extern "C" fn begin_dragging_session(
    receiver: Id,
    _sel: Sel,
    items: Id,
    event: Id,
    source: Id,
) -> Id {
    if busy() {
        eprintln!(
            "rosemary: appkit: -beginDraggingSessionWithItems: refused: a dragging session is \
             already running"
        );
        return std::ptr::null_mut();
    }
    // SAFETY: an array of dragging items, or an object that is not an array.
    let items: Vec<Id> = unsafe { corefoundation::collections::array(items as *const c_void) }
        .map(|array| array.values.iter().map(|v| *v as Id).collect())
        .unwrap_or_default();
    let writers: Vec<*const c_void> = items
        .iter()
        .map(|item| item_of(*item).map_or(std::ptr::null(), |i| i.writer as *const c_void))
        .collect();
    let board = crate::pasteboard::drag_board();
    let refused = match writers.is_empty() || writers.iter().any(|w| w.is_null()) {
        true => Some("it has no items, or an item has no pasteboard writer".to_string()),
        false => {
            let array = corefoundation::collections::new_array(
                &writers,
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            ) as Id;
            // SAFETY: `-writeObjects:` is `BOOL (id, SEL, NSArray *)`.
            let written = unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                    std::mem::transmute(objc::msg::entry());
                f(board, objc::sel::get("writeObjects:"), array)
            };
            objc::arc::release(array);
            (written == 0).then(|| {
                let classes: Vec<&str> = writers
                    .iter()
                    // SAFETY: live writers the items retain.
                    .filter_map(|w| unsafe { objc::object::class_of(*w as Id) })
                    .map(|c| c.name_str())
                    .collect();
                format!(
                    "the drag pasteboard cannot hold its writers ({})",
                    classes.join(", ")
                )
            })
        }
    };
    // The first item's frame and image are the drag's, relative to the event.
    // SAFETY: a view of this crate's.
    let window = unsafe { crate::view::window_of(receiver) };
    let pointer = crate::nsevent::location_of(event);
    let (image, image_offset) = items.first().and_then(|item| item_of(*item)).map_or(
        (std::ptr::null_mut(), NSPoint::default()),
        |item| {
            let origin = crate::view::point_to_window(receiver, item.frame.origin);
            (
                item.contents,
                NSPoint {
                    x: origin.x - pointer.x,
                    y: origin.y - pointer.y,
                },
            )
        },
    );
    let location = match window.is_null() {
        true => screen_location(event),
        false => crate::window::to_screen(window, pointer),
    };
    let session = begin(board, source, image, image_offset, location);
    PENDING.with_borrow_mut(|pending| {
        *pending = Some(Pending {
            event: objc::arc::retain(event),
            refused,
        })
    });
    // The active session owns it; the caller is handed it to adjust.
    objc::arc::autorelease(objc::arc::retain(session))
}

// NSDraggingItem.

#[repr(C)]
struct Item {
    isa: *const c_void,
    /// Retained, as are the contents.
    writer: Id,
    frame: NSRect,
    contents: Id,
}

pub const ITEM_INSTANCE_SIZE: usize = size_of::<Item>();

fn item_of<'a>(object: Id) -> Option<&'a mut Item> {
    // SAFETY: null or a live object.
    let is_item = unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSDraggingItem")
            .is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    });
    // SAFETY: the check above says the storage is this module's.
    is_item.then(|| unsafe { &mut *(object as *mut Item) })
}

unsafe extern "C" fn item_init_with_writer(receiver: Id, _sel: Sel, writer: Id) -> Id {
    if let Some(item) = item_of(receiver) {
        objc::arc::release(std::mem::replace(
            &mut item.writer,
            objc::arc::retain(writer),
        ));
    }
    receiver
}

unsafe extern "C" fn item_writer(receiver: Id, _sel: Sel) -> Id {
    item_of(receiver).map_or(std::ptr::null_mut(), |item| item.writer)
}

unsafe extern "C" fn item_frame(receiver: Id, _sel: Sel) -> NSRect {
    item_of(receiver).map_or(NSRect::default(), |item| item.frame)
}

unsafe extern "C" fn item_set_frame(receiver: Id, _sel: Sel, frame: NSRect) {
    if let Some(item) = item_of(receiver) {
        item.frame = frame;
    }
}

unsafe extern "C" fn item_set_frame_contents(receiver: Id, _sel: Sel, frame: NSRect, contents: Id) {
    if let Some(item) = item_of(receiver) {
        item.frame = frame;
        objc::arc::release(std::mem::replace(
            &mut item.contents,
            objc::arc::retain(contents),
        ));
    }
}

unsafe extern "C" fn item_dealloc(receiver: Id, _sel: Sel) {
    if let Some(item) = item_of(receiver) {
        objc::arc::release(std::mem::replace(&mut item.writer, std::ptr::null_mut()));
        objc::arc::release(std::mem::replace(&mut item.contents, std::ptr::null_mut()));
    }
    objc::arc::finalize(receiver);
}

// The two faces.

unsafe extern "C" fn dragging_pasteboard(receiver: Id, _sel: Sel) -> Id {
    with_session(receiver, |s| s.pasteboard as usize) as Id
}

unsafe extern "C" fn sequence_number(receiver: Id, _sel: Sel) -> isize {
    with_session(receiver, |s| s.sequence)
}

/// `NSDraggingSession`'s location is the pointer's on the screen.
unsafe extern "C" fn session_location(receiver: Id, _sel: Sel) -> NSPoint {
    with_session(receiver, |s| s.location)
}

/// `NSDraggingInfo`'s is in the destination window's base coordinates.
unsafe extern "C" fn info_location(receiver: Id, _sel: Sel) -> NSPoint {
    let (window, at) = with_session(receiver, |s| (s.destination_window as usize, s.location));
    match window == 0 {
        true => at,
        false => crate::window::from_screen(window as Id, at),
    }
}

unsafe extern "C" fn info_image_location(receiver: Id, sel: Sel) -> NSPoint {
    let offset_by = with_session(receiver, |s| s.image_offset);
    // SAFETY: the receiver this method was sent to.
    offset(unsafe { info_location(receiver, sel) }, offset_by)
}

unsafe extern "C" fn info_destination_window(receiver: Id, _sel: Sel) -> Id {
    with_session(receiver, |s| s.destination_window as usize) as Id
}

unsafe extern "C" fn info_source_operation_mask(receiver: Id, _sel: Sel) -> u64 {
    with_session(receiver, |s| {
        effective_mask(s.mask, s.modifiers, s.ignores_modifiers)
    })
}

unsafe extern "C" fn info_image(receiver: Id, _sel: Sel) -> Id {
    with_session(receiver, |s| s.image as usize) as Id
}

unsafe extern "C" fn info_source(receiver: Id, _sel: Sel) -> Id {
    with_session(receiver, |s| s.source as usize) as Id
}

/// `-slideDraggedImageTo:`: there is no image on screen to slide.
unsafe extern "C" fn info_slide_image(_receiver: Id, _sel: Sel, _to: NSPoint) {}

unsafe extern "C" fn formation(receiver: Id, _sel: Sel) -> isize {
    with_session(receiver, |s| s.formation)
}

unsafe extern "C" fn set_formation(receiver: Id, _sel: Sel, value: isize) {
    with_session(receiver, |s| s.formation = value);
}

unsafe extern "C" fn animates(receiver: Id, _sel: Sel) -> u8 {
    with_session(receiver, |s| s.animates as u8)
}

unsafe extern "C" fn set_animates(receiver: Id, _sel: Sel, value: u8) {
    with_session(receiver, |s| s.animates = value != 0);
}

unsafe extern "C" fn valid_items(receiver: Id, _sel: Sel) -> isize {
    with_session(receiver, |s| s.valid_items)
}

unsafe extern "C" fn set_valid_items(receiver: Id, _sel: Sel, value: isize) {
    with_session(receiver, |s| s.valid_items = value);
}

unsafe extern "C" fn leader_index(receiver: Id, _sel: Sel) -> isize {
    with_session(receiver, |s| s.leader_index)
}

unsafe extern "C" fn set_leader_index(receiver: Id, _sel: Sel, value: isize) {
    with_session(receiver, |s| s.leader_index = value);
}

/// `-springLoadingHighlight`: `NSSpringLoadingHighlightNone`, since nothing
/// here spring-loads.
unsafe extern "C" fn spring_loading_highlight(_receiver: Id, _sel: Sel) -> isize {
    0
}

unsafe extern "C" fn reset_spring_loading(_receiver: Id, _sel: Sel) {}

const DRAG_IMAGE: &std::ffi::CStr = c"v84@0:8@16{CGPoint=dd}24{CGSize=dd}40@56@64@72c80";

pub(crate) fn install() {
    for name in [c"NSView", c"NSWindow"] {
        let class = objc::class::lookup(name).expect("declared before its methods");
        methods!(class, {
            "registerForDraggedTypes:" => register_for_dragged_types, c"v24@0:8@16";
            "unregisterDraggedTypes" => unregister_dragged_types, c"v16@0:8";
        });
    }
    let view = objc::class::lookup(c"NSView").expect("declared before its methods");
    view.replace_method(
        objc::sel::get("dragImage:at:offset:event:pasteboard:source:slideBack:"),
        view_drag_image as *const c_void,
        DRAG_IMAGE,
    );
    methods!(view, {
        "registeredDraggedTypes" => registered_dragged_types, c"@16@0:8";
        "beginDraggingSessionWithItems:event:source:" => begin_dragging_session,
            c"@40@0:8@16@24@32";
    });
    let window = objc::class::lookup(c"NSWindow").expect("declared before its methods");
    window.replace_method(
        objc::sel::get("dragImage:at:offset:event:pasteboard:source:slideBack:"),
        window_drag_image as *const c_void,
        DRAG_IMAGE,
    );

    let session = objc::class::lookup(c"NSDraggingSession").expect("declared before its methods");
    methods!(session, {
        "draggingPasteboard" => dragging_pasteboard, c"@16@0:8";
        "draggingSequenceNumber" => sequence_number, c"q16@0:8";
        "draggingLocation" => session_location, c"{CGPoint=dd}16@0:8";
        "draggingFormation" => formation, c"q16@0:8";
        "setDraggingFormation:" => set_formation, c"v24@0:8q16";
        "animatesToStartingPositionsOnCancelOrFail" => animates, c"c16@0:8";
        "setAnimatesToStartingPositionsOnCancelOrFail:" => set_animates, c"v20@0:8c16";
        "draggingLeaderIndex" => leader_index, c"q16@0:8";
        "setDraggingLeaderIndex:" => set_leader_index, c"v24@0:8q16";
    });
    let info =
        objc::class::lookup(c"NSDraggingDestinationInfo").expect("declared before its methods");
    methods!(info, {
        "draggingPasteboard" => dragging_pasteboard, c"@16@0:8";
        "draggingSequenceNumber" => sequence_number, c"q16@0:8";
        "draggingLocation" => info_location, c"{CGPoint=dd}16@0:8";
        "draggedImageLocation" => info_image_location, c"{CGPoint=dd}16@0:8";
        "draggingDestinationWindow" => info_destination_window, c"@16@0:8";
        "draggingSourceOperationMask" => info_source_operation_mask, c"Q16@0:8";
        "draggedImage" => info_image, c"@16@0:8";
        "draggingSource" => info_source, c"@16@0:8";
        "slideDraggedImageTo:" => info_slide_image, c"v32@0:8{CGPoint=dd}16";
        "draggingFormation" => formation, c"q16@0:8";
        "setDraggingFormation:" => set_formation, c"v24@0:8q16";
        "animatesToDestination" => animates, c"c16@0:8";
        "setAnimatesToDestination:" => set_animates, c"v20@0:8c16";
        "numberOfValidItemsForDrop" => valid_items, c"q16@0:8";
        "setNumberOfValidItemsForDrop:" => set_valid_items, c"v24@0:8q16";
        "springLoadingHighlight" => spring_loading_highlight, c"q16@0:8";
        "resetSpringLoading" => reset_spring_loading, c"v16@0:8";
    });
    let item = objc::class::lookup(c"NSDraggingItem").expect("declared before its methods");
    methods!(item, {
        "initWithPasteboardWriter:" => item_init_with_writer, c"@24@0:8@16";
        "item" => item_writer, c"@16@0:8";
        "draggingFrame" => item_frame, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "setDraggingFrame:" => item_set_frame, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "setDraggingFrame:contents:" => item_set_frame_contents,
            c"v56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "dealloc" => item_dealloc, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static CALLS: RefCell<Vec<(&'static str, usize)>> = const { RefCell::new(Vec::new()) };
        static ANSWER: Cell<u64> = const { Cell::new(operation::COPY) };
        static PERFORMS: Cell<bool> = const { Cell::new(true) };
        static SOURCE_MASK: Cell<u64> = const { Cell::new(operation::COPY | operation::MOVE) };
        static SEEN: Cell<Option<(NSPoint, u64)>> = const { Cell::new(None) };
        static ENDED: Cell<Option<(NSPoint, u64)>> = const { Cell::new(None) };
    }

    fn record(name: &'static str, receiver: Id) {
        CALLS.with_borrow_mut(|calls| calls.push((name, receiver as usize)));
    }

    fn calls() -> Vec<&'static str> {
        CALLS.with_borrow(|calls| calls.iter().map(|(name, _)| *name).collect())
    }

    unsafe extern "C" fn entered(receiver: Id, _sel: Sel, info: Id) -> u64 {
        record("entered", receiver);
        // SAFETY: the info object this module made.
        let seen = unsafe {
            (
                info_location(info, objc::sel::get("draggingLocation")),
                info_source_operation_mask(info, objc::sel::get("draggingSourceOperationMask")),
            )
        };
        SEEN.set(Some(seen));
        ANSWER.get()
    }

    unsafe extern "C" fn updated(receiver: Id, _sel: Sel, _info: Id) -> u64 {
        record("updated", receiver);
        ANSWER.get()
    }

    unsafe extern "C" fn exited(receiver: Id, _sel: Sel, _info: Id) {
        record("exited", receiver);
    }

    unsafe extern "C" fn prepare(receiver: Id, _sel: Sel, _info: Id) -> u8 {
        record("prepare", receiver);
        1
    }

    unsafe extern "C" fn perform(receiver: Id, _sel: Sel, _info: Id) -> u8 {
        record("perform", receiver);
        PERFORMS.get() as u8
    }

    unsafe extern "C" fn conclude(receiver: Id, _sel: Sel, _info: Id) {
        record("conclude", receiver);
    }

    unsafe extern "C" fn ended(receiver: Id, _sel: Sel, _info: Id) {
        record("ended", receiver);
    }

    unsafe extern "C" fn source_mask_for(_r: Id, _sel: Sel, _session: Id, _context: isize) -> u64 {
        SOURCE_MASK.get()
    }

    unsafe extern "C" fn ignore_modifiers(_r: Id, _sel: Sel, _session: Id) -> u8 {
        1
    }

    unsafe extern "C" fn source_ended(_r: Id, _sel: Sel, _session: Id, at: NSPoint, op: u64) {
        ENDED.set(Some((at, op)));
    }

    fn class(name: &std::ffi::CStr, methods: &[(&str, *const c_void, &std::ffi::CStr)]) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(name).unwrap_or_else(|| {
            let class = corefoundation::base::register(
                name,
                objc::class::lookup(c"NSObject"),
                size_of::<usize>(),
                false,
            );
            for (selector, imp, types) in methods {
                class.add_method(objc::sel::get(selector), *imp, types);
            }
            class
        });
        objc::object::create_instance(class, 0)
    }

    fn destination() -> Id {
        class(
            c"RosemaryTestDragDestination",
            &[
                ("draggingEntered:", entered as *const c_void, c"Q24@0:8@16"),
                ("draggingUpdated:", updated as *const c_void, c"Q24@0:8@16"),
                ("draggingExited:", exited as *const c_void, c"v24@0:8@16"),
                (
                    "prepareForDragOperation:",
                    prepare as *const c_void,
                    c"c24@0:8@16",
                ),
                (
                    "performDragOperation:",
                    perform as *const c_void,
                    c"c24@0:8@16",
                ),
                (
                    "concludeDragOperation:",
                    conclude as *const c_void,
                    c"v24@0:8@16",
                ),
                ("draggingEnded:", ended as *const c_void, c"v24@0:8@16"),
            ],
        )
    }

    fn source() -> Id {
        class(
            c"RosemaryTestDragSource",
            &[
                (
                    "draggingSession:sourceOperationMaskForDraggingContext:",
                    source_mask_for as *const c_void,
                    c"Q32@0:8@16q24",
                ),
                (
                    "ignoreModifierKeysForDraggingSession:",
                    ignore_modifiers as *const c_void,
                    c"c24@0:8@16",
                ),
                (
                    "draggingSession:endedAtPoint:operation:",
                    source_ended as *const c_void,
                    c"v48@0:8@16{CGPoint=dd}24Q40",
                ),
            ],
        )
    }

    const AT: NSPoint = NSPoint { x: 10.0, y: 20.0 };

    /// A session from `source()` at [`AT`], with its info object.
    fn session() -> Id {
        let source = source();
        begin(
            crate::pasteboard::drag_board(),
            source,
            std::ptr::null_mut(),
            NSPoint::default(),
            AT,
        );
        ACTIVE.with_borrow(|a| a.as_ref().expect("a session").info)
    }

    fn over(destination: Id) -> Option<Target> {
        Some(Target {
            destination,
            window: std::ptr::null_mut(),
        })
    }

    #[test]
    fn an_accepted_drop_is_prepared_performed_and_concluded() {
        let info = session();
        let target = destination();
        let mut tracker = Tracker::default();
        tracker.track(info, over(target), true);
        tracker.track(info, over(target), true);
        assert_eq!(tracker.drop(info), operation::COPY);
        finish(operation::COPY);
        assert_eq!(
            calls(),
            [
                "entered", "updated", "prepare", "perform", "conclude", "ended"
            ]
        );
        assert_eq!(SEEN.get(), Some((AT, operation::COPY | operation::MOVE)));
        assert_eq!(ENDED.get(), Some((AT, operation::COPY)));
        assert!(ACTIVE.with_borrow(Option::is_none));
    }

    #[test]
    fn an_operation_the_source_does_not_allow_is_not_dropped() {
        SOURCE_MASK.set(operation::COPY);
        ANSWER.set(operation::MOVE);
        let info = session();
        let mut tracker = Tracker::default();
        tracker.track(info, over(destination()), true);
        assert_eq!(tracker.drop(info), operation::NONE);
        finish(operation::NONE);
        assert_eq!(calls(), ["entered", "exited", "ended"]);
        assert_eq!(ENDED.get(), Some((AT, operation::NONE)));
    }

    #[test]
    fn a_refused_perform_ends_the_drag_with_no_operation() {
        PERFORMS.set(false);
        let info = session();
        let mut tracker = Tracker::default();
        tracker.track(info, over(destination()), false);
        assert_eq!(tracker.drop(info), operation::NONE);
        finish(operation::NONE);
        assert_eq!(calls(), ["entered", "prepare", "perform", "ended"]);
    }

    #[test]
    fn moving_between_destinations_exits_one_before_entering_the_next() {
        let info = session();
        let (first, second) = (destination(), destination());
        let mut tracker = Tracker::default();
        tracker.track(info, over(first), true);
        tracker.track(info, over(second), true);
        tracker.track(info, None, true);
        tracker.cancel(info);
        finish(operation::NONE);
        let order = CALLS.with_borrow(|calls| calls.clone());
        assert_eq!(
            order,
            [
                ("entered", first as usize),
                ("exited", first as usize),
                ("entered", second as usize),
                ("exited", second as usize),
            ]
        );
    }

    #[test]
    fn cancelling_tells_the_destination_it_was_left() {
        let info = session();
        let mut tracker = Tracker::default();
        tracker.track(info, over(destination()), true);
        tracker.cancel(info);
        finish(operation::NONE);
        assert_eq!(calls(), ["entered", "exited"]);
        assert_eq!(ENDED.get(), Some((AT, operation::NONE)));
    }

    #[test]
    fn modifier_keys_narrow_the_source_mask_unless_it_ignores_them() {
        use crate::keymap::flags;
        let mask = operation::COPY | operation::LINK | operation::GENERIC;
        assert_eq!(effective_mask(mask, 0, false), mask);
        assert_eq!(effective_mask(mask, flags::OPTION, false), operation::COPY);
        assert_eq!(effective_mask(mask, flags::CONTROL, false), operation::LINK);
        assert_eq!(
            effective_mask(mask, flags::COMMAND, false),
            operation::GENERIC
        );
        assert_eq!(
            effective_mask(operation::MOVE, flags::OPTION, false),
            operation::NONE
        );
        assert_eq!(effective_mask(mask, flags::OPTION, true), mask);
    }

    #[test]
    fn a_session_from_items_ends_from_the_event_loop_not_inside_the_call() {
        let source = source();
        let class = objc::class::lookup(c"NSDraggingItem").expect("NSDraggingItem");
        let item = objc::object::create_instance(class, 0);
        // A writer the drag pasteboard cannot hold refuses the session.
        let writer = destination();
        // SAFETY: an item of this module's, with a live writer.
        unsafe { item_init_with_writer(item, objc::sel::get("initWithPasteboardWriter:"), writer) };
        let items = corefoundation::collections::new_array(
            &[item as *const c_void],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id;
        let event = crate::nsevent::make(crate::nsevent::Fields {
            kind: kind::LEFT_MOUSE_DRAGGED,
            location: AT,
            ..Default::default()
        });
        let sel = objc::sel::get("beginDraggingSessionWithItems:event:source:");
        // SAFETY: the objects above; a null view has no window.
        let session =
            unsafe { begin_dragging_session(std::ptr::null_mut(), sel, items, event, source) };
        assert!(!session.is_null());
        assert_eq!(ENDED.get(), None);
        run_pending();
        assert_eq!(ENDED.get(), Some((AT, operation::NONE)));
        assert!(!busy());
        objc::arc::release(item);
    }

    #[test]
    fn the_destination_is_the_nearest_view_registered_for_the_dragged_type() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSView").expect("NSView");
        let parent = objc::object::create_instance(class, 0);
        let child = objc::object::create_instance(class, 0);
        // SAFETY: `-addSubview:` is `void (id, SEL, id)` on a view.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(parent, objc::sel::get("addSubview:"), child);
        }
        let dragged = b"com.example.dragged".to_vec();
        let types = crate::pasteboard::strings([dragged.clone()]);
        // SAFETY: the views above, with an array of strings.
        unsafe {
            register_for_dragged_types(parent, objc::sel::get("registerForDraggedTypes:"), types)
        };
        assert_eq!(
            registered_ancestor(child, std::slice::from_ref(&dragged)),
            Some(parent)
        );
        assert_eq!(registered_ancestor(child, &[b"public.png".to_vec()]), None);
        // SAFETY: as above.
        unsafe { unregister_dragged_types(parent, objc::sel::get("unregisterDraggedTypes")) };
        assert_eq!(registered_ancestor(child, &[dragged]), None);
        objc::arc::release(parent);
    }
}
