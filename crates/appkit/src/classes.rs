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
    ("NSController", None, 0),
    (
        "NSUserDefaultsController",
        Some("NSController"),
        crate::bindings::DEFAULTS_CONTROLLER_INSTANCE_SIZE,
    ),
    (
        "NSTitlebarAccessoryViewController",
        Some("NSViewController"),
        0,
    ),
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
    // Basic animations; their state is `crate::coreanimation`'s side tables.
    ("CAAnimation", None, 0),
    ("CAPropertyAnimation", Some("CAAnimation"), 0),
    ("CABasicAnimation", Some("CAPropertyAnimation"), 0),
    ("CAMediaTimingFunction", None, 0),
    ("NSControl", Some("NSView"), 0),
    ("NSButton", Some("NSControl"), 0),
    ("NSPopUpButton", Some("NSButton"), 0),
    ("NSTextField", Some("NSControl"), 0),
    ("NSSegmentedControl", Some("NSControl"), 0),
    ("NSMatrix", Some("NSControl"), 0),
    ("NSTableView", Some("NSControl"), 0),
    ("NSOutlineView", Some("NSTableView"), 0),
    ("NSTableCellView", Some("NSView"), 0),
    ("NSTableRowView", Some("NSView"), 0),
    ("NSVisualEffectView", Some("NSView"), 0),
    ("NSTableColumn", None, 0),
    ("NSTableHeaderView", Some("NSView"), 0),
    ("NSImageView", Some("NSControl"), 0),
    ("NSCell", None, crate::cell::INSTANCE_SIZE),
    ("NSActionCell", Some("NSCell"), 0),
    ("NSButtonCell", Some("NSActionCell"), 0),
    ("NSPopUpButtonCell", Some("NSButtonCell"), 0),
    ("NSTextFieldCell", Some("NSActionCell"), 0),
    // Gecko's native theme subclasses the first and draws with both.
    ("NSSearchFieldCell", Some("NSTextFieldCell"), 0),
    ("NSComboBoxCell", Some("NSTextFieldCell"), 0),
    ("NSSegmentedCell", Some("NSActionCell"), 0),
    ("NSImageCell", Some("NSCell"), 0),
    ("NSText", Some("NSView"), 0),
    ("NSTextView", Some("NSText"), 0),
    ("NSScrollView", Some("NSView"), 0),
    ("NSScroller", Some("NSControl"), 0),
    ("NSClipView", Some("NSView"), 0),
    ("NSBox", Some("NSView"), 0),
    ("NSTabView", Some("NSView"), 0),
    ("NSTabViewItem", None, crate::tab::ITEM_INSTANCE_SIZE),
    ("NSStackView", Some("NSView"), 0),
    ("NSProgressIndicator", Some("NSView"), 0),
    ("NSSearchField", Some("NSTextField"), 0),
    // Qt's macOS style subclasses the first and draws with the rest.
    ("NSSplitView", Some("NSView"), 0),
    ("NSComboBox", Some("NSTextField"), 0),
    ("NSSlider", Some("NSControl"), crate::slider::INSTANCE_SIZE),
    ("NSSliderCell", Some("NSActionCell"), 0),
    ("NSStepperCell", Some("NSActionCell"), 0),
    ("NSLayoutConstraint", None, crate::layout::INSTANCE_SIZE),
    ("NSLayoutAnchor", None, crate::layout::ANCHOR_INSTANCE_SIZE),
    ("NSLayoutXAxisAnchor", Some("NSLayoutAnchor"), 0),
    ("NSLayoutYAxisAnchor", Some("NSLayoutAnchor"), 0),
    ("NSLayoutDimension", Some("NSLayoutAnchor"), 0),
    ("NSAnimationContext", None, 0),
    ("NSAnimation", None, 0),
    ("NSViewAnimation", Some("NSAnimation"), 0),
    ("NSAffineTransform", None, crate::transform::INSTANCE_SIZE),
    ("NSBezierPath", None, crate::bezier::INSTANCE_SIZE),
    ("NSParagraphStyle", None, crate::paragraph::INSTANCE_SIZE),
    ("NSMutableParagraphStyle", Some("NSParagraphStyle"), 0),
    (
        "NSOpenGLView",
        Some("NSView"),
        crate::opengl::VIEW_INSTANCE_SIZE,
    ),
    ("NSMenu", None, crate::menu::MENU_INSTANCE_SIZE),
    ("NSMenuItem", None, crate::menu::ITEM_INSTANCE_SIZE),
    ("NSScreen", None, crate::screen::INSTANCE_SIZE),
    // UIKit's screen shares `NSScreen`'s layout; see `crate::displaylink`.
    ("UIScreen", None, crate::screen::INSTANCE_SIZE),
    ("CADisplayLink", None, crate::displaylink::INSTANCE_SIZE),
    ("NSEvent", None, crate::nsevent::INSTANCE_SIZE),
    ("NSCursor", None, crate::cursor::CURSOR_INSTANCE_SIZE),
    ("NSColor", None, crate::color::INSTANCE_SIZE),
    (
        "NSAppearance",
        None,
        size_of::<crate::appearance::Appearance>(),
    ),
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
    // Its instances are the font dictionaries CoreText's descriptors are.
    ("NSFontDescriptor", None, 0),
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
    (
        "NSPasteboardItem",
        None,
        crate::pasteboard::ITEM_INSTANCE_SIZE,
    ),
    (
        "NSDraggingSession",
        None,
        crate::dragging::SESSION_INSTANCE_SIZE,
    ),
    // AppKit's private `NSDraggingInfo`, the sender a destination is handed.
    (
        "NSDraggingDestinationInfo",
        None,
        crate::dragging::SESSION_INSTANCE_SIZE,
    ),
    ("NSDraggingItem", None, crate::dragging::ITEM_INSTANCE_SIZE),
    ("NSWorkspace", None, 0),
    ("NSRunningApplication", None, 0),
    ("NSAlert", None, crate::alert::INSTANCE_SIZE),
    ("NSNib", None, 0),
    ("NSTextInputContext", None, 0),
    ("NSDockTile", None, crate::dock::INSTANCE_SIZE),
    ("NSStatusBar", None, 0),
    ("NSStatusItem", None, crate::status::ITEM_INSTANCE_SIZE),
    ("NSStatusBarButton", Some("NSButton"), 0),
    (
        "NSTrackingArea",
        None,
        crate::cursor::TRACKING_AREA_INSTANCE_SIZE,
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
}
