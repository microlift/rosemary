use foundation::geometry::{NSPoint, NSRect, NSSize};
use foundation::unarchiver::{Archive, Uid};
use objc::object::Id;
use objc::sel::Sel;

/// The numbers in a brace-delimited string, in order.
fn numbers(text: &str) -> Vec<f64> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e'))
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f64>().ok())
        .collect()
}

/// `{width, height}`.
pub fn size(text: &str) -> Option<NSSize> {
    match numbers(text)[..] {
        [width, height] => Some(NSSize { width, height }),
        _ => None,
    }
}

/// `{{x, y}, {width, height}}`.
pub fn rect(text: &str) -> Option<NSRect> {
    match numbers(text)[..] {
        [x, y, width, height] => Some(NSRect {
            origin: NSPoint { x, y },
            size: NSSize { width, height },
        }),
        _ => None,
    }
}

/// Send `-init...` to a freshly allocated object of `name`, or `None` if
/// nothing implements that class.
///
/// Allocation goes through the runtime rather than through `+alloc` so that a
/// guest class with its own `+alloc` override is not asked to allocate before
/// its `-init` runs, which is the order a nib's decoder uses.
fn allocate(name: &str) -> Option<Id> {
    let class = std::ffi::CString::new(name)
        .ok()
        .and_then(|n| objc::class::lookup(&n))?;
    Some(objc::object::create_instance(class, 0))
}

/// `[object selector:rect styleMask:... ]`, through the real dispatch so a
/// guest's override runs.
///
/// # Safety
///
/// The selector must name a method with this prototype.
unsafe fn send_init_window(object: Id, rect: NSRect, style: usize) -> Id {
    let sel = objc::sel::get("initWithContentRect:styleMask:backing:defer:");
    // SAFETY: the caller's obligation. `NSBackingStoreBuffered` is 2, and the
    // deferred flag is false because SDL makes the window either way.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect, usize, usize, u8) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(object, sel, rect, style, 2, 0)
    }
}

/// # Safety
///
/// The selector must name a method taking one `NSRect` and returning an object.
unsafe fn send_init_frame(object: Id, frame: NSRect) -> Id {
    let sel = objc::sel::get("initWithFrame:");
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(object, sel, frame)
    }
}

/// # Safety
///
/// The selector must name a method taking one object and returning nothing.
unsafe fn send_object(receiver: Id, selector: &str, argument: Id) {
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), argument);
    }
}

/// # Safety
///
/// The selector must name a method taking one `NSUInteger` and returning
/// nothing.
unsafe fn send_integer(receiver: Id, selector: &str, value: usize) {
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, usize) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), value);
    }
}

/// # Safety
///
/// The selector must name a method taking one `BOOL` and returning nothing.
unsafe fn send_flag(receiver: Id, selector: &str, value: bool) {
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(selector), u8::from(value));
    }
}

/// What a decoder needs from its caller: the archive, and a way to build the
/// objects an entry refers to.
pub(crate) trait Builder {
    fn archive(&self) -> &Archive;
    fn object_at(&mut self, uid: Uid) -> Option<Id>;
    /// Report a class the archive named that nothing implements.
    fn note_missing(&mut self, name: &str);
}

/// Build the object an archived entry stands for, or `None` for a class this
/// module does not decode.
pub(crate) fn build(builder: &mut dyn Builder, uid: Uid, class_name: &str) -> Option<Id> {
    match class_name {
        "NSWindowTemplate" => window_template(builder, uid),
        "NSCustomView" => custom_view(builder, uid),
        "NSView" => view(builder, uid, "NSView"),
        "NSMenu" => menu(builder, uid),
        "NSMenuItem" => menu_item(builder, uid),
        _ => None,
    }
}

/// The class names [`build`] decodes, for the report and for the tests.
pub(crate) const DECODED: &[&str] = &[
    "NSWindowTemplate",
    "NSCustomView",
    "NSView",
    "NSMenu",
    "NSMenuItem",
];

/// A boolean field, which an archive writes as a boolean or as an integer.
fn flag(builder: &dyn Builder, uid: Uid, key: &str) -> Option<bool> {
    match builder.archive().field(uid, key)? {
        foundation::plist::Value::Bool(value) => Some(*value),
        other => other.as_i64().map(|value| value != 0),
    }
}

fn integer(builder: &dyn Builder, uid: Uid, key: &str) -> Option<i64> {
    builder
        .archive()
        .field(uid, key)
        .and_then(foundation::plist::Value::as_i64)
}

/// `NSMenu`. A menu named `_NSMainMenu` is the application's menu bar, which
/// AppKit installs as it loads the main nib.
fn menu(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let Some(object) = allocate("NSMenu") else {
        builder.note_missing("NSMenu");
        return None;
    };
    let title = foundation::string::from_str(text(builder, uid, "NSTitle").unwrap_or(""));
    // SAFETY: `-initWithTitle:` takes one object and returns the menu.
    let menu = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(object, objc::sel::get("initWithTitle:"), title)
    };
    objc::arc::release(title);
    if menu.is_null() {
        return None;
    }
    if flag(builder, uid, "NSNoAutoenable") == Some(true) {
        // SAFETY: `-setAutoenablesItems:` takes one `BOOL`.
        unsafe { send_flag(menu, "setAutoenablesItems:", false) };
    }
    if let Some(items) = builder.archive().field_object(uid, "NSMenuItems") {
        for entry in builder.archive().array(items) {
            if let Some(item) = builder.object_at(entry) {
                // SAFETY: `-addItem:` takes one object.
                unsafe { send_object(menu, "addItem:", item) };
            }
        }
    }
    let application = crate::application::shared();
    let setter = match text(builder, uid, "NSName") {
        Some("_NSMainMenu") => Some("setMainMenu:"),
        Some("_NSWindowsMenu") => Some("setWindowsMenu:"),
        Some("_NSServicesMenu") => Some("setServicesMenu:"),
        _ => None,
    };
    if let Some(setter) = setter
        && !application.is_null()
    {
        // SAFETY: each setter takes one menu.
        unsafe { send_object(application, setter, menu) };
    }
    Some(menu)
}

/// `NSMenuItem`. The item's action usually arrives separately, as a control
/// connection the loader applies.
fn menu_item(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let Some(object) = allocate("NSMenuItem") else {
        builder.note_missing("NSMenuItem");
        return None;
    };
    // SAFETY: `-init` is `id (id, SEL)`.
    let item = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        f(object, objc::sel::get("init"))
    };
    if item.is_null() {
        return None;
    }
    if flag(builder, uid, "NSIsSeparator") == Some(true) {
        crate::menu::mark_separator(item);
        return Some(item);
    }
    for (key, setter) in [
        ("NSTitle", "setTitle:"),
        ("NSKeyEquiv", "setKeyEquivalent:"),
    ] {
        if let Some(value) = text(builder, uid, key) {
            let value = foundation::string::from_str(value);
            // SAFETY: both setters take one string.
            unsafe { send_object(item, setter, value) };
            objc::arc::release(value);
        }
    }
    for (key, setter) in [
        ("NSKeyEquivModMask", "setKeyEquivalentModifierMask:"),
        ("NSTag", "setTag:"),
        ("NSState", "setState:"),
        ("NSIndent", "setIndentationLevel:"),
    ] {
        if let Some(value) = integer(builder, uid, key) {
            // SAFETY: each setter takes one integer.
            unsafe { send_integer(item, setter, value as usize) };
        }
    }
    for (key, setter, when) in [
        ("NSIsDisabled", "setEnabled:", false),
        ("NSIsHidden", "setHidden:", true),
        ("NSIsAlternate", "setAlternate:", true),
    ] {
        if flag(builder, uid, key) == Some(true) {
            // SAFETY: each setter takes one `BOOL`.
            unsafe { send_flag(item, setter, when) };
        }
    }
    if let Some(action) = text(builder, uid, "NSAction") {
        // SAFETY: `-setAction:` takes one selector.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Sel) = std::mem::transmute(objc::msg::entry());
            f(item, objc::sel::get("setAction:"), objc::sel::get(action));
        }
    }
    if let Some(target) = builder.archive().field_object(uid, "NSTarget")
        && let Some(target) = builder.object_at(target)
    {
        // SAFETY: `-setTarget:` takes one object.
        unsafe { send_object(item, "setTarget:", target) };
    }
    if let Some(submenu) = builder.archive().field_object(uid, "NSSubmenu")
        && let Some(submenu) = builder.object_at(submenu)
    {
        // SAFETY: `-setSubmenu:` takes one menu.
        unsafe { send_object(item, "setSubmenu:", submenu) };
    }
    Some(item)
}

fn text<'a>(builder: &'a dyn Builder, uid: Uid, key: &str) -> Option<&'a str> {
    let value = builder.archive().field_object(uid, key)?;
    builder.archive().string(value)
}

fn window_template(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    // `NSWindowClass` is the class the document asked for, which is
    // `NSWindow` unless the program subclassed it. A subclass nothing
    // implements falls back to `NSWindow` and is reported, because a window of
    // the wrong class still shows the right pixels where a placeholder shows
    // none.
    let wanted = text(builder, uid, "NSWindowClass")
        .unwrap_or("NSWindow")
        .to_string();
    let object = match allocate(&wanted) {
        Some(object) => object,
        None => {
            builder.note_missing(&wanted);
            allocate("NSWindow")?
        }
    };

    let frame = text(builder, uid, "NSWindowRect")
        .and_then(rect)
        .unwrap_or(NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize {
                width: 640.0,
                height: 480.0,
            },
        });
    let style = builder
        .archive()
        .field(uid, "NSWindowStyleMask")
        .and_then(foundation::plist::Value::as_i64)
        .unwrap_or(0) as usize;
    // SAFETY: the selector's prototype is the one stated at `send_init_window`.
    let window = unsafe { send_init_window(object, frame, style) };
    if window.is_null() {
        return None;
    }

    if let Some(title) = text(builder, uid, "NSWindowTitle") {
        let title = foundation::string::from_str(title);
        // SAFETY: `-setTitle:` takes one object.
        unsafe { send_object(window, "setTitle:", title) };
    }
    if let Some(view) = builder.archive().field_object(uid, "NSWindowView")
        && let Some(view) = builder.object_at(view)
    {
        // SAFETY: `-setContentView:` takes one object.
        unsafe { send_object(window, "setContentView:", view) };
    }
    Some(window)
}

fn custom_view(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let wanted = text(builder, uid, "NSClassName")
        .unwrap_or("NSView")
        .to_string();
    view(builder, uid, &wanted)
}

/// The bits of `NSvFlags`, which is where a view built in Interface Builder
/// keeps what it would otherwise be sent.
///
/// This is the whole reason a nib's view resizes with its window: nothing sends
/// `-setAutoresizingMask:` to a view the document positioned, so a decoder that
/// drops these leaves every one of them `NSViewNotSizable` - and a program whose
/// render view is a subview of the content view then draws at the size the
/// window had when it opened, for the rest of the run.
///
/// Bit 31 is `hidden`, which is not decoded: `-setHidden:` is ignored in this
/// tree, so applying it would record a state nothing acts on.
mod vflags {
    pub const MASK: i64 = 0x3f;
    pub const AUTORESIZES_SUBVIEWS: i64 = 0x100;
}

fn view(builder: &mut dyn Builder, uid: Uid, class_name: &str) -> Option<Id> {
    let object = match allocate(class_name) {
        Some(object) => object,
        None => {
            builder.note_missing(class_name);
            return None;
        }
    };
    // `NSFrame` carries the origin the view sits at inside its superview, and
    // `NSFrameSize` is what Interface Builder writes instead when that origin is
    // zero. The origin is not cosmetic: it is the leading margin the
    // autoresizing mask distributes a resize over.
    let frame = text(builder, uid, "NSFrame")
        .and_then(rect)
        .or_else(|| {
            text(builder, uid, "NSFrameSize")
                .and_then(size)
                .map(|size| NSRect {
                    origin: NSPoint::default(),
                    size,
                })
        })
        .unwrap_or_default();
    // SAFETY: the selector's prototype is the one stated at `send_init_frame`.
    let view = unsafe { send_init_frame(object, frame) };
    if view.is_null() {
        return None;
    }

    if builder
        .archive()
        .field(uid, "NSViewWantsBestResolutionOpenGLSurface")
        .and_then(foundation::plist::Value::as_i64)
        .unwrap_or(0)
        != 0
    {
        // SAFETY: `-setWantsBestResolutionOpenGLSurface:` takes one `BOOL`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, u8) = std::mem::transmute(objc::msg::entry());
            f(
                view,
                objc::sel::get("setWantsBestResolutionOpenGLSurface:"),
                1,
            );
        }
    }

    if let Some(flags) = builder
        .archive()
        .field(uid, "NSvFlags")
        .and_then(foundation::plist::Value::as_i64)
    {
        // SAFETY: `-setAutoresizingMask:` takes one `NSUInteger`.
        unsafe {
            send_integer(
                view,
                "setAutoresizingMask:",
                (flags & vflags::MASK) as usize,
            )
        };
        // SAFETY: `-setAutoresizesSubviews:` takes one `BOOL`.
        unsafe {
            send_flag(
                view,
                "setAutoresizesSubviews:",
                flags & vflags::AUTORESIZES_SUBVIEWS != 0,
            )
        };
    }

    if let Some(subviews) = builder.archive().field_object(uid, "NSSubviews") {
        for entry in builder.archive().array(subviews) {
            if let Some(subview) = builder.object_at(entry) {
                // SAFETY: `-addSubview:` takes one object.
                unsafe { send_object(view, "addSubview:", subview) };
            }
        }
    }
    Some(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One archived view of each kind a nib holds: a plain `NSView` that is the
    /// window's, and the custom view a program's own class fills it with.
    const VIEWS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>$archiver</key>
	<string>NSKeyedArchiver</string>
	<key>$objects</key>
	<array>
		<string>$null</string>
		<dict>
			<key>$class</key><dict><key>CF$UID</key><integer>6</integer></dict>
			<key>NSvFlags</key><integer>319</integer>
			<key>NSFrameSize</key><dict><key>CF$UID</key><integer>2</integer></dict>
			<key>NSSubviews</key><dict><key>CF$UID</key><integer>3</integer></dict>
		</dict>
		<string>{1000, 665}</string>
		<dict>
			<key>$class</key><dict><key>CF$UID</key><integer>8</integer></dict>
			<key>NS.objects</key>
			<array><dict><key>CF$UID</key><integer>4</integer></dict></array>
		</dict>
		<dict>
			<key>$class</key><dict><key>CF$UID</key><integer>7</integer></dict>
			<key>NSvFlags</key><integer>274</integer>
			<key>NSClassName</key><dict><key>CF$UID</key><integer>9</integer></dict>
			<key>NSFrame</key><dict><key>CF$UID</key><integer>5</integer></dict>
		</dict>
		<string>{{10, 20}, {900, 600}}</string>
		<dict>
			<key>$classes</key><array><string>NSView</string></array>
			<key>$classname</key><string>NSView</string>
		</dict>
		<dict>
			<key>$classes</key><array><string>NSCustomView</string></array>
			<key>$classname</key><string>NSCustomView</string>
		</dict>
		<dict>
			<key>$classes</key><array><string>NSMutableArray</string></array>
			<key>$classname</key><string>NSMutableArray</string>
		</dict>
		<string>NSView</string>
	</array>
	<key>$top</key>
	<dict>
		<key>root</key><dict><key>CF$UID</key><integer>1</integer></dict>
	</dict>
	<key>$version</key>
	<integer>100000</integer>
</dict>
</plist>"#;

    struct Test<'a> {
        archive: &'a Archive,
    }

    impl Builder for Test<'_> {
        fn archive(&self) -> &Archive {
            self.archive
        }
        fn object_at(&mut self, uid: Uid) -> Option<Id> {
            let name = self.archive.class_name(uid)?.to_string();
            build(self, uid, &name)
        }
        fn note_missing(&mut self, name: &str) {
            panic!("nothing implements {name}");
        }
    }

    fn decode(archive: &str) -> Id {
        crate::bootstrap();
        let archive = Archive::parse(archive.as_bytes()).expect("an archive");
        let root = archive.root().expect("a root");
        let mut builder = Test { archive: &archive };
        builder.object_at(root).expect("a view")
    }

    fn send<R>(receiver: Id, selector: &str) -> R {
        // SAFETY: every selector this reads is a nullary accessor of `NSView`'s.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> R = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(selector))
        }
    }

    /// `-frame` returns a rectangle, which x86-64 returns through a hidden
    /// pointer - so it is read through the method rather than through
    /// `objc_msgSend`, whose non-`_stret` entry would shift every argument.
    fn frame_of(view: Id) -> NSRect {
        let sel = objc::sel::get("frame");
        // SAFETY: a view this module decoded.
        let class = unsafe { objc::object::class_of(view) }.expect("a class");
        let method = class.method(sel).expect("-frame");
        // SAFETY: `-frame` is `NSRect (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> NSRect =
                std::mem::transmute(method.implementation());
            f(view, sel)
        }
    }

    /// A view resizes with its superview because the *nib* said so. Nothing
    /// sends `-setAutoresizingMask:` to a view a document positioned, so a
    /// decoder that drops `NSvFlags` is a program that renders at its opening
    /// size for the rest of the run.
    #[test]
    fn a_nibs_view_gets_the_autoresizing_mask_the_document_recorded() {
        let content = decode(VIEWS);
        assert_eq!(send::<usize>(content, "autoresizingMask"), 0x3f);
        assert_eq!(send::<u8>(content, "autoresizesSubviews"), 1);

        let subviews = send::<Id>(content, "subviews");
        // SAFETY: an array this runtime made.
        let array =
            unsafe { corefoundation::collections::array(subviews as *const std::ffi::c_void) }
                .expect("an array");
        let view = *array.values.first().expect("one subview") as Id;
        // `NSViewWidthSizable | NSViewHeightSizable`.
        assert_eq!(send::<usize>(view, "autoresizingMask"), 0x12);
    }

    /// And it is where the document put it: the origin is the margin the mask
    /// distributes a resize over, so a view decoded at `(0, 0)` grows from the
    /// wrong place even once the mask is right.
    #[test]
    fn a_subview_keeps_the_origin_the_document_gave_it() {
        let content = decode(VIEWS);
        let subviews = send::<Id>(content, "subviews");
        // SAFETY: an array this runtime made.
        let array =
            unsafe { corefoundation::collections::array(subviews as *const std::ffi::c_void) }
                .expect("an array");
        let view = *array.values.first().expect("one subview") as Id;
        let frame = frame_of(view);
        assert_eq!(frame.origin, NSPoint { x: 10.0, y: 20.0 });
        assert_eq!(
            frame.size,
            NSSize {
                width: 900.0,
                height: 600.0
            }
        );
    }

    #[test]
    fn interface_builders_geometry_strings_parse() {
        assert_eq!(
            rect("{{300, 300}, {1000, 665}}"),
            Some(NSRect {
                origin: NSPoint { x: 300.0, y: 300.0 },
                size: NSSize {
                    width: 1000.0,
                    height: 665.0
                },
            })
        );
        assert_eq!(
            size("{800, 600}"),
            Some(NSSize {
                width: 800.0,
                height: 600.0
            })
        );
        // A negative origin is ordinary - a window on a display left of the
        // primary one has one.
        assert_eq!(
            rect("{{-100, 0}, {10, 20}}").map(|r| r.origin.x),
            Some(-100.0)
        );
    }

    /// Anything that is not the shape asked for is `None`, not a default: a
    /// zero rectangle would be a window at the origin with no size, which looks
    /// like a window the program asked for.
    #[test]
    fn a_string_of_the_wrong_shape_is_not_a_rectangle() {
        assert_eq!(rect("{800, 600}"), None);
        assert_eq!(rect(""), None);
        assert_eq!(size("{{0, 0}, {1, 1}}"), None);
    }
}
