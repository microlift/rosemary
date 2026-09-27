use std::cell::RefCell;
use std::ffi::{c_char, c_int};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use foundation::boxed;
use foundation::geometry::NSRect;
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::engine::{self, Block, Engine, HostBlock};
use crate::gtk::{self, Ptr};
use crate::{configuration, input, send};

/// How often a visible page is read back.
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(50);

/// What WebKit on macOS sends; with Studio's application name appended, as
/// `applicationNameForUserAgent` appends it. Sites choose their Mac layout, and
/// their Studio-specific paths, by this.
pub(crate) const USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";

struct View {
    /// The key GTK's callbacks carry: an address can be reused by a view made
    /// after this one is gone, and a callback for the old one must find nothing.
    id: usize,
    object: usize,
    /// The `WebKitWebView`, owned; zero when there is no engine.
    gtk: usize,
    /// Retained.
    configuration: usize,
    /// Unretained, as WebKit's delegates are weak.
    navigation_delegate: usize,
    ui_delegate: usize,
    /// The `WKNavigation` of the load in progress, retained.
    navigation: usize,
    allocated: (c_int, c_int),
    draws_background: bool,
    snapshot_in_flight: bool,
    last_snapshot: Option<Instant>,
    /// The pixels last handed to the overlay, so an unchanged page is not
    /// composited again.
    shown: Vec<u8>,
    /// A pointer move not yet sent: moves are coalesced to one per turn.
    pending_move: Option<(f64, f64, u32, u32)>,
    /// The widget's `GtkEventControllerFocus`, unowned; zero if it has none.
    focus_controller: usize,
}

thread_local! {
    static VIEWS: RefCell<Vec<View>> = const { RefCell::new(Vec::new()) };
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

/// Borrow a view by id. The borrow must not outlive `body`, which must not
/// call into the guest.
fn with_view<R>(id: usize, body: impl FnOnce(&mut View) -> R) -> Option<R> {
    VIEWS.with(|views| views.borrow_mut().iter_mut().find(|v| v.id == id).map(body))
}

fn with_object<R>(object: Id, body: impl FnOnce(&mut View) -> R) -> Option<R> {
    VIEWS.with(|views| {
        views
            .borrow_mut()
            .iter_mut()
            .find(|v| v.object == object as usize)
            .map(body)
    })
}

/// The id GTK's callbacks for `object` carry, its `WebKitWebView`, and its
/// configuration, for the WebKit1 `WebView` built on this one.
pub(crate) fn parts_of(object: Id) -> Option<(usize, Ptr, Id)> {
    with_object(object, |v| (v.id, v.gtk as Ptr, v.configuration as Id)).filter(|p| !p.1.is_null())
}

/// A `file:` URL with its path respelled by `respell`; anything else as is.
fn respell_file_url(url: &str, respell: impl FnOnce(&str) -> Option<String>) -> String {
    if !url.starts_with("file://") {
        return url.to_string();
    }
    let at = url.find(['?', '#']).unwrap_or(url.len());
    let (address, suffix) = url.split_at(at);
    let parsed = corefoundation::url::new(address);
    // SAFETY: a URL this call just made.
    let path = unsafe { corefoundation::url::as_url(parsed) }.and_then(|u| u.file_path());
    unsafe { corefoundation::api::CFRelease(parsed) };
    let Some(respelled) = path.and_then(|p| respell(&p)) else {
        return url.to_string();
    };
    let rebuilt = corefoundation::url::new_file_url(&respelled, address.ends_with('/'));
    // SAFETY: as above.
    let text = unsafe { corefoundation::url::as_url(rebuilt) }.map(|u| u.text.clone());
    unsafe { corefoundation::api::CFRelease(rebuilt) };
    text.map_or_else(|| url.to_string(), |text| text + suffix)
}

/// A guest's `file:` URL as the web process, a host process, has to open it.
pub(crate) fn host_url(url: &str) -> String {
    respell_file_url(url, |path| {
        Some(prefix::ns::host_path(path).to_string_lossy().into_owned())
    })
}

/// A `file:` URL WebKitGTK reports, as the guest spells it.
pub(crate) fn guest_url(url: &str) -> String {
    respell_file_url(url, |path| {
        prefix::ns::to_guest(std::path::Path::new(path)).map(|p| p.to_string_lossy().into_owned())
    })
}

/// The object a callback's id belongs to.
pub(crate) fn object_of(id: usize) -> Option<Id> {
    with_view(id, |v| v.object as Id)
}

/// The `WebKitWebView` behind a `WKWebView`, with the engine to call it with.
fn gtk_of(object: Id) -> Option<(&'static Engine, Ptr)> {
    let gtk = with_object(object, |v| v.gtk).filter(|gtk| *gtk != 0)?;
    Some((engine::get()?, gtk as Ptr))
}

/// Retained object references, released with their holder.
struct Owned(Vec<usize>);

impl Owned {
    fn new(objects: &[Id]) -> Owned {
        Owned(
            objects
                .iter()
                .map(|object| objc::arc::retain(*object) as usize)
                .collect(),
        )
    }

    fn get(&self, index: usize) -> Id {
        self.0[index] as Id
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        for object in self.0.drain(..) {
            objc::arc::release(object as Id);
        }
    }
}

struct SecurityOrigin {
    protocol: String,
    host: String,
    port: isize,
}

impl SecurityOrigin {
    fn of(url: &str) -> SecurityOrigin {
        let (protocol, rest) = url.split_once("://").unwrap_or(("", ""));
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let authority = authority.rsplit('@').next().unwrap_or_default();
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(']') || authority.starts_with('[') => {
                (host, port.parse().unwrap_or(0))
            }
            _ => (authority, 0),
        };
        SecurityOrigin {
            protocol: protocol.to_ascii_lowercase(),
            host: host.to_ascii_lowercase(),
            port,
        }
    }
}

struct FrameInfo {
    main: bool,
    /// The request and the security origin.
    objects: Owned,
}

struct NavigationAction {
    navigation_type: isize,
    /// The request, the source frame and, except for a new window, the target.
    objects: Owned,
}

struct NavigationResponse {
    main: bool,
    /// The response.
    objects: Owned,
}

fn class<T: 'static>(name: &'static std::ffi::CStr) -> &'static Class {
    boxed::register::<T>(name, None)
}

fn origin_class() -> &'static Class {
    class::<SecurityOrigin>(c"WKSecurityOrigin")
}

fn frame_class() -> &'static Class {
    class::<FrameInfo>(c"WKFrameInfo")
}

fn action_class() -> &'static Class {
    class::<NavigationAction>(c"WKNavigationAction")
}

fn response_class() -> &'static Class {
    class::<NavigationResponse>(c"WKNavigationResponse")
}

fn autoreleased<T>(class: &'static Class, contents: T) -> Id {
    objc::arc::autorelease(boxed::make(class, contents))
}

/// A frame whose document is at `url`.
fn frame_info(url: &str, main: bool) -> Id {
    let origin = autoreleased(origin_class(), SecurityOrigin::of(url));
    let request = foundation::urlsession::request_for(url);
    autoreleased(
        frame_class(),
        FrameInfo {
            main,
            objects: Owned::new(&[request, origin]),
        },
    )
}

macro_rules! getter {
    ($name:ident, $type:ty, $class:ident, |$contents:ident: &mut $contents_type:ty| $value:expr, $default:expr) => {
        unsafe extern "C" fn $name(receiver: Id, _sel: Sel) -> $type {
            // SAFETY: a receiver of the class this is installed on.
            match unsafe { boxed::contents::<$contents_type>(receiver, $class()) } {
                Some($contents) => $value,
                None => $default,
            }
        }
    };
}

getter!(
    origin_protocol,
    Id,
    origin_class,
    |o: &mut SecurityOrigin| engine::string(&o.protocol),
    std::ptr::null_mut()
);
getter!(
    origin_host,
    Id,
    origin_class,
    |o: &mut SecurityOrigin| engine::string(&o.host),
    std::ptr::null_mut()
);
getter!(
    origin_port,
    isize,
    origin_class,
    |o: &mut SecurityOrigin| o.port,
    0
);
getter!(
    frame_is_main,
    u8,
    frame_class,
    |f: &mut FrameInfo| u8::from(f.main),
    0
);
getter!(
    frame_request,
    Id,
    frame_class,
    |f: &mut FrameInfo| f.objects.get(0),
    std::ptr::null_mut()
);
getter!(
    frame_origin,
    Id,
    frame_class,
    |f: &mut FrameInfo| f.objects.get(1),
    std::ptr::null_mut()
);
getter!(
    action_request,
    Id,
    action_class,
    |a: &mut NavigationAction| a.objects.get(0),
    std::ptr::null_mut()
);
getter!(
    action_source_frame,
    Id,
    action_class,
    |a: &mut NavigationAction| a.objects.get(1),
    std::ptr::null_mut()
);
getter!(
    action_target_frame,
    Id,
    action_class,
    |a: &mut NavigationAction| match a.objects.0.len() > 2 {
        true => a.objects.get(2),
        false => std::ptr::null_mut(),
    },
    std::ptr::null_mut()
);
getter!(
    action_navigation_type,
    isize,
    action_class,
    |a: &mut NavigationAction| a.navigation_type,
    -1
);
getter!(
    response_response,
    Id,
    response_class,
    |r: &mut NavigationResponse| r.objects.get(0),
    std::ptr::null_mut()
);
getter!(
    response_is_main,
    u8,
    response_class,
    |r: &mut NavigationResponse| u8::from(r.main),
    0
);

unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> u8 {
    1
}

unsafe extern "C" fn nil(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

unsafe extern "C" fn zero(_receiver: Id, _sel: Sel) -> isize {
    0
}

/// The `WKNavigationAction` for a WebKitGTK navigation action.
///
/// WebKitGTK does not say which frame started a navigation or which it
/// targets. The source is taken to be the main frame showing the page that is
/// loaded, which is right for the navigations a page's own script starts - the
/// hidden-iframe messages Studio's bridge sends among them - and the target to
/// be the main frame unless the action opens a window.
///
/// # Safety
///
/// `action` must be a live `WebKitNavigationAction`.
unsafe fn navigation_action(engine: &Engine, gtk: Ptr, action: Ptr, new_window: bool) -> Id {
    let api = engine.api;
    // SAFETY: the caller's obligation; the strings belong to the action and
    // the view.
    let (url, kind, page) = unsafe {
        let request = (api.webkit_navigation_action_get_request)(action);
        (
            gtk::text((api.webkit_uri_request_get_uri)(request)),
            (api.webkit_navigation_action_get_navigation_type)(action),
            gtk::text((api.webkit_web_view_get_uri)(gtk)),
        )
    };
    let source_url = match page.is_empty() || page == "about:blank" {
        true => url.as_str(),
        false => page.as_str(),
    };
    let request = foundation::urlsession::request_for(&url);
    let source = frame_info(source_url, true);
    let mut objects = vec![request, source];
    if !new_window {
        objects.push(frame_info(&url, true));
    }
    autoreleased(
        action_class(),
        NavigationAction {
            // WebKitGTK counts `OTHER` as 5; WebKit's `WKNavigationTypeOther`
            // is -1. The rest are the same.
            navigation_type: match kind {
                0..=4 => kind as isize,
                _ => -1,
            },
            objects: Owned::new(&objects),
        },
    )
}

fn new_navigation() -> Id {
    let class = objc::class::lookup(c"WKNavigation").expect("registered during bootstrap");
    objc::object::create_instance(class, 0)
}

struct Decision {
    /// The `WebKitPolicyDecision`, referenced; taken when the guest decides.
    decision: AtomicUsize,
    url: String,
}

/// `void (^)(WKNavigationActionPolicy)` and the response form, which share
/// their values: cancel is 0 and allow is 1.
unsafe extern "C" fn decide(block: *mut HostBlock<Decision>, policy: isize) {
    // SAFETY: the block `decision_handler` made.
    let context = unsafe { &(*block).context };
    let decision = context.decision.swap(0, Ordering::AcqRel);
    if decision == 0 {
        return;
    }
    let Some(engine) = engine::get() else {
        return;
    };
    if engine::tracing() {
        let verdict = if policy == 1 { "allow" } else { "cancel" };
        eprintln!("rosetta3: webkit: {verdict} {}", context.url);
    }
    // SAFETY: the referenced decision, used once and released.
    unsafe {
        match policy {
            1 => (engine.api.webkit_policy_decision_use)(decision as Ptr),
            _ => (engine.api.webkit_policy_decision_ignore)(decision as Ptr),
        }
    }
    engine::unref(engine.api, decision);
}

fn decision_handler(engine: &Engine, decision: Ptr, url: String) -> Id {
    // SAFETY: a live decision; the reference is released in `decide`.
    unsafe { (engine.api.g_object_ref)(decision) };
    engine::host_block(
        decide as *const std::ffi::c_void,
        Decision {
            decision: AtomicUsize::new(decision as usize),
            url,
        },
    )
}

struct Delegates {
    object: Id,
    navigation: Id,
    ui: Id,
    configuration: Id,
}

fn delegates(id: usize) -> Option<Delegates> {
    with_view(id, |v| Delegates {
        object: v.object as Id,
        navigation: v.navigation_delegate as Id,
        ui: v.ui_delegate as Id,
        configuration: v.configuration as Id,
    })
}

unsafe extern "C" fn decide_policy(gtk: Ptr, decision: Ptr, kind: c_int, data: Ptr) -> c_int {
    let (Some(engine), Some(view)) = (engine::get(), delegates(data as usize)) else {
        return 0;
    };
    let api = engine.api;
    match kind {
        gtk::POLICY_DECISION_NAVIGATION_ACTION | gtk::POLICY_DECISION_NEW_WINDOW_ACTION => {
            const SELECTOR: &str = "webView:decidePolicyForNavigationAction:decisionHandler:";
            // SAFETY: a navigation decision, with its action.
            let (action, url) = unsafe {
                let action =
                    (api.webkit_navigation_policy_decision_get_navigation_action)(decision);
                let request = (api.webkit_navigation_action_get_request)(action);
                (action, gtk::text((api.webkit_uri_request_get_uri)(request)))
            };
            if engine::tracing() {
                eprintln!("rosetta3: webkit: navigation to {url}");
            }
            if !engine::responds(view.navigation, SELECTOR) {
                return 0;
            }
            // SAFETY: the action belongs to the decision, live for this call.
            let object = unsafe {
                navigation_action(
                    engine,
                    gtk,
                    action,
                    kind == gtk::POLICY_DECISION_NEW_WINDOW_ACTION,
                )
            };
            let handler = decision_handler(engine, decision, url);
            // SAFETY: the delegate answers the selector, whose prototype this is.
            unsafe {
                send!(view.navigation, "webView:decidePolicyForNavigationAction:decisionHandler:",
                    view.object => Id, object => Id, handler => Id; ())
            };
            1
        }
        gtk::POLICY_DECISION_RESPONSE => {
            const SELECTOR: &str = "webView:decidePolicyForNavigationResponse:decisionHandler:";
            if !engine::responds(view.navigation, SELECTOR) {
                return 0;
            }
            // SAFETY: a response decision, with its response.
            let (url, status, main) = unsafe {
                let response = (api.webkit_response_policy_decision_get_response)(decision);
                (
                    gtk::text((api.webkit_uri_response_get_uri)(response)),
                    (api.webkit_uri_response_get_status_code)(response),
                    (api.webkit_response_policy_decision_is_main_frame_main_resource)(decision)
                        != 0,
                )
            };
            let response = foundation::urlsession::http_response(&url, status as u16, Vec::new());
            let object = autoreleased(
                response_class(),
                NavigationResponse {
                    main,
                    objects: Owned::new(&[response]),
                },
            );
            let handler = decision_handler(engine, decision, url);
            unsafe {
                send!(view.navigation, "webView:decidePolicyForNavigationResponse:decisionHandler:",
                    view.object => Id, object => Id, handler => Id; ())
            };
            1
        }
        _ => 0,
    }
}

/// Tell the navigation delegate about the current navigation, if it listens.
fn notify(view: &Delegates, id: usize, selector: &'static str) {
    if !engine::responds(view.navigation, selector) {
        return;
    }
    let navigation = with_view(id, |v| {
        if v.navigation == 0 {
            v.navigation = new_navigation() as usize;
        }
        v.navigation
    })
    .unwrap_or_default() as Id;
    // SAFETY: the delegate answers the selector, which is `void (id, SEL, id, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
        f(
            view.navigation,
            objc::sel::get(selector),
            view.object,
            navigation,
        );
    }
}

/// The current navigation is over; the next load gets a new one.
fn finish_navigation(id: usize) {
    if let Some(navigation) = with_view(id, |v| std::mem::take(&mut v.navigation)) {
        objc::arc::release(navigation as Id);
    }
}

unsafe extern "C" fn load_changed(gtk: Ptr, event: c_int, data: Ptr) {
    let id = data as usize;
    let Some(view) = delegates(id) else {
        return;
    };
    if engine::tracing()
        && let Some(engine) = engine::get()
    {
        // SAFETY: the view the signal is about.
        let url = unsafe { gtk::text((engine.api.webkit_web_view_get_uri)(gtk)) };
        eprintln!("rosetta3: webkit: load event {event} for {url}");
    }
    match event {
        gtk::LOAD_STARTED => notify(&view, id, "webView:didStartProvisionalNavigation:"),
        gtk::LOAD_COMMITTED => notify(&view, id, "webView:didCommitNavigation:"),
        gtk::LOAD_FINISHED => {
            notify(&view, id, "webView:didFinishNavigation:");
            finish_navigation(id);
        }
        _ => notify(
            &view,
            id,
            "webView:didReceiveServerRedirectForProvisionalNavigation:",
        ),
    }
}

unsafe extern "C" fn load_failed(
    _gtk: Ptr,
    event: c_int,
    uri: *const c_char,
    error: *mut gtk::GError,
    data: Ptr,
) -> c_int {
    let id = data as usize;
    let (Some(engine), Some(view)) = (engine::get(), delegates(id)) else {
        return 0;
    };
    // SAFETY: the signal's arguments, owned by WebKit for its duration.
    let (url, domain, code, message) = unsafe {
        (
            gtk::text(uri),
            gtk::text((engine.api.g_quark_to_string)((*error).domain)),
            (*error).code,
            gtk::text((*error).message),
        )
    };
    eprintln!("rosetta3: webkit: loading {url} failed: {message} ({domain} {code})");
    // WebKit reports a network failure in Foundation's URL error domain, and
    // what it decided itself in its own.
    let (domain, code) = match domain.as_str() {
        "WebKitNetworkError" if code == gtk::NETWORK_ERROR_CANCELLED => ("NSURLErrorDomain", -999),
        "WebKitNetworkError" => ("NSURLErrorDomain", -1),
        "WebKitPolicyError" => ("WebKitErrorDomain", i64::from(code)),
        _ => ("NSURLErrorDomain", -1),
    };
    let error = foundation::error::make(domain, code, &message);
    let provisional = event == gtk::LOAD_STARTED || event == 1;
    let selector = match provisional {
        true => "webView:didFailProvisionalNavigation:withError:",
        false => "webView:didFailNavigation:withError:",
    };
    if engine::responds(view.navigation, selector) {
        let navigation = with_view(id, |v| v.navigation).unwrap_or_default() as Id;
        // SAFETY: the delegate answers the selector, `void (id, SEL, id, id, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id) =
                std::mem::transmute(objc::msg::entry());
            f(
                view.navigation,
                objc::sel::get(selector),
                view.object,
                navigation,
                error,
            );
        }
    }
    finish_navigation(id);
    // Handled: WKWebView shows no error page of its own.
    1
}

unsafe extern "C" fn web_process_terminated(_gtk: Ptr, reason: c_int, data: Ptr) {
    eprintln!("rosetta3: webkit: the web process terminated (reason {reason})");
    let Some(view) = delegates(data as usize) else {
        return;
    };
    const SELECTOR: &str = "webViewWebContentProcessDidTerminate:";
    if engine::responds(view.navigation, SELECTOR) {
        // SAFETY: the delegate answers the selector.
        unsafe {
            send!(view.navigation, "webViewWebContentProcessDidTerminate:", view.object => Id; ())
        };
    }
}

/// A page asked for a new window. The UI delegate is asked for a view, as
/// WebKit asks it; whatever it answers, no view is attached to the page, since
/// a GTK view cannot be related to one the guest made.
unsafe extern "C" fn create(gtk: Ptr, action: Ptr, data: Ptr) -> Ptr {
    let (Some(engine), Some(view)) = (engine::get(), delegates(data as usize)) else {
        return std::ptr::null_mut();
    };
    const SELECTOR: &str =
        "webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:";
    if engine::responds(view.ui, SELECTOR) {
        // SAFETY: the action is live for the signal's duration.
        let action = unsafe { navigation_action(engine, gtk, action, true) };
        let features = objc::arc::autorelease(objc::object::create_instance(
            objc::class::lookup(c"WKWindowFeatures").expect("registered during bootstrap"),
            0,
        ));
        // SAFETY: the delegate answers the selector.
        let created = unsafe {
            send!(view.ui, "webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:",
                view.object => Id, view.configuration => Id, action => Id, features => Id; Id)
        };
        if !created.is_null() {
            engine::warn_once("a web view made for a new window is not connected to its opener");
        }
    }
    std::ptr::null_mut()
}

pub(crate) fn connect(
    engine: &Engine,
    gtk: Ptr,
    signal: &std::ffi::CStr,
    handler: *const (),
    id: usize,
) {
    // SAFETY: a live widget, a signal it has and a handler of that signal's
    // prototype; the data is a plain integer and needs no destructor.
    unsafe {
        (engine.api.g_signal_connect_data)(
            gtk,
            signal.as_ptr(),
            handler as Ptr,
            id as Ptr,
            std::ptr::null_mut(),
            0,
        )
    };
}

/// Make the `WebKitWebView` for a new `WKWebView`.
///
/// # Safety
///
/// `configuration` must be a configuration.
/// The user agent a view made with `configuration` sends.
///
/// # Safety
///
/// `configuration` must be a configuration, or nil.
unsafe fn user_agent(configuration: Id) -> String {
    let mut agent = USER_AGENT.to_string();
    if let Some(name) = unsafe { configuration::application_name_of(configuration) } {
        agent.push(' ');
        agent.push_str(&name);
    }
    agent
}

unsafe fn make_gtk_view(engine: &Engine, configuration: Id, id: usize) -> Ptr {
    let api = engine.api;
    let controller = unsafe { configuration::controller_of(configuration) };
    // SAFETY: a content controller.
    let manager = unsafe { configuration::manager(controller, engine) };
    // SAFETY: two construct-only properties of `WebKitWebView` with objects of
    // their types, then the terminating NULL.
    let gtk = unsafe {
        (api.g_object_new)(
            (api.webkit_web_view_get_type)(),
            c"network-session".as_ptr(),
            engine.session as Ptr,
            c"user-content-manager".as_ptr(),
            manager,
            std::ptr::null::<c_char>(),
        )
    };
    if gtk.is_null() {
        return gtk;
    }
    // SAFETY: the new widget is floating until something sinks it; this owns it.
    unsafe { (api.g_object_ref_sink)(gtk) };
    let agent = unsafe { user_agent(configuration) };
    if engine::tracing() {
        eprintln!("rosetta3: webkit: user agent {agent}");
    }
    let agent = gtk::c_string(&agent);
    // SAFETY: a configuration's preferences.
    let developer =
        unsafe { configuration::preferences(configuration::preferences_of(configuration)) }
            .is_some_and(|p| p.developer_extras);
    // SAFETY: the view's own settings, and a string they copy.
    unsafe {
        let settings = (api.webkit_web_view_get_settings)(gtk);
        (api.webkit_settings_set_user_agent)(settings, agent.as_ptr());
        (api.webkit_settings_set_enable_developer_extras)(settings, c_int::from(developer));
        // Accelerated compositing schedules rendering updates from the frame
        // clock of the window the widget is in, and this widget is in none:
        // `requestAnimationFrame` would never fire, and a page that animates
        // something into view - a modal dialog - never shows it.
        // Without it WebKit schedules updates itself.
        (api.webkit_settings_set_enable_write_console_messages_to_stdout)(
            settings,
            c_int::from(engine::tracing()),
        );
        (api.webkit_settings_set_hardware_acceleration_policy)(
            settings,
            gtk::HARDWARE_ACCELERATION_POLICY_NEVER,
        );
    }
    connect(
        engine,
        gtk,
        c"decide-policy",
        decide_policy as *const (),
        id,
    );
    connect(engine, gtk, c"load-changed", load_changed as *const (), id);
    connect(engine, gtk, c"load-failed", load_failed as *const (), id);
    connect(
        engine,
        gtk,
        c"web-process-terminated",
        web_process_terminated as *const (),
        id,
    );
    connect(engine, gtk, c"create", create as *const (), id);
    gtk
}

/// The controller GTK tells a widget about keyboard focus through.
fn focus_controller(engine: &Engine, gtk: Ptr) -> Ptr {
    let api = engine.api;
    // SAFETY: the view's own widget; the list model and each item are
    // references this function releases, while the widget keeps the
    // controllers alive.
    unsafe {
        let focus = (api.gtk_event_controller_focus_get_type)();
        let controllers = (api.gtk_widget_observe_controllers)(gtk);
        let mut found: Ptr = std::ptr::null_mut();
        for index in 0..(api.g_list_model_get_n_items)(controllers) {
            let controller = (api.g_list_model_get_item)(controllers, index);
            if found.is_null() && (api.g_type_check_instance_is_a)(controller, focus) != 0 {
                found = controller;
            }
            (api.g_object_unref)(controller);
        }
        (api.g_object_unref)(controllers);
        found
    }
}

/// Tell the page it has, or has lost, the keyboard.
///
/// A widget outside any window never receives GTK focus, and a page without
/// focus neither matches `:focus` nor draws a caret - a click into a field
/// moves focus there with nothing on screen to show it. So the focus
/// controller's own signals are emitted when the AppKit view becomes, or stops
/// being, first responder, which is the same event WebKit follows on macOS.
fn set_focused(object: Id, focused: bool) {
    let Some((engine, _)) = gtk_of(object) else {
        return;
    };
    let controller = with_object(object, |v| v.focus_controller).unwrap_or_default();
    if controller == 0 {
        return;
    }
    let signal = match focused {
        true => c"enter",
        false => c"leave",
    };
    // SAFETY: the view's focus controller, whose `enter` and `leave` signals
    // take no arguments.
    unsafe { (engine.api.g_signal_emit_by_name)(controller as Ptr, signal.as_ptr()) };
}

unsafe extern "C" fn become_first_responder(receiver: Id, _sel: Sel) -> u8 {
    set_focused(receiver, true);
    1
}

unsafe extern "C" fn resign_first_responder(receiver: Id, _sel: Sel) -> u8 {
    set_focused(receiver, false);
    1
}

/// `-initWithFrame:configuration:`.
///
/// NSView's own initializer is called directly rather than through dispatch, as
/// a guest subclass may override `-initWithFrame:` to call this one.
unsafe extern "C" fn init_with_frame_configuration(
    receiver: Id,
    _sel: Sel,
    frame: NSRect,
    configuration: Id,
) -> Id {
    let init = objc::sel::get("initWithFrame:");
    let method = objc::class::lookup(c"NSView")
        .and_then(|view| view.method(init))
        .expect("AppKit implements -[NSView initWithFrame:]");
    // SAFETY: NSView's `-initWithFrame:` has this signature.
    let receiver = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
            std::mem::transmute(method.implementation());
        f(receiver, init, frame)
    };
    if receiver.is_null() {
        return receiver;
    }
    forget(receiver);
    let configuration = match configuration.is_null() {
        true => configuration::new_configuration(),
        false => objc::arc::retain(configuration),
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let gtk = match engine::get() {
        // SAFETY: a configuration, made or retained above.
        Some(engine) => unsafe { make_gtk_view(engine, configuration, id) },
        None => std::ptr::null_mut(),
    };
    VIEWS.with(|views| {
        views.borrow_mut().push(View {
            id,
            object: receiver as usize,
            gtk: gtk as usize,
            configuration: configuration as usize,
            navigation_delegate: 0,
            ui_delegate: 0,
            navigation: 0,
            allocated: (0, 0),
            draws_background: true,
            snapshot_in_flight: false,
            last_snapshot: None,
            shown: Vec::new(),
            pending_move: None,
            focus_controller: match engine::get() {
                Some(engine) if !gtk.is_null() => focus_controller(engine, gtk) as usize,
                _ => 0,
            },
        })
    });
    receiver
}

unsafe extern "C" fn init_with_frame(receiver: Id, sel: Sel, frame: NSRect) -> Id {
    // SAFETY: the designated initializer with the default configuration.
    unsafe { init_with_frame_configuration(receiver, sel, frame, std::ptr::null_mut()) }
}

/// Drop everything held for `object`.
fn forget(object: Id) {
    let Some(view) = VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        let index = views.iter().position(|v| v.object == object as usize)?;
        Some(views.remove(index))
    }) else {
        return;
    };
    appkit::backing::remove_overlay(object);
    if view.gtk != 0
        && let Some(engine) = engine::get()
    {
        let api = engine.api;
        // SAFETY: the view's own widget; handlers keyed by its id are removed
        // before the last reference goes, so none can run for a dead view.
        unsafe {
            // `G_SIGNAL_MATCH_DATA`.
            (api.g_signal_handlers_disconnect_matched)(
                view.gtk as Ptr,
                16,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                view.id as Ptr,
            );
            (api.webkit_web_view_stop_loading)(view.gtk as Ptr);
        }
        engine::unref(api, view.gtk);
    }
    objc::arc::release(view.navigation as Id);
    objc::arc::release(view.configuration as Id);
}

unsafe extern "C" fn dealloc(receiver: Id, sel: Sel) {
    forget(receiver);
    let superclass = objc::class::lookup(c"NSView").and_then(|view| view.method(sel));
    match superclass {
        // SAFETY: `-dealloc` is `void (id, SEL)`.
        Some(method) => unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(method.implementation());
            f(receiver, sel)
        },
        None => objc::arc::finalize(receiver),
    }
}

/// Size each visible view's page to its view, send coalesced input, and read
/// back pages that are due.
pub fn tick(engine: &Engine) {
    let api = engine.api;
    let views: Vec<(usize, usize, usize)> = VIEWS.with(|views| {
        views
            .borrow()
            .iter()
            .map(|v| (v.id, v.object, v.gtk))
            .collect()
    });
    for (id, object, gtk) in views {
        let (object, gtk) = (object as Id, gtk as Ptr);
        let hidden = appkit::view::is_hidden_or_has_hidden_ancestor(object);
        // SAFETY: a live view, registered while it is.
        let window = unsafe { appkit::view::window_of(object) };
        let frame = unsafe { appkit::view::presentation_frame(object) };
        if engine::tracing() {
            trace_state(id, gtk, hidden, window, frame.as_ref());
        }
        if gtk.is_null() || hidden || !appkit::window::is_shown(window) {
            continue;
        }
        let Some(frame) = frame else {
            continue;
        };
        let density = f64::from(sdl3::video::window_pixel_density(frame.window)).max(1.0);
        let size = (
            (f64::from(frame.width) / density).round().max(1.0) as c_int,
            (f64::from(frame.height) / density).round().max(1.0) as c_int,
        );
        let resized = with_view(id, |v| std::mem::replace(&mut v.allocated, size) != size);
        if resized == Some(true) {
            let mut ignored = [0; 4];
            let allocation = gtk::Allocation {
                x: 0,
                y: 0,
                width: size.0,
                height: size.1,
            };
            // SAFETY: the view's own widget. GTK wants a widget measured before
            // it is allocated, and says so on every allocation otherwise.
            unsafe {
                for orientation in [gtk::ORIENTATION_HORIZONTAL, gtk::ORIENTATION_VERTICAL] {
                    let [a, b, c, d] = &mut ignored;
                    (api.gtk_widget_measure)(gtk, orientation, -1, a, b, c, d);
                }
                (api.gtk_widget_size_allocate)(gtk, &allocation, -1);
                // A page whose widget is not mapped is hidden to WebKit, which
                // stops its animation frames and throttles its timers. Mapping
                // a widget with no toplevel makes GTK warn once and GDK report
                // the missing frame clock, but WebKit then treats the page as
                // visible, which is what the view on screen is.
                if (api.gtk_widget_get_mapped)(gtk) == 0 {
                    (api.gtk_widget_map)(gtk);
                }
            }
        }
        if let Some(Some((x, y, buttons, modifiers))) = with_view(id, |v| v.pending_move.take()) {
            run_input(
                engine,
                gtk,
                &format!("r.move({x}, {y}, {buttons}, {modifiers})"),
            );
        }
        let due = with_view(id, |v| {
            let due = !v.snapshot_in_flight
                && v.last_snapshot
                    .is_none_or(|at| at.elapsed() >= SNAPSHOT_INTERVAL);
            if due {
                v.snapshot_in_flight = true;
                v.last_snapshot = Some(Instant::now());
            }
            (due, v.draws_background)
        });
        if engine::tracing() {
            trace_page(engine, id, gtk);
        }
        if let Some((true, draws_background)) = due {
            let options = match draws_background {
                true => 0,
                false => gtk::SNAPSHOT_OPTIONS_TRANSPARENT_BACKGROUND,
            };
            // SAFETY: the view's own widget; the id is a plain integer.
            unsafe {
                (api.webkit_web_view_get_snapshot)(
                    gtk,
                    gtk::SNAPSHOT_REGION_VISIBLE,
                    options,
                    std::ptr::null_mut(),
                    Some(snapshot_ready),
                    id as Ptr,
                )
            };
        }
    }
}

/// `ROSETTA3_TRACE_WEBKIT`: every few seconds, where the page is, whether
/// WebKit thinks it visible and what has focus - what separates a page that is
/// not drawing from one that is not being given input.
fn trace_page(engine: &Engine, id: usize, gtk: Ptr) {
    thread_local! {
        static LAST: RefCell<Option<Instant>> = const { RefCell::new(None) };
    }
    let due = LAST.with(|last| {
        let mut last = last.borrow_mut();
        let due = last.is_none_or(|at| at.elapsed() >= Duration::from_secs(4));
        if due {
            *last = Some(Instant::now());
        }
        due
    });
    if !due {
        return;
    }
    unsafe extern "C" fn ready(source: Ptr, result: Ptr, data: Ptr) {
        let Some(engine) = engine::get() else {
            return;
        };
        let api = engine.api;
        let mut error = std::ptr::null_mut();
        // SAFETY: the view and result this callback was invoked with.
        let value =
            unsafe { (api.webkit_web_view_evaluate_javascript_finish)(source, result, &mut error) };
        // SAFETY: an owned error or value.
        let text = match unsafe { gtk::take_error(api, error) } {
            Some((_, message)) => format!("error: {message}"),
            None => unsafe {
                let text = (api.jsc_value_to_string)(value);
                let owned = gtk::text(text);
                (api.g_free)(text.cast());
                (api.g_object_unref)(value);
                owned
            },
        };
        eprintln!("rosetta3: webkit: view {}: page {text}", data as usize);
    }
    let script = c"JSON.stringify({ url: location.href.slice(0, 80), state: document.readyState, visibility: document.visibilityState, focus: document.hasFocus(), active: document.activeElement ? document.activeElement.outerHTML.slice(0, 80) : null })";
    // SAFETY: the view's widget and a script it copies.
    unsafe {
        (engine.api.webkit_web_view_evaluate_javascript)(
            gtk,
            script.as_ptr(),
            -1,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            Some(ready),
            id as Ptr,
        )
    };
}

/// `ROSETTA3_TRACE_WEBKIT`: say why a view is or is not being drawn, when
/// that changes.
fn trace_state(id: usize, gtk: Ptr, hidden: bool, window: Id, frame: Option<&appkit::view::Frame>) {
    thread_local! {
        static LAST: RefCell<Vec<(usize, String)>> = const { RefCell::new(Vec::new()) };
    }
    let state = format!(
        "gtk {gtk:p}, hidden {hidden}, window {window:p} shown {}, frame {:?}",
        appkit::window::is_shown(window),
        frame.map(|f| (f.x, f.y, f.width, f.height, f.window_width, f.window_height)),
    );
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        match last.iter_mut().find(|(key, _)| *key == id) {
            Some((_, seen)) if *seen == state => return,
            Some((_, seen)) => *seen = state.clone(),
            None => last.push((id, state.clone())),
        }
        eprintln!("rosetta3: webkit: view {id}: {state}");
    });
}

unsafe extern "C" fn snapshot_ready(source: Ptr, result: Ptr, data: Ptr) {
    let id = data as usize;
    let Some(engine) = engine::get() else {
        return;
    };
    let api = engine.api;
    let mut error = std::ptr::null_mut();
    // SAFETY: the view and result this callback was invoked with.
    let texture = unsafe { (api.webkit_web_view_get_snapshot_finish)(source, result, &mut error) };
    // SAFETY: an error this call owns, or null.
    let failure = unsafe { gtk::take_error(api, error) };
    let object = with_view(id, |v| {
        v.snapshot_in_flight = false;
        v.object
    });
    if texture.is_null() {
        if let Some((_, message)) = failure
            && engine::tracing()
        {
            eprintln!("rosetta3: webkit: snapshot failed: {message}");
        }
        return;
    }
    // SAFETY: a texture this call owns; `gdk_texture_download` writes
    // premultiplied native-endian ARGB32, which on a little-endian host is
    // the B, G, R, A the store holds.
    let (width, height, pixels) = unsafe {
        let width = (api.gdk_texture_get_width)(texture).max(0) as usize;
        let height = (api.gdk_texture_get_height)(texture).max(0) as usize;
        let mut pixels = vec![0u8; width * height * 4];
        if !pixels.is_empty() {
            (api.gdk_texture_download)(texture, pixels.as_mut_ptr(), width * 4);
        }
        (api.g_object_unref)(texture);
        (width, height, pixels)
    };
    let Some(object) = object else {
        return;
    };
    let changed = with_view(id, |v| {
        let changed = v.shown != pixels;
        if changed {
            v.shown.clone_from(&pixels);
        }
        changed
    });
    if changed == Some(true) {
        if engine::tracing() {
            eprintln!("rosetta3: webkit: view {id}: new {width}x{height} frame");
        }
        appkit::backing::set_overlay(
            object as Id,
            appkit::backing::Overlay {
                width: width as u32,
                height: height as u32,
                pixels,
            },
        );
    }
}

unsafe extern "C" fn configuration_getter(receiver: Id, _sel: Sel) -> Id {
    with_object(receiver, |v| v.configuration).unwrap_or_default() as Id
}

/// The private `-_userAgent`, which apps reach as `valueForKey:@"userAgent"`.
unsafe extern "C" fn user_agent_getter(receiver: Id, _sel: Sel) -> Id {
    let configuration = with_object(receiver, |v| v.configuration).unwrap_or_default() as Id;
    // SAFETY: the view's own configuration, or nil.
    engine::string(&unsafe { user_agent(configuration) })
}

unsafe extern "C" fn set_navigation_delegate(receiver: Id, _sel: Sel, delegate: Id) {
    with_object(receiver, |v| v.navigation_delegate = delegate as usize);
}

unsafe extern "C" fn navigation_delegate(receiver: Id, _sel: Sel) -> Id {
    with_object(receiver, |v| v.navigation_delegate).unwrap_or_default() as Id
}

unsafe extern "C" fn set_ui_delegate(receiver: Id, _sel: Sel, delegate: Id) {
    with_object(receiver, |v| v.ui_delegate = delegate as usize);
}

unsafe extern "C" fn ui_delegate(receiver: Id, _sel: Sel) -> Id {
    with_object(receiver, |v| v.ui_delegate).unwrap_or_default() as Id
}

unsafe extern "C" fn load_request(receiver: Id, _sel: Sel, request: Id) -> Id {
    // SAFETY: a request, or nil.
    let Some(foundation::urlsession::RequestParts {
        url,
        method,
        headers,
    }) = (unsafe { foundation::urlsession::request_parts(request) })
    else {
        return std::ptr::null_mut();
    };
    eprintln!("rosetta3: webkit: loading {url}");
    let Some((engine, gtk)) = gtk_of(receiver) else {
        return std::ptr::null_mut();
    };
    if method != "GET" || !headers.is_empty() {
        engine::warn_once("a request's method and headers are not sent; it is loaded as a GET");
    }
    let navigation = new_navigation();
    let old = with_object(receiver, |v| {
        std::mem::replace(&mut v.navigation, navigation as usize)
    });
    objc::arc::release(old.unwrap_or_default() as Id);
    let url = gtk::c_string(&host_url(&url));
    // SAFETY: the view's widget and a string it copies.
    unsafe { (engine.api.webkit_web_view_load_uri)(gtk, url.as_ptr()) };
    navigation
}

unsafe extern "C" fn stop_loading(receiver: Id, _sel: Sel) {
    if let Some((engine, gtk)) = gtk_of(receiver) {
        // SAFETY: the view's widget.
        unsafe { (engine.api.webkit_web_view_stop_loading)(gtk) };
    }
}

unsafe extern "C" fn url(receiver: Id, _sel: Sel) -> Id {
    let Some((engine, gtk)) = gtk_of(receiver) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the view's widget; the string is the view's.
    let text = unsafe { gtk::text((engine.api.webkit_web_view_get_uri)(gtk)) };
    if text.is_empty() {
        return std::ptr::null_mut();
    }
    objc::arc::autorelease(corefoundation::url::new(&guest_url(&text)) as Id)
}

/// A JavaScript value as the Objective-C object WebKit bridges it to.
///
/// # Safety
///
/// `value` must be a live `JSCValue`.
pub(crate) unsafe fn bridge(api: &gtk::Api, value: Ptr) -> Id {
    unsafe {
        if (api.jsc_value_is_undefined)(value) != 0 || (api.jsc_value_is_null)(value) != 0 {
            return std::ptr::null_mut();
        }
        if (api.jsc_value_is_boolean)(value) != 0 {
            let class = objc::class::lookup(c"NSNumber").expect("registered by Foundation");
            let flag = (api.jsc_value_to_boolean)(value) != 0;
            return send!(objc::class::ptr(class) as Id, "numberWithBool:", flag => bool; Id);
        }
        if (api.jsc_value_is_number)(value) != 0 {
            let class = objc::class::lookup(c"NSNumber").expect("registered by Foundation");
            let number = (api.jsc_value_to_double)(value);
            return send!(objc::class::ptr(class) as Id, "numberWithDouble:", number => f64; Id);
        }
        let is_string = (api.jsc_value_is_string)(value) != 0;
        let text = match is_string {
            true => (api.jsc_value_to_string)(value),
            false => (api.jsc_value_to_json)(value, 0),
        };
        let owned = gtk::text(text);
        (api.g_free)(text.cast());
        if is_string {
            return engine::string(&owned);
        }
        let data =
            objc::arc::autorelease(corefoundation::collections::new_data(owned.into_bytes()) as Id);
        let class = objc::class::lookup(c"NSJSONSerialization").expect("registered by Foundation");
        send!(objc::class::ptr(class) as Id, "JSONObjectWithData:options:error:",
            data => Id, 0usize => usize, std::ptr::null_mut::<Id>() => *mut Id; Id)
    }
}

unsafe extern "C" fn javascript_ready(source: Ptr, result: Ptr, data: Ptr) {
    // SAFETY: the box `evaluate_javascript` leaked for this call.
    let completion = unsafe { Box::from_raw(data as *mut Block) };
    let Some(engine) = engine::get() else {
        return;
    };
    let api = engine.api;
    let mut error = std::ptr::null_mut();
    // SAFETY: the view and result this callback was invoked with.
    let value =
        unsafe { (api.webkit_web_view_evaluate_javascript_finish)(source, result, &mut error) };
    // SAFETY: an error this call owns, or null.
    match unsafe { gtk::take_error(api, error) } {
        Some((_, message)) => {
            // `WKErrorJavaScriptExceptionOccurred`.
            let error = foundation::error::make("WKErrorDomain", 4, &message);
            completion.call2(std::ptr::null_mut(), error);
        }
        None => {
            // SAFETY: a value this call owns.
            let object = unsafe { bridge(api, value) };
            completion.call2(object, std::ptr::null_mut());
        }
    }
    if !value.is_null() {
        unsafe { (api.g_object_unref)(value) };
    }
}

unsafe extern "C" fn evaluate_javascript(receiver: Id, _sel: Sel, script: Id, completion: Id) {
    let completion = Block::copy(completion);
    let Some((engine, gtk)) = gtk_of(receiver) else {
        if let Some(completion) = completion {
            engine::defer(move || {
                // `WKErrorUnknown`.
                let error = foundation::error::make("WKErrorDomain", 1, "no web content");
                completion.call2(std::ptr::null_mut(), error);
            });
        }
        return;
    };
    let source = gtk::c_string(&engine::text_of(script));
    let (callback, data): (Option<gtk::AsyncReady>, Ptr) = match completion {
        Some(completion) => (
            Some(javascript_ready),
            Box::into_raw(Box::new(completion)) as Ptr,
        ),
        None => (None, std::ptr::null_mut()),
    };
    // SAFETY: the view's widget and a script it copies; the callback takes the
    // box back.
    unsafe {
        (engine.api.webkit_web_view_evaluate_javascript)(
            gtk,
            source.as_ptr(),
            -1,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            callback,
            data,
        )
    };
}

/// `-setValue:forKey:`, for the private `drawsBackground` key; other keys go
/// to key-value coding as usual.
unsafe extern "C" fn set_value_for_key(receiver: Id, sel: Sel, value: Id, key: Id) {
    if engine::text_of(key) == "drawsBackground" {
        let draws = crate::bool_value(value);
        with_object(receiver, |v| {
            v.draws_background = draws;
            v.shown.clear();
        });
        if let Some((engine, gtk)) = gtk_of(receiver) {
            let color = gtk::Rgba {
                red: 1.0,
                green: 1.0,
                blue: 1.0,
                alpha: if draws { 1.0 } else { 0.0 },
            };
            // SAFETY: the view's widget and a color it copies.
            unsafe { (engine.api.webkit_web_view_set_background_color)(gtk, &color) };
        }
        return;
    }
    let inherited = objc::class::lookup(c"NSView").and_then(|view| view.method(sel));
    if let Some(method) = inherited {
        // SAFETY: `-setValue:forKey:` is `void (id, SEL, id, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id) =
                std::mem::transmute(method.implementation());
            f(receiver, sel, value, key);
        }
    }
}

unsafe extern "C" fn input_ready(source: Ptr, result: Ptr, _data: Ptr) {
    let Some(engine) = engine::get() else {
        return;
    };
    let api = engine.api;
    let mut error = std::ptr::null_mut();
    // SAFETY: the view and result this callback was invoked with.
    let value =
        unsafe { (api.webkit_web_view_evaluate_javascript_finish)(source, result, &mut error) };
    // SAFETY: an owned error or value.
    unsafe {
        if let Some((_, message)) = gtk::take_error(api, error) {
            eprintln!("rosetta3: webkit: input script failed: {message}");
        }
        if !value.is_null() {
            (api.g_object_unref)(value);
        }
    }
}

fn run_input(engine: &Engine, gtk: Ptr, body: &str) {
    if engine::tracing() && !body.starts_with("r.move") {
        eprintln!("rosetta3: webkit: input {body}");
    }
    let callback: Option<gtk::AsyncReady> = engine::tracing().then_some(input_ready);
    let script = gtk::c_string(&input::call(body));
    // SAFETY: the view's widget, a script and a world name it copies; no
    // result is wanted.
    unsafe {
        (engine.api.webkit_web_view_evaluate_javascript)(
            gtk,
            script.as_ptr(),
            -1,
            input::WORLD.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            callback,
            std::ptr::null_mut(),
        )
    };
}

/// Where an event happened in the page, in CSS pixels from its top left.
fn page_point(object: Id, event: Id) -> Option<(f64, f64)> {
    let location = appkit::nsevent::location_of(event);
    // SAFETY: a live view.
    let frame = unsafe { appkit::view::presentation_frame(object) }?;
    let density = f64::from(sdl3::video::window_pixel_density(frame.window)).max(1.0);
    let x = location.x - f64::from(frame.x) / density;
    let y = f64::from(frame.window_height) / density - location.y - f64::from(frame.y) / density;
    Some((x, y))
}

fn modifiers(event: Id) -> u32 {
    // SAFETY: an event; `-modifierFlags` is `NSUInteger (id, SEL)`.
    input::modifier_bits(unsafe { send!(event, "modifierFlags"; u64) })
}

fn click_count(event: Id) -> i64 {
    // SAFETY: an event; `-clickCount` is `NSInteger (id, SEL)`.
    unsafe { send!(event, "clickCount"; i64) }.max(1)
}

fn mouse(receiver: Id, event: Id, action: &str, button: u8) {
    let Some((engine, gtk)) = gtk_of(receiver) else {
        return;
    };
    let Some((x, y)) = page_point(receiver, event) else {
        return;
    };
    if action == "down" {
        // A click is what gives a web view the keyboard, as it does in AppKit.
        // SAFETY: a live view; both methods are AppKit's own prototypes.
        unsafe {
            let window = send!(receiver, "window"; Id);
            if !window.is_null() {
                send!(window, "makeFirstResponder:", receiver => Id; u8);
            }
        }
    }
    // A move still pending would otherwise arrive after the button it preceded.
    with_object(receiver, |v| v.pending_move = None);
    let body = format!(
        "r.{action}({x}, {y}, {button}, {}, {})",
        click_count(event),
        modifiers(event)
    );
    run_input(engine, gtk, &body);
}

unsafe extern "C" fn mouse_down(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, "down", 0);
}

unsafe extern "C" fn mouse_up(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, "up", 0);
}

unsafe extern "C" fn right_mouse_down(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, "down", 2);
}

unsafe extern "C" fn right_mouse_up(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, "up", 2);
}

unsafe extern "C" fn other_mouse_down(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, "down", 1);
}

unsafe extern "C" fn other_mouse_up(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, "up", 1);
}

fn pointer_moved(receiver: Id, event: Id, buttons: u32) {
    let Some((x, y)) = page_point(receiver, event) else {
        return;
    };
    let modifiers = modifiers(event);
    with_object(receiver, |v| {
        v.pending_move = Some((x, y, buttons, modifiers))
    });
}

unsafe extern "C" fn mouse_moved(receiver: Id, _sel: Sel, event: Id) {
    pointer_moved(receiver, event, 0);
}

unsafe extern "C" fn mouse_dragged(receiver: Id, _sel: Sel, event: Id) {
    pointer_moved(receiver, event, 1);
}

unsafe extern "C" fn right_mouse_dragged(receiver: Id, _sel: Sel, event: Id) {
    pointer_moved(receiver, event, 2);
}

unsafe extern "C" fn scroll_wheel(receiver: Id, _sel: Sel, event: Id) {
    let Some((engine, gtk)) = gtk_of(receiver) else {
        return;
    };
    let Some((x, y)) = page_point(receiver, event) else {
        return;
    };
    // SAFETY: an event; both deltas are `CGFloat (id, SEL)`.
    let (dx, dy) = unsafe { (send!(event, "deltaX"; f64), send!(event, "deltaY"; f64)) };
    // AppKit's deltas are lines, positive toward the top and left of the
    // content; the DOM's are pixels, positive toward the bottom and right.
    const LINE: f64 = 40.0;
    let body = format!(
        "r.wheel({x}, {y}, {}, {}, {})",
        -dx * LINE,
        -dy * LINE,
        modifiers(event)
    );
    run_input(engine, gtk, &body);
}

/// The editing command a Command-key shortcut performs.
fn shortcut(characters: &str, shift: bool) -> Option<&'static std::ffi::CStr> {
    Some(match characters.to_ascii_lowercase().as_str() {
        "c" => c"Copy",
        "x" => c"Cut",
        "v" => c"Paste",
        "a" => c"SelectAll",
        "z" if shift => c"Redo",
        "z" => c"Undo",
        _ => return None,
    })
}

unsafe extern "C" fn key_down(receiver: Id, _sel: Sel, event: Id) {
    let Some((engine, gtk)) = gtk_of(receiver) else {
        return;
    };
    // SAFETY: an event; each accessor is AppKit's own prototype.
    let (characters, flags, key_code, repeat) = unsafe {
        (
            engine::text_of(send!(event, "charactersIgnoringModifiers"; Id)),
            send!(event, "modifierFlags"; u64),
            send!(event, "keyCode"; u16),
            send!(event, "isARepeat"; u8) != 0,
        )
    };
    let text = engine::text_of(unsafe { send!(event, "characters"; Id) });
    let modifiers = input::modifier_bits(flags);
    if modifiers & 0b1000 != 0 {
        if let Some(command) = shortcut(&characters, modifiers & 1 != 0) {
            // SAFETY: the view's widget and a command name.
            unsafe { (engine.api.webkit_web_view_execute_editing_command)(gtk, command.as_ptr()) };
        }
        return;
    }
    let key = input::key(key_code, &text);
    let action = match (key.special, key.key.as_str()) {
        // A space types in a text field and presses a button elsewhere; each
        // half does nothing where the other applies.
        (true, " ") => format!("r.text(\" \"); r.edit(\" \", {modifiers})"),
        (true, name) => format!("r.edit({}, {modifiers})", input::quote(name)),
        // Control-letter produces no text on a Mac.
        (false, _) if modifiers & 0b10 != 0 => String::new(),
        (false, _) if key.key == "Unidentified" => String::new(),
        (false, _) => format!("r.text({})", input::quote(&key.key)),
    };
    let dispatch = format!(
        "r.key(\"keydown\", {}, {}, {}, {modifiers}, {repeat})",
        input::quote(&key.key),
        input::quote(key.code),
        key.key_code
    );
    let body = match action.is_empty() {
        true => dispatch,
        false => format!("if ({dispatch}) {{ {action}; }}"),
    };
    run_input(engine, gtk, &body);
}

unsafe extern "C" fn key_up(receiver: Id, _sel: Sel, event: Id) {
    let Some((engine, gtk)) = gtk_of(receiver) else {
        return;
    };
    // SAFETY: an event; each accessor is AppKit's own prototype.
    let (text, key_code) = unsafe {
        (
            engine::text_of(send!(event, "characters"; Id)),
            send!(event, "keyCode"; u16),
        )
    };
    let key = input::key(key_code, &text);
    let body = format!(
        "r.key(\"keyup\", {}, {}, {}, {}, false)",
        input::quote(&key.key),
        input::quote(key.code),
        key.key_code,
        modifiers(event)
    );
    run_input(engine, gtk, &body);
}

fn editing(receiver: Id, command: &std::ffi::CStr) {
    if let Some((engine, gtk)) = gtk_of(receiver) {
        // SAFETY: the view's widget and a command name.
        unsafe { (engine.api.webkit_web_view_execute_editing_command)(gtk, command.as_ptr()) };
    }
}

unsafe extern "C" fn copy(receiver: Id, _sel: Sel, _sender: Id) {
    editing(receiver, c"Copy");
}

unsafe extern "C" fn cut(receiver: Id, _sel: Sel, _sender: Id) {
    editing(receiver, c"Cut");
}

unsafe extern "C" fn paste(receiver: Id, _sel: Sel, _sender: Id) {
    editing(receiver, c"Paste");
}

unsafe extern "C" fn select_all(receiver: Id, _sel: Sel, _sender: Id) {
    editing(receiver, c"SelectAll");
}

unsafe extern "C" fn undo(receiver: Id, _sel: Sel, _sender: Id) {
    editing(receiver, c"Undo");
}

unsafe extern "C" fn redo(receiver: Id, _sel: Sel, _sender: Id) {
    editing(receiver, c"Redo");
}

pub fn install() {
    let view = objc::class::lookup(c"NSView").expect("AppKit registers NSView");
    // Registered as a view with no storage of its own: a guest subclass lays
    // its ivars out after NSView's, so everything is kept beside the object.
    let web_view = corefoundation::base::abstract_class(c"WKWebView", Some(view));
    crate::methods!(web_view, {
        "initWithFrame:configuration:" => init_with_frame_configuration, c"@56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "initWithFrame:" => init_with_frame, c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "dealloc" => dealloc, c"v16@0:8";
        "configuration" => configuration_getter, c"@16@0:8";
        "_userAgent" => user_agent_getter, c"@16@0:8";
        "navigationDelegate" => navigation_delegate, c"@16@0:8";
        "setNavigationDelegate:" => set_navigation_delegate, c"v24@0:8@16";
        "UIDelegate" => ui_delegate, c"@16@0:8";
        "setUIDelegate:" => set_ui_delegate, c"v24@0:8@16";
        "loadRequest:" => load_request, c"@24@0:8@16";
        "stopLoading" => stop_loading, c"v16@0:8";
        "URL" => url, c"@16@0:8";
        "evaluateJavaScript:completionHandler:" => evaluate_javascript, c"v32@0:8@16@?24";
        "setValue:forKey:" => set_value_for_key, c"v32@0:8@16@24";
        "acceptsFirstResponder" => yes, c"c16@0:8";
        "becomeFirstResponder" => become_first_responder, c"c16@0:8";
        "resignFirstResponder" => resign_first_responder, c"c16@0:8";
        "isFlipped" => yes, c"c16@0:8";
        "mouseDown:" => mouse_down, c"v24@0:8@16";
        "mouseUp:" => mouse_up, c"v24@0:8@16";
        "rightMouseDown:" => right_mouse_down, c"v24@0:8@16";
        "rightMouseUp:" => right_mouse_up, c"v24@0:8@16";
        "otherMouseDown:" => other_mouse_down, c"v24@0:8@16";
        "otherMouseUp:" => other_mouse_up, c"v24@0:8@16";
        "mouseMoved:" => mouse_moved, c"v24@0:8@16";
        "mouseDragged:" => mouse_dragged, c"v24@0:8@16";
        "rightMouseDragged:" => right_mouse_dragged, c"v24@0:8@16";
        "scrollWheel:" => scroll_wheel, c"v24@0:8@16";
        "keyDown:" => key_down, c"v24@0:8@16";
        "keyUp:" => key_up, c"v24@0:8@16";
        "copy:" => copy, c"v24@0:8@16";
        "cut:" => cut, c"v24@0:8@16";
        "paste:" => paste, c"v24@0:8@16";
        "selectAll:" => select_all, c"v24@0:8@16";
        "undo:" => undo, c"v24@0:8@16";
        "redo:" => redo, c"v24@0:8@16";
    });

    corefoundation::base::abstract_class(c"WKNavigation", None);
    corefoundation::base::abstract_class(c"WKWindowFeatures", None);
    crate::methods!(origin_class(), {
        "protocol" => origin_protocol, c"@16@0:8";
        "host" => origin_host, c"@16@0:8";
        "port" => origin_port, c"q16@0:8";
    });
    crate::methods!(frame_class(), {
        "isMainFrame" => frame_is_main, c"c16@0:8";
        "request" => frame_request, c"@16@0:8";
        "securityOrigin" => frame_origin, c"@16@0:8";
        "webView" => nil, c"@16@0:8";
    });
    crate::methods!(action_class(), {
        "request" => action_request, c"@16@0:8";
        "sourceFrame" => action_source_frame, c"@16@0:8";
        "targetFrame" => action_target_frame, c"@16@0:8";
        "navigationType" => action_navigation_type, c"q16@0:8";
        "modifierFlags" => zero, c"Q16@0:8";
        "buttonNumber" => zero, c"q16@0:8";
    });
    crate::methods!(response_class(), {
        "response" => response_response, c"@16@0:8";
        "isForMainFrame" => response_is_main, c"c16@0:8";
        "canShowMIMEType" => yes, c"c16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_security_origin_is_the_scheme_host_and_explicit_port() {
        let origin = SecurityOrigin::of("HTTPS://User@WWW.Example.com:8443/login?x=1");
        assert_eq!(
            (origin.protocol.as_str(), origin.host.as_str(), origin.port),
            ("https", "www.example.com", 8443)
        );
        let origin = SecurityOrigin::of("https://messages.example.test/message?msg0=%7B%7D");
        assert_eq!(
            (origin.host.as_str(), origin.port),
            ("messages.example.test", 0)
        );
    }

    #[test]
    fn command_shortcuts_are_editing_commands() {
        assert_eq!(shortcut("V", false), Some(c"Paste"));
        assert_eq!(shortcut("z", true), Some(c"Redo"));
        assert_eq!(shortcut("q", false), None);
    }
}
