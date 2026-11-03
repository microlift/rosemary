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
        "NSClassSwapper" => custom_view(builder, uid),
        "NSView" => view(builder, uid, "NSView"),
        "NSMenu" => menu(builder, uid),
        "NSMenuItem" => menu_item(builder, uid),
        "NSImageView" => image_view(builder, uid),
        "NSCustomResource" => custom_resource(builder, uid),
        "NSBox" => box_view(builder, uid),
        "NSColor" => color(builder, uid),
        "NSFont" => font(builder, uid),
        "NSButton" => button(builder, uid),
        "NSPopUpButton" => pop_up_button(builder, uid),
        "NSScrollView" => scroll_view(builder, uid),
        "NSSegmentedControl" => segmented_control(builder, uid),
        "NSClipView" => clip_view(builder, uid),
        "NSTextView" => text_view(builder, uid),
        "NSTableView" => table_view(builder, uid),
        "NSTableColumn" => table_column(builder, uid),
        "NSScroller" => scroller(builder, uid),
        "NSTextField" => text_field(builder, uid),
        "NSMatrix" => matrix(builder, uid),
        "NSTabView" => tab_view(builder, uid),
        "NSTabViewItem" => tab_view_item(builder, uid),
        "NSTableHeaderView" => view(builder, uid, "NSTableHeaderView"),
        _ => None,
    }
}

/// The class names [`build`] decodes, for the report and for the tests.
pub(crate) const DECODED: &[&str] = &[
    "NSWindowTemplate",
    "NSCustomView",
    "NSClassSwapper",
    "NSView",
    "NSMenu",
    "NSMenuItem",
    "NSImageView",
    "NSCustomResource",
    "NSBox",
    "NSColor",
    "NSFont",
    "NSButton",
    "NSPopUpButton",
    "NSScrollView",
    "NSSegmentedControl",
    "NSClipView",
    "NSTextView",
    "NSTableView",
    "NSTableColumn",
    "NSScroller",
    "NSTextField",
    "NSMatrix",
    "NSTabView",
    "NSTabViewItem",
    "NSTableHeaderView",
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
    builder.archive().field(uid, key).and_then(|value| {
        value.as_str().or_else(|| {
            let reference = builder.archive().field_object(uid, key)?;
            builder.archive().string(reference)
        })
    })
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
    if let Some(transparent) = flag(builder, uid, "NSTitlebarAppearsTransparent") {
        // SAFETY: `-setTitlebarAppearsTransparent:` takes one `BOOL`.
        unsafe { send_flag(window, "setTitlebarAppearsTransparent:", transparent) };
    }
    if let Some(visibility) = integer(builder, uid, "NSWindowTitleVisibility") {
        // SAFETY: `-setTitleVisibility:` takes one `NSInteger`.
        unsafe { send_integer(window, "setTitleVisibility:", visibility as usize) };
    }
    Some(window)
}

fn custom_view(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let wanted = text(builder, uid, "NSClassName")
        .unwrap_or("NSView")
        .to_string();
    let view = view(builder, uid, &wanted)?;
    // A swapper keeps the archived class's own keys; a table's are its rows.
    if text(builder, uid, "NSOriginalClassName") == Some("NSTableView") {
        table_details(builder, uid, view);
    }
    Some(view)
}

/// `NSTableView`: a view, and its rows' height, background and columns.
fn table_view(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSTableView")?;
    table_details(builder, uid, view);
    Some(view)
}

fn table_details(builder: &mut dyn Builder, uid: Uid, view: Id) {
    if let Some(height) = real(builder, uid, "NSRowHeight") {
        // SAFETY: `-setRowHeight:` takes one `CGFloat`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, f64) = std::mem::transmute(objc::msg::entry());
            f(view, objc::sel::get("setRowHeight:"), height);
        }
    }
    if let Some(color) = builder.archive().field_object(uid, "NSBackgroundColor")
        && let Some(color) = builder.object_at(color)
    {
        // SAFETY: `-setBackgroundColor:` takes one colour.
        unsafe { send_object(view, "setBackgroundColor:", color) };
    }
    // The header names its table too; the link is made from this side only,
    // so building either one never needs the other half-built.
    if let Some(header) = builder.archive().field_object(uid, "NSHeaderView")
        && let Some(header) = builder.object_at(header)
    {
        // SAFETY: `-setHeaderView:` takes one view.
        unsafe { send_object(view, "setHeaderView:", header) };
    }
    if let Some(list) = builder.archive().field_object(uid, "NSTableColumns") {
        for entry in builder.archive().array(list) {
            if let Some(column) = builder.object_at(entry) {
                // SAFETY: `-addTableColumn:` takes one column.
                unsafe { send_object(view, "addTableColumn:", column) };
            }
        }
    }
}

/// `NSTableColumn`: its identifier, width, and the font and alignment of its
/// data cell.
fn table_column(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let column = allocate("NSTableColumn")?;
    if let Some(identifier) = text(builder, uid, "NSIdentifier") {
        let identifier = foundation::string::from_str(identifier);
        // SAFETY: `-setIdentifier:` takes one string.
        unsafe { send_object(column, "setIdentifier:", identifier) };
        objc::arc::release(identifier);
    }
    let width = real(builder, uid, "NSWidth").unwrap_or(100.0);
    let cell = builder.archive().field_object(uid, "NSDataCell");
    let font = cell
        .and_then(|cell| builder.archive().field_object(cell, "NSSupport"))
        .filter(|font| builder.archive().is_kind_of(*font, "NSFont"))
        .and_then(|font| builder.object_at(font))
        .unwrap_or(std::ptr::null_mut());
    let alignment = cell
        .and_then(|cell| word(builder, cell, "NSCellFlags2"))
        .map_or(0, |flags| {
            (flags & cell_flags::ALIGNMENT_MASK) >> cell_flags::ALIGNMENT_SHIFT
        });
    crate::table::set_archived_column(column, width, font, alignment as isize);
    if let Some(title) = builder
        .archive()
        .field_object(uid, "NSHeaderCell")
        .and_then(|cell| text(builder, cell, "NSContents"))
    {
        let title = foundation::string::from_str(title);
        // SAFETY: `-setTitle:` takes one string.
        unsafe { send_object(column, "setTitle:", title) };
        objc::arc::release(title);
    }
    Some(column)
}

/// An image view, and the image, scaling and alignment its cell holds.
fn image_view(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSImageView")?;
    let Some(cell) = builder.archive().field_object(uid, "NSCell") else {
        return Some(view);
    };
    if let Some(contents) = builder.archive().field_object(cell, "NSContents")
        && let Some(image) = builder.object_at(contents)
    {
        // SAFETY: `-setImage:` takes one object.
        unsafe { send_object(view, "setImage:", image) };
    }
    for (key, selector) in [
        ("NSScale", "setImageScaling:"),
        ("NSAlign", "setImageAlignment:"),
    ] {
        if let Some(value) = integer(builder, cell, key) {
            // SAFETY: both setters take one `NSUInteger`.
            unsafe { send_integer(view, selector, value as usize) };
        }
    }
    Some(view)
}

/// A named resource from the nib's bundle, which is how an image placed in
/// Interface Builder is archived. Only images are resources a nib here names.
fn custom_resource(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let class = text(builder, uid, "NSClassName").unwrap_or("").to_string();
    let name = text(builder, uid, "NSResourceName")?.to_string();
    if class != "NSImage" {
        builder.note_missing(&format!("NSCustomResource of class {class}"));
        return None;
    }
    let image_class = objc::class::lookup(c"NSImage")?;
    let name = foundation::string::from_str(&name);
    // SAFETY: `+imageNamed:` is `id (Class, SEL, id)`.
    let image = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(
            objc::class::ptr(image_class) as Id,
            objc::sel::get("imageNamed:"),
            name,
        )
    };
    objc::arc::release(name);
    (!image.is_null()).then_some(image)
}

/// An archived number field: a plain value, or an `NSNumber` entry holding one.
fn real(builder: &dyn Builder, uid: Uid, key: &str) -> Option<f64> {
    use foundation::plist::Value;
    let archive = builder.archive();
    let value = match archive.field_object(uid, key) {
        Some(number) => archive
            .field(number, "NS.dblval")
            .or_else(|| archive.field(number, "NS.intval"))?,
        None => archive.field(uid, key)?,
    };
    match value {
        Value::Real(v) => Some(*v),
        Value::Integer(v) => Some(*v as f64),
        _ => None,
    }
}

/// `NSFont`: the named face at the archived size, or the system font when
/// this host has no face of that name.
fn font(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let size = real(builder, uid, "NSSize").unwrap_or(0.0);
    let class = objc::class::lookup(c"NSFont")?;
    let name = text(builder, uid, "NSName").map(foundation::string::from_str);
    // SAFETY: both constructors are `id (Class, SEL, ...)` as declared on
    // `NSFont`, and the name is a string this call owns.
    let font = unsafe {
        let class = objc::class::ptr(class) as Id;
        let named = match name {
            Some(name) => {
                let f: unsafe extern "C" fn(Id, Sel, Id, f64) -> Id =
                    std::mem::transmute(objc::msg::entry());
                let font = f(class, objc::sel::get("fontWithName:size:"), name, size);
                objc::arc::release(name);
                font
            }
            None => std::ptr::null_mut(),
        };
        match named.is_null() {
            false => named,
            true => {
                let f: unsafe extern "C" fn(Id, Sel, f64) -> Id =
                    std::mem::transmute(objc::msg::entry());
                f(class, objc::sel::get("systemFontOfSize:"), size)
            }
        }
    };
    (!font.is_null()).then_some(font)
}

/// `NSCellFlags` and `NSCellFlags2`, the cell state a control built in
/// Interface Builder keeps instead of being sent it. The bit layout is the
/// one GNUstep's keyed `NSCell` decoder reads.
mod cell_flags {
    pub const EDITABLE: i64 = 0x1000_0000;
    pub const SELECTABLE: i64 = 0x0020_0000;
    pub const BORDERED: i64 = 0x0080_0000;
    pub const BEZELED: i64 = 0x0040_0000;
    pub const DISABLED: i64 = 0x2000_0000;
    pub const STATE: i64 = 0x8000_0000;
    pub const ALIGNMENT_SHIFT: i64 = 26;
    pub const ALIGNMENT_MASK: i64 = 0x1C00_0000;
    pub const LINE_BREAK_SHIFT: i64 = 9;
    pub const LINE_BREAK_MASK: i64 = 0xE00;
}

/// `NSButtonFlags` and `NSButtonFlags2`, as [`cell_flags`].
mod button_flags {
    /// `showsStateBy` includes `NSContentsCellMask`.
    pub const CHANGE_CONTENTS: i64 = 0x4000_0000;
    pub const BORDERED: i64 = 0x0080_0000;
    pub const TRANSPARENT: i64 = 0x8000;
    pub const BEZEL_LOW: i64 = 0x7;
    pub const BEZEL_HIGH: i64 = 0x20;
    pub const KEY_MODIFIER_SHIFT: i64 = 8;
}

/// An archived integer, read as the 32-bit word it was written as.
fn word(builder: &dyn Builder, uid: Uid, key: &str) -> Option<i64> {
    integer(builder, uid, key).map(|value| value & 0xFFFF_FFFF)
}

/// What every control and its cell carry: enabled, tag, font and alignment.
fn control(builder: &mut dyn Builder, uid: Uid, cell: Option<Uid>, view: Id) {
    if flag(builder, uid, "NSEnabled") == Some(false) {
        // SAFETY: `-setEnabled:` takes one `BOOL`.
        unsafe { send_flag(view, "setEnabled:", false) };
    }
    let tag =
        integer(builder, uid, "NSTag").or_else(|| cell.and_then(|c| integer(builder, c, "NSTag")));
    if let Some(tag) = tag {
        // SAFETY: `-setTag:` takes one `NSInteger`.
        unsafe { send_integer(view, "setTag:", tag as usize) };
    }
    let Some(cell) = cell else {
        return;
    };
    if let Some(font) = builder.archive().field_object(cell, "NSSupport")
        && builder.archive().is_kind_of(font, "NSFont")
        && responds(view, "setFont:")
        && let Some(font) = builder.object_at(font)
    {
        // SAFETY: `-setFont:` takes one object.
        unsafe { send_object(view, "setFont:", font) };
    }
    if let Some(flags2) = word(builder, cell, "NSCellFlags2")
        && responds(view, "setAlignment:")
    {
        let alignment = (flags2 & cell_flags::ALIGNMENT_MASK) >> cell_flags::ALIGNMENT_SHIFT;
        // SAFETY: `-setAlignment:` takes one `NSTextAlignment`.
        unsafe { send_integer(view, "setAlignment:", alignment as usize) };
    }
}

/// Whether `object`'s class implements `selector`: a setter for a cell
/// property only some controls here model is skipped on the others.
fn responds(object: Id, selector: &str) -> bool {
    // SAFETY: an object this runtime allocated.
    unsafe { objc::object::class_of(object) }
        .is_some_and(|class| class.method(objc::sel::get(selector)).is_some())
}

/// `NSButton`: a view, and the title, key equivalent and look its cell holds.
fn button(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSButton")?;
    let cell = builder.archive().field_object(uid, "NSCell");
    control(builder, uid, cell, view);
    if let Some(cell) = cell {
        button_cell(builder, cell, view);
    }
    Some(view)
}

/// What an `NSButtonCell` holds, given to the button that shows it.
fn button_cell(builder: &mut dyn Builder, cell: Uid, view: Id) {
    for (key, selector) in [
        ("NSContents", "setTitle:"),
        ("NSAlternateContents", "setAlternateTitle:"),
        ("NSKeyEquivalent", "setKeyEquivalent:"),
    ] {
        if let Some(value) = text(builder, cell, key) {
            let value = foundation::string::from_str(value);
            // SAFETY: each setter takes one string.
            unsafe { send_object(view, selector, value) };
            objc::arc::release(value);
        }
    }
    // An image button archives its image under `NSNormalImage`; a text button
    // stores its font where the alternate image would be, so only an image is
    // taken from there.
    for (key, selector) in [
        ("NSNormalImage", "setImage:"),
        ("NSAlternateImage", "setAlternateImage:"),
    ] {
        if let Some(image) = builder.archive().field_object(cell, key)
            && !builder.archive().is_kind_of(image, "NSFont")
            && !builder.archive().is_kind_of(image, "NSButtonImageSource")
            && let Some(image) = builder.object_at(image)
        {
            // SAFETY: both setters take one object.
            unsafe { send_object(view, selector, image) };
        }
    }
    if let Some(kind) = check_kind(builder, cell) {
        // SAFETY: `-setButtonType:` takes one `NSButtonType`.
        unsafe { send_integer(view, "setButtonType:", kind) };
    }
    if let Some(flags) = word(builder, cell, "NSButtonFlags") {
        // SAFETY: both setters take one `BOOL`.
        unsafe {
            send_flag(view, "setBordered:", flags & button_flags::BORDERED != 0);
            send_flag(
                view,
                "setTransparent:",
                flags & button_flags::TRANSPARENT != 0,
            );
        }
        crate::controls::set_shows_alternate(view, flags & button_flags::CHANGE_CONTENTS != 0);
    }
    if let Some(flags2) = word(builder, cell, "NSButtonFlags2") {
        let bezel = (flags2 & button_flags::BEZEL_LOW) | ((flags2 & button_flags::BEZEL_HIGH) >> 2);
        let modifiers = (flags2 >> button_flags::KEY_MODIFIER_SHIFT) & 0xFFFF_0000;
        // SAFETY: both setters take one `NSUInteger`.
        unsafe {
            send_integer(view, "setBezelStyle:", bezel as usize);
            send_integer(view, "setKeyEquivalentModifierMask:", modifiers as usize);
        }
    }
    if let Some(flags) = word(builder, cell, "NSCellFlags")
        && flags & cell_flags::STATE != 0
    {
        // SAFETY: `-setState:` takes one `NSInteger`.
        unsafe { send_integer(view, "setState:", 1) };
    }
}

/// `NSMatrixFlags`, the high bits of the 32-bit word.
mod matrix_flags {
    pub const HIGHLIGHT: i64 = 0x8000_0000;
    pub const RADIO: i64 = 0x4000_0000;
    pub const LIST: i64 = 0x2000_0000;
    pub const ALLOWS_EMPTY_SELECTION: i64 = 0x1000_0000;
}

/// `NSMatrix`: a view, its grid, and a button for each of its cells.
fn matrix(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    use crate::matrix::mode;
    let view = view(builder, uid, "NSMatrix")?;
    control(builder, uid, None, view);
    let button_for = |builder: &mut dyn Builder, cell: Uid| {
        let button = crate::matrix::new_cell(std::ptr::null_mut());
        control(builder, cell, Some(cell), button);
        if word(builder, cell, "NSCellFlags").is_some_and(|f| f & cell_flags::DISABLED != 0) {
            // SAFETY: `-setEnabled:` takes one `BOOL`.
            unsafe { send_flag(button, "setEnabled:", false) };
        }
        button_cell(builder, cell, button);
        button
    };
    let cells: Vec<Id> = match builder.archive().field_object(uid, "NSCells") {
        Some(list) => builder
            .archive()
            .array(list)
            .into_iter()
            .map(|cell| button_for(builder, cell))
            .collect(),
        None => Vec::new(),
    };
    let prototype = match builder.archive().field_object(uid, "NSProtoCell") {
        Some(cell) => button_for(builder, cell),
        None => std::ptr::null_mut(),
    };
    let flags = word(builder, uid, "NSMatrixFlags").unwrap_or(0);
    let mode = match flags {
        f if f & matrix_flags::RADIO != 0 => mode::RADIO,
        f if f & matrix_flags::LIST != 0 => mode::LIST,
        f if f & matrix_flags::HIGHLIGHT != 0 => mode::HIGHLIGHT,
        _ => mode::TRACK,
    };
    let index = |key: &str| integer(builder, uid, key).and_then(|v| usize::try_from(v).ok());
    let selected = index("NSSelectedRow").zip(index("NSSelectedCol"));
    let count = |key: &str| index(key).unwrap_or(0);
    crate::matrix::set_archived(
        view,
        crate::matrix::Archived {
            rows: count("NSNumRows"),
            columns: count("NSNumCols"),
            cell_size: text(builder, uid, "NSCellSize").and_then(size),
            spacing: text(builder, uid, "NSIntercellSpacing").and_then(size),
            mode,
            allows_empty_selection: flags & matrix_flags::ALLOWS_EMPTY_SELECTION != 0,
            cells,
            prototype,
            selected,
        },
    );
    Some(view)
}

/// `NSTabView`: a view, its items, and which one is shown.
fn tab_view(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSTabView")?;
    if let Some(font) = builder.archive().field_object(uid, "NSFont")
        && let Some(font) = builder.object_at(font)
    {
        // SAFETY: `-setFont:` takes one object.
        unsafe { send_object(view, "setFont:", font) };
    }
    let entries = builder
        .archive()
        .field_object(uid, "NSTabViewItems")
        .map(|list| builder.archive().array(list))
        .unwrap_or_default();
    let selected = builder.archive().field_object(uid, "NSSelectedTabViewItem");
    let selected = entries.iter().position(|entry| Some(*entry) == selected);
    let items: Vec<Id> = entries
        .into_iter()
        .filter_map(|entry| builder.object_at(entry))
        .collect();
    // The low bits of `NSTvFlags` are the `NSTabViewType`.
    let kind = word(builder, uid, "NSTvFlags").unwrap_or(0) & 0x7;
    crate::tab::set_archived(view, items, selected, kind as usize);
    Some(view)
}

/// `NSTabViewItem`: a label, an identifier and the view it shows. Its
/// `NSTabView` is the tab view that holds it, which sets that itself.
fn tab_view_item(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let Some(item) = allocate("NSTabViewItem") else {
        builder.note_missing("NSTabViewItem");
        return None;
    };
    if let Some(label) = text(builder, uid, "NSLabel") {
        let label = foundation::string::from_str(label);
        // SAFETY: `-setLabel:` takes one string.
        unsafe { send_object(item, "setLabel:", label) };
        objc::arc::release(label);
    }
    for (key, selector) in [("NSIdentifier", "setIdentifier:"), ("NSView", "setView:")] {
        if let Some(entry) = builder.archive().field_object(uid, key)
            && let Some(object) = builder.object_at(entry)
        {
            // SAFETY: both setters take one object.
            unsafe { send_object(item, selector, object) };
        }
    }
    Some(item)
}

/// The `NSButtonType` a check box or radio button cell was archived as.
///
/// Interface Builder records neither as a type: it gives the cell the system
/// artwork by name, as an `NSButtonImageSource`, which is the look and the
/// behaviour both.
fn check_kind(builder: &dyn Builder, cell: Uid) -> Option<usize> {
    ["NSNormalImage", "NSAlternateImage"]
        .iter()
        .find_map(|key| {
            let source = builder.archive().field_object(cell, key)?;
            if !builder.archive().is_kind_of(source, "NSButtonImageSource") {
                return None;
            }
            match text(builder, source, "NSImageName")? {
                "NSSwitch" => Some(crate::controls::button_type::SWITCH),
                "NSRadioButton" => Some(crate::controls::button_type::RADIO),
                _ => None,
            }
        })
}

/// `NSPopUpButton`: a view, its cell's menu, and which item is selected.
fn pop_up_button(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSPopUpButton")?;
    let cell = builder.archive().field_object(uid, "NSCell");
    control(builder, uid, cell, view);
    let Some(cell) = cell else {
        return Some(view);
    };
    if flag(builder, cell, "NSPullDown") == Some(true) {
        // SAFETY: `-setPullsDown:` takes one `BOOL`.
        unsafe { send_flag(view, "setPullsDown:", true) };
    }
    if let Some(menu) = builder.archive().field_object(cell, "NSMenu")
        && let Some(menu) = builder.object_at(menu)
    {
        // SAFETY: `-setMenu:` takes one menu.
        unsafe { send_object(view, "setMenu:", menu) };
    }
    if let Some(item) = builder.archive().field_object(cell, "NSMenuItem")
        && let Some(item) = builder.object_at(item)
    {
        // SAFETY: `-selectItem:` takes one item.
        unsafe { send_object(view, "selectItem:", item) };
    }
    Some(view)
}

/// `NSSegmentedControl`: a control, and the segments its cell lists.
fn segmented_control(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSSegmentedControl")?;
    let cell = builder.archive().field_object(uid, "NSCell");
    control(builder, uid, cell, view);
    let Some(cell) = cell else {
        return Some(view);
    };
    let mut segments = Vec::new();
    if let Some(list) = builder.archive().field_object(cell, "NSSegmentImages") {
        for item in builder.archive().array(list) {
            segments.push(crate::segmented::Archived {
                label: text(builder, item, "NSSegmentItemLabel")
                    .unwrap_or("")
                    .to_string(),
                tag: integer(builder, item, "NSSegmentItemTag").unwrap_or(0) as isize,
                width: real(builder, item, "NSSegmentItemWidth").unwrap_or(0.0),
                selected: flag(builder, item, "NSSegmentItemSelected").unwrap_or(false),
                disabled: flag(builder, item, "NSSegmentItemDisabled").unwrap_or(false),
            });
        }
    }
    crate::segmented::set_archived(view, segments);
    if let Some(mode) = integer(builder, cell, "NSTrackingMode") {
        // SAFETY: `-setTrackingMode:` takes one `NSSegmentSwitchTracking`.
        unsafe { send_integer(view, "setTrackingMode:", mode as usize) };
    }
    Some(view)
}

/// `NSScrollView`: a view whose clip view the archive names.
fn scroll_view(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSScrollView")?;
    if let Some(clip) = builder.archive().field_object(uid, "NSContentView")
        && let Some(clip) = builder.object_at(clip)
    {
        // SAFETY: `-setContentView:` takes one view.
        unsafe { send_object(view, "setContentView:", clip) };
    }
    Some(view)
}

/// `NSClipView`: a view whose document the archive names.
fn clip_view(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSClipView")?;
    if let Some(document) = builder.archive().field_object(uid, "NSDocView")
        && let Some(document) = builder.object_at(document)
    {
        // SAFETY: `-setDocumentView:` takes one view.
        unsafe { send_object(view, "setDocumentView:", document) };
    }
    Some(view)
}

/// `NSTextView`: a view; the text a nib can hold is in a text storage the
/// program replaces, so only the frame is taken.
fn text_view(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    view(builder, uid, "NSTextView")
}

/// `NSScroller`: placed where the archive puts it, and hidden, since
/// nothing here scrolls (see `crate::scroll`).
fn scroller(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSScroller")?;
    // SAFETY: `-setHidden:` takes one `BOOL`.
    unsafe { send_flag(view, "setHidden:", true) };
    Some(view)
}

/// `NSTextField`: a view, and the text, colours and editing its cell holds.
fn text_field(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSTextField")?;
    let cell = builder.archive().field_object(uid, "NSCell");
    control(builder, uid, cell, view);
    let Some(cell) = cell else {
        return Some(view);
    };
    if let Some(contents) = text(builder, cell, "NSContents") {
        let value = foundation::string::from_str(contents);
        // SAFETY: `-setStringValue:` takes one string.
        unsafe { send_object(view, "setStringValue:", value) };
        objc::arc::release(value);
    }
    for (key, selector) in [
        ("NSTextColor", "setTextColor:"),
        ("NSBackgroundColor", "setBackgroundColor:"),
    ] {
        if let Some(color) = builder.archive().field_object(cell, key)
            && let Some(color) = builder.object_at(color)
        {
            // SAFETY: both setters take one object.
            unsafe { send_object(view, selector, color) };
        }
    }
    let flags = word(builder, cell, "NSCellFlags").unwrap_or(0);
    // SAFETY: each setter takes one `BOOL`.
    unsafe {
        send_flag(view, "setEditable:", flags & cell_flags::EDITABLE != 0);
        send_flag(view, "setSelectable:", flags & cell_flags::SELECTABLE != 0);
        send_flag(
            view,
            "setBordered:",
            flags & (cell_flags::BORDERED | cell_flags::BEZELED) != 0,
        );
        send_flag(
            view,
            "setDrawsBackground:",
            flag(builder, cell, "NSDrawsBackground").unwrap_or(false),
        );
    }
    if flags & cell_flags::DISABLED != 0 {
        // SAFETY: `-setEnabled:` takes one `BOOL`.
        unsafe { send_flag(view, "setEnabled:", false) };
    }
    if let Some(flags2) = word(builder, cell, "NSCellFlags2") {
        let mode = (flags2 & cell_flags::LINE_BREAK_MASK) >> cell_flags::LINE_BREAK_SHIFT;
        // SAFETY: `-setLineBreakMode:` takes one `NSLineBreakMode`.
        unsafe { send_integer(view, "setLineBreakMode:", mode as usize) };
    }
    Some(view)
}

/// `NSBox`: a view, and how it is drawn.
fn box_view(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let view = view(builder, uid, "NSBox")?;
    // The content view is already a subview; the box only has to know which.
    if let Some(content) = builder.archive().field_object(uid, "NSContentView")
        && let Some(content) = builder.object_at(content)
    {
        // SAFETY: `-setContentView:` takes one view.
        unsafe { send_object(view, "setContentView:", content) };
    }
    if let Some(kind) = integer(builder, uid, "NSBoxType") {
        // SAFETY: `-setBoxType:` takes one `NSUInteger`.
        unsafe { send_integer(view, "setBoxType:", kind as usize) };
    }
    if let Some(transparent) = flag(builder, uid, "NSTransparent") {
        // SAFETY: `-setTransparent:` takes one `BOOL`.
        unsafe { send_flag(view, "setTransparent:", transparent) };
    }
    for (key, selector) in [
        ("NSFillColor2", "setFillColor:"),
        ("NSBorderColor2", "setBorderColor:"),
    ] {
        if let Some(entry) = builder.archive().field_object(uid, key)
            && let Some(color) = builder.object_at(entry)
        {
            // SAFETY: both setters take one object.
            unsafe { send_object(view, selector, color) };
        }
    }
    if let Some(position) = integer(builder, uid, "NSTitlePosition") {
        // SAFETY: `-setTitlePosition:` takes one `NSTitlePosition`.
        unsafe { send_integer(view, "setTitlePosition:", position as usize) };
    }
    if let Some(cell) = builder.archive().field_object(uid, "NSTitleCell") {
        if let Some(title) = text(builder, cell, "NSContents") {
            let title = foundation::string::from_str(title);
            // SAFETY: `-setTitle:` takes one string.
            unsafe { send_object(view, "setTitle:", title) };
            objc::arc::release(title);
        }
        if let Some(font) = builder.archive().field_object(cell, "NSSupport")
            && builder.archive().is_kind_of(font, "NSFont")
            && let Some(font) = builder.object_at(font)
        {
            // SAFETY: `-setTitleFont:` takes one font.
            unsafe { send_object(view, "setTitleFont:", font) };
        }
    }
    for (key, selector) in [
        ("NSBorderWidth2", "setBorderWidth:"),
        ("NSCornerRadius2", "setCornerRadius:"),
    ] {
        if let Some(value) = real(builder, uid, key) {
            // SAFETY: both setters take one `CGFloat`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, f64) = std::mem::transmute(objc::msg::entry());
                f(view, objc::sel::get(selector), value);
            }
        }
    }
    Some(view)
}

/// The numbers in an archived colour's component string, such as
/// `"0.098 0.102 0.122 1"`.
fn components(builder: &dyn Builder, uid: Uid, key: &str) -> Option<Vec<f64>> {
    let text = match builder.archive().field(uid, key)? {
        foundation::plist::Value::Data(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        foundation::plist::Value::String(text) => text.clone(),
        _ => return None,
    };
    let numbers: Vec<f64> = text
        .trim_end_matches('\0')
        .split_whitespace()
        .filter_map(|part| part.parse().ok())
        .collect();
    (!numbers.is_empty()).then_some(numbers)
}

/// `NSColor`, as Interface Builder archives one: RGB, grey, or a named
/// catalogue colour with an RGB or grey fallback. The components written for
/// a custom (sRGB) colour space are preferred over the calibrated `NSRGB`
/// ones, since sRGB is what this runtime draws in.
fn color(builder: &mut dyn Builder, uid: Uid) -> Option<Id> {
    let space = integer(builder, uid, "NSColorSpace")?;
    let rgba = |parts: &[f64]| {
        let get = |at: usize, default: f64| parts.get(at).copied().unwrap_or(default);
        crate::color::rgba(get(0, 0.0), get(1, 0.0), get(2, 0.0), get(3, 1.0))
    };
    match space {
        1 | 2 => {
            let parts = components(builder, uid, "NSComponents")
                .filter(|_| builder.archive().field(uid, "NSCustomColorSpace").is_some())
                .or_else(|| components(builder, uid, "NSRGB"))?;
            Some(rgba(&parts))
        }
        3 | 4 => {
            let parts = components(builder, uid, "NSWhite")?;
            let white = parts[0];
            Some(rgba(&[
                white,
                white,
                white,
                parts.get(1).copied().unwrap_or(1.0),
            ]))
        }
        6 => {
            let name = text(builder, uid, "NSColorName").map(str::to_owned);
            if let Some(named) = name.as_deref().and_then(crate::color::named_color) {
                return Some(named);
            }
            let fallback = builder.archive().field_object(uid, "NSColor")?;
            builder.object_at(fallback)
        }
        other => {
            builder.note_missing(&format!("NSColor in colour space {other}"));
            None
        }
    }
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
/// Bit 31 is `hidden`: a view Interface Builder left hidden is shown by the
/// program when it wants it.
mod vflags {
    pub const MASK: i64 = 0x3f;
    pub const AUTORESIZES_SUBVIEWS: i64 = 0x100;
    pub const HIDDEN: i64 = 0x8000_0000;
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
        if flags & vflags::HIDDEN != 0 {
            // SAFETY: `-setHidden:` takes one `BOOL`.
            unsafe { send_flag(view, "setHidden:", true) };
        }
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
