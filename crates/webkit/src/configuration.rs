use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::engine::{self, Engine};
use crate::gtk::{self, Ptr};
use crate::send;
use foundation::boxed;

#[derive(Default)]
pub struct Configuration {
    /// Each retained, and made on first use when the guest set none.
    controller: usize,
    process_pool: usize,
    data_store: usize,
    preferences: usize,
    pub application_name: Option<String>,
}

#[derive(Clone)]
pub struct Script {
    pub source: String,
    pub at_document_start: bool,
    pub main_frame_only: bool,
}

#[derive(Default)]
pub struct Controller {
    scripts: Vec<Script>,
    /// The `WebKitUserContentManager` the scripts are mirrored into, once a
    /// view has needed one.
    manager: usize,
    /// Script message handlers by name, each retained, as WebKit retains them.
    handlers: Vec<(String, usize)>,
    /// The user agent page scripts are shown, where WebKitGTK is sent a
    /// respelling of it; see [`crate::web_view::header_user_agent`].
    page_user_agent: Option<String>,
}

impl Drop for Controller {
    fn drop(&mut self) {
        for (_, handler) in self.handlers.drain(..) {
            objc::arc::release(handler as Id);
        }
        let manager = self.manager;
        MANAGERS.with(|managers| managers.borrow_mut().retain(|(m, _)| *m != manager));
    }
}

thread_local! {
    /// Each manager's controller, which its message signals are delivered to.
    static MANAGERS: std::cell::RefCell<Vec<(usize, usize)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Every handler name registered, indexed by the data its signal carries.
    static NAMES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn name_id(name: &str) -> usize {
    NAMES.with(|names| {
        let mut names = names.borrow_mut();
        match names.iter().position(|n| n == name) {
            Some(at) => at,
            None => {
                names.push(name.to_string());
                names.len() - 1
            }
        }
    })
}

/// `script-message-received::NAME`: a page called
/// `window.webkit.messageHandlers.NAME.postMessage(body)`.
unsafe extern "C" fn script_message_received(manager: Ptr, value: Ptr, data: Ptr) {
    let Some(engine) = engine::get() else {
        return;
    };
    let Some(name) = NAMES.with(|names| names.borrow().get(data as usize).cloned()) else {
        return;
    };
    let Some(object) = MANAGERS.with(|managers| {
        managers
            .borrow()
            .iter()
            .find(|(m, _)| *m == manager as usize)
            .map(|(_, c)| *c as Id)
    }) else {
        return;
    };
    // SAFETY: a live controller, registered while it is.
    let Some(handler) = (unsafe { controller(object) }).and_then(|c| {
        c.handlers
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, h)| *h as Id)
    }) else {
        return;
    };
    // SAFETY: the signal's value, live for its duration.
    let body = unsafe { crate::web_view::bridge(engine.api, value) };
    if engine::tracing() {
        eprintln!("rosemary: webkit: script message to '{name}'");
    }
    let message = crate::web_view::script_message(manager as usize, &name, body);
    // SAFETY: a handler answers the `WKScriptMessageHandler` method.
    unsafe {
        send!(handler, "userContentController:didReceiveScriptMessage:",
            object => Id, message => Id; ())
    };
}

/// Expose the handler `name` to pages as `window.webkit.messageHandlers`
/// does on macOS.
fn register_handler(engine: &Engine, manager: Ptr, name: &str) {
    let api = engine.api;
    let c_name = gtk::c_string(name);
    let signal = gtk::c_string(&format!("script-message-received::{name}"));
    // SAFETY: a live manager, strings it copies, and a handler of the signal's
    // prototype whose data is a plain integer.
    unsafe {
        (api.webkit_user_content_manager_register_script_message_handler)(
            manager,
            c_name.as_ptr(),
            std::ptr::null(),
        );
        (api.g_signal_connect_data)(
            manager,
            signal.as_ptr(),
            script_message_received as Ptr,
            name_id(name) as Ptr,
            std::ptr::null_mut(),
            0,
        );
    }
}

fn unregister_handler(engine: &Engine, manager: Ptr, name: &str) {
    let api = engine.api;
    let c_name = gtk::c_string(name);
    // SAFETY: as `register_handler`; `G_SIGNAL_MATCH_FUNC | G_SIGNAL_MATCH_DATA`.
    unsafe {
        (api.webkit_user_content_manager_unregister_script_message_handler)(
            manager,
            c_name.as_ptr(),
            std::ptr::null(),
        );
        (api.g_signal_handlers_disconnect_matched)(
            manager,
            8 | 16,
            0,
            0,
            std::ptr::null_mut(),
            script_message_received as Ptr,
            name_id(name) as Ptr,
        );
    }
}

#[derive(Default)]
pub struct Preferences {
    pub developer_extras: bool,
    /// Off by default, as on macOS; WebKitGTK's own default is on.
    pub tab_focuses_links: bool,
}

fn configuration_class() -> &'static Class {
    boxed::register::<Configuration>(c"WKWebViewConfiguration", None)
}

fn controller_class() -> &'static Class {
    boxed::register::<Controller>(c"WKUserContentController", None)
}

fn script_class() -> &'static Class {
    boxed::register::<Script>(c"WKUserScript", None)
}

fn preferences_class() -> &'static Class {
    boxed::register::<Preferences>(c"WKPreferences", None)
}

/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn configuration<'a>(object: Id) -> Option<&'a mut Configuration> {
    unsafe { boxed::contents(object, configuration_class()) }
}

/// # Safety
///
/// As [`configuration`].
unsafe fn controller<'a>(object: Id) -> Option<&'a mut Controller> {
    unsafe { boxed::contents(object, controller_class()) }
}

/// # Safety
///
/// As [`configuration`].
unsafe fn script<'a>(object: Id) -> Option<&'a mut Script> {
    unsafe { boxed::contents(object, script_class()) }
}

/// # Safety
///
/// As [`configuration`].
pub unsafe fn preferences<'a>(object: Id) -> Option<&'a mut Preferences> {
    unsafe { boxed::contents(object, preferences_class()) }
}

/// A new, owned instance of a class registered here.
fn new_object(class: &'static Class) -> Id {
    // SAFETY: `+new` is `id (Class, SEL)`.
    unsafe { send!(objc::class::ptr(class) as Id, "new"; Id) }
}

/// Replace a retained object slot.
fn store(slot: &mut usize, value: Id) {
    let old = std::mem::replace(slot, objc::arc::retain(value) as usize);
    objc::arc::release(old as Id);
}

/// The slot's object, made with `+new` if it is empty.
fn get_or_make(slot: &mut usize, make: impl FnOnce() -> Id) -> Id {
    if *slot == 0 {
        *slot = make() as usize;
    }
    *slot as Id
}

/// A new configuration with every default, owned by the caller.
pub fn new_configuration() -> Id {
    new_object(configuration_class())
}

unsafe extern "C" fn init_configuration(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an allocated instance of the class this is installed on.
    unsafe { boxed::fill(receiver, Configuration::default()) };
    receiver
}

/// Apple's copies; a shared configuration is indistinguishable to a guest that
/// finishes configuring before it makes its view, which is the order the API
/// requires.
unsafe extern "C" fn copy_configuration(receiver: Id, _sel: Sel) -> Id {
    objc::arc::retain(receiver)
}

unsafe extern "C" fn copy_configuration_with_zone(receiver: Id, _sel: Sel, _zone: Id) -> Id {
    objc::arc::retain(receiver)
}

macro_rules! object_property {
    ($getter:ident, $setter:ident, $field:ident, $make:expr) => {
        unsafe extern "C" fn $getter(receiver: Id, _sel: Sel) -> Id {
            // SAFETY: a configuration.
            match unsafe { configuration(receiver) } {
                Some(c) => get_or_make(&mut c.$field, $make),
                None => std::ptr::null_mut(),
            }
        }

        unsafe extern "C" fn $setter(receiver: Id, _sel: Sel, value: Id) {
            // SAFETY: a configuration.
            if let Some(c) = unsafe { configuration(receiver) } {
                store(&mut c.$field, value);
            }
        }
    };
}

object_property!(
    user_content_controller,
    set_user_content_controller,
    controller,
    || new_object(controller_class())
);
object_property!(process_pool, set_process_pool, process_pool, || {
    new_object(objc::class::lookup(c"WKProcessPool").expect("registered during bootstrap"))
});
object_property!(
    website_data_store,
    set_website_data_store,
    data_store,
    || { objc::arc::retain(crate::data_store::default_store()) }
);
object_property!(preferences_object, set_preferences, preferences, || {
    new_object(preferences_class())
});

unsafe extern "C" fn set_application_name(receiver: Id, _sel: Sel, value: Id) {
    // SAFETY: a configuration.
    if let Some(c) = unsafe { configuration(receiver) } {
        c.application_name = (!value.is_null()).then(|| engine::text_of(value));
    }
}

unsafe extern "C" fn application_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a configuration.
    match unsafe { configuration(receiver) }.and_then(|c| c.application_name.clone()) {
        Some(name) => engine::string(&name),
        None => std::ptr::null_mut(),
    }
}

/// The configuration's content controller, for a view being made with it.
///
/// # Safety
///
/// `configuration` must be a configuration.
pub unsafe fn controller_of(configuration: Id) -> Id {
    unsafe { user_content_controller(configuration, objc::sel::get("userContentController")) }
}

/// The configuration's preferences.
///
/// # Safety
///
/// `configuration` must be a configuration.
pub unsafe fn preferences_of(configuration: Id) -> Id {
    unsafe { preferences_object(configuration, objc::sel::get("preferences")) }
}

/// The name the guest asked to have appended to the user agent.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
pub unsafe fn application_name_of(object: Id) -> Option<String> {
    unsafe { configuration(object) }.and_then(|c| c.application_name.clone())
}

unsafe extern "C" fn init_controller(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an allocated instance of the class this is installed on.
    unsafe { boxed::fill(receiver, Controller::default()) };
    receiver
}

fn add_to_manager(engine: &Engine, manager: Ptr, script: &Script) {
    let api = engine.api;
    let source = gtk::c_string(&script.source);
    let frames = match script.main_frame_only {
        true => gtk::USER_CONTENT_INJECT_TOP_FRAME,
        false => gtk::USER_CONTENT_INJECT_ALL_FRAMES,
    };
    let time = match script.at_document_start {
        true => gtk::USER_SCRIPT_INJECT_AT_DOCUMENT_START,
        false => gtk::USER_SCRIPT_INJECT_AT_DOCUMENT_END,
    };
    // SAFETY: a live manager and a NUL-terminated source, which is copied.
    unsafe {
        let user_script = (api.webkit_user_script_new)(
            source.as_ptr(),
            frames,
            time,
            std::ptr::null(),
            std::ptr::null(),
        );
        (api.webkit_user_content_manager_add_script)(manager, user_script);
        (api.webkit_user_script_unref)(user_script);
    }
}

/// Make page scripts see `agent` as `navigator.userAgent`, in every frame
/// from before the page's own scripts run.
fn add_user_agent_override(engine: &Engine, manager: Ptr, agent: &str) {
    let source = format!(
        "(() => {{ const agent = {}; \
         for (const [key, value] of [['userAgent', agent], ['appVersion', agent.replace(/^Mozilla\\//, '')]]) \
         Object.defineProperty(Navigator.prototype, key, {{ get() {{ return value; }}, configurable: true }}); }})();",
        crate::js::quote(agent)
    );
    add_to_manager(
        engine,
        manager,
        &Script {
            source,
            at_document_start: true,
            main_frame_only: false,
        },
    );
}

/// Put back every script a controller's manager should have: this port's own,
/// then the guest's.
fn rebuild_scripts(engine: &Engine, controller: &Controller) {
    let manager = controller.manager as Ptr;
    // SAFETY: the controller's own manager.
    unsafe { (engine.api.webkit_user_content_manager_remove_all_scripts)(manager) };
    if let Some(agent) = &controller.page_user_agent {
        add_user_agent_override(engine, manager, agent);
    }
    for script in &controller.scripts {
        add_to_manager(engine, manager, script);
    }
}

/// The `WebKitUserContentManager` a controller's scripts live in, made the
/// first time a view is created with the controller.
///
/// # Safety
///
/// `object` must be a content controller.
pub unsafe fn manager(object: Id, engine: &Engine) -> Ptr {
    let Some(controller) = (unsafe { controller(object) }) else {
        return std::ptr::null_mut();
    };
    if controller.manager == 0 {
        // SAFETY: makes a new object, owned by the controller from here on.
        let manager = unsafe { (engine.api.webkit_user_content_manager_new)() };
        controller.manager = manager as usize;
        rebuild_scripts(engine, controller);
        for (name, _) in &controller.handlers {
            register_handler(engine, manager, name);
        }
        MANAGERS.with(|managers| {
            managers
                .borrow_mut()
                .push((manager as usize, object as usize))
        });
    }
    controller.manager as Ptr
}

/// The manager a configuration's views share, if one has been made.
///
/// # Safety
///
/// `configuration` must be null or an object this runtime allocated.
pub unsafe fn existing_manager(configuration: Id) -> usize {
    let controller = unsafe { self::configuration(configuration) }.map_or(0, |c| c.controller);
    unsafe { self::controller(controller as Id) }.map_or(0, |c| c.manager)
}

/// Show page scripts `agent` rather than what WebKitGTK sends.
///
/// # Safety
///
/// `object` must be a content controller.
pub unsafe fn set_page_user_agent(object: Id, engine: &Engine, agent: Option<String>) {
    let Some(controller) = (unsafe { controller(object) }) else {
        return;
    };
    if controller.page_user_agent == agent {
        return;
    }
    controller.page_user_agent = agent;
    if controller.manager != 0 {
        rebuild_scripts(engine, controller);
    }
}

unsafe extern "C" fn add_user_script(receiver: Id, _sel: Sel, user_script: Id) {
    // SAFETY: a controller, and a user script or nil.
    let (Some(controller), Some(user_script)) = (unsafe { controller(receiver) }, unsafe {
        script(user_script)
    }) else {
        return;
    };
    if engine::tracing() {
        eprintln!(
            "rosemary: webkit: user script (start {}, main frame only {}):\n{}",
            user_script.at_document_start, user_script.main_frame_only, user_script.source
        );
    }
    controller.scripts.push(user_script.clone());
    if controller.manager != 0
        && let Some(engine) = engine::get()
    {
        add_to_manager(engine, controller.manager as Ptr, user_script);
    }
}

unsafe extern "C" fn remove_all_user_scripts(receiver: Id, _sel: Sel) {
    // SAFETY: a controller.
    let Some(controller) = (unsafe { controller(receiver) }) else {
        return;
    };
    controller.scripts.clear();
    if controller.manager != 0
        && let Some(engine) = engine::get()
    {
        // The guest removed its own scripts, not the ones this port adds.
        rebuild_scripts(engine, controller);
    }
}

unsafe extern "C" fn user_scripts(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a controller.
    let scripts = unsafe { controller(receiver) }.map_or(Vec::new(), |c| c.scripts.clone());
    let objects: Vec<corefoundation::base::CFTypeRef> = scripts
        .into_iter()
        .map(|s| boxed::make(script_class(), s) as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for object in objects {
        objc::arc::release(object as Id);
    }
    objc::arc::autorelease(array as Id)
}

/// `-addScriptMessageHandler:name:`. The handler is retained, as WebKit
/// retains it, until it is removed or the controller goes.
unsafe extern "C" fn add_script_message_handler(receiver: Id, _sel: Sel, handler: Id, name: Id) {
    // SAFETY: a controller.
    let Some(controller) = (unsafe { controller(receiver) }) else {
        return;
    };
    let name = engine::text_of(name);
    if handler.is_null() || name.is_empty() {
        return;
    }
    let handler = objc::arc::retain(handler) as usize;
    match controller.handlers.iter_mut().find(|(n, _)| *n == name) {
        Some((_, old)) => objc::arc::release(std::mem::replace(old, handler) as Id),
        None => {
            controller.handlers.push((name.clone(), handler));
            if controller.manager != 0
                && let Some(engine) = engine::get()
            {
                register_handler(engine, controller.manager as Ptr, &name);
            }
        }
    }
}

unsafe extern "C" fn remove_script_message_handler(receiver: Id, _sel: Sel, name: Id) {
    // SAFETY: a controller.
    let Some(controller) = (unsafe { controller(receiver) }) else {
        return;
    };
    let name = engine::text_of(name);
    let Some(at) = controller.handlers.iter().position(|(n, _)| *n == name) else {
        return;
    };
    let (_, handler) = controller.handlers.remove(at);
    objc::arc::release(handler as Id);
    if controller.manager != 0
        && let Some(engine) = engine::get()
    {
        unregister_handler(engine, controller.manager as Ptr, &name);
    }
}

unsafe extern "C" fn remove_all_script_message_handlers(receiver: Id, sel: Sel) {
    // SAFETY: a controller.
    let names: Vec<String> = unsafe { controller(receiver) }
        .map(|c| c.handlers.iter().map(|(n, _)| n.clone()).collect())
        .unwrap_or_default();
    for name in names {
        // SAFETY: the same controller, and a string made for the call.
        unsafe { remove_script_message_handler(receiver, sel, engine::string(&name)) };
    }
}

unsafe extern "C" fn init_user_script(
    receiver: Id,
    _sel: Sel,
    source: Id,
    injection_time: isize,
    main_frame_only: u8,
) -> Id {
    let contents = Script {
        source: engine::text_of(source),
        // `WKUserScriptInjectionTimeAtDocumentStart` is zero.
        at_document_start: injection_time == 0,
        main_frame_only: main_frame_only != 0,
    };
    // SAFETY: an allocated instance of the class this is installed on.
    unsafe { boxed::fill(receiver, contents) };
    receiver
}

unsafe extern "C" fn script_source(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a user script.
    unsafe { script(receiver) }.map_or(std::ptr::null_mut(), |s| engine::string(&s.source))
}

unsafe extern "C" fn script_injection_time(receiver: Id, _sel: Sel) -> isize {
    // SAFETY: a user script.
    unsafe { script(receiver) }.map_or(0, |s| isize::from(!s.at_document_start))
}

unsafe extern "C" fn script_main_frame_only(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a user script.
    unsafe { script(receiver) }.map_or(0, |s| u8::from(s.main_frame_only))
}

unsafe extern "C" fn init_preferences(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an allocated instance of the class this is installed on.
    unsafe { boxed::fill(receiver, Preferences::default()) };
    receiver
}

/// `-setValue:forKey:`, which is how private preferences are set.
///
/// The keys WebKitGTK has an equivalent for are applied; the rest - Studio's
/// `backspaceKeyNavigationEnabled` among them - describe behaviour this port
/// already has or does not offer, and are accepted rather than raising the
/// undefined-key exception KVC would.
unsafe extern "C" fn set_preference_value(receiver: Id, _sel: Sel, value: Id, key: Id) {
    // SAFETY: preferences.
    let Some(preferences) = (unsafe { preferences(receiver) }) else {
        return;
    };
    let enabled = crate::bool_value(value);
    match engine::text_of(key).as_str() {
        "developerExtrasEnabled" => preferences.developer_extras = enabled,
        other => {
            if engine::tracing() {
                eprintln!("rosemary: webkit: preference {other} = {enabled} ignored");
            }
        }
    }
}

unsafe extern "C" fn tab_focuses_links(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: preferences.
    unsafe { preferences(receiver) }.map_or(0, |p| u8::from(p.tab_focuses_links))
}

unsafe extern "C" fn set_tab_focuses_links(receiver: Id, _sel: Sel, value: u8) {
    // SAFETY: preferences.
    if let Some(preferences) = unsafe { preferences(receiver) } {
        preferences.tab_focuses_links = value != 0;
    }
}

unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> u8 {
    1
}

unsafe extern "C" fn ignore_flag(_receiver: Id, _sel: Sel, _value: u8) {}

pub fn install() {
    let configuration = configuration_class();
    crate::methods!(configuration, {
        "init" => init_configuration, c"@16@0:8";
        "copy" => copy_configuration, c"@16@0:8";
        "copyWithZone:" => copy_configuration_with_zone, c"@24@0:8^v16";
        "userContentController" => user_content_controller, c"@16@0:8";
        "setUserContentController:" => set_user_content_controller, c"v24@0:8@16";
        "processPool" => process_pool, c"@16@0:8";
        "setProcessPool:" => set_process_pool, c"v24@0:8@16";
        "websiteDataStore" => website_data_store, c"@16@0:8";
        "setWebsiteDataStore:" => set_website_data_store, c"v24@0:8@16";
        "preferences" => preferences_object, c"@16@0:8";
        "setPreferences:" => set_preferences, c"v24@0:8@16";
        "applicationNameForUserAgent" => application_name, c"@16@0:8";
        "setApplicationNameForUserAgent:" => set_application_name, c"v24@0:8@16";
    });

    let controller = controller_class();
    crate::methods!(controller, {
        "init" => init_controller, c"@16@0:8";
        "addUserScript:" => add_user_script, c"v24@0:8@16";
        "removeAllUserScripts" => remove_all_user_scripts, c"v16@0:8";
        "userScripts" => user_scripts, c"@16@0:8";
        "addScriptMessageHandler:name:" => add_script_message_handler, c"v32@0:8@16@24";
        "removeScriptMessageHandlerForName:" => remove_script_message_handler, c"v24@0:8@16";
        "removeAllScriptMessageHandlers" => remove_all_script_message_handlers, c"v16@0:8";
    });

    let script = script_class();
    crate::methods!(script, {
        "initWithSource:injectionTime:forMainFrameOnly:" => init_user_script, c"@36@0:8@16q24c32";
        "source" => script_source, c"@16@0:8";
        "injectionTime" => script_injection_time, c"q16@0:8";
        "isForMainFrameOnly" => script_main_frame_only, c"c16@0:8";
    });

    let preferences = preferences_class();
    crate::methods!(preferences, {
        "init" => init_preferences, c"@16@0:8";
        "setValue:forKey:" => set_preference_value, c"v32@0:8@16@24";
        "javaScriptEnabled" => yes, c"c16@0:8";
        "setJavaScriptEnabled:" => ignore_flag, c"v20@0:8c16";
        "setJavaScriptCanOpenWindowsAutomatically:" => ignore_flag, c"v20@0:8c16";
        "tabFocusesLinks" => tab_focuses_links, c"c16@0:8";
        "setTabFocusesLinks:" => set_tab_focuses_links, c"v20@0:8c16";
    });

    corefoundation::base::abstract_class(c"WKProcessPool", None);
}
