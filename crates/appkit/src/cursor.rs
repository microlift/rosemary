use std::ffi::c_void;

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;
use static_init::dynamic;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct TrackingArea {
    isa: *const c_void,
    rect: NSRect,
    options: usize,
    owner: Id,
    user_info: Id,
    /// Whether the pointer is inside, for `-mouseEntered:` and `-mouseExited:`.
    inside: usize,
    /// Whether this is the area that last sent `-cursorUpdate:` in its window.
    has_cursor: usize,
}

pub const TRACKING_AREA_INSTANCE_SIZE: usize = size_of::<TrackingArea>();

#[repr(C)]
pub struct Cursor {
    isa: *const c_void,
    /// Which of the named cursors this is. SDL has no shape to switch to, so
    /// the value is kept for a program that reads it back and not used.
    shape: usize,
    /// The image a custom cursor was made from, retained, and where its point
    /// is. Both are read back by a program, and both are what `sdl` was built
    /// from.
    image: Id,
    hot_spot: foundation::geometry::NSPoint,
    /// The SDL cursor `-set` shows, or null for a named cursor and for an
    /// image this tree could not decode - both of which show the desktop's.
    sdl: sdl3::Cursor,
    /// `-setOnMouseEntered:` and `-setOnMouseExited:`, stored to be read back.
    on_mouse_entered: usize,
    on_mouse_exited: usize,
}

pub const CURSOR_INSTANCE_SIZE: usize = size_of::<Cursor>();

unsafe extern "C" fn init_tracking_area(
    receiver: Id,
    _sel: Sel,
    rect: NSRect,
    options: usize,
    owner: Id,
    user_info: Id,
) -> Id {
    // SAFETY: a tracking area of this crate's.
    let area = unsafe { &mut *(receiver as *mut TrackingArea) };
    area.rect = rect;
    area.options = options;
    // The owner is unretained, as AppKit's is: a view owns its tracking areas
    // and is usually the owner itself, so retaining would be a cycle.
    area.owner = owner;
    area.user_info = objc::arc::retain(user_info);
    // `NSTrackingAssumeInside` is the answer to "was the pointer inside when
    // this was added", and the first move sends the exit if it was wrong.
    area.inside = (options & tracking::ASSUME_INSIDE != 0) as usize;
    area.has_cursor = 0;
    receiver
}

unsafe extern "C" fn tracking_rect(receiver: Id, _sel: Sel) -> NSRect {
    // SAFETY: a tracking area of this crate's.
    unsafe { (*(receiver as *const TrackingArea)).rect }
}

unsafe extern "C" fn tracking_options(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a tracking area of this crate's.
    unsafe { (*(receiver as *const TrackingArea)).options }
}

unsafe extern "C" fn tracking_owner(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a tracking area of this crate's.
    unsafe { (*(receiver as *const TrackingArea)).owner }
}

unsafe extern "C" fn tracking_user_info(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a tracking area of this crate's.
    unsafe { (*(receiver as *const TrackingArea)).user_info }
}

/// Where the point lands in the pixels SDL is given.
///
/// The hot spot is in the image's points and the bitmap is in pixels, which
/// differ for an image whose representation is 2x. An image reporting no size
/// is taken as one point per pixel, since that is the only ratio available.
fn hot_spot_pixels(hot_spot: NSPoint, size: NSSize, width: u32, height: u32) -> (i32, i32) {
    let scale = |point: f64, extent: f64, pixels: u32| match extent > 0.0 {
        true => point * f64::from(pixels) / extent,
        false => point,
    };
    (
        scale(hot_spot.x, size.width, width) as i32,
        scale(hot_spot.y, size.height, height) as i32,
    )
}

/// `ROSETTA3_TRACE_CURSOR`: name every cursor a program builds, every rectangle
/// it installs one in, and which one the pointer ends up showing.
///
/// It exists because none of that is visible from the outside: a custom cursor
/// that never appears looks the same whether the image had no pixels, the
/// program's `-resetCursorRects` was never called, the rectangle it gave is on
/// a view in no window, or the pointer is outside that rectangle - and the four
/// have nothing in common to fix.
#[inline(always)]
pub fn tracing() -> bool {
    #[dynamic]
    static ON: bool = std::env::var_os("ROSETTA3_TRACE_CURSOR").is_some();

    *ON
}

/// Show a cursor object's pixels, without recording it as the one a program
/// `-set`: the cursor rectangles put a cursor on screen while leaving
/// `+currentCursor` saying what the program last chose, which is Apple's
/// split and what a program comparing the two depends on.
fn show(cursor: Id) {
    let sdl = match is_cursor(cursor) {
        // SAFETY: a cursor of this crate's.
        true => unsafe { (*(cursor as *const Cursor)).sdl },
        false => std::ptr::null_mut(),
    };
    // Only on a change: this runs on every mouse move, and SDL redraws the
    // pointer for each call whether or not it is the cursor already shown.
    if SHOWN.swap(sdl as usize, std::sync::atomic::Ordering::AcqRel) == sdl as usize {
        return;
    }
    sdl3::video::set_cursor(sdl);
}

/// The SDL cursor on screen, so [`show`] can skip the calls that change
/// nothing. Reset when a cursor is destroyed: SDL is free to hand the same
/// address out again, and a stale match would leave the new cursor unshown.
static SHOWN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(usize::MAX);

/// Whether `object` is one of this crate's cursors, checked rather than
/// assumed: `-addCursorRect:cursor:` takes an `id` from the guest.
fn is_cursor(object: Id) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: an object this runtime allocated.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSCursor").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

/// Build the SDL cursor for an image, or null when there are no pixels to
/// build it from.
///
/// A cursor with no pixels is a diagnostic rather than a refusal: the program
/// gets the desktop's pointer instead of its own, which is worse than the
/// image it asked for and far better than an invisible pointer.
fn cursor_from_image(image: Id, hot_spot: NSPoint) -> sdl3::Cursor {
    let Some(bitmap) = crate::image::bitmap(image) else {
        eprintln!(
            "rosetta3: appkit: -[NSCursor initWithImage:hotSpot:]: no pixels for this image - \
             the pointer keeps the desktop's cursor"
        );
        return std::ptr::null_mut();
    };
    let size = crate::image::declared_size(image);
    let (x, y) = hot_spot_pixels(hot_spot, size, bitmap.width, bitmap.height);
    sdl3::video::create_color_cursor(
        bitmap.width as i32,
        bitmap.height as i32,
        &bitmap.pixels,
        x,
        y,
    )
    .unwrap_or(std::ptr::null_mut())
}

/// `-initWithImage:hotSpot:`.
unsafe extern "C" fn init_with_image(receiver: Id, _sel: Sel, image: Id, hot_spot: NSPoint) -> Id {
    // SAFETY: a cursor of this crate's.
    let cursor = unsafe { &mut *(receiver as *mut Cursor) };
    cursor.image = objc::arc::retain(image);
    cursor.hot_spot = hot_spot;
    cursor.sdl = cursor_from_image(image, hot_spot);
    if tracing() {
        let size = crate::image::declared_size(image);
        eprintln!(
            "rosetta3: appkit: -[NSCursor initWithImage:hotSpot:] {receiver:p}: \
             {}x{} points, point ({}, {}), {}",
            size.width,
            size.height,
            hot_spot.x,
            hot_spot.y,
            match cursor.sdl.is_null() {
                true => "no SDL cursor - the pointer keeps the desktop's",
                false => "SDL cursor made",
            }
        );
    }
    receiver
}

/// `-dealloc`, which has to put the desktop's cursor back if this was the one
/// on screen: SDL keeps showing a destroyed cursor's pixels otherwise.
unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a cursor of this crate's.
    let cursor = unsafe { &mut *(receiver as *mut Cursor) };
    objc::arc::release(cursor.image);
    cursor.image = std::ptr::null_mut();
    if CURRENT
        .compare_exchange(
            receiver as usize,
            0,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_ok()
    {
        sdl3::video::set_cursor(std::ptr::null_mut());
    }
    if !cursor.sdl.is_null() {
        SHOWN.store(usize::MAX, std::sync::atomic::Ordering::Release);
        // SAFETY: made by `create_color_cursor`, and destroyed once - the last
        // reference to the object holding it is gone.
        unsafe { sdl3::video::destroy_cursor(cursor.sdl) };
        cursor.sdl = std::ptr::null_mut();
    }
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn image(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const Cursor)).image }
}

unsafe extern "C" fn hot_spot(receiver: Id, _sel: Sel) -> NSPoint {
    unsafe { (*(receiver as *const Cursor)).hot_spot }
}

/// One `-addCursorRect:cursor:`, kept until the view discards its rectangles.
///
/// The view is retained. AppKit's rectangles are dropped by the view's own
/// `-dealloc`; nothing here reaches that, so an unretained view would leave
/// [`apply`] reading a freed object on the next mouse move.
struct CursorRect {
    view: usize,
    rect: NSRect,
    /// Retained: the program's own reference usually goes away as soon as
    /// `-resetCursorRects` returns, and the cursor has to outlive it.
    cursor: usize,
}

static RECTS: std::sync::Mutex<Vec<CursorRect>> = std::sync::Mutex::new(Vec::new());

/// `-disableCursorRects`, which a program uses while it draws its own pointer.
static RECTS_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_rects_enabled(enabled: bool) {
    RECTS_ENABLED.store(enabled, std::sync::atomic::Ordering::Release);
    apply();
}

pub fn rects_enabled() -> bool {
    RECTS_ENABLED.load(std::sync::atomic::Ordering::Acquire)
}

/// `-[NSView addCursorRect:cursor:]`.
pub fn add_rect(view: Id, rect: NSRect, cursor: Id) {
    if !is_cursor(cursor) {
        if tracing() {
            eprintln!(
                "rosetta3: appkit: -[NSView addCursorRect:cursor:] {view:p}: {cursor:p} is \
                 not an NSCursor - the rectangle is dropped"
            );
        }
        return;
    }
    if tracing() {
        eprintln!(
            "rosetta3: appkit: -[NSView addCursorRect:cursor:] {view:p}: ({}, {}) {}x{} -> \
             {cursor:p}",
            rect.origin.x, rect.origin.y, rect.size.width, rect.size.height
        );
    }
    RECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(CursorRect {
            view: objc::arc::retain(view) as usize,
            rect,
            cursor: objc::arc::retain(cursor) as usize,
        });
    apply();
}

/// `-[NSView discardCursorRects]`, and the first half of every reset.
pub fn discard_rects(view: Id) {
    let taken: Vec<CursorRect> = {
        let mut rects = RECTS.lock().unwrap_or_else(|e| e.into_inner());
        let mut kept = Vec::with_capacity(rects.len());
        let mut taken = Vec::new();
        for entry in rects.drain(..) {
            match entry.view == view as usize {
                true => taken.push(entry),
                false => kept.push(entry),
            }
        }
        *rects = kept;
        taken
    };
    for entry in taken {
        objc::arc::release(entry.cursor as Id);
        objc::arc::release(entry.view as Id);
    }
}

/// Rebuild one view's cursor rectangles, which is what
/// `-invalidateCursorRectsForView:` means: throw the old ones away and ask the
/// view for its new ones through `-resetCursorRects`.
///
/// A view with no `-resetCursorRects` of its own is not an error - AppKit's is
/// an empty method - but it is the whole of a program's custom cursor, so the
/// trace names it.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn reset_rects(view: Id) {
    discard_rects(view);
    let sel = objc::sel::get("resetCursorRects");
    let method = unsafe { objc::object::class_of(view) }.and_then(|c| c.method(sel));
    if tracing() {
        // The window as well as the method: a rectangle on a view that is in no
        // window can never hold the pointer, and the two failures look the same
        // from the outside.
        let window = unsafe { crate::view::window_of(view) };
        eprintln!(
            "rosetta3: appkit: -resetCursorRects to {view:p} (in window {window:p}): {}",
            match method.is_some() {
                true => "implemented",
                false => "NOT implemented - this view has no cursor of its own",
            }
        );
    }
    if let Some(method) = method {
        // SAFETY: `-resetCursorRects` is `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(method.implementation());
            f(view, sel);
        }
    }
    apply();
}

/// The same, down a view tree, for `-[NSWindow resetCursorRects]`.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn reset_rects_in_tree(view: Id) {
    for child in unsafe { crate::view::children(view) } {
        // SAFETY: an object this runtime allocated.
        unsafe { reset_rects_in_tree(child) };
    }
    unsafe { reset_rects(view) };
}

/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn discard_rects_in_tree(view: Id) {
    for child in unsafe { crate::view::children(view) } {
        // SAFETY: an object this runtime allocated.
        unsafe { discard_rects_in_tree(child) };
    }
    discard_rects(view);
    apply();
}

/// Decide what the pointer shows, from the cursor rectangles and where the
/// pointer is.
///
/// The last rectangle installed that holds the pointer wins, which is AppKit's
/// order for overlapping rectangles, and a pointer in none of them falls back
/// to whatever the program last `-set`. Called wherever either half can have
/// changed: a rectangle added or discarded, and every mouse move.
pub fn apply() {
    if OVER_CHROME.load(std::sync::atomic::Ordering::Acquire) {
        show(std::ptr::null_mut());
        return;
    }
    let current = CURRENT.load(std::sync::atomic::Ordering::Acquire) as Id;
    let chosen = match rects_enabled() {
        true => holding_rect().unwrap_or(current),
        false => current,
    };
    if tracing() {
        // Only on a change: this runs on every mouse move.
        static LAST: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(usize::MAX);
        if LAST.swap(chosen as usize, std::sync::atomic::Ordering::AcqRel) != chosen as usize {
            eprintln!(
                "rosetta3: appkit: the pointer shows {chosen:p}, from {} cursor rectangle(s){}",
                RECTS.lock().unwrap_or_else(|e| e.into_inner()).len(),
                match std::ptr::eq(chosen, current) {
                    true => " - none of them holds the pointer",
                    false => "",
                }
            );
        }
    }
    show(chosen);
}

/// The cursor of the last rectangle installed that holds the pointer, which is
/// AppKit's order for overlapping rectangles.
fn holding_rect() -> Option<Id> {
    let rects = RECTS.lock().unwrap_or_else(|e| e.into_inner());
    rects.iter().rev().find_map(|entry| {
        let view = entry.view as Id;
        // SAFETY: an object this runtime allocated, retained by the entry.
        let point = unsafe { crate::view::pointer_in(view) }?;
        crate::view::holds(view, entry.rect, point).then_some(entry.cursor as Id)
    })
}

/// `NSTrackingAreaOptions`.
mod tracking {
    pub const MOUSE_ENTERED_AND_EXITED: usize = 0x01;
    pub const MOUSE_MOVED: usize = 0x02;
    pub const CURSOR_UPDATE: usize = 0x04;
    pub const ACTIVE_WHEN_FIRST_RESPONDER: usize = 0x10;
    pub const ACTIVE_IN_KEY_WINDOW: usize = 0x20;
    pub const ACTIVE_IN_ACTIVE_APP: usize = 0x40;
    pub const ACTIVE_ALWAYS: usize = 0x80;
    pub const ASSUME_INSIDE: usize = 0x100;
    pub const IN_VISIBLE_RECT: usize = 0x200;
    pub const ENABLED_DURING_MOUSE_DRAG: usize = 0x400;
}

/// Whether an area with these options is active at all.
///
/// There is one application and it is always active, so only the key-window and
/// first-responder conditions can turn an area off.
fn is_active(options: usize, in_key_window: bool, owner_is_first: bool) -> bool {
    use tracking::*;
    options & (ACTIVE_ALWAYS | ACTIVE_IN_ACTIVE_APP) != 0
        || (options & ACTIVE_IN_KEY_WINDOW != 0 && in_key_window)
        || (options & ACTIVE_WHEN_FIRST_RESPONDER != 0 && in_key_window && owner_is_first)
}

/// Whether a drag holds this area's events back until it ends.
fn held_by_drag(options: usize, dragging: bool) -> bool {
    dragging && options & tracking::ENABLED_DURING_MOUSE_DRAG == 0
}

/// A tracking area of this crate's, checked rather than assumed: the object
/// comes back out of a guest-visible list and an event a guest built.
fn area_of<'a>(object: Id) -> Option<&'a mut TrackingArea> {
    if object.is_null() {
        return None;
    }
    // SAFETY: an object this runtime allocated.
    let is_area = unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSTrackingArea")
            .is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    });
    // SAFETY: the check above says the storage is this module's.
    is_area.then(|| unsafe { &mut *(object as *mut TrackingArea) })
}

/// The owner a tracking-area event is delivered to, or null.
pub fn tracking_owner_of(area: Id) -> Id {
    area_of(area).map_or(std::ptr::null_mut(), |a| a.owner)
}

/// The `userInfo` an area was made with, which `-[NSEvent userData]` answers.
pub fn tracking_user_info_of(area: Id) -> Id {
    area_of(area).map_or(std::ptr::null_mut(), |a| a.user_info)
}

/// One area installed in a window, and whether it holds the pointer now.
struct Installed {
    /// Retained for the pass: a handler may remove the area, and the events
    /// built from it still name it.
    area: Id,
    active: bool,
    holds: bool,
}

/// Every area in a view tree, in drawing order - parents before children and
/// siblings in order - so the last one holding the pointer is the topmost.
///
/// Areas on hidden views are listed as holding nothing: a view that is hidden
/// with the pointer inside it is one the pointer has left.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
unsafe fn installed_in(view: Id, key: bool, first: Id, out: &mut Vec<Installed>) {
    if view.is_null() {
        return;
    }
    let hidden = crate::view::is_hidden_or_has_hidden_ancestor(view);
    let point = unsafe { crate::view::pointer_in(view) };
    for area in unsafe { crate::view::tracking_areas_of(view) } {
        let Some(tracked) = area_of(area) else {
            continue;
        };
        let rect = match tracked.options & tracking::IN_VISIBLE_RECT != 0 {
            true => crate::view::bounds_of(view),
            false => tracked.rect,
        };
        out.push(Installed {
            area: objc::arc::retain(area),
            active: is_active(tracked.options, key, tracked.owner == first),
            holds: !hidden && point.is_some_and(|p| crate::view::holds(view, rect, p)),
        });
    }
    for child in unsafe { crate::view::children(view) } {
        // SAFETY: an object this runtime allocated.
        unsafe { installed_in(child, key, first, out) };
    }
}

/// The areas of `window`'s content view, retained; see [`Installed`].
fn installed(window: Id) -> Vec<Installed> {
    let key = crate::window::key_window() == window;
    let first = match key {
        // SAFETY: `-firstResponder` is `id (id, SEL)` on a window this crate made.
        true => unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(window, objc::sel::get("firstResponder"))
        },
        false => std::ptr::null_mut(),
    };
    let mut out = Vec::new();
    // SAFETY: null or the content view, which this crate made.
    unsafe { installed_in(crate::window::content_view_of(window), key, first, &mut out) };
    out
}

/// Deliver tracking events for the pointer's position in `window`, as AppKit
/// does from its tracking areas. Called for every move in the window.
///
/// The state lives on each area - whether the pointer is inside it, and whether
/// it is the one that last had `-cursorUpdate:` - so an area that is removed
/// takes its state with it, and a new one at the same address starts clean.
///
/// - `NSTrackingMouseEnteredAndExited`: `-mouseEntered:` and `-mouseExited:` to
///   the owner of every area the pointer crosses, exits first.
/// - `NSTrackingCursorUpdate`: `-cursorUpdate:` when the topmost such area
///   changes, to its owner; leaving the last one sends it to the view under the
///   pointer, whose inherited method walks up to the window and puts the arrow
///   back.
///
/// During a drag an area without `NSTrackingEnabledDuringMouseDrag` keeps the
/// state it had, and is brought up to date by the first move after the drag.
pub fn update_tracking(window: Id, dragging: bool) {
    if window.is_null() {
        return;
    }
    let areas = installed(window);
    let mut exits = Vec::new();
    let mut enters = Vec::new();
    for entry in &areas {
        let Some(tracked) = area_of(entry.area) else {
            continue;
        };
        if tracked.options & tracking::MOUSE_ENTERED_AND_EXITED == 0
            || held_by_drag(tracked.options, dragging)
        {
            continue;
        }
        let inside = entry.active && entry.holds;
        if inside != (tracked.inside != 0) {
            tracked.inside = inside as usize;
            match inside {
                true => enters.push(entry.area),
                false => exits.push(entry.area),
            }
        }
    }

    let location = crate::window::pointer_location(window);
    let mut events: Vec<Id> = exits
        .into_iter()
        .map(|area| tracking_event(crate::nsevent::kind::MOUSE_EXITED, window, location, area))
        .chain(enters.into_iter().map(|area| {
            tracking_event(crate::nsevent::kind::MOUSE_ENTERED, window, location, area)
        }))
        .collect();

    let wants_cursor = |entry: &&Installed| {
        area_of(entry.area).is_some_and(|a| a.options & tracking::CURSOR_UPDATE != 0)
    };
    let topmost = areas
        .iter()
        .filter(wants_cursor)
        .filter(|e| e.active && e.holds)
        .rfind(|e| area_of(e.area).is_some_and(|a| !held_by_drag(a.options, dragging)))
        .map(|e| e.area);
    let previous = areas
        .iter()
        .find(|e| area_of(e.area).is_some_and(|a| a.has_cursor != 0))
        .map(|e| e.area);
    // A drag holds the cursor it started with, unless an area that asked to be
    // told during one takes it.
    if topmost != previous && (topmost.is_some() || !dragging) {
        if let Some(tracked) = previous.and_then(area_of) {
            tracked.has_cursor = 0;
        }
        match topmost {
            Some(area) => {
                if let Some(tracked) = area_of(area) {
                    tracked.has_cursor = 1;
                }
                events.push(tracking_event(
                    crate::nsevent::kind::CURSOR_UPDATE,
                    window,
                    location,
                    area,
                ));
            }
            None => events.push(tracking_event(
                crate::nsevent::kind::CURSOR_UPDATE,
                window,
                location,
                std::ptr::null_mut(),
            )),
        }
    }

    for event in events {
        crate::event::dispatch(window, event);
    }
    for entry in areas {
        objc::arc::release(entry.area);
    }
}

/// The areas of `window` that `-mouseMoved:` goes to for a move with no
/// button held: active, holding the pointer, and made with
/// `NSTrackingMouseMoved`. Retained, for the caller to release.
///
/// AppKit sends those moves to each area's owner, which is the only place Qt
/// listens for them: its `QNSView` gives a helper object the area, and the
/// view itself has no `-mouseMoved:`.
pub fn mouse_moved_areas(window: Id) -> Vec<Id> {
    if window.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for entry in installed(window) {
        let wanted = entry.active
            && entry.holds
            && area_of(entry.area).is_some_and(|a| a.options & tracking::MOUSE_MOVED != 0);
        match wanted {
            true => out.push(entry.area),
            false => objc::arc::release(entry.area),
        }
    }
    out
}

/// The pointer left `window`: every area it was inside is told so, and the
/// next entry updates the cursor again.
pub fn forget_tracking(window: Id) {
    if window.is_null() {
        return;
    }
    let areas = installed(window);
    let location = crate::window::pointer_location(window);
    let mut events = Vec::new();
    for entry in &areas {
        let Some(tracked) = area_of(entry.area) else {
            continue;
        };
        tracked.has_cursor = 0;
        if tracked.inside != 0 {
            tracked.inside = 0;
            events.push(tracking_event(
                crate::nsevent::kind::MOUSE_EXITED,
                window,
                location,
                entry.area,
            ));
        }
    }
    for event in events {
        crate::event::dispatch(window, event);
    }
    for entry in areas {
        objc::arc::release(entry.area);
    }
}

fn tracking_event(kind: u64, window: Id, location: NSPoint, area: Id) -> Id {
    if tracing() {
        eprintln!("rosetta3: appkit: tracking event {kind} for area {area:p}");
    }
    crate::nsevent::make(crate::nsevent::Fields {
        kind,
        modifiers: crate::keymap::modifiers(sdl3::video::keyboard_modifiers()),
        location,
        window,
        tracking_area: area,
        ..Default::default()
    })
}

/// `-[NSResponder cursorUpdate:]`: pass it to the next responder, which for a
/// view is its superview and then its window.
unsafe extern "C" fn responder_cursor_update(receiver: Id, sel: Sel, event: Id) {
    // SAFETY: installed as `v24@0:8@16`.
    unsafe { crate::responder::forward(receiver, sel, event) }
}

/// `-[NSWindow cursorUpdate:]`, the end of the chain: nothing below asked for a
/// cursor, so the pointer is the arrow.
unsafe extern "C" fn window_cursor_update(_receiver: Id, _sel: Sel, _event: Id) {
    // SAFETY: `-set` is `void (id, SEL)` on a cursor of this crate's.
    unsafe { set(cursor_for(0), objc::sel::get("set")) };
}

/// The named cursors, made once each.
///
/// The shape is the index in this list. Every one is the same pointer as far as
/// SDL is concerned, and they are distinct objects because a program compares
/// `[NSCursor currentCursor]` against the one it set.
static SHAPES: &[&str] = &[
    "arrowCursor",
    "IBeamCursor",
    "IBeamCursorForVerticalLayout",
    "pointingHandCursor",
    "crosshairCursor",
    "closedHandCursor",
    "openHandCursor",
    "resizeLeftCursor",
    "resizeRightCursor",
    "resizeLeftRightCursor",
    "resizeUpCursor",
    "resizeDownCursor",
    "resizeUpDownCursor",
    "disappearingItemCursor",
    "operationNotAllowedCursor",
    "dragLinkCursor",
    "dragCopyCursor",
    "contextualMenuCursor",
];

fn cursor_for(shape: usize) -> Id {
    static CURSORS: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
    let mut cursors = CURSORS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = cursors.iter().find(|(s, _)| *s == shape) {
        return *object as Id;
    }
    let class = objc::class::lookup(c"NSCursor").expect("declared before its methods");
    // SAFETY: `Cursor` is the layout the class was registered with.
    let cursor = unsafe { corefoundation::base::allocate::<Cursor>(class) };
    // SAFETY: freshly allocated.
    unsafe { (*cursor).shape = shape };
    // Retained forever: `+arrowCursor` hands one out unowned.
    objc::arc::retain(cursor as Id);
    cursors.push((shape, cursor as usize));
    cursor as Id
}

unsafe extern "C" fn named_cursor(_class: Id, sel: Sel) -> Id {
    // SAFETY: a registered selector is a NUL-terminated interned name.
    let name = unsafe { std::ffi::CStr::from_ptr(sel) };
    let name = name.to_string_lossy();
    match SHAPES.iter().position(|s| *s == name) {
        Some(shape) => cursor_for(shape),
        // Unreachable through dispatch: the selector came from this table.
        None => objc::fail!("+[NSCursor {name}]: not one of this runtime's cursors"),
    }
}

/// Whether the pointer is on Rosetta 3's own chrome (the titlebar, the menu
/// strip, or an open menu). That is not the program's to dress: a game that
/// hid the pointer or set a crosshair would otherwise leave nothing visible to
/// pick a menu item with, so the chrome always gets the desktop's arrow.
static OVER_CHROME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record whether the pointer is on the chrome, putting the arrow up on the
/// way in and the program's cursor and hide level back on the way out.
pub fn set_over_chrome(over: bool) {
    if OVER_CHROME.swap(over, std::sync::atomic::Ordering::AcqRel) == over {
        return;
    }
    if tracing() {
        eprintln!(
            "rosetta3: appkit: the pointer is {} the chrome",
            match over {
                true => "on",
                false => "off",
            }
        );
    }
    if HIDDEN.load(std::sync::atomic::Ordering::Acquire) > 0 {
        sdl3::video::set_cursor_visible(over);
    }
    apply();
}

/// The cursor a program last set, which `+currentCursor` answers.
static CURRENT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn set(receiver: Id, _sel: Sel) {
    CURRENT.store(receiver as usize, std::sync::atomic::Ordering::Release);
    if tracing() {
        eprintln!("rosetta3: appkit: -[NSCursor set] {receiver:p}");
    }
    // A named cursor has no SDL cursor, which restores the desktop's own: a
    // program that sets `+arrowCursor` back is taking its custom cursor off.
    if !OVER_CHROME.load(std::sync::atomic::Ordering::Acquire) {
        show(receiver);
    }
}

unsafe extern "C" fn current_cursor(_class: Id, _sel: Sel) -> Id {
    match CURRENT.load(std::sync::atomic::Ordering::Acquire) {
        0 => cursor_for(0),
        object => object as Id,
    }
}

/// `+hide` and `+unhide`, which SDL does have.
///
/// AppKit's are *counted* - two hides need two unhides - and SDL's are not, so
/// the count is kept here. A program that hides twice and unhides once expects
/// the pointer to stay hidden, and a game hiding the cursor per frame would
/// otherwise show it again on the first unhide.
static HIDDEN: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

fn hide_cursor() {
    let count = HIDDEN.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    if tracing() {
        eprintln!("rosetta3: appkit: cursor hidden {count} deep");
    }
    if count == 1 && !OVER_CHROME.load(std::sync::atomic::Ordering::Acquire) {
        sdl3::video::set_cursor_visible(false);
    }
}

fn show_cursor() {
    let previous = HIDDEN.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    if tracing() {
        eprintln!("rosetta3: appkit: cursor was hidden {previous} deep");
    }
    if previous <= 0 {
        // AppKit clamps at zero rather than letting the count go negative, or a
        // stray unhide would take an extra hide to undo.
        HIDDEN.store(0, std::sync::atomic::Ordering::Release);
        return;
    }
    if previous == 1 {
        sdl3::video::set_cursor_visible(true);
    }
}

unsafe extern "C" fn hide(_class: Id, _sel: Sel) {
    hide_cursor();
}

unsafe extern "C" fn unhide(_class: Id, _sel: Sel) {
    show_cursor();
}

/// `CGError CGDisplayHideCursor(CGDirectDisplayID)`.
///
/// SDL has one process-wide pointer rather than one per display, matching the
/// effective behavior of CoreGraphics' counted hide level. The display id is
/// consequently immaterial; games commonly pass `kCGNullDirectDisplay` here.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayHideCursor(_display: u32) -> i32 {
    hide_cursor();
    0
}

/// `CGError CGDisplayShowCursor(CGDirectDisplayID)`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayShowCursor(_display: u32) -> i32 {
    show_cursor();
    0
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGCursorIsVisible() -> bool {
    HIDDEN.load(std::sync::atomic::Ordering::Acquire) == 0
}

/// `-setOnMouseEntered:` and `-setOnMouseExited:`, which say that a *tracking
/// area* should set this cursor rather than a cursor rectangle.
///
/// Acted on by the cursor's own `-mouseEntered:` and `-mouseExited:`, which a
/// tracking area owned by the cursor delivers.
unsafe extern "C" fn set_on_mouse_entered(receiver: Id, _sel: Sel, on: u8) {
    // SAFETY: a cursor of this crate's.
    unsafe { &mut *(receiver as *mut Cursor) }.on_mouse_entered = (on != 0) as usize;
}

unsafe extern "C" fn is_set_on_mouse_entered(receiver: Id, _sel: Sel) -> u8 {
    (unsafe { (*(receiver as *const Cursor)).on_mouse_entered } != 0) as u8
}

unsafe extern "C" fn set_on_mouse_exited(receiver: Id, _sel: Sel, on: u8) {
    unsafe { &mut *(receiver as *mut Cursor) }.on_mouse_exited = (on != 0) as usize;
}

unsafe extern "C" fn is_set_on_mouse_exited(receiver: Id, _sel: Sel) -> u8 {
    (unsafe { (*(receiver as *const Cursor)).on_mouse_exited } != 0) as u8
}

/// `-mouseEntered:`: set this cursor, if it was told to on entry.
unsafe extern "C" fn cursor_mouse_entered(receiver: Id, sel: Sel, _event: Id) {
    // SAFETY: a cursor of this crate's.
    if unsafe { (*(receiver as *const Cursor)).on_mouse_entered } != 0 {
        // SAFETY: `-set` is `void (id, SEL)`.
        unsafe { set(receiver, sel) };
    }
}

/// `-mouseExited:`: set this cursor, if it was told to on exit.
unsafe extern "C" fn cursor_mouse_exited(receiver: Id, sel: Sel, _event: Id) {
    // SAFETY: a cursor of this crate's.
    if unsafe { (*(receiver as *const Cursor)).on_mouse_exited } != 0 {
        // SAFETY: `-set` is `void (id, SEL)`.
        unsafe { set(receiver, sel) };
    }
}

/// `+pop`, which is the class spelling of the instance `-pop` below.
///
/// Neither pops: this runtime keeps no cursor stack, because the cursor
/// rectangles decide what is on screen on every mouse move and would put back
/// whatever a stack restored.
unsafe extern "C" fn class_pop(_class: Id, _sel: Sel) {
    apply();
}

unsafe extern "C" fn set_hidden_until_mouse_moves(class: Id, sel: Sel, hidden: u8) {
    match hidden != 0 {
        // SAFETY: both are `void (Class, SEL)`.
        true => unsafe { hide(class, sel) },
        false => unsafe { unhide(class, sel) },
    }
}

pub(crate) fn install() {
    let responder = objc::class::lookup(c"NSResponder").expect("declared before its methods");
    methods!(responder, {
        "cursorUpdate:" => responder_cursor_update, c"v24@0:8@16";
    });
    let window = objc::class::lookup(c"NSWindow").expect("declared before its methods");
    methods!(window, {
        "cursorUpdate:" => window_cursor_update, c"v24@0:8@16";
    });

    let area = objc::class::lookup(c"NSTrackingArea").expect("declared before its methods");
    methods!(area, {
        "initWithRect:options:owner:userInfo:" => init_tracking_area,
            c"@64@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16Q48@56@64";
        "rect" => tracking_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "options" => tracking_options, c"Q16@0:8";
        "owner" => tracking_owner, c"@16@0:8";
        "userInfo" => tracking_user_info, c"@16@0:8";
    });

    let cursor = objc::class::lookup(c"NSCursor").expect("declared before its methods");
    let meta = cursor.metaclass();
    for name in SHAPES {
        meta.replace_method(
            objc::sel::get(name),
            named_cursor as *const c_void,
            c"@16@0:8",
        );
    }
    class_methods!(cursor, {
        "currentCursor" => current_cursor, c"@16@0:8";
        "pop" => class_pop, c"v16@0:8";
        "hide" => hide, c"v16@0:8";
        "unhide" => unhide, c"v16@0:8";
        "setHiddenUntilMouseMoves:" => set_hidden_until_mouse_moves, c"v20@0:8c16";
    });
    methods!(cursor, {
        "initWithImage:hotSpot:" => init_with_image, c"@40@0:8@16{CGPoint=dd}24";
        "image" => image, c"@16@0:8";
        "hotSpot" => hot_spot, c"{CGPoint=dd}16@0:8";
        "set" => set, c"v16@0:8";
        "push" => set, c"v16@0:8";
        "pop" => set, c"v16@0:8";
        "setOnMouseEntered:" => set_on_mouse_entered, c"v20@0:8c16";
        "isSetOnMouseEntered" => is_set_on_mouse_entered, c"c16@0:8";
        "setOnMouseExited:" => set_on_mouse_exited, c"v20@0:8c16";
        "isSetOnMouseExited" => is_set_on_mouse_exited, c"c16@0:8";
        "mouseEntered:" => cursor_mouse_entered, c"v24@0:8@16";
        "mouseExited:" => cursor_mouse_exited, c"v24@0:8@16";
        "dealloc" => dealloc, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hide count is what makes a program that hides twice stay hidden.
    #[test]
    fn hiding_is_counted() {
        crate::bootstrap();
        let class = objc::class::ptr(objc::class::lookup(c"NSCursor").expect("NSCursor")) as Id;
        HIDDEN.store(0, std::sync::atomic::Ordering::Release);
        // SAFETY: all three are `void (Class, SEL)`.
        unsafe {
            hide(class, objc::sel::get("hide"));
            hide(class, objc::sel::get("hide"));
            unhide(class, objc::sel::get("unhide"));
        }
        assert_eq!(HIDDEN.load(std::sync::atomic::Ordering::Acquire), 1);
        unsafe { unhide(class, objc::sel::get("unhide")) };
        assert_eq!(HIDDEN.load(std::sync::atomic::Ordering::Acquire), 0);
        // And a stray unhide does not go negative.
        unsafe { unhide(class, objc::sel::get("unhide")) };
        assert_eq!(HIDDEN.load(std::sync::atomic::Ordering::Acquire), 0);

        // CoreGraphics and AppKit manipulate the same process cursor and the
        // C entry points return `kCGErrorSuccess`.
        assert_eq!(unsafe { CGDisplayHideCursor(0) }, 0);
        assert_eq!(unsafe { CGDisplayHideCursor(0) }, 0);
        assert!(!unsafe { CGCursorIsVisible() });
        assert_eq!(unsafe { CGDisplayShowCursor(0) }, 0);
        assert!(!unsafe { CGCursorIsVisible() });
        assert_eq!(unsafe { CGDisplayShowCursor(0) }, 0);
        assert!(unsafe { CGCursorIsVisible() });
    }

    /// A 2x image's point is in points and its pixels are not, so the two
    /// differ by the scale - and an image with no size is one to one.
    #[test]
    fn the_point_is_scaled_to_the_pixels() {
        let hot_spot = NSPoint { x: 4.0, y: 6.0 };
        let two_x = NSSize {
            width: 16.0,
            height: 16.0,
        };
        assert_eq!(hot_spot_pixels(hot_spot, two_x, 32, 32), (8, 12));
        assert_eq!(hot_spot_pixels(hot_spot, two_x, 16, 16), (4, 6));
        assert_eq!(hot_spot_pixels(hot_spot, NSSize::default(), 16, 16), (4, 6));
    }

    /// The rectangles a view installs belong to that view, and the cursor in
    /// one outlives the `-resetCursorRects` that handed it over: a program
    /// releases its own reference as soon as that method returns.
    #[test]
    fn a_view_owns_its_cursor_rectangles() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSCursor").expect("NSCursor");
        let cursor = objc::object::create_instance(class, 0);
        let view = objc::class::lookup(c"NSView").expect("NSView");
        let one = objc::object::create_instance(view, 0);
        let two = objc::object::create_instance(view, 0);

        let before = objc::arc::retain_count(cursor);
        add_rect(one, NSRect::default(), cursor);
        add_rect(two, NSRect::default(), cursor);
        assert_eq!(objc::arc::retain_count(cursor), before + 2);

        discard_rects(one);
        assert_eq!(objc::arc::retain_count(cursor), before + 1);
        assert!(
            RECTS
                .lock()
                .unwrap()
                .iter()
                .all(|entry| entry.view == two as usize),
            "only the discarded view's rectangles go"
        );

        discard_rects(two);
        assert_eq!(objc::arc::retain_count(cursor), before);
    }

    /// The active option decides when an area is live: a key-window area stops
    /// in a background window, and a drag holds an area back unless it asked
    /// to be told during one.
    #[test]
    fn an_area_is_active_by_its_options() {
        use tracking::*;
        assert!(!is_active(0, true, true));
        assert!(is_active(ACTIVE_ALWAYS, false, false));
        assert!(is_active(ACTIVE_IN_ACTIVE_APP, false, false));
        assert!(is_active(ACTIVE_IN_KEY_WINDOW, true, false));
        assert!(!is_active(ACTIVE_IN_KEY_WINDOW, false, false));
        assert!(is_active(ACTIVE_WHEN_FIRST_RESPONDER, true, true));
        assert!(!is_active(ACTIVE_WHEN_FIRST_RESPONDER, true, false));
        assert!(!is_active(ACTIVE_WHEN_FIRST_RESPONDER, false, true));
        assert!(held_by_drag(ACTIVE_ALWAYS, true));
        assert!(!held_by_drag(ACTIVE_ALWAYS, false));
        assert!(!held_by_drag(ENABLED_DURING_MOUSE_DRAG, true));
    }

    /// `NSTrackingAssumeInside` starts an area inside, so the first move that
    /// finds the pointer elsewhere sends the exit; without it, the enter.
    #[test]
    fn assume_inside_starts_an_area_inside() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSTrackingArea").expect("NSTrackingArea");
        let sel = objc::sel::get("initWithRect:options:owner:userInfo:");
        for (options, inside) in [(tracking::ASSUME_INSIDE, 1), (0, 0)] {
            let area = objc::object::create_instance(class, 0);
            // SAFETY: the prototype the method is installed with.
            unsafe {
                init_tracking_area(
                    area,
                    sel,
                    NSRect::default(),
                    options,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(area_of(area).map(|a| a.inside), Some(inside));
        }
        assert!(
            area_of(objc::object::create_instance(
                objc::class::lookup(c"NSView").expect("NSView"),
                0
            ))
            .is_none()
        );
    }

    /// A tracking-area event goes to the area's owner, whatever that is, and a
    /// cursor owning one sets itself on entry when it was told to.
    #[test]
    fn a_tracking_event_reaches_its_owner() {
        crate::bootstrap();
        let cursor =
            objc::object::create_instance(objc::class::lookup(c"NSCursor").expect("NSCursor"), 0);
        // SAFETY: a cursor made above.
        unsafe { set_on_mouse_entered(cursor, objc::sel::get("setOnMouseEntered:"), 1) };
        let area = objc::object::create_instance(
            objc::class::lookup(c"NSTrackingArea").expect("NSTrackingArea"),
            0,
        );
        // SAFETY: the prototype the method is installed with.
        unsafe {
            init_tracking_area(
                area,
                objc::sel::get("initWithRect:options:owner:userInfo:"),
                NSRect::default(),
                tracking::MOUSE_ENTERED_AND_EXITED | tracking::ACTIVE_ALWAYS,
                cursor,
                std::ptr::null_mut(),
            )
        };
        let event = crate::nsevent::make(crate::nsevent::Fields {
            kind: crate::nsevent::kind::MOUSE_EXITED,
            tracking_area: area,
            ..Default::default()
        });
        CURRENT.store(0, std::sync::atomic::Ordering::Release);
        crate::event::route(std::ptr::null_mut(), event);
        assert_eq!(
            CURRENT.load(std::sync::atomic::Ordering::Acquire),
            0,
            "not set on exit"
        );
        let event = crate::nsevent::make(crate::nsevent::Fields {
            kind: crate::nsevent::kind::MOUSE_ENTERED,
            tracking_area: area,
            ..Default::default()
        });
        crate::event::route(std::ptr::null_mut(), event);
        assert_eq!(
            CURRENT.load(std::sync::atomic::Ordering::Acquire),
            cursor as usize
        );
        CURRENT.store(0, std::sync::atomic::Ordering::Release);
    }

    /// `-cursorUpdate:` is inherited by every responder, and the window's own
    /// is what puts the arrow back.
    #[test]
    fn every_responder_answers_cursor_update() {
        crate::bootstrap();
        let sel = objc::sel::get("cursorUpdate:");
        for name in [c"NSApplication", c"NSWindow", c"NSView"] {
            let class = objc::class::lookup(name).expect("a responder subclass");
            assert!(class.method(sel).is_some(), "{name:?}");
        }
        let window = objc::class::lookup(c"NSWindow").expect("NSWindow");
        assert_eq!(
            window.method(sel).map(|m| m.implementation()),
            Some(window_cursor_update as *const c_void)
        );
    }

    /// Each named cursor is its own object, which is what a program comparing
    /// against `+currentCursor` depends on.
    #[test]
    fn each_named_cursor_is_one_object() {
        crate::bootstrap();
        let class = objc::class::ptr(objc::class::lookup(c"NSCursor").expect("NSCursor")) as Id;
        // SAFETY: `+namedCursor` is `id (Class, SEL)`.
        let arrow = unsafe { named_cursor(class, objc::sel::get("arrowCursor")) };
        let again = unsafe { named_cursor(class, objc::sel::get("arrowCursor")) };
        let beam = unsafe { named_cursor(class, objc::sel::get("IBeamCursor")) };
        assert_eq!(arrow, again);
        assert_ne!(arrow, beam);
    }
}
