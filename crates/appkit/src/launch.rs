//! LaunchServices' rule that an application runs once.
//!
//! Launching a bundle that already runs - `open`, the Finder, the Dock,
//! `-[NSWorkspace launchApplicationAtURL:...]` - starts nothing on a Mac: the
//! running instance is activated and handed the launch as an Apple Event,
//! `odoc` for documents, `GURL` for a URL, and `rapp` when there is neither.
//! The arguments of such a launch are dropped. Two copies of one application
//! at different paths are two applications, and `open -n` asks for another
//! instance; an application whose `Info.plist` sets
//! `LSMultipleInstancesProhibited` is one per bundle identifier, and `-n` does
//! not get past it. Running the bundle's executable directly is not a launch
//! LaunchServices is asked about, and always starts another process.
//!
//! The prefix's process server keeps the table of running applications, and
//! a launch is handed over through `darwin::appmailbox`. The events arrive
//! once the application has finished launching, as a Mac queues them.

use std::path::Path;

use foundation::plist::Value;
use objc::object::Id;
use objc::sel::Sel;
use server::protocol::Application;

pub use server::protocol::Launch;

/// What became of a launch.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// This process is the application: run it.
    Run,
    /// The instance running as this pid was handed the launch.
    HandedTo(i32),
}

/// The application a bundle at the host path `bundle` is, or `None` for
/// something that is not a bundle.
fn describe(bundle: &Path) -> Option<Application> {
    let layout = foundation::bundle::Layout::at(bundle)?;
    // One application however its path is spelled.
    let canonical = std::fs::canonicalize(&layout.root).unwrap_or_else(|_| layout.root.clone());
    let guest = prefix::ns::to_guest(&canonical).unwrap_or(canonical);
    let name = layout
        .display_name()
        .map(str::to_owned)
        .or_else(|| Some(layout.root.file_stem()?.to_string_lossy().into_owned()));
    Some(Application {
        pid: 0,
        bundle: guest.to_string_lossy().into_owned(),
        identifier: layout.identifier().map(str::to_owned),
        name,
        prohibited: layout
            .info
            .get("LSMultipleInstancesProhibited")
            .is_some_and(truthy),
    })
}

/// A property list Boolean as `CFBundle` reads one from an `Info.plist`,
/// which accepts the spellings people write by hand as well.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(flag) => *flag,
        Value::Integer(n) => *n != 0,
        Value::String(text) => {
            text.eq_ignore_ascii_case("yes")
                || text.eq_ignore_ascii_case("true")
                || text.trim().parse::<i64>().is_ok_and(|n| n != 0)
        }
        _ => false,
    }
}

/// Make this process the application bundled at the host path `bundle`, or
/// hand `urls` to the instance that already is. Called by the runtime before
/// the program loads, for a launch of a bundle and for its executable run
/// directly ([`Launch::Direct`]), which is listed and never handed over.
///
/// Without a process server every launch is a new instance, as it was before
/// there was one to ask.
pub fn launch(bundle: &Path, launch: Launch, urls: &[String]) -> Result<Outcome, String> {
    let _host = darwin::syscall::ScopedHostSyscallContext::new();
    if !server::client::configured() {
        return Ok(Outcome::Run);
    }
    let Some(application) = describe(bundle) else {
        return Ok(Outcome::Run);
    };
    // Reachable before claimed, so whoever the server sends here can write.
    darwin::appmailbox::open();
    let Some(Some(running)) = server::client::launch(application, launch) else {
        return Ok(Outcome::Run);
    };
    darwin::appmailbox::close();
    darwin::appmailbox::send(running.pid, &encode(&Message::Open(urls.to_vec()))).map_err(|e| {
        format!(
            "{} is running as pid {}, which could not be handed the launch: {e}",
            running.bundle, running.pid
        )
    })?;
    Ok(Outcome::HandedTo(running.pid))
}

/// Whether a launch argument is a URL for the application to open, rather
/// than an argument for its `main`.
pub fn is_launch_url(argument: &str) -> bool {
    let Some((scheme, rest)) = argument.split_once(':') else {
        return false;
    };
    // Two characters at least, so a lone drive letter or a stray `x:` is not
    // mistaken for one, and a body.
    scheme.len() >= 2
        && !rest.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// The instance a launch of the host path `bundle` would be handed to.
pub(crate) fn running_instance(bundle: &Path, launch: Launch) -> Option<Application> {
    let _host = darwin::syscall::ScopedHostSyscallContext::new();
    let wanted = describe(bundle)?;
    server::client::applications(None)?
        .into_iter()
        .find(|running| running.answers(&wanted, launch))
}

/// Hand `urls` to the running application `pid`: `rapp` for none.
pub(crate) fn hand_over(pid: i32, urls: &[String]) -> bool {
    darwin::appmailbox::send(pid, &encode(&Message::Open(urls.to_vec()))).is_ok()
}

/// Bring the running application `pid` forward.
pub(crate) fn activate(pid: i32) -> bool {
    darwin::appmailbox::send(pid, &encode(&Message::Activate)).is_ok()
}

/// Open `urls` in this application, as LaunchServices does when the handler
/// it found is the application asking. False when there is no mailbox to go
/// through, and the caller opens them some other way.
pub(crate) fn open_in_self(urls: &[String]) -> bool {
    darwin::appmailbox::open() && hand_over(std::process::id() as i32, urls)
}

#[derive(Debug, PartialEq)]
enum Message {
    /// A launch: URLs and documents, or nothing for a plain reopen.
    Open(Vec<String>),
    /// `-[NSRunningApplication activateWithOptions:]`.
    Activate,
}

fn encode(message: &Message) -> Vec<u8> {
    let mut entries = std::collections::BTreeMap::new();
    match message {
        Message::Open(urls) => {
            entries.insert("event".to_owned(), Value::String("open".to_owned()));
            entries.insert(
                "urls".to_owned(),
                Value::Array(urls.iter().cloned().map(Value::String).collect()),
            );
        }
        Message::Activate => {
            entries.insert("event".to_owned(), Value::String("activate".to_owned()));
        }
    }
    foundation::plist::write_xml(&Value::Dictionary(entries))
}

fn decode(bytes: &[u8]) -> Option<Message> {
    let value = foundation::plist::parse(bytes).ok()?;
    match value.get("event")?.as_str()? {
        "open" => Some(Message::Open(
            value
                .get("urls")
                .and_then(Value::as_array)
                .unwrap_or_default()
                .iter()
                .filter_map(|url| url.as_str().map(str::to_owned))
                .collect(),
        )),
        "activate" => Some(Message::Activate),
        _ => None,
    }
}

/// Start taking what is handed to this application, those that arrived while
/// it launched first. Called at the end of `-finishLaunching`, which is when a
/// Mac starts delivering an application's queued events.
pub(crate) fn finished_launching() {
    darwin::appmailbox::consume(arrived);
}

/// A message from the mailbox's thread: handled on the main thread.
fn arrived(message: Vec<u8>) {
    let context = Box::into_raw(Box::new(message)) as u64;
    let function = handle_on_main as *const () as u64;
    // SAFETY: the box is consumed exactly once by `handle_on_main`.
    unsafe {
        darwin::dispatch::queue::async_f(darwin::dispatch::queue::main_queue(), context, function)
    };
}

unsafe extern "C" fn handle_on_main(context: *mut std::ffi::c_void) {
    // SAFETY: the box `arrived` made.
    let message = *unsafe { Box::from_raw(context.cast::<Vec<u8>>()) };
    let pool = objc::arc::pool_push();
    let message = decode(&message);
    if std::env::var_os(foundation::notification::TRACE_ENV).is_some() {
        eprintln!("rosemary: appkit: handed over: {message:?}");
    }
    match message {
        Some(Message::Open(urls)) => {
            open(&urls);
            bring_forward();
        }
        Some(Message::Activate) => bring_forward(),
        None => {}
    }
    objc::arc::pool_pop(pool);
}

fn is_file_url(url: &str) -> bool {
    url.get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
}

/// The events of a launch: one `odoc` for every document, a `GURL` for each
/// URL, and `rapp` for a launch with neither.
fn open(urls: &[String]) {
    use foundation::appleevent as ae;
    if urls.is_empty() {
        let event = ae::event(ae::CORE_EVENT_CLASS, ae::REOPEN_APPLICATION);
        dispatch(event, ae::CORE_EVENT_CLASS, ae::REOPEN_APPLICATION, || {
            reopen()
        });
        return;
    }
    let (files, others): (Vec<&String>, Vec<&String>) = urls.iter().partition(|u| is_file_url(u));
    if !files.is_empty() {
        let items: Vec<Id> = files.iter().map(|url| ae::file_url(url)).collect();
        let event = ae::event(ae::CORE_EVENT_CLASS, ae::OPEN_DOCUMENTS);
        // SAFETY: descriptors made just now.
        unsafe { ae::set_param(event, ae::KEY_DIRECT_OBJECT, ae::list(&items)) };
        let files: Vec<String> = files.into_iter().cloned().collect();
        dispatch(event, ae::CORE_EVENT_CLASS, ae::OPEN_DOCUMENTS, || {
            open_documents(&files)
        });
    }
    for url in others {
        let event = ae::event(ae::INTERNET_EVENT_CLASS, ae::GET_URL);
        // SAFETY: descriptors made just now.
        unsafe { ae::set_param(event, ae::KEY_DIRECT_OBJECT, ae::string(url)) };
        dispatch(event, ae::INTERNET_EVENT_CLASS, ae::GET_URL, || {
            // SAFETY: the shared application and its delegate, or nil.
            unsafe { crate::application::open_urls(std::slice::from_ref(url)) };
        });
    }
}

/// To the program's own handler when it installed one, and otherwise to
/// AppKit's, which is the one a program that installed none gets on a Mac.
fn dispatch(event: Id, event_class: u32, event_id: u32, appkit: impl FnOnce()) {
    match foundation::appleevent::has_handler(event_class, event_id) {
        true => {
            // SAFETY: an event `foundation::appleevent` made.
            let _ = unsafe { foundation::appleevent::dispatch(event) };
        }
        false => appkit(),
    }
}

/// AppKit's `odoc`: `-application:openURLs:` where the delegate has it, and
/// otherwise the paths, to `-application:openFiles:` or one at a time to
/// `-application:openFile:`.
fn open_documents(urls: &[String]) {
    // SAFETY: the shared application and its delegate, or nil.
    if unsafe { crate::application::open_urls(urls) } {
        return;
    }
    let paths: Vec<String> = urls
        .iter()
        .filter_map(|url| {
            let object = corefoundation::url::new(url);
            // SAFETY: a URL made just now, released after reading it.
            let path = unsafe { corefoundation::url::as_url(object) }.and_then(|u| u.file_path());
            unsafe { corefoundation::api::CFRelease(object) };
            path
        })
        .collect();
    let app = crate::application::shared();
    let delegate = crate::application::shared_delegate();
    let strings: Vec<Id> = paths
        .iter()
        .map(|path| foundation::string::from_str(path) as Id)
        .collect();
    if let Some(open_files) = method(delegate, "application:openFiles:") {
        let array = corefoundation::collections::new_array(
            &strings
                .iter()
                .map(|s| *s as corefoundation::base::CFTypeRef)
                .collect::<Vec<_>>(),
            corefoundation::callbacks::Policy::from_type_callbacks(),
            false,
        );
        // SAFETY: `void (id, SEL, NSApplication *, NSArray<NSString *> *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(open_files);
            f(
                delegate,
                objc::sel::get("application:openFiles:"),
                app,
                array as Id,
            );
            corefoundation::api::CFRelease(array);
        }
    } else if let Some(open_file) = method(delegate, "application:openFile:") {
        for string in &strings {
            // SAFETY: `BOOL (id, SEL, NSApplication *, NSString *)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> u8 = std::mem::transmute(open_file);
                f(
                    delegate,
                    objc::sel::get("application:openFile:"),
                    app,
                    *string,
                );
            }
        }
    }
    for string in strings {
        // SAFETY: strings made above.
        unsafe { corefoundation::api::CFRelease(string as _) };
    }
}

/// AppKit's `rapp`: ask the delegate, and then bring back a minimized window,
/// or open an untitled document when there is no window to show at all.
fn reopen() {
    let app = crate::application::shared();
    let delegate = crate::application::shared_delegate();
    let visible = !shown_windows().is_empty();
    let selector = "applicationShouldHandleReopen:hasVisibleWindows:";
    let proceed = match method(delegate, selector) {
        // SAFETY: `BOOL (id, SEL, NSApplication *, BOOL)`.
        Some(should) => unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, u8) -> u8 = std::mem::transmute(should);
            f(delegate, objc::sel::get(selector), app, u8::from(visible)) != 0
        },
        None => true,
    };
    if !proceed || visible {
        return;
    }
    if let Some(window) = crate::window::all().into_iter().find(|w| miniaturized(*w)) {
        // SAFETY: `-deminiaturize:` is `void (id, SEL, id)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(
                window,
                objc::sel::get("deminiaturize:"),
                std::ptr::null_mut(),
            );
        }
        return;
    }
    let Some(open_untitled) = method(delegate, "applicationOpenUntitledFile:") else {
        return;
    };
    let wanted = match method(delegate, "applicationShouldOpenUntitledFile:") {
        // SAFETY: `BOOL (id, SEL, NSApplication *)`.
        Some(should) => unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 = std::mem::transmute(should);
            f(
                delegate,
                objc::sel::get("applicationShouldOpenUntitledFile:"),
                app,
            ) != 0
        },
        None => true,
    };
    if wanted {
        // SAFETY: `BOOL (id, SEL, NSApplication *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 = std::mem::transmute(open_untitled);
            f(
                delegate,
                objc::sel::get("applicationOpenUntitledFile:"),
                app,
            );
        }
    }
}

fn miniaturized(window: Id) -> bool {
    // SAFETY: `-isMiniaturized` is `BOOL (id, SEL)`, sent to a live window.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(window, objc::sel::get("isMiniaturized")) != 0
    }
}

/// The windows on screen, frontmost first; a minimized window is not visible.
fn shown_windows() -> Vec<Id> {
    crate::window::front_to_back()
        .into_iter()
        .filter(|w| !miniaturized(*w))
        .collect()
}

/// Activation: the application's windows in front of everybody else's, in
/// their own order.
///
/// FIXME (ACCURACY): a Wayland compositor that prevents focus stealing may
/// only mark the window as wanting attention, since the launcher's activation
/// token is not carried over.
fn bring_forward() {
    for window in shown_windows().into_iter().rev() {
        // SAFETY: a live window.
        let sdl = unsafe { crate::window::sdl_window(window) };
        if !sdl.is_null() {
            sdl3::video::raise_window(sdl);
        }
    }
}

/// A method `target` implements, or `None` for it or a nil target.
fn method(target: Id, selector: &str) -> Option<*const std::ffi::c_void> {
    if target.is_null() {
        return None;
    }
    // SAFETY: a live object.
    let class = unsafe { objc::object::class_of(target) }?;
    class
        .method(objc::sel::get(selector))
        .map(|m| m.implementation())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handed_over_launch_round_trips() {
        let urls = vec!["file:///Users/me/a.txt".to_owned(), "x-app:go".to_owned()];
        assert_eq!(
            decode(&encode(&Message::Open(urls.clone()))),
            Some(Message::Open(urls))
        );
        assert_eq!(
            decode(&encode(&Message::Open(Vec::new()))),
            Some(Message::Open(Vec::new()))
        );
        assert_eq!(decode(&encode(&Message::Activate)), Some(Message::Activate));
        assert_eq!(decode(b"not a plist"), None);
    }

    #[test]
    fn documents_are_told_apart_from_urls_by_scheme() {
        assert!(is_file_url("file:///tmp/a"));
        assert!(is_file_url("FILE:///tmp/a"));
        assert!(!is_file_url("roblox://experiences"));
        assert!(!is_file_url("fil"));
    }

    fn bundle(info: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "rosemary-launch-{}-{}/A.app",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("Contents/MacOS")).unwrap();
        std::fs::write(root.join("Contents/MacOS/A"), b"").unwrap();
        std::fs::write(
            root.join("Contents/Info.plist"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                 <key>CFBundleExecutable</key><string>A</string>\
                 <key>CFBundleIdentifier</key><string>com.example.a</string>{info}\
                 </dict></plist>"
            ),
        )
        .unwrap();
        root
    }

    #[test]
    fn a_bundle_says_whether_it_prohibits_multiple_instances() {
        let plain = describe(&bundle("")).unwrap();
        assert_eq!(plain.identifier.as_deref(), Some("com.example.a"));
        assert!(!plain.prohibited);
        let prohibited =
            describe(&bundle("<key>LSMultipleInstancesProhibited</key><true/>")).unwrap();
        assert!(prohibited.prohibited);
        let spelled = describe(&bundle(
            "<key>LSMultipleInstancesProhibited</key><string>YES</string>",
        ))
        .unwrap();
        assert!(spelled.prohibited);
    }
}
