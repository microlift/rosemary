#![allow(non_snake_case)]

use std::ffi::c_void;

use foundation::geometry::NSRect;
use objc::object::Id;
use objc::sel::Sel;
use static_init::dynamic;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct PixelFormat {
    isa: *const c_void,
    /// The `NSOpenGLPixelFormatAttribute` list, terminated as the guest wrote
    /// it. Leaked with the object, which never outlives the process.
    attributes: *mut Vec<u32>,
}

#[repr(C)]
pub struct Context {
    isa: *const c_void,
    sdl: sdl3::GlContext,
    /// The view, retained: a context outliving its view and then flushing would
    /// swap a window that had gone.
    view: Id,
    format: Id,
    /// The `CGLContextObj` behind the SDL context, recorded the first time it is
    /// made current - see [`cgl_context`].
    cgl: *mut c_void,
    /// What it takes to be current on more than one thread at once, allocated
    /// on first use by [`shared_of`]. Leaked with the object.
    shared: *mut Cell,
}

/// Storage added by `NSOpenGLView` after its `NSView` body.
#[repr(C)]
struct OpenGLView {
    format: Id,
    context: Id,
}

/// [`Shared`], and the signal a thread waiting for the context sleeps on.
struct Cell {
    state: std::sync::Mutex<Shared>,
    /// Raised when the thread holding the SDL context gives it up.
    released: std::sync::Condvar,
}

/// The part of a context that more than one thread reaches.
///
/// AppKit lets a program send `-makeCurrentContext` to one context from two
/// threads - a game does exactly that to hand drawing from the thread that set
/// the renderer up to the thread that draws - and leaves serialising the two to
/// the program. The GL underneath refuses: a context current on one thread
/// cannot be made current on another, and only the thread holding it can let it
/// go, so the second thread's request would fail outright.
///
/// So the context is *handed over*: the thread asking for one another thread
/// holds says so, and the holder gives it up at its next pass through the event
/// pump - see [`release_for_other_threads`]. That is the case this is really
/// for, because it is what a program with a render thread does: set the
/// renderer up on the thread that starts, then draw on the thread that draws.
///
/// A holder that never comes back to its event loop would hang the program, so
/// the wait gives up and the asking thread gets a context of its own in the
/// same *share group* instead: a distinct context through which every shared
/// object - textures, buffers, shaders, programs, renderbuffers - is the same
/// object. Vertex arrays, framebuffers and queries are **not** shared by
/// OpenGL, so that fallback draws what a program that made none of those on the
/// other thread would draw, and nothing otherwise.
#[derive(Default)]
struct Shared {
    /// The thread the SDL context itself is current on, while one holds it.
    owner: Option<std::thread::ThreadId>,
    /// Whether another thread is waiting for the holder to give it up.
    wanted: bool,
    /// Whether the two below have been read off the SDL context yet, which can
    /// only happen on the thread it is current on.
    probed: bool,
    /// Whether a sibling can be made at all, which needs an EGL-backed SDL.
    egl: bool,
    /// The version and profile the SDL context came back as, which a sibling is
    /// asked for so that the guest sees the same GL through either. `None` for
    /// a context older than GL 3.0, which has no version to ask for.
    version: Option<(i32, i32, i32)>,
    /// A context per thread that asked while another thread held the SDL one.
    siblings: Vec<(std::thread::ThreadId, sdl3::GlContext)>,
}

pub const PIXEL_FORMAT_INSTANCE_SIZE: usize = size_of::<PixelFormat>();
pub const CONTEXT_INSTANCE_SIZE: usize = size_of::<Context>();
// `corefoundation::base::register` takes the complete instance size, not the
// number of bytes a class adds. If this were just `OpenGLView`'s size,
// `NSOpenGLView` would add no storage after the much larger `NSView`: these two
// fields would then occupy the first two ivars of a guest subclass. Geometry
// Dash exposes that collision when its `eventDelegate_` becomes the retained
// pixel format and receives `-queueEvent:selector:`.
pub const VIEW_INSTANCE_SIZE: usize = crate::view::INSTANCE_SIZE + size_of::<OpenGLView>();

fn gl_view_of<'a>(receiver: Id) -> &'a mut OpenGLView {
    let base = objc::class::lookup(c"NSView")
        .expect("NSView is declared before NSOpenGLView")
        .instance_size();
    // SAFETY: NSOpenGLView adds exactly this body after NSView's storage, and
    // every subclass inherits it at the same offset.
    unsafe { &mut *receiver.cast::<u8>().add(base).cast::<OpenGLView>() }
}

/// `NSOpenGLPixelFormatAttribute`, the ones a program in this tree has asked
/// for.
///
/// The list is not the whole header. An attribute nobody has used is one whose
/// SDL counterpart has not been checked, and guessing at it would ask SDL for a
/// visual the program did not request.
mod attribute {
    // Every `NSOpenGLPixelFormatAttribute` in `NSOpenGL.h`, whether or not it
    // maps to anything. The list has to be complete rather than convenient:
    // `takes_value` is what tells the walk how far to step, so an attribute
    // missing from it stops the walk - and one classified wrongly reads the
    // next attribute as this one's value and silently turns the list into a
    // different list.
    pub const ALL_RENDERERS: u32 = 1;
    pub const TRIPLE_BUFFER: u32 = 3;
    pub const DOUBLE_BUFFER: u32 = 5;
    pub const STEREO: u32 = 6;
    pub const AUX_BUFFERS: u32 = 7;
    pub const COLOR_SIZE: u32 = 8;
    pub const ALPHA_SIZE: u32 = 11;
    pub const DEPTH_SIZE: u32 = 12;
    pub const STENCIL_SIZE: u32 = 13;
    pub const ACCUM_SIZE: u32 = 14;
    pub const MINIMUM_POLICY: u32 = 51;
    pub const MAXIMUM_POLICY: u32 = 52;
    pub const OFF_SCREEN: u32 = 53;
    pub const FULL_SCREEN: u32 = 54;
    pub const SAMPLE_BUFFERS: u32 = 55;
    pub const SAMPLES: u32 = 56;
    pub const AUX_DEPTH_STENCIL: u32 = 57;
    pub const COLOR_FLOAT: u32 = 58;
    pub const MULTISAMPLE: u32 = 59;
    pub const SUPERSAMPLE: u32 = 60;
    pub const SAMPLE_ALPHA: u32 = 61;
    pub const RENDERER_ID: u32 = 70;
    pub const SINGLE_RENDERER: u32 = 71;
    pub const NO_RECOVERY: u32 = 72;
    pub const ACCELERATED: u32 = 73;
    pub const CLOSEST_POLICY: u32 = 74;
    pub const ROBUST: u32 = 75;
    pub const BACKING_STORE: u32 = 76;
    pub const MP_SAFE: u32 = 78;
    pub const WINDOW: u32 = 80;
    pub const MULTI_SCREEN: u32 = 81;
    pub const COMPLIANT: u32 = 83;
    pub const SCREEN_MASK: u32 = 84;
    pub const PIXEL_BUFFER: u32 = 90;
    pub const REMOTE_PIXEL_BUFFER: u32 = 91;
    pub const ALLOW_OFFLINE_RENDERERS: u32 = 96;
    pub const ACCELERATED_COMPUTE: u32 = 97;
    pub const OPENGL_PROFILE: u32 = 99;
    pub const VIRTUAL_SCREEN_COUNT: u32 = 128;

    /// The `NSOpenGLProfileVersion*` values `OPENGL_PROFILE` takes.
    pub const PROFILE_LEGACY: u32 = 0x1000;
    pub const PROFILE_3_2_CORE: u32 = 0x3200;
    pub const PROFILE_4_1_CORE: u32 = 0x4100;

    /// The attributes that are followed by a value, per the header's own
    /// comments - "number of", "bit mask of", "request ... by ID".
    pub const VALUED: &[u32] = &[
        AUX_BUFFERS,
        COLOR_SIZE,
        ALPHA_SIZE,
        DEPTH_SIZE,
        STENCIL_SIZE,
        ACCUM_SIZE,
        SAMPLE_BUFFERS,
        SAMPLES,
        RENDERER_ID,
        SCREEN_MASK,
        OPENGL_PROFILE,
        VIRTUAL_SCREEN_COUNT,
    ];

    /// The attributes that stand alone, each meaning "yes" by being present.
    pub const STANDALONE: &[u32] = &[
        ALL_RENDERERS,
        TRIPLE_BUFFER,
        DOUBLE_BUFFER,
        STEREO,
        MINIMUM_POLICY,
        MAXIMUM_POLICY,
        OFF_SCREEN,
        FULL_SCREEN,
        AUX_DEPTH_STENCIL,
        COLOR_FLOAT,
        MULTISAMPLE,
        SUPERSAMPLE,
        SAMPLE_ALPHA,
        SINGLE_RENDERER,
        NO_RECOVERY,
        ACCELERATED,
        CLOSEST_POLICY,
        ROBUST,
        BACKING_STORE,
        MP_SAFE,
        WINDOW,
        MULTI_SCREEN,
        COMPLIANT,
        PIXEL_BUFFER,
        REMOTE_PIXEL_BUFFER,
        ALLOW_OFFLINE_RENDERERS,
        ACCELERATED_COMPUTE,
    ];

    /// Whether the attribute is followed by a value rather than standing alone.
    pub fn takes_value(attribute: u32) -> Option<bool> {
        if VALUED.contains(&attribute) {
            return Some(true);
        }
        STANDALONE.contains(&attribute).then_some(false)
    }
}

fn format_of<'a>(receiver: Id) -> &'a mut PixelFormat {
    // SAFETY: a pixel format of this crate's.
    unsafe { &mut *(receiver as *mut PixelFormat) }
}

/// `-[NSOpenGLPixelFormat CGLPixelFormatObj]`.
///
/// CGL pixel formats are opaque handles. The AppKit object already owns the
/// complete chosen-format state in this runtime, so its stable object address
/// is also the handle, with the same borrowed lifetime as Apple's accessor.
unsafe extern "C" fn cgl_pixel_format(receiver: Id, _sel: Sel) -> *mut c_void {
    receiver.cast()
}

fn context_of<'a>(receiver: Id) -> &'a mut Context {
    // SAFETY: a context of this crate's.
    unsafe { &mut *(receiver as *mut Context) }
}

/// The context's [`Shared`], made on first use.
///
/// Under a lock of its own rather than a plain null check: the second thread to
/// send `-makeCurrentContext` may be the first to reach this, and two threads
/// allocating here at once would leave one of them holding a `Shared` the other
/// never sees - which is the one piece of state that only works by being the
/// same on both.
fn shared_of(context: &mut Context) -> &'static Cell {
    static CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CREATION.lock().unwrap_or_else(|e| e.into_inner());
    if context.shared.is_null() {
        context.shared = Box::into_raw(Box::new(Cell {
            state: std::sync::Mutex::new(Shared::default()),
            released: std::sync::Condvar::new(),
        }));
    }
    // SAFETY: allocated just above, and leaked with the object it belongs to.
    unsafe { &*context.shared }
}

unsafe extern "C" fn init_with_attributes(receiver: Id, _sel: Sel, attributes: *const u32) -> Id {
    let mut list = Vec::new();
    if !attributes.is_null() {
        // A pixel format attribute list is terminated by a zero, and Apple's
        // own headers cap it at nothing in particular - so this stops at the
        // terminator or at a length no real list reaches, rather than walking
        // guest memory until it faults.
        for i in 0..256 {
            // SAFETY: the caller passed a zero-terminated list, and the bound
            // stops a missing terminator from running off the end.
            let value = unsafe { *attributes.add(i) };
            if value == 0 {
                break;
            }
            list.push(value);
        }
    }
    format_of(receiver).attributes = Box::into_raw(Box::new(list));
    receiver
}

/// `-getValues:forAttribute:forVirtualScreen:`
///
/// Apple answers what the pixel format it *chose* actually provides. SDL has no
/// way to ask that - there is no `SDL_GL_GetAttribute` in these bindings - so
/// this answers what the format was asked for, and zero for an attribute the
/// list does not mention. That is the truth about the request rather than about
/// the visual, and the two differ only where the driver could not give the
/// program what it asked for.
///
/// # Safety
///
/// `values` is null or writable for one `GLint`.
unsafe extern "C" fn get_values(
    receiver: Id,
    _sel: Sel,
    values: *mut i32,
    attribute: u32,
    screen: i32,
) {
    if values.is_null() {
        return;
    }
    if screen != 0 {
        objc::fail!(
            "-[NSOpenGLPixelFormat getValues:forAttribute:forVirtualScreen:]: this format has \
             one virtual screen, so {screen} names no renderer"
        );
    }
    // One renderer, so one virtual screen - see `crate::cgl`. Answered here
    // rather than from the list because it is a property of the machine and not
    // of what the program asked for.
    if attribute == attribute::VIRTUAL_SCREEN_COUNT {
        // SAFETY: the caller states it is writable.
        unsafe { *values = 1 };
        return;
    }
    let Some(takes_value) = attribute::takes_value(attribute) else {
        objc::fail!(
            "-[NSOpenGLPixelFormat getValues:forAttribute:forVirtualScreen:]: attribute \
             {attribute} is not one this runtime knows"
        );
    };
    let list = match receiver.is_null() {
        true => None,
        // SAFETY: a pixel format this crate made; the pointer is its own.
        false => unsafe { format_of(receiver).attributes.as_ref() },
    };
    let mut answer = 0;
    if let Some(list) = list {
        let mut i = 0;
        while i < list.len() {
            let name = list[i];
            // A name the walk cannot classify would make every step after it
            // read a value as an attribute, so the walk stops rather than
            // answering from a list it has lost its place in.
            let Some(valued) = attribute::takes_value(name) else {
                break;
            };
            if name == attribute {
                answer = match takes_value {
                    true => *list.get(i + 1).unwrap_or(&0) as i32,
                    false => 1,
                };
                break;
            }
            i += 1 + valued as usize;
        }
    }
    // SAFETY: the caller states it is writable.
    unsafe { *values = answer };
}

/// Apply the format's attributes to SDL, for the context about to be created.
///
/// Every attribute the list holds is either mapped or **refused by number**.
/// Silently skipping one is how a program that asked for a depth buffer gets a
/// context without one and fails much later, drawing with no depth test and no
/// diagnostic anywhere.
fn apply(format: Id) {
    use sdl3::video::attr;
    let Some(list) = (match format.is_null() {
        true => None,
        // SAFETY: a pixel format this crate made; the pointer is its own.
        false => unsafe { format_of(format).attributes.as_ref() },
    }) else {
        return;
    };

    let mut i = 0;
    while i < list.len() {
        let name = list[i];
        let Some(takes_value) = attribute::takes_value(name) else {
            objc::fail!(
                "-[NSOpenGLContext initWithFormat:]: pixel format attribute {name} is not one \
                 this runtime knows, and skipping it would change the list"
            );
        };
        let value = match takes_value {
            true => *list.get(i + 1).unwrap_or(&0) as i32,
            false => 1,
        };
        i += 1 + takes_value as usize;

        match name {
            // The ones SDL asks the driver for directly.
            attribute::DOUBLE_BUFFER => sdl3::video::gl_set_attribute(attr::DOUBLEBUFFER, 1),
            attribute::TRIPLE_BUFFER => sdl3::video::gl_set_attribute(attr::DOUBLEBUFFER, 1),
            attribute::STEREO => sdl3::video::gl_set_attribute(attr::STEREO, 1),
            attribute::COLOR_SIZE => sdl3::video::gl_set_attribute(attr::BUFFER_SIZE, value),
            attribute::ALPHA_SIZE => sdl3::video::gl_set_attribute(attr::ALPHA_SIZE, value),
            attribute::DEPTH_SIZE => sdl3::video::gl_set_attribute(attr::DEPTH_SIZE, value),
            attribute::STENCIL_SIZE => sdl3::video::gl_set_attribute(attr::STENCIL_SIZE, value),
            attribute::SAMPLE_BUFFERS | attribute::MULTISAMPLE => {
                sdl3::video::gl_set_attribute(attr::MULTISAMPLEBUFFERS, value.max(1))
            }
            attribute::SAMPLES => sdl3::video::gl_set_attribute(attr::MULTISAMPLESAMPLES, value),
            // "Accelerated", "no recovery" and "no software renderer" are three
            // spellings of the same request.
            attribute::ACCELERATED | attribute::NO_RECOVERY | attribute::ACCELERATED_COMPUTE => {
                sdl3::video::gl_set_attribute(attr::ACCELERATED_VISUAL, 1)
            }
            attribute::OPENGL_PROFILE => match value as u32 {
                attribute::PROFILE_LEGACY => sdl3::video::gl_set_attribute(
                    attr::CONTEXT_PROFILE_MASK,
                    sdl3::video::profile::COMPATIBILITY,
                ),
                attribute::PROFILE_3_2_CORE => {
                    sdl3::video::gl_set_attribute(
                        attr::CONTEXT_PROFILE_MASK,
                        sdl3::video::profile::CORE,
                    ) && sdl3::video::gl_set_attribute(attr::CONTEXT_MAJOR_VERSION, 3)
                        && sdl3::video::gl_set_attribute(attr::CONTEXT_MINOR_VERSION, 2)
                }
                attribute::PROFILE_4_1_CORE => {
                    sdl3::video::gl_set_attribute(
                        attr::CONTEXT_PROFILE_MASK,
                        sdl3::video::profile::CORE,
                    ) && sdl3::video::gl_set_attribute(attr::CONTEXT_MAJOR_VERSION, 4)
                        && sdl3::video::gl_set_attribute(attr::CONTEXT_MINOR_VERSION, 1)
                }
                other => objc::fail!(
                    "-[NSOpenGLContext initWithFormat:]: NSOpenGLPFAOpenGLProfile {other:#x} is \
                     not a profile this runtime maps"
                ),
            },

            //
            // Each of these describes a choice SDL has already made, or a
            // renderer selection that means nothing when there is one driver.
            // They are listed rather than caught by a wildcard so that the set
            // is a decision a reader can check against the header, which is the
            // whole point of refusing an attribute that is in neither list.
            //
            // The policies say how to pick among visuals SDL picks for us; the
            // renderer selectors name a specific GPU; a window, an offscreen or
            // a pixel buffer is a *drawable kind*, and the drawable here is
            // always the window SDL made; an accumulation buffer is not
            // something SDL requests by total size, and no GL 2.1 program in
            // this tree has used one.
            attribute::ALL_RENDERERS
            | attribute::AUX_BUFFERS
            | attribute::ACCUM_SIZE
            | attribute::MINIMUM_POLICY
            | attribute::MAXIMUM_POLICY
            | attribute::CLOSEST_POLICY
            | attribute::AUX_DEPTH_STENCIL
            | attribute::COLOR_FLOAT
            | attribute::SUPERSAMPLE
            | attribute::SAMPLE_ALPHA
            | attribute::RENDERER_ID
            | attribute::SINGLE_RENDERER
            | attribute::ROBUST
            | attribute::BACKING_STORE
            | attribute::MP_SAFE
            | attribute::COMPLIANT
            | attribute::MULTI_SCREEN
            | attribute::ALLOW_OFFLINE_RENDERERS
            | attribute::SCREEN_MASK
            | attribute::VIRTUAL_SCREEN_COUNT
            | attribute::WINDOW
            | attribute::OFF_SCREEN
            | attribute::FULL_SCREEN
            | attribute::PIXEL_BUFFER
            | attribute::REMOTE_PIXEL_BUFFER => true,

            _ => unreachable!("every attribute above is one `takes_value` classified"),
        };
    }
}

unsafe extern "C" fn init_with_format(receiver: Id, _sel: Sel, format: Id, _share: Id) -> Id {
    let context = context_of(receiver);
    context.format = objc::arc::retain(format);
    receiver
}

/// `-[NSOpenGLView initWithFrame:pixelFormat:]`.
///
/// The view owns both the requested format and the context AppKit creates for
/// it. The context is pointed back at the view immediately; it remains lazy at
/// the SDL layer until the guest makes it current.
unsafe extern "C" fn init_view_with_frame(
    receiver: Id,
    _sel: Sel,
    frame: NSRect,
    format: Id,
) -> Id {
    // SAFETY: initialise the inherited NSView body directly, avoiding a second
    // dynamic dispatch through a guest subclass's own initializer.
    let receiver =
        unsafe { crate::view::init_with_frame(receiver, objc::sel::get("initWithFrame:"), frame) };
    let view = gl_view_of(receiver);
    view.format = objc::arc::retain(format);

    let class = objc::class::lookup(c"NSOpenGLContext").expect("NSOpenGLContext is declared");
    let context = objc::object::create_instance(class, 0);
    // SAFETY: a fresh context and the initializer's declared arguments.
    let context = unsafe {
        init_with_format(
            context,
            objc::sel::get("initWithFormat:shareContext:"),
            format,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: the context and view this initializer just established.
    unsafe { set_view(context, objc::sel::get("setView:"), receiver) };
    view.context = context;
    receiver
}

unsafe extern "C" fn view_pixel_format(receiver: Id, _sel: Sel) -> Id {
    gl_view_of(receiver).format
}

unsafe extern "C" fn open_gl_context(receiver: Id, _sel: Sel) -> Id {
    gl_view_of(receiver).context
}

unsafe extern "C" fn set_open_gl_context(receiver: Id, _sel: Sel, context: Id) {
    let view = gl_view_of(receiver);
    if view.context == context {
        return;
    }
    objc::arc::release(view.context);
    view.context = objc::arc::retain(context);
    if !context.is_null() {
        // SAFETY: an NSOpenGLContext supplied to its declared setter.
        unsafe { set_view(context, objc::sel::get("setView:"), receiver) };
    }
}

/// `-setView:`, which records the drawable and nothing more.
///
/// Creating the SDL context here would be the obvious place and is the wrong
/// one: AppKit lets a program make a context and send it `-makeCurrentContext`
/// *before* it has a view - the runner does exactly that - and a context created
/// only on `-setView:` is nil at the moment it is first used.
unsafe extern "C" fn set_view(receiver: Id, _sel: Sel, view: Id) {
    let context = context_of(receiver);
    objc::arc::release(context.view);
    context.view = objc::arc::retain(view);
}

/// `-clearDrawable`, which only lets go of the view.
///
/// The SDL context stays. It is not bound to a window until it is made current,
/// so a later `-setView:` replaces the drawable on its own and there is nothing
/// here that tearing the context down would fix.
unsafe extern "C" fn clear_drawable(receiver: Id, _sel: Sel) {
    let context = context_of(receiver);
    objc::arc::release(context.view);
    context.view = std::ptr::null_mut();
}

/// The window a context should draw into: its view's, or - before it has one -
/// whichever window the program has made.
///
/// SDL needs a window to *create* a context against, and AppKit does not. The
/// window is not part of the context on either side, though: `SDL_GL_MakeCurrent`
/// takes it again on every call, so binding a context made against one window to
/// another later is what the API is for rather than a workaround.
/// The `NSWindow` a context draws into, which is the one its view is in and
/// otherwise the program's current window.
fn drawable_window(context: &Context) -> Id {
    // SAFETY: the view is null or an object this runtime allocated.
    let window = unsafe { crate::view::window_of(context.view) };
    // SAFETY: null or a window this crate made.
    if !unsafe { crate::window::sdl_window(window) }.is_null() {
        return window;
    }
    match crate::window::key_window().is_null() {
        false => crate::window::key_window(),
        true => crate::window::all()
            .last()
            .copied()
            .unwrap_or(std::ptr::null_mut()),
    }
}

fn drawable(context: &Context) -> sdl3::Window {
    // SAFETY: null or a window this crate made.
    unsafe { crate::window::sdl_window(drawable_window(context)) }
}

/// A hidden window that exists only to hold a context with no view.
///
/// An `NSOpenGLContext` needs no window at all: `-makeCurrentContext` before
/// `-setView:` is legal, and is what a runner does to ask GL what it can do
/// before it opens anything. SDL has no such thing - `SDL_GL_CreateContext` and
/// `SDL_GL_MakeCurrent` both take a window - so one pixel of hidden window
/// stands in until the guest gives the context a view.
///
/// Never presented to: `-flushBuffer` swaps the context's *drawable*, which is
/// null for as long as this is all there is. Made once and kept, because a
/// context made against it may still be current when the real window arrives -
/// a context can be made current against a different window later, which is
/// what `drawable_window` relies on.
fn holding_window() -> sdl3::Window {
    static WINDOW: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *WINDOW.get_or_init(|| {
        let flags = sdl3::video::flags::OPENGL | sdl3::video::flags::HIDDEN;
        sdl3::video::create_window(c"rosetta3 offscreen GL", 1, 1, flags)
            .map_or(0, |window| window as usize)
    }) as sdl3::Window
}

/// Make the SDL context, if it is not there yet.
fn ensure(receiver: Id, sent: &str) -> sdl3::Window {
    let context = context_of(receiver);
    let object = drawable_window(context);
    // SAFETY: null or a window this crate made.
    let window = match unsafe { crate::window::sdl_window(object) }.is_null() {
        true => {
            let holding = holding_window();
            if holding.is_null() {
                objc::fail!(
                    "-[NSOpenGLContext {sent}]: this context has no view yet, and SDL could \
                     not make a window to hold it until it has one: {}",
                    sdl3::error()
                );
            }
            holding
        }
        false => {
            // A window is not made for OpenGL until a guest asks for a GL
            // context, because a window made for OpenGL cannot afterwards be
            // presented to with Vulkan. This is the asking, so the window is
            // rebuilt now.
            if !crate::window::rebuild_for_opengl(object) {
                objc::fail!(
                    "-[NSOpenGLContext {sent}]: SDL could not make this window an OpenGL \
                     window: {}",
                    sdl3::error()
                );
            }
            // The rebuild replaces it, so it is read back rather than
            // remembered.
            // SAFETY: a window this crate made, checked above.
            unsafe { crate::window::sdl_window(object) }
        }
    };
    if context.sdl.is_null() {
        apply(context.format);
        let Some(sdl) = sdl3::video::gl_create_context(window) else {
            objc::fail!(
                "-[NSOpenGLContext {sent}]: SDL could not create a GL context: {}",
                sdl3::error()
            );
        };
        context.sdl = sdl;
    }
    window
}

unsafe extern "C" fn view(receiver: Id, _sel: Sel) -> Id {
    context_of(receiver).view
}

thread_local! {
    /// The context this thread made current last, which `+currentContext`
    /// answers. Per thread because that is what AppKit promises: a render
    /// thread asking which context is current must not be told about the one
    /// the thread that set the renderer up is drawing through.
    static CURRENT: std::cell::Cell<Id> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// The context `thread` draws through while another thread holds the SDL one.
///
/// Made once per thread and kept, sharing a group with the SDL context - see
/// [`Shared`]. Vertex arrays and framebuffers are the objects OpenGL does *not*
/// share, so a program that makes one on the thread that sets the renderer up
/// and binds it on the thread that draws is one this cannot serve; that program
/// draws nothing rather than being told why, which is why the split is written
/// down here.
/// Said once, because the fallback is per-frame and the reason is not.
fn warn_about_the_fallback() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        eprintln!(
            "rosetta3: appkit: a thread asked for a GL context another thread has held since \
             before it reached its event loop, so this one draws through a context sharing its \
             objects instead - vertex arrays, framebuffers and queries the other thread made are \
             not visible through it"
        );
    });
}

fn sibling(
    state: &mut Shared,
    primary: sdl3::GlContext,
    thread: std::thread::ThreadId,
) -> sdl3::GlContext {
    if let Some(&(_, existing)) = state.siblings.iter().find(|(t, _)| *t == thread) {
        return existing;
    }
    if !state.egl {
        objc::fail!(
            "-[NSOpenGLContext makeCurrentContext]: this context is current on another thread, \
             and this SDL's OpenGL is not one a second context can be made to share with"
        );
    }
    match sdl3::video::egl::create_shared(primary, state.version) {
        Ok(sibling) => {
            state.siblings.push((thread, sibling));
            sibling
        }
        Err(why) => objc::fail!(
            "-[NSOpenGLContext makeCurrentContext]: this context is current on another thread, \
             and a second one sharing its objects could not be made: {why}"
        ),
    }
}

/// How long a thread waits for the context another thread holds before drawing
/// through a sibling instead.
///
/// Long enough for a main thread that has just started a render thread to reach
/// its event loop, which is a matter of milliseconds, and short enough that a
/// program whose main thread never gets there is slow to start rather than
/// hung.
const HANDOFF: std::time::Duration = std::time::Duration::from_secs(2);

/// Give up the SDL context if this thread holds it and another is waiting.
///
/// Called from the event pump, which is the one place a thread is known *not*
/// to be part-way through drawing: it has returned to its event loop, so a
/// context still current from its setup is one it has finished with. AppKit
/// would leave it current there forever and let the program serialise the two
/// threads itself, which is a thing the GL underneath has no call for - so the
/// handover has to happen somewhere, and this is the somewhere that is safe.
pub fn release_for_other_threads() {
    let receiver = CURRENT.get();
    if receiver.is_null() {
        return;
    }
    let cell = shared_of(context_of(receiver));
    let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    if !state.wanted || state.owner != Some(std::thread::current().id()) {
        return;
    }
    sdl3::video::gl_make_current(std::ptr::null_mut(), std::ptr::null_mut());
    state.owner = None;
    state.wanted = false;
    CURRENT.set(std::ptr::null_mut());
    cell.released.notify_all();
}

unsafe extern "C" fn make_current_context(receiver: Id, _sel: Sel) {
    let window = ensure(receiver, "makeCurrentContext");
    let context = context_of(receiver);
    let primary = context.sdl;
    let thread = std::thread::current().id();
    let cell = shared_of(context);
    let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());

    // Another thread holds it, on a GL where that is a refusal rather than the
    // program's own business to serialise. Ask, and wait: the objects a program
    // makes on the thread it sets its renderer up on are only *some* of them
    // shared, so the context itself is worth waiting for and a sibling is the
    // fallback rather than the answer.
    if state.egl && state.owner.is_some_and(|owner| owner != thread) {
        state.wanted = true;
        let (guard, wait) = cell
            .released
            .wait_timeout_while(state, HANDOFF, |state| {
                state.owner.is_some_and(|owner| owner != thread)
            })
            .unwrap_or_else(|e| e.into_inner());
        state = guard;
        if wait.timed_out() {
            state.wanted = false;
            warn_about_the_fallback();
        }
    }

    // Where a context *can* be current on several threads at once - CGL can,
    // which is the contract AppKit's own is written against - none of the above
    // applies and every thread simply gets the context SDL made.
    let holds_primary = !state.egl || state.owner.is_none_or(|owner| owner == thread);
    let sdl = match holds_primary {
        true => primary,
        false => sibling(&mut state, primary, thread),
    };

    if std::env::var_os("ROSETTA3_GL_DEBUG").is_some() {
        eprintln!(
            "rosetta3: gl: make current on {thread:?}: primary={primary:p}, selected={sdl:p}, owner={:?}",
            state.owner
        );
    }

    if !sdl3::video::gl_make_current(window, sdl) {
        objc::fail!(
            "-[NSOpenGLContext makeCurrentContext]: SDL could not make the context current: {}",
            sdl3::error()
        );
    }
    chrome_blit::fit_viewport(drawable_window(context), window, sdl);

    if holds_primary {
        state.owner = Some(thread);
        // Read off the context now that it is current, which is the only place
        // either answer can be had: the version is what a sibling has to be
        // asked for, and whether SDL's handle is the EGL one is what says a
        // sibling can be made at all.
        if !state.probed {
            state.probed = true;
            state.version = sdl3::video::gl_current_version();
            state.egl = sdl3::video::egl::backs(primary);
            install_debug_output();
        }
    }
    drop(state);

    CURRENT.set(receiver);
    if context.cgl.is_null() {
        context.cgl = current_cgl_context();
    }
}

/// `ROSETTA3_GL_DEBUG`: report every message the GL driver has about the calls
/// the guest makes.
///
/// A guest checks `glGetError` where it thinks an error is likely, which is
/// almost never where one happens - so its own log says an error arrived at the
/// next call it bothered to check, naming a function that did nothing wrong.
/// `KHR_debug` says which call, and often why, in the driver's own words.
fn install_debug_output() {
    if std::env::var_os("ROSETTA3_GL_DEBUG").is_none() {
        return;
    }
    const DEBUG_OUTPUT: u32 = 0x92E0;
    const DEBUG_OUTPUT_SYNCHRONOUS: u32 = 0x8242;
    const DONT_CARE: u32 = 0x1100;

    /// `GLDEBUGPROC`. The lengths are the driver's and the message is not NUL
    /// terminated, so it is read by length.
    unsafe extern "C" fn report(
        _source: u32,
        _kind: u32,
        id: u32,
        severity: u32,
        length: i32,
        message: *const std::ffi::c_char,
        _user: *mut c_void,
    ) {
        let text = match message.is_null() || length < 0 {
            true => String::new(),
            // SAFETY: the driver's buffer and the length it just gave for it.
            false => unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    message as *const u8,
                    length as usize,
                ))
                .into_owned()
            },
        };
        eprintln!("rosetta3: gl: [{severity:#x}/{id}] {}", text.trim_end());
    }

    let (Some(callback), Some(enable)) = (
        sdl3::video::gl_proc_address(c"glDebugMessageCallback"),
        sdl3::video::gl_proc_address(c"glEnable"),
    ) else {
        eprintln!("rosetta3: gl: this driver has no KHR_debug, so ROSETTA3_GL_DEBUG says nothing");
        return;
    };
    // SAFETY: `void glEnable(GLenum)`.
    let enable: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(enable) };
    // SAFETY: `void glDebugMessageCallback(GLDEBUGPROC, const void *)`.
    let set: unsafe extern "C" fn(
        unsafe extern "C" fn(u32, u32, u32, u32, i32, *const std::ffi::c_char, *mut c_void),
        *mut c_void,
    ) = unsafe { std::mem::transmute(callback) };
    // Synchronous, so the message arrives while the call that caused it is
    // still on the stack and a breakpoint here names it.
    // SAFETY: two GL enumerators and a callback of the declared shape.
    unsafe {
        enable(DEBUG_OUTPUT);
        enable(DEBUG_OUTPUT_SYNCHRONOUS);
        set(report, std::ptr::null_mut());
    }
    if let Some(control) = sdl3::video::gl_proc_address(c"glDebugMessageControl") {
        // SAFETY: `void glDebugMessageControl(GLenum, GLenum, GLenum, GLsizei,
        // const GLuint *, GLboolean)`.
        let control: unsafe extern "C" fn(u32, u32, u32, i32, *const u32, u8) =
            unsafe { std::mem::transmute(control) };
        // Everything: the point of asking is not knowing what to ask about.
        // SAFETY: three "don't care" enumerators and an empty id list.
        unsafe { control(DONT_CARE, DONT_CARE, DONT_CARE, 0, std::ptr::null(), 1) };
    }
}

/// The CGL context current on this thread, or null where there is no CGL.
///
/// Through SDL's loader rather than a link: `CGLGetCurrentContext` lives in the
/// OpenGL framework, which is what SDL opened to make the context, and asking
/// SDL for it keeps this file building where that framework does not exist.
fn current_cgl_context() -> *mut c_void {
    let Some(f) = sdl3::video::gl_proc_address(c"CGLGetCurrentContext") else {
        return std::ptr::null_mut();
    };
    // SAFETY: `CGLContextObj CGLGetCurrentContext(void)`.
    unsafe {
        let f: unsafe extern "C" fn() -> *mut c_void = std::mem::transmute(f);
        f()
    }
}

/// Whether making this context current on this thread would take it away from
/// another thread that is drawing through it.
///
/// Only ever true where the GL underneath is one a context cannot be current on
/// two threads at once - see [`Shared`]. Where it can, which is the CGL this
/// class is modelled on, every thread holds the context and nothing is taken.
fn held_by_another_thread(context: &mut Context) -> bool {
    let cell = shared_of(context);
    let state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    state.egl
        && state
            .owner
            .is_some_and(|owner| owner != std::thread::current().id())
}

/// `-CGLContextObj`, the context SDL made underneath this one.
///
/// Recorded when the context is made current, because the only way to name
/// SDL's is to ask the thread it is current on. A receiver that has never been
/// current is made current here: the alternative is answering null, and the CGL
/// calls a program makes with it - `CGLEnable`, `CGLSetParameter` - would then
/// silently do nothing to the context it meant.
///
/// Except when another thread is drawing through it, which is the case that is
/// not worth the answer: there is no CGL under an EGL, so making the context
/// current here would interrupt the thread that is rendering only to answer
/// null anyway. A program asks this during startup - Unreal does, from
/// `-applicationDidFinishLaunching:`, while its render thread is already
/// drawing - and taking the context there is what stops the program dead.
unsafe extern "C" fn cgl_context(receiver: Id, sel: Sel) -> *mut c_void {
    if context_of(receiver).cgl.is_null() && !held_by_another_thread(context_of(receiver)) {
        // SAFETY: the receiver is a context of this crate's.
        unsafe { make_current_context(receiver, sel) };
    }
    let host = context_of(receiver).cgl;
    if !host.is_null() {
        return host;
    }
    // No CGL under this SDL - it is EGL or GLX - and null is the one answer
    // that cannot be given: a program stores this handle and then *calls*
    // through it, and `CGLSetCurrentContext(NULL)` means "make nothing
    // current" rather than "this context". So the handle is the
    // `NSOpenGLContext` itself, and the CGL entry points below take it back
    // apart again.
    handle_out(receiver)
}

/// Every `NSOpenGLContext` handed out as a `CGLContextObj`.
///
/// A registry rather than a class check, because the CGL calls are also given
/// handles that are *not* this crate's - on macOS, where the SDL context really
/// does have a CGL one behind it - and those have to be told apart so they can
/// go to the framework that made them.
static HANDLES: std::sync::Mutex<Option<std::collections::HashSet<usize>>> =
    std::sync::Mutex::new(None);

fn handle_out(receiver: Id) -> *mut c_void {
    HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(std::collections::HashSet::new)
        .insert(receiver as usize);
    receiver.cast()
}

/// The `NSOpenGLContext` a `CGLContextObj` stands for, or `None` for a handle
/// this crate did not hand out.
fn handle_in(handle: *mut c_void) -> Option<Id> {
    let held = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    held.as_ref()
        .is_some_and(|live| live.contains(&(handle as usize)))
        .then(|| handle.cast())
}

/// The host's own CGL entry point of that name, for a handle this crate did not
/// hand out.
///
/// Through SDL's loader for the reason [`current_cgl_context`] gives: CGL is in
/// `OpenGL.framework`, which this process does not link, and on the host where
/// there is one SDL has it open already.
fn host_cgl(name: &std::ffi::CStr) -> Option<*mut c_void> {
    sdl3::video::gl_proc_address(name)
}

/// The shape every CGL call below shares: a handle in, a `CGLError` out.
type CglCall = unsafe extern "C" fn(*mut c_void) -> i32;

/// [`host_cgl`], as a call of that shape.
fn host_cgl_call(name: &std::ffi::CStr) -> Option<CglCall> {
    let entry = host_cgl(name)?;
    // SAFETY: each name this is asked for is declared `CGLError
    // (CGLContextObj)` in `<OpenGL/CGLCurrent.h>`.
    Some(unsafe { std::mem::transmute::<*mut c_void, CglCall>(entry) })
}

mod context_enable {
    pub const SWAP_RECTANGLE: u32 = 201;
    pub const RASTERIZATION: u32 = 221;
    pub const STATE_VALIDATION: u32 = 301;
    pub const SURFACE_BACKING_SIZE: u32 = 305;
    pub const DISPLAY_LIST_OPTIMIZATION: u32 = 307;

    pub fn is_known(value: u32) -> bool {
        matches!(
            value,
            SWAP_RECTANGLE
                | RASTERIZATION
                | STATE_VALIDATION
                | SURFACE_BACKING_SIZE
                | DISPLAY_LIST_OPTIMIZATION
        )
    }
}

mod context_parameter {
    pub const SWAP_RECTANGLE: u32 = 200;
    pub const SWAP_INTERVAL: u32 = 222;
    pub const DISPATCH_TABLE_SIZE: u32 = 224;
    pub const CLIENT_STORAGE: u32 = 226;
    pub const SURFACE_TEXTURE: u32 = 228;
    pub const SURFACE_ORDER: u32 = 235;
    pub const SURFACE_OPACITY: u32 = 236;
    pub const SURFACE_BACKING_SIZE: u32 = 304;
    pub const SURFACE_VOLATILE: u32 = 306;
    pub const RECLAIM_RESOURCES: u32 = 308;
    pub const CURRENT_RENDERER_ID: u32 = 309;
    pub const GPU_VERTEX_PROCESSING: u32 = 310;
    pub const GPU_FRAGMENT_PROCESSING: u32 = 311;

    pub fn is_settable(value: u32) -> bool {
        matches!(
            value,
            SWAP_RECTANGLE
                | SWAP_INTERVAL
                | DISPATCH_TABLE_SIZE
                | CLIENT_STORAGE
                | SURFACE_TEXTURE
                | SURFACE_ORDER
                | SURFACE_OPACITY
                | SURFACE_BACKING_SIZE
                | SURFACE_VOLATILE
                | RECLAIM_RESOURCES
        )
    }

    pub fn is_read_only(value: u32) -> bool {
        matches!(
            value,
            CURRENT_RENDERER_ID | GPU_VERTEX_PROCESSING | GPU_FRAGMENT_PROCESSING
        )
    }
}

#[allow(non_upper_case_globals)]
const kCGLBadEnumeration: i32 = 10010;
#[allow(non_upper_case_globals)]
const kCGLBadAddress: i32 = 10014;
#[allow(non_upper_case_globals)]
const kCGLBadValue: i32 = 10008;

unsafe fn cgl_set(ctx: *mut c_void, pname: u32, _enabled: bool) -> i32 {
    if handle_in(ctx).is_some() {
        return if context_enable::is_known(pname) {
            0
        } else {
            // `kCGLBadEnumeration`.
            kCGLBadEnumeration
        };
    } else {
        kCGLBadAddress
    }
}

/// `CGLError CGLEnable(CGLContextObj ctx, CGLContextEnable pname)`.
pub unsafe extern "C" fn CGLEnable(ctx: *mut c_void, pname: u32) -> i32 {
    // SAFETY: the caller's context-handle obligation is preserved.
    unsafe { cgl_set(ctx, pname, true) }
}

/// `CGLError CGLDisable(CGLContextObj ctx, CGLContextEnable pname)`.
pub unsafe extern "C" fn CGLDisable(ctx: *mut c_void, pname: u32) -> i32 {
    // SAFETY: the caller's context-handle obligation is preserved.
    unsafe { cgl_set(ctx, pname, false) }
}

/// `CGLError CGLSetParameter(CGLContextObj ctx, CGLContextParameter pname,
/// const GLint *params)`.
///
/// # Safety
///
/// `ctx` is a handle returned by this module
pub unsafe extern "C" fn CGLSetParameter(_ctx: *mut c_void, pname: u32, params: *const i32) -> i32 {
    if params.is_null() {
        return kCGLBadAddress;
    }
    if context_parameter::is_read_only(pname) || !context_parameter::is_settable(pname) {
        return kCGLBadEnumeration;
    }
    if pname == context_parameter::SWAP_INTERVAL {
        // SAFETY: this parameter is documented as one `GLint` and the caller
        // promises a readable parameter array.
        let requested = unsafe { *params };
        let actual = swap_interval().unwrap_or(requested);
        if !sdl3::video::gl_set_swap_interval(actual) {
            return kCGLBadValue;
        }
    }
    0
}

/// `CGLError CGLSetCurrentContext(CGLContextObj ctx)`
///
/// A render thread's way of taking the context, and the CGL spelling of
/// `-makeCurrentContext` / `+clearCurrentContext`.
///
/// # Safety
///
/// `ctx` is null, a handle `-CGLContextObj` returned, or a host `CGLContextObj`.
pub unsafe extern "C" fn CGLSetCurrentContext(ctx: *mut c_void) -> i32 {
    if ctx.is_null() {
        // SAFETY: the class method reads neither argument.
        unsafe {
            clear_current_context(
                std::ptr::null_mut(),
                objc::sel::register(c"clearCurrentContext"),
            )
        };
        return 0;
    }
    if let Some(receiver) = handle_in(ctx) {
        // SAFETY: a context of this crate's, which is what the registry says.
        unsafe { make_current_context(receiver, objc::sel::register(c"makeCurrentContext")) };
        return 0;
    }
    let Some(f) = host_cgl_call(c"CGLSetCurrentContext") else {
        // `kCGLBadContext`.
        return 10004;
    };
    // SAFETY: a handle the framework that exports the call made, and that call
    // is what `host_cgl_call` returned.
    unsafe { f(ctx) }
}

/// `CGLError CGLFlushDrawable(CGLContextObj ctx)`
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLFlushDrawable(ctx: *mut c_void) -> i32 {
    if let Some(receiver) = handle_in(ctx) {
        // SAFETY: a context of this crate's.
        unsafe { flush_buffer(receiver, objc::sel::register(c"flushBuffer")) };
        return 0;
    }
    let Some(f) = host_cgl_call(c"CGLFlushDrawable") else {
        return 10004;
    };
    // SAFETY: a handle the framework that exports the call made, and that call
    // is what `host_cgl_call` returned.
    unsafe { f(ctx) }
}

/// `CGLError CGLClearDrawable(CGLContextObj ctx)`
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLClearDrawable(ctx: *mut c_void) -> i32 {
    if let Some(receiver) = handle_in(ctx) {
        // SAFETY: a context of this crate's.
        unsafe { clear_drawable(receiver, objc::sel::register(c"clearDrawable")) };
        return 0;
    }
    let Some(f) = host_cgl_call(c"CGLClearDrawable") else {
        return 10004;
    };
    // SAFETY: a handle the framework that exports the call made, and that call
    // is what `host_cgl_call` returned.
    unsafe { f(ctx) }
}

/// The lock `CGLLockContext` takes, one per context.
///
/// CGL's is recursive and is the program's own serialisation: a game locks
/// around the GL calls its render thread makes so that its other threads do not
/// interleave with them. Rust has no recursive mutex, so this is the two
/// pieces of one - who holds it and how deep - under a lock of its own.
#[derive(Default)]
struct ContextLock {
    owner: Option<std::thread::ThreadId>,
    depth: usize,
}

static LOCKS: std::sync::Mutex<Option<std::collections::HashMap<usize, ContextLock>>> =
    std::sync::Mutex::new(None);

static UNLOCKED: std::sync::Condvar = std::sync::Condvar::new();

/// `CGLError CGLLockContext(CGLContextObj ctx)`
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLLockContext(ctx: *mut c_void) -> i32 {
    if handle_in(ctx).is_none() {
        let Some(f) = host_cgl_call(c"CGLLockContext") else {
            return 10004;
        };
        // SAFETY: a handle the framework that exports the call made.
        return unsafe { f(ctx) };
    }
    let me = std::thread::current().id();
    let mut held = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let entry = held
            .get_or_insert_with(std::collections::HashMap::new)
            .entry(ctx as usize)
            .or_default();
        if entry.depth == 0 || entry.owner == Some(me) {
            entry.owner = Some(me);
            entry.depth += 1;
            return 0;
        }
        held = UNLOCKED.wait(held).unwrap_or_else(|e| e.into_inner());
    }
}

/// `CGLError CGLUnlockContext(CGLContextObj ctx)`
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLUnlockContext(ctx: *mut c_void) -> i32 {
    if handle_in(ctx).is_none() {
        let Some(f) = host_cgl_call(c"CGLUnlockContext") else {
            return 10004;
        };
        // SAFETY: a handle the framework that exports the call made.
        return unsafe { f(ctx) };
    }
    let mut held = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = held.as_mut().and_then(|all| all.get_mut(&(ctx as usize))) else {
        // `kCGLBadContext`: nothing ever locked it.
        return 10004;
    };
    if entry.owner != Some(std::thread::current().id()) || entry.depth == 0 {
        return 10004;
    }
    entry.depth -= 1;
    if entry.depth == 0 {
        entry.owner = None;
        UNLOCKED.notify_all();
    }
    0
}

/// `CGLShareGroupObj CGLGetShareGroup(CGLContextObj ctx)`
///
/// The group of contexts whose textures, buffers and programs are the same
/// objects. Every context this crate hands out is its own group: a
/// `-initWithFormat:shareContext:` here makes a context SDL did not share with
/// anything, and the siblings [`Shared`] makes are the same `NSOpenGLContext`
/// seen from another thread rather than a second one. So the group is the
/// context, which is the answer a caller comparing two of them needs.
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLGetShareGroup(ctx: *mut c_void) -> *mut c_void {
    if handle_in(ctx).is_some() {
        return ctx;
    }
    let Some(entry) = host_cgl(c"CGLGetShareGroup") else {
        return std::ptr::null_mut();
    };
    // SAFETY: `CGLShareGroupObj CGLGetShareGroup(CGLContextObj)`, and a handle
    // the framework that exports it made.
    unsafe {
        let f = std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void) -> *mut c_void>(
            entry,
        );
        f(ctx)
    }
}

unsafe extern "C" fn current_context(_class: Id, _sel: Sel) -> Id {
    CURRENT.get()
}

unsafe extern "C" fn clear_current_context(_class: Id, _sel: Sel) {
    sdl3::video::gl_make_current(std::ptr::null_mut(), std::ptr::null_mut());
    let receiver = CURRENT.replace(std::ptr::null_mut());
    if receiver.is_null() {
        return;
    }
    // Letting go of the SDL context hands it to whichever thread asks next,
    // rather than leaving every later thread on a sibling of a context nobody
    // holds.
    let cell = shared_of(context_of(receiver));
    let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.owner == Some(std::thread::current().id()) {
        state.owner = None;
        state.wanted = false;
        cell.released.notify_all();
    }
}

/// `-flushBuffer`: the swap that puts a frame on the screen.
unsafe extern "C" fn flush_buffer(receiver: Id, _sel: Sel) {
    let window = drawable(context_of(receiver));
    if !window.is_null() {
        chrome_blit::draw(window);
        sdl3::video::swap_window(window);
    }
}

#[inline(always)]
fn swap_interval() -> Option<i32> {
    #[dynamic]
    static SWAP_INTERVAL: Option<i32> = {
        if let Ok(no_vsync) = std::env::var("ROSETTA3_SWAP_INTERVAL") {
            no_vsync.parse().ok()
        } else {
            None
        }
    };
    *SWAP_INTERVAL
}

/// `-setValues:forParameter:`, of which only the swap interval is honoured.
///
/// `NSOpenGLContextParameterSwapInterval` is 222 and is the one a program sets;
/// the rest describe a surface AppKit composites and SDL does not. Refused by
/// number rather than ignored, because a program setting a parameter expects it
/// to take.
unsafe extern "C" fn set_values(receiver: Id, _sel: Sel, values: *const i32, parameter: isize) {
    let _ = receiver;
    const SWAP_INTERVAL: isize = 222;
    const SURFACE_OPACITY: isize = 236;
    // SAFETY: the guest passed a readable array of at least one integer, which
    // every parameter here takes.
    let value = match values.is_null() {
        true => 0,
        false => unsafe { *values },
    };
    match parameter {
        SWAP_INTERVAL => {
            let actual_value = swap_interval().unwrap_or(value);
            sdl3::video::gl_set_swap_interval(actual_value);
        }
        // Opacity is the compositor's, and SDL's window is opaque.
        SURFACE_OPACITY => {}
        other => objc::fail!(
            "-[NSOpenGLContext setValues:forParameter:]: parameter {other} is not one this \
             runtime maps"
        ),
    }
}

unsafe extern "C" fn update(_receiver: Id, _sel: Sel) {}

/// `-currentVirtualScreen`, which is always the first.
///
/// A virtual screen is one renderer of the context's pixel format, and a
/// program reads this to find out which GPU it ended up on after the window
/// moved between displays. There is one renderer here - see
/// `crate::cgl::CGLQueryRendererInfo` - so there is one virtual screen, and it
/// is the one in use.
unsafe extern "C" fn current_virtual_screen(_receiver: Id, _sel: Sel) -> isize {
    0
}

/// `-setCurrentVirtualScreen:`, which can only be told what it already is.
unsafe extern "C" fn set_current_virtual_screen(_receiver: Id, _sel: Sel, screen: isize) {
    if screen != 0 {
        objc::fail!(
            "-[NSOpenGLContext setCurrentVirtualScreen:]: this context has one virtual screen, \
             so {screen} names no renderer"
        );
    }
}

unsafe extern "C" fn pixel_format(receiver: Id, _sel: Sel) -> Id {
    context_of(receiver).format
}

pub(crate) fn install() {
    let format = objc::class::lookup(c"NSOpenGLPixelFormat").expect("declared before its methods");
    methods!(format, {
        "initWithAttributes:" => init_with_attributes, c"@24@0:8^I16";
        "getValues:forAttribute:forVirtualScreen:" => get_values, c"v32@0:8^i16I24i28";
        "CGLPixelFormatObj" => cgl_pixel_format, c"^v16@0:8";
    });

    let context = objc::class::lookup(c"NSOpenGLContext").expect("declared before its methods");
    class_methods!(context, {
        "currentContext" => current_context, c"@16@0:8";
        "clearCurrentContext" => clear_current_context, c"v16@0:8";
    });
    methods!(context, {
        "initWithFormat:shareContext:" => init_with_format, c"@32@0:8@16@24";
        "setView:" => set_view, c"v24@0:8@16";
        "view" => view, c"@16@0:8";
        "clearDrawable" => clear_drawable, c"v16@0:8";
        "CGLContextObj" => cgl_context, c"^v16@0:8";
        "pixelFormat" => pixel_format, c"@16@0:8";
        "currentVirtualScreen" => current_virtual_screen, c"q16@0:8";
        "setCurrentVirtualScreen:" => set_current_virtual_screen, c"v24@0:8q16";
        "makeCurrentContext" => make_current_context, c"v16@0:8";
        "flushBuffer" => flush_buffer, c"v16@0:8";
        "update" => update, c"v16@0:8";
        "setValues:forParameter:" => set_values, c"v32@0:8^i16q24";
    });

    let view = objc::class::lookup(c"NSOpenGLView").expect("declared before its methods");
    methods!(view, {
        "initWithFrame:pixelFormat:" => init_view_with_frame,
            c"@56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "pixelFormat" => view_pixel_format, c"@16@0:8";
        "openGLContext" => open_gl_context, c"@16@0:8";
        "setOpenGLContext:" => set_open_gl_context, c"v24@0:8@16";
        "prepareOpenGL" => update, c"v16@0:8";
        "update" => update, c"v16@0:8";
    });
}

/// The chrome `crate::decor` paints, put over the top of a GL window's
/// default framebuffer just before it is swapped.
///
/// The guest owns every pixel of that framebuffer, and it is the frame's
/// height: GL counts rows from the bottom, so a guest's `glViewport(0, 0, w,
/// h)` of its content size lands below the chrome and leaves this strip to
/// be drawn here. A blit touches no state a draw call reads, which keeps the
/// guest's context as it left it once the handful of bindings below are put
/// back.
mod chrome_blit {
    use std::cell::RefCell;
    use std::ffi::{CStr, c_void};

    const READ_FRAMEBUFFER: u32 = 0x8CA8;
    const DRAW_FRAMEBUFFER: u32 = 0x8CA9;
    const READ_FRAMEBUFFER_BINDING: u32 = 0x8CAA;
    const DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
    const TEXTURE_2D: u32 = 0x0DE1;
    const TEXTURE_BINDING_2D: u32 = 0x8069;
    const COLOR_ATTACHMENT0: u32 = 0x8CE0;
    const RGBA8: i32 = 0x8058;
    const BGRA: u32 = 0x80E1;
    const UNSIGNED_BYTE: u32 = 0x1401;
    const UNPACK_ALIGNMENT: u32 = 0x0CF5;
    const UNPACK_ROW_LENGTH: u32 = 0x0CF2;
    const UNPACK_SKIP_ROWS: u32 = 0x0CF3;
    const UNPACK_SKIP_PIXELS: u32 = 0x0CF4;
    const PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
    const PIXEL_UNPACK_BUFFER_BINDING: u32 = 0x88EF;
    const SCISSOR_TEST: u32 = 0x0C11;
    const FRAMEBUFFER_SRGB: u32 = 0x8DB9;
    const COLOR_BUFFER_BIT: u32 = 0x4000;
    const NEAREST: i32 = 0x2600;
    const TEXTURE_MIN_FILTER: u32 = 0x2801;
    const TEXTURE_MAG_FILTER: u32 = 0x2800;
    const FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
    const SCISSOR_BOX: u32 = 0x0C10;
    const COLOR_CLEAR_VALUE: u32 = 0x0C22;
    const COLOR_WRITEMASK: u32 = 0x0C23;
    const RASTERIZER_DISCARD: u32 = 0x8C89;

    #[allow(non_snake_case)]
    struct Gl {
        GetIntegerv: unsafe extern "C" fn(u32, *mut i32),
        IsEnabled: unsafe extern "C" fn(u32) -> u8,
        Enable: unsafe extern "C" fn(u32),
        Disable: unsafe extern "C" fn(u32),
        GenTextures: unsafe extern "C" fn(i32, *mut u32),
        BindTexture: unsafe extern "C" fn(u32, u32),
        TexParameteri: unsafe extern "C" fn(u32, u32, i32),
        TexImage2D: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
        PixelStorei: unsafe extern "C" fn(u32, i32),
        BindBuffer: unsafe extern "C" fn(u32, u32),
        GenFramebuffers: unsafe extern "C" fn(i32, *mut u32),
        BindFramebuffer: unsafe extern "C" fn(u32, u32),
        FramebufferTexture2D: unsafe extern "C" fn(u32, u32, u32, u32, i32),
        CheckFramebufferStatus: unsafe extern "C" fn(u32) -> u32,
        BlitFramebuffer: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32),
        GetBooleanv: unsafe extern "C" fn(u32, *mut u8),
        GetFloatv: unsafe extern "C" fn(u32, *mut f32),
        ColorMask: unsafe extern "C" fn(u8, u8, u8, u8),
        ClearColor: unsafe extern "C" fn(f32, f32, f32, f32),
        Clear: unsafe extern "C" fn(u32),
        Scissor: unsafe extern "C" fn(i32, i32, i32, i32),
    }

    impl Gl {
        fn load() -> Option<Gl> {
            fn get<T>(name: &CStr) -> Option<T> {
                let found = sdl3::video::gl_proc_address(name)?;
                assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
                // SAFETY: the caller names the prototype GL declares for it.
                Some(unsafe { std::mem::transmute_copy(&found) })
            }
            Some(Gl {
                GetIntegerv: get(c"glGetIntegerv")?,
                IsEnabled: get(c"glIsEnabled")?,
                Enable: get(c"glEnable")?,
                Disable: get(c"glDisable")?,
                GenTextures: get(c"glGenTextures")?,
                BindTexture: get(c"glBindTexture")?,
                TexParameteri: get(c"glTexParameteri")?,
                TexImage2D: get(c"glTexImage2D")?,
                PixelStorei: get(c"glPixelStorei")?,
                BindBuffer: get(c"glBindBuffer")?,
                GenFramebuffers: get(c"glGenFramebuffers")?,
                BindFramebuffer: get(c"glBindFramebuffer")?,
                FramebufferTexture2D: get(c"glFramebufferTexture2D")?,
                CheckFramebufferStatus: get(c"glCheckFramebufferStatus")?,
                BlitFramebuffer: get(c"glBlitFramebuffer")?,
                GetBooleanv: get(c"glGetBooleanv")?,
                GetFloatv: get(c"glGetFloatv")?,
                ColorMask: get(c"glColorMask")?,
                ClearColor: get(c"glClearColor")?,
                Clear: get(c"glClear")?,
                Scissor: get(c"glScissor")?,
            })
        }
    }

    /// The objects made in one context: a framebuffer does not travel
    /// between contexts, so each thread's context has its own.
    struct Cached {
        context: usize,
        texture: u32,
        framebuffer: u32,
        size: (u32, u32),
        generation: u64,
    }

    thread_local! {
        static CACHED: RefCell<Vec<Cached>> = const { RefCell::new(Vec::new()) };
    }

    /// The contexts whose first viewport has been fitted to a window's
    /// content, as (context, window) addresses.
    static FITTED: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());

    /// Give a context made current on a decorated window, the first time,
    /// a viewport of the content rather than of the whole frame.
    ///
    /// GL starts a context's viewport at its drawable's size, and a guest
    /// that never sets one relies on that matching its view - which, under
    /// the chrome, it no longer does.
    pub(super) fn fit_viewport(
        object: objc::object::Id,
        window: sdl3::Window,
        context: sdl3::GlContext,
    ) {
        // SAFETY: null or a window this crate made.
        if unsafe { crate::window::sdl_window(object) } != window {
            // The hidden window a context waits in before it has a view.
            return;
        }
        let inset = crate::window::content_inset(object);
        if inset <= 0.0 {
            return;
        }
        {
            let mut fitted = FITTED.lock().unwrap_or_else(|e| e.into_inner());
            let key = (context as usize, window as usize);
            if fitted.contains(&key) {
                return;
            }
            fitted.push(key);
        }
        let (Some((points, _)), Some((width, height))) = (
            sdl3::video::window_size(window),
            sdl3::video::window_size_in_pixels(window),
        ) else {
            return;
        };
        let density = match points > 0 {
            true => f64::from(width) / f64::from(points),
            false => 1.0,
        };
        let Some(viewport) = sdl3::video::gl_proc_address(c"glViewport") else {
            return;
        };
        let inset = (inset * density).round() as i32;
        // SAFETY: `glViewport(GLint, GLint, GLsizei, GLsizei)` on the context
        // just made current.
        unsafe {
            let f: unsafe extern "C" fn(i32, i32, i32, i32) = std::mem::transmute(viewport);
            f(0, 0, width, (height - inset).max(0));
        }
    }

    /// Draw `window`'s chrome into the current context's default framebuffer.
    pub(super) fn draw(window: sdl3::Window) {
        let transparent = sdl3::video::window_flags(window) & sdl3::video::flags::TRANSPARENT != 0;
        let chrome =
            crate::decor::pixels(window).filter(|chrome| chrome.width != 0 && chrome.height != 0);
        if chrome.is_none() && !transparent {
            return;
        }
        let context = sdl3::video::gl_current_context();
        if context.is_null() {
            return;
        }
        let Some((width, height)) = sdl3::video::window_size_in_pixels(window) else {
            return;
        };
        static GL: std::sync::OnceLock<Option<Gl>> = std::sync::OnceLock::new();
        let Some(gl) = GL.get_or_init(Gl::load) else {
            return;
        };
        // SAFETY: a current context, and entry points of its GL.
        unsafe {
            if transparent {
                let top = chrome.as_ref().map_or(0, |chrome| chrome.height as i32);
                let radius = chrome.as_ref().map_or(0.0, |chrome| chrome.radius);
                seal(gl, width, height, top, radius);
            }
            if let Some(chrome) = &chrome {
                blit(gl, context as usize, chrome, height);
            }
        }
    }

    /// Give the content below the chrome's `top` rows full alpha, and clear
    /// the bottom corners outside a rounding of `radius` pixels.
    ///
    /// SDL gives a transparent window's surface an alpha channel, and the
    /// compositor shows it - but a Mac's window never shows its GL content's
    /// alpha, and a guest writes there whatever its blending leaves. The
    /// corners are cut whole pixels at a time: a clear cannot blend.
    unsafe fn seal(gl: &Gl, width: i32, height: i32, top: i32, radius: f32) {
        let get = |name| {
            let mut value = [0i32; 4];
            // SAFETY: a query of at most four values into a local.
            unsafe { (gl.GetIntegerv)(name, value.as_mut_ptr()) };
            value
        };
        // SAFETY: the current context; every piece of state changed is put
        // back.
        unsafe {
            let draw = get(DRAW_FRAMEBUFFER_BINDING)[0];
            let scissor_box = get(SCISSOR_BOX);
            let mut mask = [0u8; 4];
            (gl.GetBooleanv)(COLOR_WRITEMASK, mask.as_mut_ptr());
            let mut clear = [0f32; 4];
            (gl.GetFloatv)(COLOR_CLEAR_VALUE, clear.as_mut_ptr());
            let scissor = (gl.IsEnabled)(SCISSOR_TEST) != 0;
            let discard = (gl.IsEnabled)(RASTERIZER_DISCARD) != 0;

            (gl.BindFramebuffer)(DRAW_FRAMEBUFFER, 0);
            (gl.Enable)(SCISSOR_TEST);
            (gl.Disable)(RASTERIZER_DISCARD);
            (gl.ColorMask)(0, 0, 0, 1);
            (gl.ClearColor)(0.0, 0.0, 0.0, 1.0);
            (gl.Scissor)(0, 0, width, (height - top).max(0));
            (gl.Clear)(COLOR_BUFFER_BIT);
            if radius > 0.0 {
                (gl.ColorMask)(1, 1, 1, 1);
                (gl.ClearColor)(0.0, 0.0, 0.0, 0.0);
                // GL's first row is the bottom one, which is the corner's
                // edge.
                for (row, span) in chrome::corners::outside(radius).into_iter().enumerate() {
                    let span = (span as i32).min(width / 2);
                    (gl.Scissor)(0, row as i32, span, 1);
                    (gl.Clear)(COLOR_BUFFER_BIT);
                    (gl.Scissor)(width - span, row as i32, span, 1);
                    (gl.Clear)(COLOR_BUFFER_BIT);
                }
            }

            (gl.ClearColor)(clear[0], clear[1], clear[2], clear[3]);
            (gl.ColorMask)(mask[0], mask[1], mask[2], mask[3]);
            (gl.Scissor)(
                scissor_box[0],
                scissor_box[1],
                scissor_box[2],
                scissor_box[3],
            );
            if !scissor {
                (gl.Disable)(SCISSOR_TEST);
            }
            if discard {
                (gl.Enable)(RASTERIZER_DISCARD);
            }
            (gl.BindFramebuffer)(DRAW_FRAMEBUFFER, draw as u32);
        }
    }

    unsafe fn blit(gl: &Gl, context: usize, chrome: &crate::decor::Pixels, window_height: i32) {
        let get = |name| {
            let mut value = 0;
            // SAFETY: a single-valued query into a local.
            unsafe { (gl.GetIntegerv)(name, &mut value) };
            value
        };
        let read = get(READ_FRAMEBUFFER_BINDING);
        let draw = get(DRAW_FRAMEBUFFER_BINDING);
        let texture = get(TEXTURE_BINDING_2D);
        let unpack = [
            get(UNPACK_ALIGNMENT),
            get(UNPACK_ROW_LENGTH),
            get(UNPACK_SKIP_ROWS),
            get(UNPACK_SKIP_PIXELS),
        ];
        let unpack_buffer = get(PIXEL_UNPACK_BUFFER_BINDING);
        // SAFETY: a capability query.
        let scissor = unsafe { (gl.IsEnabled)(SCISSOR_TEST) } != 0;
        let srgb = unsafe { (gl.IsEnabled)(FRAMEBUFFER_SRGB) } != 0;

        let ready = CACHED.with(|cached| {
            let mut cached = cached.borrow_mut();
            let at = match cached.iter().position(|c| c.context == context) {
                Some(at) => at,
                None => {
                    let (mut texture, mut framebuffer) = (0, 0);
                    // SAFETY: names written into locals.
                    unsafe {
                        (gl.GenTextures)(1, &mut texture);
                        (gl.GenFramebuffers)(1, &mut framebuffer);
                    }
                    cached.push(Cached {
                        context,
                        texture,
                        framebuffer,
                        size: (0, 0),
                        generation: 0,
                    });
                    cached.len() - 1
                }
            };
            let entry = &mut cached[at];
            // SAFETY: objects of this context, and pixels that outlive the
            // upload.
            unsafe {
                (gl.BindTexture)(TEXTURE_2D, entry.texture);
                (gl.BindFramebuffer)(READ_FRAMEBUFFER, entry.framebuffer);
                if entry.generation != chrome.generation {
                    (gl.BindBuffer)(PIXEL_UNPACK_BUFFER, 0);
                    (gl.PixelStorei)(UNPACK_ALIGNMENT, 4);
                    (gl.PixelStorei)(UNPACK_ROW_LENGTH, 0);
                    (gl.PixelStorei)(UNPACK_SKIP_ROWS, 0);
                    (gl.PixelStorei)(UNPACK_SKIP_PIXELS, 0);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
                    (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
                    (gl.TexImage2D)(
                        TEXTURE_2D,
                        0,
                        RGBA8,
                        chrome.width as i32,
                        chrome.height as i32,
                        0,
                        BGRA,
                        UNSIGNED_BYTE,
                        chrome.pixels.as_ptr().cast(),
                    );
                    if entry.size != (chrome.width, chrome.height) {
                        (gl.FramebufferTexture2D)(
                            READ_FRAMEBUFFER,
                            COLOR_ATTACHMENT0,
                            TEXTURE_2D,
                            entry.texture,
                            0,
                        );
                        entry.size = (chrome.width, chrome.height);
                    }
                    entry.generation = chrome.generation;
                }
                (gl.CheckFramebufferStatus)(READ_FRAMEBUFFER) == FRAMEBUFFER_COMPLETE
            }
        });

        // SAFETY: the current context; every binding changed is put back.
        unsafe {
            if ready {
                (gl.BindFramebuffer)(DRAW_FRAMEBUFFER, 0);
                (gl.Disable)(SCISSOR_TEST);
                (gl.Disable)(FRAMEBUFFER_SRGB);
                let (width, height) = (chrome.width as i32, chrome.height as i32);
                // The store's first row is the top; GL's is the bottom, so the
                // destination runs downwards to flip it.
                (gl.BlitFramebuffer)(
                    0,
                    0,
                    width,
                    height,
                    0,
                    window_height,
                    width,
                    window_height - height,
                    COLOR_BUFFER_BIT,
                    NEAREST as u32,
                );
            }
            if scissor {
                (gl.Enable)(SCISSOR_TEST);
            }
            if srgb {
                (gl.Enable)(FRAMEBUFFER_SRGB);
            }
            (gl.BindTexture)(TEXTURE_2D, texture as u32);
            (gl.BindBuffer)(PIXEL_UNPACK_BUFFER, unpack_buffer as u32);
            (gl.PixelStorei)(UNPACK_ALIGNMENT, unpack[0]);
            (gl.PixelStorei)(UNPACK_ROW_LENGTH, unpack[1]);
            (gl.PixelStorei)(UNPACK_SKIP_ROWS, unpack[2]);
            (gl.PixelStorei)(UNPACK_SKIP_PIXELS, unpack[3]);
            (gl.BindFramebuffer)(READ_FRAMEBUFFER, read as u32);
            (gl.BindFramebuffer)(DRAW_FRAMEBUFFER, draw as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_gl_view_storage_precedes_subclass_ivars() {
        crate::bootstrap();
        let base = objc::class::lookup(c"NSView").expect("NSView");
        let open_gl = objc::class::lookup(c"NSOpenGLView").expect("NSOpenGLView");

        assert_eq!(
            open_gl.instance_size(),
            base.instance_size() + size_of::<OpenGLView>()
        );
    }

    /// No attribute is in both lists, and every one is in a list.
    ///
    /// The two are what the walk steps by, so an attribute in neither stops a
    /// program that was going to work and one in both is a step of the wrong
    /// length - after which every later attribute is read as something else.
    #[test]
    fn the_two_classifications_are_a_partition() {
        for name in attribute::VALUED {
            assert!(
                !attribute::STANDALONE.contains(name),
                "{name} is in both lists"
            );
            assert_eq!(attribute::takes_value(*name), Some(true), "{name}");
        }
        for name in attribute::STANDALONE {
            assert_eq!(attribute::takes_value(*name), Some(false), "{name}");
        }
    }

    /// And the union is the whole of `NSOpenGL.h`, which is what stops a program
    /// being refused for an attribute Apple has had since 10.0.
    ///
    /// The count rather than the values: the values are the two lists above, and
    /// asserting them against themselves would say nothing.
    #[test]
    fn every_attribute_the_header_declares_is_classified() {
        assert_eq!(
            attribute::VALUED.len() + attribute::STANDALONE.len(),
            39,
            "NSOpenGL.h declares thirty-nine pixel format attributes"
        );
    }

    /// An attribute nobody has described is refused rather than guessed at,
    /// which is what stops a list from being silently reinterpreted.
    #[test]
    fn an_unknown_attribute_is_not_classified() {
        assert_eq!(attribute::takes_value(199), None);
    }

    /// The list a program hands in is the list that comes back, terminator and
    /// all - which is what `apply` walks.
    #[test]
    fn an_attribute_list_is_read_up_to_its_terminator() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSOpenGLPixelFormat").expect("NSOpenGLPixelFormat");
        let object = objc::object::create_instance(class, 0);
        let attributes = [
            attribute::DOUBLE_BUFFER,
            attribute::COLOR_SIZE,
            24,
            attribute::DEPTH_SIZE,
            24,
            0,
            99,
        ];
        // SAFETY: `-initWithAttributes:` is `id (id, SEL, const uint32_t *)`.
        unsafe {
            init_with_attributes(
                object,
                objc::sel::get("initWithAttributes:"),
                attributes.as_ptr(),
            )
        };
        // SAFETY: the list this call just made.
        let list = unsafe { &*format_of(object).attributes };
        assert_eq!(list.as_slice(), &attributes[..5]);
    }
}
