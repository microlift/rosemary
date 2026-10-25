//! The Appearance Manager's metrics and HIToolbox's theme drawing calls.
//!
//! A widget toolkit sizes and draws native-looking controls with these rather
//! than through `NSCell`: it asks how wide a scroll bar is, then hands a rect
//! and a state to `HIThemeDraw...` with its own `CGContext`.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use foundation::geometry::{NSPoint, NSRect, NSSize};

use crate::aqua;

type OSStatus = i32;

const NO_ERR: OSStatus = 0;

/// `paramErr`, for a metric this table does not have.
const PARAM_ERR: OSStatus = -50;

/// `ThemeMetric` values and Aqua's answer for each, in points.
///
/// Only the metrics a program has been seen to ask for; the numbers are the
/// regular-size Aqua controls'.
const METRICS: &[(u32, i32)] = &[
    // kThemeMetricScrollBarWidth, kThemeMetricSmallScrollBarWidth
    (0, 15),
    (1, 11),
    // kThemeMetricCheckBoxHeight, kThemeMetricRadioButtonHeight
    (2, 18),
    (3, 18),
    // kThemeMetricEditTextWhitespace, kThemeMetricEditTextFrameOutset,
    // kThemeMetricListBoxFrameOutset, kThemeMetricFocusRectOutset
    (4, 3),
    (5, 1),
    (6, 1),
    (7, 4),
    // kThemeMetricDisclosureButtonHeight
    (17, 21),
    // kThemeMetricPushButtonHeight, kThemeMetricListHeaderHeight
    (19, 20),
    (20, 17),
    // kThemeMetricDisclosureButtonWidth
    (22, 21),
    // kThemeMetricLittleArrowsHeight, kThemeMetricLittleArrowsWidth
    (27, 23),
    (28, 13),
    // kThemeMetricPopupButtonHeight
    (30, 20),
    // kThemeMetricHSliderHeight, kThemeMetricVSliderWidth
    (41, 18),
    (45, 18),
    // kThemeMetricCheckBoxWidth, kThemeMetricRadioButtonWidth
    (50, 18),
    (52, 18),
    // kThemeMetricNormalProgressBarThickness
    (58, 14),
];

/// `OSStatus GetThemeMetric(ThemeMetric, SInt32 *)`
///
/// A metric not in the table is refused with zero written, so a caller that
/// ignores the status reads a defined value rather than its stack.
pub unsafe extern "C" fn GetThemeMetric(metric: u32, out: *mut i32) -> OSStatus {
    if out.is_null() {
        return PARAM_ERR;
    }
    let found = METRICS.iter().find(|(m, _)| *m == metric).map(|(_, v)| *v);
    // SAFETY: writable, per the caller.
    unsafe { out.write(found.unwrap_or(0)) };
    match found {
        Some(_) => NO_ERR,
        None => PARAM_ERR,
    }
}

// The info structs below are HIToolbox's, laid out under `#pragma pack(2)`:
// a rectangle after a 16-bit field starts on a 2-byte boundary.

/// `kHIThemeOrientationNormal`: user space grows downward, as an HIView's.
const ORIENTATION_NORMAL: u32 = 0;

/// The field of type `T` at `offset` bytes into an info struct.
///
/// # Safety
///
/// `info` is null or points at least `offset + size_of::<T>()` bytes.
unsafe fn field<T: Copy>(info: *const c_void, offset: usize) -> Option<T> {
    if info.is_null() {
        return None;
    }
    // SAFETY: the caller's obligation; packed fields may be unaligned.
    Some(unsafe { std::ptr::read_unaligned(info.cast::<u8>().add(offset).cast::<T>()) })
}

/// A `ThemeDrawState` as the painters take it.
fn look_of(state: u32) -> aqua::Look {
    aqua::Look {
        // kThemeStatePressed (also PressedUp) and kThemeStatePressedDown.
        pressed: matches!(state, 2 | 3),
        // kThemeStateInactive, kThemeStateUnavailable and its inactive form.
        disabled: matches!(state, 0 | 7 | 8),
        ..aqua::Look::default()
    }
}

fn painter(
    bounds: *const NSRect,
    context: CFTypeRef,
    orientation: u32,
) -> Option<(aqua::Painter, NSRect)> {
    if bounds.is_null() {
        return None;
    }
    // SAFETY: supplied by the caller.
    let bounds = unsafe { *bounds };
    let painter = aqua::Painter::new(context, bounds, orientation == ORIENTATION_NORMAL)?;
    Some((painter, bounds))
}

/// `HIThemeDrawButton(const HIRect *, const HIThemeButtonDrawInfo *,
/// CGContextRef, HIThemeOrientation, HIRect *labelRect)`
pub unsafe extern "C" fn HIThemeDrawButton(
    bounds: *const NSRect,
    info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
    label: *mut NSRect,
) -> OSStatus {
    if !label.is_null() && !bounds.is_null() {
        // SAFETY: both supplied by the caller. The label fills the button.
        unsafe { label.write(*bounds) };
    }
    // SAFETY: `version`, `state`, `kind`, `value`, `adornment`.
    let (Some(state), Some(kind), Some(value), Some(adornment)) = (unsafe {
        (
            field::<u32>(info, 4),
            field::<u16>(info, 8),
            field::<u16>(info, 10),
            field::<u16>(info, 12),
        )
    }) else {
        return PARAM_ERR;
    };
    let Some((p, r)) = painter(bounds, context, orientation) else {
        return NO_ERR;
    };
    let mut look = look_of(state);
    look.default = adornment & 1 != 0;
    look.focused = adornment & (1 << 2) != 0;
    let check = match value {
        1 => aqua::Value::On,
        2 => aqua::Value::Mixed,
        _ => aqua::Value::Off,
    };
    use aqua::Size::{Mini, Regular, Small};
    match kind {
        0 | 25 => aqua::push_button(&p, r, Regular, look),
        26 => aqua::push_button(&p, r, Small, look),
        27 => aqua::push_button(&p, r, Mini, look),
        1 => aqua::check_box(&p, r, Regular, check, look),
        13 => aqua::check_box(&p, r, Small, check, look),
        19 => aqua::check_box(&p, r, Mini, check, look),
        2 => aqua::radio_button(&p, r, Regular, check, look),
        14 => aqua::radio_button(&p, r, Small, check, look),
        20 => aqua::radio_button(&p, r, Mini, check, look),
        5 | 28 => aqua::popup_button(&p, r, Regular, look, false),
        29 => aqua::popup_button(&p, r, Small, look, false),
        30 => aqua::popup_button(&p, r, Mini, look, false),
        16 => aqua::combo_box(&p, r, Regular, look),
        17 => aqua::combo_box(&p, r, Small, look),
        18 => aqua::combo_box(&p, r, Mini, look),
        6 => aqua::disclosure(&p, r, value, look),
        7 | 21 | 22 => {
            let size = match kind {
                21 => Small,
                22 => Mini,
                _ => Regular,
            };
            let half = match state {
                2 => Some(true),
                3 => Some(false),
                _ => None,
            };
            aqua::inc_dec(&p, r, size, look, half);
        }
        4 | 23 | 24 => aqua::arrow_button(&p, r, look),
        10 => aqua::list_header(&p, r, look, value == 1),
        11 | 12 => aqua::round_button(&p, r, look),
        // Bevel buttons of each size, the rounded bevel, and kinds this
        // table does not name, which are all rectangular buttons of a kind.
        _ => aqua::bevel_button(&p, r, look),
    }
    NO_ERR
}

/// `HIThemeDrawFocusRect(const HIRect *, Boolean, CGContextRef,
/// HIThemeOrientation)`
pub unsafe extern "C" fn HIThemeDrawFocusRect(
    bounds: *const NSRect,
    focused: u8,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    if focused != 0
        && let Some((p, r)) = painter(bounds, context, orientation)
    {
        aqua::focus_ring(&p, r);
    }
    NO_ERR
}

/// `HIThemeDrawFrame(const HIRect *, const HIThemeFrameDrawInfo *, ...)`:
/// the frame around a text field or list box, whose outer edge is `bounds`.
pub unsafe extern "C" fn HIThemeDrawFrame(
    bounds: *const NSRect,
    info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    // SAFETY: `version`, `kind`, `state`, `isFocused`.
    let (Some(kind), Some(focused)) = (unsafe { (field::<u32>(info, 4), field::<u8>(info, 12)) })
    else {
        return PARAM_ERR;
    };
    if let Some((p, r)) = painter(bounds, context, orientation) {
        match kind {
            // kHIThemeFrameListBox
            1 => aqua::list_box(&p, r, focused != 0),
            // kHIThemeFrameTextFieldSquare
            _ => aqua::text_field(&p, r, focused != 0),
        }
    }
    NO_ERR
}

/// `HIThemeDrawGroupBox(const HIRect *, const HIThemeGroupBoxDrawInfo *,
/// CGContextRef, HIThemeOrientation)`
pub unsafe extern "C" fn HIThemeDrawGroupBox(
    bounds: *const NSRect,
    _info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    if let Some((p, r)) = painter(bounds, context, orientation) {
        aqua::group_box(&p, r);
    }
    NO_ERR
}

/// `HIThemeDrawTabPane`: the panel under a row of tabs, drawn as a group box.
pub unsafe extern "C" fn HIThemeDrawTabPane(
    bounds: *const NSRect,
    info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    // SAFETY: the same shape.
    unsafe { HIThemeDrawGroupBox(bounds, info, context, orientation) }
}

/// `HIThemeDrawHeader`: a window or list header strip.
pub unsafe extern "C" fn HIThemeDrawHeader(
    bounds: *const NSRect,
    info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    // SAFETY: `version`, `state`.
    let state = unsafe { field::<u32>(info, 4) }.unwrap_or(1);
    if let Some((p, r)) = painter(bounds, context, orientation) {
        aqua::list_header(&p, r, look_of(state), false);
    }
    NO_ERR
}

/// `HIThemeDrawMenuBackground(const HIRect *, const HIThemeMenuDrawInfo *,
/// ...)`
pub unsafe extern "C" fn HIThemeDrawMenuBackground(
    bounds: *const NSRect,
    _info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    if let Some((p, r)) = painter(bounds, context, orientation) {
        aqua::menu_background(&p, r);
    }
    NO_ERR
}

/// `HIThemeDrawSeparator(const HIRect *, const HIThemeSeparatorDrawInfo *,
/// ...)`: a line across the middle of the longer side.
pub unsafe extern "C" fn HIThemeDrawSeparator(
    bounds: *const NSRect,
    _info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    if let Some((p, r)) = painter(bounds, context, orientation) {
        if r.size.height > r.size.width {
            let x = (r.origin.x + r.size.width / 2.0).floor();
            aqua::fill(
                &p,
                aqua::rect(x, r.origin.y, 1.0, r.size.height),
                [0.0, 0.0, 0.0, 0.12],
            );
        } else {
            aqua::separator(&p, r);
        }
    }
    NO_ERR
}

/// `HIThemeDrawGrowBox(const HIPoint *, const HIThemeGrowBoxDrawInfo *,
/// CGContextRef, HIThemeOrientation)`
pub unsafe extern "C" fn HIThemeDrawGrowBox(
    origin: *const NSPoint,
    info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    let mut bounds = NSRect::default();
    // SAFETY: the caller's point and info; `bounds` is local.
    if unsafe { HIThemeGetGrowBoxBounds(origin, info, &mut bounds) } != NO_ERR {
        return PARAM_ERR;
    }
    if let Some((p, r)) = painter(&bounds, context, orientation) {
        aqua::grow_box(&p, r);
    }
    NO_ERR
}

/// `HIThemeDrawMenuItem(const HIRect *menu, const HIRect *item,
/// const HIThemeMenuItemDrawInfo *, CGContextRef, HIThemeOrientation,
/// HIRect *content)`
pub unsafe extern "C" fn HIThemeDrawMenuItem(
    _menu: *const NSRect,
    item: *const NSRect,
    info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
    content: *mut NSRect,
) -> OSStatus {
    if !content.is_null() && !item.is_null() {
        // SAFETY: both supplied by the caller.
        unsafe { content.write(*item) };
    }
    // SAFETY: `version`, `itemType`, then `state`.
    let state = unsafe { field::<u32>(info, 6) }.unwrap_or(0);
    if let Some((p, r)) = painter(item, context, orientation) {
        // kThemeMenuSelected
        aqua::menu_item(&p, r, state == 1);
    }
    NO_ERR
}

pub unsafe extern "C" fn HIThemeDrawMenuSeparator(
    _menu: *const NSRect,
    item: *const NSRect,
    _info: *const c_void,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    if let Some((p, r)) = painter(item, context, orientation) {
        aqua::separator(&p, r);
    }
    NO_ERR
}

/// `HIThemeDrawTrack(const HIThemeTrackDrawInfo *, const HIRect *ghost,
/// CGContextRef, HIThemeOrientation)`: a scroll bar, slider or progress bar.
pub unsafe extern "C" fn HIThemeDrawTrack(
    info: *const c_void,
    _ghost: *const NSRect,
    context: CFTypeRef,
    orientation: u32,
) -> OSStatus {
    // SAFETY: `version`, `kind`, `bounds`, `min`, `max`, `value`,
    // `reserved`, `attributes`, `enableState`, a filler byte, then the
    // kind's own info.
    let fields = unsafe {
        (
            field::<u16>(info, 4),
            field::<NSRect>(info, 6),
            field::<i32>(info, 38),
            field::<i32>(info, 42),
            field::<i32>(info, 46),
            field::<u16>(info, 54),
            field::<u8>(info, 56),
            field::<i32>(info, 58),
            field::<u8>(info, 62),
            field::<u8>(info, 59),
        )
    };
    let (
        Some(kind),
        Some(bounds),
        Some(min),
        Some(max),
        Some(value),
        Some(attributes),
        Some(enable),
        Some(view_size),
        Some(scroll_press),
        Some(slider_press),
    ) = fields
    else {
        return PARAM_ERR;
    };
    let Some((p, r)) = painter(&bounds, context, orientation) else {
        return NO_ERR;
    };
    let horizontal = attributes & 1 != 0;
    let show_thumb = attributes & (1 << 2) != 0;
    // kThemeTrackDisabled, kThemeTrackNothingToScroll.
    let disabled = matches!(enable, 1 | 2);
    let fraction = match max > min {
        true => (f64::from(value) - f64::from(min)) / (f64::from(max) - f64::from(min)),
        false => 0.0,
    };
    use aqua::Size::{Mini, Regular, Small};
    match kind {
        // Medium, small and mini scroll bars.
        0 | 1 | 12 => {
            let length = if horizontal {
                r.size.width
            } else {
                r.size.height
            };
            let thumb = (show_thumb && !disabled)
                .then(|| aqua::thumb_span(length, min, max, value, view_size))
                .flatten();
            // kThemeThumbPressed
            aqua::scroll_bar(&p, r, horizontal, thumb, scroll_press & (1 << 3) != 0);
        }
        // Medium, small and mini sliders.
        2 | 6 | 13 => {
            let look = aqua::Look {
                pressed: slider_press != 0,
                disabled,
                ..aqua::Look::default()
            };
            aqua::slider(&p, r, horizontal, fraction, look);
        }
        // Determinate bars, and the relevance bar, which fills the same way.
        3 | 5 => aqua::progress_bar(&p, r, horizontal, Some(fraction), Small),
        7 => aqua::progress_bar(&p, r, horizontal, Some(fraction), Regular),
        14 => aqua::progress_bar(&p, r, horizontal, Some(fraction), Mini),
        4 | 15 => aqua::progress_bar(&p, r, horizontal, None, Small),
        8 => aqua::progress_bar(&p, r, horizontal, None, Regular),
        _ => return PARAM_ERR,
    }
    NO_ERR
}

/// `HIThemeGetGrowBoxBounds(const HIPoint *, const HIThemeGrowBoxDrawInfo *,
/// HIRect *)`: Aqua's grow box is fifteen points square, its corner at the
/// origin.
pub unsafe extern "C" fn HIThemeGetGrowBoxBounds(
    origin: *const NSPoint,
    _info: *const c_void,
    bounds: *mut NSRect,
) -> OSStatus {
    if origin.is_null() || bounds.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: both supplied by the caller.
    unsafe {
        bounds.write(NSRect {
            origin: *origin,
            size: NSSize {
                width: 15.0,
                height: 15.0,
            },
        })
    };
    NO_ERR
}

/// `HIThemeSetFill(ThemeBrush, void *, CGContextRef, HIThemeOrientation)`:
/// the brush's colour as the context's fill. Every brush is the window
/// background here, the one colour a toolkit fills with this.
pub unsafe extern "C" fn HIThemeSetFill(
    _brush: i16,
    _info: *mut c_void,
    context: CFTypeRef,
    _orientation: u32,
) -> OSStatus {
    // SAFETY: the caller's context, or null, which the call ignores.
    unsafe { coregraphics::cgcontext::CGContextSetRGBFillColor(context, 0.93, 0.93, 0.93, 1.0) };
    NO_ERR
}

/// `cmProfileError`: the ColorSync calls below have no profile to answer
/// with, the display's colour being whatever the compositor makes of sRGB.
const CM_PROFILE_ERROR: OSStatus = -170;

/// `CMError CMGetProfileByAVID(CMDisplayIDType, CMProfileRef *)` and
/// `CMGetSystemProfile(CMProfileRef *)`: no profile, and a caller that treats
/// failure as "no colour management" is right to.
pub unsafe extern "C" fn CMGetProfileByAVID(_display: u32, profile: *mut *mut c_void) -> OSStatus {
    if !profile.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { profile.write(std::ptr::null_mut()) };
    }
    CM_PROFILE_ERROR
}

pub unsafe extern "C" fn CMGetSystemProfile(profile: *mut *mut c_void) -> OSStatus {
    // SAFETY: as above.
    unsafe { CMGetProfileByAVID(0, profile) }
}

/// `CMError NCMGetProfileLocation(CMProfileRef, CMProfileLocation *, UInt32 *)`
pub unsafe extern "C" fn NCMGetProfileLocation(
    _profile: *mut c_void,
    _location: *mut c_void,
    _size: *mut u32,
) -> OSStatus {
    CM_PROFILE_ERROR
}

/// `CMError CMCloseProfile(CMProfileRef)`: nothing was opened.
pub unsafe extern "C" fn CMCloseProfile(_profile: *mut c_void) -> OSStatus {
    NO_ERR
}

/// One CoreUI option: a string's text or a number's value.
enum Option_ {
    Text(String),
    Number(i64),
}

/// `options`' entries by key, as CoreUI's renderer reads them.
fn cui_options(options: CFTypeRef) -> Vec<(String, Option_)> {
    // SAFETY: null or a dictionary the guest built with CF.
    let Some(dictionary) = (unsafe { corefoundation::collections::dictionary(options) }) else {
        return Vec::new();
    };
    dictionary
        .entries
        .iter()
        .map(|&(key, value)| {
            // SAFETY: keys and values are CF objects; a non-string reads empty.
            let key = unsafe { foundation::string::bytes(key as objc::object::Id) };
            let value = match unsafe { corefoundation::collections::number(value) } {
                Some(number) => Option_::Number(number.as_i64()),
                None => Option_::Text(
                    String::from_utf8_lossy(&unsafe {
                        foundation::string::bytes(value as objc::object::Id)
                    })
                    .into_owned(),
                ),
            };
            (String::from_utf8_lossy(&key).into_owned(), value)
        })
        .collect()
}

/// `void CUIDraw(CUIRendererRef, CGRect, CGContextRef, CFDictionaryRef
/// options, CFDictionaryRef *result)`, CoreUI's private widget renderer.
///
/// Segmented and round toolbar buttons are drawn as `crate::aqua` draws a
/// round button. FIXME (ACCURACY): other widgets - the window frame's
/// unified title bar gradient among them - draw nothing.
pub unsafe extern "C" fn CUIDraw(
    _renderer: *mut c_void,
    rect: NSRect,
    context: CFTypeRef,
    options: CFTypeRef,
    result: *mut CFTypeRef,
) {
    if !result.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { result.write(std::ptr::null()) };
    }
    let options = cui_options(options);
    let text = |key: &str| {
        options.iter().find_map(|(k, v)| match v {
            Option_::Text(t) if k == key => Some(t.as_str()),
            _ => None,
        })
    };
    let number = |key: &str| {
        options.iter().find_map(|(k, v)| match v {
            Option_::Number(n) if k == key => Some(*n),
            _ => None,
        })
    };
    let segmented = matches!(
        text("widget"),
        Some(
            "kCUIWidgetButtonSegmentedSCurve"
                | "kCUIWidgetButtonSegmentedTexturedRounded"
                | "kCUIWidgetButtonSegmented"
                | "kCUIWidgetButtonRoundTextured"
        )
    );
    if !segmented {
        return;
    }
    let flipped = number("is.flipped").unwrap_or(0) != 0;
    let Some(p) = aqua::Painter::new(context, rect, flipped) else {
        return;
    };
    let state = text("state").unwrap_or("normal");
    let look = aqua::Look {
        pressed: state == "pressed" || number("value").unwrap_or(0) != 0,
        disabled: matches!(state, "disabled" | "inactive"),
        focused: number("focus").unwrap_or(0) != 0,
        default: false,
    };
    aqua::round_button(&p, rect, look);
}

/// `+[NSWindow coreUIRenderer]`, private: the renderer `CUIDraw` takes, which
/// is never looked at here.
unsafe extern "C" fn core_ui_renderer(
    _class: objc::object::Id,
    _sel: objc::sel::Sel,
) -> objc::object::Id {
    std::ptr::null_mut()
}

pub(crate) fn install() {
    let window = objc::class::lookup(c"NSWindow").expect("declared before its methods");
    crate::define::class_methods!(window, {
        "coreUIRenderer" => core_ui_renderer, c"^v16@0:8";
    });
}

exports::exports! {
    functions {
        "GetThemeMetric" => GetThemeMetric,
        "HIThemeDrawButton" => HIThemeDrawButton,
        "HIThemeDrawFocusRect" => HIThemeDrawFocusRect,
        "HIThemeDrawFrame" => HIThemeDrawFrame,
        "HIThemeDrawGroupBox" => HIThemeDrawGroupBox,
        "HIThemeDrawHeader" => HIThemeDrawHeader,
        "HIThemeDrawMenuBackground" => HIThemeDrawMenuBackground,
        "HIThemeDrawSeparator" => HIThemeDrawSeparator,
        "HIThemeDrawTabPane" => HIThemeDrawTabPane,
        "HIThemeDrawGrowBox" => HIThemeDrawGrowBox,
        "HIThemeDrawMenuItem" => HIThemeDrawMenuItem,
        "HIThemeDrawMenuSeparator" => HIThemeDrawMenuSeparator,
        "HIThemeDrawTrack" => HIThemeDrawTrack,
        "HIThemeGetGrowBoxBounds" => HIThemeGetGrowBoxBounds,
        "HIThemeSetFill" => HIThemeSetFill,
        "CMGetProfileByAVID" => CMGetProfileByAVID,
        "CMGetSystemProfile" => CMGetSystemProfile,
        "NCMGetProfileLocation" => NCMGetProfileLocation,
        "CMCloseProfile" => CMCloseProfile,
        "CUIDraw" => CUIDraw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_metric_answers_and_an_unknown_one_writes_zero() {
        let mut out = -1;
        // SAFETY: a writable SInt32.
        assert_eq!(unsafe { GetThemeMetric(0, &mut out) }, NO_ERR);
        assert_eq!(out, 15);
        out = -1;
        // SAFETY: as above.
        assert_eq!(unsafe { GetThemeMetric(9999, &mut out) }, PARAM_ERR);
        assert_eq!(out, 0);
    }
}
