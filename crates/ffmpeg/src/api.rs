//! libavcodec and libavutil, loaded when first needed.
//!
//! Never linked: a host without FFmpeg, or with another major version than
//! the one [`super::abi`] describes, decodes nothing through it, which is
//! what happened before there was an FFmpeg backend at all.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use super::abi;

type Ptr = *mut c_void;
type Const = *const c_void;

/// The entry points, by the prototypes FFmpeg 8's headers give them.
pub struct Api {
    pub avcodec_version: unsafe extern "C" fn() -> u32,
    pub avcodec_find_decoder_by_name: unsafe extern "C" fn(*const c_char) -> Const,
    pub avcodec_get_hw_config: unsafe extern "C" fn(Const, c_int) -> Const,
    pub avcodec_alloc_context3: unsafe extern "C" fn(Const) -> Ptr,
    pub avcodec_open2: unsafe extern "C" fn(Ptr, Const, *mut Ptr) -> c_int,
    pub avcodec_free_context: unsafe extern "C" fn(*mut Ptr),
    pub avcodec_send_packet: unsafe extern "C" fn(Ptr, Const) -> c_int,
    pub avcodec_receive_frame: unsafe extern "C" fn(Ptr, Ptr) -> c_int,
    pub avcodec_flush_buffers: unsafe extern "C" fn(Ptr),
    pub av_packet_alloc: unsafe extern "C" fn() -> Ptr,
    pub av_packet_free: unsafe extern "C" fn(*mut Ptr),
    pub av_new_packet: unsafe extern "C" fn(Ptr, c_int) -> c_int,
    pub av_packet_unref: unsafe extern "C" fn(Ptr),
    pub avutil_version: unsafe extern "C" fn() -> u32,
    pub av_hwdevice_find_type_by_name: unsafe extern "C" fn(*const c_char) -> c_int,
    pub av_hwdevice_ctx_create:
        unsafe extern "C" fn(*mut Ptr, c_int, *const c_char, Ptr, c_int) -> c_int,
    pub av_buffer_ref: unsafe extern "C" fn(Const) -> Ptr,
    pub av_buffer_unref: unsafe extern "C" fn(*mut Ptr),
    pub av_frame_alloc: unsafe extern "C" fn() -> Ptr,
    pub av_frame_free: unsafe extern "C" fn(*mut Ptr),
    pub av_frame_unref: unsafe extern "C" fn(Ptr),
    pub av_hwframe_transfer_data: unsafe extern "C" fn(Ptr, Const, c_int) -> c_int,
    pub av_get_pix_fmt: unsafe extern "C" fn(*const c_char) -> c_int,
    pub av_opt_set_int: unsafe extern "C" fn(Ptr, *const c_char, i64, c_int) -> c_int,
    pub av_log_set_level: unsafe extern "C" fn(c_int),
}

impl Api {
    /// libavcodec's version as `major.minor.micro`.
    pub fn version(&self) -> String {
        // SAFETY: a resolved entry point that takes nothing.
        let v = unsafe { (self.avcodec_version)() };
        format!("{}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff)
    }
}

/// # Safety
///
/// `T` must be the prototype the library declares for `name`.
unsafe fn entry<T: Copy>(handle: Ptr, name: &CStr) -> Option<T> {
    assert_eq!(size_of::<T>(), size_of::<Ptr>());
    // SAFETY: a handle from `dlopen` and a NUL-terminated name.
    let found = unsafe { libc::dlsym(handle, name.as_ptr()) };
    // SAFETY: a function pointer is one word, as checked above.
    (!found.is_null()).then(|| unsafe { *(&found as *const Ptr as *const T) })
}

fn open(soname: &CStr) -> Result<Ptr, String> {
    // SAFETY: a NUL-terminated soname. `RTLD_GLOBAL` and never closed, as
    // every other `dlopen` in this tree: the library's threads and callbacks
    // outlive any point at which closing it would be safe.
    let handle = unsafe { libc::dlopen(soname.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
    match handle.is_null() {
        true => Err(format!("{} is not installed", soname.to_string_lossy())),
        false => Ok(handle),
    }
}

impl Api {
    /// The two libraries by their sonames, checked to be the majors
    /// [`super::abi`] describes.
    pub fn load(avcodec: &CStr, avutil: &CStr) -> Result<Api, String> {
        let util = open(avutil)?;
        let codec = open(avcodec)?;
        macro_rules! resolve {
            ($handle:expr, $library:expr, $($name:ident),* $(,)?) => {
                $(
                    // SAFETY: the field's type is the prototype FFmpeg declares.
                    let $name = unsafe {
                        entry(
                            $handle,
                            CStr::from_bytes_with_nul(concat!(stringify!($name), "\0").as_bytes())
                                .unwrap(),
                        )
                    }
                    .ok_or_else(|| {
                        format!("{} has no {}", $library.to_string_lossy(), stringify!($name))
                    })?;
                )*
            };
        }
        resolve!(
            codec,
            avcodec,
            avcodec_version,
            avcodec_find_decoder_by_name,
            avcodec_get_hw_config,
            avcodec_alloc_context3,
            avcodec_open2,
            avcodec_free_context,
            avcodec_send_packet,
            avcodec_receive_frame,
            avcodec_flush_buffers,
            av_packet_alloc,
            av_packet_free,
            av_new_packet,
            av_packet_unref,
        );
        resolve!(
            util,
            avutil,
            avutil_version,
            av_hwdevice_find_type_by_name,
            av_hwdevice_ctx_create,
            av_buffer_ref,
            av_buffer_unref,
            av_frame_alloc,
            av_frame_free,
            av_frame_unref,
            av_hwframe_transfer_data,
            av_get_pix_fmt,
            av_opt_set_int,
            av_log_set_level,
        );
        let api = Api {
            avcodec_version,
            avcodec_find_decoder_by_name,
            avcodec_get_hw_config,
            avcodec_alloc_context3,
            avcodec_open2,
            avcodec_free_context,
            avcodec_send_packet,
            avcodec_receive_frame,
            avcodec_flush_buffers,
            av_packet_alloc,
            av_packet_free,
            av_new_packet,
            av_packet_unref,
            avutil_version,
            av_hwdevice_find_type_by_name,
            av_hwdevice_ctx_create,
            av_buffer_ref,
            av_buffer_unref,
            av_frame_alloc,
            av_frame_free,
            av_frame_unref,
            av_hwframe_transfer_data,
            av_get_pix_fmt,
            av_opt_set_int,
            av_log_set_level,
        };
        // SAFETY: resolved entry points that take nothing.
        let (codec_major, util_major) =
            unsafe { ((api.avcodec_version)() >> 16, (api.avutil_version)() >> 16) };
        if codec_major != abi::AVCODEC_MAJOR || util_major != abi::AVUTIL_MAJOR {
            return Err(format!(
                "libavcodec {codec_major} / libavutil {util_major} is not the FFmpeg 8 \
                 (libavcodec {} / libavutil {}) this build knows the structures of",
                abi::AVCODEC_MAJOR,
                abi::AVUTIL_MAJOR
            ));
        }
        // SAFETY: a resolved entry point. FFmpeg logs process-wide, and
        // nothing else here uses it, so errors only.
        unsafe { (api.av_log_set_level)(abi::LOG_ERROR) };
        Ok(api)
    }
}

/// The host's FFmpeg 8, shared by every backend that wants it - loaded once
/// per process regardless of how many ask.
pub fn global() -> Result<&'static Api, &'static str> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| Api::load(c"libavcodec.so.62", c"libavutil.so.60"))
        .as_ref()
        .map_err(String::as_str)
}

/// A library that is never there, for the test that a missing FFmpeg
/// leaves a backend unavailable.
///
/// Not `#[cfg(test)]`: the test is `mediabackend`'s, and a dependency is
/// never built with `cfg(test)` for a dependent's tests.
#[doc(hidden)]
pub fn bogus() -> Result<&'static Api, &'static str> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| {
        Api::load(
            c"librosemary-no-such-avcodec.so.62",
            c"librosemary-no-such-avutil.so.60",
        )
    })
    .as_ref()
    .map_err(String::as_str)
}
