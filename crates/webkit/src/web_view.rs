use std::cell::RefCell;
use std::ffi::{c_char, c_int};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use foundation::boxed;
use foundation::geometry::NSRect;
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::compositor::{Command, Event, Monitor};
use crate::engine::{self, Block, Engine, HostBlock};
use crate::gtk::{self, Ptr};
use crate::{clipboard, configuration, keys, send};

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
    /// The undecorated `GtkWindow` the web view is the child of, which GTK
    /// owns until it is destroyed.
    window: usize,
    /// The window's `wl_surface`, by which the compositor names it.
    surface: u32,
    /// Retained.
    configuration: usize,
    /// Weak, as WebKit's delegates are: a delegate that goes away while a
    /// load is still reporting must read back as nil, not as freed memory.
    navigation_delegate: Weak,
    ui_delegate: Weak,
    /// The `WKNavigation` of the load in progress, retained.
    navigation: usize,
    /// The size, scale, suspension and display last sent to the compositor.
    configured: Option<Configured>,
    draws_background: bool,
    /// The modifier flags of the last key event, from which `-flagsChanged:`
    /// works out which modifier key moved.
    modifier_flags: u64,
    /// The cursor the page asks for, by its CSS name.
    cursor: Option<&'static str>,
}

#[derive(Clone, PartialEq)]
struct Configured {
    width: c_int,
    height: c_int,
    scale: f64,
    suspended: bool,
    monitor: Option<Monitor>,
    /// Whether its window's presenter draws dma-bufs.
    dmabuf: bool,
}

/// A zeroing weak reference, boxed so that its slot keeps its address while
/// the view list moves.
struct Weak(Box<Id>);

impl Weak {
    fn new() -> Weak {
        Weak(Box::new(std::ptr::null_mut()))
    }

    fn set(&mut self, object: Id) {
        objc::arc::store_weak(&mut *self.0, object);
    }

    /// The object, autoreleased, or nil once it is gone.
    fn get(&mut self) -> Id {
        let object = objc::arc::load_weak_retained(&mut *self.0);
        match object.is_null() {
            true => object,
            false => objc::arc::autorelease(object),
        }
    }
}

impl Drop for Weak {
    fn drop(&mut self) {
        objc::arc::destroy_weak(&mut *self.0);
    }
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

struct ScriptMessage {
    name: String,
    /// The body, the web view and the frame, each retained.
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

fn message_class() -> &'static Class {
    class::<ScriptMessage>(c"WKScriptMessage")
}

/// The `WKScriptMessage` for a page's message to handler `name`, from the
/// first view showing pages through `manager`.
///
/// WebKitGTK does not say which view or frame posted it; views that share a
/// manager share a content controller, and the main frame is the sender for
/// the bridges that use this.
pub(crate) fn script_message(manager: usize, name: &str, body: Id) -> Id {
    let view = VIEWS.with(|views| {
        views
            .borrow()
            .iter()
            // SAFETY: each view's retained configuration.
            .find(|v| unsafe { configuration::existing_manager(v.configuration as Id) } == manager)
            .map(|v| (v.object, v.gtk))
    });
    let (object, gtk) = view.unwrap_or_default();
    let url = match engine::get() {
        // SAFETY: the view's own widget; the string is the view's.
        Some(engine) if gtk != 0 => unsafe {
            gtk::text((engine.api.webkit_web_view_get_uri)(gtk as Ptr))
        },
        _ => String::new(),
    };
    let body = match body.is_null() {
        // A page that posts `null` or `undefined`, which WebKit hands over as
        // `NSNull`.
        true => {
            let class = objc::class::lookup(c"NSNull").expect("registered by Foundation");
            // SAFETY: `+null` is `id (Class, SEL)`.
            unsafe { send!(objc::class::ptr(class) as Id, "null"; Id) }
        }
        false => body,
    };
    autoreleased(
        message_class(),
        ScriptMessage {
            name: name.to_string(),
            objects: Owned::new(&[body, object as Id, frame_info(&guest_url(&url), true)]),
        },
    )
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
    message_name,
    Id,
    message_class,
    |m: &mut ScriptMessage| engine::string(&m.name),
    std::ptr::null_mut()
);
getter!(
    message_body,
    Id,
    message_class,
    |m: &mut ScriptMessage| m.objects.get(0),
    std::ptr::null_mut()
);
getter!(
    message_web_view,
    Id,
    message_class,
    |m: &mut ScriptMessage| m.objects.get(1),
    std::ptr::null_mut()
);
getter!(
    message_frame_info,
    Id,
    message_class,
    |m: &mut ScriptMessage| m.objects.get(2),
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
        eprintln!("rosemary: webkit: {verdict} {}", context.url);
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
        navigation: v.navigation_delegate.get(),
        ui: v.ui_delegate.get(),
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
                eprintln!("rosemary: webkit: navigation to {url}");
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
        eprintln!("rosemary: webkit: load event {event} for {url}");
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
    eprintln!("rosemary: webkit: loading {url} failed: {message} ({domain} {code})");
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
    eprintln!("rosemary: webkit: the web process terminated (reason {reason})");
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

/// `agent` as WebKitGTK accepts it for the `User-Agent` header.
///
/// WebKitGTK refuses a user agent that is not a list of `product/version`
/// tokens and comments, and then sends its own. A product with a second `/` -
/// Roblox's `Roblox/DarwinRobloxApp/0.740` - is one it refuses, although
/// macOS WebKit sends it as it is. Only the last `/` of such a product is
/// kept: servers match the innermost name with its version, and Roblox serves
/// its app layout only for `DarwinRobloxApp/0.740`.
pub(crate) fn header_user_agent(agent: &str) -> String {
    let mut out = String::with_capacity(agent.len());
    let mut depth = 0usize;
    // Where the current top-level product's last `/` was written.
    let mut slash = None;
    for c in agent.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && depth == 0 => slash = None,
            '/' if depth == 0 => {
                if let Some(at) = slash.replace(out.len()) {
                    out.replace_range(at..at + 1, " ");
                }
            }
            _ => {}
        }
        out.push(c);
    }
    out
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
        eprintln!("rosemary: webkit: user agent {agent}");
    }
    let header = header_user_agent(&agent);
    // SAFETY: the view's content controller.
    unsafe {
        configuration::set_page_user_agent(
            controller,
            engine,
            (header != agent).then(|| agent.clone()),
        )
    };
    let agent = gtk::c_string(&header);
    // SAFETY: a configuration's preferences.
    let developer =
        unsafe { configuration::preferences(configuration::preferences_of(configuration)) }
            .is_some_and(|p| p.developer_extras);
    // SAFETY: the view's own settings, and a string they copy.
    unsafe {
        let settings = (api.webkit_web_view_get_settings)(gtk);
        (api.webkit_settings_set_user_agent)(settings, agent.as_ptr());
        (api.webkit_settings_set_enable_developer_extras)(settings, c_int::from(developer));
        (api.webkit_settings_set_enable_write_console_messages_to_stdout)(
            settings,
            c_int::from(engine::tracing()),
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

/// Put `gtk` in a window of its own on the compositor, sized `width` by
/// `height` until the compositor says otherwise. The window and its
/// `wl_surface`'s id.
fn make_window(engine: &Engine, gtk: Ptr, width: c_int, height: c_int) -> Option<(Ptr, u32)> {
    let api = engine.api;
    // SAFETY: a new window, which GTK owns, and the view it takes a reference
    // to; the native and surface belong to the window and live as long.
    unsafe {
        let window = (api.gtk_window_new)();
        if window.is_null() {
            return None;
        }
        (api.gtk_widget_add_css_class)(window, gtk::WINDOW_CLASS.as_ptr());
        (api.gtk_window_set_decorated)(window, 0);
        (api.gtk_window_set_default_size)(window, width.max(1), height.max(1));
        (api.gtk_window_set_child)(window, gtk);
        // The web view is the window's only widget, so it is the one the
        // keyboard goes to whenever the window has it.
        (api.gtk_widget_grab_focus)(gtk);
        // Realized, the window has its `wl_surface`, whose id is what the
        // compositor knows it by; presented, it becomes a toplevel.
        (api.gtk_widget_realize)(window);
        let surface = (api.gtk_native_get_surface)((api.gtk_widget_get_native)(window));
        let proxy = match surface.is_null() {
            true => std::ptr::null_mut(),
            false => (api.gdk_wayland_surface_get_wl_surface)(surface),
        };
        if proxy.is_null() {
            (api.gtk_window_destroy)(window);
            return None;
        }
        let id = (api.wl_proxy_get_id)(proxy);
        (api.gtk_window_present)(window);
        Some((window, id))
    }
}

/// Give the page the keyboard, or take it away, as the AppKit view becomes
/// or stops being first responder - the same event WebKit follows on macOS.
fn set_focused(object: Id, focused: bool) {
    let Some(engine) = engine::get() else {
        return;
    };
    if let Some(surface) = with_object(object, |v| v.surface).filter(|s| *s != 0) {
        focus(engine, surface, focused);
    }
}

fn focus(engine: &Engine, surface: u32, focused: bool) {
    thread_local! {
        static FOCUSED: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
    }
    if focused {
        clipboard::offer(engine);
        FOCUSED.set(Some(surface));
    } else if FOCUSED.get() == Some(surface) {
        FOCUSED.set(None);
    } else {
        // Another view has taken the keyboard already.
        return;
    }
    if engine::tracing() {
        eprintln!(
            "rosemary: webkit: keyboard focus {} surface {surface}",
            match focused {
                true => "to",
                false => "away from",
            }
        );
    }
    engine.compositor.send(Command::Focus {
        surface: focused.then_some(surface),
    });
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
    track_pointer(receiver);
    let configuration = match configuration.is_null() {
        true => configuration::new_configuration(),
        false => objc::arc::retain(configuration),
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut gtk = std::ptr::null_mut();
    let mut window = None;
    if let Some(engine) = engine::get() {
        // SAFETY: a configuration, made or retained above.
        gtk = unsafe { make_gtk_view(engine, configuration, id) };
        if !gtk.is_null() {
            window = make_window(
                engine,
                gtk,
                frame.size.width.round() as c_int,
                frame.size.height.round() as c_int,
            );
            if window.is_none() {
                engine::warn_once(
                    "a web view's GTK window has no Wayland surface; it will not show",
                );
            }
        }
    }
    let (window, surface) = window.unwrap_or((std::ptr::null_mut(), 0));
    VIEWS.with(|views| {
        views.borrow_mut().push(View {
            id,
            object: receiver as usize,
            gtk: gtk as usize,
            window: window as usize,
            surface,
            configuration: configuration as usize,
            navigation_delegate: Weak::new(),
            ui_delegate: Weak::new(),
            navigation: 0,
            configured: None,
            draws_background: true,
            modifier_flags: 0,
            cursor: None,
        })
    });
    receiver
}

unsafe extern "C" fn init_with_frame(receiver: Id, sel: Sel, frame: NSRect) -> Id {
    // SAFETY: the designated initializer with the default configuration.
    unsafe { init_with_frame_configuration(receiver, sel, frame, std::ptr::null_mut()) }
}

/// Ask for `-mouseMoved:` over the view, as WebKit's own view does: AppKit
/// sends it only to a window that accepts it or to a tracking area's owner,
/// and without it the page sees the pointer only when a button changes.
fn track_pointer(receiver: Id) {
    // `NSTrackingMouseMoved | NSTrackingActiveInActiveApp |
    // NSTrackingInVisibleRect`: the rectangle follows the bounds.
    const OPTIONS: usize = 0x02 | 0x40 | 0x200;
    let class = objc::class::lookup(c"NSTrackingArea").expect("registered by AppKit");
    // SAFETY: `+alloc` is `id (Class, SEL)`, the initializer is
    // `id (id, SEL, NSRect, NSUInteger, id, id)` and `-addTrackingArea:`,
    // which retains the area, is `void (id, SEL, id)`.
    unsafe {
        let area = send!(objc::class::ptr(class) as Id, "alloc"; Id);
        let area = send!(
            area,
            "initWithRect:options:owner:userInfo:",
            NSRect::default() => NSRect,
            OPTIONS => usize,
            receiver => Id,
            std::ptr::null_mut() => Id;
            Id
        );
        if area.is_null() {
            return;
        }
        send!(receiver, "addTrackingArea:", area => Id; ());
        objc::arc::release(area);
    }
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
            if view.window != 0 {
                (api.gtk_window_destroy)(view.window as Ptr);
            }
        }
        engine::unref(api, view.gtk);
        if view.surface != 0 {
            focus(engine, view.surface, false);
            engine.compositor.send(Command::Forget {
                surface: view.surface,
            });
        }
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

/// Size each view's window to its view, suspend the hidden ones, and show
/// the pictures the compositor has for the rest.
pub fn tick(engine: &Engine) {
    let views: Vec<(usize, usize, usize, u32)> = VIEWS.with(|views| {
        views
            .borrow()
            .iter()
            .map(|v| (v.id, v.object, v.gtk, v.surface))
            .collect()
    });
    for (id, object, gtk, surface) in views {
        let (object, gtk) = (object as Id, gtk as Ptr);
        let hidden = appkit::view::is_hidden_or_has_hidden_ancestor(object);
        // SAFETY: a live view, registered while it is.
        let window = unsafe { appkit::view::window_of(object) };
        let frame = unsafe { appkit::view::presentation_frame(object) };
        if engine::tracing() {
            trace_state(id, gtk, hidden, window, frame.as_ref());
        }
        if gtk.is_null() || surface == 0 {
            continue;
        }
        let shown = !hidden && appkit::window::is_shown(window);
        let configured = match &frame {
            Some(frame) => {
                let density = f64::from(sdl3::video::window_pixel_density(frame.window)).max(1.0);
                Some(Configured {
                    width: (f64::from(frame.width) / density).round().max(1.0) as c_int,
                    height: (f64::from(frame.height) / density).round().max(1.0) as c_int,
                    scale: density,
                    suspended: !shown,
                    monitor: monitor_of(frame.window),
                    dmabuf: appkit::backing::shows_dmabufs(frame.window),
                })
            }
            // Off every window: keep the size, and stop drawing.
            None => with_view(id, |v| v.configured.clone())
                .flatten()
                .map(|c| Configured {
                    suspended: true,
                    ..c
                }),
        };
        if let Some(configured) = configured
            && with_view(id, |v| {
                let changed = v.configured.as_ref() != Some(&configured);
                if changed {
                    v.configured = Some(configured.clone());
                }
                changed
            }) == Some(true)
        {
            engine.compositor.send(Command::Configure {
                surface,
                width: configured.width,
                height: configured.height,
                scale: configured.scale,
                suspended: configured.suspended,
                monitor: configured.monitor,
                dmabuf: configured.dmabuf,
            });
        }
        if engine::tracing() {
            trace_page(engine, id, gtk);
        }
        if !shown || frame.is_none() {
            continue;
        }
        if let Some(picture) = engine.compositor.take_frame(surface) {
            if engine::tracing() {
                let kind = match picture.picture {
                    appkit::backing::Picture::Pixels(_) => "pixel",
                    appkit::backing::Picture::Dmabuf(_) => "dma-buf",
                };
                eprintln!(
                    "rosemary: webkit: view {id}: new {}x{} {kind} frame",
                    picture.width, picture.height
                );
            }
            appkit::backing::set_overlay(
                object,
                appkit::backing::Overlay {
                    width: picture.width,
                    height: picture.height,
                    picture: picture.picture,
                },
            );
        }
    }
    for event in engine.compositor.take_events() {
        match event {
            Event::Cursor { surface, name } => set_cursor(surface, name),
            Event::Selection { entries } => clipboard::copied(entries),
        }
    }
}

/// The display most of `window` is on, and the mode it shows: a page is
/// drawn at that display's refresh rate, and follows the window to another.
fn monitor_of(window: sdl3::Window) -> Option<Monitor> {
    let display = sdl3::video::display_for_window(window)?;
    let mode = sdl3::video::current_mode(display)?;
    let density = f64::from(mode.pixel_density).max(1.0);
    let refresh = match (mode.refresh_rate_numerator, mode.refresh_rate_denominator) {
        (numerator, denominator) if numerator > 0 && denominator > 0 => {
            (i64::from(numerator) * 1000 / i64::from(denominator)) as u32
        }
        _ if mode.refresh_rate > 0.0 => (f64::from(mode.refresh_rate) * 1000.0).round() as u32,
        _ => return None,
    };
    Some(Monitor {
        name: sdl3::video::display_name(display),
        width: (f64::from(mode.w) * density).round() as i32,
        height: (f64::from(mode.h) * density).round() as i32,
        refresh,
    })
}

/// The page under the pointer wants `name`'s cursor.
fn set_cursor(surface: u32, name: Option<&'static str>) {
    let object = VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        let view = views.iter_mut().find(|v| v.surface == surface)?;
        (view.cursor != name).then(|| {
            view.cursor = name;
            view.object
        })
    });
    if let Some(object) = object {
        // SAFETY: a live view.
        unsafe { appkit::cursor::reset_rects(object as Id) };
    }
}

/// The `NSCursor` class method for a CSS cursor name.
fn cursor_selector(name: &str) -> &'static str {
    match name {
        "pointer" => "pointingHandCursor",
        "text" => "IBeamCursor",
        "vertical-text" => "IBeamCursorForVerticalLayout",
        "crosshair" | "cell" => "crosshairCursor",
        "grab" => "openHandCursor",
        "grabbing" => "closedHandCursor",
        "not-allowed" | "no-drop" => "operationNotAllowedCursor",
        "copy" => "dragCopyCursor",
        "alias" => "dragLinkCursor",
        "context-menu" => "contextualMenuCursor",
        "col-resize" | "ew-resize" => "resizeLeftRightCursor",
        "row-resize" | "ns-resize" => "resizeUpDownCursor",
        "e-resize" => "resizeRightCursor",
        "w-resize" => "resizeLeftCursor",
        "n-resize" => "resizeUpCursor",
        "s-resize" => "resizeDownCursor",
        _ => "arrowCursor",
    }
}

/// `-resetCursorRects`: the whole view shows the cursor the page asked for.
unsafe extern "C" fn reset_cursor_rects(receiver: Id, _sel: Sel) {
    let Some(name) = with_object(receiver, |v| v.cursor).flatten() else {
        return;
    };
    let class = objc::class::lookup(c"NSCursor").expect("registered by AppKit");
    // SAFETY: each cursor class method is `id (Class, SEL)`.
    let cursor = unsafe {
        let send: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        send(
            objc::class::ptr(class) as Id,
            objc::sel::get(cursor_selector(name)),
        )
    };
    appkit::cursor::add_rect(receiver, appkit::view::bounds_of(receiver), cursor);
}

/// `ROSEMARY_TRACE_WEBKIT`: every few seconds, where the page is, whether
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
        eprintln!("rosemary: webkit: view {}: page {text}", data as usize);
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

/// `ROSEMARY_TRACE_WEBKIT`: say why a view is or is not being drawn, when
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
        eprintln!("rosemary: webkit: view {id}: {state}");
    });
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
    with_object(receiver, |v| v.navigation_delegate.set(delegate));
}

unsafe extern "C" fn navigation_delegate(receiver: Id, _sel: Sel) -> Id {
    with_object(receiver, |v| v.navigation_delegate.get()).unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn set_ui_delegate(receiver: Id, _sel: Sel, delegate: Id) {
    with_object(receiver, |v| v.ui_delegate.set(delegate));
}

unsafe extern "C" fn ui_delegate(receiver: Id, _sel: Sel) -> Id {
    with_object(receiver, |v| v.ui_delegate.get()).unwrap_or(std::ptr::null_mut())
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
    eprintln!("rosemary: webkit: loading {url}");
    let Some((engine, gtk)) = gtk_of(receiver) else {
        return std::ptr::null_mut();
    };
    if method != "GET" || !headers.is_empty() {
        engine::warn_once("a request's method and headers are not sent; it is loaded as a GET");
    }
    let navigation = start_navigation(receiver);
    let url = gtk::c_string(&host_url(&url));
    // SAFETY: the view's widget and a string it copies.
    unsafe { (engine.api.webkit_web_view_load_uri)(gtk, url.as_ptr()) };
    navigation
}

unsafe extern "C" fn can_go_back(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: the view's widget.
    gtk_of(receiver)
        .is_some_and(|(engine, gtk)| unsafe { (engine.api.webkit_web_view_can_go_back)(gtk) != 0 })
        as u8
}

unsafe extern "C" fn can_go_forward(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: the view's widget.
    gtk_of(receiver).is_some_and(|(engine, gtk)| unsafe {
        (engine.api.webkit_web_view_can_go_forward)(gtk) != 0
    }) as u8
}

/// `-goBack`, answering the navigation it starts, or nil with nowhere to go.
unsafe extern "C" fn go_back(receiver: Id, sel: Sel) -> Id {
    // SAFETY: as `can_go_back`.
    if unsafe { can_go_back(receiver, sel) } == 0 {
        return std::ptr::null_mut();
    }
    let Some((engine, gtk)) = gtk_of(receiver) else {
        return std::ptr::null_mut();
    };
    unsafe { (engine.api.webkit_web_view_go_back)(gtk) };
    start_navigation(receiver)
}

unsafe extern "C" fn go_forward(receiver: Id, sel: Sel) -> Id {
    // SAFETY: as `can_go_forward`.
    if unsafe { can_go_forward(receiver, sel) } == 0 {
        return std::ptr::null_mut();
    }
    let Some((engine, gtk)) = gtk_of(receiver) else {
        return std::ptr::null_mut();
    };
    unsafe { (engine.api.webkit_web_view_go_forward)(gtk) };
    start_navigation(receiver)
}

/// A new current navigation for `receiver`, which the load it names reports
/// through.
fn start_navigation(receiver: Id) -> Id {
    let navigation = new_navigation();
    let old = with_object(receiver, |v| {
        std::mem::replace(&mut v.navigation, navigation as usize)
    });
    objc::arc::release(old.unwrap_or_default() as Id);
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
        with_object(receiver, |v| v.draws_background = draws);
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

/// The compositor, and the surface `receiver`'s page is on.
fn surface_of(receiver: Id) -> Option<(&'static Engine, u32)> {
    let surface = with_object(receiver, |v| v.surface).filter(|s| *s != 0)?;
    Some((engine::get()?, surface))
}

/// Move the page's pointer to where `event` happened.
fn pointer_to(receiver: Id, event: Id) -> Option<&'static Engine> {
    let (engine, surface) = surface_of(receiver)?;
    let (x, y) = page_point(receiver, event)?;
    engine.compositor.send(Command::Pointer { surface, x, y });
    Some(engine)
}

/// `BTN_LEFT`, `BTN_RIGHT` and `BTN_MIDDLE`.
const BUTTONS: [u32; 3] = [0x110, 0x111, 0x112];

fn mouse(receiver: Id, event: Id, pressed: bool, button: usize) {
    if pressed {
        // A click is what gives a web view the keyboard, as it does in AppKit.
        // SAFETY: a live view; both methods are AppKit's own prototypes.
        unsafe {
            let window = send!(receiver, "window"; Id);
            if !window.is_null() {
                send!(window, "makeFirstResponder:", receiver => Id; u8);
            }
        }
    }
    if let Some(engine) = pointer_to(receiver, event) {
        engine.compositor.send(Command::Button {
            button: BUTTONS[button],
            pressed,
        });
    }
}

unsafe extern "C" fn mouse_down(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, true, 0);
}

unsafe extern "C" fn mouse_up(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, false, 0);
}

unsafe extern "C" fn right_mouse_down(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, true, 1);
}

unsafe extern "C" fn right_mouse_up(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, false, 1);
}

unsafe extern "C" fn other_mouse_down(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, true, 2);
}

unsafe extern "C" fn other_mouse_up(receiver: Id, _sel: Sel, event: Id) {
    mouse(receiver, event, false, 2);
}

unsafe extern "C" fn mouse_moved(receiver: Id, _sel: Sel, event: Id) {
    pointer_to(receiver, event);
}

unsafe extern "C" fn scroll_wheel(receiver: Id, _sel: Sel, event: Id) {
    let Some(engine) = pointer_to(receiver, event) else {
        return;
    };
    // SAFETY: an event; the deltas are `CGFloat (id, SEL)` and the precision
    // flag `BOOL (id, SEL)`.
    let (dx, dy, precise) = unsafe {
        (
            send!(event, "scrollingDeltaX"; f64),
            send!(event, "scrollingDeltaY"; f64),
            send!(event, "hasPreciseScrollingDeltas"; u8) != 0,
        )
    };
    // AppKit's deltas are positive toward the top and left of the content,
    // Wayland's toward the bottom and right. A wheel's are in lines, which
    // become notches for GTK; a touchpad's are already pixels.
    // libinput reports a notch as 15 pixels, which is what GTK expects.
    const LINE: f64 = 15.0;
    let command = match precise {
        true => Command::Axis {
            dx: -dx,
            dy: -dy,
            steps: None,
        },
        false => Command::Axis {
            dx: -dx * LINE,
            dy: -dy * LINE,
            steps: Some(((-dx * 120.0).round() as i32, (-dy * 120.0).round() as i32)),
        },
    };
    engine.compositor.send(command);
}

/// Bring the keyboard's modifiers into line with the event's, so a key is
/// read with what is held when it is pressed even if a `-flagsChanged:` went
/// elsewhere.
fn sync_modifiers(engine: &Engine, receiver: Id, event: Id) {
    // SAFETY: an event; `-modifierFlags` is `NSUInteger (id, SEL)`.
    let flags = unsafe { send!(event, "modifierFlags"; u64) };
    let before = with_object(receiver, |v| {
        std::mem::replace(&mut v.modifier_flags, flags)
    });
    for (key, pressed) in keys::modifier_changes(before.unwrap_or_default(), flags) {
        engine.compositor.send(Command::Key { key, pressed });
    }
}

fn key(receiver: Id, event: Id, pressed: bool) {
    let Some((engine, _)) = surface_of(receiver) else {
        return;
    };
    sync_modifiers(engine, receiver, event);
    // SAFETY: an event; both accessors are AppKit's own prototypes.
    let (code, repeat) = unsafe { (send!(event, "keyCode"; u16), send!(event, "isARepeat"; u8)) };
    // GTK repeats a held key itself, at the rate the compositor gives it.
    if repeat != 0 {
        return;
    }
    let Some(key) = keys::evdev(code) else {
        return;
    };
    // Command-V: GTK is about to read the clipboard, which has to be the
    // host's by then.
    let modifiers = with_object(receiver, |v| v.modifier_flags).unwrap_or_default();
    if pressed
        && key == 47
        && modifiers & (appkit::keymap::flags::COMMAND | appkit::keymap::flags::CONTROL) != 0
        && modifiers & appkit::keymap::flags::OPTION == 0
    {
        clipboard::offer(engine);
    }
    if engine::tracing() {
        eprintln!(
            "rosemary: webkit: key {key} {}",
            if pressed { "down" } else { "up" }
        );
    }
    engine.compositor.send(Command::Key { key, pressed });
}

unsafe extern "C" fn key_down(receiver: Id, _sel: Sel, event: Id) {
    key(receiver, event, true);
}

unsafe extern "C" fn key_up(receiver: Id, _sel: Sel, event: Id) {
    key(receiver, event, false);
}

/// `-performKeyEquivalent:`: the editing shortcuts are the page's while it
/// has the keyboard, as they are WebKit's on a Mac, and reach it as the keys
/// they are, so the page sees them and GTK performs them. Every other
/// shortcut is left to the menus.
unsafe extern "C" fn perform_key_equivalent(receiver: Id, _sel: Sel, event: Id) -> u8 {
    // SAFETY: a live view and an event; each accessor is AppKit's prototype.
    let (first, code, flags) = unsafe {
        let window = send!(receiver, "window"; Id);
        let mut responder = if window.is_null() {
            std::ptr::null_mut()
        } else {
            send!(window, "firstResponder"; Id)
        };
        let mut first = false;
        for _ in 0..64 {
            if responder.is_null() {
                break;
            }
            if responder == receiver {
                first = true;
                break;
            }
            responder = appkit::view::superview_of(responder);
        }
        (
            first,
            send!(event, "keyCode"; u16),
            send!(event, "modifierFlags"; u64),
        )
    };
    use appkit::keymap::flags::{COMMAND, CONTROL, OPTION};
    // C, X, V, A and Z, by position.
    const EDITING: [u16; 5] = [0x08, 0x07, 0x09, 0x00, 0x06];
    let editing =
        flags & (COMMAND | CONTROL) != 0 && flags & OPTION == 0 && EDITING.contains(&code);
    if !first || !editing || surface_of(receiver).is_none() {
        return 0;
    }
    key(receiver, event, true);
    1
}

unsafe extern "C" fn flags_changed(receiver: Id, _sel: Sel, event: Id) {
    if let Some((engine, _)) = surface_of(receiver) {
        sync_modifiers(engine, receiver, event);
    }
}

fn editing(receiver: Id, command: &std::ffi::CStr) {
    if let Some((engine, gtk)) = gtk_of(receiver) {
        if engine::tracing() {
            eprintln!("rosemary: webkit: editing command {command:?}");
        }
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
    if let Some(engine) = engine::get() {
        clipboard::offer(engine);
    }
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
        "canGoBack" => can_go_back, c"c16@0:8";
        "canGoForward" => can_go_forward, c"c16@0:8";
        "goBack" => go_back, c"@16@0:8";
        "goForward" => go_forward, c"@16@0:8";
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
        "mouseDragged:" => mouse_moved, c"v24@0:8@16";
        "rightMouseDragged:" => mouse_moved, c"v24@0:8@16";
        "otherMouseDragged:" => mouse_moved, c"v24@0:8@16";
        "scrollWheel:" => scroll_wheel, c"v24@0:8@16";
        "keyDown:" => key_down, c"v24@0:8@16";
        "keyUp:" => key_up, c"v24@0:8@16";
        "flagsChanged:" => flags_changed, c"v24@0:8@16";
        "performKeyEquivalent:" => perform_key_equivalent, c"c24@0:8@16";
        "resetCursorRects" => reset_cursor_rects, c"v16@0:8";
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
    crate::methods!(message_class(), {
        "name" => message_name, c"@16@0:8";
        "body" => message_body, c"@16@0:8";
        "webView" => message_web_view, c"@16@0:8";
        "frameInfo" => message_frame_info, c"@16@0:8";
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
    fn a_product_with_two_slashes_is_split_for_the_header() {
        let agent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
            (KHTML, like Gecko) Roblox/DarwinRobloxApp/0.740.0.7400927 (GlobalDist; a/b/c)";
        assert_eq!(
            header_user_agent(agent),
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
            (KHTML, like Gecko) Roblox DarwinRobloxApp/0.740.0.7400927 (GlobalDist; a/b/c)"
        );
        assert_eq!(header_user_agent(USER_AGENT), USER_AGENT);
    }
}
