#![allow(
    clippy::doc_lazy_continuation,
    clippy::items_after_test_module,
    clippy::manual_is_multiple_of,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::only_used_in_recursion,
    clippy::ptr_eq,
    clippy::single_match,
    clippy::useless_conversion
)]

mod about;
pub mod alert;
mod animation;
pub mod appearance;
pub mod application;
pub mod backing;
pub mod button;
pub mod cgcontext;
pub mod cgevent;
pub mod cgl;
pub mod classes;
pub mod color;
mod controls;
pub mod coretext;
pub mod cursor;
pub(crate) mod decode;
pub mod decor;
mod define;
pub mod dialog;
pub mod dock;
pub mod event;
mod font;
pub use chrome::fonts;
pub mod geometry;
pub mod graphics;
pub mod icon;
pub mod image;
mod input_context;
pub mod keymap;
pub mod layer;
mod layout;
pub mod menu;
pub mod metadata;
pub mod monitor;
pub mod nib;
pub mod nsevent;
pub mod opengl;
pub mod panel;
mod paragraph;
pub mod pasteboard;
mod pasteboard_manager;
pub mod raster;
mod responder;
pub mod screen;
pub mod script;
pub mod text;
pub mod textinput;
mod transform;
pub mod uchr;
pub mod view;
pub mod window;
pub mod workspace;

pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        decor::install();
        classes::install();
        alert::install();
        panel::install();
        appearance::install();
        application::install();
        dock::install();
        workspace::install();
        color::install();
        graphics::install();
        cgcontext::install();
        cursor::install();
        font::install();
        image::install();
        image::bitmap::install();
        menu::install();
        screen::install();
        window::install();
        view::install();
        layout::install();
        controls::install();
        animation::install();
        transform::install();
        paragraph::install();
        backing::install();
        layer::install();
        text::install();
        input_context::install();
        opengl::install();
        darwin::idle::on_idle(opengl::release_for_other_threads);
        responder::install();
        pasteboard::install();
        nsevent::install();
        monitor::install();
    });
}

exports::exports! {
    functions {
        "NSApplicationMain" => application::NSApplicationMain,
        "CGMainDisplayID" => screen::CGMainDisplayID,
        "CGSessionCopyCurrentDictionary" => screen::CGSessionCopyCurrentDictionary,
        "CGDisplayBounds" => screen::CGDisplayBounds,
        "CGGetDisplayTransferByTable" => screen::CGGetDisplayTransferByTable,
        "CGSetDisplayTransferByTable" => screen::CGSetDisplayTransferByTable,
        "CGDisplayRestoreColorSyncSettings" => screen::CGDisplayRestoreColorSyncSettings,
        "CGDisplayScreenSize" => screen::CGDisplayScreenSize,
        "CGDisplayIsMain" => screen::CGDisplayIsMain,
        "CGDisplayIsOnline" => screen::CGDisplayIsOnline,
        "CGDisplayIsAsleep" => screen::CGDisplayIsAsleep,
        "CGDisplayMirrorsDisplay" => screen::CGDisplayMirrorsDisplay,
        "CGDisplayModelNumber" => screen::CGDisplayModelNumber,
        "CGDisplayVendorNumber" => screen::CGDisplayVendorNumber,
        "CGDisplaySerialNumber" => screen::CGDisplaySerialNumber,
        "CGDisplayCopyDisplayMode" => screen::CGDisplayCopyDisplayMode,
        "CGDisplayModeGetRefreshRate" => screen::CGDisplayModeGetRefreshRate,
        "CGDisplayModeRelease" => screen::CGDisplayModeRelease,
        "CGDisplayModeRetain" => screen::CGDisplayModeRetain,
        "CGDisplayCopyAllDisplayModes" => screen::CGDisplayCopyAllDisplayModes,
        "CGDisplayModeGetWidth" => screen::CGDisplayModeGetWidth,
        "CGDisplayModeGetHeight" => screen::CGDisplayModeGetHeight,
        "CGDisplayModeGetPixelWidth" => screen::CGDisplayModeGetPixelWidth,
        "CGDisplayModeGetPixelHeight" => screen::CGDisplayModeGetPixelHeight,
        "CGDisplayModeGetIOFlags" => screen::CGDisplayModeGetIOFlags,
        "CGDisplayModeIsUsableForDesktopGUI" => screen::CGDisplayModeIsUsableForDesktopGUI,
        "CGDisplayRegisterReconfigurationCallback" => screen::CGDisplayRegisterReconfigurationCallback,
        "CGDisplayRemoveReconfigurationCallback" => screen::CGDisplayRemoveReconfigurationCallback,
        "CGGetDisplaysWithRect" => screen::CGGetDisplaysWithRect,
        "CGDisplayCreateUUIDFromDisplayID" => screen::CGDisplayCreateUUIDFromDisplayID,
        "CGDisplayGetDisplayIDFromUUID" => screen::CGDisplayGetDisplayIDFromUUID,
        "NSBitsPerSampleFromDepth" => screen::NSBitsPerSampleFromDepth,
        "NSBitsPerPixelFromDepth" => screen::NSBitsPerPixelFromDepth,
        "NSNumberOfColorComponents" => screen::NSNumberOfColorComponents,
        "NSPlanarFromDepth" => screen::NSPlanarFromDepth,
        "NSColorSpaceFromDepth" => screen::NSColorSpaceFromDepth,
        "NSBestDepth" => screen::NSBestDepth,
        "CGGetOnlineDisplayList" | "CGGetActiveDisplayList" => screen::CGGetOnlineDisplayList,
        "CGDisplayIDToOpenGLDisplayMask" => screen::CGDisplayIDToOpenGLDisplayMask,
        "CGDisplayIOServicePort" => screen::CGDisplayIOServicePort,
        "CGDisplayHideCursor" => cursor::CGDisplayHideCursor,
        "CGDisplayShowCursor" => cursor::CGDisplayShowCursor,
        "CGCursorIsVisible" => cursor::CGCursorIsVisible,
        "CGWarpMouseCursorPosition" => screen::CGWarpMouseCursorPosition,
        "CGAssociateMouseAndMouseCursorPosition" => screen::CGAssociateMouseAndMouseCursorPosition,
        "CGEventSourceKeyState" => screen::CGEventSourceKeyState,
        "CGEventCreate" => cgevent::CGEventCreate,
        "CGEventGetLocation" => cgevent::CGEventGetLocation,
        "CGEventCreateMouseEvent" => cgevent::CGEventCreateMouseEvent,
        "CGEventSetType" => cgevent::CGEventSetType,
        "CGEventPost" => cgevent::CGEventPost,
        "CGColorSpaceCreateWithName" => graphics::CGColorSpaceCreateWithName,
        "CGColorSpaceCreateDeviceRGB" => graphics::CGColorSpaceCreateDeviceRGB,
        "CGColorSpaceRelease" => graphics::CGColorSpaceRelease,
        "CGDataProviderCreateWithData" => graphics::CGDataProviderCreateWithData,
        "CGDataProviderRelease" => graphics::CGDataProviderRelease,
        "CGImageCreate" => graphics::CGImageCreate,
        "CGImageCreateCopyWithColorSpace" => graphics::CGImageCreateCopyWithColorSpace,
        "CGImageRetain" => graphics::CGImageRetain,
        "CGImageRelease" => graphics::CGImageRelease,
        "CGImageGetWidth" => graphics::CGImageGetWidth,
        "CGImageGetHeight" => graphics::CGImageGetHeight,
        "CGImageGetBitsPerComponent" => graphics::CGImageGetBitsPerComponent,
        "CGImageGetAlphaInfo" => graphics::CGImageGetAlphaInfo,
        "CGImageGetColorSpace" => graphics::CGImageGetColorSpace,
        "CGImageSourceCreateWithData" => graphics::CGImageSourceCreateWithData,
        "CGImageSourceCreateImageAtIndex" => graphics::CGImageSourceCreateImageAtIndex,
        "CGRectGetWidth" => geometry::CGRectGetWidth,
        "CGRectGetHeight" => geometry::CGRectGetHeight,
        "CGRectGetMinX" => geometry::CGRectGetMinX,
        "CGRectGetMidX" => geometry::CGRectGetMidX,
        "CGRectGetMaxX" => geometry::CGRectGetMaxX,
        "CGRectGetMinY" => geometry::CGRectGetMinY,
        "CGRectGetMidY" => geometry::CGRectGetMidY,
        "CGRectGetMaxY" => geometry::CGRectGetMaxY,
        "NSPointInRect" => geometry::NSPointInRect,
        "NSMouseInRect" => geometry::NSMouseInRect,
        "NSInsetRect" => geometry::NSInsetRect,
        "NSDisableScreenUpdates" => geometry::NSDisableScreenUpdates,
        "NSEnableScreenUpdates" => geometry::NSEnableScreenUpdates,
        "CGRectEqualToRect" | "NSEqualRects" => geometry::CGRectEqualToRect,
        "GetCurrentKeyModifiers" => keymap::GetCurrentKeyModifiers,
        "TISCopyCurrentKeyboardInputSource" => textinput::TISCopyCurrentKeyboardInputSource,
        "TISCopyInputMethodKeyboardLayoutOverride" => textinput::TISCopyInputMethodKeyboardLayoutOverride,
        "TISCopyCurrentKeyboardLayoutInputSource" => textinput::TISCopyCurrentKeyboardLayoutInputSource,
        "TISCopyCurrentASCIICapableKeyboardInputSource" => textinput::TISCopyCurrentASCIICapableKeyboardInputSource,
        "TISCopyCurrentASCIICapableKeyboardLayoutInputSource" => textinput::TISCopyCurrentASCIICapableKeyboardLayoutInputSource,
        "TISCopyInputSourceForLanguage" => textinput::TISCopyInputSourceForLanguage,
        "TISCreateInputSourceList" => textinput::TISCreateInputSourceList,
        "TISCreateASCIICapableInputSourceList" => textinput::TISCreateASCIICapableInputSourceList,
        "TISSelectInputSource" => textinput::TISSelectInputSource,
        "TISDeselectInputSource" => textinput::TISDeselectInputSource,
        "TISEnableInputSource" => textinput::TISEnableInputSource,
        "TISDisableInputSource" => textinput::TISDisableInputSource,
        "TISGetInputSourceProperty" => textinput::TISGetInputSourceProperty,
        "KBGetLayoutType" => textinput::KBGetLayoutType,
        "LMGetKbdType" => textinput::LMGetKbdType,
        "LMGetKbdLast" => textinput::LMGetKbdLast,
        "UCKeyTranslate" => textinput::UCKeyTranslate,
        "CGLEnable" => opengl::CGLEnable,
        "CGLDisable" => opengl::CGLDisable,
        "CGLSetParameter" => opengl::CGLSetParameter,
        "CGLSetCurrentContext" => opengl::CGLSetCurrentContext,
        "CGLFlushDrawable" => opengl::CGLFlushDrawable,
        "CGLClearDrawable" => opengl::CGLClearDrawable,
        "CGLLockContext" => opengl::CGLLockContext,
        "CGLUnlockContext" => opengl::CGLUnlockContext,
        "CGLGetShareGroup" => opengl::CGLGetShareGroup,
    }
    groups {
        pasteboard_manager,
        coretext,
        cgcontext,
        cgl,
    }
}

pub fn ns_app_cell() -> u64 {
    corefoundation::base::global_cell("_NSApp", 0)
}

// CoreGraphics exports these structures themselves, rather than pointers to
// them. A guest loads their addresses from its GOT and copies the fields.
static CG_AFFINE_TRANSFORM_IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
/// `{{-CGFLOAT_MAX / 2, -CGFLOAT_MAX / 2}, {CGFLOAT_MAX, CGFLOAT_MAX}}`.
static CG_RECT_INFINITE: [f64; 4] = [f64::MAX / -2.0, f64::MAX / -2.0, f64::MAX, f64::MAX];

pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut out = classes::class_data();
    out.extend(coretext::data_symbols());
    out.extend(cgcontext::data_symbols());
    out.push(("_NSApp".to_string(), ns_app_cell()));
    out.push((
        "_CGAffineTransformIdentity".to_string(),
        CG_AFFINE_TRANSFORM_IDENTITY.as_ptr() as usize as u64,
    ));
    out.push((
        "_CGRectInfinite".to_string(),
        CG_RECT_INFINITE.as_ptr() as usize as u64,
    ));
    let srgb = foundation::notification::name_object("kCGColorSpaceSRGB");
    out.push((
        "_kCGColorSpaceSRGB".to_string(),
        corefoundation::base::global_cell("_kCGColorSpaceSRGB", srgb),
    ));
    out.extend(corefoundation::base::named_string_globals(
        NOTIFICATION_NAMES,
    ));
    out.push((
        "_NSAppKitVersionNumber".to_string(),
        corefoundation::base::global_cell("_NSAppKitVersionNumber", APPKIT_VERSION.to_bits()),
    ));
    out.extend(corefoundation::base::string_globals(
        textinput::STRING_GLOBALS,
    ));
    out.extend(corefoundation::base::string_globals(STRING_GLOBALS));
    // Doubles rather than pointers - see [`FONT_WEIGHTS`].
    for (symbol, weight) in FONT_WEIGHTS {
        out.push((
            (*symbol).to_string(),
            corefoundation::base::global_cell(symbol, weight.to_bits()),
        ));
    }
    out
}

/// The `NSNotificationName` globals a program compares against.
///
/// The window's are here as well as the application's, and some of them are not
/// posted yet - `NSWindowDidResizeNotification` waits on the event pump. They
/// are published anyway, and the reason is the difference between the two ways a
/// symbol can be missing: an unresolved *function* import is refused by name at
/// the call, while an unresolved **data** import binds to zero and the guest's
/// next instruction dereferences it. A name nothing posts is a gap; a name
/// nothing defines is a segfault at address zero.
pub const NOTIFICATION_NAMES: &[&str] = &[
    application::WILL_FINISH_LAUNCHING,
    application::DID_FINISH_LAUNCHING,
    application::WILL_TERMINATE,
    application::DID_BECOME_ACTIVE,
    application::WILL_RESIGN_ACTIVE,
    window::DID_RESIZE,
    window::DID_MOVE,
    window::WILL_CLOSE,
    window::DID_BECOME_KEY,
    window::DID_RESIGN_KEY,
    window::DID_BECOME_MAIN,
    window::DID_RESIGN_MAIN,
    window::DID_MINIATURIZE,
    window::DID_DEMINIATURIZE,
    window::WILL_ENTER_FULL_SCREEN,
    window::WILL_EXIT_FULL_SCREEN,
    window::DID_ENTER_FULL_SCREEN,
    window::DID_EXIT_FULL_SCREEN,
    window::DID_CHANGE_BACKING_PROPERTIES,
    text::DID_CHANGE,
    text::DID_CHANGE_SELECTION,
    text::CONTROL_DID_CHANGE,
    application::DID_CHANGE_SCREEN_PARAMETERS,
    workspace::WILL_SLEEP,
    workspace::DID_WAKE,
    workspace::SCREENS_DID_SLEEP,
    workspace::SCREENS_DID_WAKE,
    view::FRAME_DID_CHANGE,
    view::BOUNDS_DID_CHANGE,
    // Watched by a widget toolkit and posted by nothing here: there is no system
    // colour change to follow, no live resize to bracket, and no colour panel.
    // Published for the reason the note above gives - a name nothing posts is a
    // gap, a name nothing defines is a segfault at address zero.
    "NSSystemColorsDidChangeNotification",
    "NSApplicationWillBecomeActiveNotification",
    window::WILL_START_LIVE_RESIZE,
    window::DID_END_LIVE_RESIZE,
    "NSColorPanelColorDidChangeNotification",
    "NSPreferredScrollerStyleDidChangeNotification",
    "NSTextInputContextKeyboardSelectionDidChangeNotification",
];

/// The other AppKit string globals a program reads.
///
/// Each is a fixed name rather than something this runtime chooses: a colour
/// space and an attribute key are compared against by string, so publishing the
/// documented value is exact.
pub const STRING_GLOBALS: &[(&str, &str)] = &[
    ("_NSCalibratedRGBColorSpace", "NSCalibratedRGBColorSpace"),
    ("_NSDeviceRGBColorSpace", "NSDeviceRGBColorSpace"),
    // The other colour-space names, published so the *comparison* against them
    // works. `-colorSpaceName` answers calibrated RGB for every colour here, so a
    // caller testing against these finds "no" - which is the answer, and which it
    // cannot reach at all while the symbol it is comparing with is address zero.
    (
        "_NSCalibratedWhiteColorSpace",
        "NSCalibratedWhiteColorSpace",
    ),
    (
        "_NSCalibratedBlackColorSpace",
        "NSCalibratedBlackColorSpace",
    ),
    ("_NSDeviceWhiteColorSpace", "NSDeviceWhiteColorSpace"),
    ("_NSDeviceBlackColorSpace", "NSDeviceBlackColorSpace"),
    ("_NSDeviceCMYKColorSpace", "NSDeviceCMYKColorSpace"),
    ("_NSNamedColorSpace", "NSNamedColorSpace"),
    ("_NSPatternColorSpace", "NSPatternColorSpace"),
    ("_NSDeviceResolution", "NSDeviceResolution"),
    ("_NSForegroundColorAttributeName", "NSColor"),
    ("_NSFontAttributeName", "NSFont"),
    ("_NSParagraphStyleAttributeName", "NSParagraphStyle"),
    ("_NSObliquenessAttributeName", "NSObliqueness"),
    ("_NSAppearanceNameAqua", "NSAppearanceNameAqua"),
    ("_NSDeviceSize", "NSDeviceSize"),
    ("_NSModalPanelRunLoopMode", "NSModalPanelRunLoopMode"),
    // The pasteboard's plain-text type, under both its spellings: the modern one
    // is a uniform type identifier and the deprecated one is not, and a program
    // that writes with one and reads with the other has to name the same type.
    ("_NSPasteboardTypeString", "public.utf8-plain-text"),
    ("_NSStringPboardType", "public.utf8-plain-text"),
    // The rest of the deprecated pasteboard types, whose values are their own
    // names - which is what they are on a Mac, and what makes a program that
    // writes with the old spelling and reads with it find its own data.
    ("_NSFilenamesPboardType", "NSFilenamesPboardType"),
    ("_NSFilesPromisePboardType", "NSFilesPromisePboardType"),
    ("_NSURLPboardType", "NSURLPboardType"),
    ("_NSColorPboardType", "NSColorPboardType"),
    ("_NSFontPboardType", "NSFontPboardType"),
    ("_NSRulerPboardType", "NSRulerPboardType"),
    ("_NSTIFFPboardType", "NSTIFFPboardType"),
    ("_NSPDFPboardType", "NSPDFPboardType"),
    ("_NSPostScriptPboardType", "NSPostScriptPboardType"),
    ("_NSRTFPboardType", "NSRTFPboardType"),
    ("_NSRTFDPboardType", "NSRTFDPboardType"),
    ("_NSHTMLPboardType", "NSHTMLPboardType"),
    ("_NSTabularTextPboardType", "NSTabularTextPboardType"),
    ("_NSVCardPboardType", "NSVCardPboardType"),
    ("_NSInkTextPboardType", "NSInkTextPboardType"),
    ("_NSFileContentsPboardType", "NSFileContentsPboardType"),
    (
        "_NSMultipleTextSelectionPboardType",
        "NSMultipleTextSelectionPboardType",
    ),
    // The drag pasteboard's name, which is Apple's string and not the symbol's:
    // a program naming the board by hand has to name the same one.
    ("_NSPasteboardNameDrag", "Apple CFPasteboard drag"),
    // Attributed-string attribute names. Each is shorter than its symbol, as
    // `NSForegroundColorAttributeName` above already is.
    ("_NSUnderlineStyleAttributeName", "NSUnderline"),
    ("_NSUnderlineColorAttributeName", "NSUnderlineColor"),
    ("_NSDocumentTypeDocumentAttribute", "DocumentType"),
    ("_NSHTMLTextDocumentType", "NSHTML"),
    ("_NSRTFTextDocumentType", "NSRTF"),
    // The only named system image a program asks for here.
    ("_NSImageNameApplicationIcon", "NSApplicationIcon"),
];

/// `NSFontWeight` globals, which are **`CGFloat` variables** and not pointers.
///
/// The difference matters: a guest loads the symbol's address and reads a
/// *double* out of it, so publishing one as a string object hands it the bit
/// pattern of a pointer as a font weight. The values are Apple's own, on the
/// -1.0 to 1.0 scale `+systemFontOfSize:weight:` takes.
const FONT_WEIGHTS: &[(&str, f64)] = &[
    ("_NSFontWeightUltraLight", -0.8),
    ("_NSFontWeightThin", -0.6),
    ("_NSFontWeightLight", -0.4),
    ("_NSFontWeightRegular", 0.0),
    ("_NSFontWeightMedium", 0.23),
    ("_NSFontWeightSemibold", 0.3),
    ("_NSFontWeightBold", 0.4),
    ("_NSFontWeightHeavy", 0.56),
    ("_NSFontWeightBlack", 0.62),
];

/// `NSAppKitVersionNumber`, which is a `double` variable rather than a pointer.
///
/// 2022 is Big Sur's, chosen for the reason `kCFCoreFoundationVersionNumber` is
/// 1600: it is the floor every version test in a program of this vintage asks
/// about, and claiming a later one would have a guest take a path against an
/// AppKit this tree does not implement.
const APPKIT_VERSION: f64 = 2022.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// A data symbol is the address of a word holding the value, never the value
    /// itself: the guest loads through it. The check is that reading the cell
    /// gives back an object, which is what a guest's second load does.
    #[test]
    fn a_notification_name_global_is_a_cell_holding_the_string() {
        let symbols = data_symbols();
        let (_, cell) = symbols
            .iter()
            .find(|(n, _)| n == "_NSApplicationDidFinishLaunchingNotification")
            .expect("published");
        // SAFETY: the cell is a word this crate leaked.
        let object = unsafe { *(*cell as *const u64) } as objc::object::Id;
        assert!(!object.is_null());
        // SAFETY: an object this runtime allocated.
        let class = unsafe { objc::object::class_of(object) }.expect("a class");
        assert!(
            class.chain().any(|c| c.name_str().contains("String")),
            "{}",
            class.name_str()
        );
    }

    #[test]
    fn core_graphics_structure_globals_point_at_their_fields() {
        let symbols = data_symbols();
        let (_, identity) = symbols
            .iter()
            .find(|(name, _)| name == "_CGAffineTransformIdentity")
            .expect("published");
        // SAFETY: the symbol points at the six-double static above.
        let fields = unsafe { std::slice::from_raw_parts(*identity as *const f64, 6) };
        assert_eq!(fields, &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }
}
