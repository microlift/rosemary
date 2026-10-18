#![allow(non_snake_case)]

use std::ffi::{c_int, c_uint, c_void};

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
    /// What it takes to be current on more than one thread at once, allocated
    /// on first use by [`shared_of`]. Leaked with the object.
    shared: *mut Cell,
    /// The context this one shares its objects with, retained, or null.
    share: Id,
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
/// the wait gives up. An EGL backend can give the asking thread a context in
/// the same *share group*: a distinct context through which every shared
/// object - textures, buffers, shaders, programs, renderbuffers - is the same
/// object. Vertex arrays, framebuffers and queries are **not** shared by
/// OpenGL, so that fallback draws what a program that made none of those on the
/// other thread would draw, and nothing otherwise. Other backends report the
/// contention because SDL has no portable sibling-context creation API.
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
    /// Deferred until this context is current on the calling thread.
    swap_interval: Option<i32>,
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
///
/// Whether the format asks for a core profile.
///
/// SDL's defaults are put back first: the attributes are global, and one
/// format's stencil buffer left set would be read as the next one's request.
fn apply(format: Id) -> bool {
    use sdl3::video::attr;
    sdl3::video::gl_reset_attributes();
    let Some(list) = (match format.is_null() {
        true => None,
        // SAFETY: a pixel format this crate made; the pointer is its own.
        false => unsafe { format_of(format).attributes.as_ref() },
    }) else {
        return false;
    };
    let mut core = false;

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
                // A Mac answers either core request with its newest core
                // profile, 4.1, and a program asking for 3.2 is written against
                // that. A host driver instead answers with exactly what was
                // asked - NVIDIA's 3.2 is a 3.2 - so 4.1 is what is asked for.
                attribute::PROFILE_3_2_CORE | attribute::PROFILE_4_1_CORE => {
                    core = true;
                    request_core(4, 1)
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
    core
}

/// Ask SDL for a core profile of `major.minor`.
fn request_core(major: i32, minor: i32) -> bool {
    use sdl3::video::attr;
    sdl3::video::gl_set_attribute(attr::CONTEXT_PROFILE_MASK, sdl3::video::profile::CORE)
        && sdl3::video::gl_set_attribute(attr::CONTEXT_MAJOR_VERSION, major)
        && sdl3::video::gl_set_attribute(attr::CONTEXT_MINOR_VERSION, minor)
}

unsafe extern "C" fn init_with_format(receiver: Id, _sel: Sel, format: Id, share: Id) -> Id {
    let context = context_of(receiver);
    context.format = objc::arc::retain(format);
    context.share = objc::arc::retain(share);
    receiver
}

/// The first context of `receiver`'s share group, which is the one every
/// other member was ultimately made to share with.
fn share_root(mut receiver: Id) -> Id {
    while !context_of(receiver).share.is_null() {
        receiver = context_of(receiver).share;
    }
    receiver
}

/// The GL version and profile to ask a context sharing with `share` for:
/// what `share` came back as, or - before anything has made it current - the
/// core 4.1 a format asking for core is given.
fn version_for_sharing(share: Id, core: bool) -> Option<(i32, i32, i32)> {
    /// `EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT`.
    const CORE_PROFILE: i32 = 1;
    let state = shared_of(context_of(share))
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match state.probed {
        true => state.version,
        false => core.then_some((4, 1, CORE_PROFILE)),
    }
}

/// `-initWithCGLContextObj:`.
///
/// A `CGLContextObj` here is the `NSOpenGLContext` that [`CGLCreateContext`]
/// made, so the wrapper AppKit would build around it is that object itself:
/// the fresh receiver is let go and the context handed back, retained as an
/// initializer's result is. A handle this crate never gave out is nil.
unsafe extern "C" fn init_with_cgl_context(receiver: Id, _sel: Sel, cgl: *mut c_void) -> Id {
    objc::arc::release(receiver);
    match handle_in(cgl) {
        Some(context) => objc::arc::retain(context),
        None => std::ptr::null_mut(),
    }
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
    gl_view_of(receiver).format = objc::arc::retain(format);
    make_view_context(receiver);
    receiver
}

/// Make the context `view` owns from its pixel format, pointed back at the view.
///
/// A nil format is the default one, which is what AppKit builds a context from
/// when a view was never given a format.
fn make_view_context(view: Id) {
    let mut format = gl_view_of(view).format;
    if format.is_null() {
        // SAFETY: a class method of the class declared for this purpose.
        format = unsafe {
            default_pixel_format(std::ptr::null_mut(), objc::sel::get("defaultPixelFormat"))
        };
        gl_view_of(view).format = format;
    }
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
    // SAFETY: the context and view this call just established.
    unsafe { set_view(context, objc::sel::get("setView:"), view) };
    gl_view_of(view).context = context;
}

/// `+[NSOpenGLView defaultPixelFormat]`: double buffered, 24-bit colour and a
/// 16-bit depth buffer, as the class reference describes it. Autoreleased.
unsafe extern "C" fn default_pixel_format(_receiver: Id, _sel: Sel) -> Id {
    static DEFAULT: [u32; 6] = [
        attribute::DOUBLE_BUFFER,
        attribute::COLOR_SIZE,
        24,
        attribute::DEPTH_SIZE,
        16,
        0,
    ];
    let class = objc::class::lookup(c"NSOpenGLPixelFormat").expect("NSOpenGLPixelFormat");
    let object = objc::object::create_instance(class, 0);
    // SAFETY: `-initWithAttributes:` takes a zero-terminated list.
    let object = unsafe {
        init_with_attributes(
            object,
            objc::sel::get("initWithAttributes:"),
            DEFAULT.as_ptr(),
        )
    };
    objc::arc::autorelease(object)
}

/// `-[NSOpenGLView setPixelFormat:]`.
///
/// Per the class reference this sets the format the view's context is made
/// from; it does not rebuild a context that exists. A context nothing has made
/// current yet has no SDL state tied to the old format, so it takes the new one
/// in place. After that the program must `-clearGLContext` and ask again, as
/// AppKit requires.
unsafe extern "C" fn set_view_pixel_format(receiver: Id, _sel: Sel, format: Id) {
    let view = gl_view_of(receiver);
    if view.format == format {
        return;
    }
    let old = view.format;
    view.format = objc::arc::retain(format);
    if !view.context.is_null() {
        let context = context_of(view.context);
        if context.sdl.is_null() && context.format == old {
            objc::arc::release(context.format);
            context.format = objc::arc::retain(format);
        }
    }
    objc::arc::release(old);
}

/// `-[NSOpenGLView clearGLContext]`: detach and let go of the context. The
/// next `-openGLContext` makes a new one from the view's pixel format.
unsafe extern "C" fn clear_gl_context(receiver: Id, _sel: Sel) {
    let view = gl_view_of(receiver);
    let context = std::mem::replace(&mut view.context, std::ptr::null_mut());
    if context.is_null() {
        return;
    }
    // SAFETY: a context this crate made.
    unsafe { clear_drawable(context, objc::sel::get("clearDrawable")) };
    objc::arc::release(context);
}

unsafe extern "C" fn view_pixel_format(receiver: Id, _sel: Sel) -> Id {
    gl_view_of(receiver).format
}

unsafe extern "C" fn open_gl_context(receiver: Id, _sel: Sel) -> Id {
    if gl_view_of(receiver).context.is_null() {
        make_view_context(receiver);
    }
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
///
/// One per pixel format, made after [`apply`] has set that format's attributes:
/// SDL chooses an EGL config whenever it makes a GL window, and a context
/// created next takes that config. EGL will not bind a context to a surface of
/// another config, so the window a context is born against has to have been
/// made from the format the guest's real window will be remade from.
fn holding_window(format: Id) -> sdl3::Window {
    static WINDOWS: std::sync::Mutex<Vec<(Vec<u32>, usize)>> = std::sync::Mutex::new(Vec::new());
    let key = match format.is_null() {
        true => Vec::new(),
        // SAFETY: a pixel format this crate made; the pointer is its own.
        false => unsafe { format_of(format).attributes.as_ref() }
            .cloned()
            .unwrap_or_default(),
    };
    let mut windows = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, window)) = windows.iter().find(|(held, _)| *held == key) {
        return *window as sdl3::Window;
    }
    let flags = sdl3::video::flags::OPENGL | sdl3::video::flags::HIDDEN;
    let Some(window) = sdl3::video::create_window(c"rosemary offscreen GL", 1, 1, flags) else {
        return std::ptr::null_mut();
    };
    windows.push((key, window as usize));
    window
}

/// Make the SDL context, if it is not there yet.
fn ensure(receiver: Id, sent: &str) -> sdl3::Window {
    // A context sharing with another can only be made once that one exists,
    // and making it takes the creation lock below - so it goes first.
    let share = context_of(receiver).share;
    if context_of(receiver).sdl.is_null() && !share.is_null() && context_of(share).sdl.is_null() {
        ensure(share, sent);
    }
    let context = context_of(receiver);
    let object = drawable_window(context);
    // SAFETY: null or a window this crate made.
    let sdl_window = unsafe { crate::window::sdl_window(object) };
    // Whether a GL window or context is about to be made, which is when SDL's
    // global attributes are read - and so when they have to be this format's.
    let creating = context.sdl.is_null()
        || (!sdl_window.is_null() && !crate::window::draws_with_opengl(sdl_window));
    static CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = creating.then(|| CREATION.lock().unwrap_or_else(|e| e.into_inner()));
    let core = creating && apply(context.format);
    let window = match sdl_window.is_null() {
        true => {
            let holding = holding_window(context.format);
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
        // A host that has no 4.1 is a Mac that has none either, and that Mac
        // hands a core request 3.2 instead.
        let shared = (!context.share.is_null()).then(|| {
            let share = context_of(context.share).sdl;
            sdl3::video::egl::create_shared(share, version_for_sharing(context.share, core))
        });
        let created = match shared {
            Some(Ok(sdl)) => Some(sdl),
            Some(Err(why)) => {
                // Said rather than refused: an unshared context still draws
                // what it made itself, which on a GLX host is all there was.
                static ONCE: std::sync::Once = std::sync::Once::new();
                ONCE.call_once(|| {
                    eprintln!(
                        "rosemary: appkit: a GL context asked to share objects with another, \
                         and SDL could not make one that does ({why}); objects made through \
                         one will not be visible through the other"
                    )
                });
                None
            }
            None => None,
        };
        let created = created
            .or_else(|| sdl3::video::gl_create_context(window))
            .or_else(|| {
                (core && request_core(3, 2))
                    .then(|| sdl3::video::gl_create_context(window))
                    .flatten()
            });
        let Some(sdl) = created else {
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

struct ThreadBinding(std::cell::Cell<Id>);

impl Drop for ThreadBinding {
    fn drop(&mut self) {
        let receiver = self.0.replace(std::ptr::null_mut());
        if !receiver.is_null() {
            let_go(receiver);
        }
    }
}

thread_local! {
    /// The context this thread made current last, which `+currentContext`
    /// answers. Per thread because that is what AppKit promises: a render
    /// thread asking which context is current must not be told about the one
    /// the thread that set the renderer up is drawing through.
    static CURRENT: std::cell::Cell<Id> = const { std::cell::Cell::new(std::ptr::null_mut()) };

    /// The context this thread's guest let go of, which SDL still has bound
    /// here - see [`clear_current_context`]. Null when nothing is left bound.
    static LINGERING: std::cell::Cell<Id> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    static APPLIED_INTERVAL: std::cell::Cell<(sdl3::GlContext, sdl3::Window, Option<i32>)> =
        const { std::cell::Cell::new((std::ptr::null_mut(), std::ptr::null_mut(), None)) };
    // Ownership must be released even when a guest's render thread exits.
    static BINDING: ThreadBinding = const { ThreadBinding(std::cell::Cell::new(std::ptr::null_mut())) };
}

/// `ROSEMARY_GL_DEBUG`, read once: it is asked on every make-current.
fn gl_debug() -> bool {
    static DEBUG: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("ROSEMARY_GL_DEBUG").is_some());
    *DEBUG
}

/// Which window each thread has bound, and through which context.
///
/// EGL lets a window surface be current on one thread at a time, whatever the
/// context; AppKit lets two contexts on two threads draw to one view. The
/// per-context handover in [`Shared`] does not see the second case, so this
/// does: a thread binding a window another thread still has bound - often only
/// lingering, see [`clear_current_context`] - asks that thread's context to be
/// let go first.
static BOUND: std::sync::Mutex<Vec<(std::thread::ThreadId, usize, usize)>> =
    std::sync::Mutex::new(Vec::new());

/// Record that this thread now has `window` bound through `receiver`.
fn note_bound(receiver: Id, window: sdl3::Window) {
    let me = std::thread::current().id();
    let mut bound = BOUND.lock().unwrap_or_else(|e| e.into_inner());
    bound.retain(|(thread, _, _)| *thread != me);
    bound.push((me, window as usize, receiver as usize));
}

/// Unbind whatever SDL has current on this thread.
fn unbind_here() {
    sdl3::video::gl_make_current(std::ptr::null_mut(), std::ptr::null_mut());
    let me = std::thread::current().id();
    BOUND
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(thread, _, _)| *thread != me);
}

/// Wait for any other thread that has `window` bound through a context other
/// than `receiver` to let it go, asking it to first.
///
/// Bounded by [`HANDOFF`], after which the bind is attempted anyway and fails
/// by name if the other thread still has it.
fn claim_window(receiver: Id, window: sdl3::Window) {
    let me = std::thread::current().id();
    let holder = BOUND
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(thread, held, context)| {
            *thread != me && *held == window as usize && *context != receiver as usize
        })
        .map(|(thread, _, context)| (*thread, *context as Id));
    let Some((thread, context)) = holder else {
        return;
    };
    let cell = shared_of(context_of(context));
    let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.owner != Some(thread) {
        return;
    }
    state.wanted = true;
    let (mut state, wait) = cell
        .released
        .wait_timeout_while(state, HANDOFF, |state| state.owner == Some(thread))
        .unwrap_or_else(|e| e.into_inner());
    if wait.timed_out() {
        state.wanted = false;
    }
}

/// Unbind whatever SDL has current on this thread, and give `receiver`'s SDL
/// context back if this thread held it, waking a thread waiting for it.
fn let_go(receiver: Id) {
    unbind_here();
    let _ = BINDING.try_with(|binding| binding.0.set(std::ptr::null_mut()));
    let cell = shared_of(context_of(receiver));
    let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.owner == Some(std::thread::current().id()) {
        state.owner = None;
        state.wanted = false;
        cell.released.notify_all();
    }
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
            "rosemary: appkit: a thread asked for a GL context another thread has held since \
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

/// Give up the SDL context if this thread holds it, or still has it bound, and
/// another is waiting.
///
/// Called from the event pump, which is the one place a thread is known *not*
/// to be part-way through drawing: it has returned to its event loop, so a
/// context still current from its setup is one it has finished with. AppKit
/// would leave it current there forever and let the program serialise the two
/// threads itself, which is a thing the GL underneath has no call for - so the
/// handover has to happen somewhere, and this is the somewhere that is safe.
pub fn release_for_other_threads() {
    let lingering = LINGERING.get();
    if !lingering.is_null() {
        let cell = shared_of(context_of(lingering));
        if cell.state.lock().unwrap_or_else(|e| e.into_inner()).wanted {
            LINGERING.set(std::ptr::null_mut());
            let_go(lingering);
        }
        return;
    }
    let receiver = CURRENT.get();
    if receiver.is_null() {
        return;
    }
    let cell = shared_of(context_of(receiver));
    let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    if !state.wanted || state.owner != Some(std::thread::current().id()) {
        return;
    }
    unbind_here();
    BINDING.with(|binding| binding.0.set(std::ptr::null_mut()));
    state.owner = None;
    state.wanted = false;
    CURRENT.set(std::ptr::null_mut());
    cell.released.notify_all();
}

/// The idle hook: a guest thread is about to sleep, block or yield.
///
/// A context left bound after the guest let go of it is let go of for real
/// before the thread blocks. Otherwise a thread that later asks for it would
/// wait on one that may not run again until the asker does something - when
/// the guest's own lock around its GL work is what this thread is blocking on,
/// that is never.
pub fn going_idle(idle: darwin::idle::Idle) {
    if idle == darwin::idle::Idle::Wait {
        let lingering = LINGERING.replace(std::ptr::null_mut());
        if !lingering.is_null() {
            let_go(lingering);
            return;
        }
    }
    release_for_other_threads();
}

unsafe extern "C" fn make_current_context(receiver: Id, _sel: Sel) {
    // A different context still bound here underneath is let go properly, so
    // its owner is cleared and any thread waiting for it is woken.
    let lingering = LINGERING.replace(std::ptr::null_mut());
    if !lingering.is_null() && lingering != receiver {
        let_go(lingering);
    }
    let previous = CURRENT.replace(std::ptr::null_mut());
    if !previous.is_null() && previous != receiver {
        let_go(previous);
    }
    // Before this context's own lock: the thread being asked may be waiting
    // for this context in turn.
    {
        let context = context_of(receiver);
        let window = drawable(context);
        if !window.is_null() {
            claim_window(receiver, window);
        }
    }
    // Serialize lazy creation as well as subsequent ownership changes.
    let cell = shared_of(context_of(receiver));
    let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    let window = ensure(receiver, "makeCurrentContext");
    let context = context_of(receiver);
    let primary = context.sdl;
    let thread = std::thread::current().id();

    // Another thread holds it, on a GL where that is a refusal rather than the
    // program's own business to serialise. Ask, and wait: the objects a program
    // makes on the thread it sets its renderer up on are only *some* of them
    // shared, so the context itself is worth waiting for and a sibling is the
    // fallback rather than the answer.
    if state.owner.is_some_and(|owner| owner != thread) {
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
            if state.egl {
                warn_about_the_fallback();
            }
        }
    }

    let holds_primary = state.owner.is_none_or(|owner| owner == thread);
    let sdl = match holds_primary {
        true => primary,
        false => sibling(&mut state, primary, thread),
    };

    if gl_debug() {
        eprintln!(
            "rosemary: gl: make current on {thread:?}: primary={primary:p}, selected={sdl:p}, \
             window={window:p}, owner={:?}",
            state.owner
        );
    }

    // Still bound from before this thread's last clear - see
    // `clear_current_context` - so there is nothing to ask the driver for.
    let bound =
        sdl3::video::gl_current_context() == sdl && sdl3::video::gl_current_window() == window;
    if !bound && !sdl3::video::gl_make_current(window, sdl) {
        objc::fail!(
            "-[NSOpenGLContext makeCurrentContext]: SDL could not make the context current: {}",
            sdl3::error()
        );
    }
    note_bound(receiver, window);
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
    if let Some(interval) = state.swap_interval.or_else(swap_interval) {
        let setting = (sdl, window, Some(interval));
        if !bound || APPLIED_INTERVAL.get() != setting {
            if !sdl3::video::gl_set_swap_interval(interval) {
                objc::fail!(
                    "-[NSOpenGLContext makeCurrentContext]: SDL could not set swap interval \
                     {interval}: {}",
                    sdl3::error()
                );
            }
            APPLIED_INTERVAL.set(setting);
        }
    }
    drop(state);

    CURRENT.set(receiver);
    BINDING.with(|binding| binding.0.set(receiver));
}

/// `ROSEMARY_GL_DEBUG`: report every message the GL driver has about the calls
/// the guest makes.
///
/// A guest checks `glGetError` where it thinks an error is likely, which is
/// almost never where one happens - so its own log says an error arrived at the
/// next call it bothered to check, naming a function that did nothing wrong.
/// `KHR_debug` says which call, and often why, in the driver's own words.
fn install_debug_output() {
    if std::env::var_os("ROSEMARY_GL_DEBUG").is_none() {
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
        eprintln!("rosemary: gl: [{severity:#x}/{id}] {}", text.trim_end());
    }

    let (Some(callback), Some(enable)) = (
        sdl3::video::gl_proc_address(c"glDebugMessageCallback"),
        sdl3::video::gl_proc_address(c"glEnable"),
    ) else {
        eprintln!("rosemary: gl: this driver has no KHR_debug, so ROSEMARY_GL_DEBUG says nothing");
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

/// A borrowed compatibility handle; querying it never changes GL bindings.
unsafe extern "C" fn cgl_context(receiver: Id, _sel: Sel) -> CGLContextObj {
    handle_out(receiver)
}

/// Only handles this implementation published may reach its SDL contexts.
static HANDLES: std::sync::Mutex<Option<std::collections::HashSet<usize>>> =
    std::sync::Mutex::new(None);

fn handle_out(receiver: Id) -> CGLContextObj {
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
#[allow(non_upper_case_globals)]
const kCGLBadPixelFormat: i32 = 10002;
#[allow(non_upper_case_globals)]
const kCGLBadContext: i32 = 10004;

pub type CGLContextObj = *mut c_void;
pub type CGLContextParameter = c_uint;
pub type GLint = c_int;
pub type CGLError = c_int;

unsafe fn cgl_set(ctx: CGLContextObj, pname: u32, _enabled: bool) -> i32 {
    if handle_in(ctx).is_some() {
        if context_enable::is_known(pname) {
            0
        } else {
            // `kCGLBadEnumeration`.
            kCGLBadEnumeration
        }
    } else {
        kCGLBadContext
    }
}

/// `CGLError CGLEnable(CGLContextObj ctx, CGLContextEnable pname)`.
pub unsafe extern "C" fn CGLEnable(ctx: CGLContextObj, pname: CGLContextParameter) -> CGLError {
    // SAFETY: the caller's context-handle obligation is preserved.
    unsafe { cgl_set(ctx, pname, true) }
}

/// `CGLError CGLDisable(CGLContextObj ctx, CGLContextEnable pname)`.
pub unsafe extern "C" fn CGLDisable(ctx: CGLContextObj, pname: CGLContextParameter) -> CGLError {
    // SAFETY: the caller's context-handle obligation is preserved.
    unsafe { cgl_set(ctx, pname, false) }
}

// FIXME (ACCURACY): stub!
pub unsafe extern "C" fn CGLGetParameter(
    _ctx: CGLContextObj,
    _pname: CGLContextParameter,
    _params: *mut GLint,
) -> CGLError {
    kCGLBadEnumeration
}

/// `CGLError CGLSetParameter(CGLContextObj ctx, CGLContextParameter pname,
/// const GLint *params)`.
///
/// # Safety
///
/// `ctx` is a handle returned by this module
pub unsafe extern "C" fn CGLSetParameter(
    ctx: CGLContextObj,
    pname: CGLContextParameter,
    params: *const GLint,
) -> CGLError {
    let Some(receiver) = handle_in(ctx) else {
        return kCGLBadContext;
    };
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
        return set_swap_interval(receiver, requested);
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
/// `ctx` is null or a handle `-CGLContextObj` returned.
pub unsafe extern "C" fn CGLSetCurrentContext(ctx: CGLContextObj) -> i32 {
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
    kCGLBadContext
}

/// `CGLError CGLFlushDrawable(CGLContextObj ctx)`
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLFlushDrawable(ctx: CGLContextObj) -> i32 {
    if let Some(receiver) = handle_in(ctx) {
        // SAFETY: a context of this crate's.
        unsafe { flush_buffer(receiver, objc::sel::register(c"flushBuffer")) };
        return 0;
    }
    kCGLBadContext
}

/// `CGLError CGLClearDrawable(CGLContextObj ctx)`
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLClearDrawable(ctx: CGLContextObj) -> i32 {
    if let Some(receiver) = handle_in(ctx) {
        // SAFETY: a context of this crate's.
        unsafe { clear_drawable(receiver, objc::sel::register(c"clearDrawable")) };
        return 0;
    }
    kCGLBadContext
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

/// Threads asleep on [`UNLOCKED`], counted under [`LOCKS`]. An unlock is a
/// few times a draw call, and waking nobody is still a syscall.
static LOCK_WAITERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `CGLError CGLLockContext(CGLContextObj ctx)`
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLLockContext(ctx: CGLContextObj) -> i32 {
    if handle_in(ctx).is_none() {
        return kCGLBadContext;
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
        // The holder may be waiting for the GL context this thread still has
        // bound, and would wait for as long as this thread sleeps.
        going_idle(darwin::idle::Idle::Wait);
        LOCK_WAITERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        held = UNLOCKED.wait(held).unwrap_or_else(|e| e.into_inner());
        LOCK_WAITERS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// `CGLError CGLUnlockContext(CGLContextObj ctx)`
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLUnlockContext(ctx: CGLContextObj) -> i32 {
    if handle_in(ctx).is_none() {
        return kCGLBadContext;
    }
    let mut held = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = held.as_mut().and_then(|all| all.get_mut(&(ctx as usize))) else {
        // `kCGLBadContext`: nothing ever locked it.
        return kCGLBadContext;
    };
    if entry.owner != Some(std::thread::current().id()) || entry.depth == 0 {
        return kCGLBadContext;
    }
    entry.depth -= 1;
    if entry.depth == 0 {
        entry.owner = None;
        if LOCK_WAITERS.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            UNLOCKED.notify_all();
        }
    }
    0
}

/// `CGLShareGroupObj CGLGetShareGroup(CGLContextObj ctx)`
///
/// The group of contexts whose textures, buffers and programs are the same
/// objects, named by its first member - see [`share_root`]. The siblings
/// [`Shared`] makes are the same `NSOpenGLContext` seen from another thread,
/// so they are in it already.
///
/// # Safety
///
/// As [`CGLSetCurrentContext`].
pub unsafe extern "C" fn CGLGetShareGroup(ctx: CGLContextObj) -> *mut c_void {
    handle_in(ctx).map_or(std::ptr::null_mut(), |receiver| share_root(receiver).cast())
}

/// `CGLError CGLChoosePixelFormat(const CGLPixelFormatAttribute *attribs,
/// CGLPixelFormatObj *pix, GLint *npix)`.
///
/// A CGL pixel format is the `NSOpenGLPixelFormat` itself (see
/// [`cgl_pixel_format`]), and the attribute list is the same one
/// `-initWithAttributes:` takes, so the format is made the way AppKit makes it.
/// Its attributes are recorded rather than matched against a visual, so there is
/// always exactly one.
///
/// # Safety
///
/// `attribs` is a zero-terminated list; `pix` and `npix` are null or writable.
pub unsafe extern "C" fn CGLChoosePixelFormat(
    attribs: *const u32,
    pix: *mut *mut c_void,
    npix: *mut i32,
) -> i32 {
    if pix.is_null() {
        return kCGLBadValue;
    }
    let class =
        objc::class::lookup(c"NSOpenGLPixelFormat").expect("NSOpenGLPixelFormat is declared");
    let format = objc::object::create_instance(class, 0);
    // SAFETY: a fresh pixel format and the initializer's declared arguments.
    let format =
        unsafe { init_with_attributes(format, objc::sel::get("initWithAttributes:"), attribs) };
    // SAFETY: the caller's out-parameters.
    unsafe {
        *pix = format.cast();
        if !npix.is_null() {
            *npix = 1;
        }
    }
    0
}

/// `CGLError CGLDestroyPixelFormat(CGLPixelFormatObj)`.
pub unsafe extern "C" fn CGLDestroyPixelFormat(pix: *mut c_void) -> i32 {
    if pix.is_null() {
        return kCGLBadPixelFormat;
    }
    objc::arc::release(pix.cast());
    0
}

/// `CGLError CGLCreateContext(CGLPixelFormatObj, CGLContextObj share, CGLContextObj *)`.
///
/// The context is the `NSOpenGLContext` AppKit would have made, and stays lazy at
/// the SDL layer until it is made current. A `share` context makes the new one
/// share its objects, as `-initWithFormat:shareContext:` does.
///
/// # Safety
///
/// `pix` is a handle [`CGLChoosePixelFormat`] made; `ctx` is null or writable.
pub unsafe extern "C" fn CGLCreateContext(
    pix: *mut c_void,
    share: CGLContextObj,
    ctx: *mut CGLContextObj,
) -> i32 {
    if pix.is_null() {
        return kCGLBadPixelFormat;
    }
    if ctx.is_null() {
        return kCGLBadValue;
    }
    let class = objc::class::lookup(c"NSOpenGLContext").expect("NSOpenGLContext is declared");
    let context = objc::object::create_instance(class, 0);
    // SAFETY: a fresh context and the initializer's declared arguments.
    let context = unsafe {
        init_with_format(
            context,
            objc::sel::get("initWithFormat:shareContext:"),
            pix.cast(),
            handle_in(share).unwrap_or(std::ptr::null_mut()),
        )
    };
    // SAFETY: the caller's out-parameter.
    unsafe { *ctx = handle_out(context) };
    0
}

/// `void CGLDestroyContext(CGLContextObj)`: the handle stops being valid and the
/// creation reference is dropped.
pub unsafe extern "C" fn CGLDestroyContext(ctx: CGLContextObj) -> i32 {
    let Some(receiver) = handle_in(ctx) else {
        return kCGLBadContext;
    };
    if let Some(live) = HANDLES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        live.remove(&(ctx as usize));
    }
    objc::arc::release(receiver);
    0
}

/// `void CGLReleaseContext(CGLContextObj)`.
///
/// The handle is the `NSOpenGLContext`, so this is its release. The common
/// caller is the one that wrapped the context in `-initWithCGLContextObj:`,
/// which retained it, and then gives up its own creation reference - leaving
/// the wrapper holding it, as on a Mac.
pub unsafe extern "C" fn CGLReleaseContext(ctx: CGLContextObj) {
    if let Some(receiver) = handle_in(ctx) {
        objc::arc::release(receiver);
    }
}

/// `CGLContextObj CGLRetainContext(CGLContextObj)`.
pub unsafe extern "C" fn CGLRetainContext(ctx: CGLContextObj) -> CGLContextObj {
    match handle_in(ctx) {
        Some(receiver) => objc::arc::retain(receiver).cast(),
        None => std::ptr::null_mut(),
    }
}

/// `CGLPixelFormatObj CGLGetPixelFormat(CGLContextObj)`, not retained.
pub unsafe extern "C" fn CGLGetPixelFormat(ctx: CGLContextObj) -> *mut c_void {
    match handle_in(ctx) {
        Some(receiver) => context_of(receiver).format.cast(),
        None => std::ptr::null_mut(),
    }
}

/// `const char *CGLErrorString(CGLError)`.
#[allow(non_upper_case_globals)]
pub extern "C" fn CGLErrorString(error: CGLError) -> *const std::ffi::c_char {
    let text: &'static std::ffi::CStr = match error {
        0 => c"no error",
        kCGLBadPixelFormat => c"invalid pixel format",
        kCGLBadContext => c"invalid context",
        kCGLBadValue => c"invalid numerical value",
        kCGLBadEnumeration => c"invalid enumerant",
        kCGLBadAddress => c"invalid pointer",
        _ => c"unknown error",
    };
    text.as_ptr()
}

/// `CGLError CGLGetVersion(GLint *major, GLint *minor)`: CGL 1.2, which is what
/// every Mac since 10.4 reports.
pub unsafe extern "C" fn CGLGetVersion(major: *mut i32, minor: *mut i32) -> i32 {
    // SAFETY: the caller's out-parameters, each null or writable.
    unsafe {
        if !major.is_null() {
            *major = 1;
        }
        if !minor.is_null() {
            *minor = 2;
        }
    }
    0
}

/// `CGLContextObj CGLGetCurrentContext(void)`, from the guest's logical binding.
pub unsafe extern "C" fn CGLGetCurrentContext() -> CGLContextObj {
    let receiver = CURRENT.get();
    if receiver.is_null() {
        std::ptr::null_mut()
    } else {
        handle_out(receiver)
    }
}

unsafe extern "C" fn current_context(_class: Id, _sel: Sel) -> Id {
    CURRENT.get()
}

/// `+clearCurrentContext`, and `CGLSetCurrentContext(NULL)`.
///
/// The SDL context stays bound underneath. Programs that guard their GL work
/// with a lock of their own take and drop the context around every piece of it
/// - thousands of times a frame - which CGL makes a pointer swap and EGL a
/// flush and a driver round trip each way. So the binding lingers until this
/// thread takes the context back, another thread asks for it, or this thread
/// blocks (see [`release_for_other_threads`]); letting go then hands it to
/// whichever thread asks next, rather than leaving every later thread on a
/// sibling of a context nobody holds.
unsafe extern "C" fn clear_current_context(_class: Id, _sel: Sel) {
    let receiver = CURRENT.replace(std::ptr::null_mut());
    if receiver.is_null() {
        if LINGERING.get().is_null() && !sdl3::video::gl_current_context().is_null() {
            // Whatever is bound is not a context of this crate's, and clearing
            // it is what the guest asked.
            unbind_here();
        }
        return;
    }
    let cell = shared_of(context_of(receiver));
    let wanted = cell.state.lock().unwrap_or_else(|e| e.into_inner()).wanted;
    match wanted {
        true => let_go(receiver),
        false => LINGERING.set(receiver),
    }
}

/// Whether `format` asked for a back buffer. A format this crate did not make,
/// or one with no list, is the single-buffered default.
fn double_buffered(format: Id) -> bool {
    // SAFETY: a pixel format this crate made; the pointer is its own.
    let Some(list) = (!format.is_null())
        .then(|| unsafe { format_of(format).attributes.as_ref() })
        .flatten()
    else {
        return false;
    };
    let mut i = 0;
    while let Some(&name) = list.get(i) {
        if name == 0 {
            break;
        }
        if matches!(name, attribute::DOUBLE_BUFFER | attribute::TRIPLE_BUFFER) {
            return true;
        }
        i += 1 + attribute::takes_value(name).unwrap_or(false) as usize;
    }
    false
}

/// `glFlush` and `glFinish` on a single-buffered context: present.
///
/// On a Mac a single-buffered context draws to the window itself, so a flush
/// is what puts a frame on the screen and such a program never sends
/// `-flushBuffer`. Here every window surface has a back buffer, and on Wayland
/// one that is never swapped is never even mapped.
///
/// FIXME (ACCURACY): the back buffer is undefined after the swap, so a program
/// that adds to its previous frame rather than redrawing it loses that frame.
pub fn present_single_buffered() {
    let receiver = CURRENT.get();
    if receiver.is_null() || double_buffered(context_of(receiver).format) {
        return;
    }
    // SAFETY: the current context, a context of this crate's.
    unsafe { flush_buffer(receiver, objc::sel::register(c"flushBuffer")) };
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
        if let Ok(no_vsync) = std::env::var("ROSEMARY_SWAP_INTERVAL") {
            no_vsync.parse().ok()
        } else {
            None
        }
    };
    *SWAP_INTERVAL
}

/// Store a context's requested interval without binding it on another thread.
fn set_swap_interval(receiver: Id, requested: i32) -> i32 {
    let actual = swap_interval().unwrap_or(requested);
    let cell = shared_of(context_of(receiver));
    let mut state = cell.state.lock().unwrap_or_else(|e| e.into_inner());
    if CURRENT.get() == receiver {
        if !sdl3::video::gl_set_swap_interval(actual) {
            return kCGLBadValue;
        }
        APPLIED_INTERVAL.set((
            sdl3::video::gl_current_context(),
            sdl3::video::gl_current_window(),
            Some(actual),
        ));
    }
    state.swap_interval = Some(actual);
    0
}

/// `-setValues:forParameter:`, through the same context state as CGL.
unsafe extern "C" fn set_values(receiver: Id, _sel: Sel, values: *const i32, parameter: isize) {
    if !matches!(parameter, 222 | 236) {
        objc::fail!(
            "-[NSOpenGLContext setValues:forParameter:]: parameter {parameter} is not one \
             this runtime maps"
        );
    }
    let zero = 0;
    let values = if values.is_null() { &zero } else { values };
    let error = unsafe { CGLSetParameter(handle_out(receiver), parameter as u32, values) };
    if error != 0 {
        objc::fail!(
            "-[NSOpenGLContext setValues:forParameter:]: parameter {parameter} failed: {error}"
        );
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
        "initWithCGLContextObj:" => init_with_cgl_context, c"@24@0:8^v16";
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
    class_methods!(view, {
        "defaultPixelFormat" => default_pixel_format, c"@16@0:8";
    });
    methods!(view, {
        "setPixelFormat:" => set_view_pixel_format, c"v24@0:8@16";
        "clearGLContext" => clear_gl_context, c"v16@0:8";
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

    fn test_context() -> Id {
        crate::bootstrap();
        objc::object::create_instance(objc::class::lookup(c"NSOpenGLContext").unwrap(), 0)
    }

    #[test]
    fn cgl_handles_are_stable_without_creating_or_binding_gl() {
        let receiver = test_context();
        let before = CURRENT.get();
        let handle = unsafe { cgl_context(receiver, objc::sel::get("CGLContextObj")) };
        assert_eq!(handle, unsafe {
            cgl_context(receiver, objc::sel::get("CGLContextObj"))
        });
        assert_eq!(handle_in(handle), Some(receiver));
        assert!(context_of(receiver).sdl.is_null());
        assert_eq!(CURRENT.get(), before);
        assert_eq!(unsafe { CGLGetShareGroup(handle) }, handle);
    }

    #[test]
    fn foreign_cgl_handles_are_rejected_without_dereferencing_them() {
        let mut foreign = 0u64;
        let handle = (&raw mut foreign).cast();
        let value = 0;
        unsafe {
            assert_eq!(CGLSetCurrentContext(handle), kCGLBadContext);
            assert_eq!(CGLFlushDrawable(handle), kCGLBadContext);
            assert_eq!(CGLClearDrawable(handle), kCGLBadContext);
            assert_eq!(CGLLockContext(handle), kCGLBadContext);
            assert_eq!(CGLUnlockContext(handle), kCGLBadContext);
            assert_eq!(
                CGLEnable(handle, context_enable::RASTERIZATION),
                kCGLBadContext
            );
            assert_eq!(
                CGLDisable(handle, context_enable::RASTERIZATION),
                kCGLBadContext
            );
            assert_eq!(
                CGLSetParameter(handle, context_parameter::SWAP_INTERVAL, &value),
                kCGLBadContext
            );
            assert!(CGLGetShareGroup(handle).is_null());
        }
    }

    #[test]
    fn logical_current_queries_ignore_a_lingering_binding() {
        let receiver = test_context();
        CURRENT.set(receiver);
        let handle = unsafe { CGLGetCurrentContext() };
        assert_eq!(handle_in(handle), Some(receiver));
        assert_eq!(
            unsafe { current_context(std::ptr::null_mut(), objc::sel::get("currentContext")) },
            receiver
        );
        // No real SDL binding is needed to exercise the logical clear path.
        unsafe { CGLSetCurrentContext(std::ptr::null_mut()) };
        assert_eq!(LINGERING.get(), receiver);
        assert!(unsafe { CGLGetCurrentContext() }.is_null());
        assert!(CURRENT.get().is_null());
        LINGERING.set(std::ptr::null_mut());
    }

    #[test]
    fn swap_interval_is_deferred_per_context() {
        let first = test_context();
        let second = test_context();
        let first_handle = handle_out(first);
        unsafe {
            assert_eq!(
                CGLSetParameter(first_handle, context_parameter::SWAP_INTERVAL, &0),
                0
            );
            set_values(second, objc::sel::get("setValues:forParameter:"), &1, 222);
            assert_eq!(CGLSetParameter(first_handle, 9999, &0), kCGLBadEnumeration);
            assert_eq!(
                CGLSetParameter(first_handle, 222, std::ptr::null()),
                kCGLBadAddress
            );
        }
        assert_eq!(
            shared_of(context_of(first))
                .state
                .lock()
                .unwrap()
                .swap_interval,
            Some(swap_interval().unwrap_or(0))
        );
        assert_eq!(
            shared_of(context_of(second))
                .state
                .lock()
                .unwrap()
                .swap_interval,
            Some(swap_interval().unwrap_or(1))
        );
        assert!(context_of(first).sdl.is_null());
        assert!(context_of(second).sdl.is_null());
    }

    #[test]
    fn recursive_cgl_locks_exclude_other_threads_until_fully_unlocked() {
        let handle = handle_out(test_context());
        unsafe {
            assert_eq!(CGLLockContext(handle), 0);
            assert_eq!(CGLLockContext(handle), 0);
        }
        let address = handle as usize;
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || unsafe {
            let handle = address as *mut c_void;
            assert_eq!(CGLUnlockContext(handle), kCGLBadContext);
            started_tx.send(()).unwrap();
            assert_eq!(CGLLockContext(handle), 0);
            locked_tx.send(()).unwrap();
            assert_eq!(CGLUnlockContext(handle), 0);
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert!(
            locked_rx
                .recv_timeout(std::time::Duration::from_millis(30))
                .is_err()
        );
        unsafe { assert_eq!(CGLUnlockContext(handle), 0) };
        assert!(
            locked_rx
                .recv_timeout(std::time::Duration::from_millis(30))
                .is_err()
        );
        unsafe { assert_eq!(CGLUnlockContext(handle), 0) };
        locked_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        worker.join().unwrap();
        unsafe { assert_eq!(CGLUnlockContext(handle), kCGLBadContext) };
    }

    fn new_format(attributes: &[u32]) -> Id {
        let class = objc::class::lookup(c"NSOpenGLPixelFormat").expect("NSOpenGLPixelFormat");
        let object = objc::object::create_instance(class, 0);
        // SAFETY: a zero-terminated list.
        unsafe {
            init_with_attributes(
                object,
                objc::sel::get("initWithAttributes:"),
                attributes.as_ptr(),
            )
        }
    }

    #[test]
    fn only_a_format_with_a_back_buffer_is_double_buffered() {
        crate::bootstrap();
        assert!(!double_buffered(std::ptr::null_mut()));
        assert!(!double_buffered(new_format(&[0])));
        // A depth of 5 is a value, not `NSOpenGLPFADoubleBuffer`.
        assert!(!double_buffered(new_format(&[attribute::DEPTH_SIZE, 5, 0])));
        assert!(double_buffered(new_format(&[
            attribute::DEPTH_SIZE,
            16,
            attribute::DOUBLE_BUFFER,
            0
        ])));
        assert!(double_buffered(new_format(&[attribute::TRIPLE_BUFFER, 0])));
    }

    fn new_view(format: Id) -> Id {
        let class = objc::class::lookup(c"NSOpenGLView").expect("NSOpenGLView");
        let object = objc::object::create_instance(class, 0);
        let frame = NSRect::default();
        // SAFETY: the initializer's declared arguments.
        unsafe {
            init_view_with_frame(
                object,
                objc::sel::get("initWithFrame:pixelFormat:"),
                frame,
                format,
            )
        }
    }

    #[test]
    fn set_pixel_format_replaces_the_views_format() {
        crate::bootstrap();
        let first = new_format(&[attribute::DOUBLE_BUFFER, 0]);
        let second = new_format(&[attribute::DEPTH_SIZE, 24, 0]);
        let view = new_view(first);
        // SAFETY: the declared signatures.
        unsafe {
            assert_eq!(
                view_pixel_format(view, objc::sel::get("pixelFormat")),
                first
            );
            set_view_pixel_format(view, objc::sel::get("setPixelFormat:"), second);
            assert_eq!(
                view_pixel_format(view, objc::sel::get("pixelFormat")),
                second
            );
        }
    }

    #[test]
    fn set_pixel_format_reaches_a_context_not_yet_made_current() {
        crate::bootstrap();
        let first = new_format(&[attribute::DOUBLE_BUFFER, 0]);
        let second = new_format(&[attribute::DEPTH_SIZE, 24, 0]);
        let view = new_view(first);
        // SAFETY: the declared signatures.
        unsafe { set_view_pixel_format(view, objc::sel::get("setPixelFormat:"), second) };
        assert_eq!(context_of(gl_view_of(view).context).format, second);
    }

    #[test]
    fn clear_gl_context_makes_the_next_context_from_the_new_format() {
        crate::bootstrap();
        let first = new_format(&[attribute::DOUBLE_BUFFER, 0]);
        let second = new_format(&[attribute::DEPTH_SIZE, 24, 0]);
        let view = new_view(first);
        // SAFETY: the declared signatures.
        unsafe {
            clear_gl_context(view, objc::sel::get("clearGLContext"));
            assert!(gl_view_of(view).context.is_null());
            set_view_pixel_format(view, objc::sel::get("setPixelFormat:"), second);
            let context = open_gl_context(view, objc::sel::get("openGLContext"));
            assert!(!context.is_null());
            assert_eq!(context_of(context).format, second);
            assert_eq!(context_of(context).view, view);
        }
    }

    #[test]
    fn a_view_without_a_format_gets_the_default_one() {
        crate::bootstrap();
        let view = new_view(std::ptr::null_mut());
        let format = gl_view_of(view).format;
        assert!(!format.is_null());
        // SAFETY: the list this runtime made.
        let list = unsafe { &*format_of(format).attributes };
        assert_eq!(
            list.as_slice(),
            &[
                attribute::DOUBLE_BUFFER,
                attribute::COLOR_SIZE,
                24,
                attribute::DEPTH_SIZE,
                16
            ]
        );
    }

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
