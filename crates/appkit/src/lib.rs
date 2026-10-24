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
mod accessibility;
pub mod alert;
mod animation;
pub mod appearance;
pub mod application;
mod aqua;
pub mod backing;
mod bezier;
mod bindings;
pub mod button;
mod cassowary;
mod catalog;
mod cell;
pub mod cgcontext;
pub mod cgevent;
pub mod cgfont;
pub mod cgl;
pub mod cgpaint;
pub mod cgpath;
pub mod classes;
pub mod color;
mod controls;
mod coreanimation;
pub mod coretext;
mod ctline;
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
mod props;
pub(crate) mod quartz;
mod table_views;
mod view_controller;
mod visual_effect;
mod visual_format;
pub use chrome::fonts;
pub mod geometry;
pub mod graphics;
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
mod pdf;
mod popup;
pub mod raster;
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
        decor::install();
        classes::install();
        accessibility::install();
        ctline::install();
        view_controller::install();
        bindings::install();
        table_views::install();
        visual_effect::install();
        alert::install();
        panel::install();
        appearance::install();
        application::install();
        nib::install();
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
        "CGMainDisplayID" => screen::CGMainDisplayID,
        "CGSessionCopyCurrentDictionary" => screen::CGSessionCopyCurrentDictionary,
        "CGWindowServerCreateServerPort" => screen::CGWindowServerCreateServerPort,
        "CGDisplayBounds" => screen::CGDisplayBounds,
        "CGGetDisplayTransferByTable" => screen::CGGetDisplayTransferByTable,
        "CGSetDisplayTransferByTable" => screen::CGSetDisplayTransferByTable,
        "CGDisplayRestoreColorSyncSettings" => screen::CGDisplayRestoreColorSyncSettings,
        "CGDisplayScreenSize" => screen::CGDisplayScreenSize,
        "CGDisplayIsMain" => screen::CGDisplayIsMain,
        "CGDisplayPixelsWide" => screen::CGDisplayPixelsWide,
        "CGDisplayPixelsHigh" => screen::CGDisplayPixelsHigh,
        "CGDisplayBitsPerPixel" => screen::CGDisplayBitsPerPixel,
        "CGDisplayMoveCursorToPoint" => screen::CGDisplayMoveCursorToPoint,
        "CGDisplayCapture" => screen::CGDisplayCapture,
        "CGCaptureAllDisplays" => screen::CGCaptureAllDisplays,
        "CGReleaseAllDisplays" => screen::CGReleaseAllDisplays,
        "CGDisplaySetDisplayMode" => screen::CGDisplaySetDisplayMode,
        "CGAcquireDisplayFadeReservation" => screen::CGAcquireDisplayFadeReservation,
        "CGReleaseDisplayFadeReservation" => screen::CGReleaseDisplayFadeReservation,
        "CGDisplayFade" => screen::CGDisplayFade,
        "CGShieldingWindowLevel" => screen::CGShieldingWindowLevel,
        "CGDisplayIsBuiltin" => screen::CGDisplayIsBuiltin,
        "CGDisplayPrimaryDisplay" => screen::CGDisplayPrimaryDisplay,
        "CGDisplayRelease" => screen::CGDisplayRelease,
        "CGDisplayRotation" => screen::CGDisplayRotation,
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
        "CGSetLocalEventsSuppressionInterval" => screen::CGSetLocalEventsSuppressionInterval,
        "CGAssociateMouseAndMouseCursorPosition" => screen::CGAssociateMouseAndMouseCursorPosition,
        "CGEventSourceKeyState" => screen::CGEventSourceKeyState,
        "CGEventCreate" => cgevent::CGEventCreate,
        "CGEventGetLocation" => cgevent::CGEventGetLocation,
        "CGEventCreateMouseEvent" => cgevent::CGEventCreateMouseEvent,
        "CGEventSetType" => cgevent::CGEventSetType,
        "CGEventGetType" => cgevent::CGEventGetType,
        "CGEventCreateKeyboardEvent" => cgevent::CGEventCreateKeyboardEvent,
        "CGEventGetFlags" => cgevent::CGEventGetFlags,
        "CGEventSetFlags" => cgevent::CGEventSetFlags,
        "CGEventGetIntegerValueField" => cgevent::CGEventGetIntegerValueField,
        "CGEventSetIntegerValueField" => cgevent::CGEventSetIntegerValueField,
        "CGEventPost" => cgevent::CGEventPost,
        "CGGetLastMouseDelta" => cgevent::CGGetLastMouseDelta,
        "CGEventSourceFlagsState" => cgevent::CGEventSourceFlagsState,
        "CGEventSourceCounterForEventType" => cgevent::CGEventSourceCounterForEventType,
        "CGEventTapCreate" => cgevent::CGEventTapCreate,
        "CGDisplayModeCopyPixelEncoding" => screen::CGDisplayModeCopyPixelEncoding,
        "CGColorSpaceCreateWithName" => graphics::CGColorSpaceCreateWithName,
        "CGColorSpaceCreateDeviceRGB" => graphics::CGColorSpaceCreateDeviceRGB,
        "CGColorSpaceCreateDeviceGray" => graphics::CGColorSpaceCreateDeviceGray,
        "CGColorSpaceCreateWithPlatformColorSpace" => graphics::CGColorSpaceCreateWithPlatformColorSpace,
        "CGColorSpaceCreatePattern" => graphics::CGColorSpaceCreatePattern,
        "CGColorSpaceGetNumberOfComponents" => graphics::CGColorSpaceGetNumberOfComponents,
        "CGColorSpaceRelease" => graphics::CGColorSpaceRelease,
        "CGColorSpaceGetName" => graphics::CGColorSpaceGetName,
        "CGColorSpaceCopyName" => graphics::CGColorSpaceCopyName,
        "CGColorSpaceCopyPropertyList" => graphics::CGColorSpaceCopyPropertyList,
        "CGDataProviderCreateWithData" => graphics::CGDataProviderCreateWithData,
        "CGDataProviderRelease" => graphics::CGDataProviderRelease,
        "CGImageCreate" => graphics::CGImageCreate,
        "CGImageMaskCreate" => graphics::CGImageMaskCreate,
        "CGImageCreateWithImageInRect" => graphics::CGImageCreateWithImageInRect,
        "CGImageCreateWithPNGDataProvider" => graphics::CGImageCreateWithPNGDataProvider,
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
        "NSIsEmptyRect" => geometry::NSIsEmptyRect,
        "NSUnionRect" => geometry::NSUnionRect,
        "NSMouseInRect" => geometry::NSMouseInRect,
        "NSInsetRect" => geometry::NSInsetRect,
        "NSIntersectionRect" => geometry::NSIntersectionRect,
        "NSOffsetRect" => geometry::NSOffsetRect,
        "CGRectInset" => geometry::CGRectInset,
        "CGRectOffset" => geometry::CGRectOffset,
        "CGRectIntegral" => geometry::CGRectIntegral,
        "CGRectIsEmpty" => geometry::CGRectIsEmpty,
        "CGRectIntersection" => geometry::CGRectIntersection,
        "CGRectContainsPoint" => geometry::CGRectContainsPoint,
        "CGRectContainsRect" => geometry::CGRectContainsRect,
        "NSContainsRect" => geometry::NSContainsRect,
        "NSIntersectsRect" => geometry::NSIntersectsRect,
        "NSIntegralRect" => geometry::NSIntegralRect,
        "NSEqualPoints" => geometry::NSEqualPoints,
        "NSEqualSizes" => geometry::NSEqualSizes,
        "NSIntersectionRange" => geometry::NSIntersectionRange,
        "NSStringFromPoint" => geometry::NSStringFromPoint,
        "NSStringFromSize" => geometry::NSStringFromSize,
        "NSStringFromRect" => geometry::NSStringFromRect,
        "NSStringFromRange" => geometry::NSStringFromRange,
        "NSPointFromString" => geometry::NSPointFromString,
        "NSSizeFromString" => geometry::NSSizeFromString,
        "NSRectFromString" => geometry::NSRectFromString,
        "NSFrameRect" => color::NSFrameRect,
        "NSFrameRectWithWidth" => color::NSFrameRectWithWidth,
        "NSEraseRect" => color::NSEraseRect,
        "NSRectClip" => color::NSRectClip,
        "NSRectFillUsingOperation" => color::NSRectFillUsingOperation,
        "NSBeep" => application::NSBeep,
        "NSAccessibilityPostNotification" => accessibility::NSAccessibilityPostNotification,
        "NSAccessibilityPostNotificationWithUserInfo" => accessibility::NSAccessibilityPostNotificationWithUserInfo,
        "NSAccessibilityActionDescription" => accessibility::NSAccessibilityActionDescription,
        "NSAccessibilityRoleDescription" => accessibility::NSAccessibilityRoleDescription,
        "NSAccessibilityRoleDescriptionForUIElement" => accessibility::NSAccessibilityRoleDescriptionForUIElement,
        "NSAccessibilityUnignoredAncestor" => accessibility::NSAccessibilityUnignoredAncestor,
        "NSAccessibilityFrameInView" => accessibility::NSAccessibilityFrameInView,
        // FIXME (PORTABILITY): this needs to be ported to macOS
        #[cfg(not(target_os = "macos"))]
        "NSDefaultMallocZone" => darwin::malloc::malloc_default_zone,
        "NSRectFill" => color::NSRectFill,
        "NSDrawNinePartImage" => image::NSDrawNinePartImage,
        "NSDisableScreenUpdates" => geometry::NSDisableScreenUpdates,
        "NSEnableScreenUpdates" => geometry::NSEnableScreenUpdates,
        "CGRectEqualToRect" | "NSEqualRects" => geometry::CGRectEqualToRect,
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
        ctline,
        pasteboard_manager,
        coretext,
        cgfont,
        hitheme,
        launchservices,
        cgcontext,
        cgpath,
        cgpaint,
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
    let gravities: Vec<(&str, &str)> = coreanimation::GRAVITIES
        .iter()
        .map(|(symbol, name, _)| (*symbol, *name))
        .collect();
    out.extend(corefoundation::base::string_globals(&gravities));
    // `const CGFloat`, read as a double.
    out.push((
        "_NSViewNoIntrinsicMetric".to_string(),
        corefoundation::base::global_cell("_NSViewNoIntrinsicMetric", (-1.0f64).to_bits()),
    ));
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
    "NSApplicationDidResignActiveNotification",
    "NSWorkspaceDidMountNotification",
    "NSWorkspaceDidUnmountNotification",
    "NSWorkspaceDidRenameVolumeNotification",
    "NSOutlineViewItemDidCollapseNotification",
    "NSOutlineViewItemDidExpandNotification",
    "NSPopUpButtonWillPopUpNotification",
    // Keys and option names whose text is their own name.
    "NSApplicationLaunchUserNotificationKey",
    "NSAccessibilityException",
    "NSPrintHeaderAndFooter",
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
    ("_kUTTypeURL", "public.url"),
    ("_kUTTypePlainText", "public.plain-text"),
    ("_kUTTypeText", "public.text"),
    ("_kUTTypeData", "public.data"),
    ("_kUTTypeItem", "public.item"),
    ("_kUTTypeDirectory", "public.directory"),
    ("_kUTTypeFolder", "public.folder"),
    ("_kUTTypeApplicationBundle", "com.apple.application-bundle"),
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
    ("_NSAccessibilityRoleAttribute", "AXRole"),
    (
        "_NSAccessibilityRoleDescriptionAttribute",
        "AXRoleDescription",
    ),
    ("_NSAccessibilitySubroleAttribute", "AXSubrole"),
    ("_NSAccessibilityChildrenAttribute", "AXChildren"),
    ("_NSAccessibilityParentAttribute", "AXParent"),
    ("_NSAccessibilityWindowAttribute", "AXWindow"),
    (
        "_NSAccessibilityTopLevelUIElementAttribute",
        "AXTopLevelUIElement",
    ),
    ("_NSAccessibilityPositionAttribute", "AXPosition"),
    ("_NSAccessibilitySizeAttribute", "AXSize"),
    ("_NSAccessibilityValueAttribute", "AXValue"),
    ("_NSAccessibilityURLAttribute", "AXURL"),
    ("_NSAccessibilitySelectedTextAttribute", "AXSelectedText"),
    (
        "_NSAccessibilitySelectedTextRangeAttribute",
        "AXSelectedTextRange",
    ),
    (
        "_NSAccessibilityNumberOfCharactersAttribute",
        "AXNumberOfCharacters",
    ),
    (
        "_NSAccessibilityVisibleCharacterRangeAttribute",
        "AXVisibleCharacterRange",
    ),
    (
        "_NSAccessibilityLineForIndexParameterizedAttribute",
        "AXLineForIndex",
    ),
    (
        "_NSAccessibilityRangeForLineParameterizedAttribute",
        "AXRangeForLine",
    ),
    (
        "_NSAccessibilityStringForRangeParameterizedAttribute",
        "AXStringForRange",
    ),
    (
        "_NSAccessibilityRangeForPositionParameterizedAttribute",
        "AXRangeForPosition",
    ),
    (
        "_NSAccessibilityRangeForIndexParameterizedAttribute",
        "AXRangeForIndex",
    ),
    (
        "_NSAccessibilityBoundsForRangeParameterizedAttribute",
        "AXBoundsForRange",
    ),
    (
        "_NSAccessibilityRTFForRangeParameterizedAttribute",
        "AXRTFForRange",
    ),
    (
        "_NSAccessibilityStyleRangeForIndexParameterizedAttribute",
        "AXStyleRangeForIndex",
    ),
    (
        "_NSAccessibilityAttributedStringForRangeParameterizedAttribute",
        "AXAttributedStringForRange",
    ),
    ("_NSAccessibilityFontTextAttribute", "AXFont"),
    (
        "_NSAccessibilityForegroundColorTextAttribute",
        "AXForegroundColor",
    ),
    (
        "_NSAccessibilityBackgroundColorTextAttribute",
        "AXBackgroundColor",
    ),
    ("_NSAccessibilityLinkTextAttribute", "AXLink"),
    ("_NSAccessibilityMisspelledTextAttribute", "AXMisspelled"),
    (
        "_NSAccessibilityStrikethroughTextAttribute",
        "AXStrikethrough",
    ),
    ("_NSAccessibilityUnderlineTextAttribute", "AXUnderline"),
    ("_NSAccessibilityFontFamilyKey", "AXFontFamily"),
    ("_NSAccessibilityFontNameKey", "AXFontName"),
    ("_NSAccessibilityFontSizeKey", "AXFontSize"),
    ("_NSAccessibilityVisibleNameKey", "AXVisibleName"),
    ("_NSAccessibilityGroupRole", "AXGroup"),
    ("_NSAccessibilityLinkRole", "AXLink"),
    ("_NSAccessibilityStaticTextRole", "AXStaticText"),
    ("_NSAccessibilityRadioButtonRole", "AXRadioButton"),
    ("_NSAccessibilityRadioGroupRole", "AXRadioGroup"),
    ("_NSAccessibilityTabGroupRole", "AXTabGroup"),
    ("_NSAccessibilityTextAreaRole", "AXTextArea"),
    ("_NSAccessibilityTextFieldRole", "AXTextField"),
    ("_NSAccessibilityTextLinkSubrole", "AXTextLink"),
    ("_NSAccessibilityPressAction", "AXPress"),
    (
        "_NSAccessibilitySelectedRowsChangedNotification",
        "AXSelectedRowsChanged",
    ),
    (
        "_NSAccessibilitySelectedTextChangedNotification",
        "AXSelectedTextChanged",
    ),
    ("_NSAccessibilityValueChangedNotification", "AXValueChanged"),
    (
        "_NSAccessibilityAnnouncementRequestedNotification",
        "AXAnnouncementRequested",
    ),
    ("_NSAccessibilityAnnouncementKey", "AXAnnouncementKey"),
    // The only named system image a program asks for here.
    ("_NSImageNameApplicationIcon", "NSApplicationIcon"),
    ("_NSImageNameActionTemplate", "NSActionTemplate"),
    ("_NSImageNameAddTemplate", "NSAddTemplate"),
    ("_NSImageNameRemoveTemplate", "NSRemoveTemplate"),
    ("_NSImageNameRefreshTemplate", "NSRefreshTemplate"),
    ("_NSImageNameGoLeftTemplate", "NSGoLeftTemplate"),
    ("_NSImageNameGoRightTemplate", "NSGoRightTemplate"),
    (
        "_NSImageNameStopProgressFreestandingTemplate",
        "NSStopProgressFreestandingTemplate",
    ),
    ("_NSImageNameComputer", "NSComputer"),
    ("_NSImageNameMultipleDocuments", "NSMultipleDocuments"),
    ("_NSImageNameStatusAvailable", "NSStatusAvailable"),
    ("_NSImageNameStatusUnavailable", "NSStatusUnavailable"),
    ("_NSBackgroundColorAttributeName", "NSBackgroundColor"),
    ("_NSShadowAttributeName", "NSShadow"),
    ("_NSStrikethroughStyleAttributeName", "NSStrikethrough"),
    ("_NSExpansionAttributeName", "NSExpansion"),
    ("_NSGlyphInfoAttributeName", "NSGlyphInfo"),
    (
        "_NSFontFeatureSettingsAttribute",
        "NSCTFontFeatureSettingsAttribute",
    ),
    ("_NSFontFeatureTypeIdentifierKey", "CTFeatureTypeIdentifier"),
    (
        "_NSFontFeatureSelectorIdentifierKey",
        "CTFeatureSelectorIdentifier",
    ),
    ("_NSGeneralPboard", "Apple CFPasteboard general"),
    ("_NSFindPboard", "Apple CFPasteboard find"),
    (
        "_NSToolbarFlexibleSpaceItemIdentifier",
        "NSToolbarFlexibleSpaceItem",
    ),
    // Cocoa bindings' names and options.
    ("_NSValueBinding", "value"),
    ("_NSContentBinding", "content"),
    ("_NSContentValuesBinding", "contentValues"),
    ("_NSSelectedValueBinding", "selectedValue"),
    ("_NSSelectedIndexBinding", "selectedIndex"),
    ("_NSSelectedTagBinding", "selectedTag"),
    ("_NSSelectionIndexesBinding", "selectionIndexes"),
    ("_NSTitleBinding", "title"),
    ("_NSToolTipBinding", "toolTip"),
    ("_NSHiddenBinding", "hidden"),
    ("_NSImageBinding", "image"),
    ("_NSEnabledBinding", "enabled"),
    ("_NSEditableBinding", "editable"),
    ("_NSAnimateBinding", "animate"),
    ("_NSDocumentEditedBinding", "documentEdited"),
    (
        "_NSValueTransformerNameBindingOption",
        "NSValueTransformerName",
    ),
    ("_NSValueTransformerBindingOption", "NSValueTransformer"),
    ("_NSNullPlaceholderBindingOption", "NSNullPlaceholder"),
    (
        "_NSContinuouslyUpdatesValueBindingOption",
        "NSContinuouslyUpdatesValue",
    ),
    ("_NSObservedObjectKey", "NSObservedObject"),
    ("_NSObservedKeyPathKey", "NSObservedKeyPath"),
    ("_NSOptionsKey", "NSOptions"),
];

/// `NSFontWeight` globals, which are **`CGFloat` variables** and not pointers.
///
/// The difference matters: a guest loads the symbol's address and reads a
/// *double* out of it, so publishing one as a string object hands it the bit
/// pattern of a pointer as a font weight. The values are Apple's own, on the
/// -1.0 to 1.0 scale `+systemFontOfSize:weight:` takes.
const FONT_WEIGHTS: &[(&str, f64)] = &[
    // Not weights, but the same kind of `CGFloat` variable: the gray levels.
    ("_NSBlack", 0.0),
    ("_NSDarkGray", 1.0 / 3.0),
    ("_NSLightGray", 2.0 / 3.0),
    ("_NSWhite", 1.0),
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
