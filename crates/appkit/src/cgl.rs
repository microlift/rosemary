#![allow(non_snake_case)]

use std::ffi::c_void;

/// `CGLError`. Only the two this module can produce are named.
type CGLError = i32;
const K_CGL_NO_ERROR: CGLError = 0;
/// `kCGLBadProperty`: a renderer property this tree cannot answer.
const K_CGL_BAD_PROPERTY: CGLError = 10001;
/// `kCGLBadRendererInfo`: the object is not one of ours, or the index is past
/// the end of the list.
const K_CGL_BAD_RENDERER_INFO: CGLError = 10003;
/// `kCGLBadValue`: a null out-parameter.
const K_CGL_BAD_VALUE: CGLError = 10008;

/// `CGLRendererProperty`, in the numbering `<OpenGL/CGLRenderers.h>` gives.
mod property {
    pub const OFF_SCREEN: u32 = 53;
    pub const FULL_SCREEN: u32 = 54;
    pub const RENDERER_ID: u32 = 70;
    pub const ACCELERATED: u32 = 73;
    pub const ROBUST: u32 = 75;
    pub const BACKING_STORE: u32 = 76;
    pub const WINDOW: u32 = 80;
    pub const MULTI_SCREEN: u32 = 81;
    pub const COMPLIANT: u32 = 83;
    pub const DISPLAY_MASK: u32 = 84;
    pub const BUFFER_MODES: u32 = 100;
    pub const COLOR_MODES: u32 = 103;
    pub const ACCUM_MODES: u32 = 104;
    pub const DEPTH_MODES: u32 = 105;
    pub const STENCIL_MODES: u32 = 106;
    pub const MAX_AUX_BUFFERS: u32 = 107;
    pub const MAX_SAMPLE_BUFFERS: u32 = 108;
    pub const MAX_SAMPLES: u32 = 109;
    pub const SAMPLE_MODES: u32 = 110;
    pub const SAMPLE_ALPHA: u32 = 111;
    pub const VIDEO_MEMORY: u32 = 120;
    pub const TEXTURE_MEMORY: u32 = 121;
    pub const GPU_VERT_PROC_CAPABLE: u32 = 122;
    pub const GPU_FRAG_PROC_CAPABLE: u32 = 123;
    pub const RENDERER_COUNT: u32 = 128;
    pub const ONLINE: u32 = 129;
    pub const ACCELERATED_COMPUTE: u32 = 130;
    pub const VIDEO_MEMORY_MEGABYTES: u32 = 131;
    pub const TEXTURE_MEMORY_MEGABYTES: u32 = 132;
}

/// `kCGLRendererGenericFloatID`, which is what a renderer that is not one of
/// Apple's own hardware IDs reports. Naming a real one - an AMD or Intel ID -
/// would be telling the program which GPU family this is, and it is not one of
/// the four Apple ever shipped.
const GENERIC_FLOAT_ID: i32 = 0x0002_0400;

/// `kCGLRPBufferModes`: single and double buffered, and stereo is not offered.
const MONOSCOPIC_BIT: i32 = 1;
const DOUBLE_BUFFER_BIT: i32 = 4;

/// `kCGLRPColorModes`: `kCGLRGB888Bit` and `kCGLARGB8888Bit`, the two a window
/// on this host is ever made with.
const RGB888_BIT: i32 = 0x0001_0000;
const ARGB8888_BIT: i32 = 0x0002_0000;

/// `kCGLRPDepthModes` / `kCGLRPStencilModes`: `kCGL0Bit` and the depths SDL asks
/// for. The bit for *n* planes is `1 << n` in the low word.
const DEPTH_0_BIT: i32 = 1;
const DEPTH_16_BIT: i32 = 1 << 5;
const DEPTH_24_BIT: i32 = 1 << 6;
const DEPTH_32_BIT: i32 = 1 << 7;
const STENCIL_0_BIT: i32 = 1;
const STENCIL_8_BIT: i32 = 1 << 3;

/// The object `CGLQueryRendererInfo` hands out.
///
/// Boxed and handed over as a pointer, which is what makes
/// [`CGLDestroyRendererInfo`] the thing that frees it.
struct RendererInfo {
    display_mask: i32,
}

/// Every object handed out and not yet destroyed.
///
/// A tag inside the object would not do: the pointer a guest passes may be one
/// it already destroyed, and reading a tag out of freed storage to find that
/// out is the bug it would be there to catch. Checked against this first, so a
/// pointer that is not live is never dereferenced.
static LIVE: std::sync::Mutex<Option<std::collections::HashSet<usize>>> =
    std::sync::Mutex::new(None);

/// The renderer list behind a pointer, or `None` for one this module did not
/// hand out or has already destroyed.
///
/// # Safety
///
/// `info` is null or a pointer [`CGLQueryRendererInfo`] returned.
unsafe fn info<'a>(info: *mut c_void) -> Option<&'a RendererInfo> {
    let held = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    if !held
        .as_ref()
        .is_some_and(|live| live.contains(&(info as usize)))
    {
        return None;
    }
    // SAFETY: the pointer is one this module boxed and has not freed.
    Some(unsafe { &*info.cast::<RendererInfo>() })
}

/// `CGLError CGLQueryRendererInfo(GLuint display_mask, CGLRendererInfoObj *rend,
/// GLint *nrend)`
///
/// # Safety
///
/// `rend` and `nrend` are null or writable.
pub unsafe extern "C" fn CGLQueryRendererInfo(
    display_mask: u32,
    rend: *mut *mut c_void,
    nrend: *mut i32,
) -> CGLError {
    if rend.is_null() || nrend.is_null() {
        return K_CGL_BAD_VALUE;
    }
    let boxed = Box::into_raw(Box::new(RendererInfo {
        // Echoed rather than invented: a caller that asked about one display
        // reads this back to confirm it got that display's renderer.
        display_mask: display_mask as i32,
    }));
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(std::collections::HashSet::new)
        .insert(boxed as usize);
    // SAFETY: the caller states both are writable.
    unsafe {
        *rend = boxed.cast();
        *nrend = 1;
    }
    K_CGL_NO_ERROR
}

/// `CGLError CGLDescribeRenderer(CGLRendererInfoObj rend, GLint rend_num,
/// CGLRendererProperty prop, GLint *value)`
///
/// A property this tree cannot answer is `kCGLBadProperty` rather than a
/// plausible number: a caller sizing its texture pool from an invented video
/// memory is worse off than one told the question went unanswered.
///
/// # Safety
///
/// `rend` is a pointer [`CGLQueryRendererInfo`] returned and `value` is
/// writable.
pub unsafe extern "C" fn CGLDescribeRenderer(
    rend: *mut c_void,
    rend_num: i32,
    prop: u32,
    value: *mut i32,
) -> CGLError {
    if value.is_null() {
        return K_CGL_BAD_VALUE;
    }
    let Some(info) = (unsafe { info(rend) }) else {
        return K_CGL_BAD_RENDERER_INFO;
    };
    if rend_num != 0 {
        return K_CGL_BAD_RENDERER_INFO;
    }
    let answer = match prop {
        // The renderer draws into a window on a screen, through a driver.
        property::ACCELERATED | property::ONLINE | property::COMPLIANT => 1,
        property::WINDOW | property::BACKING_STORE | property::ROBUST => 1,
        property::ACCELERATED_COMPUTE => 1,
        property::GPU_VERT_PROC_CAPABLE | property::GPU_FRAG_PROC_CAPABLE => 1,
        // Off-screen is the software renderer's mode, and full-screen capture
        // of a display is a Mac facility with nothing behind it here.
        property::OFF_SCREEN | property::FULL_SCREEN => 0,
        // One renderer, one screen: a context here cannot migrate between GPUs.
        property::MULTI_SCREEN => 0,
        property::RENDERER_COUNT => 1,
        property::RENDERER_ID => GENERIC_FLOAT_ID,
        property::DISPLAY_MASK => info.display_mask,
        property::BUFFER_MODES => MONOSCOPIC_BIT | DOUBLE_BUFFER_BIT,
        property::COLOR_MODES => RGB888_BIT | ARGB8888_BIT,
        // No accumulation buffer: core profiles have not had one for years and
        // SDL asks for none.
        property::ACCUM_MODES => 0,
        property::DEPTH_MODES => DEPTH_0_BIT | DEPTH_16_BIT | DEPTH_24_BIT | DEPTH_32_BIT,
        property::STENCIL_MODES => STENCIL_0_BIT | STENCIL_8_BIT,
        property::MAX_AUX_BUFFERS => 0,
        property::MAX_SAMPLE_BUFFERS => 1,
        property::MAX_SAMPLES => 8,
        // `kCGLSupersampleBit | kCGLMultisampleBit`.
        property::SAMPLE_MODES => 3,
        property::SAMPLE_ALPHA => 1,
        property::VIDEO_MEMORY | property::TEXTURE_MEMORY => match memory_bytes() {
            Some(bytes) => bytes,
            None => return K_CGL_BAD_PROPERTY,
        },
        property::VIDEO_MEMORY_MEGABYTES | property::TEXTURE_MEMORY_MEGABYTES => {
            match memory_bytes() {
                Some(bytes) => bytes >> 20,
                None => return K_CGL_BAD_PROPERTY,
            }
        }
        _ => return K_CGL_BAD_PROPERTY,
    };
    // SAFETY: the caller states `value` is writable.
    unsafe { *value = answer };
    K_CGL_NO_ERROR
}

/// The GPU's memory in bytes, as the driver reports it, or `None` where it
/// does not.
///
/// There is no portable GL query for this - Apple answers it from the kernel's
/// own device tree, which is not a thing this host has - so it comes from
/// whichever vendor extension the driver carries, and from nothing at all when
/// neither is there. Only with a context current: `glGetIntegerv` without one
/// is undefined, and asking is how a caller finds out there is no GPU yet
/// rather than a GPU with no memory.
///
/// Capped at `GLint`, which is the type the property is read back as: a card
/// with more than two gigabytes would otherwise be reported as a negative
/// number.
fn memory_bytes() -> Option<i32> {
    // The last driver answer, since the question is often asked before any
    // context exists - a launcher checking the hardware before the game makes
    // one - and a GPU's memory does not change between contexts.
    static LAST: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
    if let Some(bytes) = memory_from_driver() {
        LAST.store(bytes, std::sync::atomic::Ordering::Relaxed);
        return Some(bytes);
    }
    match LAST.load(std::sync::atomic::Ordering::Relaxed) {
        0 => memory_from_sysfs(),
        bytes => Some(bytes),
    }
}

/// With no context: the largest dedicated memory a DRM driver reports, which
/// amdgpu and xe publish as `mem_info_vram_total`, in bytes.
#[cfg(target_os = "linux")]
fn memory_from_sysfs() -> Option<i32> {
    let cards = std::fs::read_dir("/sys/class/drm").ok()?;
    let largest = cards
        .filter_map(Result::ok)
        .filter_map(|card| {
            std::fs::read_to_string(card.path().join("device/mem_info_vram_total")).ok()
        })
        .filter_map(|text| text.trim().parse::<u64>().ok())
        .max()?;
    Some(i32::try_from(largest).unwrap_or(i32::MAX))
}

#[cfg(not(target_os = "linux"))]
fn memory_from_sysfs() -> Option<i32> {
    None
}

/// The current context's driver, through a vendor extension.
fn memory_from_driver() -> Option<i32> {
    /// `GL_GPU_MEMORY_INFO_DEDICATED_VIDMEM_NVX`, in kilobytes.
    const NVX_DEDICATED_VIDMEM: u32 = 0x9047;
    /// `GL_TEXTURE_FREE_MEMORY_ATI`, whose first word is the free pool in
    /// kilobytes. Free rather than total, which is the only thing this
    /// extension reports.
    const ATI_TEXTURE_FREE_MEMORY: u32 = 0x87FC;

    sdl3::video::gl_current_version()?;
    let f = sdl3::video::gl_proc_address(c"glGetIntegerv")?;
    // SAFETY: `void glGetIntegerv(GLenum, GLint *)`.
    let get: unsafe extern "C" fn(u32, *mut i32) = unsafe { std::mem::transmute(f) };
    let read = |name| {
        let mut value = [-1_i32; 4];
        // SAFETY: four writable words, which is more than either name writes.
        unsafe { get(name, value.as_mut_ptr()) };
        value[0]
    };
    let kilobytes = match read(NVX_DEDICATED_VIDMEM) {
        kb if kb > 0 => kb,
        // The read failed and left the sentinel, or the driver has neither
        // extension.
        _ => match read(ATI_TEXTURE_FREE_MEMORY) {
            kb if kb > 0 => kb,
            _ => return None,
        },
    };
    Some(i32::try_from(i64::from(kilobytes) * 1024).unwrap_or(i32::MAX))
}

/// `CGLError CGLDestroyRendererInfo(CGLRendererInfoObj rend)`
///
/// # Safety
///
/// `rend` is a pointer [`CGLQueryRendererInfo`] returned and not yet destroyed.
pub unsafe extern "C" fn CGLDestroyRendererInfo(rend: *mut c_void) -> CGLError {
    if unsafe { info(rend) }.is_none() {
        return K_CGL_BAD_RENDERER_INFO;
    }
    LIVE.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .map(|live| live.remove(&(rend as usize)));
    // SAFETY: a box this module made, taken out of the live set above - so a
    // second destroy of the same pointer is refused rather than freeing twice.
    drop(unsafe { Box::from_raw(rend.cast::<RendererInfo>()) });
    K_CGL_NO_ERROR
}

exports::exports! {
    functions {
        "CGLQueryRendererInfo" => CGLQueryRendererInfo,
        "CGLDescribeRenderer" => CGLDescribeRenderer,
        "CGLDestroyRendererInfo" => CGLDestroyRendererInfo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One renderer, and it is accelerated and online - which is the pair a
    /// caller tests before it keeps a renderer at all.
    #[test]
    fn the_one_renderer_is_accelerated_and_online() {
        let mut rend = std::ptr::null_mut();
        let mut count = 0;
        // SAFETY: two writable out-parameters.
        assert_eq!(
            unsafe { CGLQueryRendererInfo(0xffff_ffff, &mut rend, &mut count) },
            K_CGL_NO_ERROR
        );
        assert_eq!(count, 1);
        for prop in [property::ACCELERATED, property::ONLINE] {
            let mut value = 0;
            // SAFETY: an object the call above returned, and a writable word.
            assert_eq!(
                unsafe { CGLDescribeRenderer(rend, 0, prop, &mut value) },
                K_CGL_NO_ERROR
            );
            assert_eq!(value, 1, "property {prop}");
        }
        let mut mask = 0;
        unsafe { CGLDescribeRenderer(rend, 0, property::DISPLAY_MASK, &mut mask) };
        assert_eq!(mask, -1, "the mask the caller asked about");
        // SAFETY: an object nothing has destroyed yet.
        assert_eq!(unsafe { CGLDestroyRendererInfo(rend) }, K_CGL_NO_ERROR);
        // SAFETY: the same pointer, now destroyed - refused rather than freed
        // twice.
        assert_eq!(
            unsafe { CGLDestroyRendererInfo(rend) },
            K_CGL_BAD_RENDERER_INFO
        );
    }

    /// A renderer past the end of the list, and an object nothing made, are
    /// both refused rather than described.
    #[test]
    fn an_index_past_the_end_is_refused() {
        let mut rend = std::ptr::null_mut();
        let mut count = 0;
        let mut value = 0;
        // SAFETY: two writable out-parameters.
        unsafe { CGLQueryRendererInfo(1, &mut rend, &mut count) };
        // SAFETY: a live object and a writable word.
        assert_eq!(
            unsafe { CGLDescribeRenderer(rend, 1, property::ACCELERATED, &mut value) },
            K_CGL_BAD_RENDERER_INFO
        );
        let mut other = 0_u64;
        // SAFETY: readable storage that is not one of this module's objects.
        assert_eq!(
            unsafe {
                CGLDescribeRenderer(
                    (&raw mut other).cast(),
                    0,
                    property::ACCELERATED,
                    &mut value,
                )
            },
            K_CGL_BAD_RENDERER_INFO
        );
        // SAFETY: the object made above.
        unsafe { CGLDestroyRendererInfo(rend) };
    }
}
