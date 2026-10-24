use std::collections::VecDeque;
use std::ffi::{c_char, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Application {
    isa: *const c_void,
    appearance: Id,
    delegate: Id,
    main_menu: Id,
    icon: Id,
    running: usize,
    /// The three menus a program hands AppKit to fill in: the Services, Windows
    /// and Help menus. Kept and not populated - there is no Services registry and
    /// no window list to build a Windows menu from - because a program reads back
    /// the menu it set and adds its own items to it.
    services_menu: Id,
    windows_menu: Id,
    help_menu: Id,
    /// `-dockMenu`, which a program sets and AppKit would ask its delegate for.
    dock_menu: Id,
    /// The modal window of the innermost session, or nil outside one. See
    /// [`run_modal_for_window`].
    modal_window: Id,
    /// How deep the modal nesting is, so `-stopModal` ends one session rather
    /// than all of them.
    modal_depth: usize,
    /// What the innermost stopped session answers: `-stopModalWithCode:`'s code.
    modal_code: isize,
}

pub const INSTANCE_SIZE: usize = size_of::<Application>();

pub const WILL_FINISH_LAUNCHING: &str = "NSApplicationWillFinishLaunchingNotification";
pub const DID_FINISH_LAUNCHING: &str = "NSApplicationDidFinishLaunchingNotification";
pub const WILL_TERMINATE: &str = "NSApplicationWillTerminateNotification";
pub const WILL_BECOME_ACTIVE: &str = "NSApplicationWillBecomeActiveNotification";
pub const DID_BECOME_ACTIVE: &str = "NSApplicationDidBecomeActiveNotification";
pub const WILL_RESIGN_ACTIVE: &str = "NSApplicationWillResignActiveNotification";
/// Posted when a display is added, removed or resized.
pub const DID_CHANGE_SCREEN_PARAMETERS: &str = "NSApplicationDidChangeScreenParametersNotification";

static SHARED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Events posted through `-postEvent:atStart:`. AppKit owns a reference while
/// an event is queued; the application object itself is process-global, so the
/// queue is too.
static POSTED_EVENTS: Mutex<VecDeque<usize>> = Mutex::new(VecDeque::new());

/// The event inside `-sendEvent:`, for `-currentEvent`.
static CURRENT_EVENT: AtomicUsize = AtomicUsize::new(0);

/// The main run loop's source for posted events, which exists to end its wait.
///
/// A program that runs work on the main thread by posting an event and waiting
/// for its `-sendEvent:` - Feral's ports do, several times a frame - pays for
/// every round trip whatever the loop's slice is. On a Mac the event queue is a
/// run loop source, so posting from another thread wakes the loop at once.
static POSTED_SOURCE: std::sync::OnceLock<std::sync::Arc<corefoundation::runloop::Source>> =
    std::sync::OnceLock::new();

/// The main run loop's source for the main dispatch queue, which also exists
/// only to end its wait: `turn` drains the queue once the loop returns.
static MAIN_QUEUE_SOURCE: std::sync::OnceLock<std::sync::Arc<corefoundation::runloop::Source>> =
    std::sync::OnceLock::new();

fn main_queue_submitted() {
    if let Some(source) = MAIN_QUEUE_SOURCE.get() {
        source.signal();
        corefoundation::runloop::main().wake();
    }
}

/// Nothing to do: the events are sent by `turn`, or taken by
/// `-nextEventMatchingMask:`, once the loop returns for this source.
unsafe extern "C" fn posted_source_fired(_info: *mut c_void) {}

fn posted_events() -> std::sync::MutexGuard<'static, VecDeque<usize>> {
    POSTED_EVENTS.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn shared() -> Id {
    SHARED.load(std::sync::atomic::Ordering::Acquire) as Id
}

/// `+sharedApplication`, on whichever class it was sent to.
///
/// The receiver rather than `NSApplication`: `NSApplicationMain` sends it to the
/// bundle's `NSPrincipalClass`, and a program whose principal class is
/// `YYApplication` expects `NSApp` to be a `YYApplication` - its own overrides
/// are the reason it declared one.
unsafe extern "C" fn shared_application(class: Id, _sel: Sel) -> Id {
    let existing = shared();
    if !existing.is_null() {
        return existing;
    }
    // SAFETY: the receiver of a class method is a class this runtime knows.
    let Some(class) = (unsafe { objc::class::from_ptr(class as *const Class) }) else {
        return std::ptr::null_mut();
    };
    let app = objc::object::create_instance(class, 0);
    // Retained forever: `NSApp` is a process-lifetime global, and an application
    // a guest can release is one it can make dangle.
    objc::arc::make_immortal(app);
    SHARED.store(app as usize, std::sync::atomic::Ordering::Release);
    // SAFETY: the cell is this crate's own, and the guest reads it as `NSApp`.
    unsafe { *(crate::ns_app_cell() as *mut u64) = app as usize as u64 };
    register_application_fonts();
    app
}

/// The fonts `ATSApplicationFontsPath` names, which a Mac makes available to
/// the application before its main nib is loaded: a file or a directory of
/// them, relative to the bundle's resources.
fn register_application_fonts() {
    let Some(bundle) = foundation::bundle::main() else {
        return;
    };
    let Some(relative) = bundle.info_string("ATSApplicationFontsPath") else {
        return;
    };
    let path = bundle.resources.join(relative.trim_start_matches('/'));
    let files: Vec<std::path::PathBuf> = match std::fs::read_dir(&path) {
        Ok(entries) => entries.flatten().map(|e| e.path()).collect(),
        Err(_) => vec![path],
    };
    for file in files.iter().filter(|f| f.is_file()) {
        crate::fonts::register_file(&file.to_string_lossy());
    }
}

unsafe extern "C" fn delegate(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an application.
    unsafe { (*(receiver as *const Application)).delegate }
}

/// `-setDelegate:`, which does not retain - Apple's rule, and the reason a
/// delegate outliving its application is the program's business rather than
/// this one's.
unsafe extern "C" fn set_delegate(receiver: Id, _sel: Sel, value: Id) {
    // SAFETY: an application.
    unsafe { (*(receiver as *mut Application)).delegate = value };
}

unsafe extern "C" fn main_menu(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an application.
    unsafe { (*(receiver as *const Application)).main_menu }
}

unsafe extern "C" fn set_main_menu(receiver: Id, _sel: Sel, value: Id) {
    // SAFETY: an application.
    let slot = unsafe { &mut (*(receiver as *mut Application)).main_menu };
    // Toolkits may reassign the same menu when a popup takes or returns focus.
    // Reassignment is not a replacement: closing menu tracking here would
    // dismiss the popup before the user can choose an item.
    if *slot == value {
        return;
    }
    let old = std::mem::replace(slot, objc::arc::retain(value));
    objc::arc::release(old);
    // The menu bar shows the menus of the one it is given now.
    crate::decor::close_menus();
}

/// The icon the program set on `NSApp`, or nil when it set none or there is no
/// `NSApp` yet.
pub(crate) fn icon_image() -> Id {
    match shared() {
        app if app.is_null() => app,
        // SAFETY: the shared application.
        app => unsafe { (*(app as *const Application)).icon },
    }
}

/// `-applicationIconImage`.
///
/// What the program itself set, and nil otherwise. Apple would answer with the
/// bundle's icon here; this does not, because the icon is decoded straight into
/// a window rather than into an `NSImage`, and an image made only to answer this
/// would be one nothing else in the runtime can draw.
unsafe extern "C" fn application_icon_image(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an application.
    unsafe { (*(receiver as *const Application)).icon }
}

/// `-setApplicationIconImage:`, which is the Dock tile on macOS and the window
/// icon elsewhere - see [`crate::icon`].
unsafe extern "C" fn set_application_icon_image(receiver: Id, _sel: Sel, image: Id) {
    // SAFETY: an application.
    unsafe { (*(receiver as *mut Application)).icon = objc::arc::retain(image) };
    crate::icon::replace(crate::image::bitmap(image));
}

unsafe extern "C" fn is_running(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: an application.
    unsafe { ((*(receiver as *const Application)).running != 0) as u8 }
}

/// `-isActive`: whether this is the frontmost application.
///
/// The same state as [`is_running`], and deliberately: nothing here ever
/// resigns active, because there is no other application for this one to be
/// behind. A game asks this to decide whether to keep rendering, so answering
/// NO is not the cautious choice - it pauses the program for good.
unsafe extern "C" fn is_active(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: an application.
    unsafe { ((*(receiver as *const Application)).running != 0) as u8 }
}

/// `-finishLaunching`: the two notifications, in the order Apple sends them,
/// with the launch Apple Event between them - and then the pair that says the
/// application became active, which a Mac sends as the launched application
/// comes to the front. [`is_active`] has answered YES all along; a program that
/// tracks focus from the notifications instead - Unity does, and drops keyboard
/// input until it hears - needs them said once.
///
/// The delegate is sent each directly as well as through the notification
/// center, because AppKit registers a delegate for the notifications it declares
/// the moment it is set, and a program that only implements the delegate method
/// never registers as an observer of anything.
///
/// `aevt`/`oapp` is raised where Apple raises it - after the program has had
/// `applicationWillFinishLaunching:` to install its handlers, and before it is
/// told launching finished. It is the one Apple Event every application on a
/// real Mac receives, and a program that opens its untitled document from the
/// handler opens nothing without it.
unsafe extern "C" fn finish_launching(receiver: Id, _sel: Sel) {
    // SAFETY: an application.
    let delegate = unsafe { (*(receiver as *const Application)).delegate };
    for (name, selector) in [
        (WILL_FINISH_LAUNCHING, "applicationWillFinishLaunching:"),
        (DID_FINISH_LAUNCHING, "applicationDidFinishLaunching:"),
        (WILL_BECOME_ACTIVE, "applicationWillBecomeActive:"),
        (DID_BECOME_ACTIVE, "applicationDidBecomeActive:"),
    ] {
        if name == DID_FINISH_LAUNCHING {
            // SAFETY: an application, and a delegate the program set.
            unsafe { open_launch_urls(receiver, delegate) };
            // A launch with documents opens them instead of announcing a bare
            // launch, which is what would make an untitled document.
            // SAFETY: as above.
            if !unsafe { open_launch_files(receiver, delegate) } {
                foundation::appleevent::raise(
                    foundation::appleevent::CORE_EVENT_CLASS,
                    foundation::appleevent::OPEN_APPLICATION,
                );
            }
        }
        let notification = foundation::notification::make(name, receiver, std::ptr::null_mut());
        // SAFETY: a delegate method is `void (id, SEL, NSNotification *)`.
        unsafe { send_if_implemented(delegate, selector, notification) };
        foundation::notification::post(name, receiver, std::ptr::null_mut());
    }
    // After the delegate has built its menus, and before any key press, so
    // Control-Command-F works without the menu having been opened.
    crate::menu::add_full_screen_item();
    // Launches handed over while this one was under way, then any later.
    crate::launch::finished_launching();
}

/// The URLs this launch was asked to open, in the order they were given.
fn launch_urls() -> Vec<String> {
    foundation::processinfo::guest_arguments()
        .iter()
        .skip(1)
        .filter_map(|argument| argument.to_str().map(str::to_owned))
        .filter(|argument| crate::launch::is_launch_url(argument))
        .collect()
}

/// `-[delegate application:openURLs:]`, where Apple sends it: after the program
/// has had `applicationWillFinishLaunching:` to build whatever handles a URL,
/// and before it is told launching finished, because a delegate that defers the
/// URL to "after initialization" is waiting on the notification that has not
/// been sent yet.
///
/// Nothing is sent for a launch with no URLs, which is the ordinary case - Apple
/// does not send the method either, and a delegate is entitled to treat being
/// called at all as meaning there is a URL.
///
/// # Safety
///
/// `receiver` is an application and `delegate` is null or live.
unsafe fn open_launch_urls(receiver: Id, delegate: Id) {
    let urls = launch_urls();
    if urls.is_empty() {
        return;
    }
    // SAFETY: the caller's.
    unsafe { send_open_urls(receiver, delegate, &urls) };
}

/// The files named on the command line, as AppKit opens them: every
/// argument that is not an option (`-Key value` pairs are the argument domain's)
/// or a URL and names a file that exists, made absolute against the working
/// directory.
fn launch_files() -> Vec<String> {
    let arguments: Vec<String> = foundation::processinfo::guest_arguments()
        .iter()
        .skip(1)
        .filter_map(|argument| argument.to_str().map(str::to_owned))
        .collect();
    let manager = {
        let class = objc::class::lookup(c"NSFileManager").expect("Foundation registered it");
        // SAFETY: `id (Class, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(
                objc::class::ptr(class) as Id,
                objc::sel::get("defaultManager"),
            )
        }
    };
    let directory = {
        // SAFETY: `NSString * (id, SEL)`.
        let path = unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(manager, objc::sel::get("currentDirectoryPath"))
        };
        // SAFETY: a string or nil.
        String::from_utf8_lossy(&unsafe { foundation::string::bytes(path) }).into_owned()
    };
    let exists = |path: &str| {
        let string = foundation::string::from_str(path);
        // SAFETY: `BOOL (id, SEL, NSString *)`.
        let found = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                std::mem::transmute(objc::msg::entry());
            f(manager, objc::sel::get("fileExistsAtPath:"), string)
        } != 0;
        objc::arc::release(string);
        found
    };
    let mut files = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument.starts_with("-psn_") {
            continue;
        }
        if argument.starts_with('-') {
            arguments.next();
            continue;
        }
        if crate::launch::is_launch_url(&argument) {
            continue;
        }
        let path = match argument.starts_with('/') {
            true => argument,
            false => format!("{}/{argument}", directory.trim_end_matches('/')),
        };
        if exists(&path) {
            files.push(path);
        }
    }
    files
}

/// `-[delegate application:openFiles:]`, or `-application:openFile:` for
/// each, for the files named on the command line. False when there are none
/// or the delegate takes neither.
///
/// # Safety
///
/// `receiver` is an application and `delegate` is null or live.
unsafe fn open_launch_files(receiver: Id, delegate: Id) -> bool {
    if delegate.is_null() {
        return false;
    }
    let files = launch_files();
    if files.is_empty() {
        return false;
    }
    // SAFETY: a live delegate.
    let class = unsafe { objc::object::class_of(delegate) };
    let responds = |sel: &str| class.is_some_and(|c| c.method(objc::sel::get(sel)).is_some());
    let strings: Vec<Id> = files
        .iter()
        .map(|f| foundation::string::from_str(f))
        .collect();
    let opened = if responds("application:openFiles:") {
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
            let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute(objc::msg::entry());
            f(
                delegate,
                objc::sel::get("application:openFiles:"),
                receiver,
                array as Id,
            );
            corefoundation::api::CFRelease(array);
        }
        true
    } else if responds("application:openFile:") {
        for string in &strings {
            // SAFETY: `BOOL (id, SEL, NSApplication *, NSString *)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> u8 =
                    std::mem::transmute(objc::msg::entry());
                f(
                    delegate,
                    objc::sel::get("application:openFile:"),
                    receiver,
                    *string,
                );
            }
        }
        true
    } else {
        false
    };
    for string in strings {
        objc::arc::release(string);
    }
    opened
}

/// The delegate of the shared application, or nil.
pub(crate) fn shared_delegate() -> Id {
    match shared() {
        app if app.is_null() => app,
        // SAFETY: the shared application.
        app => unsafe { (*(app as *const Application)).delegate },
    }
}

/// `-[delegate application:openURLs:]` on the shared application's delegate,
/// for URLs handed to the running application. False when the delegate does
/// not implement it.
///
/// # Safety
///
/// The shared application's delegate is null or live.
pub(crate) unsafe fn open_urls(urls: &[String]) -> bool {
    // SAFETY: the caller's.
    unsafe { send_open_urls(shared(), shared_delegate(), urls) }
}

/// # Safety
///
/// `receiver` is an application and `delegate` is null or live.
unsafe fn send_open_urls(receiver: Id, delegate: Id, urls: &[String]) -> bool {
    if delegate.is_null() {
        return false;
    }
    let sel = objc::sel::get("application:openURLs:");
    let Some(method) = (unsafe { objc::object::class_of(delegate) }).and_then(|c| c.method(sel))
    else {
        return false;
    };
    let objects: Vec<corefoundation::base::CFTypeRef> = urls
        .iter()
        .map(|url| corefoundation::url::new(url))
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // The array retained each URL, so this call's own references are done with.
    for object in &objects {
        // SAFETY: a URL this call just made.
        unsafe { corefoundation::api::CFRelease(*object) };
    }
    // SAFETY: `-application:openURLs:` is `void (id, SEL, NSApplication *,
    // NSArray<NSURL *> *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) = std::mem::transmute::<
            *const c_void,
            unsafe extern "C" fn(Id, Sel, Id, Id),
        >(method.implementation());
        f(delegate, sel, receiver, array as Id);
    }
    // SAFETY: the array this call made, which the delegate retains if it keeps it.
    unsafe { corefoundation::api::CFRelease(array) };
    true
}

unsafe extern "C" fn run(receiver: Id, sel: Sel) {
    // SAFETY: an application.
    unsafe { (*(receiver as *mut Application)).running = 1 };
    // SAFETY: as above; `-finishLaunching` is `void (id, SEL)`.
    unsafe { finish_launching(receiver, sel) };

    loop {
        // SAFETY: an application.
        if unsafe { (*(receiver as *const Application)).running } == 0 {
            break;
        }
        // SAFETY: an application.
        unsafe { turn(receiver, corefoundation::runloop::DEFAULT_MODE) };
    }
}

/// `NSModalPanelRunLoopMode` and `NSEventTrackingRunLoopMode`, which AppKit adds
/// to the common modes: a timer a toolkit schedules in the common modes has to
/// keep firing while a modal session or a tracking loop runs.
fn register_common_modes() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let loop_ = corefoundation::runloop::main();
        loop_.add_common_mode(MODAL_PANEL_MODE);
        loop_.add_common_mode("NSEventTrackingRunLoopMode");
        let source = POSTED_SOURCE
            .get_or_init(|| corefoundation::runloop::new_source(Some(posted_source_fired), 0));
        loop_.add_source(source, corefoundation::runloop::COMMON_MODES);
        // Drained by `service_run_loop`, not by the source, so a main-queue block
        // that runs a nested loop does not start the next one inside it.
        let source = MAIN_QUEUE_SOURCE
            .get_or_init(|| corefoundation::runloop::new_source(Some(posted_source_fired), 0));
        loop_.add_source(source, corefoundation::runloop::COMMON_MODES);
        darwin::dispatch::queue::set_main_wake_hook(main_queue_submitted);
    });
}

pub(crate) const MODAL_PANEL_MODE: &str = "NSModalPanelRunLoopMode";

/// One turn of the main thread's loop in `mode`: what `-run` does each time
/// round, and what `-runModalSession:` does once.
///
/// # Safety
///
/// `receiver` must be an application.
pub(crate) unsafe fn turn(receiver: Id, mode: &str) {
    // Popped on unwind too: a skipped pop would leave the next one draining
    // the wrong pool.
    struct Pool(*mut c_void);
    impl Drop for Pool {
        fn drop(&mut self) {
            objc::arc::pool_pop(self.0);
        }
    }
    let _pool = Pool(objc::arc::pool_push());
    unsafe { turn_in_pool(receiver, mode) };
}

unsafe fn turn_in_pool(receiver: Id, mode: &str) {
    register_common_modes();
    // The window system first, and every turn. A window that is never pumped is
    // one the platform has not been told to show: on macOS SDL's Cocoa backend
    // needs its events drained for the window to appear at all, so this is not
    // only about delivering input.
    crate::event::pump(receiver);
    // `-postEvent:atStart:` is used by Cocoa backends to wake work that must run
    // through their `-sendEvent:` override. Dispatch through the receiver rather
    // than calling this class's implementation directly so a principal
    // `NSApplication` subclass sees the event.
    unsafe { dispatch_posted_events(receiver) };
    unsafe { service_run_loop(mode, 0.002) };
}

/// Drain the main queue and run the main run loop in `mode` for `seconds`.
///
/// The main queue is drained here because this is the main thread: a block sent
/// to it with `dispatch_async` runs nowhere else. The slice is short because the
/// run loop's timers are what drive a frame - see `corevideo` - and the window
/// system is pumped between slices, not by the loop.
///
/// # Safety
///
/// Called on the main thread, from a place a run loop callback may reenter.
unsafe fn service_run_loop(mode: &str, seconds: f64) {
    darwin::dispatch::queue::drain_main();
    // Returning after a source lets an event posted from another thread be sent
    // now rather than when the slice ends - see `POSTED_SOURCE`.
    let result = corefoundation::runloop::main().run_in_mode(mode, seconds, true);
    if result == corefoundation::runloop::result::FINISHED && seconds > 0.0 {
        // Nothing scheduled: a program with no display link and no source is
        // waiting to be told to stop, and spinning on that would be a core at a
        // hundred per cent for a window that is only being looked at.
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Seconds until a deadline, or zero for a date this runtime cannot read.
fn wait_interval(until: Id) -> f64 {
    if until.is_null() {
        return 0.0;
    }
    // SAFETY: `-timeIntervalSinceNow` is `double (id, SEL)` on an `NSDate`.
    let seconds = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(objc::msg::entry());
        f(until, objc::sel::get("timeIntervalSinceNow"))
    };
    seconds.max(0.0)
}

fn event_matches_mask(event: Id, mask: u64) -> bool {
    let kind = crate::nsevent::kind_of(event);
    kind < u64::BITS as u64 && mask & (1_u64 << kind) != 0
}

/// Take or inspect the first posted event matching `mask`.
fn queued_event(mask: u64, dequeue: bool) -> Id {
    let mut queue = posted_events();
    let Some(index) = queue
        .iter()
        .position(|event| event_matches_mask(*event as Id, mask))
    else {
        return std::ptr::null_mut();
    };
    let event = match dequeue {
        true => queue
            .remove(index)
            .expect("the matching event is still queued") as Id,
        false => objc::arc::retain(queue[index] as Id),
    };
    drop(queue);
    if dequeue {
        crate::event::handing_out(event);
    }
    // A dequeued event transfers the queue's reference to the current
    // autorelease pool. A peek acquired its own reference above.
    objc::arc::autorelease(event)
}

/// `-nextEventMatchingMask:untilDate:inMode:dequeue:`.
///
/// A program that drives its own loop - UE3 does - calls this instead of
/// `-run`, so the pump has to happen here too or its window is never serviced.
/// Keyboard, mouse button, mouse motion and scroll events, and events posted to
/// the application, are the objects this method returns; the rest of
/// window-system input is delivered by `event::pump` directly.
///
/// FIXME (ACCURACY): AppKit has one event queue. Every window-server event -
/// mouse, keyboard, scroll, tracking, app-defined - is dequeued here, run
/// through the local monitors (`+addLocalMonitorForEventsMatchingMask:`, whose
/// handler can replace or swallow it), and returned; nothing reaches a window
/// until the caller hands it to `-[NSApplication sendEvent:]`, which `-run` does
/// for itself. A mask leaves non-matching events queued, in order, for a later
/// call - that is how a mouse-tracking loop keeps keystrokes for afterwards.
/// Here key, flags-changed, mouse button, motion and scroll events take that
/// path; the rest - entered/exited, cursor updates, and the moves a tracking
/// area's owner is sent - are sent from inside `event::pump`, before this
/// returns, and never come back from it. The difference shows when a guest
/// reads its input from what this returns rather than from its responders: SDL
/// 2.0.4 handles the keyboard only in its `Cocoa_PumpEvents` loop, and
/// ClassiCube the mouse only in its own loop, which is why those moved first.
/// Tracking events still bypass a guest loop that filters or drops them, and
/// can arrive ahead of input that happened earlier.
///
/// Done properly, `event::pump` only translates: each SDL event becomes an
/// `NSEvent` (or a window notification, which is not an event) appended to
/// `POSTED_EVENTS`, keeping SDL's order. This method returns the first match
/// after the monitors have run, waiting on the run loop until `until` for one
/// to arrive; `turn` dequeues and sends exactly as `-run` does; and
/// `-discardEventsMatchingMask:beforeEvent:` becomes real. Two things move with it. Text resolved by the host travels on
/// its key down - `nsevent::attach_text` - since a separate text event would be
/// committed before the guest saw the key; the IME's own commits and
/// compositions (`Event::Text` and `Event::TextEditing` not paired with a key)
/// need an event of their own in the same queue so they stay behind the key
/// that started them. And `-currentEvent` and `+pressedMouseButtons` describe
/// the event being sent, not the host's state when the pump ran.
unsafe extern "C" fn next_event(
    receiver: Id,
    _sel: Sel,
    mask: u64,
    until: Id,
    mode: Id,
    dequeue: u8,
) -> Id {
    register_common_modes();
    crate::event::pump(receiver);
    let event = queued_event(mask, dequeue != 0);
    if !event.is_null() {
        return event;
    }
    // Apple runs the run loop in `mode` until an event arrives or the deadline
    // passes, and a toolkit waiting in here - Qt in a modal session does - relies
    // on its timers and sources firing meanwhile. Window-system input does not
    // wake the loop here, so the wait is a short slice and the caller asks again.
    let mode = match mode.is_null() {
        true => corefoundation::runloop::DEFAULT_MODE.to_string(),
        // SAFETY: a mode is a string object.
        false => String::from_utf8_lossy(&unsafe { foundation::string::bytes(mode) }).into_owned(),
    };
    let seconds = wait_interval(until).min(0.002);
    // SAFETY: the main thread's event call, where Apple runs the loop too.
    unsafe { service_run_loop(&mode, seconds) };
    queued_event(mask, dequeue != 0)
}

/// `-stop:`, which ends the loop above after the current turn.
unsafe extern "C" fn stop(receiver: Id, _sel: Sel, _sender: Id) {
    // SAFETY: an application.
    unsafe { (*(receiver as *mut Application)).running = 0 };
    corefoundation::runloop::main().stop();
}

/// `-terminate:`.
///
/// Apple asks the delegate `applicationShouldTerminate:` first and may be told
/// to wait. That answer is an enumeration whose "later" arm needs a reply the
/// program sends back asynchronously, so it is not half-implemented here: the
/// delegate is asked nothing, and the process ends after the notification. A
/// program that wanted to veto its own exit gets no chance to, which is worth
/// saying out loud rather than discovering.
unsafe extern "C" fn terminate(receiver: Id, _sel: Sel, _sender: Id) {
    // SAFETY: an application.
    let delegate = unsafe { (*(receiver as *const Application)).delegate };
    let notification =
        foundation::notification::make(WILL_TERMINATE, receiver, std::ptr::null_mut());
    // SAFETY: a delegate method is `void (id, SEL, NSNotification *)`.
    unsafe { send_if_implemented(delegate, "applicationWillTerminate:", notification) };
    foundation::notification::post(WILL_TERMINATE, receiver, std::ptr::null_mut());
    darwin::exit::finalize(0);
    std::process::exit(0);
}

/// Set by `-[NSWindow close]` and taken by the event pump.
static WINDOW_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A window closed; whether the application follows it is decided on the next
/// turn of the event pump, as Apple defers it, so the program finishes the
/// close it is in the middle of before `-terminate:` can end the process.
pub(crate) fn window_closed() {
    WINDOW_CLOSED.store(true, Ordering::Release);
}

/// Terminate if a window closed, none is left on screen, and the delegate's
/// `-applicationShouldTerminateAfterLastWindowClosed:` says to.
pub(crate) fn check_last_window_closed() {
    if !WINDOW_CLOSED.swap(false, Ordering::AcqRel) {
        return;
    }
    let app = shared();
    if app.is_null()
        || crate::window::all()
            .into_iter()
            .any(crate::window::is_shown)
    {
        return;
    }
    // SAFETY: an application.
    let delegate = unsafe { (*(app as *const Application)).delegate };
    let sel = objc::sel::get("applicationShouldTerminateAfterLastWindowClosed:");
    // SAFETY: null or a live object this runtime allocated.
    let Some(method) = unsafe { objc::object::class_of(delegate) }.and_then(|c| c.method(sel))
    else {
        return;
    };
    // SAFETY: the method is `BOOL (id, SEL, NSApplication *)`.
    let quit = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
            std::mem::transmute(method.implementation());
        f(delegate, sel, app)
    };
    if quit != 0 {
        // SAFETY: `-terminate:` is `void (id, SEL, id)`; sent so a subclass's
        // override runs.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(app, objc::sel::get("terminate:"), app);
        }
    }
}

/// `-activateIgnoringOtherApps:`, which has nothing to activate yet.
///
/// Present rather than absent because every Cocoa `main` calls it, and a refusal
/// there would stop a program on a line that does not matter until there is a
/// window to bring forward.
unsafe extern "C" fn activate(_receiver: Id, _sel: Sel, _flag: u8) {}

/// `-setActivationPolicy:`, which answers that it succeeded.
unsafe extern "C" fn set_activation_policy(_receiver: Id, _sel: Sel, _policy: isize) -> u8 {
    1
}

/// `NSApplicationPresentationOptions`: whether the menu bar and Dock are hidden.
///
/// Kept and reported rather than acted on. There is no menu bar and no Dock in
/// an SDL window, so the *effect* of hiding them is already the case; what a
/// program needs is for the value it set to be the value it reads back, because
/// a game toggling full screen compares the two.
static PRESENTATION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn presentation_options(_receiver: Id, _sel: Sel) -> usize {
    PRESENTATION.load(std::sync::atomic::Ordering::Acquire)
}

unsafe extern "C" fn set_presentation_options(_receiver: Id, _sel: Sel, options: usize) {
    PRESENTATION.store(options, std::sync::atomic::Ordering::Release);
}

/// `-windowWithWindowNumber:`, or nil for a number no window of this
/// process's has.
///
/// Asked through dispatch, as `-windowNumber` is what a subclass may answer
/// differently. Zero is no window's number: a window with no SDL window yet
/// answers it, and must not be found by it.
unsafe extern "C" fn window_with_window_number(_receiver: Id, _sel: Sel, number: isize) -> Id {
    if number == 0 {
        return std::ptr::null_mut();
    }
    let sel = objc::sel::get("windowNumber");
    crate::window::all()
        .into_iter()
        // SAFETY: every entry is a live `NSWindow`, and this is
        // `NSInteger (id, SEL)`.
        .find(|window| unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
            f(*window, sel) == number
        })
        .unwrap_or(std::ptr::null_mut())
}

/// The windows this runtime has made, in the order they were made.
unsafe extern "C" fn windows(_receiver: Id, _sel: Sel) -> Id {
    let objects: Vec<corefoundation::base::CFTypeRef> = crate::window::all()
        .into_iter()
        .map(|w| w as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

/// `-orderedWindows`: front to back, and without panels, which Apple leaves
/// out as not scriptable.
unsafe extern "C" fn ordered_windows(_receiver: Id, _sel: Sel) -> Id {
    let panel = objc::class::lookup(c"NSPanel");
    let objects: Vec<corefoundation::base::CFTypeRef> = crate::window::front_to_back()
        .into_iter()
        .filter(|w| {
            // SAFETY: a live window of this crate's.
            !panel.is_some_and(|p| unsafe { corefoundation::base::is_kind_of(w.cast(), p) })
        })
        .map(|w| w as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn enumerate_windows(_receiver: Id, _sel: Sel, options: u64, block: Id) {
    // SAFETY: `void (^)(NSWindow *, BOOL *)`, as the selector declares.
    let Some(invoke) = (unsafe { crate::animation::invoke(block) }) else {
        return;
    };
    // `NSWindowListOrderedFrontToBack`.
    let windows = match options & 1 != 0 {
        true => crate::window::front_to_back(),
        false => crate::window::all(),
    };
    for window in &windows {
        objc::arc::retain(*window);
    }
    let mut stop = 0u8;
    for window in &windows {
        // SAFETY: the block's own prototype.
        unsafe {
            let f: unsafe extern "C" fn(Id, Id, *mut u8) = std::mem::transmute(invoke);
            f(block, *window, &mut stop);
        }
        if stop != 0 {
            break;
        }
    }
    windows.into_iter().for_each(objc::arc::release);
}

unsafe extern "C" fn key_window(_receiver: Id, _sel: Sel) -> Id {
    crate::window::key_window()
}

/// The four settable menus, each kept for the program to read back.
///
/// One implementation for all of them, resolved by the selector: the bodies would
/// otherwise be eight copies of two lines, which is eight places to retain the
/// wrong field.
///
/// # Safety
///
/// `receiver` is an application of this crate's.
unsafe fn menu_slot<'a>(receiver: Id, sel: Sel) -> &'a mut Id {
    // SAFETY: an application.
    let app = unsafe { &mut *(receiver as *mut Application) };
    // SAFETY: a registered selector is a NUL-terminated interned name.
    let name = unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes();
    match name {
        b"windowsMenu" | b"setWindowsMenu:" => &mut app.windows_menu,
        b"helpMenu" | b"setHelpMenu:" => &mut app.help_menu,
        b"dockMenu" | b"setDockMenu:" => &mut app.dock_menu,
        _ => &mut app.services_menu,
    }
}

unsafe extern "C" fn menu_for(receiver: Id, sel: Sel) -> Id {
    // SAFETY: an application of this crate's.
    *unsafe { menu_slot(receiver, sel) }
}

unsafe extern "C" fn set_menu_for(receiver: Id, sel: Sel, menu: Id) {
    let slot = unsafe { menu_slot(receiver, sel) };
    objc::arc::release(*slot);
    *slot = objc::arc::retain(menu);
}

/// `-postEvent:atStart:`. The queue owns the event until it is dequeued or sent.
unsafe extern "C" fn post_event(_receiver: Id, _sel: Sel, event: Id, at_start: u8) {
    if event.is_null() {
        return;
    }
    let event = objc::arc::retain(event) as usize;
    let mut queue = posted_events();
    match at_start != 0 {
        true => queue.push_front(event),
        false => queue.push_back(event),
    }
    drop(queue);
    if let Some(source) = POSTED_SOURCE.get() {
        source.signal();
        corefoundation::runloop::main().wake();
    }
}

/// Queue an input event for `-nextEventMatchingMask:`, behind what is already
/// there. The queue takes its own reference.
pub(crate) fn enqueue_input(event: Id) {
    if event.is_null() {
        return;
    }
    posted_events().push_back(objc::arc::retain(event) as usize);
}

/// Send every queued event through the application's dynamic `-sendEvent:`.
///
/// # Safety
///
/// `receiver` is a live application.
unsafe fn dispatch_posted_events(receiver: Id) {
    // Taken as one batch, with the lock released before any is sent: a
    // `-sendEvent:` override that posts another event must not find the queue
    // locked, and what it posts is for the next turn, not this one.
    let batch = std::mem::take(&mut *posted_events());
    for event in batch {
        let event = event as Id;
        crate::event::handing_out(event);
        // SAFETY: the caller's obligation.
        unsafe { send_event_through(receiver, event) };
        objc::arc::release(event);
    }
}

/// Send `event` through the application's dynamic `-sendEvent:`, so a
/// principal subclass's override sees it, with `-currentEvent` answering it.
///
/// # Safety
///
/// `receiver` is a live application.
pub(crate) unsafe fn send_event_through(receiver: Id, event: Id) {
    // Restored rather than cleared: an override can send another event from
    // inside this one, and the outer one is current again once it returns.
    let outer = CURRENT_EVENT.swap(event as usize, Ordering::AcqRel);
    // SAFETY: the caller's obligation; `-sendEvent:` is `void (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("sendEvent:"), event);
    }
    CURRENT_EVENT.store(outer, Ordering::Release);
}

/// `-currentEvent`, the event inside `-sendEvent:`, and nil outside it.
unsafe extern "C" fn register_services(_receiver: Id, _sel: Sel, _send: Id, _return: Id) {}

unsafe extern "C" fn reply_to_open_or_print(_receiver: Id, _sel: Sel, _reply: usize) {}

pub(crate) unsafe extern "C" fn current_event(_receiver: Id, _sel: Sel) -> Id {
    CURRENT_EVENT.load(Ordering::Acquire) as Id
}

/// `-sendEvent:`, the entry point a subclass overrides and then calls up into.
///
/// Every event passes through here on its way to its window: SDL's
/// `SDL3Application` handles the keyboard in its override and nowhere else,
/// and `QNSApplication` filters before calling `super`.
unsafe extern "C" fn send_event(_receiver: Id, _sel: Sel, event: Id) {
    crate::event::route_from_application(event);
}

/// `-hide:`, `-unhide:`, `-unhideWithoutActivation` and `-arrangeInFront:`.
///
/// Window-server requests with no window server behind them. Accepted rather than
/// refused because each is a thing a program asks *for* - hiding is a user
/// command it forwards - and the honest outcome is that nothing hides, not that
/// the program stops.
unsafe extern "C" fn ignored_sender(_receiver: Id, _sel: Sel, _sender: Id) {}

/// `-isFullKeyboardAccessEnabled`: whether Tab moves focus through every
/// control rather than text fields and lists alone. That is a macOS System
/// Settings switch with no desktop counterpart here, and it is off by default.
unsafe extern "C" fn is_full_keyboard_access_enabled(_receiver: Id, _sel: Sel) -> u8 {
    0
}

unsafe extern "C" fn ignored_nullary(_receiver: Id, _sel: Sel) {}

/// `-requestUserAttention:`, which bounces the Dock icon.
///
/// There is no Dock. The return value is the request identifier a caller passes
/// to `-cancelUserAttentionRequest:`, and zero is the identifier of no request -
/// which is the truth and what a caller cancelling it will find.
unsafe extern "C" fn request_user_attention(_receiver: Id, _sel: Sel, _kind: isize) -> isize {
    0
}

unsafe extern "C" fn cancel_user_attention_request(_receiver: Id, _sel: Sel, _request: isize) {}

unsafe extern "C" fn modal_window(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an application.
    unsafe { (*(receiver as *const Application)).modal_window }
}

/// `-_isRunningModal`, private but sent by Gecko: whether a modal session is
/// running, which here is whether there is a modal window.
unsafe extern "C" fn is_running_modal(receiver: Id, sel: Sel) -> u8 {
    // SAFETY: an application.
    u8::from(!unsafe { modal_window(receiver, sel) }.is_null())
}

/// `NSModalResponse`. Apple's values, because a caller compares against them.
mod modal_response {
    /// `NSModalResponseStop`, which is what `-stopModal` makes the session return.
    pub const STOP: isize = -1000;
    /// `NSModalResponseContinue`, "the session is still running".
    pub const CONTINUE: isize = -1002;
}

/// `-beginModalSessionForWindow:`
///
/// The session handle is the window, because that is all a session is here: the
/// three calls that take one only ever ask which window it is about. A caller
/// drives the loop itself with `-runModalSession:`, which is the shape that works
/// without a nested event loop of AppKit's own.
unsafe extern "C" fn begin_modal_session(receiver: Id, _sel: Sel, window: Id) -> Id {
    // SAFETY: an application.
    let app = unsafe { &mut *(receiver as *mut Application) };
    app.modal_window = objc::arc::retain(window);
    app.modal_depth += 1;
    // Beginning a session is what puts the window on screen: Qt shows an
    // application-modal dialog with this call alone and never orders it front.
    if !window.is_null() {
        // SAFETY: `-makeKeyAndOrderFront:` is `void (id, SEL, id)`, sent so a
        // window subclass's override runs.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(
                window,
                objc::sel::get("makeKeyAndOrderFront:"),
                std::ptr::null_mut(),
            );
        }
    }
    window
}

/// `-runModalSession:`, one turn of the caller's own modal loop.
///
/// Takes one turn of the main loop in `NSModalPanelRunLoopMode` - events, posted
/// events, the main queue and the run loop's timers and sources, which a toolkit
/// spinning this call depends on - and answers `NSModalResponseContinue` while
/// the session is still open, so the caller loops; `-stopModal` closes it and the
/// next turn answers `NSModalResponseStop`. Answering `Stop` straight away would
/// make every modal dialog close before it was drawn, and answering `Continue`
/// for ever would make one that never closes.
unsafe extern "C" fn run_modal_session(receiver: Id, _sel: Sel, _session: Id) -> isize {
    // SAFETY: an application.
    unsafe { turn(receiver, MODAL_PANEL_MODE) };
    // SAFETY: an application.
    let app = unsafe { &*(receiver as *const Application) };
    match app.modal_depth > 0 {
        true => modal_response::CONTINUE,
        false => modal_response::STOP,
    }
}

unsafe extern "C" fn end_modal_session(receiver: Id, _sel: Sel, _session: Id) {
    // SAFETY: an application.
    let app = unsafe { &mut *(receiver as *mut Application) };
    app.modal_depth = app.modal_depth.saturating_sub(1);
    if app.modal_depth == 0 {
        objc::arc::release(app.modal_window);
        app.modal_window = std::ptr::null_mut();
    }
}

/// `-stopModal` and `-abortModal`, which end the innermost session.
unsafe extern "C" fn stop_modal(receiver: Id, sel: Sel) {
    // SAFETY: an application.
    unsafe { stop_modal_with_code(receiver, sel, modal_response::STOP) };
}

unsafe extern "C" fn stop_modal_with_code(receiver: Id, _sel: Sel, code: isize) {
    // SAFETY: an application.
    let app = unsafe { &mut *(receiver as *mut Application) };
    app.modal_depth = app.modal_depth.saturating_sub(1);
    app.modal_code = code;
}

/// `-runModalForWindow:`, the blocking form.
///
/// Runs the loop `-runModalSession:` leaves to the caller: pump until something
/// calls `-stopModal`. The guard is that *only* guest code can stop it, so a
/// window nothing ever dismisses would hang the program - which is what it does
/// on a Mac too, and is why the pump is the same one the main loop uses rather
/// than a spin.
unsafe extern "C" fn run_modal_for_window(receiver: Id, sel: Sel, window: Id) -> isize {
    // SAFETY: an application and a window of this crate's.
    unsafe { begin_modal_session(receiver, sel, window) };
    loop {
        // SAFETY: as above; the session handle is the window.
        let response = unsafe { run_modal_session(receiver, sel, window) };
        if response != modal_response::CONTINUE {
            break;
        }
    }
    unsafe { end_modal_session(receiver, sel, window) };
    // SAFETY: an application.
    unsafe { (*(receiver as *const Application)).modal_code }
}

/// Send `selector` to `target` if it has one, and do nothing if it does not.
///
/// An *optional* protocol method, which `applicationDidFinishLaunching:` is - so
/// a delegate that does not implement one is a program making a choice, not a
/// program that is broken. Everything else in this tree that cannot find a
/// method reports it; this is the one place where not finding it is the answer.
///
/// # Safety
///
/// `target` must be null or live, and `selector` must name a method taking one
/// object and returning nothing.
unsafe fn send_if_implemented(target: Id, selector: &str, argument: Id) {
    if target.is_null() {
        return;
    }
    let sel = objc::sel::get(selector);
    let Some(method) = (unsafe { objc::object::class_of(target) }).and_then(|c| c.method(sel))
    else {
        return;
    };
    let f: unsafe extern "C" fn(Id, Sel, Id) = unsafe {
        std::mem::transmute::<*const c_void, unsafe extern "C" fn(Id, Sel, Id)>(
            method.implementation(),
        )
    };
    unsafe { f(target, sel, argument) };
}

/// `int NSApplicationMain(int argc, const char *argv[])`.
///
/// The arguments are ignored, as Apple's are: everything it needs is in the
/// bundle, and a program's own arguments reached `main` before this was called.
///
/// Never returns on Darwin - `-run` ends in `exit` - and never returns here for
/// the same reason, but the return type is `int` because that is what the guest
/// declared and a call site expects `eax` to have been written.
#[expect(non_snake_case, reason = "the C spelling is the interface")]
pub unsafe extern "C" fn NSApplicationMain(_argc: i32, _argv: *const *const c_char) -> i32 {
    crate::bootstrap();
    let bundle = foundation::bundle::main();
    let principal = bundle
        .and_then(|b| b.principal_class())
        .unwrap_or("NSApplication");
    let Some(class) = std::ffi::CString::new(principal)
        .ok()
        .and_then(|n| objc::class::lookup(&n))
    else {
        objc::fail!(
            "NSApplicationMain: the bundle's NSPrincipalClass is {principal}, which no image \
             declared"
        );
    };

    let app = {
        let class_object = objc::class::ptr(class) as Id;
        // SAFETY: `+sharedApplication` is `id (Class, SEL)`, and this crate
        // installed it on `NSApplication`, from which the principal class
        // descends - which is checked below rather than assumed.
        unsafe { shared_application(class_object, objc::sel::get("sharedApplication")) }
    };
    if app.is_null() {
        objc::fail!("NSApplicationMain: {principal} could not be instantiated");
    }
    let is_application = objc::class::lookup(c"NSApplication")
        .is_some_and(|a| class.chain().any(|c| std::ptr::eq(c, a)));
    if !is_application {
        objc::fail!(
            "NSApplicationMain: the bundle's NSPrincipalClass {principal} does not descend from \
             NSApplication"
        );
    }

    match bundle.and_then(|b| b.main_nib()) {
        Some(path) => match crate::nib::load(&path, app) {
            Ok(loaded) => loaded.report(&path),
            Err(e) => objc::fail!("NSApplicationMain: {}: {e}", path.display()),
        },
        // A program with no `NSMainNibFile` is a program that wires its delegate
        // in code, which is ordinary and not an error.
        None => {}
    }

    // SAFETY: `-run` is `void (id, SEL)` and `app` is an application.
    unsafe { run(app, objc::sel::get("run")) };
    0
}

unsafe extern "C" fn appearance(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const Application)).appearance }
}

unsafe extern "C" fn set_appearance(receiver: Id, _sel: Sel, appearance: Id) {
    let appearance = objc::arc::retain(appearance);
    let old = unsafe {
        std::mem::replace(
            &mut (*(receiver as *mut Application)).appearance,
            appearance,
        )
    };
    objc::arc::release(old);
}

unsafe extern "C" fn effective_appearance(receiver: Id, sel: Sel) -> Id {
    let own = unsafe { appearance(receiver, sel) };
    if !own.is_null() {
        return own;
    }
    crate::appearance::for_program()
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSApplication").expect("declared before its methods");
    class_methods!(class, {
        "sharedApplication" => shared_application, c"@16@0:8";
    });
    methods!(class, {
        "appearance" => appearance, c"@16@0:8";
        "setAppearance:" => set_appearance, c"v24@0:8@16";
        "delegate" => delegate, c"@16@0:8";
        "setDelegate:" => set_delegate, c"v24@0:8@16";
        "mainMenu" => main_menu, c"@16@0:8";
        "setMainMenu:" => set_main_menu, c"v24@0:8@16";
        "applicationIconImage" => application_icon_image, c"@16@0:8";
        "setApplicationIconImage:" => set_application_icon_image, c"v24@0:8@16";
        "isRunning" => is_running, c"c16@0:8";
        "isActive" => is_active, c"c16@0:8";
        "isFullKeyboardAccessEnabled" => is_full_keyboard_access_enabled, c"c16@0:8";
        "effectiveAppearance" => effective_appearance, c"@16@0:8";
        "finishLaunching" => finish_launching, c"v16@0:8";
        "run" => run, c"v16@0:8";
        "stop:" => stop, c"v24@0:8@16";
        "terminate:" => terminate, c"v24@0:8@16";
        "activateIgnoringOtherApps:" => activate, c"v20@0:8c16";
        "setActivationPolicy:" => set_activation_policy, c"c24@0:8q16";
        "currentSystemPresentationOptions" => presentation_options, c"Q16@0:8";
        "presentationOptions" => presentation_options, c"Q16@0:8";
        "setPresentationOptions:" => set_presentation_options, c"v24@0:8Q16";
        "nextEventMatchingMask:untilDate:inMode:dequeue:" => next_event, c"@48@0:8Q16@24@32B40";
        "windows" => windows, c"@16@0:8";
        "orderedWindows" => ordered_windows, c"@16@0:8";
        "enumerateWindowsWithOptions:usingBlock:" => enumerate_windows, c"v32@0:8Q16@?24";
        "windowWithWindowNumber:" => window_with_window_number, c"@24@0:8q16";
        "keyWindow" => key_window, c"@16@0:8";
        "mainWindow" => key_window, c"@16@0:8";
        "servicesMenu" => menu_for, c"@16@0:8";
        "setServicesMenu:" => set_menu_for, c"v24@0:8@16";
        "windowsMenu" => menu_for, c"@16@0:8";
        "setWindowsMenu:" => set_menu_for, c"v24@0:8@16";
        "helpMenu" => menu_for, c"@16@0:8";
        "setHelpMenu:" => set_menu_for, c"v24@0:8@16";
        "dockMenu" => menu_for, c"@16@0:8";
        "setDockMenu:" => set_menu_for, c"v24@0:8@16";
        "postEvent:atStart:" => post_event, c"v28@0:8@16B24";
        "currentEvent" => current_event, c"@16@0:8";
        // Documents opened by command-line arguments, not an Apple Event, so
        // there is no reply to send.
        "replyToOpenOrPrint:" => reply_to_open_or_print, c"v24@0:8Q16";
        // There is no Services menu to offer types to.
        "registerServicesMenuSendTypes:returnTypes:" => register_services, c"v32@0:8@16@24";
        "sendEvent:" => send_event, c"v24@0:8@16";
        // Private: the window server's hit-testing cache, which this runtime
        // has none of. Gecko drops a hidden popup from it.
        "_removeWindowFromCache:" => ignored_sender, c"v24@0:8@16";
        // Private but SDL 1.2's SDLMain calls it for the application menu.
        "setAppleMenu:" => ignored_sender, c"v24@0:8@16";
        "hide:" => ignored_sender, c"v24@0:8@16";
        "hideOtherApplications:" => ignored_sender, c"v24@0:8@16";
        "unhideAllApplications:" => ignored_sender, c"v24@0:8@16";
        "unhide:" => ignored_sender, c"v24@0:8@16";
        "arrangeInFront:" => ignored_sender, c"v24@0:8@16";
        "orderFrontStandardAboutPanel:" => crate::about::order_front, c"v24@0:8@16";
        "orderFrontStandardAboutPanelWithOptions:" => crate::about::order_front_with_options, c"v24@0:8@16";
        "unhideWithoutActivation" => ignored_nullary, c"v16@0:8";
        "updateWindows" => ignored_nullary, c"v16@0:8";
        "requestUserAttention:" => request_user_attention, c"q24@0:8q16";
        "cancelUserAttentionRequest:" => cancel_user_attention_request, c"v24@0:8q16";
        "modalWindow" => modal_window, c"@16@0:8";
        "_isRunningModal" => is_running_modal, c"c16@0:8";
        "_isRunningAppModal" => is_running_modal, c"c16@0:8";
        "beginModalSessionForWindow:" => begin_modal_session, c"@24@0:8@16";
        "runModalSession:" => run_modal_session, c"q24@0:8^v16";
        "endModalSession:" => end_modal_session, c"v24@0:8^v16";
        "runModalForWindow:" => run_modal_for_window, c"q24@0:8@16";
        "stopModal" => stop_modal, c"v16@0:8";
        "stopModalWithCode:" => stop_modal_with_code, c"v24@0:8q16";
        "occlusionState" => crate::window::visible_occlusion, c"Q16@0:8";
        "endSheet:" => ignored_sender, c"v24@0:8@16";
        "abortModal" => stop_modal, c"v16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No window has the number zero or a number nothing was given, and
    /// asking for one is nil rather than the first window.
    #[test]
    fn an_unknown_window_number_is_nil() {
        crate::bootstrap();
        let sel = objc::sel::get("windowWithWindowNumber:");
        // SAFETY: `-windowWithWindowNumber:` is `id (id, SEL, NSInteger)`.
        unsafe {
            assert!(window_with_window_number(std::ptr::null_mut(), sel, 0).is_null());
            assert!(window_with_window_number(std::ptr::null_mut(), sel, -7).is_null());
        }
    }

    /// Qt can send the same menu again when a popup window gains focus. A
    /// no-op reassignment must preserve both identity and ownership.
    #[test]
    fn setting_the_same_main_menu_does_not_replace_or_release_it() {
        crate::bootstrap();
        let app_class = objc::class::lookup(c"NSApplication").expect("application class");
        let menu_class = objc::class::lookup(c"NSMenu").expect("menu class");
        let app = objc::object::create_instance(app_class, INSTANCE_SIZE);
        let first = objc::object::create_instance(menu_class, 0);
        let second = objc::object::create_instance(menu_class, 0);
        let selector = objc::sel::get("setMainMenu:");
        // SAFETY: `app` is an NSApplication and both menus are live NSMenus.
        unsafe {
            set_main_menu(app, selector, first);
            assert_eq!(main_menu(app, selector), first);
            let count = objc::arc::retain_count(first);
            set_main_menu(app, selector, first);
            assert_eq!(main_menu(app, selector), first);
            assert_eq!(objc::arc::retain_count(first), count);
            set_main_menu(app, selector, second);
            assert_eq!(main_menu(app, selector), second);
            assert_eq!(objc::arc::retain_count(first), count - 1);
            set_main_menu(app, selector, std::ptr::null_mut());
        }
        objc::arc::release(first);
        objc::arc::release(second);
        objc::arc::release(app);
    }

    /// Tests below temporarily replace the process-wide guest argv.
    static ARGUMENTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// One test for the singleton, because a process has one of it.
    ///
    /// Two claims: `+sharedApplication` builds an instance of the class it was
    /// *sent to*, which is what makes a bundle's `YYApplication` principal class
    /// mean anything; and the `NSApp` cell holds that object, because a guest
    /// reads `NSApp` by loading through the symbol rather than using it.
    #[test]
    fn the_shared_application_is_the_class_it_was_asked_for_and_is_what_ns_app_holds() {
        crate::bootstrap();
        let subclass = corefoundation::base::register(
            c"RosemaryTestApplicationSubclass",
            objc::class::lookup(c"NSApplication"),
            INSTANCE_SIZE,
            false,
        );
        // SAFETY: `+sharedApplication` is `id (Class, SEL)`.
        let app = unsafe {
            shared_application(
                objc::class::ptr(subclass) as Id,
                objc::sel::get("sharedApplication"),
            )
        };
        assert!(!app.is_null());
        // SAFETY: a live object this runtime allocated.
        let class = unsafe { objc::object::class_of(app) }.expect("a class");
        assert_eq!(class.name_str(), "RosemaryTestApplicationSubclass");

        // The same object the second time, whatever it is sent to.
        let again = unsafe {
            shared_application(
                objc::class::ptr(objc::class::lookup(c"NSApplication").expect("NSApplication"))
                    as Id,
                objc::sel::get("sharedApplication"),
            )
        };
        assert_eq!(app, again);

        // SAFETY: the cell is a word this crate leaked.
        let held = unsafe { *(crate::ns_app_cell() as *const u64) };
        assert_eq!(held, app as usize as u64);
    }

    #[test]
    fn only_scheme_shaped_arguments_are_launch_urls() {
        let _arguments = ARGUMENTS.lock().expect("no panic");
        crate::bootstrap();
        foundation::processinfo::set_arguments(
            &[
                "example-player://not-argv-zero",
                "-protocolString",
                "example-player://experiences/start?id=1",
                "--place",
                "/Users/me/a:file",
                "example-player:1+launchmode:play",
                "x:",
                "a:b",
            ]
            .map(Into::into),
        );
        assert_eq!(
            launch_urls(),
            [
                "example-player://experiences/start?id=1",
                "example-player:1+launchmode:play",
            ]
        );
        foundation::processinfo::set_arguments(&["guest".into()]);
        assert!(launch_urls().is_empty());
    }

    /// Where the URL arrives, which is the whole of the contract: a delegate
    /// that defers a cold-launch URL until initialization has finished is
    /// waiting for `applicationDidFinishLaunching:`, so a URL delivered after
    /// it is a URL delivered too late.
    #[test]
    fn a_launch_url_reaches_the_delegate_between_the_two_notifications() {
        let _arguments = ARGUMENTS.lock().expect("no panic");
        crate::bootstrap();
        static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

        unsafe extern "C" fn will_finish(_receiver: Id, _sel: Sel, _notification: Id) {
            SEEN.lock().expect("no panic").push(String::from("will"));
        }
        unsafe extern "C" fn did_finish(_receiver: Id, _sel: Sel, _notification: Id) {
            SEEN.lock().expect("no panic").push(String::from("did"));
        }
        unsafe extern "C" fn open_urls(_receiver: Id, _sel: Sel, _app: Id, urls: Id) {
            // SAFETY: an `NSArray` of `NSURL`, which is what is sent.
            let first = unsafe {
                let f: unsafe extern "C" fn(Id, Sel) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(
                    f(urls, objc::sel::get("firstObject")),
                    objc::sel::get("absoluteString"),
                )
            };
            // SAFETY: an `NSString` the URL answered with.
            let bytes = unsafe { foundation::string::bytes(first) };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            SEEN.lock().expect("no panic").push(text);
        }

        let delegate_class = corefoundation::base::register(
            c"RosemaryTestOpenUrlsDelegate",
            objc::class::lookup(c"NSObject"),
            size_of::<usize>(),
            false,
        );
        for (name, imp) in [
            (
                "applicationWillFinishLaunching:",
                will_finish as *const c_void,
            ),
            (
                "applicationDidFinishLaunching:",
                did_finish as *const c_void,
            ),
        ] {
            delegate_class.add_method(objc::sel::get(name), imp, c"v24@0:8@16");
        }
        delegate_class.add_method(
            objc::sel::get("application:openURLs:"),
            open_urls as *const c_void,
            c"v32@0:8@16@24",
        );
        let delegate = objc::object::create_instance(delegate_class, 0);

        let class = objc::class::lookup(c"NSApplication").expect("NSApplication");
        let app = objc::object::create_instance(class, INSTANCE_SIZE);
        // SAFETY: an application this test made.
        unsafe { set_delegate(app, objc::sel::get("setDelegate:"), delegate) };
        foundation::processinfo::set_arguments(
            &[
                "guest",
                "-protocolString",
                "example-player://experiences/start?id=1",
            ]
            .map(Into::into),
        );
        // SAFETY: `-finishLaunching` is `void (id, SEL)`.
        unsafe { finish_launching(app, objc::sel::get("finishLaunching")) };
        foundation::processinfo::set_arguments(&["guest".into()]);

        assert_eq!(
            *SEEN.lock().expect("no panic"),
            ["will", "example-player://experiences/start?id=1", "did"]
        );
    }

    #[test]
    fn effective_appearance_is_the_programs_default() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSApplication").expect("NSApplication");
        let app = objc::object::create_instance(class, 0);
        let appearance =
            unsafe { effective_appearance(app, objc::sel::get("effectiveAppearance")) };
        let name = unsafe { crate::appearance::name(appearance, objc::sel::get("name")) };
        assert_eq!(appearance, crate::appearance::for_program());
        assert!(!name.is_null());
    }

    #[test]
    fn posted_events_honor_at_start_and_are_current_while_sent() {
        crate::bootstrap();
        static SEEN: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

        unsafe extern "C" fn record(receiver: Id, _sel: Sel, event: Id) {
            // SAFETY: `receiver` is the test application.
            let current = unsafe { current_event(receiver, objc::sel::get("currentEvent")) };
            assert_eq!(current, event);
            SEEN.lock()
                .expect("no panic")
                .push(crate::nsevent::kind_of(event));
        }

        SEEN.lock().expect("no panic").clear();
        let class = corefoundation::base::register(
            c"RosemaryTestPostedEventApplication",
            objc::class::lookup(c"NSApplication"),
            INSTANCE_SIZE,
            false,
        );
        class.add_method(
            objc::sel::get("sendEvent:"),
            record as *const c_void,
            c"v24@0:8@16",
        );
        let app = objc::object::create_instance(class, 0);
        let first = crate::nsevent::make(crate::nsevent::Fields {
            kind: crate::nsevent::kind::KEY_DOWN,
            ..Default::default()
        });
        let urgent = crate::nsevent::make(crate::nsevent::Fields {
            kind: crate::nsevent::kind::KEY_UP,
            ..Default::default()
        });

        // SAFETY: the objects and prototypes are the ones this test made.
        unsafe {
            post_event(app, objc::sel::get("postEvent:atStart:"), first, 0);
            post_event(app, objc::sel::get("postEvent:atStart:"), urgent, 1);
            dispatch_posted_events(app);
        }

        assert_eq!(
            *SEEN.lock().expect("no panic"),
            [crate::nsevent::kind::KEY_UP, crate::nsevent::kind::KEY_DOWN]
        );
        // SAFETY: outside `-sendEvent:` there is no current event.
        assert!(unsafe { current_event(app, objc::sel::get("currentEvent")) }.is_null());
    }

    /// `-run` turns the run loop, and `-stop:` ends it.
    #[test]
    fn run_services_the_run_loop_until_it_is_stopped() {
        crate::bootstrap();
        static FIRED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        static APP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

        unsafe extern "C" fn perform(_info: *mut std::ffi::c_void) {
            FIRED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let app = APP.load(std::sync::atomic::Ordering::SeqCst) as Id;
            // SAFETY: `-stop:` is `void (id, SEL, id)` and `app` is the one the
            // test made. Stopping from inside the callback rather than after a
            // few turns, because a source fires once per signal and a test that
            // waited for a second one would wait forever.
            unsafe { stop(app, objc::sel::get("stop:"), std::ptr::null_mut()) };
        }

        let app = objc::object::create_instance(
            objc::class::lookup(c"NSApplication").expect("NSApplication"),
            0,
        );
        APP.store(app as usize, std::sync::atomic::Ordering::SeqCst);

        let source = corefoundation::runloop::new_source(Some(perform), 0);
        let loop_ = corefoundation::runloop::main();
        loop_.add_source(&source, corefoundation::runloop::DEFAULT_MODE);
        source.signal();

        // SAFETY: `-run` is `void (id, SEL)` and `app` is an application.
        unsafe { run(app, objc::sel::get("run")) };

        assert_eq!(
            FIRED.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the source was not serviced from inside -run"
        );
        loop_.remove_source(&source, corefoundation::runloop::DEFAULT_MODE);
    }

    #[test]
    fn what_a_turn_autoreleases_is_released_when_it_ends() {
        crate::bootstrap();
        static OBJECT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        thread_local! {
            static PERFORMED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
        }

        // The main loop is shared with every other test in the process, so
        // another test's turn may service the source; only this one counts.
        unsafe extern "C" fn perform(_info: *mut std::ffi::c_void) {
            if PERFORMED.get().is_none() {
                return;
            }
            let object = OBJECT.load(std::sync::atomic::Ordering::SeqCst) as Id;
            objc::arc::autorelease(objc::arc::retain(object));
            PERFORMED.set(Some(true));
        }

        let app = objc::object::create_instance(
            objc::class::lookup(c"NSApplication").expect("NSApplication"),
            0,
        );
        let object =
            objc::object::create_instance(objc::class::lookup(c"NSObject").expect("NSObject"), 0);
        OBJECT.store(object as usize, std::sync::atomic::Ordering::SeqCst);
        let source = corefoundation::runloop::new_source(Some(perform), 0);
        let loop_ = corefoundation::runloop::main();
        loop_.add_source(&source, corefoundation::runloop::DEFAULT_MODE);
        PERFORMED.set(Some(false));

        let outer = objc::arc::pool_push();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while PERFORMED.get() != Some(true) && std::time::Instant::now() < deadline {
            source.signal();
            // SAFETY: `app` is an application.
            unsafe { turn(app, corefoundation::runloop::DEFAULT_MODE) };
        }
        assert_eq!(PERFORMED.get(), Some(true), "the source never fired here");
        assert_eq!(objc::arc::retain_count(object), 1);
        objc::arc::pool_pop(outer);
        loop_.remove_source(&source, corefoundation::runloop::DEFAULT_MODE);
        objc::arc::release(object);
    }
}

/// `void NSBeep(void)`: the alert sound. There is no system sound to play.
#[allow(non_snake_case)]
pub extern "C" fn NSBeep() {}
