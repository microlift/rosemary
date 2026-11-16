//! `glGenerateMipmap` for Apple's GL, which builds the levels of a 32-bit float
//! texture on the CPU: it reads the base level back from the GPU, waiting for
//! every command queued ahead of it, filters it and uploads the result. A
//! texture rendered and then mipmapped every frame (Unity does this to an
//! `RG32F` target) stalls each frame on the GPU.
//!
//! Instead, each level is blitted from the one above it with linear filtering,
//! which stays on the GPU. For a level of even size that is the 2x2 box filter
//! the GL spec recommends; the spec leaves the filter to the implementation.
//! Anything this path cannot vouch for goes to the driver's own function.

use std::ffi::CStr;

const TEXTURE_2D: u32 = 0x0DE1;
const TEXTURE_BINDING_2D: u32 = 0x8069;
const TEXTURE_BASE_LEVEL: u32 = 0x813C;
const TEXTURE_MAX_LEVEL: u32 = 0x813D;
const TEXTURE_IMMUTABLE_FORMAT: u32 = 0x912F;
const TEXTURE_WIDTH: u32 = 0x1000;
const TEXTURE_HEIGHT: u32 = 0x1001;
const TEXTURE_INTERNAL_FORMAT: u32 = 0x1003;
const READ_FRAMEBUFFER: u32 = 0x8CA8;
const DRAW_FRAMEBUFFER: u32 = 0x8CA9;
const READ_FRAMEBUFFER_BINDING: u32 = 0x8CAA;
const DRAW_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const COLOR_ATTACHMENT0: u32 = 0x8CE0;
const FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const COLOR_BUFFER_BIT: u32 = 0x4000;
const LINEAR: u32 = 0x2601;
const SCISSOR_TEST: u32 = 0x0C11;
const RASTERIZER_DISCARD: u32 = 0x8C89;
const COLOR_WRITEMASK: u32 = 0x0C23;
const TRUE: u8 = 1;

const R32F: i32 = 0x822E;
const RG32F: i32 = 0x8230;
const RGBA32F: i32 = 0x8814;

/// The formats Apple's GL mipmaps on the CPU, measured on Apple silicon. Half
/// floats and normalized formats stay on the GPU there, and integer formats
/// cannot be filtered by a blit at all.
const CPU_FILTERED: [i32; 3] = [R32F, RG32F, RGBA32F];

/// The driver functions this path calls, all from one library.
pub struct Gl {
    generate_mipmap: unsafe extern "C" fn(u32),
    get_integerv: unsafe extern "C" fn(u32, *mut i32),
    get_booleanv: unsafe extern "C" fn(u32, *mut u8),
    get_tex_parameteriv: unsafe extern "C" fn(u32, u32, *mut i32),
    get_tex_level_parameteriv: unsafe extern "C" fn(u32, i32, u32, *mut i32),
    is_enabled: unsafe extern "C" fn(u32) -> u8,
    enable: unsafe extern "C" fn(u32),
    disable: unsafe extern "C" fn(u32),
    color_mask: unsafe extern "C" fn(u8, u8, u8, u8),
    gen_framebuffers: unsafe extern "C" fn(i32, *mut u32),
    delete_framebuffers: unsafe extern "C" fn(i32, *const u32),
    bind_framebuffer: unsafe extern "C" fn(u32, u32),
    framebuffer_texture_2d: unsafe extern "C" fn(u32, u32, u32, u32, i32),
    check_framebuffer_status: unsafe extern "C" fn(u32) -> u32,
    blit_framebuffer: unsafe extern "C" fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32),
}

impl Gl {
    /// Every function from `lookup`, or `None` if any is missing.
    pub fn load(lookup: impl Fn(&CStr) -> Option<usize>) -> Option<Gl> {
        macro_rules! get {
            ($name:literal) => {{
                let address = lookup($name)?;
                // SAFETY: `address` is the driver's function of that name, and
                // the field it is stored in has the prototype `<OpenGL/gl3.h>`
                // gives it.
                unsafe { std::mem::transmute::<usize, _>(address) }
            }};
        }
        Some(Gl {
            generate_mipmap: get!(c"glGenerateMipmap"),
            get_integerv: get!(c"glGetIntegerv"),
            get_booleanv: get!(c"glGetBooleanv"),
            get_tex_parameteriv: get!(c"glGetTexParameteriv"),
            get_tex_level_parameteriv: get!(c"glGetTexLevelParameteriv"),
            is_enabled: get!(c"glIsEnabled"),
            enable: get!(c"glEnable"),
            disable: get!(c"glDisable"),
            color_mask: get!(c"glColorMask"),
            gen_framebuffers: get!(c"glGenFramebuffers"),
            delete_framebuffers: get!(c"glDeleteFramebuffers"),
            bind_framebuffer: get!(c"glBindFramebuffer"),
            framebuffer_texture_2d: get!(c"glFramebufferTexture2D"),
            check_framebuffer_status: get!(c"glCheckFramebufferStatus"),
            blit_framebuffer: get!(c"glBlitFramebuffer"),
        })
    }

    /// `glGenerateMipmap(target)` on the current context.
    ///
    /// # Safety
    ///
    /// A context is current on this thread, made by the library `self` came from.
    pub unsafe fn generate_mipmap(&self, target: u32) {
        // SAFETY: as the caller promised.
        if !unsafe { self.blit_levels(target) } {
            // SAFETY: the guest's call, passed on.
            unsafe { (self.generate_mipmap)(target) };
        }
    }

    /// Fill the bound texture's levels by blitting, or return `false` having
    /// changed nothing the driver's function would not overwrite.
    ///
    /// FIXME (ACCURACY): a blit inside an active conditional render is
    /// discarded when the query failed, and `glGenerateMipmap` never is.
    unsafe fn blit_levels(&self, target: u32) -> bool {
        if target != TEXTURE_2D {
            return false;
        }
        // SAFETY: plain queries on the current context.
        let integer = |name| {
            let mut value = 0;
            unsafe { (self.get_integerv)(name, &mut value) };
            value
        };
        let parameter = |name| {
            let mut value = 0;
            unsafe { (self.get_tex_parameteriv)(target, name, &mut value) };
            value
        };
        let level = |level, name| {
            let mut value = 0;
            unsafe { (self.get_tex_level_parameteriv)(target, level, name, &mut value) };
            value
        };

        let texture = integer(TEXTURE_BINDING_2D);
        let base = parameter(TEXTURE_BASE_LEVEL);
        let format = level(base, TEXTURE_INTERNAL_FORMAT);
        if texture <= 0 || !CPU_FILTERED.contains(&format) {
            return false;
        }
        let (width, height) = (level(base, TEXTURE_WIDTH), level(base, TEXTURE_HEIGHT));
        if width <= 0 || height <= 0 {
            return false;
        }
        let top = base + width.max(height).ilog2() as i32;
        let mut last = top.min(parameter(TEXTURE_MAX_LEVEL));
        let immutable = parameter(TEXTURE_IMMUTABLE_FORMAT) != 0;
        // A mutable texture's missing level is one `glGenerateMipmap` would
        // allocate, which is left to it. An immutable texture has no levels past
        // the ones it was made with, and generation stops there.
        for l in base + 1..=last {
            let shift = (l - base) as u32;
            let expected = ((width >> shift).max(1), (height >> shift).max(1));
            let actual = (level(l, TEXTURE_WIDTH), level(l, TEXTURE_HEIGHT));
            if immutable && actual.0 == 0 {
                last = l - 1;
                break;
            }
            if actual != expected || level(l, TEXTURE_INTERNAL_FORMAT) != format {
                return false;
            }
        }
        if last <= base {
            return false;
        }

        // SAFETY: everything below is GL state of the current context, saved
        // first and restored before returning.
        unsafe {
            let read = integer(READ_FRAMEBUFFER_BINDING) as u32;
            let draw = integer(DRAW_FRAMEBUFFER_BINDING) as u32;
            let scissor = (self.is_enabled)(SCISSOR_TEST) == TRUE;
            let discard = (self.is_enabled)(RASTERIZER_DISCARD) == TRUE;
            let mut mask = [TRUE; 4];
            (self.get_booleanv)(COLOR_WRITEMASK, mask.as_mut_ptr());
            if scissor {
                (self.disable)(SCISSOR_TEST);
            }
            if discard {
                (self.disable)(RASTERIZER_DISCARD);
            }
            (self.color_mask)(TRUE, TRUE, TRUE, TRUE);

            let mut framebuffers = [0u32; 2];
            (self.gen_framebuffers)(2, framebuffers.as_mut_ptr());
            (self.bind_framebuffer)(READ_FRAMEBUFFER, framebuffers[0]);
            (self.bind_framebuffer)(DRAW_FRAMEBUFFER, framebuffers[1]);
            let mut done = true;
            for l in base + 1..=last {
                let (from, to) = ((l - 1 - base) as u32, (l - base) as u32);
                let size = |shift: u32| ((width >> shift).max(1), (height >> shift).max(1));
                let ((sw, sh), (dw, dh)) = (size(from), size(to));
                (self.framebuffer_texture_2d)(
                    READ_FRAMEBUFFER,
                    COLOR_ATTACHMENT0,
                    TEXTURE_2D,
                    texture as u32,
                    l - 1,
                );
                (self.framebuffer_texture_2d)(
                    DRAW_FRAMEBUFFER,
                    COLOR_ATTACHMENT0,
                    TEXTURE_2D,
                    texture as u32,
                    l,
                );
                if (self.check_framebuffer_status)(READ_FRAMEBUFFER) != FRAMEBUFFER_COMPLETE
                    || (self.check_framebuffer_status)(DRAW_FRAMEBUFFER) != FRAMEBUFFER_COMPLETE
                {
                    done = false;
                    break;
                }
                (self.blit_framebuffer)(0, 0, sw, sh, 0, 0, dw, dh, COLOR_BUFFER_BIT, LINEAR);
            }

            (self.bind_framebuffer)(READ_FRAMEBUFFER, read);
            (self.bind_framebuffer)(DRAW_FRAMEBUFFER, draw);
            (self.delete_framebuffers)(2, framebuffers.as_ptr());
            (self.color_mask)(mask[0], mask[1], mask[2], mask[3]);
            if discard {
                (self.enable)(RASTERIZER_DISCARD);
            }
            if scissor {
                (self.enable)(SCISSOR_TEST);
            }
            done
        }
    }
}

/// Apple's own OpenGL.framework, which a guest on a macOS host draws through
/// unless this runtime's frameworks stand in for it.
#[cfg(target_os = "macos")]
pub mod host {
    use std::sync::OnceLock;

    use super::Gl;

    const FRAMEWORK: &std::ffi::CStr = c"/System/Library/Frameworks/OpenGL.framework/OpenGL";

    fn gl() -> Option<&'static Gl> {
        static GL: OnceLock<Option<Gl>> = OnceLock::new();
        GL.get_or_init(|| {
            // SAFETY: a constant path; the handle is kept for the process.
            let library = unsafe { libc::dlopen(FRAMEWORK.as_ptr(), libc::RTLD_LAZY) };
            if library.is_null() {
                return None;
            }
            Gl::load(|name| {
                // SAFETY: a live handle and a NUL-terminated name.
                let address = unsafe { libc::dlsym(library, name.as_ptr()) };
                (!address.is_null()).then_some(address as usize)
            })
        })
        .as_ref()
    }

    unsafe extern "C" fn generate_mipmap(target: u32) {
        if let Some(gl) = gl() {
            // SAFETY: the guest's call, made with its context current.
            unsafe { gl.generate_mipmap(target) };
        }
    }

    /// This module's replacement for the framework's function `name`, if any.
    pub fn symbol(name: &str) -> Option<u64> {
        match name {
            "glGenerateMipmap" if gl().is_some() => Some(generate_mipmap as usize as u64),
            _ => None,
        }
    }
}
