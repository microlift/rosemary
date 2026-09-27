use objc::class::Class;

static HIERARCHY: &[(&str, Option<&str>, usize)] = &[
    ("NSResponder", None, 0),
    (
        "NSApplication",
        Some("NSResponder"),
        crate::application::INSTANCE_SIZE,
    ),
    (
        "NSWindow",
        Some("NSResponder"),
        crate::window::INSTANCE_SIZE,
    ),
    ("NSPanel", Some("NSWindow"), 0),
    ("NSSavePanel", Some("NSPanel"), 0),
    ("NSOpenPanel", Some("NSSavePanel"), 0),
    ("NSWindowController", Some("NSResponder"), 0),
    ("NSViewController", Some("NSResponder"), 0),
    ("NSView", Some("NSResponder"), crate::view::INSTANCE_SIZE),
    // Core Animation, and here for `-contentsScale`; see `crate::layer`.
    ("CALayer", None, crate::layer::INSTANCE_SIZE),
    // QuartzCore's Metal presentation layer. Its inherited `+layer` is enough
    // to let a view establish that its backing layer is Metal-compatible; the
    // Metal runtime adds the drawable-producing surface behind it.
    (
        "CAMetalLayer",
        Some("CALayer"),
        crate::layer::METAL_INSTANCE_SIZE,
    ),
    // QuartzCore's transaction scope. The guest opens and commits these around
    // single property changes - ten `+begin` and nineteen `+commit` - so
    // without the class the first one aborts by name; see `crate::quartzcore`.
    // The runtime's own unbound-import report names it and where it comes from:
    // "_OBJC_CLASS_$_CATransaction from .../QuartzCore.framework".
    ("CATransaction", None, 0),
    ("NSControl", Some("NSView"), 0),
    ("NSButton", Some("NSControl"), 0),
    ("NSTextField", Some("NSControl"), 0),
    ("NSImageView", Some("NSControl"), 0),
    ("NSText", Some("NSView"), 0),
    ("NSTextView", Some("NSText"), 0),
    ("NSScrollView", Some("NSView"), 0),
    ("NSScroller", Some("NSControl"), 0),
    ("NSClipView", Some("NSView"), 0),
    ("NSBox", Some("NSView"), 0),
    ("NSStackView", Some("NSView"), 0),
    ("NSProgressIndicator", Some("NSView"), 0),
    ("NSSearchField", Some("NSTextField"), 0),
    ("NSLayoutConstraint", None, crate::layout::INSTANCE_SIZE),
    ("NSAnimationContext", None, 0),
    ("NSAffineTransform", None, 0),
    ("NSParagraphStyle", None, 0),
    ("NSMutableParagraphStyle", Some("NSParagraphStyle"), 0),
    (
        "NSOpenGLView",
        Some("NSView"),
        crate::opengl::VIEW_INSTANCE_SIZE,
    ),
    ("NSMenu", None, crate::menu::MENU_INSTANCE_SIZE),
    ("NSMenuItem", None, crate::menu::ITEM_INSTANCE_SIZE),
    ("NSScreen", None, crate::screen::INSTANCE_SIZE),
    ("NSEvent", None, crate::nsevent::INSTANCE_SIZE),
    ("NSCursor", None, crate::cursor::CURSOR_INSTANCE_SIZE),
    ("NSColor", None, crate::color::INSTANCE_SIZE),
    ("NSAppearance", None, size_of::<usize>()),
    ("NSImage", None, crate::image::INSTANCE_SIZE),
    ("NSGraphicsContext", None, 0),
    ("NSColorSpace", None, 0),
    ("NSImageRep", None, 0),
    (
        "NSBitmapImageRep",
        Some("NSImageRep"),
        crate::image::bitmap::INSTANCE_SIZE,
    ),
    ("NSFont", None, 0),
    ("NSFontManager", None, 0),
    (
        "NSOpenGLContext",
        None,
        crate::opengl::CONTEXT_INSTANCE_SIZE,
    ),
    (
        "NSOpenGLPixelFormat",
        None,
        crate::opengl::PIXEL_FORMAT_INSTANCE_SIZE,
    ),
    ("NSPasteboard", None, 0),
    ("NSWorkspace", None, 0),
    ("NSRunningApplication", None, 0),
    ("NSAlert", None, crate::alert::INSTANCE_SIZE),
    ("NSNib", None, 0),
    ("NSTextInputContext", None, 0),
    ("NSDockTile", None, crate::dock::INSTANCE_SIZE),
    (
        "NSTrackingArea",
        None,
        crate::cursor::TRACKING_AREA_INSTANCE_SIZE,
    ),
    // Apple's header puts this one in Foundation rather than AppKit, so
    // `crates/foundation/src/classes.rs` is where it belongs; it is here only
    // because that file is not this one's to edit. `register` returns an
    // existing class rather than failing, so declaring it twice is harmless.
    // The unbound-import report agrees about the framework:
    // "_OBJC_CLASS_$_NSAssertionHandler from .../Foundation.framework".
    ("NSAssertionHandler", None, 0),
    // Declared for the subclasses the guest cannot register without, which is
    // the whole of the difference between having a class and not having one: a
    // missing superclass refuses the *subclass*, so every method it defines
    // and every message sent to it goes with it, where a declared one
    // registers and only the inherited messages actually sent are refused.
    //
    // The evidence is the Framework's own bind stream, not a header. The guest
    // binds `_OBJC_CLASS_$_X` into `__objc_data` at `class + 8`, which is
    // `objc_class::superclass` - `isa` is at +0 - so the class at `slot - 8`
    // is the one that cannot be realised. Reading the stream that way names
    // exactly one subclass per row, and there are no others: of the 171 classes
    // in `__objc_classlist`, eight have a superclass in a system framework,
    // and five of those superclasses were unbound.
    //
    //   NSAnimation                     <- ConstrainedWindowAnimationBase
    //                                   <- FadeAllWindowsAnimation
    //   NSAccessibilityElement          <- AXPlatformNodeCocoa
    //   NSTableView                     <- TaskManagerMacTableView
    //   NSTitlebarAccessoryViewController <- ImmersiveModeTitlebarViewController
    //   NSATSTypesetter                 <- GTMBreakRecordingTypeSetter
    //
    // The eighth is `ASAuthorizationController`, which has no crate here.
    //
    // No methods of their own: what these subclasses send is not knowable from
    // the image, and an invented one is worse than a refusal that names it. The
    // superclasses below are AppKit's documented ones, so a subclass also
    // inherits whatever `NSView`/`NSViewController` already answers here.
    //
    // `NSATSTypesetter` is declared straight off `NSObject` rather than off
    // `NSTypesetter`, which is what Apple's header says it derives from:
    // `NSTypesetter` has no crate and no call site in the guest, so naming it
    // would add a class nothing can use and refuse `-setString:` for a subclass
    // that only wants its own methods.
    ("NSAnimation", None, 0),
    ("NSAccessibilityElement", None, 0),
    ("NSATSTypesetter", None, 0),
    ("NSTableView", Some("NSControl"), 0),
    (
        "NSTitlebarAccessoryViewController",
        Some("NSViewController"),
        0,
    ),
];

pub fn install() {
    for (name, superclass, size) in HIERARCHY {
        declare(name, *superclass, *size);
    }
}

fn declare(name: &str, superclass: Option<&str>, size: usize) -> &'static Class {
    let parent = superclass.map(|s| {
        let (_, grandparent, size) = HIERARCHY
            .iter()
            .find(|(n, _, _)| n == &s)
            .copied()
            .unwrap_or((s, None, 0));
        declare(s, grandparent, size)
    });
    let name = std::ffi::CString::new(name).expect("a class name with no NUL in it");
    match size {
        0 => corefoundation::base::abstract_class(&name, parent),
        size => corefoundation::base::register(&name, parent, size, false),
    }
}

/// A class the nib named that nothing has declared, invented so the graph can
/// be built at all.
///
/// Descends from `NSObject` and nothing else, because the archive does not say
/// what it descends from - only what it is called. [`crate::nib`] reports every
/// one of these by name, so an instance that turns out to matter says so the
/// first time it is sent a message.
pub fn placeholder(name: &str) -> Option<&'static Class> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut invented = INVENTED.lock().unwrap_or_else(|e| e.into_inner());
    if !invented.iter().any(|n| n == name) {
        invented.push(name.to_string());
    }
    Some(corefoundation::base::abstract_class(&c, None))
}

static INVENTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn is_placeholder(name: &str) -> bool {
    INVENTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|n| n == name)
}

pub fn class_data() -> Vec<(String, u64)> {
    crate::bootstrap();
    let mut out = Vec::new();
    for (name, _, _) in HIERARCHY {
        let Some(class) = std::ffi::CString::new(*name)
            .ok()
            .and_then(|n| objc::class::lookup(&n))
        else {
            continue;
        };
        out.push((
            format!("_OBJC_CLASS_$_{name}"),
            objc::class::ptr(class) as usize as u64,
        ));
        out.push((
            format!("_OBJC_METACLASS_$_{name}"),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ));
    }
    out
}

pub fn names() -> Vec<&'static str> {
    HIERARCHY.iter().map(|(n, _, _)| *n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_class_is_registered_under_its_own_superclass() {
        crate::bootstrap();
        for (name, superclass, _) in HIERARCHY {
            let c = std::ffi::CString::new(*name).expect("a name");
            let class = objc::class::lookup(&c).unwrap_or_else(|| panic!("{name} is declared"));
            let parent = class.superclass().map(|p| p.name_str().to_string());
            let expected = superclass.unwrap_or("NSObject");
            assert_eq!(parent.as_deref(), Some(expected), "{name}");
        }
    }

    /// The one relationship a guest actually asks about, spelled out: a panel is
    /// a window, and a view is not.
    #[test]
    fn the_hierarchy_answers_is_kind_of() {
        crate::bootstrap();
        let window = objc::class::lookup(c"NSWindow").expect("NSWindow");
        let panel = objc::class::lookup(c"NSPanel").expect("NSPanel");
        let view = objc::class::lookup(c"NSView").expect("NSView");
        assert!(panel.chain().any(|c| std::ptr::eq(c, window)));
        assert!(!view.chain().any(|c| std::ptr::eq(c, window)));
    }

    /// The five superclasses the guest's own classes are declared against, by
    /// the exact Mach-O spelling it imports them under.
    ///
    /// The spelling is the point of the test, not a detail of it. A guest binds
    /// `_OBJC_CLASS_$_X` **verbatim, with no normalisation**, and a class
    /// registered and published under any other spelling leaves the import
    /// unbound - which a clean build and a green suite both pass straight over,
    /// because nothing in either can see a name that is never looked up. These
    /// five strings are the names in the Framework's bind stream, copied.
    const GUEST_SUBCLASSES: &[&str] = &[
        // ConstrainedWindowAnimationBase, FadeAllWindowsAnimation
        "NSAnimation",
        // AXPlatformNodeCocoa
        "NSAccessibilityElement",
        // GTMBreakRecordingTypeSetter
        "NSATSTypesetter",
        // TaskManagerMacTableView
        "NSTableView",
        // ImmersiveModeTitlebarViewController
        "NSTitlebarAccessoryViewController",
    ];

    #[test]
    fn a_class_the_guest_subclasses_is_published_under_the_name_it_imports() {
        crate::bootstrap();
        let published: Vec<String> = class_data().into_iter().map(|(name, _)| name).collect();
        for name in GUEST_SUBCLASSES {
            let c = std::ffi::CString::new(*name).expect("a name");
            let class = objc::class::lookup(&c)
                .unwrap_or_else(|| panic!("{name} is registered, or its subclass cannot be"));
            assert_eq!(
                objc::class::ptr(class) as usize as u64,
                class_data()
                    .into_iter()
                    .find(|(n, _)| n == &format!("_OBJC_CLASS_$_{name}"))
                    .unwrap_or_else(|| panic!("{name} is published as data"))
                    .1,
                "{name} is published as something other than itself"
            );
            assert!(
                published.contains(&format!("_OBJC_METACLASS_$_{name}")),
                "{name} publishes no metaclass, so a guest's +message on a subclass has nowhere \
                 to go"
            );
        }
    }

    /// A declared superclass has to be *reachable* from the subclass, not merely
    /// present: `ConstrainedWindowAnimationBase` sends `[super ...]`, and a
    /// parent that is not in the chain turns that into a refusal.
    #[test]
    fn a_declared_superclass_is_in_the_subclasss_chain() {
        crate::bootstrap();
        for (name, superclass, _) in HIERARCHY {
            let c = std::ffi::CString::new(*name).expect("a name");
            let Some(class) = objc::class::lookup(&c) else {
                continue;
            };
            let parent = std::ffi::CString::new(superclass.unwrap_or("NSObject")).expect("a name");
            let parent = objc::class::lookup(&parent).expect("the declared parent");
            assert!(
                class.chain().any(|c| std::ptr::eq(c, parent)),
                "{name} does not reach {superclass:?}"
            );
        }
    }
}
