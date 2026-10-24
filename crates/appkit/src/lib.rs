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
mod aqua;
pub mod backing;
mod bezier;
pub mod button;
mod catalog;
mod cell;
pub use coregraphics::cgbitmap;
pub use coregraphics::cgcolor;
pub use coregraphics::cgcontext;
pub use coregraphics::cgdataprovider;
pub use coregraphics::cgimage;
pub use coregraphics::quartz_event;
pub mod cgl;
pub use coregraphics::cgpaint;
pub mod cgpath;
pub mod classes;
pub mod color;
mod controls;
mod coreanimation;
pub use coretext;
pub mod cursor;
pub(crate) mod decode;
pub mod decor;
mod define;
pub mod dialog;
mod displaylink;
pub mod dock;
pub mod event;
mod font;
pub mod hitheme;
pub mod launch;
pub mod launchservices;
pub use coregraphics::cggeometry;
pub use coregraphics::fonts;
pub(crate) use coregraphics::quartz;
pub mod icon;
pub mod image;
mod input_context;
pub mod keymap;
mod label;
pub mod layer;
mod layout;
mod matrix;
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
mod popup;
pub use coregraphics::raster;
mod responder;
pub mod screen;
pub mod script;
mod scroll;
mod segmented;
mod status;
mod tab;
mod table;
pub mod text;
pub mod textinput;
mod transform;
pub mod uchr;
pub mod view;
mod view_animation;
pub mod window;
mod window_controller;
pub mod workspace;

pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        coregraphics::bootstrap();
        event::install_quartz_event_provider();
        decor::install();
        classes::install();
        alert::install();
        panel::install();
        appearance::install();
        application::install();
        nib::install();
        dock::install();
        workspace::install();
        color::install();
        cursor::install();
        screen::install_quartz_display_provider();
        font::install();
        image::install();
        image::bitmap::install();
        menu::install();
        screen::install();
        displaylink::install();
        window::install();
        window_controller::install();
        view::install();
        layout::install();
        controls::install();
        animation::install();
        transform::install();
        paragraph::install();
        backing::install();
        popup::install();
        scroll::install();
        segmented::install();
        matrix::install();
        tab::install();
        table::install();
        view_animation::install();
        bezier::install();
        layer::install();
        coreanimation::install();
        text::install();
        label::install();
        // Last of the controls (text fields and labels included): a cell
        // forwards what its control implements.
        cell::install();
        hitheme::install();
        input_context::install();
        opengl::install();
        darwin::idle::on_idle(opengl::going_idle);
        responder::install();
        pasteboard::install();
        nsevent::install();
        monitor::install();
        status::install();
    });
}

exports::exports! {
    functions {
        "NSApplicationMain" => application::NSApplicationMain,
        "CAFrameRateRangeMake" => displaylink::CAFrameRateRangeMake,
        "NSBitsPerSampleFromDepth" => screen::NSBitsPerSampleFromDepth,
        "NSBitsPerPixelFromDepth" => screen::NSBitsPerPixelFromDepth,
        "NSNumberOfColorComponents" => screen::NSNumberOfColorComponents,
        "NSPlanarFromDepth" => screen::NSPlanarFromDepth,
        "NSColorSpaceFromDepth" => screen::NSColorSpaceFromDepth,
        "NSBestDepth" => screen::NSBestDepth,

        "GetCurrentKeyModifiers" | "GetCurrentEventKeyModifiers" => keymap::GetCurrentKeyModifiers,
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
        "CGLChoosePixelFormat" => opengl::CGLChoosePixelFormat,
        "CGLDestroyPixelFormat" => opengl::CGLDestroyPixelFormat,
        "CGLCreateContext" => opengl::CGLCreateContext,
        "CGLDestroyContext" => opengl::CGLDestroyContext,
        "CGLReleaseContext" => opengl::CGLReleaseContext,
        "CGLRetainContext" => opengl::CGLRetainContext,
        "CGLGetPixelFormat" => opengl::CGLGetPixelFormat,
        "CGLErrorString" => opengl::CGLErrorString,
        "CGLGetVersion" => opengl::CGLGetVersion,
        "CGLEnable" => opengl::CGLEnable,
        "CGLDisable" => opengl::CGLDisable,
        "CGLGetParameter" => opengl::CGLGetParameter,
        "CGLSetParameter" => opengl::CGLSetParameter,
        "CGLSetCurrentContext" => opengl::CGLSetCurrentContext,
        "CGLGetCurrentContext" => opengl::CGLGetCurrentContext,
        "CGLFlushDrawable" => opengl::CGLFlushDrawable,
        "CGLClearDrawable" => opengl::CGLClearDrawable,
        "CGLLockContext" => opengl::CGLLockContext,
        "CGLUnlockContext" => opengl::CGLUnlockContext,
        "CGLGetShareGroup" => opengl::CGLGetShareGroup,
    }
    groups {
        pasteboard_manager,
        hitheme,
        launchservices,
        cgl,
    }
}

pub fn ns_app_cell() -> u64 {
    corefoundation::base::global_cell("_NSApp", 0)
}

/// `{{-CGFLOAT_MAX / 2, -CGFLOAT_MAX / 2}, {CGFLOAT_MAX, CGFLOAT_MAX}}`.
static CG_RECT_INFINITE: [f64; 4] = [f64::MAX / -2.0, f64::MAX / -2.0, f64::MAX, f64::MAX];

pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut out = classes::class_data();
    out.push(("_NSApp".to_string(), ns_app_cell()));
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
    let gravities: Vec<(&str, &str)> = coreanimation::GRAVITIES
        .iter()
        .map(|(symbol, name, _)| (*symbol, *name))
        .collect();
    out.extend(corefoundation::base::string_globals(&gravities));
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
    application::WILL_BECOME_ACTIVE,
    application::DID_BECOME_ACTIVE,
    application::WILL_RESIGN_ACTIVE,
    window::DID_RESIZE,
    window::DID_MOVE,
    window::DID_CHANGE_SCREEN,
    window::DID_CHANGE_SCREEN_PROFILE,
    window::DID_EXPOSE,
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
    window::WILL_START_LIVE_RESIZE,
    window::DID_END_LIVE_RESIZE,
    "NSColorPanelColorDidChangeNotification",
    "NSPreferredScrollerStyleDidChangeNotification",
    "NSTextInputContextKeyboardSelectionDidChangeNotification",
    // Observed by SDL's window listener; there is no minimize animation, window
    // drag or occlusion tracking here to post them.
    "NSWindowWillMiniaturizeNotification",
    "NSWindowWillMoveNotification",
    "NSWindowDidChangeOcclusionStateNotification",
    // Observed by Gecko's widget layer; no tint change, menu action broadcast
    // or global frame tracking posts them here.
    "NSControlTintDidChangeNotification",
    "NSMenuWillSendActionNotification",
    "NSViewGlobalFrameDidChangeNotification",
];

/// The other AppKit string globals a program reads.
///
/// Each is a fixed name rather than something this runtime chooses: a colour
/// space and an attribute key are compared against by string, so publishing the
/// documented value is exact.
pub const STRING_GLOBALS: &[(&str, &str)] = &[
    ("_kCAMediaTimingFunctionLinear", coreanimation::LINEAR),
    ("_kCAMediaTimingFunctionEaseIn", coreanimation::EASE_IN),
    ("_kCAMediaTimingFunctionEaseOut", coreanimation::EASE_OUT),
    (
        "_kCAMediaTimingFunctionEaseInEaseOut",
        coreanimation::EASE_IN_EASE_OUT,
    ),
    ("_kCAMediaTimingFunctionDefault", coreanimation::DEFAULT),
    ("_kCAFillModeForwards", coreanimation::FILL_FORWARDS),
    ("_kCAFillModeBackwards", coreanimation::FILL_BACKWARDS),
    ("_kCAFillModeBoth", coreanimation::FILL_BOTH),
    ("_kCAFillModeRemoved", coreanimation::FILL_REMOVED),
    ("_kCAFilterNearest", coreanimation::FILTER_NEAREST),
    ("_kCAFilterLinear", coreanimation::FILTER_LINEAR),
    ("_kCAFilterTrilinear", coreanimation::FILTER_TRILINEAR),
    (
        "_NSWorkspaceLaunchConfigurationArguments",
        workspace::LAUNCH_ARGUMENTS,
    ),
    (
        "_NSWorkspaceLaunchConfigurationEnvironment",
        workspace::LAUNCH_ENVIRONMENT,
    ),
    (
        "_NSWorkspaceLaunchConfigurationAppleEvent",
        workspace::LAUNCH_APPLE_EVENT,
    ),
    (
        "_NSWorkspaceLaunchConfigurationArchitecture",
        workspace::LAUNCH_ARCHITECTURE,
    ),
    ("_NSDeviceIsScreen", "NSDeviceIsScreen"),
    ("_NSDragPboard", "Apple CFPasteboard drag"),
    ("_NSEventTrackingRunLoopMode", "NSEventTrackingRunLoopMode"),
    ("_NSPrintPanelAccessorySummaryItemNameKey", "name"),
    (
        "_NSPrintPanelAccessorySummaryItemDescriptionKey",
        "description",
    ),
    ("_kCATransactionAnimationDuration", "animationDuration"),
    ("_kCATransactionDisableActions", "disableActions"),
    ("_kCGColorSpaceGenericRGB", "kCGColorSpaceGenericRGB"),
    (
        "_kCGImageSourceShouldAllowFloat",
        "kCGImageSourceShouldAllowFloat",
    ),
    (
        "_kCGImageSourceTypeIdentifierHint",
        "kCGImageSourceTypeIdentifierHint",
    ),
    ("_kUTTypePNG", "public.png"),
    ("_kLSItemDisplayName", "LSItemDisplayName"),
    // Dragged files arrive as this type; the guest compares pasteboard types to it.
    ("_kUTTypeFileURL", "public.file-url"),
    // SDL's clipboard offers text under this type and maps MIME types through
    // the tag class. Both are CoreServices' but compared only as strings.
    ("_kUTTypeUTF8PlainText", "public.utf8-plain-text"),
    ("_kUTTagClassMIMEType", "public.mime-type"),
    ("_kUTTagClassFilenameExtension", "public.filename-extension"),
    // CoreGraphics names, passed back in as colour space names and as an
    // options key that is free to be ignored.
    (
        "_kCGColorSpaceExtendedLinearSRGB",
        "kCGColorSpaceExtendedLinearSRGB",
    ),
    // SDL's GPU swapchain table reads it whenever the OS version says it exists.
    ("_kCGColorSpaceITUR_2100_PQ", "kCGColorSpaceITUR_2100_PQ"),
    (
        "_kCGDisplayShowDuplicateLowResolutionModes",
        "kCGDisplayShowDuplicateLowResolutionModes",
    ),
    // The old-scale key of a backing-properties change; posted by nothing yet.
    (
        "_NSBackingPropertyOldScaleFactorKey",
        "NSBackingPropertyOldScaleFactorKey",
    ),
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
    ("_NSLinkAttributeName", "NSLink"),
    ("_NSParagraphStyleAttributeName", "NSParagraphStyle"),
    ("_NSObliquenessAttributeName", "NSObliqueness"),
    ("_NSAppearanceNameAqua", "NSAppearanceNameAqua"),
    ("_NSAppearanceNameDarkAqua", appearance::DARK_AQUA),
    (
        "_NSAppearanceNameLightContent",
        "NSAppearanceNameLightContent",
    ),
    (
        "_NSAppearanceNameVibrantLight",
        "NSAppearanceNameVibrantLight",
    ),
    (
        "_NSAppearanceNameVibrantDark",
        "NSAppearanceNameVibrantDark",
    ),
    (
        "_NSAppearanceNameAccessibilityHighContrastAqua",
        "NSAppearanceNameAccessibilityHighContrastAqua",
    ),
    (
        "_NSAppearanceNameAccessibilityHighContrastDarkAqua",
        "NSAppearanceNameAccessibilityHighContrastDarkAqua",
    ),
    (
        "_NSAppearanceNameAccessibilityHighContrastVibrantLight",
        "NSAppearanceNameAccessibilityHighContrastVibrantLight",
    ),
    (
        "_NSAppearanceNameAccessibilityHighContrastVibrantDark",
        "NSAppearanceNameAccessibilityHighContrastVibrantDark",
    ),
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
    ("_NSBaselineOffsetAttributeName", "NSBaselineOffset"),
    // `NSViewAnimation`'s dictionary keys and effects, Apple's strings.
    ("_NSViewAnimationTargetKey", "NSViewAnimationTargetKey"),
    (
        "_NSViewAnimationStartFrameKey",
        "NSViewAnimationStartFrameKey",
    ),
    ("_NSViewAnimationEndFrameKey", "NSViewAnimationEndFrameKey"),
    ("_NSViewAnimationEffectKey", "NSViewAnimationEffectKey"),
    (
        "_NSViewAnimationFadeInEffect",
        "NSViewAnimationFadeInEffect",
    ),
    (
        "_NSViewAnimationFadeOutEffect",
        "NSViewAnimationFadeOutEffect",
    ),
    // Accessibility attribute names, whose values are Apple's strings. Nothing
    // here reads accessibility overrides, but a program sets them by name.
    ("_NSAccessibilityHelpAttribute", "AXHelp"),
    ("_NSAccessibilityTitleAttribute", "AXTitle"),
    ("_NSAccessibilityDescriptionAttribute", "AXDescription"),
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
    fn the_window_screen_notification_exports_its_name() {
        let symbols = data_symbols();
        let (_, cell) = symbols
            .iter()
            .find(|(name, _)| name == "_NSWindowDidChangeScreenNotification")
            .expect("screen notification is published");
        // SAFETY: the export is a live cell holding this runtime's string.
        let object = unsafe { *(*cell as *const objc::object::Id) };
        assert_eq!(
            unsafe { foundation::string::bytes(object) },
            window::DID_CHANGE_SCREEN.as_bytes()
        );
    }
}
