//! `NSScrollView` and `NSClipView`, as far as a document that fits needs
//! them: which view is the document, and which view clips it.
//!
//! FIXME (ACCURACY): nothing scrolls. A document larger than its clip view
//! shows its top-left corner and the scrollers are not drawn.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

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

unsafe extern "C" fn ignored_integer(_receiver: Id, _sel: Sel, _value: usize) {}

unsafe extern "C" fn zero(_receiver: Id, _sel: Sel) -> usize {
    0
}

/// `-scrollToPoint:` and `-reflectScrolledClipView:`, which move nothing
/// here: see the module note.
unsafe extern "C" fn ignored_point(
    _receiver: Id,
    _sel: Sel,
    _point: foundation::geometry::NSPoint,
) {
}

pub(crate) fn install() {
    let scroll = objc::class::lookup(c"NSScrollView").expect("declared in crate::classes");
    methods!(scroll, {
        "documentView" => scroll_document_view, c"@16@0:8";
        "setDocumentView:" => scroll_set_document_view, c"v24@0:8@16";
        "contentView" => scroll_content_view, c"@16@0:8";
        "setContentView:" => scroll_set_content_view, c"v24@0:8@16";
        "setHasVerticalScroller:" => ignored_flag, c"v20@0:8c16";
        "setHasHorizontalScroller:" => ignored_flag, c"v20@0:8c16";
        "hasVerticalScroller" => no, c"c16@0:8";
        "hasHorizontalScroller" => no, c"c16@0:8";
        "setAutohidesScrollers:" => ignored_flag, c"v20@0:8c16";
        "setDrawsBackground:" => ignored_flag, c"v20@0:8c16";
        "setBackgroundColor:" => ignored_object, c"v24@0:8@16";
        "setBorderType:" => ignored_integer, c"v24@0:8Q16";
        "borderType" => zero, c"Q16@0:8";
        "reflectScrolledClipView:" => ignored_object, c"v24@0:8@16";
    });
    // Rubber-banding, which nothing here does; kept for reading back.
    crate::props::properties!(scroll, {
        integer "verticalScrollElasticity", "setVerticalScrollElasticity:";
        integer "horizontalScrollElasticity", "setHorizontalScrollElasticity:";
    });
    let clip = objc::class::lookup(c"NSClipView").expect("declared in crate::classes");
    methods!(clip, {
        "documentView" => clip_document_view, c"@16@0:8";
        "setDocumentView:" => clip_set_document_view, c"v24@0:8@16";
        "setDrawsBackground:" => ignored_flag, c"v20@0:8c16";
        "setBackgroundColor:" => ignored_object, c"v24@0:8@16";
        "scrollToPoint:" => ignored_point, c"v32@0:8{CGPoint=dd}16";
    });
}
