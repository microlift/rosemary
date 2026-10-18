use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use foundation::geometry::{CGFloat, NSRect};
use objc::object::Id;
use objc::sel::Sel;
use static_init::dynamic;

/// `CGSessionCopyCurrentDictionary`.
///
/// There is no WindowServer login session on this host. An empty dictionary is
/// still preferable to null: it represents a local session with none of the
/// optional flags (notably `kCGSSessionRemote`) set, which is the environment
/// the SDL-backed application actually runs in.
#[expect(non_snake_case, reason = "the C spelling is the interface")]
pub unsafe extern "C" fn CGSessionCopyCurrentDictionary() -> corefoundation::base::CFTypeRef {
    corefoundation::collections::new_dictionary(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    )
}

/// `CGWindowServerCreateServerPort`, a private call callers use to ask whether
/// a WindowServer is reachable. There is none here, so the answer is
/// `MACH_PORT_NULL`, which is what Apple's returns when it cannot connect.
#[expect(non_snake_case, reason = "the C spelling is the interface")]
pub unsafe extern "C" fn CGWindowServerCreateServerPort() -> u32 {
    0
}

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Screen {
    isa: *const c_void,
    display: usize,
}

pub const INSTANCE_SIZE: usize = size_of::<Screen>();

/// The screen object for a display, made once per display and kept.
///
/// `+mainScreen` is compared against `-[NSWindow screen]` by programs deciding
/// whether they are on the display they meant to be, so two objects for one
/// display would make that comparison false on a single-monitor machine.
fn screen_for(display: u32) -> Id {
    screen_object(c"NSScreen", display)
}

/// [`screen_for`] for any class using the `Screen` layout, which is how
/// `UIScreen` stands for the same display.
pub(crate) fn screen_object(class_name: &std::ffi::CStr, display: u32) -> Id {
    static SCREENS: std::sync::Mutex<Vec<(std::ffi::CString, u32, usize)>> =
        std::sync::Mutex::new(Vec::new());
    let mut screens = SCREENS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, _, object)) = screens
        .iter()
        .find(|(n, d, _)| n.as_c_str() == class_name && *d == display)
    {
        return *object as Id;
    }
    let class = objc::class::lookup(class_name).expect("declared before its methods");
    // SAFETY: `Screen` is the layout the class was registered with.
    let screen = unsafe { corefoundation::base::allocate::<Screen>(class) };
    // SAFETY: freshly allocated.
    unsafe { (*screen).display = display as usize };
    // Retained forever: a screen a guest can release is one it can make dangle,
    // and these are handed out unowned by `+mainScreen`.
    objc::arc::retain(screen as Id);
    screens.push((class_name.to_owned(), display, screen as usize));
    screen as Id
}

pub(crate) fn display_of(receiver: Id) -> u32 {
    // SAFETY: a screen of this crate's.
    unsafe { (*(receiver as *const Screen)).display as u32 }
}

unsafe extern "C" fn main_screen(_class: Id, _sel: Sel) -> Id {
    match sdl3::video::displays().first() {
        Some(display) => screen_for(*display),
        // No display is not an error: a headless host has none, and a program
        // that asks gets the nil AppKit gives it rather than an invented screen.
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn deepest_screen(class: Id, sel: Sel) -> Id {
    // SAFETY: `+mainScreen` is `id (Class, SEL)`.
    unsafe { main_screen(class, sel) }
}

unsafe extern "C" fn screens(_class: Id, _sel: Sel) -> Id {
    let objects: Vec<corefoundation::base::CFTypeRef> = sdl3::video::displays()
        .into_iter()
        .map(|d| screen_for(d) as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &objects,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // SAFETY: a fresh array.
    unsafe { corefoundation::api::CFAutorelease(array) as Id }
}

unsafe extern "C" fn frame(receiver: Id, _sel: Sel) -> NSRect {
    let display = display_of(receiver);
    match sdl3::video::display_bounds(display) {
        Some(b) => crate::geometry::rect_from_sdl(b.x, b.y, b.w, b.h),
        None => NSRect::default(),
    }
}

/// `-visibleFrame`: the part not under the menu bar or the Dock.
///
/// SDL's usable bounds is the same question, and where it cannot answer the
/// whole frame is the honest fallback - a program using it to place a window
/// gets one that may sit under the menu bar, rather than one placed against a
/// margin this runtime invented.
unsafe extern "C" fn visible_frame(receiver: Id, sel: Sel) -> NSRect {
    let display = display_of(receiver);
    match sdl3::video::display_usable_bounds(display) {
        Some(b) => crate::geometry::rect_from_sdl(b.x, b.y, b.w, b.h),
        // SAFETY: `-frame` is `NSRect (id, SEL)`.
        None => unsafe { frame(receiver, sel) },
    }
}

/// `-maximumFramesPerSecond`: how often the display can present, which a game
/// uses to pick a frame cap. Sixty where SDL has no mode to report, because the
/// answer is a divisor a caller will use either way.
unsafe extern "C" fn maximum_frames_per_second(receiver: Id, _sel: Sel) -> isize {
    refresh_rate(receiver).round().max(1.0) as isize
}

pub(crate) fn refresh_rate(receiver: Id) -> f64 {
    sdl3::video::display_refresh_rate(display_of(receiver))
        .filter(|rate| rate.is_finite() && *rate > 0.0)
        .map(f64::from)
        .unwrap_or(60.0)
}

/// SDL exposes the current mode's fixed refresh rate, in frames per second.
unsafe extern "C" fn refresh_interval(receiver: Id, _sel: Sel) -> f64 {
    1.0 / refresh_rate(receiver)
}

unsafe extern "C" fn backing_scale_factor(receiver: Id, _sel: Sel) -> CGFloat {
    sdl3::video::display_content_scale(display_of(receiver)) as f64
}

/// `-deviceDescription`: `NSScreenNumber` for a `CGDirectDisplayID`, and
/// `NSDeviceSize`/`NSDeviceResolution` for anything measuring the display.
///
/// `NSDeviceResolution` is dots per *inch* and is 72 for a display at unit
/// scale, because AppKit's user-space unit is 1/72 inch by definition. It is
/// scaled by the backing factor, which is what makes a Retina screen report 144
/// and is the only place the scale enters the arithmetic.
unsafe extern "C" fn device_description(receiver: Id, _sel: Sel) -> Id {
    use foundation::geometry::NSSize;

    let display = display_of(receiver);
    // SAFETY: a screen of this crate's, which is what this method is sent to.
    let frame = unsafe { frame(receiver, objc::sel::get("frame")) };
    let scale = sdl3::video::display_content_scale(display) as f64;
    // A scale of zero would make the resolution zero, which is the value this
    // whole entry exists to keep a caller from dividing by.
    let scale = match scale > 0.0 {
        true => scale,
        false => 1.0,
    };
    // Every value is autoreleased, so this function owns none of them and the
    // dictionary's retain is the only one that matters. Mixing an owned value
    // in with autoreleased ones is how a leak or a double release gets written.
    let number = |value: i64| {
        // SAFETY: a fresh number this closure owns a reference to.
        unsafe {
            corefoundation::api::CFAutorelease(corefoundation::collections::new_number(
                corefoundation::collections::Number::Integer(value),
            ))
        }
    };
    let entries: [(&str, corefoundation::base::CFTypeRef); 4] = [
        ("NSScreenNumber", number(display as i64)),
        (
            // Pixels, which on a scaled display is the frame times the scale.
            "NSDeviceSize",
            foundation::geometry::boxed_size(NSSize {
                width: frame.size.width * scale,
                height: frame.size.height * scale,
            }) as corefoundation::base::CFTypeRef,
        ),
        (
            "NSDeviceResolution",
            foundation::geometry::boxed_size(NSSize {
                width: POINTS_PER_INCH * scale,
                height: POINTS_PER_INCH * scale,
            }) as corefoundation::base::CFTypeRef,
        ),
        ("NSDeviceBitsPerSample", number(8)),
    ];
    let pairs: Vec<(
        corefoundation::base::CFTypeRef,
        corefoundation::base::CFTypeRef,
    )> = entries
        .iter()
        .map(|(key, value)| {
            (
                foundation::string::from_str(key) as corefoundation::base::CFTypeRef,
                *value,
            )
        })
        .collect();
    let dictionary = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    // The dictionary retained each key; the boxed values are autoreleased
    // already, so only this function's own references to the keys are spent.
    for (key, _) in &pairs {
        // SAFETY: a string this function made.
        unsafe { corefoundation::api::CFRelease(*key) };
    }
    // SAFETY: a fresh dictionary.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

/// `-localizedName`: the name the host gives the display, such as `DP-1`.
///
/// Never nil, as AppKit's is not: a program puts it straight into a menu, and a
/// host that names no display is described by what it is rather than by a
/// string this runtime made up for a specific machine.
unsafe extern "C" fn localized_name(receiver: Id, _sel: Sel) -> Id {
    let name = sdl3::video::display_name(display_of(receiver))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Display".to_owned());
    // SAFETY: a fresh string, which autorelease takes over.
    unsafe { corefoundation::api::CFAutorelease(foundation::string::from_str(&name) as _) as Id }
}

/// `-safeAreaInsets` and the two `auxiliaryTop*Area` rectangles describe a
/// camera housing. No display here has one, which AppKit reports as zeros.
unsafe extern "C" fn safe_area_insets(_receiver: Id, _sel: Sel) -> crate::controls::NSEdgeInsets {
    crate::controls::NSEdgeInsets::default()
}

unsafe extern "C" fn zero_rect(_receiver: Id, _sel: Sel) -> NSRect {
    NSRect::default()
}

/// `+screensHaveSeparateSpaces`: whether each display has its own Spaces. The
/// host's displays are one desktop to SDL.
unsafe extern "C" fn screens_have_separate_spaces(_class: Id, _sel: Sel) -> u8 {
    0
}

/// `-userSpaceScaleFactor`: points to pixels, which is the backing factor here
/// because nothing scales user space apart from it.
unsafe extern "C" fn user_space_scale_factor(receiver: Id, sel: Sel) -> CGFloat {
    // SAFETY: `-backingScaleFactor` is `CGFloat (id, SEL)`.
    unsafe { backing_scale_factor(receiver, sel) }
}

/// The three extended-dynamic-range limits. The display is SDR, so 1.0; the
/// reference limit is 0.0 because AppKit documents that for a screen without a
/// reference mode.
unsafe extern "C" fn edr_limit(_receiver: Id, _sel: Sel) -> CGFloat {
    1.0
}

unsafe extern "C" fn edr_reference_limit(_receiver: Id, _sel: Sel) -> CGFloat {
    0.0
}

fn scale_rect(rect: NSRect, factor: f64) -> NSRect {
    NSRect {
        origin: foundation::geometry::NSPoint {
            x: rect.origin.x * factor,
            y: rect.origin.y * factor,
        },
        size: foundation::geometry::NSSize {
            width: rect.size.width * factor,
            height: rect.size.height * factor,
        },
    }
}

unsafe extern "C" fn convert_rect_to_backing(receiver: Id, sel: Sel, rect: NSRect) -> NSRect {
    // SAFETY: `-backingScaleFactor` is `CGFloat (id, SEL)`.
    scale_rect(rect, unsafe { backing_scale_factor(receiver, sel) })
}

unsafe extern "C" fn convert_rect_from_backing(receiver: Id, sel: Sel, rect: NSRect) -> NSRect {
    // SAFETY: as above.
    let factor = unsafe { backing_scale_factor(receiver, sel) };
    scale_rect(rect, if factor > 0.0 { 1.0 / factor } else { 1.0 })
}

/// `-supportedWindowDepths`: a zero-terminated array, of the one depth
/// [`depth`] reports.
unsafe extern "C" fn supported_window_depths(_receiver: Id, _sel: Sel) -> *const i32 {
    static DEPTHS: [i32; 2] = [WINDOW_DEPTH_24_BIT_RGB, 0];
    DEPTHS.as_ptr()
}

/// `-[NSScreen depth]`, an `NSWindowDepth`.
///
/// An `NSWindowDepth` is not a bit count: it is an opaque value a caller passes to
/// `NSBitsPerPixelFromDepth` and its relatives, and the constants are small
/// integers AppKit assigns. This answers `NSWindowDepthTwentyfourBitRGB`, which is
/// the 8-bits-per-sample RGB depth `-deviceDescription` already reports under
/// `NSDeviceBitsPerSample` - the two have to agree, because a caller that reads
/// both and finds them different concludes the screen changed between the calls.
unsafe extern "C" fn depth(_receiver: Id, _sel: Sel) -> i32 {
    WINDOW_DEPTH_24_BIT_RGB
}

/// `NSWindowDepthTwentyfourBitRGB`. AppKit's own value, because a caller compares
/// against the constant it imported.
const WINDOW_DEPTH_24_BIT_RGB: i32 = 0x208;

/// `NSBitsPerSampleFromDepth`, `NSBitsPerPixelFromDepth`,
/// `NSNumberOfColorComponents` and `NSPlanarFromDepth` for the one depth this
/// runtime reports.
///
/// Published because the depth is opaque: a caller that has one can only learn
/// anything about it through these, and an unbound one leaves it dividing by a
/// zero it got from nowhere.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSBitsPerSampleFromDepth(_depth: i32) -> isize {
    8
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSBitsPerPixelFromDepth(_depth: i32) -> isize {
    32
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSNumberOfColorComponents(_space: Id) -> isize {
    3
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn NSPlanarFromDepth(_depth: i32) -> u8 {
    0
}

/// The color-space name associated with the RGB depth this runtime reports.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSColorSpaceFromDepth(_depth: i32) -> Id {
    foundation::notification::name_object("NSCalibratedRGBColorSpace") as Id
}

/// Choose the sole window depth this backend exposes.
#[allow(non_snake_case)]
pub unsafe extern "C" fn NSBestDepth(
    _color_space: Id,
    _bits_per_sample: isize,
    _bits_per_pixel: isize,
    _planar: u8,
    exact: *mut u8,
) -> i32 {
    if !exact.is_null() {
        unsafe { exact.write(1) };
    }
    WINDOW_DEPTH_24_BIT_RGB
}

/// `-[NSScreen colorSpace]`, an `NSColorSpace`.
///
/// Nil, and deliberately. A colour space here would have to be one this tree can
/// convert *through*, and `crate::color`'s note says why there is no conversion:
/// every colour in this runtime is already in the space its only consumer wants.
/// Nil is the documented answer for a screen whose profile cannot be read, and a
/// caller takes its "assume sRGB" branch - which is what the display is.
unsafe extern "C" fn color_space(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

/// AppKit's user-space unit is 1/72 inch, so an unscaled display is 72 dpi by
/// definition rather than by measurement.
const POINTS_PER_INCH: f64 = 72.0;

/// `CGDirectDisplayID CGMainDisplayID(void)`.
///
/// CoreGraphics is below AppKit and this is its only entry point here, which is
/// the argument for it living in this file rather than a crate of its own: what
/// a `CGDirectDisplayID` *is* in this tree is decided by [`device_description`],
/// which publishes an SDL display id under `NSScreenNumber` for `CVDisplayLink`
/// to open. Two definitions of that would be two display numbering schemes.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGMainDisplayID() -> u32 {
    sdl3::video::primary_display()
}

/// Gamma tables a program set, per display, as red, green and blue ramps.
///
/// Kept so a program reads back what it wrote - one that fades by stepping its
/// own table depends on that - but not applied: SDL 3 has no gamma ramp
/// control, so the screen's colours do not change.
static GAMMA: std::sync::Mutex<Vec<(u32, [Vec<f32>; 3])>> = std::sync::Mutex::new(Vec::new());

/// `CGError CGGetDisplayTransferByTable(CGDirectDisplayID, uint32_t capacity,
/// CGGammaValue *red, CGGammaValue *green, CGGammaValue *blue, uint32_t *count)`
///
/// # Safety
///
/// Each table is null or holds `capacity` values; `count` is null or writable.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGGetDisplayTransferByTable(
    display: u32,
    capacity: u32,
    red: *mut f32,
    green: *mut f32,
    blue: *mut f32,
    count: *mut u32,
) -> i32 {
    let size = capacity.min(1024) as usize;
    let tables = GAMMA.lock().unwrap_or_else(|e| e.into_inner());
    let set = tables
        .iter()
        .find(|(d, _)| *d == display)
        .map(|(_, t)| t.clone());
    drop(tables);
    // Identity until a program sets one, sampled to the caller's size.
    let sample = |channel: usize, at: usize| -> f32 {
        let position = match size > 1 {
            true => at as f32 / (size - 1) as f32,
            false => 0.0,
        };
        match &set {
            Some(t) if !t[channel].is_empty() => {
                let index = (position * (t[channel].len() - 1) as f32).round() as usize;
                t[channel][index.min(t[channel].len() - 1)]
            }
            _ => position,
        }
    };
    for (channel, table) in [red, green, blue].into_iter().enumerate() {
        if table.is_null() {
            continue;
        }
        for at in 0..size {
            // SAFETY: the caller's table of `capacity` values.
            unsafe { *table.add(at) = sample(channel, at) };
        }
    }
    if !count.is_null() {
        // SAFETY: the caller's out-parameter.
        unsafe { *count = size as u32 };
    }
    0
}

/// `CGError CGSetDisplayTransferByTable(CGDirectDisplayID, uint32_t size,
/// const CGGammaValue *red, const CGGammaValue *green, const CGGammaValue *blue)`
///
/// # Safety
///
/// Each table holds `size` values.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGSetDisplayTransferByTable(
    display: u32,
    size: u32,
    red: *const f32,
    green: *const f32,
    blue: *const f32,
) -> i32 {
    if red.is_null() || green.is_null() || blue.is_null() {
        // `kCGErrorIllegalArgument`.
        return 1001;
    }
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosemary: appkit: the program set a display gamma table; it is kept but not \
             applied, as SDL has no gamma ramp control"
        )
    });
    let size = size.min(1024) as usize;
    // SAFETY: the caller's tables of `size` values.
    let read = |table: *const f32| unsafe { std::slice::from_raw_parts(table, size) }.to_vec();
    let entry = [read(red), read(green), read(blue)];
    let mut tables = GAMMA.lock().unwrap_or_else(|e| e.into_inner());
    tables.retain(|(d, _)| *d != display);
    tables.push((display, entry));
    0
}

/// `void CGDisplayRestoreColorSyncSettings(void)`: every table back to identity.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayRestoreColorSyncSettings() {
    GAMMA.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// `CGRect CGDisplayBounds(CGDirectDisplayID)`.
///
/// CoreGraphics measures the global display space from the **top left** with y
/// increasing downwards, which is SDL's convention and not `-[NSScreen frame]`'s
/// - so this is not [`frame`] with a different receiver, and converting it as
/// though it were puts a second display on the wrong side of the first.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayBounds(display: u32) -> NSRect {
    match sdl3::video::display_bounds(display) {
        Some(b) => NSRect {
            origin: foundation::geometry::NSPoint {
                x: b.x as f64,
                y: b.y as f64,
            },
            size: foundation::geometry::NSSize {
                width: b.w as f64,
                height: b.h as f64,
            },
        },
        // What CoreGraphics answers for a display id it does not know.
        None => NSRect::default(),
    }
}

/// `size_t CGDisplayPixelsWide(CGDirectDisplayID)` and `CGDisplayPixelsHigh`.
///
/// The display's size in the global space's own units, which is what
/// [`CGDisplayBounds`] reports and what a guest sizes a full-screen window from.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayPixelsWide(display: u32) -> u64 {
    // SAFETY: a plain call.
    unsafe { CGDisplayBounds(display) }.size.width as u64
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayPixelsHigh(display: u32) -> u64 {
    // SAFETY: a plain call.
    unsafe { CGDisplayBounds(display) }.size.height as u64
}

/// `size_t CGDisplayBitsPerPixel(CGDirectDisplayID)`
///
/// 32, matching the `IO32BitDirectPixels` encoding every mode reports; zero
/// for a display id that is not online, as CoreGraphics answers.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayBitsPerPixel(display: u32) -> u64 {
    // SAFETY: a plain call.
    if unsafe { CGDisplayIsOnline(display) } != 0 {
        32
    } else {
        0
    }
}

/// `CGError CGDisplayMoveCursorToPoint(CGDirectDisplayID, CGPoint)`.
///
/// The point is relative to the display's origin; the warp wants global space.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayMoveCursorToPoint(
    display: u32,
    point: foundation::geometry::NSPoint,
) -> i32 {
    // SAFETY: a plain call.
    let origin = unsafe { CGDisplayBounds(display) }.origin;
    let global = foundation::geometry::NSPoint {
        x: origin.x + point.x,
        y: origin.y + point.y,
    };
    // SAFETY: a plain call.
    unsafe { CGWarpMouseCursorPosition(global) }
}

/// `kCGErrorFailure`: what a request for something this host cannot do answers.
const CG_ERROR_FAILURE: i32 = 1001;

/// `CGError CGDisplayCapture(CGDirectDisplayID)` and `CGCaptureAllDisplays`.
///
/// Refused: SDL gives no exclusive claim on a display, and a caller told it has one
/// would go on to assume it can change the mode. The caller is expected to carry on
/// with a window, as it does when another process holds the display.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayCapture(_display: u32) -> i32 {
    CG_ERROR_FAILURE
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGCaptureAllDisplays() -> i32 {
    CG_ERROR_FAILURE
}

/// `CGError CGReleaseAllDisplays(void)`: nothing is ever captured.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGReleaseAllDisplays() -> i32 {
    0
}

/// `CGError CGDisplaySetDisplayMode(display, mode, CFDictionaryRef options)`.
///
/// Refused: this host's display mode is not the guest's to change.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplaySetDisplayMode(
    _display: u32,
    _mode: *const std::ffi::c_void,
    _options: *const std::ffi::c_void,
) -> i32 {
    CG_ERROR_FAILURE
}

/// The fade-reservation token written by [`CGAcquireDisplayFadeReservation`].
const FADE_TOKEN: u32 = 1;

/// `CGError CGAcquireDisplayFadeReservation(CGDisplayReservationInterval, token *)`.
///
/// Granted, and the fade itself is a no-op: there is no gamma ramp to fade here,
/// and a display that does not fade is merely less polished, not wrong.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGAcquireDisplayFadeReservation(_seconds: f32, token: *mut u32) -> i32 {
    if !token.is_null() {
        // SAFETY: an out parameter the caller sized.
        unsafe { token.write(FADE_TOKEN) };
    }
    0
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGReleaseDisplayFadeReservation(_token: u32) -> i32 {
    0
}

/// `CGError CGDisplayFade(token, seconds, startBlend, endBlend, r, g, b, synchronous)`.
#[allow(non_snake_case)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGDisplayFade(
    _token: u32,
    _seconds: f32,
    _start_blend: f32,
    _end_blend: f32,
    _red: f32,
    _green: f32,
    _blue: f32,
    _synchronous: i32,
) -> i32 {
    0
}

/// `CGWindowLevel CGShieldingWindowLevel(void)`: `kCGMaximumWindowLevel - 3`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGShieldingWindowLevel() -> i32 {
    2147483628
}

/// `boolean_t CGDisplayIsMain(CGDirectDisplayID)`
///
/// The primary display, which is the one SDL lists first.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayIsMain(display: u32) -> u32 {
    (display == sdl3::video::primary_display()) as u32
}

/// `boolean_t CGDisplayIsOnline(CGDirectDisplayID)`
///
/// Whether SDL still lists it. A display unplugged between two calls is exactly
/// the case the question exists for.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayIsOnline(display: u32) -> u32 {
    sdl3::video::displays().contains(&display) as u32
}

/// `boolean_t CGDisplayIsBuiltin(CGDirectDisplayID)`
///
/// False: SDL does not say which panel is the laptop's own, and claiming a
/// built-in one would have callers look for brightness and lid state that is
/// not modelled.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayIsBuiltin(_display: u32) -> u32 {
    0
}

/// `CGDirectDisplayID CGDisplayPrimaryDisplay(CGDirectDisplayID)`
///
/// With no mirroring modelled, every display is its own primary.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayPrimaryDisplay(display: u32) -> u32 {
    display
}

/// `CGError CGDisplayRelease(CGDirectDisplayID)`
///
/// `CGDisplayCapture` is not modelled, so there is never anything to release;
/// kCGErrorSuccess.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayRelease(_display: u32) -> i32 {
    0
}

/// `double CGDisplayRotation(CGDirectDisplayID)`
///
/// SDL's display orientation is not mapped to degrees; an unrotated panel.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayRotation(_display: u32) -> f64 {
    0.0
}

/// `boolean_t CGDisplayIsAsleep(CGDirectDisplayID)`
///
/// False: a display this process can enumerate is one SDL is talking to, and
/// there is no sleep state to read. Answering true would have a caller skip the
/// screen entirely.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayIsAsleep(_display: u32) -> u32 {
    0
}

/// `CGDirectDisplayID CGDisplayMirrorsDisplay(CGDirectDisplayID)`
///
/// `kCGNullDirectDisplay`, because nothing here mirrors - the same fact
/// [`CGGetOnlineDisplayList`] already relies on to answer for both display lists.
/// A caller uses this to drop a mirror from its own list, and zero keeps every
/// display in it.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayMirrorsDisplay(_display: u32) -> u32 {
    0
}

/// `uint32_t CGDisplayModelNumber(CGDirectDisplayID)`, and the vendor and serial
/// numbers beside it.
///
/// `kDisplayProductIDGeneric` and zeroes. These come from a display's EDID, which
/// SDL does not expose, and a caller uses them to tell two monitors apart or to
/// key a saved window position on - so an invented value that *changed* between
/// runs would be worse than a constant one. The display id is what distinguishes
/// screens here, and a caller already has it.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModelNumber(_display: u32) -> u32 {
    // `kDisplayProductIDGeneric`, the value CoreGraphics itself reports for a
    // display whose product id it could not read.
    0x717
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayVendorNumber(_display: u32) -> u32 {
    // `kDisplayVendorIDUnknown`.
    0
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplaySerialNumber(_display: u32) -> u32 {
    0
}

/// A `CGDisplayModeRef`: the display it came from, and the SDL mode it names.
///
/// `fixed` is `None` for the mode `CGDisplayCopyDisplayMode` hands out, which reads
/// the desktop mode when asked, for the reason the module note gives about
/// caching. A listed mode is a snapshot of one SDL fullscreen mode.
///
/// A caller reaches into the real structure: `+0x10` holds the mode's
/// `CFDictionary` of IOKit properties, and fastfetch reads it there. So the
/// layout starts with a CoreFoundation-sized header and that dictionary - empty,
/// since SDL has none of those properties - rather than the Rust fields, which
/// such a caller would otherwise read as a garbage object.
#[repr(C)]
struct DisplayMode {
    _base: [usize; 2],
    properties: CFTypeRef,
    display: u32,
    fixed: Option<sdl3::DisplayMode>,
}

fn new_mode(display: u32, fixed: Option<sdl3::DisplayMode>) -> *const c_void {
    let properties = corefoundation::collections::new_dictionary(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    let mode = Box::leak(Box::new(DisplayMode {
        _base: [0; 2],
        properties,
        display,
        fixed,
    }));
    let handle = mode as *const DisplayMode as *const c_void;
    corefoundation::base::note_opaque(handle);
    handle
}

/// The SDL mode behind a handle this module made, or `None`.
fn sdl_mode_of(mode: *const c_void) -> Option<sdl3::DisplayMode> {
    if mode.is_null() || !corefoundation::base::is_opaque(mode) {
        return None;
    }
    // SAFETY: an opaque handle this module made, whose layout is `DisplayMode`.
    let mode = unsafe { &*(mode as *const DisplayMode) };
    mode.fixed
        .or_else(|| sdl3::video::desktop_mode(mode.display))
}

/// `CGDisplayModeRef CGDisplayCopyDisplayMode(CGDirectDisplayID)`
///
/// The handle is not an Objective-C object, so it is registered as an opaque
/// CoreFoundation type: a caller releases it with `CGDisplayModeRelease`, and may
/// pass it to `CFRetain`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayCopyDisplayMode(display: u32) -> *const c_void {
    if !sdl3::video::displays().contains(&display) {
        return std::ptr::null();
    }
    new_mode(display, None)
}

/// `CFArrayRef CGDisplayCopyAllDisplayModes(CGDirectDisplayID, CFDictionaryRef)`
///
/// SDL's fullscreen modes, with the desktop mode added when SDL does not list it,
/// so the mode a caller is already in is always one it can find. The options
/// dictionary only asks for Retina duplicates, which SDL does not list apart.
/// Null for an unknown display, as CoreGraphics answers.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayCopyAllDisplayModes(
    display: u32,
    _options: CFTypeRef,
) -> CFTypeRef {
    if !sdl3::video::displays().contains(&display) {
        return std::ptr::null();
    }
    let mut modes = sdl3::video::fullscreen_modes(display);
    if let Some(desktop) = sdl3::video::desktop_mode(display) {
        let listed = modes.iter().any(|m| {
            (m.w, m.h, m.pixel_density, m.refresh_rate)
                == (
                    desktop.w,
                    desktop.h,
                    desktop.pixel_density,
                    desktop.refresh_rate,
                )
        });
        if !listed {
            modes.insert(0, desktop);
        }
    }
    let handles: Vec<CFTypeRef> = modes
        .into_iter()
        .map(|m| new_mode(display, Some(m)) as CFTypeRef)
        .collect();
    // No retain callbacks: the modes are leaked, so there is nothing to count.
    corefoundation::collections::new_array(&handles, corefoundation::callbacks::Policy::NONE, false)
}

/// `double CGDisplayModeGetRefreshRate(CGDisplayModeRef)`
///
/// Zero for a mode this runtime did not make, and zero when SDL reports no rate -
/// which is what CoreGraphics answers for a display whose rate is not fixed, and
/// the value a caller already has to handle.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeGetRefreshRate(mode: *const c_void) -> f64 {
    sdl_mode_of(mode).map_or(0.0, |m| m.refresh_rate.max(0.0) as f64)
}

/// `size_t CGDisplayModeGetWidth(CGDisplayModeRef)`, in points.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeGetWidth(mode: *const c_void) -> usize {
    sdl_mode_of(mode).map_or(0, |m| m.w.max(0) as usize)
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeGetHeight(mode: *const c_void) -> usize {
    sdl_mode_of(mode).map_or(0, |m| m.h.max(0) as usize)
}

fn pixels(points: i32, density: f32) -> usize {
    let density = if density > 0.0 { density } else { 1.0 };
    (points.max(0) as f32 * density).round() as usize
}

/// `size_t CGDisplayModeGetPixelWidth(CGDisplayModeRef)`, points times density.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize {
    sdl_mode_of(mode).map_or(0, |m| pixels(m.w, m.pixel_density))
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeGetPixelHeight(mode: *const c_void) -> usize {
    sdl_mode_of(mode).map_or(0, |m| pixels(m.h, m.pixel_density))
}

/// `uint32_t CGDisplayModeGetIOFlags(CGDisplayModeRef)`
///
/// `kDisplayModeValidFlag | kDisplayModeSafeFlag`: every mode SDL lists can be set.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeGetIOFlags(mode: *const c_void) -> u32 {
    sdl_mode_of(mode).map_or(0, |_| 0x1 | 0x2)
}

/// `bool CGDisplayModeIsUsableForDesktopGUI(CGDisplayModeRef)`
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeIsUsableForDesktopGUI(mode: *const c_void) -> u8 {
    sdl_mode_of(mode).is_some() as u8
}

/// `CFStringRef CGDisplayModeCopyPixelEncoding(CGDisplayModeRef)`
///
/// `IO32BitDirectPixels`, eight bits a channel, which is what every mode a
/// host compositor offers is to a program choosing among them.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeCopyPixelEncoding(mode: *const c_void) -> CFTypeRef {
    if sdl_mode_of(mode).is_none() {
        return std::ptr::null_mut();
    }
    corefoundation::string::from_bytes(b"--------RRRRRRRRGGGGGGGGBBBBBBBB", false)
}

/// `void CGDisplayModeRelease(CGDisplayModeRef)` and its retain.
///
/// A no-op: the handle is leaked for the life of the process, as every opaque
/// handle in this tree is - see `corefoundation::base::note_opaque`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeRelease(_mode: *const c_void) {}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayModeRetain(mode: *const c_void) -> *const c_void {
    mode
}

/// `CGError CGDisplayRegisterReconfigurationCallback(CGDisplayReconfigurationCallBack,
/// void *)`
///
/// Accepted and never called. The callback fires when a display is added, removed
/// or resized, and this runtime has no display-reconfiguration notification to
/// hang it on - so a caller registers one, keeps its own cached screen list, and
/// refreshes it the next time it asks about a screen instead. Refusing the
/// registration is the worse answer: a caller reads that as "this process cannot
/// track displays" and some give up on multi-monitor support entirely.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayRegisterReconfigurationCallback(
    _callback: *const c_void,
    _context: *mut c_void,
) -> i32 {
    CG_SUCCESS
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayRemoveReconfigurationCallback(
    _callback: *const c_void,
    _context: *mut c_void,
) -> i32 {
    CG_SUCCESS
}

/// `CGError CGGetDisplaysWithRect(CGRect, uint32_t maxDisplays,
/// CGDirectDisplayID *displays, uint32_t *matchingDisplayCount)`
///
/// The displays a rectangle touches, which is how a caller works out where to put
/// a window it has a position for. Measured in CoreGraphics' top-left space, so
/// the comparison is against [`CGDisplayBounds`] and not `-[NSScreen frame]`.
///
/// # Safety
///
/// As [`CGGetOnlineDisplayList`].
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGGetDisplaysWithRect(
    rect: NSRect,
    max: u32,
    list: *mut u32,
    count: *mut u32,
) -> i32 {
    let matching: Vec<u32> = sdl3::video::displays()
        .into_iter()
        .filter(|display| {
            // SAFETY: a display id from SDL's own list.
            let bounds = unsafe { CGDisplayBounds(*display) };
            // Touching, not containing: a window straddling two screens is on
            // both, which is what a caller asking this has to know.
            rect.origin.x < bounds.origin.x + bounds.size.width
                && bounds.origin.x < rect.origin.x + rect.size.width
                && rect.origin.y < bounds.origin.y + bounds.size.height
                && bounds.origin.y < rect.origin.y + rect.size.height
        })
        .collect();
    if list.is_null() {
        if count.is_null() {
            return CG_ILLEGAL_ARGUMENT;
        }
        // SAFETY: the caller states it is writable.
        unsafe { count.write(matching.len() as u32) };
        return CG_SUCCESS;
    }
    let room = (max as usize).min(matching.len());
    // SAFETY: the caller states `list` holds `max` ids.
    unsafe { std::ptr::copy_nonoverlapping(matching.as_ptr(), list, room) };
    if !count.is_null() {
        // The number written, for the reason `CGGetOnlineDisplayList` gives.
        // SAFETY: the caller states it is writable.
        unsafe { count.write(room as u32) };
    }
    CG_SUCCESS
}

/// `CFUUIDRef CGDisplayCreateUUIDFromDisplayID(CGDirectDisplayID)`
///
/// A display's UUID is how a program remembers *which* monitor a window was on
/// across runs, because a display id is only stable while the display stays
/// plugged in. On a Mac it is derived from the EDID; SDL does not expose one, so
/// this derives it from the display id instead.
///
/// What that costs is stated rather than hidden: the UUID is stable for as long as
/// the display keeps its id, which is the whole of a run, and is **not** stable
/// across a replug or a reboot the way Apple's is. A program using it to key a
/// saved window position finds its own key again within the session and starts
/// from the default next time - which is the behaviour a first run on a new
/// monitor already has.
///
/// Version 4 in the variant and version nibbles, so a caller parsing it finds a
/// well-formed UUID rather than something that fails its own validation.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> CFTypeRef {
    if !sdl3::video::displays().contains(&display) {
        return std::ptr::null();
    }
    let mut bytes = [0u8; 16];
    // A fixed prefix so the UUID reads as this runtime's rather than looking like
    // a real display's, and the id in the last four bytes.
    bytes[..8].copy_from_slice(b"ROSEMARY");
    bytes[12..].copy_from_slice(&display.to_be_bytes());
    // Version 4, variant RFC 4122.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    // Interned, so two calls for one display hand back the same object - which is
    // what `CFEqual` on two of them has to agree with. A `Create` hands over a
    // reference, so this takes one for the caller to release.
    let uuid = corefoundation::uuid::constant(bytes);
    // SAFETY: an object this crate's UUID table keeps for the process.
    unsafe { corefoundation::api::CFRetain(uuid) }
}

/// `CGDirectDisplayID CGDisplayGetDisplayIDFromUUID(CFUUIDRef)`
///
/// The inverse of [`CGDisplayCreateUUIDFromDisplayID`], which reads the id back
/// out of the bytes it put there. `kCGNullDirectDisplay` for a UUID this runtime
/// did not make, or for a display that is no longer attached - the second being
/// exactly what a program restoring a saved window position needs to be told.
///
/// # Safety
///
/// `uuid` is null or an object this runtime allocated.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayGetDisplayIDFromUUID(uuid: CFTypeRef) -> u32 {
    let Some(bytes) = (unsafe { corefoundation::uuid::bytes_of(uuid) }) else {
        return 0;
    };
    if &bytes[..6] != b"ROSETT" {
        return 0;
    }
    let display = u32::from_be_bytes(bytes[12..].try_into().expect("four bytes"));
    match sdl3::video::displays().contains(&display) {
        true => display,
        false => 0,
    }
}

/// `kCGErrorSuccess`.
const CG_SUCCESS: i32 = 0;
/// `kCGErrorIllegalArgument`.
const CG_ILLEGAL_ARGUMENT: i32 = 1001;

/// `CGError CGGetOnlineDisplayList(uint32_t, CGDirectDisplayID *, uint32_t *)`
///
/// Also `CGGetActiveDisplayList`. The two differ on a Mac only in whether a
/// display mirroring another is listed, and nothing here mirrors.
///
/// A null list with a count is the enquiry form, which is how a caller sizes
/// the array it is about to pass - so it succeeds rather than failing.
///
/// # Safety
///
/// `list` is null or writable for `max` display ids, and `count` is null or a
/// writable `uint32_t`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGGetOnlineDisplayList(max: u32, list: *mut u32, count: *mut u32) -> i32 {
    let displays = sdl3::video::displays();
    if list.is_null() {
        if count.is_null() {
            return CG_ILLEGAL_ARGUMENT;
        }
        // SAFETY: the caller states it is writable.
        unsafe { count.write(displays.len() as u32) };
        return CG_SUCCESS;
    }
    let room = (max as usize).min(displays.len());
    // SAFETY: the caller states `list` holds `max` ids.
    unsafe { std::ptr::copy_nonoverlapping(displays.as_ptr(), list, room) };
    if !count.is_null() {
        // The number *written*, which is what a caller iterates over. The total
        // would walk past the end of its own array.
        // SAFETY: the caller states it is writable.
        unsafe { count.write(room as u32) };
    }
    CG_SUCCESS
}

/// Points per inch assumed for a display whose physical size nothing here can
/// measure.
///
/// SDL reports a display's size in points and its scale factor, and neither
/// says how large the panel is. Zero is what CoreGraphics answers for a display
/// id it does not know, and it cannot be the answer here: a caller works its
/// display density out as pixels over inches, and a zero denominator is an
/// infinite density rather than a missing one. So a conventional desktop
/// density is assumed and said so.
const ASSUMED_DPI: f64 = 96.0;

/// `CGSize CGDisplayScreenSize(CGDirectDisplayID)` - the panel's size in
/// millimetres.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayScreenSize(display: u32) -> foundation::geometry::NSSize {
    const MM_PER_INCH: f64 = 25.4;
    match sdl3::video::display_bounds(display) {
        Some(b) => foundation::geometry::NSSize {
            width: b.w as f64 / ASSUMED_DPI * MM_PER_INCH,
            height: b.h as f64 / ASSUMED_DPI * MM_PER_INCH,
        },
        None => foundation::geometry::NSSize::default(),
    }
}

/// `CGOpenGLDisplayMask CGDisplayIDToOpenGLDisplayMask(CGDirectDisplayID)`.
///
/// One bit per display, which is what a pixel-format attribute list carries.
/// The mask is only ever handed back to a CGL call this tree answers, so the
/// numbering is this tree's own; a display past the thirty-second gets no bit,
/// as it does on a Mac.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayIDToOpenGLDisplayMask(display: u32) -> u32 {
    sdl3::video::displays()
        .iter()
        .position(|d| *d == display)
        .filter(|index| *index < 32)
        .map_or(0, |index| 1u32 << index)
}

/// `io_service_t CGDisplayIOServicePort(CGDirectDisplayID)`.
///
/// `MACH_PORT_NULL`, which is what a Mac answers for a display with no IOKit
/// service behind it. There is no IORegistry here, and a fabricated port number
/// would be one a caller went on to send a message to.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGDisplayIOServicePort(_display: u32) -> u32 {
    0
}

/// The display id a screen stands for, for `CVDisplayLink` and its relatives.
///
/// # Safety
///
/// `screen` must be null or a screen this crate made.
pub unsafe fn display_id(screen: Id) -> Option<u32> {
    if screen.is_null() {
        return None;
    }
    let is_screen = unsafe { objc::object::class_of(screen) }
        .is_some_and(|c| objc::class::lookup(c"NSScreen").is_some_and(|k| std::ptr::eq(c, k)));
    is_screen.then(|| display_of(screen))
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSScreen").expect("declared before its methods");
    class_methods!(class, {
        "mainScreen" => main_screen, c"@16@0:8";
        "deepestScreen" => deepest_screen, c"@16@0:8";
        "screens" => screens, c"@16@0:8";
        "screensHaveSeparateSpaces" => screens_have_separate_spaces, c"B16@0:8";
    });
    methods!(class, {
        "frame" => frame, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "visibleFrame" => visible_frame, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "backingScaleFactor" => backing_scale_factor, c"d16@0:8";
        "maximumFramesPerSecond" => maximum_frames_per_second, c"q16@0:8";
        "minimumRefreshInterval" => refresh_interval, c"d16@0:8";
        "maximumRefreshInterval" => refresh_interval, c"d16@0:8";
        "displayUpdateGranularity" => refresh_interval, c"d16@0:8";
        "depth" => depth, c"i16@0:8";
        "colorSpace" => color_space, c"@16@0:8";
        "deviceDescription" => device_description, c"@16@0:8";
        "localizedName" => localized_name, c"@16@0:8";
        "safeAreaInsets" => safe_area_insets, c"{NSEdgeInsets=dddd}16@0:8";
        "auxiliaryTopLeftArea" => zero_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "auxiliaryTopRightArea" => zero_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "userSpaceScaleFactor" => user_space_scale_factor, c"d16@0:8";
        "maximumExtendedDynamicRangeColorComponentValue" => edr_limit, c"d16@0:8";
        "maximumPotentialExtendedDynamicRangeColorComponentValue" => edr_limit, c"d16@0:8";
        "maximumReferenceExtendedDynamicRangeColorComponentValue" => edr_reference_limit, c"d16@0:8";
        "convertRectToBacking:" => convert_rect_to_backing, c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "convertRectFromBacking:" => convert_rect_from_backing, c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "supportedWindowDepths" => supported_window_depths, c"^i16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_refresh_intervals_are_available_without_a_display_mode() {
        crate::bootstrap();
        let screen = screen_for(0);
        let class = objc::class::lookup(c"NSScreen").unwrap();
        for name in [
            "minimumRefreshInterval",
            "maximumRefreshInterval",
            "displayUpdateGranularity",
        ] {
            let selector = objc::sel::get(name);
            let method = class
                .method(selector)
                .expect("refresh method is registered");
            // SAFETY: the registered method returns NSTimeInterval for NSScreen.
            let call: unsafe extern "C" fn(Id, Sel) -> f64 =
                unsafe { std::mem::transmute(method.implementation()) };
            assert_eq!(unsafe { call(screen, selector) }, 1.0 / 60.0);
        }
    }

    /// One object per display, which is what makes `+mainScreen ==
    /// [window screen]` mean anything.
    #[test]
    fn a_display_has_one_screen_object() {
        crate::bootstrap();
        let Some(display) = sdl3::video::displays().first().copied() else {
            eprintln!("skipped: this host has no display");
            return;
        };
        assert_eq!(screen_for(display), screen_for(display));
    }

    /// And a screen's frame has the display's size, whatever the origin
    /// conversion did to it.
    #[test]
    fn a_screens_frame_is_its_displays_size() {
        crate::bootstrap();
        let Some(display) = sdl3::video::displays().first().copied() else {
            eprintln!("skipped: this host has no display");
            return;
        };
        let bounds = sdl3::video::display_bounds(display).expect("a display has bounds");
        let screen = screen_for(display);
        // SAFETY: `-frame` is `NSRect (id, SEL)`.
        let rect = unsafe { frame(screen, objc::sel::get("frame")) };
        assert_eq!(rect.size.width, bounds.w as f64);
        assert_eq!(rect.size.height, bounds.h as f64);
    }

    /// Calls the registered `NSScreen` method `name` with only `(self, _cmd)`.
    fn screen_method<R>(screen: Id, name: &str) -> R {
        let class = objc::class::lookup(c"NSScreen").unwrap();
        let selector = objc::sel::get(name);
        let method = class.method(selector).expect("method is registered");
        // SAFETY: the caller names the method's real return type.
        let call: unsafe extern "C" fn(Id, Sel) -> R =
            unsafe { std::mem::transmute(method.implementation()) };
        unsafe { call(screen, selector) }
    }

    #[test]
    fn a_screen_has_a_nonempty_localized_name() {
        crate::bootstrap();
        let name: Id = screen_method(screen_for(0), "localizedName");
        assert!(!name.is_null());
        assert!(!unsafe { foundation::string::bytes(name) }.is_empty());
    }

    #[test]
    fn a_screen_without_a_camera_housing_has_no_safe_area_inset() {
        crate::bootstrap();
        let screen = screen_for(0);
        let insets: crate::controls::NSEdgeInsets = screen_method(screen, "safeAreaInsets");
        assert_eq!(insets, crate::controls::NSEdgeInsets::default());
        for name in ["auxiliaryTopLeftArea", "auxiliaryTopRightArea"] {
            let rect: NSRect = screen_method(screen, name);
            assert_eq!(rect, NSRect::default());
        }
    }

    #[test]
    fn extended_dynamic_range_limits_describe_an_sdr_display() {
        crate::bootstrap();
        let screen = screen_for(0);
        let limit = |n| screen_method::<f64>(screen, n);
        assert_eq!(limit("maximumExtendedDynamicRangeColorComponentValue"), 1.0);
        assert_eq!(
            limit("maximumPotentialExtendedDynamicRangeColorComponentValue"),
            1.0
        );
        assert_eq!(
            limit("maximumReferenceExtendedDynamicRangeColorComponentValue"),
            0.0
        );
    }

    #[test]
    fn backing_conversion_round_trips_through_the_scale_factor() {
        crate::bootstrap();
        let screen = screen_for(0);
        let rect = NSRect {
            origin: foundation::geometry::NSPoint { x: 3.0, y: 5.0 },
            size: foundation::geometry::NSSize {
                width: 40.0,
                height: 20.0,
            },
        };
        let scale: f64 = screen_method(screen, "userSpaceScaleFactor");
        // SAFETY: the conversions are `NSRect (id, SEL, NSRect)`.
        let there =
            unsafe { convert_rect_to_backing(screen, objc::sel::get("backingScaleFactor"), rect) };
        assert_eq!(there.size.width, 40.0 * scale);
        let back = unsafe {
            convert_rect_from_backing(screen, objc::sel::get("backingScaleFactor"), there)
        };
        assert!((back.size.width - 40.0).abs() < 1e-9);
        assert!((back.origin.y - 5.0).abs() < 1e-9);
    }

    #[test]
    fn supported_window_depths_end_in_zero() {
        crate::bootstrap();
        let depths: *const i32 = screen_method(screen_for(0), "supportedWindowDepths");
        let depth: i32 = screen_method(screen_for(0), "depth");
        // SAFETY: a two-element static array.
        unsafe {
            assert_eq!(*depths, depth);
            assert_eq!(*depths.add(1), 0);
        }
    }

    #[test]
    fn a_push_stopped_short_of_the_edge_is_pressed_against_it() {
        // 20 from the end, moving 12 a step: the next one would have crossed.
        assert_eq!(press(0, 80.0, 12.0, 0, 101), 1);
        assert_eq!(press(0, 20.0, -12.0, 0, 100), -1);
        assert_eq!(press(0, 50.0, 2.0, 0, 100), 0);
        assert_eq!(press(0, 99.5, -1.0, 0, 100), 0, "heading away");
    }

    #[test]
    fn a_held_edge_survives_a_wobble_but_not_a_move_back() {
        assert_eq!(press(1, 97.5, -0.5, 0, 100), 1);
        assert_eq!(press(1, 95.0, -1.0, 0, 100), 0);
        assert_eq!(press(-1, 0.0, 0.0, 0, 100), -1, "sliding along it");
    }

    #[test]
    fn a_pointer_over_the_chrome_is_not_pressed() {
        assert_eq!(press(0, 10.0, -3.0, 28, 50), 0);
    }

    #[test]
    fn a_global_warp_is_translated_to_window_coordinates() {
        assert_eq!(local_warp_point((420.0, 310.0), (100, 50)), (320.0, 260.0));
    }
}

/// `CGError CGWarpMouseCursorPosition(CGPoint)` - move the pointer, in global
/// screen coordinates.
///
/// No event is posted, but the jump is not lost either: the next motion event's
/// deltas include it (see `event::note_warp_delta`).
///
/// SDL treats an in-window warp during relative mode as virtual, so the first
/// warp to a target momentarily releases relative mode and moves the physical
/// desktop cursor, as CoreGraphics promises. Repeated warps to that same target
/// only refresh SDL's virtual position: the physical cursor is already there,
/// and toggling relative mode every frame would flush real camera motion.
/// `kCGErrorSuccess` is `0`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGWarpMouseCursorPosition(point: foundation::geometry::NSPoint) -> i32 {
    let asked = point;
    let point = (point.x as f32, point.y as f32);
    // An edge hold is not the guest's relative mode: the warp is a plain one.
    end_edge_hold();
    let windows = crate::window::sdl_windows();
    let target = crate::event::pointer_window();
    let origin = target
        .and_then(crate::window::global_position)
        .unwrap_or((0, 0));
    let local = local_warp_point(point, origin);
    let from = crate::event::pointer_position();
    // Measured from the cursor the guest is told about: the frozen point when
    // it is disassociated, which a repeated warp to one target leaves exactly
    // where it was, and otherwise the answer the window system gives.
    let moved = match mouse_is_associated() {
        true => {
            let at = crate::cgevent::global_pointer();
            (asked.x - at.x, asked.y - at.y)
        }
        false => (f64::from(local.0 - from.0), f64::from(local.1 - from.1)),
    };
    crate::event::note_warp_delta(moved.0, moved.1);
    crate::event::warp_pointer(local.0, local.1);
    if mouse_is_associated()
        && let Some(window) = target
        && recentre(window, point)
    {
        return 0;
    }

    let relative: Vec<_> = windows
        .iter()
        .copied()
        .filter(|window| sdl3::video::relative_mouse_mode(*window))
        .collect();
    if relative.is_empty() {
        forget_relative_warp();
        crate::event::expect_warp_echo(from, local);
        warp_physical(target, point, local);
        return 0;
    }
    if known_relative_warp() == Some(point) {
        // Relative deltas advance SDL's virtual absolute position. Refresh it
        // even when the physical cursor needs no second warp, or SDL exposes
        // the accumulated drag when relative mode ends.
        for window in relative {
            let origin = crate::window::global_position(window).unwrap_or((0, 0));
            let local = local_warp_point(point, origin);
            sdl3::video::warp_mouse_in_window(window, local.0, local.1);
        }
        return 0;
    }

    crate::event::expect_warp_echo(from, local);
    let mut released = true;
    for window in &relative {
        released &= sdl3::video::set_relative_mouse_mode(*window, false);
    }
    let warped = warp_physical(target, point, local);
    for window in relative {
        sdl3::video::set_relative_mouse_mode(window, true);
    }
    match released && warped {
        true => remember_relative_warp(point),
        false => forget_relative_warp(),
    }
    0
}

/// `CGError CGSetLocalEventsSuppressionInterval(CFTimeInterval)`.
///
/// How long real mouse input is ignored after a warp. [`CGWarpMouseCursorPosition`]
/// never suppresses anything, which is the interval games set - zero, so mouse
/// look keeps its deltas. Any other interval is reported once rather than kept.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGSetLocalEventsSuppressionInterval(seconds: f64) -> i32 {
    static TOLD: std::sync::Once = std::sync::Once::new();
    if seconds != 0.0 {
        TOLD.call_once(|| {
            eprintln!(
                "rosemary: appkit: CGSetLocalEventsSuppressionInterval({seconds}) is not honoured; \
                 input is never suppressed after a warp"
            )
        });
    }
    0
}

/// Move the physical cursor to a desktop point, `local` being the same point
/// in `window`'s coordinates.
///
/// On Wayland the desktop is this runtime's own (see
/// `window::global_position`) and SDL would measure a global warp from its
/// placeholder window origin, so the warp goes through the window instead.
fn warp_physical(window: Option<sdl3::Window>, global: (f32, f32), local: (f32, f32)) -> bool {
    if !sdl3::video::is_wayland() && sdl3::video::warp_mouse_global(global.0, global.1) {
        return true;
    }
    window.is_some_and(|window| sdl3::video::warp_mouse_in_window(window, local.0, local.1))
}

fn local_warp_point(point: (f32, f32), origin: (i32, i32)) -> (f32, f32) {
    (point.0 - origin.0 as f32, point.1 - origin.1 as f32)
}

/// The desktop position reached by the last physical warp. It is valid only
/// while the current relative-mode session continues.
static RELATIVE_WARP: std::sync::Mutex<Option<(f32, f32)>> = std::sync::Mutex::new(None);

fn known_relative_warp() -> Option<(f32, f32)> {
    *RELATIVE_WARP.lock().unwrap_or_else(|e| e.into_inner())
}

fn remember_relative_warp(point: (f32, f32)) {
    *RELATIVE_WARP.lock().unwrap_or_else(|e| e.into_inner()) = Some(point);
}

fn forget_relative_warp() {
    *RELATIVE_WARP.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A guest that keeps warping the cursor back to one point while a button is
/// held - Studio's right-drag camera - is doing mouse look without
/// disassociating. Following each warp physically would fight the user's hand,
/// so after the second such warp the window goes into relative mode and the
/// warps only move the pointer the guest is told about.
#[derive(Clone, Copy)]
struct Recentring {
    window: usize,
    target: (f32, f32),
    at: std::time::Instant,
    /// Real motion arrived since the warp to `target`: the next warp there
    /// takes back the user's movement rather than repeating a snap.
    moved: bool,
    active: bool,
}

static RECENTRING: std::sync::Mutex<Option<Recentring>> = std::sync::Mutex::new(None);

const RECENTRE_WITHIN: std::time::Duration = std::time::Duration::from_millis(250);
const RECENTRE_IDLE: std::time::Duration = std::time::Duration::from_millis(300);

fn same_point(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() < 0.5 && (a.1 - b.1).abs() < 0.5
}

fn button_held() -> bool {
    sdl3::video::mouse_state().is_some_and(|(_, _, buttons)| buttons != 0)
}

/// Note a guest warp to `point`. True when it was taken as a recentring one,
/// which moves only the pointer the guest sees.
fn recentre(window: sdl3::Window, point: (f32, f32)) -> bool {
    let now = std::time::Instant::now();
    let mut state = RECENTRING.lock().unwrap_or_else(|e| e.into_inner());
    let same = state.filter(|r| r.window == window as usize);
    match same {
        Some(r) if r.active && same_point(r.target, point) => {
            *state = Some(Recentring { at: now, ..r });
            return true;
        }
        // A warp anywhere else is a real one: the user has to see it.
        Some(r) if r.active => {
            sdl3::video::set_relative_mouse_mode(r.window as sdl3::Window, false);
        }
        Some(r)
            if r.moved
                && now - r.at < RECENTRE_WITHIN
                && same_point(r.target, point)
                && button_held() =>
        {
            sdl3::video::set_hint(sdl3::video::HINT_RELATIVE_CURSOR_VISIBLE, c"1");
            if sdl3::video::set_relative_mouse_mode(window, true) {
                forget_relative_warp();
                *state = Some(Recentring {
                    at: now,
                    active: true,
                    ..r
                });
                return true;
            }
        }
        _ => {}
    }
    *state = Some(Recentring {
        window: window as usize,
        target: point,
        at: now,
        moved: false,
        active: false,
    });
    false
}

pub(crate) fn recentring() -> bool {
    RECENTRING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|r| r.active)
}

/// Record real pointer motion, for [`recentre`] to tell a warp that undoes
/// the user's movement from one that repeats a snap.
pub(crate) fn note_recentring_motion() {
    if let Some(r) = RECENTRING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        r.moved = true;
    }
}

/// Leave recentring once the guest has stopped warping, putting the physical
/// cursor where the guest believes it is. `from` is where SDL last reported
/// the cursor, in the window's coordinates, for the warp's echo to be dropped.
/// True when it ended, with the pointer now at [`crate::event::pointer_position`].
pub(crate) fn end_recentring_if_idle(from: (f32, f32)) -> bool {
    let ended = {
        let mut state = RECENTRING.lock().unwrap_or_else(|e| e.into_inner());
        match *state {
            Some(r) if r.active && r.at.elapsed() > RECENTRE_IDLE => state.take(),
            _ => None,
        }
    };
    let Some(r) = ended else {
        return false;
    };
    let window = r.window as sdl3::Window;
    let local = crate::event::pointer_position();
    // Before relative mode ends, or SDL exposes the position it integrated.
    sdl3::video::warp_mouse_in_window(window, local.0, local.1);
    sdl3::video::set_relative_mouse_mode(window, false);
    crate::event::expect_warp_echo(from, local);
    let origin = crate::window::global_position(window).unwrap_or((0, 0));
    let global = (local.0 + origin.0 as f32, local.1 + origin.1 as f32);
    warp_physical(Some(window), global, local);
    true
}

/// Drop recentring without moving anything, for a window losing focus: SDL
/// has already left relative mode, and a cursor now over another program must
/// not be pulled back to where the guest last had it.
pub(crate) fn forget_recentring() {
    let Some(r) = RECENTRING.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    if r.active {
        sdl3::video::set_relative_mouse_mode(r.window as sdl3::Window, false);
    }
}

/// `CGError CGAssociateMouseAndMouseCursorPosition(boolean_t connected)`.
///
/// Disassociating is the second half of mouse look: a game hides the cursor and
/// then breaks the link between pointer and cursor so the cursor stops moving
/// while deltas keep arriving. SDL's relative mouse mode is the same thing, so
/// that is what this turns on.
///
/// The mode is per-window in SDL and process-wide in CoreGraphics, hence
/// [`apply_mouse_association`] over every window rather than the key one.
/// `kCGErrorSuccess` is `0`, and stays the answer on a host whose SDL is too
/// old for the mode: a program told this failed treats mouse look as
/// unavailable rather than running without capture.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32 {
    // The cursor stops where it is, which is what the guest is told from here.
    if connected == 0 && mouse_is_associated() {
        crate::event::settle_pointer();
    }
    ASSOCIATED.store(connected != 0, std::sync::atomic::Ordering::Release);
    apply_mouse_association();
    0
}

/// Whether the pointer and the cursor are meant to be moving together.
///
/// Read back rather than merely written: a caller that disassociates and then
/// asks is entitled to be told what it set.
static ASSOCIATED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Whether the last [`CGAssociateMouseAndMouseCursorPosition`] asked for the
/// cursor to follow the pointer.
pub fn mouse_is_associated() -> bool {
    ASSOCIATED.load(std::sync::atomic::Ordering::Acquire)
}

/// `ROSEMARY_CONFINE_POINTER=1`: keep the pointer inside whichever window has
/// focus.
///
/// FIXME (ACCURACY): a workaround, not AppKit. Some ports confine the cursor
/// with a Win32 `ClipCursor` emulation that only clamps once and relies on
/// macOS delivering drags outside the window to warp it back; Wayland neither
/// delivers those nor lets a warp reach a pointer outside the window. Life Is
/// Strange's drag interactions stop short without this, on macOS too.
pub const CONFINE_ENV: &str = "ROSEMARY_CONFINE_POINTER";

fn confining() -> bool {
    #[dynamic]
    static ON: bool = std::env::var_os(CONFINE_ENV).is_some_and(|v| v != "0");
    *ON
}

/// Confine the pointer to `window`'s content if [`CONFINE_ENV`] asks for it.
///
/// The content and not the frame: over the chrome the pointer is the chrome's,
/// and the guest stops hearing from it exactly as if it had left the window.
/// Called again on a resize, since the rectangle does not follow the window.
pub fn confine_pointer(window: Id) {
    if !confining() {
        return;
    }
    // SAFETY: null or an object this runtime allocated.
    let sdl = unsafe { crate::window::sdl_window(window) };
    if sdl.is_null() {
        return;
    }
    if let Some(rect) = content_rect(window, sdl) {
        sdl3::video::set_mouse_rect(sdl, Some(rect));
    }
    sdl3::video::set_mouse_grab(sdl, true);
}

/// The content area of `window` in SDL window coordinates.
fn content_rect(window: Id, sdl: sdl3::Window) -> Option<sdl3::Rect> {
    let (w, h) = sdl3::video::window_size(sdl)?;
    let inset = crate::window::content_inset(window).ceil() as i32;
    (w > 0 && h > inset).then_some(sdl3::Rect {
        x: 0,
        y: inset,
        w,
        h: h - inset,
    })
}

/// A window put into relative mode by [`hold_at_edge`], and the edge of its
/// confinement each axis is held against: -1 the low one, 1 the high one, 0
/// neither.
#[derive(Clone, Copy)]
struct Hold {
    id: u32,
    x: i8,
    y: i8,
}

static EDGE_HOLD: std::sync::Mutex<Option<Hold>> = std::sync::Mutex::new(None);

/// How far a held axis comes back inside before it is let go, in points. With
/// none, a hand pressing against an edge wobbles straight out of the hold.
const RELEASE_MARGIN: f32 = 2.0;

/// Keep deltas coming from a confined pointer pushed against its edge.
///
/// A confined pointer stops at the edge, and one that does not move reports
/// nothing: on Wayland no event arrives at all. The ports [`CONFINE_ENV`] is
/// for measure drags in deltas and count on them continuing while the pointer
/// strays and is warped back. So at the edge the window enters relative mode,
/// whose deltas are raw input and whose position SDL clamps to the same
/// rectangle, and leaves it once the pointer heads back inside. The visible
/// cursor holds still meanwhile.
pub(crate) fn hold_at_edge(id: u32, x: f32, y: f32, dx: f32, dy: f32) {
    if !confining() || !mouse_is_associated() {
        return;
    }
    let window = crate::event::object_for(id);
    // SAFETY: null or an object this runtime allocated.
    let sdl = unsafe { crate::window::sdl_window(window) };
    if sdl.is_null() {
        return;
    }
    let Some(rect) = content_rect(window, sdl) else {
        return;
    };
    let mut hold = EDGE_HOLD.lock().unwrap_or_else(|e| e.into_inner());
    let current = hold.filter(|h| h.id == id && sdl3::video::relative_mouse_mode(sdl));
    let next = Hold {
        id,
        x: press(current.map_or(0, |h| h.x), x, dx, rect.x, rect.w),
        y: press(current.map_or(0, |h| h.y), y, dy, rect.y, rect.h),
    };
    let pressed = next.x != 0 || next.y != 0;
    match (current.is_some(), pressed) {
        (false, true) => {
            sdl3::video::set_hint(sdl3::video::HINT_RELATIVE_CURSOR_VISIBLE, c"1");
            *hold = sdl3::video::set_relative_mouse_mode(sdl, true).then_some(next);
        }
        (true, true) => *hold = Some(next),
        (true, false) => {
            *hold = None;
            // The in-window warp is the lock's position hint on Wayland, where
            // the physical cursor then reappears; elsewhere the host cursor
            // kept moving under the grab and has to be brought back.
            sdl3::video::warp_mouse_in_window(sdl, x, y);
            sdl3::video::set_relative_mouse_mode(sdl, false);
            if !sdl3::video::is_wayland() {
                let origin = crate::window::global_position(sdl).unwrap_or((0, 0));
                let global = (x + origin.0 as f32, y + origin.1 as f32);
                warp_physical(Some(sdl), global, (x, y));
            }
        }
        (false, false) => *hold = None,
    }
}

/// Which edge of `[start, start + len)` one axis is pressed against, having
/// moved `d` to `p` while `held` was.
///
/// Not only a pointer that reached the edge: KWin does not clamp a confined
/// pointer, it drops a move that would leave the region, so a quick push stops
/// up to a step short of the edge with nothing more to report. An axis is
/// pressed once another step like this one would cross, and stays pressed
/// until it has come back more than [`RELEASE_MARGIN`] inside.
fn press(held: i8, p: f32, d: f32, start: i32, len: i32) -> i8 {
    let to_low = p - start as f32;
    // SDL's relative mode clamps to the last pixel, one short of the end.
    let to_high = (start + len) as f32 - 1.0 - p;
    // Outside is a pointer the confinement has not caught, over the chrome.
    if to_low < 0.0 || to_high < -1.0 {
        return 0;
    }
    let reach = 2.0 * d.abs() + 1.0;
    match held {
        -1 if !(d > 0.0 && to_low > RELEASE_MARGIN) => -1,
        1 if !(d < 0.0 && to_high > RELEASE_MARGIN) => 1,
        _ if d < 0.0 && to_low < reach => -1,
        _ if d > 0.0 && to_high < reach => 1,
        _ => 0,
    }
}

/// Take the window [`hold_at_edge`] put into relative mode back out of it,
/// without moving anything: the caller is about to put the pointer somewhere.
fn end_edge_hold() {
    let hold = EDGE_HOLD.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(hold) = hold.filter(|_| mouse_is_associated()) else {
        return;
    };
    let held = crate::window::sdl_windows()
        .into_iter()
        .find(|w| sdl3::video::window_id(*w) == hold.id);
    if let Some(sdl) = held {
        sdl3::video::set_relative_mouse_mode(sdl, false);
    }
}

/// Put every window into relative mouse mode, or take it out, to match the last
/// [`CGAssociateMouseAndMouseCursorPosition`].
///
/// Called again when a window takes focus, because SDL leaves the mode when
/// focus goes and a window made after the guest disassociated never had it.
pub fn apply_mouse_association() {
    *EDGE_HOLD.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *RECENTRING.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let relative = !mouse_is_associated();
    // Disassociating is not hiding: on macOS the cursor stays on screen where
    // it was, and only `+[NSCursor hide]` takes it away - which this tree
    // already counts in `crate::cursor`. SDL's relative mode hides the pointer
    // itself, so the hint is what stops it from making that decision twice.
    sdl3::video::set_hint(sdl3::video::HINT_RELATIVE_CURSOR_VISIBLE, c"1");
    let mut changed = false;
    for window in crate::window::sdl_windows() {
        if sdl3::video::relative_mouse_mode(window) != relative {
            if !relative {
                // Reset both positions on release. The in-window warp supplies
                // SDL's restore point; the global warp after disabling reaches
                // the physical cursor even on backends where those differ.
                let point = crate::event::pointer_position();
                sdl3::video::warp_mouse_in_window(window, point.0, point.1);
                sdl3::video::set_relative_mouse_mode(window, false);
                let origin = crate::window::global_position(window).unwrap_or((0, 0));
                let global = (point.0 + origin.0 as f32, point.1 + origin.1 as f32);
                warp_physical(Some(window), global, point);
            } else {
                sdl3::video::set_relative_mouse_mode(window, true);
            }
            changed = true;
        }
    }
    if changed {
        forget_relative_warp();
    }
}

/// `bool CGEventSourceKeyState(CGEventSourceStateID, CGKeyCode)`.
///
/// False for every key. This asks the *window server* whether a key is down
/// right now, independently of the event queue, and there is no window server
/// here to ask - SDL's keyboard state is this process's own view and answers a
/// different question, one that is wrong in exactly the case the call exists
/// for (a key pressed while the program was not focused).
///
/// False rather than refused because the return type has no error value: a
/// caller gets "not pressed", which is the answer for all but one key at any
/// moment anyway.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGEventSourceKeyState(_source: i32, _key: u16) -> u8 {
    0
}
