//! What a Mac's OpenGL says about itself.
//!
//! macOS answers a core-profile request with GL 4.1 and a short extension list,
//! whatever was asked for. A host driver asked for 4.1 core hands back its
//! newest core context instead, and a Mac program was never tested against
//! what that advertises: Feral's GL layer switches to direct state access when
//! it sees `GL_ARB_direct_state_access`, and on that path unmaps buffers through
//! the wrong entry point. So a core context reports the Mac's extensions -
//! those the host also has - and, where the driver's version is newer, the
//! Mac's version, and the driver is unchanged underneath. That holds whatever
//! version the driver made: NVIDIA's 4.1 or 3.2 still lists every extension it
//! has. Compatibility contexts are passed through as they are.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::sync::{Mutex, OnceLock};

const VERSION: u32 = 0x1F02;
const EXTENSIONS: u32 = 0x1F03;
const SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;
const NUM_EXTENSIONS: u32 = 0x821D;
const MAJOR_VERSION: u32 = 0x821B;
const MINOR_VERSION: u32 = 0x821C;
const CONTEXT_PROFILE_MASK: u32 = 0x9126;
const CONTEXT_CORE_PROFILE_BIT: i32 = 0x1;

/// The newest core profile macOS offers.
const MAC_CORE: (u32, u32) = (4, 1);

/// The extensions macOS lists for a 4.1 core context, across its GPU vendors.
const MAC_CORE_EXTENSIONS: &[&str] = &[
    "GL_APPLE_client_storage",
    "GL_APPLE_container_object_shareable",
    "GL_APPLE_flush_render",
    "GL_APPLE_object_purgeable",
    "GL_APPLE_rgb_422",
    "GL_APPLE_row_bytes",
    "GL_APPLE_texture_range",
    "GL_ARB_ES2_compatibility",
    "GL_ARB_blend_func_extended",
    "GL_ARB_draw_buffers_blend",
    "GL_ARB_draw_indirect",
    "GL_ARB_explicit_attrib_location",
    "GL_ARB_gpu_shader5",
    "GL_ARB_gpu_shader_fp64",
    "GL_ARB_instanced_arrays",
    "GL_ARB_internalformat_query",
    "GL_ARB_occlusion_query2",
    "GL_ARB_sample_shading",
    "GL_ARB_sampler_objects",
    "GL_ARB_separate_shader_objects",
    "GL_ARB_shader_bit_encoding",
    "GL_ARB_shader_subroutine",
    "GL_ARB_shading_language_include",
    "GL_ARB_tessellation_shader",
    "GL_ARB_texture_buffer_object_rgb32",
    "GL_ARB_texture_cube_map_array",
    "GL_ARB_texture_gather",
    "GL_ARB_texture_query_lod",
    "GL_ARB_texture_rgb10_a2ui",
    "GL_ARB_texture_storage",
    "GL_ARB_texture_swizzle",
    "GL_ARB_timer_query",
    "GL_ARB_transform_feedback2",
    "GL_ARB_transform_feedback3",
    "GL_ARB_vertex_attrib_64bit",
    "GL_ARB_vertex_type_2_10_10_10_rev",
    "GL_ARB_viewport_array",
    "GL_ATI_texture_mirror_once",
    "GL_EXT_debug_label",
    "GL_EXT_debug_marker",
    "GL_EXT_depth_bounds_test",
    "GL_EXT_framebuffer_multisample_blit_scaled",
    "GL_EXT_texture_compression_s3tc",
    "GL_EXT_texture_filter_anisotropic",
    "GL_EXT_texture_mirror_clamp",
    "GL_EXT_texture_sRGB_decode",
    "GL_NV_texture_barrier",
];

type GetString = unsafe extern "C" fn(u32) -> *const c_char;
type GetStringi = unsafe extern "C" fn(u32, u32) -> *const c_char;
type GetIntegerv = unsafe extern "C" fn(u32, *mut i32);

/// The driver's own entry point for `name`, looked up once.
fn driver(name: &'static CStr, slot: &'static OnceLock<usize>) -> Option<usize> {
    let address =
        *slot.get_or_init(|| sdl3::video::gl_proc_address(name).map_or(0, |p| p as usize));
    (address != 0).then_some(address)
}

fn real_get_string() -> Option<GetString> {
    static SLOT: OnceLock<usize> = OnceLock::new();
    // SAFETY: `const GLubyte *glGetString(GLenum)`.
    driver(c"glGetString", &SLOT).map(|a| unsafe { std::mem::transmute::<usize, GetString>(a) })
}

fn real_get_stringi() -> Option<GetStringi> {
    static SLOT: OnceLock<usize> = OnceLock::new();
    // SAFETY: `const GLubyte *glGetStringi(GLenum, GLuint)`.
    driver(c"glGetStringi", &SLOT).map(|a| unsafe { std::mem::transmute::<usize, GetStringi>(a) })
}

fn real_get_integerv() -> Option<GetIntegerv> {
    static SLOT: OnceLock<usize> = OnceLock::new();
    // SAFETY: `void glGetIntegerv(GLenum, GLint *)`.
    driver(c"glGetIntegerv", &SLOT).map(|a| unsafe { std::mem::transmute::<usize, GetIntegerv>(a) })
}

/// `major.minor` from the front of a `GL_VERSION` string.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let number = text.split_whitespace().next()?;
    let mut parts = number.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// The driver's version, if the current context is a core profile. Asked in an
/// order that raises no GL error: the profile mask is only queried where the
/// version says it exists.
fn core_version() -> Option<(u32, u32)> {
    let (Some(get_string), Some(get_integerv)) = (real_get_string(), real_get_integerv()) else {
        return None;
    };
    // SAFETY: a GL enumerator, answered with a static string or null.
    let text = unsafe { get_string(VERSION) };
    if text.is_null() {
        return None;
    }
    // SAFETY: the driver's NUL-terminated string.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let version = parse_version(&text)?;
    if version < (3, 2) {
        return None;
    }
    let mut mask = 0;
    // SAFETY: a writable int; the mask exists from 3.2 on.
    unsafe { get_integerv(CONTEXT_PROFILE_MASK, &mut mask) };
    (mask & CONTEXT_CORE_PROFILE_BIT != 0).then_some(version)
}

/// Whether the current context lists the Mac's extensions.
fn filtered() -> bool {
    core_version().is_some()
}

/// Whether the current context also reports the Mac's version.
fn capped() -> bool {
    core_version().is_some_and(|version| version > MAC_CORE)
}

/// `text` with its leading version number replaced by the Mac's.
fn mac_version_string(text: &str) -> String {
    match text.split_once(' ') {
        Some((_, rest)) => format!("{}.{} {rest}", MAC_CORE.0, MAC_CORE.1),
        None => format!("{}.{}", MAC_CORE.0, MAC_CORE.1),
    }
}

/// The driver's extension indices that a Mac core context would also list.
fn mac_extension_indices() -> Vec<u32> {
    let (Some(get_stringi), Some(get_integerv)) = (real_get_stringi(), real_get_integerv()) else {
        return Vec::new();
    };
    let mut count = 0;
    // SAFETY: a writable int.
    unsafe { get_integerv(NUM_EXTENSIONS, &mut count) };
    (0..count.max(0) as u32)
        .filter(|&i| {
            // SAFETY: an index below the driver's own count.
            let name = unsafe { get_stringi(EXTENSIONS, i) };
            // SAFETY: the driver's NUL-terminated string.
            !name.is_null()
                && MAC_CORE_EXTENSIONS
                    .contains(&&*unsafe { CStr::from_ptr(name) }.to_string_lossy())
        })
        .collect()
}

/// Strings handed to the guest, kept for the life of the process as a
/// driver's are.
fn interned(text: String) -> *const c_char {
    static STRINGS: Mutex<Option<HashMap<String, &'static CStr>>> = Mutex::new(None);
    let mut strings = STRINGS.lock().unwrap_or_else(|e| e.into_inner());
    let strings = strings.get_or_insert_with(HashMap::new);
    if let Some(kept) = strings.get(&text) {
        return kept.as_ptr();
    }
    let kept: &'static CStr = Box::leak(
        CString::new(text.clone())
            .unwrap_or_default()
            .into_boxed_c_str(),
    );
    strings.insert(text, kept);
    kept.as_ptr()
}

unsafe extern "C" fn get_string(name: u32) -> *const c_char {
    let Some(real) = real_get_string() else {
        return std::ptr::null();
    };
    // A core context has no extension string; NVIDIA's answers with every
    // extension anyway. The driver raises the `GL_INVALID_ENUM` a Mac's would.
    if name == EXTENSIONS && filtered() {
        // SAFETY: an enumerator no driver accepts.
        return unsafe { real(0) };
    }
    // SAFETY: the guest's call, passed on.
    let answer = unsafe { real(name) };
    if answer.is_null() || !matches!(name, VERSION | SHADING_LANGUAGE_VERSION) || !capped() {
        return answer;
    }
    match name {
        // SAFETY: the driver's NUL-terminated string.
        VERSION => interned(mac_version_string(
            &unsafe { CStr::from_ptr(answer) }.to_string_lossy(),
        )),
        _ => c"4.10".as_ptr(),
    }
}

unsafe extern "C" fn get_stringi(name: u32, index: u32) -> *const c_char {
    let Some(real) = real_get_stringi() else {
        return std::ptr::null();
    };
    if name != EXTENSIONS || !filtered() {
        // SAFETY: the guest's call, passed on.
        return unsafe { real(name, index) };
    }
    match mac_extension_indices().get(index as usize) {
        // SAFETY: an index the driver itself listed.
        Some(&driver_index) => unsafe { real(EXTENSIONS, driver_index) },
        // Out of range: the driver raises the `GL_INVALID_VALUE` Apple's would.
        // SAFETY: as above, with an index no driver lists.
        None => unsafe { real(EXTENSIONS, u32::MAX) },
    }
}

unsafe extern "C" fn get_integerv(name: u32, out: *mut i32) {
    let Some(real) = real_get_integerv() else {
        return;
    };
    // SAFETY: the guest's call, passed on.
    unsafe { real(name, out) };
    let value = match name {
        _ if out.is_null() => return,
        NUM_EXTENSIONS if filtered() => mac_extension_indices().len() as i32,
        MAJOR_VERSION if capped() => MAC_CORE.0 as i32,
        MINOR_VERSION if capped() => MAC_CORE.1 as i32,
        _ => return,
    };
    // SAFETY: the guest's writable int, just written by the driver.
    unsafe { out.write(value) };
}

type Flush = unsafe extern "C" fn();

fn real_flush() -> Option<Flush> {
    static SLOT: OnceLock<usize> = OnceLock::new();
    // SAFETY: `void glFlush(void)`.
    driver(c"glFlush", &SLOT).map(|a| unsafe { std::mem::transmute::<usize, Flush>(a) })
}

fn real_finish() -> Option<Flush> {
    static SLOT: OnceLock<usize> = OnceLock::new();
    // SAFETY: `void glFinish(void)`.
    driver(c"glFinish", &SLOT).map(|a| unsafe { std::mem::transmute::<usize, Flush>(a) })
}

/// A single-buffered Mac context presents on a flush; see
/// `appkit::opengl::present_single_buffered`.
unsafe extern "C" fn flush() {
    if let Some(real) = real_flush() {
        // SAFETY: the guest's call, passed on.
        unsafe { real() };
    }
    appkit::opengl::present_single_buffered();
}

unsafe extern "C" fn finish() {
    if let Some(real) = real_finish() {
        // SAFETY: the guest's call, passed on.
        unsafe { real() };
    }
    appkit::opengl::present_single_buffered();
}

/// This module's replacement for `name`, if it has one.
pub fn address(name: &str) -> Option<u64> {
    let f = match name {
        "glFlush" if crate::frameworks::using_builtin() => flush as *const (),
        "glFinish" if crate::frameworks::using_builtin() => finish as *const (),
        "glGetString" => get_string as *const (),
        "glGetStringi" => get_stringi as *const (),
        "glGetIntegerv" => get_integerv as *const (),
        _ => return None,
    };
    Some(f as usize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_version_string_keeps_its_vendor_text() {
        assert_eq!(
            parse_version("4.6 (Core Profile) Mesa 26.1.8"),
            Some((4, 6))
        );
        assert_eq!(parse_version("4.6.0 NVIDIA 610.57"), Some((4, 6)));
        assert_eq!(
            mac_version_string("4.6 (Core Profile) Mesa 26.1.8"),
            "4.1 (Core Profile) Mesa 26.1.8"
        );
        assert_eq!(
            mac_version_string("4.6.0 NVIDIA 610.57"),
            "4.1 NVIDIA 610.57"
        );
    }

    #[test]
    fn the_mac_list_has_no_direct_state_access() {
        assert!(
            !MAC_CORE_EXTENSIONS
                .iter()
                .any(|e| e.contains("direct_state_access"))
        );
    }
}
