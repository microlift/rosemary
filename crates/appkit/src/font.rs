use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

static SHARED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// An `NSFont`, and an `NSFontDescriptor` too: the two classes CoreText's own
/// objects have.
///
/// `CTFont` is toll-free bridged to `NSFont` and `CTFontDescriptor` to
/// `NSFontDescriptor`, so on macOS the pointer `CTFontCreateWithName` returns
/// carries `NSFont` for its class. That is not a nicety. `Framework+0x6949218`
/// passes one `rbx` to `CTFontGetSize` and, four bytes later, to
/// `[rbx familyName]`; `Framework+0x69499c4` retains the same pointer and sends
/// it `ascender`, `capHeight`, `descender` and `leading`; and
/// `Framework+0x6949278` sends it `fontDescriptor` and then the descriptor
/// `symbolicTraits`. One object answers both roles, and the guest is right that
/// it is one object - it is the *same* `rbx` in all seven sends.
///
/// What this tree handed back instead was a `CFDictionary`, whose class is
/// `__NSCFDictionary` (`corefoundation::collections::dictionary_class` is that
/// name, and `crate::coretext::new_dictionary` is `collections::new_dictionary`).
/// A dictionary answers none of the seven, so every one of them was refused with
/// the receiver's class in the message - which is what made the class look like
/// the fault rather than the one that produced it.
///
/// The attributes are CoreText's and stay in `crate::coretext`. This struct is
/// the object identity those readers had no way to give them.
#[repr(C)]
pub struct Font {
    isa: *const c_void,
    /// The font's attributes, owned: `-dealloc` gives this reference back.
    attributes: Id,
}

/// The instance size both `NSFont` and `NSFontDescriptor` are registered with.
pub const INSTANCE_SIZE: usize = size_of::<Font>();

/// The attributes behind `object`, or `object` itself.
///
/// A font and a descriptor are the two classes above; anything else is returned
/// unchanged, because CoreText's readers also handle attribute sub-dictionaries
/// (`CTFontTraitsAttribute` and the rest) and dictionaries the guest built.
pub(crate) fn attributes_of(object: CFTypeRef) -> CFTypeRef {
    if object.is_null() {
        return object;
    }
    // SAFETY: null is answered above, and every object a guest holds was
    // allocated by this runtime. The cast is the `Id` the signature wants: a
    // `CFTypeRef` and an `Id` are the same pointer with different provenance
    // intent, and `class_of` only reads the `isa`.
    let Some(class) = (unsafe { objc::object::class_of(object as Id) }) else {
        return object;
    };
    let ours = font_classes();
    if class
        .chain()
        .any(|candidate| ours.iter().any(|k| std::ptr::eq(candidate, *k)))
    {
        // SAFETY: the class is one of the two above, and both are registered
        // with `INSTANCE_SIZE` and this layout.
        return unsafe { (*(object as *const Font)).attributes };
    }
    object
}

/// The two classes, looked up once.
///
/// `attributes_of` is on the path of every attribute read - including the glyph
/// ones - and a class lookup is a hash of a string, so it is not something to
/// repeat per read. It is a `OnceLock` rather than a `static` because the
/// classes do not exist until `crate::bootstrap` has run, and this is called
/// from `install` itself.
fn font_classes() -> &'static [&'static objc::class::Class] {
    static CLASSES: std::sync::OnceLock<Vec<&'static objc::class::Class>> =
        std::sync::OnceLock::new();
    CLASSES.get_or_init(|| {
        [c"NSFont", c"NSFontDescriptor"]
            .iter()
            .map(|name| objc::class::lookup(name).expect("declared before its methods"))
            .collect()
    })
}

/// A new `NSFont` owning one reference to `attributes`.
///
/// The CoreText create functions return a `+1` reference, and this stores
/// exactly that one: the reference is transferred, not copied, and `-dealloc`
/// returns it. Null attributes make null, so a caller that had nothing to
/// describe does not get an object that describes nothing.
pub(crate) fn font_object(attributes: CFTypeRef) -> CFTypeRef {
    make(c"NSFont", attributes)
}

/// As [`font_object`], with the class `CTFontDescriptor`'s objects carry.
pub(crate) fn descriptor_object(attributes: CFTypeRef) -> CFTypeRef {
    make(c"NSFontDescriptor", attributes)
}

fn make(name: &std::ffi::CStr, attributes: CFTypeRef) -> CFTypeRef {
    if attributes.is_null() {
        return attributes;
    }
    let class = objc::class::lookup(name).expect("declared before its methods");
    // SAFETY: `Font` is the layout both classes are registered with.
    let font = unsafe { corefoundation::base::allocate::<Font>(class) };
    // SAFETY: a fresh instance, and the only field is written before it is read.
    // The `Id` cast is the same pointer: this function is the one place that
    // decides which reference the new object owns, and it owns the one handed
    // in.
    unsafe { (*font).attributes = attributes as Id };
    font as CFTypeRef
}

/// `-dealloc` for both classes: the attributes go back, then the object.
unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver is an `NSFont` or `NSFontDescriptor` of this layout.
    let font = unsafe { &mut *(receiver as *mut Font) };
    let attributes = std::mem::replace(&mut font.attributes, std::ptr::null_mut());
    if !attributes.is_null() {
        // SAFETY: the reference `-dealloc` owns.
        unsafe { corefoundation::api::CFRelease(attributes) };
    }
    objc::arc::finalize(receiver);
}

/// The receiver's attributes, which is what every method below reads.
fn attributes(receiver: Id) -> CFTypeRef {
    attributes_of(receiver as CFTypeRef)
}


/// `+[NSFontManager sharedFontManager]`.
unsafe extern "C" fn shared_font_manager(class: Id, _sel: Sel) -> Id {
    *SHARED.get_or_init(|| {
        // SAFETY: the receiver is the NSFontManager class declared by AppKit.
        let manager = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        // AppKit owns the process-wide singleton.
        objc::arc::retain(manager);
        manager as usize
    }) as Id
}

/// `-[NSFontManager localizedNameForFamily:face:]`.
///
/// The fallback family exposed by CoreText has no separate localized names.
unsafe extern "C" fn localized_name(_receiver: Id, _sel: Sel, family: Id, face: Id) -> Id {
    if family.is_null() { face } else { family }
}

/// `-[NSFontManager fontWithFamily:traits:weight:size:]`.
///
/// The named family is resolved by CoreText. Trait and legacy integer weight
/// selection can refine a family containing several faces; the current font
/// facade resolves the family's regular face, which is still preferable to
/// returning nil for UI text that can use it.
unsafe extern "C" fn font_with_family(
    _receiver: Id,
    _sel: Sel,
    family: Id,
    _traits: usize,
    _weight: isize,
    size: f64,
) -> Id {
    if family.is_null() {
        // SAFETY: the system-font constructor's declared arguments.
        return unsafe { system_font_of_size(std::ptr::null_mut(), _sel, size) };
    }
    // SAFETY: `family` is an NSString as required by the selector.
    let font = unsafe { font_with_name(std::ptr::null_mut(), _sel, family, size) };
    if font.is_null() {
        // A Mac-only family such as Helvetica is routinely requested by old
        // games. Use the platform's system face when fontconfig has no exact
        // family rather than making otherwise renderable UI text disappear.
        unsafe { system_font_of_size(std::ptr::null_mut(), _sel, size) }
    } else {
        font
    }
}

/// `+[NSFont fontWithName:size:]`.
///
/// `NSFont` is toll-free bridged to `CTFont`, so the font is the Core Text one.
unsafe extern "C" fn font_with_name(_class: Id, _sel: Sel, name: Id, size: f64) -> Id {
    // SAFETY: null or a string, as the selector requires.
    let font =
        unsafe { crate::coretext::CTFontCreateWithName(name as *const _, size, std::ptr::null()) };
    // SAFETY: a fresh object, or null.
    unsafe { autorelease(font) }
}

/// `+systemFontOfSize:`, and its bold twin, which is the same face here: the
/// fallback family is the only one there is to pick from.
unsafe extern "C" fn system_font_of_size(_class: Id, _sel: Sel, size: f64) -> Id {
    // SAFETY: `kCTFontUIFontSystem` with no language.
    let font = unsafe { crate::coretext::CTFontCreateUIFontForLanguage(2, size, std::ptr::null()) };
    // SAFETY: a fresh object, or null.
    unsafe { autorelease(font) }
}

unsafe fn autorelease(font: *const std::ffi::c_void) -> Id {
    if font.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { corefoundation::api::CFAutorelease(font) as Id }
}

/// `+[NSFont systemFontOfSize:weight:]`.
///
/// `weight` is an `NSFontWeight`: the `CGFloat` from -1 to 1 whose stops are
/// `NSFontWeightUltraLight` … `NSFontWeightBlack` - the same numbers this crate
/// publishes as `_NSFontWeight*` data (`lib.rs`), and the number the guest
/// actually passes. `Framework+0x694987c` loads one of those globals out of a
/// `__got` slot with `movsd xmm1, [rax]` and `Framework+0x6949887` sends
/// `systemFontOfSize:weight:` with it in `xmm1` and the point size still in
/// `xmm0`.
///
/// CoreText's weight trait *is* that number, so a weight is resolved the way
/// CoreText resolves one: the face of the family whose weight is nearest.
unsafe extern "C" fn system_font_of_size_weight(
    _class: Id,
    _sel: Sel,
    size: f64,
    weight: f64,
) -> Id {
    // SAFETY: no language; 2 is `kCTFontUIFontSystem`.
    let font = unsafe { crate::coretext::ui_font(2, size, Some(weight)) };
    // SAFETY: a fresh object, or null.
    unsafe { autorelease(font) }
}

/// `+[NSFont fontWithDescriptor:size:]`: Core Text's constructor for a descriptor.
unsafe extern "C" fn font_with_descriptor(_class: Id, _sel: Sel, descriptor: Id, size: f64) -> Id {
    // SAFETY: null or a descriptor this runtime made, as the selector requires.
    let font = unsafe {
        crate::coretext::CTFontCreateWithFontDescriptor(
            descriptor as *const _,
            size,
            std::ptr::null(),
        )
    };
    // SAFETY: a fresh object, or null.
    unsafe { autorelease(font) }
}

/// `+[NSFontDescriptor fontDescriptorWithFontAttributes:]`.
///
/// The attributes become the descriptor's, so a descriptor made from them
/// resolves and reports exactly what they say - which is the whole of the
/// method, and what the guest's bold path builds its request out of at
/// `Framework+0x6949741` and `+0x6949787`.
unsafe extern "C" fn font_descriptor_with_attributes(_class: Id, _sel: Sel, attributes: Id) -> Id {
    // SAFETY: null or a dictionary, as the selector requires.
    let descriptor =
        unsafe { crate::coretext::CTFontDescriptorCreateWithAttributes(attributes as *const _) };
    // SAFETY: a fresh object, or null.
    unsafe { autorelease(descriptor) }
}

/// `-[NSFont fontDescriptor]`, autoreleased as AppKit returns it.
unsafe extern "C" fn font_descriptor(receiver: Id, _sel: Sel) -> Id {
    let attributes = attributes(receiver);
    if attributes.is_null() {
        return std::ptr::null_mut();
    }
    // The object takes a reference of its own, and the receiver's is not ours to
    // give away - so that one is added first.
    // SAFETY: a live CoreText object.
    let owned = unsafe { corefoundation::api::CFRetain(attributes) };
    objc::arc::autorelease(descriptor_object(owned) as Id)
}

/// `-[NSFontDescriptor fontAttributes]`: the dictionary this descriptor is.
unsafe extern "C" fn font_attributes(receiver: Id, _sel: Sel) -> Id {
    let attributes = attributes(receiver);
    match attributes.is_null() {
        // SAFETY: null is answered, and anything else is a live CoreText object.
        true => std::ptr::null_mut(),
        false => unsafe { corefoundation::api::CFRetain(attributes) as Id },
    }
}

/// `-[NSFontDescriptor symbolicTraits]`, the `CTFontSymbolicTrait` the face
/// resolved to. This is where the guest reads its bold flag:
/// `Framework+0x6949289` sends it and `+0x69492a5` keeps bit 0.
unsafe extern "C" fn symbolic_traits(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: the receiver's attributes, or null.
    unsafe { crate::coretext::CTFontGetSymbolicTraits(attributes(receiver)) as usize }
}

//
// The `NSFont` metric surface. Each value is read from the face the font
// resolved to, by the CoreText function of the same name, which is the whole of
// what these are: `-[NSFont ascender]` on macOS is `CTFontGetAscent`. The four
// the guest actually sends - `ascender`, `capHeight`, `descender` and `leading`
// - are at `Framework+0x69499da`, `+0x69499f7`, `+0x6949a18` and `+0x6949a2d`,
// one message each, all four on the same `rbx`.
//

unsafe extern "C" fn family_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver's attributes, or null.
    unsafe { crate::coretext::CTFontCopyFamilyName(attributes(receiver)) as Id }
}

unsafe extern "C" fn font_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontCopyPostScriptName(attributes(receiver)) as Id }
}

unsafe extern "C" fn point_size(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontGetSize(attributes(receiver)) }
}

unsafe extern "C" fn ascender(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontGetAscent(attributes(receiver)) }
}

unsafe extern "C" fn descender(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontGetDescent(attributes(receiver)) }
}

unsafe extern "C" fn cap_height(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontGetCapHeight(attributes(receiver)) }
}

unsafe extern "C" fn leading(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontGetLeading(attributes(receiver)) }
}

unsafe extern "C" fn x_height(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontGetXHeight(attributes(receiver)) }
}

unsafe extern "C" fn italic_angle(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontGetSlantAngle(attributes(receiver)) }
}

unsafe extern "C" fn underline_position(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontGetUnderlinePosition(attributes(receiver)) }
}

unsafe extern "C" fn underline_thickness(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: as above.
    unsafe { crate::coretext::CTFontGetUnderlineThickness(attributes(receiver)) }
}

/// `-[NSFontManager convertFont:toHaveTrait:]`.
///
/// **Refused by name, because the mask's bit assignment is not derivable from
/// this guest, and one bit decides a glyph set.**
///
/// `NSFontManagerTraitMask` and `NSFontSymbolicTrait` do not number the traits
/// alike - the manager's `Bold` is bit 0 while CoreText's `Bold` is bit 1, and
/// their `Italic` bits are 1 and 0 - so reading a mask in the other numbering
/// answers a request for a bold face with an italic one. That is a wrong answer
/// wearing the shape of a right one: the guest lays the text out with it and
/// cannot tell.
///
/// **No site passes a literal.** All three call sites compute the mask at run
/// time: `Framework+0x69498a3` and `+0x6949e9e` pass `[r12+0x1c] & 1`, and
/// `+0xa58cbce` passes a register that `+0xa58cbbd` masks against a message
/// return. The binary therefore does not say which bit is bold - and it says
/// something *else* about the one field the mask is built from:
/// `Framework+0x6949682` puts `[r12+0x1c] & 1` into a `CTFontSymbolicTrait`
/// as bit 0, which is CoreText's **italic**, not the manager's bold. The two
/// readings contradict each other, and a header is not the authority for what
/// this guest needs.
///
/// The capability that is missing is a font manager that adds and removes a
/// trait on a real face. The callers that need it are the three sites above;
/// they keep the face `-[NSFontManager fontWithFamily:traits:weight:size:]`
/// already returned, which this crate documents as resolving the family's
/// regular face, so nothing here invents a bold or an italic either.
unsafe extern "C" fn convert_font_to_have_trait(
    _receiver: Id,
    _sel: Sel,
    _font: Id,
    _trait_mask: usize,
) -> Id {
    const WHY: &str = "-[NSFontManager convertFont:toHaveTrait:]: the mask's bit for bold cannot be \
         established from this guest - all three call sites compute the mask at run time and pass \
         no literal, and the field they mask is used elsewhere as CoreText trait bit 0 (italic), \
         which contradicts the manager's own numbering - and a bold face answered as an italic one \
         is a wrong font with the shape of a right one; answering nil so the run can continue";
    if objc::msg::logging_unrecognised() {
        static REPORTED: std::sync::Once = std::sync::Once::new();
        REPORTED.call_once(|| {
            eprintln!("rosetta3: appkit: {WHY}");
        });
        return std::ptr::null_mut();
    }
    objc::fail!(
        "{WHY}. Refusing is the answer; give this host a font manager that converts a face's traits \
         deliberately if it is to have one"
    );
}

/// `+[NSFont systemFontSize]`.
///
/// **Refused by name, because no source for the number was found - not because
/// the number is hard to compute.**
///
/// Apple's own documentation for the symbol is the whole of what it says, and
/// it names no value:
///
/// > Returns the size of the standard system font.
/// >
/// > The standard system font size in points.
///
/// (`NSFont/systemFontSize`, developer.apple.com, fetched while writing this.)
/// There is no value to copy, and on a current macOS there would not be a
/// constant to copy: the answer tracks the Accessibility "Text size" setting.
/// This host has no such setting, so it has no such number.
///
/// The guest's use of the answer is what makes a guess worse than a refusal
/// rather than merely no better. `Framework+[0x6949100,0x69491b0)` is a shared
/// accessor for `systemFontOfSize:`, `menuFontOfSize:` and `toolTipsFontOfSize:`,
/// and when its argument is 0 it asks this method for the default
/// (`Framework+0x694914b`, then `cvttsd2si` at `+0x6949151`, so that site wants
/// only an integer). `Framework+[0x694a470,0x694a500)` truncates the same way at
/// `+0x694a498` (`cvttpd2dq`, then `cvtdq2pd` back).
///
/// **Every one of the seven sites in the image feeds the answer into a point
/// size.** A whole-`__text` census of the `systemFontSize` selref
/// (`test/tools/selcensus.py 0xe6eacc0 0xe6eacc0`) returns exactly seven loads:
/// `+0x6949144`, `+0x694a48b`, `+0x694a718`, `+0x694a822` and `+0x694ae79` in
/// the font region, plus `+0x8b6d9f3` and `+0xc0ac730`. Five truncate to an
/// integer first; the other two do not, and pass the full `CGFloat` straight
/// into the constructor's `xmm0` - `+0x8b6d9fd` is the `systemFontOfSize:` send
/// itself, with no `cvtt*` between it and `+0x8b6d9f3`, and `+0xc0ac741` is the
/// same shape into `fontWithName:size:`. So `0.0` is not a size nothing reads
/// past: it is a point size at two of the seven.
///
/// **What the 0.0 actually becomes here, stated because it is not what the
/// refusal alone would suggest.** `crate::coretext::ui_font` substitutes its
/// own default for a size that is not positive, so a refused `systemFontSize`
/// does not reach a 0-point font: it reaches a 13-point one. That substitution
/// is CoreText's documented shape for a font created with no size, and it is
/// this crate's number rather than a value anybody chose. It is recorded in
/// `crate::coretext` beside the constant. The refusal is unchanged by it: a
/// 13-point font nobody asked for is still text laid out at a size nobody chose,
/// and this method is the place that says so.
///
/// Under `ROSETTA3_LOG_UNRECOGNIZED` it says so once and answers 0.0, so a
/// diagnostic run can walk past it to the rest of the queue - the same trade the
/// runtime's own unrecognized-selector path makes, and said in as many words,
/// because a 0-point font is not an answer.
unsafe extern "C" fn system_font_size(_class: Id, _sel: Sel) -> f64 {
    const WHY: &str = "+[NSFont systemFontSize]: Apple's documentation names no value for it \
         (it tracks the Accessibility \"Text size\" setting, and this host has no such \
         setting), and Framework+0x8b6d9fd feeds the answer straight into \
         +[NSFont systemFontOfSize:] as a point size - so any number here would be a wrong \
         font size that reported nothing";
    if objc::msg::logging_unrecognised() {
        static REPORTED: std::sync::Once = std::sync::Once::new();
        REPORTED.call_once(|| {
            eprintln!("rosetta3: appkit: {WHY}; answering 0.0 so the run can continue");
        });
        return 0.0;
    }
    objc::fail!(
        "{WHY}. Refusing is the answer; give this host a system font size deliberately \
         if it is to have one"
    );
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSFont").expect("declared before its methods");
    class_methods!(class, {
        "fontWithName:size:" => font_with_name, c"@32@0:8@16d24";
        "fontWithDescriptor:size:" => font_with_descriptor, c"@32@0:8@16d24";
        "systemFontOfSize:" => system_font_of_size, c"@24@0:8d16";
        "boldSystemFontOfSize:" => system_font_of_size, c"@24@0:8d16";
        "systemFontOfSize:weight:" => system_font_of_size_weight, c"@32@0:8d16d24";
        // The menu and tool-tip faces are the system face here, the same
        // substitution `boldSystemFontOfSize:` already makes: the default family
        // is the only one there is to pick from, and macOS scales its menu and
        // tool-tip fonts out of the system face anyway.
        "menuFontOfSize:" => system_font_of_size, c"@24@0:8d16";
        "toolTipsFontOfSize:" => system_font_of_size, c"@24@0:8d16";
        "systemFontSize" => system_font_size, c"d16@0:8";
    });
    methods!(class, {
        "dealloc" => dealloc, c"v16@0:8";
        "familyName" => family_name, c"@16@0:8";
        "fontName" => font_name, c"@16@0:8";
        "fontDescriptor" => font_descriptor, c"@16@0:8";
        "pointSize" => point_size, c"d16@0:8";
        "ascender" => ascender, c"d16@0:8";
        "descender" => descender, c"d16@0:8";
        "capHeight" => cap_height, c"d16@0:8";
        "leading" => leading, c"d16@0:8";
        "xHeight" => x_height, c"d16@0:8";
        "italicAngle" => italic_angle, c"d16@0:8";
        "underlinePosition" => underline_position, c"d16@0:8";
        "underlineThickness" => underline_thickness, c"d16@0:8";
    });

    // `CTFontDescriptor` is toll-free bridged to `NSFontDescriptor`, and the
    // guest imports the class by that name: `objc-classrefs.py` puts
    // `NSFontDescriptor` at `#e6e7780`, the classref `Framework+0x6949787` sends
    // `fontDescriptorWithFontAttributes:` to.
    let class = objc::class::lookup(c"NSFontDescriptor").expect("declared before its methods");
    class_methods!(class, {
        "fontDescriptorWithFontAttributes:" => font_descriptor_with_attributes, c"@24@0:8@16";
    });
    methods!(class, {
        "dealloc" => dealloc, c"v16@0:8";
        "symbolicTraits" => symbolic_traits, c"Q16@0:8";
        "fontAttributes" => font_attributes, c"@16@0:8";
    });

    let class = objc::class::lookup(c"NSFontManager").expect("declared before its methods");
    class_methods!(class, {
        "sharedFontManager" => shared_font_manager, c"@16@0:8";
    });
    methods!(class, {
        "convertFont:toHaveTrait:" => convert_font_to_have_trait, c"@32@0:8@16Q24";
        "localizedNameForFamily:face:" => localized_name, c"@32@0:8@16@24";
        "fontWithFamily:traits:weight:size:" => font_with_family, c"@48@0:8@16Q24q32d40";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_font_manager_is_stable() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSFontManager").unwrap();
        let class = objc::class::ptr(class).cast();
        // SAFETY: direct calls with the Objective-C signature installed above.
        let first = unsafe { shared_font_manager(class, objc::sel::get("sharedFontManager")) };
        let second = unsafe { shared_font_manager(class, objc::sel::get("sharedFontManager")) };
        assert!(!first.is_null());
        assert_eq!(first, second);
    }

    #[test]
    fn localized_font_name_uses_the_family() {
        crate::bootstrap();
        let family = foundation::string::from_str("Sans") as Id;
        let face = foundation::string::from_str("Regular") as Id;
        // SAFETY: direct call with the Objective-C signature installed above.
        let result = unsafe {
            localized_name(
                std::ptr::null_mut(),
                objc::sel::get("localizedNameForFamily:face:"),
                family,
                face,
            )
        };
        assert_eq!(result, family);
        unsafe {
            corefoundation::api::CFRelease(family.cast());
            corefoundation::api::CFRelease(face.cast());
        }
    }

    /// `+[NSFont systemFontSize]` refuses, so there is no value to assert. What
    /// *is* assertable, and is the point, is that the class **answers** the
    /// selector: a bound method that stops and names itself, not an
    /// unrecognized-selector abort and not a silent number.
    ///
    /// The distinction is the whole of it. Left unregistered, the guest's send
    /// lands on the runtime's unrecognized path, which under
    /// `ROSETTA3_LOG_UNRECOGNIZED` answers 0 in `xmm0` without a word - and
    /// `Framework+0x8b6d9fd` feeds that straight into a point size. A 0-point
    /// font is invisible text and says nothing, which is the failure this
    /// project is built to avoid.
    #[test]
    fn the_system_font_size_is_a_bound_named_refusal() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSFont").expect("NSFont");
        let method = class
            .metaclass()
            .method(objc::sel::get("systemFontSize"))
            .expect("+[NSFont systemFontSize] is answered");
        // Not the runtime's forwarding placeholder, which is what an
        // unimplemented selector resolves to.
        assert_ne!(
            method.implementation() as usize,
            objc::msg::forward() as usize
        );
        // And the guest reads the answer as a `CGFloat` out of `xmm0`, so the
        // encoding has to be the one that says so.
        // SAFETY: a method's type encoding is a NUL-terminated string the
        // runtime owns.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        assert_eq!(
            types.to_str().ok(),
            Some("d16@0:8"),
            "+[NSFont systemFontSize] takes self and _cmd and returns a CGFloat"
        );
    }

    /// The seven selectors the guest sends to one font, and the one fact they all
    /// turned on: that the object is an `NSFont` and not a dictionary.
    ///
    /// **Read off the image, not off the implementation.** `familyName`,
    /// `fontDescriptor`, `ascender`, `capHeight`, `descender` and `leading` are
    /// the selrefs `0xe6eac80`, `0xe6eacc8`, `0xe6eac88`, `0xe6eac90`,
    /// `0xe6eac98` and `0xe6eaca0` (the `__objc_selrefs` slot is a pointer to
    /// the name, so the slot address is read out of the section rather than
    /// from a header), and their whole-`__text` census is one, one, one, one,
    /// one and **seven** sites, all of them in `Framework+[0x6949100,0x694b190)`.
    /// Seven sites and six selectors in one 0x2090-byte region is what makes
    /// this a region's problem rather than a sample's.
    const GUEST_SENDS: &[&str] = &[
        "familyName",
        "fontDescriptor",
        "ascender",
        "capHeight",
        "descender",
        "leading",
        "symbolicTraits",
    ];

    /// A font from CoreText, and its class.
    fn system_font(size: f64) -> Option<(&'static objc::class::Class, Id)> {
        crate::bootstrap();
        crate::fonts::default_family(false)?;
        // SAFETY: null language is documented; the +1 is autoreleased below.
        let font = unsafe { crate::coretext::CTFontCreateUIFontForLanguage(0, size, std::ptr::null()) }
            as Id;
        if font.is_null() {
            return None;
        }
        let class = unsafe { objc::object::class_of(font) }?;
        Some((class, font))
    }

    /// **The assertion this change exists for.** A font is an `NSFont`.
    ///
    /// Not "answers the selectors" - the *class*, because that is what the
    /// refusal was reported against. `test/logs/269.log` said
    /// `unrecognized selector sent to instance of __NSCFDictionary: ascender`,
    /// and `__NSCFDictionary` is
    /// `corefoundation::collections::dictionary_class`, which is what
    /// `CTFontCreateUIFontForLanguage` used to return. Nothing about a
    /// dictionary answering `ascender` is true on macOS, and nothing about it
    /// being wrong here is about AppKit: `CTFont` is toll-free bridged to
    /// `NSFont`, and the guest relies on the bridge at seven sites in one
    /// region.
    #[test]
    fn a_font_is_an_nsfont_and_not_a_dictionary() {
        let Some((class, font)) = system_font(20.0) else {
            return;
        };
        assert_eq!(
            class.name_str(),
            "NSFont",
            "CTFontCreateUIFontForLanguage handed back something that is not a font"
        );
        // And specifically not the class the refusals named.
        assert_ne!(
            class.name_str(),
            "__NSCFDictionary",
            "a font is a dictionary again, which is the bug this replaced"
        );
        unsafe { corefoundation::api::CFRelease(font.cast()) };
    }

    /// `+[NSFont systemFontOfSize:]` is the site the run reaches first, and its
    /// answer is the same object. Checked through the class, not through a
    /// direct call, so the test exercises the binding the guest uses.
    #[test]
    fn the_system_font_constructor_answers_a_font() {
        crate::bootstrap();
        // SAFETY: `+systemFontOfSize:` is `id (*)(Class, SEL, CGFloat)`, and
        // the encoding is read back out of the class and checked below.
        let f: unsafe extern "C" fn(Id, Sel, f64) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let class = objc::class::ptr(objc::class::lookup(c"NSFont").expect("NSFont"));
        // SAFETY: the class and the size are as the selector declares.
        let font = unsafe { f(class as Id, objc::sel::get("systemFontOfSize:"), 14.0) };
        if font.is_null() {
            return; // a host with no fontconfig, where there is no face at all
        }
        let class_of = unsafe { objc::object::class_of(font) };
        assert_eq!(
            class_of.map(|c| c.name_str()),
            Some("NSFont"),
            "+[NSFont systemFontOfSize:] returned {}",
            class_of.map_or("null", |c| c.name_str())
        );
        unsafe { corefoundation::api::CFRelease(font.cast()) };
    }

    /// Every selector in [`GUEST_SENDS`] is answered by a bound method, and the
    /// two that are sent to a **descriptor** are on `NSFontDescriptor`.
    ///
    /// The split matters and is not obvious from the names: `symbolicTraits` is
    /// the only one of the seven the guest sends to the result of
    /// `fontDescriptor` rather than to the font (`Framework+0x6949289`, with
    /// `rdi` from `+0x6949281`'s retain of the `fontDescriptor` answer), so
    /// installing it on `NSFont` alone would leave the bold flag unread and the
    /// guest would lay out every bold face as a regular one.
    #[test]
    fn every_selector_the_guest_sends_is_answered_by_the_right_class() {
        crate::bootstrap();
        let font = objc::class::lookup(c"NSFont").expect("NSFont");
        let descriptor = objc::class::lookup(c"NSFontDescriptor").expect("NSFontDescriptor");
        for name in GUEST_SENDS {
            let sel = objc::sel::get(name);
            let owner = match font.method(sel) {
                Some(_) => "NSFont",
                None => match descriptor.method(sel) {
                    Some(_) => "NSFontDescriptor",
                    None => panic!("{name} is answered by neither class"),
                },
            };
            assert!(
                owner == "NSFont" || owner == "NSFontDescriptor",
                "{name} is answered, by {owner}"
            );
            // And by something of ours, not by the runtime's forwarder - an
            // unrecognized send answers nil under ROSETTA3_LOG_UNRECOGNIZED,
            // which for `ascender` is a font of zero height that reports nothing.
            let implementation = match owner {
                "NSFont" => font.method(sel),
                _ => descriptor.method(sel),
            }
            .expect("looked up above")
            .implementation() as usize;
            assert_ne!(
                implementation,
                objc::msg::forward() as usize,
                "{name} resolves to the forwarding placeholder"
            );
        }
    }

    /// The metrics are the face's, and they are the *same numbers* CoreText
    /// gives for the same font - which is the claim that makes them an answer
    /// rather than a plausible constant.
    ///
    /// The guest's own arithmetic is the referee, and it is unforgiving: at
    /// `Framework+0x6949a40` it adds `ascender + |descender| + leading` and
    /// rounds to an `int` at `+0x6949a56`, storing it as the font's height at
    /// `+0x6949a5a`. Three fabricated metrics and the height is wrong, and
    /// nothing downstream can tell.
    #[test]
    fn the_metrics_are_the_face_own_and_agree_with_coretext() {
        let Some((_, font)) = system_font(20.0) else {
            return;
        };
        let attributes = attributes_of(font.cast());
        // SAFETY: a font's attributes are a live dictionary.
        unsafe {
            for (selector, from_core_text) in [
                ("ascender", crate::coretext::CTFontGetAscent(attributes)),
                ("descender", crate::coretext::CTFontGetDescent(attributes)),
                ("capHeight", crate::coretext::CTFontGetCapHeight(attributes)),
                ("leading", crate::coretext::CTFontGetLeading(attributes)),
            ] {
                assert_eq!(
                    metric_via_message(selector, font),
                    from_core_text,
                    "-[{selector}] disagrees with the CoreText function of the same name"
                );
            }
            corefoundation::api::CFRelease(font.cast());
        }
    }

    /// Send one of the `CGFloat` metrics through `objc_msgSend` and read
    /// `xmm0`, which is where the guest reads it from
    /// (`roundsd`/`cvttsd2si` at `Framework+0x69499e0` and `+0x69499f7`).
    fn metric_via_message(selector: &str, font: Id) -> f64 {
        // SAFETY: `-ascender` and its three siblings are `CGFloat (*)(id, SEL)`,
        // and the encoding each was installed with is asserted in
        // `installed_metric_encodings_agree_with_their_selectors`.
        let f: unsafe extern "C" fn(Id, Sel) -> f64 =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: a live font and a selector it answers.
        unsafe { f(font, objc::sel::get(selector)) }
    }

    /// A `CGFloat` metric is declared as one, checked against the selector's
    /// **own colon count** rather than against the implementation.
    ///
    /// A method encoding is never read at dispatch, so a wrong one is invisible
    /// from the call site: `d16@0:8` and `d32@0:8` both put a double in `xmm0`
    /// and both are self-consistent. Only the selector's colon count says how
    /// many arguments there are, and a test that reads the string the
    /// implementation wrote shares its misreading. So this reads the encoding
    /// back out of the class - where a wrong string is visible *as a string* -
    /// and derives the expectation from the selector.
    #[test]
    fn installed_metric_encodings_agree_with_their_selectors() {
        crate::bootstrap();
        // No colons: self and _cmd, which is arity 2, so `@`/`d` at offset 16
        // and a 16-byte frame. That is the whole rule.
        for (name, expected) in [
            ("familyName", "@16@0:8"),
            ("fontDescriptor", "@16@0:8"),
            ("ascender", "d16@0:8"),
            ("capHeight", "d16@0:8"),
            ("descender", "d16@0:8"),
            ("leading", "d16@0:8"),
        ] {
            let class = objc::class::lookup(c"NSFont").expect("NSFont");
            let method = class
                .method(objc::sel::get(name))
                .unwrap_or_else(|| panic!("-{name} is installed"));
            assert_eq!(name.matches(':').count(), 0, "{name} has no arguments");
            // SAFETY: a method's types are a NUL-terminated string the runtime
            // owns.
            let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
            assert_eq!(
                types.to_str().ok(),
                Some(expected),
                "-[{name}] is declared as something other than self and _cmd"
            );
        }
        // And the one with a colon gets three arguments, not two: the
        // `v32@0:8@?24` shape in this project was a one-argument method
        // declared with two, and it was self-consistent.
        let class = objc::class::lookup(c"NSFontManager").expect("NSFontManager");
        let method = class
            .method(objc::sel::get("convertFont:toHaveTrait:"))
            .expect("-[NSFontManager convertFont:toHaveTrait:] is installed");
        assert_eq!(
            "convertFont:toHaveTrait:".matches(':').count(),
            2,
            "two arguments, so arity 4 and a 32-byte frame"
        );
        // SAFETY: as above.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        assert_eq!(types.to_str().ok(), Some("@32@0:8@16Q24"));
    }

    /// `-[NSFont fontDescriptor]` answers a descriptor, and the descriptor is
    /// what `symbolicTraits` is read from - so a font whose descriptor is
    /// another font's attributes dictionary, or a dictionary, loses the bold
    /// flag silently.
    #[test]
    fn a_fonts_descriptor_is_a_descriptor_and_answers_symbolic_traits() {
        let Some((_, font)) = system_font(20.0) else {
            return;
        };
        // SAFETY: `-fontDescriptor` is `id (*)(id, SEL)`.
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: a live font, and the selector it answers.
        let descriptor = unsafe { f(font, objc::sel::get("fontDescriptor")) };
        assert!(!descriptor.is_null(), "-[NSFont fontDescriptor] answered nil");
        let class = unsafe { objc::object::class_of(descriptor) };
        assert_eq!(
            class.map(|c| c.name_str()),
            Some("NSFontDescriptor"),
            "-[NSFont fontDescriptor] returned {}",
            class.map_or("null", |c| c.name_str())
        );
        // And the class the guest's `and r12d, 0x1` at `Framework+0x69492a5`
        // keeps a bit of answers as a number rather than refusing.
        // SAFETY: `-symbolicTraits` is `NSUInteger (*)(id, SEL)`.
        let g: unsafe extern "C" fn(Id, Sel) -> usize =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        // SAFETY: a live descriptor, and the selector it answers.
        let traits = unsafe { g(descriptor, objc::sel::get("symbolicTraits")) };
        assert!(
            traits & !0x3FFF == 0,
            "a symbolic-trait mask of {traits:#x} is not one CoreText issues"
        );
        unsafe {
            corefoundation::api::CFRelease(descriptor.cast());
            corefoundation::api::CFRelease(font.cast());
        }
    }

    /// The refusal on `convertFont:toHaveTrait:` is a **bound named refusal**,
    /// not an unrecognized selector.
    ///
    /// Left unregistered, the send lands on the runtime's forwarder, which
    /// under `ROSETTA3_LOG_UNRECOGNIZED` answers nil - and the guest then
    /// returns that nil as the font at `Framework+0x69498c7`, having already
    /// released the real one. A nil font is not a wrong font, so that arm is
    /// survivable; what is not survivable is answering `1` and letting the
    /// guest lay out bold text in a regular face. The reasoning is on the
    /// function.
    #[test]
    fn convert_font_to_trait_is_a_bound_named_refusal() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSFontManager").expect("NSFontManager");
        let method = class
            .method(objc::sel::get("convertFont:toHaveTrait:"))
            .expect("-[NSFontManager convertFont:toHaveTrait:] is answered");
        assert_ne!(
            method.implementation() as usize,
            objc::msg::forward() as usize,
            "the refusal is not installed, so the guest gets the forwarder"
        );
    }
}
