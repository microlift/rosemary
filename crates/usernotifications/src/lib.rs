use std::ffi::{CStr, c_void};
use std::ptr::null_mut;
use std::sync::OnceLock;

use foundation::boxed;
use objc::{class::Class, object::Id, sel::Sel};

const DENIED: i64 = 1;
const NOTIFICATIONS_NOT_ALLOWED: i64 = 1;
const CLASSES: &[&CStr] = &[
    c"UNUserNotificationCenter",
    c"UNNotificationSettings",
    c"UNNotificationContent",
    c"UNMutableNotificationContent",
    c"UNNotificationAction",
    c"UNNotificationCategory",
    c"UNNotificationRequest",
    c"UNNotificationSound",
    c"UNNotificationTrigger",
    c"UNTimeIntervalNotificationTrigger",
];

// Slots are owned copies; immutable values can share them across copies.
#[derive(Default)]
struct State {
    objects: [Id; 12],
    options: u64,
    interval: f64,
    repeats: bool,
}

impl Drop for State {
    fn drop(&mut self) {
        for object in self.objects {
            objc::arc::release(object);
        }
    }
}

fn class(name: &CStr) -> &'static Class {
    objc::class::lookup(name).expect("UserNotifications class registered")
}

unsafe fn state<'a>(object: Id) -> &'a mut State {
    let cls = unsafe { objc::object::class_of(object) }.expect("live notification object");
    unsafe { boxed::contents(object, cls) }.expect("initialized notification object")
}

unsafe fn send_copy(object: Id) -> Id {
    if object.is_null() {
        return object;
    }
    let sel = objc::sel::get("copyWithZone:");
    let method = unsafe { objc::object::class_of(object) }
        .unwrap()
        .method(sel)
        .expect("NSCopying");
    let imp: unsafe extern "C" fn(Id, Sel, Id) -> Id =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { imp(object, sel, null_mut()) }
}

unsafe extern "C" fn init(object: Id, _: Sel) -> Id {
    let mut value = State::default();
    if unsafe { corefoundation::base::is_kind_of(object as _, class(c"UNNotificationContent")) } {
        for index in [0, 1, 2, 5, 6] {
            value.objects[index] = corefoundation::string::immortal("") as Id;
        }
        value.objects[4] = corefoundation::collections::new_dictionary(
            &[],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        ) as Id;
    }
    unsafe { boxed::fill(object, value) };
    object
}

fn make(name: &CStr, state: State) -> Id {
    objc::arc::autorelease(boxed::make(class(name), state))
}

unsafe extern "C" fn copy(object: Id, _: Sel, _: Id) -> Id {
    let old = unsafe { state(object) };
    let contents = State {
        objects: old.objects.map(objc::arc::retain),
        options: old.options,
        interval: old.interval,
        repeats: old.repeats,
    };
    let cls = unsafe { objc::object::class_of(object) }.unwrap();
    let target = if cls.name_str() == "UNMutableNotificationContent" {
        class(c"UNNotificationContent")
    } else {
        cls
    };
    boxed::make(target, contents)
}

unsafe extern "C" fn mutable_copy(object: Id, _: Sel, _: Id) -> Id {
    let result = unsafe { copy(object, objc::sel::get("copyWithZone:"), null_mut()) };
    let old = unsafe { state(result) };
    let contents = State {
        objects: old.objects.map(objc::arc::retain),
        ..Default::default()
    };
    objc::arc::release(result);
    boxed::make(class(c"UNMutableNotificationContent"), contents)
}

macro_rules! property {
    ($get:ident, $set:ident, $index:expr, $copy:expr) => {
        unsafe extern "C" fn $get(object: Id, _: Sel) -> Id {
            unsafe { state(object).objects[$index] }
        }
        unsafe extern "C" fn $set(object: Id, _: Sel, value: Id) {
            let owned = if $copy {
                unsafe { send_copy(value) }
            } else {
                objc::arc::retain(value)
            };
            let old = unsafe { std::mem::replace(&mut state(object).objects[$index], owned) };
            objc::arc::release(old);
        }
    };
}
property!(title, set_title, 0, true);
property!(subtitle, set_subtitle, 1, true);
property!(body, set_body, 2, true);
property!(sound, set_sound, 3, true);
property!(user_info, set_user_info, 4, true);
property!(category_identifier, set_category_identifier, 5, true);
property!(thread_identifier, set_thread_identifier, 6, true);
property!(badge, set_badge, 7, true);
property!(identifier, set_identifier, 8, true);
property!(actions, set_actions, 9, true);
property!(intents, set_intents, 10, true);
property!(trigger, set_trigger, 11, false);
// Request content occupies slot zero; action title occupies slot one.
property!(content, set_content, 0, true);

struct Center {
    delegate: Id,
}
impl Drop for Center {
    fn drop(&mut self) {
        objc::arc::destroy_weak(&mut self.delegate);
    }
}
unsafe extern "C" fn current(_: Id, _: Sel) -> Id {
    static CENTER: OnceLock<usize> = OnceLock::new();
    *CENTER.get_or_init(|| {
        boxed::make(
            class(c"UNUserNotificationCenter"),
            Center {
                delegate: null_mut(),
            },
        ) as usize
    }) as Id
}
unsafe extern "C" fn delegate(object: Id, _: Sel) -> Id {
    let center =
        unsafe { boxed::contents::<Center>(object, class(c"UNUserNotificationCenter")) }.unwrap();
    objc::arc::autorelease(objc::arc::load_weak_retained(&mut center.delegate))
}
unsafe extern "C" fn set_delegate(object: Id, _: Sel, value: Id) {
    let center =
        unsafe { boxed::contents::<Center>(object, class(c"UNUserNotificationCenter")) }.unwrap();
    objc::arc::store_weak(&mut center.delegate, value);
}

enum Reply {
    Authorization,
    Settings,
    Error,
}
struct Pending {
    block: Id,
    reply: Reply,
}
unsafe fn enqueue(block: Id, reply: Reply) {
    if block.is_null() {
        return;
    }
    let context = Box::into_raw(Box::new(Pending {
        block: objc::block::copy(block),
        reply,
    }));
    unsafe {
        darwin::dispatch::queue::async_f(
            darwin::dispatch::queue::get_global_queue(0, 0),
            context as u64,
            answer as *const () as u64,
        )
    };
}
unsafe extern "C" fn answer(context: *mut c_void) {
    let pending = unsafe { Box::from_raw(context.cast::<Pending>()) };
    let pool = objc::arc::pool_push();
    let invoke = unsafe { *((pending.block as *const u8).add(16).cast::<usize>()) };
    if invoke != 0 {
        match pending.reply {
            Reply::Authorization => {
                let call: unsafe extern "C" fn(Id, i8, Id) = unsafe { std::mem::transmute(invoke) };
                unsafe { call(pending.block, 0, null_mut()) };
            }
            reply => {
                let value = match reply {
                    Reply::Settings => make(c"UNNotificationSettings", State::default()),
                    _ => foundation::error::make(
                        "UNErrorDomain",
                        NOTIFICATIONS_NOT_ALLOWED,
                        "Notifications are not allowed",
                    ),
                };
                let call: unsafe extern "C" fn(Id, Id) = unsafe { std::mem::transmute(invoke) };
                unsafe { call(pending.block, value) };
            }
        }
    }
    objc::block::release(pending.block);
    objc::arc::pool_pop(pool);
}
unsafe extern "C" fn request_authorization(_: Id, _: Sel, _: u64, block: Id) {
    unsafe { enqueue(block, Reply::Authorization) };
}
unsafe extern "C" fn settings(_: Id, _: Sel, block: Id) {
    unsafe { enqueue(block, Reply::Settings) };
}
unsafe extern "C" fn add_request(_: Id, _: Sel, _: Id, block: Id) {
    unsafe { enqueue(block, Reply::Error) };
}
unsafe extern "C" fn noop(_: Id, _: Sel) {}
unsafe extern "C" fn noop_object(_: Id, _: Sel, _: Id) {}
unsafe extern "C" fn denied(_: Id, _: Sel) -> i64 {
    DENIED
}
unsafe extern "C" fn disabled(_: Id, _: Sel) -> i64 {
    1
}
unsafe extern "C" fn no(_: Id, _: Sel) -> bool {
    false
}
unsafe extern "C" fn options(object: Id, _: Sel) -> u64 {
    unsafe { state(object).options }
}
unsafe extern "C" fn interval(object: Id, _: Sel) -> f64 {
    unsafe { state(object).interval }
}
unsafe extern "C" fn repeats(object: Id, _: Sel) -> bool {
    unsafe { state(object).repeats }
}
unsafe extern "C" fn default_sound(_: Id, _: Sel) -> Id {
    static SOUND: OnceLock<usize> = OnceLock::new();
    *SOUND.get_or_init(|| boxed::make(class(c"UNNotificationSound"), State::default()) as usize)
        as Id
}
unsafe extern "C" fn action(_: Id, _: Sel, identifier: Id, title: Id, options: u64) -> Id {
    let result = make(
        c"UNNotificationAction",
        State {
            options,
            ..Default::default()
        },
    );
    unsafe {
        set_identifier(result, objc::sel::get("init"), identifier);
        set_subtitle(result, objc::sel::get("init"), title);
    }
    result
}
unsafe extern "C" fn category(
    _: Id,
    _: Sel,
    identifier: Id,
    actions: Id,
    intents: Id,
    options: u64,
) -> Id {
    let result = make(
        c"UNNotificationCategory",
        State {
            options,
            ..Default::default()
        },
    );
    unsafe {
        set_identifier(result, objc::sel::get("init"), identifier);
        set_actions(result, objc::sel::get("init"), actions);
        set_intents(result, objc::sel::get("init"), intents);
    }
    result
}
unsafe extern "C" fn request(_: Id, _: Sel, identifier: Id, content: Id, trigger: Id) -> Id {
    let result = make(c"UNNotificationRequest", State::default());
    unsafe {
        set_identifier(result, objc::sel::get("init"), identifier);
        set_content(result, objc::sel::get("init"), content);
        set_trigger(result, objc::sel::get("init"), trigger);
    }
    result
}
unsafe extern "C" fn time_trigger(_: Id, _: Sel, interval: f64, repeats: bool) -> Id {
    make(
        c"UNTimeIntervalNotificationTrigger",
        State {
            interval,
            repeats,
            ..Default::default()
        },
    )
}

macro_rules! methods {
    ($class:expr, { $($name:literal => $imp:expr, $types:literal);* $(;)? }) => {
        $( $class.replace_method(objc::sel::get($name), $imp as *const c_void, $types); )*
    }
}

pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        let center = boxed::register::<Center>(c"UNUserNotificationCenter", None);
        for &name in &CLASSES[1..] {
            let parent = match name.to_bytes() {
                b"UNMutableNotificationContent" => Some(class(c"UNNotificationContent")),
                b"UNTimeIntervalNotificationTrigger" => Some(class(c"UNNotificationTrigger")),
                _ => None,
            };
            let cls = boxed::register::<State>(name, parent);
            methods!(cls, { "init" => init, c"@16@0:8"; "copyWithZone:" => copy, c"@24@0:8^v16" });
        }
        // FIXME (ACCURACY): No notification service; authorization stays denied.
        methods!(center.metaclass(), { "currentNotificationCenter" => current, c"@16@0:8" });
        // FIXME (ACCURACY): Categories are discarded; there is no delivery store.
        methods!(center, {
            "delegate" => delegate, c"@16@0:8"; "setDelegate:" => set_delegate, c"v24@0:8@16";
            "requestAuthorizationWithOptions:completionHandler:" => request_authorization, c"v32@0:8Q16@?24";
            "getNotificationSettingsWithCompletionHandler:" => settings, c"v24@0:8@?16";
            "addNotificationRequest:withCompletionHandler:" => add_request, c"v32@0:8@16@?24";
            "setNotificationCategories:" => noop_object, c"v24@0:8@16";
            "removeAllPendingNotificationRequests" => noop, c"v16@0:8";
            "removeAllDeliveredNotifications" => noop, c"v16@0:8";
            "removePendingNotificationRequestsWithIdentifiers:" => noop_object, c"v24@0:8@16";
            "removeDeliveredNotificationsWithIdentifiers:" => noop_object, c"v24@0:8@16"
        });
        let content = class(c"UNNotificationContent");
        methods!(content, { "mutableCopyWithZone:" => mutable_copy, c"@24@0:8^v16" });
        methods!(content, { "title" => title, c"@16@0:8" });
        methods!(class(c"UNMutableNotificationContent"), { "setTitle:" => set_title, c"v24@0:8@16" });
        methods!(content, { "subtitle" => subtitle, c"@16@0:8" });
        methods!(class(c"UNMutableNotificationContent"), { "setSubtitle:" => set_subtitle, c"v24@0:8@16" });
        methods!(content, { "body" => body, c"@16@0:8" });
        methods!(class(c"UNMutableNotificationContent"), { "setBody:" => set_body, c"v24@0:8@16" });
        methods!(content, { "sound" => sound, c"@16@0:8" });
        methods!(class(c"UNMutableNotificationContent"), { "setSound:" => set_sound, c"v24@0:8@16" });
        methods!(content, { "userInfo" => user_info, c"@16@0:8" });
        methods!(class(c"UNMutableNotificationContent"), { "setUserInfo:" => set_user_info, c"v24@0:8@16" });
        methods!(content, { "categoryIdentifier" => category_identifier, c"@16@0:8" });
        methods!(class(c"UNMutableNotificationContent"), { "setCategoryIdentifier:" => set_category_identifier, c"v24@0:8@16" });
        methods!(content, { "threadIdentifier" => thread_identifier, c"@16@0:8" });
        methods!(class(c"UNMutableNotificationContent"), { "setThreadIdentifier:" => set_thread_identifier, c"v24@0:8@16" });
        methods!(content, { "badge" => badge, c"@16@0:8" });
        methods!(class(c"UNMutableNotificationContent"), { "setBadge:" => set_badge, c"v24@0:8@16" });
        methods!(class(c"UNNotificationSettings"), { "authorizationStatus" => denied, c"q16@0:8"; "providesAppNotificationSettings" => no, c"B16@0:8" });
        // FIXME (ACCURACY): All delivery channels are disabled without a service.
        for selector in ["soundSetting", "badgeSetting", "alertSetting", "notificationCenterSetting", "lockScreenSetting", "carPlaySetting", "criticalAlertSetting", "announcementSetting", "scheduledDeliverySetting", "timeSensitiveSetting"] {
            class(c"UNNotificationSettings").replace_method(objc::sel::get(selector), disabled as *const c_void, c"q16@0:8");
        }
        methods!(class(c"UNNotificationAction").metaclass(), { "actionWithIdentifier:title:options:" => action, c"@40@0:8@16@24Q32" });
        methods!(class(c"UNNotificationAction"), { "identifier" => identifier, c"@16@0:8"; "title" => subtitle, c"@16@0:8"; "options" => options, c"Q16@0:8" });
        methods!(class(c"UNNotificationCategory").metaclass(), { "categoryWithIdentifier:actions:intentIdentifiers:options:" => category, c"@48@0:8@16@24@32Q40" });
        methods!(class(c"UNNotificationCategory"), { "identifier" => identifier, c"@16@0:8"; "actions" => actions, c"@16@0:8"; "intentIdentifiers" => intents, c"@16@0:8"; "options" => options, c"Q16@0:8" });
        methods!(class(c"UNNotificationRequest").metaclass(), { "requestWithIdentifier:content:trigger:" => request, c"@40@0:8@16@24@32" });
        methods!(class(c"UNNotificationRequest"), { "identifier" => identifier, c"@16@0:8"; "content" => content_getter, c"@16@0:8"; "trigger" => trigger, c"@16@0:8" });
        methods!(class(c"UNNotificationSound").metaclass(), { "defaultSound" => default_sound, c"@16@0:8" });
        methods!(class(c"UNTimeIntervalNotificationTrigger").metaclass(), { "triggerWithTimeInterval:repeats:" => time_trigger, c"@32@0:8d16B24" });
        methods!(class(c"UNNotificationTrigger"), { "repeats" => repeats, c"B16@0:8" });
        methods!(class(c"UNTimeIntervalNotificationTrigger"), { "timeInterval" => interval, c"d16@0:8" });
    });
}

unsafe extern "C" fn content_getter(object: Id, sel: Sel) -> Id {
    unsafe { content(object, sel) }
}

pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut symbols = corefoundation::base::string_globals(&[("_UNErrorDomain", "UNErrorDomain")]);
    for &name in CLASSES {
        let cls = class(name);
        for (prefix, cls) in [
            ("_OBJC_CLASS_$_", cls),
            ("_OBJC_METACLASS_$_", cls.metaclass()),
        ] {
            symbols.push((
                format!("{prefix}{}", name.to_str().unwrap()),
                objc::class::ptr(cls) as usize as u64,
            ));
        }
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, mpsc};

    fn sel() -> Sel {
        objc::sel::get("init")
    }

    #[test]
    fn notification_values_copy_and_preserve_properties() {
        bootstrap();
        let pool = objc::arc::pool_push();
        unsafe {
            let object = boxed::make(class(c"UNMutableNotificationContent"), State::default());
            let text = corefoundation::string::immortal("hello") as Id;
            for setter in [
                set_title,
                set_subtitle,
                set_body,
                set_category_identifier,
                set_thread_identifier,
            ] {
                setter(object, sel(), text);
            }
            let dict = corefoundation::collections::new_dictionary(
                &[],
                corefoundation::callbacks::Policy::from_type_callbacks(),
                corefoundation::callbacks::Policy::from_type_callbacks(),
                false,
            ) as Id;
            set_user_info(object, sel(), dict);
            objc::arc::release(dict);
            let snd = default_sound(null_mut(), sel());
            set_sound(object, sel(), snd);
            let copied = copy(object, sel(), null_mut());
            assert_ne!(copied, object);
            assert_eq!(
                objc::object::class_of(copied).unwrap().name_str(),
                "UNNotificationContent"
            );
            set_title(
                object,
                sel(),
                corefoundation::string::immortal("changed") as Id,
            );
            for getter in [
                title,
                subtitle,
                body,
                category_identifier,
                thread_identifier,
            ] {
                assert_eq!(
                    corefoundation::string::as_string(getter(copied, sel()) as _)
                        .unwrap()
                        .utf8(),
                    b"hello"
                );
            }
            assert!(!user_info(copied, sel()).is_null());
            assert!(!sound(copied, sel()).is_null());
            let fresh = objc::object::create_instance(class(c"UNMutableNotificationContent"), 0);
            init(fresh, sel());
            assert_eq!(
                corefoundation::string::as_string(title(fresh, sel()) as _)
                    .unwrap()
                    .utf8(),
                b""
            );
            assert!(!user_info(fresh, sel()).is_null());
            assert!(badge(fresh, sel()).is_null());
            objc::arc::release(fresh);
            let req = request(null_mut(), sel(), text, object, null_mut());
            assert_ne!(content(req, sel()), object);
            let cat = category(null_mut(), sel(), text, null_mut(), null_mut(), 5);
            assert_eq!(options(cat, sel()), 5);
            let act = action(null_mut(), sel(), text, text, 3);
            assert_eq!(options(act, sel()), 3);
            let time = time_trigger(null_mut(), sel(), 60.0, true);
            assert_eq!(interval(time, sel()), 60.0);
            assert!(repeats(time, sel()));
            assert_eq!(default_sound(null_mut(), sel()), snd);
            objc::arc::release(copied);
            objc::arc::release(object);
        }
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn notification_center_singleton_has_a_weak_delegate_and_exports() {
        let symbols = data_symbols();
        assert_eq!(symbols.len(), CLASSES.len() * 2 + 1);
        assert!(symbols.iter().all(|(_, value)| *value != 0));
        let pool = objc::arc::pool_push();
        unsafe {
            let center = current(null_mut(), sel());
            assert_eq!(center, current(null_mut(), sel()));
            let object = boxed::make(class(c"UNNotificationSound"), State::default());
            set_delegate(center, sel(), object);
            let inner = objc::arc::pool_push();
            assert_eq!(delegate(center, sel()), object);
            objc::arc::pool_pop(inner);
            objc::arc::release(object);
            assert!(delegate(center, sel()).is_null());
        }
        objc::arc::pool_pop(pool);
    }

    #[repr(C)]
    struct Block {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: *const c_void,
        descriptor: *const c_void,
    }
    static REPLIES: Mutex<Option<mpsc::Sender<(std::thread::ThreadId, i64)>>> = Mutex::new(None);
    unsafe extern "C" fn authorized(_: Id, granted: i8, error: Id) {
        assert_eq!(granted, 0);
        assert!(error.is_null());
        REPLIES
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send((std::thread::current().id(), 0))
            .unwrap();
    }
    unsafe extern "C" fn got_settings(_: Id, value: Id) {
        let cls = unsafe { objc::object::class_of(value) }.unwrap();
        let method = cls.method(objc::sel::get("authorizationStatus")).unwrap();
        let call: unsafe extern "C" fn(Id, Sel) -> i64 =
            unsafe { std::mem::transmute(method.implementation()) };
        let status = unsafe { call(value, sel()) };
        REPLIES
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send((std::thread::current().id(), status))
            .unwrap();
    }
    unsafe extern "C" fn got_error(_: Id, value: Id) {
        let cls = unsafe { objc::object::class_of(value) }.unwrap();
        let method = cls.method(objc::sel::get("code")).unwrap();
        let call: unsafe extern "C" fn(Id, Sel) -> i64 =
            unsafe { std::mem::transmute(method.implementation()) };
        assert_eq!(unsafe { call(value, sel()) }, NOTIFICATIONS_NOT_ALLOWED);
        let method = cls.method(objc::sel::get("domain")).unwrap();
        let call: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(method.implementation()) };
        assert_eq!(
            unsafe { corefoundation::string::as_string(call(value, sel()) as _) }
                .unwrap()
                .utf8(),
            b"UNErrorDomain"
        );
        REPLIES
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send((std::thread::current().id(), 2))
            .unwrap();
    }

    #[test]
    fn notification_callbacks_deny_on_a_background_queue() {
        bootstrap();
        let (tx, rx) = mpsc::channel();
        *REPLIES.lock().unwrap() = Some(tx);
        let caller = std::thread::current().id();
        for (invoke, reply, expected) in [
            (authorized as *const c_void, Reply::Authorization, 0),
            (got_settings as *const c_void, Reply::Settings, DENIED),
            (got_error as *const c_void, Reply::Error, 2),
        ] {
            let block = Box::leak(Box::new(Block {
                isa: objc::class::ptr(class(c"__NSGlobalBlock__")) as _,
                flags: 1 << 28,
                reserved: 0,
                invoke,
                descriptor: std::ptr::null(),
            }));
            let block = block as *mut Block as Id;
            unsafe {
                match reply {
                    Reply::Authorization => request_authorization(null_mut(), sel(), 7, block),
                    Reply::Settings => settings(null_mut(), sel(), block),
                    Reply::Error => add_request(null_mut(), sel(), null_mut(), block),
                }
            }
            let (thread, value) = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            assert_ne!(thread, caller);
            assert_eq!(value, expected);
        }
        *REPLIES.lock().unwrap() = None;
    }
}
