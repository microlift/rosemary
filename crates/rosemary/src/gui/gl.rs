use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::sync::Mutex;

use super::draw::{DrawList, Vertex};
use super::objc::{Id, class, send0, send1};
use super::sys;

// `<OpenGL/gl.h>`, `<OpenGL/gl3.h>`. Spelled out rather than pulled from a
// binding crate.
const NO_ERROR: u32 = 0;
const RENDERER: u32 = 0x1f01;
const VERSION: u32 = 0x1f02;
const SHADING_LANGUAGE_VERSION: u32 = 0x8b8c;
/// Added in GL 3.0, which is what makes it the profile test: a 2.1 context
/// answers `GL_INVALID_ENUM` rather than a number.
const MAJOR_VERSION: u32 = 0x821b;
const INFO_LOG_LENGTH: u32 = 0x8b84;
const TEXTURE_2D: u32 = 0x0de1;
const UNSIGNED_BYTE: u32 = 0x1401;
const FLOAT: u32 = 0x1406;
const ALPHA: u32 = 0x1906;
const RED: u32 = 0x1903;
const R8: u32 = 0x8229;
const TEXTURE_MAG_FILTER: u32 = 0x2800;
const TEXTURE_MIN_FILTER: u32 = 0x2801;
const TEXTURE_WRAP_S: u32 = 0x2802;
const TEXTURE_WRAP_T: u32 = 0x2803;
const NEAREST: i32 = 0x2600;
const CLAMP_TO_EDGE: i32 = 0x812f;
const BLEND: u32 = 0x0be2;
const SRC_ALPHA: u32 = 0x0302;
const ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
const DEPTH_TEST: u32 = 0x0b71;
const CULL_FACE: u32 = 0x0b44;
const SCISSOR_TEST: u32 = 0x0c11;
const LIGHTING: u32 = 0x0b50;
const TRIANGLES: u32 = 0x0004;
const VIEWPORT: u32 = 0x0ba2;
const UNPACK_SWAP_BYTES: u32 = 0x0cf0;
const UNPACK_LSB_FIRST: u32 = 0x0cf1;
const UNPACK_ROW_LENGTH: u32 = 0x0cf2;
const UNPACK_SKIP_ROWS: u32 = 0x0cf3;
const UNPACK_SKIP_PIXELS: u32 = 0x0cf4;
const UNPACK_ALIGNMENT: u32 = 0x0cf5;
/// The buffer target that turns a `glTexImage2D` pointer into an offset. See
/// [`Unpack`] for why the overlay has to know about it.
const PIXEL_UNPACK_BUFFER: u32 = 0x88ec;
const PIXEL_UNPACK_BUFFER_BINDING: u32 = 0x88ef;
const TEXTURE0: u32 = 0x84c0;
const ARRAY_BUFFER: u32 = 0x8892;
const STREAM_DRAW: u32 = 0x88e0;
const VERTEX_SHADER: u32 = 0x8b31;
const FRAGMENT_SHADER: u32 = 0x8b30;
const COMPILE_STATUS: u32 = 0x8b81;
const LINK_STATUS: u32 = 0x8b82;
const ARRAY_BUFFER_BINDING: u32 = 0x8894;
const CURRENT_PROGRAM: u32 = 0x8b8d;
const VERTEX_ARRAY_BINDING: u32 = 0x85b5;
const TEXTURE_BINDING_2D: u32 = 0x8069;
const ACTIVE_TEXTURE: u32 = 0x84e0;
const BLEND_SRC_RGB: u32 = 0x80c9;
const BLEND_DST_RGB: u32 = 0x80c8;
const BLEND_SRC_ALPHA_STATE: u32 = 0x80cb;
const BLEND_DST_ALPHA_STATE: u32 = 0x80ca;
const MODELVIEW: u32 = 0x1700;
const PROJECTION: u32 = 0x1701;
const VERTEX_ARRAY: u32 = 0x8074;
const COLOR_ARRAY: u32 = 0x8076;
const TEXTURE_COORD_ARRAY: u32 = 0x8078;
const TEXTURE_ENV: u32 = 0x2300;
const TEXTURE_ENV_MODE: u32 = 0x2200;
const MODULATE: i32 = 0x2100;
const ALL_ATTRIB_BITS: u32 = 0x000f_ffff;
const CLIENT_ALL_ATTRIB_BITS: u32 = 0xffff_ffff;

/// Every OpenGL function the overlay calls, resolved once.
#[allow(non_snake_case)]
struct Gl {
    GetString: unsafe extern "C" fn(u32) -> *const u8,
    GetIntegerv: unsafe extern "C" fn(u32, *mut i32),
    GetError: unsafe extern "C" fn() -> u32,
    IsEnabled: unsafe extern "C" fn(u32) -> u8,
    Enable: unsafe extern "C" fn(u32),
    Disable: unsafe extern "C" fn(u32),
    BlendFunc: unsafe extern "C" fn(u32, u32),
    BlendFuncSeparate: unsafe extern "C" fn(u32, u32, u32, u32),
    Viewport: unsafe extern "C" fn(i32, i32, i32, i32),
    GenTextures: unsafe extern "C" fn(i32, *mut u32),
    BindTexture: unsafe extern "C" fn(u32, u32),
    TexImage2D: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void),
    TexParameteri: unsafe extern "C" fn(u32, u32, i32),
    PixelStorei: unsafe extern "C" fn(u32, i32),
    ActiveTexture: unsafe extern "C" fn(u32),
    DrawArrays: unsafe extern "C" fn(u32, i32, i32),

    // The 2.1 half.
    PushAttrib: Option<unsafe extern "C" fn(u32)>,
    PopAttrib: Option<unsafe extern "C" fn()>,
    PushClientAttrib: Option<unsafe extern "C" fn(u32)>,
    PopClientAttrib: Option<unsafe extern "C" fn()>,
    MatrixMode: Option<unsafe extern "C" fn(u32)>,
    PushMatrix: Option<unsafe extern "C" fn()>,
    PopMatrix: Option<unsafe extern "C" fn()>,
    LoadIdentity: Option<unsafe extern "C" fn()>,
    Ortho: Option<unsafe extern "C" fn(f64, f64, f64, f64, f64, f64)>,
    EnableClientState: Option<unsafe extern "C" fn(u32)>,
    VertexPointer: Option<unsafe extern "C" fn(i32, u32, i32, *const c_void)>,
    TexCoordPointer: Option<unsafe extern "C" fn(i32, u32, i32, *const c_void)>,
    ColorPointer: Option<unsafe extern "C" fn(i32, u32, i32, *const c_void)>,
    TexEnvi: Option<unsafe extern "C" fn(u32, u32, i32)>,

    // The core half.
    CreateShader: Option<unsafe extern "C" fn(u32) -> u32>,
    ShaderSource: Option<unsafe extern "C" fn(u32, i32, *const *const u8, *const i32)>,
    CompileShader: Option<unsafe extern "C" fn(u32)>,
    GetShaderiv: Option<unsafe extern "C" fn(u32, u32, *mut i32)>,
    CreateProgram: Option<unsafe extern "C" fn() -> u32>,
    AttachShader: Option<unsafe extern "C" fn(u32, u32)>,
    LinkProgram: Option<unsafe extern "C" fn(u32)>,
    GetProgramiv: Option<unsafe extern "C" fn(u32, u32, *mut i32)>,
    UseProgram: Option<unsafe extern "C" fn(u32)>,
    GetUniformLocation: Option<unsafe extern "C" fn(u32, *const u8) -> i32>,
    GetAttribLocation: Option<unsafe extern "C" fn(u32, *const u8) -> i32>,
    Uniform2f: Option<unsafe extern "C" fn(i32, f32, f32)>,
    Uniform1i: Option<unsafe extern "C" fn(i32, i32)>,
    GenVertexArrays: Option<unsafe extern "C" fn(i32, *mut u32)>,
    BindVertexArray: Option<unsafe extern "C" fn(u32)>,
    GenBuffers: Option<unsafe extern "C" fn(i32, *mut u32)>,
    BindBuffer: Option<unsafe extern "C" fn(u32, u32)>,
    BufferData: Option<unsafe extern "C" fn(u32, isize, *const c_void, u32)>,
    EnableVertexAttribArray: Option<unsafe extern "C" fn(u32)>,
    VertexAttribPointer: Option<unsafe extern "C" fn(u32, i32, u32, u8, i32, *const c_void)>,
    GetShaderInfoLog: Option<unsafe extern "C" fn(u32, i32, *mut i32, *mut u8)>,
    GetProgramInfoLog: Option<unsafe extern "C" fn(u32, i32, *mut i32, *mut u8)>,
}

/// Resolve the entry points, or `None` if this system has no OpenGL.
fn gl() -> Option<&'static Gl> {
    static GL: std::sync::OnceLock<Option<Gl>> = std::sync::OnceLock::new();
    GL.get_or_init(|| {
        // SAFETY for every lookup below: each type above is the function's
        // prototype from `<OpenGL/gl.h>` or `<OpenGL/gl3.h>`. `OpenGL.framework`
        // exports all of them regardless of the current context's profile,
        // which is why the profile is decided by [`Profile::of`] and not by
        // which of these came back `Some`.
        macro_rules! need {
            ($name:literal) => {
                match unsafe { sys::function(concat!("gl", $name)) } {
                    Some(f) => f,
                    None => return None,
                }
            };
        }
        macro_rules! want {
            ($name:literal) => {
                unsafe { sys::function(concat!("gl", $name)) }
            };
        }
        Some(Gl {
            GetString: need!("GetString"),
            GetIntegerv: need!("GetIntegerv"),
            GetError: need!("GetError"),
            IsEnabled: need!("IsEnabled"),
            Enable: need!("Enable"),
            Disable: need!("Disable"),
            BlendFunc: need!("BlendFunc"),
            BlendFuncSeparate: need!("BlendFuncSeparate"),
            Viewport: need!("Viewport"),
            GenTextures: need!("GenTextures"),
            BindTexture: need!("BindTexture"),
            TexImage2D: need!("TexImage2D"),
            TexParameteri: need!("TexParameteri"),
            PixelStorei: need!("PixelStorei"),
            ActiveTexture: need!("ActiveTexture"),
            DrawArrays: need!("DrawArrays"),
            PushAttrib: want!("PushAttrib"),
            PopAttrib: want!("PopAttrib"),
            PushClientAttrib: want!("PushClientAttrib"),
            PopClientAttrib: want!("PopClientAttrib"),
            MatrixMode: want!("MatrixMode"),
            PushMatrix: want!("PushMatrix"),
            PopMatrix: want!("PopMatrix"),
            LoadIdentity: want!("LoadIdentity"),
            Ortho: want!("Ortho"),
            EnableClientState: want!("EnableClientState"),
            VertexPointer: want!("VertexPointer"),
            TexCoordPointer: want!("TexCoordPointer"),
            ColorPointer: want!("ColorPointer"),
            TexEnvi: want!("TexEnvi"),
            CreateShader: want!("CreateShader"),
            ShaderSource: want!("ShaderSource"),
            CompileShader: want!("CompileShader"),
            GetShaderiv: want!("GetShaderiv"),
            CreateProgram: want!("CreateProgram"),
            AttachShader: want!("AttachShader"),
            LinkProgram: want!("LinkProgram"),
            GetProgramiv: want!("GetProgramiv"),
            UseProgram: want!("UseProgram"),
            GetUniformLocation: want!("GetUniformLocation"),
            GetAttribLocation: want!("GetAttribLocation"),
            Uniform2f: want!("Uniform2f"),
            Uniform1i: want!("Uniform1i"),
            GenVertexArrays: want!("GenVertexArrays"),
            BindVertexArray: want!("BindVertexArray"),
            GenBuffers: want!("GenBuffers"),
            BindBuffer: want!("BindBuffer"),
            BufferData: want!("BufferData"),
            EnableVertexAttribArray: want!("EnableVertexAttribArray"),
            VertexAttribPointer: want!("VertexAttribPointer"),
            GetShaderInfoLog: want!("GetShaderInfoLog"),
            GetProgramInfoLog: want!("GetProgramInfoLog"),
        })
    })
    .as_ref()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    /// 2.1: fixed function, client arrays, matrix stack.
    Legacy,
    /// 3.2 or later: a program object and a vertex array object, and nothing
    /// else is even legal.
    Core,
}

impl Profile {
    /// Which profile the current context is.
    ///
    /// **Asked of the driver, not of a string.** The first version of this parsed
    /// `GL_VERSION`, and getting it wrong is not a cosmetic failure:
    ///
    /// # Safety
    ///
    /// Must be called with the guest's context current, and with no error pending that
    /// the caller wanted to keep: this consumes the error flag.
    unsafe fn of(gl: &Gl) -> Option<Profile> {
        // SAFETY: the caller's obligation. `glGetString(GL_VERSION)` returns a
        // NUL-terminated string owned by the implementation, or null when there
        // is no context at all.
        if unsafe { (gl.GetString)(VERSION) }.is_null() {
            return None;
        }
        let mut major = 0i32;
        // SAFETY: as above. The query is expected to fail on 2.1, which is the
        // signal being read - so the error flag is cleared first, so that a
        // *pending* error is not mistaken for this query's, and drained after.
        let major_is_known = unsafe {
            drain(gl);
            (gl.GetIntegerv)(MAJOR_VERSION, &mut major);
            (gl.GetError)() == NO_ERROR
        };
        match major_is_known && major >= 3 {
            true => Some(Profile::Core),
            false => Some(Profile::Legacy),
        }
    }
}

/// Whether this context will actually accept a vertex array object call.
///
/// # Safety
///
/// A context must be current, and this consumes the error flag.
unsafe fn vao_usable(gl: &Gl) -> bool {
    let Some(bind) = gl.BindVertexArray else {
        return false;
    };
    // SAFETY: the caller's obligation. Every step checks the error flag before
    // relying on what it just did, which is the whole point of the probe.
    unsafe {
        drain(gl);
        let mut current = 0i32;
        (gl.GetIntegerv)(VERTEX_ARRAY_BINDING, &mut current);
        if (gl.GetError)() != NO_ERROR {
            return false;
        }
        bind(0);
        if (gl.GetError)() != NO_ERROR {
            return false;
        }
        bind(current as u32);
        drain(gl) == NO_ERROR
    }
}

/// Name the call that just failed, once per call site.
///
/// # Safety
///
/// A context must be current.
unsafe fn checked(gl: &Gl, label: &'static str) {
    let error = unsafe { (gl.GetError)() };
    if error == NO_ERROR {
        return;
    }
    static SAID: Mutex<Option<std::collections::BTreeSet<&'static str>>> = Mutex::new(None);
    let mut guard = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if guard.get_or_insert_with(Default::default).insert(label) {
        eprintln!("rosemary: gui  OpenGL error {error} from {label}");
    }
}

/// Wrap one OpenGL call so that, under `ROSEMARY_GUI_DEBUG`, a failure names it.
macro_rules! step {
    ($gl:expr, $label:literal, $call:expr) => {{
        $call;
        if super::debugging() {
            // SAFETY: expanded inside the same `unsafe` block as the call
            // itself, whose enclosing function has already established that a
            // context is current.
            #[allow(unused_unsafe)]
            unsafe {
                checked($gl, $label)
            };
        }
    }};
}

/// Read the error flag until it is clear, returning the first error found.
///
/// **The overlay must not leave an error behind it**, and that is the whole reason this
/// exists.
///
/// # Safety
///
/// A context must be current.
unsafe fn drain(gl: &Gl) -> u32 {
    let mut first = NO_ERROR;
    // Bounded, because a driver that returns errors forever would otherwise
    // hang the render thread inside an overlay.
    for _ in 0..16 {
        let error = unsafe { (gl.GetError)() };
        if error == NO_ERROR {
            break;
        }
        if first == NO_ERROR {
            first = error;
        }
    }
    first
}

/// `glGetString`, as a `String`, for a diagnostic.
///
/// # Safety
///
/// A context must be current.
unsafe fn describe(gl: &Gl, token: u32) -> String {
    // SAFETY: the caller's obligation; the string is the implementation's and
    // lives as long as the context.
    let raw = unsafe { (gl.GetString)(token) };
    if raw.is_null() {
        return "?".into();
    }
    unsafe { std::ffi::CStr::from_ptr(raw.cast()) }
        .to_string_lossy()
        .into_owned()
}

/// The core profile's shaders, matching [`super::metal`]'s exactly: the same
/// projection, and an atlas read as coverage.
const VERTEX_SHADER_SOURCE: &str = "#version 150\n\
    in vec2 Position;\n\
    in vec2 TexCoord;\n\
    in vec4 Color;\n\
    uniform vec2 Surface;\n\
    out vec2 vTexCoord;\n\
    out vec4 vColor;\n\
    void main() {\n\
        vTexCoord = TexCoord;\n\
        vColor = Color;\n\
        vec2 unit = Position / Surface * 2.0 - 1.0;\n\
        gl_Position = vec4(unit.x, -unit.y, 0.0, 1.0);\n\
    }\n";

const FRAGMENT_SHADER_SOURCE: &str = "#version 150\n\
    uniform sampler2D Atlas;\n\
    in vec2 vTexCoord;\n\
    in vec4 vColor;\n\
    out vec4 Fragment;\n\
    void main() {\n\
        float coverage = texture(Atlas, vTexCoord).r;\n\
        Fragment = vec4(vColor.rgb, vColor.a * coverage);\n\
    }\n";

/// How far one context has got.
///
/// **The reason this is a state and not a `bool` is a bug this had.** The first version
/// retried `build_program` whenever `program` was still zero, which is every frame
/// after a failure - so one shader that would not compile became sixty compile attempts
/// a second, each leaving an error flag for the guest to find.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Setup {
    #[default]
    Untried,
    Ready,
    /// Tried, failed, said so. Nothing further is issued into this context.
    Failed,
}

/// What one context needs, built on first sight of it.
#[derive(Default)]
struct Resources {
    setup: Setup,
    /// Decided once, from the driver rather than from a version string, and kept.
    profile: Option<Profile>,
    /// Whether this context accepts vertex array object calls at all. Probed,
    /// not assumed - see [`vao_usable`].
    vao_usable: bool,
    atlas: u32,
    atlas_key: (usize, usize),
    /// Core profile only.
    program: u32,
    vao: u32,
    vbo: u32,
    surface_uniform: i32,
    atlas_uniform: i32,
}

static CONTEXTS: Mutex<Option<HashMap<usize, Resources>>> = Mutex::new(None);

/// The context that is actually current on this thread.
fn current_context(fallback: usize) -> usize {
    type GetCurrent = unsafe extern "C" fn() -> *mut c_void;
    // Looked up once: this runs on every frame the guest presents.
    static GET: std::sync::OnceLock<Option<GetCurrent>> = std::sync::OnceLock::new();
    // SAFETY: `CGLGetCurrentContext`'s prototype from `<OpenGL/CGLCurrent.h>`;
    // it takes nothing and returns the thread's current context or null.
    let Some(get) =
        *GET.get_or_init(|| unsafe { sys::function::<GetCurrent>("CGLGetCurrentContext") })
    else {
        return fallback;
    };
    match unsafe { get() } as usize {
        0 => fallback,
        current => current,
    }
}

/// Draw the overlay into whatever is current, immediately before the swap.
pub fn present(context: usize) {
    let Some(gl) = gl() else {
        return;
    };
    let context = current_context(context);
    let mut guard = CONTEXTS.lock().unwrap_or_else(|e| e.into_inner());
    let contexts = guard.get_or_insert_with(HashMap::new);
    let resources = contexts.entry(context).or_default();
    if resources.setup == Setup::Failed {
        return;
    }

    // Decided before anything else is asked of the context, and before the draw
    // list is built: `None` is "no context is current", where every call below
    // would be a silent no-op.
    //
    // SAFETY: the hook is on the guest's own flush, so its context is the
    // current one on this thread.
    if resources.profile.is_none() {
        let Some(profile) = (unsafe { Profile::of(gl) }) else {
            return;
        };
        resources.profile = Some(profile);
        // SAFETY: as above. Probed here, on the first frame, because it is the
        // one moment nothing has been saved yet and the probe can put its own
        // disturbance back.
        resources.vao_usable = unsafe { vao_usable(gl) };
        unsafe { announce(gl, profile, resources.vao_usable) };
    }
    let Some(profile) = resources.profile else {
        return;
    };

    let Some((width, height)) = surface(gl) else {
        super::say_once("the OpenGL context reports no drawable size, so there is nowhere to draw");
        return;
    };
    let Some(list) = super::compose(width, height) else {
        return;
    };

    // SAFETY of everything below: the guest's context is current on this
    // thread - the hook is on its own flush - and every call is made with the
    // arguments its prototype declares. The state the calls change is saved
    // first and restored last, which is the invariant this whole file is about.
    let saved = unsafe { Saved::take(gl, profile, resources.vao_usable) };
    unsafe {
        // Nothing is drawn if the guest's state could not be saved, because
        // every call below changes state that would then never be put back.
        if !saved.can_draw(profile) {
            saved.restore(gl, profile);
            return;
        }
        upload_atlas(gl, resources, &list, profile);
        match profile {
            Profile::Legacy => draw_legacy(gl, resources, &list),
            Profile::Core => draw_core(gl, resources, &list),
        }
        super::drew(
            match profile {
                Profile::Core => "OpenGL core",
                Profile::Legacy => "OpenGL legacy",
            },
            list.width,
            list.height,
            list.vertices.len(),
        );
        saved.restore(gl, profile);

        // Whatever happened above stays here. See [`drain`].
        let error = drain(gl);
        if error != NO_ERROR {
            report(gl, resources, error);
        }
    }
}

/// Say what context the overlay decided it was drawing into.
///
/// # Safety
///
/// A context must be current.
unsafe fn announce(gl: &Gl, profile: Profile, vao_usable: bool) {
    if !super::debugging() {
        return;
    }
    unsafe {
        eprintln!(
            "rosemary: gui  OpenGL {} / {} / GLSL {} -> the {} path, vertex array objects {}",
            describe(gl, VERSION),
            describe(gl, RENDERER),
            describe(gl, SHADING_LANGUAGE_VERSION),
            match profile {
                Profile::Core => "core",
                Profile::Legacy => "legacy",
            },
            match vao_usable {
                true => "usable",
                false => "refused by this context",
            },
        )
    };
}

/// An error the overlay caused.
///
/// # Safety
///
/// A context must be current.
unsafe fn report(gl: &Gl, resources: &Resources, error: u32) {
    // Once.
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    unsafe {
        eprintln!(
            "rosemary: gui  the overlay left OpenGL error {error} on {} / {} ({} path); \
             it keeps drawing, the guest never sees it, and this is said once. \
             ROSEMARY_GUI_DEBUG=1 names the call responsible",
            describe(gl, VERSION),
            describe(gl, RENDERER),
            match resources.profile {
                Some(Profile::Core) => "core",
                Some(Profile::Legacy) => "legacy",
                None => "unknown",
            },
        )
    };
}

/// The drawable's size in pixels.
///
/// The context's own view first, converted to backing pixels so that a retina window
/// reports 2560×1600 rather than 1280×800 - the overlay is in pixels and half-size text
/// on a retina display is the bug this avoids.
fn surface(gl: &Gl) -> Option<(f32, f32)> {
    let ns_context = class("NSOpenGLContext");
    if !ns_context.is_null() {
        // SAFETY: `+currentContext`, `-view` and `-bounds`/`-convertRectToBacking:`
        // are documented AppKit methods with these prototypes. `-bounds` and the
        // conversion return a `CGRect`, which AAPCS64 returns in registers -
        // there is no `_stret` on this architecture.
        let current = unsafe { send0::<Id>(ns_context, super::objc::sel("currentContext")) };
        let view = match current.is_null() {
            true => std::ptr::null_mut(),
            false => unsafe { send0::<Id>(current, super::objc::sel("view")) },
        };
        if !view.is_null() {
            let bounds = unsafe { send0::<sys::Rect>(view, super::objc::sel("bounds")) };
            let backing = unsafe {
                send1::<sys::Rect, sys::Rect>(
                    view,
                    super::objc::sel("convertRectToBacking:"),
                    bounds,
                )
            };
            if backing.size.width >= 1.0 && backing.size.height >= 1.0 {
                return Some((backing.size.width as f32, backing.size.height as f32));
            }
        }
    }
    let mut viewport = [0i32; 4];
    // SAFETY: a context is current and `GL_VIEWPORT` writes exactly four
    // integers.
    unsafe { (gl.GetIntegerv)(VIEWPORT, viewport.as_mut_ptr()) };
    match viewport[2] > 0 && viewport[3] > 0 {
        true => Some((viewport[2] as f32, viewport[3] as f32)),
        false => None,
    }
}

/// The guest's state, as it was before the overlay touched anything.
#[derive(Default)]
struct Saved {
    program: i32,
    vao: i32,
    array_buffer: i32,
    active_texture: i32,
    texture: i32,
    viewport: [i32; 4],
    blend: bool,
    blend_src_rgb: i32,
    blend_dst_rgb: i32,
    blend_src_alpha: i32,
    blend_dst_alpha: i32,
    depth: bool,
    cull: bool,
    scissor: bool,
    /// Copied from the context's [`Resources`], so that restore touches exactly
    /// what take touched.
    vao_usable: bool,
    /// Whether the attribute stacks actually accepted the push.
    pushed: bool,
}

/// One integer of context state.
///
/// # Safety
///
/// A context must be current, and `token` must be one that writes exactly one
/// integer - every token this file passes is.
unsafe fn integer(gl: &Gl, token: u32) -> i32 {
    let mut value = 0i32;
    // SAFETY: the caller's obligation, and `value` is a live local.
    unsafe { (gl.GetIntegerv)(token, &mut value) };
    value
}

impl Saved {
    /// # Safety
    ///
    /// A context must be current on this thread.
    unsafe fn take(gl: &Gl, profile: Profile, vao_usable: bool) -> Saved {
        let mut saved = Saved {
            vao_usable,
            ..Saved::default()
        };
        // SAFETY: the caller's obligation; every query below writes into a live
        // local of the size the token names.
        unsafe {
            (gl.GetIntegerv)(VIEWPORT, saved.viewport.as_mut_ptr());
            // **Before the profile split, because the attribute stacks do not
            // cover these.** `glPushAttrib` saves the fixed-function machine -
            // enables, blend, the texture environment - and
            // `glPushClientAttrib` saves the array pointers.
            saved.program = integer(gl, CURRENT_PROGRAM);
            if vao_usable {
                saved.vao = integer(gl, VERTEX_ARRAY_BINDING);
            }
            saved.array_buffer = integer(gl, ARRAY_BUFFER_BINDING);
            if profile == Profile::Legacy {
                // The driver's own save for everything that *is* covered, which
                // is more complete than a list written here could be - provided
                // it took it. See [`Saved::pushed`].
                if let (Some(push), Some(push_client)) = (gl.PushAttrib, gl.PushClientAttrib) {
                    drain(gl);
                    push(ALL_ATTRIB_BITS);
                    push_client(CLIENT_ALL_ATTRIB_BITS);
                    saved.pushed = (gl.GetError)() == NO_ERROR;
                    if !saved.pushed {
                        super::say_once(
                            "the guest's OpenGL attribute stack is full, so the overlay cannot \
                             save its state and will not draw this frame",
                        );
                    }
                }
                return saved;
            }
            saved.active_texture = integer(gl, ACTIVE_TEXTURE);
            // Read after selecting unit zero, because that is the unit the
            // overlay binds its atlas to and therefore the only binding it
            // disturbs.
            (gl.ActiveTexture)(TEXTURE0);
            saved.texture = integer(gl, TEXTURE_BINDING_2D);
            saved.blend_src_rgb = integer(gl, BLEND_SRC_RGB);
            saved.blend_dst_rgb = integer(gl, BLEND_DST_RGB);
            saved.blend_src_alpha = integer(gl, BLEND_SRC_ALPHA_STATE);
            saved.blend_dst_alpha = integer(gl, BLEND_DST_ALPHA_STATE);
            saved.blend = (gl.IsEnabled)(BLEND) != 0;
            saved.depth = (gl.IsEnabled)(DEPTH_TEST) != 0;
            saved.cull = (gl.IsEnabled)(CULL_FACE) != 0;
            saved.scissor = (gl.IsEnabled)(SCISSOR_TEST) != 0;
        }
        saved
    }

    /// Whether the overlay may touch this context's state at all.
    fn can_draw(&self, profile: Profile) -> bool {
        profile != Profile::Legacy || self.pushed
    }

    /// # Safety
    ///
    /// The same context must still be current.
    unsafe fn restore(&self, gl: &Gl, profile: Profile) {
        // SAFETY: the caller's obligation; each value came out of the same
        // context's own state a few microseconds ago.
        unsafe {
            if profile == Profile::Legacy {
                // The two the attribute stacks do not carry. See [`Saved::take`].
                if let Some(use_program) = gl.UseProgram {
                    use_program(self.program as u32);
                }
                if let Some(bind_vao) = gl.BindVertexArray
                    && self.vao_usable
                {
                    bind_vao(self.vao as u32);
                }
                // Only if the push was taken. Popping an unpushed stack is
                // what [`Saved::pushed`] exists to prevent.
                if let (Some(pop), Some(pop_client)) = (gl.PopAttrib, gl.PopClientAttrib)
                    && self.pushed
                {
                    pop_client();
                    pop();
                }
                (gl.Viewport)(
                    self.viewport[0],
                    self.viewport[1],
                    self.viewport[2],
                    self.viewport[3],
                );
                return;
            }
            if let Some(use_program) = gl.UseProgram {
                use_program(self.program as u32);
            }
            if let Some(bind_vao) = gl.BindVertexArray {
                bind_vao(self.vao as u32);
            }
            if let Some(bind_buffer) = gl.BindBuffer {
                bind_buffer(ARRAY_BUFFER, self.array_buffer as u32);
            }
            (gl.BindTexture)(TEXTURE_2D, self.texture as u32);
            (gl.ActiveTexture)(self.active_texture as u32);
            (gl.BlendFuncSeparate)(
                self.blend_src_rgb as u32,
                self.blend_dst_rgb as u32,
                self.blend_src_alpha as u32,
                self.blend_dst_alpha as u32,
            );
            for (enabled, token) in [
                (self.blend, BLEND),
                (self.depth, DEPTH_TEST),
                (self.cull, CULL_FACE),
                (self.scissor, SCISSOR_TEST),
            ] {
                match enabled {
                    true => (gl.Enable)(token),
                    false => (gl.Disable)(token),
                }
            }
            (gl.Viewport)(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
        }
    }
}

/// Every piece of *pixel unpack* state a texture upload reads, saved.
///
/// So the upload sets all of it and puts it back. This is the shape of every bug this
/// backend has: OpenGL's state is the guest's, and reading it wrong is silent.
struct Unpack {
    buffer: i32,
    swap_bytes: i32,
    lsb_first: i32,
    row_length: i32,
    skip_rows: i32,
    skip_pixels: i32,
    alignment: i32,
}

/// The pixel-store parameters, paired with the value the upload needs.
const UNPACK_DEFAULTS: [(u32, i32); 6] = [
    (UNPACK_SWAP_BYTES, 0),
    (UNPACK_LSB_FIRST, 0),
    (UNPACK_ROW_LENGTH, 0),
    (UNPACK_SKIP_ROWS, 0),
    (UNPACK_SKIP_PIXELS, 0),
    // One, because the atlas is one byte per texel and as wide as it needs to
    // be - which is very often not a multiple of four.
    (UNPACK_ALIGNMENT, 1),
];

impl Unpack {
    /// # Safety
    ///
    /// A context must be current.
    unsafe fn take(gl: &Gl) -> Unpack {
        // SAFETY: the caller's obligation; each token writes one integer.
        unsafe {
            Unpack {
                buffer: integer(gl, PIXEL_UNPACK_BUFFER_BINDING),
                swap_bytes: integer(gl, UNPACK_SWAP_BYTES),
                lsb_first: integer(gl, UNPACK_LSB_FIRST),
                row_length: integer(gl, UNPACK_ROW_LENGTH),
                skip_rows: integer(gl, UNPACK_SKIP_ROWS),
                skip_pixels: integer(gl, UNPACK_SKIP_PIXELS),
                alignment: integer(gl, UNPACK_ALIGNMENT),
            }
        }
    }

    /// # Safety
    ///
    /// The same context must still be current.
    unsafe fn restore(&self, gl: &Gl) {
        // SAFETY: the caller's obligation; every value came from this context.
        unsafe {
            for (token, value) in [
                (UNPACK_SWAP_BYTES, self.swap_bytes),
                (UNPACK_LSB_FIRST, self.lsb_first),
                (UNPACK_ROW_LENGTH, self.row_length),
                (UNPACK_SKIP_ROWS, self.skip_rows),
                (UNPACK_SKIP_PIXELS, self.skip_pixels),
                (UNPACK_ALIGNMENT, self.alignment),
            ] {
                (gl.PixelStorei)(token, value);
            }
            if let Some(bind_buffer) = gl.BindBuffer {
                bind_buffer(PIXEL_UNPACK_BUFFER, self.buffer as u32);
            }
        }
    }
}

/// # Safety
///
/// A context must be current, and `resources` must belong to it.
unsafe fn upload_atlas(gl: &Gl, resources: &mut Resources, list: &DrawList, profile: Profile) {
    let atlas = list.atlas();
    let key = (atlas.width, atlas.height);
    if resources.atlas != 0 && resources.atlas_key == key {
        return;
    }
    // SAFETY: the caller's obligation. A texture name is generated once per
    // context and reused; when the atlas changes size the same name is
    // respecified, which is what `glTexImage2D` is for.
    unsafe {
        if resources.atlas == 0 {
            (gl.GenTextures)(1, &mut resources.atlas);
        }
        (gl.ActiveTexture)(TEXTURE0);
        (gl.BindTexture)(TEXTURE_2D, resources.atlas);

        // See [`Unpack`]: without this the upload is read out of whatever
        // buffer the guest last bound, at whatever stride it last set.
        let unpack = Unpack::take(gl);
        if let Some(bind_buffer) = gl.BindBuffer {
            bind_buffer(PIXEL_UNPACK_BUFFER, 0);
        }
        for (token, value) in UNPACK_DEFAULTS {
            (gl.PixelStorei)(token, value);
        }

        // A single-channel texture, spelled the way each profile spells it.
        let (internal, format) = match profile {
            Profile::Core => (R8 as i32, RED),
            Profile::Legacy => (ALPHA as i32, ALPHA),
        };
        (gl.TexImage2D)(
            TEXTURE_2D,
            0,
            internal,
            atlas.width as i32,
            atlas.height as i32,
            0,
            format,
            UNSIGNED_BYTE,
            atlas.pixels.as_ptr().cast(),
        );
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MIN_FILTER, NEAREST);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_MAG_FILTER, NEAREST);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_WRAP_S, CLAMP_TO_EDGE);
        (gl.TexParameteri)(TEXTURE_2D, TEXTURE_WRAP_T, CLAMP_TO_EDGE);
        unpack.restore(gl);
    }
    resources.atlas_key = key;
}

/// # Safety
///
/// A 2.1 context must be current.
unsafe fn draw_legacy(gl: &Gl, resources: &mut Resources, list: &DrawList) {
    let (
        Some(matrix_mode),
        Some(push_matrix),
        Some(pop_matrix),
        Some(load_identity),
        Some(ortho),
        Some(enable_client),
        Some(vertex_pointer),
        Some(texcoord_pointer),
        Some(color_pointer),
        Some(tex_env),
    ) = (
        gl.MatrixMode,
        gl.PushMatrix,
        gl.PopMatrix,
        gl.LoadIdentity,
        gl.Ortho,
        gl.EnableClientState,
        gl.VertexPointer,
        gl.TexCoordPointer,
        gl.ColorPointer,
        gl.TexEnvi,
    )
    else {
        return;
    };

    let base = list.vertices.as_ptr().cast::<u8>();
    let stride = size_of::<Vertex>() as i32;
    // SAFETY: the caller's obligation. The offsets below are this crate's own
    // `Vertex`, whose layout is fixed by `repr(C)`; the pointers stay valid for
    // the `glDrawArrays` that reads them because `list` outlives this function
    // and no buffer is bound to intercept them.
    unsafe {
        // **First, because nothing below has any effect otherwise.** A 2.1
        // context can still have a program bound - A Hat in Time's has one, at
        // GLSL 1.20 - and while it is bound the matrices, the texture
        // environment and the client arrays set up here are all inputs to a
        // pipeline that is switched off.
        if let Some(use_program) = gl.UseProgram {
            step!(gl, "glUseProgram(0)", use_program(0));
        }
        if let Some(bind_vao) = gl.BindVertexArray
            && resources.vao_usable
        {
            step!(gl, "glBindVertexArray(0)", bind_vao(0));
        }

        step!(
            gl,
            "glViewport",
            (gl.Viewport)(0, 0, list.width as i32, list.height as i32)
        );
        step!(gl, "glMatrixMode(GL_PROJECTION)", matrix_mode(PROJECTION));
        step!(gl, "glPushMatrix(projection)", push_matrix());
        load_identity();
        // Top-left origin, matching the draw list and matching what the core
        // path's vertex shader computes.
        step!(
            gl,
            "glOrtho",
            ortho(
                0.0,
                f64::from(list.width),
                f64::from(list.height),
                0.0,
                -1.0,
                1.0,
            )
        );
        step!(gl, "glMatrixMode(GL_MODELVIEW)", matrix_mode(MODELVIEW));
        step!(gl, "glPushMatrix(modelview)", push_matrix());
        load_identity();

        for token in [DEPTH_TEST, CULL_FACE, SCISSOR_TEST, LIGHTING] {
            step!(gl, "glDisable", (gl.Disable)(token));
        }
        step!(gl, "glEnable(GL_BLEND)", (gl.Enable)(BLEND));
        step!(
            gl,
            "glBlendFunc",
            (gl.BlendFunc)(SRC_ALPHA, ONE_MINUS_SRC_ALPHA)
        );
        step!(gl, "glEnable(GL_TEXTURE_2D)", (gl.Enable)(TEXTURE_2D));
        step!(
            gl,
            "glActiveTexture(GL_TEXTURE0)",
            (gl.ActiveTexture)(TEXTURE0)
        );
        step!(
            gl,
            "glBindTexture(the overlay's atlas)",
            (gl.BindTexture)(TEXTURE_2D, resources.atlas)
        );
        // `GL_MODULATE` over a `GL_ALPHA` texture leaves the fragment colour
        // alone and multiplies its alpha by the coverage - which is the fixed
        // function spelling of the core profile's fragment shader.
        step!(
            gl,
            "glTexEnvi(GL_MODULATE)",
            tex_env(TEXTURE_ENV, TEXTURE_ENV_MODE, MODULATE)
        );

        // Client arrays read from our memory, which means no buffer may be
        // bound.
        if let Some(bind_buffer) = gl.BindBuffer {
            step!(
                gl,
                "glBindBuffer(GL_ARRAY_BUFFER, 0)",
                bind_buffer(ARRAY_BUFFER, 0)
            );
        }
        for state in [VERTEX_ARRAY, TEXTURE_COORD_ARRAY, COLOR_ARRAY] {
            step!(gl, "glEnableClientState", enable_client(state));
        }
        step!(
            gl,
            "glVertexPointer",
            vertex_pointer(2, FLOAT, stride, base.cast())
        );
        step!(
            gl,
            "glTexCoordPointer",
            texcoord_pointer(2, FLOAT, stride, base.add(8).cast())
        );
        step!(
            gl,
            "glColorPointer",
            color_pointer(4, FLOAT, stride, base.add(16).cast())
        );
        step!(
            gl,
            "glDrawArrays",
            (gl.DrawArrays)(TRIANGLES, 0, list.vertices.len() as i32)
        );

        step!(
            gl,
            "glMatrixMode(GL_MODELVIEW) again",
            matrix_mode(MODELVIEW)
        );
        step!(gl, "glPopMatrix(modelview)", pop_matrix());
        step!(
            gl,
            "glMatrixMode(GL_PROJECTION) again",
            matrix_mode(PROJECTION)
        );
        step!(gl, "glPopMatrix(projection)", pop_matrix());
    }
}

/// # Safety
///
/// A core-profile context must be current.
unsafe fn draw_core(gl: &Gl, resources: &mut Resources, list: &DrawList) {
    // Built once per context, and **once** is the operative word: a failure
    // marks the context [`Setup::Failed`] and nothing is issued into it again.
    if resources.setup == Setup::Untried {
        resources.setup = match unsafe { build_program(gl, resources) } {
            true => Setup::Ready,
            false => Setup::Failed,
        };
    }
    if resources.setup != Setup::Ready {
        return;
    }
    let (
        Some(use_program),
        Some(bind_vao),
        Some(bind_buffer),
        Some(buffer_data),
        Some(uniform2f),
        Some(uniform1i),
    ) = (
        gl.UseProgram,
        gl.BindVertexArray,
        gl.BindBuffer,
        gl.BufferData,
        gl.Uniform2f,
        gl.Uniform1i,
    )
    else {
        return;
    };

    // SAFETY: the caller's obligation. The buffer is respecified whole with
    // `glBufferData`, which is the orphaning idiom - the driver hands back new
    // storage rather than waiting for the previous frame's draw to finish.
    unsafe {
        (gl.Viewport)(0, 0, list.width as i32, list.height as i32);
        for token in [DEPTH_TEST, CULL_FACE, SCISSOR_TEST] {
            (gl.Disable)(token);
        }
        (gl.Enable)(BLEND);
        (gl.BlendFuncSeparate)(
            SRC_ALPHA,
            ONE_MINUS_SRC_ALPHA,
            SRC_ALPHA,
            ONE_MINUS_SRC_ALPHA,
        );
        use_program(resources.program);
        uniform2f(resources.surface_uniform, list.width, list.height);
        uniform1i(resources.atlas_uniform, 0);
        (gl.ActiveTexture)(TEXTURE0);
        (gl.BindTexture)(TEXTURE_2D, resources.atlas);
        bind_vao(resources.vao);
        bind_buffer(ARRAY_BUFFER, resources.vbo);
        buffer_data(
            ARRAY_BUFFER,
            std::mem::size_of_val(&list.vertices[..]) as isize,
            list.vertices.as_ptr().cast(),
            STREAM_DRAW,
        );
        (gl.DrawArrays)(TRIANGLES, 0, list.vertices.len() as i32);
    }
}

/// A shader's or a program's info log.
///
/// # Safety
///
/// A context must be current and `object` must be of the kind the two function pointers
/// are for.
unsafe fn info_log(
    get_log: Option<unsafe extern "C" fn(u32, i32, *mut i32, *mut u8)>,
    get_iv: Option<unsafe extern "C" fn(u32, u32, *mut i32)>,
    object: u32,
) -> String {
    let (Some(get_log), Some(get_iv)) = (get_log, get_iv) else {
        return "(no info log available)".into();
    };
    let mut length = 0i32;
    // SAFETY: the caller's obligation; `length` is a live local.
    unsafe { get_iv(object, INFO_LOG_LENGTH, &mut length) };
    // The reported length includes the NUL. A driver with nothing to say
    // reports zero or one.
    if length <= 1 {
        return "(the driver gave no reason)".into();
    }
    let mut buffer = vec![0u8; length as usize];
    let mut written = 0i32;
    // SAFETY: the buffer is `length` bytes and the call is told so; the driver
    // writes at most that many including the NUL.
    unsafe { get_log(object, length, &mut written, buffer.as_mut_ptr()) };
    buffer.truncate(written.max(0) as usize);
    String::from_utf8_lossy(&buffer).trim_end().to_string()
}

/// Compile, link and describe the vertex layout. Once per context.
///
/// # Safety
///
/// A core-profile context must be current.
unsafe fn build_program(gl: &Gl, resources: &mut Resources) -> bool {
    let (
        Some(create_shader),
        Some(shader_source),
        Some(compile),
        Some(get_shader),
        Some(create_program),
        Some(attach),
        Some(link),
        Some(get_program),
        Some(uniform_location),
        Some(attrib_location),
        Some(gen_vao),
        Some(bind_vao),
        Some(gen_buffers),
        Some(bind_buffer),
        Some(enable_attrib),
        Some(attrib_pointer),
    ) = (
        gl.CreateShader,
        gl.ShaderSource,
        gl.CompileShader,
        gl.GetShaderiv,
        gl.CreateProgram,
        gl.AttachShader,
        gl.LinkProgram,
        gl.GetProgramiv,
        gl.GetUniformLocation,
        gl.GetAttribLocation,
        gl.GenVertexArrays,
        gl.BindVertexArray,
        gl.GenBuffers,
        gl.BindBuffer,
        gl.EnableVertexAttribArray,
        gl.VertexAttribPointer,
    )
    else {
        return false;
    };

    // SAFETY: the caller's obligation. Each source is a live NUL-terminated
    // string for the length of the `glShaderSource` that copies it.
    let stage = |kind: u32, source: &str| -> u32 {
        let Ok(c) = CString::new(source) else {
            return 0;
        };
        unsafe {
            let shader = create_shader(kind);
            if shader == 0 {
                // Zero is not a shader name, and passing it to the three calls
                // below would be three `GL_INVALID_VALUE`s - which is how this
                // failure reached the guest's error checking in the first place.
                return 0;
            }
            let pointer = c.as_ptr().cast::<u8>();
            shader_source(shader, 1, &pointer, std::ptr::null());
            compile(shader);
            let mut ok = 0i32;
            get_shader(shader, COMPILE_STATUS, &mut ok);
            if ok != 0 {
                return shader;
            }
            // The driver's own account of what was wrong with the source.
            eprintln!(
                "rosemary: gui  the overlay's {} shader would not compile on GLSL {}:\n{}",
                match kind == VERTEX_SHADER {
                    true => "vertex",
                    false => "fragment",
                },
                describe(gl, SHADING_LANGUAGE_VERSION),
                info_log(gl.GetShaderInfoLog, gl.GetShaderiv, shader),
            );
            0
        }
    };

    let vertex = stage(VERTEX_SHADER, VERTEX_SHADER_SOURCE);
    let fragment = stage(FRAGMENT_SHADER, FRAGMENT_SHADER_SOURCE);
    if vertex == 0 || fragment == 0 {
        return false;
    }

    // SAFETY: the caller's obligation, and the shaders above compiled.
    unsafe {
        let program = create_program();
        attach(program, vertex);
        attach(program, fragment);
        link(program);
        let mut ok = 0i32;
        get_program(program, LINK_STATUS, &mut ok);
        if ok == 0 {
            eprintln!(
                "rosemary: gui  the overlay's OpenGL program would not link:\n{}",
                info_log(gl.GetProgramInfoLog, gl.GetProgramiv, program),
            );
            return false;
        }
        let name = |s: &str| CString::new(s).expect("no interior NUL in a literal");
        resources.surface_uniform = uniform_location(program, name("Surface").as_ptr().cast());
        resources.atlas_uniform = uniform_location(program, name("Atlas").as_ptr().cast());
        let position = attrib_location(program, name("Position").as_ptr().cast());
        let texcoord = attrib_location(program, name("TexCoord").as_ptr().cast());
        let color = attrib_location(program, name("Color").as_ptr().cast());

        gen_vao(1, &mut resources.vao);
        gen_buffers(1, &mut resources.vbo);
        bind_vao(resources.vao);
        bind_buffer(ARRAY_BUFFER, resources.vbo);
        let stride = size_of::<Vertex>() as i32;
        for (location, size, offset) in [(position, 2, 0usize), (texcoord, 2, 8), (color, 4, 16)] {
            if location < 0 {
                continue;
            }
            enable_attrib(location as u32);
            attrib_pointer(
                location as u32,
                size,
                FLOAT,
                0,
                stride,
                std::ptr::without_provenance::<c_void>(offset),
            );
        }
        resources.program = program;
    }
    true
}
