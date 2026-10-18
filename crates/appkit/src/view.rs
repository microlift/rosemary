use std::ffi::c_void;

use foundation::geometry::{CGFloat, NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

#[repr(C)]
pub struct View {
    isa: *const c_void,
    appearance: Id,
    /// Unretained, as AppKit's is: a view's window owns the view.
    window: Id,
    superview: Id,
    subviews: Id,
    tracking_areas: Id,
    /// Retained, as AppKit's is: a layer outlives the call that set it.
    layer: Id,
    /// `-setAllowedTouchTypes:`, stored so `-allowedTouchTypes` reads back what
    /// was set. Nothing in this tree delivers a touch event, so the mask only
    /// ever says which ones the view *would* accept.
    allowed_touch_types: usize,
    frame: NSRect,
    wants_best_resolution: usize,
    /// `-setPostsFrameChangedNotifications:` and its bounds twin, as the two
    /// low bits. A view posts nothing until it is asked to, which is AppKit's
    /// default for a view built with `-initWithFrame:`.
    posts: usize,
    /// `NSAutoresizingMaskOptions`, which says which of the six lengths around
    /// and inside this view give when its superview resizes. See
    /// [`resize_subviews`].
    autoresizing_mask: usize,
    /// `-setAutoresizesSubviews:`, which is `YES` on a fresh view - so the
    /// field is stored inverted, and zero means the AppKit default.
    does_not_autoresize_subviews: usize,
    /// `-setHidden:`.
    hidden: usize,
    /// `-setAlphaValue:`, as the bits of `1 - alpha` so that zero is AppKit's
    /// fully opaque default.
    transparency: u64,
    /// `-setTranslatesAutoresizingMaskIntoConstraints:`, inverted like
    /// `does_not_autoresize_subviews` so that zero is AppKit's `YES`.
    constrained: usize,
    /// `-setNextResponder:`, unretained as AppKit's is; null means the
    /// superview, which is a view's next responder until something is put
    /// between them.
    next_responder: Id,
    /// `-setWantsLayer:` and whether the layer is one AppKit made for it, as
    /// [`WANTS_LAYER`] and [`BACKING_LAYER`], and `-setLayerContentsPlacement:`
    /// from [`PLACEMENT_SHIFT`] up.
    layer_backing: usize,
}

/// The bits of [`View::layer_backing`]: the view asked for a layer, and the
/// layer it has is the one `-makeBackingLayer` made rather than one a program
/// set with `-setLayer:`.
const WANTS_LAYER: usize = 1;
const BACKING_LAYER: usize = 2;
const CLIPS_TO_BOUNDS: usize = 4;
const PLACEMENT_SHIFT: u32 = 8;

/// The bits of [`View::posts`].
const POSTS_FRAME: usize = 1;
const POSTS_BOUNDS: usize = 2;

/// `NSAutoresizingMaskOptions`.
mod autoresizing {
    pub const MIN_X_MARGIN: usize = 1;
    pub const WIDTH_SIZABLE: usize = 2;
    pub const MAX_X_MARGIN: usize = 4;
    pub const MIN_Y_MARGIN: usize = 8;
    pub const HEIGHT_SIZABLE: usize = 16;
    pub const MAX_Y_MARGIN: usize = 32;
}

/// The notification a view posts when its frame changes.
pub const FRAME_DID_CHANGE: &str = "NSViewFrameDidChangeNotification";
/// ...and when its bounds do.
pub const BOUNDS_DID_CHANGE: &str = "NSViewBoundsDidChangeNotification";

/// Post the frame and bounds notifications a resized view owes its observers.
///
/// Both, because this tree's `-bounds` is the frame's size at the origin: a
/// change to one *is* a change to the other, and a guest watching only the
/// bounds - which is what `-setPostsBoundsChangedNotifications:` asks for -
/// would otherwise never hear that the view resized.
fn note_geometry_change(receiver: Id, before: NSRect) {
    let view = view_of(receiver);
    if view.frame == before {
        return;
    }
    if view.frame.size != before.size {
        resize_subviews(receiver, before.size);
    }
    // AppKit keeps a view's layer the size of its bounds.
    let layer = view_of(receiver).layer;
    if !layer.is_null() {
        crate::layer::set_bounds_of(layer, bounds_of(receiver));
    }
    crate::layout::solve();
    let view = view_of(receiver);
    let posts = view.posts;
    if posts & POSTS_FRAME != 0 {
        foundation::notification::post(FRAME_DID_CHANGE, receiver, std::ptr::null_mut());
    }
    if posts & POSTS_BOUNDS != 0 && view.frame.size != before.size {
        foundation::notification::post(BOUNDS_DID_CHANGE, receiver, std::ptr::null_mut());
    }
    // Moving or resizing the focused editor also moves the host IME UI.
    // SAFETY: `receiver` is the live view whose geometry changed.
    unsafe { crate::window::refresh_text_input_area_for(receiver) };
}

/// Resize `receiver`'s subviews for a superview that went from `old` to its
/// current size, as `-resizeSubviewsWithOldSize:` does.
///
/// This is what makes a window resize reach the view a program actually draws
/// into. AppKit resizes subviews on *every* frame change of their superview,
/// and a program's GL view is a subview of the window's content view - so
/// without this the content view follows the window and the view underneath it
/// keeps its original frame forever. The program then asks its own view how big
/// it is, gets the size the window had when it opened, and renders that size
/// for the rest of the run: a window that resizes and a picture that does not.
///
/// Each subview is *sent* `-resizeWithOldSuperviewSize:` rather than having its
/// frame written here, because that is the method AppKit documents as the one a
/// view overrides to lay itself out - so a guest's override has to be what runs.
fn resize_subviews(receiver: Id, old: NSSize) {
    let view = view_of(receiver);
    if view.does_not_autoresize_subviews != 0 || view.subviews.is_null() {
        return;
    }
    let list = view.subviews;
    // SAFETY: an array this module made and only ever fills with views.
    let Some(array) = (unsafe { corefoundation::collections::array(list as *const c_void) }) else {
        return;
    };
    let subviews: Vec<Id> = array.values.iter().map(|v| *v as Id).collect();
    let sel = objc::sel::get("resizeWithOldSuperviewSize:");
    for subview in subviews {
        if !is_view(subview) {
            continue;
        }
        // SAFETY: `-resizeWithOldSuperviewSize:` is `void (id, SEL, NSSize)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSSize) = std::mem::transmute(objc::msg::entry());
            f(subview, sel, old);
        }
    }
}

/// `-resizeWithOldSuperviewSize:`: put this view where its autoresizing mask
/// says it belongs, now that its superview has gone from `old` to its current
/// size.
///
/// A view whose mask is `NSViewNotSizable` keeps its frame, which is a real
/// answer and not a gap: a program that positions a fixed-size control in a
/// resizing window means it.
unsafe extern "C" fn resize_with_old_superview_size(receiver: Id, _sel: Sel, old: NSSize) {
    let view = view_of(receiver);
    let superview = view.superview;
    // A view placed by constraints is not also placed by its mask.
    if !is_view(superview) || view.constrained != 0 {
        return;
    }
    let new = view_of(superview).frame.size;
    let mask = view.autoresizing_mask;
    let before = view.frame;
    let (x, width) = distribute(
        mask & autoresizing::MIN_X_MARGIN != 0,
        mask & autoresizing::WIDTH_SIZABLE != 0,
        mask & autoresizing::MAX_X_MARGIN != 0,
        before.origin.x,
        before.size.width,
        old.width,
        new.width,
    );
    let (y, height) = distribute(
        mask & autoresizing::MIN_Y_MARGIN != 0,
        mask & autoresizing::HEIGHT_SIZABLE != 0,
        mask & autoresizing::MAX_Y_MARGIN != 0,
        before.origin.y,
        before.size.height,
        old.height,
        new.height,
    );
    let frame = NSRect {
        origin: NSPoint { x, y },
        size: NSSize { width, height },
    };
    if frame == before {
        return;
    }
    // Sent rather than written, for the reason `-[NSWindow setContentView:]`
    // sends it: a view that tracks its own size overrides `-setFrame:`, and
    // this path also has to reach the subviews and observers of *this* view.
    // SAFETY: `-setFrame:` is `void (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("setFrame:"), frame);
    }
}

/// Spread a superview's change along one axis over the three lengths that make
/// it up: the leading margin, the subview itself, and the trailing margin.
///
/// Only the flexible ones give, and they give in proportion to their current
/// sizes - which is AppKit's rule and the one that matters, because a view that
/// is `WidthSizable` with two fixed margins must absorb the whole change. When
/// every flexible length is currently zero the change is split evenly between
/// them instead, since proportional shares of nothing are all nothing and the
/// view would not move at all.
///
/// Nothing flexible means nothing moves: `NSViewNotSizable` is a real answer
/// and a view that asked for it keeps its frame.
#[allow(clippy::too_many_arguments)]
fn distribute(
    leading: bool,
    sizable: bool,
    trailing: bool,
    origin: CGFloat,
    size: CGFloat,
    old: CGFloat,
    new: CGFloat,
) -> (CGFloat, CGFloat) {
    let delta = new - old;
    if delta == 0.0 {
        return (origin, size);
    }
    let trailing_margin = old - origin - size;
    let parts = [
        (leading, origin),
        (sizable, size),
        (trailing, trailing_margin),
    ];
    let flexible: Vec<CGFloat> = parts
        .iter()
        .filter(|(is, _)| *is)
        .map(|(_, length)| *length)
        .collect();
    if flexible.is_empty() {
        return (origin, size);
    }
    let total: CGFloat = flexible.iter().sum();
    let share = |length: CGFloat| match total > 0.0 {
        true => delta * (length / total),
        false => delta / flexible.len() as CGFloat,
    };
    let origin = match leading {
        true => origin + share(origin),
        false => origin,
    };
    let size = match sizable {
        true => (size + share(size)).max(0.0),
        false => size,
    };
    (origin, size)
}

unsafe extern "C" fn set_autoresizing_mask(receiver: Id, _sel: Sel, mask: usize) {
    view_of(receiver).autoresizing_mask = mask;
}

unsafe extern "C" fn autoresizing_mask(receiver: Id, _sel: Sel) -> usize {
    view_of(receiver).autoresizing_mask
}

unsafe extern "C" fn set_autoresizes_subviews(receiver: Id, _sel: Sel, flag: u8) {
    view_of(receiver).does_not_autoresize_subviews = usize::from(flag == 0);
}

unsafe extern "C" fn autoresizes_subviews(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).does_not_autoresize_subviews == 0)
}

/// `-resizeSubviewsWithOldSize:`, which a program may call itself and which an
/// `NSView` subclass overriding `-resizeWithOldSuperviewSize:` expects to be
/// the thing that drives it.
unsafe extern "C" fn resize_subviews_with_old_size(receiver: Id, _sel: Sel, old: NSSize) {
    resize_subviews(receiver, old);
}

unsafe extern "C" fn set_posts_frame_changed(receiver: Id, _sel: Sel, flag: u8) {
    set_posts(receiver, POSTS_FRAME, flag);
}

unsafe extern "C" fn set_posts_bounds_changed(receiver: Id, _sel: Sel, flag: u8) {
    set_posts(receiver, POSTS_BOUNDS, flag);
}

fn set_posts(receiver: Id, bit: usize, flag: u8) {
    let view = view_of(receiver);
    view.posts = match flag != 0 {
        true => view.posts | bit,
        false => view.posts & !bit,
    };
}

unsafe extern "C" fn posts_frame_changed(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).posts & POSTS_FRAME != 0)
}

unsafe extern "C" fn posts_bounds_changed(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).posts & POSTS_BOUNDS != 0)
}

/// `-setLayer:`, which stores the layer and points it back at this view.
///
/// The view does not draw the layer. A guest that hands its view a
/// `CAMetalLayer` draws through the layer's own drawables, which are Metal's
/// business rather than the view's, and what the view owes it is to give the
/// same object back from `-layer`, because that is where the guest reaches
/// for it on every frame.
///
/// The back pointer is what makes those drawables presentable. A drawable
/// knows its layer and nothing else, and the window it belongs in is up the
/// view tree from here; see [`presentation_frame`].
unsafe extern "C" fn set_layer(receiver: Id, _sel: Sel, layer: Id) {
    let view = view_of(receiver);
    if view.layer == layer {
        return;
    }
    let previous = view.layer;
    view.layer = objc::arc::retain(layer);
    // Whatever replaced AppKit's layer is the program's; `-setWantsLayer:`
    // marks its own again once this returns.
    view.layer_backing &= !BACKING_LAYER;
    // SAFETY: a layer this runtime made, and a view which outlives it - the
    // view holds the only reference the layer is reached through.
    unsafe {
        crate::layer::set_view(previous, std::ptr::null_mut());
        crate::layer::set_view(layer, receiver);
    }
    if view.layer_backing & CLIPS_TO_BOUNDS != 0 && !layer.is_null() {
        crate::layer::set_masks_to_bounds_of(layer, true);
    }
    objc::arc::release(previous);
}

/// `-setWantsLayer:`. Asking for a layer makes the view layer-backed: AppKit
/// sends the view `-makeBackingLayer` and then `-setLayer:` with what it
/// answered, so a subclass that overrides either - Qt's `QNSView` overrides
/// both - is the one that decides. A view that already has a layer, one a
/// program set, keeps it. Giving the wish up takes away a layer AppKit made
/// and leaves one a program set.
unsafe extern "C" fn set_wants_layer(receiver: Id, _sel: Sel, wants: u8) {
    let view = view_of(receiver);
    if wants == 0 {
        let made = view.layer_backing & BACKING_LAYER != 0;
        view.layer_backing &= !(WANTS_LAYER | BACKING_LAYER);
        if made {
            send_object(receiver, "setLayer:", std::ptr::null_mut());
        }
        return;
    }
    view.layer_backing |= WANTS_LAYER;
    if !view.layer.is_null() {
        return;
    }
    // SAFETY: `-makeBackingLayer` is `CALayer *(id, SEL)`, answered at +0.
    let layer = unsafe {
        let make: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        make(receiver, objc::sel::get("makeBackingLayer"))
    };
    if layer.is_null() {
        return;
    }
    crate::layer::set_bounds_of(layer, bounds_of(receiver));
    // A backing layer's delegate is its view, which is what routes the
    // layer's `-display` to the view.
    if crate::layer::delegate_of(layer).is_null() {
        crate::layer::set_delegate_of(layer, receiver);
    }
    crate::layer::set_contents_gravity_for_placement(layer, contents_placement(receiver));
    send_object(receiver, "setLayer:", layer);
    // `-setLayer:` may be overridden to refuse; the flag describes the layer
    // the view ended up with.
    let view = view_of(receiver);
    if view.layer == layer {
        view.layer_backing |= BACKING_LAYER;
    }
}

fn send_object(receiver: Id, name: &str, object: Id) {
    // SAFETY: a `void (id, SEL, id)` method on a live view.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(name), object);
    }
}

/// The view's `-layerContentsPlacement`, sent so an override answers.
fn contents_placement(view: Id) -> isize {
    // SAFETY: `-layerContentsPlacement` is `NSInteger (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
        f(view, objc::sel::get("layerContentsPlacement"))
    }
}

/// `-makeBackingLayer`: a plain `CALayer`, autoreleased.
unsafe extern "C" fn make_backing_layer(_receiver: Id, _sel: Sel) -> Id {
    let Some(class) = objc::class::lookup(c"CALayer") else {
        return std::ptr::null_mut();
    };
    // SAFETY: `+layer` is `id (Class, SEL)`.
    unsafe {
        let make: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        make(objc::class::ptr(class) as Id, objc::sel::get("layer"))
    }
}

unsafe extern "C" fn wants_layer(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).layer_backing & WANTS_LAYER != 0)
}

/// `-layerContentsPlacement`, whose default of zero is
/// `NSViewLayerContentsPlacementScaleAxesIndependently`.
unsafe extern "C" fn layer_contents_placement(receiver: Id, _sel: Sel) -> isize {
    (view_of(receiver).layer_backing >> PLACEMENT_SHIFT) as isize
}

/// `-setLayerContentsPlacement:`, which AppKit applies to a backing layer's
/// `contentsGravity`.
unsafe extern "C" fn set_layer_contents_placement(receiver: Id, _sel: Sel, placement: isize) {
    let view = view_of(receiver);
    let flags = view.layer_backing & ((1 << PLACEMENT_SHIFT) - 1);
    view.layer_backing = flags | ((placement.clamp(0, 11) as usize) << PLACEMENT_SHIFT);
    if view.layer_backing & BACKING_LAYER != 0 {
        crate::layer::set_contents_gravity_for_placement(view.layer, placement);
    }
}

/// `-[NSView setClipsToBounds:]`.
///
/// Sets whether the view confines its drawing and that of its subviews to its
/// bounds. If the view is layer-backed, setting this sets the backing layer's
/// `masksToBounds`.
unsafe extern "C" fn set_clips_to_bounds(receiver: Id, _sel: Sel, clips: u8) {
    let view = view_of(receiver);
    if clips != 0 {
        view.layer_backing |= CLIPS_TO_BOUNDS;
    } else {
        view.layer_backing &= !CLIPS_TO_BOUNDS;
    }
    let layer = view.layer;
    if !layer.is_null() {
        crate::layer::set_masks_to_bounds_of(layer, clips != 0);
    }
}

/// `-[NSView clipsToBounds]`.
unsafe extern "C" fn clips_to_bounds(receiver: Id, _sel: Sel) -> u8 {
    let view = view_of(receiver);
    u8::from(view.layer_backing & CLIPS_TO_BOUNDS != 0)
}

/// Whether `view` is layer-backed: it asked for a layer and has one.
pub(crate) fn is_layer_backed(view: Id) -> bool {
    is_view(view)
        && view_of(view).layer_backing & WANTS_LAYER != 0
        && !view_of(view).layer.is_null()
}

unsafe extern "C" fn layer(receiver: Id, _sel: Sel) -> Id {
    view_of(receiver).layer
}

/// The layer `-setLayer:` gave a view, or null.
pub(crate) fn layer_of(view: Id) -> Id {
    match is_view(view) {
        true => view_of(view).layer,
        false => std::ptr::null_mut(),
    }
}

/// Whether `-setHidden:` hid this view itself, ancestors aside.
pub(crate) fn is_hidden_itself(view: Id) -> bool {
    is_view(view) && view_of(view).hidden != 0
}

/// `-alphaValue` of `view` alone, ancestors aside.
pub(crate) fn alpha_of(view: Id) -> f64 {
    match is_view(view) {
        true => 1.0 - f64::from_bits(view_of(view).transparency),
        false => 1.0,
    }
}

/// The opacity `view` is composited with: its own `-alphaValue` times every
/// ancestor's, since AppKit fades a view's whole subtree with it.
pub(crate) fn effective_alpha(view: Id) -> f64 {
    let mut alpha = 1.0;
    let mut current = view;
    for _ in 0..64 {
        if !is_view(current) {
            break;
        }
        alpha *= alpha_of(current);
        current = view_of(current).superview;
    }
    alpha
}

unsafe extern "C" fn set_alpha_value(receiver: Id, _sel: Sel, alpha: CGFloat) {
    // NaN clamps to opaque rather than poisoning every blend below it.
    let alpha = match alpha.is_nan() {
        true => 1.0,
        false => alpha.clamp(0.0, 1.0),
    };
    let before = alpha_of(receiver);
    view_of(receiver).transparency = (1.0 - alpha).to_bits();
    if before != alpha {
        crate::backing::needs_display(receiver);
    }
}

unsafe extern "C" fn alpha_value(receiver: Id, _sel: Sel) -> CGFloat {
    alpha_of(receiver)
}

unsafe extern "C" fn set_allowed_touch_types(receiver: Id, _sel: Sel, mask: usize) {
    view_of(receiver).allowed_touch_types = mask;
}

unsafe extern "C" fn allowed_touch_types(receiver: Id, _sel: Sel) -> usize {
    view_of(receiver).allowed_touch_types
}

pub const INSTANCE_SIZE: usize = size_of::<View>();

fn view_of<'a>(receiver: Id) -> &'a mut View {
    // SAFETY: a view of this crate's, which is what every method here is sent to.
    unsafe { &mut *(receiver as *mut View) }
}

pub(crate) fn is_view(object: Id) -> bool {
    // SAFETY: null or an object this runtime allocated, which is what every
    // caller has.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSView").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

/// Tell a view which window it is in, from `-[NSWindow setContentView:]`.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn set_window(view: Id, window: Id) {
    if !is_view(view) {
        return;
    }
    view_of(view).window = window;
}

/// Forget `window` as the window `view` is the content view of, if it still is.
///
/// # Safety
///
/// As [`set_window`].
pub unsafe fn detach_window(view: Id, window: Id) {
    if is_view(view) && view_of(view).window == window {
        view_of(view).window = std::ptr::null_mut();
    }
}

/// The window a view is in, following the superview chain.
///
/// # Safety
///
/// As [`set_window`].
pub unsafe fn window_of(view: Id) -> Id {
    let mut current = view;
    // Bounded rather than a bare loop: a subview list a guest built with a cycle
    // in it would otherwise hang inside a method that looks like an accessor.
    for _ in 0..64 {
        if !is_view(current) {
            return std::ptr::null_mut();
        }
        let v = view_of(current);
        if !v.window.is_null() {
            return v.window;
        }
        if v.superview.is_null() {
            return std::ptr::null_mut();
        }
        current = v.superview;
    }
    std::ptr::null_mut()
}

/// One line per view, describing the tree under `view`.
///
/// The question this exists for is "what size does the *guest's* view think it
/// is", and it cannot be answered any other way: a release build inlines the
/// whole geometry path out of gdb's reach - `appkit::event::post` and
/// `foundation::notification::post` both resolve to "Function not defined" -
/// and the view that matters is one the guest made, so it has no symbol either.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn describe(view: Id, depth: usize, out: &mut String) {
    use std::fmt::Write;

    if !is_view(view) || depth > 16 {
        return;
    }
    let v = view_of(view);
    // The class is what names it: `NSView` is this crate's, and anything else is
    // the guest's own subclass, which is the one the question is about.
    // SAFETY: a live object.
    let class = unsafe { objc::object::class_of(view) }.map_or("?", objc::class::Class::name_str);
    let _ = writeln!(
        out,
        "rosemary: appkit: {:indent$}{class} {view:p} frame {}x{} at ({},{}) mask {:#x}{}",
        "",
        v.frame.size.width,
        v.frame.size.height,
        v.frame.origin.x,
        v.frame.origin.y,
        v.autoresizing_mask,
        match v.does_not_autoresize_subviews {
            0 => "",
            _ => " autoresizesSubviews=NO",
        },
        indent = depth * 2,
    );
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let children = unsafe { corefoundation::collections::array(v.subviews as *const c_void) };
    for child in children.map(|a| a.values.clone()).unwrap_or_default() {
        // SAFETY: an object this runtime allocated.
        unsafe { describe(child as Id, depth + 1, out) };
    }
}

/// The deepest view under a point, as `-[NSView hitTest:]` finds it.
///
/// `point` is in the *superview's* coordinates, which is the argument AppKit's
/// takes. Subviews are tried last-first, because the last one added is the one
/// drawn on top.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn hit_test(view: Id, point: NSPoint) -> Id {
    unsafe { hit(view, point, 0) }
}

/// Where the pointer is in this view's own coordinates, or `None` for a view
/// in no window - which is every view before it is installed, and the point
/// where a cursor rectangle cannot be about the pointer at all.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn pointer_in(view: Id) -> Option<NSPoint> {
    if !is_view(view) {
        return None;
    }
    let window = unsafe { window_of(view) };
    if window.is_null() {
        return None;
    }
    Some(point_from_window(
        view,
        crate::window::pointer_location(window),
    ))
}

/// `-isFlipped`, sent so that a subclass's override answers: Qt's views are
/// all flipped, and nothing about one says so except that method.
pub fn is_flipped(view: Id) -> bool {
    if !is_view(view) {
        return false;
    }
    // SAFETY: a live view; the selector is `BOOL (id, SEL)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        send(view, objc::sel::get("isFlipped")) != 0
    }
}

/// The vertical coordinate `y` of a view's own space as its superview's.
///
/// Both ends can be flipped: `flipped` is the view's, `outer_flipped` the
/// superview's, and a view in no superview is in its window, which is not.
fn y_to_superview(frame: NSRect, flipped: bool, outer_flipped: bool, y: f64) -> f64 {
    let from_top = match flipped {
        true => y,
        false => frame.size.height - y,
    };
    match outer_flipped {
        true => frame.origin.y + from_top,
        false => frame.origin.y + frame.size.height - from_top,
    }
}

/// The inverse of [`y_to_superview`].
fn y_from_superview(frame: NSRect, flipped: bool, outer_flipped: bool, y: f64) -> f64 {
    let from_top = match outer_flipped {
        true => y - frame.origin.y,
        false => frame.origin.y + frame.size.height - y,
    };
    match flipped {
        true => from_top,
        false => frame.size.height - from_top,
    }
}

/// A point in `view`'s coordinates, in its superview's.
fn to_superview(view: Id, point: NSPoint) -> NSPoint {
    let v = view_of(view);
    NSPoint {
        x: point.x + v.frame.origin.x,
        y: y_to_superview(v.frame, is_flipped(view), is_flipped(v.superview), point.y),
    }
}

/// A point in `view`'s superview's coordinates, in the view's own.
fn from_superview(view: Id, point: NSPoint) -> NSPoint {
    let v = view_of(view);
    NSPoint {
        x: point.x - v.frame.origin.x,
        y: y_from_superview(v.frame, is_flipped(view), is_flipped(v.superview), point.y),
    }
}

/// A point in `view`'s coordinates, in its window's base coordinates.
///
/// Every `-isFlipped` on the way up applies, which is what puts a flipped
/// superview's subview at `y = 0` along its top edge. Null is the window.
pub fn point_to_window(view: Id, point: NSPoint) -> NSPoint {
    let mut point = point;
    let mut node = view;
    // Bounded for the reason [`window_of`] is.
    for _ in 0..64 {
        if !is_view(node) {
            break;
        }
        point = to_superview(node, point);
        node = view_of(node).superview;
    }
    point
}

/// The inverse of [`point_to_window`].
pub fn point_from_window(view: Id, point: NSPoint) -> NSPoint {
    let mut chain = Vec::new();
    let mut node = view;
    for _ in 0..64 {
        if !is_view(node) {
            break;
        }
        chain.push(node);
        node = view_of(node).superview;
    }
    chain
        .into_iter()
        .rev()
        .fold(point, |point, node| from_superview(node, point))
}

/// A rectangle from one coordinate space to another by its corners, which
/// swap vertically when exactly one of the two spaces is flipped.
fn rect_between(rect: NSRect, map: impl Fn(NSPoint) -> NSPoint) -> NSRect {
    let a = map(rect.origin);
    let b = map(NSPoint {
        x: rect.origin.x + rect.size.width,
        y: rect.origin.y + rect.size.height,
    });
    NSRect {
        origin: NSPoint {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
        },
        size: NSSize {
            width: (b.x - a.x).abs(),
            height: (b.y - a.y).abs(),
        },
    }
}

/// A view's bounds in its window's base coordinates.
pub fn rect_in_window(view: Id) -> NSRect {
    let size = match is_view(view) {
        true => view_of(view).frame.size,
        false => NSSize::default(),
    };
    rect_between(
        NSRect {
            origin: NSPoint::default(),
            size,
        },
        |point| point_to_window(view, point),
    )
}

/// Whether a rectangle of a view's holds a point, for [`crate::cursor`].
pub fn holds(view: Id, rect: NSRect, point: NSPoint) -> bool {
    is_view(view) && contains(rect, point)
}

/// Where a view's layer lands in its window, in pixels from the top left.
///
/// This is the whole of the mapping from AppKit's view tree to a window a
/// compositor knows about. AppKit gives every view a frame in its
/// superview's coordinates, with the origin at the bottom left; a window
/// system wants one rectangle in the window's own pixels, counted from the
/// top left. So the origins are summed up the superview chain, the result is
/// flipped against the window's height, and both are scaled by the window's
/// pixel density - which is what separates a view's size in points from the
/// number of pixels a drawable for it has to cover.
///
/// `None` where the view is not in a window this runtime made, which is the
/// only answer presentation can act on: there is nothing to present to.
///
/// # Safety
///
/// `view` is null or a view of this crate's.
pub unsafe fn presentation_frame(view: Id) -> Option<Frame> {
    if !is_view(view) {
        return None;
    }
    // SAFETY: the caller's obligation, checked above.
    let window = unsafe { window_of(view) };
    // SAFETY: null or a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    if sdl.is_null() {
        return None;
    }
    let rect = rect_in_window(view);
    let (x, y, size) = (rect.origin.x, rect.origin.y, rect.size);
    let (_, window_height) = sdl3::video::window_size(sdl)?;
    let (pixel_width, pixel_height) = sdl3::video::window_size_in_pixels(sdl)?;
    let density = sdl3::video::window_pixel_density(sdl) as f64;
    let density = if density > 0.0 { density } else { 1.0 };
    // AppKit counts up from the bottom of the window's content rect; a
    // swapchain image counts down from the top of it.
    let top = f64::from(window_height) - (y + size.height);
    let frame = Frame {
        window: sdl,
        x: (x * density).round() as i32,
        y: (top * density).round() as i32,
        width: (size.width * density).round().max(0.0) as u32,
        height: (size.height * density).round().max(0.0) as u32,
        window_width: pixel_width.max(0) as u32,
        window_height: pixel_height.max(0) as u32,
    };
    // A view with no frame of its own - one a guest made with
    // `-initWithFrame:` of a zero rect and then never sized - covers its
    // window's content rather than nothing, which is what AppKit's own
    // autoresizing would have made of it.
    match frame.width == 0 || frame.height == 0 {
        true => {
            let inset = (crate::window::content_inset(window) * density).round() as u32;
            let inset = inset.min(frame.window_height);
            Some(Frame {
                x: 0,
                y: inset as i32,
                width: frame.window_width,
                height: frame.window_height - inset,
                ..frame
            })
        }
        false => Some(frame),
    }
}

/// The view's rectangle in SDL's logical window coordinates.
///
/// This is deliberately not [`presentation_frame`]: SDL text-input areas are
/// measured in window coordinates, while presentation uses drawable pixels.
/// AppKit and SDL also put their origins at opposite vertical edges.
///
/// # Safety
///
/// `view` is null or a view of this crate's.
pub unsafe fn input_frame(view: Id) -> Option<(sdl3::Window, sdl3::Rect)> {
    if !is_view(view) {
        return None;
    }
    // SAFETY: the caller's obligation, checked above.
    let window = unsafe { window_of(view) };
    // SAFETY: null or a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    if sdl.is_null() {
        return None;
    }
    let rect = rect_in_window(view);
    let (x, y, size) = (rect.origin.x, rect.origin.y, rect.size);
    let (_, window_height) = sdl3::video::window_size(sdl)?;
    let top = f64::from(window_height) - (y + size.height);
    Some((
        sdl,
        sdl3::Rect {
            x: x.round() as i32,
            y: top.round() as i32,
            w: size.width.round().max(0.0) as i32,
            h: size.height.round().max(0.0) as i32,
        },
    ))
}

/// A view's layer as a window system sees it: a rectangle of pixels, and the
/// window it is a rectangle of.
pub struct Frame {
    pub window: sdl3::Window,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub window_width: u32,
    pub window_height: u32,
}

impl Frame {
    /// Whether this frame is the whole window, so that whatever is drawn for
    /// it needs nothing composited under or around it.
    pub fn covers_window(&self) -> bool {
        self.x <= 0
            && self.y <= 0
            && self.width >= self.window_width
            && self.height >= self.window_height
    }
}

/// The subviews of a view, for a caller walking the tree outside this module.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn children(view: Id) -> Vec<Id> {
    if !is_view(view) {
        return Vec::new();
    }
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let list =
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) };
    list.map(|a| a.values.iter().map(|v| *v as Id).collect())
        .unwrap_or_default()
}

unsafe fn hit(view: Id, point: NSPoint, depth: usize) -> Id {
    if !is_view(view) || depth > 32 {
        return std::ptr::null_mut();
    }
    // A guest's own `-hitTest:` is the answer where it has one: a view that
    // reshapes its hit region - a round button, a transparent overlay - is
    // saying so in code this crate cannot see from the frame.
    let sel = objc::sel::get("hitTest:");
    // SAFETY: a live object this runtime allocated.
    if let Some(method) = unsafe { objc::object::class_of(view) }
        .and_then(|c| c.method(sel))
        .filter(|method| method.implementation() != hit_test_method as *const c_void)
    {
        // SAFETY: `-hitTest:` is `id (id, SEL, NSPoint)`.
        return unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSPoint) -> Id =
                std::mem::transmute(method.implementation());
            f(view, sel, point)
        };
    }
    // SAFETY: validated above.
    unsafe { hit_base(view, point, depth) }
}

/// The inherited `NSView` hit-test behavior, without redispatching an override
/// on `view`. This distinction matters when that override calls `super`.
unsafe fn hit_base(view: Id, point: NSPoint, depth: usize) -> Id {
    let frame = view_of(view).frame;
    if !contains(frame, point) {
        return std::ptr::null_mut();
    }
    let local = from_superview(view, point);
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let children =
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) };
    for child in children
        .map(|a| a.values.clone())
        .unwrap_or_default()
        .into_iter()
        .rev()
    {
        // SAFETY: an object this runtime allocated.
        let hit = unsafe { hit(child as Id, local, depth + 1) };
        if !hit.is_null() {
            return hit;
        }
    }
    view
}

/// `-[NSView hitTest:]`; subclasses with their own method are dispatched by
/// [`hit`] before this base implementation is considered.
unsafe extern "C" fn hit_test_method(receiver: Id, _sel: Sel, point: NSPoint) -> Id {
    // SAFETY: the receiver is a live view and the point is in its superview's
    // coordinates, as expected by this module's tree walk.
    unsafe { hit_base(receiver, point, 0) }
}

/// Whether a rectangle holds a point, on AppKit's half-open rule: the minimum
/// edges are inside and the maximum ones are not, so two views that share an
/// edge do not both claim the click.
fn contains(rect: NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x
        && point.y >= rect.origin.y
        && point.x < rect.origin.x + rect.size.width
        && point.y < rect.origin.y + rect.size.height
}

/// Send `-updateTrackingAreas` down a view tree.
///
/// AppKit sends it when a view is added to a window and whenever the geometry
/// changes, and it is the *only* time a program is asked to install its
/// tracking areas: a view's `-updateTrackingAreas` is where an application
/// removes the old area and adds one for its new bounds. A runtime that never
/// sends it has a program that never adds one - so `-mouseMoved:`,
/// `-mouseEntered:` and `-mouseExited:` are all events nothing ever asked for,
/// and the pointer appears to a game to be standing still.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn update_tracking_areas(view: Id) {
    if !is_view(view) {
        return;
    }
    let sel = objc::sel::get("updateTrackingAreas");
    // SAFETY: a live object this runtime allocated.
    if let Some(method) = unsafe { objc::object::class_of(view) }.and_then(|c| c.method(sel)) {
        // SAFETY: `-updateTrackingAreas` is `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(method.implementation());
            f(view, sel);
        }
    }
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let children =
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) };
    for child in children.map(|a| a.values.clone()).unwrap_or_default() {
        // SAFETY: an object this runtime allocated.
        unsafe { update_tracking_areas(child as Id) };
    }
}

/// Whether any view in this tree has a tracking area.
///
/// Not *which* area and not its rectangle: the question a caller has is whether
/// the program asked to be told about the pointer at all, and a view that added
/// one covering part of itself still wants `-mouseMoved:` while the pointer is
/// in it. Enter and exit are per-area, and [`crate::cursor::update_tracking`]
/// delivers those.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn any_tracking_area(view: Id) -> bool {
    if !is_view(view) {
        return false;
    }
    let v = view_of(view);
    // SAFETY: the list is an array of this runtime's.
    let areas = unsafe { corefoundation::collections::array(v.tracking_areas as *const c_void) };
    if areas.is_some_and(|a| !a.values.is_empty()) {
        return true;
    }
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let children = unsafe { corefoundation::collections::array(v.subviews as *const c_void) };
    children
        .map(|a| a.values.clone())
        .unwrap_or_default()
        .into_iter()
        // SAFETY: an object this runtime allocated.
        .any(|child| unsafe { any_tracking_area(child as Id) })
}

/// The tracking areas a view holds, in the order they were added.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn tracking_areas_of(view: Id) -> Vec<Id> {
    if !is_view(view) {
        return Vec::new();
    }
    // SAFETY: the list is an array of this runtime's.
    let list = unsafe {
        corefoundation::collections::array(view_of(view).tracking_areas as *const c_void)
    };
    list.map(|a| a.values.iter().map(|v| *v as Id).collect())
        .unwrap_or_default()
}

/// The bounds a view reports, for a tracking area made with
/// `NSTrackingInVisibleRect`.
pub fn bounds_of(view: Id) -> NSRect {
    match is_view(view) {
        // SAFETY: a view of this crate's.
        true => unsafe { bounds(view, objc::sel::get("bounds")) },
        false => NSRect::default(),
    }
}

pub(crate) fn subviews_of(view: Id) -> Vec<Id> {
    if !is_view(view) {
        return Vec::new();
    }
    // SAFETY: a view holds an array of retained subviews.
    let list =
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) };
    list.map(|array| array.values.iter().map(|child| *child as Id).collect())
        .unwrap_or_default()
}

/// The view above this one, for walking the responder chain.
///
/// # Safety
///
/// As [`hit_test`].
pub unsafe fn superview_of(view: Id) -> Id {
    match is_view(view) {
        true => view_of(view).superview,
        false => std::ptr::null_mut(),
    }
}

/// The backing scale of a view's window, or one.
///
/// `wantsBestResolutionOpenGLSurface` controls the resolution AppKit gives an
/// OpenGL surface; it does not change the window's backing coordinate system.
/// SDL's OpenGL surface is always the high-density one because the window was
/// created with `HIGH_PIXEL_DENSITY`, so reporting one here for a view that did
/// not set that flag leaves the guest drawing a point-sized viewport into a
/// pixel-sized surface.
fn scale(receiver: Id) -> f64 {
    // SAFETY: a view of this crate's.
    let window = unsafe { window_of(receiver) };
    // SAFETY: null or a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    match sdl.is_null() {
        true => 1.0,
        false => sdl3::video::window_pixel_density(sdl) as f64,
    }
}

pub(crate) unsafe extern "C" fn init_with_frame(receiver: Id, _sel: Sel, frame: NSRect) -> Id {
    view_of(receiver).frame = frame;
    receiver
}

unsafe extern "C" fn frame(receiver: Id, _sel: Sel) -> NSRect {
    view_of(receiver).frame
}

unsafe extern "C" fn set_frame(receiver: Id, _sel: Sel, frame: NSRect) {
    let before = view_of(receiver).frame;
    view_of(receiver).frame = frame;
    note_geometry_change(receiver, before);
}

unsafe extern "C" fn set_frame_size(receiver: Id, _sel: Sel, size: NSSize) {
    let before = view_of(receiver).frame;
    view_of(receiver).frame.size = size;
    note_geometry_change(receiver, before);
}

unsafe extern "C" fn set_frame_origin(receiver: Id, _sel: Sel, origin: NSPoint) {
    let before = view_of(receiver).frame;
    view_of(receiver).frame.origin = origin;
    note_geometry_change(receiver, before);
}

/// `-bounds`: the frame's size at the origin, which is what a view with no
/// transform of its own has.
unsafe extern "C" fn bounds(receiver: Id, _sel: Sel) -> NSRect {
    NSRect {
        origin: NSPoint::default(),
        size: view_of(receiver).frame.size,
    }
}

unsafe extern "C" fn set_bounds(receiver: Id, _sel: Sel, bounds: NSRect) {
    let before = view_of(receiver).frame;
    view_of(receiver).frame.size = bounds.size;
    note_geometry_change(receiver, before);
}

/// `-visibleRect`: the part of the view not clipped away by an ancestor.
///
/// The bounds, because nothing in this tree clips: there is no scroll view and
/// no clip view with a smaller frame, so every view is wholly visible. A guest
/// sizing a viewport from this gets the same answer it would from `-bounds`,
/// which is what it would get from AppKit for a view that fills its window.
unsafe extern "C" fn visible_rect(receiver: Id, sel: Sel) -> NSRect {
    // SAFETY: `-bounds` is `NSRect (id, SEL)`.
    unsafe { bounds(receiver, sel) }
}

/// `-convertRectToBacking:`, which is the whole reason a Retina program asks a
/// view anything.
unsafe extern "C" fn convert_rect_to_backing(receiver: Id, _sel: Sel, rect: NSRect) -> NSRect {
    let scale = scale(receiver);
    NSRect {
        origin: NSPoint {
            x: rect.origin.x * scale,
            y: rect.origin.y * scale,
        },
        size: NSSize {
            width: rect.size.width * scale,
            height: rect.size.height * scale,
        },
    }
}

unsafe extern "C" fn convert_rect_from_backing(receiver: Id, _sel: Sel, rect: NSRect) -> NSRect {
    let scale = scale(receiver);
    NSRect {
        origin: NSPoint {
            x: rect.origin.x / scale,
            y: rect.origin.y / scale,
        },
        size: NSSize {
            width: rect.size.width / scale,
            height: rect.size.height / scale,
        },
    }
}

unsafe extern "C" fn convert_size_to_backing(receiver: Id, _sel: Sel, size: NSSize) -> NSSize {
    let scale = scale(receiver);
    NSSize {
        width: size.width * scale,
        height: size.height * scale,
    }
}

/// `-mouse:inRect:`, which is `NSMouseInRect` with the view's own flippedness:
/// the edge a point on it counts as inside is the top one either way up.
unsafe extern "C" fn mouse_in_rect(receiver: Id, _sel: Sel, point: NSPoint, rect: NSRect) -> u8 {
    let (min_y, max_y) = (rect.origin.y, rect.origin.y + rect.size.height);
    let x = point.x >= rect.origin.x && point.x < rect.origin.x + rect.size.width;
    let y = match is_flipped(receiver) {
        true => point.y >= min_y && point.y < max_y,
        false => point.y > min_y && point.y <= max_y,
    };
    (x && y) as u8
}

/// A point from `from`'s coordinates to `to`'s, through their window; `nil`
/// on either side is the window itself.
fn convert_point(point: NSPoint, from: Id, to: Id) -> NSPoint {
    point_from_window(to, point_to_window(from, point))
}

/// `-convertPoint:fromView:`, which is how Qt turns `-locationInWindow` into
/// a position in its flipped view.
unsafe extern "C" fn convert_point_from_view(
    receiver: Id,
    _sel: Sel,
    point: NSPoint,
    from: Id,
) -> NSPoint {
    convert_point(point, from, receiver)
}

unsafe extern "C" fn convert_point_to_view(
    receiver: Id,
    _sel: Sel,
    point: NSPoint,
    to: Id,
) -> NSPoint {
    convert_point(point, receiver, to)
}

unsafe extern "C" fn convert_rect_from_view(
    receiver: Id,
    _sel: Sel,
    rect: NSRect,
    from: Id,
) -> NSRect {
    convert_rect(rect, from, receiver)
}

/// `-convertRect:toView:`, which Qt uses to place a popup relative to the
/// window before asking the window for screen coordinates.
unsafe extern "C" fn convert_rect_to_view(receiver: Id, _sel: Sel, rect: NSRect, to: Id) -> NSRect {
    convert_rect(rect, receiver, to)
}

/// `-convertSize:toView:` and `-convertSize:fromView:`: a rect at the origin
/// converted, keeping only its extent, which is how scale applies to a size.
unsafe extern "C" fn convert_size_to_view(receiver: Id, _sel: Sel, size: NSSize, to: Id) -> NSSize {
    convert_rect(
        NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        },
        receiver,
        to,
    )
    .size
}

unsafe extern "C" fn convert_size_from_view(
    receiver: Id,
    _sel: Sel,
    size: NSSize,
    from: Id,
) -> NSSize {
    convert_rect(
        NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        },
        from,
        receiver,
    )
    .size
}

/// `(view, next key view)`, unretained as Apple's are. Kept for the keyboard
/// loop a program wires and reads back; nothing here tabs through it yet.
fn key_views() -> &'static std::sync::Mutex<Vec<(usize, usize)>> {
    static KEY_VIEWS: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
    &KEY_VIEWS
}

unsafe extern "C" fn set_next_key_view(receiver: Id, _sel: Sel, next: Id) {
    let mut views = key_views().lock().unwrap_or_else(|e| e.into_inner());
    views.retain(|(view, _)| *view != receiver as usize);
    if !next.is_null() {
        views.push((receiver as usize, next as usize));
    }
}

unsafe extern "C" fn next_key_view(receiver: Id, _sel: Sel) -> Id {
    let views = key_views().lock().unwrap_or_else(|e| e.into_inner());
    views
        .iter()
        .find(|(view, _)| *view == receiver as usize)
        .map_or(std::ptr::null_mut(), |(_, next)| *next as Id)
}

/// `(view, retained tool tip)`. Kept for `-toolTip`; nothing here shows one.
fn tool_tips() -> &'static std::sync::Mutex<Vec<(usize, usize)>> {
    static TIPS: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
    &TIPS
}

unsafe extern "C" fn set_tool_tip(receiver: Id, _sel: Sel, tip: Id) {
    let mut tips = tool_tips().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = tips.iter().position(|(view, _)| *view == receiver as usize) {
        let (_, old) = tips.remove(at);
        objc::arc::release(old as Id);
    }
    if !tip.is_null() {
        tips.push((receiver as usize, objc::arc::retain(tip) as usize));
    }
}

unsafe extern "C" fn tool_tip(receiver: Id, _sel: Sel) -> Id {
    let tips = tool_tips().lock().unwrap_or_else(|e| e.into_inner());
    tips.iter()
        .find(|(view, _)| *view == receiver as usize)
        .map_or(std::ptr::null_mut(), |(_, tip)| *tip as Id)
}

/// `-tag`: a plain view has none, which Apple spells -1. `NSControl`
/// overrides it with a stored one.
unsafe extern "C" fn tag(_receiver: Id, _sel: Sel) -> isize {
    -1
}

/// `-viewWithTag:`: the receiver or the first descendant, depth first, whose
/// `-tag` is `wanted`.
unsafe extern "C" fn view_with_tag(receiver: Id, sel: Sel, wanted: isize) -> Id {
    // SAFETY: `-tag` is `NSInteger (id, SEL)` on every view.
    let own = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("tag"))
    };
    if own == wanted {
        return receiver;
    }
    subviews_of(receiver)
        .into_iter()
        // SAFETY: each is a live subview, searched with the same selector.
        .map(|view| unsafe { view_with_tag(view, sel, wanted) })
        .find(|found| !found.is_null())
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn window(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a view of this crate's.
    unsafe { window_of(receiver) }
}

/// `-isInFullScreenMode`: the window's answer, because a view is only ever in
/// full screen by being in a window that is.
///
/// A view with no window is not, which is also what `-window` returning nil
/// means everywhere else here.
unsafe extern "C" fn is_in_full_screen_mode(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a view of this crate's.
    let window = unsafe { window_of(receiver) };
    if window.is_null() {
        return 0;
    }
    let sel = objc::sel::get("isInFullScreenMode");
    // SAFETY: `-isInFullScreenMode` is `BOOL (id, SEL)` on a window this crate
    // made.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(window, sel)
    }
}

unsafe extern "C" fn superview(receiver: Id, _sel: Sel) -> Id {
    view_of(receiver).superview
}

unsafe extern "C" fn next_responder(receiver: Id, _sel: Sel) -> Id {
    let view = view_of(receiver);
    match view.next_responder.is_null() {
        true => view.superview,
        false => view.next_responder,
    }
}

unsafe extern "C" fn set_next_responder(receiver: Id, _sel: Sel, responder: Id) {
    view_of(receiver).next_responder = responder;
}

/// `-setSubviews:`: the current subviews removed and these added, in order.
unsafe extern "C" fn set_subviews(receiver: Id, _sel: Sel, views: Id) {
    // SAFETY: a copy of the current list, so removing does not disturb the
    // walk, and the new list, each an `NSArray` of views or nil.
    unsafe {
        let current =
            corefoundation::collections::array(subviews(receiver, objc::sel::get("subviews")) as _)
                .map(|a| a.values.clone())
                .unwrap_or_default();
        let wanted = corefoundation::collections::array(views as _)
            .map(|a| a.values.clone())
            .unwrap_or_default();
        for view in current {
            if !wanted.contains(&view) {
                remove_from_superview(view as Id, objc::sel::get("removeFromSuperview"));
            }
        }
        for view in wanted {
            add_subview(receiver, objc::sel::get("addSubview:"), view as Id);
        }
    }
}

unsafe extern "C" fn subviews(receiver: Id, _sel: Sel) -> Id {
    let view = view_of(receiver);
    if view.subviews.is_null() {
        let array = corefoundation::collections::new_array(
            &[],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            true,
        );
        view.subviews = array as Id;
    }
    view.subviews
}

unsafe extern "C" fn add_subview(receiver: Id, sel: Sel, subview: Id) {
    if !is_view(subview) {
        return;
    }
    // A view has one superview: moving it takes it out of the old list, or the
    // old parent keeps a reference and, when it goes, frees what it no longer owns.
    // Held across the move because the old list may own the only reference.
    objc::arc::retain(subview);
    if !view_of(subview).superview.is_null() {
        // SAFETY: `-removeFromSuperview` is `void (id, SEL)`.
        unsafe { remove_from_superview(subview, objc::sel::get("removeFromSuperview")) };
    }
    view_of(subview).superview = receiver;
    // A window's content view that becomes a subview is in its new parent's
    // window, and a link left here would win over the superview chain.
    view_of(subview).window = std::ptr::null_mut();
    // SAFETY: `-subviews` is `id (id, SEL)` and answers a mutable array.
    let list = unsafe { subviews(receiver, sel) };
    if let Some(array) = unsafe { corefoundation::collections::array(list as *const c_void) } {
        array
            .values
            .push(array.policy.retain(subview as *const c_void));
    }
    objc::arc::release(subview);
    // SAFETY: a view this crate made, now in a window.
    unsafe { update_tracking_areas(subview) };
    crate::backing::needs_display_in_tree(subview, 0);
    crate::layout::solve();
}

/// `NSWindowOrderingMode`'s `NSWindowBelow`; `NSWindowAbove` is everything else.
const ORDER_BELOW: isize = -1;

/// `-addSubview:positioned:relativeTo:`: `-addSubview:`, then moved to sit just
/// above or below `other`, or above or below every sibling when `other` is not
/// one.
unsafe extern "C" fn add_subview_positioned(
    receiver: Id,
    sel: Sel,
    subview: Id,
    place: isize,
    other: Id,
) {
    // SAFETY: as `-addSubview:`.
    unsafe { add_subview(receiver, sel, subview) };
    if !is_view(subview) || view_of(subview).superview != receiver {
        return;
    }
    let list = view_of(receiver).subviews;
    // SAFETY: an array this module made.
    let Some(array) = (unsafe { corefoundation::collections::array(list as *const c_void) }) else {
        return;
    };
    let Some(from) = array
        .values
        .iter()
        .rposition(|v| *v == subview as *const c_void)
    else {
        return;
    };
    let entry = array.values.remove(from);
    let anchor = (other != subview)
        .then(|| {
            array
                .values
                .iter()
                .position(|v| *v == other as *const c_void)
        })
        .flatten();
    let at = match (anchor, place == ORDER_BELOW) {
        (Some(i), true) => i,
        (Some(i), false) => i + 1,
        (None, true) => 0,
        (None, false) => array.values.len(),
    };
    array.values.insert(at, entry);
}

unsafe extern "C" fn set_translates_autoresizing_mask(receiver: Id, _sel: Sel, flag: u8) {
    view_of(receiver).constrained = usize::from(flag == 0);
    crate::layout::solve();
}

unsafe extern "C" fn translates_autoresizing_mask(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).constrained == 0)
}

/// Whether constraints, rather than the frame a program sets, place `view`.
pub(crate) fn is_constrained(view: Id) -> bool {
    is_view(view) && view_of(view).constrained != 0
}

pub(crate) fn frame_of(view: Id) -> NSRect {
    view_of(view).frame
}

/// A rectangle in `from`'s coordinates, in `to`'s.
pub(crate) fn convert_rect(rect: NSRect, from: Id, to: Id) -> NSRect {
    rect_between(rect, |point| convert_point(point, from, to))
}

unsafe extern "C" fn remove_from_superview(receiver: Id, _sel: Sel) {
    let view = view_of(receiver);
    let parent = view.superview;
    view.superview = std::ptr::null_mut();
    if !is_view(parent) {
        return;
    }
    let list = view_of(parent).subviews;
    if list.is_null() {
        return;
    }
    // SAFETY: an array this module made.
    if let Some(array) = unsafe { corefoundation::collections::array(list as *const c_void) } {
        let before = array.values.len();
        array.values.retain(|v| *v != receiver as *const c_void);
        // The list retained the view once per `-addSubview:`, which now moves
        // out of a single entry at most; last, as it may free the receiver.
        for _ in array.values.len()..before {
            array.policy.release(receiver as *const c_void);
        }
    }
}

/// `-dealloc`. Subviews are told their superview is gone before the list that
/// retains them is released: a subview something else still holds would
/// otherwise walk its ancestors into freed memory.
unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let view = view_of(receiver);
    objc::arc::release(std::mem::replace(
        &mut view.appearance,
        std::ptr::null_mut(),
    ));
    let subviews = std::mem::replace(&mut view.subviews, std::ptr::null_mut());
    // SAFETY: an array this module made, or null.
    if let Some(array) = unsafe { corefoundation::collections::array(subviews as *const c_void) } {
        for child in &array.values {
            let child = *child as Id;
            if is_view(child) && view_of(child).superview == receiver {
                view_of(child).superview = std::ptr::null_mut();
            }
        }
    }
    objc::arc::release(subviews);
    crate::layout::forget(receiver);
    crate::backing::forget(receiver);
    let tracking_areas = std::mem::replace(&mut view.tracking_areas, std::ptr::null_mut());
    objc::arc::release(tracking_areas);
    let layer = std::mem::replace(&mut view.layer, std::ptr::null_mut());
    // SAFETY: the layer this view set, which must not point back at it.
    unsafe { crate::layer::set_view(layer, std::ptr::null_mut()) };
    objc::arc::release(layer);
    crate::cursor::discard_rects(receiver);
    objc::arc::finalize(receiver);
}

/// `-addTrackingArea:` and its two companions.
///
/// The area is kept in the subview-shaped list a view already has for it, so
/// `-removeTrackingArea:` can find the object it was given back. Delivery is
/// [`crate::cursor::update_tracking`]'s, which walks these lists on every move.
unsafe extern "C" fn add_tracking_area(receiver: Id, _sel: Sel, area: Id) {
    let view = view_of(receiver);
    if view.tracking_areas.is_null() {
        let array = corefoundation::collections::new_array(
            &[],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            true,
        );
        view.tracking_areas = array as Id;
    }
    // SAFETY: an array this module made.
    if let Some(array) =
        unsafe { corefoundation::collections::array(view.tracking_areas as *const c_void) }
    {
        array
            .values
            .push(array.policy.retain(area as *const c_void));
    }
}

unsafe extern "C" fn remove_tracking_area(receiver: Id, _sel: Sel, area: Id) {
    let list = view_of(receiver).tracking_areas;
    if list.is_null() {
        return;
    }
    // SAFETY: an array this module made.
    if let Some(array) = unsafe { corefoundation::collections::array(list as *const c_void) } {
        array.values.retain(|v| *v != area as *const c_void);
    }
}

/// `-addTrackingRect:owner:userData:assumeInside:`, the API tracking areas
/// replaced: an always-active enter-and-exit area, whose tag is the area.
///
/// `userData` is not carried to the events: an area's `userInfo` is a
/// dictionary, and the events built from it answer that.
unsafe extern "C" fn add_tracking_rect(
    receiver: Id,
    sel: Sel,
    rect: NSRect,
    owner: Id,
    _data: *mut c_void,
    inside: u8,
) -> isize {
    use crate::cursor::tracking;
    let Some(class) = objc::class::lookup(c"NSTrackingArea") else {
        return 0;
    };
    let mut options = tracking::MOUSE_ENTERED_AND_EXITED | tracking::ACTIVE_ALWAYS;
    if inside != 0 {
        options |= tracking::ASSUME_INSIDE;
    }
    let area = objc::object::create_instance(class, 0);
    // SAFETY: `-initWithRect:options:owner:userInfo:` is
    // `id (id, SEL, NSRect, NSUInteger, id, id)`.
    let area = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect, usize, Id, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            area,
            objc::sel::get("initWithRect:options:owner:userInfo:"),
            rect,
            options,
            owner,
            std::ptr::null_mut(),
        )
    };
    if area.is_null() {
        return 0;
    }
    // SAFETY: the list retains the area, so this reference is given up.
    unsafe { add_tracking_area(receiver, sel, area) };
    objc::arc::release(area);
    area as isize
}

unsafe extern "C" fn remove_tracking_rect(receiver: Id, sel: Sel, tag: isize) {
    // SAFETY: a tag is the area `-addTrackingRect:...` added, compared by
    // address only.
    unsafe { remove_tracking_area(receiver, sel, tag as Id) };
}

/// `-animator`: the view itself, so that what a program animates is set at
/// once, which is where the animation would have ended.
unsafe extern "C" fn animator(receiver: Id, _sel: Sel) -> Id {
    receiver
}

unsafe extern "C" fn tracking_areas(receiver: Id, sel: Sel) -> Id {
    let view = view_of(receiver);
    if view.tracking_areas.is_null() {
        // SAFETY: `-addTrackingArea:` is `void (id, SEL, id)`, and adding nil
        // is what makes the empty list without adding anything to it.
        unsafe { add_tracking_area(receiver, sel, std::ptr::null_mut()) };
        unsafe { remove_tracking_area(receiver, sel, std::ptr::null_mut()) };
    }
    view.tracking_areas
}

unsafe extern "C" fn set_wants_best_resolution(receiver: Id, _sel: Sel, wants: u8) {
    view_of(receiver).wants_best_resolution = wants as usize;
}

unsafe extern "C" fn wants_best_resolution(receiver: Id, _sel: Sel) -> u8 {
    (view_of(receiver).wants_best_resolution != 0) as u8
}

/// Answered as true, because a view here is always ready to be drawn into.
unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> u8 {
    1
}

unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> u8 {
    0
}

/// Whether `imp` is NSView's own `-acceptsFirstResponder`. It answers yes for
/// every view, so it says nothing about whether a click should focus one.
pub(crate) fn is_default_accepts_first_responder(imp: objc::class::Imp) -> bool {
    imp == yes as objc::class::Imp
}

/// `-display`: draw the view now.
///
/// **This is what calls `-drawRect:`,** and a no-op here is a program that
/// renders nothing while looking like it is running. Deltarune's frame is
/// exactly this path: its display link sends `-[YYGLView display]`, AppKit turns
/// that into `-drawRect:`, and the guest's override makes its GL context current
/// and swaps. With `-display` empty the game ticks, the display link fires, and
/// the window stays black.
///
/// Sent rather than called, so the *guest's* override is what runs, with the
/// view's focus locked so that what it draws lands in the window's store. A
/// view with no `-drawRect:` of its own draws nothing, which is what an empty
/// view does.
unsafe extern "C" fn display(receiver: Id, _sel: Sel) {
    crate::backing::display_view(receiver);
}

unsafe extern "C" fn draw_rect(_receiver: Id, _sel: Sel, _rect: NSRect) {}

unsafe extern "C" fn set_needs_display(receiver: Id, _sel: Sel, flag: u8) {
    if flag != 0 {
        crate::backing::needs_display(receiver);
    }
}

/// `-setNeedsDisplay` with no argument: not in AppKit's headers, but programs
/// written against `UIView` habits send it and a Mac's AppKit answers it.
unsafe extern "C" fn mark_needs_display(receiver: Id, _sel: Sel) {
    crate::backing::needs_display(receiver);
}

unsafe extern "C" fn set_needs_display_in_rect(receiver: Id, _sel: Sel, _rect: NSRect) {
    crate::backing::needs_display(receiver);
}

unsafe extern "C" fn set_hidden(receiver: Id, _sel: Sel, hidden: u8) {
    let was = view_of(receiver).hidden != 0;
    view_of(receiver).hidden = usize::from(hidden != 0);
    if was && hidden == 0 {
        crate::backing::needs_display(receiver);
    }
}

unsafe extern "C" fn is_hidden(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).hidden != 0)
}

unsafe extern "C" fn is_hidden_or_has_hidden_ancestor_method(receiver: Id, _sel: Sel) -> u8 {
    u8::from(is_hidden_or_has_hidden_ancestor(receiver))
}

/// Whether `view` or any view above it is hidden, which is what decides
/// whether it is drawn or presented at all.
pub fn is_hidden_or_has_hidden_ancestor(view: Id) -> bool {
    let mut current = view;
    for _ in 0..64 {
        if !is_view(current) {
            return false;
        }
        let node = view_of(current);
        if node.hidden != 0 {
            return true;
        }
        current = node.superview;
    }
    false
}

unsafe extern "C" fn ignored(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _object: Id) {}

unsafe extern "C" fn ignored_flag(_receiver: Id, _sel: Sel, _flag: u8) {}

unsafe extern "C" fn ignored_integer(_receiver: Id, _sel: Sel, _value: isize) {}

/// `-addCursorRect:cursor:`, which is only ever called from inside a view's own
/// `-resetCursorRects`: the rectangle it records lasts until the next
/// `-discardCursorRects`, and [`crate::cursor`] decides from the whole set
/// which cursor the pointer is over.
unsafe extern "C" fn add_cursor_rect(receiver: Id, _sel: Sel, rect: NSRect, cursor: Id) {
    crate::cursor::add_rect(receiver, rect, cursor);
}

unsafe extern "C" fn discard_cursor_rects(receiver: Id, _sel: Sel) {
    crate::cursor::discard_rects(receiver);
}

unsafe extern "C" fn backing_scale_factor(receiver: Id, _sel: Sel) -> CGFloat {
    scale(receiver)
}

unsafe extern "C" fn appearance(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const View)).appearance }
}

unsafe extern "C" fn set_appearance(receiver: Id, _sel: Sel, appearance: Id) {
    let appearance = objc::arc::retain(appearance);
    let old = unsafe { std::mem::replace(&mut (*(receiver as *mut View)).appearance, appearance) };
    objc::arc::release(old);
}

unsafe extern "C" fn effective_appearance(receiver: Id, sel: Sel) -> Id {
    let own = unsafe { appearance(receiver, sel) };
    if !own.is_null() {
        return own;
    }
    let view = view_of(receiver);
    let source = if !view.superview.is_null() {
        view.superview
    } else if !view.window.is_null() {
        view.window
    } else {
        crate::application::shared()
    };
    crate::appearance::effective_of(source)
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSView").expect("declared before its methods");
    methods!(class, {
        "appearance" => appearance, c"@16@0:8";
        "setAppearance:" => set_appearance, c"v24@0:8@16";
        "effectiveAppearance" => effective_appearance, c"@16@0:8";
        "initWithFrame:" => init_with_frame, c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "frame" => frame, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "setFrame:" => set_frame, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "setFrameSize:" => set_frame_size, c"v32@0:8{CGSize=dd}16";
        "setFrameOrigin:" => set_frame_origin, c"v32@0:8{CGPoint=dd}16";
        "bounds" => bounds, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "visibleRect" => visible_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "setBounds:" => set_bounds, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "convertRectToBacking:" => convert_rect_to_backing,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "convertRectFromBacking:" => convert_rect_from_backing,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "convertSizeToBacking:" => convert_size_to_backing, c"{CGSize=dd}32@0:8{CGSize=dd}16";
        "convertPoint:fromView:" => convert_point_from_view, c"{CGPoint=dd}40@0:8{CGPoint=dd}16@32";
        "convertPoint:toView:" => convert_point_to_view, c"{CGPoint=dd}40@0:8{CGPoint=dd}16@32";
        "mouse:inRect:" => mouse_in_rect,
            c"c56@0:8{CGPoint=dd}16{CGRect={CGPoint=dd}{CGSize=dd}}32";
        "convertRect:fromView:" => convert_rect_from_view,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "convertRect:toView:" => convert_rect_to_view,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "convertSize:toView:" => convert_size_to_view, c"{CGSize=dd}40@0:8{CGSize=dd}16@32";
        "convertSize:fromView:" => convert_size_from_view, c"{CGSize=dd}40@0:8{CGSize=dd}16@32";
        "tag" => tag, c"q16@0:8";
        "setNextKeyView:" => set_next_key_view, c"v24@0:8@16";
        "setToolTip:" => set_tool_tip, c"v24@0:8@16";
        "toolTip" => tool_tip, c"@16@0:8";
        "nextKeyView" => next_key_view, c"@16@0:8";
        "viewWithTag:" => view_with_tag, c"@24@0:8q16";
        "window" => window, c"@16@0:8";
        "isInFullScreenMode" => is_in_full_screen_mode, c"c16@0:8";
        "superview" => superview, c"@16@0:8";
        "nextResponder" => next_responder, c"@16@0:8";
        "setNextResponder:" => set_next_responder, c"v24@0:8@16";
        "setSubviews:" => set_subviews, c"v24@0:8@16";
        "subviews" => subviews, c"@16@0:8";
        "hitTest:" => hit_test_method, c"@32@0:8{CGPoint=dd}16";
        "addSubview:" => add_subview, c"v24@0:8@16";
        "addSubview:positioned:relativeTo:" => add_subview_positioned, c"v40@0:8@16q24@32";
        "setTranslatesAutoresizingMaskIntoConstraints:" => set_translates_autoresizing_mask,
            c"v20@0:8c16";
        "translatesAutoresizingMaskIntoConstraints" => translates_autoresizing_mask, c"c16@0:8";
        "removeFromSuperview" => remove_from_superview, c"v16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
        "setWantsBestResolutionOpenGLSurface:" => set_wants_best_resolution, c"v20@0:8c16";
        "wantsBestResolutionOpenGLSurface" => wants_best_resolution, c"c16@0:8";
        "backingScaleFactor" => backing_scale_factor, c"d16@0:8";
        "acceptsFirstResponder" => yes, c"c16@0:8";
        "canBecomeKeyView" => yes, c"c16@0:8";
        "isOpaque" => yes, c"c16@0:8";
        "isFlipped" => no, c"c16@0:8";
        "isHidden" => is_hidden, c"c16@0:8";
        // No live-resize session is ever started here: the window system resizes
        // a window in one step and nothing posts NSWindowWillStartLiveResize.
        "inLiveResize" => no, c"c16@0:8";
        "isHiddenOrHasHiddenAncestor" => is_hidden_or_has_hidden_ancestor_method, c"c16@0:8";
        "display" => display, c"v16@0:8";
        "displayIfNeeded" => display, c"v16@0:8";
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "setNeedsDisplay:" => set_needs_display, c"v20@0:8c16";
        "setNeedsDisplay" => mark_needs_display, c"v16@0:8";
        // There is no trackpad touch stream to opt in to.
        "setAcceptsTouchEvents:" => ignored_flag, c"v20@0:8c16";
        "setHidden:" => set_hidden, c"v20@0:8c16";
        "setAlphaValue:" => set_alpha_value, c"v24@0:8d16";
        "alphaValue" => alpha_value, c"d16@0:8";
        // Drawing focus rings belongs to AppKit's control renderer, which this
        // view does not have; accepting the policy still lets Qt construct it.
        "setFocusRingType:" => ignored_integer, c"v24@0:8Q16";
        // Drag delivery is not wired to SDL yet, but registration itself has
        // no observable result other than which future drag messages arrive.
        "registerForDraggedTypes:" => ignored_object, c"v24@0:8@16";
        "unregisterDraggedTypes" => ignored, c"v16@0:8";
        "setAutoresizingMask:" => set_autoresizing_mask, c"v24@0:8Q16";
        "autoresizingMask" => autoresizing_mask, c"Q16@0:8";
        "setAutoresizesSubviews:" => set_autoresizes_subviews, c"v20@0:8c16";
        "autoresizesSubviews" => autoresizes_subviews, c"c16@0:8";
        "resizeSubviewsWithOldSize:" => resize_subviews_with_old_size,
            c"v32@0:8{CGSize=dd}16";
        "resizeWithOldSuperviewSize:" => resize_with_old_superview_size,
            c"v32@0:8{CGSize=dd}16";
        "setNeedsDisplayInRect:" => set_needs_display_in_rect,
            c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "addCursorRect:cursor:" => add_cursor_rect,
            c"v56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "discardCursorRects" => discard_cursor_rects, c"v16@0:8";
        // AppKit's own is empty; subclasses override it and may call super.
        "resetCursorRects" => ignored, c"v16@0:8";
        "updateTrackingAreas" => ignored, c"v16@0:8";
        "addTrackingArea:" => add_tracking_area, c"v24@0:8@16";
        "removeTrackingArea:" => remove_tracking_area, c"v24@0:8@16";
        "trackingAreas" => tracking_areas, c"@16@0:8";
        "addTrackingRect:owner:userData:assumeInside:" => add_tracking_rect,
            c"q68@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48^v56c64";
        "removeTrackingRect:" => remove_tracking_rect, c"v24@0:8q16";
        "animator" => animator, c"@16@0:8";
        "setPostsFrameChangedNotifications:" => set_posts_frame_changed, c"v20@0:8c16";
        "setPostsBoundsChangedNotifications:" => set_posts_bounds_changed, c"v20@0:8c16";
        "postsFrameChangedNotifications" => posts_frame_changed, c"c16@0:8";
        "postsBoundsChangedNotifications" => posts_bounds_changed, c"c16@0:8";
        "setLayer:" => set_layer, c"v24@0:8@16";
        "layer" => layer, c"@16@0:8";
        "setWantsLayer:" => set_wants_layer, c"v20@0:8c16";
        // A hint about when the backing store may be reused across a resize.
        // Nothing here keeps one, so every policy draws the same.
        "setLayerContentsRedrawPolicy:" => ignored_integer, c"v24@0:8q16";
        "setLayerContentsPlacement:" => set_layer_contents_placement, c"v24@0:8q16";
        "layerContentsPlacement" => layer_contents_placement, c"q16@0:8";
        "makeBackingLayer" => make_backing_layer, c"@16@0:8";
        // A view draws with `-drawRect:` unless it says otherwise.
        "wantsUpdateLayer" => no, c"c16@0:8";
        "updateLayer" => ignored, c"v16@0:8";
        "wantsLayer" => wants_layer, c"c16@0:8";
        "setAllowedTouchTypes:" => set_allowed_touch_types, c"v24@0:8Q16";
        "allowedTouchTypes" => allowed_touch_types, c"Q16@0:8";
        "setClipsToBounds:" => set_clips_to_bounds, c"v20@0:8c16";
        "clipsToBounds" => clips_to_bounds, c"c16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(frame: NSRect) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSView").expect("NSView");
        let object = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
        unsafe { init_with_frame(object, objc::sel::get("initWithFrame:"), frame) }
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect {
            origin: NSPoint { x, y },
            size: NSSize { width, height },
        }
    }

    /// A superview and one subview of the given mask, wired as `-addSubview:`
    /// wires them.
    fn tree(parent: NSRect, child: NSRect, mask: usize) -> (Id, Id) {
        let superview = view(parent);
        let subview = view(child);
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(superview, objc::sel::get("addSubview:"), subview) };
        view_of(subview).autoresizing_mask = mask;
        (superview, subview)
    }

    /// A view starts opaque, reads back the clamped alpha it was given, and
    /// fades its subviews with it.
    #[test]
    fn alpha_value_is_clamped_and_multiplies_down_the_tree() {
        let (superview, subview) = tree(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 5.0, 5.0), 0);
        let set = objc::sel::get("setAlphaValue:");
        let get = objc::sel::get("alphaValue");
        // SAFETY: `-setAlphaValue:` is `void (id, SEL, CGFloat)` and
        // `-alphaValue` is `CGFloat (id, SEL)`.
        unsafe {
            assert_eq!(alpha_value(subview, get), 1.0);
            set_alpha_value(superview, set, 0.5);
            set_alpha_value(subview, set, 2.0);
            assert_eq!(alpha_value(subview, get), 1.0);
            set_alpha_value(subview, set, 0.5);
        }
        assert_eq!(effective_alpha(subview), 0.25);
        objc::arc::release(superview);
    }

    #[test]
    fn a_freed_superview_leaves_its_retained_subview_detached() {
        let (superview, subview) = tree(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 5.0, 5.0), 0);
        objc::arc::retain(subview);
        objc::arc::release(superview);
        assert!(view_of(subview).superview.is_null());
        assert!(!is_hidden_or_has_hidden_ancestor(subview));
        objc::arc::release(subview);
    }

    /// Qt makes a window's content view a subview elsewhere when the window
    /// becomes a child; the old window must not keep answering `-window`.
    #[test]
    fn a_content_view_added_as_a_subview_leaves_its_window() {
        let parent = view(rect(0.0, 0.0, 10.0, 10.0));
        let content = view(rect(0.0, 0.0, 5.0, 5.0));
        let window = parent;
        // SAFETY: two views this test made; the window pointer is only compared.
        unsafe {
            set_window(content, window);
            add_subview(parent, objc::sel::get("addSubview:"), content);
            assert!(window_of(content).is_null());
        }
    }

    #[test]
    fn detaching_a_window_only_forgets_that_window() {
        let content = view(rect(0.0, 0.0, 5.0, 5.0));
        let (window, other) = (
            view(rect(0.0, 0.0, 1.0, 1.0)),
            view(rect(0.0, 0.0, 1.0, 1.0)),
        );
        // SAFETY: views this test made; the window pointers are only compared.
        unsafe {
            set_window(content, window);
            detach_window(content, other);
            assert_eq!(window_of(content), window);
            detach_window(content, window);
            assert!(window_of(content).is_null());
        }
    }

    #[test]
    fn moving_a_subview_takes_it_out_of_its_old_superview() {
        let (first, subview) = tree(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 5.0, 5.0), 0);
        let second = view(rect(0.0, 0.0, 10.0, 10.0));
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(second, objc::sel::get("addSubview:"), subview) };
        assert_eq!(view_of(subview).superview, second);
        assert!(children(first).is_empty());
        assert_eq!(children(second), vec![subview]);
        assert_eq!(objc::arc::retain_count(subview), 2);
    }

    #[test]
    fn converts_a_rect_between_a_subview_and_its_window() {
        let (superview, subview) = tree(
            rect(10.0, 20.0, 100.0, 100.0),
            rect(5.0, 7.0, 50.0, 50.0),
            0,
        );
        let sel = objc::sel::get("convertRect:toView:");
        // SAFETY: both are views of this module's, and nil is the window.
        unsafe {
            assert_eq!(
                convert_rect_to_view(subview, sel, rect(1.0, 1.0, 4.0, 4.0), std::ptr::null_mut()),
                rect(16.0, 28.0, 4.0, 4.0)
            );
            assert_eq!(
                convert_rect_to_view(subview, sel, rect(1.0, 1.0, 4.0, 4.0), superview),
                rect(6.0, 8.0, 4.0, 4.0)
            );
            assert_eq!(
                convert_rect_from_view(subview, sel, rect(6.0, 8.0, 4.0, 4.0), superview),
                rect(1.0, 1.0, 4.0, 4.0)
            );
        }
    }

    unsafe extern "C" fn flipped_yes(_receiver: Id, _sel: Sel) -> u8 {
        1
    }

    fn flipped_view(frame: NSRect) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(c"RosemaryTestFlippedView").unwrap_or_else(|| {
            let class = corefoundation::base::register(
                c"RosemaryTestFlippedView",
                objc::class::lookup(c"NSView"),
                INSTANCE_SIZE,
                false,
            );
            methods!(class, {
                "isFlipped" => flipped_yes, c"c16@0:8";
            });
            class
        });
        let object = objc::object::create_instance(class, 0);
        // SAFETY: the object has `View` storage.
        unsafe { init_with_frame(object, objc::sel::get("initWithFrame:"), frame) }
    }

    /// Qt's views are flipped, and a subview at `y = 0` of one is along its
    /// top edge: in window coordinates, the top of the window.
    #[test]
    fn a_flipped_superview_places_its_subviews_from_the_top() {
        let content = flipped_view(rect(0.0, 0.0, 200.0, 100.0));
        let ribbon = flipped_view(rect(0.0, 0.0, 200.0, 20.0));
        let plain = view(rect(10.0, 30.0, 50.0, 40.0));
        // SAFETY: `-addSubview:` is `void (id, SEL, id)` on views.
        unsafe {
            add_subview(content, objc::sel::get("addSubview:"), ribbon);
            add_subview(content, objc::sel::get("addSubview:"), plain);
        }
        assert_eq!(rect_in_window(ribbon), rect(0.0, 80.0, 200.0, 20.0));
        assert_eq!(rect_in_window(plain), rect(10.0, 30.0, 50.0, 40.0));

        // A window point near the top is near the top of the flipped ribbon,
        // and one inside the unflipped view counts up from its bottom.
        let top = NSPoint { x: 5.0, y: 95.0 };
        assert_eq!(point_from_window(ribbon, top), NSPoint { x: 5.0, y: 5.0 });
        assert_eq!(
            point_from_window(plain, NSPoint { x: 15.0, y: 35.0 }),
            NSPoint { x: 5.0, y: 5.0 }
        );
        assert_eq!(point_to_window(ribbon, NSPoint { x: 5.0, y: 5.0 }), top);
        let sel = objc::sel::get("convertPoint:fromView:");
        // SAFETY: a view of this module's, and nil is the window.
        let local = unsafe { convert_point_from_view(ribbon, sel, top, std::ptr::null_mut()) };
        assert_eq!(local, NSPoint { x: 5.0, y: 5.0 });

        // The hit test takes the point in the content view's superview's
        // coordinates, which for a content view are the window's.
        // SAFETY: live views.
        assert_eq!(unsafe { hit_test(content, top) }, ribbon);
        assert_eq!(
            unsafe { hit_test(content, NSPoint { x: 15.0, y: 35.0 }) },
            plain
        );
    }

    fn children(view: Id) -> Vec<Id> {
        // SAFETY: an array this module made.
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) }
            .map(|a| a.values.iter().map(|v| *v as Id).collect())
            .unwrap_or_default()
    }

    fn set_frame_of(view: Id, frame: NSRect) {
        // SAFETY: `-setFrame:` is `void (id, SEL, NSRect)`.
        unsafe { set_frame(view, objc::sel::get("setFrame:"), frame) };
    }

    /// The case every windowed program is in: a render view that fills the
    /// content view, and a window the person resizes. Without this the program
    /// asks its own view how big it is and gets the size the window opened at.
    #[test]
    fn a_sizable_subview_follows_its_superview() {
        let (superview, subview) = tree(
            rect(0.0, 0.0, 1000.0, 665.0),
            rect(0.0, 0.0, 1000.0, 665.0),
            autoresizing::WIDTH_SIZABLE | autoresizing::HEIGHT_SIZABLE,
        );
        set_frame_of(superview, rect(0.0, 0.0, 1200.0, 760.0));
        assert_eq!(view_of(subview).frame, rect(0.0, 0.0, 1200.0, 760.0));
    }

    /// `NSViewNotSizable` is a real answer: a fixed-size control in a resizing
    /// window keeps both its size and its place.
    #[test]
    fn a_not_sizable_subview_keeps_its_frame() {
        let (superview, subview) = tree(
            rect(0.0, 0.0, 400.0, 400.0),
            rect(10.0, 10.0, 80.0, 20.0),
            0,
        );
        set_frame_of(superview, rect(0.0, 0.0, 800.0, 400.0));
        assert_eq!(view_of(subview).frame, rect(10.0, 10.0, 80.0, 20.0));
    }

    /// A view that is sizable with a fixed leading margin absorbs the whole
    /// change and stays where it started.
    #[test]
    fn a_fixed_margin_holds_a_sizable_subview_in_place() {
        let (superview, subview) = tree(
            rect(0.0, 0.0, 200.0, 100.0),
            rect(20.0, 0.0, 180.0, 100.0),
            autoresizing::WIDTH_SIZABLE,
        );
        set_frame_of(superview, rect(0.0, 0.0, 300.0, 100.0));
        assert_eq!(view_of(subview).frame, rect(20.0, 0.0, 280.0, 100.0));
    }

    /// A resize reaches all the way down, because a subview that resized has
    /// subviews of its own to tell.
    #[test]
    fn a_resize_reaches_a_grandchild() {
        let both = autoresizing::WIDTH_SIZABLE | autoresizing::HEIGHT_SIZABLE;
        let (superview, subview) = tree(
            rect(0.0, 0.0, 100.0, 100.0),
            rect(0.0, 0.0, 100.0, 100.0),
            both,
        );
        let grandchild = view(rect(0.0, 0.0, 100.0, 100.0));
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(subview, objc::sel::get("addSubview:"), grandchild) };
        view_of(grandchild).autoresizing_mask = both;
        set_frame_of(superview, rect(0.0, 0.0, 200.0, 50.0));
        assert_eq!(view_of(grandchild).frame, rect(0.0, 0.0, 200.0, 50.0));
    }

    /// A superview told not to autoresize its subviews does not.
    #[test]
    fn autoresizes_subviews_no_leaves_the_subviews_alone() {
        let (superview, subview) = tree(
            rect(0.0, 0.0, 100.0, 100.0),
            rect(0.0, 0.0, 100.0, 100.0),
            autoresizing::WIDTH_SIZABLE | autoresizing::HEIGHT_SIZABLE,
        );
        // SAFETY: `-setAutoresizesSubviews:` is `void (id, SEL, BOOL)`.
        unsafe {
            set_autoresizes_subviews(superview, objc::sel::get("setAutoresizesSubviews:"), 0)
        };
        set_frame_of(superview, rect(0.0, 0.0, 200.0, 200.0));
        assert_eq!(view_of(subview).frame, rect(0.0, 0.0, 100.0, 100.0));
    }

    static OVERRIDE_RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static HIT_OVERRIDE_RUNS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "C" fn overridden_resize(_receiver: Id, _sel: Sel, _old: NSSize) {
        OVERRIDE_RAN.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    unsafe extern "C" fn overridden_hit_test(receiver: Id, sel: Sel, point: NSPoint) -> Id {
        HIT_OVERRIDE_RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Model an Objective-C override's `[super hitTest:point]`: enter the
        // inherited implementation without redispatching this override.
        unsafe { hit_test_method(receiver, sel, point) }
    }

    #[test]
    fn a_hit_test_override_can_call_super_once() {
        crate::bootstrap();
        let class = corefoundation::base::register(
            c"RosemaryTestHitView",
            objc::class::lookup(c"NSView"),
            INSTANCE_SIZE,
            false,
        );
        methods!(class, {
            "hitTest:" => overridden_hit_test, c"@32@0:8{CGPoint=dd}16";
        });
        let object = objc::object::create_instance(class, 0);
        // SAFETY: the object has `View` storage and the method signatures match.
        unsafe {
            init_with_frame(
                object,
                objc::sel::get("initWithFrame:"),
                rect(0.0, 0.0, 100.0, 100.0),
            );
        }
        HIT_OVERRIDE_RUNS.store(0, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: a live view and a point inside it.
        assert_eq!(
            unsafe { hit_test(object, NSPoint { x: 5.0, y: 5.0 }) },
            object
        );
        assert_eq!(
            HIT_OVERRIDE_RUNS.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    /// The subview is *sent* `-resizeWithOldSuperviewSize:`, so a program that
    /// overrides it lays itself out and the mask is not consulted behind its
    /// back. AppKit's contract, and the reason a view can size itself against
    /// something the mask cannot express.
    #[test]
    fn a_subclasss_own_resize_method_is_what_runs() {
        crate::bootstrap();
        let class = corefoundation::base::register(
            c"RosemaryTestResizeView",
            objc::class::lookup(c"NSView"),
            INSTANCE_SIZE,
            false,
        );
        methods!(class, {
            "resizeWithOldSuperviewSize:" => overridden_resize, c"v32@0:8{CGSize=dd}16";
        });
        let superview = view(rect(0.0, 0.0, 100.0, 100.0));
        let subview = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
        unsafe {
            init_with_frame(
                subview,
                objc::sel::get("initWithFrame:"),
                rect(0.0, 0.0, 100.0, 100.0),
            )
        };
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(superview, objc::sel::get("addSubview:"), subview) };
        view_of(subview).autoresizing_mask =
            autoresizing::WIDTH_SIZABLE | autoresizing::HEIGHT_SIZABLE;

        set_frame_of(superview, rect(0.0, 0.0, 200.0, 200.0));
        assert!(OVERRIDE_RAN.load(std::sync::atomic::Ordering::Relaxed));
        // The override did not call through, so nothing moved the frame.
        assert_eq!(view_of(subview).frame, rect(0.0, 0.0, 100.0, 100.0));
    }

    /// A view with no window scales by one, whatever it was asked - which is
    /// the case a headless test and a program before `-setContentView:` are in.
    #[test]
    fn a_view_with_no_window_does_not_scale_its_backing() {
        let rect = NSRect {
            origin: NSPoint::default(),
            size: NSSize {
                width: 640.0,
                height: 480.0,
            },
        };
        let v = view(rect);
        // SAFETY: `-convertRectToBacking:` is `NSRect (id, SEL, NSRect)`.
        let backing =
            unsafe { convert_rect_to_backing(v, objc::sel::get("convertRectToBacking:"), rect) };
        assert_eq!(backing, rect);
    }

    /// A subview knows its superview, and the superview lists it.
    #[test]
    fn a_subview_is_in_its_superviews_list() {
        let parent = view(NSRect::default());
        let child = view(NSRect::default());
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(parent, objc::sel::get("addSubview:"), child) };
        // SAFETY: `-superview` is `id (id, SEL)`.
        assert_eq!(
            unsafe { superview(child, objc::sel::get("superview")) },
            parent
        );
        // SAFETY: `-subviews` is `id (id, SEL)`.
        let list = unsafe { subviews(parent, objc::sel::get("subviews")) };
        // SAFETY: an array this module made.
        let array =
            unsafe { corefoundation::collections::array(list as *const c_void) }.expect("an array");
        assert_eq!(array.values.len(), 1);

        // SAFETY: `-removeFromSuperview` is `void (id, SEL)`.
        unsafe { remove_from_superview(child, objc::sel::get("removeFromSuperview")) };
        let array =
            unsafe { corefoundation::collections::array(list as *const c_void) }.expect("an array");
        assert!(array.values.is_empty());
    }

    /// A view finds its window through its superview, which is how a guest's
    /// `YYGLView` inside a content view reaches one.
    #[test]
    fn a_view_finds_its_window_up_the_chain() {
        let parent = view(NSRect::default());
        let child = view(NSRect::default());
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(parent, objc::sel::get("addSubview:"), child) };
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        // SAFETY: both are objects this runtime allocated.
        unsafe { set_window(parent, window) };
        assert_eq!(unsafe { window_of(child) }, window);
    }

    /// A subview's rectangle in the pixels of the window it is in, which is
    /// what presentation blits a `CAMetalLayer`'s drawable into.
    ///
    /// Needs a real window, because the height to flip against and the
    /// pixel density to scale by are SDL's answers and not this crate's, so
    /// it is skipped where no window can be made.
    #[test]
    fn a_subviews_frame_reaches_the_window_in_pixels() {
        crate::bootstrap();
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        // SAFETY: `-initWithContentRect:styleMask:backing:defer:` is
        // `id (id, SEL, NSRect, NSUInteger, NSUInteger, BOOL)`.
        let made = unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 400.0, 300.0))
        };
        if !made {
            return;
        }
        // SAFETY: a window this test made.
        let sdl = unsafe { crate::window::sdl_window(window) };
        let Some((_, height)) = sdl3::video::window_size(sdl) else {
            return;
        };
        let density = sdl3::video::window_pixel_density(sdl) as f64;

        let content = view(rect(0.0, 0.0, f64::from(height), f64::from(height)));
        // SAFETY: both are objects this runtime allocated.
        unsafe { set_window(content, window) };
        let child = view(rect(50.0, 40.0, 100.0, 80.0));
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(content, objc::sel::get("addSubview:"), child) };

        // SAFETY: a view of this crate's.
        let frame = unsafe { presentation_frame(child) }.expect("the view is in a window");
        assert_eq!(frame.window, sdl);
        // AppKit counts the origin up from the bottom of the content rect;
        // the answer counts down from the top of it.
        assert_eq!(frame.x, (50.0 * density).round() as i32);
        assert_eq!(
            frame.y,
            ((f64::from(height) - (40.0 + 80.0)) * density).round() as i32
        );
        assert_eq!(frame.width, (100.0 * density).round() as u32);
        assert_eq!(frame.height, (80.0 * density).round() as u32);
        // A subview of that size is not the window.
        assert!(!frame.covers_window());

        // A view the size of the window is, which is what puts a game's one
        // layer on the direct path.
        let whole = view(rect(0.0, 0.0, 400.0, f64::from(height)));
        unsafe { add_subview(content, objc::sel::get("addSubview:"), whole) };
        // SAFETY: a view of this crate's.
        let frame = unsafe { presentation_frame(whole) }.expect("the view is in a window");
        assert!(frame.covers_window());
    }

    /// A view with no window has nowhere to be presented, which is what
    /// sends a drawable down the readback path instead.
    #[test]
    fn a_view_outside_any_window_has_no_presentation_frame() {
        let orphan = view(rect(0.0, 0.0, 100.0, 100.0));
        // SAFETY: a view of this crate's.
        assert!(unsafe { presentation_frame(orphan) }.is_none());
        // SAFETY: null is one of the values this accepts.
        assert!(unsafe { presentation_frame(std::ptr::null_mut()) }.is_none());
    }

    #[test]
    fn clips_to_bounds_round_trips_and_syncs_to_layer() {
        let view = view(rect(0.0, 0.0, 10.0, 10.0));
        let set_clips = objc::sel::get("setClipsToBounds:");
        let get_clips = objc::sel::get("clipsToBounds");
        let sel_wants_layer = objc::sel::get("setWantsLayer:");
        unsafe {
            assert_eq!(clips_to_bounds(view, get_clips), 0);
            set_clips_to_bounds(view, set_clips, 1);
            assert_eq!(clips_to_bounds(view, get_clips), 1);
            set_wants_layer(view, sel_wants_layer, 1);
            let layer = view_of(view).layer;
            assert!(!layer.is_null());
            let get_masks = objc::sel::get("masksToBounds");
            let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
            assert_eq!(f(layer, get_masks), 1);

            set_clips_to_bounds(view, set_clips, 0);
            assert_eq!(clips_to_bounds(view, get_clips), 0);
            assert_eq!(f(layer, get_masks), 0);
        }
        objc::arc::release(view);
    }
}
