//! `NSScrollView` and `NSClipView`: which view is the document, which view
//! clips it, and where in the document the clip view looks - its bounds
//! origin, which moves the document under it.
//!
//! FIXME (ACCURACY): the scrollers are not drawn and cannot be dragged; the
//! wheel and `-scrollRectToVisible:` are what scroll.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

/// `(scroll or clip view, the view it names)`: a scroll view's clip view and
/// a clip view's document, neither retained - both are subviews already.
fn links() -> &'static Mutex<HashMap<usize, usize>> {
    static LINKS: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    LINKS.get_or_init(Default::default)
}

fn link(of: Id) -> Id {
    links()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(of as usize))
        .map_or(std::ptr::null_mut(), |view| *view as Id)
}

fn set_link(of: Id, to: Id) {
    let mut links = links().lock().unwrap_or_else(|e| e.into_inner());
    match to.is_null() {
        true => links.remove(&(of as usize)),
        false => links.insert(of as usize, to as usize),
    };
}

/// Drop the links of a view that is going away.
pub(crate) fn forget(view: Id) {
    links()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(view as usize));
}

fn send_view(receiver: Id, selector: &str, view: Id) {
    // SAFETY: each selector here is `void (id, SEL, NSView *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), view);
    }
}

/// Make `child` a subview of `parent` if it is not one already.
fn adopt(parent: Id, child: Id) {
    // SAFETY: both are views of this crate's, or nil.
    if child.is_null() || unsafe { crate::view::superview_of(child) } == parent {
        return;
    }
    send_view(parent, "addSubview:", child);
}

/// The clip view a scroll view shows its document through, made on first use.
fn clip_view_of(scroll: Id) -> Id {
    let existing = link(scroll);
    if !existing.is_null() {
        return existing;
    }
    let class = objc::class::lookup(c"NSClipView").expect("declared in crate::classes");
    let clip = objc::object::create_instance(class, 0);
    // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)` on a view.
    let clip = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, foundation::geometry::NSRect) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            clip,
            objc::sel::get("initWithFrame:"),
            crate::view::bounds_of(scroll),
        )
    };
    // The clip view fills the scroll view and follows it as it resizes.
    // SAFETY: `void (id, SEL, NSUInteger)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, usize) = std::mem::transmute(objc::msg::entry());
        f(clip, objc::sel::get("setAutoresizingMask:"), 2 | 16);
    }
    set_link(scroll, clip);
    adopt(scroll, clip);
    objc::arc::release(clip);
    clip
}

unsafe extern "C" fn scroll_document_view(receiver: Id, _sel: Sel) -> Id {
    link(link(receiver))
}

unsafe extern "C" fn scroll_set_document_view(receiver: Id, _sel: Sel, document: Id) {
    send_view(clip_view_of(receiver), "setDocumentView:", document);
}

unsafe extern "C" fn scroll_content_view(receiver: Id, _sel: Sel) -> Id {
    clip_view_of(receiver)
}

unsafe extern "C" fn scroll_set_content_view(receiver: Id, _sel: Sel, clip: Id) {
    let old = link(receiver);
    if old == clip {
        return;
    }
    if !old.is_null() {
        // SAFETY: `-removeFromSuperview` is `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
            f(old, objc::sel::get("removeFromSuperview"));
        }
    }
    set_link(receiver, clip);
    adopt(receiver, clip);
}

unsafe extern "C" fn clip_document_view(receiver: Id, _sel: Sel) -> Id {
    link(receiver)
}

unsafe extern "C" fn clip_set_document_view(receiver: Id, _sel: Sel, document: Id) {
    let old = link(receiver);
    if old == document {
        return;
    }
    if !old.is_null() {
        // SAFETY: `-removeFromSuperview` is `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
            f(old, objc::sel::get("removeFromSuperview"));
        }
    }
    set_link(receiver, document);
    adopt(receiver, document);
}

/// Settings a scroll view keeps for scrolling, which nothing here does.
unsafe extern "C" fn ignored_flag(_receiver: Id, _sel: Sel, _value: u8) {}

unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _value: Id) {}

unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> u8 {
    0
}

unsafe extern "C" fn nothing(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

unsafe extern "C" fn ignored(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn ignored_integer(_receiver: Id, _sel: Sel, _value: usize) {}

unsafe extern "C" fn zero(_receiver: Id, _sel: Sel) -> usize {
    0
}

fn is_kind(object: Id, name: &std::ffi::CStr) -> bool {
    objc::class::lookup(name).is_some_and(|class| {
        // SAFETY: a live object.
        unsafe { corefoundation::base::is_kind_of(object.cast(), class) }
    })
}

/// The document's frame, in the clip view's coordinates.
fn document_rect(clip: Id) -> NSRect {
    let document = link(clip);
    match document.is_null() {
        true => crate::view::bounds_of(clip),
        false => crate::view::frame_of(document),
    }
}

/// Where the clip view's bounds may go: inside the document, or at the
/// document's origin when the document is the smaller.
fn constrain(clip: Id, proposed: NSRect) -> NSRect {
    let document = document_rect(clip);
    let clamp =
        |at: f64, length: f64, start: f64, extent: f64| at.min(start + extent - length).max(start);
    NSRect {
        origin: NSPoint {
            x: clamp(
                proposed.origin.x,
                proposed.size.width,
                document.origin.x,
                document.size.width,
            ),
            y: clamp(
                proposed.origin.y,
                proposed.size.height,
                document.origin.y,
                document.size.height,
            ),
        },
        size: proposed.size,
    }
}

unsafe extern "C" fn clip_constrain_bounds_rect(
    receiver: Id,
    _sel: Sel,
    proposed: NSRect,
) -> NSRect {
    constrain(receiver, proposed)
}

/// `-scrollToPoint:`: constrained by `-constrainBoundsRect:`, sent so an
/// override has its say, then reflected in the scroll view.
unsafe extern "C" fn clip_scroll_to_point(receiver: Id, _sel: Sel, point: NSPoint) {
    let proposed = NSRect {
        origin: point,
        size: crate::view::bounds_of(receiver).size,
    };
    // SAFETY: `NSRect (id, SEL, NSRect)`, which returns through the `_stret`
    // entry.
    let allowed = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) -> NSRect =
            std::mem::transmute(objc::msg::entry_stret());
        f(receiver, objc::sel::get("constrainBoundsRect:"), proposed)
    };
    set_origin(receiver, allowed.origin);
    // SAFETY: a view of this crate's.
    let scroll = unsafe { crate::view::superview_of(receiver) };
    if is_kind(scroll, c"NSScrollView") {
        send_view(scroll, "reflectScrolledClipView:", receiver);
    }
}

/// Move the clip view's bounds origin, by message so an override sees it.
fn set_origin(clip: Id, origin: NSPoint) {
    // SAFETY: `void (id, SEL, NSPoint)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSPoint) = std::mem::transmute(objc::msg::entry());
        f(clip, objc::sel::get("setBoundsOrigin:"), origin);
    }
}

unsafe extern "C" fn clip_document_rect(receiver: Id, _sel: Sel) -> NSRect {
    document_rect(receiver)
}

/// `-documentVisibleRect`: the clip view's bounds, in the document's
/// coordinates.
unsafe extern "C" fn clip_document_visible_rect(receiver: Id, _sel: Sel) -> NSRect {
    let document = link(receiver);
    let bounds = crate::view::bounds_of(receiver);
    match document.is_null() {
        true => bounds,
        false => crate::view::convert_rect(bounds, receiver, document),
    }
}

/// A clip view is flipped when its document is, which is what keeps a flipped
/// document's top at the clip view's top.
unsafe extern "C" fn clip_is_flipped(receiver: Id, _sel: Sel) -> u8 {
    u8::from(crate::view::is_flipped(link(receiver)))
}

/// `-tile`: the clip view fills the scroll view.
unsafe extern "C" fn scroll_tile(receiver: Id, _sel: Sel) {
    let clip = clip_view_of(receiver);
    // SAFETY: `void (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(
            clip,
            objc::sel::get("setFrame:"),
            crate::view::bounds_of(receiver),
        );
    }
}

unsafe extern "C" fn scroll_content_size(receiver: Id, _sel: Sel) -> NSSize {
    crate::view::frame_of(clip_view_of(receiver)).size
}

unsafe extern "C" fn scroll_document_visible_rect(receiver: Id, sel: Sel) -> NSRect {
    // SAFETY: a clip view of this module's.
    unsafe { clip_document_visible_rect(clip_view_of(receiver), sel) }
}

/// `-scrollWheel:`: a wheel notch scrolls by the line scroll, in whichever
/// direction moves the content with the wheel.
unsafe extern "C" fn scroll_wheel(receiver: Id, _sel: Sel, event: Id) {
    let delta = |sel: &str| {
        // SAFETY: `CGFloat (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(objc::msg::entry());
            f(event, objc::sel::get(sel))
        }
    };
    let (dx, dy) = (delta("scrollingDeltaX"), delta("scrollingDeltaY"));
    let clip = clip_view_of(receiver);
    let mut origin = crate::view::bounds_of(clip).origin;
    let line = line_scroll(receiver);
    origin.x -= dx * line;
    origin.y += match crate::view::is_flipped(clip) {
        true => -dy * line,
        false => dy * line,
    };
    // SAFETY: a clip view of this module's.
    unsafe { clip_scroll_to_point(clip, objc::sel::get("scrollToPoint:"), origin) };
}

/// The distance one wheel line scrolls: `-verticalLineScroll`, or AppKit's
/// ten points when nothing set one.
fn line_scroll(scroll: Id) -> f64 {
    let set = f64::from_bits(crate::props::integer(scroll, "verticalLineScroll") as u64);
    match set > 0.0 {
        true => set,
        false => 10.0,
    }
}

/// `-[NSView scrollRectToVisible:]`: scroll the nearest clip view above the
/// receiver as little as makes `rect` visible. Yes if anything moved.
unsafe extern "C" fn view_scroll_rect_to_visible(receiver: Id, _sel: Sel, rect: NSRect) -> u8 {
    let mut clip = receiver;
    for _ in 0..64 {
        // SAFETY: a view of this crate's, or nil.
        clip = unsafe { crate::view::superview_of(clip) };
        if clip.is_null() || is_kind(clip, c"NSClipView") {
            break;
        }
    }
    if clip.is_null() {
        return 0;
    }
    let target = crate::view::convert_rect(rect, receiver, clip);
    let bounds = crate::view::bounds_of(clip);
    let fit = |at: f64, length: f64, start: f64, extent: f64| {
        if start < at || extent > length {
            start
        } else if start + extent > at + length {
            start + extent - length
        } else {
            at
        }
    };
    let origin = NSPoint {
        x: fit(
            bounds.origin.x,
            bounds.size.width,
            target.origin.x,
            target.size.width,
        ),
        y: fit(
            bounds.origin.y,
            bounds.size.height,
            target.origin.y,
            target.size.height,
        ),
    };
    if origin == bounds.origin {
        return 0;
    }
    // SAFETY: `void (id, SEL, NSPoint)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSPoint) = std::mem::transmute(objc::msg::entry());
        f(clip, objc::sel::get("scrollToPoint:"), origin);
    }
    u8::from(crate::view::bounds_of(clip).origin != bounds.origin)
}

/// `-[NSView scrollPoint:]`: the nearest clip view above scrolls to `point`.
unsafe extern "C" fn view_scroll_point(receiver: Id, sel: Sel, point: NSPoint) {
    let rect = NSRect {
        origin: point,
        size: NSSize::default(),
    };
    let mut clip = receiver;
    for _ in 0..64 {
        // SAFETY: a view of this crate's, or nil.
        clip = unsafe { crate::view::superview_of(clip) };
        if clip.is_null() || is_kind(clip, c"NSClipView") {
            break;
        }
    }
    if clip.is_null() {
        return;
    }
    let at = crate::view::convert_rect(rect, receiver, clip).origin;
    // SAFETY: a clip view.
    unsafe { clip_scroll_to_point(clip, sel, at) };
}

unsafe extern "C" fn view_autoscroll(_receiver: Id, _sel: Sel, _event: Id) -> u8 {
    0
}

pub(crate) fn install() {
    let scroll = objc::class::lookup(c"NSScrollView").expect("declared in crate::classes");
    methods!(scroll, {
        "documentView" => scroll_document_view, c"@16@0:8";
        "setDocumentView:" => scroll_set_document_view, c"v24@0:8@16";
        "contentView" => scroll_content_view, c"@16@0:8";
        "setContentView:" => scroll_set_content_view, c"v24@0:8@16";
        "documentVisibleRect" => scroll_document_visible_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "contentSize" => scroll_content_size, c"{CGSize=dd}16@0:8";
        "tile" => scroll_tile, c"v16@0:8";
        "scrollWheel:" => scroll_wheel, c"v24@0:8@16";
        "setHasVerticalScroller:" => ignored_flag, c"v20@0:8c16";
        "setHasHorizontalScroller:" => ignored_flag, c"v20@0:8c16";
        "hasVerticalScroller" => no, c"c16@0:8";
        "hasHorizontalScroller" => no, c"c16@0:8";
        "verticalScroller" => nothing, c"@16@0:8";
        "horizontalScroller" => nothing, c"@16@0:8";
        "setVerticalScroller:" => ignored_object, c"v24@0:8@16";
        "setHorizontalScroller:" => ignored_object, c"v24@0:8@16";
        "setAutohidesScrollers:" => ignored_flag, c"v20@0:8c16";
        "setDrawsBackground:" => ignored_flag, c"v20@0:8c16";
        "setBackgroundColor:" => ignored_object, c"v24@0:8@16";
        "setBorderType:" => ignored_integer, c"v24@0:8Q16";
        "borderType" => zero, c"Q16@0:8";
        "reflectScrolledClipView:" => ignored_object, c"v24@0:8@16";
        "flashScrollers" => ignored, c"v16@0:8";
        "setDocumentCursor:" => ignored_object, c"v24@0:8@16";
    });
    // Kept for reading back; only the line scroll is used.
    crate::props::properties!(scroll, {
        integer "verticalScrollElasticity", "setVerticalScrollElasticity:";
        integer "horizontalScrollElasticity", "setHorizontalScrollElasticity:";
        integer "scrollerKnobStyle", "setScrollerKnobStyle:";
        integer "scrollerStyle", "setScrollerStyle:";
        double "verticalLineScroll", "setVerticalLineScroll:";
        double "horizontalLineScroll", "setHorizontalLineScroll:";
        double "verticalPageScroll", "setVerticalPageScroll:";
        double "horizontalPageScroll", "setHorizontalPageScroll:";
        bool "scrollsDynamically", "setScrollsDynamically:";
        bool "automaticallyAdjustsContentInsets", "setAutomaticallyAdjustsContentInsets:";
        bool "allowsMagnification", "setAllowsMagnification:";
        bool "usesPredominantAxisScrolling", "setUsesPredominantAxisScrolling:";
    });
    let clip = objc::class::lookup(c"NSClipView").expect("declared in crate::classes");
    methods!(clip, {
        "documentView" => clip_document_view, c"@16@0:8";
        "setDocumentView:" => clip_set_document_view, c"v24@0:8@16";
        "setDrawsBackground:" => ignored_flag, c"v20@0:8c16";
        "setBackgroundColor:" => ignored_object, c"v24@0:8@16";
        "scrollToPoint:" => clip_scroll_to_point, c"v32@0:8{CGPoint=dd}16";
        "constrainBoundsRect:" => clip_constrain_bounds_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "documentRect" => clip_document_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "documentVisibleRect" => clip_document_visible_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "isFlipped" => clip_is_flipped, c"c16@0:8";
    });
    crate::props::properties!(clip, {
        bool "copiesOnScroll", "setCopiesOnScroll:";
    });
    let view = objc::class::lookup(c"NSView").expect("declared in crate::classes");
    methods!(view, {
        "scrollRectToVisible:" => view_scroll_rect_to_visible, c"c48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "scrollPoint:" => view_scroll_point, c"v32@0:8{CGPoint=dd}16";
        "autoscroll:" => view_autoscroll, c"c24@0:8@16";
    });
}
