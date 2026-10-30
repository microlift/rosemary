#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::OnceLock;

#[cfg(any(test, feature = "test-support"))]
pub mod isolated;
pub mod keycodes;
pub mod mouse;
pub mod tray;
pub mod wayland;
#[cfg(target_os = "linux")]
pub mod wayland_mask;
#[cfg(target_os = "linux")]
pub mod x11_mask;

pub mod init {
    pub const VIDEO: u32 = 0x0000_0020;
    pub const AUDIO: u32 = 0x0000_0010;
    pub const JOYSTICK: u32 = 0x0000_0200;
    pub const GAMEPAD: u32 = 0x0000_2000;
}

/// Open a URL with the desktop's registered handler.
pub fn open_url(url: &str) -> bool {
    let Ok(url) = CString::new(url) else {
        return false;
    };
    let Some(handle) = library() else {
        return false;
    };
    let Some(open) =
        (unsafe { entry::<unsafe extern "C" fn(*const c_char) -> bool>(handle, "SDL_OpenURL") })
    else {
        return false;
    };
    unsafe { open(url.as_ptr()) }
}

pub type AudioStream = *mut c_void;

/// `SDL_AudioStreamCallback`: `(userdata, stream, additional, total)`.
pub type AudioStreamCallback = Option<unsafe extern "C" fn(*mut c_void, AudioStream, c_int, c_int)>;

/// `SDL_AudioSpec`. Three fields and no padding on either host.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioSpec {
    pub format: u32,
    pub channels: c_int,
    pub freq: c_int,
}

pub type Joystick = *mut c_void;
pub type HidDevice = *mut c_void;
pub type Gamepad = *mut c_void;
pub type JoystickId = u32;
pub type PropertiesId = u32;
pub type Window = *mut c_void;
pub type GlContext = *mut c_void;
pub type Cursor = *mut c_void;

/// `SDL_Rect`: a display's bounds, in the desktop's own coordinates.
/// SDL3's public `SDL_Surface`. Only the fields before `refcount` are read.
#[repr(C)]
pub struct Surface {
    pub flags: u32,
    pub format: u32,
    pub w: c_int,
    pub h: c_int,
    pub pitch: c_int,
    pub pixels: *mut c_void,
    pub refcount: c_int,
    pub reserved: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: c_int,
    pub y: c_int,
    pub w: c_int,
    pub h: c_int,
}

impl Rect {
    /// The parts of a `width` by `height` surface outside this rectangle:
    /// full-height columns left and right, then the strips above and below
    /// between them. Every mask and blackout uses these, so they agree on
    /// each edge pixel; empty bands are left out.
    pub fn complements(self, (width, height): (c_int, c_int)) -> impl Iterator<Item = Rect> {
        let (width, height) = (width.max(0), height.max(0));
        let x0 = self.x.clamp(0, width);
        let y0 = self.y.clamp(0, height);
        let x1 = self.x.saturating_add(self.w).clamp(x0, width);
        let y1 = self.y.saturating_add(self.h).clamp(y0, height);
        [
            (0, 0, x0, height),
            (x1, 0, width - x1, height),
            (x0, 0, x1 - x0, y0),
            (x0, y1, x1 - x0, height - y1),
        ]
        .into_iter()
        .map(|(x, y, w, h)| Rect { x, y, w, h })
        .filter(|band| band.w > 0 && band.h > 0)
    }
}

/// `SDL_DisplayMode`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct DisplayMode {
    pub display: u32,
    pub format: u32,
    pub w: c_int,
    pub h: c_int,
    pub pixel_density: f32,
    pub refresh_rate: f32,
    pub refresh_rate_numerator: c_int,
    pub refresh_rate_denominator: c_int,
    pub internal: *mut c_void,
}

/// `SDL_Event`, as bytes.
///
/// The 128-byte union is decoded into [`video::Event`] only where needed.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct Event {
    pub bytes: [u8; 128],
}

impl Default for Event {
    fn default() -> Self {
        Event { bytes: [0; 128] }
    }
}

/// The entry points this tree calls.
///
/// Each is `Option`, and a missing one is a name this build of SDL does not
/// have - which is reported rather than called, since a null call would be a
/// crash with nothing to say which symbol was absent.
#[allow(non_snake_case)]
struct Api {
    SDL_InitSubSystem: Option<unsafe extern "C" fn(u32) -> bool>,
    SDL_SetAppMetadataProperty: Option<unsafe extern "C" fn(*const c_char, *const c_char) -> bool>,
    SDL_GetError: Option<unsafe extern "C" fn() -> *const c_char>,
    SDL_PumpEvents: Option<unsafe extern "C" fn()>,
    SDL_UpdateJoysticks: Option<unsafe extern "C" fn()>,
    SDL_QuitSubSystem: Option<unsafe extern "C" fn(u32)>,

    SDL_GetJoysticks: Option<unsafe extern "C" fn(*mut c_int) -> *mut JoystickId>,
    SDL_OpenJoystick: Option<unsafe extern "C" fn(JoystickId) -> Joystick>,
    SDL_CloseJoystick: Option<unsafe extern "C" fn(Joystick)>,
    SDL_GetJoystickName: Option<unsafe extern "C" fn(Joystick) -> *const c_char>,
    SDL_GetJoystickPath: Option<unsafe extern "C" fn(Joystick) -> *const c_char>,
    SDL_GetJoystickVendor: Option<unsafe extern "C" fn(Joystick) -> u16>,
    SDL_GetJoystickProduct: Option<unsafe extern "C" fn(Joystick) -> u16>,
    SDL_GetJoystickProductVersion: Option<unsafe extern "C" fn(Joystick) -> u16>,
    SDL_GetJoystickProperties: Option<unsafe extern "C" fn(Joystick) -> PropertiesId>,
    SDL_JoystickConnected: Option<unsafe extern "C" fn(Joystick) -> bool>,
    SDL_GetNumJoystickAxes: Option<unsafe extern "C" fn(Joystick) -> c_int>,
    SDL_GetNumJoystickButtons: Option<unsafe extern "C" fn(Joystick) -> c_int>,
    SDL_GetNumJoystickHats: Option<unsafe extern "C" fn(Joystick) -> c_int>,
    SDL_GetJoystickAxis: Option<unsafe extern "C" fn(Joystick, c_int) -> i16>,
    SDL_GetJoystickButton: Option<unsafe extern "C" fn(Joystick, c_int) -> bool>,
    SDL_GetJoystickHat: Option<unsafe extern "C" fn(Joystick, c_int) -> u8>,
    SDL_SendJoystickEffect: Option<unsafe extern "C" fn(Joystick, *const c_void, c_int) -> bool>,
    SDL_RumbleJoystick: Option<unsafe extern "C" fn(Joystick, u16, u16, u32) -> bool>,
    SDL_hid_open_path: Option<unsafe extern "C" fn(*const c_char) -> HidDevice>,
    SDL_hid_close: Option<unsafe extern "C" fn(HidDevice)>,
    SDL_hid_get_input_report: Option<unsafe extern "C" fn(HidDevice, *mut u8, usize) -> c_int>,
    SDL_hid_get_feature_report: Option<unsafe extern "C" fn(HidDevice, *mut u8, usize) -> c_int>,
    SDL_hid_get_report_descriptor: Option<unsafe extern "C" fn(HidDevice, *mut u8, usize) -> c_int>,
    SDL_hid_read_timeout: Option<unsafe extern "C" fn(HidDevice, *mut u8, usize, c_int) -> c_int>,
    SDL_hid_write: Option<unsafe extern "C" fn(HidDevice, *const u8, usize) -> c_int>,
    SDL_hid_send_feature_report: Option<unsafe extern "C" fn(HidDevice, *const u8, usize) -> c_int>,
    SDL_GetGamepads: Option<unsafe extern "C" fn(*mut c_int) -> *mut JoystickId>,
    SDL_OpenGamepad: Option<unsafe extern "C" fn(JoystickId) -> Gamepad>,
    SDL_CloseGamepad: Option<unsafe extern "C" fn(Gamepad)>,
    SDL_GetGamepadName: Option<unsafe extern "C" fn(Gamepad) -> *const c_char>,
    SDL_GetGamepadVendor: Option<unsafe extern "C" fn(Gamepad) -> u16>,
    SDL_GetGamepadProperties: Option<unsafe extern "C" fn(Gamepad) -> PropertiesId>,
    SDL_GetBooleanProperty: Option<unsafe extern "C" fn(PropertiesId, *const c_char, bool) -> bool>,
    SDL_GetGamepadAxis: Option<unsafe extern "C" fn(Gamepad, c_int) -> i16>,
    SDL_GetGamepadButton: Option<unsafe extern "C" fn(Gamepad, c_int) -> bool>,
    SDL_free: Option<unsafe extern "C" fn(*mut c_void)>,

    SDL_CreateWindow: Option<unsafe extern "C" fn(*const c_char, c_int, c_int, u64) -> Window>,
    SDL_CreatePopupWindow:
        Option<unsafe extern "C" fn(Window, c_int, c_int, c_int, c_int, u64) -> Window>,
    SDL_GetWindowParent: Option<unsafe extern "C" fn(Window) -> Window>,
    SDL_DestroyWindow: Option<unsafe extern "C" fn(Window)>,
    SDL_SetWindowTitle: Option<unsafe extern "C" fn(Window, *const c_char) -> bool>,
    SDL_GetWindowTitle: Option<unsafe extern "C" fn(Window) -> *const c_char>,
    SDL_GetWindowSize: Option<unsafe extern "C" fn(Window, *mut c_int, *mut c_int) -> bool>,
    SDL_SetWindowSize: Option<unsafe extern "C" fn(Window, c_int, c_int) -> bool>,
    SDL_GetWindowSizeInPixels: Option<unsafe extern "C" fn(Window, *mut c_int, *mut c_int) -> bool>,
    SDL_GetWindowPosition: Option<unsafe extern "C" fn(Window, *mut c_int, *mut c_int) -> bool>,
    SDL_SetWindowPosition: Option<unsafe extern "C" fn(Window, c_int, c_int) -> bool>,
    SDL_SetWindowOpacity: Option<unsafe extern "C" fn(Window, f32) -> bool>,
    SDL_GetWindowOpacity: Option<unsafe extern "C" fn(Window) -> f32>,
    SDL_ShowWindow: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_HideWindow: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_RaiseWindow: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_SetWindowParent: Option<unsafe extern "C" fn(Window, Window) -> bool>,
    SDL_SetWindowModal: Option<unsafe extern "C" fn(Window, bool) -> bool>,
    SDL_SetWindowFullscreen: Option<unsafe extern "C" fn(Window, bool) -> bool>,
    SDL_SetWindowResizable: Option<unsafe extern "C" fn(Window, bool) -> bool>,
    SDL_SetWindowBordered: Option<unsafe extern "C" fn(Window, bool) -> bool>,
    SDL_MinimizeWindow: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_MaximizeWindow: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_RestoreWindow: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_SetWindowMinimumSize: Option<unsafe extern "C" fn(Window, c_int, c_int) -> bool>,
    SDL_ShowWindowSystemMenu: Option<unsafe extern "C" fn(Window, c_int, c_int) -> bool>,
    SDL_SetWindowHitTest:
        Option<unsafe extern "C" fn(Window, Option<video::HitTest>, *mut c_void) -> bool>,
    SDL_SetWindowAlwaysOnTop: Option<unsafe extern "C" fn(Window, bool) -> bool>,
    SDL_SetWindowFocusable: Option<unsafe extern "C" fn(Window, bool) -> bool>,
    SDL_GetWindowPixelDensity: Option<unsafe extern "C" fn(Window) -> f32>,
    SDL_GetWindowID: Option<unsafe extern "C" fn(Window) -> u32>,
    SDL_GetWindowFromID: Option<unsafe extern "C" fn(u32) -> Window>,
    SDL_GetWindowFlags: Option<unsafe extern "C" fn(Window) -> u64>,
    SDL_SetWindowIcon: Option<unsafe extern "C" fn(Window, *mut c_void) -> bool>,
    SDL_CreateSurfaceFrom:
        Option<unsafe extern "C" fn(c_int, c_int, u32, *mut c_void, c_int) -> *mut c_void>,
    SDL_DestroySurface: Option<unsafe extern "C" fn(*mut c_void)>,
    SDL_GetWindowSurface: Option<unsafe extern "C" fn(Window) -> *mut Surface>,
    SDL_UpdateWindowSurface: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_LockSurface: Option<unsafe extern "C" fn(*mut Surface) -> bool>,
    SDL_UnlockSurface: Option<unsafe extern "C" fn(*mut Surface)>,

    /// `VkInstance` is a dispatchable handle, so a pointer; `VkSurfaceKHR` is
    /// not, so a `uint64_t` on every platform. Neither is spelled in Vulkan's
    /// own types here, because this crate does not depend on `ash`.
    SDL_Vulkan_LoadLibrary: Option<unsafe extern "C" fn(*const c_char) -> bool>,
    SDL_Vulkan_UnloadLibrary: Option<unsafe extern "C" fn()>,
    SDL_Vulkan_CreateSurface:
        Option<unsafe extern "C" fn(Window, *mut c_void, *const c_void, *mut u64) -> bool>,
    SDL_Vulkan_DestroySurface: Option<unsafe extern "C" fn(*mut c_void, u64, *const c_void)>,

    SDL_GL_LoadLibrary: Option<unsafe extern "C" fn(*const c_char) -> bool>,
    SDL_GL_CreateContext: Option<unsafe extern "C" fn(Window) -> GlContext>,
    SDL_GL_DestroyContext: Option<unsafe extern "C" fn(GlContext) -> bool>,
    SDL_GL_MakeCurrent: Option<unsafe extern "C" fn(Window, GlContext) -> bool>,
    SDL_GL_GetCurrentContext: Option<unsafe extern "C" fn() -> GlContext>,
    SDL_GL_GetCurrentWindow: Option<unsafe extern "C" fn() -> Window>,
    SDL_GL_SwapWindow: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_GL_SetAttribute: Option<unsafe extern "C" fn(c_int, c_int) -> bool>,
    SDL_GL_ResetAttributes: Option<unsafe extern "C" fn()>,
    SDL_GL_SetSwapInterval: Option<unsafe extern "C" fn(c_int) -> bool>,
    SDL_GL_GetProcAddress: Option<unsafe extern "C" fn(*const c_char) -> *mut c_void>,
    SDL_GL_ExtensionSupported: Option<unsafe extern "C" fn(*const c_char) -> bool>,

    /// The EGL objects SDL made, for the one thing `SDL_GL_*` cannot express -
    /// see [`video::egl`]. Absent on an SDL whose GL is not EGL-backed.
    SDL_EGL_GetCurrentDisplay: Option<unsafe extern "C" fn() -> *mut c_void>,
    SDL_EGL_GetCurrentConfig: Option<unsafe extern "C" fn() -> *mut c_void>,
    SDL_EGL_GetProcAddress: Option<unsafe extern "C" fn(*const c_char) -> *mut c_void>,

    SDL_GetPrimaryDisplay: Option<unsafe extern "C" fn() -> u32>,
    SDL_GetDisplays: Option<unsafe extern "C" fn(*mut c_int) -> *mut u32>,
    SDL_GetDisplayBounds: Option<unsafe extern "C" fn(u32, *mut Rect) -> bool>,
    SDL_GetDisplayUsableBounds: Option<unsafe extern "C" fn(u32, *mut Rect) -> bool>,
    SDL_GetDisplayContentScale: Option<unsafe extern "C" fn(u32) -> f32>,
    SDL_GetDesktopDisplayMode: Option<unsafe extern "C" fn(u32) -> *const DisplayMode>,
    SDL_GetCurrentDisplayMode: Option<unsafe extern "C" fn(u32) -> *const DisplayMode>,
    SDL_GetDisplayForWindow: Option<unsafe extern "C" fn(Window) -> u32>,
    SDL_GetDisplayName: Option<unsafe extern "C" fn(u32) -> *const c_char>,
    SDL_GetFullscreenDisplayModes:
        Option<unsafe extern "C" fn(u32, *mut c_int) -> *mut *mut DisplayMode>,

    SDL_PollEvent: Option<unsafe extern "C" fn(*mut Event) -> bool>,
    SDL_PeepEvents: Option<unsafe extern "C" fn(*mut Event, c_int, c_int, u32, u32) -> c_int>,
    SDL_WaitEventTimeout: Option<unsafe extern "C" fn(*mut Event, i32) -> bool>,
    SDL_StartTextInput: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_StopTextInput: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_TextInputActive: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_SetTextInputArea: Option<unsafe extern "C" fn(Window, *const Rect, c_int) -> bool>,
    SDL_HideCursor: Option<unsafe extern "C" fn() -> bool>,
    SDL_ShowCursor: Option<unsafe extern "C" fn() -> bool>,
    SDL_CreateColorCursor: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int) -> Cursor>,
    SDL_CreateSystemCursor: Option<unsafe extern "C" fn(c_int) -> Cursor>,
    SDL_SetCursor: Option<unsafe extern "C" fn(Cursor) -> bool>,
    SDL_GetDefaultCursor: Option<unsafe extern "C" fn() -> Cursor>,
    SDL_DestroyCursor: Option<unsafe extern "C" fn(Cursor)>,
    SDL_GetMouseFocus: Option<unsafe extern "C" fn() -> Window>,
    SDL_GetMouseState: Option<unsafe extern "C" fn(*mut f32, *mut f32) -> u32>,
    SDL_GetGlobalMouseState: Option<unsafe extern "C" fn(*mut f32, *mut f32) -> u32>,
    SDL_WarpMouseInWindow: Option<unsafe extern "C" fn(Window, f32, f32)>,
    SDL_WarpMouseGlobal: Option<unsafe extern "C" fn(f32, f32) -> bool>,
    SDL_SetWindowRelativeMouseMode: Option<unsafe extern "C" fn(Window, bool) -> bool>,
    SDL_GetWindowRelativeMouseMode: Option<unsafe extern "C" fn(Window) -> bool>,
    SDL_SetWindowMouseGrab: Option<unsafe extern "C" fn(Window, bool) -> bool>,
    SDL_SetWindowMouseRect: Option<unsafe extern "C" fn(Window, *const Rect) -> bool>,
    SDL_GetWindowMouseRect: Option<unsafe extern "C" fn(Window) -> *const Rect>,
    SDL_GetModState: Option<unsafe extern "C" fn() -> u16>,
    SDL_GetKeyFromScancode: Option<unsafe extern "C" fn(u32, u16, bool) -> u32>,
    SDL_SetHint: Option<unsafe extern "C" fn(*const c_char, *const c_char) -> bool>,
    SDL_ResetHint: Option<unsafe extern "C" fn(*const c_char) -> bool>,
    SDL_GetSystemTheme: Option<unsafe extern "C" fn() -> c_int>,
    SDL_GetCurrentVideoDriver: Option<unsafe extern "C" fn() -> *const c_char>,
    SDL_GetWindowProperties: Option<unsafe extern "C" fn(Window) -> PropertiesId>,
    SDL_GetNumberProperty: Option<unsafe extern "C" fn(PropertiesId, *const c_char, i64) -> i64>,
    SDL_GetPointerProperty:
        Option<unsafe extern "C" fn(PropertiesId, *const c_char, *mut c_void) -> *mut c_void>,

    SDL_OpenAudioDeviceStream: Option<
        unsafe extern "C" fn(
            u32,
            *const AudioSpec,
            AudioStreamCallback,
            *mut c_void,
        ) -> AudioStream,
    >,
    SDL_DestroyAudioStream: Option<unsafe extern "C" fn(AudioStream)>,
    SDL_PutAudioStreamData: Option<unsafe extern "C" fn(AudioStream, *const c_void, c_int) -> bool>,
    SDL_GetAudioStreamQueued: Option<unsafe extern "C" fn(AudioStream) -> c_int>,
    SDL_ClearAudioStream: Option<unsafe extern "C" fn(AudioStream) -> bool>,
    SDL_ResumeAudioStreamDevice: Option<unsafe extern "C" fn(AudioStream) -> bool>,
    SDL_PauseAudioStreamDevice: Option<unsafe extern "C" fn(AudioStream) -> bool>,
    SDL_GetAudioDeviceFormat: Option<unsafe extern "C" fn(u32, *mut AudioSpec, *mut c_int) -> bool>,
    SDL_GetAudioDeviceName: Option<unsafe extern "C" fn(u32) -> *const c_char>,
    SDL_GetAudioRecordingDevices: Option<unsafe extern "C" fn(*mut c_int) -> *mut u32>,
    SDL_CreateAudioStream:
        Option<unsafe extern "C" fn(*const AudioSpec, *const AudioSpec) -> AudioStream>,
    SDL_SetAudioStreamFormat:
        Option<unsafe extern "C" fn(AudioStream, *const AudioSpec, *const AudioSpec) -> bool>,
    SDL_GetAudioStreamData: Option<unsafe extern "C" fn(AudioStream, *mut c_void, c_int) -> c_int>,
    SDL_GetAudioStreamAvailable: Option<unsafe extern "C" fn(AudioStream) -> c_int>,
    SDL_FlushAudioStream: Option<unsafe extern "C" fn(AudioStream) -> bool>,
}

/// The library, opened once.
///
/// Several sonames: a host may have any of them, and asking for each in turn is
/// cheaper than requiring one.
fn library() -> Option<*mut c_void> {
    static HANDLE: OnceLock<usize> = OnceLock::new();
    let handle = *HANDLE.get_or_init(|| {
        for name in [
            c"libSDL3.so.0",
            c"libSDL3.so",
            c"libSDL3.0.dylib",
            c"libSDL3.dylib",
            c"/opt/homebrew/lib/libSDL3.dylib",
            c"/usr/local/lib/libSDL3.dylib",
        ] {
            // SAFETY: a NUL-terminated soname. `RTLD_GLOBAL` and never closed,
            // for the reason every other `dlopen` in this tree is: a pointer
            // already handed out must not dangle.
            let handle =
                unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
            if !handle.is_null() {
                return handle as usize;
            }
        }
        0
    });
    (handle != 0).then_some(handle as *mut c_void)
}

/// Resolve one entry point by name.
///
/// # Safety
///
/// `T` must be the prototype SDL declares for `name`.
unsafe fn entry<T: Copy>(handle: *mut c_void, name: &str) -> Option<T> {
    const _: () = assert!(size_of::<*const c_void>() == 8);
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: a handle from `dlopen` and a NUL-terminated name.
    let found = unsafe { libc::dlsym(handle, c.as_ptr()) };
    if found.is_null() {
        return None;
    }
    assert_eq!(
        size_of::<T>(),
        size_of::<*const c_void>(),
        "an entry point is a bare function pointer"
    );
    Some(unsafe { *(&found as *const *mut c_void as *const T) })
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let handle = library()?;
        /// Resolve every field by its own name, which is SDL's spelling.
        macro_rules! resolve {
            ($($name:ident),* $(,)?) => {
                Api {
                    // SAFETY: each field's type above is the prototype SDL3's
                    // headers declare for that name.
                    $($name: unsafe { entry(handle, stringify!($name)) },)*
                }
            };
        }
        Some(resolve!(
            SDL_InitSubSystem,
            SDL_SetAppMetadataProperty,
            SDL_GetError,
            SDL_PumpEvents,
            SDL_UpdateJoysticks,
            SDL_QuitSubSystem,
            SDL_GetJoysticks,
            SDL_OpenJoystick,
            SDL_CloseJoystick,
            SDL_GetJoystickName,
            SDL_GetJoystickPath,
            SDL_GetJoystickVendor,
            SDL_GetJoystickProduct,
            SDL_GetJoystickProductVersion,
            SDL_GetJoystickProperties,
            SDL_JoystickConnected,
            SDL_GetNumJoystickAxes,
            SDL_GetNumJoystickButtons,
            SDL_GetNumJoystickHats,
            SDL_GetJoystickAxis,
            SDL_GetJoystickButton,
            SDL_GetJoystickHat,
            SDL_SendJoystickEffect,
            SDL_RumbleJoystick,
            SDL_hid_open_path,
            SDL_hid_close,
            SDL_hid_get_input_report,
            SDL_hid_get_feature_report,
            SDL_hid_get_report_descriptor,
            SDL_hid_read_timeout,
            SDL_hid_write,
            SDL_hid_send_feature_report,
            SDL_GetGamepads,
            SDL_OpenGamepad,
            SDL_CloseGamepad,
            SDL_GetGamepadName,
            SDL_GetGamepadVendor,
            SDL_GetGamepadProperties,
            SDL_GetBooleanProperty,
            SDL_GetGamepadAxis,
            SDL_GetGamepadButton,
            SDL_free,
            SDL_CreateWindow,
            SDL_CreatePopupWindow,
            SDL_GetWindowParent,
            SDL_DestroyWindow,
            SDL_SetWindowTitle,
            SDL_GetWindowTitle,
            SDL_GetWindowSize,
            SDL_SetWindowSize,
            SDL_GetWindowSizeInPixels,
            SDL_GetWindowPosition,
            SDL_SetWindowPosition,
            SDL_SetWindowOpacity,
            SDL_GetWindowOpacity,
            SDL_ShowWindow,
            SDL_HideWindow,
            SDL_RaiseWindow,
            SDL_SetWindowParent,
            SDL_SetWindowModal,
            SDL_SetWindowFullscreen,
            SDL_SetWindowResizable,
            SDL_SetWindowBordered,
            SDL_MinimizeWindow,
            SDL_MaximizeWindow,
            SDL_RestoreWindow,
            SDL_SetWindowMinimumSize,
            SDL_ShowWindowSystemMenu,
            SDL_SetWindowHitTest,
            SDL_SetWindowAlwaysOnTop,
            SDL_SetWindowFocusable,
            SDL_GetWindowPixelDensity,
            SDL_GetWindowID,
            SDL_GetWindowFromID,
            SDL_GetWindowFlags,
            SDL_SetWindowIcon,
            SDL_CreateSurfaceFrom,
            SDL_DestroySurface,
            SDL_GetWindowSurface,
            SDL_UpdateWindowSurface,
            SDL_LockSurface,
            SDL_UnlockSurface,
            SDL_Vulkan_LoadLibrary,
            SDL_Vulkan_UnloadLibrary,
            SDL_Vulkan_CreateSurface,
            SDL_Vulkan_DestroySurface,
            SDL_GL_LoadLibrary,
            SDL_GL_CreateContext,
            SDL_GL_DestroyContext,
            SDL_GL_MakeCurrent,
            SDL_GL_GetCurrentContext,
            SDL_GL_GetCurrentWindow,
            SDL_GL_SwapWindow,
            SDL_GL_SetAttribute,
            SDL_GL_ResetAttributes,
            SDL_GL_SetSwapInterval,
            SDL_GL_GetProcAddress,
            SDL_GL_ExtensionSupported,
            SDL_EGL_GetCurrentDisplay,
            SDL_EGL_GetCurrentConfig,
            SDL_EGL_GetProcAddress,
            SDL_GetPrimaryDisplay,
            SDL_GetDisplays,
            SDL_GetDisplayBounds,
            SDL_GetDisplayUsableBounds,
            SDL_GetDisplayContentScale,
            SDL_GetDesktopDisplayMode,
            SDL_GetCurrentDisplayMode,
            SDL_GetDisplayForWindow,
            SDL_GetDisplayName,
            SDL_GetFullscreenDisplayModes,
            SDL_PollEvent,
            SDL_PeepEvents,
            SDL_WaitEventTimeout,
            SDL_StartTextInput,
            SDL_StopTextInput,
            SDL_TextInputActive,
            SDL_SetTextInputArea,
            SDL_HideCursor,
            SDL_ShowCursor,
            SDL_CreateColorCursor,
            SDL_CreateSystemCursor,
            SDL_SetCursor,
            SDL_GetDefaultCursor,
            SDL_DestroyCursor,
            SDL_GetMouseFocus,
            SDL_GetMouseState,
            SDL_GetGlobalMouseState,
            SDL_WarpMouseInWindow,
            SDL_WarpMouseGlobal,
            SDL_SetWindowRelativeMouseMode,
            SDL_GetWindowRelativeMouseMode,
            SDL_SetWindowMouseGrab,
            SDL_SetWindowMouseRect,
            SDL_GetWindowMouseRect,
            SDL_GetModState,
            SDL_GetKeyFromScancode,
            SDL_SetHint,
            SDL_ResetHint,
            SDL_GetSystemTheme,
            SDL_GetCurrentVideoDriver,
            SDL_GetWindowProperties,
            SDL_GetNumberProperty,
            SDL_GetPointerProperty,
            SDL_OpenAudioDeviceStream,
            SDL_DestroyAudioStream,
            SDL_PutAudioStreamData,
            SDL_GetAudioStreamQueued,
            SDL_ClearAudioStream,
            SDL_ResumeAudioStreamDevice,
            SDL_PauseAudioStreamDevice,
            SDL_GetAudioDeviceFormat,
            SDL_GetAudioDeviceName,
            SDL_GetAudioRecordingDevices,
            SDL_CreateAudioStream,
            SDL_SetAudioStreamFormat,
            SDL_GetAudioStreamData,
            SDL_GetAudioStreamAvailable,
            SDL_FlushAudioStream,
        ))
    })
    .as_ref()
}

pub fn available() -> bool {
    api().is_some()
}

pub fn refusal() -> &'static str {
    "this host has no SDL3: install it from your package manager"
}

/// SDL's last error, for a diagnostic.
pub fn error() -> String {
    let Some(api) = api() else {
        return refusal().to_string();
    };
    let Some(get) = api.SDL_GetError else {
        return String::new();
    };
    // SAFETY: `SDL_GetError` returns a NUL-terminated string SDL owns.
    let text = unsafe { get() };
    match text.is_null() {
        true => String::new(),
        false => unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned(),
    }
}

/// What the desktop is told this program is.
///
/// Every field is optional, and one left out stays unset in SDL rather than
/// being set to an empty string: the identifier in particular is matched
/// against by a compositor, and an empty one matches the wrong thing.
#[derive(Default, Debug)]
pub struct AppMetadata {
    pub name: Option<String>,
    pub version: Option<String>,
    pub identifier: Option<String>,
    pub creator: Option<String>,
    pub copyright: Option<String>,
    pub url: Option<String>,
    /// `"game"`, `"mediaplayer"` or `"application"` - SDL's own three.
    pub kind: Option<String>,
}

/// Publish `metadata`, which SDL passes on to the desktop: the name an audio
/// mixer shows for a stream, the Wayland `app_id` a compositor matches a
/// desktop entry and its icon against, and the X11 `WM_CLASS`.
///
/// Has to happen before SDL comes up. SDL reads these when its drivers start,
/// and a later call changes the properties without changing what the audio
/// server and the compositor were already told.
pub fn set_app_metadata(metadata: &AppMetadata) {
    let Some(set) = api().and_then(|a| a.SDL_SetAppMetadataProperty) else {
        return;
    };
    for (property, value) in [
        (c"SDL.app.metadata.name", &metadata.name),
        (c"SDL.app.metadata.version", &metadata.version),
        (c"SDL.app.metadata.identifier", &metadata.identifier),
        (c"SDL.app.metadata.creator", &metadata.creator),
        (c"SDL.app.metadata.copyright", &metadata.copyright),
        (c"SDL.app.metadata.url", &metadata.url),
        (c"SDL.app.metadata.type", &metadata.kind),
    ] {
        let Some(value) = value.as_deref().and_then(|v| CString::new(v).ok()) else {
            continue;
        };
        // SAFETY: two NUL-terminated strings, both of which SDL copies.
        unsafe { set(property.as_ptr(), value.as_ptr()) };
    }
}

/// Bring up one or more subsystems, or say why not.
///
/// Idempotent per subsystem: SDL counts them, and this tree may want the
/// joystick one from IOKit and the video one from AppKit without either knowing
/// about the other.
pub fn init(flags: u32) -> Result<(), String> {
    let Some(api) = api() else {
        return Err(refusal().to_string());
    };
    let Some(sub) = api.SDL_InitSubSystem else {
        return Err("this SDL3 has no SDL_InitSubSystem".to_string());
    };
    // SAFETY: takes a flag word and nothing else. `SDL_InitSubSystem` brings
    // SDL up on its own if nothing has yet, and counts a subsystem already
    // running rather than restarting it - which is what lets IOKit ask for the
    // joysticks and AppKit for the video without either knowing about the other.
    let ok = unsafe { sub(flags) };
    match ok {
        true => Ok(()),
        false => Err(error()),
    }
}

/// The thread SDL's *video* subsystem was brought up on, as a `pthread_t`, or
/// zero while there is no video at all.
static VIDEO_THREAD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn this_thread() -> usize {
    unsafe { libc::pthread_self() as usize }
}

/// Whether servicing the event queue from this thread is allowed: either there
/// is no video subsystem, or this is the thread it belongs to.
pub fn may_pump() -> bool {
    let owner = VIDEO_THREAD.load(std::sync::atomic::Ordering::Acquire);
    owner == 0 || owner == this_thread()
}

/// Record the thread the video subsystem came up on. Called once, from
/// [`video::init_video`].
fn claim_video_thread() {
    let _ = VIDEO_THREAD.compare_exchange(
        0,
        this_thread(),
        std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn release_video_thread() {
    VIDEO_THREAD.store(0, std::sync::atomic::Ordering::Release);
}

/// Service SDL's own event queue, which is what makes the state below change.
///
/// A no-op on any thread but the video subsystem's. The Cocoa backend pumps by calling
/// `-[NSApplication nextEventMatchingMask:...]`, which **throws** off the main
/// thread - and that exception unwinds out through whatever C callback this was
/// reached from, which aborts the process with no diagnostic. The
/// [`crate::video`] state a caller wanted refreshed is refreshed by the main
/// thread's own pump instead.
pub fn pump() {
    if !may_pump() {
        return;
    }
    if let Some(pump) = api().and_then(|a| a.SDL_PumpEvents) {
        // SAFETY: takes nothing, and this is a thread allowed to service the
        // queue.
        unsafe { pump() };
    }
}

/// Initialize both joystick enumeration and semantic gamepad mappings once.
pub fn init_controllers() -> Result<(), String> {
    static READY: OnceLock<Result<(), String>> = OnceLock::new();
    READY.get_or_init(|| init(init::GAMEPAD)).clone()
}

/// Refresh decoded input without servicing the video event queue.
/// SDL permits this operation from any thread.
pub fn update_joysticks() {
    if let Some(update) = api().and_then(|a| a.SDL_UpdateJoysticks) {
        unsafe { update() };
    }
}

/// The joystick instance ids attached right now.
pub fn joysticks() -> Vec<JoystickId> {
    let Some(api) = api() else {
        return Vec::new();
    };
    let (Some(get), Some(free)) = (api.SDL_GetJoysticks, api.SDL_free) else {
        return Vec::new();
    };
    let mut count: c_int = 0;
    // SAFETY: `SDL_GetJoysticks` writes the count and returns an array of that
    // many ids, allocated with SDL's own allocator and freed below.
    unsafe {
        let ids = get(&mut count);
        if ids.is_null() || count <= 0 {
            if !ids.is_null() {
                free(ids as *mut c_void);
            }
            return Vec::new();
        }
        let out = std::slice::from_raw_parts(ids, count as usize).to_vec();
        free(ids as *mut c_void);
        out
    }
}

/// Open a joystick, or `None`.
pub fn open(id: JoystickId) -> Option<Joystick> {
    let open = api()?.SDL_OpenJoystick?;
    // SAFETY: an id `SDL_GetJoysticks` reported.
    let joystick = unsafe { open(id) };
    (!joystick.is_null()).then_some(joystick)
}

/// # Safety
///
/// `joystick` must be one [`open`] returned and not yet closed.
pub unsafe fn close(joystick: Joystick) {
    if let Some(close) = api().and_then(|a| a.SDL_CloseJoystick) {
        unsafe { close(joystick) };
    }
}

/// A joystick's name, or empty.
///
/// # Safety
///
/// As [`close`].
pub unsafe fn name(joystick: Joystick) -> String {
    let Some(get) = api().and_then(|a| a.SDL_GetJoystickName) else {
        return String::new();
    };
    // SAFETY: the caller's obligation; the string is SDL's and NUL-terminated.
    let text = unsafe { get(joystick) };
    match text.is_null() {
        true => String::new(),
        false => unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned(),
    }
}

/// Open SDL's raw HID handle for the same physical device as `joystick`.
/// Virtual and platform-native devices without a HID path return `None`.
///
/// # Safety
///
/// As [`close`].
pub unsafe fn open_hid(joystick: Joystick) -> Option<HidDevice> {
    let api = api()?;
    // A handle is useful only if its descriptor and interrupt input can be read
    // and its lifetime can be ended.
    api.SDL_hid_close?;
    api.SDL_hid_read_timeout?;
    api.SDL_hid_get_report_descriptor?;
    let path = unsafe { api.SDL_GetJoystickPath?(joystick) };
    if path.is_null() {
        return None;
    }
    let device = unsafe { api.SDL_hid_open_path?(path) };
    (!device.is_null()).then_some(device)
}

/// # Safety
/// `device` must be an open HID handle that is not used again after this call.
pub unsafe fn close_hid(device: HidDevice) {
    if let Some(close) = api().and_then(|a| a.SDL_hid_close) {
        unsafe { close(device) };
    }
}

pub fn hid_report_descriptor(device: HidDevice) -> Option<Vec<u8>> {
    let get = api()?.SDL_hid_get_report_descriptor?;
    let mut bytes = vec![0; 4096];
    // SAFETY: an open HID handle and a writable vector.
    let length = unsafe { get(device, bytes.as_mut_ptr(), bytes.len()) };
    let length = usize::try_from(length).ok()?;
    bytes.truncate(length);
    Some(bytes)
}

pub fn hid_get_report(device: HidDevice, feature: bool, bytes: &mut [u8]) -> Option<usize> {
    let api = api()?;
    let get = match feature {
        true => api.SDL_hid_get_feature_report?,
        false => api.SDL_hid_get_input_report?,
    };
    // SAFETY: an open HID handle and a writable slice.
    usize::try_from(unsafe { get(device, bytes.as_mut_ptr(), bytes.len()) }).ok()
}

pub fn hid_read(device: HidDevice, bytes: &mut [u8]) -> Option<usize> {
    let read = api()?.SDL_hid_read_timeout?;
    // SAFETY: an open HID handle and a writable slice; zero never blocks.
    usize::try_from(unsafe { read(device, bytes.as_mut_ptr(), bytes.len(), 0) }).ok()
}

pub fn hid_write(device: HidDevice, feature: bool, bytes: &[u8]) -> bool {
    let Some(api) = api() else {
        return false;
    };
    let Some(write) = (match feature {
        true => api.SDL_hid_send_feature_report,
        false => api.SDL_hid_write,
    }) else {
        return false;
    };
    // SAFETY: an open HID handle and a readable slice.
    usize::try_from(unsafe { write(device, bytes.as_ptr(), bytes.len()) })
        .is_ok_and(|written| written == bytes.len())
}

/// Build a getter that takes a joystick and answers a number.
macro_rules! getter {
    ($(#[$meta:meta])* $name:ident, $field:ident, $ty:ty, $default:expr) => {
        $(#[$meta])*
        ///
        /// # Safety
        ///
        /// As [`close`].
        pub unsafe fn $name(joystick: Joystick) -> $ty {
            match api().and_then(|a| a.$field) {
                Some(f) => unsafe { f(joystick) },
                None => $default,
            }
        }
    };
}

getter!(/// The USB vendor id, or zero for a device that reports none.
    vendor, SDL_GetJoystickVendor, u16, 0);
getter!(/// The USB product id.
    product, SDL_GetJoystickProduct, u16, 0);
getter!(/// The device's version, which is the third word of a HID GUID.
    version, SDL_GetJoystickProductVersion, u16, 0);
getter!(/// How many axes it has.
    axes, SDL_GetNumJoystickAxes, c_int, 0);
getter!(/// How many buttons it has.
    buttons, SDL_GetNumJoystickButtons, c_int, 0);
getter!(/// How many hats it has.
    hats, SDL_GetNumJoystickHats, c_int, 0);

/// One axis, in SDL's own `-32768..32767`.
///
/// # Safety
///
/// As [`close`].
pub unsafe fn axis(joystick: Joystick, index: c_int) -> i16 {
    match api().and_then(|a| a.SDL_GetJoystickAxis) {
        Some(f) => unsafe { f(joystick, index) },
        None => 0,
    }
}

/// One button, pressed or not.
///
/// # Safety
///
/// As [`close`].
pub unsafe fn button(joystick: Joystick, index: c_int) -> bool {
    match api().and_then(|a| a.SDL_GetJoystickButton) {
        Some(f) => unsafe { f(joystick, index) },
        None => false,
    }
}

/// One hat, as SDL's bitfield: 1 up, 2 right, 4 down, 8 left.
///
/// # Safety
///
/// As [`close`].
pub unsafe fn hat(joystick: Joystick, index: c_int) -> u8 {
    match api().and_then(|a| a.SDL_GetJoystickHat) {
        Some(f) => unsafe { f(joystick, index) },
        None => 0,
    }
}

/// Send device-specific output bytes to a joystick.
///
/// # Safety
///
/// `joystick` must be open and `bytes` must be a report accepted by its SDL
/// driver.
pub unsafe fn send_effect(joystick: Joystick, bytes: &[u8]) -> bool {
    let Some(send) = api().and_then(|a| a.SDL_SendJoystickEffect) else {
        return false;
    };
    let Ok(length) = c_int::try_from(bytes.len()) else {
        return false;
    };
    // SAFETY: the caller's obligation, and the slice covers `length` bytes.
    unsafe { send(joystick, bytes.as_ptr().cast(), length) }
}

/// Start the joystick's two rumble motors for `duration_ms` milliseconds.
///
/// # Safety
///
/// `joystick` must be open. SDL accepts `0` for either motor to stop it.
pub unsafe fn rumble(
    joystick: Joystick,
    low_frequency: u16,
    high_frequency: u16,
    duration_ms: u32,
) -> bool {
    let Some(rumble) = api().and_then(|a| a.SDL_RumbleJoystick) else {
        return false;
    };
    // SAFETY: the caller guarantees that the joystick is open.
    unsafe { rumble(joystick, low_frequency, high_frequency, duration_ms) }
}

/// SDL's standard gamepad axes.
pub mod gamepad_axis {
    pub const INVALID: i32 = -1;
    pub const LEFT_X: i32 = 0;
    pub const LEFT_Y: i32 = 1;
    pub const RIGHT_X: i32 = 2;
    pub const RIGHT_Y: i32 = 3;
    pub const LEFT_TRIGGER: i32 = 4;
    pub const RIGHT_TRIGGER: i32 = 5;
    pub const COUNT: i32 = 6;
}

/// SDL's standard gamepad buttons.
pub mod gamepad_button {
    pub const INVALID: i32 = -1;
    pub const SOUTH: i32 = 0;
    pub const EAST: i32 = 1;
    pub const WEST: i32 = 2;
    pub const NORTH: i32 = 3;
    pub const BACK: i32 = 4;
    pub const GUIDE: i32 = 5;
    pub const START: i32 = 6;
    pub const LEFT_STICK: i32 = 7;
    pub const RIGHT_STICK: i32 = 8;
    pub const LEFT_SHOULDER: i32 = 9;
    pub const RIGHT_SHOULDER: i32 = 10;
    pub const DPAD_UP: i32 = 11;
    pub const DPAD_DOWN: i32 = 12;
    pub const DPAD_LEFT: i32 = 13;
    pub const DPAD_RIGHT: i32 = 14;
    pub const MISC1: i32 = 15;
    pub const RIGHT_PADDLE1: i32 = 16;
    pub const LEFT_PADDLE1: i32 = 17;
    pub const RIGHT_PADDLE2: i32 = 18;
    pub const LEFT_PADDLE2: i32 = 19;
    pub const TOUCHPAD: i32 = 20;
    pub const MISC2: i32 = 21;
    pub const MISC3: i32 = 22;
    pub const MISC4: i32 = 23;
    pub const MISC5: i32 = 24;
    pub const MISC6: i32 = 25;
    pub const COUNT: i32 = 26;
}

/// USB vendor identifiers used when SDL's gamepad metadata is interpreted.
pub mod usb_vendor {
    pub const MICROSOFT: u16 = 0x045e;
}

pub mod usb_product {
    pub const XBOX_360_CONTROLLER: u16 = 0x028e;
    pub const XBOX_SERIES_CONTROLLER: u16 = 0x0b13;
}

/// The gamepad instance ids attached right now.
pub fn gamepads() -> Vec<JoystickId> {
    let Some(api) = api() else {
        return Vec::new();
    };
    let (Some(get), Some(free)) = (api.SDL_GetGamepads, api.SDL_free) else {
        return Vec::new();
    };
    let mut count = 0;
    // SAFETY: SDL writes `count` and returns an SDL-allocated array of ids.
    unsafe {
        let ids = get(&mut count);
        if ids.is_null() || count <= 0 {
            if !ids.is_null() {
                free(ids.cast());
            }
            return Vec::new();
        }
        let out = std::slice::from_raw_parts(ids, count as usize).to_vec();
        free(ids.cast());
        out
    }
}

/// Open an SDL standard-mapped gamepad.
pub fn open_gamepad(id: JoystickId) -> Option<Gamepad> {
    let open = api()?.SDL_OpenGamepad?;
    // SAFETY: `id` came from `SDL_GetGamepads`.
    let gamepad = unsafe { open(id) };
    (!gamepad.is_null()).then_some(gamepad)
}

/// # Safety
///
/// `gamepad` must be one [`open_gamepad`] returned and not yet closed.
pub unsafe fn close_gamepad(gamepad: Gamepad) {
    if let Some(close) = api().and_then(|a| a.SDL_CloseGamepad) {
        unsafe { close(gamepad) };
    }
}

/// A gamepad's display name, or empty.
///
/// # Safety
///
/// As [`close_gamepad`].
pub unsafe fn gamepad_name(gamepad: Gamepad) -> String {
    let Some(get) = api().and_then(|a| a.SDL_GetGamepadName) else {
        return String::new();
    };
    // SAFETY: the caller's obligation; SDL owns the NUL-terminated string.
    let text = unsafe { get(gamepad) };
    if text.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

/// A gamepad's USB vendor id, or zero when SDL does not know it.
///
/// # Safety
///
/// As [`close_gamepad`].
pub unsafe fn gamepad_vendor(gamepad: Gamepad) -> u16 {
    match api().and_then(|a| a.SDL_GetGamepadVendor) {
        Some(get) => unsafe { get(gamepad) },
        None => 0,
    }
}

/// Output capabilities SDL reports for an opened gamepad.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct GamepadProperties {
    pub mono_led: bool,
    pub rgb_led: bool,
    pub player_led: bool,
    pub rumble: bool,
    pub trigger_rumble: bool,
}

/// # Safety
/// `joystick` must be an open joystick handle.
pub unsafe fn joystick_has_rumble(joystick: Joystick) -> bool {
    let Some(api) = api() else { return false };
    let (Some(properties), Some(get)) = (api.SDL_GetJoystickProperties, api.SDL_GetBooleanProperty)
    else {
        return false;
    };
    let properties = unsafe { properties(joystick) };
    properties != 0 && unsafe { get(properties, c"SDL.joystick.cap.rumble".as_ptr(), false) }
}

/// # Safety
/// `joystick` must be an open joystick handle.
pub unsafe fn joystick_connected(joystick: Joystick) -> bool {
    api()
        .and_then(|a| a.SDL_JoystickConnected)
        .is_some_and(|f| unsafe { f(joystick) })
}

/// Read the properties associated with an opened gamepad.
///
/// # Safety
///
/// `gamepad` must be one [`open_gamepad`] returned and not yet closed.
pub unsafe fn gamepad_properties(gamepad: Gamepad) -> GamepadProperties {
    let Some(api) = api() else {
        return GamepadProperties::default();
    };
    let (Some(properties), Some(boolean)) =
        (api.SDL_GetGamepadProperties, api.SDL_GetBooleanProperty)
    else {
        return GamepadProperties::default();
    };
    let properties = unsafe { properties(gamepad) };
    if properties == 0 {
        return GamepadProperties::default();
    }
    let get = |name: &CStr| {
        // SAFETY: `properties` is live and `name` is NUL-terminated.
        unsafe { boolean(properties, name.as_ptr(), false) }
    };
    GamepadProperties {
        mono_led: get(c"SDL.joystick.cap.mono_led"),
        rgb_led: get(c"SDL.joystick.cap.rgb_led"),
        player_led: get(c"SDL.joystick.cap.player_led"),
        rumble: get(c"SDL.joystick.cap.rumble"),
        trigger_rumble: get(c"SDL.joystick.cap.trigger_rumble"),
    }
}

/// Read one standard gamepad axis in SDL's `-32768..32767` range.
///
/// # Safety
///
/// As [`close_gamepad`].
pub unsafe fn gamepad_axis(gamepad: Gamepad, axis: c_int) -> i16 {
    match api().and_then(|a| a.SDL_GetGamepadAxis) {
        Some(get) => unsafe { get(gamepad, axis) },
        None => 0,
    }
}

/// Read one standard gamepad button.
///
/// # Safety
///
/// As [`close_gamepad`].
pub unsafe fn gamepad_button(gamepad: Gamepad, button: c_int) -> bool {
    match api().and_then(|a| a.SDL_GetGamepadButton) {
        Some(get) => unsafe { get(gamepad, button) },
        None => false,
    }
}

/// The desktop clipboard, as UTF-8 text.
pub mod clipboard {
    use super::*;

    pub fn set_text(text: &[u8]) -> bool {
        let Ok(text) = CString::new(text) else {
            return false;
        };
        let Some(handle) = library() else {
            return false;
        };
        // SAFETY: SDL3 declares this exact prototype.
        let Some(set) = (unsafe {
            entry::<unsafe extern "C" fn(*const c_char) -> bool>(handle, "SDL_SetClipboardText")
        }) else {
            return false;
        };
        // SAFETY: SDL copies the NUL-terminated text during the call.
        unsafe { set(text.as_ptr()) }
    }

    pub fn text() -> Option<Vec<u8>> {
        let handle = library()?;
        // SAFETY: SDL3 declares this exact prototype.
        let get = unsafe {
            entry::<unsafe extern "C" fn() -> *mut c_char>(handle, "SDL_GetClipboardText")?
        };
        let free = api()?.SDL_free?;
        // SAFETY: SDL returns either null or a NUL-terminated allocation that
        // `SDL_free` owns and accepts.
        let text = unsafe { get() };
        if text.is_null() {
            return None;
        }
        // SAFETY: as above; copy before giving the allocation back to SDL.
        let bytes = unsafe { CStr::from_ptr(text).to_bytes().to_vec() };
        unsafe { free(text.cast()) };
        Some(bytes)
    }

    /// Whether SDL currently has a text payload. This distinguishes an empty
    /// desktop clipboard from an unavailable video/clipboard backend.
    pub fn has_text() -> Option<bool> {
        let handle = library()?;
        // SAFETY: SDL3 declares this exact prototype.
        let has =
            unsafe { entry::<unsafe extern "C" fn() -> bool>(handle, "SDL_HasClipboardText")? };
        Some(unsafe { has() })
    }
}

/// The desktop's file chooser (`SDL_ShowOpenFileDialog` and friends), which
/// answers asynchronously: [`show`] returns at once and the [`Answer`] fills
/// in when SDL calls back, possibly on another thread.
pub mod dialog {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub enum Kind {
        Open { many: bool },
        Save,
        Folder { many: bool },
    }

    /// How a request ended.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Outcome {
        /// The paths chosen; never empty.
        Chosen(Vec<Vec<u8>>),
        Cancelled,
        /// SDL could not show a chooser, with its error.
        Failed(String),
    }

    pub type Answer = Arc<Mutex<Option<Outcome>>>;

    /// `SDL_DialogFileFilter`.
    #[repr(C)]
    struct Filter {
        name: *const c_char,
        pattern: *const c_char,
    }

    type Callback = unsafe extern "C" fn(*mut c_void, *const *const c_char, c_int);

    /// What SDL borrows until it calls back: the filter strings must outlive
    /// the call, since a portal backend reads them later.
    struct Request {
        answer: Answer,
        _strings: Vec<(CString, CString)>,
        filters: Vec<Filter>,
        default: Option<CString>,
    }

    unsafe extern "C" fn answered(
        userdata: *mut c_void,
        files: *const *const c_char,
        _filter: c_int,
    ) {
        // SAFETY: the box `show` leaked; SDL answers each request once.
        let request = unsafe { Box::from_raw(userdata as *mut Request) };
        // Null means an error; an empty list means the user cancelled.
        let outcome = if files.is_null() {
            Outcome::Failed(error())
        } else {
            let mut paths = Vec::new();
            let mut at = files;
            // SAFETY: a null-terminated list of C strings SDL owns for the call.
            unsafe {
                while !(*at).is_null() {
                    paths.push(CStr::from_ptr(*at).to_bytes().to_vec());
                    at = at.add(1);
                }
            }
            match paths.is_empty() {
                true => Outcome::Cancelled,
                false => Outcome::Chosen(paths),
            }
        };
        *request.answer.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
    }

    /// The filters as C strings. A pair is kept or dropped whole, so a name
    /// with a NUL in it cannot shift every later name onto the wrong pattern.
    fn pairs(filters: &[(String, String)]) -> Vec<(CString, CString)> {
        filters
            .iter()
            .filter_map(|(name, pattern)| {
                Some((
                    CString::new(name.as_str()).ok()?,
                    CString::new(pattern.as_str()).ok()?,
                ))
            })
            .collect()
    }

    /// Ask for a chooser. `filters` are `(name, pattern)` pairs, a pattern being
    /// `;`-separated extensions without dots (`"png;jpg"`, or `"*"`). `default`
    /// is a starting folder, or for a save a folder and file name.
    ///
    /// `None` only when SDL itself is missing; any later failure arrives as
    /// [`Outcome::Failed`].
    pub fn show(
        kind: Kind,
        window: Window,
        filters: &[(String, String)],
        default: Option<&[u8]>,
    ) -> Option<Answer> {
        type Open = unsafe extern "C" fn(
            Callback,
            *mut c_void,
            Window,
            *const Filter,
            c_int,
            *const c_char,
            bool,
        );
        type Save = unsafe extern "C" fn(
            Callback,
            *mut c_void,
            Window,
            *const Filter,
            c_int,
            *const c_char,
        );
        type Folder = unsafe extern "C" fn(Callback, *mut c_void, Window, *const c_char, bool);
        let handle = library()?;
        let strings = pairs(filters);
        let filters = strings
            .iter()
            .map(|(name, pattern)| Filter {
                name: name.as_ptr(),
                pattern: pattern.as_ptr(),
            })
            .collect();
        let answer = Answer::default();
        let request = Box::new(Request {
            answer: answer.clone(),
            _strings: strings,
            filters,
            default: default.and_then(|path| CString::new(path).ok()),
        });
        let location = request
            .default
            .as_ref()
            .map_or(std::ptr::null(), |p| p.as_ptr());
        let count = request.filters.len() as c_int;
        let list = match count {
            0 => std::ptr::null(),
            _ => request.filters.as_ptr(),
        };
        // SAFETY: SDL3 declares these prototypes. Each entry is resolved before
        // the request is leaked, and `answered` takes it back exactly once.
        unsafe {
            match kind {
                Kind::Open { many } => {
                    let show = entry::<Open>(handle, "SDL_ShowOpenFileDialog")?;
                    show(
                        answered,
                        Box::into_raw(request).cast(),
                        window,
                        list,
                        count,
                        location,
                        many,
                    );
                }
                Kind::Save => {
                    let show = entry::<Save>(handle, "SDL_ShowSaveFileDialog")?;
                    show(
                        answered,
                        Box::into_raw(request).cast(),
                        window,
                        list,
                        count,
                        location,
                    );
                }
                Kind::Folder { many } => {
                    let show = entry::<Folder>(handle, "SDL_ShowOpenFolderDialog")?;
                    show(
                        answered,
                        Box::into_raw(request).cast(),
                        window,
                        location,
                        many,
                    );
                }
            }
        }
        Some(answer)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_bad_filter_drops_its_whole_pair() {
            let filters = [
                ("Bad\0".to_string(), "txt".to_string()),
                ("Images".to_string(), "png;jpg".to_string()),
            ];
            let pairs = pairs(&filters);
            assert_eq!(pairs.len(), 1);
            assert_eq!(pairs[0].0.to_bytes(), b"Images");
            assert_eq!(pairs[0].1.to_bytes(), b"png;jpg");
        }
    }
}

pub mod virtual_joystick {
    use super::*;

    /// Replace the semantic mapping of a virtual device for regression tests.
    pub fn set_mapping(id: JoystickId, mapping: &str) -> bool {
        #[repr(C)]
        struct Guid {
            bytes: [u8; 16],
        }
        let Some(handle) = library() else {
            return false;
        };
        let Some(guid) = (unsafe {
            entry::<unsafe extern "C" fn(JoystickId) -> Guid>(handle, "SDL_GetJoystickGUIDForID")
        }) else {
            return false;
        };
        let Some(add) = (unsafe {
            entry::<unsafe extern "C" fn(*const c_char) -> c_int>(handle, "SDL_AddGamepadMapping")
        }) else {
            return false;
        };
        let guid = unsafe { guid(id) }
            .bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let Ok(mapping) = CString::new(format!("{guid},rosemary mapped pad,{mapping}")) else {
            return false;
        };
        unsafe { add(mapping.as_ptr()) >= 0 }
    }

    /// # Safety
    /// `joystick` must be an open virtual joystick handle.
    pub unsafe fn set_hat(joystick: Joystick, hat: c_int, value: u8) -> bool {
        let Some(handle) = library() else {
            return false;
        };
        let Some(set) = (unsafe {
            entry::<unsafe extern "C" fn(Joystick, c_int, u8) -> bool>(
                handle,
                "SDL_SetJoystickVirtualHat",
            )
        }) else {
            return false;
        };
        unsafe { set(joystick, hat, value) }
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct Desc {
        pub version: u32,
        pub kind: u16,
        pub padding: u16,
        pub vendor_id: u16,
        pub product_id: u16,
        pub naxes: u16,
        pub nbuttons: u16,
        pub nballs: u16,
        pub nhats: u16,
        pub ntouchpads: u16,
        pub nsensors: u16,
        pub padding2: [u16; 2],
        pub button_mask: u32,
        pub axis_mask: u32,
        pub name: *const c_char,
        pub touchpads: *const c_void,
        pub sensors: *const c_void,
        pub userdata: *mut c_void,
        pub update: Option<unsafe extern "C" fn(*mut c_void)>,
        pub set_player_index: Option<unsafe extern "C" fn(*mut c_void, c_int)>,
        pub rumble: Option<unsafe extern "C" fn(*mut c_void, u16, u16) -> bool>,
        pub rumble_triggers: Option<unsafe extern "C" fn(*mut c_void, u16, u16) -> bool>,
        pub set_led: Option<unsafe extern "C" fn(*mut c_void, u8, u8, u8) -> bool>,
        pub send_effect: Option<unsafe extern "C" fn(*mut c_void, *const c_void, c_int) -> bool>,
        pub set_sensors_enabled: Option<unsafe extern "C" fn(*mut c_void, bool) -> bool>,
        pub cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    }

    pub fn attach(desc: &Desc) -> Option<JoystickId> {
        let handle = library()?;
        let mut desc = Desc {
            version: size_of::<Desc>() as u32,
            ..*desc
        };
        let desc = &mut desc;
        // SAFETY: the prototype SDL3 declares for this name.
        let attach: unsafe extern "C" fn(*const Desc) -> JoystickId =
            unsafe { entry(handle, "SDL_AttachVirtualJoystick")? };
        // SAFETY: a descriptor this caller owns, which SDL copies.
        let id = unsafe { attach(desc) };
        (id != 0).then_some(id)
    }

    pub fn detach(id: JoystickId) -> bool {
        let Some(handle) = library() else {
            return false;
        };
        let Some(detach) = (unsafe {
            entry::<unsafe extern "C" fn(JoystickId) -> bool>(handle, "SDL_DetachVirtualJoystick")
        }) else {
            return false;
        };
        // SAFETY: an id `attach` returned.
        unsafe { detach(id) }
    }

    /// Set one axis of a virtual joystick.
    ///
    /// # Safety
    ///
    /// `joystick` must be an open virtual joystick.
    pub unsafe fn set_axis(joystick: Joystick, axis: c_int, value: i16) -> bool {
        let Some(handle) = library() else {
            return false;
        };
        // SAFETY: the prototype SDL3 declares.
        let Some(set) = (unsafe {
            entry::<unsafe extern "C" fn(Joystick, c_int, i16) -> bool>(
                handle,
                "SDL_SetJoystickVirtualAxis",
            )
        }) else {
            return false;
        };
        unsafe { set(joystick, axis, value) }
    }

    /// Set one button.
    ///
    /// # Safety
    ///
    /// As [`set_axis`].
    pub unsafe fn set_button(joystick: Joystick, button: c_int, down: bool) -> bool {
        let Some(handle) = library() else {
            return false;
        };
        // SAFETY: the prototype SDL3 declares.
        let Some(set) = (unsafe {
            entry::<unsafe extern "C" fn(Joystick, c_int, bool) -> bool>(
                handle,
                "SDL_SetJoystickVirtualButton",
            )
        }) else {
            return false;
        };
        unsafe { set(joystick, button, down) }
    }

    /// SDL asserts this size at compile time, so this tree asserts it too: a
    /// field out of place would attach a joystick with the wrong shape, and
    /// every assertion downstream would be about the wrong thing.
    const _: () = assert!(size_of::<Desc>() == 136 || size_of::<*const c_void>() == 4);

    /// Whether this SDL has the virtual-joystick API at all.
    pub fn available() -> bool {
        library().is_some_and(|handle| {
            // SAFETY: only the presence of the name is read.
            unsafe {
                entry::<unsafe extern "C" fn(*const Desc) -> JoystickId>(
                    handle,
                    "SDL_AttachVirtualJoystick",
                )
            }
            .is_some()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_absence_of_sdl_is_an_answer_rather_than_a_crash() {
        if !available() {
            assert!(joysticks().is_empty());
            assert!(init(init::JOYSTICK).is_err());
            assert_eq!(error(), refusal());
            return;
        }
        // Present: the joystick subsystem comes up, and enumerating finds
        // whatever is attached - which on a build machine is nothing.
        init(init::JOYSTICK).expect("the joystick subsystem");
        pump();
        let _ = joysticks();
    }

    #[test]
    fn a_virtual_joystick_attaches_and_reads_back() {
        // SDL's joystick event updates are global and compete with libtest
        // threads pumping SDL. Preserve the real state assertions in a
        // process where the test exclusively owns the SDL subsystem.
        if !crate::isolated::run_here() {
            return;
        }
        if !available() || !virtual_joystick::available() {
            return;
        }
        init(init::JOYSTICK).expect("the joystick subsystem");

        let label = c"rosemary test pad";
        let desc = virtual_joystick::Desc {
            kind: 1,
            vendor_id: 0x1234,
            product_id: 0x5678,
            naxes: 6,
            nbuttons: 15,
            nhats: 1,
            name: label.as_ptr(),
            ..Default::default()
        };
        let Some(id) = virtual_joystick::attach(&desc) else {
            panic!("SDL declined to attach a virtual joystick: {}", error());
        };
        pump();
        assert!(
            joysticks().contains(&id),
            "the attached joystick is not listed"
        );

        let joystick = open(id).expect("the joystick opens");
        // SAFETY: a joystick just opened, closed at the end of this test.
        unsafe {
            assert_eq!(name(joystick), "rosemary test pad");
            assert_eq!(vendor(joystick), 0x1234);
            assert_eq!(product(joystick), 0x5678);
            assert_eq!(axes(joystick), 6);
            assert_eq!(buttons(joystick), 15);
            assert_eq!(hats(joystick), 1);

            assert!(virtual_joystick::set_axis(joystick, 0, 12_345));
            assert!(virtual_joystick::set_button(joystick, 3, true));
            pump();
            assert_eq!(axis(joystick, 0), 12_345);
            assert!(button(joystick, 3));
            assert!(!button(joystick, 4));

            close(joystick);
        }
        virtual_joystick::detach(id);
    }
}

/// The window, the GL context, the displays, and the event queue.
///
/// A thin layer: every function here is one SDL call and a null check, because
/// the decisions belong to `crates/appkit` - what a window's coordinates mean,
/// which events become an `NSEvent`, when a context is current. What is *not*
/// thin is [`Event`](video::Event), which is decoded here: `SDL_Event` is a
/// union whose members are this crate's business to know and nobody else's.
pub mod video {
    use std::{ffi::CString, sync::LazyLock};

    use super::*;

    /// `SDL_WindowFlags`.
    pub mod flags {
        pub const FULLSCREEN: u64 = 0x0000_0001;
        pub const OPENGL: u64 = 0x0000_0002;
        pub const METAL: u64 = 0x2000_0000;
        pub const HIDDEN: u64 = 0x0000_0008;
        pub const BORDERLESS: u64 = 0x0000_0010;
        pub const RESIZABLE: u64 = 0x0000_0020;
        pub const MINIMIZED: u64 = 0x0000_0040;
        pub const MAXIMIZED: u64 = 0x0000_0080;
        pub const INPUT_FOCUS: u64 = 0x0000_0200;
        pub const HIGH_PIXEL_DENSITY: u64 = 0x0000_2000;
        pub const ALWAYS_ON_TOP: u64 = 0x0001_0000;
        pub const UTILITY: u64 = 0x0002_0000;
        pub const TOOLTIP: u64 = 0x0004_0000;
        pub const POPUP_MENU: u64 = 0x0008_0000;
        pub const VULKAN: u64 = 0x1000_0000;
        pub const TRANSPARENT: u64 = 0x4000_0000;
        pub const NOT_FOCUSABLE: u64 = 0x0000_0000_8000_0000;
    }

    /// The flags a window actually has, which is not always the set it was
    /// asked for: SDL clears `OPENGL` and sets `VULKAN` on a window converted
    /// by `SDL_Vulkan_CreateSurface`.
    pub fn window_flags(window: Window) -> u64 {
        let Some(f) = api().and_then(|a| a.SDL_GetWindowFlags) else {
            return 0;
        };
        // SAFETY: a window this crate created.
        unsafe { f(window) }
    }

    /// `SDL_GLAttr`, in declaration order - the enum has no assigned values, so
    /// the ordinal *is* the interface.
    pub mod attr {
        pub const RED_SIZE: i32 = 0;
        pub const GREEN_SIZE: i32 = 1;
        pub const BLUE_SIZE: i32 = 2;
        pub const ALPHA_SIZE: i32 = 3;
        pub const BUFFER_SIZE: i32 = 4;
        pub const DOUBLEBUFFER: i32 = 5;
        pub const DEPTH_SIZE: i32 = 6;
        pub const STENCIL_SIZE: i32 = 7;
        pub const STEREO: i32 = 12;
        pub const MULTISAMPLEBUFFERS: i32 = 13;
        pub const MULTISAMPLESAMPLES: i32 = 14;
        pub const ACCELERATED_VISUAL: i32 = 15;
        pub const CONTEXT_MAJOR_VERSION: i32 = 17;
        pub const CONTEXT_MINOR_VERSION: i32 = 18;
        pub const CONTEXT_PROFILE_MASK: i32 = 20;
    }

    /// `SDL_GLProfile`.
    pub mod profile {
        pub const CORE: i32 = 0x0001;
        pub const COMPATIBILITY: i32 = 0x0002;
    }

    /// The `SDL_EventType` values this tree acts on.
    pub mod event_type {
        pub const QUIT: u32 = 0x100;
        pub const SYSTEM_THEME_CHANGED: u32 = 0x108;
        pub const DISPLAY_DESKTOP_MODE_CHANGED: u32 = 0x155;
        pub const DISPLAY_CURRENT_MODE_CHANGED: u32 = 0x156;
        pub const WINDOW_EXPOSED: u32 = 0x204;
        pub const WINDOW_MOVED: u32 = 0x205;
        pub const WINDOW_RESIZED: u32 = 0x206;
        pub const WINDOW_PIXEL_SIZE_CHANGED: u32 = 0x207;
        pub const WINDOW_MINIMIZED: u32 = 0x209;
        pub const WINDOW_MAXIMIZED: u32 = 0x20a;
        pub const WINDOW_RESTORED: u32 = 0x20b;
        pub const WINDOW_MOUSE_ENTER: u32 = 0x20c;
        pub const WINDOW_MOUSE_LEAVE: u32 = 0x20d;
        pub const WINDOW_FOCUS_GAINED: u32 = 0x20e;
        pub const WINDOW_FOCUS_LOST: u32 = 0x20f;
        pub const WINDOW_CLOSE_REQUESTED: u32 = 0x210;
        pub const WINDOW_HIT_TEST: u32 = 0x211;
        pub const WINDOW_DISPLAY_CHANGED: u32 = 0x213;
        pub const WINDOW_DISPLAY_SCALE_CHANGED: u32 = 0x214;
        pub const WINDOW_ENTER_FULLSCREEN: u32 = 0x217;
        pub const WINDOW_LEAVE_FULLSCREEN: u32 = 0x218;
        pub const KEY_DOWN: u32 = 0x300;
        pub const KEY_UP: u32 = 0x301;
        pub const TEXT_EDITING: u32 = 0x302;
        pub const TEXT_INPUT: u32 = 0x303;
        pub const MOUSE_MOTION: u32 = 0x400;
        pub const MOUSE_BUTTON_DOWN: u32 = 0x401;
        pub const MOUSE_BUTTON_UP: u32 = 0x402;
        pub const MOUSE_WHEEL: u32 = 0x403;
    }

    /// `SDL_WindowEvent`, whose head every other event shares.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct WindowEvent {
        kind: u32,
        reserved: u32,
        timestamp: u64,
        window: u32,
        data1: i32,
        data2: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KeyboardEvent {
        kind: u32,
        reserved: u32,
        timestamp: u64,
        window: u32,
        which: u32,
        scancode: u32,
        key: u32,
        modifiers: u16,
        raw: u16,
        down: bool,
        repeat: bool,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MouseMotionEvent {
        kind: u32,
        reserved: u32,
        timestamp: u64,
        window: u32,
        which: u32,
        state: u32,
        x: f32,
        y: f32,
        xrel: f32,
        yrel: f32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MouseButtonEvent {
        kind: u32,
        reserved: u32,
        timestamp: u64,
        window: u32,
        which: u32,
        button: u8,
        down: bool,
        clicks: u8,
        padding: u8,
        x: f32,
        y: f32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MouseWheelEvent {
        kind: u32,
        reserved: u32,
        timestamp: u64,
        window: u32,
        which: u32,
        x: f32,
        y: f32,
        direction: u32,
        mouse_x: f32,
        mouse_y: f32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TextInputEvent {
        kind: u32,
        reserved: u32,
        timestamp: u64,
        window: u32,
        text: *const c_char,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TextEditingEvent {
        kind: u32,
        reserved: u32,
        timestamp: u64,
        window: u32,
        text: *const c_char,
        start: i32,
        length: i32,
    }

    // The offsets these structures put their fields at, checked against the
    // ones SDL's own headers compute. A member read at the wrong offset is a
    // coordinate or a keycode that is merely *wrong*, which is the failure this
    // whole file is arranged to avoid - and the window-event tags above were
    // three off by one when they were counted by hand rather than printed.
    const _: () = {
        assert!(size_of::<super::Event>() == 128);
        assert!(std::mem::offset_of!(KeyboardEvent, scancode) == 0x18);
        assert!(std::mem::offset_of!(KeyboardEvent, down) == 0x24);
        assert!(size_of::<KeyboardEvent>() == 0x28);
        assert!(std::mem::offset_of!(MouseButtonEvent, x) == 0x1c);
        assert!(std::mem::offset_of!(MouseMotionEvent, xrel) == 0x24);
        assert!(std::mem::offset_of!(TextInputEvent, text) == 0x18);
        assert!(std::mem::offset_of!(TextEditingEvent, start) == 0x20);
        // `SDL_MouseWheelEvent` grew `integer_x`/`integer_y` in 3.2.12, which is
        // why this one is shorter than SDL's: the head is what is read.
        assert!(std::mem::offset_of!(MouseWheelEvent, y) == 0x1c);
    };

    /// One event, decoded.
    ///
    /// Every window-relative coordinate is in SDL's own frame: y down from the
    /// top-left. Flipping is AppKit's, and doing it here would flip it twice for
    /// the one caller that wants the desktop's.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Event {
        Quit,
        /// The window's desktop position changed, including a compositor
        /// correcting one SDL had only been asked for.
        WindowMoved {
            window: u32,
        },
        WindowDisplayChanged {
            window: u32,
        },
        DisplayModeChanged {
            display: u32,
        },
        WindowResized {
            window: u32,
            width: i32,
            height: i32,
        },
        /// The drawable's pixel dimensions or scale changed without changing
        /// the window's size in desktop coordinates.
        WindowBackingChanged {
            window: u32,
        },
        WindowClosed {
            window: u32,
        },
        WindowFocus {
            window: u32,
            gained: bool,
        },
        WindowMouse {
            window: u32,
            entered: bool,
        },
        /// A press the hit test gave to the window manager.
        /// FIXME: X11 reports it, but incorrectly
        WindowHitTest {
            window: u32,
        },
        /// Minimized, maximized, restored, or in or out of fullscreen: read
        /// [`window_flags`] for which.
        WindowState {
            window: u32,
        },
        Key {
            window: u32,
            scancode: u32,
            key: u32,
            modifiers: u16,
            down: bool,
            repeat: bool,
        },
        Text {
            window: u32,
            text: String,
        },
        TextEditing {
            window: u32,
            text: String,
            start: i32,
            length: i32,
        },
        MouseMotion {
            window: u32,
            x: f32,
            y: f32,
            dx: f32,
            dy: f32,
            buttons: u32,
        },
        MouseButton {
            window: u32,
            button: u8,
            down: bool,
            clicks: u8,
            x: f32,
            y: f32,
        },
        MouseWheel {
            window: u32,
            x: f32,
            y: f32,
        },
        /// One SDL delivers that nothing here acts on, kept so a caller can see
        /// that the queue moved.
        Other(u32),
    }

    /// Read `T` out of a raw event.
    ///
    /// # Safety
    ///
    /// `T` must be the SDL structure the event's type tag names.
    unsafe fn decode<T: Copy>(raw: &super::Event) -> T {
        const _: () = assert!(size_of::<super::Event>() == 128);
        assert!(size_of::<T>() <= 128, "an event member is inside the union");
        unsafe { *(raw.bytes.as_ptr() as *const T) }
    }

    fn kind(raw: &super::Event) -> u32 {
        u32::from_ne_bytes([raw.bytes[0], raw.bytes[1], raw.bytes[2], raw.bytes[3]])
    }

    /// The next event, or `None` when the queue is empty.
    pub fn poll() -> Option<Event> {
        let poll = api()?.SDL_PollEvent?;
        let mut raw = super::Event::default();
        // SAFETY: a writable event of SDL's own size.
        if !unsafe { poll(&mut raw) } {
            return None;
        }
        Some(translate(&raw))
    }

    pub fn take_next_text() -> Option<String> {
        let peep = api()?.SDL_PeepEvents?;
        let mut raw = super::Event::default();
        // SAFETY: room for one event; `SDL_PEEKEVENT` over every type.
        if unsafe { peep(&mut raw, 1, 1, 0, u32::MAX) } != 1 || kind(&raw) != event_type::TEXT_INPUT
        {
            return None;
        }
        // SAFETY: as above, `SDL_GETEVENT`, now that it is known to be text.
        if unsafe {
            peep(
                &mut raw,
                1,
                2,
                event_type::TEXT_INPUT,
                event_type::TEXT_INPUT,
            )
        } != 1
        {
            return None;
        }
        match translate(&raw) {
            Event::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    /// The next event, waiting up to `milliseconds` for one: for a loop with
    /// nothing to do between events, which [`poll`] would spin.
    pub fn wait(milliseconds: i32) -> Option<Event> {
        let wait = api()?.SDL_WaitEventTimeout?;
        let mut raw = super::Event::default();
        // SAFETY: a writable event of SDL's own size.
        if !unsafe { wait(&mut raw, milliseconds) } {
            return None;
        }
        Some(translate(&raw))
    }

    fn translate(raw: &super::Event) -> Event {
        let raw = *raw;
        let tag = kind(&raw);
        // SAFETY: each arm names the structure SDL documents for that tag.
        unsafe {
            match tag {
                event_type::QUIT => Event::Quit,
                event_type::WINDOW_MOVED => {
                    let e: WindowEvent = decode(&raw);
                    Event::WindowMoved { window: e.window }
                }
                event_type::DISPLAY_DESKTOP_MODE_CHANGED
                | event_type::DISPLAY_CURRENT_MODE_CHANGED => {
                    // DisplayEvent and WindowEvent share their four-field
                    // header, including the display/window ID at byte 16.
                    let e: WindowEvent = decode(&raw);
                    Event::DisplayModeChanged { display: e.window }
                }
                event_type::WINDOW_DISPLAY_CHANGED => {
                    let e: WindowEvent = decode(&raw);
                    Event::WindowDisplayChanged { window: e.window }
                }
                event_type::WINDOW_RESIZED => {
                    let e: WindowEvent = decode(&raw);
                    Event::WindowResized {
                        window: e.window,
                        width: e.data1,
                        height: e.data2,
                    }
                }
                event_type::WINDOW_PIXEL_SIZE_CHANGED
                | event_type::WINDOW_DISPLAY_SCALE_CHANGED => {
                    let e: WindowEvent = decode(&raw);
                    Event::WindowBackingChanged { window: e.window }
                }
                event_type::WINDOW_CLOSE_REQUESTED => {
                    let e: WindowEvent = decode(&raw);
                    Event::WindowClosed { window: e.window }
                }
                event_type::WINDOW_HIT_TEST => {
                    let e: WindowEvent = decode(&raw);
                    Event::WindowHitTest { window: e.window }
                }
                event_type::WINDOW_FOCUS_GAINED | event_type::WINDOW_FOCUS_LOST => {
                    let e: WindowEvent = decode(&raw);
                    Event::WindowFocus {
                        window: e.window,
                        gained: tag == event_type::WINDOW_FOCUS_GAINED,
                    }
                }
                event_type::WINDOW_MOUSE_ENTER | event_type::WINDOW_MOUSE_LEAVE => {
                    let e: WindowEvent = decode(&raw);
                    Event::WindowMouse {
                        window: e.window,
                        entered: tag == event_type::WINDOW_MOUSE_ENTER,
                    }
                }
                event_type::WINDOW_MINIMIZED
                | event_type::WINDOW_MAXIMIZED
                | event_type::WINDOW_RESTORED
                | event_type::WINDOW_ENTER_FULLSCREEN
                | event_type::WINDOW_LEAVE_FULLSCREEN => {
                    let e: WindowEvent = decode(&raw);
                    Event::WindowState { window: e.window }
                }
                event_type::KEY_DOWN | event_type::KEY_UP => {
                    let e: KeyboardEvent = decode(&raw);
                    Event::Key {
                        window: e.window,
                        scancode: e.scancode,
                        key: e.key,
                        modifiers: e.modifiers,
                        down: e.down,
                        repeat: e.repeat,
                    }
                }
                event_type::TEXT_INPUT => {
                    let e: TextInputEvent = decode(&raw);
                    let text = match e.text.is_null() {
                        true => String::new(),
                        // SAFETY: SDL owns the string until the next poll, which
                        // is after this copy.
                        false => CStr::from_ptr(e.text).to_string_lossy().into_owned(),
                    };
                    Event::Text {
                        window: e.window,
                        text,
                    }
                }
                event_type::TEXT_EDITING => {
                    let e: TextEditingEvent = decode(&raw);
                    let text = match e.text.is_null() {
                        true => String::new(),
                        // SAFETY: SDL owns the string until the next poll, which
                        // is after this copy.
                        false => CStr::from_ptr(e.text).to_string_lossy().into_owned(),
                    };
                    Event::TextEditing {
                        window: e.window,
                        text,
                        start: e.start,
                        length: e.length,
                    }
                }
                event_type::MOUSE_MOTION => {
                    let e: MouseMotionEvent = decode(&raw);
                    Event::MouseMotion {
                        window: e.window,
                        x: e.x,
                        y: e.y,
                        dx: e.xrel,
                        dy: e.yrel,
                        buttons: e.state,
                    }
                }
                event_type::MOUSE_BUTTON_DOWN | event_type::MOUSE_BUTTON_UP => {
                    let e: MouseButtonEvent = decode(&raw);
                    Event::MouseButton {
                        window: e.window,
                        button: e.button,
                        down: e.down,
                        clicks: e.clicks,
                        x: e.x,
                        y: e.y,
                    }
                }
                event_type::MOUSE_WHEEL => {
                    let e: MouseWheelEvent = decode(&raw);
                    Event::MouseWheel {
                        window: e.window,
                        x: e.x,
                        y: e.y,
                    }
                }
                other => Event::Other(other),
            }
        }
    }

    /// Start the video subsystem, once.
    ///
    /// SDL refuses to create a window without it, and there is no earlier point
    /// to do it from: a program that never opens a window should not have paid
    /// for a display connection.
    pub fn init_video() -> bool {
        let mut up = VIDEO_UP.lock().unwrap_or_else(|e| e.into_inner());
        if !*up {
            *up = super::init(super::init::VIDEO).is_ok();
            if *up {
                // This thread now owns the event queue: see [`super::pump`].
                super::claim_video_thread();
            }
        }
        *up
    }

    static VIDEO_UP: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

    /// Whether the video subsystem is running.
    pub fn video_up() -> bool {
        *VIDEO_UP.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stop the video subsystem, for a caller that brought it up early and
    /// has finished with it. Hints and app metadata are read when video
    /// starts, so leaving it running would keep the early ones.
    pub fn quit_video() {
        let mut up = VIDEO_UP.lock().unwrap_or_else(|e| e.into_inner());
        if !*up {
            return;
        }
        vulkan::service_surface_requests();
        vulkan::unload_library_if_idle();
        if let Some(quit) = api().and_then(|a| a.SDL_QuitSubSystem) {
            // SAFETY: takes a flag word, for a subsystem this crate started.
            unsafe { quit(super::init::VIDEO) };
        }
        *up = false;
        super::release_video_thread();
    }

    /// Vulkan surfaces for a window SDL already owns.
    ///
    /// Since SDL 3.4.0 a window need not be created as a Vulkan one:
    /// `SDL_Vulkan_CreateSurface` converts the window in place, clearing
    /// `SDL_WINDOW_OPENGL`, unloading the GL library and loading the Vulkan
    /// one - so the flags a window was made with no longer decide how it is
    /// presented to.
    ///
    /// The conversion is one-way and it takes the GL library with it, so a
    /// caller must not ask for a surface on a window the guest is also
    /// drawing into with OpenGL.
    pub mod vulkan {
        use super::Window;
        use std::collections::HashMap;
        use std::ffi::{CStr, CString, c_void};
        use std::sync::{LazyLock, Mutex};

        /// Surface initialization is an SDL video operation. A Metal worker
        /// can request it but must never perform it on a background thread.
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        /// Owns the surface-initialization lifetime. SDL_Window cannot be
        /// freed while the detached Vulkan initializer is still using the
        /// surface created from it. The main pump defers actual destruction.
        pub struct SurfaceInitToken(Arc<AtomicBool>);
        impl Drop for SurfaceInitToken {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        #[cfg(test)]
        pub fn test_publish_window_graphics_state(window: Window) {
            publish_window_graphics_state(window);
        }

        #[cfg(test)]
        pub fn test_insert_failed_surface(
            window: Window,
            instance: u64,
            reason: &str,
            transient: bool,
        ) {
            SURFACES.lock().unwrap().requests.insert(
                (window as usize, instance),
                State::Failed(SurfaceFailure {
                    reason: reason.to_owned(),
                    transient,
                }),
            );
        }

        #[cfg(test)]
        pub fn test_insert_ready_surface(window: Window, instance: u64) {
            SURFACES
                .lock()
                .unwrap()
                .requests
                .insert((window as usize, instance), State::Ready(0));
        }

        #[cfg(test)]
        pub fn test_pending_windows() -> Vec<(usize, usize)> {
            SURFACES
                .lock()
                .unwrap()
                .pending_windows
                .iter()
                .map(|(window, tokens)| (*window, tokens.len()))
                .collect()
        }

        #[derive(Clone, Debug)]
        pub struct SurfaceFailure {
            pub reason: String,
            pub transient: bool,
        }

        pub enum PreparedSurface {
            Pending,
            Ready(u64, SurfaceInitToken),
            Failed(SurfaceFailure),
            Abandoned,
            Retired,
        }

        enum State {
            Pending(Option<CString>, u32),
            Creating,
            Ready(u64),
            Taken,
            /// Vulkan conversion succeeded, but initialization was abandoned.
            /// The SDL window-surface API remains incompatible with it.
            Abandoned,
            Failed(SurfaceFailure),
        }

        struct Surfaces {
            requests: HashMap<(usize, u64), State>,
            destruction: Vec<(u64, u64)>,
            pending_windows: Vec<(usize, Vec<Arc<AtomicBool>>)>,
            converted: std::collections::HashSet<usize>,
            // Independent of request state: a retry can replace Taken while
            // the old initializer still holds its SDL window lifetime token.
            active_initializers: HashMap<usize, Vec<Arc<AtomicBool>>>,
            // SDL_Vulkan_LoadLibrary is reference-counted. Load once per
            // video subsystem, not again on every failed surface retry.
            loaded_library: Option<Option<CString>>,
        }
        static SURFACES: LazyLock<Mutex<Surfaces>> = LazyLock::new(|| {
            Mutex::new(Surfaces {
                requests: HashMap::new(),
                destruction: Vec::new(),
                pending_windows: Vec::new(),
                converted: std::collections::HashSet::new(),
                active_initializers: HashMap::new(),
                loaded_library: None,
            })
        });

        /// SDL may set SDL_WINDOW_VULKAN before backend surface creation
        /// fails. Publish the actual SDL graphics state even on failure.
        /// This runs only on SDL's video thread, never on a Metal worker.
        fn publish_window_graphics_state(window: Window) {
            if super::window_flags(window) & super::flags::VULKAN != 0 {
                let live = super::LIVE_WINDOWS.lock().expect("live SDL windows");
                if live
                    .get(&(window as usize))
                    .is_some_and(|state| !state.retired)
                {
                    SURFACES
                        .lock()
                        .expect("SDL Vulkan surfaces")
                        .converted
                        .insert(window as usize);
                }
            }
        }

        /// A Vulkan-owned window must never be passed into SDL_GetWindowSurface.
        pub fn owns_window(window: Window) -> bool {
            SURFACES
                .lock()
                .expect("SDL Vulkan surfaces")
                .converted
                .contains(&(window as usize))
        }

        /// The worker receives a ready surface only once, after SDL created
        /// it on its video thread. Pending requests are serviced at the pump.
        pub fn acquire_prepared_surface(
            window: Window,
            instance: u64,
            library: Option<&CStr>,
        ) -> PreparedSurface {
            let key = (window as usize, instance);
            // Match destruction lock order. An identity check separated from
            // the insertion would still admit a request after teardown.
            let live = super::LIVE_WINDOWS.lock().expect("live SDL windows");
            let Some(id) = live
                .get(&key.0)
                .filter(|state| !state.retired && !state.closing)
                .map(|state| state.id)
            else {
                return PreparedSurface::Retired;
            };
            let mut queue = SURFACES.lock().expect("SDL Vulkan surfaces");
            drop(live);
            match queue.requests.get_mut(&key) {
                Some(State::Ready(surface)) => {
                    let result = *surface;
                    let done = Arc::new(AtomicBool::new(false));
                    queue.requests.insert(key, State::Taken);
                    queue
                        .active_initializers
                        .entry(window as usize)
                        .or_default()
                        .push(Arc::clone(&done));
                    PreparedSurface::Ready(result, SurfaceInitToken(done))
                }
                Some(State::Failed(reason)) => PreparedSurface::Failed(reason.clone()),
                Some(State::Abandoned) => PreparedSurface::Abandoned,
                Some(State::Pending(_, _) | State::Creating | State::Taken) => {
                    PreparedSurface::Pending
                }
                None => {
                    queue
                        .requests
                        .insert(key, State::Pending(library.map(CStr::to_owned), id));
                    drop(queue);
                    // Unit tests create windows and present on the SDL owner
                    // directly, without an AppKit event pump.
                    if super::is_video_thread() {
                        service_surface_requests();
                        return acquire_prepared_surface(window, instance, library);
                    }
                    PreparedSurface::Pending
                }
            }
        }

        /// A state still owned by SDL or an asynchronous initializer must
        /// not trigger redundant backing-store GPU uploads on each pump.
        pub fn surface_request_pending(window: Window) -> bool {
            SURFACES
                .lock()
                .expect("SDL Vulkan surfaces")
                .requests
                .iter()
                .any(|(&(key, _), state)| {
                    key == window as usize
                        && matches!(state, State::Pending(_, _) | State::Creating | State::Taken)
                })
        }

        /// An initializer failed after SDL converted the window to Vulkan.
        /// Retire its old VkSurface, then let the SDL owner create another.
        /// Conversion remains one-way: the window cannot use SDL readback.
        pub fn retry_surface(window: Window, instance: u64, library: Option<&CStr>) -> bool {
            let Some(id) = super::live_window_id(window) else {
                return false;
            };
            let mut registry = SURFACES.lock().expect("SDL Vulkan surfaces");
            let key = (window as usize, instance);
            match registry.requests.get_mut(&key) {
                Some(state @ State::Taken) | Some(state @ State::Failed(_)) => {
                    *state = State::Pending(library.map(CStr::to_owned), id);
                    true
                }
                _ => false, // Destroyed, cancelled, or already retrying.
            }
        }

        /// Refuse a permanently unsupported, never-converted SDL surface.
        /// Retain a terminal state until the window's normal destruction.
        pub fn refuse_failed_surface(window: Window, instance: u64) {
            let mut registry = SURFACES.lock().expect("SDL Vulkan surfaces");
            if matches!(
                registry.requests.get(&(window as usize, instance)),
                Some(State::Failed(_))
            ) {
                registry
                    .requests
                    .insert((window as usize, instance), State::Abandoned);
            }
        }

        pub fn abandon_surface(window: Window, instance: u64) {
            let mut registry = SURFACES.lock().expect("SDL Vulkan surfaces");
            if matches!(
                registry.requests.get(&(window as usize, instance)),
                Some(State::Taken)
            ) {
                registry
                    .requests
                    .insert((window as usize, instance), State::Abandoned);
            }
        }

        /// Called on the SDL video thread after input polling, before AppKit
        /// can submit new drawables. No queue mutex spans SDL_Vulkan_* calls.
        pub fn service_surface_requests() {
            if !super::is_video_thread() {
                return;
            }
            let (pending, destroyed) = {
                let mut q = SURFACES.lock().expect("SDL Vulkan surfaces");
                q.active_initializers.retain(|_, tokens| {
                    tokens.retain(|done| !done.load(Ordering::Acquire));
                    !tokens.is_empty()
                });
                let pending = q
                    .requests
                    .iter_mut()
                    .filter_map(|(&key, value)| match value {
                        State::Pending(library, id) => {
                            let library = library.clone();
                            let id = *id;
                            *value = State::Creating;
                            Some((key, library, id))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                (pending, std::mem::take(&mut q.destruction))
            };
            for (instance, surface) in destroyed {
                destroy_surface_now(instance, surface);
            }
            for ((window, instance), library, id) in pending {
                // SDL native deletion runs on this same video thread.
                // Reject work logically retired by a different thread before
                // calling any SDL_Window native API with its former pointer.
                if super::live_window_id(window as Window) != Some(id) {
                    let mut q = SURFACES.lock().expect("SDL Vulkan surfaces");
                    if matches!(q.requests.get(&(window, instance)), Some(State::Creating)) {
                        q.requests.remove(&(window, instance));
                    }
                    continue;
                }
                let created = create_surface(window as Window, instance, library.as_deref());
                let failure = if created.is_none() {
                    // Capture SDL_GetError on the thread that made the call.
                    // Window conversion cannot be inferred from the error.
                    let reason = super::super::error();
                    let minimized =
                        super::window_flags(window as Window) & super::flags::MINIMIZED != 0;
                    let zero_area = super::window_size_in_pixels(window as Window)
                        .is_some_and(|(w, h)| w <= 0 || h <= 0);
                    let lower = reason.to_ascii_lowercase();
                    Some(SurfaceFailure {
                        transient: minimized
                            || zero_area
                            || lower.contains("out of date")
                            || lower.contains("out_of_date")
                            || lower.contains("not ready")
                            || lower.contains("temporarily unavailable"),
                        reason,
                    })
                } else {
                    None
                };
                let mut q = SURFACES.lock().expect("SDL Vulkan surfaces");
                let accepted = match q.requests.get_mut(&(window, instance)) {
                    Some(slot @ State::Creating) => {
                        *slot = match created {
                            Some(handle) => State::Ready(handle),
                            None => State::Failed(failure.expect("SDL error captured")),
                        };
                        true
                    }
                    _ => false,
                };
                if accepted && created.is_some() {
                    q.converted.insert(window);
                }
                drop(q);
                if !accepted && let Some(surface) = created {
                    destroy_surface_now(instance, surface);
                }
            }
            // Destroy only after the Vulkan initializer has retired its
            // resources and queued SDL_Vulkan_DestroySurface on this thread.
            let retired = {
                let mut q = SURFACES.lock().expect("SDL Vulkan surfaces");
                let (done, pending): (Vec<_>, Vec<_>) = std::mem::take(&mut q.pending_windows)
                    .into_iter()
                    .partition(|(_, tokens)| {
                        tokens.iter().all(|done| done.load(Ordering::Acquire))
                    });
                q.pending_windows = pending;
                done
            };
            // Worker teardown may have queued more surface destruction after
            // the first queue drain; retire those before the SDL_Window.
            let destroyed =
                std::mem::take(&mut SURFACES.lock().expect("SDL Vulkan surfaces").destruction);
            for (instance, surface) in destroyed {
                destroy_surface_now(instance, surface);
            }
            for (window, _) in retired {
                super::destroy_window_now(window as Window);
            }
        }

        /// A WSI present and SDL's own show or hide commit the same
        /// wl_surface from different threads. Under explicit sync a present
        /// sets its acquire point before it attaches, and an SDL commit in
        /// between commits that point with no buffer - a protocol error that
        /// closes the display connection.
        static SURFACE_COMMITS: Mutex<()> = Mutex::new(());

        /// Hold across a `vkQueuePresentKHR` to a surface of an SDL window.
        pub fn surface_commit_guard() -> std::sync::MutexGuard<'static, ()> {
            SURFACE_COMMITS.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// Hold SDL_Window alive while a Metal swapchain and its worker
        /// threads retire. The video thread observes this token at the same
        /// destruction barrier used for asynchronous Vulkan initializers.
        pub fn hold_window_until(window: Window, retired: Arc<AtomicBool>) -> bool {
            hold_window_until_inner(window, retired, false)
        }

        /// Only teardown hooks may retain a frozen native window while
        /// cancelling its Metal worker. New background producers are refused.
        pub fn hold_window_for_teardown(window: Window, retired: Arc<AtomicBool>) -> bool {
            hold_window_until_inner(window, retired, true)
        }

        fn hold_window_until_inner(
            window: Window,
            retired: Arc<AtomicBool>,
            teardown: bool,
        ) -> bool {
            let live = super::LIVE_WINDOWS.lock().expect("live SDL windows");
            if !live
                .get(&(window as usize))
                .is_some_and(|state| !state.retired && (teardown || !state.closing))
            {
                return false;
            }
            SURFACES
                .lock()
                .expect("SDL Vulkan surfaces")
                .active_initializers
                .entry(window as usize)
                .or_default()
                .push(retired);
            true
        }

        /// Centralized at SDL_DestroyWindow, including callers that bypass
        /// AppKit hooks (notably independent Vulkan tests).
        /// Returns true when SDL destruction must await an active initializer.
        pub fn defer_window_destruction(window: Window) {
            SURFACES
                .lock()
                .expect("SDL Vulkan surfaces")
                .pending_windows
                .push((window as usize, Vec::new()));
        }

        /// SDL destroys popups recursively: retain the parent until every
        /// Vulkan surface initializer belonging to its descendants retires.
        pub fn forget_window_family(window: Window, family: &[Window]) -> bool {
            let keys: std::collections::HashSet<usize> =
                family.iter().map(|member| *member as usize).collect();
            let (removed, active) = {
                let mut q = SURFACES.lock().expect("SDL Vulkan surfaces");
                for key in &keys {
                    q.converted.remove(key);
                }
                let mut active: Vec<_> = keys
                    .iter()
                    .flat_map(|key| q.active_initializers.remove(key).unwrap_or_default())
                    .collect();
                // A popup can have been logically destroyed on its own
                // while its physical SDL_Window is retained by an initializer.
                // Merge that deferred deletion into the parent's barrier;
                // otherwise SDL's recursive destroy would free it early.
                q.pending_windows.retain_mut(|(pending, tokens)| {
                    if !keys.contains(pending) {
                        return true;
                    }
                    active.append(tokens);
                    false
                });
                let requests: Vec<_> = q
                    .requests
                    .keys()
                    .filter(|(key, _)| keys.contains(key))
                    .copied()
                    .collect();
                let removed = requests
                    .into_iter()
                    .filter_map(|key| q.requests.remove(&key).map(|state| (key.1, state)))
                    .collect::<Vec<_>>();
                (removed, active)
            };
            let active: Vec<_> = active
                .into_iter()
                .filter(|done| !done.load(Ordering::Acquire))
                .collect();
            for (instance, state) in removed {
                match state {
                    State::Ready(surface) => destroy_surface(instance, surface),
                    State::Taken => {}
                    _ => {}
                }
            }
            if !active.is_empty() {
                SURFACES
                    .lock()
                    .expect("SDL Vulkan surfaces")
                    .pending_windows
                    .push((window as usize, active));
                true
            } else {
                false
            }
        }

        pub fn forget_window(window: Window) -> bool {
            forget_window_family(window, &[window])
        }

        /// A Vulkan surface for `window`, as a raw `VkSurfaceKHR`.
        ///
        /// `instance` is a raw `VkInstance`, which must have been created with
        /// `VK_KHR_surface` and the platform's own surface extension enabled;
        /// SDL asks the same loader for the entry points, so the handle it
        /// returns belongs to that instance. `library` selects the implementation
        /// which owns `instance`, or SDL's default when it is `None`.
        pub fn create_surface(
            window: Window,
            instance: u64,
            library: Option<&CStr>,
        ) -> Option<u64> {
            if !super::is_video_thread() {
                return None;
            }
            let api = super::api()?;
            let create = api.SDL_Vulkan_CreateSurface?;
            // SDL load calls increment a global reference count. Retrying
            // surface creation must not repeatedly increment that count.
            {
                let mut state = SURFACES.lock().expect("SDL Vulkan surfaces");
                match &state.loaded_library {
                    Some(loaded) if loaded.as_deref() == library => {}
                    Some(_) => return None, // Cannot switch loaders mid-instance.
                    None => {
                        let load = api.SDL_Vulkan_LoadLibrary?;
                        // SAFETY: SDL copies/uses this path during the call.
                        if !unsafe { load(library.map_or(std::ptr::null(), CStr::as_ptr)) } {
                            return None;
                        }
                        state.loaded_library = Some(library.map(CStr::to_owned));
                    }
                }
            }
            let mut surface = 0u64;
            // SAFETY: a window this crate created, a live instance handle from
            // the caller, the default allocator, and an out parameter which
            // lives across the call.
            let made = unsafe {
                create(
                    window,
                    instance as *mut c_void,
                    std::ptr::null(),
                    &raw mut surface,
                )
            };
            // SDL sets SDL_WINDOW_VULKAN before its backend is asked to
            // create a VkSurface. A backend failure does not undo conversion.
            publish_window_graphics_state(window);
            if !made || surface == 0 {
                return None;
            }
            // Successful surface creation is authoritative even if a host
            // SDL build did not expose the updated flag through the getter.
            {
                let live = super::LIVE_WINDOWS.lock().expect("live SDL windows");
                if live
                    .get(&(window as usize))
                    .is_some_and(|state| !state.retired)
                {
                    SURFACES
                        .lock()
                        .expect("SDL Vulkan surfaces")
                        .converted
                        .insert(window as usize);
                }
            }
            Some(surface)
        }

        pub fn destroy_surface(instance: u64, surface: u64) {
            if !super::is_video_thread() {
                SURFACES
                    .lock()
                    .expect("SDL Vulkan surfaces")
                    .destruction
                    .push((instance, surface));
                return;
            }
            destroy_surface_now(instance, surface);
        }

        pub fn unload_library_if_idle() {
            if !super::is_video_thread() {
                return;
            }
            let mut state = SURFACES.lock().expect("SDL Vulkan surfaces");
            if !state.requests.is_empty()
                || !state.converted.is_empty()
                || !state.pending_windows.is_empty()
                || !state.destruction.is_empty()
                || !state.active_initializers.is_empty()
            {
                return;
            }
            if state.loaded_library.take().is_some()
                && let Some(unload) = super::api().and_then(|a| a.SDL_Vulkan_UnloadLibrary)
            {
                // SAFETY: all SDL Vulkan surfaces and windows have retired.
                unsafe { unload() };
            }
        }

        fn destroy_surface_now(instance: u64, surface: u64) {
            let Some(destroy) = super::api().and_then(|a| a.SDL_Vulkan_DestroySurface) else {
                return;
            };
            if surface == 0 {
                return;
            }
            // SAFETY: a surface this module made from that instance, with
            // nothing left depending on it.
            unsafe { destroy(instance as *mut c_void, surface, std::ptr::null()) };
        }
    }

    /// The window's presentable surface, described exactly as SDL3's public
    /// `SDL_Surface` is: a caller writes `pitch`-strided rows into `pixels`.
    pub struct Presentation {
        pub width: i32,
        pub height: i32,
        pub pitch: i32,
        pub format: u32,
        pub pixels: *mut u8,
    }

    /// Borrow the window's surface, write into it, and show what was written.
    ///
    /// `body` is called only when SDL has a surface for the window; its
    /// return value says whether anything was drawn and so whether the
    /// surface is worth presenting.
    ///
    /// The calling thread's GL context is kept; see [`keep_gl_context`].
    pub fn present_window(window: Window, body: impl FnOnce(&Presentation) -> bool) -> bool {
        // SDL explicitly forbids mixing SDL_GetWindowSurface with Vulkan.
        // Losing the Vulkan WSI surface does not undo the conversion.
        if !is_video_thread() || vulkan::owns_window(window) {
            return false;
        }
        // A window's store and a Metal layer read back for it present from
        // different threads, and the GL context behind a renderer's surface
        // can be current on only one of them at a time.
        static PRESENTING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _presenting = PRESENTING.lock().unwrap_or_else(|e| e.into_inner());
        keep_gl_context(|| present_window_surface(window, body))
    }

    /// Run `f`, then put back the calling thread's GL context if SDL changed
    /// it - including back to none.
    ///
    /// A window surface may be backed by a renderer of SDL's own, whose GL
    /// context SDL makes current to draw and again to destroy, and leaves
    /// current (or leaves none) afterwards. The thread's context is usually a
    /// guest's, and its next GL call would otherwise land where none of its
    /// objects exist.
    fn keep_gl_context<T>(f: impl FnOnce() -> T) -> T {
        let context = gl_current_context();
        let window = gl_current_window();
        let result = f();
        if gl_current_context() != context {
            gl_make_current(window, context);
        }
        result
    }

    fn present_window_surface(window: Window, body: impl FnOnce(&Presentation) -> bool) -> bool {
        let Some(api) = api() else {
            return false;
        };
        let (Some(get), Some(update), Some(lock), Some(unlock)) = (
            api.SDL_GetWindowSurface,
            api.SDL_UpdateWindowSurface,
            api.SDL_LockSurface,
            api.SDL_UnlockSurface,
        ) else {
            return false;
        };
        // SAFETY: a window this crate created.
        let surface = unsafe { get(window) };
        if surface.is_null() {
            return false;
        }
        // SAFETY: SDL just returned this surface and owns it until the window
        // is resized or destroyed, neither of which happens under this call.
        let described = unsafe {
            let surface = &*surface;
            Presentation {
                width: surface.w,
                height: surface.h,
                pitch: surface.pitch,
                format: surface.format,
                pixels: surface.pixels as *mut u8,
            }
        };
        if described.pixels.is_null() || described.width <= 0 || described.height <= 0 {
            return false;
        }
        // SAFETY: the surface SDL just returned.
        if !unsafe { lock(surface) } {
            return false;
        }
        let drew = body(&described);
        // SAFETY: locked immediately above.
        unsafe { unlock(surface) };
        // SAFETY: a window this crate created.
        drew && unsafe { update(window) }
    }

    /// Native addresses may be recycled after SDL_DestroyWindow. Cache the
    /// identity while the pointer is valid, so consumers can reject stale
    /// frames without calling SDL on an already destroyed window.
    #[derive(Clone, Copy)]
    struct LiveWindow {
        id: u32,
        readbacks_allowed: bool,
        parent: Option<usize>,
        // A logically retired popup may remain allocated while its Vulkan
        // worker runs; preserve parent ancestry until physical SDL deletion.
        retired: bool,
        closing: bool,
    }

    static LIVE_WINDOWS: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<usize, LiveWindow>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

    fn register_live_window(window: Window, parent: Option<Window>) {
        let id = window_id(window);
        if id != 0 {
            LIVE_WINDOWS.lock().expect("live SDL windows").insert(
                window as usize,
                LiveWindow {
                    id,
                    readbacks_allowed: true,
                    parent: parent.map(|parent| parent as usize),
                    retired: false,
                    closing: false,
                },
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn test_register_synthetic_window(window: Window) {
        LIVE_WINDOWS.lock().unwrap().insert(
            window as usize,
            LiveWindow {
                id: 0x57ab,
                readbacks_allowed: true,
                parent: None,
                retired: false,
                closing: false,
            },
        );
    }

    #[cfg(test)]
    pub(crate) fn test_unregister_synthetic_window(window: Window) {
        LIVE_WINDOWS.lock().unwrap().remove(&(window as usize));
    }

    /// Mark the native SDL family logically dead, retaining ancestry for
    /// popups whose Vulkan workers defer their actual physical destruction.
    fn retire_window_family(window: Window, reserved: bool) -> Option<Vec<Window>> {
        let root = window as usize;
        let mut live = LIVE_WINDOWS.lock().expect("live SDL windows");
        if !live
            .get(&root)
            .is_some_and(|state| !state.retired && state.closing == reserved)
        {
            return None;
        }
        let family = family_keys(&live, root);
        for key in &family {
            if let Some(state) = live.get_mut(key) {
                state.retired = true;
                state.readbacks_allowed = false;
            }
        }
        Some(family.into_iter().map(|key| key as Window).collect())
    }

    fn family_keys(live: &std::collections::HashMap<usize, LiveWindow>, root: usize) -> Vec<usize> {
        let mut family = vec![root];
        let mut index = 0;
        while index < family.len() {
            let parent = family[index];
            for (&key, state) in live.iter() {
                if state.parent == Some(parent) && !family.contains(&key) {
                    family.push(key);
                }
            }
            index += 1;
        }
        family
    }

    /// Only the SDL owner can drop native windows. Release their tombstones
    /// at the same moment SDL recursively frees the child allocations.
    fn forget_native_window_family(window: Window) {
        let mut live = LIVE_WINDOWS.lock().expect("live SDL windows");
        for key in family_keys(&live, window as usize) {
            live.remove(&key);
        }
    }

    /// Freeze family membership while AppKit notifies Metal and other owners.
    /// A concurrently creating popup cannot join after this snapshot.
    pub fn begin_window_family_destroy(window: Window) -> Vec<Window> {
        let mut live = LIVE_WINDOWS.lock().expect("live SDL windows");
        if !live
            .get(&(window as usize))
            .is_some_and(|s| !s.retired && !s.closing)
        {
            return Vec::new();
        }
        let family = family_keys(&live, window as usize);
        for key in &family {
            if let Some(state) = live.get_mut(key) {
                state.closing = true;
            }
        }
        family.into_iter().map(|key| key as Window).collect()
    }

    /// The native SDL family which recursive destruction will consume.
    /// Entries already logically retired remain for lifetime coordination,
    /// but callers should only notify owners for live entries.
    pub fn window_family(window: Window) -> Vec<Window> {
        let live = LIVE_WINDOWS.lock().expect("live SDL windows");
        if !live
            .get(&(window as usize))
            .is_some_and(|state| !state.retired)
        {
            return Vec::new();
        }
        family_keys(&live, window as usize)
            .into_iter()
            .map(|key| key as Window)
            .collect()
    }

    /// Pure registry lookup: unlike SDL_GetWindowID, safe for a stale pointer.
    pub fn live_window_id(window: Window) -> Option<u32> {
        LIVE_WINDOWS
            .lock()
            .expect("live SDL windows")
            .get(&(window as usize))
            .filter(|live| !live.retired)
            .map(|live| live.id)
    }

    /// A window may still physically exist while AppKit is tearing it down.
    /// This prevents a GPU readback from being published after its destroy hook.
    pub fn readback_window_id(window: Window) -> Option<u32> {
        LIVE_WINDOWS
            .lock()
            .expect("live SDL windows")
            .get(&(window as usize))
            .filter(|live| !live.retired && live.readbacks_allowed)
            .map(|live| live.id)
    }

    pub fn revoke_window_readbacks(window: Window) {
        if let Some(live) = LIVE_WINDOWS
            .lock()
            .expect("live SDL windows")
            .get_mut(&(window as usize))
        {
            live.readbacks_allowed = false;
        }
    }

    pub fn create_window(title: &CStr, width: i32, height: i32, flags: u64) -> Option<Window> {
        if !init_video() {
            return None;
        }
        let create = api()?.SDL_CreateWindow?;
        // SAFETY: a NUL-terminated title and SDL's own flags.
        let window = unsafe { create(title.as_ptr(), width, height, flags) };
        if window.is_null() {
            return None;
        }
        register_live_window(window, None);
        Some(window)
    }

    pub fn create_popup_window(
        parent: Window,
        offset_x: i32,
        offset_y: i32,
        width: i32,
        height: i32,
        flags: u64,
    ) -> Option<Window> {
        if !init_video() || parent.is_null() {
            return None;
        }
        let create = api()?.SDL_CreatePopupWindow?;
        let mut live = LIVE_WINDOWS.lock().expect("live SDL windows");
        if !live
            .get(&(parent as usize))
            .is_some_and(|state| !state.retired && !state.closing)
        {
            return None;
        }
        // SAFETY: the live registry prevents logical parent teardown until
        // the child has been inserted.
        let window = unsafe { create(parent, offset_x, offset_y, width, height, flags) };
        if window.is_null() {
            return None;
        }
        let id = window_id(window);
        if id != 0 {
            live.insert(
                window as usize,
                LiveWindow {
                    id,
                    readbacks_allowed: true,
                    parent: Some(parent as usize),
                    retired: false,
                    closing: false,
                },
            );
        }
        Some(window)
    }

    /// Make `window` a dialog of `parent` that blocks input to it until it
    /// closes; the compositor may also centre and attach it.
    pub fn set_window_modal_for(window: Window, parent: Window) -> bool {
        let Some(api) = api() else {
            return false;
        };
        let (Some(set_parent), Some(set_modal)) = (api.SDL_SetWindowParent, api.SDL_SetWindowModal)
        else {
            return false;
        };
        let mut live = LIVE_WINDOWS.lock().expect("live SDL windows");
        if ![parent, window].iter().all(|window| {
            live.get(&(*window as usize))
                .is_some_and(|state| !state.retired && !state.closing)
        }) {
            return false;
        }
        // Hold the live-registry lock until the child association is recorded.
        // SDL may attach the child even if SDL_SetWindowModal later fails.
        if !unsafe { set_parent(window, parent) } {
            return false;
        }
        live.get_mut(&(window as usize))
            .expect("checked live child")
            .parent = Some(parent as usize);
        drop(live);
        unsafe { set_modal(window, true) }
    }

    /// The window a popup is positioned against, or null for a toplevel.
    pub fn window_parent(window: Window) -> Window {
        match api().and_then(|a| a.SDL_GetWindowParent) {
            // SAFETY: a window this crate created.
            Some(f) => unsafe { f(window) },
            None => std::ptr::null_mut(),
        }
    }

    /// One-argument window calls that answer whether they worked.
    macro_rules! window_call {
        ($($name:ident => $entry:ident),* $(,)?) => {
            $(
                pub fn $name(window: Window) -> bool {
                    let Some(f) = api().and_then(|a| a.$entry) else {
                        return false;
                    };
                    // SAFETY: a window this crate created.
                    unsafe { f(window) }
                }
            )*
        };
    }

    /// [`window_call`]s that change what [`presentation_window`] reports.
    macro_rules! window_state_call {
        ($($name:ident => $entry:ident),* $(,)?) => {
            $(
                pub fn $name(window: Window) -> bool {
                    let Some(f) = api().and_then(|a| a.$entry) else {
                        return false;
                    };
                    // Showing and hiding commit the window's surface.
                    let commits = vulkan::surface_commit_guard();
                    // SAFETY: a window this crate created.
                    let done = unsafe { f(window) };
                    drop(commits);
                    resample_presentation_window(window);
                    done
                }
            )*
        };
    }

    window_state_call! {
        show_window => SDL_ShowWindow,
        hide_window => SDL_HideWindow,
        minimize_window => SDL_MinimizeWindow,
        maximize_window => SDL_MaximizeWindow,
        restore_window => SDL_RestoreWindow,
    }

    window_call! {
        raise_window => SDL_RaiseWindow,
        swap_window => SDL_GL_SwapWindow,
        start_text_input => SDL_StartTextInput,
        stop_text_input => SDL_StopTextInput,
        text_input_active => SDL_TextInputActive,
    }

    /// Tell the host input method where the active editor and caret are.
    ///
    /// SDL measures both in window coordinates, not drawable pixels. `cursor`
    /// is the horizontal offset from the area's left edge.
    pub fn set_text_input_area(window: Window, area: Rect, cursor: i32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetTextInputArea) else {
            return false;
        };
        // SAFETY: a window this crate created and a live rectangle for the call.
        unsafe { f(window, &area, cursor) }
    }

    /// The calling thread's GL context is kept; see [`keep_gl_context`].
    pub fn destroy_window(window: Window) {
        destroy_window_inner(window, false);
    }

    /// Finish an AppKit-initiated family destroy after its hooks have run.
    pub fn destroy_window_after_hooks(window: Window) {
        destroy_window_inner(window, true);
    }

    fn destroy_window_inner(window: Window, reserved: bool) {
        let Some(family) = retire_window_family(window, reserved) else {
            return;
        };
        let pending = vulkan::forget_window_family(window, &family);
        for member in family {
            forget_cached_window_refresh_rate(member);
        }
        vulkan::service_surface_requests();
        if !pending {
            if is_video_thread() {
                destroy_window_now(window);
            } else {
                vulkan::defer_window_destruction(window);
            }
        }
    }

    fn destroy_window_now(window: Window) {
        let Some(f) = api().and_then(|a| a.SDL_DestroyWindow) else {
            return;
        };
        forget_native_window_family(window);
        // SAFETY: the SDL owner thread has finished using this window.
        keep_gl_context(|| unsafe { f(window) });
    }

    pub fn set_window_title(window: Window, title: &CStr) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowTitle) else {
            return false;
        };
        // SAFETY: a window and a NUL-terminated title.
        unsafe { f(window, title.as_ptr()) }
    }

    /// `SDL_PIXELFORMAT_RGBA32`: four bytes per pixel, red first in memory.
    ///
    /// The number rather than the name, because SDL builds this one out of
    /// field widths in a macro and there is no symbol to bind to.
    const PIXELFORMAT_RGBA: u32 = 0x1676_2004;

    /// Give a window an icon, from `width * height` RGBA pixels.
    ///
    /// On a desktop that shows a per-window icon this is that icon; on macOS
    /// SDL routes it to the application icon, which is the Dock tile - and that
    /// is the only way to set it here, because the *process* is `rosemary` and
    /// its own bundle is not the guest's.
    ///
    /// SDL converts the surface into its own representation, so nothing here
    /// has to outlive the call.
    pub fn set_window_icon(window: Window, width: i32, height: i32, pixels: &[u8]) -> bool {
        if pixels.len() != (width as usize) * (height as usize) * 4 {
            return false;
        }
        let Some(api) = api() else {
            return false;
        };
        let (Some(create), Some(set), Some(destroy)) = (
            api.SDL_CreateSurfaceFrom,
            api.SDL_SetWindowIcon,
            api.SDL_DestroySurface,
        ) else {
            return false;
        };
        // `SDL_CreateSurfaceFrom` borrows the pixels rather than copying them,
        // so they are a local this call outlives.
        let mut pixels = pixels.to_vec();
        // SAFETY: the buffer is `width * height * 4` bytes, which is what the
        // format and pitch below describe, and it outlives the surface.
        let surface = unsafe {
            create(
                width,
                height,
                PIXELFORMAT_RGBA,
                pixels.as_mut_ptr().cast(),
                width * 4,
            )
        };
        if surface.is_null() {
            return false;
        }
        // SAFETY: a window this crate created and a surface just made.
        let ok = unsafe { set(window, surface) };
        // SAFETY: as above; SDL has taken its own copy by now.
        unsafe { destroy(surface) };
        ok
    }

    /// The small integer SDL puts in an event to name a window.
    pub fn window_id(window: Window) -> u32 {
        let Some(f) = api().and_then(|a| a.SDL_GetWindowID) else {
            return 0;
        };
        // SAFETY: a window this crate created.
        unsafe { f(window) }
    }

    /// SDL's window registry lookup does not dereference a stale Window.
    pub fn window_from_id(id: u32) -> Window {
        api()
            .and_then(|api| api.SDL_GetWindowFromID)
            .map_or(std::ptr::null_mut(), |get| unsafe { get(id) })
    }

    /// The window's title, empty where it has none.
    ///
    /// SDL owns the string and keeps it until the title is set again or the
    /// window is destroyed, so it is copied out rather than borrowed.
    pub fn window_title(window: Window) -> CString {
        let empty = || CString::new("").expect("no interior NUL");
        let Some(f) = api().and_then(|a| a.SDL_GetWindowTitle) else {
            return empty();
        };
        // SAFETY: a window this crate created.
        let title = unsafe { f(window) };
        if title.is_null() {
            return empty();
        }
        // SAFETY: SDL returns a NUL-terminated string it owns; it is copied
        // before anything can set the title again.
        unsafe { CStr::from_ptr(title) }.to_owned()
    }

    pub fn window_size(window: Window) -> Option<(i32, i32)> {
        let f = api()?.SDL_GetWindowSize?;
        let (mut w, mut h) = (0, 0);
        // SAFETY: a window and two writable ints.
        unsafe { f(window, &mut w, &mut h) }.then_some((w, h))
    }

    pub fn set_window_size(window: Window, width: i32, height: i32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowSize) else {
            return false;
        };
        // SAFETY: a window this crate created.
        let done = unsafe { f(window, width, height) };
        resample_presentation_window(window);
        done
    }

    pub fn window_size_in_pixels(window: Window) -> Option<(i32, i32)> {
        let f = api()?.SDL_GetWindowSizeInPixels?;
        let (mut w, mut h) = (0, 0);
        // SAFETY: a window and two writable ints.
        unsafe { f(window, &mut w, &mut h) }.then_some((w, h))
    }

    pub fn window_position(window: Window) -> Option<(i32, i32)> {
        let f = api()?.SDL_GetWindowPosition?;
        let (mut x, mut y) = (0, 0);
        // SAFETY: a window and two writable ints.
        unsafe { f(window, &mut x, &mut y) }.then_some((x, y))
    }

    pub fn set_window_position(window: Window, x: i32, y: i32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowPosition) else {
            return false;
        };
        // SAFETY: a window this crate created.
        let done = unsafe { f(window, x, y) };
        resample_presentation_window(window);
        done
    }

    /// `SDL_SetWindowOpacity`, where 1.0 is opaque.
    ///
    /// False when the window system will not composite a translucent window,
    /// which is a thing a caller has to be able to tell: AppKit's alpha is a
    /// property that reads back whatever was set, and this is not.
    pub fn set_window_opacity(window: Window, opacity: f32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowOpacity) else {
            return false;
        };
        // SAFETY: a window this crate created.
        unsafe { f(window, opacity) }
    }

    /// The window's opacity, or `None` when SDL cannot say - it answers a
    /// negative number for a window it has no opacity for.
    pub fn window_opacity(window: Window) -> Option<f32> {
        let f = api()?.SDL_GetWindowOpacity?;
        // SAFETY: a window this crate created.
        let opacity = unsafe { f(window) };
        (opacity >= 0.0).then_some(opacity)
    }

    pub fn set_window_fullscreen(window: Window, on: bool) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowFullscreen) else {
            return false;
        };
        // SAFETY: a window this crate created.
        let done = unsafe { f(window, on) };
        resample_presentation_window(window);
        done
    }

    /// `SDL_SetWindowBordered`: whether the window system draws a frame around
    /// the window.
    ///
    /// A guest that draws its own title bar - which is what a toolkit does for
    /// a borderless window - gets two of them without this.
    pub fn set_window_bordered(window: Window, on: bool) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowBordered) else {
            return false;
        };
        // SAFETY: a window this crate created.
        let done = unsafe { f(window, on) };
        resample_presentation_window(window);
        done
    }

    /// `SDL_SetWindowMinimumSize`, in window coordinates.
    pub fn set_window_minimum_size(window: Window, width: i32, height: i32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowMinimumSize) else {
            return false;
        };
        // SAFETY: a window this crate created.
        unsafe { f(window, width, height) }
    }

    /// `SDL_ShowWindowSystemMenu`: the window manager's own menu for the
    /// window, at a point in window coordinates.
    pub fn show_window_system_menu(window: Window, x: i32, y: i32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_ShowWindowSystemMenu) else {
            return false;
        };
        // SAFETY: a window this crate created.
        unsafe { f(window, x, y) }
    }

    /// `SDL_HitTestResult`: what a point of a borderless window is to the
    /// window manager.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum HitTestResult {
        Normal = 0,
        Draggable = 1,
        ResizeTopLeft = 2,
        ResizeTop = 3,
        ResizeTopRight = 4,
        ResizeRight = 5,
        ResizeBottomRight = 6,
        ResizeBottom = 7,
        ResizeBottomLeft = 8,
        ResizeLeft = 9,
    }

    /// `SDL_HitTest`. SDL may call it from whichever thread pumps events, and
    /// the point is in window coordinates.
    pub type HitTest = unsafe extern "C" fn(Window, *const Point, *mut c_void) -> HitTestResult;

    /// `SDL_Point`.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Point {
        pub x: c_int,
        pub y: c_int,
    }

    /// `SDL_SetWindowHitTest`: let the window manager move and resize a
    /// borderless window from regions `callback` names, or stop with `None`.
    pub fn set_window_hit_test(window: Window, callback: Option<HitTest>) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowHitTest) else {
            return false;
        };
        // SAFETY: a window this crate created and a callback that ignores its
        // data pointer.
        unsafe { f(window, callback, std::ptr::null_mut()) }
    }

    pub fn set_window_always_on_top(window: Window, on: bool) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowAlwaysOnTop) else {
            return false;
        };
        // SAFETY: a window this crate created.
        unsafe { f(window, on) }
    }

    pub fn set_window_focusable(window: Window, on: bool) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowFocusable) else {
            return false;
        };
        // SAFETY: a window this crate created.
        unsafe { f(window, on) }
    }

    pub fn set_window_resizable(window: Window, on: bool) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowResizable) else {
            return false;
        };
        // SAFETY: a window this crate created.
        unsafe { f(window, on) }
    }

    /// Backing pixels per point, which is what `-backingScaleFactor` answers.
    ///
    /// One when SDL cannot say: a scale of zero would divide a window's size to
    /// nothing, and a guest sizing a framebuffer by it would ask for none.
    pub fn window_pixel_density(window: Window) -> f32 {
        let Some(f) = api().and_then(|a| a.SDL_GetWindowPixelDensity) else {
            return 1.0;
        };
        // SAFETY: a window this crate created.
        let density = unsafe { f(window) };
        match density > 0.0 {
            true => density,
            false => 1.0,
        }
    }

    /// Open the GL library SDL will make its contexts against, once.
    ///
    /// Every `gl` entry point a guest binds has to come out of this one: a
    /// second GL library, found through the host loader, is a different driver
    /// object with its own dispatch state, and a context created here but drawn
    /// through there renders nothing or corrupts both.
    ///
    /// Implied by creating a window with `flags::OPENGL`, and called ahead of
    /// that here because a guest binds its imports long before it opens a
    /// window - `SDL_GL_GetProcAddress` answers null until this has run.
    pub fn gl_load_library() -> bool {
        static LOADED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *LOADED.get_or_init(|| {
            let Some(f) = init_video()
                .then(api)
                .flatten()
                .and_then(|a| a.SDL_GL_LoadLibrary)
            else {
                return false;
            };
            // SAFETY: a null path asks SDL for the platform default, which is
            // the library it would load for a GL window anyway.
            unsafe { f(std::ptr::null()) }
        })
    }

    pub fn gl_create_context(window: Window) -> Option<GlContext> {
        let create = api()?.SDL_GL_CreateContext?;
        // SAFETY: a window created with `flags::OPENGL`.
        let context = unsafe { create(window) };
        (!context.is_null()).then_some(context)
    }

    pub fn gl_destroy_context(context: GlContext) {
        let Some(f) = api().and_then(|a| a.SDL_GL_DestroyContext) else {
            return;
        };
        // SAFETY: a context this crate created.
        unsafe { f(context) };
    }

    /// The context current on this thread, or null.
    pub fn gl_current_context() -> GlContext {
        match api().and_then(|a| a.SDL_GL_GetCurrentContext) {
            // SAFETY: takes nothing.
            Some(f) => unsafe { f() },
            None => std::ptr::null_mut(),
        }
    }

    /// The window the current context was made current against, or null.
    pub fn gl_current_window() -> Window {
        match api().and_then(|a| a.SDL_GL_GetCurrentWindow) {
            // SAFETY: takes nothing.
            Some(f) => unsafe { f() },
            None => std::ptr::null_mut(),
        }
    }

    pub fn gl_make_current(window: Window, context: GlContext) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_GL_MakeCurrent) else {
            return false;
        };
        // SAFETY: a window and a context this crate created.
        unsafe { f(window, context) }
    }

    pub fn gl_set_attribute(attribute: i32, value: i32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_GL_SetAttribute) else {
            return false;
        };
        // SAFETY: SDL's own enumerator and an integer.
        unsafe { f(attribute, value) }
    }

    /// Put every `gl_set_attribute` back to SDL's default.
    pub fn gl_reset_attributes() {
        if let Some(f) = api().and_then(|a| a.SDL_GL_ResetAttributes) {
            // SAFETY: takes nothing.
            unsafe { f() }
        }
    }

    pub fn gl_set_swap_interval(interval: i32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_GL_SetSwapInterval) else {
            return false;
        };
        // SAFETY: an integer.
        unsafe { f(interval) }
    }

    /// The version and profile mask of the GL context current on this thread,
    /// or `None` where there is none or it is older than GL 3.0.
    ///
    /// This is what a second context has to be asked for so that the guest sees
    /// the same GL on both of its threads; see [`egl::create_shared`].
    pub fn gl_current_version() -> Option<(i32, i32, i32)> {
        const VERSION: u32 = 0x1F02;
        const CONTEXT_PROFILE_MASK: u32 = 0x9126;
        let f = gl_proc_address(c"glGetString")?;
        // SAFETY: const GLubyte *glGetString(GLenum).
        let get_string: unsafe extern "C" fn(u32) -> *const c_char =
            unsafe { std::mem::transmute(f) };
        let text = unsafe { get_string(VERSION) };
        if text.is_null() {
            return None;
        }
        // GL_MAJOR_VERSION is invalid before GL 3.0. Parse the universally
        // supported version string without consuming or introducing GL errors.
        let version = unsafe { CStr::from_ptr(text) }.to_str().ok()?;
        let mut parts = version.split_whitespace().next()?.split('.');
        let (major, minor): (i32, i32) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
        if major < 3 {
            return None;
        }
        let f = gl_proc_address(c"glGetIntegerv")?;
        // SAFETY: `void glGetIntegerv(GLenum, GLint *)`.
        let get: unsafe extern "C" fn(u32, *mut i32) = unsafe { std::mem::transmute(f) };
        let read = |name| {
            let mut value = -1;
            // SAFETY: a writable int, which is one value for each name here.
            unsafe { get(name, &mut value) };
            value
        };
        // The profile mask is GL 3.2 and later; below that it stays zero, which
        // is what "no profile was asked for" is spelled as.
        let profile = match (major, minor) {
            (3, 0..=1) => 0,
            _ => read(CONTEXT_PROFILE_MASK).max(0),
        };
        Some((major, minor, profile))
    }

    /// EGL underneath SDL's GL, for the one thing SDL's own API cannot express.
    ///
    /// AppKit lets a program send `-makeCurrentContext` to one `NSOpenGLContext`
    /// from two threads and serialise the two itself, which is how every program
    /// with a render thread hands drawing over. EGL refuses outright: a context
    /// current on one thread cannot be made current on another, and only the
    /// thread holding it can let it go - so a render thread asking for a context
    /// the main thread still holds deadlocks or fails, and neither is what the
    /// program asked for.
    ///
    /// The way out is a second context in the same *share group*: a distinct
    /// context, current on the second thread, through which every shared
    /// object (textures, buffers, shaders, programs, renderbuffers) is the
    /// same object. `eglCreateContext` takes the context to share with as an
    /// argument rather than reading it off the calling thread, which is exactly
    /// the part `SDL_GL_CreateContext` cannot do.
    pub mod egl {
        use super::*;

        const NONE: i32 = 0x3038;
        const OPENGL_API: u32 = 0x30A2;
        const CONTEXT_MAJOR_VERSION: i32 = 0x3098;
        const CONTEXT_MINOR_VERSION: i32 = 0x30FB;
        const CONTEXT_OPENGL_PROFILE_MASK: i32 = 0x30FD;

        /// The core EGL calls this module makes. Resolved by hand because SDL
        /// exposes its display and config but not the calls that take them.
        struct Egl {
            get_current_context: unsafe extern "C" fn() -> *mut c_void,
            create_context: unsafe extern "C" fn(
                *mut c_void,
                *mut c_void,
                *mut c_void,
                *const i32,
            ) -> *mut c_void,
            destroy_context: unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32,
            bind_api: unsafe extern "C" fn(u32) -> u32,
            get_error: unsafe extern "C" fn() -> i32,
        }

        /// One EGL entry point.
        ///
        /// `eglGetProcAddress` answers core entry points only from EGL 1.5, so
        /// the library itself is asked first: it is already in the process, SDL
        /// having loaded it to make the context this shares with.
        fn entry_point(name: &CStr) -> Option<*mut c_void> {
            // SAFETY: a NUL-terminated name against the default search order.
            let found = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            if !found.is_null() {
                return Some(found);
            }
            let handle = {
                static EGL: OnceLock<usize> = OnceLock::new();
                // SAFETY: a NUL-terminated soname. Never closed, as with every
                // other `dlopen` in this crate.
                *EGL.get_or_init(|| unsafe {
                    libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL)
                        as usize
                })
            };
            if handle != 0 {
                // SAFETY: a handle from `dlopen` and a NUL-terminated name.
                let found = unsafe { libc::dlsym(handle as *mut c_void, name.as_ptr()) };
                if !found.is_null() {
                    return Some(found);
                }
            }
            let f = api()?.SDL_EGL_GetProcAddress?;
            // SAFETY: a NUL-terminated name.
            let found = unsafe { f(name.as_ptr()) };
            (!found.is_null()).then_some(found)
        }

        // The target type of each transmute is the field it is assigned to,
        // declared on `Egl` a few lines up; spelling it again inside the macro
        // would be the same statement twice and only one of them checked.
        #[allow(clippy::missing_transmute_annotations)]
        fn egl() -> Option<&'static Egl> {
            static EGL: OnceLock<Option<Egl>> = OnceLock::new();
            EGL.get_or_init(|| {
                /// Each field from the entry point of the same name.
                macro_rules! resolve {
                    ($($field:ident => $name:literal),* $(,)?) => {
                        Egl {
                            // SAFETY: each field's type above is the prototype
                            // `EGL/egl.h` declares for that name.
                            $($field: unsafe {
                                std::mem::transmute(entry_point($name)?)
                            },)*
                        }
                    };
                }
                Some(resolve! {
                    get_current_context => c"eglGetCurrentContext",
                    create_context => c"eglCreateContext",
                    destroy_context => c"eglDestroyContext",
                    bind_api => c"eglBindAPI",
                    get_error => c"eglGetError",
                })
            })
            .as_ref()
        }

        /// SDL's `EGLDisplay` and `EGLConfig`, which are the video device's
        /// rather than any thread's despite the names.
        fn display_and_config() -> Option<(*mut c_void, *mut c_void)> {
            let api = api()?;
            let (display, config) = (
                api.SDL_EGL_GetCurrentDisplay?,
                api.SDL_EGL_GetCurrentConfig?,
            );
            // SAFETY: no arguments; both answer null before a GL window exists.
            let (display, config) = unsafe { (display(), config()) };
            (!display.is_null()).then_some((display, config))
        }

        /// Whether `current` - the SDL context current on this thread - is an
        /// `EGLContext`, which is what lets this module hand one back to
        /// [`gl_make_current`].
        ///
        /// SDL's EGL backend returns the `EGLContext` as the `SDL_GLContext`
        /// rather than wrapping it. That is an implementation detail, so it is
        /// checked against the thread's own EGL state instead of assumed: on any
        /// other backend the two differ and the caller is told there is no EGL
        /// path here, rather than passing a foreign pointer into SDL.
        pub fn backs(current: GlContext) -> bool {
            let (Some(egl), Some(_)) = (egl(), display_and_config()) else {
                return false;
            };
            !current.is_null() && unsafe { (egl.get_current_context)() } == current
        }

        /// A second context sharing every object with `share`, asked for at the
        /// given GL version and profile so that both threads see the same GL.
        ///
        /// `version` is `None` for a context older than GL 3.0, where there is
        /// no version to ask for and the driver's default is the only choice.
        pub fn create_shared(
            share: GlContext,
            version: Option<(i32, i32, i32)>,
        ) -> Result<GlContext, String> {
            let Some(egl) = egl() else {
                return Err("this SDL's OpenGL is not EGL-backed".into());
            };
            let Some((display, config)) = display_and_config() else {
                return Err("SDL has no EGL display".into());
            };

            let mut attributes = Vec::new();
            if let Some((major, minor, profile)) = version {
                attributes.extend_from_slice(&[
                    CONTEXT_MAJOR_VERSION,
                    major,
                    CONTEXT_MINOR_VERSION,
                    minor,
                ]);
                if profile != 0 {
                    attributes.extend_from_slice(&[CONTEXT_OPENGL_PROFILE_MASK, profile]);
                }
            }
            attributes.push(NONE);

            // The client API is per-thread state, and this thread has never
            // named one - so without this `eglCreateContext` would make an
            // OpenGL ES context, which cannot share with a desktop GL one.
            // SAFETY: an EGL enumerator.
            unsafe { (egl.bind_api)(OPENGL_API) };
            // SAFETY: SDL's own display and config, a context SDL created, and
            // an `EGL_NONE`-terminated attribute list.
            let context =
                unsafe { (egl.create_context)(display, config, share, attributes.as_ptr()) };
            if context.is_null() {
                let code = unsafe { (egl.get_error)() };
                return Err(format!("eglCreateContext failed with {code:#x}"));
            }
            Ok(context)
        }

        /// Give back a context [`create_shared`] made.
        pub fn destroy_shared(context: GlContext) {
            let (Some(egl), Some((display, _))) = (egl(), display_and_config()) else {
                return;
            };
            // SAFETY: SDL's display and a context this module created.
            unsafe { (egl.destroy_context)(display, context) };
        }
    }

    /// A GL entry point by name, which is how the `gl*` imports are satisfied.
    pub fn gl_proc_address(name: &CStr) -> Option<*mut c_void> {
        let f = api()?.SDL_GL_GetProcAddress?;
        // SAFETY: a NUL-terminated name.
        let found = unsafe { f(name.as_ptr()) };
        (!found.is_null()).then_some(found)
    }

    /// Whether the current driver context supports this extension.
    pub fn gl_extension_supported(name: &CStr) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_GL_ExtensionSupported) else {
            return false;
        };
        // SAFETY: a NUL-terminated extension name.
        unsafe { f(name.as_ptr()) }
    }

    pub fn primary_display() -> u32 {
        let Some(f) = init_video()
            .then(|| api().and_then(|a| a.SDL_GetPrimaryDisplay))
            .flatten()
        else {
            return 0;
        };
        unsafe { f() }
    }

    /// Every display, primary first - which is the order `+[NSScreen screens]`
    /// promises and the reason this is not just what SDL hands back.
    pub fn displays() -> Vec<u32> {
        let Some(api) = init_video().then(api).flatten() else {
            return Vec::new();
        };
        let (Some(get), Some(free)) = (api.SDL_GetDisplays, api.SDL_free) else {
            return Vec::new();
        };
        let mut count = 0;
        // SAFETY: a writable int; the result is SDL's to free and is freed below.
        let ids = unsafe { get(&mut count) };
        if ids.is_null() || count <= 0 {
            return Vec::new();
        }
        // SAFETY: `count` ids, as SDL just reported.
        let mut out = unsafe { std::slice::from_raw_parts(ids, count as usize) }.to_vec();
        // SAFETY: SDL allocated it.
        unsafe { free(ids as *mut c_void) };
        let primary = primary_display();
        if let Some(at) = out.iter().position(|id| *id == primary) {
            out.swap(0, at);
        }
        out
    }

    pub fn display_bounds(display: u32) -> Option<Rect> {
        let f = api()?.SDL_GetDisplayBounds?;
        let mut rect = Rect::default();
        // SAFETY: a display id and a writable rect.
        unsafe { f(display, &mut rect) }.then_some(rect)
    }

    pub fn display_usable_bounds(display: u32) -> Option<Rect> {
        let f = api()?.SDL_GetDisplayUsableBounds?;
        let mut rect = Rect::default();
        // SAFETY: a display id and a writable rect.
        unsafe { f(display, &mut rect) }.then_some(rect)
    }

    /// As [`window_pixel_density`], for a display with no window on it.
    pub fn display_content_scale(display: u32) -> f32 {
        let Some(f) = api().and_then(|a| a.SDL_GetDisplayContentScale) else {
            return 1.0;
        };
        // SAFETY: a display id.
        let scale = unsafe { f(display) };
        match scale > 0.0 {
            true => scale,
            false => 1.0,
        }
    }

    fn refresh_rate_override() -> Option<f32> {
        static RATE: LazyLock<Option<f32>> = LazyLock::new(|| {
            let value = std::env::var("ROSEMARY_SCREEN_REFRESH_RATE").ok()?;
            match value.trim().parse::<f32>() {
                Ok(rate) if rate.is_finite() && rate > 0.0 => Some(rate),
                _ => {
                    eprintln!("rosemary: invalid ROSEMARY_SCREEN_REFRESH_RATE {value} - ignoring.");
                    None
                }
            }
        });
        *RATE
    }

    /// Captured on the SDL video thread, consumed by presentation workers.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct PresentationWindow {
        pub id: u32,
        pub logical_size: (i32, i32),
        /// SDL host-window position, sampled by the video owner for workers.
        pub position: (i32, i32),
        pub pixel_size: (i32, i32),
        pub density: f32,
        pub flags: u64,
    }

    static PRESENTATION_WINDOWS: std::sync::Mutex<
        Option<std::collections::HashMap<usize, PresentationWindow>>,
    > = std::sync::Mutex::new(None);

    pub fn is_video_thread() -> bool {
        let owner = super::VIDEO_THREAD.load(std::sync::atomic::Ordering::Acquire);
        owner != 0 && owner == super::this_thread()
    }

    #[cfg(test)]
    static WINDOW_SNAPSHOT_QUERIES: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    fn sample_presentation_window(window: Window) -> Option<PresentationWindow> {
        if !is_video_thread() {
            return None;
        }
        #[cfg(test)]
        WINDOW_SNAPSHOT_QUERIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(PresentationWindow {
            id: window_id(window),
            logical_size: window_size(window)?,
            position: window_position(window)?,
            pixel_size: window_size_in_pixels(window)?,
            density: window_pixel_density(window),
            flags: window_flags(window),
        })
    }

    /// Access on the Metal threads is a cache lookup only. The main thread
    /// may publish a freshly created window before its first event pump.
    pub fn presentation_window(window: Window) -> Option<PresentationWindow> {
        let key = window as usize;
        // Reject stale native addresses before any SDL call or cache query.
        let id = live_window_id(window)?;
        if let Some(snapshot) = PRESENTATION_WINDOWS
            .lock()
            .expect("presentation windows")
            .as_ref()
            .and_then(|windows| windows.get(&key).copied())
        {
            return (snapshot.id == id).then_some(snapshot);
        }
        // Only the SDL video owner can sample native window properties.
        let snapshot = sample_presentation_window(window)?;
        if snapshot.id != id || live_window_id(window) != Some(id) {
            return None;
        }
        PRESENTATION_WINDOWS
            .lock()
            .expect("presentation windows")
            .get_or_insert_with(std::collections::HashMap::new)
            .insert(key, snapshot);
        Some(snapshot)
    }

    pub fn invalidate_presentation_window(id: u32, windows: &[Window]) {
        if !is_video_thread() {
            return;
        }
        let Some(window) = windows
            .iter()
            .copied()
            .find(|&window| window_id(window) == id)
        else {
            return;
        };
        if let Some(cache) = PRESENTATION_WINDOWS
            .lock()
            .expect("presentation windows")
            .as_mut()
        {
            cache.remove(&(window as usize));
        }
    }

    /// Take the snapshot again after this process changed the window. Not
    /// every change comes back as an event to invalidate it: a hidden window
    /// takes a new size at once and reports no resize.
    fn resample_presentation_window(window: Window) {
        let Some(snapshot) = sample_presentation_window(window) else {
            return;
        };
        PRESENTATION_WINDOWS
            .lock()
            .expect("presentation windows")
            .get_or_insert_with(std::collections::HashMap::new)
            .insert(window as usize, snapshot);
    }

    /// SDL event pump publishes geometry/flags before CPU backing stores are
    /// presented, without querying unchanged windows on every redraw.
    pub fn publish_presentation_windows(windows: &[Window]) {
        if !is_video_thread() {
            return;
        }
        let unknown: Vec<Window> = {
            let cache = PRESENTATION_WINDOWS.lock().expect("presentation windows");
            windows
                .iter()
                .copied()
                .filter(|window| {
                    !cache
                        .as_ref()
                        .is_some_and(|cache| cache.contains_key(&(*window as usize)))
                })
                .collect()
        };
        let sampled: Vec<_> = unknown
            .into_iter()
            .filter_map(|window| {
                sample_presentation_window(window).map(|value| (window as usize, value))
            })
            .collect();
        let mut cache = PRESENTATION_WINDOWS.lock().expect("presentation windows");
        let cache = cache.get_or_insert_with(std::collections::HashMap::new);
        cache.retain(|key, _| windows.iter().any(|window| *key == *window as usize));
        cache.extend(sampled);
    }

    /// Published only by the SDL video thread; the Metal worker never calls
    /// SDL_GetDisplayForWindow or SDL_GetDesktopDisplayMode. `None` is cached
    /// too, so missing display information does not trigger a query each pump.
    static WINDOW_REFRESH_RATES: std::sync::Mutex<
        Option<std::collections::HashMap<usize, Option<f32>>>,
    > = std::sync::Mutex::new(None);

    pub fn cached_window_refresh_rate(window: Window) -> Option<f32> {
        WINDOW_REFRESH_RATES
            .lock()
            .expect("window refresh rates")
            .as_ref()
            .and_then(|rates| rates.get(&(window as usize)).copied().flatten())
    }

    /// Called only by the AppKit pump on the main video thread. Query newly
    /// created windows once, not every 250ms: SDL display queries can block
    /// an entire guest frame, especially when many windows are present.
    pub fn publish_window_refresh_rates(windows: &[Window]) {
        if super::VIDEO_THREAD.load(std::sync::atomic::Ordering::Acquire) == 0 || !super::may_pump()
        {
            return;
        }
        let unknown: Vec<Window> = {
            let cache = WINDOW_REFRESH_RATES.lock().expect("window refresh rates");
            windows
                .iter()
                .copied()
                .filter(|w| {
                    !cache
                        .as_ref()
                        .is_some_and(|cache| cache.contains_key(&(*w as usize)))
                })
                .collect()
        };
        let new_rates: Vec<(usize, Option<f32>)> = unknown
            .into_iter()
            .map(|window| {
                let rate = display_for_window(window)
                    .and_then(display_refresh_rate)
                    .filter(|rate| rate.is_finite() && *rate > 0.0);
                (window as usize, rate)
            })
            .collect();
        let mut cache = WINDOW_REFRESH_RATES.lock().expect("window refresh rates");
        let cache = cache.get_or_insert_with(std::collections::HashMap::new);
        cache.retain(|key, _| windows.iter().any(|window| *key == *window as usize));
        cache.extend(new_rates);
    }

    /// Re-query display modes periodically even if the window never moves.
    /// A monitor may switch refresh rate without WINDOW_DISPLAY_CHANGED.
    pub fn refresh_window_display_modes(windows: &[Window]) {
        if !is_video_thread() {
            return;
        }
        // One main-thread SDL query per distinct display, even when many
        // windows share it. Never hold the cache mutex across SDL calls.
        let mut by_display = std::collections::HashMap::new();
        let results: Vec<_> = windows
            .iter()
            .copied()
            .map(|window| {
                let display = display_for_window(window);
                let rate = display.and_then(|id| {
                    *by_display
                        .entry(id)
                        .or_insert_with(|| display_refresh_rate(id))
                });
                (window as usize, rate)
            })
            .collect();
        let mut cache = WINDOW_REFRESH_RATES.lock().expect("window refresh rates");
        let cache = cache.get_or_insert_with(std::collections::HashMap::new);
        cache.retain(|key, _| windows.iter().any(|window| *key == *window as usize));
        cache.extend(results);
    }

    /// The SDL_WINDOW_DISPLAY_CHANGED event reaches the main thread before
    /// the next pump. Re-query only that window on its new display.
    pub fn window_display_changed(window: u32, windows: &[Window]) {
        let Some(window) = windows.iter().copied().find(|&w| window_id(w) == window) else {
            return;
        };
        WINDOW_REFRESH_RATES
            .lock()
            .expect("window refresh rates")
            .get_or_insert_with(std::collections::HashMap::new)
            .remove(&(window as usize));
    }

    pub fn forget_cached_window_refresh_rate(window: Window) {
        if let Some(cache) = PRESENTATION_WINDOWS
            .lock()
            .expect("presentation windows")
            .as_mut()
        {
            cache.remove(&(window as usize));
        }
        if let Some(cache) = WINDOW_REFRESH_RATES
            .lock()
            .expect("window refresh rates")
            .as_mut()
        {
            cache.remove(&(window as usize));
        }
    }

    #[cfg(test)]
    #[test]
    fn destroyed_window_cannot_requeue_vulkan_initialization_after_metadata_lookup() {
        if !crate::isolated::run_here() {
            return;
        }
        let window = create_window(c"late Vulkan initialization", 120, 90, 0).unwrap();
        let id = presentation_window(window).unwrap().id;
        let key = window as usize;
        let instance = 0x7251_1500u64;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            assert_eq!(presentation_window(key as Window).unwrap().id, id);
            ready_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
            assert!(matches!(
                vulkan::acquire_prepared_surface(key as Window, instance, None),
                vulkan::PreparedSurface::Retired
            ));
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        destroy_window(window);
        assert!(
            window_from_id(id).is_null(),
            "SDL still has the retired window"
        );
        continue_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(!vulkan::surface_request_pending(window));
        assert!(
            !vulkan::hold_window_until(
                window,
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))
            ),
            "a retired window accepted a new native-lifetime token"
        );
        // Would have invoked SDL_Vulkan_CreateSurface on a dangling address
        // before the generation gate was added.
        vulkan::service_surface_requests();
        assert!(!vulkan::surface_request_pending(window));

        // A request admitted before teardown must be cancelled, too.
        let window = create_window(c"pending Vulkan initialization", 120, 90, 0).unwrap();
        let key = window as usize;
        assert!(matches!(
            std::thread::spawn(move || vulkan::acquire_prepared_surface(
                key as Window,
                instance + 1,
                None
            ))
            .join()
            .unwrap(),
            vulkan::PreparedSurface::Pending
        ));
        assert!(vulkan::surface_request_pending(window));
        destroy_window(window);
        assert!(!vulkan::surface_request_pending(window));
        vulkan::service_surface_requests();
    }

    #[cfg(test)]
    #[test]
    fn frozen_popup_family_rejects_late_children_and_vulkan_initializers() {
        if !crate::isolated::run_here() {
            return;
        }
        let parent = create_window(c"SDL family reservation", 120, 90, 0).unwrap();
        let id = window_id(parent);
        assert_eq!(begin_window_family_destroy(parent), [parent]);
        assert!(
            begin_window_family_destroy(parent).is_empty(),
            "double reservation"
        );
        assert!(create_popup_window(parent, 2, 2, 20, 20, flags::TOOLTIP).is_none());
        let old = parent as usize;
        assert!(matches!(
            std::thread::spawn(move || vulkan::acquire_prepared_surface(
                old as Window,
                0xbeef,
                None
            ))
            .join()
            .unwrap(),
            vulkan::PreparedSurface::Retired
        ));
        let retired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert!(!vulkan::hold_window_until(
            parent,
            std::sync::Arc::clone(&retired)
        ));
        assert!(vulkan::hold_window_for_teardown(parent, retired));
        destroy_window(parent); // ordinary concurrent destruction cannot steal the reservation
        assert_eq!(window_from_id(id), parent);
        destroy_window_after_hooks(parent);
        vulkan::service_surface_requests();
        assert!(window_from_id(id).is_null());
        assert_eq!(live_window_id(parent), None);
    }

    #[cfg(test)]
    #[test]
    fn parent_destruction_retires_popup_identity_and_waits_for_child_surface_token() {
        if !crate::isolated::run_here() {
            return;
        }
        let parent = create_window(c"popup lifetime parent", 160, 120, 0).unwrap();
        let popup = create_popup_window(parent, 8, 8, 64, 40, flags::TOOLTIP)
            .expect("real SDL tooltip popup");
        let parent_id = window_id(parent);
        let popup_id = window_id(popup);
        assert_eq!(live_window_id(popup), Some(popup_id));
        assert_eq!(window_from_id(popup_id), popup);

        // A child initializer must retain the parent's native allocation,
        // because SDL destroys both natively in one recursive operation.
        let instance = 0x7650_7650u64;
        vulkan::test_insert_ready_surface(popup, instance);
        let popup_key = popup as usize;
        let token = match std::thread::spawn(move || {
            vulkan::acquire_prepared_surface(popup_key as Window, instance, None)
        })
        .join()
        .unwrap()
        {
            vulkan::PreparedSurface::Ready(_, token) => token,
            _ => panic!("child Vulkan initializer not ready"),
        };
        destroy_window(parent);
        assert_eq!(live_window_id(parent), None);
        assert_eq!(live_window_id(popup), None);
        assert_eq!(presentation_window(popup), None);
        assert!(!vulkan::surface_request_pending(popup));
        assert!(matches!(
            vulkan::acquire_prepared_surface(popup, instance + 1, None),
            vulkan::PreparedSurface::Retired
        ));
        assert_eq!(window_from_id(parent_id), parent);
        assert_eq!(window_from_id(popup_id), popup);
        assert!(
            vulkan::test_pending_windows()
                .iter()
                .any(|(root, tokens)| *root == parent as usize && *tokens >= 1),
            "child initializer did not defer the parent's native destruction"
        );
        drop(token);
        vulkan::service_surface_requests();
        assert!(window_from_id(parent_id).is_null());
        assert!(
            window_from_id(popup_id).is_null(),
            "SDL freed popup but left its identity registered"
        );
        assert_eq!(live_window_id(popup), None);

        // The child may already have been *logically* destroyed on its own
        // while its native allocation is held by Vulkan. Parent destruction
        // must absorb the child's existing retirement token instead of
        // recursively deleting its still-in-use SDL_Window.
        let parent = create_window(c"deferred child parent", 160, 120, 0).unwrap();
        let popup = create_popup_window(parent, 8, 8, 64, 40, flags::TOOLTIP).unwrap();
        let parent_id = window_id(parent);
        let popup_id = window_id(popup);
        let instance = 0x7650_7651u64;
        vulkan::test_insert_ready_surface(popup, instance);
        let popup_key = popup as usize;
        let token = match std::thread::spawn(move || {
            vulkan::acquire_prepared_surface(popup_key as Window, instance, None)
        })
        .join()
        .unwrap()
        {
            vulkan::PreparedSurface::Ready(_, token) => token,
            _ => panic!("child Vulkan initializer not ready"),
        };
        destroy_window(popup);
        assert_eq!(live_window_id(popup), None);
        assert_eq!(window_from_id(popup_id), popup);
        destroy_window(parent);
        assert_eq!(window_from_id(parent_id), parent);
        assert_eq!(window_from_id(popup_id), popup);
        assert!(
            vulkan::test_pending_windows()
                .iter()
                .any(|(root, count)| *root == parent as usize && *count >= 1),
            "parent did not absorb an already-deferred child initializer"
        );
        assert!(
            !vulkan::test_pending_windows()
                .iter()
                .any(|(root, _)| *root == popup as usize),
            "child must not be destroyed twice after parent destruction"
        );
        drop(token);
        vulkan::service_surface_requests();
        assert!(window_from_id(parent_id).is_null());
        assert!(window_from_id(popup_id).is_null());
        assert_eq!(live_window_id(popup), None);
    }

    #[cfg(test)]
    #[test]
    fn destroying_an_sdl_window_invalidates_flags_before_pointer_reuse() {
        if !crate::isolated::run_here() {
            return;
        }
        if !super::may_pump() {
            return;
        }
        let Some(window) = create_window(c"SDL cached Vulkan window", 160, 120, 0) else {
            return;
        };
        // Another test thread may have claimed video since `may_pump`.
        if !is_video_thread() {
            return;
        }
        let first = presentation_window(window).expect("video owner sampled window");
        assert_eq!(first.flags & flags::OPENGL, 0);
        let key = window as usize;
        destroy_window(window);
        let queried_before = WINDOW_SNAPSHOT_QUERIES.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(live_window_id(window), None);
        assert_eq!(presentation_window(window), None);
        assert_eq!(
            WINDOW_SNAPSHOT_QUERIES.load(std::sync::atomic::Ordering::Relaxed),
            queried_before,
            "stale SDL pointer triggered a native metadata query"
        );
        assert!(
            PRESENTATION_WINDOWS
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|cached| !cached.contains_key(&key)),
            "stale metadata after SDL_DestroyWindow"
        );
        // A later SDL allocation may reuse exactly the same address. Its
        // graphics flags must be sampled from the replacement, not inherited.
        let Some(opengl) = create_window(c"SDL OpenGL successor", 160, 120, flags::OPENGL) else {
            return;
        };
        let successor = presentation_window(opengl).expect("fresh SDL window metadata");
        assert_ne!(successor.flags & flags::OPENGL, 0);
        destroy_window(opengl);
    }

    #[cfg(test)]
    #[test]
    fn retry_does_not_lose_an_initializer_window_lifetime_token() {
        if !crate::isolated::run_here() {
            return;
        }
        if !super::may_pump() {
            return;
        }
        let Some(window) = create_window(c"SDL token lifetime", 64, 64, 0) else {
            return;
        };
        // Another test thread may have claimed video since `may_pump`.
        if !is_video_thread() {
            return;
        }
        let key = (window as usize, 0xfeedu64);
        vulkan::test_insert_ready_surface(window, key.1);
        let token = match vulkan::acquire_prepared_surface(window, key.1, None) {
            vulkan::PreparedSurface::Ready(_, token) => token,
            _ => panic!("fixture must be ready"),
        };
        assert!(vulkan::retry_surface(window, key.1, None));
        // destroy_window must not discard the token simply because retry
        // replaced Taken with Pending.
        destroy_window(window);
        assert!(
            vulkan::test_pending_windows()
                .iter()
                .any(|(ptr, tokens)| *ptr == key.0 && *tokens > 0)
        );
        drop(token);
        vulkan::service_surface_requests();
        assert!(
            !vulkan::test_pending_windows()
                .iter()
                .any(|(ptr, _)| *ptr == key.0)
        );
    }

    #[cfg(test)]
    #[test]
    fn a_hidden_windows_resize_reaches_its_presentation_snapshot() {
        if !crate::isolated::run_here() {
            return;
        }
        let Some(window) = create_window(c"hidden resize", 140, 90, flags::HIDDEN) else {
            return;
        };
        assert_eq!(presentation_window(window).unwrap().logical_size, (140, 90));
        assert!(set_window_size(window, 200, 120));
        let live = window_size(window).unwrap();
        let key = window as usize;
        let seen = std::thread::spawn(move || presentation_window(key as Window))
            .join()
            .unwrap()
            .expect("a published snapshot");
        assert_eq!(
            seen.logical_size, live,
            "a worker saw a size SDL no longer has"
        );
        destroy_window(window);
    }

    #[cfg(test)]
    #[test]
    fn uncached_video_queries_are_rejected_on_non_owner_threads() {
        if !super::may_pump() {
            return;
        }
        let Some(window) = create_window(c"SDL threading metadata", 140, 90, 0) else {
            return;
        };
        // Another test thread may have claimed video since `may_pump`.
        if !is_video_thread() {
            return;
        }
        let original = presentation_window(window).expect("SDL owner snapshot");
        let key = window as usize;
        let count = WINDOW_SNAPSHOT_QUERIES.load(std::sync::atomic::Ordering::Relaxed);
        let from_worker = std::thread::spawn(move || presentation_window(key as Window));
        assert_eq!(from_worker.join().expect("worker"), Some(original));
        assert_eq!(
            WINDOW_SNAPSHOT_QUERIES.load(std::sync::atomic::Ordering::Relaxed),
            count,
            "background thread must not invoke SDL video queries"
        );
        PRESENTATION_WINDOWS
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .remove(&key);
        let missing = std::thread::spawn(move || presentation_window(key as Window));
        assert_eq!(missing.join().expect("worker"), None);
        assert_eq!(
            WINDOW_SNAPSHOT_QUERIES.load(std::sync::atomic::Ordering::Relaxed),
            count
        );
        // No SDL_Vulkan_LoadLibrary/CreateSurface on a background thread,
        // even if the window itself and requested instance are invalid.
        let surface = std::thread::spawn(move || vulkan::create_surface(key as Window, 0, None));
        assert_eq!(surface.join().expect("worker"), None);
        destroy_window(window);
    }

    /// The desktop mode's refresh rate in hertz, or `None` when SDL has no
    /// mode for the display - which is every host with no video subsystem.
    ///
    /// `ROSEMARY_SCREEN_REFRESH_RATE` replaces it for every display.
    pub fn display_refresh_rate(display: u32) -> Option<f32> {
        if let Some(rate) = refresh_rate_override() {
            return Some(rate);
        }
        let f = api()?.SDL_GetDesktopDisplayMode?;
        // SAFETY: a display id; the mode belongs to SDL and outlives the read.
        let mode = unsafe { f(display) };
        if mode.is_null() {
            return None;
        }
        // SAFETY: SDL's own mode record, valid until the display changes.
        let rate = unsafe { (*mode).refresh_rate };
        (rate > 0.0).then_some(rate)
    }

    /// The desktop mode, copied out of SDL's record.
    pub fn desktop_mode(display: u32) -> Option<DisplayMode> {
        let f = api()?.SDL_GetDesktopDisplayMode?;
        // SAFETY: a display id.
        let mode = unsafe { f(display) };
        // SAFETY: SDL's own mode record, valid until the display changes.
        (!mode.is_null()).then(|| unsafe { *mode })
    }

    /// The mode `display` is showing now, copied out of SDL's record: the
    /// desktop mode unless a window made it fullscreen at another.
    pub fn current_mode(display: u32) -> Option<DisplayMode> {
        let f = api()?.SDL_GetCurrentDisplayMode?;
        // SAFETY: a display id.
        let mode = unsafe { f(display) };
        // SAFETY: SDL's own mode record, valid until the display changes.
        (!mode.is_null()).then(|| unsafe { *mode })
    }

    /// The display most of `window` is on, or `None` when SDL cannot say.
    pub fn display_for_window(window: Window) -> Option<u32> {
        let f = api()?.SDL_GetDisplayForWindow?;
        // SAFETY: a window this crate created.
        let display = unsafe { f(window) };
        (display != 0).then_some(display)
    }

    /// The name SDL gives `display`: on Wayland and X11, usually the host's
    /// name for the connector, such as `DP-1`.
    pub fn display_name(display: u32) -> Option<String> {
        let f = api()?.SDL_GetDisplayName?;
        // SAFETY: a display id.
        let name = unsafe { f(display) };
        // SAFETY: SDL's own NUL-terminated string, copied at once.
        (!name.is_null()).then(|| {
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        })
    }

    /// Every fullscreen mode SDL lists for `display`, largest first.
    pub fn fullscreen_modes(display: u32) -> Vec<DisplayMode> {
        let Some(api) = api() else {
            return Vec::new();
        };
        let (Some(get), Some(free)) = (api.SDL_GetFullscreenDisplayModes, api.SDL_free) else {
            return Vec::new();
        };
        let mut count = 0;
        // SAFETY: a display id and a writable int; the array is SDL's to free.
        let list = unsafe { get(display, &mut count) };
        if list.is_null() {
            return Vec::new();
        }
        // SAFETY: `count` non-null mode pointers, as SDL just reported.
        let out = unsafe { std::slice::from_raw_parts(list, count.max(0) as usize) }
            .iter()
            .map(|m| unsafe { **m })
            .collect();
        // SAFETY: SDL allocated it, modes included, as one block.
        unsafe { free(list as *mut c_void) };
        out
    }

    pub fn set_cursor_visible(visible: bool) -> bool {
        let Some(api) = api() else {
            return false;
        };
        let f = match visible {
            true => api.SDL_ShowCursor,
            false => api.SDL_HideCursor,
        };
        f.is_some_and(|f| unsafe { f() })
    }

    /// Make a cursor out of `width * height` RGBA pixels, with its point at
    /// `(hot_x, hot_y)` in those pixels.
    ///
    /// SDL converts the surface into its own representation, so the pixels do
    /// not have to outlive the call - the cursor does, and belongs to the
    /// caller until [`destroy_cursor`].
    pub fn create_color_cursor(
        width: i32,
        height: i32,
        pixels: &[u8],
        hot_x: i32,
        hot_y: i32,
    ) -> Option<Cursor> {
        if pixels.len() != (width as usize) * (height as usize) * 4 {
            return None;
        }
        let api = api()?;
        let (create, destroy) = (api.SDL_CreateColorCursor?, api.SDL_DestroySurface?);
        let from = api.SDL_CreateSurfaceFrom?;
        // Borrowed rather than copied, as in `set_window_icon`.
        let mut pixels = pixels.to_vec();
        // SAFETY: the buffer is `width * height * 4` bytes, which is what the
        // format and pitch describe, and it outlives the surface.
        let surface = unsafe {
            from(
                width,
                height,
                PIXELFORMAT_RGBA,
                pixels.as_mut_ptr().cast(),
                width * 4,
            )
        };
        if surface.is_null() {
            return None;
        }
        // SAFETY: a surface just made, and a point SDL clamps itself.
        let cursor = unsafe { create(surface, hot_x, hot_y) };
        // SAFETY: as above; SDL has taken its own copy by now.
        unsafe { destroy(surface) };
        (!cursor.is_null()).then_some(cursor)
    }

    /// `SDL_SystemCursor`, for [`create_system_cursor`].
    pub mod system_cursor {
        use std::ffi::c_int;

        pub const TEXT: c_int = 1;
        pub const CROSSHAIR: c_int = 3;
        pub const NWSE_RESIZE: c_int = 5;
        pub const NESW_RESIZE: c_int = 6;
        pub const EW_RESIZE: c_int = 7;
        pub const NS_RESIZE: c_int = 8;
        pub const NOT_ALLOWED: c_int = 10;
        pub const POINTER: c_int = 11;
        pub const NW_RESIZE: c_int = 12;
        pub const N_RESIZE: c_int = 13;
        pub const NE_RESIZE: c_int = 14;
        pub const E_RESIZE: c_int = 15;
        pub const SE_RESIZE: c_int = 16;
        pub const S_RESIZE: c_int = 17;
        pub const SW_RESIZE: c_int = 18;
        pub const W_RESIZE: c_int = 19;
    }

    /// One of the desktop's own cursors, by its `SDL_SystemCursor` value. The
    /// cursor belongs to the caller until [`destroy_cursor`].
    pub fn create_system_cursor(id: c_int) -> Option<Cursor> {
        let create = api()?.SDL_CreateSystemCursor?;
        // SAFETY: SDL rejects an id outside its enumeration with null.
        let cursor = unsafe { create(id) };
        (!cursor.is_null()).then_some(cursor)
    }

    /// Show `cursor`, or the desktop's own when it is null.
    pub fn set_cursor(cursor: Cursor) -> bool {
        let Some(api) = api() else {
            return false;
        };
        let Some(set) = api.SDL_SetCursor else {
            return false;
        };
        // Handing SDL the null through would redraw the cursor it already has
        // rather than restoring the desktop's, which is the opposite of what a
        // program setting `+arrowCursor` back asked for.
        let cursor = match cursor.is_null() {
            true => match api.SDL_GetDefaultCursor.map(|f| unsafe { f() }) {
                Some(default) if !default.is_null() => default,
                _ => return false,
            },
            false => cursor,
        };
        // SAFETY: a cursor from SDL, either made here or its own default.
        unsafe { set(cursor) }
    }

    /// # Safety
    ///
    /// `cursor` must come from [`create_color_cursor`] and not be destroyed
    /// twice.
    pub unsafe fn destroy_cursor(cursor: Cursor) {
        let Some(f) = api().and_then(|a| a.SDL_DestroyCursor) else {
            return;
        };
        unsafe { f(cursor) };
    }

    /// Which modifier keys are held right now, as `SDL_Keymod` bits.
    pub fn keyboard_modifiers() -> u16 {
        api()
            .and_then(|a| a.SDL_GetModState)
            .map_or(0, |f| unsafe { f() })
    }

    /// The keycode the current layout produces for `scancode` with `modifiers`
    /// held, or `None` when SDL cannot say.
    pub fn key_from_scancode(scancode: u32, modifiers: u16) -> Option<u32> {
        let f = api()?.SDL_GetKeyFromScancode?;
        // Not `key_event`: that asks for what a key event would carry, which
        // SDL leaves unshifted by default, so Shift-/ would still be "/".
        // SAFETY: plain values in, a keycode out.
        Some(unsafe { f(scancode, modifiers, false) })
    }

    /// The pointer in desktop coordinates, and which buttons are down.
    pub fn global_mouse_state() -> Option<(f32, f32, u32)> {
        let f = api()?.SDL_GetGlobalMouseState?;
        let (mut x, mut y) = (0.0, 0.0);
        // SAFETY: two writable floats.
        let buttons = unsafe { f(&mut x, &mut y) };
        Some((x, y, buttons))
    }

    /// The window whose coordinate system `mouse_state` uses. Main thread only.
    pub fn mouse_focus() -> Option<Window> {
        let f = api()?.SDL_GetMouseFocus?;
        // SAFETY: SDL owns the returned window; this does not transfer ownership.
        let window = unsafe { f() };
        (!window.is_null()).then_some(window)
    }

    /// The pointer in the focused window's coordinates, and held buttons.
    pub fn mouse_state() -> Option<(f32, f32, u32)> {
        let f = api()?.SDL_GetMouseState?;
        let (mut x, mut y) = (0.0, 0.0);
        // SAFETY: two writable floats.
        let buttons = unsafe { f(&mut x, &mut y) };
        Some((x, y, buttons))
    }

    pub fn warp_mouse_global(x: f32, y: f32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_WarpMouseGlobal) else {
            return false;
        };
        // SAFETY: two coordinates.
        unsafe { f(x, y) }
    }

    /// Move the cursor to a window-relative point.
    ///
    /// Unlike the global spelling, SDL exposes no result, so `false` means
    /// only that the host SDL does not publish the function.
    pub fn warp_mouse_in_window(window: Window, x: f32, y: f32) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_WarpMouseInWindow) else {
            return false;
        };
        // SAFETY: a window from SDL and two coordinates.
        unsafe { f(window, x, y) };
        true
    }

    /// `SDL_SetHint`, for the hints this tree has to override rather than
    /// leave at SDL's default. False on a host whose SDL does not have it.
    pub fn set_hint(name: &CStr, value: &CStr) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetHint) else {
            return false;
        };
        // SAFETY: two NUL-terminated strings.
        unsafe { f(name.as_ptr(), value.as_ptr()) }
    }

    /// Whether SDL's video driver is Wayland's. False until video has
    /// started, as SDL has no driver to name before then.
    pub fn is_wayland() -> bool {
        is_video_driver(c"wayland")
    }

    /// Whether SDL's video driver is X11's. Its window hit-test events
    /// report left-button presses rather than arbitrary hit tests.
    pub fn is_x11() -> bool {
        is_video_driver(c"x11")
    }

    fn is_video_driver(expected: &CStr) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_GetCurrentVideoDriver) else {
            return false;
        };
        // SAFETY: takes nothing; the answer is null or a static string.
        let name = unsafe { f() };
        // SAFETY: a NUL-terminated string SDL keeps for the process.
        !name.is_null() && unsafe { CStr::from_ptr(name) } == expected
    }

    /// X11's live display and native window handle. Borrowed from SDL; do
    /// not close the display, and finish host requests before destroying SDL.
    #[cfg(target_os = "linux")]
    pub(crate) fn x11_native_window(window: Window) -> Option<(*mut c_void, u64)> {
        if !is_x11() {
            return None;
        }
        let api = api()?;
        let (props, pointer, number) = (
            api.SDL_GetWindowProperties?,
            api.SDL_GetPointerProperty?,
            api.SDL_GetNumberProperty?,
        );
        let properties = unsafe { props(window) };
        let display = unsafe {
            pointer(
                properties,
                c"SDL.window.x11.display".as_ptr(),
                std::ptr::null_mut(),
            )
        };
        let handle = unsafe { number(properties, c"SDL.window.x11.window".as_ptr(), 0) };
        (!display.is_null() && handle != 0).then_some((display, handle as u64))
    }

    /// A window's `wl_display` and `wl_surface`, on Wayland.
    pub fn wayland_surface(window: Window) -> Option<(*mut c_void, *mut c_void)> {
        let api = api()?;
        let (props, get) = (api.SDL_GetWindowProperties?, api.SDL_GetPointerProperty?);
        // SAFETY: a window from SDL, and property names SDL documents.
        let (display, surface) = unsafe {
            let id = props(window);
            (
                get(
                    id,
                    c"SDL.window.wayland.display".as_ptr(),
                    std::ptr::null_mut(),
                ),
                get(
                    id,
                    c"SDL.window.wayland.surface".as_ptr(),
                    std::ptr::null_mut(),
                ),
            )
        };
        (!display.is_null() && !surface.is_null()).then_some((display, surface))
    }

    /// `SDL_SystemTheme`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SystemTheme {
        Unknown,
        Light,
        Dark,
    }

    /// The desktop's light or dark preference. SDL keeps it current from the
    /// platform (the settings portal on Linux), so reading it is cheap.
    pub fn system_theme() -> SystemTheme {
        let Some(f) = api().and_then(|a| a.SDL_GetSystemTheme) else {
            return SystemTheme::Unknown;
        };
        // SAFETY: takes nothing and returns an enum value.
        match unsafe { f() } {
            1 => SystemTheme::Light,
            2 => SystemTheme::Dark,
            _ => SystemTheme::Unknown,
        }
    }

    /// `SDL_HINT_MOUSE_RELATIVE_CURSOR_VISIBLE`: whether the pointer stays
    /// visible while relative mouse mode is on. SDL hides it by default.
    pub const HINT_RELATIVE_CURSOR_VISIBLE: &CStr = c"SDL_MOUSE_RELATIVE_CURSOR_VISIBLE";

    /// `SDL_HINT_VIDEO_WAYLAND_ALLOW_LIBDECOR`. libdecor's GTK plugin loads
    /// GTK 3, which cannot share a process with the GTK 4 WebKit uses, so a
    /// runtime drawing its own decorations turns it off before video starts.
    pub const HINT_WAYLAND_ALLOW_LIBDECOR: &CStr = c"SDL_VIDEO_WAYLAND_ALLOW_LIBDECOR";

    /// `SDL_HINT_VIDEO_WAYLAND_PREFER_LIBDECOR`.
    pub const HINT_WAYLAND_PREFER_LIBDECOR: &CStr = c"SDL_VIDEO_WAYLAND_PREFER_LIBDECOR";

    /// Hold the pointer in place over `window` and report motion only as
    /// deltas, or stop doing so.
    ///
    /// False when this build of SDL is older than the per-window spelling of
    /// the call, which is a host too old for the mode rather than a failure to
    /// enter it.
    pub fn set_relative_mouse_mode(window: Window, enabled: bool) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowRelativeMouseMode) else {
            return false;
        };
        // SAFETY: a window from SDL.
        unsafe { f(window, enabled) }
    }

    /// Keep the pointer inside `window` while it has focus, still moving and
    /// still reporting positions, or let it go.
    pub fn set_mouse_grab(window: Window, grabbed: bool) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowMouseGrab) else {
            return false;
        };
        // SAFETY: a window from SDL.
        unsafe { f(window, grabbed) }
    }

    /// Keep the pointer inside `rect` of `window` while it has focus, or
    /// anywhere in the window for `None`. In window coordinates.
    pub fn set_mouse_rect(window: Window, rect: Option<Rect>) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetWindowMouseRect) else {
            return false;
        };
        let ptr = rect.as_ref().map_or(std::ptr::null(), |r| r as *const Rect);
        // SAFETY: a window from SDL, and null or a rectangle SDL copies.
        unsafe { f(window, ptr) }
    }

    /// The current pointer-confinement rectangle, if any. SDL owns the
    /// returned memory, so copy the rectangle before returning it to a caller.
    pub fn get_mouse_rect(window: Window) -> Option<Rect> {
        let f = api()?.SDL_GetWindowMouseRect?;
        // SAFETY: this is a valid SDL window, and the returned pointer, if
        // non-null, refers to SDL-owned memory valid through this call.
        unsafe { f(window).as_ref().copied() }
    }

    /// Whether `window` is holding the pointer, asked of SDL rather than
    /// remembered: SDL drops the mode itself when a window loses focus.
    pub fn relative_mouse_mode(window: Window) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_GetWindowRelativeMouseMode) else {
            return false;
        };
        // SAFETY: a window from SDL.
        unsafe { f(window) }
    }
}

/// Playback and recording, as much of them as an `AudioUnit` and the HAL need.
///
/// The pull model is SDL's too: a stream opened with a callback asks for more
/// bytes when the device is short of them, which is what an `AURenderCallback`
/// is called for as well. So there is no buffering here beyond SDL's own.
pub mod audio {
    use super::*;

    /// `SDL_AudioFormat`.
    pub mod format {
        /// `SDL_AUDIO_U8`.
        pub const U8: u32 = 0x0008;
        /// `SDL_AUDIO_S8`.
        pub const S8: u32 = 0x8008;
        /// `SDL_AUDIO_S16LE`.
        pub const S16: u32 = 0x8010;
        /// `SDL_AUDIO_S16BE`.
        pub const S16BE: u32 = 0x9010;
        /// `SDL_AUDIO_S32LE`.
        pub const S32: u32 = 0x8020;
        /// `SDL_AUDIO_S32BE`.
        pub const S32BE: u32 = 0x9020;
        /// `SDL_AUDIO_F32LE`.
        pub const F32: u32 = 0x8120;
        /// `SDL_AUDIO_F32BE`.
        pub const F32BE: u32 = 0x9120;
    }

    /// `SDL_AUDIO_DEVICE_DEFAULT_PLAYBACK`.
    const DEFAULT_PLAYBACK: u32 = 0xFFFF_FFFF;
    /// `SDL_AUDIO_DEVICE_DEFAULT_RECORDING`.
    const DEFAULT_RECORDING: u32 = 0xFFFF_FFFE;

    /// Start the audio subsystem, once - see `video::init_video` for why this is
    /// not done at load.
    fn init_audio() -> bool {
        static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *READY.get_or_init(|| super::init(super::init::AUDIO).is_ok())
    }

    /// Open the default playback device, converting from `spec` to whatever it
    /// actually runs at.
    ///
    /// The stream starts *paused*: an `AudioUnit` is initialized before it is
    /// started, and a device that ran between the two would call back before the
    /// guest said to.
    ///
    /// # Safety
    ///
    /// `callback` is called on SDL's audio thread with `userdata`, which must
    /// outlive the stream.
    pub unsafe fn open_playback(
        spec: AudioSpec,
        callback: AudioStreamCallback,
        userdata: *mut c_void,
    ) -> Option<AudioStream> {
        // SAFETY: the caller's obligation.
        unsafe { open_device(DEFAULT_PLAYBACK, spec, callback, userdata) }
    }

    /// `SDL_HINT_AUDIO_DEVICE_SAMPLE_FRAMES`.
    const HINT_SAMPLE_FRAMES: &std::ffi::CStr = c"SDL_AUDIO_DEVICE_SAMPLE_FRAMES";

    /// As [`open_recording`], with the device running `frames` at a time. The
    /// hint is global, so it is reset after the open; an environment variable
    /// still overrides it.
    ///
    /// # Safety
    ///
    /// As [`open_playback`].
    pub unsafe fn open_recording_sized(
        spec: AudioSpec,
        frames: u32,
        callback: AudioStreamCallback,
        userdata: *mut c_void,
    ) -> Option<AudioStream> {
        if !init_audio() {
            return None;
        }
        let value = std::ffi::CString::new(frames.to_string()).ok()?;
        super::video::set_hint(HINT_SAMPLE_FRAMES, &value);
        // SAFETY: the caller's obligation.
        let stream = unsafe { open_recording(spec, callback, userdata) };
        if let Some(reset) = api().and_then(|a| a.SDL_ResetHint) {
            // SAFETY: a NUL-terminated name.
            unsafe { reset(HINT_SAMPLE_FRAMES.as_ptr()) };
        }
        stream
    }

    /// Open the default recording device, converting from whatever it runs at
    /// to `spec`. Paused, as [`open_playback`]'s is.
    ///
    /// The callback is told when captured bytes have arrived in the stream,
    /// rather than asked for more.
    ///
    /// # Safety
    ///
    /// As [`open_playback`].
    pub unsafe fn open_recording(
        spec: AudioSpec,
        callback: AudioStreamCallback,
        userdata: *mut c_void,
    ) -> Option<AudioStream> {
        // SAFETY: the caller's obligation.
        unsafe { open_device(DEFAULT_RECORDING, spec, callback, userdata) }
    }

    /// # Safety
    ///
    /// As [`open_playback`].
    unsafe fn open_device(
        device: u32,
        spec: AudioSpec,
        callback: AudioStreamCallback,
        userdata: *mut c_void,
    ) -> Option<AudioStream> {
        if !init_audio() {
            return None;
        }
        let open = api()?.SDL_OpenAudioDeviceStream?;
        // SAFETY: a live spec for the duration of the call, and the caller's
        // obligation for the callback.
        let stream = unsafe { open(device, &spec, callback, userdata) };
        (!stream.is_null()).then_some(stream)
    }

    /// What the default playback device is actually running at.
    pub fn device_format() -> Option<AudioSpec> {
        format_of(DEFAULT_PLAYBACK)
    }

    /// What the default recording device is actually running at.
    pub fn recording_format() -> Option<AudioSpec> {
        format_of(DEFAULT_RECORDING)
    }

    fn format_of(device: u32) -> Option<AudioSpec> {
        if !init_audio() {
            return None;
        }
        let f = api()?.SDL_GetAudioDeviceFormat?;
        let mut spec = AudioSpec {
            format: format::F32,
            channels: 2,
            freq: 48000,
        };
        // SAFETY: a writable spec; the frame count is not wanted.
        unsafe { f(device, &mut spec, std::ptr::null_mut()) }.then_some(spec)
    }

    /// Whether the host has anything to record from. The default recording
    /// device opens whichever one that is, so only whether there is one matters.
    pub fn has_recording_device() -> bool {
        if !init_audio() {
            return false;
        }
        let Some(api) = api() else {
            return false;
        };
        let (Some(get), Some(free)) = (api.SDL_GetAudioRecordingDevices, api.SDL_free) else {
            return false;
        };
        let mut count: c_int = 0;
        // SAFETY: SDL writes the count and returns an array from its own
        // allocator, freed below.
        unsafe {
            let ids = get(&mut count);
            if !ids.is_null() {
                free(ids.cast());
            }
        }
        count > 0
    }

    /// # Safety
    ///
    /// `stream` must be one [`open_playback`] returned and not yet destroyed.
    pub unsafe fn put(stream: AudioStream, bytes: &[u8]) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_PutAudioStreamData) else {
            return false;
        };
        // SAFETY: the caller's obligation, and a live slice.
        unsafe { f(stream, bytes.as_ptr().cast(), bytes.len() as c_int) }
    }

    /// # Safety
    ///
    /// As [`put`].
    pub unsafe fn resume(stream: AudioStream) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_ResumeAudioStreamDevice) else {
            return false;
        };
        unsafe { f(stream) }
    }

    /// # Safety
    ///
    /// As [`put`].
    pub unsafe fn pause(stream: AudioStream) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_PauseAudioStreamDevice) else {
            return false;
        };
        unsafe { f(stream) }
    }

    /// # Safety
    ///
    /// As [`put`], and nothing may use `stream` afterwards.
    pub unsafe fn destroy(stream: AudioStream) {
        if let Some(f) = api().and_then(|a| a.SDL_DestroyAudioStream) {
            unsafe { f(stream) };
        }
    }

    /// # Safety
    ///
    /// As [`put`].
    pub unsafe fn clear(stream: AudioStream) {
        if let Some(f) = api().and_then(|a| a.SDL_ClearAudioStream) {
            unsafe { f(stream) };
        }
    }

    /// The default playback device's name, for a caller that shows the user
    /// which device it is using.
    pub fn device_name() -> Option<String> {
        name_of(DEFAULT_PLAYBACK)
    }

    /// The default recording device's name.
    pub fn recording_name() -> Option<String> {
        name_of(DEFAULT_RECORDING)
    }

    fn name_of(device: u32) -> Option<String> {
        if !init_audio() {
            return None;
        }
        let f = api()?.SDL_GetAudioDeviceName?;
        // SAFETY: SDL owns the string and it outlives this call.
        let name = unsafe { f(device) };
        match name.is_null() {
            true => None,
            // SAFETY: a NUL-terminated string of SDL's.
            false => Some(
                unsafe { CStr::from_ptr(name) }
                    .to_string_lossy()
                    .into_owned(),
            ),
        }
    }

    /// A stream that converts between two formats and is attached to no device.
    ///
    /// This is what an `AudioConverter` is: bytes in one format in, the same
    /// audio in another out, including a sample-rate change. SDL resamples,
    /// which is the part that cannot be written in a few lines.
    pub fn create_converter(from: AudioSpec, to: AudioSpec) -> Option<AudioStream> {
        if !init_audio() {
            return None;
        }
        let f = api()?.SDL_CreateAudioStream?;
        // SAFETY: two live specs for the duration of the call.
        let stream = unsafe { f(&from, &to) };
        (!stream.is_null()).then_some(stream)
    }

    /// # Safety
    ///
    /// As [`put`].
    pub unsafe fn set_format(stream: AudioStream, from: AudioSpec, to: AudioSpec) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_SetAudioStreamFormat) else {
            return false;
        };
        // SAFETY: the caller's obligation, and two live specs.
        unsafe { f(stream, &from, &to) }
    }

    /// Take converted bytes out, up to `out.len()`. The count is what was
    /// written, and a negative return from SDL is reported as none.
    ///
    /// # Safety
    ///
    /// As [`put`].
    pub unsafe fn get(stream: AudioStream, out: &mut [u8]) -> usize {
        let Some(f) = api().and_then(|a| a.SDL_GetAudioStreamData) else {
            return 0;
        };
        // SAFETY: the caller's obligation, and a live buffer of that length.
        let taken = unsafe { f(stream, out.as_mut_ptr().cast(), out.len() as c_int) };
        taken.max(0) as usize
    }

    /// Converted bytes ready to be taken.
    ///
    /// # Safety
    ///
    /// As [`put`].
    pub unsafe fn available(stream: AudioStream) -> usize {
        let Some(f) = api().and_then(|a| a.SDL_GetAudioStreamAvailable) else {
            return 0;
        };
        unsafe { f(stream) }.max(0) as usize
    }

    /// Tell SDL no more input is coming, so it hands back the tail rather than
    /// holding it for a resampler that will never be fed again.
    ///
    /// # Safety
    ///
    /// As [`put`].
    pub unsafe fn flush(stream: AudioStream) -> bool {
        let Some(f) = api().and_then(|a| a.SDL_FlushAudioStream) else {
            return false;
        };
        unsafe { f(stream) }
    }

    /// Bytes SDL still holds for this stream, which is what a feeder uses to
    /// decide whether to pull another buffer.
    ///
    /// # Safety
    ///
    /// As [`put`].
    pub unsafe fn queued(stream: AudioStream) -> i32 {
        let Some(f) = api().and_then(|a| a.SDL_GetAudioStreamQueued) else {
            return 0;
        };
        unsafe { f(stream) }
    }
}

#[cfg(test)]
#[test]
fn failed_sdl_vulkan_surface_retains_failure_classification_and_refusal() {
    let window = 0x7e0411usize as Window;
    // This fixture injects a synthetic SDL pointer to test failure state,
    // rather than running an actual native surface initialization.
    video::test_register_synthetic_window(window);
    let instance = 0x7e0412u64;
    video::vulkan::test_insert_failed_surface(
        window,
        instance,
        "SDL Vulkan is not supported by this window",
        false,
    );
    match video::vulkan::acquire_prepared_surface(window, instance, None) {
        video::vulkan::PreparedSurface::Failed(reason) => {
            assert_eq!(reason.reason, "SDL Vulkan is not supported by this window");
            assert!(!reason.transient);
        }
        _ => panic!("SDL failure reason was discarded"),
    }
    video::vulkan::refuse_failed_surface(window, instance);
    assert!(matches!(
        video::vulkan::acquire_prepared_surface(window, instance, None),
        video::vulkan::PreparedSurface::Abandoned,
    ));
    assert!(!video::vulkan::owns_window(window));
    assert!(!video::vulkan::forget_window(window));

    video::vulkan::test_insert_failed_surface(
        window,
        instance,
        "temporarily unavailable during minimization",
        true,
    );
    match video::vulkan::acquire_prepared_surface(window, instance, None) {
        video::vulkan::PreparedSurface::Failed(reason) => assert!(reason.transient),
        _ => panic!("transient SDL failure was discarded"),
    }
    assert!(video::vulkan::retry_surface(window, instance, None));
    assert!(!video::vulkan::owns_window(window));
    assert!(!video::vulkan::forget_window(window));
    video::test_unregister_synthetic_window(window);
}

#[cfg(test)]
#[test]
fn failed_backend_surface_creation_preserves_vulkan_window_graphics_state() {
    // SDL video ownership belongs to one OS thread per process, not the
    // moving libtest thread pool. Run the real window assertion in isolation.
    if !crate::isolated::run_here() {
        return;
    }
    if !video::init_video() {
        return; // Graphical preflight is required in the Vulkan integration suite.
    }
    assert!(video::is_video_thread());
    // A real SDL_WINDOW_VULKAN window, with the backend failure injected
    // after SDL changed the graphics flags, as SDL can do during conversion.
    let window = video::create_window(
        c"failed backend Vulkan conversion",
        80,
        64,
        video::flags::VULKAN,
    )
    .expect("create real SDL Vulkan window");
    assert_ne!(video::window_flags(window) & video::flags::VULKAN, 0);
    let instance = 0x8e09311_u64;
    video::vulkan::test_insert_failed_surface(
        window,
        instance,
        "unsupported Vulkan surface backend",
        false,
    );
    video::vulkan::test_publish_window_graphics_state(window);
    assert!(
        video::vulkan::owns_window(window),
        "a failed backend must not lose SDL's earlier Vulkan conversion"
    );
    assert!(matches!(
        video::vulkan::acquire_prepared_surface(window, instance, None),
        video::vulkan::PreparedSurface::Failed(_),
    ));
    assert!(
        !video::present_window(window, |_| true),
        "CPU SDL window-surface fallback is illegal after Vulkan conversion"
    );
    video::destroy_window(window);
}
