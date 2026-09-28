use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::block;
use crate::define::{class_methods, methods};

/// An `NSNotification`, laid out by this crate.
#[repr(C)]
struct Notification {
    isa: *const c_void,
    name: Id,
    object: Id,
    user_info: Id,
}

/// One `-addObserver:selector:name:object:`.
///
/// The observer is *not* retained, which is Apple's rule and the reason
/// `-removeObserver:` exists at all. A center that retained its observers would
/// keep every one of them alive forever.
struct Observer {
    observer: Id,
    selector: Sel,
    /// `None` matches any name.
    name: Option<String>,
    /// Null matches any sender.
    object: Id,
}

// SAFETY: the ids are only ever used on the thread that posts, and the guard
// below is what serializes access to the table itself.
unsafe impl Send for Observer {}

static OBSERVERS: std::sync::Mutex<Vec<Observer>> = std::sync::Mutex::new(Vec::new());

/// A `CFNotificationCallback`.
pub type Callback = unsafe extern "C" fn(Id, *const c_void, Id, *const c_void, Id);

/// One `CFNotificationCenterAddObserver`.
///
/// The CoreFoundation spelling of an observer is a C callback and an opaque
/// pointer rather than a receiver and a selector, so it cannot go in
/// [`OBSERVERS`]. It is still the same event: [`post`] walks both tables, which
/// is what lets a guest observe an AppKit notification through either vocabulary.
struct CallbackObserver {
    center: Id,
    observer: *const c_void,
    callback: Callback,
    /// `None` matches any name.
    name: Option<String>,
    /// Null matches any sender.
    object: *const c_void,
}

// SAFETY: as `Observer`'s - the pointers are only used on the posting thread,
// and the mutex serializes the table.
unsafe impl Send for CallbackObserver {}

static CALLBACK_OBSERVERS: std::sync::Mutex<Vec<CallbackObserver>> =
    std::sync::Mutex::new(Vec::new());

/// One `-addObserverForName:object:queue:usingBlock:`.
///
/// The center *owns* what is in here, unlike the two tables above: the token it
/// handed back is this crate's object, the block is a heap copy, and both are
/// freed by `-removeObserver:`. That is Apple's rule - a block observer has no
/// observer object of the guest's to outlive, so the only thing that can end one
/// is the token being removed.
struct BlockObserver {
    /// What `-addObserverForName:...` returned, and what `-removeObserver:`
    /// will be given back.
    token: Id,
    /// A heap copy of the `void (^)(NSNotification *)`.
    block: usize,
    /// The queue to deliver on, retained; null means the posting thread.
    queue: Id,
    /// `None` matches any name.
    name: Option<String>,
    /// Null matches any sender.
    object: Id,
}

// SAFETY: the block is a heap copy this table owns, and the objects belong to
// runtimes that are shared across threads already.
unsafe impl Send for BlockObserver {}

static BLOCK_OBSERVERS: std::sync::Mutex<Vec<BlockObserver>> = std::sync::Mutex::new(Vec::new());

/// The one center, which `+defaultCenter` must return every time.
static DEFAULT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn notification_class() -> &'static Class {
    corefoundation::base::register(c"NSNotification", None, size_of::<Notification>(), false)
}

/// Build a notification, autoreleased, as every method that hands one out does.
///
/// `name` is text rather than an `NSString` because every caller here has text:
/// the constants are `&'static str` in this crate and in AppKit.
pub fn make(name: &str, object: Id, user_info: Id) -> Id {
    crate::bootstrap();
    // SAFETY: `Notification` is the layout `notification_class` was registered
    // with, and its first field is the `isa`.
    let n = unsafe { corefoundation::base::allocate::<Notification>(notification_class()) };
    // SAFETY: freshly allocated, and every field is written before it is read.
    unsafe {
        (*n).name = objc::arc::retain(crate::string::from_str(name));
        (*n).object = objc::arc::retain(object);
        (*n).user_info = objc::arc::retain(user_info);
    }
    objc::arc::autorelease(n as Id)
}

/// Post `name` to every observer that asked for it.
///
/// Callable from this tree as well as from a guest, which is what AppKit's
/// launch notifications use.
/// Set to print every notification posted, how many observers took it, and
/// every registration as it is made.
///
/// The question it answers is the one a program that ignores an event raises
/// and neither end can answer: a notification nobody observes and an observer
/// nobody posts to look identical from outside, and a release build inlines
/// this whole path so a breakpoint cannot tell them apart either.
pub const TRACE_ENV: &str = "ROSETTA3_TRACE_EVENT";

fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(TRACE_ENV).is_some())
}

pub fn post(name: &str, object: Id, user_info: Id) {
    if tracing() {
        let takers = OBSERVERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|o| o.name.as_deref().is_none_or(|n| n == name))
            .filter(|o| o.object.is_null() || o.object == object)
            .count();
        eprintln!("rosetta3: foundation: post {name} object {object:p} to {takers} observer(s)");
    }
    let notification = make(name, object, user_info);
    // Copied out before sending: an observer is entitled to add or remove one
    // from inside its own handler, and holding the lock across the send would
    // deadlock on the first that did.
    let targets: Vec<(Id, Sel)> = OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|o| o.name.as_deref().is_none_or(|n| n == name))
        .filter(|o| o.object.is_null() || o.object == object)
        .map(|o| (o.observer, o.selector))
        .collect();
    for (observer, selector) in targets {
        // SAFETY: an observer method is `void (id, SEL, NSNotification *)` by
        // the contract of `-addObserver:selector:name:object:`.
        unsafe { send_notification(observer, selector, notification) };
    }
    post_to_blocks(name, object, notification);
    post_to_callbacks(cf::local_center(), name, object as *const c_void, user_info);
}

/// Hand the notification to every block observer of `name`.
///
/// Each block is copied *under the lock*: a `-removeObserver:` between the scan
/// and the call would otherwise free a block this is about to invoke, and a
/// queued delivery outlives the scan by definition.
fn post_to_blocks(name: &str, object: Id, notification: Id) {
    let targets: Vec<(usize, Id)> = BLOCK_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|o| o.name.as_deref().is_none_or(|n| n == name))
        .filter(|o| o.object.is_null() || o.object == object)
        .map(|o| (objc::block::copy(o.block as Id) as usize, o.queue))
        .collect();
    for (block, queue) in targets {
        deliver(block, queue, notification);
    }
}

/// A notification and the block that wants it, on their way to another thread.
struct Delivery {
    block: usize,
    notification: usize,
}

// SAFETY: the block is a copy this delivery owns and the notification is
// retained for it, so neither can go while the operation is queued.
unsafe impl Send for Delivery {}

/// Run one block observer's handler, on its queue if it asked for one.
///
/// A queue rather than the posting thread is the whole point of the `queue:`
/// argument: a program observes on the main queue so that its handler can touch
/// the UI, and running it wherever the post happened to come from would be the
/// race it registered that way to avoid.
fn deliver(block: usize, queue: Id, notification: Id) {
    if queue.is_null() {
        // SAFETY: a handler is `void (^)(NSNotification *)`.
        unsafe { block::call_with(block as Id, notification) };
        objc::block::release(block as Id);
        return;
    }
    let delivery = Delivery {
        block,
        notification: objc::arc::retain(notification) as usize,
    };
    let operation = crate::operation::with_work(move || {
        let delivery = delivery;
        // SAFETY: as above; both were kept alive for this call.
        unsafe { block::call_with(delivery.block as Id, delivery.notification as Id) };
        objc::block::release(delivery.block as Id);
        objc::arc::release(delivery.notification as Id);
    });
    crate::operation::add_to_queue(queue, operation);
    objc::arc::release(operation);
}

/// Call every CoreFoundation observer of `name` on `center`.
///
/// Copied out under the lock for the reason the selector observers are: a
/// callback may add or remove one.
fn post_to_callbacks(center: Id, name: &str, object: *const c_void, user_info: Id) {
    let name_object = crate::string::from_str(name);
    let targets: Vec<(Callback, *const c_void)> = CALLBACK_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|o| o.center == center)
        .filter(|o| o.name.as_deref().is_none_or(|n| n == name))
        .filter(|o| o.object.is_null() || o.object == object)
        .map(|o| (o.callback, o.observer))
        .collect();
    for (callback, observer) in targets {
        // SAFETY: the signature `CFNotificationCenterAddObserver` declares.
        unsafe { callback(center, observer, name_object, object, user_info) };
    }
    objc::arc::release(name_object);
}

/// # Safety
///
/// `observer` must be live and `selector` must name a method taking one object.
unsafe fn send_notification(observer: Id, selector: Sel, notification: Id) {
    let Some(method) =
        (unsafe { objc::object::class_of(observer) }).and_then(|c| c.method(selector))
    else {
        // Apple raises here. A named abort instead, because a handler that is
        // not there is a program whose notification never arrives, and silence
        // makes that look like a notification that was never posted.
        objc::fail!(
            "-[NSNotificationCenter postNotification:]: the observer does not respond to the \
             selector it registered"
        );
    };
    let f: unsafe extern "C" fn(Id, Sel, Id) = unsafe {
        std::mem::transmute::<*const c_void, unsafe extern "C" fn(Id, Sel, Id)>(
            method.implementation(),
        )
    };
    unsafe { f(observer, selector, notification) };
}

unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a notification.
    unsafe { (*(receiver as *const Notification)).name }
}

unsafe extern "C" fn object(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a notification.
    unsafe { (*(receiver as *const Notification)).object }
}

unsafe extern "C" fn user_info(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a notification.
    unsafe { (*(receiver as *const Notification)).user_info }
}

unsafe extern "C" fn notification_with_name(class: Id, _sel: Sel, name: Id, sender: Id) -> Id {
    let _ = class;
    // SAFETY: `name` is null or a string.
    let text = unsafe { text_of(name) };
    make(&text, sender, std::ptr::null_mut())
}

unsafe extern "C" fn default_center(class: Id, _sel: Sel) -> Id {
    // SAFETY: `class` is `NSNotificationCenter`, which this crate registered.
    match unsafe { objc::class::from_ptr(class as *const Class) } {
        Some(class) => center_of(class),
        None => std::ptr::null_mut(),
    }
}

/// The one center, which `+defaultCenter` and `CFNotificationCenterGetLocalCenter`
/// both have to answer with.
fn center_of(class: &'static Class) -> Id {
    *DEFAULT.get_or_init(|| {
        let center = objc::object::create_instance(class, 0);
        // Retained forever: a singleton a guest can release is one it can make
        // dangle by releasing what it never owned.
        objc::arc::retain(center);
        center as usize
    }) as Id
}

unsafe extern "C" fn add_observer(
    _receiver: Id,
    _sel: Sel,
    observer: Id,
    selector: Sel,
    name: Id,
    object: Id,
) {
    // SAFETY: null or a string.
    let name = (!name.is_null()).then(|| unsafe { text_of(name) });
    if tracing() {
        eprintln!(
            "rosetta3: foundation: addObserver {observer:p} -{} for {} object {object:p}",
            // SAFETY: a selector this runtime uniqued, whose name is a live
            // NUL-terminated string for the life of the process.
            unsafe { std::ffi::CStr::from_ptr(objc::sel::name(selector)) }.to_string_lossy(),
            name.as_deref().unwrap_or("<any name>")
        );
    }
    OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Observer {
            observer,
            selector,
            name,
            object,
        });
}

/// `-addObserverForName:object:queue:usingBlock:`.
///
/// The token handed back is an object of this crate's rather than the observer
/// the caller never supplied: `-removeObserver:` takes one argument, so the
/// registration has to be identifiable by a single value.
unsafe extern "C" fn add_observer_for_name(
    _receiver: Id,
    _sel: Sel,
    name: Id,
    object: Id,
    queue: Id,
    handler: Id,
) -> Id {
    let copied = objc::block::copy(handler) as usize;
    if copied == 0 {
        // Apple's returns a token for a nil block, and posting to it does
        // nothing. Nothing to remove later either, so nothing is recorded.
        return std::ptr::null_mut();
    }
    let token = objc::object::create_instance(token_class(), 0);
    // SAFETY: null or a string.
    let name = (!name.is_null()).then(|| unsafe { text_of(name) });
    BLOCK_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(BlockObserver {
            token,
            block: copied,
            queue: objc::arc::retain(queue),
            name,
            object,
        });
    token
}

/// The class of the token `-addObserverForName:...` returns.
///
/// Apple's is `__NSObserver`, and a guest has no business doing anything with it
/// but handing it back, so the name is the only thing about it that matters.
fn token_class() -> &'static Class {
    corefoundation::base::abstract_class(c"__NSObserver", None)
}

/// Forget every block observer `keep` rejects, freeing what the center owns.
fn drop_block_observers(keep: impl Fn(&BlockObserver) -> bool) {
    let mut dropped = Vec::new();
    BLOCK_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|o| {
            if keep(o) {
                return true;
            }
            dropped.push((o.block, o.queue, o.token));
            false
        });
    // Freed outside the lock: releasing the queue may run its `-dealloc`, and a
    // handler is entitled to register another observer from inside one.
    for (held, queue, token) in dropped {
        objc::block::release(held as Id);
        objc::arc::release(queue);
        objc::arc::release(token);
    }
}

unsafe extern "C" fn remove_observer(_receiver: Id, _sel: Sel, observer: Id) {
    OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|o| o.observer != observer);
    drop_block_observers(|o| o.token != observer);
}

unsafe extern "C" fn remove_observer_name(
    _receiver: Id,
    _sel: Sel,
    observer: Id,
    name: Id,
    object: Id,
) {
    // SAFETY: null or a string.
    let name = (!name.is_null()).then(|| unsafe { text_of(name) });
    OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|o| {
            o.observer != observer
                || (name.is_some() && o.name != name)
                || (!object.is_null() && o.object != object)
        });
    drop_block_observers(|o| {
        o.token != observer
            || (name.is_some() && o.name != name)
            || (!object.is_null() && o.object != object)
    });
}

unsafe extern "C" fn post_notification(_receiver: Id, _sel: Sel, notification: Id) {
    if notification.is_null() {
        return;
    }
    // SAFETY: a notification this crate made.
    let n = unsafe { &*(notification as *const Notification) };
    // SAFETY: its name field is a string.
    let text = unsafe { text_of(n.name) };
    post(&text, n.object, n.user_info);
}

unsafe extern "C" fn post_name(_receiver: Id, _sel: Sel, name: Id, object: Id) {
    // SAFETY: null or a string.
    let text = unsafe { text_of(name) };
    post(&text, object, std::ptr::null_mut());
}

unsafe extern "C" fn post_name_user_info(
    _receiver: Id,
    _sel: Sel,
    name: Id,
    object: Id,
    user_info: Id,
) {
    // SAFETY: null or a string.
    let text = unsafe { text_of(name) };
    post(&text, object, user_info);
}

/// A notification name as text.
///
/// # Safety
///
/// `string` must be null or a string object.
unsafe fn text_of(string: Id) -> String {
    if string.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(&unsafe { crate::string::bytes(string) }).into_owned()
}

/// A notification name a guest reads as a global, as `(Mach-O name, object)`.
///
/// Interned the same way IOKit's keys are, and for the same reason: a guest
/// compares the constant against the name it was handed, and two objects with
/// equal text are not the same object.
pub fn name_object(text: &'static str) -> u64 {
    corefoundation::base::interned_name(text)
}

/// The CoreFoundation spellings, which name the same centers and the same
/// observers as the class above.
///
/// A center is an object rather than an opaque handle so that the local one can
/// simply *be* `+[NSNotificationCenter defaultCenter]` - which is what Apple's
/// `CFNotificationCenterGetLocalCenter` is, and what makes a notification posted
/// through one arrive at an observer registered through the other.
pub mod cf {
    // The C spellings are the interface; renaming them would only hide it.
    #![allow(non_snake_case)]

    use std::ffi::c_void;

    use objc::object::Id;

    use super::{CALLBACK_OBSERVERS, Callback, CallbackObserver};

    /// `CFNotificationCenterGetLocalCenter`, which is this process's own.
    pub fn local_center() -> Id {
        super::default_center_object()
    }

    unsafe extern "C" fn CFNotificationCenterGetLocalCenter() -> Id {
        local_center()
    }

    /// The center for notifications from *other* processes.
    ///
    /// A center of its own, and one nothing ever posts to: there is no
    /// distributed notification daemon behind this runtime, so no other process
    /// is talking to the guest. Distinct from the local center rather than
    /// aliased onto it, because a guest's own local posts must not arrive at an
    /// observer that asked to hear only what another process said.
    unsafe extern "C" fn CFNotificationCenterGetDistributedCenter() -> Id {
        static CENTER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        *CENTER.get_or_init(|| {
            let class = corefoundation::base::abstract_class(c"CFNotificationCenter", None);
            let center = objc::object::create_instance(class, 0);
            // Retained forever, as every singleton here is.
            objc::arc::retain(center);
            center as usize
        }) as Id
    }

    /// `CFNotificationCenterAddObserver(center, observer, callback, name,
    /// object, suspensionBehavior)`
    ///
    /// The suspension behaviour is ignored: it says what a *distributed* center
    /// should do with notifications that arrive while the process is suspended,
    /// and nothing posts to the distributed center here.
    unsafe extern "C" fn CFNotificationCenterAddObserver(
        center: Id,
        observer: *const c_void,
        callback: Option<Callback>,
        name: Id,
        object: *const c_void,
        _suspension: isize,
    ) {
        let Some(callback) = callback else {
            return;
        };
        // SAFETY: null or a string.
        let name = (!name.is_null()).then(|| unsafe { super::text_of(name) });
        CALLBACK_OBSERVERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(CallbackObserver {
                center,
                observer,
                callback,
                name,
                object,
            });
    }

    unsafe extern "C" fn CFNotificationCenterRemoveEveryObserver(
        center: Id,
        observer: *const c_void,
    ) {
        CALLBACK_OBSERVERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|o| o.center != center || o.observer != observer);
    }

    exports::exports! {
        functions {
            "CFNotificationCenterGetLocalCenter" => CFNotificationCenterGetLocalCenter,
            "CFNotificationCenterGetDistributedCenter" => CFNotificationCenterGetDistributedCenter,
            "CFNotificationCenterAddObserver" => CFNotificationCenterAddObserver,
            "CFNotificationCenterRemoveEveryObserver" => CFNotificationCenterRemoveEveryObserver,
        }
    }
}

/// The default center, for a caller with no class object in hand.
pub fn default_center_object() -> Id {
    match objc::class::lookup(c"NSNotificationCenter") {
        Some(class) => center_of(class),
        None => std::ptr::null_mut(),
    }
}

//
// NSDistributedNotificationCenter
//

/// One distributed observer, kept apart from [`OBSERVERS`] so that a local post
/// does not reach an observer that asked to hear only what another process said.
///
/// Which is not a distinction anybody can observe yet, because nothing posts to
/// this center from another process: there is no `notify(3)` daemon, no
/// `CFNotificationCenterGetDistributedCenter` behind it, and no `notify_post`
/// this runtime can issue. What *is* real and is implemented here is the
/// registration half and the in-process delivery half, because that is what the
/// guest actually does with it.
struct DistributedObserver {
    observer: Id,
    selector: Sel,
    /// `None` matches any name.
    name: Option<String>,
    /// Null matches any sender.
    object: Id,
}

// SAFETY: as `Observer`.
unsafe impl Send for DistributedObserver {}

static DISTRIBUTED_OBSERVERS: std::sync::Mutex<Vec<DistributedObserver>> =
    std::sync::Mutex::new(Vec::new());

/// The one distributed center, which `+defaultCenter` must return every time.
static DISTRIBUTED_DEFAULT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Say once that no notification will cross a process boundary here.
///
/// The alternative is worse than silence and worse than a wrong value: a
/// distributed center that silently accepted registrations would look exactly
/// like one that works, and the guest's own code has no way to tell - Chromium
/// registers a `NSWorkspaceDidActivateApplicationNotification` observer and
/// carries on. One line, once, naming the gap, is the honest shape.
fn refuse_cross_process(what: &str) {
    static SAID: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    SAID.get_or_init(|| {
        eprintln!(
            "rosetta3: foundation: NSDistributedNotificationCenter {what}: this runtime has no \
             distributed notification centre, so nothing crosses a process boundary. \
             Registrations are kept and in-process posts are delivered; a notification from \
             another process is not, and the guest cannot tell the difference."
        );
    });
}

unsafe extern "C" fn distributed_default_center(class: Id, _sel: Sel) -> Id {
    // SAFETY: `class` is `NSDistributedNotificationCenter`, registered below.
    match unsafe { objc::class::from_ptr(class as *const Class) } {
        Some(class) => *DISTRIBUTED_DEFAULT.get_or_init(|| {
            let center = objc::object::create_instance(class, 0);
            // Retained forever, as every singleton in this module is.
            objc::arc::retain(center);
            center as usize
        }) as Id,
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn distributed_add_observer(
    _receiver: Id,
    _sel: Sel,
    observer: Id,
    selector: Sel,
    name: Id,
    object: Id,
) {
    // SAFETY: null or a string.
    let name = (!name.is_null()).then(|| unsafe { text_of(name) });
    refuse_cross_process("addObserver:selector:name:object:");
    DISTRIBUTED_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(DistributedObserver {
            observer,
            selector,
            name,
            object,
        });
}

/// `-addObserver:selector:name:object:suspensionBehavior:`.
///
/// The behaviour says what the *system* should do with notifications that
/// arrive while the process is suspended. There is no system to do it and
/// nothing arrives, so the registration is the four-argument one and the extra
/// argument is read only to be discarded.
unsafe extern "C" fn distributed_add_observer_suspension(
    receiver: Id,
    sel: Sel,
    observer: Id,
    selector: Sel,
    name: Id,
    object: Id,
    _suspension: isize,
) {
    // SAFETY: as `distributed_add_observer`'s, one fewer argument.
    unsafe { distributed_add_observer(receiver, sel, observer, selector, name, object) }
}

unsafe extern "C" fn distributed_remove_observer(_receiver: Id, _sel: Sel, observer: Id) {
    DISTRIBUTED_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|o| o.observer != observer);
}

unsafe extern "C" fn distributed_remove_observer_name(
    _receiver: Id,
    _sel: Sel,
    observer: Id,
    name: Id,
    object: Id,
) {
    // SAFETY: null or a string.
    let name = (!name.is_null()).then(|| unsafe { text_of(name) });
    DISTRIBUTED_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|o| {
            o.observer != observer
                || (name.is_some() && o.name != name)
                || (!object.is_null() && o.object != object)
        });
}

/// Deliver `name` to this center's own observers, and to nobody else's.
///
/// Copied out under the lock for the reason the local center does it: an
/// observer is entitled to add or remove one from inside its own handler.
fn post_distributed(name: &str, object: Id, user_info: Id) {
    let targets: Vec<(Id, Sel)> = DISTRIBUTED_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|o| o.name.as_deref().is_none_or(|n| n == name))
        .filter(|o| o.object.is_null() || o.object == object)
        .map(|o| (o.observer, o.selector))
        .collect();
    if targets.is_empty() {
        return;
    }
    let notification = make(name, object, user_info);
    for (observer, selector) in targets {
        // SAFETY: `void (id, SEL, NSNotification *)`, as the selector contract.
        unsafe { send_notification(observer, selector, notification) };
    }
}

unsafe extern "C" fn distributed_post_name(_receiver: Id, _sel: Sel, name: Id, object: Id) {
    // SAFETY: null or a string.
    let text = unsafe { text_of(name) };
    refuse_cross_process("postNotificationName:object:");
    post_distributed(&text, object, std::ptr::null_mut());
}

unsafe extern "C" fn distributed_post_name_user_info(
    _receiver: Id,
    _sel: Sel,
    name: Id,
    object: Id,
    user_info: Id,
) {
    // SAFETY: null or a string.
    let text = unsafe { text_of(name) };
    refuse_cross_process("postNotificationName:object:userInfo:");
    post_distributed(&text, object, user_info);
}

pub(crate) fn install() {
    let notification = notification_class();
    methods!(notification, {
        "name" => name, c"@16@0:8";
        "object" => object, c"@16@0:8";
        "userInfo" => user_info, c"@16@0:8";
    });
    class_methods!(notification, {
        "notificationWithName:object:" => notification_with_name, c"@32@0:8@16@24";
    });

    let center =
        corefoundation::base::register(c"NSNotificationCenter", None, size_of::<usize>(), false);
    class_methods!(center, {
        "defaultCenter" => default_center, c"@16@0:8";
    });
    methods!(center, {
        "addObserver:selector:name:object:" => add_observer, c"v48@0:8@16:24@32@40";
        "addObserverForName:object:queue:usingBlock:"
            => add_observer_for_name, c"@48@0:8@16@24@32@?40";
        "removeObserver:" => remove_observer, c"v24@0:8@16";
        "removeObserver:name:object:" => remove_observer_name, c"v40@0:8@16@24@32";
        "postNotification:" => post_notification, c"v24@0:8@16";
        "postNotificationName:object:" => post_name, c"v32@0:8@16@24";
        "postNotificationName:object:userInfo:" => post_name_user_info, c"v40@0:8@16@24@32";
    });

    // The guest imports this class from Foundation and uses it from three
    // places, all of which are in the browser's own machinery rather than in a
    // leaf feature - read off the Framework's `__objc_classrefs` and the
    // enclosing method each load sits in:
    //
    //   `+[NSDistributedNotificationCenter defaultCenter]` then
    //   `-addObserver:selector:name:object:` and `-removeObserver:` in
    //     `-[SystemThemeObserver initWithColorsChangedCallback:]` and
    //     `-[MacScreenMonitor init]` (ui/base/mac), and in
    //     `-[BridgedContentView initWithBridge:bounds:]` and `-clearView`
    //     (content/browser/renderer_host/cocoa) - a view made for *every*
    //     RenderWidgetHostView, so this is the renderer's own start-up path.
    //   `-postNotificationName:object:` at `Framework+0x5aeecdf` in the same
    //     region, with a `__CFConstantString` name.
    //
    // Three of the names those observers register for are themselves unbound
    // (`_NSWorkspaceActiveSpaceDidChangeNotification`,
    // `_NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`,
    // `_NSWorkspaceDidActivateApplicationNotification`), so a nil name arrives
    // and Apple treats that as "every name" - which this does too, by leaving
    // `name` as `None`.
    let distributed = corefoundation::base::register(
        c"NSDistributedNotificationCenter",
        None,
        size_of::<usize>(),
        false,
    );
    class_methods!(distributed, {
        "defaultCenter" => distributed_default_center, c"@16@0:8";
    });
    methods!(distributed, {
        "addObserver:selector:name:object:" => distributed_add_observer, c"v48@0:8@16:24@32@40";
        "addObserver:selector:name:object:suspensionBehavior:"
            => distributed_add_observer_suspension, c"v56@0:8@16:24@32@40q48";
        "removeObserver:" => distributed_remove_observer, c"v24@0:8@16";
        "removeObserver:name:object:" => distributed_remove_observer_name, c"v40@0:8@16@24@32";
        "postNotificationName:object:" => distributed_post_name, c"v32@0:8@16@24";
        "postNotificationName:object:userInfo:" => distributed_post_name_user_info,
            c"v40@0:8@16@24@32";
    });

    system_notification_refusals();
}

// UserNotifications.framework, and the NSUserNotification pair it replaced
// ---------------------------------------------------------------------
//
// # The five slots, and what reaches them
//
// Every count below is a whole-`__TEXT,__text` scan at **every byte offset** for
// a `disp32` equal to `target - offset - 4`, with each candidate re-decoded by
// `objdump` and kept only if objdump's own displacement comment names the
// target (`test/tools/selcensus.py`). Zero here is a measurement, not a failure
// to look. The positive control is in the same output: the three
// `currentNotificationCenter` sites below were found by the same pass.
//
//   selref  0xe6e9cb8  `currentNotificationCenter`         3 loads
//           0x50fcbfa, 0x5102fd3, 0x5106898
//   selref  0xe6e9d28  `defaultUserNotificationCenter`     2 loads
//           0x510305b, 0x5106945
//   classref 0xe6e75d0 `UNUserNotificationCenter`          3 refs
//           0x50fcbf3, 0x5102fcc, 0x5106891
//   classref 0xe6e75f0 `NSUserNotificationCenter`          2 refs
//           0x5103054, 0x510693e
//   classref 0xe6e7618 `NSUserNotification`                1 ref
//           0x5107217
//
// One of those three was wrong here and is corrected above: the middle
// `UNUserNotificationCenter` classref is **0x5102fcc**, not 0x5102eb4. 0x5102eb4
// loads `0xe6e7408`, which is the `NSBundle` classref - the `mainBundle` line
// three paragraphs below - and the pair at 0x5102fcc/0x5102fd3 is the
// `currentNotificationCenter` send that 0x5102fd3 was already correct about. The
// selref site 0x5102fd3 and the classref site 0x5102fcc are seven bytes apart,
// which is the shape of `mov rdi,[classref]` / `mov rsi,[selref]` / `call`; the
// wrong address was a different classref seven bytes from a *different* selref,
// and nothing but the disassembly distinguishes the two.
//
// The other classrefs the guest imports are at 0xe6e7628, 0xe6e7630 and 0xe6e7638
// (one load each: 0x510bce8, 0x510caf6, 0x510ccca), derived from the slid slot
// addresses in `test/logs/206.log` by the slide that
// `NSUserNotificationCenter` fixes - slid 0x7f076d36b5f0 against unslid
// 0xe6e75f0 gives 0x7f075af4f400, and the same slide turns the log's
// `UNUserNotificationCenter` and `NSUserNotification` slots into 0xe6e75d0 and
// 0xe6e7618, the two addresses `selcensus.py` independently finds. Two
// confirmations from sources that do not share a failure mode.
//
// The class *symbols* the image imports are ten, found by scanning the file for
// the `_OBJC_CLASS_$_` prefix and reading each NUL-terminated name that follows -
// which is the bind stream's own spelling, so it cannot miss a name the way a
// truncated run log can: `test/logs/206.log` shows 43 of the 122 missing classes
// and **does not list `UNNotificationAction`, `UNNotificationCategory`,
// `UNTextInputNotificationAction` or `UNTextInputNotificationResponse`**, which
// are all four in the image. Ten names, and the table below carries all ten -
// `UNTextInputNotificationAction` was **missing** from this file until that scan,
// and `every_class_the_guest_imports_has_an_arm_and_vice_versa` is the test that
// now holds the two lists against each other.

// Each `classref` load is immediately followed by the matching `selref` load and
// then `call QWORD PTR [rip+..] # 0xdd50038`, which is the send. So there are
// **two** `+[NSUserNotificationCenter defaultUserNotificationCenter]` sends in
// the whole image and no third, and `NSUserNotification` is mentioned once - at
// `0x5107217`, which is not a send at all: it loads the classref into `%rdi` and
// calls `_objc_alloc_init` (0xcca0912) directly, so the instance is made by the
// runtime function and the first message that class ever receives is
// `-setTitle:`. The run aborts some 300 lines before even that.
//
// The census line next to `UNUserNotificationCenter` in the unbound report,
// `_OBJC_CLASS_$_NSUserNotificationCenter` from `Foundation.framework`, is a
// **different class**, not a second slot: the Framework's bind stream gives
// `UNUserNotificationCenter` exactly one `__objc_classrefs` slot, and the two
// names sit on adjacent lines only because the linker ordered the slots that way.
//
// # The two sends are two different gates, and BOTH select this class
//
// This is the part an earlier reading of this file had backwards, and the
// correction is load-bearing: **a gate that is off is what routes the run into
// `NSUserNotificationCenter`.** It is not a gate that keeps the class
// unreachable. Each send is the `else` arm of a test, and the tests are false in
// this build.
//
// Function 1, entry `0x5102e80` (`push rbp; mov rbp,rsp; push r15/r14/r13/r12/
// rbx; sub rsp,0x188`, `ret` at `0x510330a`):
//
//   0x5102ea0  `lea rdi,[rip+..] # 0xe6da550` = the cstring
//              "AppShimNotificationAttribution"; `call 0x5cbf210`.
//              `test al,al; je 0x5103355`, and `0x5103355` is `int3; ud2` - a
//              `CHECK`, so that arm is a refusal by the guest.
//   0x5102ebb  `[[NSBundle mainBundle] infoDictionary]["CrAppModeIsAdhocSigned"]
//              boolValue]`: `0xe6e7408` is the `NSBundle` classref, `0xe6e9cc8`
//              is `mainBundle`, `0xe6e9858` is `infoDictionary`, `0xe6e8658` is
//              `objectForKeyedSubscript:`, `0xe6e9280` is `boolValue`, and the
//              key at `0xe41fd78` is a `__cfstring` whose 22-byte payload is
//              `CrAppModeIsAdhocSigned`.
//   0x5102f3a  `test r15b,r15b; je 0x5103054`  <- the NS arm, at 0x510305b
//
// Function 2, entry `0x5106860` (same prologue shape, `sub rsp,0x178`):
//
//   0x510687d  `lea rdi,[rip+..] # 0xe6da980` = "NewMacNotificationAPI";
//              `call 0x5cbf210`.
//   0x510688b  `test al,al; je 0x510693e`  <- **the NS arm, at 0x5106945**
//
// **Both status bytes are 0, and 0 is "disabled" - derived, not assumed.**
// `0x5cbf210` (`base::FeatureList::IsEnabled`) returns through
// `cmp DWORD PTR [rbx+0x8],0x1 ; sete al` on both of its return paths, at
// `0x5cbf312` and `0x5cbf32d`, so **+8 is the status and 1 is enabled**. The
// `+0xc` field it also reads is not the status: it is compared against a 16-bit
// mask at `[r14+0x7a]` and then shifted right by `0x18`, which is a bitfield.
// The feature array in `__DATA,__data` is 1642 such records, 714 of them with
// status 1, and `ChromeStructuredMetrics` (0xe6da5d0) and
// `PrerenderFallbackToPreconnect` (0xe6da9f0) are two of the enabled ones -
// which is the positive control, since Chromium 130 ships both on by default.
// `AppShimNotificationAttribution` (0xe6da550) and `NewMacNotificationAPI`
// (0xe6da980) are two of the 928 disabled ones.
//
// **And `NewMacNotificationAPI` has exactly one reference in 214 MB of text** -
// 0x510687d, the line above - so no other code in the image even asks about it,
// and the run is on the NS arm for want of a switch nobody throws. That reading
// is also confirmed without the status byte at all: the run's abort is the NS
// arm, so `0x5106889`'s `test al,al` really did come out zero.
//
// `CrAppModeIsAdhocSigned` is **absent** from
// `test/Chromium/Chromium.app/Contents/Info.plist`, so function 1's second gate
// is false on its own. **Function 1 was not reached in any run measured here** -
// and if it ever is, its first gate is false, so the `CHECK` arm traps
// (`int3`, exit 133) before the NS arm does. That is a different wall, in a
// different process, and it is a `CHECK` in the guest rather than anything this
// runtime does.
//
// The `NSUserNotificationCenter` arm of function 2 is the one this run walks.
// `ROSETTA3_ABORT_WHO=1` puts the abort in a process whose image table is
// `Chromium 0x100000000..0x100037000` + `Chromium Framework` - the browser -
// and the frame chain it prints runs through the guest's SIGABRT handler
// (Framework+0x5c9xxxx) without reaching the sender, so the static census above
// is what names the site, not the stack.
//
// Both `else` arms build a `MacNotificationServiceNS`: a raw `E8 rel32` scan
// finds exactly two callers of its constructor at `0x5106d60` - `0x51030ce` and
// `0x51069b4` - which is exactly the two arms. Control for the same scan: the
// UN constructor at `0x510a230` has three callers, `0x50fcc4b`, `0x5103028`,
// `0x5106912`, the same count as the three `currentNotificationCenter` sites.
// (The three sends and the three calls are in the same three functions, but this
// is a count correspondence, not a per-site pairing, and it is only offered as
// the scan's positive control.)
//
// # The result is used with no nil test
//
// At 0x5106945 the send's return value is retained and moved straight into
// `%rcx` as the fourth argument of the `0x5106d60` constructor
// (`0x51069b1: mov rcx,r15`; `0x51069b4: call 0x5106d60`). The `test rdi,rdi` at
// 0x51069bd tests `[rbp-0x50]` - the moved-in receiver, not the centre - and
// the same holds for the UN arm. So `nil` is not an answer this code can carry:
// it would be stored and used.
//
// # Why this is a refusal, and not an in-process centre
//
// `NSNotificationCenter` above **is** a real thing this host has, and
// implementing it is right. `NSUserNotificationCenter` and
// `UNUserNotificationCenter` are different objects and neither may be aliased
// to it. The UN framework's contract is that a request handed to its centre is
// carried by `usernotificationsd` - a launchd job, reached over a Mach service,
// under a bundle identity and a code signature that launchd registers on the
// app's behalf - and that the daemon, not the caller, decides whether and when
// anything is shown. There is no `usernotificationsd` here, no launchd to start
// one, and no signature for one to register. A request that reached an
// in-process observer table instead would be delivered to a callback the guest
// itself wrote and **nothing else would happen**: delivery that looks like
// success and is not.
//
// The deprecated `NSUserNotificationCenter` is the same shape of missing
// platform: on macOS its banner is drawn by the system's notification UI under
// the app's registered bundle identity, and there is no such UI, no bundle
// registration and no login session here. `deliverNotification:` is one-way -
// it returns `void` and reports nothing - so a stub that swallowed it would look
// exactly like a delivered notification and a notification the user never saw.
//
// # Hang or abort, on the arm this run is on
//
// It is an **abort**, and none of the hang-shaped selectors is on this arm.
// Every one-way `UNUserNotificationCenter` message was counted over the whole
// of `__TEXT,__text` with the method above, and **all eight of them** land
// inside `0x510a143..0x5111d1c` - the region the `__FILE__` markers bracket
// with `chrome/services/mac_notifications/mac_notification_service_un.mm`, i.e.
// the UN arm this build does not take:
//
//   getNotificationSettingsWithCompletionHandler:   2  0x510b0bf 0x510f1ef
//   getDeliveredNotificationsWithCompletionHandler: 3  0x510a8e0 0x510d2f2
//                                                     0x510e47b
//   getNotificationCategoriesWithCompletionHandler: 1  0x510fb97
//   requestAuthorizationWithOptions:completionHandler:  1  0x5111d1c
//   addNotificationRequest:withCompletionHandler:      1  0x510ccfb
//   removeDeliveredNotificationsWithIdentifiers:       2  0x510df9a 0x510eac6
//   setNotificationCategories:                         3  0x51163f2 0x511689e
//                                                     0x511698e
//   shouldPreventNotificationDismissalAfterDefaultAction  1  0x510cbd4
//
// A request handed to one of those is answered by the daemon's reply, and with
// no daemon the handler never runs - which is the hang, on the other arm. The NS
// arm has no completion-handler selector at all: `MacNotificationServiceNS`
// walks `deliveredNotifications` and runs its own callback on the spot, from
// its own process, so nothing on the path this run is walking can wait for
// anyone.
//
// `shouldPreventNotificationDismissalAfterDefaultAction` is the sibling that
// must **not** be refused, and the census says why: 1 site, and the load at
// 0x510cbd4 is into **`%rdx`**, not `%rsi` - it is the `forKey:` argument of a
// `setValue:forKey:` that a `respondsToSelector:` probe guards. The guest reads
// the answer, so its absence is tolerable, and it is deliberately not installed
// below. The same reasoning covers `setValue:forKey:`, `valueForKey:` and
// `respondsToSelector:` on `NSUserNotification`, which the census does find
// there.
//
// # Which file function 2 is in: not settled, and now bounded
//
// `whichfile.py` brackets both function 1 and function 2 to
// `chrome/app_shim/app_shim_controller.mm`, and the marker census agrees there
// are no intervening `__FILE__` markers between `0x5103ed8` and `0x5106ab0`.
// That bracket is nonetheless **not trustworthy for function 2**: a selector
// census of `0x5104400..0x5106d60`, which lies inside it, returns
// `removeHistoryOverlay`, `processTouchEventForHistorySwiping`,
// `updateProgressBar` and `navigateBrowserInDirection` - `content/browser`
// history-overlay and translate-bubble code, not the app shim. Re-run here and
// confirmed, and widened: the same census over `0x5106600..0x5106d60` returns
// only `scrollingDeltaY` at 0x510661f and then **nothing but function 2's two
// centre sends** at 0x510689b and 0x5106948.
//
// A marker census over `0x50f0000..0x5120000` (every byte offset scanned for a
// `disp32` landing on a source-path cstring) shows the last
// `app_shim_controller.mm` marker at **0x5103edb**, no source-path marker at all
// between it and `0x5106ab3`, and the first
// `mac_notification_service_un.mm` marker at **0x510a5b9** - seven of them, the
// last at 0x5111bf5. There is **no `mac_notification_service_ns.mm` string
// anywhere in the image**, so the NS file emits no marker and cannot be located
// by one.
//
// So what is established is the boundary, not the name: function 2 and the
// service class are the only code in the image that talks to a notification
// centre, they sit in a run from 0x5106860 to 0x510a230 that is otherwise
// silent of selectors, and that run begins immediately after the
// `content/browser` gesture code and ends at the UN service's constructor. Which
// Chromium method function 2 is, and in which `.mm`, is **not** established, and
// this file names it by its shape - the only code in the image that constructs
// the NS notification service under a `NewMacNotificationAPI` test.
//
// # THE QUESTION THIS REOPENING WAS FOR: what does the service do with a nil centre?
//
// The answer decides whether `nil` is the truthful answer or a lie, so it is
// worth the whole constructor.
//
// `MacNotificationServiceNS` is the class whose vtable is at `0xe2dbab0`; its
// `isa` is written by the constructor at `0x5106d92`, by the destructor at
// `0x510702d` and by the destructor at `0x510711d`, and nothing else. Its eight
// virtual methods are the vtable's other seven entries plus the constructor, so
// **every method the class has is inside `0x5106d60..0x510a143`** and there is no
// code outside that range that can reach the field. Two names make it concrete
// without any source: the constructor allocates the classref at `0xe6e7610`,
// whose `ro_data` name at `0xe6f8540` is `AlertNSNotificationCenterDelegate` and
// which it messages `initWithActionHandler:` (selref `0xe6e9eb8`); and it builds
// a mojo remote, passing the cstring at `0xcf22cc0`, which is
// `mac_notifications.mojom.MacNotificationService`.
//
// The constructor, `%rcx` is the centre as its fourth argument:
//
//   0x5106d7e  `mov rdi,rcx` ; 0x5106d81 `call [0xdd50040]` = `_objc_retain`
//   0x5106d87  `mov [rbp-0x40],rax`        - retained, held on the frame
//   ...        the mojo remote, an `operator new(8)` refcount block, the vtable
//   0x5106eda  `_objc_alloc` of the delegate class, `initWithActionHandler:`
//   0x5106f12  `mov [r14+0x38],rax`        - the delegate, at this+0x38
//   0x5106f36  `mov rdi,[rbp-0x40]` ; 0x5106f3a `_objc_retain` again
//   0x5106f43  `mov [r14+0x40],rax`        - **the centre, at this+0x40**
//   0x5106f55  `[centre setDelegate: delegate]` through `objc_msgSend`
//   0x5106f5b  `_objc_release(centre)` ; `ret`
//
// It is retained twice, stored once, and messaged once, and there is no branch
// on it anywhere in the constructor. **And there is no branch on it anywhere in
// the class.** `test/tools/field-nilcheck.py 0x5106d60:0x510a143 0x40` walks
// every read of `+0x40` and what the value is used for:
//
//   SENT     0x5107037  0x5107127   the two destructors, `-setDelegate:nil`
//   RETAINED 0x510704a  0x510713a   the two destructors, releasing it
//   SENT     0x51076c1  **`-deliverNotification:` in the same function**
//   SENT     0x51077f5  0x5108195  0x5108526   `-deliveredNotifications` x3
//   SENT     0x5108427  0x5108762   `-removeDeliveredNotification:` x2
//   SENT     0x51087c4  0x51087d0   `-removeAllDeliveredNotifications`,
//                                     a tail jump straight into `objc_msgSend`
//   STORE    0x5106f43                the constructor's own write
//   TESTED   0x5109b03                **the positive control**, below
//
// Eleven reads of the field, **nine sends, zero tests**. The control is in the
// same output: `0x5109b03` reads a `+0x40` in a *different* object in the same
// translation unit and **is** nil-tested three instructions later, so a walk
// that could not see a `test` would report zero and read like a fact. Widening
// the range to `0x5106000..0x510c000` adds two more `+0x40` reads (0x510ad56,
// 0x510bdaf), both into `%rsi`/`%r15` and both past the first
// `mac_notification_service_un.mm` marker at `0x510a5b9`, so the class's own
// eleven are the whole of it.
//
// **So the class never asks whether it has a centre, and the only reason a null
// centre would not crash is Objective-C's own rule that a message to `nil` is a
// no-op.** That rule is real and this runtime implements it - `msg_send` on
// x86-64 begins `test rdi,rdi; je` at `crates/objc/src/msg.rs:301` and returns
// with `%rax` and `%rdx` zeroed, `msg_send_stret` begins `test rsi,rsi` at
// `crates/objc/src/msg.rs:362` and returns the buffer address, and the aarch64
// body says why in a comment at `crates/objc/src/msg.rs:281`: "Messaging nil is
// the language, not an error". The guest relies on it elsewhere in the very same
// function: at `0x510782f` it sends `countByEnumeratingWithState:objects:count:`
// to whatever `deliveredNotifications` returned, and `0x5107838: test rax,rax; je`
// skips the whole loop when the answer is zero, which is the protocol's own "stop
// enumerating" convention and is exactly what a nil receiver returns.
//
// **Which makes `nil` survivable - and survivable is the problem.** The
// constructor's own `setDelegate:` would be a no-op; nothing would be shown; and
// `-deliverNotification:` is `void` and reports nothing, so at `0x51076c1..0x51076e1`
// the guest sends the toast, releases it, releases the delegate and returns. It
// has no way to know. A null centre is not "notifications unavailable": it is
// **delivery that looks like delivery**, which is the one outcome worse than a
// fault, and the reason the earlier reading here - "nil is not survivable",
// argued from the centre never being nil on macOS - reached the right verdict
// for the wrong reason. It is an argument about macOS, not about this
// constructor; the constructor answers it, and the answer is that it would carry
// on.
//
// Therefore: **`nil` is refused, and the refusal is unconditional.** The missing
// capability is named per class by the two constants below, and the caller is
// named in the message: `+[NSUserNotificationCenter defaultUserNotificationCenter]`
// at `0x5106945`, whose result goes through `_objc_retainAutoreleasedReturnValue`
// into `%rcx` at `0x51069b1` and straight into this constructor at `0x51069b4`.
//
// # What is installed, and what it does
//
// Each class the guest imports from `UserNotifications.framework`, plus the two
// `NSUserNotification*` classes it imports from `Foundation.framework`, with on
// each the selectors the census measured as sent to **that** class. Every one
// prints the missing service and the caller, and then aborts. Nothing is
// answered, and no guest path is taught to tolerate anything.
//
// Registering under the guest's own spelling is what makes the message reach the
// right object whatever the order of the two registrations is:
// `corefoundation::base::register` returns the class already in the table when the
// name is taken, and `replace_method` installs onto whatever it returned. So
// whether this runs before or after `rosetta3::program::name_missing_classes`,
// there is one class object per name and the guest's slot ends up pointing at it.
//
// **The one thing that changes on the milestone's default output is one line,
// and it is the line at the fault.** Before: `rosetta3: objc: unrecognized
// selector sent to instance of NSUserNotificationCenter:
// defaultUserNotificationCenter`, from `objc::msg::unrecognised`, because
// `NAME_MISSING_CLASSES` gave the classref an empty class. After: the line
// above, from this file. Same instruction, same `abort()`, same exit 134, the
// same line number in the log, and the line now says what is missing and who
// asked. Measured on runs 206 and 217 of `test/logs/`.
//
// # What is behind this wall, and why the default run cannot show it
//
// With `ROSETTA3_LOG_UNRECOGNIZED=1` - and only with it, or with `--raw`, which
// leaves the classref unbound so the guest messages nil - the run walks past this
// refusal and the **same wall** appears in both: `Chromium Framework+0x65d1bac`,
// a store at `+8` through `%rbx = 0`, in a function bracketed by
// `net/disk_cache/simple/simple_backend_impl.cc` markers at 0x65d0f52, 0x65d1999
// and 0x65d1bc5. Runs 216 and 219.
//
// Two controls make that a finding rather than a coincidence. In 219 (`--raw`)
// the guest never messages the notification centre at all - zero refusal lines,
// zero `objc` refusal lines, only the unbound-import listing - and the fault is
// still at 0x65d1bac. And the one `-[TKTokenWatcher tokenIDs]` line in 216, which
// looks like the trigger, is **not**: 219 has no such line and faults identically,
// so it is another thread's output interleaved. The disk-cache fault is
// independent of the notification surface, and this wall is the only thing
// between the default run and it.
//
// Which is the operational consequence worth writing down: **this refusal is now
// a hard stop for the milestone**, by design. The capability is genuinely absent,
// the alternative answer is delivery that looks like delivery, and the other arm
// of the gate is the same missing daemon. Whoever picks the milestone up next
// needs to know that the run cannot get past this line without answering nil
// somewhere it should not, and that the next *fixable* item is not visible from
// the default run at all.


/// `CStr` from a string literal, because `macro_rules!` bodies cannot name
/// `c"..."` and this file's other tables are written with it.
macro_rules! name_cstr {
    ($name:literal) => {
        match std::ffi::CStr::from_bytes_with_nul(concat!($name, "\0").as_bytes()) {
            Ok(c) => c,
            // A literal with no interior NUL is always a CStr; this arm is
            // unreachable and says so rather than panicking.
            Err(_) => panic!("a string literal with no interior NUL is a CStr"),
        }
    };
}

/// The classes, and the selectors the census measured as sent to each.
///
/// These are **not** all the selectors the classes have; they are the ones
/// `__objc_selrefs` shows loaded into `%rsi` ahead of a send. `setDelegate:` and
/// `setTitle:` appear once each here rather than at their 67 and 31 load sites,
/// because every one of those sites is a send to some *other* class, and the ones
/// that are ours are both in `mac_notification_service_un.mm`.
/// One selector and the type encoding declared for it.
type SelEntry = (&'static str, &'static std::ffi::CStr);

/// A class, and the class methods and instance methods measured on it.
type Measured = (&'static str, &'static [SelEntry], &'static [SelEntry]);

const SYSTEM_NOTIFICATIONS: &[Measured] = &[
    (
        "UNUserNotificationCenter",
        &[("currentNotificationCenter", c"@16@0:8")],
        &[
            (
                "requestAuthorizationWithOptions:completionHandler:",
                c"v40@0:8@16@24",
            ),
            (
                "getNotificationSettingsWithCompletionHandler:",
                c"v32@0:8@16",
            ),
            (
                "addNotificationRequest:withCompletionHandler:",
                c"v40@0:8@16@24",
            ),
            (
                "removeDeliveredNotificationsWithIdentifiers:",
                c"v32@0:8@16",
            ),
            (
                "getDeliveredNotificationsWithCompletionHandler:",
                c"v32@0:8@16",
            ),
            ("setNotificationCategories:", c"v32@0:8@16"),
            ("setDelegate:", c"v32@0:8@16"),
            ("delegate", c"@16@0:8"),
        ],
    ),
    // The two rows below are the arm this build's run actually walks: a
    // `MacNotificationServiceNS` built on the `else` of a `NewMacNotificationAPI`
    // test that is false. Every selector is measured, with the site that sends it
    // (`test/tools/localcensus.py` over `0x5106d60..0x510a143`, the region
    // between the NS constructor and the `mac_notification_service_un.mm` marker);
    // the receiver is the `notification_center_` / `toast` pointer the constructor
    // stores, read off the register the send puts it in.
    //
    //   NSUserNotificationCenter, 3 x `setDelegate:` (0x5106f4b ctor, 0x510703b
    //     dtor, 0x510712b), `deliverNotification:` (0x51076c5),
    //     `deliveredNotifications` (0x51077f9, 0x5108199, 0x510852a),
    //     `removeDeliveredNotification:` (0x510842b, 0x5108600),
    //     `removeAllDeliveredNotifications` (0x51087c8)
    //   NSUserNotification, `setTitle:` 0x5107259, `setSubtitle:` 0x51072a4,
    //     `setInformativeText:` 0x51072ef, `setUserInfo:` 0x5107326,
    //     `setActionButtonTitle:` 0x51074fa, `setHasActionButton:` 0x5107518,
    //     `setContentImage:` 0x510761a, `setIdentifier:` 0x51076aa,
    //     `userInfo` 0x5108f1f
    //
    // Three of the selectors the census finds in that region are **not** in these
    // tables, and each exclusion is a measurement rather than a preference:
    //
    //  * `setValue:forKey:` (0x51073a2, 0x5107561, 0x51075a5, 0x51075c5),
    //    `valueForKey:` (0x5109092, 0x51090e5, 0x51091c2) and
    //    `respondsToSelector:` (0x510735b, 0x5107453, 0x5109077, 0x51091a3) are
    //    the guest probing private keys (`_showsButtons`, `_hasActionButton`,
    //    `_alwaysShowAlternateActionMenu`, `_alternateActionButtonTitles`) and
    //    reading the answer. Refusing `setValue:forKey:` would abort a guest that
    //    is explicitly asking whether the key exists.
    //  * `activationType` (0x5108f66, 0x5109167) returns a `NSUserNotification
    //    ActivationType`, an `NSInteger`, so its encoding is `q16@0:8` and not the
    //    `v`/`@` this table declares. It is read only inside the centre's own
    //    `didActivateNotification:` delegate callback, which nothing here can
    //    cause to run, and an empty class refuses it by name without a lie.
    //  * `+alloc` is not installable here and is not missing either: the guest
    //    does not send it. `0x5107217` loads the `NSUserNotification` classref
    //    into `%rdi` and calls `_objc_alloc_init` (0xcca0912) **directly**, so the
    //    instance is made by the runtime function rather than by a message, and
    //    the first message that class ever receives is `-setTitle:`.
    (
        "NSUserNotificationCenter",
        &[("defaultUserNotificationCenter", c"@16@0:8")],
        &[
            ("deliverNotification:", c"v32@0:8@16"),
            ("removeAllDeliveredNotifications", c"v16@0:8"),
            ("deliveredNotifications", c"@16@0:8"),
            ("removeDeliveredNotification:", c"v32@0:8@16"),
            ("setDelegate:", c"v32@0:8@16"),
            ("delegate", c"@16@0:8"),
        ],
    ),
    (
        "NSUserNotification",
        &[],
        &[
            ("setTitle:", c"v32@0:8@16"),
            ("setSubtitle:", c"v32@0:8@16"),
            ("setInformativeText:", c"v32@0:8@16"),
            ("setUserInfo:", c"v32@0:8@16"),
            ("userInfo", c"@16@0:8"),
            ("setActionButtonTitle:", c"v32@0:8@16"),
            ("setHasActionButton:", c"v32@0:8@16"),
            ("setContentImage:", c"v32@0:8@16"),
            ("setIdentifier:", c"v32@0:8@16"),
        ],
    ),
    (
        "UNMutableNotificationContent",
        &[],
        &[
            ("setTitle:", c"v32@0:8@16"),
            ("setSubtitle:", c"v32@0:8@16"),
            ("setBody:", c"v32@0:8@16"),
            ("setUserInfo:", c"v32@0:8@16"),
            ("setCategoryIdentifier:", c"v32@0:8@16"),
            ("setAttachments:", c"v32@0:8@16"),
        ],
    ),
    (
        "UNNotificationRequest",
        &[(
            "requestWithIdentifier:content:trigger:",
            c"@40@0:8@16@24@32",
        )],
        &[],
    ),
    (
        "UNNotificationAttachment",
        &[(
            "attachmentWithIdentifier:URL:options:error:",
            c"@48@0:8@16@24@32@40",
        )],
        &[],
    ),
    (
        "UNTextInputNotificationResponse",
        &[],
        &[("userText", c"@16@0:8")],
    ),
    // No selector of either kind was measured on these four: the image imports
    // their class symbols and the census found no send to them. They are
    // registered so that a send the census has not seen names the class and the
    // missing daemon rather than leaving the classref bound to an anonymous
    // empty one. Note what is **not** claimed: that the image never loads a
    // classref for them. Their classref slot addresses are not in any log here -
    // `test/logs/206.log` prints 43 of the 122 missing classes - and a census
    // that cannot name a slot cannot count its loads.
    ("UNTextInputNotificationAction", &[], &[]),
    ("UNNotificationAction", &[], &[]),
    ("UNNotificationCategory", &[], &[]),
    ("UNTextInputNotificationResponse", &[], &[]),
];

/// Say once that nothing here can deliver a notification, then stop.
///
/// `abort()` and not a return value, for the reason
/// `darwin::last_unavailable::refuse_unwind` gives: a caller that asked for a
/// permission, a settings object or a delivery is going to read the answer and
/// decide what to do next on the strength of it. Every value available here -
/// `nil`, `0`, an empty settings object - is one this guest's own code has no way
/// to tell from a real answer, so the only honest result is to stop.
///
/// **The one exception is the runtime's own diagnostic switch, and it is the
/// runtime's, not this file's.** `ROSETTA3_LOG_UNRECOGNIZED=1` is documented, in
/// `objc::msg::logging_unrecognised`, as *answer nil instead of aborting on a
/// message no class answers*. These selectors are a message no class can answer,
/// so leaving them out of that switch would make this wall the one place the
/// lever stops working, and the lever is how the queue behind a wall is read in
/// one run. Under it the line is still printed - naming the class, the selector
/// and the missing service, which is more than the switch's own line says - and
/// then `nil` is returned, which is what "answer nil" means. The default run,
/// with the variable unset, still aborts here.
///
/// This is not "the guest tolerates a failure". The guest is untouched and has
/// no path that could: `deliverNotification:` is one-way `void` and there is no
/// "notifications unavailable" branch anywhere in the class, which is the
/// measurement in the comment above.
///
/// Once **per (class, selector)**, not once per process: the two classes are
/// refused for two different missing capabilities, and a message that named only
/// the first would be wrong about the second.
fn refuse_system_notification(class: &str, missing: &str, sel: Sel) -> usize {
    // SAFETY: `objc::sel::name` is the selector itself, which `sel_registerName`
    // made a NUL-terminated string, and this is the only read of it.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) };
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.insert((class.to_string(), name.to_string_lossy().into_owned())) {
        eprintln!(
            "rosetta3: foundation: {class} {selector}: refused by name. {missing} Nothing on this \
             host can put a notification in front of a user, so a request made through this class \
             could never be delivered and no permission or settings answer read back from it would \
             be true. The in-process NSNotificationCenter in this file is a different object and \
             not a substitute: a post to it reaches the guest's own observer and nothing else, \
             which would look like delivery. {caller}{switch}",
            selector = name.to_string_lossy(),
            caller = caller_of(class),
            switch = if objc::msg::logging_unrecognised() {
                " ROSETTA3_LOG_UNRECOGNIZED is set, so this refusal is answering nil instead of \
                 aborting; a run this way cannot be trusted."
            } else {
                ""
            },
        );
    }
    // `0` is not a stand-in for an answer - it is what "answer nil" means, and
    // the only way out of this function. The guest cannot reach it in the
    // default configuration, because `logging_unrecognised()` is false there and
    // the process stops instead.
    match objc::msg::logging_unrecognised() {
        true => 0,
        false => std::process::abort(),
    }
}

/// `(class, selector)` pairs already named, so the refusal speaks once each.
///
/// A `Mutex` rather than an `AtomicBool` because the key is the pair: with one
/// flag per process, a run that touched two of these classes would name only the
/// first and the message would be wrong about the second.
static SAID: std::sync::Mutex<std::collections::BTreeSet<(String, String)>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// What is missing for the `UN*` classes, which the UserNotifications framework
/// exists only to front.
///
/// `usernotificationsd` is a launchd job reached over a Mach service, under a
/// bundle identity and a code signature that **launchd** registers on the app's
/// behalf, and the daemon rather than the caller decides whether anything is
/// shown. There is no such daemon here, no launchd to start one, and no
/// signature for one to register - so the completion handlers on
/// `UNUserNotificationCenter` could not complete even if the class existed,
/// because a request is only ever answered by the daemon's reply.
const MISSING_USER_NOTIFICATIONS_DAEMON: &str = "The UserNotifications framework's centre is a \
    handle on usernotificationsd - a launchd job, reached over a Mach service, under a bundle \
    identity and a code signature that launchd registers for the app.";

/// What is missing for the deprecated `NSUserNotification*` pair.
///
/// A different missing capability from the one above, and saying "usernotificationsd" here
/// would be wrong: the Foundation centre has no daemon at all. It hands the toast to the
/// system's notification UI, which draws it under the app's registered bundle identity and
/// the user's login session, and answers the two "delivered notifications" questions out of
/// what that UI has shown. This host has no notification UI, no bundle registration and no
/// login session, and `deliverNotification:` returns `void` and reports nothing - so a stub
/// that swallowed it would be indistinguishable from a notification the user saw. Nothing
/// here is a hang either: every reply on this arm is the process's own, and
/// `GetDisplayedNotifications` runs its callback as it builds the vector.
const MISSING_NOTIFICATION_UI: &str = "The deprecated NSUserNotification centre has no daemon \
    and no client either: it hands the toast to the system's notification UI, which draws it \
    under the app's registered bundle identity and the user's login session.";

/// Which guest code asks each class for a notification centre, and why it asks.
///
/// A refusal that names the class but not the caller is half a refusal: the class is
/// a third-party name, and the caller is the line in Chromium that has to change.
/// Every address here is measured, and the two gates are derived rather than
/// assumed (`FeatureList::IsEnabled` returns through `cmp DWORD PTR [rbx+0x8],0x1;
/// sete al` on both its return paths, so `+8` is the status and `1` is enabled).
///
/// Per class rather than one shared sentence, because the two centres are reached
/// from opposite arms of the **same** test - and a message that named one arm while
/// refusing a message on the other would send whoever reads it to the wrong gate.
/// `NewMacNotificationAPI` (0xe6da980) is one of the 928 disabled records of the
/// 1642-entry feature array, which is why the `else` arm is the one this build walks.
///
/// The two classes that the census found with no measured selector are in here with
/// the same shape, saying that nothing in the image messages them, rather than
/// left to the `unreachable!` that guards an unpaired table row.
const CALLERS: &[(&str, &str)] = &[
    (
        "UNUserNotificationCenter",
        "The caller is +[UNUserNotificationCenter currentNotificationCenter], measured at three \
         sites - 0x50fcbf3, 0x5102eb4 and 0x5106891 - of which 0x5106898 is the one whose \
         constructor this file's comment traces; the feature that would select the other \
         implementation, NewMacNotificationAPI, is disabled in this build, so the UN arm is \
         unreachable from here and this is the arm the run takes.",
    ),
    (
        "NSUserNotificationCenter",
        "The caller is +[NSUserNotificationCenter defaultUserNotificationCenter] at 0x5106945, \
         the else arm of a test on the feature NewMacNotificationAPI, which is disabled in this \
         build, so this is the only path the guest has to a notification here. Its result goes \
         through _objc_retainAutoreleasedReturnValue into %rcx at 0x51069b1 and straight into \
         the service's constructor at 0x51069b4, which retains it, stores it at +0x40 and \
         messages it nine times with no nil test at all - measured in the comment above. That is \
         why the answer cannot be nil: a message to nil is a no-op in Objective-C, so a nil \
         centre would let the guest record a delivered notification that no user could ever have \
         seen, from a -deliverNotification: that returns void and reports nothing.",
    ),
    (
        "NSUserNotification",
        "The caller is _objc_alloc_init at 0x510721e, called directly on this classref - the \
         guest does not send +alloc - and the first message the class ever receives is \
         -setTitle: at 0x5107259, in the same function that then sends \
         -deliverNotification: at 0x51076cf.",
    ),
    (
        "UNMutableNotificationContent",
        "Nothing in this image messages this class in a way the run reaches: its classref is \
         0xe6e7628, loaded once, at 0x510bce8, which is inside the UN service's code - past \
         0x510a5b9, the first mac_notification_service_un.mm marker - and the UN arm is the one \
         NewMacNotificationAPI selects, so this build does not take it. The class is registered \
         anyway, so that a send the census has not seen names the class and the missing daemon \
         rather than an anonymous empty class.",
    ),
    (
        "UNNotificationRequest",
        "Its classref is 0xe6e7638, loaded once, at 0x510ccca - also inside the UN arm - and the \
         only selector the census finds for it is \
         +requestWithIdentifier:content:trigger:. Registered so the refusal names the class.",
    ),
    (
        "UNNotificationAttachment",
        "Its classref is 0xe6e7630, loaded once, at 0x510caf6 - also inside the UN arm - and the \
         only selector the census finds for it is \
         +attachmentWithIdentifier:URL:options:error:. Registered so the refusal names the class.",
    ),
    (
        "UNTextInputNotificationResponse",
        "The image imports the class symbol - the bind stream's own spelling has it, and the \
         selector census in the comment above is how that was found - but no send to it was \
         measured, so there is nothing to name. It is a response type the UN arm's delegate \
         callback would receive, and that callback cannot run without the daemon. Registered so \
         a send that the census has not seen names the class rather than an anonymous empty one.",
    ),
    (
        "UNTextInputNotificationAction",
        "Like UNTextInputNotificationResponse, the image imports the class symbol and the census \
         found no send to it. It belongs to a `UNTextInputNotificationAction` category, which is \
         built on the daemon's side of the service. Registered so a send names the class and the \
         missing daemon.",
    ),
    (
        "UNNotificationAction",
        "The image imports the class symbol and the census found no send to it. Registered anyway, \
         so that a send the census has not seen yet names the class and the missing daemon rather \
         than an anonymous empty class.",
    ),
    (
        "UNNotificationCategory",
        "The image imports the class symbol and the census found no send to it. Registered anyway, \
         so that a send the census has not seen yet names the class and the missing daemon rather \
         than an anonymous empty class.",
    ),
];

/// The [`CALLERS`] row for one class, or a sentence that says the table has drifted.
///
/// Not an `unwrap` and not an `expect` on a lookup that can be wrong: a class in
/// [`SYSTEM_NOTIFICATIONS`] with no row here is a table that has grown a class
/// without saying who wants it, and the message would then be silent about the
/// caller of a refusal - which is the half that tells whoever reads it what to do.
fn caller_of(class: &str) -> &'static str {
    CALLERS
        .iter()
        .find(|(name, _)| *name == class)
        .map(|(_, text)| *text)
        .unwrap_or(
            "The caller is not recorded: this class is in the refusal table with no row in \
             CALLERS, so the census behind that table has to be re-run before this line can be \
             trusted.",
        )
}

/// The implementation for a selector, chosen by its declared return type.
///
/// `!` coerces to every return type, so one body would serve both. What this
/// picks is the *declared* shape, so a selector that returns an object leaves
/// nothing undefined in `%rax` on the way to the abort. The encoding is also what
/// the guest reads back out of the method list, and a wrong one there is a lie
/// even when no call ever returns.
fn refuse_pick(
    types: &std::ffi::CStr,
    returning: *const std::ffi::c_void,
    returning_void: *const std::ffi::c_void,
) -> *const std::ffi::c_void {
    if types.to_bytes().first() == Some(&b'@') {
        returning
    } else {
        returning_void
    }
}

/// Install the refusals for one class, naming it at the point of installation.
///
/// The class name is a **literal** in the macro's argument rather than something
/// read at run time out of the selector tables, so the message cannot name a
/// different class than the one a selector was installed on - which is the
/// failure mode of the obvious implementation, where one shared body has to be
/// told which class it belongs to and that answer can be stale by the time it is
/// asked. The missing capability beside it is one of the two `&'static str`
/// constants above, so it is equally impossible to pair the UN framework's
/// daemon with the deprecated Foundation centre by accident.
///
/// Each block declares its own two functions, so nine invocations do not collide
/// on the names.
macro_rules! refuse_system_class {
    ($name:literal, $missing:expr, $class_methods:expr, $instance_methods:expr) => {{
        // Two bodies, and the split is by **declared return type** rather than by
        // convenience: a selector that returns an object leaves nothing undefined
        // in `%rax` on the way out even when the switch above answers nil, and
        // `refuse_pick` is what decides which of the two a row gets.
        unsafe extern "C" fn returning(_receiver: Id, sel: Sel) -> usize {
            refuse_system_notification($name, $missing, sel)
        }
        unsafe extern "C" fn returning_void(_receiver: Id, sel: Sel) {
            refuse_system_notification($name, $missing, sel);
        }
        let class = system_notification_class(name_cstr!($name));
        for (selector, types) in $class_methods.iter() {
            class.metaclass().replace_method(
                objc::sel::get(selector),
                refuse_pick(
                    types,
                    returning as *const std::ffi::c_void,
                    returning_void as *const std::ffi::c_void,
                ),
                types,
            );
        }
        for (selector, types) in $instance_methods.iter() {
            class.replace_method(
                objc::sel::get(selector),
                refuse_pick(
                    types,
                    returning as *const std::ffi::c_void,
                    returning_void as *const std::ffi::c_void,
                ),
                types,
            );
        }
    }};
}

/// Register, or find, the class for one of the refusals.
fn system_notification_class(name: &std::ffi::CStr) -> &'static Class {
    corefoundation::base::register(name, None, size_of::<usize>(), false)
}

#[cfg(test)]
/// The same [`name_cstr`] for a name that is a `&str` rather than a literal, so
/// the table can be walked without a macro per row.
fn class_name_cstr(name: &str) -> &std::ffi::CStr {
    // Leaked rather than built per call: `register` wants a `&'static CStr` and
    // this is one allocation per class per process, bounded by the length of
    // `SYSTEM_NOTIFICATIONS`.
    Box::leak(
        std::ffi::CString::new(name)
            .expect("a class name has no interior NUL")
            .into_boxed_c_str(),
    )
}

/// Whether the refusal installs unconditionally.
///
/// It does, and there is nothing to set; the name survives only so the test
/// above reads as a claim about behaviour rather than about a variable.
#[cfg(test)]
fn system_notification_refusals_enabled() -> bool {
    true
}

/// Register the notification classes whose only honest answer is a refusal.
///
/// **Unconditional, and that is the decision this file was reopened to make.**
/// A refusal behind a variable is not a refusal the run can hit: the milestone
/// sets `ROSETTA3_NAME_MISSING_CLASSES` and no other variable, so the default
/// already stops at this wall by the shorter route, and every line the census
/// here took to produce - the two send sites, the two gates, the nine sends with
/// no nil test, the reason a nil centre is a lie rather than an absence - would
/// be documentation about a code path nothing takes.
///
/// The two things the opt-in was said to cost are both answered, and neither
/// needed a variable:
///
///  * `ROSETTA3_LOG_UNRECOGNIZED=1` keeps working across this wall, because
///    [`refuse_system_notification`] asks `objc::msg::logging_unrecognised`
///    - the runtime's own switch - rather than keeping a private one. The line is
///    still printed, and it names more than the switch's own line does.
///  * `--raw` was never going to be stopped by this file, and saying so is the
///    measurement. `--raw` turns off `ROSETTA3_NAME_MISSING_CLASSES`, and *that*
///    is what writes the guest's `__objc_classrefs` slot
///    (`crates/rosetta3/src/program.rs:327`); `corefoundation::base::register`
///    makes a class in this runtime's own table and touches no guest memory. So
///    under `--raw` the classref still reads zero, the guest still messages nil,
///    and the run still walks into a null centre with these methods installed.
///    Installing them changes which refusal is printed, not whether the class is
///    reachable - and the fix for that belongs to the loader, not here.
fn system_notification_refusals() {
    // One arm per class, each passing its **own literal**, because the macro
    // needs a `literal` to put in the message and a `macro_rules!` cannot turn
    // a variable into one. The `other` arm is what keeps a class added to
    // `SYSTEM_NOTIFICATIONS` without an arm here from being silently skipped:
    // the table and this match are two lists that have to agree, and the test
    // below is what checks they do.
    for (name, class_methods, instance_methods) in SYSTEM_NOTIFICATIONS {
        match *name {
            "UNUserNotificationCenter" => refuse_system_class!(
                "UNUserNotificationCenter",
                MISSING_USER_NOTIFICATIONS_DAEMON,
                class_methods,
                instance_methods
            ),
            "NSUserNotificationCenter" => refuse_system_class!(
                "NSUserNotificationCenter",
                MISSING_NOTIFICATION_UI,
                class_methods,
                instance_methods
            ),
            "NSUserNotification" => refuse_system_class!(
                "NSUserNotification",
                MISSING_NOTIFICATION_UI,
                class_methods,
                instance_methods
            ),
            "UNMutableNotificationContent" => refuse_system_class!(
                "UNMutableNotificationContent",
                MISSING_USER_NOTIFICATIONS_DAEMON,
                class_methods,
                instance_methods
            ),
            "UNNotificationRequest" => refuse_system_class!(
                "UNNotificationRequest",
                MISSING_USER_NOTIFICATIONS_DAEMON,
                class_methods,
                instance_methods
            ),
            "UNNotificationAttachment" => refuse_system_class!(
                "UNNotificationAttachment",
                MISSING_USER_NOTIFICATIONS_DAEMON,
                class_methods,
                instance_methods
            ),
            "UNTextInputNotificationResponse" => refuse_system_class!(
                "UNTextInputNotificationResponse",
                MISSING_USER_NOTIFICATIONS_DAEMON,
                class_methods,
                instance_methods
            ),
            "UNTextInputNotificationAction" => refuse_system_class!(
                "UNTextInputNotificationAction",
                MISSING_USER_NOTIFICATIONS_DAEMON,
                class_methods,
                instance_methods
            ),
            "UNNotificationAction" => refuse_system_class!(
                "UNNotificationAction",
                MISSING_USER_NOTIFICATIONS_DAEMON,
                class_methods,
                instance_methods
            ),
            "UNNotificationCategory" => refuse_system_class!(
                "UNNotificationCategory",
                MISSING_USER_NOTIFICATIONS_DAEMON,
                class_methods,
                instance_methods
            ),
            other => unreachable!("SYSTEM_NOTIFICATIONS has no arm for {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::block::fake;

    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    unsafe extern "C" fn record(_receiver: Id, _sel: Sel, notification: Id) {
        // SAFETY: the poster built it.
        let n = unsafe { &*(notification as *const Notification) };
        // SAFETY: its name is a string.
        let text = unsafe { text_of(n.name) };
        SEEN.lock().unwrap_or_else(|e| e.into_inner()).push(text);
    }

    /// An observer built for one test, so the tests do not share a table entry.
    fn observer(class_name: &std::ffi::CStr) -> Id {
        crate::bootstrap();
        let class = corefoundation::base::register(class_name, None, size_of::<usize>(), false);
        methods!(class, {
            "heard:" => record, c"v24@0:8@16";
        });
        objc::object::create_instance(class, 0)
    }

    #[test]
    fn an_observer_hears_the_name_it_registered_and_no_other() {
        let who = observer(c"R3TestNotificationObserver");
        OBSERVERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Observer {
                observer: who,
                selector: objc::sel::get("heard:"),
                name: Some("R3Wanted".into()),
                object: std::ptr::null_mut(),
            });
        SEEN.lock().unwrap_or_else(|e| e.into_inner()).clear();
        post("R3Unwanted", std::ptr::null_mut(), std::ptr::null_mut());
        post("R3Wanted", std::ptr::null_mut(), std::ptr::null_mut());
        let seen = SEEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(seen, ["R3Wanted"]);
    }

    /// The notification the observer is handed carries the sender it was posted
    /// with, which is the whole reason the argument is a real object.
    #[test]
    fn a_notification_carries_its_sender() {
        crate::bootstrap();
        let sender = crate::string::from_str("sender");
        let n = make("R3Named", sender, std::ptr::null_mut());
        // SAFETY: `-object` is `id (id, SEL)`.
        let got = unsafe { object(n, objc::sel::get("object")) };
        assert_eq!(got, sender);
        // SAFETY: `-name` is `id (id, SEL)`, and its result is a string.
        let text = unsafe { text_of(name(n, objc::sel::get("name"))) };
        assert_eq!(text, "R3Named");
    }

    /// The same name is the same object every time, because a guest compares
    /// the global it was given against the one it stored.
    #[test]
    fn a_name_global_is_one_object() {
        assert_eq!(
            name_object("NSApplicationDidFinishLaunchingNotification"),
            name_object("NSApplicationDidFinishLaunchingNotification")
        );
    }

    static HEARD: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    unsafe extern "C" fn record_block(_block: Id, notification: Id) {
        // SAFETY: a notification this crate posted.
        let n = unsafe { &*(notification as *const Notification) };
        // SAFETY: its name is a string.
        let text = unsafe { text_of(n.name) };
        HEARD.lock().unwrap_or_else(|e| e.into_inner()).push(text);
    }

    fn add_block_observer(
        name: &str,
        queue: Id,
        imp: *const c_void,
    ) -> (Id, Box<fake::GlobalBlock>) {
        crate::bootstrap();
        let block = fake::block(imp);
        let name = crate::string::from_str(name);
        // SAFETY: the prototype is the method's, and the block is a valid
        // literal whose `invoke` takes itself and one object.
        let token = unsafe {
            add_observer_for_name(
                std::ptr::null_mut(),
                objc::sel::get("addObserverForName:object:queue:usingBlock:"),
                name,
                std::ptr::null_mut(),
                queue,
                block.id(),
            )
        };
        objc::arc::release(name);
        assert!(!token.is_null());
        (token, block)
    }

    /// A block observer with no queue hears its name on the posting thread, and
    /// stops hearing anything once its token is removed.
    #[test]
    fn a_block_observer_hears_its_name_until_it_is_removed() {
        let (token, _block) = add_block_observer(
            "R3BlockWanted",
            std::ptr::null_mut(),
            record_block as *const c_void,
        );
        HEARD.lock().unwrap_or_else(|e| e.into_inner()).clear();
        post(
            "R3BlockUnwanted",
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        post("R3BlockWanted", std::ptr::null_mut(), std::ptr::null_mut());
        assert_eq!(
            HEARD.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            ["R3BlockWanted"]
        );

        // SAFETY: `-removeObserver:` is `void (id, SEL, id)`.
        unsafe {
            remove_observer(
                std::ptr::null_mut(),
                objc::sel::get("removeObserver:"),
                token,
            )
        };
        post("R3BlockWanted", std::ptr::null_mut(), std::ptr::null_mut());
        assert_eq!(
            HEARD.lock().unwrap_or_else(|e| e.into_inner()).len(),
            1,
            "a removed block observer still heard a post"
        );
    }

    /// The thread the queued handler ran on, which is what the `queue:`
    /// argument decides.
    static ON_QUEUE: std::sync::Mutex<Option<std::thread::ThreadId>> = std::sync::Mutex::new(None);
    static DELIVERED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn record_thread(_block: Id, _notification: Id) {
        *ON_QUEUE.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::thread::current().id());
        DELIVERED.fetch_add(1, Ordering::SeqCst);
    }

    /// A block observer given a queue is delivered *through* it, on the queue's
    /// thread rather than the poster's.
    #[test]
    fn a_block_observer_with_a_queue_is_delivered_on_it() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSOperationQueue").expect("registered by `operation`");
        // SAFETY: `+new` is `id (Class, SEL)`.
        let queue = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(objc::class::ptr(class) as Id, objc::sel::get("new"))
        };
        assert!(!queue.is_null());

        let (token, _block) =
            add_block_observer("R3BlockQueued", queue, record_thread as *const c_void);
        DELIVERED.store(0, Ordering::SeqCst);
        post("R3BlockQueued", std::ptr::null_mut(), std::ptr::null_mut());

        for _ in 0..500 {
            if DELIVERED.load(Ordering::SeqCst) != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(DELIVERED.load(Ordering::SeqCst), 1, "the handler never ran");
        assert_ne!(
            *ON_QUEUE.lock().unwrap_or_else(|e| e.into_inner()),
            Some(std::thread::current().id()),
            "the handler ran on the posting thread"
        );

        // SAFETY: `-removeObserver:` is `void (id, SEL, id)`.
        unsafe {
            remove_observer(
                std::ptr::null_mut(),
                objc::sel::get("removeObserver:"),
                token,
            )
        };
        objc::arc::release(queue);
    }

    /// `+[NSDistributedNotificationCenter defaultCenter]` is one object, and it
    /// is not the local center: an observer registered on one must not hear a
    /// post made through the other, which is the only distinction between them
    /// this runtime can honour.
    #[test]
    fn the_distributed_default_center_is_one_object_and_not_the_local_one() {
        crate::bootstrap();
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            let send0: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(objc::msg::entry());
            let a = send0(
                objc::class::ptr(objc::class::lookup(c"NSDistributedNotificationCenter").unwrap())
                    as Id,
                objc::sel::get("defaultCenter"),
            );
            let b = send0(
                objc::class::ptr(objc::class::lookup(c"NSDistributedNotificationCenter").unwrap())
                    as Id,
                objc::sel::get("defaultCenter"),
            );
            let local = send0(
                objc::class::ptr(objc::class::lookup(c"NSNotificationCenter").unwrap()) as Id,
                objc::sel::get("defaultCenter"),
            );
            assert!(!a.is_null());
            assert_eq!(a, b, "+defaultCenter made two centers");
            assert_ne!(
                a, local,
                "the distributed center is the local one, so a local post reaches a distributed \
                 observer"
            );
        }
    }

    /// A distributed observer hears a distributed post for the name it
    /// registered, and does not hear a *local* post of the same name.
    #[test]
    fn a_distributed_observer_hears_its_own_posts_and_no_local_ones() {
        crate::bootstrap();
        let who = observer(c"R3DistributedObserver");
        // SAFETY: `v48@0:8@16:24@32@40` is what the selector declares.
        unsafe {
            distributed_add_observer(
                std::ptr::null_mut(),
                objc::sel::get("addObserver:selector:name:object:"),
                who,
                objc::sel::get("heard:"),
                crate::string::from_str("R3DistributedWanted"),
                std::ptr::null_mut(),
            );
            post_distributed(
                "R3DistributedWanted",
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            let heard = SEEN.lock().unwrap_or_else(|e| e.into_inner()).len();
            assert_eq!(heard, 1, "the distributed post did not reach its observer");

            // The local center must not reach it: same name, wrong center.
            post(
                "R3DistributedWanted",
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            assert_eq!(
                SEEN.lock().unwrap_or_else(|e| e.into_inner()).len(),
                heard,
                "a local post reached a distributed observer"
            );

            // And a post for another name must not either.
            post_distributed(
                "R3DistributedOther",
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            assert_eq!(
                SEEN.lock().unwrap_or_else(|e| e.into_inner()).len(),
                heard,
                "a distributed post for another name reached this observer"
            );

            distributed_remove_observer(
                std::ptr::null_mut(),
                objc::sel::get("removeObserver:"),
                who,
            );
            post_distributed(
                "R3DistributedWanted",
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            assert_eq!(
                SEEN.lock().unwrap_or_else(|e| e.into_inner()).len(),
                heard,
                "a removed distributed observer still heard a post"
            );
        }
    }

    /// The five-argument registration is the four-argument one: the suspension
    /// behaviour describes what the *system* does with notifications that arrive
    /// while the process is suspended, and there is no such system here.
    #[test]
    fn a_suspension_behaviour_registration_still_hears_its_post() {
        crate::bootstrap();
        let who = observer(c"R3SuspendedObserver");
        // SAFETY: `v56@0:8@16:24@32@40q48` is what the selector declares.
        unsafe {
            distributed_add_observer_suspension(
                std::ptr::null_mut(),
                objc::sel::get("addObserver:selector:name:object:suspensionBehavior:"),
                who,
                objc::sel::get("heard:"),
                crate::string::from_str("R3SuspendedWanted"),
                std::ptr::null_mut(),
                2,
            );
            post_distributed(
                "R3SuspendedWanted",
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            distributed_remove_observer(
                std::ptr::null_mut(),
                objc::sel::get("removeObserver:"),
                who,
            );
        }
        assert!(
            SEEN.lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&"R3SuspendedWanted".into()),
            "the suspended-behaviour registration never heard its own post"
        );
    }

    /// `-removeObserver:name:object:` narrows a registration rather than
    /// dropping it, which is the behaviour a dealloc path depends on.
    #[test]
    fn a_named_removal_leaves_the_other_registrations_alone() {
        crate::bootstrap();
        let who = observer(c"R3NarrowObserver");
        // SAFETY: each prototype is the one its selector declares.
        unsafe {
            distributed_add_observer(
                std::ptr::null_mut(),
                objc::sel::get("addObserver:selector:name:object:"),
                who,
                objc::sel::get("heard:"),
                crate::string::from_str("R3NarrowA"),
                std::ptr::null_mut(),
            );
            distributed_add_observer(
                std::ptr::null_mut(),
                objc::sel::get("addObserver:selector:name:object:"),
                who,
                objc::sel::get("heard:"),
                crate::string::from_str("R3NarrowB"),
                std::ptr::null_mut(),
            );
            let before = SEEN.lock().unwrap_or_else(|e| e.into_inner()).len();
            distributed_remove_observer_name(
                std::ptr::null_mut(),
                objc::sel::get("removeObserver:name:object:"),
                who,
                crate::string::from_str("R3NarrowA"),
                std::ptr::null_mut(),
            );
            post_distributed("R3NarrowA", std::ptr::null_mut(), std::ptr::null_mut());
            post_distributed("R3NarrowB", std::ptr::null_mut(), std::ptr::null_mut());
            let after = SEEN.lock().unwrap_or_else(|e| e.into_inner()).len();
            assert_eq!(
                after - before,
                1,
                "the named removal did not narrow the registration"
            );
            distributed_remove_observer(
                std::ptr::null_mut(),
                objc::sel::get("removeObserver:"),
                who,
            );
        }
    }
}

/// The class names the guest imports from `UserNotifications.framework` and from
/// `Foundation.framework`, frozen from the census in the comment above.
///
/// This is a **copy of what the binary says**, not a derivation from it, and it
/// is here so that the table above cannot quietly drift from the guest. If a
/// later census finds another `UN*` classref, this is the list to extend - and
/// the census is what says so, not the presence of a name anyone expected.
#[cfg(test)]
const GUEST_IMPORTS: &[&str] = &[
    "UNUserNotificationCenter",
    "UNMutableNotificationContent",
    "UNNotificationAttachment",
    "UNNotificationRequest",
    "UNNotificationAction",
    "UNNotificationCategory",
    "UNTextInputNotificationAction",
    "UNTextInputNotificationResponse",
    "NSUserNotification",
    "NSUserNotificationCenter",
];

/// The table above, and the guest's import list, are two lists that have to
/// agree. The `unreachable!` in `system_notification_refusals` would catch a
/// class in the table with no arm; this catches the other direction, and the
/// direction that actually happens, which is a classref the census reports and
/// nobody adds.
#[test]
fn every_class_the_guest_imports_has_an_arm_and_vice_versa() {
    let tabled: std::collections::BTreeSet<&str> =
        SYSTEM_NOTIFICATIONS.iter().map(|(n, _, _)| *n).collect();
    let imported: std::collections::BTreeSet<&str> = GUEST_IMPORTS.iter().copied().collect();
    assert_eq!(
        tabled, imported,
        "the refusal table and the guest's UN*/NSUserNotification* imports have drifted"
    );
}

/// Every selector's declared arity is the arity its **name** has.
///
/// The colon count is a property of the name, and a mismatched encoding has
/// crashed this guest twice. It cannot be caught by calling these methods,
/// because every one of them aborts before it returns - so the only place the
/// check can live is here, against the name.
#[test]
fn every_declared_encoding_has_as_many_arguments_as_the_selector_has_colons() {
    for (class, class_methods, instance_methods) in SYSTEM_NOTIFICATIONS {
        for (kind, table) in [("+", class_methods), ("-", instance_methods)] {
            for (selector, types) in table.iter() {
                let colons = selector.bytes().filter(|b| *b == b':').count();
                // The encoding is `<return><arg>@<off>:<size>` repeated, where
                // `<return>` is `v` or `@<frame size in bytes>`. So the number of
                // `@` is one for the return if it is an object, plus one for each
                // argument - and the first argument is always `self`/`_cmd` at
                // offset 0, which is not one of the selector's arguments.
                let ats = types.to_bytes().iter().filter(|b| **b == b'@').count();
                let returns_object = types.to_bytes().first() == Some(&b'@');
                let args = ats - usize::from(returns_object) - 1;
                assert_eq!(
                    colons,
                    args,
                    "{class} {kind}{selector}: {colons} colon(s) but {args} argument(s) in {}",
                    types.to_string_lossy()
                );
                assert!(
                    types.to_bytes().starts_with(b"v") || types.to_bytes().starts_with(b"@"),
                    "{class} {kind}{selector}: a return type of {} is neither void nor an object",
                    types.to_string_lossy()
                );
            }
        }
    }
}

/// The encoding's offsets are the real ones, and not a copy of the neighbouring
/// line's.
///
/// A wrong offset is invisible until the guest reads the fifth argument out of
/// the wrong register, and a `0`/`:8` pair is easy to fat-finger. The return
/// value's word count is checked against `0`, `8`, `16` and so on in steps of
/// eight, and every argument's offset is checked to be in range and a multiple
/// of eight.
#[test]
fn every_encoding_names_offsets_that_are_multiples_of_eight_in_range() {
    for (class, class_methods, instance_methods) in SYSTEM_NOTIFICATIONS {
        for (_, table) in [("+", class_methods), ("-", instance_methods)] {
            for (selector, types) in table.iter() {
                let text = types.to_string_lossy().into_owned();
                let bytes = types.to_bytes();
                // `<return>` is `v`, or `@` followed by the whole frame's size in
                // bytes. That size is the only place the frame's extent is
                // written down for an object return, so it is the thing to check
                // against the arguments.
                // `<return>` is `v`, or `@` followed by the whole frame's size in
                // bytes. That size is the only place the frame's extent is written
                // down for an object return, so it is the thing to check against the
                // arguments.
                //
                // Each argument is then `@<offset>` or `@<offset>:<size>`; this tree
                // uses both spellings, and a parser that assumes one of them loses
                // the other's offsets - which is what a bare `farthest` of 8 on a
                // three-argument selector looks like from the outside.
                //
                // The frame size is written for a `void` return too - `v40@0:8@16@24` -
                // so both arms read the digits that follow the return type, and the
                // only difference between them is whether that return type is an
                // object.
                let (returns_object, rest) = match bytes.first() {
                    Some(b'v') => (false, &text[1..]),
                    Some(b'@') => (true, &text[1..]),
                    other => panic!("{class} {selector}: return type {other:?} in {text:?}"),
                };
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                let frame: usize = digits
                    .parse()
                    .unwrap_or_else(|_| panic!("{class} {selector}: no frame size in {text:?}"));
                assert_eq!(
                    frame % 8,
                    0,
                    "{class} {selector}: frame size {frame} in {text:?} is not a whole number of \
                     words"
                );
                assert!(
                    frame >= 16,
                    "{class} {selector}: {text:?} says a {frame}-byte frame, which cannot hold self \
                     and _cmd"
                );
                let rest = &rest[digits.len()..];
                let declared_frame = returns_object.then_some(frame);
                assert!(
                    rest.starts_with('@'),
                    "{class} {selector}: {text:?} has no arguments after its return type"
                );
                let mut farthest = 0usize;
                let mut arguments = 0usize;
                for part in rest[1..].split('@') {
                    let (offset, size) = match part.split_once(':') {
                        Some((o, s)) => (o, s),
                        None => (part, "8"),
                    };
                    let offset: usize = offset.parse().unwrap_or_else(|_| {
                        panic!("{class} {selector}: offset {offset:?} in {text:?}")
                    });
                    let size: usize = size.parse().unwrap_or_else(|_| {
                        panic!("{class} {selector}: size {size:?} in {text:?}")
                    });
                    assert_eq!(
                        offset % 8,
                        0,
                        "{class} {selector}: offset {offset} is not a multiple of eight"
                    );
                    assert_eq!(
                        size % 8,
                        0,
                        "{class} {selector}: size {size} is not a multiple of eight"
                    );
                    assert!(
                        size == 8 || size == 16,
                        "{class} {selector}: size {size} in {text:?} is neither a pointer nor two"
                    );
                    farthest = farthest.max(offset + size);
                    arguments += 1;
                }
                // `self` is the first entry and is not one of the selector's
                // arguments, so the count of entries is one more than the colon
                // count. Asserted here as well as in the other test because this
                // loop is the one that actually parses the offsets.
                assert_eq!(
                    arguments - 1,
                    selector.bytes().filter(|b| *b == b':').count(),
                    "{class} {selector}: {text:?} names {arguments} entries, so {} argument(s)",
                    arguments - 1
                );
                if let Some(declared) = declared_frame {
                    assert!(
                        declared >= farthest && declared - farthest <= 8,
                        "{class} {selector}: {text:?} says the frame is {declared} bytes but its \
                         arguments reach {farthest}"
                    );
                    assert!(
                        declared >= 16,
                        "{class} {selector}: {text:?} says a {declared}-byte frame, which cannot \
                         hold self and _cmd"
                    );
                } else {
                    assert_eq!(
                        farthest % 8,
                        0,
                        "{class} {selector}: {text:?} reaches {farthest}, not a whole number of words"
                    );
                }
            }
        }
    }
}

/// Every class is a real class carrying the selectors the census measured, and
/// not the empty placeholder `NAME_MISSING_CLASSES` builds.
///
/// The point is that the *message* now names the missing service, and the
/// behaviour is still to stop, so there is a way to see which of the two is
/// which: a class with no own methods is the empty placeholder, and this asserts
/// every one has its own.
#[test]
fn the_refusal_installs_a_method_for_every_measured_selector() {
    system_notification_refusals();

    for (class_name, class_methods, instance_methods) in SYSTEM_NOTIFICATIONS {
        let class = system_notification_class(class_name_cstr(class_name));
        assert!(
            class.is_registered(),
            "{class_name} is not registered, so the guest's classref would still be a zero"
        );
        assert!(
            class.chain().count() > 1,
            "{class_name} has no superclass, so +alloc and -isKindOfClass: would not resolve"
        );
        // Not "has own *instance* methods": `UNNotificationRequest` is reached
        // only through `+requestWithIdentifier:content:trigger:`, so all of its
        // methods live on the metaclass and the class itself has none.
        //
        // And not "has methods" at all, either, because the two classes the
        // census found with no measured selector have none **by design** - that
        // is the finding, and `the_two_classes_with_no_measured_selector_are_the_two_named`
        // is what holds it to account. What is checked here is the weaker and
        // still meaningful thing: a class that does have measured selectors has
        // all of them, on the side of the class they were measured on.
        if class_methods.is_empty() && instance_methods.is_empty() {
            continue;
        }
        assert!(
            class.chain().count() > 1,
            "{class_name} has no superclass, so +alloc and -isKindOfClass: would not resolve"
        );
        let have: std::collections::BTreeSet<String> = class
            .own_methods()
            .iter()
            .map(|m| String::from_utf8_lossy(sel_name_of(m)).into_owned())
            .collect();
        for (selector, _) in instance_methods.iter() {
            assert!(
                have.contains(*selector),
                "{class_name} -{selector} is not installed; it has {have:?}"
            );
        }
        let meta_have: std::collections::BTreeSet<String> = class
            .metaclass()
            .own_methods()
            .iter()
            .map(|m| String::from_utf8_lossy(sel_name_of(m)).into_owned())
            .collect();
        for (selector, _) in class_methods.iter() {
            assert!(
                meta_have.contains(*selector),
                "{class_name} +{selector} is not installed; it has {meta_have:?}"
            );
        }
    }
}

/// The two reasons are two reasons, and neither names the other's capability.
///
/// A refusal that attributed `usernotificationsd` to the deprecated Foundation
/// centre would be wrong in a way nothing else would catch: the behaviour is a
/// refusal either way, the class name is still right, and the sentence is only
/// read by whoever hits the wall. So the two constants are checked to be
/// different, to name the capability each claims, and - the part that matters -
/// for the Foundation pair **not** to name a daemon it does not have.
#[test]
fn the_two_reasons_name_two_different_missing_capabilities() {
    assert_ne!(
        MISSING_USER_NOTIFICATIONS_DAEMON, MISSING_NOTIFICATION_UI,
        "both classes are refused for the same stated reason, so one of them is misattributed"
    );
    assert!(
        MISSING_USER_NOTIFICATIONS_DAEMON.contains("usernotificationsd"),
        "the UserNotifications framework's centre is a handle on usernotificationsd, and the \
         message does not say so"
    );
    assert!(
        MISSING_NOTIFICATION_UI.contains("notification UI"),
        "the deprecated centre's client is the system's notification UI, and the message does not \
         say so"
    );
    assert!(
        !MISSING_NOTIFICATION_UI.contains("usernotificationsd"),
        "NSUserNotificationCenter has no daemon; naming one would send whoever reads this \
         looking for a process that was never involved"
    );
    // And neither may offer the in-process centre as a way out, because that is
    // the substitute the whole file exists to refuse.
    for (class, reason) in [
        ("UN*", MISSING_USER_NOTIFICATIONS_DAEMON),
        ("NSUserNotification*", MISSING_NOTIFICATION_UI),
    ] {
        assert!(
            !reason.contains("NSNotificationCenter"),
            "the reason for {class} offers NSNotificationCenter, which is the substitute this \
             refusal exists to refuse"
        );
    }
}

/// There is no gate, and that is the point: a refusal the default run cannot
/// reach is documentation.
///
/// What replaced the gate is the runtime's **own** switch. Under
/// `ROSETTA3_LOG_UNRECOGNIZED=1` the diagnostic lever must keep working across
/// this wall, and it does, because [`refuse_system_notification`] asks
/// `objc::msg::logging_unrecognised()` - the same function the rest of the
/// runtime asks - rather than keeping a private variable. So there is nothing
/// here for an operator to set, and nothing that can make this wall quieter than
/// the rest of the runtime.
#[test]
fn the_refusal_has_no_gate_of_its_own() {
    assert!(
        system_notification_refusals_enabled(),
        "the refusal is behind a variable again, so the default run does not hit it"
    );
    // And the function that decides whether to abort is the runtime's, so a run
    // that sets the diagnostic lever walks past this wall exactly as it walks
    // past every other unrecognised selector - which is the whole of what the
    // opt-in used to cost.
    assert_eq!(
        objc::msg::logging_unrecognised(),
        std::env::var_os("ROSETTA3_LOG_UNRECOGNIZED").is_some(),
        "the refusal and objc::msg disagree about whether the diagnostic switch is set"
    );
}

/// The four classes the census found with **no** measured selector are named in
/// the table comment, and are the only four.
///
/// This is a freeze on a census result. It was two classes until
/// `UNTextInputNotificationAction` and `UNTextInputNotificationResponse` were
/// found by scanning the bind stream's own spelling for the `_OBJC_CLASS_$_`
/// prefix: the run log that the earlier list came from prints 43 of the 122
/// missing classes, so its silence about a name was never evidence of anything.
/// If a later census finds a send to any of these four, this fails and the
/// comment above has to be corrected - which is the point: a note that can go
/// stale without anything noticing is not a note.
#[test]
fn the_four_classes_with_no_measured_selector_are_the_four_named() {
    let silent: Vec<&str> = SYSTEM_NOTIFICATIONS
        .iter()
        .filter(|(_, c, i)| c.is_empty() && i.is_empty())
        .map(|(n, _, _)| *n)
        .collect();
    assert_eq!(
        silent,
        vec![
            "UNTextInputNotificationAction",
            "UNNotificationAction",
            "UNNotificationCategory",
            "UNTextInputNotificationResponse",
        ],
        "the set of classes with no measured selector has changed; update the comment too"
    );
}

/// Every class in the refusal table has a row in [`CALLERS`], and every row names
/// a class in the table.
///
/// The message a reader gets at the fault is the only account of the caller that
/// will exist, and it is assembled from two independent lists. A class added to
/// one and not the other is the failure that matters: `caller_of` would answer
/// with the "not recorded" sentence, which is true and useless, and nothing
/// downstream would notice.
#[test]
fn every_refused_class_has_a_caller_and_vice_versa() {
    let tabled: std::collections::BTreeSet<&str> =
        SYSTEM_NOTIFICATIONS.iter().map(|(n, _, _)| *n).collect();
    let called: std::collections::BTreeSet<&str> = CALLERS.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        tabled, called,
        "the refusal table and the caller table have drifted, so a refusal would name a caller \
         that does not exist or say nothing at all"
    );
    for (class, text) in CALLERS {
        assert!(
            text.len() > 80,
            "{class}: the caller text is too short to be a measurement"
        );
        // Either it names an address, which is checkable against the binary, or
        // it reports the census's own negative - "no send to it" -
        // which is the measurement for the four classes the image imports and
        // never messages. What is not allowed is a sentence that could have
        // been written without looking. The negative is deliberately *not* a
        // stronger claim: this file has no census of classref loads for those
        // four, because the run log that would give their slot addresses prints
        // 43 of the 122 missing classes.
        assert!(
            text.contains("0x") || text.contains("no send to it"),
            "{class}: the caller text names no address and does not report the census's negative, \
             so it cannot be checked against the binary"
        );
    }
    // And the two classes the run actually walks must name an address each. A
    // census of a class nothing loads cannot be a sentence, and these two are
    // the ones the wall's message is read for.
    for class in ["NSUserNotificationCenter", "NSUserNotification"] {
        let text = CALLERS
            .iter()
            .find(|(n, _)| *n == class)
            .map(|(_, t)| *t)
            .unwrap();
        assert!(
            text.contains("0x"),
            "{class}: the caller text for a class the run walks names no address"
        );
    }
}

/// Every selector in the refusal table is one the census measured, and the census
/// for the two classes this run walks is frozen here.
///
/// `objcenc.py` **cannot** be the certifier for these rows: it finds install
/// sites by reading a `methods!` table, and these selectors arrive through a
/// `for ... in` over a `const` slice, so it reports 0 of them
/// (`objcenc.py ours | grep -c NSUserNotification` is 0). The two encoding tests
/// above are therefore the only arity check these rows get, and this is the check
/// that they are *real*.
///
/// The arm the run walks is `NSUserNotificationCenter` and `NSUserNotification`,
/// and these are their measured sends, from the census in the comment above:
/// three `deliveredNotifications` sites, two `removeDeliveredNotification:` sites,
/// and one each of the rest. `shouldPreventNotificationDismissalAfterDefaultAction`
/// is deliberately **absent**: it is a `respondsToSelector:` probe whose answer the
/// guest reads, so absence is tolerated and installing it would refuse a message
/// the guest is asking whether it can send.
#[test]
fn the_walked_arms_selectors_are_exactly_the_measured_ones() {
    let measured: &[(&str, &[(&str, &str)])] = &[
        (
            "NSUserNotificationCenter",
            &[
                ("+", "defaultUserNotificationCenter"),
                ("-", "deliverNotification:"),
                ("-", "deliveredNotifications"),
                ("-", "removeAllDeliveredNotifications"),
                ("-", "removeDeliveredNotification:"),
                ("-", "setDelegate:"),
                ("-", "delegate"),
            ],
        ),
        (
            "NSUserNotification",
            &[
                ("-", "setActionButtonTitle:"),
                ("-", "setContentImage:"),
                ("-", "setHasActionButton:"),
                ("-", "setIdentifier:"),
                ("-", "setInformativeText:"),
                ("-", "setSubtitle:"),
                ("-", "setTitle:"),
                ("-", "setUserInfo:"),
                ("-", "userInfo"),
            ],
        ),
    ];
    for (class, expected) in measured {
        let tabled = SYSTEM_NOTIFICATIONS
            .iter()
            .find(|(n, _, _)| n == class)
            .map(|(_, c, i)| {
                c.iter()
                    .map(|(s, _)| ("+", *s))
                    .chain(i.iter().map(|(s, _)| ("-", *s)))
                    .collect::<std::collections::BTreeSet<(&str, &str)>>()
            })
            .unwrap_or_else(|| panic!("{class} is not in the refusal table"));
        let want: std::collections::BTreeSet<(&str, &str)> = expected.iter().copied().collect();
        assert_eq!(
            tabled, want,
            "{class}: the table and the measured selector census have drifted"
        );
    }
}

#[cfg(test)]
/// A `Method`'s selector name, which is the selector itself.
fn sel_name_of(m: &objc::class::Method) -> &[u8] {
    // SAFETY: `objc::sel::name` is the selector, which `sel_registerName` made a
    // NUL-terminated string, and this stops at the NUL.
    unsafe { std::ffi::CStr::from_ptr(objc::sel::name(m.sel)) }.to_bytes()
}

/// The two registrations cannot fight, whichever order they run in.
///
/// `rosetta3::program::name_missing_classes` registers an empty class under the
/// guest's spelling and writes it into the classref slot, and this file's refusal
/// registers the same spelling with methods on it. The comment above claims
/// either order ends with one class object carrying the methods, and that claim
/// rests on two facts about `corefoundation::base::register` and
/// `Class::replace_method`. Both are checked here, on a name nothing else in
/// this tree registers, so the experiment does not depend on which tests ran
/// first.
#[test]
fn an_empty_class_registered_first_still_ends_up_carrying_the_methods() {
    let name = class_name_cstr("R3NotificationOrderingProbe");
    // What `name_missing_classes` does: an empty class, no superclass argument
    // beyond the default, no methods.
    let first = system_notification_class(name);
    let second = system_notification_class(name);
    assert_eq!(
        objc::class::ptr(first),
        objc::class::ptr(second),
        "registering one name twice must return one class, or the two paths publish two and the \
         slot ends up on whichever was last"
    );
    assert!(
        first.own_methods().is_empty(),
        "the probe class should start empty, or this is not testing the empty case"
    );
    // What this file's refusal then does.
    let sel = objc::sel::get("aSelectorNobodySends");
    first.replace_method(
        sel,
        refuse_pick(
            c"v32@0:8@16",
            refusing_stub as *const std::ffi::c_void,
            refusing_void_stub as *const std::ffi::c_void,
        ),
        c"v32@0:8@16",
    );
    assert_eq!(
        first.own_methods().len(),
        1,
        "the method did not land on the class the first registration returned"
    );
}

#[cfg(test)]
unsafe extern "C" fn refusing_stub(_receiver: Id, _sel: Sel) -> usize {
    0
}

#[cfg(test)]
unsafe extern "C" fn refusing_void_stub(_receiver: Id, _sel: Sel) {}
