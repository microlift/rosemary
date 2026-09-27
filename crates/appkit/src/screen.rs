use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use foundation::geometry::{CGFloat, NSRect};
use objc::object::Id;
use objc::sel::Sel;

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
    static SCREENS: std::sync::Mutex<Vec<(u32, usize)>> = std::sync::Mutex::new(Vec::new());
    let mut screens = SCREENS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = screens.iter().find(|(d, _)| *d == display) {
        return *object as Id;
    }
    let class = objc::class::lookup(c"NSScreen").expect("declared before its methods");
    // SAFETY: `Screen` is the layout the class was registered with.
    let screen = unsafe { corefoundation::base::allocate::<Screen>(class) };
    // SAFETY: freshly allocated.
    unsafe { (*screen).display = display as usize };
    // Retained forever: a screen a guest can release is one it can make dangle,
    // and these are handed out unowned by `+mainScreen`.
    objc::arc::retain(screen as Id);
    screens.push((display, screen as usize));
    screen as Id
}

fn display_of(receiver: Id) -> u32 {
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
    match sdl3::video::display_refresh_rate(display_of(receiver)) {
        Some(rate) => rate.round() as isize,
        None => 60,
    }
}

unsafe extern "C" fn backing_scale_factor(receiver: Id, _sel: Sel) -> CGFloat {
    sdl3::video::display_content_scale(display_of(receiver)) as f64
}

/// `NSString * -[NSScreen localizedName]` - the display's name, as its user sees
/// it.
///
/// # What the guest does with it
///
/// `BuildDisplayForScreen` ends with
/// `display.set_label(base::SysNSStringToUTF8(screen.localizedName))`, and the
/// disassembly matches: the send is at `Framework+0x6a8df60` from the selector
/// loaded at `+0x6a8df56` (`__objc_selrefs` `+0xe6ead78`), the result is
/// retained into `%r13` and **nil-tested** at `+0x6a8df71`
/// (`test rax,rax; je +0x6a8df8e`, which zeroes a 24-byte `std::string`), and
/// the non-nil path assigns it through `+0x5d99b80` into the `std::string` the
/// `Display` keeps as its label.
///
/// That nil test is the one place in this whole function where the guest
/// tolerates absence, and it is worth contrasting with the two EDR selectors
/// above: here a nil would have cost a display an empty label, so the source
/// checks. This runtime answers an unrecognised selector by aborting rather than
/// by returning nil, so the tolerance is not something to lean on - the message
/// has to be answered either way - but it does say the value is only ever a
/// label.
///
/// # The census, which is two sites and not one
///
/// Whole-`__TEXT,__text`, by the same displacement scan: **two** loads of
/// `+0xe6ead78`, at `+0x6a8df56` and `+0x7929608`. The second is a *spill* -
/// six selectors loaded into consecutive stack slots at `+0x79295d0`-`+0x792961d`
/// and reloaded 0x21f bytes later - which is the shape a window filter calls a
/// store and would have reported as unread. Re-disassembled, the reload at
/// `+0x792982e` is a **send** at `+0x7929835`, on `[rbp-0x1b8]`, and it is
/// nil-tested the same way (`+0x7929846`).
///
/// That second one is not this screen: the sibling selectors in the same spill
/// are `hasMediaType:`, `isSuspended`, `uniqueID`, `transportType` and
/// `formats`, which is `AVCaptureDevice` - the capture-device list, not the
/// display list. It is live code, but it is not on the milestone's path today.
///
/// # Where the string comes from
///
/// The host's own name for the display, which is what SDL reports and what the
/// compositor said: `SDL_GetDisplayName` answers `"Built-in display"` on this
/// session's Wayland display and `"eDP-1 16\""` on the same panel through
/// XWayland. Neither is Apple's wording - on a Mac this method returns a
/// localized, user-facing name like "Built-in Retina Display" - and neither
/// could: there is no Mac here to have one. What *is* true of this host's
/// display is that the display server calls it that, and a label is a label.
unsafe extern "C" fn localized_name(receiver: Id, _sel: Sel) -> Id {
    let display = display_of(receiver);
    match sdl_properties::display_name(display) {
        Some(name) => {
            let string = foundation::string::from_str(&name);
            // SAFETY: a fresh CFString, toll-free bridged to `NSString`.
            unsafe { corefoundation::api::CFAutorelease(string as _) as Id }
        }
        // A display SDL will not name is a display SDL does not have, and this
        // screen came from `sdl3::video::displays()`. Rather than hand back an
        // empty string - which is a name, and would be read as one - this says
        // so.
        None => refuse_unnamed_display(display),
    }
}

/// Refuse a display with no name, rather than invent one.
fn refuse_unnamed_display(display: u32) -> ! {
    objc::fail!(
        "appkit::screen: -[NSScreen localizedName] has no derivable answer - SDL display \
         {display} has no name, and there is no other host source for one here (an EDID, \
         a DRM connector name, a compositor output name: none of which this tree reads). \
         Refusing rather than answering an empty string, which is a name and would be read \
         as one. The guest reads the return value at Framework+0x6a8df60 and nil-tests it \
         at +0x6a8df71, so nil would have been survivable and a wrong name would not"
    )
}

// ---------------------------------------------------------------------------
// Extended dynamic range
//
// Two selectors, one quantity, and a question that this host can answer.
//
// # What the guest asks
//
// `-[NSScreen maximumPotentialExtendedDynamicRangeColorComponentValue]` and
// `-[NSScreen maximumExtendedDynamicRangeColorComponentValue]` are `CGFloat` -
// "how many times the standard dynamic range the display can reach", `1.0` being
// exactly the SDR range and nothing above it.
//
// `BuildDisplayForScreen` in `ui/display/mac/screen_mac.mm` reads both, and the
// disassembly of this framework's copy of it is the authority for what it does
// with them. The sending function is `Framework+0x6a8d750`, the two sends are at
// `+0x6a8da70` and `+0x6a8da85` from selectors loaded at `+0x6a8da66` and
// `+0x6a8da7b` (`__objc_selrefs` `+0xe6ead68` and `+0xe6ead70`), and each is the
// **only** load of its selref anywhere in `__TEXT,__text` - measured by asking,
// at every byte of the section, whether the `disp32` there names the slot,
// which makes no assumption about where the instruction begins. The result
// lands in `xmm0` and is stored with `movsd`, so it is a `double`.
//
// The source, which the disassembly matches instruction for instruction:
//
//     const float max_potential_edr_value = screen.<potential>;
//     const float max_edr_value         = screen.<current>;
//     if (max_potential_edr_value > 1.f) {
//       enable_hdr = true;
//       if (CGDisplayIsBuiltin(display_id) && max_potential_edr_value <= 2.f)
//         enable_hdr = false;                      // crbug.com/1402882
//       if (enable_hdr)
//         hdr_max_lum_relative = std::max(1.0625f, max_edr_value);
//     }
//
// The three constants are the binary's own: `1.0f` at `+0xd64f580`, `2.0f` at
// `+0xd64f62c`, `1.0625f` at `+0xd650b14`.
//
// `enable_hdr` is then published: `set_color_depth(30)` and
// `set_depth_per_component(10)` rather than `24` and `8` (`0x1e`/`0x18` and
// `10`/`8` at `+0x6a8ddaf`-`+0x6a8ddc9`), the output colour space switched to
// extended sRGB with an `RGBA_F16` buffer, and `SetHDRMaxLuminanceRelative` given
// the headroom. So the value is read and acted on; it is not a probe.
//
// # Whether the guest tolerates absence
//
// It cannot tell. Over the whole `ScreenMac` region `+0x6a8a000..+0x6a8e200`
// there is **no** load of `instancesRespondToSelector:` (`+0xe6e8620`),
// `respondsToSelector:` (`+0xe6e8ac8`) or `conformsToProtocol:` (`+0xe6ea6d0`) -
// zero of each, with a positive control firing in the same instrument and over
// the same range. So the send at `+0x6a8da70` is unconditional, at the callee
// and at the caller alike.
//
// # Where the number comes from
//
// This runtime's `NSScreen` is one object per SDL display - `screen_for` over
// `sdl3::video::displays()` - so the question "what is this screen's EDR
// headroom" is exactly "what does the host say about this display", and the host
// is asked. SDL states it per display, and states it in the *same units*:
//
//   src/video/SDL_video.c, SDL_AddVideoDisplay:
//       new_display->HDR.HDR_headroom = SDL_max(display->HDR.HDR_headroom, 1.0f);
//       SDL_SetBooleanProperty(props, SDL_PROP_DISPLAY_HDR_ENABLED_BOOLEAN,
//                              new_display->HDR.HDR_headroom > 1.0f);
//
// Two consequences, and the second is the one that makes this derivable rather
// than a guess:
//
//   * the headroom has a **floor of 1.0**, so `HDR_enabled == false` is not
//     "unknown": it is the statement `headroom <= 1.0`, and the floor makes that
//     `headroom == 1.0`. `1.0` is the *only* value consistent with it.
//   * on the Wayland backend the headroom is the compositor's own number.
//     `src/video/wayland/SDL_waylandcolor.c`:
//         image_description_info_handle_luminances:
//             state->HDR.HDR_headroom = (float)max_lum / (float)reference_lum;
//     fed by `wp_color_manager_v1` ->
//     `wp_color_management_output_v1.get_image_description` ->
//     `wp_image_description_info_v1.v1_luminances`.
//
// # Measured on this host, not assumed
//
// With `SDL_VIDEODRIVER=wayland` (the session's own display server; the display
// is `eDP-1`, 2560x1600, on `card2`):
//
//     SDL.display.HDR_enabled     present, BOOLEAN, false
//     SDL.window.HDR_headroom     present, FLOAT,  1.0  (bits 0x3f800000)
//     SDL.window.SDR_white_level  present, FLOAT,  1.0
//
// and identically under `SDL_VIDEODRIVER=x11`. The window-level float is the
// compositor's `max_lum / reference_lum` read through the same protocol, so it
// is the same quantity this platform reports for a display with no headroom
// above the SDR white point. This is why the answer below is `1.0` **because the
// host's display has no EDR headroom**, and not because `1.0` is a value that
// suits the guest's branch.
//
// The caveat, stated because it bounds the claim: the X11 backend of SDL 3.4.16
// contains **no** reference to headroom or HDR at all
// (`src/video/x11/SDL_x11video.c`), so on X11 the `1.0` is SDL's floor rather
// than a compositor report. The Wayland figure is the measurement; the X11
// figure agrees with it, and this host runs Wayland.

/// The SDR range as a multiple: the range 0.0 - 1.0, and the floor SDL clamps
/// every display's headroom to.
///
/// Named rather than written as a bare `1.0` so that the one place it is a
/// *measurement* and the one place it is a *definition* - this, and SDL's
/// `SDL_max(..., 1.0f)` two lines into the comment above - are visibly the same
/// number rather than two literals that happen to agree.
const SDR_HEADROOM: CGFloat = 1.0;

/// The host's extended-dynamic-range headroom for a display, when it says.
///
/// `Some(v)` is measured; `None` is *not derivable*, which is a different
/// answer from `Some(1.0)` and is the case the callers below refuse on.
fn host_edr_headroom(display: u32) -> Option<CGFloat> {
    let Some(api) = sdl_properties::api() else {
        return None;
    };
    // `display` is an SDL display id: every one this crate has is, they all come
    // from `sdl3::video::displays()`. SDL's own `CHECK_DISPLAY_MAGIC` answers 0
    // for one it does not know, and 0 is also the "no properties" answer
    // `SDL_GetPropertyType` gives for a properties id of 0, so the check below
    // is the right way to see that.
    // SAFETY: a display id, as above.
    let properties = unsafe { (api.get_display_properties)(display) };
    if properties == 0 {
        return None;
    }
    // SAFETY: a live properties id and SDL's own NUL-terminated key.
    let known = unsafe { (api.has_property)(properties, sdl_properties::HDR_ENABLED.as_ptr()) };
    if !known {
        // Absent is *not* false. SDL sets this property unconditionally when a
        // display is added, so an absent one means the entry point or the key
        // is not the one this was written against - which is a wrong answer
        // waiting to happen, not an SDR display.
        return None;
    }
    // SAFETY: the property is present, so its type is BOOLEAN by construction,
    // and the third argument is the default for an absent one - which it is not.
    let enabled = unsafe {
        (api.get_boolean_property)(properties, sdl_properties::HDR_ENABLED.as_ptr(), false)
    };
    // `false` is `headroom <= 1.0` and SDL's floor is `1.0`, so this is the
    // value and not a bound on one. `true` means there is headroom and SDL does
    // not publish how much for a *display* - only a boolean - so there is
    // nothing here to read and the number is not derivable.
    (!enabled).then_some(SDR_HEADROOM)
}

/// `CGFloat -[NSScreen maximumPotentialExtendedDynamicRangeColorComponentValue]`.
///
/// The display's *ceiling*: the most headroom it could reach, as a multiple of
/// the SDR range.
unsafe extern "C" fn maximum_potential_extended_dynamic_range_color_component_value(
    receiver: Id,
    sel: Sel,
) -> CGFloat {
    // SAFETY: a screen of this crate's, and `-...Value` is `CGFloat (id, SEL)`.
    unsafe { extended_dynamic_range(receiver, sel) }
}

/// `CGFloat -[NSScreen maximumExtendedDynamicRangeColorComponentValue]`.
///
/// The display's *current* headroom, which is what the guest stores as
/// `SetHDRMaxLuminanceRelative` once it has decided the display is HDR.
///
/// Sent unconditionally by the source and, because the compiler sinks the send
/// to the branch that uses it, reached in this binary only when the potential
/// value above came back above `1.0`. It is answered from the same measurement
/// and for the same reason, and the two differ only where the host reports a
/// difference between what a display could reach and what it is reaching now -
/// which SDL does not distinguish for a display at all, so both are the same
/// figure here and saying so is better than inventing a difference.
unsafe extern "C" fn maximum_extended_dynamic_range_color_component_value(
    receiver: Id,
    sel: Sel,
) -> CGFloat {
    // SAFETY: a screen of this crate's, and `-...Value` is `CGFloat (id, SEL)`.
    unsafe { extended_dynamic_range(receiver, sel) }
}

/// The shared body of the two selectors, and the refusal.
///
/// # Safety
///
/// `screen` must be null or a screen of this crate's.
unsafe fn extended_dynamic_range(screen: Id, sel: Sel) -> CGFloat {
    let display = display_of(screen);
    match host_edr_headroom(display) {
        Some(headroom) => headroom,
        None => refuse_underivable_headroom(
            sel,
            display,
            if sdl_properties::api().is_some() {
                "SDL reported no extended-dynamic-range information for it"
            } else {
                "this host's SDL has no SDL_GetDisplayProperties, so its per-display \
                 properties cannot be read at all"
            },
        ),
    }
}

/// Refuse a number that cannot be derived, and say what would settle it.
///
/// The return type is a `CGFloat` with no error channel, so there is nothing to
/// refuse *with*: a number handed back here would be read as a claim about the
/// display's dynamic range, and a wrong one of those is invisible - the guest
/// would configure its colour pipeline from it and carry on. So this stops,
/// having named itself, which is the outcome this project prefers over an
/// answer that looks like success.
///
/// What would settle it: a host source that reports the display's headroom as a
/// *number*. SDL publishes one per **window** (`SDL_PROP_WINDOW_HDR_HEADROOM_FLOAT`,
/// the compositor's own `max_lum / reference_lum`) but only a boolean per
/// **display**, and this is a question about the display. A DRM connector's
/// `HDR_OUTPUT_METADATA` would be the other candidate; `modetest -c` on this
/// host's one connected connector lists no HDR/EDR/colour property on any of
/// them.
fn refuse_underivable_headroom(sel: Sel, display: u32, why: &str) -> ! {
    // SAFETY: a registered selector is its own NUL-terminated name.
    let name = unsafe { std::ffi::CStr::from_ptr(sel) }.to_string_lossy();
    objc::fail!(
        "appkit::screen: -[{name}] has no derivable answer - SDL display {display} reports \
         headroom above the SDR white point, and {why}; refusing rather than returning a \
         number that is not a fact about this host's display. The guest reads the return \
         value unconditionally: Framework+0x6a8da70 sends it with no \
         instancesRespondToSelector: guard anywhere in +0x6a8a000..+0x6a8e200, and \
         BuildDisplayForScreen (ui/display/mac/screen_mac.mm) turns it into \
         set_color_depth(30) and an extended-sRGB RGBA_F16 output"
    )
}

/// SDL's per-display properties, reached without going through `sdl3`.
///
/// # Why not `crates/sdl3`
///
/// That crate resolves a fixed list of SDL entry points and these are not on it,
/// and it is another agent's file. `dlsym(RTLD_DEFAULT, ...)` finds them anyway
/// and finds the *same* library: `sdl3::library()` opens it with `RTLD_GLOBAL`,
/// so its symbols are in the global scope as soon as anything in this crate has
/// called into `sdl3` - and every screen in this file comes from
/// `sdl3::video::displays()`, so by the time one of these methods can run that
/// has happened. The lookup therefore happens on a path that has *already* been
/// through `sdl3`, which is what makes `RTLD_DEFAULT` the right handle here
/// rather than a guess.
///
/// # Why `dlsym` is declared here rather than taken from `libc`
///
/// Because `appkit` does not depend on `libc`, and a manifest edit to add it
/// would be outside this file. Two declarations and one constant, both read out
/// of the host's own headers: `void *dlsym(void *handle, const char *symbol)`
/// from `dlfcn.h`, and `RTLD_DEFAULT` as `((void *) 0)` from the same header at
/// line 50. Nothing here is a remembered value.
///
/// # Why a misspelt key cannot produce a wrong value
///
/// The property name is a string literal, so it *can* be wrong. It is checked
/// against SDL itself: [`host_edr_headroom`] returns `None` unless
/// `SDL_HasProperty` says the key is really there, and `None` refuses. A wrong
/// key can only ever cost a refusal, never produce a number - and
/// `the_edr_property_key_is_one_sdl_really_has` fails if it ever happens.
mod sdl_properties {
    use std::ffi::{CStr, c_char, c_void};
    use std::sync::OnceLock;

    // `void *dlsym(void *handle, const char *symbol)`, from `<dlfcn.h>`.
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    /// `RTLD_DEFAULT`, `((void *) 0)` in `<dlfcn.h>`: search the global scope,
    /// which is where an `RTLD_GLOBAL` `dlopen` put the library.
    const RTLD_DEFAULT: *mut c_void = std::ptr::null_mut();

    /// `SDL_PropertiesID SDL_GetDisplayProperties(SDL_DisplayID displayID)`.
    ///
    /// `SDL_PropertiesID` is `Uint32`; `0` is SDL's own "no such object" value
    /// and every `SDL_GetPropertyType` answers `SDL_PROPERTY_TYPE_INVALID` for
    /// it, which is what makes the `0` check in the caller sound.
    type GetDisplayProperties = unsafe extern "C" fn(u32) -> u32;
    /// `bool SDL_HasProperty(SDL_PropertiesID props, const char *name)`.
    type HasProperty = unsafe extern "C" fn(u32, *const c_char) -> bool;
    /// `bool SDL_GetBooleanProperty(SDL_PropertiesID props, const char *name, bool default_value)`.
    type GetBooleanProperty = unsafe extern "C" fn(u32, *const c_char, bool) -> bool;
    /// `const char * SDL_GetDisplayName(SDL_DisplayID displayID)`.
    ///
    /// Owned by SDL, and valid until the display changes - which is why the
    /// caller copies it out rather than keeping the pointer.
    type GetDisplayName = unsafe extern "C" fn(u32) -> *const c_char;

    /// `SDL_PROP_DISPLAY_HDR_ENABLED_BOOLEAN`, from `SDL3/SDL_video.h`:
    /// `#define SDL_PROP_DISPLAY_HDR_ENABLED_BOOLEAN "SDL.display.HDR_enabled"`.
    pub(super) const HDR_ENABLED: &CStr = c"SDL.display.HDR_enabled";

    pub(super) struct Api {
        pub(super) get_display_properties: GetDisplayProperties,
        pub(super) has_property: HasProperty,
        pub(super) get_boolean_property: GetBooleanProperty,
        pub(super) get_display_name: GetDisplayName,
    }

    /// The display's name, as its user sees it, or `None` if SDL has none.
    ///
    /// # Safety
    ///
    /// `display` must be an SDL display id.
    pub(super) unsafe fn display_name(display: u32) -> Option<String> {
        let api = api()?;
        // SAFETY: a display id, and the returned pointer is SDL's own string for
        // as long as the display exists - so it is copied here, not kept.
        let name = unsafe { (api.get_display_name)(display) };
        if name.is_null() {
            return None;
        }
        // SAFETY: SDL returns a NUL-terminated string or null.
        Some(unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy().into_owned())
    }

    /// The three entry points, resolved once.
    ///
    /// `None` when the host's SDL is too old for them, which is the
    /// "cannot be measured" case and not an error.
    pub(super) fn api() -> Option<&'static Api> {
        static API: OnceLock<Option<Api>> = OnceLock::new();
        API.get_or_init(|| {
            /// `T` must be the prototype SDL's header declares for `name`.
            ///
            /// # Safety
            ///
            /// The caller states which prototype that is.
            unsafe fn entry<T: Copy>(name: &CStr) -> Option<T> {
                // An entry point is a bare function pointer on this target; a
                // `T` of any other shape would be a call through the wrong one.
                assert_eq!(size_of::<T>(), size_of::<*const c_void>());
                // SAFETY: `RTLD_DEFAULT` and a NUL-terminated name.
                let found = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) };
                if found.is_null() {
                    return None;
                }
                // The *value* of the symbol, which is a function pointer: the
                // address `dlsym` answered is where the pointer is stored, so
                // this reads through it rather than casting it. Casting alone
                // would call whatever bytes sit at the entry point, which is the
                // first instruction of the function - and segfault on it.
                // SAFETY: a resolved symbol, read at the size asserted above.
                Some(unsafe { *(&found as *const *mut c_void as *const T) })
            }
            Some(Api {
                // SAFETY: SDL's prototypes for these three names.
                get_display_properties: unsafe { entry(c"SDL_GetDisplayProperties") }?,
                // SAFETY: as above.
                has_property: unsafe { entry(c"SDL_HasProperty") }?,
                // SAFETY: as above.
                get_boolean_property: unsafe { entry(c"SDL_GetBooleanProperty") }?,
                // SAFETY: as above.
                get_display_name: unsafe { entry(c"SDL_GetDisplayName") }?,
            })
        })
        .as_ref()
    }
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
    let set = tables.iter().find(|(d, _)| *d == display).map(|(_, t)| t.clone());
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
            "rosetta3: appkit: the program set a display gamma table; it is kept but not \
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
#[repr(C)]
struct DisplayMode {
    display: u32,
    fixed: Option<sdl3::DisplayMode>,
}

fn new_mode(display: u32, fixed: Option<sdl3::DisplayMode>) -> *const c_void {
    let mode = Box::leak(Box::new(DisplayMode { display, fixed }));
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
    bytes[..8].copy_from_slice(b"ROSETTA3");
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
    });
    methods!(class, {
        "frame" => frame, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "visibleFrame" => visible_frame, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "backingScaleFactor" => backing_scale_factor, c"d16@0:8";
        "maximumPotentialExtendedDynamicRangeColorComponentValue" =>
            maximum_potential_extended_dynamic_range_color_component_value, c"d16@0:8";
        "maximumExtendedDynamicRangeColorComponentValue" =>
            maximum_extended_dynamic_range_color_component_value, c"d16@0:8";
        "maximumFramesPerSecond" => maximum_frames_per_second, c"q16@0:8";
        "depth" => depth, c"i16@0:8";
        "colorSpace" => color_space, c"@16@0:8";
        "deviceDescription" => device_description, c"@16@0:8";
        "localizedName" => localized_name, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_global_warp_is_translated_to_window_coordinates() {
        assert_eq!(local_warp_point((420.0, 310.0), (100, 50)), (320.0, 260.0));
    }

    /// The property key is one SDL really has.
    ///
    /// This is the guard on the one thing in the derivation that is a string
    /// literal rather than a measurement. A key SDL does not have makes
    /// `SDL_HasProperty` false, `host_edr_headroom` return `None`, and the two
    /// selectors refuse by name - safe, but a silent loss of the answer, and the
    /// kind of thing a green suite would never notice. So: **if the three entry
    /// points resolved, the key must be a real property.** A host whose SDL is too
    /// old to have them resolves nothing and this is vacuous, which is correct.
    #[test]
    fn the_edr_property_key_is_one_sdl_really_has() {
        crate::bootstrap();
        let Some(display) = sdl3::video::displays().first().copied() else {
            eprintln!("skipped: this host has no display");
            return;
        };
        let Some(api) = sdl_properties::api() else {
            eprintln!("skipped: this host's SDL has no SDL_GetDisplayProperties");
            return;
        };
        // SAFETY: a display id from `sdl3::video::displays()`.
        let properties = unsafe { (api.get_display_properties)(display) };
        assert_ne!(properties, 0, "SDL_GetDisplayProperties answered 0 for a display id SDL knows");
        // SAFETY: a live properties id and SDL's own NUL-terminated key.
        assert!(
            unsafe { (api.has_property)(properties, sdl_properties::HDR_ENABLED.as_ptr()) },
            "{} is not a property of this display; the answer would be a refusal and \
             never a number",
            sdl_properties::HDR_ENABLED.to_string_lossy(),
        );
    }

    /// `1.0` here is a *measurement*, and this is the test that makes it one.
    ///
    /// `host_edr_headroom` is allowed to answer exactly two things: the headroom
    /// the host reports, or "not derivable". A `Some(v)` for any `v` is a fact
    /// SDL stated about this display, because SDL defines the property as
    /// `HDR_headroom > 1.0` with `HDR_headroom` floored at `1.0`
    /// (`src/video/SDL_video.c`, `SDL_AddVideoDisplay`). So the value is checked
    /// against the host rather than against a constant: whatever SDL says must be
    /// consistent with the property, and `1.0` is the only value a `false`
    /// property admits.
    #[test]
    fn the_edr_headroom_is_either_sdl_s_value_or_not_derivable_at_all() {
        crate::bootstrap();
        let Some(display) = sdl3::video::displays().first().copied() else {
            eprintln!("skipped: this host has no display");
            return;
        };
        match host_edr_headroom(display) {
            // SDL's floor. A `Some` above it would mean SDL says the display has
            // headroom, which `host_edr_headroom` refuses rather than guesses at.
            Some(headroom) => assert_eq!(
                headroom, SDR_HEADROOM,
                "SDL reported no headroom above the SDR white point, so the only \
                 value consistent with that is the floor itself"
            ),
            // Not derivable, which is a real outcome and not a failure: a host
            // whose SDL lacks the entry points, or which declines to report HDR
            // for the display. The selectors refuse by name in that case.
            None => eprintln!(
                "note: this host's SDL reports no EDR headroom for display {display}; \
                 the two selectors refuse by name rather than answer"
            ),
        }
    }

    /// The measurement is stable, and a screen's answer is its display's answer.
    ///
    /// A per-call `dlsym` or a per-call property read that varied would make the
    /// two selectors disagree with each other, and the guest's own source treats
    /// them as one quantity: `hdr_max_lum_relative` is `max(1.0625f,
    /// max_edr_value)` and the branch is `max_potential_edr_value > 1.f`.
    ///
    /// The IMPs are read back out of the class rather than called directly, so
    /// this also exercises the table the guest dispatches through.
    #[test]
    fn both_edr_selectors_answer_the_same_display_the_same_way() {        crate::bootstrap();
        let Some(display) = sdl3::video::displays().first().copied() else {
            eprintln!("skipped: this host has no display");
            return;
        };
        let Some(headroom) = host_edr_headroom(display) else {
            eprintln!("skipped: not derivable on this host, so both refuse alike");
            return;
        };
        let screen = screen_for(display);
        let class = objc::class::lookup(c"NSScreen").expect("declared");
        for name in [
            "maximumPotentialExtendedDynamicRangeColorComponentValue",
            "maximumExtendedDynamicRangeColorComponentValue",
        ] {
            let imp = class
                .method(objc::sel::get(name))
                .unwrap_or_else(|| panic!("-[{name}] is not on NSScreen"))
                .implementation();
            let call: unsafe extern "C" fn(Id, Sel) -> CGFloat =
                unsafe { std::mem::transmute(imp) };
            // SAFETY: a `CGFloat` getter on a screen of this crate's, which is
            // the prototype both selectors are registered with.
            let got: CGFloat = unsafe { call(screen, objc::sel::get(name)) };
            assert_eq!(got, headroom, "-[{name}] disagreed with the host's display");
        }
    }

    /// The two selectors are registered, and their encodings are the guest's.
    /// `instancesRespondToSelector:` answers from a real method table, so this is
    /// also the check that the guest's *guard*, had it had one, would see. It has
    /// none - measured over `+0x6a8a000..+0x6a8e200` - so the table is the only
    /// thing standing between the guest and an unrecognised-selector abort, and a
    /// typo in either spelling would put the run back where it started.
    #[test]
    fn a_screen_answers_both_extended_dynamic_range_selectors() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSScreen").expect("declared");
        for name in [
            "maximumPotentialExtendedDynamicRangeColorComponentValue",
            "maximumExtendedDynamicRangeColorComponentValue",
        ] {
            let method = class
                .method(objc::sel::get(name))
                .unwrap_or_else(|| {
                    panic!(
                        "-[{name}] is not on NSScreen, so the send at Framework+0x6a8da70 \
                         reaches the unrecognised-selector failure"
                    )
                });
            // SAFETY: a method's type encoding is a NUL-terminated string the
            // class holds for as long as the method exists.
            let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
            assert_eq!(
                types.to_bytes(),
                c"d16@0:8".to_bytes(),
                "-[{name}] has the wrong type encoding: the guest reads the result as a \
                 CGFloat, and anything else is a value read out of the wrong register"
            );
        }
    }

    /// A screen's name is the host's name for its display, and it is an
    /// `NSString` the guest can keep.
    ///
    /// Checked rather than asserted against a literal, because the *host* is
    /// what has to agree: this is a statement about the display server's name
    /// for the panel, and there is no Mac here whose wording could be compared
    /// to. What must hold is that the string is the one SDL reports and that it
    /// survives as an object - the guest stores it in a `std::string` for the
    /// lifetime of the `Display`, so a borrowed or freed pointer would be a
    /// use-after-free rather than a wrong name.
    #[test]
    fn a_screens_name_is_the_displays_name_and_outlives_the_call() {
        crate::bootstrap();
        let Some(display) = sdl3::video::displays().first().copied() else {
            eprintln!("skipped: this host has no display");
            return;
        };
        // SAFETY: a display id, from `sdl3::video::displays()`.
        let Some(expected) = (unsafe { sdl_properties::display_name(display) }) else {
            eprintln!("skipped: SDL has no name for this display, so the selector refuses");
            return;
        };
        let screen = screen_for(display);
        // SAFETY: a screen of this crate's, and `-localizedName` is
        // `NSString * (id, SEL)`.
        let name = unsafe { localized_name(screen, objc::sel::get("localizedName")) };
        assert!(!name.is_null());
        // Read it back the way the guest does - `base::SysNSStringToUTF8` is
        // `-UTF8String` - so this is the same read, and it also proves the
        // object outlived the call that made it.
        let class = objc::class::lookup(c"NSString").expect("declared");
        let imp = class
            .method(objc::sel::get("UTF8String"))
            .expect("-[NSString UTF8String] is Foundation's")
            .implementation();
        let call: unsafe extern "C" fn(Id, Sel) -> *const std::ffi::c_char =
            unsafe { std::mem::transmute(imp) };
        // SAFETY: a string this runtime made, and `-UTF8String` is
        // `const char * (id, SEL)`.
        let utf8 = unsafe { call(name, objc::sel::get("UTF8String")) };
        assert!(!utf8.is_null());
        // SAFETY: a NUL-terminated string Foundation just produced.
        let text = unsafe { std::ffi::CStr::from_ptr(utf8) }.to_string_lossy();
        assert_eq!(
            text, expected,
            "the screen's name is not the name the host reports for its display"
        );
    }
}

/// `CGError CGWarpMouseCursorPosition(CGPoint)` - move the pointer, in global
/// screen coordinates.
///
/// SDL treats an in-window warp during relative mode as virtual, so the first
/// warp to a target momentarily releases relative mode and moves the physical
/// desktop cursor, as CoreGraphics promises. Repeated warps to that same target
/// only refresh SDL's virtual position: the physical cursor is already there,
/// and toggling relative mode every frame would flush real camera motion.
/// `kCGErrorSuccess` is `0`.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CGWarpMouseCursorPosition(point: foundation::geometry::NSPoint) -> i32 {
    let point = (point.x as f32, point.y as f32);
    let windows = crate::window::sdl_windows();
    let target = crate::event::pointer_window();
    let origin = target
        .and_then(sdl3::video::window_position)
        .unwrap_or((0, 0));
    let local = local_warp_point(point, origin);
    let from = crate::event::pointer_position();
    crate::event::warp_pointer(local.0, local.1);

    let relative: Vec<_> = windows
        .iter()
        .copied()
        .filter(|window| sdl3::video::relative_mouse_mode(*window))
        .collect();
    if relative.is_empty() {
        forget_relative_warp();
        crate::event::expect_warp_echo(from, local);
        if !sdl3::video::warp_mouse_global(point.0, point.1)
            && let Some(window) = target
        {
            sdl3::video::warp_mouse_in_window(window, local.0, local.1);
        }
        return 0;
    }
    if known_relative_warp() == Some(point) {
        // Relative deltas advance SDL's virtual absolute position. Refresh it
        // even when the physical cursor needs no second warp, or SDL exposes
        // the accumulated drag when relative mode ends.
        for window in relative {
            let origin = sdl3::video::window_position(window).unwrap_or((0, 0));
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
    let warped = sdl3::video::warp_mouse_global(point.0, point.1)
        || target.is_some_and(|window| sdl3::video::warp_mouse_in_window(window, local.0, local.1));
    for window in relative {
        sdl3::video::set_relative_mouse_mode(window, true);
    }
    match released && warped {
        true => remember_relative_warp(point),
        false => forget_relative_warp(),
    }
    0
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

/// Put every window into relative mouse mode, or take it out, to match the last
/// [`CGAssociateMouseAndMouseCursorPosition`].
///
/// Called again when a window takes focus, because SDL leaves the mode when
/// focus goes and a window made after the guest disassociated never had it.
pub fn apply_mouse_association() {
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
                let origin = sdl3::video::window_position(window).unwrap_or((0, 0));
                let global = (point.0 + origin.0 as f32, point.1 + origin.1 as f32);
                if !sdl3::video::warp_mouse_global(global.0, global.1) {
                    sdl3::video::warp_mouse_in_window(window, point.0, point.1);
                }
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
