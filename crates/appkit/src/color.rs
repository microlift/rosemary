use std::ffi::c_void;

use foundation::geometry::CGFloat;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Color {
    isa: *const c_void,
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
}

pub const INSTANCE_SIZE: usize = size_of::<Color>();

/// The components of `color`, or opaque black for anything that is not one.
///
/// # Safety
///
/// `color` must be null or an object this runtime allocated.
pub unsafe fn components(color: Id) -> (f64, f64, f64, f64) {
    let is_color = unsafe { objc::object::class_of(color) }.is_some_and(|c| {
        objc::class::lookup(c"NSColor").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    });
    if !is_color {
        return (0.0, 0.0, 0.0, 1.0);
    }
    // SAFETY: the check above says it is one of this crate's colours.
    let c = unsafe { &*(color as *const Color) };
    (c.red, c.green, c.blue, c.alpha)
}

/// A colour object holding these components, owned by nobody: the caller gets it
/// back autoreleased, as every `+colorWith...` constructor's result is.
fn make(red: f64, green: f64, blue: f64, alpha: f64) -> Id {
    objc::arc::autorelease(allocate(red, green, blue, alpha))
}

/// A colour object with a reference this function keeps.
fn allocate(red: f64, green: f64, blue: f64, alpha: f64) -> Id {
    let class = objc::class::lookup(c"NSColor").expect("declared before its methods");
    // SAFETY: `Color` is the layout the class was registered with.
    let color = unsafe { corefoundation::base::allocate::<Color>(class) };
    // SAFETY: freshly allocated; every field is written before it is read.
    unsafe {
        (*color).red = red;
        (*color).green = green;
        (*color).blue = blue;
        (*color).alpha = alpha;
    }
    color as Id
}

/// The object for one named colour, made once and kept for the process.
///
/// **Not autoreleased, and that is the whole point.** `+controlTextColor` on a Mac
/// answers the same object every time and that object lives for ever, so a caller
/// may put one straight into a `static` - which is exactly what a widget toolkit
/// does: Qt's Cocoa theme fills a file-scope table of role colours from these in a
/// module initializer and reads it back whenever it builds a palette. An
/// autoreleased colour there is freed when the pool drains and the table is left
/// holding dangling pointers, which surfaces much later as a message sent to freed
/// memory in code that has nothing to do with colours.
///
/// Interned by name rather than per call for the same reason identity matters
/// elsewhere in this tree: a caller comparing two `+textColor`s expects one object.
fn singleton(name: &str, red: f64, green: f64, blue: f64, alpha: f64) -> Id {
    static COLORS: std::sync::Mutex<Vec<(String, usize)>> = std::sync::Mutex::new(Vec::new());
    let mut colors = COLORS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = colors.iter().find(|(n, _)| n == name) {
        return *object as Id;
    }
    let color = allocate(red, green, blue, alpha);
    // Retained forever: a named colour a guest can release is one it can make
    // dangle, and these are handed out unowned.
    objc::arc::retain(color);
    colors.push((name.to_string(), color as usize));
    color
}

/// The value of `+alternatingContentBackgroundColor`: a light grey.
///
/// **An invented constant, named here rather than inlined, and it is standing in
/// for a shade this host cannot measure.** It is not derived from an appearance,
/// a `.colorset`, a preference store or a theme file: there is no such thing on
/// this host, and [`refuse_no_catalog`] gives the census. Every other number in
/// [`NAMED`] that is not a device colour is in the same position, and this one is
/// singled out because the **plural** selector
/// (`+[NSColor alternatingContentBackgroundColors]`, see
/// [`alternating_content_background_colors`]) returns a *pair* whose second
/// member this constant is, and because
/// [`alternating_row_colors`](alternating_row_colors()) builds its pair out of it
/// as well. A constant nobody chose is a wrong answer wearing the shape of a
/// right one, so it has a name and a reason attached rather than a bare `0.96`.
///
/// The same objection is on record against `UI_DEFAULT_SIZE = 13.0` in
/// `coretext` and against the other entries in `MEMORY.md`'s list. **Ruling
/// whether each stays is a decision this file does not make for itself**; what
/// it does do is stop any of them from being inherited silently.
const ALTERNATING_CONTENT_BACKGROUND_GREY: f64 = 0.96;

/// The named colours, as `(selector, r, g, b, a)`.
///
/// The first group is the device colours, whose values are their names. The
/// second is the semantic ones - `+textColor` and its relatives - whose value
/// depends on the appearance, and which are answered with the light appearance's
/// because this runtime has one appearance and it is that one. A program that
/// draws with `+textColor` on a dark background gets black text, which is what
/// it would get from AppKit in light mode; a program that switched appearance
/// would not be followed, and there is no appearance here to switch.
///
/// **Two of these are off every path this guest can take, measured, and one of
/// them is a spelling miss that matters.** Of the 56 selectors below, 13 have an
/// `__objc_selrefs` entry and 43 do not - four because the name is in
/// `__TEXT,__objc_methname` with no selref pointing at it, and 39 because the
/// name does not occur in the guest image at all (positive controls on the same
/// two-sided scan: `controlTextColor`, `selectedTextBackgroundColor`,
/// `keyboardFocusIndicatorColor`, `disabledControlTextColor`,
/// `unemphasizedSelectedContentBackgroundColor`, `deviceRGBColorSpace` and
/// `colorUsingColorSpace:` are each exactly 1 occurrence, and
/// `underPageBackgroundColor` and `selectedContentBackgroundColor` are each 0).
/// Among the 39 are **`alternatingContentBackgroundColor`** - a selector this
/// tree answers, which **the guest does not contain**: the byte scan for it
/// NUL-terminated returns 0, and the 1 hit for the unterminated spelling is the
/// plural's own prefix. The guest sends the **plural**, which is why
/// [`alternating_content_background_colors`] exists and why the singular's
/// [`ALTERNATING_CONTENT_BACKGROUND_GREY`] is inherited by nothing. Also among
/// the 39 is `controlBackgroundColor`, the other half of the fabricated pair in
/// [`alternating_row_colors`](alternating_row_colors()).
static NAMED: &[(&str, f64, f64, f64, f64)] = &[
    ("textColor", 0.0, 0.0, 0.0, 1.0),
    ("labelColor", 0.0, 0.0, 0.0, 1.0),
    ("windowBackgroundColor", 0.93, 0.93, 0.93, 1.0),
    ("controlColor", 0.93, 0.93, 0.93, 1.0),
    ("controlTextColor", 0.0, 0.0, 0.0, 1.0),
    ("controlBackgroundColor", 1.0, 1.0, 1.0, 1.0),
    ("textBackgroundColor", 1.0, 1.0, 1.0, 1.0),
    ("selectedTextColor", 0.0, 0.0, 0.0, 1.0),
    ("windowFrameTextColor", 0.0, 0.0, 0.0, 1.0),
    ("disabledControlTextColor", 0.53, 0.53, 0.53, 1.0),
    // The selection and header colours a widget toolkit builds a palette out of.
    // Qt asks for all of these while its Cocoa plugin is still loading, and one
    // it cannot get is not a wrong shade - it is an unrecognised selector and a
    // program that stops. The accent-dependent ones are the blue accent's, which
    // is the default and the only one this runtime has: there is no
    // `NSColorAppearance` here to read another from.
    ("headerTextColor", 0.0, 0.0, 0.0, 1.0),
    ("headerColor", 0.93, 0.93, 0.93, 1.0),
    ("selectedControlColor", 0.70, 0.84, 1.0, 1.0),
    ("selectedControlTextColor", 0.0, 0.0, 0.0, 1.0),
    ("selectedTextBackgroundColor", 0.70, 0.84, 1.0, 1.0),
    ("alternateSelectedControlColor", 0.0, 0.39, 0.88, 1.0),
    ("alternateSelectedControlTextColor", 1.0, 1.0, 1.0, 1.0),
    ("selectedContentBackgroundColor", 0.0, 0.39, 0.88, 1.0),
    ("selectedMenuItemTextColor", 1.0, 1.0, 1.0, 1.0),
    // "Unemphasized" is the selection in a view that is not the first responder:
    // grey rather than accented, with the ordinary text colour on top.
    (
        "unemphasizedSelectedContentBackgroundColor",
        0.86,
        0.86,
        0.86,
        1.0,
    ),
    (
        "unemphasizedSelectedTextBackgroundColor",
        0.86,
        0.86,
        0.86,
        1.0,
    ),
    ("unemphasizedSelectedTextColor", 0.0, 0.0, 0.0, 1.0),
    // The focus ring is drawn over what it surrounds, so its alpha is part of
    // the colour rather than something the caller applies.
    ("keyboardFocusIndicatorColor", 0.0, 0.37, 0.89, 0.25),
    ("linkColor", 0.0, 0.41, 0.85, 1.0),
    ("shadowColor", 0.0, 0.0, 0.0, 1.0),
    ("highlightColor", 1.0, 1.0, 1.0, 1.0),
    ("gridColor", 0.90, 0.90, 0.90, 1.0),
    // The label tiers and the separator differ from `labelColor` only in alpha,
    // which is how AppKit defines them.
    ("secondaryLabelColor", 0.0, 0.0, 0.0, 0.5),
    ("tertiaryLabelColor", 0.0, 0.0, 0.0, 0.26),
    ("quaternaryLabelColor", 0.0, 0.0, 0.0, 0.1),
    ("placeholderTextColor", 0.0, 0.0, 0.0, 0.25),
    ("separatorColor", 0.0, 0.0, 0.0, 0.1),
    ("underPageBackgroundColor", 0.60, 0.60, 0.60, 1.0),
    (
        "alternatingContentBackgroundColor",
        ALTERNATING_CONTENT_BACKGROUND_GREY,
        ALTERNATING_CONTENT_BACKGROUND_GREY,
        ALTERNATING_CONTENT_BACKGROUND_GREY,
        1.0,
    ),
    // The `systemX` palette, whose values are Apple's published sRGB ones. A
    // program picks one to draw with rather than asking the appearance about it.
    ("systemRedColor", 1.0, 0.23, 0.19, 1.0),
    ("systemGreenColor", 0.16, 0.80, 0.25, 1.0),
    ("systemBlueColor", 0.0, 0.48, 1.0, 1.0),
    ("systemOrangeColor", 1.0, 0.58, 0.0, 1.0),
    ("systemYellowColor", 1.0, 0.80, 0.0, 1.0),
    ("systemPinkColor", 1.0, 0.18, 0.33, 1.0),
    ("systemPurpleColor", 0.69, 0.32, 0.87, 1.0),
    ("systemTealColor", 0.35, 0.78, 0.98, 1.0),
    ("systemIndigoColor", 0.35, 0.34, 0.84, 1.0),
    ("systemBrownColor", 0.64, 0.52, 0.37, 1.0),
    ("systemGrayColor", 0.56, 0.56, 0.58, 1.0),
    ("controlAccentColor", 0.0, 0.39, 0.88, 1.0),
    ("blackColor", 0.0, 0.0, 0.0, 1.0),
    ("whiteColor", 1.0, 1.0, 1.0, 1.0),
    ("clearColor", 0.0, 0.0, 0.0, 0.0),
    ("redColor", 1.0, 0.0, 0.0, 1.0),
    ("greenColor", 0.0, 1.0, 0.0, 1.0),
    ("blueColor", 0.0, 0.0, 1.0, 1.0),
    ("yellowColor", 1.0, 1.0, 0.0, 1.0),
    ("cyanColor", 0.0, 1.0, 1.0, 1.0),
    ("magentaColor", 1.0, 0.0, 1.0, 1.0),
    ("grayColor", 0.5, 0.5, 0.5, 1.0),
    ("lightGrayColor", 2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 1.0),
    ("darkGrayColor", 1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 1.0),
];

/// One `+namedColor`, resolved by the selector it was sent.
///
/// A single implementation for the whole table rather than twelve: the selector
/// is an argument of every method, so the name it was reached by is the lookup
/// key, and twelve near-identical functions would be twelve places to get a
/// component wrong.
unsafe extern "C" fn named(_class: Id, sel: Sel) -> Id {
    // SAFETY: a registered selector is a NUL-terminated interned name.
    let name = unsafe { std::ffi::CStr::from_ptr(sel) };
    let name = name.to_string_lossy();
    match NAMED.iter().find(|(n, ..)| *n == name) {
        Some((_, r, g, b, a)) => singleton(&name, *r, *g, *b, *a),
        // Unreachable through dispatch - the selector was installed from this
        // very table - and named rather than silent in case it stops being.
        None => objc::fail!("+[NSColor {name}]: not one of this runtime's named colours"),
    }
}

/// The one site in the image that sends this selector, and the pair it sends.
///
/// Named because the refusal below has to be able to say *which* lookup it is
/// declining, and the answer is one pair for the whole 214 MB image: the
/// selector's text occurs **once** in the Framework, its `__objc_selrefs` word is
/// at `+0xe6e9438`, and a disp32 scan of all of `__TEXT,__text` for that address
/// returns **one** site, `+0x3ab57c0` — against a control address
/// (`+0xe6e9460`, `appearanceNamed:`) that returns six. The two arguments are
/// `__cfstring` constants loaded into `rdx` and `rcx` at `+0x3ab57fb`:
/// `+0xe672d50` and `+0xe672d70`.
const CATALOG_SITE: &str = "Chromium Framework+0x3ab57fb";

/// `+[NSColor colorWithCatalogName:colorName:]` — a **named colour out of a named
/// asset catalog** — answered `nil`, and said out loud.
///
/// # What the method is, and why there is nothing to answer it from
///
/// It resolves a colour *asset*: AppKit opens the asset catalog called
/// `catalogName`, finds the `.colorset` called `colorName` inside it, and reads
/// the colour out of that. Both halves of that are absent here, and both were
/// measured rather than assumed:
///
/// * **The bundle has no asset catalog of any kind.** Over all 214 files of
///   `test/Chromium/`, `find` returns **0** for `*.colorset`, **0** for
///   `*.xcassets`, **0** for `Contents.json` and **0** for `*.car`. The only
///   thing under `Chromium.app/Contents/Resources` is a set of `.lproj`
///   localisations.
/// * **The guest has no colour-management library that could hold one.** The
///   Framework's own list of 60 dependent libraries contains no ColorSync, and
///   across the 8828 dyld bind records of the 11 images whose bind stream
///   decodes, **no** symbol name contains `ColorSync` or `NamedColor`.
///   (The `Chromium` launcher's stream is the one image this project's decoder
///   skips, and it is the launcher, not the Framework.)
///
/// # Why `nil` rather than a colour, and not a refusal that aborts
///
/// `nil` here is not a stub and not a stand-in. It says **"this host has no
/// colour by that name"**, and on this host that is a fact. A substitute colour
/// would be the wrong answer that looks like success: the caller stores the
/// result into the table it shares with every renderer, nothing downstream can
/// tell a fabricated blue from a real one, and the lie is invisible forever.
///
/// The pair the guest actually asks for is **`System` /
/// `controlAccentBlueColor`** — and that matters for what the refusal has to
/// say. `System` is **AppKit's own** catalog, and `controlAccentBlueColor` is a
/// colour asset in it, so this is a **macOS platform colour**, not one of
/// Chromium's that we failed to find. The gap is the platform's, and no amount
/// of searching the bundle would close it.
///
/// # The guest does not merely survive the `nil`; it has a branch *for* it
///
/// The single call site is `content::ThemeHelperMac::LoadSystemColorsForCurrentAppearance`
/// (`{CATALOG_SITE}`), reached from `LoadSystemColors` through
/// `-[NSAppearance performAsCurrentDrawingAppearance:]` — which is why run 137's
/// fault line names `+0x3ab5286`, the *outer* frame, and not this one. The
/// result is tested immediately:
///
/// ```text
///  3ab581e:  mov    eax,0xff000000        ; SK_ColorBLACK, the else-branch's value
///  3ab5826:  test   r12,r12
///  3ab5829:  je     0x3ab5867             ; `if (color)` — the source's `if (color)`
///  3ab582b:  mov    rdi,r12
///  3ab582e:  call   0x3ab58b0             ; NSColorToSkColor, reached only if non-nil
/// ```
///
/// so `nil` skips the conversion helper, skips the message to the result, and
/// stores `0xff000000` — the constant `mov`ed into `eax` two instructions
/// earlier, and the guest's own comment for that path. Every iteration of the
/// same five-way switch takes the *other* four selectors, and those message
/// their result unconditionally, so this one is the only entry where `nil` is a
/// designed outcome.
///
/// The consequence is stated rather than hidden: the accent-blue entry of the
/// macOS system-colour table is black on this host, where on macOS it is blue.
/// The guest cannot tell, which is why [`refuse_no_catalog`] prints.
///
/// # The encoding, and why it is two arguments
///
/// `c"@32@0:8@16@24"`. Two `:` in the selector, two arguments; the frame is
/// `last_arg_offset + 8 = 24 + 8 = 32`; and the call site writes the catalog
/// into `rdx` and the colour into `rcx`, which is the register pair two object
/// arguments occupy. The guest declares no method for this selector, so its own
/// encoding string is not available to check against — the *call site* is, and
/// `v32@0:8@16@24` is the return-void twin of this exact frame and offset pair
/// in the image's own encoding strings.
///
/// # Safety
///
/// `catalog` and `name` are `NSString`s or nil.
unsafe extern "C" fn color_with_catalog_name(
    _class: Id,
    _sel: Sel,
    catalog: Id,
    name: Id,
) -> Id {
    refuse_no_catalog(catalog, name);
    std::ptr::null_mut()
}

/// Say, once per pair, that there is no catalog here to resolve the pair
/// against.
///
/// Once per *pair* rather than once per call or once per run, for the reason
/// `foundation`'s `refuse_missing_class` gives: the guest may ask for more than
/// one, and the point of the line is that it is a census of what was asked for
/// and not answered, not a single event.
fn refuse_no_catalog(catalog: Id, name: Id) {
    // SAFETY: the selector declares two `NSString`s, and nil reads as an empty
    // string rather than faulting.
    let (catalog, name) = unsafe {
        (
            String::from_utf8(foundation::string::bytes(catalog)).unwrap_or_default(),
            String::from_utf8(foundation::string::bytes(name)).unwrap_or_default(),
        )
    };
    // A `Vec` and not a set, as in `refuse_missing_class`: the census is a
    // handful of pairs, so a linear scan of owned strings is cheaper than
    // hashing and keeps the one-shot set in a static with no lazy initialiser.
    static SAID: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let pair = (catalog.clone(), name.clone());
    if said.contains(&pair) {
        return;
    }
    said.push(pair);
    eprintln!(
        "rosetta3: appkit: +[NSColor colorWithCatalogName:\"{catalog}\" colorName:\"{name}\"]: \
         answered nil. A named-colour lookup resolves a .colorset out of a named asset catalog, \
         and this host has none to resolve against: 0 *.colorset, 0 *.xcassets and 0 \
         Contents.json in the whole 214-file Chromium bundle, no ColorSync among the Framework's \
         60 dependent libraries, and no ColorSync or NamedColor symbol among the 8828 dyld bind \
         records of the 11 images whose bind stream decodes. On macOS this pair names a real \
         colour - controlAccentBlueColor is an asset in AppKit's own System catalog - so this is \
         the platform's missing capability, not a name the guest got wrong and not a Chromium \
         asset we failed to find. The only caller in the image is \
         content::ThemeHelperMac::LoadSystemColorsForCurrentAppearance at {CATALOG_SITE}, and it \
         branches on the answer (0x3ab5826 `test r12,r12; je 0x3ab5867`, the source's \
         `if (color)`), so the run carries on and the taken path stores SK_ColorBLACK, the \
         0xff000000 loaded into eax at 0x3ab581e - the guest's own \"if it isn't available just \
         set a black value\". The accent-blue entry of the macOS system-colour table this host \
         shares with every renderer is therefore black here, and the guest cannot tell."
    );
}

/// `+[NSColor alternatingContentBackgroundColors]` — a **plural** selector
/// returning an `NSArray` of **two** colours — answered `nil`, and said out loud.
///
/// # The decision, on both halves
///
/// **Half one — is there a truthful pair on this host? No, and the reason is
/// categorical rather than a matter of not having looked.** This is *not* one of
/// the `-[NSWorkspace accessibilityDisplayShould…]` family that
/// `workspace.rs` answers NO to. Those four report *whether the user asked for a
/// display option*, and the answer is a fact about a user account: nobody here has
/// asked, so `false` is true, and that is why refusing-by-answering works for
/// them. This selector has no such boolean. It reports *what colour the current
/// appearance uses for alternating content* — a system colour, in the same family
/// as the other 56 entries in [`NAMED`], every one of which is a shade this host
/// cannot measure. There is no true value to return and no true value to refuse
/// *with*; the only way to answer it at all is to invent two colours.
///
/// The census behind that, which is a measurement rather than an argument:
///
/// * **No appearance and no system colours.** No asset catalog of any kind: over
///   all 214 files of `test/Chromium/`, 0 `*.colorset`, 0 `*.xcassets`, 0
///   `Contents.json`, 0 `*.car`, and the only thing under
///   `Chromium.app/Contents/Resources` is a set of `.lproj` localisations. No
///   ColorSync among the Framework's 60 dependent libraries, and no symbol
///   containing `ColorSync` or `NamedColor` among the 8828 dyld bind records of
///   the 11 images whose bind stream decodes. That is
///   [`refuse_no_catalog`]'s census, and it is the same census, because it is
///   the same missing thing: an AppKit system colour.
/// * **Nothing on this host that could stand in for it, and a substitution from
///   elsewhere would be a *different* wrong answer.** This is a desktop Linux
///   with an XDG freedesktop theme; answering a macOS platform colour from a
///   Linux theme would hand the guest a colour from a platform it is not on,
///   which is less honest than `nil`, not more.
///
/// **Half two — if it must answer something, what? `nil`, and the reason `nil`
/// beats an empty array is a fact about the guest rather than a preference.**
///
/// An empty `NSArray` is *not* a value this method can have on macOS, so it is a
/// fabrication like any other — and it is a fabrication with a worse consequence
/// than the other one, which is measurable. See the call-site section below: the
/// one caller subscripts the answer at index **1** with no bounds check and no
/// nil test, so an empty array raises `NSRangeException` *inside AppKit emulation*
/// while `nil` is a message to nil, which yields nil, which is what reaches the
/// guest's own `CHECK`. **`nil` is the only one of the three answers that is
/// both truthful about the absence and survivable by the one caller in the image.**
/// The two-element pair is the third option and is rejected: it is the pair
/// [`alternating_row_colors`](alternating_row_colors()) already fabricates, and
/// handing it out again here is precisely the quiet inheritance the invented
/// [`ALTERNATING_CONTENT_BACKGROUND_GREY`] is not allowed to have.
///
/// # The one call site, enumerated rather than sampled
///
/// The whole enclosing function, `Framework+0x8142aa0 .. 0x814375a` — 3258 bytes,
/// 790 instructions, **163 control transfers enumerated and classified from their
/// own bytes** (70 `call`, 56 `je`, 34 `jne`, 2 `jle`, 1 `ret`, no `ud2` or
/// `int3` inside the function) — loads exactly **six** selectors:
///
/// | address | selector | then |
/// |---|---|---|
/// | `+0x8142bea` | `disabledControlTextColor` | straight to `+0x61ac660` |
/// | `+0x8142d1f` | `controlTextColor` | straight to `+0x61ac660` |
/// | **`+0x8142e4f`** | **`alternatingContentBackgroundColors`** | **`objectAtIndexedSubscript: 1`, then `+0x61ac660`** |
/// | `+0x8142e6b` | `objectAtIndexedSubscript:` | belongs to the row above |
/// | `+0x8142fbd` | `keyboardFocusIndicatorColor` | straight to `+0x61ac660` |
/// | `+0x81431ee` | `selectedTextBackgroundColor` | straight to `+0x61ac660` |
///
/// Four of the five colour selectors loaded here are the *singular* named
/// colours, handed to the Skia colour accessor `+0x61ac660` unchanged; the fifth
/// load is `objectAtIndexedSubscript:`, and it belongs to this one selector and
/// no other. **The plural is the only one of the six that is subscripted:**
///
/// ```text
///  8142e48  mov  rdi,[rip+…]        # 0xe6e72d0 = NSColor, a classref
///  8142e4f  mov  rsi,[rip+…]        # 0xe6eba38 = the selector
///  8142e5d  call r14                # objc_msgSend(NSColor, sel)   -> nil here
///  8142e60  mov  rdi,rax
///  8142e63  call _objc_retainAutoreleasedReturnValue
///  8142e68  mov  r15,rax                                 # r15 = the answer
///  8142e6b  mov  rsi,[rip+…]        # 'objectAtIndexedSubscript:'
///  8142e72  mov  edx,0x1                                  # <-- index 1
///  8142e77  mov  rdi,rax                                 # the answer, untested
///  8142e7a  call r14                # message to nil -> nil
///  8142e8b  call 0x61ac660          # a nil NSColor
/// ```
///
/// **There is no `respondsToSelector:` in that function** — the six-selector
/// enumeration is the control, and a gate would be a seventh load — and **no test
/// of the answer between `+0x8142e5d` and `+0x8142e7a`**: the retain, the
/// register shuffle and `edx = 1` are all that is between them. So the guest has
/// no designed path for this selector being absent, which is why a wrong pair here
/// is painted rather than survived.
///
/// It is the only reference in the image. A whole-`__TEXT,__text` rip-relative
/// sweep for the selref word `0xe6eba38` returns **1** site, `+0x8142e4f`;
/// positive controls on the same pass: `objectAtIndexedSubscript:`'s
/// `0xe6e7dc0` returns 34, `appearanceNamed:`'s `0xe6e9460` returns 6.
///
/// # What the `nil` reaches, and why that is the guest's own `CHECK`
///
/// `+0x61ac660` retains its argument, sends `-colorUsingColorSpace:`, and
/// branches on the result: `je 0x61ac7b7` at `+0x61ac6cb` is the **nil** arm, and
/// that arm does `objc_retainAutorelease` → `-[NSColor CGColor]` →
/// `_CGColorGetNumberOfComponents`. Two things make that the honest outcome rather
/// than a second defect to repair:
///
/// * **`-colorUsingColorSpace:` is not what puts us there.** It returns the
///   receiver whatever the space is (`color_using_color_space`, this file), and
///   the space the guest asks for is a *real* object here: the classref
///   `0xe6e7370` is paired with the `sRGBColorSpace` selref at `+0x3ab58e2` and
///   the `deviceRGBColorSpace` selref at `+0x61ac681`, and
///   `+[NSColorSpace deviceRGBColorSpace]` is installed at `backing.rs:1241`.
///   The `CGColor` arm is entered **iff the colour itself is nil**, which is this
///   answer and nothing else.
/// * **`-[NSColor CGColor]` is registered by no class in this tree**, so that arm
///   yields nil, `_CGColorGetNumberOfComponents(nil)` is 0 (`cgcontext.rs:687`),
///   and `add rax,-3; cmp rax,-3; jbe 0x61ac864` fires. That `int3` is
///   `test/logs/REPORT-cgcolor-components.md`'s `CHECK`, and it is the guest
///   reporting our incompleteness. It is **not** a reason to invent a component
///   count: `+0x6bb010` is `SkColor4f::toColor()` (`255.0f` at `0xd637fe0`,
///   `0.5f` at `0xd637f40`, read out of the image), so a made-up count becomes
///   painted pixels.
///
/// # What the answer costs, said rather than hidden
///
/// Installing this method moves a `Refusal` abort into that `CHECK` on a default
/// run: with `ROSETTA3_LOG_UNRECOGNIZED` unset the selector used to abort here by
/// name, and it will now answer `nil` and let the guest trap. The line below is
/// **unconditional** and printed **immediately before** the trap, so the cause is
/// still named at the moment of the fault — the guest's own assertion, with this
/// refusal as the line above it. That is the `_NSServiceEntry` trade and the
/// `CGColor` trade already made in this project, and it is the right one: the
/// alternative is a plausible component count nobody here measured.
///
/// `-[NSColor CGColor]` is a genuinely missing public method and it is the reason
/// a nil colour becomes a `CHECK` rather than a colour. It is **not** implemented
/// here, and implementing it would not move this wall: the only path to it is a
/// nil receiver, where a method on nil answers nil by definition.
unsafe extern "C" fn alternating_content_background_colors(_class: Id, _sel: Sel) -> Id {
    refuse_alternating_content_background_colors();
    std::ptr::null_mut()
}

/// Say, once per process, that there is no system colour to make the pair from.
///
/// Once per process rather than once per call, for the reason
/// [`refuse_no_catalog`] and `refuse_display_option` give: the point of the line
/// is that a named capability is absent, not that one event happened. Unconditional
/// and not env-gated, because the whole deliverable is that the absence is named;
/// an env-gated refusal is not a refusal.
fn refuse_alternating_content_background_colors() {
    static SAID: std::sync::Mutex<bool> = std::sync::Mutex::new(false);
    let mut said = SAID.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if *said {
        return;
    }
    *said = true;
    eprintln!(
        "rosetta3: appkit: +[NSColor alternatingContentBackgroundColors]: answered nil. This is \
         a public, documented AppKit class property (macOS 10.14+, typed NSColor[], \"the colors \
         to use for alternating content, typically found in table views and collection views\"), so \
         the missing capability is a real platform one and it is named here: **an AppKit system \
         colour for the current appearance, of which this host has none**. Measured: 0 *.colorset, \
         0 *.xcassets, 0 Contents.json and 0 *.car across all 214 files of the Chromium bundle, no \
         ColorSync among the Framework's 60 dependent libraries, and no ColorSync or NamedColor \
         symbol among the 8828 dyld bind records of the 11 images whose bind stream decodes. This \
         is deliberately NOT answered like the four -[NSWorkspace accessibilityDisplayShould...] \
         flags in workspace.rs: those report whether the user asked for a display option, and \
         \"nobody has asked\" is a true answer with a real boolean behind it. This one has no such \
         boolean - it reports a shade, and the only way to answer it is to invent two colours that \
         the guest paints and cannot tell from real ones. NOT answered as an empty array either: \
         that is not a value this method can have on macOS, and it is worse than nil, because the \
         one caller in the image subscripts the answer at index 1 with no bounds check and no nil \
         test (Framework+0x8142e72 `mov edx,0x1`; 0x8142e5d the send, 0x8142e7a the subscript, \
         nothing in between), so an empty NSArray raises inside AppKit emulation while nil is a \
         message to nil. The caller is the native-theme colour table at Framework+0x8142aa0 \
         (3258 bytes, 790 instructions, 163 control transfers enumerated and classified from their \
         own bytes); it loads exactly six selectors - four are the singular named colours \
         disabledControlTextColor, controlTextColor, keyboardFocusIndicatorColor and \
         selectedTextBackgroundColor, each handed straight to the Skia accessor at +0x61ac660, and \
         the fifth is objectAtIndexedSubscript:, which belongs to this selector alone - and there is \
         no respondsToSelector: in that function, so \
         there is no designed path for the selector being absent. This selref (0xe6eba38) has \
         exactly 1 reference in the whole 214 MB of __text, at +0x8142e4f; positive controls on the \
         same pass: objectAtIndexedSubscript: 34, appearanceNamed: 6. It uses element 1 - the \
         *alternate* of the pair - for the theme's alternating-content band, and \
         +[NSColor alternatingContentBackgroundColor] (singular) is answered in this tree at 56/56 \
         of the NAMED table but is NOT a selector the guest contains at all: the NUL-terminated \
         byte scan returns 0 for it, 39 of the 56 NAMED selectors return 0 that way, and the four \
         positive controls on the same two-sided scan (controlTextColor, \
         selectedTextBackgroundColor, keyboardFocusIndicatorColor, disabledControlTextColor) each \
         return 1. INHERITANCE REFUSED: the pair a two-element answer would have to be is exactly \
         the one +[NSColor controlAlternatingRowBackgroundColors] already fabricates in this file \
         (the function alternating_row_colors) - [+controlBackgroundColor white, \
         +alternatingContentBackgroundColor 0.96 grey] - and 0.96 is an invented constant standing \
         in for a shade nothing here measures, now named ALTERNATING_CONTENT_BACKGROUND_GREY in \
         this file for that reason. A two-element answer would hand the guest precisely that \
         unmeasured grey as element 1. Neither half of the fabricated pair is on a path this \
         guest can take anyway: controlAlternatingRowBackgroundColors occurs 0 times in the image \
         and controlBackgroundColor is among the 39 NAMED names the image does not contain. \
         CONSEQUENCE, stated rather than hidden: with a real colour the CHECK at +0x61ac864 is not \
         reached; with nil the guest indexes nil, -colorUsingColorSpace: returns the receiver \
         (nil), -[NSColor CGColor] - registered by no class in this tree - answers nil, \
         _CGColorGetNumberOfComponents(nil) is 0 (cgcontext.rs:687), and add rax,-3; cmp rax,-3; \
         jbe 0x61ac864 traps. That is the guest correctly reporting our incompleteness, the same \
         trade as _NSServiceEntry and the same trade as the CGColor component count, and the line \
         above it is unconditional so the cause is named at the moment of the fault. Full report: \
         test/logs/REPORT-alternating-colors.md"
    );
}

/// `+controlAlternatingRowBackgroundColors`, an `NSArray` of **two** colours.
///
/// **This is a fabricated pair, and it is kept and documented rather than removed
/// or inherited.** It is not in [`NAMED`] because it is not a colour: a table view
/// alternates its row background between the two, so the answer is the pair and a
/// caller indexes it with the row number. The members are the even and odd row
/// backgrounds, which match `+controlBackgroundColor` and
/// `+alternatingContentBackgroundColor` — a caller that uses one table and the
/// other constant has to get the same shade.
///
/// Two things are on record about it, both measured:
///
/// * **Both halves are invented.** `+controlBackgroundColor` is white here and
///   `+alternatingContentBackgroundColor` is
///   [`ALTERNATING_CONTENT_BACKGROUND_GREY`], and neither is derived from
///   anything on this host. This is the same objection as the three constants
///   `MEMORY.md` lists, and it is **not** resolved here: removing the method would
///   turn a fabricated pair into an unrecognized-selector abort on the default
///   run, which is a worse milestone outcome and not this file's call to make. It
///   is named so that a ruling can be made about it deliberately.
/// * **No path in this guest can reach it.** The selector occurs **0** times in
///   the Framework (positive controls on the same scan: `controlTextColor`,
///   `selectedTextBackgroundColor`, `keyboardFocusIndicatorColor`,
///   `disabledControlTextColor`, `unemphasizedSelectedContentBackgroundColor`,
///   `deviceRGBColorSpace`, `colorUsingColorSpace:` each exactly 1), and
///   `+controlBackgroundColor`, the even member, is one of the 39 [`NAMED`]
///   selectors whose name the image does not contain at all. So this fabrication
///   is off the milestone's path entirely — which is also why the plural
///   selector's answer does **not** reuse it. See
///   [`alternating_content_background_colors`].
///
/// Kept rather than autoreleased, for [`singleton`]'s reason: this is a named
/// constant a caller may put straight into a `static`.
unsafe extern "C" fn alternating_row_colors(_class: Id, _sel: Sel) -> Id {
    static COLORS: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
    let mut kept = COLORS.lock().unwrap_or_else(|e| e.into_inner());
    if *kept != 0 {
        return *kept as Id;
    }
    let even = singleton("controlBackgroundColor", 1.0, 1.0, 1.0, 1.0);
    let odd = singleton(
        "alternatingContentBackgroundColor",
        ALTERNATING_CONTENT_BACKGROUND_GREY,
        ALTERNATING_CONTENT_BACKGROUND_GREY,
        ALTERNATING_CONTENT_BACKGROUND_GREY,
        1.0,
    );
    let array = corefoundation::collections::new_array(
        &[
            even as corefoundation::base::CFTypeRef,
            odd as corefoundation::base::CFTypeRef,
        ],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    *kept = array as usize;
    array as Id
}

unsafe extern "C" fn color_with_rgba(
    _class: Id,
    _sel: Sel,
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
) -> Id {
    make(red, green, blue, alpha)
}

unsafe extern "C" fn color_with_white(_class: Id, _sel: Sel, white: CGFloat, alpha: CGFloat) -> Id {
    make(white, white, white, alpha)
}

unsafe extern "C" fn component(receiver: Id, sel: Sel) -> CGFloat {
    // SAFETY: a colour of this crate's.
    let c = unsafe { &*(receiver as *const Color) };
    // SAFETY: a registered selector is a NUL-terminated interned name.
    match unsafe { std::ffi::CStr::from_ptr(sel) }.to_bytes() {
        b"redComponent" => c.red,
        b"greenComponent" => c.green,
        b"blueComponent" => c.blue,
        _ => c.alpha,
    }
}

/// `-colorSpaceName`, the deprecated name of the space a colour is in.
///
/// Calibrated RGB, which is what every colour here is: four components in the
/// space the module note describes. A caller compares the result against
/// `NSDeviceCMYKColorSpace` before reaching for the CMYK accessors, and this sends
/// it down the RGB branch - which is the one that can be answered.
unsafe extern "C" fn color_space_name(_receiver: Id, _sel: Sel) -> Id {
    // The same interned object the `_NSCalibratedRGBColorSpace` global holds, so a
    // caller comparing the two with `==` - which is what a space-name check is
    // written as - finds them identical as well as equal.
    foundation::notification::name_object("NSCalibratedRGBColorSpace") as Id
}

/// `-colorUsingColorSpaceName:`
///
/// The receiver when the space asked for is an RGB one, and nil otherwise.
///
/// This is not the `self`-returning conversion the module note refuses. The note's
/// objection is to a conversion that silently did not happen; a colour here *is*
/// calibrated RGB, so answering it to a caller that asked for RGB is the
/// conversion, complete. A caller that asked for CMYK or grey gets nil, which is
/// the documented answer for a colour that cannot be converted and the branch it
/// already has - rather than four RGB components relabelled as something else.
///
/// # Safety
///
/// `name` is an `NSString` or nil.
unsafe extern "C" fn color_using_color_space_name(receiver: Id, _sel: Sel, name: Id) -> Id {
    let name = unsafe { foundation::string::bytes(name) };
    // A null or empty name means "the most appropriate space", which is this one.
    if name.is_empty() {
        return receiver;
    }
    const RGB: [&str; 4] = [
        "NSCalibratedRGBColorSpace",
        "NSDeviceRGBColorSpace",
        "NSCustomColorSpace",
        "kCGColorSpaceSRGB",
    ];
    match RGB.iter().any(|space| space.as_bytes() == name.as_slice()) {
        true => receiver,
        false => std::ptr::null_mut(),
    }
}

/// `-colorUsingColorSpace:`
///
/// The receiver. There is one colour space in this runtime and every colour is
/// already in it - see the module note - and the `NSColorSpace` a caller would pass
/// to ask for another is a class this tree does not have, so the argument is
/// always the space the receiver is in or nil.
unsafe extern "C" fn color_using_color_space(receiver: Id, _sel: Sel, _space: Id) -> Id {
    receiver
}

/// `-catalogNameComponent` and `-colorNameComponent`.
///
/// Nil, because a colour here is not a catalog colour: it is four components, and
/// `+controlTextColor` resolved them when it was sent rather than handing back a
/// named reference into a catalog that is resolved later.
///
/// The distinction is one a caller acts on. Qt asks whether the catalog is
/// `System`, and renders a colour that says yes into a pattern image instead of
/// reading its components - so claiming a catalog would send it down a drawing
/// path to represent a colour that is already a plain one.
unsafe extern "C" fn name_component(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

/// `-numberOfComponents`, which is four: red, green, blue and alpha.
unsafe extern "C" fn number_of_components(_receiver: Id, _sel: Sel) -> isize {
    4
}

/// `-whiteComponent`, the brightness of a grey colour.
///
/// The Rec. 601 luma of the three components, which is what AppKit answers for a
/// colour that is not already grey - and for one that is, the three are equal and
/// any weighting gives the same number back.
unsafe extern "C" fn white_component(receiver: Id, _sel: Sel) -> CGFloat {
    // SAFETY: a colour of this crate's.
    let c = unsafe { &*(receiver as *const Color) };
    0.299 * c.red + 0.587 * c.green + 0.114 * c.blue
}

/// `-getWhite:alpha:`
///
/// # Safety
///
/// Each out-parameter is null or writable for one `CGFloat`.
unsafe extern "C" fn get_white(receiver: Id, sel: Sel, white: *mut CGFloat, alpha: *mut CGFloat) {
    // SAFETY: a colour of this crate's.
    let c = unsafe { &*(receiver as *const Color) };
    // SAFETY: the caller states each is null or writable.
    unsafe {
        if !white.is_null() {
            *white = white_component(receiver, sel);
        }
        if !alpha.is_null() {
            *alpha = c.alpha;
        }
    }
}

unsafe extern "C" fn get_components(
    receiver: Id,
    _sel: Sel,
    red: *mut CGFloat,
    green: *mut CGFloat,
    blue: *mut CGFloat,
    alpha: *mut CGFloat,
) {
    // SAFETY: a colour of this crate's.
    let c = unsafe { &*(receiver as *const Color) };
    // SAFETY: four out-parameters, each null or writable, as the method's
    // contract says.
    unsafe {
        for (slot, value) in [
            (red, c.red),
            (green, c.green),
            (blue, c.blue),
            (alpha, c.alpha),
        ] {
            if !slot.is_null() {
                *slot = value;
            }
        }
    }
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSColor").expect("declared before its methods");
    let meta = class.metaclass();
    for (name, ..) in NAMED {
        meta.replace_method(objc::sel::get(name), named as *const c_void, c"@16@0:8");
    }
    class_methods!(class, {
        "alternatingContentBackgroundColors" => alternating_content_background_colors, c"@16@0:8";
        "controlAlternatingRowBackgroundColors" => alternating_row_colors, c"@16@0:8";
        "colorWithCatalogName:colorName:" => color_with_catalog_name, c"@32@0:8@16@24";
        "colorWithCalibratedRed:green:blue:alpha:" => color_with_rgba, c"@48@0:8d16d24d32d40";
        "colorWithDeviceRed:green:blue:alpha:" => color_with_rgba, c"@48@0:8d16d24d32d40";
        "colorWithSRGBRed:green:blue:alpha:" => color_with_rgba, c"@48@0:8d16d24d32d40";
        "colorWithRed:green:blue:alpha:" => color_with_rgba, c"@48@0:8d16d24d32d40";
        "colorWithCalibratedWhite:alpha:" => color_with_white, c"@32@0:8d16d24";
        "colorWithDeviceWhite:alpha:" => color_with_white, c"@32@0:8d16d24";
        "colorWithWhite:alpha:" => color_with_white, c"@32@0:8d16d24";
    });
    methods!(class, {
        "redComponent" => component, c"d16@0:8";
        "greenComponent" => component, c"d16@0:8";
        "blueComponent" => component, c"d16@0:8";
        "alphaComponent" => component, c"d16@0:8";
        "getRed:green:blue:alpha:" => get_components, c"v48@0:8^d16^d24^d32^d40";
        "whiteComponent" => white_component, c"d16@0:8";
        "getWhite:alpha:" => get_white, c"v32@0:8^d16^d24";
        "numberOfComponents" => number_of_components, c"q16@0:8";
        "colorSpaceName" => color_space_name, c"@16@0:8";
        "colorUsingColorSpaceName:" => color_using_color_space_name, c"@24@0:8@16";
        "colorUsingColorSpaceName:device:" => color_using_color_space_name, c"@32@0:8@16@24";
        "colorUsingColorSpace:" => color_using_color_space, c"@24@0:8@16";
        "catalogNameComponent" => name_component, c"@16@0:8";
        "colorNameComponent" => name_component, c"@16@0:8";
        "localizedCatalogNameComponent" => name_component, c"@16@0:8";
        "localizedColorNameComponent" => name_component, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every named colour answers, and answers with its own components - which
    /// is the claim one shared implementation has to earn.
    #[test]
    fn each_named_colour_is_its_own() {
        crate::bootstrap();
        let class = objc::class::ptr(objc::class::lookup(c"NSColor").expect("NSColor")) as Id;
        for (name, r, g, b, a) in NAMED {
            // SAFETY: `+namedColor` is `id (Class, SEL)`.
            let color = unsafe { named(class, objc::sel::get(name)) };
            assert!(!color.is_null(), "{name}");
            // SAFETY: a colour this crate just made.
            assert_eq!(unsafe { components(color) }, (*r, *g, *b, *a), "{name}");
        }
    }

    /// A named colour is the *same object* every time and is not autoreleased.
    ///
    /// The case this protects: a caller that puts one in a `static` during a module
    /// initializer and reads it back after the pool has drained. An autoreleased
    /// colour there is freed, and the read is a message to freed memory.
    #[test]
    fn a_named_colour_is_one_kept_object() {
        crate::bootstrap();
        let class = objc::class::ptr(objc::class::lookup(c"NSColor").expect("NSColor")) as Id;
        let sel = objc::sel::get("controlTextColor");
        // SAFETY: `+namedColor` is `id (Class, SEL)`.
        let (first, second) = unsafe { (named(class, sel), named(class, sel)) };
        assert_eq!(first, second, "a named colour is a singleton");
        // Drain a pool around a read: an autoreleased colour would not survive it.
        let pool = objc::arc::pool_push();
        // SAFETY: the colour just obtained.
        let before = unsafe { components(first) };
        objc::arc::pool_pop(pool);
        // SAFETY: still live, because nothing autoreleased it.
        assert_eq!(unsafe { components(first) }, before);
        assert_eq!(before, (0.0, 0.0, 0.0, 1.0));
    }

    /// And something that is not a colour reads as opaque black rather than as
    /// whatever its second word happens to be.
    #[test]
    fn a_non_colour_reads_as_black() {
        crate::bootstrap();
        let string = foundation::string::from_str("not a colour");
        // SAFETY: a live object.
        assert_eq!(unsafe { components(string) }, (0.0, 0.0, 0.0, 1.0));
        // SAFETY: null is allowed.
        assert_eq!(
            unsafe { components(std::ptr::null_mut()) },
            (0.0, 0.0, 0.0, 1.0)
        );
    }

    /// The pair the one call site in the image sends.
    const GUEST_PAIR: (&str, &str) = ("System", "controlAccentBlueColor");

    /// `+[NSColor colorWithCatalogName:colorName:]` through `objc_msgSend`, with
    /// the catalog in `rdx` and the colour in `rcx` — the two registers the
    /// guest's own call site writes, at `Framework+0x3ab57fb`.
    fn send_catalog(catalog: Id, color: Id) -> Id {
        // SAFETY: `+colorWithCatalogName:colorName:` is
        // `id (*)(Class, SEL, NSString *, NSString *)`. That is the claim the
        // test below checks against the *installed encoding*, read back out of
        // the class, rather than taken from this signature.
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let class = objc::class::ptr(objc::class::lookup(c"NSColor").expect("NSColor"));
        // SAFETY: the class and both arguments are as the selector declares.
        unsafe { f(class as Id, objc::sel::get("colorWithCatalogName:colorName:"), catalog, color) }
    }

    /// The guest's own pair resolves to nothing, and so does every other pair,
    /// because there is no catalog on this host to resolve any of them against.
    #[test]
    fn a_named_catalog_colour_is_nil() {
        crate::bootstrap();
        let (catalog, color) = GUEST_PAIR;
        let catalog = foundation::string::from_str(catalog) as Id;
        let color = foundation::string::from_str(color) as Id;
        // SAFETY: two strings, as the selector declares.
        assert!(send_catalog(catalog, color).is_null(), "the guest's own pair");
        // SAFETY: the same two, asked twice: still nothing, and no crash.
        assert!(send_catalog(catalog, color).is_null());
        // SAFETY: two more strings.
        let other = foundation::string::from_str("Chromium") as Id;
        assert!(send_catalog(other, color).is_null(), "a catalog that does not exist");
        // SAFETY: an empty colour name.
        let empty = foundation::string::from_str("") as Id;
        assert!(send_catalog(catalog, empty).is_null(), "an empty colour name");
        // SAFETY: the nil path is the method's documented answer, not a crash.
        assert!(send_catalog(std::ptr::null_mut(), std::ptr::null_mut()).is_null());
    }

    /// The **installed** type encoding agrees with the selector, on three counts
    /// re-derived here rather than copied from the implementation.
    ///
    /// This is the assertion that would have caught the `v32@0:8@?24` installed
    /// for `performAsCurrentDrawingAppearance:` — a one-argument method declared
    /// with two. That frame was self-consistent and its arity was not, so the
    /// block was read out of `rcx`, which the guest never writes, and the
    /// process executed the heap. Its unit test passed throughout, because it
    /// agreed with the misreading. So this reads the encoding back out of the
    /// class, where a wrong string is visible *as a string*, and checks it
    /// against the selector's own colon count and the `last_arg_offset + 8`
    /// rule — neither of which the implementation gets a vote on.
    #[test]
    fn the_declared_encoding_agrees_with_the_selector() {
        crate::bootstrap();
        const SEL: &str = "colorWithCatalogName:colorName:";
        let class = objc::class::lookup(c"NSColor").expect("NSColor");
        let method = class
            .metaclass()
            .own_method(objc::sel::get(SEL))
            .expect("+colorWithCatalogName:colorName: is installed");
        // SAFETY: `Method::types` is a `CString` the class leaked and still owns.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        let types = types.to_str().expect("an encoding is ASCII");

        // An encoding is `@<frame>@0:8` then one `type<offset>` per argument, so
        // it can be walked without a full type parser: the head is fixed and
        // every argument is a one-character descriptor followed by digits.
        let (head, args) = types
            .strip_prefix('@')
            .and_then(|rest| rest.split_once("@0:8"))
            .unwrap_or_else(|| panic!("{types} is not `@<frame>@0:8...`"));
        let frame: usize = head.parse().unwrap_or_else(|_| panic!("{head} is not a frame size"));

        // Rule 1: one argument per colon, counted from the selector itself.
        let wanted = SEL.bytes().filter(|b| *b == b':').count();
        assert!(wanted > 0, "the selector has arguments to declare");

        let mut kinds = Vec::new();
        let mut last = 0;
        let mut rest = args;
        while !rest.is_empty() {
            let (kind, tail) = rest.split_at(1);
            let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
            assert!(
                !digits.is_empty(),
                "{kind:?} in {types} has no offset, so the encoding is malformed"
            );
            last = digits.parse().expect("a decimal argument offset");
            rest = &tail[digits.len()..];
            kinds.push(kind);
        }
        assert_eq!(
            kinds.len(),
            wanted,
            "{types} declares {} argument(s); {SEL} has {wanted} colon(s)",
            kinds.len()
        );

        // Rule 2: a frame is `last_arg_offset + 8`. A frame that is not is a
        // hard error, and the cheapest place to find one is here.
        assert_eq!(
            frame,
            last + 8,
            "{types}: frame {frame} is not last_arg_offset {last} + 8"
        );

        // Rule 3: two objects, because the call site puts two `__cfstring`
        // pointers in `rdx` and `rcx` and reads them as strings.
        assert_eq!(kinds, vec!["@", "@"], "{types}: both arguments are objects");
    }

    /// The plural selector, sent the way the guest sends it: through
    /// `objc_msgSend` with the class in `rdi` and the selector in `rsi`.
    fn send_alternating_content_background_colors() -> Id {
        // SAFETY: `+alternatingContentBackgroundColors` takes no arguments and
        // returns an object, which is the claim the test below checks against
        // the *installed* encoding rather than taken from this signature.
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let class = objc::class::ptr(objc::class::lookup(c"NSColor").expect("NSColor"));
        // SAFETY: a class and a zero-argument selector, as the selector declares.
        unsafe { f(class as Id, objc::sel::get("alternatingContentBackgroundColors")) }
    }

    /// **The answer is `nil`, and it is `nil` rather than a pair or an empty
    /// array** — the claim this method has to earn on every run, because the
    /// three candidate answers differ only in what the guest paints.
    ///
    /// The test pins the *answer*, not the printing, which is the point: a
    /// function that returned the fabricated pair would still print the refusal
    /// and would still pass a test that only checked the refusal appeared. So
    /// this asserts the value the guest will read at `Framework+0x8142e77`.
    #[test]
    fn the_plural_is_nil_and_not_a_pair() {
        crate::bootstrap();
        let answer = send_alternating_content_background_colors();
        assert!(
            answer.is_null(),
            "+[NSColor alternatingContentBackgroundColors] answered {:?}, not nil: the \
             guest indexes this at 1 with no bounds check and no nil test and paints it, so a \
             pair here is a fabricated pair it cannot tell from a real one",
            answer
        );
        // Asked twice: still nil, and still no pair invented on the second ask.
        assert!(send_alternating_content_background_colors().is_null());
    }

    /// **The once-per-process latch on the refusal does not change the answer.**
    ///
    /// The refusal prints once per process, so the second ask is the interesting
    /// one: an implementation that let the latch return something other than
    /// `nil` — the singleton the latch caches, say — would answer differently
    /// the second time, and the guest's one call site is a `std::call_once`-style
    /// initialiser that runs at most once anyway, so the difference would be
    /// invisible at run level and visible only here.
    ///
    /// **This test deliberately does not assert on the printed line**, because
    /// `libtest` discards a passing test's stderr — which is how "9 passing" was
    /// once 9 quiet skips. A mutation that *deletes* the refusal call leaves all
    /// nine tests in this module passing, and that is the intended result: the
    /// tests pin the answer, so no test of them can pass while the function
    /// invents a colour.
    #[test]
    fn the_answer_is_null_on_every_ask_and_not_only_the_first() {
        crate::bootstrap();
        assert!(send_alternating_content_background_colors().is_null());
        assert!(send_alternating_content_background_colors().is_null());
    }

    /// **The installed encoding agrees with the selector's own colon count, for
    /// the plural.** Re-derived here the way the two-argument case above is,
    /// because a method encoding is never read at dispatch: a wrong string would
    /// be invisible to every other test in this file.
    ///
    /// `alternatingContentBackgroundColors` has **zero** colons, so it declares
    /// **zero** arguments, and a frame of 16 is the one a zero-argument
    /// object-returning method has. The `last_arg_offset + 8` rule of the
    /// two-argument case does not apply here and is not applied: there is no
    /// last argument. A two-argument shape installed here would be the exact
    /// `v32@0:8@?24` class of error this file already hit once, in a method whose
    /// unit test passed throughout because it agreed with the misreading.
    #[test]
    fn the_plurals_declared_encoding_agrees_with_its_zero_colon_selector() {
        crate::bootstrap();
        const SEL: &str = "alternatingContentBackgroundColors";
        let class = objc::class::lookup(c"NSColor").expect("NSColor");
        let method = class
            .metaclass()
            .own_method(objc::sel::get(SEL))
            .expect("+alternatingContentBackgroundColors is installed");
        // SAFETY: `Method::types` is a `CString` the class leaked and still owns.
        let types = unsafe { std::ffi::CStr::from_ptr(method.types()) };
        let types = types.to_str().expect("an encoding is ASCII");

        let wanted = SEL.bytes().filter(|b| *b == b':').count();
        assert_eq!(wanted, 0, "{SEL} takes no arguments by its own name");
        // `@<frame>@0:8` and nothing after it: anything more is a declared
        // argument for a selector that declares none.
        let (head, rest) = types
            .strip_prefix('@')
            .and_then(|rest| rest.split_once("@0:8"))
            .unwrap_or_else(|| panic!("{types} is not `@<frame>@0:8...`"));
        let frame: usize = head.parse().unwrap_or_else(|_| panic!("{head} is not a frame size"));
        assert_eq!(rest, "", "{types} declares arguments; {SEL} has {wanted} colons");
        assert_eq!(frame, 16, "{types}: a zero-argument method returning an object");
    }

    /// **`+[NSColor alternatingContentBackgroundColors]` is not the selector
    /// the guest sends, and the plural is not derived from the singular.**
    ///
    /// The singular *is* answered, from [`NAMED`], with
    /// [`ALTERNATING_CONTENT_BACKGROUND_GREY`]; the plural is the one the guest
    /// actually sends and it is answered `nil`. This test exists so the two
    /// cannot be confused by a later reader, and so the invented constant is
    /// pinned to the selector that is off every path this guest can take —
    /// which is the whole reason the plural is not answered with it.
    ///
    /// **What this does *not* do is assert that the constant is `0.96`, and the
    /// omission is deliberate.** The constant is a fabrication, so there is no
    /// true value to check it against; a test asserting `0.96` would be a witness
    /// assertion about an invented number, and it would make the number look
    /// chosen. Measured consequence: a mutation that changes
    /// `ALTERNATING_CONTENT_BACKGROUND_GREY` to `0.5` leaves **all nine** tests in
    /// this module passing, because both sides of every comparison here are the
    /// same constant. The claim this test does earn is narrower and is the one
    /// that matters: the constant is the *single source* for the singular's
    /// value, and the plural's answer is `nil` whatever the constant says — the
    /// `pair` mutation, which wires the plural to the fabricated two-element
    /// answer, fails this test.
    #[test]
    fn the_singular_is_answered_from_the_named_constant_and_the_plural_is_nil() {
        crate::bootstrap();
        let class = objc::class::ptr(objc::class::lookup(c"NSColor").expect("NSColor")) as Id;
        let grey = ALTERNATING_CONTENT_BACKGROUND_GREY;
        // SAFETY: `+namedColor` is `id (Class, SEL)`.
        let singular = unsafe { named(class, objc::sel::get("alternatingContentBackgroundColor")) };
        assert!(!singular.is_null(), "the singular is answered");
        // SAFETY: a colour this crate just made.
        assert_eq!(
            unsafe { components(singular) },
            (grey, grey, grey, 1.0),
            "the singular's value is exactly the named constant and nothing else, so a reader \
             who changes the constant knows this is what moves"
        );
        // And the plural is a *different* answer to a *different* selector, not
        // the singular's answer handed back in an array.
        assert!(send_alternating_content_background_colors().is_null());
    }
}
