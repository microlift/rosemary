//! WebKit1's `WebView`, built on the `WKWebView` this crate runs on WebKitGTK.
//!
//! A `WebView` is a `WKWebView` subclass here, so drawing, input and loading
//! are that class's. What WebKit1 adds is its delegates and its script bridge:
//! `WebScriptObject`, through which a page calls Objective-C objects a program
//! put on its `window`, synchronously, and gets their answers back.
//!
//! WebKitGTK runs the page in another process and has no synchronous call into
//! this one - except a script dialog, which blocks the page until it is
//! answered. So a script injected before the page's own calls `prompt()` with
//! a marker and a JSON request, and the `script-dialog` handler here answers
//! it with the call's JSON result. Script that native code runs while the page
//! is blocked that way cannot run then; it is handed back with the answer and
//! run by the injected script, and the native caller gets `WebUndefined`.

use std::cell::RefCell;
use std::ffi::c_int;
use std::time::{Duration, Instant};

use foundation::boxed;
use foundation::geometry::NSRect;
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::engine;
use crate::gtk::{self, Ptr};
use crate::{send, web_view};

/// The message of a bridge prompt; any other prompt is the page's own.
const MAGIC: &str = "\u{1}rosemary-webscript";

/// How long a synchronous evaluation waits for the page.
const EVALUATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Run at document start in the top frame, before the page's scripts.
const HELPER: &str = r#"(function () {
  if (window.__rosemaryWebScript) return;
  var ask = window.prompt.bind(window);
  var run = eval;
  function call(message) {
    var text = ask("\u0001rosemary-webscript", JSON.stringify(message));
    var answer = JSON.parse(text || "{}");
    (answer.s || []).forEach(function (script) { run(script); });
    if (answer.e !== undefined) throw new Error(answer.e);
    return answer.r;
  }
  function define(binding) {
    var object = {};
    binding.m.forEach(function (name) {
      object[name] = function () {
        return call({t: "call", o: binding.o, m: name, a: Array.prototype.slice.call(arguments)});
      };
    });
    window[binding.k] = object;
  }
  Object.defineProperty(window, "__rosemaryWebScript", {value: {call: call, define: define}});
  call({t: "clear"});
})();"#;

/// An Objective-C object a page can call, and the names it answers to.
struct Exported {
    id: usize,
    /// Retained.
    object: usize,
    methods: Vec<(String, Sel)>,
}

struct Legacy {
    object: usize,
    /// Unretained, as WebKit's delegates are.
    frame_load: usize,
    policy: usize,
    ui: usize,
    editing: usize,
    resource_load: usize,
    download: usize,
    /// Retained; each keeps an unretained pointer back to the view.
    main_frame: usize,
    window: usize,
    preferences: usize,
    exported: Vec<Exported>,
    /// Script run while the page waited on a bridge call, for the answer.
    pending: Vec<String>,
    /// Bridge calls in progress.
    depth: usize,
    application_name: String,
    draws_background: bool,
}

thread_local! {
    static VIEWS: RefCell<Vec<Legacy>> = const { RefCell::new(Vec::new()) };
}

static NEXT_EXPORT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

fn with<R>(object: Id, body: impl FnOnce(&mut Legacy) -> R) -> Option<R> {
    VIEWS.with(|views| {
        views
            .borrow_mut()
            .iter_mut()
            .find(|v| v.object == object as usize)
            .map(body)
    })
}

/// The `WebView` a WebKitGTK callback is about.
fn view_of(data: Ptr) -> Option<Id> {
    let object = web_view::object_of(data as usize)?;
    with(object, |_| ()).map(|()| object)
}

/// A view's frame, window object, or delegate, by field.
fn field(object: Id, pick: impl FnOnce(&Legacy) -> usize) -> Id {
    with(object, |v| pick(v)).unwrap_or_default() as Id
}

// -- Plain objects ------------------------------------------------------------

/// `WebFrame`, `WebDataSource` and the window's `WebScriptObject`: each names
/// the view it belongs to, unretained - the view owns them.
struct Frame {
    view: usize,
}

struct DataSource {
    view: usize,
    url: String,
}

struct ScriptObject {
    view: usize,
}

/// A `WebPolicyDecisionListener`, holding the GTK decision it answers.
struct Listener {
    decision: usize,
}

impl Drop for Listener {
    fn drop(&mut self) {
        // A decision never answered is released unanswered, which WebKitGTK
        // takes as "use", as WebKit1 does for a listener dropped undecided.
        if let Some(engine) = engine::get() {
            engine::unref(engine.api, std::mem::take(&mut self.decision));
        }
    }
}

fn frame_class() -> &'static Class {
    boxed::register::<Frame>(c"WebFrame", None)
}

fn data_source_class() -> &'static Class {
    boxed::register::<DataSource>(c"WebDataSource", None)
}

fn script_object_class() -> &'static Class {
    boxed::register::<ScriptObject>(c"WebScriptObject", None)
}

fn listener_class() -> &'static Class {
    boxed::register::<Listener>(c"WebPolicyDecisionListenerImpl", None)
}

fn view_in<T: 'static>(object: Id, class: &'static Class, view: impl FnOnce(&T) -> usize) -> Id {
    // SAFETY: an instance of `class`, or anything else, which answers `None`.
    unsafe { boxed::contents::<T>(object, class) }.map_or(0, |c| view(c)) as Id
}

/// `+[WebUndefined undefined]`.
fn undefined() -> Id {
    static UNDEFINED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *UNDEFINED.get_or_init(|| {
        let class = objc::class::lookup(c"WebUndefined").expect("registered by install");
        objc::arc::retain(objc::object::create_instance(class, 0)) as usize
    }) as Id
}

// -- Script -------------------------------------------------------------------

/// Run `script` in the page: now, answering its value, or - while the page is
/// blocked on a bridge call - once the call returns, answering nil.
fn run_script(object: Id, script: String) -> Id {
    let deferred = with(object, |v| {
        if v.depth == 0 {
            return None;
        }
        v.pending.push(script.clone());
        Some(())
    })
    .flatten();
    if deferred.is_some() {
        if engine::tracing() {
            eprintln!("rosemary: webkit: script deferred until the page's bridge call returns");
        }
        return std::ptr::null_mut();
    }
    evaluate_now(object, &script)
}

struct Slot {
    done: bool,
    value: Id,
}

unsafe extern "C" fn evaluated(source: Ptr, result: Ptr, data: Ptr) {
    // SAFETY: the reference `evaluate_now` leaked for this callback.
    let slot = unsafe { std::rc::Rc::from_raw(data as *const RefCell<Slot>) };
    let Some(engine) = engine::get() else {
        return;
    };
    let api = engine.api;
    let mut error = std::ptr::null_mut();
    // SAFETY: the view and result this callback was invoked with.
    let value =
        unsafe { (api.webkit_web_view_evaluate_javascript_finish)(source, result, &mut error) };
    let object = match unsafe { gtk::take_error(api, error) } {
        Some((_, message)) => {
            eprintln!("rosemary: webkit: a script native code ran threw: {message}");
            std::ptr::null_mut()
        }
        // SAFETY: a value this callback owns; the bridged object is
        // autoreleased, so it is retained across the return to the caller.
        None => objc::arc::retain(unsafe { web_view::bridge(api, value) }),
    };
    if !value.is_null() {
        unsafe { (api.g_object_unref)(value) };
    }
    let mut slot = slot.borrow_mut();
    slot.done = true;
    slot.value = object;
}

/// Evaluate `script` and turn the loop until it answers, as WebKit1's
/// in-process evaluation returns its value.
fn evaluate_now(object: Id, script: &str) -> Id {
    let (Some(engine), Some((_, gtk, _))) = (engine::get(), web_view::parts_of(object)) else {
        return std::ptr::null_mut();
    };
    let api = engine.api;
    let slot = std::rc::Rc::new(RefCell::new(Slot {
        done: false,
        value: std::ptr::null_mut(),
    }));
    let source = gtk::c_string(script);
    // SAFETY: the view's widget, a script it copies, and a reference the
    // callback takes back.
    unsafe {
        (api.webkit_web_view_evaluate_javascript)(
            gtk,
            source.as_ptr(),
            -1,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            Some(evaluated),
            std::rc::Rc::into_raw(slot.clone()) as Ptr,
        )
    };
    let deadline = Instant::now() + EVALUATION_TIMEOUT;
    while !slot.borrow().done && Instant::now() < deadline {
        // SAFETY: the default context, on the thread GTK runs on.
        if unsafe { (api.g_main_context_iteration)(std::ptr::null_mut(), 0) } == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let slot = slot.borrow();
    if !slot.done {
        eprintln!("rosemary: webkit: a script native code ran did not answer in time");
        return std::ptr::null_mut();
    }
    objc::arc::autorelease(slot.value)
}

/// `object` as a JavaScript expression, or `None` for one JSON cannot carry.
fn expression(object: Id) -> Option<String> {
    if object.is_null() || object == undefined() {
        return Some("undefined".to_string());
    }
    foundation::json::to_text(object).ok()
}

// -- The bridge -----------------------------------------------------------------

/// The selectors of `object` a page may call, by the names it calls them.
///
/// WebKit1's rules: a method is hidden unless the class says it is not, by
/// `+isSelectorExcludedFromWebScript:`, and its name is what
/// `+webScriptNameForSelector:` answers, or the selector with each colon made
/// an underscore.
fn web_methods(object: Id) -> Vec<(String, Sel)> {
    // SAFETY: a live object.
    let Some(class) = (unsafe { objc::object::class_of(object) }) else {
        return Vec::new();
    };
    let class_object = objc::class::ptr(class) as Id;
    let decides = engine::responds(class_object, "isSelectorExcludedFromWebScript:");
    let names = engine::responds(class_object, "webScriptNameForSelector:");
    let mut out: Vec<(String, Sel)> = Vec::new();
    for c in class.chain() {
        if c.name_str() == "NSObject" {
            break;
        }
        for method in c.own_methods() {
            let sel = method.sel;
            if out.iter().any(|(_, s)| *s == sel) {
                continue;
            }
            // SAFETY: the class answers each selector asked, with these
            // prototypes.
            let excluded = !decides
                || unsafe {
                    send!(class_object, "isSelectorExcludedFromWebScript:", sel => Sel; u8)
                } != 0;
            if excluded {
                continue;
            }
            // SAFETY: a registered selector's name is a live C string.
            let selector = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(sel)) }
                .to_string_lossy()
                .into_owned();
            let named = match names {
                true => unsafe { send!(class_object, "webScriptNameForSelector:", sel => Sel; Id) },
                false => std::ptr::null_mut(),
            };
            let name = match named.is_null() {
                true => selector.replace(':', "_"),
                false => engine::text_of(named),
            };
            out.push((name, sel));
        }
    }
    out
}

/// Put `value` on the page's `window` under `key`: data as itself, any other
/// object as a set of functions that call it.
fn set_window_value(object: Id, key: &str, value: Id) {
    if let Some(data) = expression(value) {
        run_script(
            object,
            format!("window[{}] = {data};", crate::js::quote(key)),
        );
        return;
    }
    let methods = web_methods(value);
    let id = NEXT_EXPORT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let names: Vec<String> = methods.iter().map(|(n, _)| crate::js::quote(n)).collect();
    if engine::tracing() {
        eprintln!(
            "rosemary: webkit: window.{key} is an Objective-C object answering {}",
            names.join(", ")
        );
    }
    with(object, |v| {
        v.exported.push(Exported {
            id,
            object: objc::arc::retain(value) as usize,
            methods,
        })
    });
    run_script(
        object,
        format!(
            "window.__rosemaryWebScript.define({{o: {id}, k: {}, m: [{}]}});",
            crate::js::quote(key),
            names.join(", ")
        ),
    );
}

/// One of a parsed request's members.
fn member(message: Id, key: &str) -> Id {
    let key = engine::string(key);
    // SAFETY: a dictionary from the JSON reader, and a string key.
    unsafe { corefoundation::api::CFDictionaryGetValue(message as _, key as _) as Id }
}

/// The type codes of a method's return and its arguments after `self` and
/// `_cmd`, qualifiers and offsets dropped; aggregates as their first byte.
fn signature(types: &str) -> Vec<u8> {
    let bytes = types.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        while at < bytes.len() && b"rnNoORV".contains(&bytes[at]) {
            at += 1;
        }
        if at >= bytes.len() {
            break;
        }
        let code = bytes[at];
        at += 1;
        match code {
            b'{' | b'(' | b'[' => {
                let (open, close) = match code {
                    b'{' => (b'{', b'}'),
                    b'(' => (b'(', b')'),
                    _ => (b'[', b']'),
                };
                let mut depth = 1;
                while at < bytes.len() && depth > 0 {
                    depth += (bytes[at] == open) as i32 - (bytes[at] == close) as i32;
                    at += 1;
                }
            }
            // The pointee is part of the pointer's code.
            b'^' => {
                at += types[at..]
                    .find(|c: char| c.is_ascii_digit())
                    .unwrap_or(types.len() - at);
            }
            b'@' if bytes.get(at) == Some(&b'"') => {
                at += 1;
                while at < bytes.len() && bytes[at] != b'"' {
                    at += 1;
                }
                at += 1;
            }
            b'@' if bytes.get(at) == Some(&b'?') => at += 1,
            _ => {}
        }
        while at < bytes.len() && (bytes[at].is_ascii_digit() || bytes[at] == b'-') {
            at += 1;
        }
        out.push(code);
    }
    out
}

/// Send `sel` with integer-class arguments, answering the integer register.
///
/// # Safety
///
/// `receiver` answers `sel` with a prototype of this many word-sized
/// arguments.
unsafe fn send_words(receiver: Id, sel: Sel, args: &[usize]) -> Option<usize> {
    type W = usize;
    let entry = objc::msg::entry();
    unsafe {
        Some(match *args {
            [] => {
                std::mem::transmute::<_, unsafe extern "C" fn(Id, Sel) -> W>(entry)(receiver, sel)
            }
            [a] => std::mem::transmute::<_, unsafe extern "C" fn(Id, Sel, W) -> W>(entry)(
                receiver, sel, a,
            ),
            [a, b] => std::mem::transmute::<_, unsafe extern "C" fn(Id, Sel, W, W) -> W>(entry)(
                receiver, sel, a, b,
            ),
            [a, b, c] => std::mem::transmute::<_, unsafe extern "C" fn(Id, Sel, W, W, W) -> W>(
                entry,
            )(receiver, sel, a, b, c),
            [a, b, c, d] => {
                std::mem::transmute::<_, unsafe extern "C" fn(Id, Sel, W, W, W, W) -> W>(entry)(
                    receiver, sel, a, b, c, d,
                )
            }
            _ => return None,
        })
    }
}

/// A JSON value from the page as the argument a type code asks for.
fn argument(value: Id, code: u8) -> Result<usize, String> {
    let value = match value == foundation::value::null_object() {
        true => std::ptr::null_mut(),
        false => value,
    };
    match code {
        b'@' => Ok(value as usize),
        b'c' | b'C' | b'B' | b's' | b'S' | b'i' | b'I' | b'l' | b'L' | b'q' | b'Q' => {
            if value.is_null() {
                return Ok(0);
            }
            // SAFETY: a number from the JSON reader; `-longLongValue` answers
            // a boolean as 0 or 1.
            Ok(unsafe { send!(value, "longLongValue"; i64) } as usize)
        }
        other => Err(format!(
            "a script argument of type '{}' cannot be passed through the bridge",
            other as char
        )),
    }
}

/// A method's answer as JSON, by its return type; `None` for undefined.
fn answer(value: usize, code: u8) -> Result<Option<String>, String> {
    Ok(match code {
        b'v' => None,
        b'@' => {
            let object = value as Id;
            match object.is_null() || object == undefined() {
                true => None,
                false => {
                    Some(foundation::json::to_text(object).map_err(|why| {
                        format!("an answer the bridge cannot give the page: {why}")
                    })?)
                }
            }
        }
        b'c' | b'B' | b'C' => Some(((value & 0xff) != 0).to_string()),
        b's' => Some((value as i16).to_string()),
        b'S' => Some((value as u16).to_string()),
        b'i' | b'l' => Some((value as i32).to_string()),
        b'I' | b'L' => Some((value as u32).to_string()),
        b'q' => Some((value as i64).to_string()),
        b'Q' => Some((value as u64).to_string()),
        other => {
            return Err(format!(
                "an answer of type '{}' cannot be returned through the bridge",
                other as char
            ));
        }
    })
}

/// A page's call of an exported object's method.
fn call(object: Id, message: Id) -> Result<Option<String>, String> {
    // SAFETY: members of a parsed request; `-longLongValue` of a number.
    let target = member(message, "o");
    let id = match target.is_null() {
        true => 0,
        false => (unsafe { send!(target, "longLongValue"; i64) }) as usize,
    };
    let name = engine::text_of(member(message, "m"));
    let found = with(object, |v| {
        let exported = v.exported.iter().find(|e| e.id == id)?;
        let sel = exported.methods.iter().find(|(n, _)| *n == name)?.1;
        Some((exported.object as Id, sel))
    })
    .flatten();
    let Some((receiver, sel)) = found else {
        return Err(format!(
            "{name} is not a method of the object it was called on"
        ));
    };
    // SAFETY: a live object that declared `sel`.
    let method = unsafe { objc::object::class_of(receiver) }
        .and_then(|class| class.method(sel))
        .ok_or_else(|| format!("{name} has no implementation"))?;
    // SAFETY: a method's type encoding is a live C string.
    let types = unsafe { std::ffi::CStr::from_ptr(method.types()) }
        .to_string_lossy()
        .into_owned();
    let codes = signature(&types);
    let (ret, params) = match codes.as_slice() {
        [ret, _, _, params @ ..] => (*ret, params.to_vec()),
        _ => return Err(format!("{name} has an unreadable type encoding {types}")),
    };
    // SAFETY: the parsed arguments array, or nil.
    let given = unsafe { corefoundation::collections::array(member(message, "a") as _) }
        .map(|a| a.values.clone())
        .unwrap_or_default();
    let mut args = Vec::with_capacity(params.len());
    for (index, code) in params.iter().enumerate() {
        let value = given.get(index).copied().unwrap_or(std::ptr::null()) as Id;
        args.push(argument(value, *code)?);
    }
    if engine::tracing() {
        eprintln!("rosemary: webkit: the page calls {name} ({types})");
    }
    // SAFETY: the receiver answers `sel` with word-sized arguments, as checked.
    let value = unsafe { send_words(receiver, sel, &args) }
        .ok_or_else(|| format!("{name} takes more arguments than the bridge passes"))?;
    answer(value, ret)
}

/// The window object is new: tell the frame load delegate, which is where a
/// program puts its objects on it.
fn clear(object: Id) {
    let released = with(object, |v| std::mem::take(&mut v.exported)).unwrap_or_default();
    for exported in released {
        objc::arc::release(exported.object as Id);
    }
    let delegate = field(object, |v| v.frame_load);
    let window = field(object, |v| v.window);
    let frame = field(object, |v| v.main_frame);
    // SAFETY: the delegate answers each selector, with these prototypes.
    unsafe {
        if engine::responds(delegate, "webView:didClearWindowObject:forFrame:") {
            send!(delegate, "webView:didClearWindowObject:forFrame:",
                object => Id, window => Id, frame => Id; ());
        } else if engine::responds(delegate, "webView:windowScriptObjectAvailable:") {
            send!(delegate, "webView:windowScriptObjectAvailable:", object => Id, window => Id; ());
        }
    }
}

/// Answer one bridge request with `{r, e, s}`.
fn handle(object: Id, request: &str) -> String {
    if engine::tracing() {
        let shown: String = request.chars().take(300).collect();
        eprintln!("rosemary: webkit: bridge request {shown}");
    }
    let pool = objc::arc::pool_push();
    with(object, |v| v.depth += 1);
    let result = foundation::json::from_text(request).and_then(|message| {
        match engine::text_of(member(message, "t")).as_str() {
            "clear" => {
                clear(object);
                Ok(None)
            }
            "call" => call(object, message),
            other => Err(format!("unknown bridge request {other:?}")),
        }
    });
    let scripts = with(object, |v| {
        v.depth -= 1;
        std::mem::take(&mut v.pending)
    })
    .unwrap_or_default();
    objc::arc::pool_pop(pool);
    let mut out = String::from("{");
    match result {
        Ok(Some(value)) => out.push_str(&format!("\"r\": {value}, ")),
        Ok(None) => {}
        Err(why) => {
            eprintln!("rosemary: webkit: a page's call into native code failed: {why}");
            out.push_str(&format!("\"e\": {}, ", crate::js::quote(&why)));
        }
    }
    let scripts: Vec<String> = scripts.iter().map(|s| crate::js::quote(s)).collect();
    out.push_str(&format!("\"s\": [{}]}}", scripts.join(", ")));
    if engine::tracing() {
        let shown: String = out.chars().take(300).collect();
        eprintln!("rosemary: webkit: bridge answer {shown}");
    }
    out
}

// -- WebKitGTK signals ----------------------------------------------------------

unsafe extern "C" fn script_dialog(_gtk: Ptr, dialog: Ptr, data: Ptr) -> c_int {
    let (Some(engine), Some(object)) = (engine::get(), view_of(data)) else {
        return 0;
    };
    let api = engine.api;
    // SAFETY: the dialog this signal is about.
    let (kind, message) = unsafe {
        (
            (api.webkit_script_dialog_get_dialog_type)(dialog),
            gtk::text((api.webkit_script_dialog_get_message)(dialog)),
        )
    };
    if kind == gtk::SCRIPT_DIALOG_PROMPT && message == MAGIC {
        let request =
            unsafe { gtk::text((api.webkit_script_dialog_prompt_get_default_text)(dialog)) };
        let reply = gtk::c_string(&handle(object, &request));
        // SAFETY: a prompt dialog, and text it copies.
        unsafe { (api.webkit_script_dialog_prompt_set_text)(dialog, reply.as_ptr()) };
        return 1;
    }
    // The page's own dialogs go to the UI delegate, as WebKit1 sends them;
    // with none, WebKit1 shows nothing, and neither does this.
    let ui = field(object, |v| v.ui);
    let frame = field(object, |v| v.main_frame);
    let text = engine::string(&message);
    // SAFETY: the delegate answers each selector, with these prototypes.
    unsafe {
        match kind {
            gtk::SCRIPT_DIALOG_ALERT
                if engine::responds(
                    ui,
                    "webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:",
                ) =>
            {
                send!(ui, "webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:",
                    object => Id, text => Id, frame => Id; ());
            }
            gtk::SCRIPT_DIALOG_CONFIRM => {
                let confirmed = engine::responds(
                    ui,
                    "webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:",
                ) && send!(ui, "webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:",
                        object => Id, text => Id, frame => Id; u8)
                    != 0;
                (api.webkit_script_dialog_confirm_set_confirmed)(dialog, c_int::from(confirmed));
            }
            gtk::SCRIPT_DIALOG_PROMPT => {
                const SELECTOR: &str =
                    "webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:";
                if engine::responds(ui, SELECTOR) {
                    let default = engine::string(&gtk::text((api
                        .webkit_script_dialog_prompt_get_default_text)(
                        dialog
                    )));
                    let answer = send!(ui, "webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:",
                        object => Id, text => Id, default => Id, frame => Id; Id);
                    if !answer.is_null() {
                        let answer = gtk::c_string(&engine::text_of(answer));
                        (api.webkit_script_dialog_prompt_set_text)(dialog, answer.as_ptr());
                    }
                }
            }
            _ => eprintln!("rosemary: webkit: the page's dialog went unshown: {message}"),
        }
    }
    1
}

unsafe extern "C" fn load_changed(_gtk: Ptr, event: c_int, data: Ptr) {
    let Some(object) = view_of(data) else {
        return;
    };
    let selector = match event {
        gtk::LOAD_STARTED => "webView:didStartProvisionalLoadForFrame:",
        gtk::LOAD_COMMITTED => "webView:didCommitLoadForFrame:",
        gtk::LOAD_FINISHED => "webView:didFinishLoadForFrame:",
        _ => return,
    };
    let delegate = field(object, |v| v.frame_load);
    if !engine::responds(delegate, selector) {
        return;
    }
    let frame = field(object, |v| v.main_frame);
    // SAFETY: the delegate answers the selector, `void (id, SEL, id, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
        f(delegate, objc::sel::get(selector), object, frame);
    }
}

unsafe extern "C" fn load_failed(
    _gtk: Ptr,
    event: c_int,
    uri: *const std::ffi::c_char,
    error: *mut gtk::GError,
    data: Ptr,
) -> c_int {
    let Some(object) = view_of(data) else {
        return 0;
    };
    let selector = match event < gtk::LOAD_COMMITTED {
        true => "webView:didFailProvisionalLoadWithError:forFrame:",
        false => "webView:didFailLoadWithError:forFrame:",
    };
    let delegate = field(object, |v| v.frame_load);
    if !engine::responds(delegate, selector) {
        return 0;
    }
    // SAFETY: the error and URI this signal carries.
    let (code, message) = unsafe {
        match error.is_null() {
            true => (0, String::new()),
            false => ((*error).code, gtk::text((*error).message)),
        }
    };
    let message = format!("{message} ({})", unsafe { gtk::text(uri) });
    let error = foundation::error::make("WebKitErrorDomain", i64::from(code), &message);
    let frame = field(object, |v| v.main_frame);
    // SAFETY: the delegate answers the selector, `void (id, SEL, id, id, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id) = std::mem::transmute(objc::msg::entry());
        f(delegate, objc::sel::get(selector), object, error, frame);
    }
    0
}

unsafe extern "C" fn decide_policy(_gtk: Ptr, decision: Ptr, kind: c_int, data: Ptr) -> c_int {
    let (Some(engine), Some(object)) = (engine::get(), view_of(data)) else {
        return 0;
    };
    let selector = match kind {
        gtk::POLICY_DECISION_NAVIGATION_ACTION => {
            "webView:decidePolicyForNavigationAction:request:frame:decisionListener:"
        }
        gtk::POLICY_DECISION_NEW_WINDOW_ACTION => {
            "webView:decidePolicyForNewWindowAction:request:newFrameName:decisionListener:"
        }
        _ => return 0,
    };
    let delegate = field(object, |v| v.policy);
    if !engine::responds(delegate, selector) {
        return 0;
    }
    let api = engine.api;
    // SAFETY: a navigation decision, with its action and request.
    let (url, navigation_type) = unsafe {
        let action = (api.webkit_navigation_policy_decision_get_navigation_action)(decision);
        let request = (api.webkit_navigation_action_get_request)(action);
        (
            gtk::text((api.webkit_uri_request_get_uri)(request)),
            (api.webkit_navigation_action_get_navigation_type)(action),
        )
    };
    // WebKitGTK's navigation types are WebKit1's, in the same order.
    let url = web_view::guest_url(&url);
    let information = action_information(&url, navigation_type);
    let request = foundation::urlsession::request_for(&url);
    // SAFETY: a decision this listener now holds a reference to.
    let listener = objc::arc::autorelease(boxed::make(
        listener_class(),
        Listener {
            decision: unsafe { (api.g_object_ref)(decision) } as usize,
        },
    ));
    let third = match kind {
        gtk::POLICY_DECISION_NAVIGATION_ACTION => field(object, |v| v.main_frame),
        _ => std::ptr::null_mut(),
    };
    if engine::tracing() {
        eprintln!("rosemary: webkit: asking the policy delegate about {url}");
    }
    // SAFETY: the delegate answers the selector, `void (id, SEL, id x5)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id, Id, Id) =
            std::mem::transmute(objc::msg::entry());
        f(
            delegate,
            objc::sel::get(selector),
            object,
            information,
            request,
            third,
            listener,
        );
    }
    1
}

/// `actionInformation` for a policy decision.
fn action_information(url: &str, navigation_type: c_int) -> Id {
    let number = |value: i64| {
        let class = objc::class::lookup(c"NSNumber").expect("registered by Foundation");
        // SAFETY: `+numberWithLongLong:` is `id (id, SEL, long long)`.
        unsafe { send!(objc::class::ptr(class) as Id, "numberWithLongLong:", value => i64; Id) }
    };
    let url = objc::arc::autorelease(corefoundation::url::new(url) as Id);
    let entries = [
        (
            engine::string("WebActionNavigationTypeKey"),
            number(i64::from(navigation_type)),
        ),
        (engine::string("WebActionOriginalURLKey"), url),
        (engine::string("WebActionModifierFlagsKey"), number(0)),
        (engine::string("WebActionButtonKey"), number(-1)),
    ];
    let pairs: Vec<_> = entries
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| {
            (
                *k as corefoundation::base::CFTypeRef,
                *v as corefoundation::base::CFTypeRef,
            )
        })
        .collect();
    let dictionary = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

// -- WebView ----------------------------------------------------------------------

/// Everything a new `WebView` needs beside its `WKWebView`.
fn set_up(object: Id) {
    let frame = boxed::make(
        frame_class(),
        Frame {
            view: object as usize,
        },
    );
    let window = boxed::make(
        script_object_class(),
        ScriptObject {
            view: object as usize,
        },
    );
    VIEWS.with(|views| {
        views.borrow_mut().push(Legacy {
            object: object as usize,
            frame_load: 0,
            policy: 0,
            ui: 0,
            editing: 0,
            resource_load: 0,
            download: 0,
            main_frame: frame as usize,
            window: window as usize,
            preferences: 0,
            exported: Vec::new(),
            pending: Vec::new(),
            depth: 0,
            application_name: String::new(),
            draws_background: true,
        })
    });
    let (Some(engine), Some((id, gtk, configuration))) =
        (engine::get(), web_view::parts_of(object))
    else {
        return;
    };
    let api = engine.api;
    // SAFETY: the view's configuration and its content manager, which takes
    // its own reference to the script.
    unsafe {
        let controller = crate::configuration::controller_of(configuration);
        let manager = crate::configuration::manager(controller, engine);
        let source = gtk::c_string(HELPER);
        let script = (api.webkit_user_script_new)(
            source.as_ptr(),
            gtk::USER_CONTENT_INJECT_TOP_FRAME,
            gtk::USER_SCRIPT_INJECT_AT_DOCUMENT_START,
            std::ptr::null(),
            std::ptr::null(),
        );
        (api.webkit_user_content_manager_add_script)(manager, script);
        (api.webkit_user_script_unref)(script);
    }
    // WebKit1's defaults, which a `WKWebView` does not share: a page loaded
    // from a file may read the files beside it, as an application's bundled
    // interface does to import its own parts.
    // SAFETY: the view's own settings.
    unsafe {
        let settings = (api.webkit_web_view_get_settings)(gtk);
        (api.webkit_settings_set_allow_file_access_from_file_urls)(settings, 1);
        (api.webkit_settings_set_allow_universal_access_from_file_urls)(settings, 1);
    }
    web_view::connect(
        engine,
        gtk,
        c"script-dialog",
        script_dialog as *const (),
        id,
    );
    web_view::connect(engine, gtk, c"load-changed", load_changed as *const (), id);
    web_view::connect(engine, gtk, c"load-failed", load_failed as *const (), id);
    web_view::connect(
        engine,
        gtk,
        c"decide-policy",
        decide_policy as *const (),
        id,
    );
}

fn forget(object: Id) {
    let Some(view) = VIEWS.with(|views| {
        let mut views = views.borrow_mut();
        let index = views.iter().position(|v| v.object == object as usize)?;
        Some(views.remove(index))
    }) else {
        return;
    };
    for exported in view.exported {
        objc::arc::release(exported.object as Id);
    }
    objc::arc::release(view.main_frame as Id);
    objc::arc::release(view.window as Id);
    objc::arc::release(view.preferences as Id);
}

/// A `WKWebView` method, called directly: a guest subclass of `WebView` may
/// override the WebKit1 spelling that leads here.
fn inherited(name: &str) -> objc::class::Imp {
    objc::class::lookup(c"WKWebView")
        .and_then(|class| class.method(objc::sel::get(name)))
        .map(|method| method.implementation())
        .expect("WKWebView is installed first")
}

unsafe extern "C" fn init_with_frame_names(
    receiver: Id,
    _sel: Sel,
    frame: NSRect,
    _name: Id,
    _group: Id,
) -> Id {
    let sel = "initWithFrame:configuration:";
    // SAFETY: WKWebView's designated initializer has this prototype.
    let receiver = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect, Id) -> Id =
            std::mem::transmute(inherited(sel));
        f(receiver, objc::sel::get(sel), frame, std::ptr::null_mut())
    };
    if !receiver.is_null() {
        forget(receiver);
        set_up(receiver);
    }
    receiver
}

unsafe extern "C" fn init_with_frame(receiver: Id, _sel: Sel, frame: NSRect) -> Id {
    // SAFETY: dispatched, as WebKit1's `-initWithFrame:` sends the longer
    // initializer, which a subclass may override.
    unsafe {
        send!(receiver, "initWithFrame:frameName:groupName:",
            frame => NSRect, std::ptr::null_mut() => Id, std::ptr::null_mut() => Id; Id)
    }
}

unsafe extern "C" fn dealloc(receiver: Id, sel: Sel) {
    forget(receiver);
    // SAFETY: `-dealloc` is `void (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(inherited("dealloc"));
        f(receiver, sel)
    }
}

macro_rules! delegate_accessors {
    ($($getter:ident, $setter:ident, $field:ident;)*) => {$(
        unsafe extern "C" fn $getter(receiver: Id, _sel: Sel) -> Id {
            field(receiver, |v| v.$field)
        }
        unsafe extern "C" fn $setter(receiver: Id, _sel: Sel, delegate: Id) {
            with(receiver, |v| v.$field = delegate as usize);
        }
    )*};
}

delegate_accessors! {
    frame_load_delegate, set_frame_load_delegate, frame_load;
    policy_delegate, set_policy_delegate, policy;
    ui_delegate, set_ui_delegate, ui;
    editing_delegate, set_editing_delegate, editing;
    resource_load_delegate, set_resource_load_delegate, resource_load;
    download_delegate, set_download_delegate, download;
}

/// `-close`: the view stops loading and lets go of what it was given; it is
/// still a view until it is released, but calls nothing back again.
unsafe extern "C" fn close(receiver: Id, _sel: Sel) {
    // SAFETY: `-stopLoading` of the view itself.
    unsafe { send!(receiver, "stopLoading"; ()) };
    let released = with(receiver, |v| {
        v.frame_load = 0;
        v.policy = 0;
        v.ui = 0;
        v.editing = 0;
        v.resource_load = 0;
        v.download = 0;
        std::mem::take(&mut v.exported)
    })
    .unwrap_or_default();
    for exported in released {
        objc::arc::release(exported.object as Id);
    }
}

unsafe extern "C" fn main_frame(receiver: Id, _sel: Sel) -> Id {
    field(receiver, |v| v.main_frame)
}

unsafe extern "C" fn window_script_object(receiver: Id, _sel: Sel) -> Id {
    field(receiver, |v| v.window)
}

unsafe extern "C" fn main_frame_url(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: `-URL` of the view itself.
    let url = unsafe { send!(receiver, "URL"; Id) };
    match url.is_null() {
        true => std::ptr::null_mut(),
        false => unsafe { send!(url, "absoluteString"; Id) },
    }
}

unsafe extern "C" fn set_main_frame_url(receiver: Id, _sel: Sel, url: Id) {
    let request = foundation::urlsession::request_for(&engine::text_of(url));
    // SAFETY: `-loadRequest:` of the view itself.
    unsafe { send!(receiver, "loadRequest:", request => Id; Id) };
}

unsafe extern "C" fn string_by_evaluating(receiver: Id, _sel: Sel, script: Id) -> Id {
    let value = run_script(receiver, engine::text_of(script));
    if value.is_null() {
        return engine::string("");
    }
    // SAFETY: a string, number or JSON container from `bridge`.
    unsafe {
        if corefoundation::string::as_string(value as _).is_some() {
            value
        } else if corefoundation::collections::number(value as _).is_some() {
            send!(value, "stringValue"; Id)
        } else {
            engine::string("")
        }
    }
}

unsafe extern "C" fn set_draws_background(receiver: Id, _sel: Sel, draws: u8) {
    with(receiver, |v| v.draws_background = draws != 0);
    let class = objc::class::lookup(c"NSNumber").expect("registered by Foundation");
    // SAFETY: the WKWebView key-value path that sets the page's background.
    unsafe {
        let value = send!(objc::class::ptr(class) as Id, "numberWithBool:", draws != 0 => bool; Id);
        send!(receiver, "setValue:forKey:", value => Id, engine::string("drawsBackground") => Id; ());
    }
}

unsafe extern "C" fn draws_background(receiver: Id, _sel: Sel) -> u8 {
    with(receiver, |v| v.draws_background as u8).unwrap_or(1)
}

unsafe extern "C" fn set_application_name(receiver: Id, _sel: Sel, name: Id) {
    // A header value is printable ASCII; WebKitGTK refuses a whole agent with
    // anything else in it, such as a trademark sign in a product name.
    let name: String = engine::text_of(name)
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .collect();
    with(receiver, |v| v.application_name = name.clone());
    let (Some(engine), Some((_, gtk, _))) = (engine::get(), web_view::parts_of(receiver)) else {
        return;
    };
    let agent = match name.is_empty() {
        true => web_view::USER_AGENT.to_string(),
        false => format!("{} {name}", web_view::USER_AGENT),
    };
    if engine::tracing() {
        eprintln!("rosemary: webkit: user agent {agent}");
    }
    let agent = gtk::c_string(&agent);
    // SAFETY: the view's own settings, and a string they copy.
    unsafe {
        let settings = (engine.api.webkit_web_view_get_settings)(gtk);
        (engine.api.webkit_settings_set_user_agent)(settings, agent.as_ptr());
    }
}

unsafe extern "C" fn application_name(receiver: Id, _sel: Sel) -> Id {
    engine::string(&with(receiver, |v| v.application_name.clone()).unwrap_or_default())
}

unsafe extern "C" fn preferences(receiver: Id, _sel: Sel) -> Id {
    let set = field(receiver, |v| v.preferences);
    if !set.is_null() {
        return set;
    }
    let class = objc::class::lookup(c"WebPreferences").expect("registered by bootstrap");
    // SAFETY: `+standardPreferences` is `id (id, SEL)`.
    unsafe { send!(objc::class::ptr(class) as Id, "standardPreferences"; Id) }
}

unsafe extern "C" fn set_preferences(receiver: Id, _sel: Sel, preferences: Id) {
    let old = with(receiver, |v| {
        std::mem::replace(&mut v.preferences, objc::arc::retain(preferences) as usize)
    });
    objc::arc::release(old.unwrap_or_default() as Id);
}

unsafe extern "C" fn reload(receiver: Id, _sel: Sel, _sender: Id) {
    if let (Some(engine), Some((_, gtk, _))) = (engine::get(), web_view::parts_of(receiver)) {
        // SAFETY: the view's widget.
        unsafe { (engine.api.webkit_web_view_reload)(gtk) };
    }
}

unsafe extern "C" fn stop_loading_sender(receiver: Id, _sel: Sel, _sender: Id) {
    // SAFETY: `-stopLoading` of the view itself.
    unsafe { send!(receiver, "stopLoading"; ()) };
}

unsafe extern "C" fn main_frame_title(receiver: Id, _sel: Sel) -> Id {
    let (Some(engine), Some((_, gtk, _))) = (engine::get(), web_view::parts_of(receiver)) else {
        return engine::string("");
    };
    // SAFETY: the view's widget; the title is its own.
    engine::string(&unsafe { gtk::text((engine.api.webkit_web_view_get_title)(gtk)) })
}

unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _value: Id) {}

unsafe extern "C" fn ignored_flag(_receiver: Id, _sel: Sel, _value: u8) {}

unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> u8 {
    0
}

unsafe extern "C" fn nil(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

// -- WebFrame and WebDataSource -------------------------------------------------

fn frame_view(frame: Id) -> Id {
    view_in::<Frame>(frame, frame_class(), |f| f.view)
}

unsafe extern "C" fn frame_web_view(receiver: Id, _sel: Sel) -> Id {
    frame_view(receiver)
}

unsafe extern "C" fn frame_name(_receiver: Id, _sel: Sel) -> Id {
    engine::string("")
}

unsafe extern "C" fn frame_load_request(receiver: Id, _sel: Sel, request: Id) {
    let view = frame_view(receiver);
    if !view.is_null() {
        // SAFETY: `-loadRequest:` of the frame's view.
        unsafe { send!(view, "loadRequest:", request => Id; Id) };
    }
}

unsafe extern "C" fn frame_load_html(receiver: Id, _sel: Sel, html: Id, base: Id) {
    let view = frame_view(receiver);
    let (Some(engine), Some((_, gtk, _))) = (engine::get(), web_view::parts_of(view)) else {
        return;
    };
    let base = match base.is_null() {
        true => String::new(),
        // SAFETY: an `NSURL`.
        false => engine::text_of(unsafe { send!(base, "absoluteString"; Id) }),
    };
    let html = gtk::c_string(&engine::text_of(html));
    let base = gtk::c_string(&web_view::host_url(&base));
    // SAFETY: the view's widget and two strings it copies.
    unsafe { (engine.api.webkit_web_view_load_html)(gtk, html.as_ptr(), base.as_ptr()) };
}

unsafe extern "C" fn frame_reload(receiver: Id, sel: Sel) {
    unsafe { reload(frame_view(receiver), sel, std::ptr::null_mut()) };
}

unsafe extern "C" fn frame_stop_loading(receiver: Id, _sel: Sel) {
    let view = frame_view(receiver);
    if !view.is_null() {
        unsafe { send!(view, "stopLoading"; ()) };
    }
}

/// The frame's data source: the document it shows, or is loading.
unsafe extern "C" fn frame_data_source(receiver: Id, _sel: Sel) -> Id {
    let view = frame_view(receiver);
    let (Some(engine), Some((_, gtk, _))) = (engine::get(), web_view::parts_of(view)) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the view's widget; the URI is its own.
    let url = unsafe { gtk::text((engine.api.webkit_web_view_get_uri)(gtk)) };
    if url.is_empty() {
        return std::ptr::null_mut();
    }
    let url = web_view::guest_url(&url);
    objc::arc::autorelease(boxed::make(
        data_source_class(),
        DataSource {
            view: view as usize,
            url,
        },
    ))
}

unsafe extern "C" fn frame_window_object(receiver: Id, _sel: Sel) -> Id {
    field(frame_view(receiver), |v| v.window)
}

unsafe extern "C" fn frame_child_frames(_receiver: Id, _sel: Sel) -> Id {
    let array = corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn null_pointer(_receiver: Id, _sel: Sel) -> *mut std::ffi::c_void {
    std::ptr::null_mut()
}

fn source_of<'a>(object: Id) -> Option<&'a mut DataSource> {
    // SAFETY: a data source, or anything else, which answers `None`.
    unsafe { boxed::contents::<DataSource>(object, data_source_class()) }
}

unsafe extern "C" fn source_request(receiver: Id, _sel: Sel) -> Id {
    match source_of(receiver) {
        Some(source) => foundation::urlsession::request_for(&source.url),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn source_frame(receiver: Id, _sel: Sel) -> Id {
    let view = source_of(receiver).map_or(0, |s| s.view) as Id;
    field(view, |v| v.main_frame)
}

// -- WebScriptObject --------------------------------------------------------------

fn script_view(object: Id) -> Id {
    view_in::<ScriptObject>(object, script_object_class(), |s| s.view)
}

unsafe extern "C" fn script_set_value(receiver: Id, _sel: Sel, value: Id, key: Id) {
    let view = script_view(receiver);
    if !view.is_null() {
        set_window_value(view, &engine::text_of(key), value);
    }
}

unsafe extern "C" fn script_value(receiver: Id, _sel: Sel, key: Id) -> Id {
    let script = format!("window[{}]", crate::js::quote(&engine::text_of(key)));
    let value = run_script(script_view(receiver), script);
    match value.is_null() {
        true => undefined(),
        false => value,
    }
}

unsafe extern "C" fn script_remove_key(receiver: Id, _sel: Sel, key: Id) {
    let script = format!(
        "delete window[{}];",
        crate::js::quote(&engine::text_of(key))
    );
    run_script(script_view(receiver), script);
}

unsafe extern "C" fn script_call(receiver: Id, _sel: Sel, name: Id, arguments: Id) -> Id {
    let name = engine::text_of(name);
    let arguments = match arguments.is_null() {
        true => "[]".to_string(),
        false => match foundation::json::to_text(arguments) {
            Ok(text) => text,
            Err(why) => {
                eprintln!(
                    "rosemary: webkit: {name} was not called: its arguments are not data ({why})"
                );
                return undefined();
            }
        },
    };
    let script = format!(
        "window[{name}].apply(window, {arguments})",
        name = crate::js::quote(&name)
    );
    let value = run_script(script_view(receiver), script);
    match value.is_null() {
        true => undefined(),
        false => value,
    }
}

unsafe extern "C" fn script_evaluate(receiver: Id, _sel: Sel, script: Id) -> Id {
    let value = run_script(script_view(receiver), engine::text_of(script));
    match value.is_null() {
        true => undefined(),
        false => value,
    }
}

unsafe extern "C" fn script_string(_receiver: Id, _sel: Sel) -> Id {
    engine::string("[object Window]")
}

unsafe extern "C" fn throw_exception(_class: Id, _sel: Sel, message: Id) -> u8 {
    eprintln!(
        "rosemary: webkit: a native method threw into the page: {}",
        engine::text_of(message)
    );
    0
}

unsafe extern "C" fn web_undefined(_class: Id, _sel: Sel) -> Id {
    undefined()
}

unsafe extern "C" fn undefined_description(_receiver: Id, _sel: Sel) -> Id {
    engine::string("undefined")
}

// -- The policy listener ----------------------------------------------------------

fn decide(receiver: Id, answer: fn(&gtk::Api, Ptr)) {
    // SAFETY: a listener, or anything else, which answers `None`.
    let Some(listener) = (unsafe { boxed::contents::<Listener>(receiver, listener_class()) })
    else {
        return;
    };
    let decision = std::mem::take(&mut listener.decision);
    if decision == 0 {
        return;
    }
    if let Some(engine) = engine::get() {
        answer(engine.api, decision as Ptr);
        engine::unref(engine.api, decision);
    }
}

unsafe extern "C" fn listener_use(receiver: Id, _sel: Sel) {
    // SAFETY: a decision this listener held a reference to.
    decide(receiver, |api, d| unsafe {
        (api.webkit_policy_decision_use)(d)
    });
}

unsafe extern "C" fn listener_ignore(receiver: Id, _sel: Sel) {
    decide(receiver, |api, d| unsafe {
        (api.webkit_policy_decision_ignore)(d)
    });
}

unsafe extern "C" fn listener_download(receiver: Id, _sel: Sel) {
    eprintln!(
        "rosemary: webkit: a policy delegate asked for a download, which is not supported; ignored"
    );
    decide(receiver, |api, d| unsafe {
        (api.webkit_policy_decision_ignore)(d)
    });
}

/// `+registerURLSchemeAsLocal:`. Local-file access is not restricted by scheme
/// in this runtime's legacy web view, so there is nothing to record.
unsafe extern "C" fn register_scheme_as_local(_class: Id, _sel: Sel, _scheme: Id) {}

pub fn install() {
    let wk = objc::class::lookup(c"WKWebView").expect("installed before this");
    let web_view = corefoundation::base::abstract_class(c"WebView", Some(wk));
    crate::methods!(web_view, {
        "initWithFrame:frameName:groupName:" => init_with_frame_names, c"@64@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48@56";
        "initWithFrame:" => init_with_frame, c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "dealloc" => dealloc, c"v16@0:8";
        "mainFrame" => main_frame, c"@16@0:8";
        "close" => close, c"v16@0:8";
        "windowScriptObject" => window_script_object, c"@16@0:8";
        "mainFrameURL" => main_frame_url, c"@16@0:8";
        "setMainFrameURL:" => set_main_frame_url, c"v24@0:8@16";
        "mainFrameTitle" => main_frame_title, c"@16@0:8";
        "stringByEvaluatingJavaScriptFromString:" => string_by_evaluating, c"@24@0:8@16";
        "frameLoadDelegate" => frame_load_delegate, c"@16@0:8";
        "setFrameLoadDelegate:" => set_frame_load_delegate, c"v24@0:8@16";
        "policyDelegate" => policy_delegate, c"@16@0:8";
        "setPolicyDelegate:" => set_policy_delegate, c"v24@0:8@16";
        "UIDelegate" => ui_delegate, c"@16@0:8";
        "setUIDelegate:" => set_ui_delegate, c"v24@0:8@16";
        "editingDelegate" => editing_delegate, c"@16@0:8";
        "setEditingDelegate:" => set_editing_delegate, c"v24@0:8@16";
        "resourceLoadDelegate" => resource_load_delegate, c"@16@0:8";
        "setResourceLoadDelegate:" => set_resource_load_delegate, c"v24@0:8@16";
        "downloadDelegate" => download_delegate, c"@16@0:8";
        "setDownloadDelegate:" => set_download_delegate, c"v24@0:8@16";
        "setDrawsBackground:" => set_draws_background, c"v20@0:8c16";
        "drawsBackground" => draws_background, c"c16@0:8";
        "setApplicationNameForUserAgent:" => set_application_name, c"v24@0:8@16";
        "applicationNameForUserAgent" => application_name, c"@16@0:8";
        "preferences" => preferences, c"@16@0:8";
        "setPreferences:" => set_preferences, c"v24@0:8@16";
        "reload:" => reload, c"v24@0:8@16";
        "stopLoading:" => stop_loading_sender, c"v24@0:8@16";
        "setShouldUpdateWhileOffscreen:" => ignored_flag, c"v20@0:8c16";
        "setMaintainsBackForwardList:" => ignored_flag, c"v20@0:8c16";
        "setContinuousSpellCheckingEnabled:" => ignored_flag, c"v20@0:8c16";
        "setGroupName:" => ignored_object, c"v24@0:8@16";
        "setHostWindow:" => ignored_object, c"v24@0:8@16";
        "setCustomUserAgent:" => set_application_name, c"v24@0:8@16";
        "isLoading" => no, c"c16@0:8";
    });

    crate::methods!(frame_class(), {
        "webView" => frame_web_view, c"@16@0:8";
        "name" => frame_name, c"@16@0:8";
        "loadRequest:" => frame_load_request, c"v24@0:8@16";
        "loadHTMLString:baseURL:" => frame_load_html, c"v32@0:8@16@24";
        "reload" => frame_reload, c"v16@0:8";
        "stopLoading" => frame_stop_loading, c"v16@0:8";
        "dataSource" => frame_data_source, c"@16@0:8";
        "provisionalDataSource" => frame_data_source, c"@16@0:8";
        "windowObject" => frame_window_object, c"@16@0:8";
        "parentFrame" => nil, c"@16@0:8";
        "childFrames" => frame_child_frames, c"@16@0:8";
        "DOMDocument" => nil, c"@16@0:8";
        "frameElement" => nil, c"@16@0:8";
        "globalContext" => null_pointer, c"^v16@0:8";
    });
    crate::methods!(data_source_class(), {
        "request" => source_request, c"@16@0:8";
        "initialRequest" => source_request, c"@16@0:8";
        "response" => nil, c"@16@0:8";
        "webFrame" => source_frame, c"@16@0:8";
        "isLoading" => no, c"c16@0:8";
    });
    let script = script_object_class();
    crate::methods!(script, {
        "setValue:forKey:" => script_set_value, c"v32@0:8@16@24";
        "valueForKey:" => script_value, c"@24@0:8@16";
        "removeWebScriptKey:" => script_remove_key, c"v24@0:8@16";
        "callWebScriptMethod:withArguments:" => script_call, c"@32@0:8@16@24";
        "evaluateWebScript:" => script_evaluate, c"@24@0:8@16";
        "stringRepresentation" => script_string, c"@16@0:8";
        "JSObject" => null_pointer, c"^v16@0:8";
    });
    crate::class_methods!(script, {
        "throwException:" => throw_exception, c"c24@0:8@16";
    });
    crate::class_methods!(web_view, {
        "registerURLSchemeAsLocal:" => register_scheme_as_local, c"v24@0:8@16";
    });
    let undefined = corefoundation::base::abstract_class(c"WebUndefined", None);
    crate::class_methods!(undefined, {
        "undefined" => web_undefined, c"@16@0:8";
    });
    crate::methods!(undefined, {
        "description" => undefined_description, c"@16@0:8";
    });
    crate::methods!(listener_class(), {
        "use" => listener_use, c"v16@0:8";
        "ignore" => listener_ignore, c"v16@0:8";
        "download" => listener_download, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_lists_the_return_and_each_argument() {
        assert_eq!(signature("@32@0:8@16@24"), b"@@:@@".to_vec());
        assert_eq!(signature("c24@0:8r*16"), b"c@:*".to_vec());
        assert_eq!(signature("v56@0:8@16{CGPoint=dd}24Q40"), b"v@:@{Q".to_vec());
        assert_eq!(signature("@24@0:8@\"NSString\"16"), b"@@:@".to_vec());
    }
}
