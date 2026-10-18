use std::ffi::{CStr, CString};

use machish::resolve::{LazyDylib, NativeDylibProvider, Resolved};

/// Built-in frameworks always draw through SDL's GL backend.
pub fn interposed() -> bool {
    crate::frameworks::using_builtin()
}

/// Symbol families owned here, including unsupported entry points.
pub fn owns(name: &str) -> bool {
    interposed() && (name.starts_with("gl") || name.starts_with("CGL"))
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
    let mut lib = LazyDylib::new(INSTALL_PATHS[0], interposed, resolve);
    for name in &INSTALL_PATHS[1..] {
        lib.alias(name);
    }
    // Claims are backend-dependent even for explicitly named imports. Driver
    // loading stays in symbol(), so CGL queries need no graphics initialization.
    lib.flat(interposed())
        .claims(|symbol| symbol.strip_prefix('_').is_some_and(owns));
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
        provider.deny_prefix("_CGL");
    }
}

/// The address a `dlsym` of `name` should answer, for [`crate::frameworks`].
///
/// The C name, without the leading underscore an import carries.
pub fn symbol(name: &str) -> Option<u64> {
    if !owns(name) {
        return None;
    }
    // Opaque CGL handles and queries do not require a display or driver.
    if name.starts_with("CGL") {
        return appkit::symbol(name);
    }
    if name.starts_with("glu") && name != "gluOrtho2D" {
        return None;
    }
    open().then(|| address(name)).flatten()
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
                "rosemary: SDL could not load a GL library ({}); the guest's OpenGL imports \
                 will be refused",
                sdl3::error()
            );
        }
        loaded
    })
}

fn resolve(name: &str) -> Option<Resolved> {
    symbol(name.strip_prefix('_')?).map(Resolved::function)
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
    let wrapper = match name {
        "glBindVertexArrayAPPLE" => Some(bind_vertex_array as *const ()),
        "glGenVertexArraysAPPLE" => Some(gen_vertex_arrays as *const ()),
        "glDeleteVertexArraysAPPLE" => Some(delete_vertex_arrays as *const ()),
        "gluOrtho2D" => Some(ortho_2d as *const ()),
        _ => None,
    };
    if let Some(wrapper) = wrapper {
        return Some(wrapper as usize as u64);
    }
    let c = CString::new(name).ok()?;
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
    let (draw, get) = unsafe {
        (
            std::mem::transmute::<usize, Draw>(draw),
            std::mem::transmute::<usize, Get>(get),
        )
    };
    const ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
    const VERTEX_ARRAY_BINDING: u32 = 0x85b5;
    let (mut element, mut vao) = (0, 0);
    unsafe {
        get(ELEMENT_ARRAY_BUFFER_BINDING, &mut element);
        get(VERTEX_ARRAY_BINDING, &mut vao);
    }
    if element == 0 && (indices as usize) < 0x1_0000 {
        eprintln!(
            "rosemary: OpenGL: glDrawElements has VAO {vao}, no element-array buffer, and offset {:#x}; draw skipped",
            indices as usize
        );
        return;
    }
    unsafe { draw(mode_, count, type_, indices) };
}

/// Select VAO entry points by the active driver's capabilities. A legacy
/// Apple legacy contexts have APPLE VAOs; other drivers can expose core/ARB VAOs.
fn vao_entry(core: &CStr, apple: &CStr) -> Option<*mut std::ffi::c_void> {
    let core_supported = sdl3::video::gl_current_version().is_some()
        || sdl3::video::gl_extension_supported(c"GL_ARB_vertex_array_object");
    let name = if core_supported {
        core
    } else if sdl3::video::gl_extension_supported(c"GL_APPLE_vertex_array_object") {
        apple
    } else {
        return None;
    };
    sdl3::video::gl_proc_address(name)
}

unsafe extern "C" fn bind_vertex_array(array: u32) {
    if let Some(entry) = vao_entry(c"glBindVertexArray", c"glBindVertexArrayAPPLE") {
        // SAFETY: both spellings have the prototype void (GLuint).
        let f: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(entry) };
        unsafe { f(array) };
    }
}

unsafe extern "C" fn gen_vertex_arrays(count: i32, arrays: *mut u32) {
    if let Some(entry) = vao_entry(c"glGenVertexArrays", c"glGenVertexArraysAPPLE") {
        // SAFETY: both spellings have the prototype void (GLsizei, GLuint *).
        let f: unsafe extern "C" fn(i32, *mut u32) = unsafe { std::mem::transmute(entry) };
        unsafe { f(count, arrays) };
    }
}

unsafe extern "C" fn delete_vertex_arrays(count: i32, arrays: *const u32) {
    if let Some(entry) = vao_entry(c"glDeleteVertexArrays", c"glDeleteVertexArraysAPPLE") {
        // SAFETY: both spellings have the prototype void (GLsizei, const GLuint *).
        let f: unsafe extern "C" fn(i32, *const u32) = unsafe { std::mem::transmute(entry) };
        unsafe { f(count, arrays) };
    }
}

unsafe extern "C" fn ortho_2d(left: f64, right: f64, bottom: f64, top: f64) {
    if let Some(entry) = sdl3::video::gl_proc_address(c"glOrtho") {
        // SAFETY: glOrtho takes six GLdouble parameters, per OpenGL/gl.h.
        let f: unsafe extern "C" fn(f64, f64, f64, f64, f64, f64) =
            unsafe { std::mem::transmute(entry) };
        unsafe { f(left, right, bottom, top, -1.0, 1.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use machish::fixups::{LibraryRef, SymbolRef};
    use machish::resolve::SymbolProvider;

    #[test]
    fn backend_selection_child() {
        let Ok(mode) = std::env::var("ROSEMARY_GL_TEST_CHILD") else {
            return;
        };
        let builtin = mode == "builtin";
        assert_eq!(interposed(), builtin);
        let lib = provider();
        let mut host = NativeDylibProvider::system();
        deny_host_gl(&mut host);
        assert_eq!(host.is_denied("_CGLChoosePixelFormat"), builtin);
        assert_eq!(host.is_denied("_glClear"), builtin);
        crate::frameworks::provider();
        for name in [
            "CGLGetCurrentContext",
            "CGLFlushDrawable",
            "CGLGetShareGroup",
        ] {
            let mangled = format!("_{name}");
            let request = SymbolRef {
                name: &mangled,
                library: LibraryRef::InstallName(INSTALL_PATHS[0]),
                weak_import: false,
            };
            if builtin {
                let expected = appkit::symbol(name).unwrap();
                assert_eq!(symbol(name), Some(expected));
                assert_eq!(lib.resolve(&request).unwrap().addr, expected);
                assert_eq!(darwin::symbol(name), Some(expected));
                assert_eq!(darwin::shim_symbol(name), Some(expected));
            } else {
                assert!(symbol(name).is_none());
                assert!(lib.resolve(&request).is_none());
            }
        }
        if builtin {
            for name in ["CGLDescribePixelFormat", "gluPerspective"] {
                assert!(symbol(name).is_none());
                assert!(darwin::symbol(name).is_none());
                assert!(darwin::shim_symbol(name).is_none());
            }
        } else {
            assert_eq!(lib.opened(), None);
        }
    }

    #[test]
    fn backend_selection_controls_imports_and_fallbacks() {
        let modes: &[&str] = if cfg!(target_os = "macos") {
            &["native", "builtin"]
        } else {
            &["builtin"]
        };
        for mode in modes {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "opengl::tests::backend_selection_child",
                    "--nocapture",
                ])
                .env("ROSEMARY_OBJC", mode)
                .env("ROSEMARY_GL_TEST_CHILD", mode)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
