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
pub const TRACE_ENV: &str = "ROSEMARY_TRACE_EVENT";

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
        eprintln!("rosemary: foundation: post {name} object {object:p} to {takers} observer(s)");
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
        objc::arc::make_immortal(center);
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
            "rosemary: foundation: addObserver {observer:p} -{} for {} object {object:p}",
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
            objc::arc::make_immortal(center);
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
        let who = observer(c"RosemaryTestNotificationObserver");
        OBSERVERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Observer {
                observer: who,
                selector: objc::sel::get("heard:"),
                name: Some("RosemaryWanted".into()),
                object: std::ptr::null_mut(),
            });
        SEEN.lock().unwrap_or_else(|e| e.into_inner()).clear();
        post(
            "RosemaryUnwanted",
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        post("RosemaryWanted", std::ptr::null_mut(), std::ptr::null_mut());
        let seen = SEEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(seen, ["RosemaryWanted"]);
    }

    /// The notification the observer is handed carries the sender it was posted
    /// with, which is the whole reason the argument is a real object.
    #[test]
    fn a_notification_carries_its_sender() {
        crate::bootstrap();
        let sender = crate::string::from_str("sender");
        let n = make("RosemaryNamed", sender, std::ptr::null_mut());
        // SAFETY: `-object` is `id (id, SEL)`.
        let got = unsafe { object(n, objc::sel::get("object")) };
        assert_eq!(got, sender);
        // SAFETY: `-name` is `id (id, SEL)`, and its result is a string.
        let text = unsafe { text_of(name(n, objc::sel::get("name"))) };
        assert_eq!(text, "RosemaryNamed");
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
            "RosemaryBlockWanted",
            std::ptr::null_mut(),
            record_block as *const c_void,
        );
        HEARD.lock().unwrap_or_else(|e| e.into_inner()).clear();
        post(
            "RosemaryBlockUnwanted",
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        post(
            "RosemaryBlockWanted",
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        assert_eq!(
            HEARD.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            ["RosemaryBlockWanted"]
        );

        // SAFETY: `-removeObserver:` is `void (id, SEL, id)`.
        unsafe {
            remove_observer(
                std::ptr::null_mut(),
                objc::sel::get("removeObserver:"),
                token,
            )
        };
        post(
            "RosemaryBlockWanted",
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
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
            add_block_observer("RosemaryBlockQueued", queue, record_thread as *const c_void);
        DELIVERED.store(0, Ordering::SeqCst);
        post(
            "RosemaryBlockQueued",
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );

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
}
