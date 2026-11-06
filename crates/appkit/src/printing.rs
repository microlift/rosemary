//! `NSPrintInfo`, `NSPrintOperation` and `NSPrintPanel`: a Mac without a
//! printer. There is no spooler and no printer driver behind any of this, so
//! every path that would hand a job to one instead answers the way the panel
//! would if the user cancelled - `NSModalResponseCancel`, not a crash, and
//! not a job that silently vanishes.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use foundation::geometry::{CGFloat, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// `NSModalResponseCancel`; see [`crate::panel`]'s own copy of the same value.
const CANCEL: i64 = 0;

#[repr(C)]
pub struct PrintInfo {
    isa: *const c_void,
    paper_width: CGFloat,
    paper_height: CGFloat,
    top_margin: CGFloat,
    bottom_margin: CGFloat,
    left_margin: CGFloat,
    right_margin: CGFloat,
    scaling_factor: CGFloat,
    orientation: isize,
    horizontally_centered: usize,
    vertically_centered: usize,
    job_disposition: Id,
}

pub const INFO_INSTANCE_SIZE: usize = size_of::<PrintInfo>();

#[repr(C)]
pub struct PrintOperation {
    isa: *const c_void,
    view: Id,
    print_info: Id,
    shows_print_panel: usize,
    shows_progress_panel: usize,
}

pub const OPERATION_INSTANCE_SIZE: usize = size_of::<PrintOperation>();

#[repr(C)]
pub struct PrintPanel {
    isa: *const c_void,
    /// `NSPrintPanelOptions`. Kept and handed back, and otherwise unread:
    /// there is no panel on screen whose sections it would show or hide.
    options: usize,
}

pub const PANEL_INSTANCE_SIZE: usize = size_of::<PrintPanel>();

static SHARED_INFO: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static CURRENT_OPERATION: AtomicUsize = AtomicUsize::new(0);

fn info_of<'a>(receiver: Id) -> &'a mut PrintInfo {
    // SAFETY: a `PrintInfo` instance.
    unsafe { &mut *(receiver as *mut PrintInfo) }
}

fn operation_of<'a>(receiver: Id) -> &'a mut PrintOperation {
    // SAFETY: a `PrintOperation` instance.
    unsafe { &mut *(receiver as *mut PrintOperation) }
}

fn panel_of<'a>(receiver: Id) -> &'a mut PrintPanel {
    // SAFETY: a `PrintPanel` instance.
    unsafe { &mut *(receiver as *mut PrintPanel) }
}

/// Apple's own defaults for a fresh `NSPrintInfo`: US Letter, centred, full
/// scale. The margins are this crate's own round number, not read from any
/// locale or printer - there is neither here.
fn reset(info: &mut PrintInfo) {
    info.paper_width = 612.0;
    info.paper_height = 792.0;
    info.top_margin = 72.0;
    info.bottom_margin = 72.0;
    info.left_margin = 72.0;
    info.right_margin = 72.0;
    info.scaling_factor = 1.0;
    info.orientation = 0;
    info.horizontally_centered = 1;
    info.vertically_centered = 1;
    let old = info.job_disposition;
    info.job_disposition = objc::arc::retain(foundation::string::from_str("NSPrintSpoolJob"));
    objc::arc::release(old);
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    reset(info_of(receiver));
    receiver
}

/// `-initWithDictionary:`: Apple's reads back the attributes the dictionary
/// names and leaves the rest at their defaults.
///
/// FIXME (ACCURACY): the dictionary's entries are not applied - a fresh
/// `NSPrintInfo` is returned instead of one carrying the caller's values,
/// since this class is not itself backed by the dictionary it hands out (see
/// [`dictionary`]).
unsafe extern "C" fn init_with_dictionary(receiver: Id, sel: Sel, _attributes: Id) -> Id {
    unsafe { init(receiver, sel) }
}

/// `-dictionary`: a snapshot, built fresh each call under this crate's own
/// keys - nothing here reads a `NSPrintInfo`'s dictionary back by Apple's
/// documented key names, so the names are free to be this file's own.
unsafe extern "C" fn dictionary(receiver: Id, _sel: Sel) -> Id {
    use corefoundation::collections::Number;
    let info = info_of(receiver);
    let numbers: [(&str, Id); 8] = [
        (
            "RosemaryPaperWidth",
            corefoundation::collections::new_number(Number::Float(info.paper_width)) as Id,
        ),
        (
            "RosemaryPaperHeight",
            corefoundation::collections::new_number(Number::Float(info.paper_height)) as Id,
        ),
        (
            "RosemaryTopMargin",
            corefoundation::collections::new_number(Number::Float(info.top_margin)) as Id,
        ),
        (
            "RosemaryBottomMargin",
            corefoundation::collections::new_number(Number::Float(info.bottom_margin)) as Id,
        ),
        (
            "RosemaryLeftMargin",
            corefoundation::collections::new_number(Number::Float(info.left_margin)) as Id,
        ),
        (
            "RosemaryRightMargin",
            corefoundation::collections::new_number(Number::Float(info.right_margin)) as Id,
        ),
        (
            "RosemaryScalingFactor",
            corefoundation::collections::new_number(Number::Float(info.scaling_factor)) as Id,
        ),
        (
            "RosemaryOrientation",
            corefoundation::collections::new_number(Number::Integer(info.orientation as i64)) as Id,
        ),
    ];
    let keys: Vec<Id> = numbers
        .iter()
        .map(|(key, _)| foundation::string::from_str(key))
        .chain(std::iter::once(foundation::string::from_str(
            "RosemaryJobDisposition",
        )))
        .collect();
    let pairs: Vec<(
        corefoundation::base::CFTypeRef,
        corefoundation::base::CFTypeRef,
    )> = keys
        .iter()
        .zip(
            numbers
                .iter()
                .map(|(_, v)| *v)
                .chain(std::iter::once(info.job_disposition)),
        )
        .map(|(key, value)| (*key as _, value as _))
        .collect();
    let dict = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        true,
    );
    for key in keys {
        objc::arc::release(key);
    }
    for (_, number) in numbers {
        objc::arc::release(number);
    }
    objc::arc::autorelease(dict as Id)
}

unsafe extern "C" fn orientation(receiver: Id, _sel: Sel) -> isize {
    info_of(receiver).orientation
}

unsafe extern "C" fn set_orientation(receiver: Id, _sel: Sel, value: isize) {
    info_of(receiver).orientation = value;
}

unsafe extern "C" fn paper_size(receiver: Id, _sel: Sel) -> NSSize {
    let info = info_of(receiver);
    NSSize {
        width: info.paper_width,
        height: info.paper_height,
    }
}

unsafe extern "C" fn set_paper_size(receiver: Id, _sel: Sel, size: NSSize) {
    let info = info_of(receiver);
    info.paper_width = size.width;
    info.paper_height = size.height;
}

macro_rules! margin_accessors {
    ($get:ident, $set:ident, $field:ident) => {
        unsafe extern "C" fn $get(receiver: Id, _sel: Sel) -> CGFloat {
            info_of(receiver).$field
        }

        unsafe extern "C" fn $set(receiver: Id, _sel: Sel, value: CGFloat) {
            info_of(receiver).$field = value;
        }
    };
}

margin_accessors!(top_margin, set_top_margin, top_margin);
margin_accessors!(bottom_margin, set_bottom_margin, bottom_margin);
margin_accessors!(left_margin, set_left_margin, left_margin);
margin_accessors!(right_margin, set_right_margin, right_margin);

unsafe extern "C" fn scaling_factor(receiver: Id, _sel: Sel) -> CGFloat {
    info_of(receiver).scaling_factor
}

unsafe extern "C" fn set_scaling_factor(receiver: Id, _sel: Sel, value: CGFloat) {
    info_of(receiver).scaling_factor = value;
}

unsafe extern "C" fn is_horizontally_centered(receiver: Id, _sel: Sel) -> u8 {
    info_of(receiver).horizontally_centered as u8
}

unsafe extern "C" fn set_horizontally_centered(receiver: Id, _sel: Sel, value: u8) {
    info_of(receiver).horizontally_centered = value as usize;
}

unsafe extern "C" fn is_vertically_centered(receiver: Id, _sel: Sel) -> u8 {
    info_of(receiver).vertically_centered as u8
}

unsafe extern "C" fn set_vertically_centered(receiver: Id, _sel: Sel, value: u8) {
    info_of(receiver).vertically_centered = value as usize;
}

unsafe extern "C" fn job_disposition(receiver: Id, _sel: Sel) -> Id {
    info_of(receiver).job_disposition
}

unsafe extern "C" fn set_job_disposition(receiver: Id, _sel: Sel, value: Id) {
    let info = info_of(receiver);
    let old = info.job_disposition;
    info.job_disposition = objc::arc::retain(value);
    objc::arc::release(old);
}

/// `-printer`: no printer exists on this host, the same answer a Mac with
/// none configured gives.
unsafe extern "C" fn printer(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

unsafe extern "C" fn set_printer(_receiver: Id, _sel: Sel, _printer: Id) {}

unsafe extern "C" fn info_dealloc(receiver: Id, _sel: Sel) {
    objc::arc::release(info_of(receiver).job_disposition);
    objc::arc::finalize(receiver);
}

/// `+sharedPrintInfo`: one process-lifetime default, made on first use and
/// never replaced by `+setSharedPrintInfo:` here - nothing reads it back
/// across runs, since there is nowhere for a Mac without a printer to save
/// page setup to.
unsafe extern "C" fn shared_print_info(_class: Id, _sel: Sel) -> Id {
    *SHARED_INFO.get_or_init(|| {
        let class = objc::class::lookup(c"NSPrintInfo").expect("declared before its methods");
        let info = objc::object::create_instance(class, 0);
        reset(info_of(info));
        objc::arc::make_immortal(info);
        info as usize
    }) as Id
}

// -- NSPrintOperation --

/// # Safety
///
/// `selector` names a method taking no arguments and returning an object.
unsafe fn send0(receiver: Id, selector: &str) -> Id {
    if receiver.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's obligation.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector))
    }
}

fn new_operation() -> Id {
    let class = objc::class::lookup(c"NSPrintOperation").expect("declared before its methods");
    objc::object::create_instance(class, 0)
}

unsafe extern "C" fn print_operation_with_view(_class: Id, _sel: Sel, view: Id) -> Id {
    let operation = new_operation();
    let state = operation_of(operation);
    state.view = objc::arc::retain(view);
    let info_class = objc::class::ptr(objc::class::lookup(c"NSPrintInfo").expect("declared")) as Id;
    // SAFETY: `+sharedPrintInfo` takes no arguments and returns an object.
    state.print_info = objc::arc::retain(unsafe { send0(info_class, "sharedPrintInfo") });
    state.shows_print_panel = 1;
    state.shows_progress_panel = 1;
    objc::arc::autorelease(operation)
}

unsafe extern "C" fn print_operation_with_view_print_info(
    _class: Id,
    _sel: Sel,
    view: Id,
    print_info: Id,
) -> Id {
    let operation = new_operation();
    let state = operation_of(operation);
    state.view = objc::arc::retain(view);
    state.print_info = objc::arc::retain(print_info);
    state.shows_print_panel = 1;
    state.shows_progress_panel = 1;
    objc::arc::autorelease(operation)
}

unsafe extern "C" fn current_operation(_class: Id, _sel: Sel) -> Id {
    CURRENT_OPERATION.load(Ordering::Acquire) as Id
}

unsafe extern "C" fn set_current_operation(_class: Id, _sel: Sel, value: Id) {
    let old = CURRENT_OPERATION.swap(value as usize, Ordering::AcqRel) as Id;
    if value != old {
        objc::arc::retain(value);
        objc::arc::release(old);
    }
}

unsafe extern "C" fn operation_view(receiver: Id, _sel: Sel) -> Id {
    operation_of(receiver).view
}

unsafe extern "C" fn set_operation_view(receiver: Id, _sel: Sel, value: Id) {
    let state = operation_of(receiver);
    let old = state.view;
    state.view = objc::arc::retain(value);
    objc::arc::release(old);
}

unsafe extern "C" fn operation_print_info(receiver: Id, _sel: Sel) -> Id {
    operation_of(receiver).print_info
}

unsafe extern "C" fn set_operation_print_info(receiver: Id, _sel: Sel, value: Id) {
    let state = operation_of(receiver);
    let old = state.print_info;
    state.print_info = objc::arc::retain(value);
    objc::arc::release(old);
}

unsafe extern "C" fn shows_print_panel(receiver: Id, _sel: Sel) -> u8 {
    operation_of(receiver).shows_print_panel as u8
}

unsafe extern "C" fn set_shows_print_panel(receiver: Id, _sel: Sel, value: u8) {
    operation_of(receiver).shows_print_panel = value as usize;
}

unsafe extern "C" fn shows_progress_panel(receiver: Id, _sel: Sel) -> u8 {
    operation_of(receiver).shows_progress_panel as u8
}

unsafe extern "C" fn set_shows_progress_panel(receiver: Id, _sel: Sel, value: u8) {
    operation_of(receiver).shows_progress_panel = value as usize;
}

/// `-runOperation`: there is no printer to spool to, and (when the operation
/// asks to show one) no print panel that could come back with anything but
/// cancel - so this always answers NO, bracketed by `+currentOperation` the
/// way Apple's documentation says a real run is.
unsafe extern "C" fn run_operation(receiver: Id, _sel: Sel) -> u8 {
    let previous = CURRENT_OPERATION.swap(receiver as usize, Ordering::AcqRel) as Id;
    objc::arc::retain(receiver);
    let result = 0;
    let restored = CURRENT_OPERATION.swap(previous as usize, Ordering::AcqRel) as Id;
    objc::arc::release(restored);
    result
}

/// `-runOperationModalForWindow:delegate:didRunSelector:contextInfo:`: the
/// legacy modal entry point, which still ends by telling the delegate -
/// `didRunSelector` is a guest selector on a guest delegate, sent exactly as
/// `-runOperation`'s caller would see it called directly.
unsafe extern "C" fn run_operation_modal_for_window(
    receiver: Id,
    _sel: Sel,
    _window: Id,
    delegate: Id,
    did_run_selector: Sel,
    context_info: *mut c_void,
) {
    let success = unsafe { run_operation(receiver, objc::sel::get("runOperation")) };
    if delegate.is_null() || did_run_selector.is_null() {
        return;
    }
    // SAFETY: `didRunSelector:` is `void (id, SEL, id, BOOL, void *)`, the
    // shape Apple's documentation gives it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, u8, *mut c_void) =
            std::mem::transmute(objc::msg::entry());
        f(delegate, did_run_selector, receiver, success, context_info);
    }
}

unsafe extern "C" fn operation_dealloc(receiver: Id, _sel: Sel) {
    let state = operation_of(receiver);
    objc::arc::release(state.view);
    objc::arc::release(state.print_info);
    if CURRENT_OPERATION.load(Ordering::Acquire) == receiver as usize {
        CURRENT_OPERATION.store(0, Ordering::Release);
    }
    objc::arc::finalize(receiver);
}

// -- NSPrintPanel --

unsafe extern "C" fn print_panel(_class: Id, _sel: Sel) -> Id {
    let class = objc::class::lookup(c"NSPrintPanel").expect("declared before its methods");
    let panel = objc::object::create_instance(class, 0);
    objc::arc::autorelease(panel)
}

unsafe extern "C" fn panel_options(receiver: Id, _sel: Sel) -> usize {
    panel_of(receiver).options
}

unsafe extern "C" fn set_panel_options(receiver: Id, _sel: Sel, value: usize) {
    panel_of(receiver).options = value;
}

/// `-runModal`: the same cancel a user declining to print would leave
/// behind - see the module doc.
unsafe extern "C" fn run_modal(_receiver: Id, _sel: Sel) -> i64 {
    CANCEL
}

/// `-beginSheetWithPrintInfo:modalForWindow:completionHandler:`: the sheet
/// never appears, and the handler hears the cancel at once.
unsafe extern "C" fn begin_sheet_with_print_info(
    _receiver: Id,
    _sel: Sel,
    _print_info: Id,
    _window: Id,
    handler: Id,
) {
    let Some(invoke) = (unsafe { objc::block::invoke(handler) }) else {
        return;
    };
    // SAFETY: the completion handler is `void (^)(NSInteger)`.
    let call: unsafe extern "C" fn(Id, i64) = unsafe { std::mem::transmute(invoke) };
    unsafe { call(handler, CANCEL) };
}

unsafe extern "C" fn panel_dealloc(receiver: Id, _sel: Sel) {
    objc::arc::finalize(receiver);
}

pub(crate) fn install() {
    let info = objc::class::lookup(c"NSPrintInfo").expect("declared before its methods");
    class_methods!(info, {
        "sharedPrintInfo" => shared_print_info, c"@16@0:8";
    });
    methods!(info, {
        "init" => init, c"@16@0:8";
        "initWithDictionary:" => init_with_dictionary, c"@24@0:8@16";
        "dictionary" => dictionary, c"@16@0:8";
        "orientation" => orientation, c"q16@0:8";
        "setOrientation:" => set_orientation, c"v24@0:8q16";
        "paperSize" => paper_size, c"{CGSize=dd}16@0:8";
        "setPaperSize:" => set_paper_size, c"v32@0:8{CGSize=dd}16";
        "topMargin" => top_margin, c"d16@0:8";
        "setTopMargin:" => set_top_margin, c"v24@0:8d16";
        "bottomMargin" => bottom_margin, c"d16@0:8";
        "setBottomMargin:" => set_bottom_margin, c"v24@0:8d16";
        "leftMargin" => left_margin, c"d16@0:8";
        "setLeftMargin:" => set_left_margin, c"v24@0:8d16";
        "rightMargin" => right_margin, c"d16@0:8";
        "setRightMargin:" => set_right_margin, c"v24@0:8d16";
        "scalingFactor" => scaling_factor, c"d16@0:8";
        "setScalingFactor:" => set_scaling_factor, c"v24@0:8d16";
        "isHorizontallyCentered" => is_horizontally_centered, c"B16@0:8";
        "setHorizontallyCentered:" => set_horizontally_centered, c"v24@0:8B16";
        "isVerticallyCentered" => is_vertically_centered, c"B16@0:8";
        "setVerticallyCentered:" => set_vertically_centered, c"v24@0:8B16";
        "jobDisposition" => job_disposition, c"@16@0:8";
        "setJobDisposition:" => set_job_disposition, c"v24@0:8@16";
        "printer" => printer, c"@16@0:8";
        "setPrinter:" => set_printer, c"v24@0:8@16";
        "dealloc" => info_dealloc, c"v16@0:8";
    });

    let operation = objc::class::lookup(c"NSPrintOperation").expect("declared before its methods");
    class_methods!(operation, {
        "printOperationWithView:" => print_operation_with_view, c"@24@0:8@16";
        "printOperationWithView:printInfo:" => print_operation_with_view_print_info, c"@32@0:8@16@24";
        "currentOperation" => current_operation, c"@16@0:8";
        "setCurrentOperation:" => set_current_operation, c"v24@0:8@16";
    });
    methods!(operation, {
        "view" => operation_view, c"@16@0:8";
        "setView:" => set_operation_view, c"v24@0:8@16";
        "printInfo" => operation_print_info, c"@16@0:8";
        "setPrintInfo:" => set_operation_print_info, c"v24@0:8@16";
        "showsPrintPanel" => shows_print_panel, c"B16@0:8";
        "setShowsPrintPanel:" => set_shows_print_panel, c"v24@0:8B16";
        "showsProgressPanel" => shows_progress_panel, c"B16@0:8";
        "setShowsProgressPanel:" => set_shows_progress_panel, c"v24@0:8B16";
        "runOperation" => run_operation, c"B16@0:8";
        "runOperationModalForWindow:delegate:didRunSelector:contextInfo:" =>
            run_operation_modal_for_window, c"v40@0:8@16@24:32^v36";
        "dealloc" => operation_dealloc, c"v16@0:8";
    });

    let panel = objc::class::lookup(c"NSPrintPanel").expect("declared before its methods");
    class_methods!(panel, {
        "printPanel" => print_panel, c"@16@0:8";
    });
    methods!(panel, {
        "options" => panel_options, c"Q16@0:8";
        "setOptions:" => set_panel_options, c"v24@0:8Q16";
        "runModal" => run_modal, c"q16@0:8";
        "beginSheetWithPrintInfo:modalForWindow:completionHandler:" =>
            begin_sheet_with_print_info, c"v32@0:8@16@24@?";
        "dealloc" => panel_dealloc, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_print_info_is_letter_sized_and_centered() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSPrintInfo").expect("declared");
        let info = objc::object::create_instance(class, 0);
        unsafe { init(info, objc::sel::get("init")) };
        let size = unsafe { paper_size(info, objc::sel::get("paperSize")) };
        assert_eq!((size.width, size.height), (612.0, 792.0));
        assert_eq!(
            unsafe { is_horizontally_centered(info, objc::sel::get("isHorizontallyCentered")) },
            1
        );
        unsafe { info_dealloc(info, objc::sel::get("dealloc")) };
    }

    #[test]
    fn a_print_panel_cancels_without_a_printer() {
        crate::bootstrap();
        assert_eq!(
            unsafe { run_modal(std::ptr::null_mut(), objc::sel::get("runModal")) },
            CANCEL
        );
    }

    #[test]
    fn running_an_operation_tracks_it_as_current_and_then_clears_it() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSPrintOperation").expect("declared");
        let operation = objc::object::create_instance(class, 0);
        let result = unsafe { run_operation(operation, objc::sel::get("runOperation")) };
        assert_eq!(result, 0);
        assert_eq!(CURRENT_OPERATION.load(Ordering::Acquire), 0);
        objc::arc::release(operation);
    }
}
