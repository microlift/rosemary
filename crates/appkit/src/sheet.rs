//! Sheets: a window attached to another as a document-modal dialog.
//!
//! FIXME (ACCURACY): a sheet is its own window placed over the top of its
//! parent's content, not one slid out from under the title bar, and the parent
//! goes on taking input. A toolkit that blocks its window-modal parent, as Qt
//! does, blocks it itself.

use std::cell::RefCell;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use foundation::geometry::{NSPoint, NSRect};

pub const WILL_BEGIN: &str = "NSWindowWillBeginSheetNotification";
pub const DID_END: &str = "NSWindowDidEndSheetNotification";

/// `NSModalResponseStop`, which `-endSheet:` ends a sheet with.
pub const RESPONSE_STOP: i64 = -1000;

struct Sheet {
    parent: usize,
    sheet: usize,
    /// Called once, with the code the sheet ended with.
    ended: Box<dyn FnOnce(i64)>,
}

thread_local! {
    /// Oldest first. The main thread's, where windows are made and answered.
    static SHEETS: RefCell<Vec<Sheet>> = const { RefCell::new(Vec::new()) };
}

/// Record `sheet` as attached to `parent`; `ended` hears the code it ends with.
pub(crate) fn attach(parent: Id, sheet: Id, ended: Box<dyn FnOnce(i64)>) {
    SHEETS.with_borrow_mut(|sheets| {
        sheets.push(Sheet {
            parent: parent as usize,
            sheet: sheet as usize,
            ended,
        })
    });
    crate::event::post_for(parent, WILL_BEGIN);
}

/// End `sheet` with `code`: detach it, order it out, and run its handler.
/// False when it is not attached to anything.
pub(crate) fn end(sheet: Id, code: i64) -> bool {
    let Some(ended) = SHEETS.with_borrow_mut(|sheets| {
        let at = sheets.iter().position(|s| s.sheet == sheet as usize)?;
        Some(sheets.remove(at))
    }) else {
        return false;
    };
    send(sheet, "orderOut:", std::ptr::null_mut());
    crate::event::post_for(ended.parent as Id, DID_END);
    // Out of the borrow: a handler often begins the next sheet.
    (ended.ended)(code);
    true
}

/// Drop every attachment `window` is part of, without running the handlers:
/// it is being freed, and AppKit ends no sheet on its behalf.
pub(crate) fn forget(window: Id) {
    let gone: Vec<Sheet> = SHEETS.with_borrow_mut(|sheets| {
        let (gone, kept) = std::mem::take(sheets)
            .into_iter()
            .partition(|s| s.parent == window as usize || s.sheet == window as usize);
        *sheets = kept;
        gone
    });
    drop(gone);
}

pub(crate) fn parent_of(sheet: Id) -> Option<Id> {
    SHEETS.with_borrow(|sheets| {
        sheets
            .iter()
            .find(|s| s.sheet == sheet as usize)
            .map(|s| s.parent as Id)
    })
}

/// The sheets attached to `parent`, oldest first.
fn attached_to(parent: Id) -> Vec<Id> {
    SHEETS.with_borrow(|sheets| {
        sheets
            .iter()
            .filter(|s| s.parent == parent as usize)
            .map(|s| s.sheet as Id)
            .collect()
    })
}

fn send(receiver: Id, selector: &str, argument: Id) {
    // SAFETY: every selector this is used with is `void (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), argument);
    }
}

fn rect(receiver: Id, selector: &str) -> NSRect {
    // SAFETY: `-frame` is `NSRect (id, SEL)`, returned in memory.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> NSRect =
            std::mem::transmute(objc::msg::entry_stret());
        f(receiver, objc::sel::get(selector))
    }
}

/// Centre `sheet` across the top of `parent`'s content, where AppKit hangs it.
fn place(parent: Id, sheet: Id) {
    let frame = rect(parent, "frame");
    // SAFETY: `-contentRectForFrameRect:` is `NSRect (id, SEL, NSRect)`.
    let content = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) -> NSRect =
            std::mem::transmute(objc::msg::entry_stret());
        f(parent, objc::sel::get("contentRectForFrameRect:"), frame)
    };
    let size = rect(sheet, "frame").size;
    let top_left = NSPoint {
        x: content.origin.x + (content.size.width - size.width) / 2.0,
        y: content.origin.y + content.size.height,
    };
    // SAFETY: `-setFrameTopLeftPoint:` is `void (id, SEL, NSPoint)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSPoint) = std::mem::transmute(objc::msg::entry());
        f(sheet, objc::sel::get("setFrameTopLeftPoint:"), top_left);
    }
}

/// `-beginSheet:completionHandler:` and `-beginCriticalSheet:completionHandler:`.
///
/// A critical sheet is shown at once even over another one; this shows every
/// sheet at once, so the two are the same.
unsafe extern "C" fn begin_sheet(receiver: Id, _sel: Sel, sheet: Id, handler: Id) {
    if sheet.is_null() {
        return;
    }
    let handler = match handler.is_null() {
        true => None,
        false => Some(objc::block::copy(handler) as usize),
    };
    place(receiver, sheet);
    attach(
        receiver,
        sheet,
        Box::new(move |code| {
            let Some(handler) = handler.map(|h| h as Id) else {
                return;
            };
            // SAFETY: `void (^)(NSModalResponse)`, as the selector declares.
            if let Some(invoke) = unsafe { objc::block::invoke(handler) } {
                unsafe {
                    let f: unsafe extern "C" fn(Id, i64) = std::mem::transmute(invoke);
                    f(handler, code);
                }
            }
            objc::block::release(handler);
        }),
    );
    send(sheet, "makeKeyAndOrderFront:", std::ptr::null_mut());
}

unsafe extern "C" fn end_sheet(_receiver: Id, _sel: Sel, sheet: Id) {
    end(sheet, RESPONSE_STOP);
}

unsafe extern "C" fn end_sheet_with_code(_receiver: Id, _sel: Sel, sheet: Id, code: i64) {
    end(sheet, code);
}

unsafe extern "C" fn is_sheet(receiver: Id, _sel: Sel) -> u8 {
    parent_of(receiver).is_some() as u8
}

unsafe extern "C" fn sheet_parent(receiver: Id, _sel: Sel) -> Id {
    parent_of(receiver).unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn attached_sheet(receiver: Id, _sel: Sel) -> Id {
    attached_to(receiver)
        .first()
        .copied()
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn sheets(receiver: Id, _sel: Sel) -> Id {
    let list: Vec<corefoundation::base::CFTypeRef> = attached_to(receiver)
        .into_iter()
        .map(|sheet| sheet as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &list,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array this call owns.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// `-[NSApplication endSheet:]` and `-endSheet:returnCode:`, the older
/// spelling, which find the parent themselves.
pub(crate) unsafe extern "C" fn application_end_sheet(receiver: Id, sel: Sel, sheet: Id) {
    unsafe { end_sheet(receiver, sel, sheet) };
}

pub(crate) unsafe extern "C" fn application_end_sheet_with_code(
    receiver: Id,
    sel: Sel,
    sheet: Id,
    code: i64,
) {
    unsafe { end_sheet_with_code(receiver, sel, sheet, code) };
}

pub(crate) fn install() {
    let window = objc::class::lookup(c"NSWindow").expect("declared in `classes`");
    methods!(window, {
        "beginSheet:completionHandler:" => begin_sheet, c"v32@0:8@16@?24";
        "beginCriticalSheet:completionHandler:" => begin_sheet, c"v32@0:8@16@?24";
        "endSheet:" => end_sheet, c"v24@0:8@16";
        "endSheet:returnCode:" => end_sheet_with_code, c"v32@0:8@16q24";
        "isSheet" => is_sheet, c"c16@0:8";
        "sheetParent" => sheet_parent, c"@16@0:8";
        "attachedSheet" => attached_sheet, c"@16@0:8";
        "sheets" => sheets, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_window() -> Id {
        crate::bootstrap();
        objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0)
    }

    fn ask(receiver: Id, selector: &str) -> Id {
        // SAFETY: every selector this is used with is `id (id, SEL)`, or a
        // BOOL, which comes back in the same register.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector))
        }
    }

    #[test]
    fn a_sheet_is_attached_until_its_parent_ends_it() {
        let (parent, sheet) = (new_window(), new_window());
        // SAFETY: two windows and no handler.
        unsafe {
            begin_sheet(
                parent,
                objc::sel::get("beginSheet:completionHandler:"),
                sheet,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(ask(sheet, "isSheet") as usize & 0xff, 1);
        assert_eq!(ask(sheet, "sheetParent"), parent);
        assert_eq!(ask(parent, "attachedSheet"), sheet);
        assert_eq!(ask(sheet, "isVisible") as usize & 0xff, 1);

        send(parent, "endSheet:", sheet);
        assert_eq!(ask(sheet, "isSheet") as usize & 0xff, 0);
        assert!(ask(parent, "attachedSheet").is_null());
        objc::arc::release(sheet);
        objc::arc::release(parent);
    }

    #[test]
    fn ending_a_sheet_hands_its_code_to_the_handler() {
        let (parent, sheet) = (new_window(), new_window());
        let heard = std::rc::Rc::new(std::cell::Cell::new(None));
        let into = std::rc::Rc::clone(&heard);
        attach(parent, sheet, Box::new(move |code| into.set(Some(code))));
        assert!(end(sheet, 42));
        assert_eq!(heard.get(), Some(42));
        assert!(!end(sheet, 42), "a sheet ends once");
        objc::arc::release(sheet);
        objc::arc::release(parent);
    }
}
