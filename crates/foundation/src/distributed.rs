//! `NSDistributedNotificationCenter`, and the center
//! `CFNotificationCenterGetDistributedCenter` answers with, which are one
//! object as they are on a Mac.
//!
//! A post goes to every guest of the prefix through `darwin::distnote`, this
//! process included, and arrives on the main thread, which is where a Mac's
//! run loop delivers one. The object of a distributed notification is a string
//! naming the sender, compared by its text, and the user info must be a
//! property list - what is carried is the plist, as a Mac carries it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};
use crate::notification::{WeakObserver, text_of};
use crate::plist::Value;

/// One `-addObserver:selector:name:object:`, holding its observer weakly as
/// the local center does.
struct Observer {
    observer: WeakObserver,
    selector: Sel,
    /// `None` matches any name.
    name: Option<String>,
    /// `None` matches any sender.
    object: Option<String>,
}

// SAFETY: the ids are used only on the main thread, which is where delivery
// happens; the mutex serializes the table.
unsafe impl Send for Observer {}

static OBSERVERS: Mutex<Vec<Observer>> = Mutex::new(Vec::new());

struct BlockObserver {
    token: Id,
    /// A heap copy of the `void (^)(NSNotification *)`.
    block: usize,
    /// Retained; null delivers on the main thread, as everything here arrives.
    queue: Id,
    name: Option<String>,
    object: Option<String>,
}

// SAFETY: the block is a copy this table owns, and the objects belong to
// runtimes shared across threads already.
unsafe impl Send for BlockObserver {}

static BLOCK_OBSERVERS: Mutex<Vec<BlockObserver>> = Mutex::new(Vec::new());

static SUSPENDED: AtomicBool = AtomicBool::new(false);

/// Messages that arrived while suspended, delivered when that ends.
static HELD: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

fn class() -> &'static Class {
    let parent = objc::class::lookup(c"NSNotificationCenter");
    corefoundation::base::register(
        c"NSDistributedNotificationCenter",
        parent,
        size_of::<usize>(),
        false,
    )
}

/// The one distributed center.
pub fn center() -> Id {
    static CENTER: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CENTER.get_or_init(|| {
        let center = objc::object::create_instance(class(), 0);
        objc::arc::make_immortal(center);
        center as usize
    }) as Id
}

fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(crate::notification::TRACE_ENV).is_some())
}

/// Start hearing other processes' posts; the first observer does this.
pub(crate) fn listen_now() {
    darwin::distnote::listen(arrived);
}

/// A message from the transport, on its thread: hand it to the main thread.
fn arrived(message: Vec<u8>) {
    let context = Box::into_raw(Box::new(message)) as u64;
    let function = deliver_on_main as *const () as u64;
    // SAFETY: the box is consumed exactly once by `deliver_on_main`.
    unsafe {
        darwin::dispatch::queue::async_f(darwin::dispatch::queue::main_queue(), context, function)
    };
}

unsafe extern "C" fn deliver_on_main(context: *mut std::ffi::c_void) {
    // SAFETY: the box `arrived` made.
    let message = *unsafe { Box::from_raw(context.cast::<Vec<u8>>()) };
    if SUSPENDED.load(Ordering::Acquire) {
        HELD.lock().unwrap_or_else(|e| e.into_inner()).push(message);
        return;
    }
    deliver(&message);
}

/// What travels: the name, the sender's string and the user info.
fn encode(name: &str, object: Option<&str>, user_info: Option<Value>) -> Vec<u8> {
    let mut entries = vec![("name".to_owned(), Value::String(name.to_owned()))];
    if let Some(object) = object {
        entries.push(("object".to_owned(), Value::String(object.to_owned())));
    }
    if let Some(info) = user_info {
        entries.push(("userInfo".to_owned(), info));
    }
    crate::plist::write_xml(&Value::Dictionary(entries.into_iter().collect()))
}

fn decode(message: &[u8]) -> Option<(String, Option<String>, Option<Value>)> {
    let Ok(Value::Dictionary(entries)) = crate::plist::parse(message) else {
        return None;
    };
    let text = |key: &str| match entries.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    Some((
        text("name")?,
        text("object"),
        entries.get("userInfo").cloned(),
    ))
}

/// Run every observer of one arrived message.
fn deliver(message: &[u8]) {
    let Some((name, object, info)) = decode(message) else {
        return;
    };
    let matches = |n: &Option<String>, o: &Option<String>| {
        n.as_deref().is_none_or(|n| n == name)
            && o.as_deref().is_none_or(|o| Some(o) == object.as_deref())
    };
    let sender = object
        .as_deref()
        .map_or(std::ptr::null_mut(), crate::string::from_str);
    let user_info = info.as_ref().map_or(std::ptr::null_mut(), |v| {
        crate::objectify::object(v, "a distributed notification's user info")
    });
    let notification = crate::notification::make(&name, sender, user_info);
    objc::arc::release(sender);
    if tracing() {
        eprintln!("rosemary: foundation: distributed {name} arrived");
    }

    let targets: Vec<(Id, Sel)> = {
        let mut table = OBSERVERS.lock().unwrap_or_else(|e| e.into_inner());
        table.retain(|o| !o.observer.address().is_null());
        table
            .iter()
            .filter(|o| matches(&o.name, &o.object))
            .map(|o| (o.observer.load(), o.selector))
            .filter(|(observer, _)| !observer.is_null())
            .collect()
    };
    for (observer, selector) in targets {
        // SAFETY: an observer method takes the notification, by the contract
        // of `-addObserver:selector:name:object:`, and the observer is
        // retained for the send.
        unsafe { crate::notification::send_notification(observer, selector, notification) };
        objc::arc::release(observer);
    }
    let blocks: Vec<(usize, Id)> = BLOCK_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|o| matches(&o.name, &o.object))
        .map(|o| (objc::block::copy(o.block as Id) as usize, o.queue))
        .collect();
    for (block, queue) in blocks {
        crate::notification::deliver(block, queue, notification);
    }
    // SAFETY: the notification's object, which the CF observers compare by
    // identity as their table records it.
    let object = unsafe { crate::notification::object_of(notification) };
    crate::notification::post_to_callbacks(center(), &name, object.cast_const().cast(), user_info);
}

/// Post to every process in the prefix.
pub fn post(name: &str, object: Id, user_info: Id) {
    // SAFETY: null or a string, the only sender a distributed post may name.
    let object = (!object.is_null()).then(|| unsafe { text_of(object) });
    // SAFETY: null or a property list object.
    let info = (!user_info.is_null())
        .then(|| unsafe { corefoundation::propertylist::value_of(user_info as _) })
        .flatten();
    if !user_info.is_null() && info.is_none() {
        objc::fail!(
            "-[NSDistributedNotificationCenter postNotificationName:...]: the user info is not \
             a property list"
        );
    }
    let message = encode(name, object.as_deref(), info);
    if tracing() {
        eprintln!("rosemary: foundation: distributed post {name}");
    }
    if !darwin::distnote::post(&message) {
        // No prefix to post in: this process is the only listener there is.
        arrived(message);
    }
}

unsafe extern "C" fn default_center(_class: Id, _sel: Sel) -> Id {
    center()
}

unsafe extern "C" fn center_for_type(_class: Id, _sel: Sel, _kind: Id) -> Id {
    // `NSLocalNotificationCenterType` is the only type there is.
    center()
}

/// Null or the string's text.
fn text(object: Id) -> Option<String> {
    // SAFETY: null or a string.
    (!object.is_null()).then(|| unsafe { text_of(object) })
}

unsafe extern "C" fn add_observer_suspension(
    _receiver: Id,
    _sel: Sel,
    observer: Id,
    selector: Sel,
    name: Id,
    object: Id,
    _behavior: usize,
) {
    OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Observer {
            observer: WeakObserver::new(observer),
            selector,
            name: text(name),
            object: text(object),
        });
    listen_now();
}

unsafe extern "C" fn add_observer(
    receiver: Id,
    sel: Sel,
    observer: Id,
    selector: Sel,
    name: Id,
    object: Id,
) {
    // SAFETY: the same arguments, with the default behaviour.
    unsafe { add_observer_suspension(receiver, sel, observer, selector, name, object, 0) };
}

unsafe extern "C" fn add_observer_for_name(
    _receiver: Id,
    _sel: Sel,
    name: Id,
    object: Id,
    queue: Id,
    handler: Id,
) -> Id {
    let block = objc::block::copy(handler) as usize;
    if block == 0 {
        return std::ptr::null_mut();
    }
    let token = objc::object::create_instance(crate::notification::token_class(), 0);
    BLOCK_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(BlockObserver {
            token,
            block,
            queue: objc::arc::retain(queue),
            name: text(name),
            object: text(object),
        });
    listen_now();
    token
}

fn drop_blocks(keep: impl Fn(&BlockObserver) -> bool) {
    let mut dropped = Vec::new();
    BLOCK_OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|o| {
            let kept = keep(o);
            if !kept {
                dropped.push((o.block, o.queue, o.token));
            }
            kept
        });
    for (block, queue, token) in dropped {
        objc::block::release(block as Id);
        objc::arc::release(queue);
        objc::arc::release(token);
    }
}

unsafe extern "C" fn remove_observer(_receiver: Id, _sel: Sel, observer: Id) {
    OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|o| o.observer.address() != observer);
    drop_blocks(|o| o.token != observer);
}

unsafe extern "C" fn remove_observer_name(
    _receiver: Id,
    _sel: Sel,
    observer: Id,
    name: Id,
    object: Id,
) {
    let (name, object) = (text(name), text(object));
    let other = |o_name: &Option<String>, o_object: &Option<String>| {
        (name.is_some() && *o_name != name) || (object.is_some() && *o_object != object)
    };
    OBSERVERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|o| o.observer.address() != observer || other(&o.name, &o.object));
    drop_blocks(|o| o.token != observer || other(&o.name, &o.object));
}

unsafe extern "C" fn post_notification(_receiver: Id, _sel: Sel, notification: Id) {
    if notification.is_null() {
        return;
    }
    // SAFETY: a notification of this crate's.
    let (name, object, info) = unsafe {
        (
            crate::notification::name_of(notification),
            crate::notification::object_of(notification),
            crate::notification::user_info_of(notification),
        )
    };
    post(&text(name).unwrap_or_default(), object, info);
}

unsafe extern "C" fn post_name(_receiver: Id, _sel: Sel, name: Id, object: Id) {
    post(
        &text(name).unwrap_or_default(),
        object,
        std::ptr::null_mut(),
    );
}

unsafe extern "C" fn post_name_user_info(
    _receiver: Id,
    _sel: Sel,
    name: Id,
    object: Id,
    user_info: Id,
) {
    post(&text(name).unwrap_or_default(), object, user_info);
}

/// The `deliverImmediately:` and `options:` forms. Both say what to do for a
/// suspended *receiver*, which is the receiver's business here: a suspended
/// process holds what arrives either way.
unsafe extern "C" fn post_name_user_info_flag(
    _receiver: Id,
    _sel: Sel,
    name: Id,
    object: Id,
    user_info: Id,
    _flag: usize,
) {
    post(&text(name).unwrap_or_default(), object, user_info);
}

unsafe extern "C" fn suspended(_receiver: Id, _sel: Sel) -> u8 {
    SUSPENDED.load(Ordering::Acquire) as u8
}

unsafe extern "C" fn set_suspended(_receiver: Id, _sel: Sel, value: u8) {
    SUSPENDED.store(value != 0, Ordering::Release);
    if value == 0 {
        let held = std::mem::take(&mut *HELD.lock().unwrap_or_else(|e| e.into_inner()));
        for message in held {
            deliver(&message);
        }
    }
}

pub(crate) fn install() {
    let class = class();
    class_methods!(class, {
        "defaultCenter" => default_center, c"@16@0:8";
        "notificationCenterForType:" => center_for_type, c"@24@0:8@16";
    });
    methods!(class, {
        "addObserver:selector:name:object:" => add_observer, c"v48@0:8@16:24@32@40";
        "addObserver:selector:name:object:suspensionBehavior:"
            => add_observer_suspension, c"v56@0:8@16:24@32@40Q48";
        "addObserverForName:object:queue:usingBlock:"
            => add_observer_for_name, c"@48@0:8@16@24@32@?40";
        "removeObserver:" => remove_observer, c"v24@0:8@16";
        "removeObserver:name:object:" => remove_observer_name, c"v40@0:8@16@24@32";
        "postNotification:" => post_notification, c"v24@0:8@16";
        "postNotificationName:object:" => post_name, c"v32@0:8@16@24";
        "postNotificationName:object:userInfo:" => post_name_user_info, c"v40@0:8@16@24@32";
        "postNotificationName:object:userInfo:deliverImmediately:"
            => post_name_user_info_flag, c"v44@0:8@16@24@32c40";
        "postNotificationName:object:userInfo:options:"
            => post_name_user_info_flag, c"v48@0:8@16@24@32Q40";
        "suspended" => suspended, c"c16@0:8";
        "setSuspended:" => set_suspended, c"v20@0:8c16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_carries_name_sender_and_user_info() {
        let info = Value::Dictionary(
            [("count".to_owned(), Value::Integer(3))]
                .into_iter()
                .collect(),
        );
        let message = encode("com.example.ping", Some("sender"), Some(info.clone()));
        let (name, object, user_info) = decode(&message).expect("a message");
        assert_eq!(name, "com.example.ping");
        assert_eq!(object.as_deref(), Some("sender"));
        assert_eq!(user_info, Some(info));
    }

    #[test]
    fn a_message_may_name_no_sender() {
        let (_, object, user_info) = decode(&encode("n", None, None)).expect("a message");
        assert_eq!(object, None);
        assert_eq!(user_info, None);
    }
}
