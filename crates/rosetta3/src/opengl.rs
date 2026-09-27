use std::ffi::CString;

use machish::resolve::{LazyDylib, NativeDylibProvider, Resolved};

/// Whether GL entry points are answered here rather than by the host loader.
///
/// Off on macOS, where SDL makes its contexts against the same
/// `OpenGL.framework` the loader would bind - there is nothing to keep apart -
/// and where a guest on Apple's own AppKit never brings SDL up at all.
pub fn interposed() -> bool {
    !cfg!(target_os = "macos")
}

/// The paths a guest names the framework by: the install name an
/// `LC_LOAD_DYLIB` records, and the spellings a `dlopen` uses.
pub const INSTALL_PATHS: &[&str] = &[
    "/System/Library/Frameworks/OpenGL.framework/Versions/Current/OpenGL",
    "/System/Library/Frameworks/OpenGL.framework/Versions/A/OpenGL",
    "/System/Library/Frameworks/OpenGL.framework/OpenGL",
    "/System/Library/Frameworks/OpenGL.framework/Versions/Current/Libraries/libGL.dylib",
    "/System/Library/Frameworks/OpenGL.framework/Versions/A/Libraries/libGL.dylib",
    "/System/Library/Frameworks/OpenGL.framework/Libraries/libGL.dylib",
    "OpenGL",
];

/// The framework as a provider, for the link chains in [`crate::program`] and
/// [`crate::exec::native`].
///
/// Flat, and ahead of the host provider, so that a two-level import naming the
/// framework and a flat one naming nothing both land here. Lazy because that
/// placement would otherwise open a display connection and a GL driver in every
/// process, including the ones that never draw.
pub fn provider() -> LazyDylib {
    let mut lib = LazyDylib::new(INSTALL_PATHS[0], open, resolve);
    for name in &INSTALL_PATHS[1..] {
        lib.alias(name);
    }
    // Flat, so every import of every guest passes through here; opening the
    // library for one that is not its own would bring up the host driver in a
    // guest that never uses it.
    lib.flat(interposed())
        .claims(|symbol| symbol.strip_prefix('_').is_some_and(is_gl));
    lib
}

/// Keep the host's GL out of a guest that has SDL's.
///
/// The failure this exists to stop is not a missing symbol: the host loader has
/// a `libGL.so.1` of its own and answers every `gl` name from it, so the guest
/// links, runs, and draws through a driver that never saw the context SDL made.
/// What that looks like is a blank window, a GL error that names nothing, or a
/// crash inside the driver - never a report that two libraries were mixed.
///
/// The whole family, `glu` included: GLU lives in `OpenGL.framework` on Darwin,
/// so this provider is where it belongs here too. See [`resolve`].
pub fn deny_host_gl(provider: &mut NativeDylibProvider) {
    if interposed() {
        provider.deny_prefix("_gl");
    }
}

/// The address a `dlsym` of `name` should answer, for [`crate::frameworks`].
///
/// The C name, without the leading underscore an import carries.
pub fn symbol(name: &str) -> Option<u64> {
    if !interposed() || !is_gl(name) || !open() {
        return None;
    }
    address(name)
}

fn is_gl(name: &str) -> bool {
    name.starts_with("gl")
}

fn open() -> bool {
    static LOADED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LOADED.get_or_init(|| {
        let loaded = sdl3::video::gl_load_library();
        if !loaded {
            // Said once and here, because the symptom otherwise is every `gl`
            // import refused by name, which reads as a missing shim table
            // rather than as a driver that could not be opened.
            eprintln!(
                "rosetta3: SDL could not load a GL library ({}); the guest's OpenGL imports \
                 will be refused",
                sdl3::error()
            );
        }
        loaded
    })
}

fn resolve(symbol: &str) -> Option<Resolved> {
    let name = symbol.strip_prefix('_')?;
    is_gl(name)
        .then(|| address(name))
        .flatten()
        .map(Resolved::function)
}

fn address(name: &str) -> Option<u64> {
    // A few entry points are this runtime's rather than the driver's, because
    // what passes through them needs changing on the way. See [`crate::glsl`].
    if let Some(ours) = crate::glsl::address(name).or_else(|| crate::macgl::address(name)) {
        return Some(ours);
    }
    if name == "glDrawElements" {
        return Some(draw_elements as *const () as usize as u64);
    }
    // Apple's 2.1 contexts expose vertex array objects through the APPLE
    // extension. Mesa may return addresses for those old spellings even when
    // the active context implements only the core/ARB object, in which case
    // calls quietly leave VAO zero bound. Their prototypes and semantics are
    // identical, so route the three object operations to the core names.
    let host_name = match name {
        "glBindVertexArrayAPPLE" => "glBindVertexArray",
        "glGenVertexArraysAPPLE" => "glGenVertexArrays",
        "glDeleteVertexArraysAPPLE" => "glDeleteVertexArrays",
        _ => name,
    };
    let c = CString::new(host_name).ok()?;
    // GLU is not the driver's. It is a client-side library layered on GL, which
    // SDL does not load and whose names `SDL_GL_GetProcAddress` therefore does
    // not know - so this is the one part of the framework that is still the
    // host's, and the `gl` denial above is what routes it through here.
    if name.starts_with("glu") {
        return darwin::host_symbol(name);
    }
    sdl3::video::gl_proc_address(&c).map(|p| p as usize as u64)
}

/// Keep a malformed indexed draw from taking the host driver down with it.
/// Apple's GL treats an index argument as an offset whenever an element array
/// buffer is bound; Mesa dereferences a small offset as a client pointer when
/// no such buffer is present. The diagnostic names the lost state at the ABI
/// boundary instead of leaving a driver crash with no GL error.
unsafe extern "C" fn draw_elements(mode_: u32, count: i32, type_: u32, indices: *const u8) {
    type Draw = unsafe extern "C" fn(u32, i32, u32, *const u8);
    type Get = unsafe extern "C" fn(u32, *mut i32);
    static DRAW: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    static GET: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let draw = *DRAW.get_or_init(|| {
        sdl3::video::gl_proc_address(c"glDrawElements").map(|entry| entry as usize)
    });
    let get = *GET
        .get_or_init(|| sdl3::video::gl_proc_address(c"glGetIntegerv").map(|entry| entry as usize));
    let (Some(draw), Some(get)) = (draw, get) else {
        return;
    };
    // SAFETY: both pointers came from SDL for the named GL prototypes.
    let (draw, get): (Draw, Get) = unsafe { (std::mem::transmute(draw), std::mem::transmute(get)) };
    const ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
    const VERTEX_ARRAY_BINDING: u32 = 0x85b5;
    let (mut element, mut vao) = (0, 0);
    unsafe {
        get(ELEMENT_ARRAY_BUFFER_BINDING, &mut element);
        get(VERTEX_ARRAY_BINDING, &mut vao);
    }
    if element == 0 && (indices as usize) < 0x1_0000 {
        eprintln!(
            "rosetta3: OpenGL: glDrawElements has VAO {vao}, no element-array buffer, and offset {:#x}; draw skipped",
            indices as usize
        );
        return;
    }
    unsafe { draw(mode_, count, type_, indices) };
}
