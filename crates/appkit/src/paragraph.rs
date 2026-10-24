use std::ffi::c_void;
use std::sync::OnceLock;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// What text drawing reads from a paragraph style. A fresh instance is all
/// zeroes, which is left alignment and word wrapping.
#[repr(C)]
#[derive(Clone, Copy)]
struct Style {
    isa: *const c_void,
    alignment: isize,
    line_break: isize,
}

pub const INSTANCE_SIZE: usize = size_of::<Style>();

fn style_of<'a>(receiver: Id) -> &'a mut Style {
    // SAFETY: an `NSParagraphStyle`, which this module laid out.
    unsafe { &mut *(receiver as *mut Style) }
}

/// The `NSTextAlignment` of a paragraph style, or `None` for something else.
pub(crate) fn alignment(style: Id) -> Option<isize> {
    let class = objc::class::lookup(c"NSParagraphStyle")?;
    // SAFETY: null or an object this runtime allocated.
    let object_class = unsafe { objc::object::class_of(style) }?;
    object_class
        .chain()
        .any(|c| std::ptr::eq(c, class))
        .then(|| style_of(style).alignment)
}

unsafe extern "C" fn default_paragraph_style(_class: Id, _sel: Sel) -> Id {
    static DEFAULT: OnceLock<usize> = OnceLock::new();
    *DEFAULT.get_or_init(|| {
        let class = objc::class::lookup(c"NSParagraphStyle").expect("declared");
        let object = objc::object::create_instance(class, 0);
        objc::arc::retain(object);
        object as usize
    }) as Id
}

unsafe extern "C" fn get_alignment(receiver: Id, _sel: Sel) -> isize {
    style_of(receiver).alignment
}

unsafe extern "C" fn set_alignment(receiver: Id, _sel: Sel, value: isize) {
    style_of(receiver).alignment = value;
}

unsafe extern "C" fn line_break_mode(receiver: Id, _sel: Sel) -> isize {
    style_of(receiver).line_break
}

unsafe extern "C" fn set_line_break_mode(receiver: Id, _sel: Sel, value: isize) {
    style_of(receiver).line_break = value;
}

unsafe extern "C" fn set_paragraph_style(receiver: Id, _sel: Sel, other: Id) {
    if other.is_null() {
        return;
    }
    let from = *style_of(other);
    let to = style_of(receiver);
    to.alignment = from.alignment;
    to.line_break = from.line_break;
    copy_properties(other, receiver);
}

/// The properties kept as associations, see [`crate::props`]: the objects,
/// then the numbers.
const OBJECTS: &[&str] = &["tabStops", "textBlocks", "textLists"];
const NUMBERS: &[&str] = &[
    "headIndent",
    "firstLineHeadIndent",
    "tailIndent",
    "lineSpacing",
    "paragraphSpacing",
    "paragraphSpacingBefore",
    "minimumLineHeight",
    "maximumLineHeight",
    "lineHeightMultiple",
    "defaultTabInterval",
    "baseWritingDirection",
    "hyphenationFactor",
    "tighteningFactorForTruncation",
];

/// Copy `from`'s association-backed properties onto `to`.
fn copy_properties(from: Id, to: Id) {
    for name in OBJECTS {
        let key = objc::sel::get(name) as *const c_void;
        objc::arc::set_associated_with_policy(to, key, objc::arc::associated(from, key), 1);
    }
    for name in NUMBERS {
        let key = objc::sel::get(name) as *const c_void;
        objc::arc::set_associated_word(to, key, objc::arc::associated(from, key) as usize);
    }
}

fn copy_into(receiver: Id, class: &'static std::ffi::CStr) -> Id {
    let class = objc::class::lookup(class).expect("declared");
    let object = objc::object::create_instance(class, 0);
    // SAFETY: both are paragraph styles.
    unsafe { set_paragraph_style(object, objc::sel::get("setParagraphStyle:"), receiver) };
    object
}

unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *const c_void) -> Id {
    copy_into(receiver, c"NSParagraphStyle")
}

unsafe extern "C" fn mutable_copy_with_zone(receiver: Id, _sel: Sel, _zone: *const c_void) -> Id {
    copy_into(receiver, c"NSMutableParagraphStyle")
}

pub(crate) fn install() {
    let paragraph = objc::class::lookup(c"NSParagraphStyle").expect("declared");
    class_methods!(paragraph, {
        "defaultParagraphStyle" => default_paragraph_style, c"@16@0:8";
    });
    methods!(paragraph, {
        "alignment" => get_alignment, c"q16@0:8";
        "lineBreakMode" => line_break_mode, c"Q16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
        "mutableCopyWithZone:" => mutable_copy_with_zone, c"@24@0:8^v16";
    });
    // FIXME (ACCURACY): kept for reading back; text drawing honours only the
    // alignment and the line break mode.
    crate::props::properties!(paragraph, {
        retained "tabStops", "setTabStops:";
        retained "textBlocks", "setTextBlocks:";
        retained "textLists", "setTextLists:";
        double "headIndent", "setHeadIndent:";
        double "firstLineHeadIndent", "setFirstLineHeadIndent:";
        double "tailIndent", "setTailIndent:";
        double "lineSpacing", "setLineSpacing:";
        double "paragraphSpacing", "setParagraphSpacing:";
        double "paragraphSpacingBefore", "setParagraphSpacingBefore:";
        double "minimumLineHeight", "setMinimumLineHeight:";
        double "maximumLineHeight", "setMaximumLineHeight:";
        double "lineHeightMultiple", "setLineHeightMultiple:";
        double "defaultTabInterval", "setDefaultTabInterval:";
        integer "baseWritingDirection", "setBaseWritingDirection:";
        double "hyphenationFactor", "setHyphenationFactor:";
        double "tighteningFactorForTruncation", "setTighteningFactorForTruncation:";
    });

    let mutable = objc::class::lookup(c"NSMutableParagraphStyle").expect("declared");
    methods!(mutable, {
        "setParagraphStyle:" => set_paragraph_style, c"v24@0:8@16";
        "setLineBreakMode:" => set_line_break_mode, c"v24@0:8Q16";
        "setAlignment:" => set_alignment, c"v24@0:8q16";
    });
}
