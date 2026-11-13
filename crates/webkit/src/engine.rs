use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::OnceLock;

use objc::object::Id;

use crate::compositor::{Composition, Compositor};
use crate::gtk::{self, Api, Ptr};

pub struct Engine {
    pub api: &'static Api,
    /// Where GTK's windows are shown: every view's pictures and input go
    /// through it.
    pub compositor: Compositor,
    /// The `WebKitNetworkSession` every view shares, as every `WKWebView` with
    /// the default data store shares one cookie jar.
    pub session: usize,
    thread: u32,
}

static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();

/// The engine, started on first use; `None` where WebKitGTK is missing or
/// cannot open a display, or on a thread other than the one it runs on.
pub fn get() -> Option<&'static Engine> {
    let engine = ENGINE.get_or_init(|| {
        let started = start();
        if let Err(why) = &started {
            eprintln!("rosemary: webkit: web views will have no content: {why}");
        }
        started
    });
    let engine = engine.as_ref().ok()?;
    if engine.thread != darwin::pthread::id() {
        warn_once("a web view was used off the thread WebKitGTK runs on; ignored");
        return None;
    }
    Some(engine)
}

/// The engine if something has already started it, without starting it.
fn running() -> Option<&'static Engine> {
    let engine = ENGINE.get()?.as_ref().ok()?;
    (engine.thread == darwin::pthread::id()).then_some(engine)
}

fn start() -> Result<Engine, String> {
    if std::env::var_os("ROSEMARY_WEBKIT").is_some_and(|value| value == "0") {
        return Err("disabled by ROSEMARY_WEBKIT=0".to_string());
    }
    if appkit::decor::config().decorations == appkit::decor::Decorations::Server
        && std::env::var_os("WAYLAND_DISPLAY").is_some()
    {
        // SDL may have loaded libdecor's GTK 3 plugin for the host titlebar,
        // and GTK 3 and the GTK 4 loaded next cannot share a process.
        warn_once(
            "ROSEMARY_DECORATIONS=server lets libdecor load GTK 3 next to \
             WebKit's GTK 4; if the program crashes, use the default client decorations",
        );
    }
    let api = gtk::api()?;
    let mode = std::env::var("ROSEMARY_WEBKIT_GPU").unwrap_or("gtk".into());
    let composition = match mode.as_str() {
        "0" => Composition::Cpu,
        _ => Composition::Gpu(appkit::backing::importer()),
    };
    let compositor = Compositor::start(wake, composition)?;
    // GTK would otherwise call `setlocale(LC_ALL, "")`, which changes how the
    // host's C library formats numbers under the guest's feet.
    // SAFETY: safe to call before anything else in GTK.
    unsafe { (api.gtk_disable_setlocale)() };
    // GTK's display is the compositor, never the host's. GTK opens its
    // display in `gtk_init` from the environment alone, and refuses to have
    // one opened for it earlier, so the environment says where the compositor
    // is for that one call. The guest reads its own copy of the environment,
    // which this does not touch.
    let socket = compositor.socket.clone();
    let mut variables = vec![
        ("WAYLAND_DISPLAY", socket.as_os_str()),
        ("GDK_BACKEND", std::ffi::OsStr::new("wayland")),
    ];
    // GTK draws with cairo unless asked: its GPU renderers take WebKit's
    // pages as dma-bufs, and that costs a page a frame of latency - about 70
    // frames a second where cairo keeps up with 110 on a 144 Hz display.
    // Without a GPU compositor they could only run in software anyway.
    if !compositor.gpu || mode != "gtk" {
        variables.push(("GDK_DISABLE", std::ffi::OsStr::new("gl,vulkan")));
    }
    let started = with_environment(
        &variables,
        // SAFETY: both are safe to call before anything else in GTK.
        || unsafe {
            (api.gdk_set_allowed_backends)(c"wayland".as_ptr());
            (api.gtk_init_check)() != 0
        },
    );
    if !started {
        return Err("GTK could not connect to the web view compositor".to_string());
    }
    // SAFETY: the display manager GTK just set up.
    let display =
        unsafe { (api.gdk_display_manager_get_default_display)((api.gdk_display_manager_get)()) };
    // SAFETY: the display GTK now runs on.
    unsafe { gtk::make_windows_transparent(api, display) };
    let identifier = foundation::bundle::main()
        .and_then(|bundle| bundle.identifier())
        .unwrap_or("rosemary")
        .to_string();
    let home = foundation::host_home_directory();
    let data = home.join("Library/WebKit").join(&identifier);
    let cache = home.join("Library/Caches").join(&identifier).join("WebKit");
    let data = gtk::c_string(&data.to_string_lossy());
    let cache = gtk::c_string(&cache.to_string_lossy());
    // SAFETY: two NUL-terminated paths, which the session copies.
    let session = unsafe { (api.webkit_network_session_new)(data.as_ptr(), cache.as_ptr()) };
    if session.is_null() {
        return Err("WebKitGTK made no network session".to_string());
    }
    appkit::event::add_pump_hook(pump);
    Ok(Engine {
        api,
        compositor,
        session: session as usize,
        thread: darwin::pthread::id(),
    })
}

/// Run `f` with `variables` set in the host's environment, then put back
/// what was there.
fn with_environment<T>(variables: &[(&str, &std::ffi::OsStr)], f: impl FnOnce() -> T) -> T {
    let saved: Vec<_> = variables
        .iter()
        .map(|(name, _)| (*name, std::env::var_os(name)))
        .collect();
    for (name, value) in variables {
        // SAFETY: not strictly - another host thread could be reading the
        // environment. The window is one `gtk_init` long, at the first web
        // view, and no other host code reads these two variables.
        unsafe { std::env::set_var(name, value) };
    }
    let result = f();
    for (name, value) in saved {
        // SAFETY: as above.
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
    result
}

/// Get the main thread to the pump, from the compositor's thread.
fn wake() {
    corefoundation::runloop::main().wake();
}

pub fn warn_once(message: &str) {
    static WARNED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if !warned.iter().any(|seen| seen == message) {
        eprintln!("rosemary: webkit: {message}");
        warned.push(message.to_string());
    }
}

pub fn tracing() -> bool {
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("ROSEMARY_TRACE_WEBKIT").is_some())
}

thread_local! {
    static DEFERRED: RefCell<Vec<Box<dyn FnOnce()>>> = const { RefCell::new(Vec::new()) };
}

/// Run `work` at the next turn of the pump.
///
/// For a completion handler WebKit calls asynchronously: calling one before
/// the method that took it has returned is a reentrancy its caller does not
/// expect.
pub fn defer(work: impl FnOnce() + 'static) {
    DEFERRED.with(|deferred| deferred.borrow_mut().push(Box::new(work)));
}

fn pump() {
    let Some(engine) = running() else {
        return;
    };
    let pool = objc::arc::pool_push();
    // Bounded, as `drain_main` is: a context that always has work would
    // otherwise keep the run loop from ever getting back to input and drawing.
    for _ in 0..64 {
        // SAFETY: the default context, iterated on the thread GTK runs on.
        if unsafe { (engine.api.g_main_context_iteration)(std::ptr::null_mut(), 0) } == 0 {
            break;
        }
    }
    let work = DEFERRED.with(|deferred| std::mem::take(&mut *deferred.borrow_mut()));
    for item in work {
        item();
    }
    crate::web_view::tick(engine);
    restore_guest_gl(engine.api);
    objc::arc::pool_pop(pool);
}

/// GTK and the embedded compositor bind EGL directly, bypassing SDL's TLS.
/// Clear both libraries' bookkeeping before restoring the guest's binding;
/// otherwise SDL may skip the native bind because its cached pointers match.
fn restore_guest_gl(api: &Api) {
    let context = sdl3::video::gl_current_context();
    let window = sdl3::video::gl_current_window();
    // SAFETY: called on GTK's thread after its rendering work has returned.
    unsafe { (api.gdk_gl_context_clear_current)() };
    if !context.is_null() && !sdl3::video::egl::backs(context) {
        sdl3::video::gl_make_current(std::ptr::null_mut(), std::ptr::null_mut());
        if !sdl3::video::gl_make_current(window, context) {
            warn_once("could not restore the guest OpenGL context after web rendering");
        }
    }
}

/// A guest block kept past the call that passed it.
pub struct Block(usize);

impl Block {
    pub fn copy(block: Id) -> Option<Block> {
        (!block.is_null()).then(|| Block(objc::block::copy(block) as usize))
    }

    fn invoke(&self) -> usize {
        // SAFETY: a block literal, whose `invoke` is its third word.
        unsafe { *((self.0 as *const u8).add(BLOCK_INVOKE_OFFSET) as *const usize) }
    }

    /// Call a `void (^)(id)`.
    pub fn call1(&self, first: Id) {
        let invoke = self.invoke();
        if invoke == 0 {
            return;
        }
        // SAFETY: the caller took this block for a handler of this prototype.
        unsafe {
            let f: unsafe extern "C" fn(Id, Id) = std::mem::transmute(invoke);
            f(self.0 as Id, first);
        }
    }

    /// Call a `void (^)(id, id)`.
    pub fn call2(&self, first: Id, second: Id) {
        let invoke = self.invoke();
        if invoke == 0 {
            return;
        }
        unsafe {
            let f: unsafe extern "C" fn(Id, Id, Id) = std::mem::transmute(invoke);
            f(self.0 as Id, first, second);
        }
    }

    /// Call a `void (^)(void)`.
    pub fn call0(&self) {
        let invoke = self.invoke();
        if invoke == 0 {
            return;
        }
        unsafe {
            let f: unsafe extern "C" fn(Id) = std::mem::transmute(invoke);
            f(self.0 as Id);
        }
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        objc::block::release(self.0 as Id);
    }
}

/// `isa`, `flags`, `reserved`, then `invoke`.
const BLOCK_INVOKE_OFFSET: usize = 16;

#[repr(C)]
struct Descriptor {
    reserved: u64,
    size: u64,
}

/// A block made here for a guest to call, carrying `context` after the
/// literal's fields.
#[repr(C)]
pub struct HostBlock<T> {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: *const c_void,
    descriptor: *const Descriptor,
    pub context: T,
}

/// A block whose `invoke` is `invoke`, which receives the block itself first.
///
/// Marked global, so a guest's copy of it is the block itself and a release
/// does nothing. It is leaked: a guest may hold it, and release it, after it has
/// been called, and a few words per navigation is the price of never freeing
/// it under someone.
pub fn host_block<T>(invoke: *const c_void, context: T) -> Id {
    let isa = objc::class::lookup(c"__NSGlobalBlock__")
        .map(|class| objc::class::ptr(class) as *const c_void)
        .expect("registered by `objc::block`");
    let descriptor = Box::leak(Box::new(Descriptor {
        reserved: 0,
        size: size_of::<HostBlock<T>>() as u64,
    }));
    Box::leak(Box::new(HostBlock {
        isa,
        // `BLOCK_IS_GLOBAL`.
        flags: 1 << 28,
        reserved: 0,
        invoke,
        descriptor,
        context,
    })) as *mut HostBlock<T> as Id
}

/// Whether `object` answers `selector`, asked the way a caller would ask, so
/// that a proxy's forwarding is honoured.
pub fn responds(object: Id, selector: &str) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: a live object; `-respondsToSelector:` is `BOOL (id, SEL, SEL)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, objc::sel::Sel, objc::sel::Sel) -> u8 =
            std::mem::transmute(objc::msg::entry());
        send(
            object,
            objc::sel::get("respondsToSelector:"),
            objc::sel::get(selector),
        ) != 0
    }
}

/// An autoreleased string.
pub fn string(text: &str) -> Id {
    objc::arc::autorelease(foundation::string::from_str(text))
}

/// A string object's text, or empty for nil.
pub fn text_of(string: Id) -> String {
    if string.is_null() {
        return String::new();
    }
    // SAFETY: null or a string object, which every caller passes.
    String::from_utf8_lossy(&unsafe { foundation::string::bytes(string) }).into_owned()
}

/// Release a GObject reference held as an address.
pub fn unref(api: &Api, object: usize) {
    if object != 0 {
        // SAFETY: a reference the caller owns.
        unsafe { (api.g_object_unref)(object as Ptr) };
    }
}

#[cfg(all(test, target_os = "linux"))]
mod gl_tests {
    use super::*;

    #[test]
    #[ignore = "requires a host EGL driver and WebKitGTK"]
    fn web_rendering_restores_native_gl_even_when_sdl_cached_state_matches() {
        appkit::bootstrap();
        let api = gtk::api().expect("WebKitGTK");
        let window = sdl3::video::create_window(
            c"GL restoration test",
            2,
            2,
            sdl3::video::flags::OPENGL | sdl3::video::flags::HIDDEN,
        )
        .expect("GL window");
        let guest = sdl3::video::gl_create_context(window).expect("guest context");
        let foreign = sdl3::video::gl_create_context(window).expect("foreign context");
        assert!(sdl3::video::egl::backs(foreign), "test needs EGL");
        assert!(sdl3::video::gl_make_current(window, guest));
        let symbol = |name: &std::ffi::CStr| {
            // SAFETY: a static symbol name in the EGL driver already loaded by SDL.
            let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            assert!(!address.is_null(), "missing {name:?}");
            address
        };
        // SAFETY: checked EGL entry points, with their declared prototypes.
        unsafe {
            let current_display: unsafe extern "C" fn() -> Ptr =
                std::mem::transmute(symbol(c"eglGetCurrentDisplay"));
            let current_surface: unsafe extern "C" fn(u32) -> Ptr =
                std::mem::transmute(symbol(c"eglGetCurrentSurface"));
            let make_current: unsafe extern "C" fn(Ptr, Ptr, Ptr, Ptr) -> u32 =
                std::mem::transmute(symbol(c"eglMakeCurrent"));
            const EGL_DRAW: u32 = 0x3059;
            const EGL_READ: u32 = 0x305A;
            assert_ne!(
                make_current(
                    current_display(),
                    current_surface(EGL_DRAW),
                    current_surface(EGL_READ),
                    foreign
                ),
                0
            );
        }
        assert_eq!(sdl3::video::gl_current_context(), guest);
        assert!(sdl3::video::egl::backs(foreign));
        restore_guest_gl(api);
        assert_eq!(sdl3::video::gl_current_context(), guest);
        assert_eq!(sdl3::video::gl_current_window(), window);
        assert!(sdl3::video::egl::backs(guest));
        sdl3::video::gl_make_current(std::ptr::null_mut(), std::ptr::null_mut());
        sdl3::video::gl_destroy_context(foreign);
        sdl3::video::gl_destroy_context(guest);
        sdl3::video::destroy_window(window);
    }
}
