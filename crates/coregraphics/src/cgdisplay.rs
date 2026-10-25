//! `CGDisplay`: the host's displays, their modes, and their gamma tables.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;

use crate::{CGPoint, CGRect, CGSize};

/// `CGDirectDisplayID CGMainDisplayID(void)`.
///
/// CoreGraphics is below AppKit and this is its only entry point here, which is
/// the argument for it living in this file rather than a crate of its own: what
/// a `CGDirectDisplayID` *is* in this tree is decided by the AppKit screen's
/// `device_description`, which publishes an SDL display id under
/// `NSScreenNumber` for `CVDisplayLink` to open. Two definitions of that would
/// be two display numbering schemes.
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
            "rosemary: coregraphics: the program set a display gamma table; it is kept but not \
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
pub unsafe extern "C" fn CGDisplayRestoreColorSyncSettings() {
    GAMMA.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// `CGRect CGDisplayBounds(CGDirectDisplayID)`.
///
/// CoreGraphics measures the global display space from the **top left** with y
/// increasing downwards, which is SDL's convention and not `-[NSScreen frame]`'s
/// - so this is not the AppKit screen's frame with a different receiver, and
/// converting it as though it were puts a second display on the wrong side of
/// the first.
pub unsafe extern "C" fn CGDisplayBounds(display: u32) -> CGRect {
    match sdl3::video::display_bounds(display) {
        Some(b) => CGRect {
            origin: CGPoint {
                x: b.x as f64,
                y: b.y as f64,
            },
            size: CGSize {
                width: b.w as f64,
                height: b.h as f64,
            },
        },
        // What CoreGraphics answers for a display id it does not know.
        None => CGRect::default(),
    }
}

/// `size_t CGDisplayPixelsWide(CGDirectDisplayID)` and `CGDisplayPixelsHigh`.
///
/// The display's size in the global space's own units, which is what
/// [`CGDisplayBounds`] reports and what a guest sizes a full-screen window from.
pub unsafe extern "C" fn CGDisplayPixelsWide(display: u32) -> u64 {
    // SAFETY: a plain call.
    unsafe { CGDisplayBounds(display) }.size.width as u64
}

pub unsafe extern "C" fn CGDisplayPixelsHigh(display: u32) -> u64 {
    // SAFETY: a plain call.
    unsafe { CGDisplayBounds(display) }.size.height as u64
}

/// `size_t CGDisplayBitsPerPixel(CGDirectDisplayID)`
///
/// 32, matching the `IO32BitDirectPixels` encoding every mode reports; zero
/// for a display id that is not online, as CoreGraphics answers.
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
pub unsafe extern "C" fn CGDisplayMoveCursorToPoint(display: u32, point: CGPoint) -> i32 {
    // SAFETY: a plain call.
    let origin = unsafe { CGDisplayBounds(display) }.origin;
    let global = CGPoint {
        x: origin.x + point.x,
        y: origin.y + point.y,
    };
    crate::quartz_event::warp_pointer(global);
    0
}

/// `kCGErrorFailure`: what a request for something this host cannot do answers.
const CG_ERROR_FAILURE: i32 = 1001;

/// `CGError CGDisplayCapture(CGDirectDisplayID)` and `CGCaptureAllDisplays`.
///
/// Refused: SDL gives no exclusive claim on a display, and a caller told it has one
/// would go on to assume it can change the mode. The caller is expected to carry on
/// with a window, as it does when another process holds the display.
pub unsafe extern "C" fn CGDisplayCapture(_display: u32) -> i32 {
    CG_ERROR_FAILURE
}

pub unsafe extern "C" fn CGCaptureAllDisplays() -> i32 {
    CG_ERROR_FAILURE
}

/// `CGError CGReleaseAllDisplays(void)`: nothing is ever captured.
pub unsafe extern "C" fn CGReleaseAllDisplays() -> i32 {
    0
}

/// `CGError CGDisplaySetDisplayMode(display, mode, CFDictionaryRef options)`.
///
/// Refused: this host's display mode is not the guest's to change.
pub unsafe extern "C" fn CGDisplaySetDisplayMode(
    _display: u32,
    _mode: *const c_void,
    _options: *const c_void,
) -> i32 {
    CG_ERROR_FAILURE
}

/// The fade-reservation token written by [`CGAcquireDisplayFadeReservation`].
const FADE_TOKEN: u32 = 1;

/// `CGError CGAcquireDisplayFadeReservation(CGDisplayReservationInterval, token *)`.
///
/// Granted, and the fade itself is a no-op: there is no gamma ramp to fade here,
/// and a display that does not fade is merely less polished, not wrong.
pub unsafe extern "C" fn CGAcquireDisplayFadeReservation(_seconds: f32, token: *mut u32) -> i32 {
    if !token.is_null() {
        // SAFETY: an out parameter the caller sized.
        unsafe { token.write(FADE_TOKEN) };
    }
    0
}

pub unsafe extern "C" fn CGReleaseDisplayFadeReservation(_token: u32) -> i32 {
    0
}

/// `CGError CGDisplayFade(token, seconds, startBlend, endBlend, r, g, b, synchronous)`.
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

/// `boolean_t CGDisplayIsMain(CGDirectDisplayID)`
///
/// The primary display, which is the one SDL lists first.
pub unsafe extern "C" fn CGDisplayIsMain(display: u32) -> u32 {
    (display == sdl3::video::primary_display()) as u32
}

/// `boolean_t CGDisplayIsOnline(CGDirectDisplayID)`
///
/// Whether SDL still lists it. A display unplugged between two calls is exactly
/// the case the question exists for.
pub unsafe extern "C" fn CGDisplayIsOnline(display: u32) -> u32 {
    sdl3::video::displays().contains(&display) as u32
}

/// `boolean_t CGDisplayIsBuiltin(CGDirectDisplayID)`
///
/// False: SDL does not say which panel is the laptop's own, and claiming a
/// built-in one would have callers look for brightness and lid state that is
/// not modelled.
pub unsafe extern "C" fn CGDisplayIsBuiltin(_display: u32) -> u32 {
    0
}

/// `CGDirectDisplayID CGDisplayPrimaryDisplay(CGDirectDisplayID)`
///
/// With no mirroring modelled, every display is its own primary.
pub unsafe extern "C" fn CGDisplayPrimaryDisplay(display: u32) -> u32 {
    display
}

/// `CGError CGDisplayRelease(CGDirectDisplayID)`
///
/// `CGDisplayCapture` is not modelled, so there is never anything to release;
/// kCGErrorSuccess.
pub unsafe extern "C" fn CGDisplayRelease(_display: u32) -> i32 {
    0
}

/// `double CGDisplayRotation(CGDirectDisplayID)`
///
/// SDL's display orientation is not mapped to degrees; an unrotated panel.
pub unsafe extern "C" fn CGDisplayRotation(_display: u32) -> f64 {
    0.0
}

/// `boolean_t CGDisplayIsAsleep(CGDirectDisplayID)`
///
/// False: a display this process can enumerate is one SDL is talking to, and
/// there is no sleep state to read. Answering true would have a caller skip the
/// screen entirely.
pub unsafe extern "C" fn CGDisplayIsAsleep(_display: u32) -> u32 {
    0
}

/// `CGDirectDisplayID CGDisplayMirrorsDisplay(CGDirectDisplayID)`
///
/// `kCGNullDirectDisplay`, because nothing here mirrors - the same fact
/// [`CGGetOnlineDisplayList`] already relies on to answer for both display lists.
/// A caller uses this to drop a mirror from its own list, and zero keeps every
/// display in it.
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
pub unsafe extern "C" fn CGDisplayModelNumber(_display: u32) -> u32 {
    // `kDisplayProductIDGeneric`, the value CoreGraphics itself reports for a
    // display whose product id it could not read.
    0x717
}

pub unsafe extern "C" fn CGDisplayVendorNumber(_display: u32) -> u32 {
    // `kDisplayVendorIDUnknown`.
    0
}

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
pub unsafe extern "C" fn CGDisplayModeGetRefreshRate(mode: *const c_void) -> f64 {
    sdl_mode_of(mode).map_or(0.0, |m| m.refresh_rate.max(0.0) as f64)
}

/// `size_t CGDisplayModeGetWidth(CGDisplayModeRef)`, in points.
pub unsafe extern "C" fn CGDisplayModeGetWidth(mode: *const c_void) -> usize {
    sdl_mode_of(mode).map_or(0, |m| m.w.max(0) as usize)
}

pub unsafe extern "C" fn CGDisplayModeGetHeight(mode: *const c_void) -> usize {
    sdl_mode_of(mode).map_or(0, |m| m.h.max(0) as usize)
}

fn pixels(points: i32, density: f32) -> usize {
    let density = if density > 0.0 { density } else { 1.0 };
    (points.max(0) as f32 * density).round() as usize
}

/// `size_t CGDisplayModeGetPixelWidth(CGDisplayModeRef)`, points times density.
pub unsafe extern "C" fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize {
    sdl_mode_of(mode).map_or(0, |m| pixels(m.w, m.pixel_density))
}

pub unsafe extern "C" fn CGDisplayModeGetPixelHeight(mode: *const c_void) -> usize {
    sdl_mode_of(mode).map_or(0, |m| pixels(m.h, m.pixel_density))
}

/// `uint32_t CGDisplayModeGetIOFlags(CGDisplayModeRef)`
///
/// `kDisplayModeValidFlag | kDisplayModeSafeFlag`: every mode SDL lists can be set.
pub unsafe extern "C" fn CGDisplayModeGetIOFlags(mode: *const c_void) -> u32 {
    sdl_mode_of(mode).map_or(0, |_| 0x1 | 0x2)
}

/// `bool CGDisplayModeIsUsableForDesktopGUI(CGDisplayModeRef)`
pub unsafe extern "C" fn CGDisplayModeIsUsableForDesktopGUI(mode: *const c_void) -> u8 {
    sdl_mode_of(mode).is_some() as u8
}

/// `CFStringRef CGDisplayModeCopyPixelEncoding(CGDisplayModeRef)`
///
/// `IO32BitDirectPixels`, eight bits a channel, which is what every mode a
/// host compositor offers is to a program choosing among them.
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
pub unsafe extern "C" fn CGDisplayModeRelease(_mode: *const c_void) {}

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
pub unsafe extern "C" fn CGDisplayRegisterReconfigurationCallback(
    _callback: *const c_void,
    _context: *mut c_void,
) -> i32 {
    CG_SUCCESS
}

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
pub unsafe extern "C" fn CGGetDisplaysWithRect(
    rect: CGRect,
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
pub unsafe extern "C" fn CGDisplayScreenSize(display: u32) -> CGSize {
    const MM_PER_INCH: f64 = 25.4;
    match sdl3::video::display_bounds(display) {
        Some(b) => CGSize {
            width: b.w as f64 / ASSUMED_DPI * MM_PER_INCH,
            height: b.h as f64 / ASSUMED_DPI * MM_PER_INCH,
        },
        None => CGSize::default(),
    }
}

/// `CGOpenGLDisplayMask CGDisplayIDToOpenGLDisplayMask(CGDirectDisplayID)`.
///
/// One bit per display, which is what a pixel-format attribute list carries.
/// The mask is only ever handed back to a CGL call this tree answers, so the
/// numbering is this tree's own; a display past the thirty-second gets no bit,
/// as it does on a Mac.
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
pub unsafe extern "C" fn CGDisplayIOServicePort(_display: u32) -> u32 {
    0
}

exports::exports! {
    functions {
        "CGMainDisplayID" => CGMainDisplayID,
        "CGGetDisplayTransferByTable" => CGGetDisplayTransferByTable,
        "CGSetDisplayTransferByTable" => CGSetDisplayTransferByTable,
        "CGDisplayRestoreColorSyncSettings" => CGDisplayRestoreColorSyncSettings,
        "CGDisplayBounds" => CGDisplayBounds,
        "CGDisplayPixelsWide" => CGDisplayPixelsWide,
        "CGDisplayPixelsHigh" => CGDisplayPixelsHigh,
        "CGDisplayBitsPerPixel" => CGDisplayBitsPerPixel,
        "CGDisplayMoveCursorToPoint" => CGDisplayMoveCursorToPoint,
        "CGDisplayCapture" => CGDisplayCapture,
        "CGCaptureAllDisplays" => CGCaptureAllDisplays,
        "CGReleaseAllDisplays" => CGReleaseAllDisplays,
        "CGDisplaySetDisplayMode" => CGDisplaySetDisplayMode,
        "CGAcquireDisplayFadeReservation" => CGAcquireDisplayFadeReservation,
        "CGReleaseDisplayFadeReservation" => CGReleaseDisplayFadeReservation,
        "CGDisplayFade" => CGDisplayFade,
        "CGDisplayIsMain" => CGDisplayIsMain,
        "CGDisplayIsOnline" => CGDisplayIsOnline,
        "CGDisplayIsBuiltin" => CGDisplayIsBuiltin,
        "CGDisplayPrimaryDisplay" => CGDisplayPrimaryDisplay,
        "CGDisplayRelease" => CGDisplayRelease,
        "CGDisplayRotation" => CGDisplayRotation,
        "CGDisplayIsAsleep" => CGDisplayIsAsleep,
        "CGDisplayMirrorsDisplay" => CGDisplayMirrorsDisplay,
        "CGDisplayModelNumber" => CGDisplayModelNumber,
        "CGDisplayVendorNumber" => CGDisplayVendorNumber,
        "CGDisplaySerialNumber" => CGDisplaySerialNumber,
        "CGDisplayCopyDisplayMode" => CGDisplayCopyDisplayMode,
        "CGDisplayCopyAllDisplayModes" => CGDisplayCopyAllDisplayModes,
        "CGDisplayModeGetRefreshRate" => CGDisplayModeGetRefreshRate,
        "CGDisplayModeGetWidth" => CGDisplayModeGetWidth,
        "CGDisplayModeGetHeight" => CGDisplayModeGetHeight,
        "CGDisplayModeGetPixelWidth" => CGDisplayModeGetPixelWidth,
        "CGDisplayModeGetPixelHeight" => CGDisplayModeGetPixelHeight,
        "CGDisplayModeGetIOFlags" => CGDisplayModeGetIOFlags,
        "CGDisplayModeIsUsableForDesktopGUI" => CGDisplayModeIsUsableForDesktopGUI,
        "CGDisplayModeCopyPixelEncoding" => CGDisplayModeCopyPixelEncoding,
        "CGDisplayModeRelease" => CGDisplayModeRelease,
        "CGDisplayModeRetain" => CGDisplayModeRetain,
        "CGDisplayRegisterReconfigurationCallback" => CGDisplayRegisterReconfigurationCallback,
        "CGDisplayRemoveReconfigurationCallback" => CGDisplayRemoveReconfigurationCallback,
        "CGGetDisplaysWithRect" => CGGetDisplaysWithRect,
        "CGDisplayCreateUUIDFromDisplayID" => CGDisplayCreateUUIDFromDisplayID,
        "CGDisplayGetDisplayIDFromUUID" => CGDisplayGetDisplayIDFromUUID,
        "CGGetOnlineDisplayList" | "CGGetActiveDisplayList" => CGGetOnlineDisplayList,
        "CGDisplayScreenSize" => CGDisplayScreenSize,
        "CGDisplayIDToOpenGLDisplayMask" => CGDisplayIDToOpenGLDisplayMask,
        "CGDisplayIOServicePort" => CGDisplayIOServicePort,
    }
}
