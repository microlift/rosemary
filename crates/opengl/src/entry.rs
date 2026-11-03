//! Entry points whose arguments or effects need changing on the way to the
//! driver.

use std::ffi::{CStr, c_void};

use crate::driver::Entry;

/// This crate's own `libGL` function `name`, where it has one.
pub fn address(name: &str) -> Option<u64> {
    let f = match name {
        "glDrawElements" => draw_elements as *const (),
        "glBindVertexArrayAPPLE" => bind_vertex_array as *const (),
        "glGenVertexArraysAPPLE" => gen_vertex_arrays as *const (),
        "glDeleteVertexArraysAPPLE" => delete_vertex_arrays as *const (),
        "glFlush" => flush as *const (),
        "glFinish" => finish as *const (),
        "glShaderSource" => crate::glsl::shader_source as *const (),
        _ => return crate::profile::address(name),
    };
    Some(f as usize as u64)
}

/// This crate's own `libGLU` function `name`, where it has one.
pub fn glu(name: &str) -> Option<u64> {
    match name {
        "gluOrtho2D" => Some(ortho_2d as *const () as usize as u64),
        _ => None,
    }
}

const ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
const VERTEX_ARRAY_BINDING: u32 = 0x85B5;

/// Offsets below this are never a client array's address.
const LOWEST_CLIENT_POINTER: usize = 0x1_0000;

// SAFETY: each prototype is the one `<OpenGL/gl.h>` gives the name.
static DRAW_ELEMENTS: Entry<unsafe extern "C" fn(u32, i32, u32, *const c_void)> =
    unsafe { Entry::new(c"glDrawElements") };
static GET_INTEGERV: Entry<unsafe extern "C" fn(u32, *mut i32)> =
    unsafe { Entry::new(c"glGetIntegerv") };
static FLUSH: Entry<unsafe extern "C" fn()> = unsafe { Entry::new(c"glFlush") };
static FINISH: Entry<unsafe extern "C" fn()> = unsafe { Entry::new(c"glFinish") };
static ORTHO: Entry<unsafe extern "C" fn(f64, f64, f64, f64, f64, f64)> =
    unsafe { Entry::new(c"glOrtho") };

/// Keep a malformed indexed draw from taking the host driver down with it.
///
/// Apple's GL treats `indices` as an offset whenever an element array buffer
/// is bound; Mesa dereferences a small offset as a client pointer when none is.
/// The diagnostic names the lost state instead of leaving a driver crash with
/// no GL error.
unsafe extern "C" fn draw_elements(mode: u32, count: i32, kind: u32, indices: *const c_void) {
    let (Some(draw), Some(get)) = (DRAW_ELEMENTS.get(), GET_INTEGERV.get()) else {
        return;
    };
    let (mut element, mut vao) = (0, 0);
    // SAFETY: writable ints, one value each.
    unsafe {
        get(ELEMENT_ARRAY_BUFFER_BINDING, &mut element);
        get(VERTEX_ARRAY_BINDING, &mut vao);
    }
    if element == 0 && (indices as usize) < LOWEST_CLIENT_POINTER {
        eprintln!(
            "rosemary: OpenGL: glDrawElements has VAO {vao}, no element-array buffer, and \
             offset {:#x}; draw skipped",
            indices as usize
        );
        return;
    }
    // SAFETY: the guest's call, passed on.
    unsafe { draw(mode, count, kind, indices) };
}

/// The driver's spelling of a vertex array function: Apple's legacy contexts
/// have the APPLE extension, other drivers the core or ARB functions.
fn vao_entry(core: &CStr, apple: &CStr) -> Option<*mut c_void> {
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
        // SAFETY: both spellings are `void (GLuint)`.
        let f: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(entry) };
        unsafe { f(array) };
    }
}

unsafe extern "C" fn gen_vertex_arrays(count: i32, arrays: *mut u32) {
    if let Some(entry) = vao_entry(c"glGenVertexArrays", c"glGenVertexArraysAPPLE") {
        // SAFETY: both spellings are `void (GLsizei, GLuint *)`.
        let f: unsafe extern "C" fn(i32, *mut u32) = unsafe { std::mem::transmute(entry) };
        unsafe { f(count, arrays) };
    }
}

unsafe extern "C" fn delete_vertex_arrays(count: i32, arrays: *const u32) {
    if let Some(entry) = vao_entry(c"glDeleteVertexArrays", c"glDeleteVertexArraysAPPLE") {
        // SAFETY: both spellings are `void (GLsizei, const GLuint *)`.
        let f: unsafe extern "C" fn(i32, *const u32) = unsafe { std::mem::transmute(entry) };
        unsafe { f(count, arrays) };
    }
}

/// A single-buffered Mac context presents on a flush; see
/// `appkit::opengl::present_single_buffered`.
unsafe extern "C" fn flush() {
    if let Some(real) = FLUSH.get() {
        // SAFETY: the guest's call, passed on.
        unsafe { real() };
    }
    appkit::opengl::present_single_buffered();
}

unsafe extern "C" fn finish() {
    if let Some(real) = FINISH.get() {
        // SAFETY: the guest's call, passed on.
        unsafe { real() };
    }
    appkit::opengl::present_single_buffered();
}

/// `gluOrtho2D`, which GLU defines as `glOrtho` with a near and far plane of
/// -1 and 1.
unsafe extern "C" fn ortho_2d(left: f64, right: f64, bottom: f64, top: f64) {
    if let Some(ortho) = ORTHO.get() {
        // SAFETY: the guest's call, as GLU forwards it.
        unsafe { ortho(left, right, bottom, top, -1.0, 1.0) };
    }
}
