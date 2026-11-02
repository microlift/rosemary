//! What a Mac's OpenGL says about itself.
//!
//! macOS has two profiles: 4.1 core, whatever core version was asked for, and
//! 2.1 legacy, each with a short extension list. A host driver hands back its
//! newest context of either kind instead, and a Mac program was never tested
//! against what that advertises: Feral's GL layer switches to direct state
//! access when it sees `GL_ARB_direct_state_access`, and on that path unmaps
//! buffers through the wrong entry point. So a context reports the Mac's
//! version where the driver's is newer, and those of the Mac's extensions the
//! driver also has; the driver is unchanged underneath. That holds whatever
//! version the driver made: NVIDIA's 4.1 core still lists every extension it
//! has.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::sync::Mutex;

use crate::driver::Entry;

const VERSION: u32 = 0x1F02;
const EXTENSIONS: u32 = 0x1F03;
const SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;
const NUM_EXTENSIONS: u32 = 0x821D;
const MAJOR_VERSION: u32 = 0x821B;
const MINOR_VERSION: u32 = 0x821C;
const CONTEXT_PROFILE_MASK: u32 = 0x9126;
const CONTEXT_CORE_PROFILE_BIT: i32 = 0x1;

/// An enumerator no driver accepts, passed on so that the driver raises the
/// `GL_INVALID_ENUM` a Mac's would.
const INVALID_ENUM: u32 = 0;

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

/// The extensions macOS lists for a 2.1 legacy context, across its GPU vendors.
const MAC_LEGACY_EXTENSIONS: &[&str] = &[
    "GL_APPLE_aux_depth_stencil",
    "GL_APPLE_client_storage",
    "GL_APPLE_element_array",
    "GL_APPLE_fence",
    "GL_APPLE_float_pixels",
    "GL_APPLE_flush_buffer_range",
    "GL_APPLE_flush_render",
    "GL_APPLE_object_purgeable",
    "GL_APPLE_packed_pixels",
    "GL_APPLE_pixel_buffer",
    "GL_APPLE_rgb_422",
    "GL_APPLE_row_bytes",
    "GL_APPLE_specular_vector",
    "GL_APPLE_texture_range",
    "GL_APPLE_transform_hint",
    "GL_APPLE_vertex_array_object",
    "GL_APPLE_vertex_array_range",
    "GL_APPLE_vertex_point_size",
    "GL_APPLE_vertex_program_evaluators",
    "GL_APPLE_ycbcr_422",
    "GL_ARB_color_buffer_float",
    "GL_ARB_depth_buffer_float",
    "GL_ARB_depth_clamp",
    "GL_ARB_depth_texture",
    "GL_ARB_draw_buffers",
    "GL_ARB_draw_elements_base_vertex",
    "GL_ARB_draw_instanced",
    "GL_ARB_fragment_program",
    "GL_ARB_fragment_program_shadow",
    "GL_ARB_fragment_shader",
    "GL_ARB_framebuffer_object",
    "GL_ARB_framebuffer_sRGB",
    "GL_ARB_half_float_pixel",
    "GL_ARB_half_float_vertex",
    "GL_ARB_imaging",
    "GL_ARB_instanced_arrays",
    "GL_ARB_multisample",
    "GL_ARB_multitexture",
    "GL_ARB_occlusion_query",
    "GL_ARB_pixel_buffer_object",
    "GL_ARB_point_parameters",
    "GL_ARB_point_sprite",
    "GL_ARB_provoking_vertex",
    "GL_ARB_seamless_cube_map",
    "GL_ARB_shader_objects",
    "GL_ARB_shader_texture_lod",
    "GL_ARB_shading_language_100",
    "GL_ARB_shadow",
    "GL_ARB_sync",
    "GL_ARB_texture_border_clamp",
    "GL_ARB_texture_compression",
    "GL_ARB_texture_compression_rgtc",
    "GL_ARB_texture_cube_map",
    "GL_ARB_texture_env_add",
    "GL_ARB_texture_env_combine",
    "GL_ARB_texture_env_crossbar",
    "GL_ARB_texture_env_dot3",
    "GL_ARB_texture_float",
    "GL_ARB_texture_mirrored_repeat",
    "GL_ARB_texture_non_power_of_two",
    "GL_ARB_texture_rectangle",
    "GL_ARB_texture_rg",
    "GL_ARB_transpose_matrix",
    "GL_ARB_vertex_array_bgra",
    "GL_ARB_vertex_blend",
    "GL_ARB_vertex_buffer_object",
    "GL_ARB_vertex_program",
    "GL_ARB_vertex_shader",
    "GL_ARB_window_pos",
    "GL_ATI_separate_stencil",
    "GL_ATI_texture_env_combine3",
    "GL_ATI_texture_float",
    "GL_ATI_texture_mirror_once",
    "GL_EXT_abgr",
    "GL_EXT_bgra",
    "GL_EXT_bindable_uniform",
    "GL_EXT_blend_color",
    "GL_EXT_blend_equation_separate",
    "GL_EXT_blend_func_separate",
    "GL_EXT_blend_minmax",
    "GL_EXT_blend_subtract",
    "GL_EXT_clip_volume_hint",
    "GL_EXT_debug_label",
    "GL_EXT_debug_marker",
    "GL_EXT_depth_bounds_test",
    "GL_EXT_draw_buffers2",
    "GL_EXT_draw_range_elements",
    "GL_EXT_fog_coord",
    "GL_EXT_framebuffer_blit",
    "GL_EXT_framebuffer_multisample",
    "GL_EXT_framebuffer_multisample_blit_scaled",
    "GL_EXT_framebuffer_object",
    "GL_EXT_framebuffer_sRGB",
    "GL_EXT_geometry_shader4",
    "GL_EXT_gpu_program_parameters",
    "GL_EXT_gpu_shader4",
    "GL_EXT_multi_draw_arrays",
    "GL_EXT_packed_depth_stencil",
    "GL_EXT_packed_float",
    "GL_EXT_provoking_vertex",
    "GL_EXT_rescale_normal",
    "GL_EXT_secondary_color",
    "GL_EXT_separate_specular_color",
    "GL_EXT_shadow_funcs",
    "GL_EXT_stencil_two_side",
    "GL_EXT_stencil_wrap",
    "GL_EXT_texture_array",
    "GL_EXT_texture_compression_dxt1",
    "GL_EXT_texture_compression_s3tc",
    "GL_EXT_texture_env_add",
    "GL_EXT_texture_filter_anisotropic",
    "GL_EXT_texture_integer",
    "GL_EXT_texture_lod_bias",
    "GL_EXT_texture_mirror_clamp",
    "GL_EXT_texture_rectangle",
    "GL_EXT_texture_sRGB",
    "GL_EXT_texture_sRGB_decode",
    "GL_EXT_texture_shared_exponent",
    "GL_EXT_timer_query",
    "GL_EXT_transform_feedback",
    "GL_EXT_vertex_array_bgra",
    "GL_IBM_rasterpos_clip",
    "GL_NV_blend_square",
    "GL_NV_conditional_render",
    "GL_NV_depth_clamp",
    "GL_NV_fog_distance",
    "GL_NV_light_max_exponent",
    "GL_NV_texgen_reflection",
    "GL_NV_texture_barrier",
    "GL_SGIS_generate_mipmap",
    "GL_SGIS_texture_edge_clamp",
    "GL_SGIS_texture_lod",
];

/// One of the two profiles a Mac offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    Core,
    Legacy,
}

impl Profile {
    fn version(self) -> (u32, u32) {
        match self {
            Profile::Core => (4, 1),
            Profile::Legacy => (2, 1),
        }
    }

    fn shading_language(self) -> &'static CStr {
        match self {
            Profile::Core => c"4.10",
            Profile::Legacy => c"1.20",
        }
    }

    fn extensions(self) -> &'static [&'static str] {
        match self {
            Profile::Core => MAC_CORE_EXTENSIONS,
            Profile::Legacy => MAC_LEGACY_EXTENSIONS,
        }
    }
}

/// The current context, as a Mac would describe it.
#[derive(Clone, Copy, Debug)]
struct Context {
    profile: Profile,
    /// The driver's own version.
    version: (u32, u32),
}

impl Context {
    /// Whether the driver's version is newer than the Mac's, and is replaced.
    fn capped(self) -> bool {
        self.version > self.profile.version()
    }
}

// SAFETY: each prototype is the one `<OpenGL/gl3.h>` gives the name.
static GET_STRING: Entry<unsafe extern "C" fn(u32) -> *const c_char> =
    unsafe { Entry::new(c"glGetString") };
static GET_STRINGI: Entry<unsafe extern "C" fn(u32, u32) -> *const c_char> =
    unsafe { Entry::new(c"glGetStringi") };
static GET_INTEGERV: Entry<unsafe extern "C" fn(u32, *mut i32)> =
    unsafe { Entry::new(c"glGetIntegerv") };

/// `major.minor` from the front of a `GL_VERSION` string.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let number = text.split_whitespace().next()?;
    let mut parts = number.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// The current context, or `None` where there is none. Asked in an order that
/// raises no GL error: the profile mask is only queried where the version says
/// it exists.
fn current() -> Option<Context> {
    let (get_string, get_integerv) = (GET_STRING.get()?, GET_INTEGERV.get()?);
    // SAFETY: a GL enumerator, answered with a static string or null.
    let text = unsafe { get_string(VERSION) };
    if text.is_null() {
        return None;
    }
    // SAFETY: the driver's NUL-terminated string.
    let version = parse_version(&unsafe { CStr::from_ptr(text) }.to_string_lossy())?;
    let mut mask = 0;
    if version >= (3, 2) {
        // SAFETY: a writable int.
        unsafe { get_integerv(CONTEXT_PROFILE_MASK, &mut mask) };
    }
    let profile = match mask & CONTEXT_CORE_PROFILE_BIT != 0 {
        true => Profile::Core,
        false => Profile::Legacy,
    };
    Some(Context { profile, version })
}

/// `text` with its leading version number replaced by `version`.
fn replace_version(text: &str, (major, minor): (u32, u32)) -> String {
    match text.split_once(' ') {
        Some((_, rest)) => format!("{major}.{minor} {rest}"),
        None => format!("{major}.{minor}"),
    }
}

/// The driver's extension indices that a Mac core context would also list.
fn core_extension_indices() -> Vec<u32> {
    let (Some(get_stringi), Some(get_integerv)) = (GET_STRINGI.get(), GET_INTEGERV.get()) else {
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

/// The driver's extension string, less what a Mac legacy context lacks.
fn legacy_extension_string(driver: &str) -> String {
    driver
        .split_whitespace()
        .filter(|name| Profile::Legacy.extensions().contains(name))
        .collect::<Vec<_>>()
        .join(" ")
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
    let Some(real) = GET_STRING.get() else {
        return std::ptr::null();
    };
    let context = match name {
        EXTENSIONS | VERSION | SHADING_LANGUAGE_VERSION => current(),
        _ => None,
    };
    let Some(context) = context else {
        // SAFETY: the guest's call, passed on.
        return unsafe { real(name) };
    };
    match (name, context.profile) {
        // A core context has no extension string; NVIDIA's answers with every
        // extension anyway.
        // SAFETY: an enumerator, refused.
        (EXTENSIONS, Profile::Core) => unsafe { real(INVALID_ENUM) },
        (EXTENSIONS, Profile::Legacy) => {
            // SAFETY: the guest's call, passed on.
            let answer = unsafe { real(EXTENSIONS) };
            match answer.is_null() {
                true => answer,
                // SAFETY: the driver's NUL-terminated string.
                false => interned(legacy_extension_string(
                    &unsafe { CStr::from_ptr(answer) }.to_string_lossy(),
                )),
            }
        }
        (VERSION, _) if context.capped() => {
            // SAFETY: the guest's call, passed on.
            let answer = unsafe { real(VERSION) };
            match answer.is_null() {
                true => answer,
                // SAFETY: the driver's NUL-terminated string.
                false => interned(replace_version(
                    &unsafe { CStr::from_ptr(answer) }.to_string_lossy(),
                    context.profile.version(),
                )),
            }
        }
        (SHADING_LANGUAGE_VERSION, profile) if context.capped() => {
            profile.shading_language().as_ptr()
        }
        // SAFETY: the guest's call, passed on.
        _ => unsafe { real(name) },
    }
}

unsafe extern "C" fn get_stringi(name: u32, index: u32) -> *const c_char {
    let Some(real) = GET_STRINGI.get() else {
        return std::ptr::null();
    };
    let context = match name {
        EXTENSIONS => current(),
        _ => None,
    };
    match context.map(|context| context.profile) {
        Some(Profile::Core) => match core_extension_indices().get(index as usize) {
            // SAFETY: an index the driver itself listed.
            Some(&driver_index) => unsafe { real(EXTENSIONS, driver_index) },
            // Out of range: the driver raises the `GL_INVALID_VALUE` Apple's
            // would.
            // SAFETY: as above, with an index no driver lists.
            None => unsafe { real(EXTENSIONS, u32::MAX) },
        },
        // GL 2.1 has no indexed extension list.
        // SAFETY: an enumerator, refused.
        Some(Profile::Legacy) => unsafe { real(INVALID_ENUM, index) },
        // SAFETY: the guest's call, passed on.
        None => unsafe { real(name, index) },
    }
}

unsafe extern "C" fn get_integerv(name: u32, out: *mut i32) {
    let Some(real) = GET_INTEGERV.get() else {
        return;
    };
    let context = match name {
        NUM_EXTENSIONS | MAJOR_VERSION | MINOR_VERSION if !out.is_null() => current(),
        _ => None,
    };
    let Some(context) = context else {
        // SAFETY: the guest's call, passed on.
        return unsafe { real(name, out) };
    };
    if context.profile == Profile::Legacy {
        // GL 2.1 has none of the three.
        // SAFETY: an enumerator, refused; nothing is written.
        return unsafe { real(INVALID_ENUM, out) };
    }
    // SAFETY: the guest's call, passed on.
    unsafe { real(name, out) };
    let (major, minor) = Profile::Core.version();
    let value = match name {
        NUM_EXTENSIONS => core_extension_indices().len() as i32,
        MAJOR_VERSION if context.capped() => major as i32,
        MINOR_VERSION if context.capped() => minor as i32,
        _ => return,
    };
    // SAFETY: the guest's writable int, just written by the driver.
    unsafe { out.write(value) };
}

/// This module's replacement for `name`, unless the driver's own features are
/// on offer.
pub fn address(name: &str) -> Option<u64> {
    if crate::host_features() {
        return None;
    }
    let f = match name {
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
            replace_version("4.6 (Core Profile) Mesa 26.1.8", (4, 1)),
            "4.1 (Core Profile) Mesa 26.1.8"
        );
        assert_eq!(
            replace_version("4.6.0 NVIDIA 610.57", (2, 1)),
            "2.1 NVIDIA 610.57"
        );
    }

    #[test]
    fn the_mac_lists_have_no_direct_state_access() {
        for profile in [Profile::Core, Profile::Legacy] {
            assert!(
                !profile
                    .extensions()
                    .iter()
                    .any(|e| e.contains("direct_state_access"))
            );
        }
    }

    #[test]
    fn a_legacy_extension_string_keeps_only_the_macs() {
        assert_eq!(
            legacy_extension_string("GL_ARB_multitexture GL_ARB_direct_state_access GL_EXT_bgra"),
            "GL_ARB_multitexture GL_EXT_bgra"
        );
    }

    #[test]
    fn only_a_newer_driver_is_capped() {
        let context = |profile, version| Context { profile, version };
        assert!(context(Profile::Core, (4, 6)).capped());
        assert!(!context(Profile::Core, (4, 1)).capped());
        assert!(context(Profile::Legacy, (4, 6)).capped());
        assert!(!context(Profile::Legacy, (2, 1)).capped());
    }
}
