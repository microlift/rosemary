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
pub mod accessibility;
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
pub mod killring;
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
pub mod quartzcore;
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
        quartzcore::install();
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
        // The `NSGeometry` half of the same header. Foundation's spellings of
        // questions about a rectangle, and this tree has answered four of them
        // here since the start; the guest imports these six from Foundation and
        // the provider is a flat name-to-address map, so which framework the
        // symbol is attributed to makes no difference to what it binds to.
        "NSPointInRect" => geometry::NSPointInRect,
        "NSMouseInRect" => geometry::NSMouseInRect,
        "NSInsetRect" => geometry::NSInsetRect,
        "NSContainsRect" => geometry::NSContainsRect,
        "NSIntersectsRect" => geometry::NSIntersectsRect,
        "NSEqualSizes" => geometry::NSEqualSizes,
        "NSOffsetRect" => geometry::NSOffsetRect,
        "NSStringFromPoint" => geometry::NSStringFromPoint,
        "NSDisableScreenUpdates" => geometry::NSDisableScreenUpdates,
        "NSEnableScreenUpdates" => geometry::NSEnableScreenUpdates,
        // `CGRectEqualToRect` is deliberately *absent* here.
        //
        // It was on this row until the duplicate census found it here and in
        // `darwin::cg_unavailable`, and the two were not the same function:
        //
        //   darwin   one.x == two.x && one.y == two.y
        //            && one.width == two.width && one.height == two.height
        //   appkit   u8::from(standardized(a) == standardized(b))
        //
        // CoreGraphics is the raw one. Apple's own documentation for
        // `CGRectEqualToRect` says it returns "true if the two specified
        // rectangles have equal size and origin values", and deprecates the
        // function in favour of `==` on `CGRect` - and `origin` and `size` are
        // the stored fields, which `standardized` has just rewritten. So
        // `{0,0,10,10}` and `{10,10,-10,-10}` are *not* equal, and the
        // standardised answer was a wrong one that looked like a right one.
        //
        // `darwin` is also the provider that has to keep it: the loader's chain
        // puts `darwin` ahead of `frameworks` (`program::HostProviders::chain`),
        // so AppKit's row was reachable only by `dlsym` - which is how the two
        // answers disagreed in the first place.
        //
        // `NSEqualRects` stays, because nothing else in the tree publishes it
        // and Foundation's `NSGeometry.h` defines it with the same field
        // comparison. `rosetta3::shims::coregraphics::rect_equal` also claims
        // both spellings and standardises; that file is not mine and the shim
        // path wins the call, so it is reported rather than edited.
        "NSEqualRects" => geometry::CGRectEqualToRect,
        // The rectangle fill, which is a function *about* the current graphics
        // context and so belongs with the module that owns that.
        "NSRectFill" => backing::NSRectFill,
        // The system alert, with the application it is an alert of.
        "NSBeep" => application::NSBeep,
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
        accessibility,
        killring,
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
    out.extend(appearance::name_cells());
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
    // `_NSAppearanceNameAqua` is deliberately absent: `appearance::name_cells`
    // publishes it, together with `_NSAppearanceNameDarkAqua`, as the cell
    // `+[NSAppearance appearanceNamed:]` compares its argument against. A second
    // row here would define the same symbol twice, and `global_cell` interns by
    // name - so whichever ran first would decide what the cell holds, and this
    // one holds a different `NSString` object from the same text, which is
    // exactly the identity the appearance lookup depends on.
    ("_NSDeviceSize", "NSDeviceSize"),
    ("_NSModalPanelRunLoopMode", "NSModalPanelRunLoopMode"),
    // The mode AppKit's event-tracking loop turns in, which the guest reads twice
    // and *dereferences* both times.
    //
    // `Framework+0x815e991`: `mov rax,[0xdd518f8]; mov r14,[rax]` then
    // `cmp r13,r14; je` - an **identity** comparison, so this is literally
    // `[[NSRunLoop currentRunLoop] currentMode] == NSEventTrackingRunLoopMode`.
    // `Framework+0xbc873d7`: `mov rax,[0xdd518f8]; mov r8,[rax]`, passed as
    // `inMode:` to `@selector(nextEventMatchingMask:untilDate:inMode:dequeue:)`.
    //
    // With the symbol unbound, `ROSETTA3_FILL_UNBOUND_DATA` points the slot at a
    // read-only page of zeros, so **both** dereferences read null. "Am I in a
    // tracking loop?" is then permanently false, and the tracking loop is handed
    // the *null* mode to run in. Nothing crashes; the answer is simply always
    // wrong. `application.rs:304` already builds this very string when making the
    // mode common, so the two halves are meant to agree and now do.
    ("_NSEventTrackingRunLoopMode", "NSEventTrackingRunLoopMode"),
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

    /// Every Foundation and AppKit function import this crate claims, answered -
    /// and each row is the guest's own spelling with **exactly one** leading
    /// underscore removed.
    ///
    /// This is the check a clean build and a green suite both miss. `exports!`
    /// publishes `"_" + <row>` and `machish::resolve::SyntheticDylib::resolve`
    /// looks the import up by that string verbatim, so a row with the wrong
    /// number of leading underscores leaves the guest's slot at zero - and a call
    /// through it is a jump to address 0, not a compile error and not a test
    /// failure. Nothing in `cargo test` can see it, because nothing in `cargo
    /// test` reads the guest's indirect symbol table.
    ///
    /// The two spellings, both from that table via `test/tools/callers.py`:
    ///
    /// - fifteen of the twenty are `NS...` with **one** underscore, so the row has
    ///   none - which is the ordinary Mach-O convention and easy to assume.
    /// - the five kill-ring names are `__NS...` with **two**, because AppKit's
    ///   private prefix is a leading underscore of the C identifier's own. The row
    ///   keeps that one and drops only the Mach-O one. Two versions of
    ///   `killring`'s table got this wrong - one dropped both and one kept both -
    ///   and each compiled, passed every test, and left the import unbound.
    ///
    /// `NSLogv` is the one name of the twenty deliberately absent: it reads the
    /// guest's `va_list` and an `NSString` format, which is a shim in
    /// `crates/rosetta3` rather than an `extern "C"` this crate can write. It is
    /// reported, not bound.
    #[test]
    fn every_claimed_foundation_and_appkit_function_is_answered_as_the_guest_spells_it() {
        bootstrap();
        for (row, guest) in [
            // Foundation's `NSGeometry`, which the guest imports from Foundation
            // and this crate answers from `geometry` beside the four it always
            // has.
            ("NSContainsRect", "_NSContainsRect"),
            ("NSIntersectsRect", "_NSIntersectsRect"),
            ("NSEqualSizes", "_NSEqualSizes"),
            ("NSOffsetRect", "_NSOffsetRect"),
            ("NSStringFromPoint", "_NSStringFromPoint"),
            // AppKit's drawing, its alert, and its drag.
            ("NSRectFill", "_NSRectFill"),
            ("NSBeep", "_NSBeep"),
            ("NSPerformService", "_NSPerformService"),
            // AppKit's client-side accessibility.
            (
                "NSAccessibilityPostNotification",
                "_NSAccessibilityPostNotification",
            ),
            (
                "NSAccessibilityPostNotificationWithUserInfo",
                "_NSAccessibilityPostNotificationWithUserInfo",
            ),
            (
                "NSAccessibilityUnignoredAncestor",
                "_NSAccessibilityUnignoredAncestor",
            ),
            (
                "NSAccessibilityUnignoredChildren",
                "_NSAccessibilityUnignoredChildren",
            ),
            (
                "NSAccessibilityRoleDescription",
                "_NSAccessibilityRoleDescription",
            ),
            (
                "NSAccessibilityActionDescription",
                "_NSAccessibilityActionDescription",
            ),
            // AppKit's kill ring, whose symbol carries the private `__NS` prefix
            // on top of the Mach-O one.
            ("_NSInitializeKillRing", "__NSInitializeKillRing"),
            ("_NSNewKillRingSequence", "__NSNewKillRingSequence"),
            ("_NSAppendToKillRing", "__NSAppendToKillRing"),
            ("_NSYankFromKillRing", "__NSYankFromKillRing"),
            ("_NSSetKillRingToYankedState", "__NSSetKillRingToYankedState"),
        ] {
            assert_eq!(
                format!("_{row}"),
                guest,
                "`exports!` puts one underscore in front of the row and the loader looks \
                 the import up by that string verbatim"
            );
            let address = symbol(row).unwrap_or_else(|| {
                panic!("{row} is not answered, so the guest's {guest} is unbound")
            });
            assert_ne!(
                address, 0,
                "{row} answered address zero, which is a jump to address 0"
            );
        }
    }

    // ------------------------------------------------------------ the boundary
    //
    // Nothing below this line existed until `_CGRectEqualToRect` was found
    // published here *and* in `darwin::cg_unavailable`, with different bodies.
    //
    // AppKit and `darwin` are two providers in one loader chain
    // (`rosetta3::frameworks` and `darwin`, `program::HostProviders::chain`),
    // and they disagree about two things at once:
    //
    //   * which one wins. `darwin` is ahead of `frameworks` in the chain, so a
    //     guest's *import* binds to `darwin`'s answer. But a guest's `dlsym`
    //     goes `darwin::symbol` -> `registered()` -> `frameworks::symbol`, and
    //     that consults `frameworks` **first**. One name, two winners, chosen
    //     by which mechanism asked.
    //   * `darwin::provider()` and `darwin::symbol()` do not even agree with
    //     each other inside one crate: `provide_all` writes into a `BTreeMap`
    //     (`SyntheticDylib::define` -> `insert`), so the **last** module in
    //     `MODULES` wins the published address, while
    //     `exports::Module::resolve` is `find_map`, so the **first** wins the
    //     `dlsym`.
    //
    // So "one provider per name" is not a style rule here, it is the only thing
    // that makes a name's answer well defined, and the two checks below are
    // the two ways it can fail.

    /// Everything this crate publishes, as a recording rather than a map.
    fn published() -> exports::Recording {
        bootstrap();
        let mut out = exports::Recording::default();
        provide(&mut out);
        out
    }

    /// No name this crate publishes is also published by `darwin`.
    ///
    /// The cross-crate half of `no_module_defines_a_name_another_one_does`,
    /// which is `#[cfg(test)]` inside `darwin` and therefore cannot see a
    /// framework. It is the check that would have caught `_CGRectEqualToRect`
    /// and `_CGCursorIsVisible`; neither is caught anywhere else, because
    /// `rosetta3::frameworks::tests::no_framework_defines_a_symbol_another_one_
    /// does` only walks frameworks and so cannot see `darwin` either.
    ///
    /// `darwin::symbol` rather than `darwin::provider`: the former is the
    /// `dlsym` path, it is public, and it is the one that reaches across into
    /// this crate - `rosetta3::frameworks::install` registers it as a provider
    /// of `darwin`. A name both crates answer is therefore a name a guest can
    /// reach at two different addresses.
    #[test]
    fn no_darwin_symbol_is_also_answered_here() {
        let out = published();
        let clash: Vec<&str> = out
            .symbols()
            .into_iter()
            .filter(|mach_o| darwin::symbol(mach_o).is_some())
            .collect();
        assert!(
            clash.is_empty(),
            "answered by both `darwin` and `appkit`, and the two do not have to \
             agree: {clash:?}. `darwin` is ahead of `frameworks` in \
             `HostProviders::chain`, so the import binds to `darwin` while \
             `darwin::symbol` - the `dlsym` path - answers from `frameworks`"
        );
    }

    /// Every name this crate publishes is answered by **this** crate's own
    /// `symbol`, at the address it is published at.
    ///
    /// The intra-crate half, and the one that has teeth: `symbol` is generated
    /// by the same `exports!` as `provide`, so a disagreement is impossible
    /// today - but `rosetta3::frameworks::symbol` is a `find_map` over the
    /// framework list, and a name published twice inside one `exports!` block
    /// (two rows, or a row and one of its aliases) would be a name whose
    /// published address and `dlsym` address differ, with no other test in the
    /// tree able to see it.
    #[test]
    fn a_published_name_is_answered_by_this_crates_own_symbol() {
        let out = published();
        for (mach_o, address) in out.functions.iter().chain(&out.data) {
            let bare = mach_o.strip_prefix('_').expect("the Mach-O underscore");
            assert_eq!(
                symbol(bare),
                Some(*address),
                "{mach_o} is published at {address:#x} and answered at a \
                 different address by `symbol`"
            );
        }
    }

    /// A name this crate publishes is not published twice by this crate.
    ///
    /// `exports!` will not stop a second row, and the loser is chosen by list
    /// order rather than by anything a reader can see.
    #[test]
    fn no_name_is_published_twice_by_this_crate() {
        let out = published();
        let mut seen: Vec<&str> = Vec::new();
        let mut twice: Vec<&str> = Vec::new();
        for name in out.symbols() {
            if seen.contains(&name) {
                twice.push(name);
            } else {
                seen.push(name);
            }
        }
        assert!(twice.is_empty(), "published more than once: {twice:?}");
    }

    /// `CGRectEqualToRect` and `NSEqualRects` compare the stored fields.
    ///
    /// The assertion the duplicate was hiding. `standardized` was applied here
    /// and CoreGraphics does not apply it: with it, `{0,0,10,10}` equalled
    /// `{10,10,-10,-10}`, and the guest's own two call sites
    /// (`Framework+0x3ad40c6`, `+0x4593129`) branch on the answer, so this is
    /// a wrong answer that changes control flow.
    ///
    /// `-0.0 == 0.0` is the other half and is *right*: Apple's own wording is
    /// "equal size and origin values", and `==` on `CGFloat` is what makes the
    /// two compare equal, which is also why `CGRectIsNull` accepts an all-`-0.0`
    /// rectangle. `NaN` compares unequal to itself, as it must.
    #[test]
    fn rectangle_equality_is_field_equality_not_standardized_equality() {
        use foundation::geometry::{NSPoint, NSRect, NSSize};
        let rect = |x: f64, y: f64, w: f64, h: f64| NSRect {
            origin: NSPoint { x, y },
            size: NSSize {
                width: w,
                height: h,
            },
        };
        // SAFETY: two `NSRect` values, built here, by value.
        let same = |a: NSRect, b: NSRect| unsafe { geometry::CGRectEqualToRect(a, b) };

        // A rectangle is equal to itself, however it is written.
        assert_eq!(1, same(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 10.0, 10.0)));
        // And to one that differs in a single field, in either direction.
        assert_eq!(0, same(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 10.0, 11.0)));
        assert_eq!(0, same(rect(0.0, 0.0, 10.0, 10.0), rect(1.0, 0.0, 10.0, 10.0)));
        // A negative extent is a *different* rectangle, not the same one
        // written the other way round. This is the line the old
        // `standardized(a) == standardized(b)` got wrong.
        assert_eq!(0, same(rect(0.0, 0.0, 10.0, 10.0), rect(10.0, 10.0, -10.0, -10.0)));
        // `-0.0` is `0.0` to a `CGFloat` comparison.
        assert_eq!(1, same(rect(0.0, 0.0, 10.0, 10.0), rect(-0.0, -0.0, 10.0, 10.0)));
        // `NaN` is not equal to itself, and that is CoreGraphics too.
        assert_eq!(0, same(rect(f64::NAN, 0.0, 10.0, 10.0), rect(f64::NAN, 0.0, 10.0, 10.0)));
        // The null rectangle equals itself.
        assert_eq!(1, same(rect(0.0, 0.0, 0.0, 0.0), rect(0.0, 0.0, 0.0, 0.0)));
    }
}
