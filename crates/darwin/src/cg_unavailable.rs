//! CoreGraphics, CoreText and ImageIO: the geometry that is real, and the rest
//! named.
//!
//! # Why this file exists
//!
//! Ninety-three of the guest's remaining unbound *function* imports come from
//! three frameworks, and an unbound import is a jump through a zero slot - which
//! is how whole bring-up runs have been lost. Every name here is bound to a
//! function that either answers the truth or says, once, on stderr, that it
//! cannot.
//!
//! ImageIO contributes **nothing**, and that is a measurement rather than an
//! omission: the whole `Chromium.app` bundle was scanned through its indirect
//! symbol tables (`__TEXT,__stubs` and `__DATA,__la_symbol_ptr` of both
//! `MacOS/Chromium` and `Chromium Framework`, 1579 + 82 stubs) and not one
//! `CGImageSource*`, `CGImageDestination*` or `CGImageMetadata*` name appears in
//! either. There is no ImageIO codec surface to answer, so there is no ImageIO
//! refusal to invent either.
//!
//! # The two ABI facts, both read out of the binary
//!
//! Everything below that touches a `CGRect` depends on how the guest's calls
//! actually pass one, and the answer is not the one a header suggests:
//!
//! * **A `CGRect` argument is on the stack.** 32 bytes is four eightbytes, which
//!   the classification rules would put in `xmm0`-`xmm3`, and the compiler did
//!   not: `CGRectEqualToRect` is called at `Framework+0x3ad40c6` with the first
//!   rectangle written to `[rsp]`/`[rsp+0x10]` and the second to
//!   `[rsp+0x20]`/`[rsp+0x30]`, and `CGRectIsEmpty` at `+0x4591420` and
//!   `CGRectIsNull` at `+0x80532c3` each with one rectangle in the same two
//!   slots. A `#[repr(C)]` four-`f64` struct passed by value is a memory-class
//!   aggregate, so `extern "C"` reads it from exactly there.
//! * **A `CGRect` return goes through a hidden pointer in `rdi`.** Also
//!   memory-class: `CGContextConvertRectToDeviceSpace` is called at
//!   `+0x520adbf` with `lea rdi,[rbp-0x50]` as the *first* operand and the
//!   context in `rsi`, and `CGPDFPageGetBoxRect` at `+0x8b56c18` with the page in
//!   `rsi` and the box in `edx`. Declaring those two as `-> [Rect; 1]`-free
//!   plain `-> Rect` gets the hidden pointer for free.
//!
//! # What is answered, and what is refused
//!
//! | | names | |
//! |---|--:|---|
//! | computed from the arguments or from the class table | 8 | `CGRectEqualToRect`, `CGRectIsEmpty`, `CGRectIsNull`, `CGColorGetTypeID`, `CTFontDescriptorGetTypeID`, `CGPathIsEmpty`, `CGColorCreateCopyByMatchingToColorSpace`, `CGContextConvertRectToDeviceSpace` |
//! | true negatives of a facility that is not here | 9 | the display, screen-capture and window-server probes |
//! | a no-op where the whole of the function is a side effect on a subsystem that is not here | 21 | the private `CGSSetWindow*`, the `CGContextSet*Subpixel*` flags, the `void` PDF page calls, `CGFontRelease` |
//! | refused by the framework's own null / false / zero | 55 | everything that needs a window server, a real `CGEvent`, a PDF codec, a font's glyph data, or a `CFNumber` |
//!
//! The refusals are grouped by the reason they are refusals, and every one of
//! them names its reason on stderr once, through [`note`]. The reasons are:
//!
//! * **There is no window server.** No `CGSDefaultConnection`, no display, no
//!   windows, no screen capture, no event tap. Chromium's own copy of the
//!   private headers reaches the same conclusion from the other end: it asks
//!   `CGPreflightScreenCaptureAccess` first (`+0x6929a08`) and skips the
//!   capture path when the answer is false, which is the answer there is.
//! * **There is no font here that this crate can read.** CoreText *is*
//!   implemented in another crate of this tree, with a real TrueType parser, but
//!   a `CTFontRef` in this runtime is a `CFDictionary` of attributes
//!   (`crates/appkit/src/coretext.rs`), and reading one needs `CFDictionary` and
//!   `CFString`, which `darwin` does not depend on. So every function that
//!   needs the *contents* of a font or a descriptor refuses; the two that only
//!   need to name a *type* are answered, and they are answered from the class
//!   table.
//! * **There is no PDF codec and no image data.** `CGPDFContextCreate` and its
//!   page calls, `CGDataConsumerCreateWithCFData`, `CGPDFDocument*`: printing
//!   to PDF and reading a PDF are decoding work, and the guest checks every one
//!   of these answers.
//! * **The answer would be a layout, and this crate has none.** A
//!   `CGRectCreateDictionaryRepresentation` answer is four `CFNumber`s; a
//!   `CGRectMakeWithDictionaryRepresentation` answer is four of them read back.
//!   `darwin` cannot make a `CFNumber`.
//!
//! # The two answers that are neither refusals nor obvious
//!
//! `CGSSetDenyWindowServerConnections` is the one place where the honest
//! refusal would be worse than the answer, and it is worth being explicit about
//! why. Chromium's sandbox prologue calls it with `1` at `+0xc8f770f` and
//! **`int3`s if it returns non-zero** (`+0xc8f7716` is `jne` to `0xc8f7736`,
//! which is `int3; ud2`). The request is "deny this process any window-server
//! connection". There is no window server, so there is nothing to deny, so the
//! state the call asks for already holds and the answer is success. That is a
//! claim about the world rather than about a capability, and it is the only
//! claim in this file of that shape.
//!
//! `CGEventSourceSecondsSinceLastEventType` is the one value chosen by
//! reasoning about the field's initial value rather than by measurement. The
//! guest uses it as a staleness test, not as a duration: at `+0x84fdad1` it
//! truncates the answer to an integer and returns `seconds >= threshold`. Zero
//! is the value the field holds before any event has been seen, and it puts the
//! guest on its "this event source is not stale" arm, which is true of a host
//! whose event source has never seen anything.

#[cfg(not(target_os = "macos"))]
mod imp {
    //! The answers, and why each one is the true answer.
    //!
    //! Every signature below was read out of `Chromium Framework` first and
    //! compared with the header afterwards; where the two disagree the binary
    //! wins, and the note says so. The `Framework+0x…` addresses are the guest's
    //! own call sites.

    use std::ffi::{c_int, c_void};
    use std::sync::atomic::{AtomicBool, Ordering};

    // ---------------------------------------------------------- the ABI types

    /// A `CGRect`: four `CGFloat`s, which are `double` on x86-64.
    ///
    /// `#[repr(C)]` and exactly 32 bytes, so it is a memory-class aggregate in
    /// both directions - on the stack as an argument, through a hidden pointer
    /// as a return value. See the module note for the instructions that
    /// establish both.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    struct Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    /// A `CGAffineTransform`: the six `CGFloat`s of `CGGeometry.h`'s struct, in
    /// its declared order.
    ///
    /// `#[repr(C)]` and six `f64`s, so it is a memory-class aggregate like
    /// [`Rect`] - on the stack as an argument. It is named rather than spelled
    /// `[f64; 6]` because a raw array is not FFI-safe and a struct with the
    /// right layout is.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct Affine {
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        tx: f64,
        ty: f64,
    }

    /// A refusal, said once.
    ///
    /// `write(2)` rather than `eprintln!`, for `crate::mach::policy`'s reason:
    /// a message about a refusal has to be out before the refusal is, and a
    /// buffered one can be lost. Allocating to build the line is safe here for
    /// a stated reason: this runtime suspends nothing, so no thread is ever
    /// stopped holding this tree's allocator lock.
    fn note(once: &AtomicBool, what: &str, why: &str) {
        if !once.swap(true, Ordering::Relaxed) {
            let line = format!("rosetta3: {what}: {why}\n");
            // SAFETY: a live buffer for the length written.
            unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
        }
    }

    /// One flag per reason, so a process that hits a family a thousand times says
    /// it once and a process that hits two families says both.
    mod said {
        use std::sync::atomic::AtomicBool;

        pub static WINDOW_SERVER: AtomicBool = AtomicBool::new(false);
        pub static DISPLAY: AtomicBool = AtomicBool::new(false);
        pub static SCREEN_CAPTURE: AtomicBool = AtomicBool::new(false);
        pub static EVENT_TAP: AtomicBool = AtomicBool::new(false);
        pub static PDF: AtomicBool = AtomicBool::new(false);
        pub static FONT_CONTENTS: AtomicBool = AtomicBool::new(false);
        pub static FONT_TYPE: AtomicBool = AtomicBool::new(false);
        pub static CF_CONTAINER: AtomicBool = AtomicBool::new(false);
        pub static IMAGE_DATA: AtomicBool = AtomicBool::new(false);
        pub static IMAGE_GEOMETRY: AtomicBool = AtomicBool::new(false);
        pub static CTM: AtomicBool = AtomicBool::new(false);
    }

    /// The class pointer `corefoundation` reports as a `CFTypeID`.
    ///
    /// `corefoundation::base`'s own definition, quoted rather than restated: "A
    /// `CFTypeID`. Apple's is a small integer; here it is the class pointer,
    /// which is the same thing said more directly - `CFGetTypeID(x) ==
    /// CFStringGetTypeID()` asks whether `x` is a string, and the class answers
    /// that exactly." (`crates/corefoundation/src/base.rs`.) So the type id a
    /// `...GetTypeID` function must return here is the pointer of the class
    /// whose instances report it, and the two `...GetTypeID` entries below are
    /// that pointer looked up rather than a number invented.
    fn type_id_of(class: &std::ffi::CStr) -> Option<u64> {
        objc::class::lookup(class).map(|c| objc::class::ptr(c) as u64)
    }

    // ------------------------------------------------------------ CoreGraphics

    /// `bool CGRectEqualToRect(CGRect rect1, CGRect rect2)`.
    ///
    /// **Computed, and the computation is Apple's.** "Returns whether two
    /// rectangles have identical origin and size" - four `==` comparisons, and
    /// `==` on doubles is what makes `-0.0` equal `0.0` and `NaN` unequal to
    /// itself, which is CoreGraphics' behaviour too.
    ///
    /// The guest uses it as a filter and branches on it:
    /// `Framework+0x3ad40c6` compares the rectangle of each entry of a
    /// `CGWindowListCopyWindowInfo` array against a rectangle it is looking for
    /// and sets a flag (`test r14b,r14b; jne 0x3ad4121`) for the match, and
    /// `+0x4593129` does the same for an image's bounds.
    unsafe extern "C" fn cg_rect_equal_to_rect(one: Rect, two: Rect) -> bool {
        one.x == two.x && one.y == two.y && one.width == two.width && one.height == two.height
    }

    /// `bool CGRectIsEmpty(CGRect rect)`.
    ///
    /// **Computed.** "Returns whether a rectangle has zero width or height" -
    /// `width <= 0 || height <= 0`. The null rectangle is empty under it too,
    /// which is Apple's documented behaviour rather than an extra rule here: a
    /// null rectangle has both sizes zero.
    ///
    /// The guest branches on it at `Framework+0x4591425` (`test al,al; jne
    /// 0x45914bc`) before it crops an image, so a rectangle it believes empty is
    /// never passed to `CGImageCreateWithImageInRect`.
    unsafe extern "C" fn cg_rect_is_empty(rect: Rect) -> bool {
        rect.width <= 0.0 || rect.height <= 0.0
    }

    /// `bool CGRectIsNull(CGRect rect)`.
    ///
    /// **Computed.** "Returns whether a rectangle is null", which Apple's
    /// `CGGeometry.h` defines as *the origin and size are zero*. `==` against
    /// `0.0` is what makes an all `-0.0` rectangle null, which is also what
    /// CoreGraphics' own comparison answers.
    ///
    /// The guest branches on it at `Framework+0x80532ca` (`test al,al; je
    /// 0x80533d7`), so a rectangle it believes null takes its own path rather
    /// than being used as a clip or a drawable.
    unsafe extern "C" fn cg_rect_is_null(rect: Rect) -> bool {
        rect.x == 0.0 && rect.y == 0.0 && rect.width == 0.0 && rect.height == 0.0
    }

    /// `CFTypeID CGColorGetTypeID(void)`.
    ///
    /// **Computed, from the class table, and this one the guest *needs* to be
    /// right.** Both call sites compare the answer against `CFGetTypeID` of the
    /// object in hand: `Framework+0x626f32d` (`cmp r15,rax; jne 0x626f39c`) and
    /// `+0x80a9d24` (`cmp rbx,rax`). A `CGColor` in this runtime is a `__CGColor`
    /// - `crates/appkit/src/cgcontext.rs` registers exactly that class, and
    /// registers it with `is_type = true`, which is what puts it in the set
    /// `corefoundation::base::type_id_of` searches. Its superclass is `NSObject`,
    /// which is not a CoreFoundation type, so the nearest type in the chain is
    /// `__CGColor` itself and `CFGetTypeID` of every `__CGColor` instance is
    /// `objc::class::ptr(__CGColor)`. That is what this returns.
    ///
    /// Both non-matching arms are paths the guest has: `+0x626f39c` returns,
    /// and `+0x80a9dcd` tries `AXValueGetTypeID` instead - the object may be an
    /// `AXValue` rather than a colour, which is a real alternative and not a
    /// failure.
    unsafe extern "C" fn cg_color_get_type_id() -> u64 {
        match type_id_of(c"__CGColor") {
            Some(id) => id,
            None => {
                note(
                    &said::FONT_TYPE,
                    "CGColorGetTypeID",
                    "no __CGColor class is registered, so there is no type id to name; \
                     __CGColor is installed by appkit's bootstrap and a guest that asks \
                     before it has run has asked too early",
                );
                0
            }
        }
    }

    /// `CGColorRef CGColorCreateCopyByMatchingToColorSpace(CGColorSpaceRef space, CGBitmapInfo options, CGColorRef color, CGColorMatchingIntent intent)`.
    ///
    /// **The source colour, and the argument is that this runtime has exactly
    /// one colour space.** `crates/appkit/src/color.rs` maps
    /// `+colorWithCalibratedRed:`, `+colorWithDeviceRed:`,
    /// `+colorWithSRGBRed:` and `+colorWithRed:green:blue:alpha:` onto one
    /// constructor that stores four components, and its `-colorUsingColorSpace:`
    /// returns the receiver for every colour. A `CGColorRef` here is therefore
    /// always already in the only colour space that exists, and converting it to
    /// a `CGColorSpaceRef` this runtime can hand out - which is the one
    /// `CGColorSpaceCreateWithName` builds, a `ColorSpace` object - is the
    /// identity on the component vector. Returning the colour is that identity.
    ///
    /// **The guest dereferences the answer without checking it**, which is why
    /// this is not a refusal: `Framework+0x61ac9a7` calls it and at `+0x61ac9b2`
    /// passes the result straight to `CGColorGetComponents`, whose pointer is
    /// read at `+0x61ac9b7` with no test in between. A null here is a fault at
    /// address zero.
    ///
    /// The reachability of that is worth recording, because it is not this
    /// function's doing: the caller is behind `test r14,r14; je 0x61ac7b7` on
    /// the result of `-colorUsingColorSpace:`, which in this runtime returns the
    /// receiver and so never falls into the `CGColor` arm. So the answer is
    /// correct for every call that can be made and unreachable for the rest.
    ///
    /// Ownership: CoreGraphics returns a new reference. The one call site holds
    /// its own reference to `color` (`objc_retainAutorelease` at `+0x61ac7ba`)
    /// and releases the answer exactly once at `+0x61ac9fd`, so handing back
    /// the colour it already owns balances; nothing is over-released.
    unsafe extern "C" fn cg_color_create_copy_by_matching_to_color_space(
        _space: *const c_void,
        _options: u32,
        color: *const c_void,
        _intent: c_int,
    ) -> *const c_void {
        color
    }

    /// `CGRect CGContextConvertRectToDeviceSpace(CGContextRef c, CGRect rect)`.
    ///
    /// The rectangle unchanged, and the argument is that **the value cannot be
    /// observed**: the only call site passes the answer straight to
    /// `CGContextClipToMask` (`Framework+0x520adbf` into `+0x520ae1a`), which is
    /// a no-op in this same file, and the hidden return slot at `[rbp-0x50]` is
    /// read only to be stored at `[rbp-0x100]`. Converting a rectangle requires
    /// the context's CTM, which belongs to the `__CGContext` another crate owns,
    /// so no conversion is performed and none is claimed.
    unsafe extern "C" fn cg_context_convert_rect_to_device_space(
        _context: *const c_void,
        rect: Rect,
    ) -> Rect {
        rect
    }

    /// `void CGPathAddEllipseInRect(CGMutablePathRef path, ... , CGRect rect)`.
    ///
    /// Nothing, and the prototype is worth recording because it is not the one in
    /// the current header: the guest passes the path in `rdi`, **`0` in `rsi`**,
    /// and the rectangle as 32 bytes on the stack (`Framework+0xbc8ba7a`), which
    /// is three operands rather than the header's `(path, const CGRect *)`. The
    /// 32 bytes are in the red zone and a `call` would clobber them, so they are
    /// this call's stack arguments and not a spill.
    ///
    /// **Reading nothing is what makes both readings safe.** Whether `rsi` is a
    /// null `const CGRect *` or a null `const CGAffineTransform *` - the natural
    /// reading of a null pointer plus a by-value rectangle - this function
    /// dereferences neither and returns nothing, so it is correct under both.
    ///
    /// The loss is recorded rather than hidden: the path would have gained an
    /// ellipse. A `__CGPath` in this runtime holds one bounding rectangle
    /// (`crates/appkit/src/cgcontext.rs`'s `Path`), and the field is in another
    /// crate's layout, so the bounds of an inscribed ellipse - which would be the
    /// rectangle itself, and is therefore *not* hard - cannot be written from
    /// here. The guest's next action is an `objc_msgSend` on the path
    /// (`+0xbc8ba96`), which is unaffected.
    ///
    /// Worth pairing with [`cg_path_is_empty`]: the only caller of that asks about
    /// a path from `CTFontCreatePathForGlyph`, which is built in a different
    /// function, so this path is never the one whose emptiness is tested.
    unsafe extern "C" fn cg_path_add_ellipse_in_rect(_path: *const c_void, _transform: *const c_void) {
        note(
            &said::IMAGE_DATA,
            "CGPathAddEllipseInRect",
            "declining: an ellipse's bounding box is the rectangle it is inscribed in, so \
             the geometry is easy, but the path's bounds field belongs to a __CGPath in \
             another crate and cannot be written from here",
        );
    }

    /// `bool CGPathIsEmpty(CGPathRef path)`.
    ///
    /// **False, and the binary is what makes that true rather than convenient.**
    /// The one call site is `Framework+0x62696e3`, and it is reached only with
    /// the path that `CTFontCreatePathForGlyph` returned - the guest tests the
    /// path for null first and jumps away at `+0x62696d7` otherwise. In this
    /// runtime that function is `CGPathCreateMutable()` followed by
    /// `CGPathAddRect(path, transform, bounds)` (`crates/appkit/src/coretext.rs`),
    /// and `CGPathAddRect` is `extend_path`, which sets the path's bounds
    /// unconditionally. A path with a rectangle added to it is not empty.
    ///
    /// The guest's own two arms are both real: empty and positive width draws an
    /// ellipse in the rectangle (`+0x6269700`), non-empty uses the path. So a
    /// path answered "empty" would replace every glyph outline with an ellipse,
    /// which is why the value is the one the object model supports.
    unsafe extern "C" fn cg_path_is_empty(_path: *const c_void) -> bool {
        false
    }

    /// `CGDirectDisplayID` for a display this host does not have, so
    /// `CGDisplayIsBuiltin` is false for all of them.
    ///
    /// The true answer, not a degraded one: the property is "this display is
    /// built into the Mac", and a display that does not exist is not built into
    /// anything. The guest stores it as a bool (`Framework+0x72cf7f8`,
    /// `setne [rbp-0x38]`) and carries on.
    unsafe extern "C" fn cg_display_is_builtin(_display: u32) -> u32 {
        0
    }

    /// `CGFloat CGDisplayRotation(CGDirectDisplayID display)`.
    ///
    /// Zero degrees, which is the identity and therefore the state of a display
    /// nothing has rotated. The return is a `CGFloat` and the guest truncates
    /// it (`cvttsd2si` at `Framework+0x6a8de82`) and maps 0, 90 and 179 onto
    /// its own rotation enum, with every other value on a fourth arm - so zero is
    /// one of the answers the caller is built to receive, and it is the one that
    /// means "not rotated".
    unsafe extern "C" fn cg_display_rotation(_display: u32) -> f64 {
        0.0
    }

    /// `Boolean CGDisplayUsesForceToGray(CGDirectDisplayID display)`.
    ///
    /// False: no display here renders in grayscale, and the question has no
    /// other answer for a display that is not there. The guest stores the bool
    /// (`Framework+0x6a8ddd6`, `setne [rbp-0xe0]`) and continues.
    unsafe extern "C" fn cg_display_uses_force_to_gray(_display: u32) -> u8 {
        0
    }

    /// `Boolean CGPreflightScreenCaptureAccess(void)`.
    ///
    /// **False, and this is the load-bearing one of the two.** "May this process
    /// capture the screen?" - there is no screen, so no. The guest branches on it
    /// at `Framework+0x6929a0f` (`test al,al; je 0x6929a9d`) and skips the
    /// media-privacy check and the whole capture path when it is false, which is
    /// the correct outcome rather than a loss.
    unsafe extern "C" fn cg_preflight_screen_capture_access() -> u8 {
        note(
            &said::SCREEN_CAPTURE,
            "CGPreflightScreenCaptureAccess",
            "no screen to capture: this host has no window server, so no display, no \
             windows and no screen-recording permission could mean anything; the answer \
             is false and the guest's own not-permitted path is taken",
        );
        0
    }

    /// `Boolean CGRequestScreenCaptureAccess(void)`.
    ///
    /// False, and the reason it is worth refusing loudly is that this function
    /// *asks the user*: it puts up the system's screen-recording prompt. There
    /// is no prompt to put up and nothing to record, so no is the answer and no
    /// is the answer that cannot block a headless start-up on a dialog.
    unsafe extern "C" fn cg_request_screen_capture_access() -> u8 {
        note(
            &said::SCREEN_CAPTURE,
            "CGRequestScreenCaptureAccess",
            "declining: this function's whole effect is to raise the system's \
             screen-recording prompt, and there is no system here to raise it - and no \
             screen for the permission to be about",
        );
        0
    }

    /// `CGSConnectionID CGSMainConnectionID(void)`.
    ///
    /// Zero, which is `kCGNullDirectConnection`: there is no main connection
    /// because there is no window server to have one. The guest passes the answer
    /// to `+[NSGraphicsContext contextWithCGSConnection:options:]`
    /// (`Framework+0xc6df7c1`, selector at `+0xe6ed6f8`) and nothing else.
    unsafe extern "C" fn cgs_main_connection_id() -> c_int {
        note(
            &said::WINDOW_SERVER,
            "CGSMainConnectionID",
            "0, which is kCGNullDirectConnection: this host has no window server, so \
             there is no main CoreGraphics connection to name",
        );
        0
    }

    /// `int CGSSetDenyWindowServerConnections(int deny)`.
    ///
    /// **Success, and the reason is in the guest.** Chromium's sandbox prologue
    /// calls this with `1` at `Framework+0xc8f770f` and **`int3`s on a non-zero
    /// answer** - `+0xc8f7716` is `jne` to `0xc8f7736`, which is `int3; ud2` -
    /// because a sandboxed process that cannot deny window-server connections has
    /// failed its own check. The request is to prevent this process from talking
    /// to the window server; there is no window server; so the state the call
    /// asks for already holds, and the honest answer is that it was established.
    ///
    /// This is the one claim in the file about the world rather than about a
    /// capability, and it is worth being able to defend: it is true for
    /// `deny == 0` as well, since the answer is about there being no connection
    /// to allow either.
    unsafe extern "C" fn cgs_set_deny_window_server_connections(_deny: c_int) -> c_int {
        note(
            &said::WINDOW_SERVER,
            "CGSSetDenyWindowServerConnections",
            "reporting success: the requested state - this process cannot reach a \
             window server - already holds, because there is no window server; the guest \
             asserts on a non-zero answer (Framework+0xc8f7716 is a jump to int3)",
        );
        0
    }

    /// `Boolean CGFontRenderingGetFontSmoothingDisabled(void)`.
    ///
    /// False: nothing here has disabled font smoothing. The guest inverts the
    /// answer and stores it (`xor al,0x1` at `Framework+0xc8901cd`), so false is
    /// the smoothing-enabled state, which is the state a process starts in.
    unsafe extern "C" fn cg_font_rendering_get_font_smoothing_disabled() -> u8 {
        0
    }

    /// `uint64_t CGEventSourceCounterForEventType(CGEventSourceStateID stateID, CGEventType eventType)`.
    ///
    /// Zero events, which is the true count: this host delivers no events of any
    /// type, because the event tap that would deliver them is refused below. The
    /// guest stores the answer as a 32-bit counter (`Framework+0x11367b4`) beside
    /// a seconds-since value that is also zero, and the pair is self-consistent:
    /// no events, and no time since one.
    unsafe extern "C" fn cg_event_source_counter_for_event_type(_state: u32, _event: u32) -> u64 {
        0
    }

    /// `double CGEventSourceSecondsSinceLastEventType(CGEventSourceStateID stateID, CGEventType eventType)`.
    ///
    /// Zero, and the guest's use is what makes it the right value: at
    /// `Framework+0x84fdad1` it truncates the answer to an integer and returns
    /// `seconds >= threshold`, so this is a **staleness test** and not a
    /// duration. Zero is the field's value before any event has been seen, and
    /// it puts the guest on its "this event source is not stale" arm - which is
    /// true of a source that has never seen an event at all. A large invented
    /// number here would be the answer that reports a healthy tap as dead.
    unsafe extern "C" fn cg_event_source_seconds_since_last_event_type(
        _state: u32,
        _event: u32,
    ) -> f64 {
        0.0
    }

    /// `int64_t CGEventGetIntegerValueField(CGEventRef event, CGEventField field)`.
    ///
    /// Zero for every field, and the argument is the guest's own: at
    /// `Framework+0x6aa9b52` it tests the event for null and returns `0`
    /// itself, so "an event that says nothing" is a value this function already
    /// produces. The fields the guest asks for are `kKeyboardEventAutorepeat`
    /// (`0x58`, `+0x6aa9b5a`), `kCGMouseEventScrollWheel` (`0xc`) and
    /// `kCGMouseEventScrollDeltaY` (`0xb`), and the answer is packed into a
    /// scroll-delta word by `imul` at `+0x6aa9b7d`.
    ///
    /// Zero for `kKeyboardEventAutorepeat` is also the *passing* value: the
    /// guest requires it to be zero and returns early otherwise
    /// (`+0x6aa9b64`). The event itself cannot be anything else on this host -
    /// [`cg_event_tap_create`] refuses, and the two `CGEventCreate*` functions
    /// in this file produce no event.
    unsafe extern "C" fn cg_event_get_integer_value_field(_event: *const c_void, _field: u32) -> i64 {
        0
    }

    // ------------------------------------------------- CoreGraphics: no window server

    /// `CGEventTapRef CGEventTapCreate(...)`.
    ///
    /// Null, and this is the feature detection for the whole input stack. An
    /// event tap is a subscription to the window server's event stream; there is
    /// no window server and therefore no stream. The guest null-checks the
    /// answer and keeps the old (null) tap (`Framework+0x692e510`).
    unsafe extern "C" fn cg_event_tap_create(
        _tap: *const c_void,
        _place: u32,
        _options: u32,
        _events_of_interest: u64,
        _callback: *const c_void,
        _user_info: *const c_void,
    ) -> *const c_void {
        note(
            &said::EVENT_TAP,
            "CGEventTapCreate",
            "null: an event tap subscribes to the window server's event stream and this \
             host has no window server; Chromium's EventTapManager treats the null tap \
             as \"no input events\" (Framework+0x692e510)",
        );
        std::ptr::null()
    }

    /// `void CGEventTapEnable(CGEventTapRef tap, bool enable)`.
    ///
    /// Nothing. There is no tap - [`cg_event_tap_create`] refused - so there is
    /// nothing to enable or disable. The guest discards the return
    /// (`Framework+0x692ea72` jumps straight past it).
    unsafe extern "C" fn cg_event_tap_enable(_tap: *const c_void, _enable: bool) {}

    /// `CFDataRef CGEventCreateData(CGEventRef event)`.
    ///
    /// Null, because there is no event to serialise. The guest takes the byte
    /// pointer of the answer (`Framework+0x6aa935d`) and its length
    /// (`+0x6aa9368`), and `CFDataGetBytePtr` and `CFDataGetLength` both answer
    /// null and zero for a null `CFDataRef`, which the guest tests at
    /// `+0x6aa937e`.
    unsafe extern "C" fn cg_event_create_data(_event: *const c_void) -> *const c_void {
        note(
            &said::EVENT_TAP,
            "CGEventCreateData",
            "null: there is no CGEvent on this host to serialise - the event tap that \
             would have produced one is refused, so the CFData is empty and the guest's \
             own length test (Framework+0x6aa937e) ends the walk",
        );
        std::ptr::null()
    }

    /// `CGEventRef CGEventCreateFromData(CGDataRef data)`.
    ///
    /// Null, and the reverse of [`cg_event_create_data`]: the data would have come
    /// from a window server that is not there. The guest tests it
    /// (`Framework+0x6aa94dd`).
    unsafe extern "C" fn cg_event_create_from_data(_data: *const c_void) -> *const c_void {
        std::ptr::null()
    }

    /// `CGSConnectionID CGSDefaultConnection(void)`.
    ///
    /// Zero, like [`cgs_main_connection_id`]: the connection every other private
    /// `CGS*` entry point takes, and there is none. The guest reads the answer
    /// as a 32-bit id (`mov r14d,eax` at `Framework+0x68f8e28`) and passes it
    /// only to the `CGSSetWindow*` functions below, which do nothing with it.
    unsafe extern "C" fn cgs_default_connection() -> c_int {
        0
    }

    /// `void CGSSetWindowAlpha(CGSConnectionID cid, CGSWindowID wid, CGFloat alpha)`.
    ///
    /// Nothing, and the argument is the argument for every `CGSSetWindow*`: each
    /// one is a request to the window server to change a window that belongs to
    /// it. There is no window server and no window, and a non-zero connection id
    /// to send it to would be the invented value to avoid. The return is `void`
    /// and the guest has nothing to check.
    unsafe extern "C" fn cgs_set_window_alpha(_cid: c_int, _wid: u32, _alpha: f64) {
        note(
            &said::WINDOW_SERVER,
            "CGSSetWindowAlpha",
            "declining: setting a window's alpha is a request to the window server, and \
             there is no window server and no window to set",
        );
    }

    /// `void CGSSetWindowTransform(CGSConnectionID cid, CGSWindowID wid, CGAffineTransform transform)`.
    ///
    /// As [`cgs_set_window_alpha`].
    unsafe extern "C" fn cgs_set_window_transform(_cid: c_int, _wid: u32, _transform: Affine) {
        note(
            &said::WINDOW_SERVER,
            "CGSSetWindowTransform",
            "declining: a window transform is applied by the window server, and there is \
             no window server here",
        );
    }

    /// `void CGSSetWindowWarp(CGSConnectionID cid, CGSWindowID wid, CGSSetWindowWarpOptions options, const CGRect *rect)`.
    ///
    /// As [`cgs_set_window_alpha`]. The rectangle is not read: a null `rect` is
    /// legal for this call and dereferencing one the caller did not supply is
    /// the one thing a `void` function here must never do.
    unsafe extern "C" fn cgs_set_window_warp(
        _cid: c_int,
        _wid: u32,
        _options: u32,
        _rect: *const Rect,
    ) {
        note(
            &said::WINDOW_SERVER,
            "CGSSetWindowWarp",
            "declining: warping a window is done by the window server, and there is no \
             window server here",
        );
    }

    // ------------------------------------------- CoreGraphics: no display or windows

    /// `CFArrayRef CGWindowListCopyWindowInfo(CGWindowListOption option, CFRange range)`.
    ///
    /// Null, which is CoreGraphics' documented failure answer and which the guest
    /// has a path for (`test rax,rax; je 0x4595e09` at `Framework+0x45955c7`).
    ///
    /// The alternative - an empty array, which would be true of a window server
    /// with no windows in it - is not available and would not be better: making a
    /// `CFArray` needs `corefoundation`, and a null here says "the query did not
    /// happen" where an empty array would say "the query happened and there is
    /// nothing", and only the first of those is true.
    unsafe extern "C" fn cg_window_list_copy_window_info(_option: u32, _range: i64) -> *const c_void {
        note(
            &said::WINDOW_SERVER,
            "CGWindowListCopyWindowInfo",
            "null: enumerating windows is a query to the window server, and there is no \
             window server - the guest tests the answer and takes its no-window-list \
             path (Framework+0x45955c7)",
        );
        std::ptr::null()
    }

    /// `CFArrayRef CGWindowListCreateDescriptionFromArray(CFArrayRef windowArray)`.
    ///
    /// Null, for [`cg_window_list_copy_window_info`]'s reason: it is the same
    /// window server, and its input is an array this runtime never produces. The
    /// guest passes the answer on rather than dereferencing it.
    unsafe extern "C" fn cg_window_list_create_description_from_array(
        _windows: *const c_void,
    ) -> *const c_void {
        note(
            &said::WINDOW_SERVER,
            "CGWindowListCreateDescriptionFromArray",
            "null: the descriptions are the window server's, and its window list is null",
        );
        std::ptr::null()
    }

    /// `CGImageRef CGWindowListCreateImage(CGRectArrayRef windowBounds, CGWindowImageOption option, CGRect *boundsOut, CGWindowListOption windowList, CGFloat imageBoundsScale, CGWindowOrderingMode windowOrdering)`.
    ///
    /// Null: a screenshot of other processes' windows, which requires the window
    /// server and the screen-recording permission it grants. The guest tests the
    /// answer (`Framework+0x8541a78`) and reports that the capture produced no
    /// image.
    unsafe extern "C" fn cg_window_list_create_image(
        _bounds: *const c_void,
        _option: u32,
        _out: *mut Rect,
        _window_list: u32,
        _scale: f64,
        _ordering: u32,
    ) -> *const c_void {
        note(
            &said::SCREEN_CAPTURE,
            "CGWindowListCreateImage",
            "null: capturing windows needs the window server and the screen-recording \
             permission; the guest tests the answer (Framework+0x8541a78)",
        );
        std::ptr::null()
    }

    /// `CGImageRef CGWindowListCreateImageFromArray(CFArrayRef windowDescriptions, CGRect *boundsOut, CGWindowImageOption option, CGFloat imageBoundsScale, CGWindowOrderingMode windowOrdering)`.
    ///
    /// Null, for [`cg_window_list_create_image`]'s reason. The `boundsOut` slot is
    /// deliberately left alone: there is no image, so there are no bounds, and
    /// writing a rectangle into a caller's out-parameter to describe nothing
    /// would be the answer that looks like a capture.
    unsafe extern "C" fn cg_window_list_create_image_from_array(
        _descriptions: *const c_void,
        _out: *mut Rect,
        _option: u32,
        _scale: f64,
        _ordering: u32,
    ) -> *const c_void {
        std::ptr::null()
    }

    /// `CGImageRef CGDisplayCreateImage(CGDirectDisplayID display)`.
    ///
    /// Null: a photograph of a display, and there is no display. The guest uses
    /// the answer where a screenshot would go and has a path for a null one.
    unsafe extern "C" fn cg_display_create_image(_display: u32) -> *const c_void {
        note(
            &said::DISPLAY,
            "CGDisplayCreateImage",
            "null: there is no display on this host to photograph",
        );
        std::ptr::null()
    }

    /// `CGColorSpaceRef CGDisplayCopyColorSpace(CGDirectDisplayID display)`.
    ///
    /// Null: the colour space of a display is a property of that display's
    /// hardware. The guest tests the answer (`Framework+0x8225d9b`) and falls
    /// back to its own space.
    unsafe extern "C" fn cg_display_copy_color_space(_display: u32) -> *const c_void {
        note(
            &said::DISPLAY,
            "CGDisplayCopyColorSpace",
            "null: a display's colour space is that display's, and this host has no \
             displays; the guest falls back to its own space",
        );
        std::ptr::null()
    }

    /// `CGDisplayStreamRef CGDisplayStreamCreate(CGDirectDisplayID display, CGRectUpdate *sourceRect, CGFloat displayWide, CGFloat displayHigh, uint32_t pixelFormat, CGFloat ioFPS, CFDictionaryRef properties, CFRunLoopRef runloop, CGDisplayStreamClientCallBack callback, void *userInfo)`.
    ///
    /// Null, and the guest's handling of it is the reason this is safe:
    /// `test r14,r14; je 0x3acb894` at `Framework+0x3acb7e2` logs and gives up
    /// before it would call [`cg_display_stream_start`].
    unsafe extern "C" fn cg_display_stream_create(
        _display: u32,
        _source: *const c_void,
        _wide: f64,
        _high: f64,
        _format: u32,
        _fps: f64,
        _properties: *const c_void,
        _run_loop: *const c_void,
        _callback: *const c_void,
        _user_info: *const c_void,
    ) -> *const c_void {
        note(
            &said::DISPLAY,
            "CGDisplayStreamCreate",
            "null: a display stream delivers frames from a display, and this host has no \
             displays; the guest gives up on null (Framework+0x3acb7e2) rather than \
             starting it",
        );
        std::ptr::null()
    }

    /// `CGError CGDisplayStreamStart(CGDisplayStreamRef stream)`.
    ///
    /// `kCGErrorInvalidDisplay` (`12` from `CGError.h`), which is the code that
    /// says the display it was asked about is not one. Unreachable in practice -
    /// [`cg_display_stream_create`] answered null and the guest tests that first
    /// - and the guest checks the answer anyway
    /// (`test eax,eax; je 0x3acb9a2` at `Framework+0x3acb7f0`).
    unsafe extern "C" fn cg_display_stream_start(_stream: *const c_void) -> c_int {
        note(
            &said::DISPLAY,
            "CGDisplayStreamStart",
            "kCGErrorInvalidDisplay: there is no display stream to start, because there \
             is no display to stream",
        );
        12
    }

    /// `CGError CGDisplayStreamStop(CGDisplayStreamRef stream)`.
    ///
    /// As [`cg_display_stream_start`], and for the same reason: nothing is
    /// running, so stopping is not an operation that can succeed.
    unsafe extern "C" fn cg_display_stream_stop(_stream: *const c_void) -> c_int {
        12
    }

    /// `CFRunLoopSourceRef CGDisplayStreamGetRunLoopSource(CGDisplayStreamRef stream)`.
    ///
    /// Null: a run-loop source for a stream that does not exist, and one that
    /// did exist would have to fire, which nothing here can make happen. The
    /// guest adds it to a run loop only after a successful
    /// [`cg_display_stream_start`], so this is not reached.
    unsafe extern "C" fn cg_display_stream_get_run_loop_source(_stream: *const c_void) -> *const c_void {
        note(
            &said::DISPLAY,
            "CGDisplayStreamGetRunLoopSource",
            "null: a run-loop source for a display stream that does not exist; the guest \
             asks for one only after the stream started, and nothing starts here",
        );
        std::ptr::null()
    }

    /// `CGError CGDisplayStreamUpdateGetRects(CGDisplayStreamRef stream, CGRectUpdateInfo info, size_t *numRects)`.
    ///
    /// **The second argument is read as a value and never dereferenced, because
    /// the guest passes the constant 2 there** (`mov esi,0x2` at
    /// `Framework+0x72d3952`) - it is not the `CGRectUpdate **` the public
    /// prototype declares, and writing through it would be a fault at address
    /// two. The binary is the authority on the prototype, so the parameter is an
    /// integer and this function touches only the count.
    ///
    /// `*numRects` is set to zero and `kCGErrorFailure` (`1`) returned: there is
    /// no display stream, so the update cannot be described. The guest reads the
    /// count and tests it (`Framework+0x72d3960`, `test r14,r14; je`) and the
    /// count of rectangles in no update is zero. Unreachable anyway - the
    /// callback this belongs to is one [`cg_display_stream_create`] would have
    /// installed.
    unsafe extern "C" fn cg_display_stream_update_get_rects(
        _stream: *const c_void,
        _info: u32,
        num_rects: *mut usize,
    ) -> c_int {
        if !num_rects.is_null() {
            // SAFETY: the caller's own out-parameter, declared non-null.
            unsafe { num_rects.write(0) };
        }
        1
    }

    // -------------------------------------- CoreGraphics: no PDF codec, no image data

    /// `CGDataConsumerRef CGDataConsumerCreateWithCFData(CFDataRef data)`.
    ///
    /// Null. A data consumer is CoreGraphics' end of a byte stream, and the only
    /// stream this host would give it is a PDF that is not being written. The
    /// guest tests it and abandons the document (`Framework+0x8b566df`).
    unsafe extern "C" fn cg_data_consumer_create_with_cf_data(_data: *const c_void) -> *const c_void {
        note(
            &said::PDF,
            "CGDataConsumerCreateWithCFData",
            "null: nothing here writes a PDF, so there is no byte stream to consume",
        );
        std::ptr::null()
    }

    /// `CGDataProviderRef CGDataProviderCreateWithCFData(CFDataRef data)`.
    ///
    /// Null, for the mirror-image reason: reading a PDF needs a data provider
    /// over a PDF, and there is no PDF. The guest passes the answer straight on
    /// to [`cg_pdf_document_create_with_provider`], which is in this file.
    unsafe extern "C" fn cg_data_provider_create_with_cf_data(_data: *const c_void) -> *const c_void {
        null()
    }

    /// `CGPDFDocumentRef CGPDFDocumentCreateWithProvider(CGDataProviderRef provider)`.
    ///
    /// Null: parsing a PDF is decoding work, and this crate holds no PDF parser.
    /// The caller already has a null provider, so this is the second half of the
    /// same refusal.
    unsafe extern "C" fn cg_pdf_document_create_with_provider(
        _provider: *const c_void,
    ) -> *const c_void {
        note(
            &said::PDF,
            "CGPDFDocumentCreateWithProvider",
            "null: parsing a PDF needs a PDF parser, and this runtime has none; the \
             guest keeps the null document and its page count is zero",
        );
        null()
    }

    /// `size_t CGPDFDocumentGetNumberOfPages(CGPDFDocumentRef document)`.
    ///
    /// **Zero, and this is a real count rather than a refusal.** A page count of
    /// a document that does not exist is zero, and the guest uses it as a loop
    /// bound (`Framework+0x8b56bf9`), so the loop does not run and the guest
    /// concludes the document has no pages - which is the truth about every PDF
    /// on this host.
    unsafe extern "C" fn cg_pdf_document_get_number_of_pages(_document: *const c_void) -> usize {
        0
    }

    /// `CGPDFPageRef CGPDFDocumentGetPage(CGPDFDocumentRef document, size_t pageNumber)`.
    ///
    /// Null, and there is no page to hand back: the document is null and its
    /// count is zero, so no index is in range. The guest tests the answer.
    unsafe extern "C" fn cg_pdf_document_get_page(_document: *const c_void, _page: usize) -> *const c_void {
        null()
    }

    /// `CGRect CGPDFPageGetBoxRect(CGPDFPageRef page, CGPDFBox box, CGRect rect)`.
    ///
    /// **A zero rectangle written through the hidden return pointer, and that is
    /// a refusal rather than a size.** The `rect` argument is the media box a
    /// previous page declared; the answer is the intersection of the two, and
    /// with no page there is no box to intersect it with. The guest reads the
    /// four doubles straight out of the return slot
    /// (`cvttsd2si` at `Framework+0x8b57124`) with no test, so *something* has
    /// to be written - and zero is the only value that is not a page size.
    ///
    /// Unreachable: the page comes from [`cg_pdf_document_get_page`], which is
    /// null, and the document from [`cg_pdf_document_create_with_provider`],
    /// which is null.
    unsafe extern "C" fn cg_pdf_page_get_box_rect(
        _page: *const c_void,
        _box: u32,
        _rect: Rect,
    ) -> Rect {
        note(
            &said::PDF,
            "CGPDFPageGetBoxRect",
            "writing a zero rectangle: there is no PDF page, so there is no media box; the \
             guest reads the return slot without testing it, and a zero box is the only \
             value that is not a fabricated page size",
        );
        Rect::default()
    }

    /// `int CGPDFPageGetRotationAngle(CGPDFPageRef page)`.
    ///
    /// Zero degrees, the identity rotation, for the same reason as the box: there
    /// is no page whose rotation could be anything else. The guest keeps the
    /// answer in `r13d` (`Framework+0x8b56c29`).
    unsafe extern "C" fn cg_pdf_page_get_rotation_angle(_page: *const c_void) -> c_int {
        0
    }

    /// `CGPDFContextRef CGPDFContextCreate(CGDataConsumerRef consumer, CGRect *mediaBox, CFDictionaryRef auxiliaryInfo)`.
    ///
    /// Null, and this is Chromium's *Print to PDF* path. Writing a PDF means
    /// CoreGraphics' PDF engine serialising a drawing; there is no engine here
    /// and no data consumer to write into. The guest tests the answer and takes
    /// its failure arm (`jne 0x8b567f4` at `Framework+0x8b5670c`).
    unsafe extern "C" fn cg_pdf_context_create(
        _consumer: *const c_void,
        _media_box: *const Rect,
        _auxiliary: *const c_void,
    ) -> *const c_void {
        note(
            &said::PDF,
            "CGPDFContextCreate",
            "null: writing a PDF needs CoreGraphics' PDF engine and a data consumer, and \
             this host has neither; the guest reports that the print job could not start",
        );
        null()
    }

    /// `void CGPDFContextClose(CGPDFContextRef context)`.
    ///
    /// Nothing: there is no context to close, and it was never this crate's to
    /// free. Freeing a handle that came from a PDF engine is the one thing a
    /// `void` close here must not do.
    unsafe extern "C" fn cg_pdf_context_close(_context: *const c_void) {}

    /// `void CGContextBeginPage(CGPDFContextRef context, CFDictionaryRef pageInfo)`.
    unsafe extern "C" fn cg_context_begin_page(_context: *const c_void, _info: *const c_void) {}

    /// `void CGContextEndPage(CGPDFContextRef context)`.
    unsafe extern "C" fn cg_context_end_page(_context: *const c_void) {}

    /// `void CGContextDrawPDFPage(CGContextRef c, CGPDFPageRef page)`.
    ///
    /// Nothing: there is no page to draw. The context and the page are both
    /// reached only through the refused PDF constructors above.
    unsafe extern "C" fn cg_context_draw_pdf_page(_context: *const c_void, _page: *const c_void) {}

    /// `void CGContextRotateCTM(CGContextRef c, CGFloat angle)`.
    ///
    /// Nothing, and this is the one `CGContext` mutator in the file. A CTM
    /// belongs to a `__CGContext` that `crates/appkit` owns and lays out; this
    /// crate has no way to read the matrix it would have to rotate, and a
    /// plausible rotation of a matrix it cannot see is exactly the invented
    /// value to avoid. The context the guest is drawing a PDF into is the one
    /// [`cg_pdf_context_create`] refused to create.
    unsafe extern "C" fn cg_context_rotate_ctm(_context: *const c_void, _angle: f64) {
        note(
            &said::CTM,
            "CGContextRotateCTM",
            "declining: rotating the CTM needs the matrix, which belongs to a \
             __CGContext that another crate owns; the context in question is a PDF \
             context that was never created",
        );
    }

    /// `void CGContextSetShouldSubpixelPositionFonts(CGContextRef c, bool shouldSubpixelPosition)`.
    ///
    /// Nothing, for the same reason as [`cg_context_rotate_ctm`], with one extra
    /// thing in its favour: the flag's state is *already* what this leaves it.
    /// The context is fresh, a fresh context has no subpixel positioning, and
    /// setting a flag on a text-rendering engine this host does not have cannot
    /// make it have one.
    ///
    /// `void` and the guest discards everything (`Framework+0x626943e` and
    /// `+0x62696e3`'s function set four of these and reads no result), so there
    /// is no value here that could be wrong - only the rendering difference,
    /// which is the honest one: this runtime does not position glyphs
    /// subpixel-accurately.
    unsafe extern "C" fn cg_context_set_should_subpixel_position_fonts(
        _context: *const c_void,
        _should: bool,
    ) {
    }

    /// `void CGContextSetShouldSubpixelQuantizeFonts(CGContextRef c, bool shouldSubpixelQuantize)`.
    unsafe extern "C" fn cg_context_set_should_subpixel_quantize_fonts(
        _context: *const c_void,
        _should: bool,
    ) {
    }

    /// `void CGContextSetAllowsFontSubpixelPositioning(CGContextRef c, bool allows)`.
    unsafe extern "C" fn cg_context_set_allows_font_subpixel_positioning(
        _context: *const c_void,
        _allows: bool,
    ) {
    }

    /// `void CGContextSetAllowsFontSubpixelQuantization(CGContextRef c, bool allows)`.
    unsafe extern "C" fn cg_context_set_allows_font_subpixel_quantization(
        _context: *const c_void,
        _allows: bool,
    ) {
    }

    /// `CGImageRef CGBitmapContextCreateImage(CGBitmapContextRef context)`.
    ///
    /// Null: reading a context's pixels back into an image is image work, and the
    /// only context this file is asked about is a PDF one that was never
    /// created. The guest keeps the image it already had
    /// (`cmovne r12,r13` at `Framework+0x72d0ddd`).
    unsafe extern "C" fn cg_bitmap_context_create_image(_context: *const c_void) -> *const c_void {
        note(
            &said::IMAGE_DATA,
            "CGBitmapContextCreateImage",
            "null: making an image out of a context's pixels needs an image this runtime \
             does not build; the guest keeps the image it had",
        );
        null()
    }

    /// `CGImageRef CGImageCreateWithImageInRect(CGImageRef image, CGRect rect)`.
    ///
    /// Null: cropping an image is image work - it copies and re-strides the
    /// source's pixels. The guest tests the answer
    /// (`test rax,rax; je 0x4591461` at `Framework+0x4591450`) and keeps the
    /// uncropped image.
    unsafe extern "C" fn cg_image_create_with_image_in_rect(
        _image: *const c_void,
        _rect: Rect,
    ) -> *const c_void {
        note(
            &said::IMAGE_DATA,
            "CGImageCreateWithImageInRect",
            "null: cropping an image means copying and re-striding its pixels, and there \
             is no image implementation here; the guest keeps the image it had",
        );
        null()
    }

    /// `CGImageRef CGImageMaskCreate(CGDataProviderRef provider, const CGRect *rect, CGRect decodingRect, bool shouldDecode)`.
    ///
    /// Null, and this is a text mask: the provider is the glyph coverage data
    /// CoreText would supply, and CoreText here is a dictionary of attributes
    /// rather than an outline rasteriser. The guest branches on the answer
    /// before it clips to it (`test r15,r15; je` at `Framework+0x520add8`), which
    /// is the path that does not draw.
    unsafe extern "C" fn cg_image_mask_create(
        _provider: *const c_void,
        _rect: *const Rect,
        _decoding: Rect,
        _should_decode: bool,
    ) -> *const c_void {
        note(
            &said::IMAGE_DATA,
            "CGImageMaskCreate",
            "null: a mask is glyph coverage rasterised from outlines, and this runtime's \
             CoreText holds attributes rather than outlines",
        );
        null()
    }

    /// `void CGContextSetFont(CGContextRef c, CGFontRef font)`.
    ///
    /// Nothing, and the `CGFontRef` it is handed is null:
    /// [`cg_font_create_with_data_provider`] refused it, and the guest passes it
    /// straight on at `Framework+0x520bc8f` with no test. That is the safe
    /// ordering - a `void` function that reads its argument would fault on the
    /// null font, so it reads nothing.
    ///
    /// Recording the loss rather than hiding it: the guest's text path sets a
    /// font, sets a size and calls [`cg_context_show_glyphs_at_positions`], and
    /// on this host none of the three draws anything.
    unsafe extern "C" fn cg_context_set_font(_context: *const c_void, _font: *const c_void) {
        note(
            &said::FONT_CONTENTS,
            "CGContextSetFont",
            "declining: the CGFontRef is null, because the font data it would name is \
             not something this runtime can read; nothing is set, and the text that \
             follows draws nothing",
        );
    }

    /// `void CGContextShowGlyphsAtPositions(CGContextRef c, const CGGlyph *glyphs, const CGPoint *positions, size_t count)`.
    ///
    /// Nothing. The glyph array and the positions are read from the argument area
    /// by the caller that pushed them and are not read here: the context's font is
    /// null ([`cg_context_set_font`]) and CoreGraphics would have no glyph
    /// outlines to draw with, so reading the arrays could only be a way to fault.
    unsafe extern "C" fn cg_context_show_glyphs_at_positions(
        _context: *const c_void,
        _glyphs: *const u16,
        _positions: *const c_void,
        _count: usize,
    ) {
    }

    /// `void CGContextClipToMask(CGContextRef c, CGRect rect, CGImageRef mask)`.
    ///
    /// Nothing, for the same reason as
    /// [`cg_context_convert_rect_to_device_space`]'s answer: the mask is
    /// [`cg_image_mask_create`]'s null and the rectangle is the one that function
    /// was handed, and clipping to a null mask clips to nothing. The guest has a
    /// separate arm for a null mask (`Framework+0x520add8`).
    unsafe extern "C" fn cg_context_clip_to_mask(
        _context: *const c_void,
        _rect: Rect,
        _mask: *const c_void,
    ) {
    }

    /// `CFDataRef CGDataProviderCopyData(CGDataProviderRef provider)`.
    ///
    /// Null: copying a provider's bytes is reading whatever the provider wraps -
    /// a file, a `CFData`, an encoded image - and this runtime holds none of
    /// them. The guest takes the byte pointer of the answer
    /// (`CFDataGetBytePtr` at `Framework+0x4597d63`), which answers null for a
    /// null `CFDataRef`, and the length, which answers zero.
    unsafe extern "C" fn cg_data_provider_copy_data(_provider: *const c_void) -> *const c_void {
        note(
            &said::IMAGE_DATA,
            "CGDataProviderCopyData",
            "null: a data provider's bytes are whatever it wraps, and nothing on this host \
             wraps anything; the guest's CFDataGetBytePtr/CFDataGetLength both answer \
             null and zero for the null answer",
        );
        null()
    }

    /// `size_t CGImageGetBitsPerPixel(CGImageRef image)`.
    ///
    /// **Zero bits per pixel, and that is a refusal shaped like a number.** The
    /// real answer belongs to the image's own layout, and a `__CGImage` in this
    /// tree is not described anywhere `darwin` can see. Zero is the width of no
    /// pixels: it is the one value that cannot be mistaken for a real geometry,
    /// and the guest's own answer for "no image" is the same, because every path
    /// that would have produced one is refused above.
    unsafe extern "C" fn cg_image_get_bits_per_pixel(_image: *const c_void) -> usize {
        note(
            &said::IMAGE_GEOMETRY,
            "CGImageGetBitsPerPixel",
            "0, meaning an image with no pixels rather than an image of some width: the \
             real figure belongs to a __CGImage layout this crate does not have",
        );
        0
    }

    /// `size_t CGImageGetBytesPerRow(CGImageRef image)`.
    ///
    /// As [`cg_image_get_bits_per_pixel`]: zero is the length of no row.
    unsafe extern "C" fn cg_image_get_bytes_per_row(_image: *const c_void) -> usize {
        0
    }

    /// `CGBitmapInfo CGImageGetBitmapInfo(CGImageRef image)`.
    ///
    /// As [`cg_image_get_bits_per_pixel`]. Zero is a legal `CGBitmapInfo` and it
    /// says "no alpha, no byte order, no bitmap" - which is the description of
    /// no image, and the only reading that is not a claim about a real one's
    /// layout.
    unsafe extern "C" fn cg_image_get_bitmap_info(_image: *const c_void) -> u32 {
        0
    }

    /// `CGDataProviderRef CGImageGetDataProvider(CGImageRef image)`.
    ///
    /// Null: an image's provider is where its encoded bytes live, and there are
    /// none. The guest passes the answer to [`cg_data_provider_copy_data`], which
    /// is in this file and refuses the same way.
    unsafe extern "C" fn cg_image_get_data_provider(_image: *const c_void) -> *const c_void {
        null()
    }

    /// `CGFontRef CGFontCreateWithDataProvider(CGDataProviderRef provider, CGFloat size, const CGAffineTransform *matrix)`.
    ///
    /// Null, and this is the load-bearing refusal of the text path: a `CGFontRef`
    /// is CoreGraphics' own view of a font, built from font data, and this
    /// runtime's CoreText is a dictionary of attributes
    /// (`crates/appkit/src/coretext.rs`) with no `CGFont` behind it at all. A
    /// fabricated `CGFontRef` would be dereferenced by `CGContextSetFont` in
    /// CoreGraphics proper, and nothing here could service that.
    ///
    /// The guest returns the answer to its caller
    /// (`mov rax,r14` at `Framework+0x520b44b`) rather than dereferencing it.
    unsafe extern "C" fn cg_font_create_with_data_provider(
        _provider: *const c_void,
        _size: f64,
        _matrix: *const Affine,
    ) -> *const c_void {
        note(
            &said::FONT_CONTENTS,
            "CGFontCreateWithDataProvider",
            "null: a CGFontRef is CoreGraphics' own view of a font and this runtime has \
             none - its CoreText is a dictionary of attributes; the guest passes the null \
             on rather than dereferencing it",
        );
        null()
    }

    /// `void CGFontRelease(CGFontRef font)`.
    ///
    /// Nothing to release: this crate never allocated a `CGFontRef`, and the one
    /// it is handed is null. Freeing a font that came from CoreGraphics' own
    /// allocator is the one thing a `void` release here must never do.
    unsafe extern "C" fn cg_font_release(_font: *const c_void) {}

    /// `uint32_t CGFontGetUnitsPerEm(CGFontRef font)`.
    ///
    /// Zero font units per em, and it is a refusal shaped like a number: the real
    /// figure is a property of a font file, and there is no font file behind the
    /// null `CGFontRef` this is called with. Zero is what "there is no em" means
    /// in the same units the answer is counted in.
    ///
    /// The guest returns the answer straight out of its own function
    /// (`mov r14d,eax` at `Framework+0x626d2cd`) with no arithmetic on it, so
    /// there is no division here to be a divide-by-zero.
    unsafe extern "C" fn cg_font_get_units_per_em(_font: *const c_void) -> u32 {
        note(
            &said::FONT_CONTENTS,
            "CGFontGetUnitsPerEm",
            "0 font units: the em of a font that does not exist, rather than a \
             fabricated units-per-em that would scale every glyph by a made-up factor",
        );
        0
    }

    /// `CGGlyph CGFontGetGlyphWithGlyphName(CGFontRef font, CFStringRef name)`.
    ///
    /// Glyph 0, which is `.notdef` - the glyph CoreGraphics returns for a name a
    /// font does not have. The name is a `CFString` and the font is null, so
    /// neither the font's glyph table nor the string can be read from here, and
    /// the answer that does not name a real glyph is `.notdef`.
    unsafe extern "C" fn cg_font_get_glyph_with_glyph_name(
        _font: *const c_void,
        _name: *const c_void,
    ) -> u16 {
        note(
            &said::FONT_CONTENTS,
            "CGFontGetGlyphWithGlyphName",
            "0, which is .notdef: the glyph is named by the font's own glyph table and \
             this host has no font, so no other glyph can be named truthfully",
        );
        0
    }

    /// `CFDataRef CGFontCopyTableForTag(CGFontRef font, CGFontTableTag tag)`.
    ///
    /// Null: a font table is a block of bytes out of a font file, and there is no
    /// font file. The guest copies a table to read a font's `head` or `cmap`
    /// table by hand, and there is nothing to read.
    unsafe extern "C" fn cg_font_copy_table_for_tag(_font: *const c_void, _tag: u32) -> *const c_void {
        null()
    }

    // ------------------------------------------------- CoreGraphics: dictionaries

    /// `CFDictionaryRef CGRectCreateDictionaryRepresentation(CGRect rect)`.
    ///
    /// Null, and the reason is a dependency rather than a difficulty: the answer
    /// is a dictionary of four `CFNumber`s keyed `"X"`, `"Y"`, `"Width"` and
    /// `"Height"`, and making a `CFNumber` means calling into `corefoundation`,
    /// which `darwin` does not depend on and must not.
    ///
    /// The guest handles the null exactly as it would handle any other: it tests
    /// the answer and skips the `CFRelease` (`test r12,r12; je 0x3acb779` at
    /// `Framework+0x3acb76c`), and the dictionary is not one of the five pairs it
    /// builds for `CGDisplayStreamCreate` besides.
    unsafe extern "C" fn cg_rect_create_dictionary_representation(_rect: Rect) -> *const c_void {
        note(
            &said::CF_CONTAINER,
            "CGRectCreateDictionaryRepresentation",
            "null: the answer is a dictionary of four CFNumbers, and darwin cannot make a \
             CFNumber - it has no corefoundation dependency; the guest skips its release \
             of the null (Framework+0x3acb76c)",
        );
        null()
    }

    /// `bool CGRectMakeWithDictionaryRepresentation(CFDictionaryRef dict, CGRect *result)`.
    ///
    /// False, and `*result` is deliberately **not** written. The answer is four
    /// `CFNumber`s read back out of the dictionary, which is the same missing
    /// dependency as [`cg_rect_create_dictionary_representation`].
    ///
    /// False is the right refusal value rather than a guess: it is the value
    /// CoreGraphics itself returns for a dictionary that is not a rectangle
    /// representation. The guest tests it (`test al,al; je 0x4591361` at
    /// `Framework+0x4591316`) and zeroes the rectangle itself, so returning false
    /// produces a defined result rather than an unread one. At the second call
    /// site (`+0x3aced78`) it stores the bool and carries on.
    unsafe extern "C" fn cg_rect_make_with_dictionary_representation(
        _dictionary: *const c_void,
        _result: *mut Rect,
    ) -> bool {
        note(
            &said::CF_CONTAINER,
            "CGRectMakeWithDictionaryRepresentation",
            "false: reading the answer means reading four CFNumbers out of a \
             dictionary, and darwin cannot read a CFNumber; false is also what \
             CoreGraphics returns for a dictionary that is not a rectangle, and the \
             guest zeroes the rectangle itself on it",
        );
        false
    }

    /// `CGColorSpaceRef CGColorSpaceCopyName(CGColorSpaceRef space)`.
    ///
    /// Null: a colour space's name is a property of the profile it was made
    /// from. The colour spaces in this runtime are `ColorSpace` objects from
    /// `crates/appkit` that carry no profile and no name, so there is no name to
    /// copy. The guest tests the answer
    /// (`test rax,rax; je 0x6237168` at `Framework+0x6237146`).
    unsafe extern "C" fn cg_color_space_copy_name(_space: *const c_void) -> *const c_void {
        note(
            &said::CF_CONTAINER,
            "CGColorSpaceCopyName",
            "null: a colour space's name comes from the profile behind it, and the colour \
             spaces in this runtime have no profile; the guest falls through to its ICC \
             path",
        );
        null()
    }

    /// `CFDataRef CGColorSpaceCopyICCData(CGColorSpaceRef space)`.
    ///
    /// Null, and this is the same absence one level down: the ICC profile *is*
    /// the colour space's description, and the `ColorSpace` objects here have
    /// none. Copying the ~500 bytes the guest reserves for a header
    /// (`mov edx,0x3f8` at `Framework+0x6237186`) would mean writing a profile
    /// this runtime has never seen. The guest tests the answer
    /// (`+0x6237170`).
    unsafe extern "C" fn cg_color_space_copy_icc_data(_space: *const c_void) -> *const c_void {
        null()
    }

    /// `CGColorSpaceRef CGColorSpaceCreateICCBased(CFDataRef iccData)`.
    ///
    /// Null, and this one has an additional reason on top of the missing profile:
    /// the guest hands it `CGColorSpaceCopyICCData`'s answer, which is null, so
    /// there is no data to build a space from even in principle. The guest tests
    /// the answer and keeps whatever space it had
    /// (`test rax,rax; je 0x623709e` at `Framework+0x623708b`).
    unsafe extern "C" fn cg_color_space_create_icc_based(_data: *const c_void) -> *const c_void {
        null()
    }

    // --------------------------------------------------------------- CoreText

    /// `CFTypeID CTFontDescriptorGetTypeID(void)`.
    ///
    /// **Computed, and the binary says what it has to be.** In this runtime a
    /// `CTFontDescriptorRef` **is** a `CFDictionary` of attributes:
    /// `crates/appkit/src/coretext.rs` builds every descriptor with
    /// `collections::new_dictionary`, and `CTFontDescriptorCreateWithAttributes`
    /// (line 764) is a retain of the dictionary it is handed.
    /// `corefoundation::base::type_id` defines a `CFTypeID` as the class pointer,
    /// and `CFDictionaryGetTypeID()` is `type_id_of(dictionary_class())` - which
    /// walks up to `NSDictionary`, the class
    /// `collections::dictionary_class` notes as the CoreFoundation type. So the
    /// type id a descriptor reports on this host is `ptr(NSDictionary)`, and that
    /// is what this returns.
    ///
    /// **The guest needs this to be right, and the reason is an assertion.** Its
    /// helper at `Framework+0x5d933c0` does
    /// `CFGetTypeID(descriptor) == CTFontDescriptorGetTypeID()` and **`int3`s if
    /// they disagree while the descriptor is non-null** (`+0x5d933fe`). The
    /// corroboration from the guest's own code is at `+0x626ba38`, where it
    /// compares `CFGetTypeID` against `CFDictionaryGetTypeID()` for a traits
    /// attribute - the same test, for the same reason.
    ///
    /// A value that could not be `ptr(NSDictionary)` would put the guest on that
    /// `int3` the first time a real descriptor reached it, which is the
    /// difference between a computed answer and a plausible one.
    unsafe extern "C" fn ct_font_descriptor_get_type_id() -> u64 {
        match type_id_of(c"NSDictionary") {
            Some(id) => id,
            None => {
                note(
                    &said::FONT_TYPE,
                    "CTFontDescriptorGetTypeID",
                    "0: NSDictionary is not registered, and on this host a CTFontDescriptor \
                     *is* a dictionary of attributes, so there is no other type id to report; \
                     the guest asserts on a mismatch (Framework+0x5d933fe is an int3)",
                );
                0
            }
        }
    }

    /// `Boolean CTFontDescriptorIsSystemUIFont(CTFontDescriptorRef descriptor)`.
    ///
    /// **False, and the direction is chosen: false is the answer that cannot be
    /// wrong in the dangerous direction.** The question is whether a descriptor
    /// names one of the system UI fonts, and the way to answer it is to compare
    /// the descriptor's family against the system UI font list - which is a
    /// `CFString` in a `CFDictionary` this crate cannot read. True would claim a
    /// system font that was never identified; false claims only that none was.
    ///
    /// It is also exactly true for the null descriptor, which is reachable and is
    /// not: `CTFontCopyFontDescriptor` is called at `Framework+0x694ab7e` and its
    /// answer is passed here **without a test**, and
    /// `crates/appkit/src/coretext.rs:383` returns null for a null font. There
    /// is no font, so there is no system UI font, and the guest stores the bool
    /// (`mov r13d,eax` at `+0x694ab8e`).
    unsafe extern "C" fn ct_font_descriptor_is_system_ui_font(_descriptor: *const c_void) -> bool {
        note(
            &said::FONT_CONTENTS,
            "CTFontDescriptorIsSystemUIFont",
            "false: identifying the system UI font means comparing the descriptor's family \
             against Apple's list, and the family is a CFString inside a CFDictionary this \
             crate cannot read; false is the direction that never claims a system font. \
             For the null descriptor the guest can pass it is exactly right",
        );
        false
    }

    /// `CTFontCollectionRef CTFontCollectionCreateFromAvailableFonts(void)`.
    ///
    /// Null: a collection of every font CoreText can see, and CoreText here sees
    /// a dictionary of attributes per font rather than a font service. The guest
    /// tests the answer.
    unsafe extern "C" fn ct_font_collection_create_from_available_fonts() -> *const c_void {
        note(
            &said::FONT_CONTENTS,
            "CTFontCollectionCreateFromAvailableFonts",
            "null: there is no font service to enumerate; this runtime's CoreText is a \
             dictionary of attributes per font",
        );
        null()
    }

    /// `CFArrayRef CTFontCollectionCreateMatchingFontDescriptors(CTFontCollectionRef collection, CFDictionaryRef options)`.
    ///
    /// Null, for [`ct_font_collection_create_from_available_fonts`]'s reason: no
    /// collection means no matching descriptors.
    ///
    /// The guest does **not** test this one before using it - at
    /// `Framework+0x6266941` it passes the answer straight to
    /// `CFArrayGetCount` - and that is safe, because `CFArrayGetCount` answers
    /// zero for a null array
    /// (`crates/corefoundation/src/api.rs:1293`, `map_or(0, ..)`), and the guest's
    /// own `test rax,rax; jle` then takes the "no fonts" arm. The other call site
    /// (`+0x1c62e59`) tests it and jumps away on null.
    unsafe extern "C" fn ct_font_collection_create_matching_font_descriptors(
        _collection: *const c_void,
        _options: *const c_void,
    ) -> *const c_void {
        note(
            &said::FONT_CONTENTS,
            "CTFontCollectionCreateMatchingFontDescriptors",
            "null: no collection, so no matching descriptors; the guest's CFArrayGetCount \
             answers zero for the null (Framework+0x6266941) and its other site tests the \
             answer",
        );
        null()
    }

    /// `CFArrayRef CTFontCollectionCreateMatchingFontDescriptorsForFamily(CTFontCollectionRef collection, CFStringRef familyName, CFDictionaryRef options)`.
    ///
    /// Null, for [`ct_font_collection_create_matching_font_descriptors`]'s reason.
    /// The guest tests it (`test rax,rax; je 0xa58d33c` at
    /// `Framework+0xa58ce28`).
    unsafe extern "C" fn ct_font_collection_create_matching_font_descriptors_for_family(
        _collection: *const c_void,
        _family: *const c_void,
        _options: *const c_void,
    ) -> *const c_void {
        null()
    }

    /// `CFArrayRef CTFontCopyAvailableTables(CTFontRef font)`.
    ///
    /// Null: the list of tables in a font file, and there is no font file. The
    /// guest copies a table to read `head` or `cmap` by hand; with no list there
    /// is nothing to read and the loop that would walk it does not run.
    unsafe extern "C" fn ct_font_copy_available_tables(_font: *const c_void) -> *const c_void {
        note(
            &said::FONT_CONTENTS,
            "CTFontCopyAvailableTables",
            "null: a font's tables are blocks of a font file, and there is no font file \
             behind a font here",
        );
        null()
    }

    /// `CFCharacterSetRef CTFontCopyCharacterSet(CTFontRef font)`.
    ///
    /// Null: the characters a font can draw, which is the font's `cmap` table.
    /// The guest tests the answer (`test rax,rax; je 0x626c2c7` at
    /// `Framework+0x626c1e7`) and draws a space where the character set would
    /// have been.
    unsafe extern "C" fn ct_font_copy_character_set(_font: *const c_void) -> *const c_void {
        null()
    }

    /// `CFStringRef CTFontCopyFullName(CTFontRef font)`.
    ///
    /// Null: a font's full name is a `name` table entry, and there is no font
    /// file. The guest's first call site tests the answer
    /// (`test rax,rax; je 0x626dafb` at `Framework+0x626dae9`).
    unsafe extern "C" fn ct_font_copy_full_name(_font: *const c_void) -> *const c_void {
        null()
    }

    /// `CFStringRef CTFontCopyLocalizedName(CTFontRef font, CFStringRef nameKey)`.
    ///
    /// Null: a name in the user's language, which is a localised `name` table
    /// entry. The guest uses it to show a font's name in a font list, and gets
    /// no name rather than an invented one.
    unsafe extern "C" fn ct_font_copy_localized_name(
        _font: *const c_void,
        _key: *const c_void,
    ) -> *const c_void {
        null()
    }

    /// `CFTypeRef CTFontCopyVariation(CTFontRef font)`.
    ///
    /// Null: a font's `fvar` table, which is how a variable font's axis
    /// settings are named. There is no font file, and a variation dictionary
    /// would have to carry the font's own axis identifiers to be worth anything.
    unsafe extern "C" fn ct_font_copy_variation(_font: *const c_void) -> *const c_void {
        note(
            &said::FONT_CONTENTS,
            "CTFontCopyVariation",
            "null: a font's variation dictionary comes from its fvar table, and there is \
             no font file here to have one",
        );
        null()
    }

    /// `CFArrayRef CTFontCopyVariationAxes(CTFontRef font)`.
    ///
    /// Null, for [`ct_font_copy_variation`]'s reason.
    unsafe extern "C" fn ct_font_copy_variation_axes(_font: *const c_void) -> *const c_void {
        null()
    }

    /// `CFArrayRef CTFontCopyVariationAxesInternal(CTFontRef font)`.
    ///
    /// Null, for [`ct_font_copy_variation`]'s reason. The name says what it is:
    /// the same axes as [`ct_font_copy_variation_axes`] plus the ones a font does
    /// not publicly declare, and the private half of a `fvar` table is still a
    /// `fvar` table.
    unsafe extern "C" fn ct_font_copy_variation_axes_internal(_font: *const c_void) -> *const c_void {
        null()
    }

    /// `CTFontRef CTFontCreateCopyWithSymbolicTraits(CTFontRef original, CGFloat size, CGAffineTransform matrix, CTFontSymbolicTraits symTraitValue, CTFontSymbolicTraits symTraitMask)`.
    ///
    /// Null, and this is a refusal rather than an empty font: a copy with
    /// different traits is the *same face* with a different weight or slant
    /// applied, so answering with anything but a real face would draw a different
    /// letterform than the guest asked for.
    ///
    /// The guest tests it - `Framework+0x62688a0`'s caller and
    /// `+0xa58bce0+0x243` - and takes its no-alternate-face path.
    unsafe extern "C" fn ct_font_create_copy_with_symbolic_traits(
        _original: *const c_void,
        _size: f64,
        _matrix: *const Affine,
        _traits_value: u32,
        _traits_mask: u32,
    ) -> *const c_void {
        note(
            &said::FONT_CONTENTS,
            "CTFontCreateCopyWithSymbolicTraits",
            "null: this is the same face with different weight or slant applied, so any \
             answer other than a real face would draw a different letterform than the guest \
             asked for",
        );
        null()
    }

    /// `CTFontRef CTFontCreateForString(CTFontRef font, CFStringRef string, CFRange range)`.
    ///
    /// Null: "the font that would draw this string" is a fallback chain walk over
    /// the font service, and there is no font service. The guest tests the
    /// answers (three sites, `Framework+0x62668e0`, `+0xa58aff0` twice and once
    /// more) and falls back to the font it already has.
    unsafe extern "C" fn ct_font_create_for_string(
        _font: *const c_void,
        _string: *const c_void,
        _range: i64,
    ) -> *const c_void {
        note(
            &said::FONT_CONTENTS,
            "CTFontCreateForString",
            "null: choosing the font for a string is a walk of the fallback chain over the \
             font service, and there is no font service here",
        );
        null()
    }

    /// `CFDictionaryRef CTFontDescriptorCopyAttributes(CTFontDescriptorRef descriptor)`.
    ///
    /// Null, and note what it is *not* refusing: on this runtime the descriptor
    /// is already a dictionary of attributes, so the honest answer is "the
    /// descriptor". This file cannot return it, because it cannot retain a
    /// `CFDictionary` it did not create and cannot tell one apart from anything
    /// else - `CFDictionaryCreate` and `CFRetain` both live in
    /// `corefoundation`. Null is the documented answer for a descriptor there is
    /// none of, and the guest tests it.
    unsafe extern "C" fn ct_font_descriptor_copy_attributes(_descriptor: *const c_void) -> *const c_void {
        note(
            &said::CF_CONTAINER,
            "CTFontDescriptorCopyAttributes",
            "null: on this host a descriptor *is* its attribute dictionary, but darwin \
             cannot retain or re-wrap a CFDictionary it did not create - corefoundation has \
             both CFRetain and CFDictionaryCreate and darwin depends on neither",
        );
        null()
    }

    /// `CFTypeRef CTFontDescriptorCopyLocalizedAttribute(CTFontDescriptorRef descriptor, CFStringRef attribute, CFStringRef language)`.
    ///
    /// Null, for the same reason as
    /// [`ct_font_descriptor_copy_attributes`] with the attribute lookup on top:
    /// the value is a `CFString` in a `CFDictionary`. The guest tests the answers
    /// and shows no localised name.
    unsafe extern "C" fn ct_font_descriptor_copy_localized_attribute(
        _descriptor: *const c_void,
        _attribute: *const c_void,
        _language: *const c_void,
    ) -> *const c_void {
        null()
    }

    /// `uint16_t CTFontGetGlyphCount(CTFontRef font)`.
    ///
    /// **Zero, and the guest's own code is what makes it safe.**
    /// `Framework+0x626c1aa` computes `count * 4` and tests *that* for null
    /// before it loops (`test r12,r12; je 0x626c1df`), so a zero count skips the
    /// glyph array entirely and the guest draws no glyphs from this font - which
    /// is what it would do for a font that has none, and what
    /// [`ct_font_create_for_string`]'s null has already arranged.
    unsafe extern "C" fn ct_font_get_glyph_count(_font: *const c_void) -> u16 {
        note(
            &said::FONT_CONTENTS,
            "CTFontGetGlyphCount",
            "0: a glyph count is the size of a font's cmap table, and the font is the null \
             from CTFontCreateForString; the guest's own count*4 test (Framework+0x626c1b2) \
             skips the glyph loop",
        );
        0
    }

    /// A `NULL` data or object reference, for the refusals above.
    ///
    /// One function so that the dozens of `std::ptr::null()` returns are one
    /// decision rather than dozens: a null object reference is what every one of
    /// these frameworks documents for "there is no such object here".
    fn null() -> *const c_void {
        std::ptr::null()
    }

    exports::exports! {
        functions {
            // CoreGraphics: rectangle predicates, computed.
            "CGRectEqualToRect" => cg_rect_equal_to_rect,
            "CGRectIsEmpty" => cg_rect_is_empty,
            "CGRectIsNull" => cg_rect_is_null,

            // CoreGraphics: the type id a `__CGColor` reports.
            "CGColorGetTypeID" => cg_color_get_type_id,

            // CoreGraphics: one colour space, so the conversion is the identity.
            "CGColorCreateCopyByMatchingToColorSpace" => cg_color_create_copy_by_matching_to_color_space,
            "CGContextConvertRectToDeviceSpace" => cg_context_convert_rect_to_device_space,
            "CGPathIsEmpty" => cg_path_is_empty,
            "CGPathAddEllipseInRect" => cg_path_add_ellipse_in_rect,

            // CoreGraphics: true negatives of a display that is not there.
            "CGDisplayIsBuiltin" => cg_display_is_builtin,
            "CGDisplayRotation" => cg_display_rotation,
            "CGDisplayUsesForceToGray" => cg_display_uses_force_to_gray,
            "CGPreflightScreenCaptureAccess" => cg_preflight_screen_capture_access,
            "CGRequestScreenCaptureAccess" => cg_request_screen_capture_access,
            "CGSMainConnectionID" => cgs_main_connection_id,
            "CGSSetDenyWindowServerConnections" => cgs_set_deny_window_server_connections,
            "CGFontRenderingGetFontSmoothingDisabled" => cg_font_rendering_get_font_smoothing_disabled,

            // CoreGraphics: the event source, which has seen nothing.
            "CGEventSourceCounterForEventType" => cg_event_source_counter_for_event_type,
            "CGEventSourceSecondsSinceLastEventType" => cg_event_source_seconds_since_last_event_type,
            "CGEventGetIntegerValueField" => cg_event_get_integer_value_field,
            "CGEventTapCreate" => cg_event_tap_create,
            "CGEventTapEnable" => cg_event_tap_enable,
            "CGEventCreateData" => cg_event_create_data,
            "CGEventCreateFromData" => cg_event_create_from_data,

            // CoreGraphics: the private window-server calls.
            "_CGSDefaultConnection" => cgs_default_connection,
            "CGSSetWindowAlpha" => cgs_set_window_alpha,
            "CGSSetWindowTransform" => cgs_set_window_transform,
            "CGSSetWindowWarp" => cgs_set_window_warp,

            // CoreGraphics: windows and displays.
            "CGWindowListCopyWindowInfo" => cg_window_list_copy_window_info,
            "CGWindowListCreateDescriptionFromArray" => cg_window_list_create_description_from_array,
            "CGWindowListCreateImage" => cg_window_list_create_image,
            "CGWindowListCreateImageFromArray" => cg_window_list_create_image_from_array,
            "CGDisplayCreateImage" => cg_display_create_image,
            "CGDisplayCopyColorSpace" => cg_display_copy_color_space,
            "CGDisplayStreamCreate" => cg_display_stream_create,
            "CGDisplayStreamStart" => cg_display_stream_start,
            "CGDisplayStreamStop" => cg_display_stream_stop,
            "CGDisplayStreamGetRunLoopSource" => cg_display_stream_get_run_loop_source,
            "CGDisplayStreamUpdateGetRects" => cg_display_stream_update_get_rects,

            // CoreGraphics: PDF, which is a codec.
            "CGDataConsumerCreateWithCFData" => cg_data_consumer_create_with_cf_data,
            "CGDataProviderCreateWithCFData" => cg_data_provider_create_with_cf_data,
            "CGPDFContextCreate" => cg_pdf_context_create,
            "CGPDFContextClose" => cg_pdf_context_close,
            "CGContextBeginPage" => cg_context_begin_page,
            "CGContextEndPage" => cg_context_end_page,
            "CGContextDrawPDFPage" => cg_context_draw_pdf_page,
            "CGPDFDocumentCreateWithProvider" => cg_pdf_document_create_with_provider,
            "CGPDFDocumentGetNumberOfPages" => cg_pdf_document_get_number_of_pages,
            "CGPDFDocumentGetPage" => cg_pdf_document_get_page,
            "CGPDFPageGetBoxRect" => cg_pdf_page_get_box_rect,
            "CGPDFPageGetRotationAngle" => cg_pdf_page_get_rotation_angle,

            // CoreGraphics: context state this host does not have.
            "CGContextRotateCTM" => cg_context_rotate_ctm,
            "CGContextSetShouldSubpixelPositionFonts" => cg_context_set_should_subpixel_position_fonts,
            "CGContextSetShouldSubpixelQuantizeFonts" => cg_context_set_should_subpixel_quantize_fonts,
            "CGContextSetAllowsFontSubpixelPositioning" => cg_context_set_allows_font_subpixel_positioning,
            "CGContextSetAllowsFontSubpixelQuantization" => cg_context_set_allows_font_subpixel_quantization,

            // CoreGraphics: images, which are codecs and layouts.
            "CGBitmapContextCreateImage" => cg_bitmap_context_create_image,
            "CGImageCreateWithImageInRect" => cg_image_create_with_image_in_rect,
            "CGImageMaskCreate" => cg_image_mask_create,
            "CGContextSetFont" => cg_context_set_font,
            "CGContextShowGlyphsAtPositions" => cg_context_show_glyphs_at_positions,
            "CGContextClipToMask" => cg_context_clip_to_mask,
            "CGDataProviderCopyData" => cg_data_provider_copy_data,
            "CGImageGetBitsPerPixel" => cg_image_get_bits_per_pixel,
            "CGImageGetBytesPerRow" => cg_image_get_bytes_per_row,
            "CGImageGetBitmapInfo" => cg_image_get_bitmap_info,
            "CGImageGetDataProvider" => cg_image_get_data_provider,

            // CoreGraphics: `CGFont`, of which this host has none.
            "CGFontCreateWithDataProvider" => cg_font_create_with_data_provider,
            "CGFontRelease" => cg_font_release,
            "CGFontGetUnitsPerEm" => cg_font_get_units_per_em,
            "CGFontGetGlyphWithGlyphName" => cg_font_get_glyph_with_glyph_name,
            "CGFontCopyTableForTag" => cg_font_copy_table_for_tag,

            // CoreGraphics: dictionaries, which need a CFNumber.
            "CGRectCreateDictionaryRepresentation" => cg_rect_create_dictionary_representation,
            "CGRectMakeWithDictionaryRepresentation" => cg_rect_make_with_dictionary_representation,

            // CoreGraphics: colour spaces, which are ICC profiles.
            "CGColorSpaceCopyName" => cg_color_space_copy_name,
            "CGColorSpaceCopyICCData" => cg_color_space_copy_icc_data,
            "CGColorSpaceCreateICCBased" => cg_color_space_create_icc_based,

            // CoreText: the descriptor's type id, computed from the class table.
            "CTFontDescriptorGetTypeID" => ct_font_descriptor_get_type_id,
            "CTFontDescriptorIsSystemUIFont" => ct_font_descriptor_is_system_ui_font,

            // CoreText: collections, of which there are none.
            "CTFontCollectionCreateFromAvailableFonts" => ct_font_collection_create_from_available_fonts,
            "CTFontCollectionCreateMatchingFontDescriptors" => ct_font_collection_create_matching_font_descriptors,
            "CTFontCollectionCreateMatchingFontDescriptorsForFamily" => ct_font_collection_create_matching_font_descriptors_for_family,

            // CoreText: what a font file holds.
            "CTFontCopyAvailableTables" => ct_font_copy_available_tables,
            "CTFontCopyCharacterSet" => ct_font_copy_character_set,
            "CTFontCopyFullName" => ct_font_copy_full_name,
            "CTFontCopyLocalizedName" => ct_font_copy_localized_name,
            "CTFontCopyVariation" => ct_font_copy_variation,
            "CTFontCopyVariationAxes" => ct_font_copy_variation_axes,
            "CTFontCopyVariationAxesInternal" => ct_font_copy_variation_axes_internal,
            "CTFontCreateCopyWithSymbolicTraits" => ct_font_create_copy_with_symbolic_traits,
            "CTFontCreateForString" => ct_font_create_for_string,
            "CTFontDescriptorCopyAttributes" => ct_font_descriptor_copy_attributes,
            "CTFontDescriptorCopyLocalizedAttribute" => ct_font_descriptor_copy_localized_attribute,
            "CTFontGetGlyphCount" => ct_font_get_glyph_count,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A `CGRect` in the argument area, and the reading of it.
        ///
        /// The three rectangle predicates take their rectangle **on the stack**,
        /// which is a property of the ABI rather than of this code, so the test
        /// that matters is that the four doubles come back in the order Core
        /// Graphics defines them. A `#[repr(C)]` four-`f64` struct is a
        /// memory-class aggregate on x86-64, so `extern "C"` reads it from the
        /// same sixteen bytes the guest's `movups [rsp]` / `movups [rsp+0x10]`
        /// pair writes - `Framework+0x4591436` for `CGRectIsEmpty` and
        /// `+0x80532b2` for `CGRectIsNull`.
        fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
            Rect {
                x,
                y,
                width,
                height,
            }
        }

        #[test]
        fn a_rectangle_comes_back_in_the_order_core_graphics_defines_it() {
            // SAFETY: every one of these reads nothing but its own arguments.
            unsafe {
                // A rectangle with the fields permuted is a *different*
                // rectangle, and only the field order can tell these apart.
                let wide = rect(1.0, 2.0, 30.0, 40.0);
                let tall = rect(1.0, 2.0, 40.0, 30.0);
                assert!(!cg_rect_is_empty(wide));
                assert!(!cg_rect_is_null(wide));
                assert!(!cg_rect_equal_to_rect(wide, tall));
                assert!(cg_rect_equal_to_rect(wide, rect(1.0, 2.0, 30.0, 40.0)));

                // `CGRectIsNull` is "the origin and size are zero", so a
                // rectangle that is only *empty* is not null.
                let zero_sized = rect(5.0, 6.0, 0.0, 0.0);
                assert!(cg_rect_is_empty(zero_sized));
                assert!(!cg_rect_is_null(zero_sized));
                assert!(cg_rect_is_null(rect(0.0, 0.0, 0.0, 0.0)));

                // `CGRectIsEmpty` is "zero width or height", which includes a
                // negative one.
                assert!(cg_rect_is_empty(rect(0.0, 0.0, -1.0, 10.0)));
                assert!(cg_rect_is_empty(rect(0.0, 0.0, 10.0, -1.0)));
            }
        }

        #[test]
        fn the_display_probes_are_false_and_the_connection_is_null() {
            // SAFETY: each reads nothing and returns a constant.
            unsafe {
                // Not a built-in Mac display, not rotated, not forced to grey.
                assert_eq!(cg_display_is_builtin(0), 0);
                assert_eq!(cg_display_rotation(0), 0.0);
                assert_eq!(cg_display_uses_force_to_gray(0), 0);
                // No permission can be held for a screen that is not there.
                assert_eq!(cg_preflight_screen_capture_access(), 0);
                assert_eq!(cg_request_screen_capture_access(), 0);
                // `kCGNullDirectConnection`.
                assert_eq!(cgs_main_connection_id(), 0);
                assert_eq!(cgs_default_connection(), 0);
                // Smoothing was never disabled.
                assert_eq!(cg_font_rendering_get_font_smoothing_disabled(), 0);
            }
        }

        /// The one answer where a non-zero value would abort the guest.
        #[test]
        fn denying_window_server_connections_succeeds_because_there_are_none() {
            // SAFETY: the function reads nothing.
            //
            // `Framework+0xc8f7716` is `jne` to `int3; ud2` on this answer.
            assert_eq!(unsafe { cgs_set_deny_window_server_connections(1) }, 0);
            // And the other direction asks for the same thing.
            assert_eq!(unsafe { cgs_set_deny_window_server_connections(0) }, 0);
        }

        #[test]
        fn the_event_source_has_never_seen_an_event() {
            // SAFETY: each reads nothing and returns a constant.
            unsafe {
                // The count and the seconds agree with each other: no events, and
                // no time since one.
                assert_eq!(cg_event_source_counter_for_event_type(1, 0xFFFFFFFF), 0);
                assert_eq!(cg_event_source_seconds_since_last_event_type(1, 0xFFFFFFFF), 0.0);
                // `Framework+0x84fdad1` truncates this and returns
                // `seconds >= threshold`, so zero must answer "not stale" for
                // every positive threshold a caller could pick. A threshold of
                // zero is excluded because it answers "stale" for *every* value
                // including CoreGraphics', so it cannot distinguish anything.
                for threshold in [1, 10, 3600] {
                    assert!(
                        !(0.0 as i64 >= threshold),
                        "zero seconds must not read as stale to a {threshold}s threshold"
                    );
                }
                // Every field is zero, and zero is what the guest returns for a
                // null event itself (`Framework+0x6aa9b69`).
                assert_eq!(cg_event_get_integer_value_field(std::ptr::null(), 0x58), 0);
                assert_eq!(cg_event_get_integer_value_field(std::ptr::null(), 0x0C), 0);
                assert_eq!(cg_event_get_integer_value_field(std::ptr::null(), 0x0B), 0);
            }
        }

        #[test]
        fn a_colour_conversion_is_the_identity_because_there_is_one_colour_space() {
            // SAFETY: the function returns its third argument and reads nothing.
            let colour = 0x1000 as *const c_void;
            let space = 0x2000 as *const c_void;
            // SAFETY: the arguments are only passed through.
            let same = unsafe { cg_color_create_copy_by_matching_to_color_space(space, 0, colour, 0) };
            // Not null: the guest dereferences this answer without testing it
            // (`Framework+0x61ac9b7`).
            assert!(!same.is_null());
            assert_eq!(same, colour, "the conversion is the identity on this host");
        }

        /// A `CGPath` from `CTFontCreatePathForGlyph` has a rectangle in it.
        #[test]
        fn a_path_this_runtime_produced_is_not_empty() {
            // The argument is never read, so a null path stands in for any.
            // SAFETY: the function reads nothing.
            assert!(!unsafe { cg_path_is_empty(std::ptr::null()) });
        }

        #[test]
        fn a_rectangle_conversion_returns_the_rectangle_it_was_given() {
            let given = rect(3.0, 4.0, 5.0, 6.0);
            // SAFETY: the function returns its argument and reads nothing; the
            // hidden return pointer is the ABI's, not this test's.
            let back = unsafe { cg_context_convert_rect_to_device_space(std::ptr::null(), given) };
            assert_eq!(back, given);
        }

        /// The type ids, which are class pointers rather than invented numbers.
        #[test]
        fn a_type_id_is_the_class_the_runtime_registers() {
            // SAFETY: both read a class table entry and return a constant.
            unsafe {
                let colour = cg_color_get_type_id();
                // It must be *a* class pointer, and specifically the one the
                // CoreGraphics objects report. If `__CGColor` is not registered
                // the answer is 0 and the note has been said; either way this
                // must not be some other class's pointer.
                if let Some(class) = objc::class::lookup(c"__CGColor") {
                    assert_eq!(colour, objc::class::ptr(class) as u64);
                } else {
                    assert_eq!(colour, 0, "no class, so no type id");
                }

                // A `CTFontDescriptor` is a dictionary on this host, so its type
                // id is `NSDictionary`'s - the same answer
                // `CFDictionaryGetTypeID` gives, which the guest itself relies on
                // at `Framework+0x626ba38`.
                let descriptor = ct_font_descriptor_get_type_id();
                if let Some(class) = objc::class::lookup(c"NSDictionary") {
                    assert_eq!(descriptor, objc::class::ptr(class) as u64);
                } else {
                    assert_eq!(descriptor, 0, "no class, so no type id");
                }
            }
        }

        #[test]
        fn a_dictionary_round_trip_is_refused_rather_than_faked() {
            // SAFETY: `result` is a live, writable `CGRect`.
            unsafe {
                let mut out = rect(9.0, 9.0, 9.0, 9.0);
                assert!(
                    !cg_rect_make_with_dictionary_representation(std::ptr::null(), &mut out)
                );
                // The out-parameter is *not* written: the guest zeroes the
                // rectangle itself on a false answer, and writing here would
                // claim a rectangle nobody decoded.
                assert_eq!(out, rect(9.0, 9.0, 9.0, 9.0));

                // A null out-parameter is declined rather than dereferenced.
                assert!(!cg_rect_make_with_dictionary_representation(
                    std::ptr::null(),
                    std::ptr::null_mut()
                ));
            }
        }

        #[test]
        fn every_refusal_is_the_frameworks_own_empty_value() {
            // SAFETY: every one of these reads nothing and returns a constant,
            // a null, or a zero.
            unsafe {
                let nothing: *const c_void = std::ptr::null();
                let mut count = 12345usize;
                let mut out = rect(1.0, 2.0, 3.0, 4.0);

                // Objects that are not here are null.
                assert!(cg_event_tap_create(nothing, 0, 0, 0, nothing, nothing).is_null());
                assert!(cg_event_create_data(nothing).is_null());
                assert!(cg_event_create_from_data(nothing).is_null());
                assert!(cg_window_list_copy_window_info(0, 0).is_null());
                assert!(cg_window_list_create_description_from_array(nothing).is_null());
                assert!(
                    cg_window_list_create_image(nothing, 0, &mut out, 0, 1.0, 0).is_null()
                );
                assert!(cg_window_list_create_image_from_array(nothing, &mut out, 0, 1.0, 0).is_null());
                assert!(cg_display_create_image(0).is_null());
                assert!(cg_display_copy_color_space(0).is_null());
                assert!(cg_display_stream_get_run_loop_source(nothing).is_null());
                assert!(cg_bitmap_context_create_image(nothing).is_null());
                assert!(cg_image_create_with_image_in_rect(nothing, out).is_null());
                assert!(cg_image_mask_create(nothing, &out, out, false).is_null());
                assert!(cg_data_provider_copy_data(nothing).is_null());
                assert!(cg_image_get_data_provider(nothing).is_null());
                assert!(
                    cg_font_create_with_data_provider(nothing, 12.0, std::ptr::null()).is_null()
                );
                assert!(cg_font_copy_table_for_tag(nothing, 0x68656164).is_null());
                assert!(cg_rect_create_dictionary_representation(out).is_null());
                assert!(cg_color_space_copy_name(nothing).is_null());
                assert!(cg_color_space_copy_icc_data(nothing).is_null());
                assert!(cg_color_space_create_icc_based(nothing).is_null());
                assert!(
                    ct_font_collection_create_from_available_fonts().is_null()
                );
                assert!(ct_font_collection_create_matching_font_descriptors(nothing, nothing)
                    .is_null());
                assert!(
                    ct_font_collection_create_matching_font_descriptors_for_family(
                        nothing, nothing, nothing
                    )
                    .is_null()
                );
                assert!(ct_font_copy_available_tables(nothing).is_null());
                assert!(ct_font_copy_character_set(nothing).is_null());
                assert!(ct_font_copy_full_name(nothing).is_null());
                assert!(ct_font_copy_localized_name(nothing, nothing).is_null());
                assert!(ct_font_copy_variation(nothing).is_null());
                assert!(ct_font_copy_variation_axes(nothing).is_null());
                assert!(ct_font_copy_variation_axes_internal(nothing).is_null());
                assert!(ct_font_descriptor_copy_attributes(nothing).is_null());
                assert!(ct_font_descriptor_copy_localized_attribute(nothing, nothing, nothing)
                    .is_null());
                assert!(
                    ct_font_create_copy_with_symbolic_traits(nothing, 12.0, std::ptr::null(), 0, 0)
                        .is_null()
                );
                assert!(ct_font_create_for_string(nothing, nothing, 0).is_null());
                assert!(cg_data_consumer_create_with_cf_data(nothing).is_null());
                assert!(cg_data_provider_create_with_cf_data(nothing).is_null());
                assert!(cg_pdf_context_create(nothing, &out, nothing).is_null());
                assert!(cg_pdf_document_create_with_provider(nothing).is_null());
                assert!(cg_pdf_document_get_page(nothing, 1).is_null());
                assert!(cg_display_stream_create(0, nothing, 0.0, 0.0, 0, 0.0, nothing, nothing, nothing, nothing).is_null());

                // Counts are zero, which is a count of nothing and not an error.
                assert_eq!(cg_pdf_document_get_number_of_pages(nothing), 0);
                assert_eq!(cg_event_source_counter_for_event_type(1, 2), 0);
                assert_eq!(cg_image_get_bits_per_pixel(nothing), 0);
                assert_eq!(cg_image_get_bytes_per_row(nothing), 0);
                assert_eq!(cg_image_get_bitmap_info(nothing), 0);
                assert_eq!(cg_font_get_units_per_em(nothing), 0);
                assert_eq!(cg_font_get_glyph_with_glyph_name(nothing, nothing), 0);
                assert_eq!(ct_font_get_glyph_count(nothing), 0);
                assert_eq!(cg_pdf_page_get_rotation_angle(nothing), 0);

                // Error codes name the missing thing.
                assert_eq!(cg_display_stream_start(nothing), 12); // kCGErrorInvalidDisplay
                assert_eq!(cg_display_stream_stop(nothing), 12);
                assert_eq!(cg_display_stream_update_get_rects(nothing, 2, &mut count), 1);
                assert_eq!(count, 0, "no update, so no rectangles in it");
                // And a null count pointer is declined rather than written.
                assert_eq!(cg_display_stream_update_get_rects(nothing, 2, std::ptr::null_mut()), 1);

                // The box rectangle is written zero, because the guest reads the
                // return slot without testing it.
                assert_eq!(cg_pdf_page_get_box_rect(nothing, 0, out), Rect::default());

                // False, never "yes it is the system UI font".
                assert!(!ct_font_descriptor_is_system_ui_font(nothing));
            }
        }

        #[test]
        fn the_void_functions_touch_nothing() {
            // SAFETY: each of these is `void` and reads no argument, which is the
            // whole point: the guest hands several of them a null font, a null
            // mask and a null tap, and a function that read one would fault.
            unsafe {
                cg_event_tap_enable(std::ptr::null(), true);
                cg_pdf_context_close(std::ptr::null());
                cg_context_begin_page(std::ptr::null(), std::ptr::null());
                cg_context_end_page(std::ptr::null());
                cg_context_draw_pdf_page(std::ptr::null(), std::ptr::null());
                cg_context_rotate_ctm(std::ptr::null(), std::f64::consts::FRAC_PI_4);
                cg_context_set_should_subpixel_position_fonts(std::ptr::null(), true);
                cg_context_set_should_subpixel_quantize_fonts(std::ptr::null(), true);
                cg_context_set_allows_font_subpixel_positioning(std::ptr::null(), true);
                cg_context_set_allows_font_subpixel_quantization(std::ptr::null(), true);
                cg_context_set_font(std::ptr::null(), std::ptr::null());
                cg_context_show_glyphs_at_positions(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    8,
                );
                cg_context_clip_to_mask(std::ptr::null(), Rect::default(), std::ptr::null());
                cg_path_add_ellipse_in_rect(std::ptr::null(), std::ptr::null());
                cgs_set_window_alpha(0, 1, 0.5);
                cgs_set_window_transform(
                    0,
                    1,
                    Affine { a: 1.0, d: 1.0, ..Affine::default() },
                );
                cgs_set_window_warp(0, 1, 0, std::ptr::null());
                cg_font_release(std::ptr::null());
            }
        }
    }
}

/// The one row [`crate::lib`] holds for this file.
///
/// The table is a list of `exports::Module` values, and every one of them is
/// declared by the macro at the bottom of `imp` - so this is a re-export
/// rather than a second declaration, and the two cannot disagree about which
/// names the module answers for.
#[cfg(not(target_os = "macos"))]
pub use imp::MODULE;

/// Nothing of this file's on Darwin: the host has CoreGraphics, CoreText and
/// ImageIO, and shadowing its own with a refusal would be the worst of both
/// worlds. See the `exports!` at the foot of the file.
#[cfg(target_os = "macos")]
exports::exports! {}
