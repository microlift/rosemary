//! `NSDocument` and `NSDocumentController`, enough of a document-based app's
//! launch path for a program that subclasses both: the window controllers it
//! owns, its file identity, and the controller's list of open documents.
//!
//! Reading and writing a document's actual contents is left to the subclass
//! that overrides `-readFromURL:ofType:error:` (and `-writeToURL:...:`, not
//! implemented here because nothing in this file ever calls it); this class's
//! own defaults just keep the bookkeeping Apple's documentation describes for
//! them - succeed trivially, so a subclass's `super` call is a no-op rather
//! than a crash.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use corefoundation::callbacks::Policy;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Document {
    isa: *const c_void,
    file_url: Id,
    file_type: Id,
    /// Set by `-setDisplayName:`; absent, `displayName` is derived from the
    /// file URL instead. See [`display_name`].
    display_name: Id,
    /// A real `NSMutableArray`, retained - every reader and mutator here is an
    /// ordinary Objective-C message, the same ones a guest would send.
    window_controllers: Id,
    change_count: isize,
    /// `-undoManager`, made lazily; owned once it exists.
    undo_manager: Id,
    /// `-hasUndoManager`. Apple defaults this to YES, which is what makes a
    /// bare `-undoManager` get a manager at all on a fresh document.
    has_undo_manager: usize,
}

pub const INSTANCE_SIZE: usize = size_of::<Document>();

#[repr(C)]
pub struct DocumentController {
    isa: *const c_void,
    /// A real `NSMutableArray` of the open documents, retained.
    documents: Id,
}

pub const CONTROLLER_INSTANCE_SIZE: usize = size_of::<DocumentController>();

/// The first `NSDocumentController` (or subclass) instance made, which is
/// what every later `+sharedDocumentController` answers - the same rule
/// [`crate::application::shared`] follows, and for the same reason: a
/// document-based app's custom controller is usually a nib's top-level
/// object, built before anything asks for the shared one by name.
static SHARED_CONTROLLER: AtomicUsize = AtomicUsize::new(0);

fn doc_of<'a>(receiver: Id) -> &'a mut Document {
    // SAFETY: a `Document` instance.
    unsafe { &mut *(receiver as *mut Document) }
}

fn controller_of<'a>(receiver: Id) -> &'a mut DocumentController {
    // SAFETY: a `DocumentController` instance.
    unsafe { &mut *(receiver as *mut DocumentController) }
}

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

/// # Safety
///
/// `selector` names a method taking one object and returning nothing.
unsafe fn send1_void(receiver: Id, selector: &str, argument: Id) {
    if receiver.is_null() {
        return;
    }
    // SAFETY: the caller's obligation.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), argument);
    }
}

fn new_mutable_array() -> Id {
    corefoundation::collections::new_array(&[], Policy::from_type_callbacks(), true) as Id
}

/// A shallow, immutable copy of a real array ivar, autoreleased - what
/// `-documents` and `-windowControllers` hand back, matching Apple's own
/// return type of `NSArray` rather than the mutable storage behind it.
fn array_snapshot(array: Id) -> Id {
    // SAFETY: a real array this file made with [`new_mutable_array`].
    unsafe { send0(array, "copy") }
}

/// # Safety
///
/// `url` is null or an `NSURL`.
unsafe fn file_path(url: Id) -> Option<String> {
    if url.is_null() {
        return None;
    }
    // SAFETY: the caller's obligation.
    unsafe { corefoundation::url::as_url(url as *const c_void) }.map(|u| u.path())
}

unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    let doc = doc_of(receiver);
    doc.window_controllers = new_mutable_array();
    doc.has_undo_manager = 1;
    receiver
}

/// `-initWithType:error:`: Apple's default invokes `-init` and records the
/// type; a subclass that overrides `-init` is still called, through the
/// dynamic dispatch of the message send rather than a direct call to this
/// file's own [`init`].
unsafe extern "C" fn init_with_type(receiver: Id, _sel: Sel, doc_type: Id, error: *mut Id) -> Id {
    if !error.is_null() {
        // SAFETY: the caller's writable `NSError **`.
        unsafe { *error = std::ptr::null_mut() };
    }
    // SAFETY: `receiver` is already allocated; this re-enters `-init`
    // polymorphically, as `+alloc]-init]` would.
    let receiver = unsafe { send0(receiver, "init") };
    if receiver.is_null() {
        return receiver;
    }
    let doc = doc_of(receiver);
    doc.file_type = objc::arc::retain(doc_type);
    receiver
}

/// `-initWithContentsOfURL:ofType:error:`: as Apple's default, chains through
/// `-initWithType:error:` and then `-readFromURL:ofType:error:`, each sent as
/// a message so a subclass's override of either runs.
unsafe extern "C" fn init_with_contents_of_url(
    receiver: Id,
    _sel: Sel,
    url: Id,
    doc_type: Id,
    error: *mut Id,
) -> Id {
    // SAFETY: a getter is `id (id, SEL, id, id *)`.
    let receiver = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            receiver,
            objc::sel::get("initWithType:error:"),
            doc_type,
            error,
        )
    };
    if receiver.is_null() {
        return receiver;
    }
    let doc = doc_of(receiver);
    doc.file_url = objc::arc::retain(url);
    // SAFETY: `-readFromURL:ofType:error:` is `BOOL (id, SEL, id, id, id *)`.
    let ok = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> u8 =
            std::mem::transmute(objc::msg::entry());
        f(
            receiver,
            objc::sel::get("readFromURL:ofType:error:"),
            url,
            doc_type,
            error,
        )
    };
    if ok == 0 {
        objc::arc::release(receiver);
        return std::ptr::null_mut();
    }
    receiver
}

/// `-readFromURL:ofType:error:`: Apple's own default "does nothing and
/// returns YES" - the actual reading is a subclass's job, and a subclass that
/// does not override it has nothing to read anyway.
unsafe extern "C" fn read_from_url(
    _receiver: Id,
    _sel: Sel,
    _url: Id,
    _doc_type: Id,
    _error: *mut Id,
) -> u8 {
    1
}

unsafe extern "C" fn file_url(receiver: Id, _sel: Sel) -> Id {
    doc_of(receiver).file_url
}

unsafe extern "C" fn set_file_url(receiver: Id, _sel: Sel, url: Id) {
    let doc = doc_of(receiver);
    let old = doc.file_url;
    doc.file_url = objc::arc::retain(url);
    objc::arc::release(old);
}

unsafe extern "C" fn file_type(receiver: Id, _sel: Sel) -> Id {
    doc_of(receiver).file_type
}

unsafe extern "C" fn set_file_type(receiver: Id, _sel: Sel, doc_type: Id) {
    let doc = doc_of(receiver);
    let old = doc.file_type;
    doc.file_type = objc::arc::retain(doc_type);
    objc::arc::release(old);
}

/// `-displayName`: the name set explicitly, else the file URL's last path
/// component, else "Untitled".
///
/// FIXME (ACCURACY): a Mac numbers unsaved windows ("Untitled 2", "Untitled
/// 3", ...); every untitled document here answers the same bare name.
unsafe extern "C" fn display_name(receiver: Id, _sel: Sel) -> Id {
    let doc = doc_of(receiver);
    if !doc.display_name.is_null() {
        return doc.display_name;
    }
    match unsafe { file_path(doc.file_url) } {
        Some(path) => {
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(path);
            objc::arc::autorelease(foundation::string::from_str(&name))
        }
        None => objc::arc::autorelease(foundation::string::from_str("Untitled")),
    }
}

unsafe extern "C" fn set_display_name(receiver: Id, _sel: Sel, name: Id) {
    let doc = doc_of(receiver);
    let old = doc.display_name;
    doc.display_name = objc::arc::retain(name);
    objc::arc::release(old);
}

unsafe extern "C" fn window_controllers(receiver: Id, _sel: Sel) -> Id {
    objc::arc::autorelease(array_snapshot(doc_of(receiver).window_controllers))
}

/// `-addWindowController:`: appended, and told its document - the two
/// directions `NSWindowController` keeps in sync, same as Apple's.
unsafe extern "C" fn add_window_controller(receiver: Id, _sel: Sel, controller: Id) {
    let array = doc_of(receiver).window_controllers;
    // SAFETY: a setter is `void (id, SEL, id)`.
    unsafe {
        send1_void(array, "addObject:", controller);
        send1_void(controller, "setDocument:", receiver);
    }
}

unsafe extern "C" fn remove_window_controller(receiver: Id, _sel: Sel, controller: Id) {
    let array = doc_of(receiver).window_controllers;
    // SAFETY: as above.
    unsafe {
        send1_void(array, "removeObjectIdenticalTo:", controller);
        send1_void(controller, "setDocument:", std::ptr::null_mut());
    }
}

/// `-makeWindowControllers`: nothing, here - there is no nib to load one
/// from, and a subclass that wants a window makes its own controller and
/// overrides this to add it.
unsafe extern "C" fn make_window_controllers(_receiver: Id, _sel: Sel) {}

/// `-showWindows`: `-showWindow:` to each controller, in the order they were
/// added.
unsafe extern "C" fn show_windows(receiver: Id, _sel: Sel) {
    let array = doc_of(receiver).window_controllers;
    // SAFETY: a real array this file made.
    let Some(contents) = (unsafe { corefoundation::collections::array(array as *const c_void) })
    else {
        return;
    };
    for controller in contents.values.clone() {
        // SAFETY: a setter is `void (id, SEL, id)`; the sender is the
        // controller itself, as Apple's own `-showWindows` passes.
        unsafe { send1_void(controller as Id, "showWindow:", controller as Id) };
    }
}

unsafe extern "C" fn is_document_edited(receiver: Id, _sel: Sel) -> u8 {
    u8::from(doc_of(receiver).change_count != 0)
}

/// `-updateChangeCount:`: `NSChangeCleared` (2) zeroes the count; every other
/// `NSDocumentChangeType` moves it away from zero, which is all `-isDocumentEdited`
/// looks at.
unsafe extern "C" fn update_change_count(receiver: Id, _sel: Sel, change: usize) {
    let doc = doc_of(receiver);
    doc.change_count = match change {
        2 => 0,
        _ => doc.change_count + 1,
    };
}

/// `-undoManager`: a manager made lazily when `-hasUndoManager` says the
/// document owns one, else nil.
///
/// Apple also registers the document as an observer of the manager's
/// notifications here, so that undoing or redoing updates
/// `-isDocumentEdited` on its own. Left out: nothing in this file observes
/// `NSUndoManager`'s notifications, so a program relying on that wiring
/// must call `-updateChangeCount:` itself.
unsafe extern "C" fn undo_manager(receiver: Id, _sel: Sel) -> Id {
    let doc = doc_of(receiver);
    if doc.has_undo_manager == 0 {
        return std::ptr::null_mut();
    }
    if doc.undo_manager.is_null() {
        doc.undo_manager = foundation::undomanager::make();
    }
    doc.undo_manager
}

unsafe extern "C" fn set_undo_manager(receiver: Id, _sel: Sel, manager: Id) {
    let doc = doc_of(receiver);
    let old = doc.undo_manager;
    doc.undo_manager = objc::arc::retain(manager);
    objc::arc::release(old);
    doc.has_undo_manager = 1;
}

unsafe extern "C" fn has_undo_manager(receiver: Id, _sel: Sel) -> u8 {
    u8::from(doc_of(receiver).has_undo_manager != 0)
}

/// `-setHasUndoManager:`: turning this off also drops whatever manager the
/// document already owns, per Apple's own note on the property - a document
/// that stops supporting undo has nothing left to ask for one.
unsafe extern "C" fn set_has_undo_manager(receiver: Id, _sel: Sel, value: u8) {
    let doc = doc_of(receiver);
    doc.has_undo_manager = value as usize;
    if value == 0 {
        objc::arc::release(std::mem::replace(
            &mut doc.undo_manager,
            std::ptr::null_mut(),
        ));
    }
}

/// `-close`: close every window controller, then leave whichever document
/// controller holds this one - Apple's order, so a window's own `-close`
/// delegate callbacks still see the document attached.
unsafe extern "C" fn close(receiver: Id, _sel: Sel) {
    let array = doc_of(receiver).window_controllers;
    if let Some(contents) = unsafe { corefoundation::collections::array(array as *const c_void) } {
        for controller in contents.values.clone() {
            // SAFETY: `-close` is `void (id, SEL)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
                f(controller as Id, objc::sel::get("close"));
            }
        }
    }
    let controller = SHARED_CONTROLLER.load(Ordering::Acquire) as Id;
    // SAFETY: a setter is `void (id, SEL, id)`.
    unsafe { send1_void(controller, "removeDocument:", receiver) };
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let doc = doc_of(receiver);
    objc::arc::release(doc.file_url);
    objc::arc::release(doc.file_type);
    objc::arc::release(doc.display_name);
    objc::arc::release(doc.window_controllers);
    objc::arc::release(doc.undo_manager);
    objc::arc::finalize(receiver);
}

/// `+[NSDocumentController sharedDocumentController]`: the first instance
/// made - of whichever class, since a custom document controller registers
/// itself from `-init` - or a default one of the receiving class, made now.
unsafe extern "C" fn shared_document_controller(class: Id, _sel: Sel) -> Id {
    let existing = SHARED_CONTROLLER.load(Ordering::Acquire) as Id;
    if !existing.is_null() {
        return existing;
    }
    // SAFETY: the receiver of a class method is a class this runtime knows.
    let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) }) else {
        return std::ptr::null_mut();
    };
    let controller = objc::object::create_instance(class, 0);
    unsafe { controller_init(controller, objc::sel::get("init")) };
    controller
}

/// `-init`: the controller that inits first becomes the shared one, matching
/// how Apple's own document controller publishes itself - a nib's top-level
/// object is inited while the nib loads, well before anything names
/// `+sharedDocumentController`.
unsafe extern "C" fn controller_init(receiver: Id, _sel: Sel) -> Id {
    let controller = controller_of(receiver);
    controller.documents = new_mutable_array();
    // `compare_exchange` rather than an unconditional store: a second
    // controller made later must not displace the first.
    let _ = SHARED_CONTROLLER.compare_exchange(
        0,
        receiver as usize,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
    receiver
}

unsafe extern "C" fn documents(receiver: Id, _sel: Sel) -> Id {
    objc::arc::autorelease(array_snapshot(controller_of(receiver).documents))
}

/// `-currentDocument`: the most recently added document, in the absence of
/// any notion of a key window here to pick by.
unsafe extern "C" fn current_document(receiver: Id, _sel: Sel) -> Id {
    unsafe { send0(controller_of(receiver).documents, "lastObject") }
}

/// `-documentForURL:`: the open document whose `-fileURL` names the same
/// path, compared the way two file URLs naming the same file should be.
unsafe extern "C" fn document_for_url(receiver: Id, _sel: Sel, url: Id) -> Id {
    let Some(target) = (unsafe { file_path(url) }) else {
        return std::ptr::null_mut();
    };
    let array = controller_of(receiver).documents;
    let Some(contents) = (unsafe { corefoundation::collections::array(array as *const c_void) })
    else {
        return std::ptr::null_mut();
    };
    contents
        .values
        .iter()
        .copied()
        .find(|doc| {
            let url = unsafe { send0(*doc as Id, "fileURL") };
            unsafe { file_path(url) }.as_deref() == Some(target.as_str())
        })
        .map_or(std::ptr::null_mut(), |doc| doc as Id)
}

unsafe extern "C" fn add_document(receiver: Id, _sel: Sel, document: Id) {
    unsafe { send1_void(controller_of(receiver).documents, "addObject:", document) };
}

unsafe extern "C" fn remove_document(receiver: Id, _sel: Sel, document: Id) {
    unsafe {
        send1_void(
            controller_of(receiver).documents,
            "removeObjectIdenticalTo:",
            document,
        )
    };
}

unsafe extern "C" fn controller_dealloc(receiver: Id, _sel: Sel) {
    objc::arc::release(controller_of(receiver).documents);
    SHARED_CONTROLLER
        .compare_exchange(receiver as usize, 0, Ordering::AcqRel, Ordering::Acquire)
        .ok();
    objc::arc::finalize(receiver);
}

pub(crate) fn install() {
    let document = objc::class::lookup(c"NSDocument").expect("declared before its methods");
    methods!(document, {
        "init" => init, c"@16@0:8";
        "initWithType:error:" => init_with_type, c"@32@0:8@16^@24";
        "initWithContentsOfURL:ofType:error:" => init_with_contents_of_url, c"@40@0:8@16@24^@32";
        "readFromURL:ofType:error:" => read_from_url, c"B40@0:8@16@24^@32";
        "fileURL" => file_url, c"@16@0:8";
        "setFileURL:" => set_file_url, c"v24@0:8@16";
        "fileType" => file_type, c"@16@0:8";
        "setFileType:" => set_file_type, c"v24@0:8@16";
        "displayName" => display_name, c"@16@0:8";
        "setDisplayName:" => set_display_name, c"v24@0:8@16";
        "windowControllers" => window_controllers, c"@16@0:8";
        "addWindowController:" => add_window_controller, c"v24@0:8@16";
        "removeWindowController:" => remove_window_controller, c"v24@0:8@16";
        "makeWindowControllers" => make_window_controllers, c"v16@0:8";
        "showWindows" => show_windows, c"v16@0:8";
        "isDocumentEdited" => is_document_edited, c"B16@0:8";
        "updateChangeCount:" => update_change_count, c"v24@0:8Q16";
        "undoManager" => undo_manager, c"@16@0:8";
        "setUndoManager:" => set_undo_manager, c"v24@0:8@16";
        "hasUndoManager" => has_undo_manager, c"B16@0:8";
        "setHasUndoManager:" => set_has_undo_manager, c"v20@0:8B16";
        "close" => close, c"v16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
    });

    let controller =
        objc::class::lookup(c"NSDocumentController").expect("declared before its methods");
    class_methods!(controller, {
        "sharedDocumentController" => shared_document_controller, c"@16@0:8";
    });
    methods!(controller, {
        "init" => controller_init, c"@16@0:8";
        "documents" => documents, c"@16@0:8";
        "currentDocument" => current_document, c"@16@0:8";
        "documentForURL:" => document_for_url, c"@24@0:8@16";
        "addDocument:" => add_document, c"v24@0:8@16";
        "removeDocument:" => remove_document, c"v24@0:8@16";
        "dealloc" => controller_dealloc, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(class_name: &std::ffi::CStr) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(class_name).expect("declared");
        objc::object::create_instance(class, 0)
    }

    #[test]
    fn display_name_falls_back_to_the_file_names_last_component() {
        let doc = make(c"NSDocument");
        unsafe { init(doc, objc::sel::get("init")) };
        let url = corefoundation::url::new_file_url("/tmp/report.csp", false);
        unsafe { set_file_url(doc, objc::sel::get("setFileURL:"), url as Id) };
        let name = unsafe { display_name(doc, objc::sel::get("displayName")) };
        assert_eq!(unsafe { foundation::string::bytes(name) }, b"report.csp");
        // SAFETY: a URL this test made.
        unsafe { corefoundation::api::CFRelease(url) };
        objc::arc::release(doc);
    }

    #[test]
    fn an_untitled_document_is_not_edited_until_changed() {
        let doc = make(c"NSDocument");
        unsafe { init(doc, objc::sel::get("init")) };
        assert_eq!(
            unsafe { is_document_edited(doc, objc::sel::get("isDocumentEdited")) },
            0
        );
        unsafe { update_change_count(doc, objc::sel::get("updateChangeCount:"), 0) };
        assert_eq!(
            unsafe { is_document_edited(doc, objc::sel::get("isDocumentEdited")) },
            1
        );
        unsafe { update_change_count(doc, objc::sel::get("updateChangeCount:"), 2) };
        assert_eq!(
            unsafe { is_document_edited(doc, objc::sel::get("isDocumentEdited")) },
            0
        );
        objc::arc::release(doc);
    }

    #[test]
    fn adding_a_document_is_what_the_shared_controller_then_lists() {
        let controller = make(c"NSDocumentController");
        unsafe { controller_init(controller, objc::sel::get("init")) };
        let doc = make(c"NSDocument");
        unsafe { init(doc, objc::sel::get("init")) };
        unsafe { add_document(controller, objc::sel::get("addDocument:"), doc) };
        let listed = unsafe { documents(controller, objc::sel::get("documents")) };
        let count = unsafe { corefoundation::collections::array(listed as *const c_void) }
            .map_or(0, |a| a.values.len());
        assert_eq!(count, 1);
        unsafe { remove_document(controller, objc::sel::get("removeDocument:"), doc) };
        let listed = unsafe { documents(controller, objc::sel::get("documents")) };
        let count = unsafe { corefoundation::collections::array(listed as *const c_void) }
            .map_or(0, |a| a.values.len());
        assert_eq!(count, 0);
        objc::arc::release(doc);
        unsafe { controller_dealloc(controller, objc::sel::get("dealloc")) };
    }

    /// A fresh document has an undo manager by default, made the first time
    /// it is asked for and the same instance after - and turning
    /// `hasUndoManager` off both answers nil and drops the one it had.
    #[test]
    fn a_document_lazily_owns_one_undo_manager_until_told_not_to() {
        let doc = make(c"NSDocument");
        unsafe { init(doc, objc::sel::get("init")) };
        assert_eq!(
            unsafe { has_undo_manager(doc, objc::sel::get("hasUndoManager")) },
            1,
            "a fresh document supports undo by default"
        );
        let first = unsafe { undo_manager(doc, objc::sel::get("undoManager")) };
        assert!(!first.is_null());
        let second = unsafe { undo_manager(doc, objc::sel::get("undoManager")) };
        assert_eq!(first, second, "the same manager every time");

        unsafe { set_has_undo_manager(doc, objc::sel::get("setHasUndoManager:"), 0) };
        assert!(
            unsafe { undo_manager(doc, objc::sel::get("undoManager")) }.is_null(),
            "nil once the document no longer owns one"
        );
        objc::arc::release(doc);
    }

    /// Assigning a manager directly is what the document then hands back,
    /// and does so whether or not it previously supported undo at all.
    #[test]
    fn setting_an_undo_manager_is_what_is_read_back() {
        let doc = make(c"NSDocument");
        unsafe { init(doc, objc::sel::get("init")) };
        unsafe { set_has_undo_manager(doc, objc::sel::get("setHasUndoManager:"), 0) };
        let manager = foundation::undomanager::make();
        unsafe { set_undo_manager(doc, objc::sel::get("setUndoManager:"), manager) };
        assert_eq!(
            unsafe { has_undo_manager(doc, objc::sel::get("hasUndoManager")) },
            1,
            "assigning one turns hasUndoManager back on"
        );
        assert_eq!(
            unsafe { undo_manager(doc, objc::sel::get("undoManager")) },
            manager
        );
        objc::arc::release(manager);
        objc::arc::release(doc);
    }
}
