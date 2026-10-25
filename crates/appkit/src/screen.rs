use std::ffi::c_void;

use foundation::geometry::{CGFloat, NSRect};
use objc::object::Id;
use objc::sel::Sel;
use static_init::dynamic;

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
        Some(b) => {
            crate::cgpath::ns_rect(coregraphics::cggeometry::rect_from_sdl(b.x, b.y, b.w, b.h))
        }
        None => NSRect::default(),
    }
}

/// A fullscreen Space hides desktop docks, but keeps its global menu area.
/// This backend puts menus in each window instead, so that reservation is zero.
fn fullscreen_visible_frame(mut frame: NSRect, menu_height: CGFloat) -> NSRect {
    frame.size.height = (frame.size.height - menu_height.max(0.0)).max(0.0);
    frame
}

/// `-visibleFrame`: desktop panels constrain ordinary windows, not a fullscreen
/// Space. SDL's usable bounds keep those reservations even when panels are
/// covered; passing them to the guest can shrink its fullscreen window.
unsafe extern "C" fn visible_frame(receiver: Id, sel: Sel) -> NSRect {
    let display = display_of(receiver);
    if crate::window::fullscreen_on(display) {
        // SAFETY: the receiver is this runtime's NSScreen.
        let full = unsafe { frame(receiver, sel) };
        return fullscreen_visible_frame(full, crate::menu::global_menu_bar_height());
    }
    match sdl3::video::display_usable_bounds(display) {
        Some(b) => {
            crate::cgpath::ns_rect(coregraphics::cggeometry::rect_from_sdl(b.x, b.y, b.w, b.h))
        }
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

    /// Serialises the tests that touch the process-wide pointer state, and
    /// puts it back even when one fails.
    struct MouseState {
        _lock: std::sync::MutexGuard<'static, ()>,
        associated: bool,
        recentring: Option<Recentring>,
    }

    impl MouseState {
        fn take(associated: bool, recentring: Option<Recentring>) -> Self {
            crate::bootstrap();
            static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            Self {
                _lock: lock,
                associated: ASSOCIATED.swap(associated, std::sync::atomic::Ordering::AcqRel),
                recentring: std::mem::replace(
                    &mut *RECENTRING.lock().unwrap_or_else(|e| e.into_inner()),
                    recentring,
                ),
            }
        }
    }

    impl Drop for MouseState {
        fn drop(&mut self) {
            *RECENTRING.lock().unwrap_or_else(|e| e.into_inner()) = self.recentring;
            ASSOCIATED.store(self.associated, std::sync::atomic::Ordering::Release);
        }
    }

    fn camera_recentre(at: std::time::Instant) -> Option<Recentring> {
        Some(Recentring {
            window: 0,
            target: (420.0, 300.0),
            at,
            moved: true,
            active: true,
        })
    }

    #[test]
    fn redundant_mouse_association_preserves_camera_recentring() {
        let _state = MouseState::take(true, camera_recentre(std::time::Instant::now()));
        assert_eq!(
            unsafe { coregraphics::quartz_display::CGAssociateMouseAndMouseCursorPosition(1) },
            0
        );
        assert!(
            recentring(),
            "a reasserted association dropped the recentre"
        );
    }

    #[test]
    fn mouse_association_releases_an_idle_recentre() {
        let stale = std::time::Instant::now() - 2 * RECENTRE_IDLE;
        let _state = MouseState::take(true, camera_recentre(stale));
        assert_eq!(
            unsafe { coregraphics::quartz_display::CGAssociateMouseAndMouseCursorPosition(1) },
            0
        );
        assert!(!recentring(), "an idle recentre outlived the association");
    }

    #[test]
    fn disassociating_takes_over_from_a_recentre() {
        let _state = MouseState::take(true, camera_recentre(std::time::Instant::now()));
        assert_eq!(
            unsafe { coregraphics::quartz_display::CGAssociateMouseAndMouseCursorPosition(0) },
            0
        );
        assert!(!mouse_is_associated());
        assert!(!recentring(), "disassociation left the recentre in charge");
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
pub(crate) unsafe fn warp_pointer_impl(point: foundation::geometry::NSPoint) -> i32 {
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
            let at = coregraphics::quartz_event::global_pointer();
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
            // SDL only changes its virtual position for repeated relative
            // warps. If the physical pointer has drifted or another API
            // replaced the X11 barriers, repair that without changing the
            // relative input stream or generating an artificial camera delta.
            lock_relative_pointer(window, local);
        }
        return 0;
    }

    crate::event::expect_warp_echo(from, local);
    let mut released = true;
    for window in &relative {
        // A visible relative cursor on X11 is confined at the last guest warp.
        // Lift the barrier before relocating it; otherwise it may stay outside
        // the new one-pixel rectangle and be free to move indefinitely.
        clear_relative_pointer_lock(*window);
        released &= sdl3::video::set_relative_mouse_mode(*window, false);
    }
    let warped = warp_physical(target, point, local);
    for window in relative {
        if sdl3::video::set_relative_mouse_mode(window, true) && warped {
            let origin = crate::window::global_position(window).unwrap_or((0, 0));
            lock_relative_pointer(window, local_warp_point(point, origin));
        }
    }
    match released && warped {
        true => remember_relative_warp(point),
        false => forget_relative_warp(),
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
pub(crate) unsafe fn associate_impl(connected: bool) {
    let was = ASSOCIATED.swap(connected, std::sync::atomic::Ordering::AcqRel);
    // The cursor stops where it is, which is what the guest is told from here.
    if !connected && was {
        crate::event::settle_pointer();
    }
    // Reapplied even when unchanged: games reassert every frame, and that is
    // what puts back a window that lost relative mode without a focus event.
    apply_mouse_association();
}

/// The display services' view of AppKit's process-wide cursor and association.
struct QuartzDisplayProvider;

impl coregraphics::quartz_display::Provider for QuartzDisplayProvider {
    fn hide_cursor(&self) {
        crate::cursor::hide_cursor();
    }

    fn show_cursor(&self) {
        crate::cursor::show_cursor();
    }

    fn cursor_visible(&self) -> bool {
        crate::cursor::wants_visible()
    }

    fn associate(&self, connected: bool) {
        // SAFETY: the process-wide association state, which this provider owns.
        unsafe { associate_impl(connected) };
    }
}

/// Install the provider the display services ask for cursor and association state.
pub(crate) fn install_quartz_display_provider() {
    coregraphics::quartz_display::set_provider(Box::new(QuartzDisplayProvider));
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
        set_pointer_confinement(sdl, Some(rect));
    }
    sdl3::video::set_mouse_grab(sdl, true);
}

/// The content area of `window` in SDL window coordinates.
fn content_rect(window: Id, _sdl: sdl3::Window) -> Option<sdl3::Rect> {
    let surface = crate::window::surface_frame(window)?;
    let (w, h) = (surface.size.width as i32, surface.size.height as i32);
    let inset = crate::window::content_inset(window).ceil() as i32;
    (w > 0 && h > inset).then_some(sdl3::Rect {
        x: surface.origin.x.round() as i32,
        y: surface.origin.y.round() as i32 + inset,
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

/// When the guest disassociates its pointer from the cursor, SDL's X11
/// relative mode delivers raw deltas but only *hides* the physical cursor by
/// default. We explicitly leave the cursor visible (as macOS does), so without
/// an additional constraint it can travel across the window while the guest
/// sees it frozen. A one-point SDL mouse rectangle restricts physical motion
/// without interfering with XI2 raw motion.
// Keep any confinement the application already requested through AppKit's
// `-setMouseConfinementRect:` or the optional ROSEMARY_CONFINE_POINTER path.
// Relative-mode confinement is temporary and must restore the previous rect.
static PREVIOUS_MOUSE_RECTS: std::sync::Mutex<Vec<(usize, Option<sdl3::Rect>)>> =
    std::sync::Mutex::new(Vec::new());

fn lock_relative_pointer(window: sdl3::Window, point: (f32, f32)) {
    // Recentring and edge holds also use relative mode while associated, and
    // the paths that end them never lift a lock: only disassociation gets one.
    if !sdl3::video::is_x11()
        || mouse_is_associated()
        || sdl3::video::window_flags(window) & sdl3::video::flags::INPUT_FOCUS == 0
    {
        return;
    }
    // XFixes barriers only block crossing their edges from inside. Enabling
    // them when the physical pointer is outside produces exactly the observed
    // intermittent horizontal/vertical cursor drift: it cannot enter the one-
    // pixel region until a later input happens to cross the barrier.
    let rect = sdl3::Rect {
        x: point.0.round() as i32,
        y: point.1.round() as i32,
        w: 1,
        h: 1,
    };
    // Avoid calling into SDL while holding the bookkeeping lock: SDL may
    // dispatch window events while updating pointer confinement.
    let original = {
        let previous = PREVIOUS_MOUSE_RECTS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if previous.iter().any(|(id, _)| *id == window as usize) {
            None
        } else {
            Some(sdl3::video::get_mouse_rect(window))
        }
    };
    let origin = crate::window::global_position(window).unwrap_or((0, 0));
    let global = (
        origin.0 as f32 + rect.x as f32,
        origin.1 as f32 + rect.y as f32,
    );
    let physical = sdl3::video::global_mouse_state();
    let outside =
        physical.is_none_or(|(x, y, _)| (x - global.0).abs() > 0.5 || (y - global.1).abs() > 0.5);
    if outside {
        // SDL_WarpMouseGlobal *does* reposition the physical X11 cursor even
        // during relative mode, unlike SDL_WarpMouseInWindow. Verified with
        // SDL 3.4 and XI2 raw events: this produces no false camera delta.
        sdl3::video::warp_mouse_global(global.0, global.1);
    }
    let installed = if sdl3::video::get_mouse_rect(window) != Some(rect) {
        // A fullscreen/resize/confinement transition can replace the active
        // barrier. SDL uses cached rect equality to skip reinstallation; drop
        // the old barrier before requesting the correct one.
        sdl3::video::set_mouse_rect(window, None);
        sdl3::video::set_mouse_rect(window, Some(rect))
    } else {
        true
    };
    if installed && let Some(previous_rect) = original {
        let mut previous = PREVIOUS_MOUSE_RECTS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !previous.iter().any(|(id, _)| *id == window as usize) {
            previous.push((window as usize, previous_rect));
        }
    }
}

/// Keep the visible host cursor at the guest's fixed point while XI2 delivers
/// raw mouse deltas. Explicit host warps and window transitions can place the
/// physical pointer outside an active XFixes region; barriers do not pull it
/// back in, so correct that on the next actual mouse motion, not only if the
/// guest subsequently calls CGWarpMouseCursorPosition again.
pub(crate) fn refresh_relative_pointer(window_id: u32) {
    if !sdl3::video::is_x11() || mouse_is_associated() {
        return;
    }
    let Some(window) = crate::window::sdl_windows().into_iter().find(|window| {
        sdl3::video::window_id(*window) == window_id && sdl3::video::relative_mouse_mode(*window)
    }) else {
        return;
    };
    lock_at_frozen_pointer(window);
}

/// [`refresh_relative_pointer`] for real motion, which only needs to repair a
/// window already locked: found in the lock table rather than by listing every
/// window, so an unlocked one costs nothing per report.
pub(crate) fn repair_relative_pointer(window_id: u32) {
    if mouse_is_associated() {
        return;
    }
    let locked = PREVIOUS_MOUSE_RECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(window, _)| *window as sdl3::Window)
        .find(|window| sdl3::video::window_id(*window) == window_id);
    let Some(window) = locked else {
        return;
    };
    lock_at_frozen_pointer(window);
}

/// Lock `window` where the disassociated guest's cursor is frozen.
fn lock_at_frozen_pointer(window: sdl3::Window) {
    let Some(frozen) = crate::event::frozen_pointer() else {
        return;
    };
    let origin = crate::window::global_position(window).unwrap_or((0, 0));
    lock_relative_pointer(window, local_warp_point(frozen, origin));
}

fn clear_relative_pointer_lock(window: sdl3::Window) {
    let original = {
        let mut previous = PREVIOUS_MOUSE_RECTS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        previous
            .iter()
            .position(|(id, _)| *id == window as usize)
            .map(|index| previous.swap_remove(index).1)
    };
    if let Some(rect) = original {
        sdl3::video::set_mouse_rect(window, rect);
    }
}

/// Confine `window`'s pointer to `rect`, or lift it for `None`. While a
/// relative lock holds the pointer, the rectangle replaces the one saved for
/// when the lock lifts instead, or the next refresh would discard it.
pub(crate) fn set_pointer_confinement(window: sdl3::Window, rect: Option<sdl3::Rect>) {
    if let Some(saved) = PREVIOUS_MOUSE_RECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter_mut()
        .find(|(id, _)| *id == window as usize)
    {
        saved.1 = rect;
        return;
    }
    sdl3::video::set_mouse_rect(window, rect);
}

/// Drop a destroyed window's saved rectangle without touching SDL. Its focus
/// and close events arrive once the window has left [`crate::window::sdl_windows`],
/// so [`release_relative_pointer`] cannot find it, and SDL reuses the address.
pub(crate) fn forget_relative_pointer(window: sdl3::Window) {
    PREVIOUS_MOUSE_RECTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(id, _)| *id != window as usize);
}

pub(crate) fn release_relative_pointer(window_id: u32) {
    if !sdl3::video::is_x11() {
        return;
    }
    if let Some(window) = crate::window::sdl_windows()
        .into_iter()
        .find(|window| sdl3::video::window_id(*window) == window_id)
    {
        clear_relative_pointer_lock(window);
    }
}

/// Put every window into relative mouse mode, or take it out, to match the last
/// [`CGAssociateMouseAndMouseCursorPosition`].
///
/// Called again when a window takes focus, because SDL leaves the mode when
/// focus goes and a window made after the guest disassociated never had it.
pub fn apply_mouse_association() {
    let relative = !mouse_is_associated();
    // While associated, a live recentre or edge hold owns its window's
    // relative mode: Studio reasserts association mid-drag, and dropping the
    // recentre there turns each camera warp into real cursor movement. A
    // recentre gone idle is stale, so it is released like any other.
    let (held, recentred) = {
        let mut hold = EDGE_HOLD.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = RECENTRING.lock().unwrap_or_else(|e| e.into_inner());
        if relative || state.is_some_and(|r| r.active && r.at.elapsed() > RECENTRE_IDLE) {
            *state = None;
        }
        if relative {
            *hold = None;
        }
        (
            hold.map(|h| h.id),
            state.filter(|r| r.active).map(|r| r.window),
        )
    };
    // Disassociating is not hiding: on macOS the cursor stays on screen where
    // it was, and only `+[NSCursor hide]` takes it away - which this tree
    // already counts in `crate::cursor`. SDL's relative mode hides the pointer
    // itself, so the hint is what stops it from making that decision twice.
    sdl3::video::set_hint(sdl3::video::HINT_RELATIVE_CURSOR_VISIBLE, c"1");
    let mut changed = false;
    for window in crate::window::sdl_windows() {
        if !relative {
            // Remove the X11 lock before SDL releases relative mode and the
            // guest's cursor position is restored.
            clear_relative_pointer_lock(window);
        }
        let owned =
            held == Some(sdl3::video::window_id(window)) || recentred == Some(window as usize);
        if !owned && sdl3::video::relative_mouse_mode(window) != relative {
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
        if relative && sdl3::video::relative_mouse_mode(window) {
            lock_at_frozen_pointer(window);
        }
    }
    if changed {
        forget_relative_warp();
    }
}
