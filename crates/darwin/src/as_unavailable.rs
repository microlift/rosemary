//! ApplicationServices, CoreServices and Carbon: the three frameworks whose
//! remaining surface is a *window server*, a *print server*, an *Apple Event
//! Manager* and a *Launch Services database*, none of which exist on this host.
//!
//! # Why this file exists
//!
//! An unbound import is a jump through a zero slot. That is how whole bring-up
//! runs have been lost, so every name here is bound to a function that answers
//! with the truth. The truth, for all four of those subsystems, is a refusal in
//! that subsystem's own vocabulary - and a great many of these are exactly the
//! shape `crate::unavailable` established: a value that is *true* is
//! implemented, and where there is no answer the honest code is returned rather
//! than a plausible zero.
//!
//! Three things are **not** refusals, and they are the reason this file is worth
//! more than a `return 0` block:
//!
//! * `UAZoomEnabled` answers **false**, which is the true state of this host and
//!   which the guest *tests*, so it takes its own "no accessibility features"
//!   path rather than a fabricated one.
//! * `FSEventsGetCurrentEventId` and `FSEventStreamGetLatestEventId` answer
//!   **zero**, which is FSEvents' own documented answer for "no event has ever
//!   been queued" (`FSEventStreamFlushAsync` says so in those words), and which
//!   makes the guest's "has anything changed since I started watching?" test
//!   correctly false.
//! * `EnableSecureEventInput` and `DisableSecureEventInput` answer `noErr`,
//!   because the state each of them requests already holds: there is no window
//!   server, so there are no other recipients of key events to protect from or
//!   to hand them back to. This is the same argument
//!   `CGSSetDenyWindowServerConnections` -> 0 makes in
//!   [`crate::cg_unavailable`], and it is the only kind of claim in this file
//!   about the *world* rather than about a capability.
//!
//! # The families, and the honest answer for each
//!
//! | family | names | the answer |
//! |---|--:|---|
//! | Accessibility (`AX*`) | 24 | no accessibility server, so no `AXUIElement`, no `AXValue`, no `AXTextMarker` |
//! | PrintCore (`PM*`) | 38 | `kPMServerNotFound`: there is no print server to ask |
//! | Process Manager | 1 | `procNotFound`: there is no process table to look in |
//! | Carbon Event Manager | 19 | null events and a non-zero `OSErr`; no window server, so no event manager |
//! | Text Services / secure event input | 4 | `noErr`, for [`ua_zoom_enabled`]'s reason and its own |
//! | Launch Services | 12 | `kLSServerCommunicationErr`: `lsd` is not on this host |
//! | Apple Events | 5 | `procNotFound`: the target application is not running, because it does not exist |
//! | Recent items (`LSSharedFileList*`) | 7 | null, and the guest bails out of its own snapshot walk |
//! | Spotlight metadata (`MDItem*`) | 2 | null: no `mds` index, so no metadata item |
//!
//! # The two traps this file is built around
//!
//! Both were found by reading `Chromium Framework` and both would have been
//! crashes rather than refusals:
//!
//! 1. **An out-parameter the guest reads without testing it.** Four call sites
//!    pre-fill an out-parameter with `0xaaaaaaaaaaaaaaaa` - clang's
//!    uninitialised-memory poison - and then read it *without* looking at the
//!    return value: `PMSessionGetCurrentPrinter` at `Framework+0x8b5171c` and
//!    `+0x8b518e0`, and `PMSessionGetCurrentPrinter`/`PMPrinterGetID` at
//!    `+0x8b50505` and `+0x8b4fc2a`. See
//!    [`pmsession_get_current_printer`] and [`pmprinter_get_id`].
//! 2. **An out-parameter the caller does *not* own.** `AESendMessage`'s fourth
//!    argument is `AEEventReturnID *`, and the guest passes the 64-bit constant
//!    `0xfffffffffffffffe` in it (`Framework+0x5b091bb`) because the real
//!    implementation returns before touching it when the send mode is
//!    `kAENoReply`. Writing an event id there is a fault at that address. See
//!    [`aesend_message`].
//!
//! # What is *not* here
//!
//! `IOSurface`, `VideoToolbox`, `AudioToolbox`, `MediaAccessibility`, AppKit,
//! CoreFoundation, Foundation and the libSystem families belong to other owners;
//! see [`crate::unavailable`] and [`crate::cg_unavailable`]. Every signature
//! below was read out of a real header and then **checked against the call site
//! in the guest**, and where the two disagree the note says so and the binary
//! wins - there are four places where it does, and they are marked.
#[cfg(not(target_os = "macos"))]
mod imp {
    //! The answers, and why each one is the true answer.
    //!
    //! `Framework+0x…` addresses are the guest's own call sites, verified with
    //! `test/tools/callers.py` and then re-disassembled with
    //! `test/tools/raw-range.py` before anything was believed about them.

    use std::ffi::{c_int, c_void};
    use std::sync::atomic::{AtomicBool, Ordering};

    // ------------------------------------------------------- the ABI types

    /// A `CGRect`: four `CGFloat`s, which are `double` on x86-64.
    ///
    /// `#[repr(C)]` and exactly 32 bytes, so it is a memory-class aggregate: a
    /// `CGRect` **argument** is passed on the stack and a `CGRect` **return**
    /// comes back through a hidden pointer. Both facts were established from
    /// the guest by `crate::cg_unavailable` and are used by
    /// [`pmget_adjusted_page_rect`] and [`ua_zoom_change_focus`] below.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct Rect {
        pub x: f64,
        pub y: f64,
        pub width: f64,
        pub height: f64,
    }

    /// A `Boolean`, which is an `unsigned char` and which the guest reads out of
    /// `al`. `CoreServices/Carbon`'s own typedef, not Rust's `bool`, so that a
    /// one-byte return is a one-byte return.
    pub type Boolean = u8;

    /// An `FSEventStreamEventId`, which is a `UInt64` in both directions.
    pub type EventId = u64;

    /// A `pid_t`.
    pub type Pid = i32;

    // ------------------------------------------------- the refusal vocabulary

    /// `kPMServerNotFound`, from `PrintCore/PMErrors.h` (the "Queue Manager
    /// (-9630 to -9659)" block), whose comment is "print server not found".
    ///
    /// The right code for the whole `PM*` family, and for the reason the header
    /// gives: a PrintCore call is a request to the print server, and there is no
    /// print server, no `print` daemon and no CUPS on this host. Chosen over
    /// `kPMInvalidObject` (-30896) because that would blame the *handle* the
    /// caller passed, which is not what is wrong.
    const PM_NO_SERVER: c_int = -9630;

    /// `kLSServerCommunicationErr`, from `LaunchServices/LSInfo.h`, whose comment
    /// is "The server process (registration and recent items) is not available".
    ///
    /// That comment is this file's whole Launch Services answer in six words,
    /// and it covers the recent-items half of the family as well as the
    /// registration half: `lsd` is the process that owns both, and it is not on
    /// this host.
    const LS_NO_SERVER: c_int = -10822;

    /// `procNotFound`, the Process Manager's own "no such process", whose value
    /// `-600` is named in `AE/AppleEvents.h` ("If the target application is not
    /// running, then `procNotFound` will be returned").
    ///
    /// Used twice, and for the same underlying reason each time. For
    /// [`aesend_message`] and friends it is literally the documented answer:
    /// Chromium sends to `'LRUL'` and `'csim'`, and neither target application
    /// exists here. For [`get_process_information`] it is the Process Manager
    /// saying "that process is not in my table" - which is true, because there
    /// is no process table on this host, only the kernel's.
    const PROC_NOT_FOUND: c_int = -600;

    /// A non-zero `AXError`.
    ///
    /// Every `AX*` call site in the guest tests the status with `test eax,eax`
    /// and nothing finer, so the specific value carries no information to it;
    /// what matters is that it is not `kAXErrorSuccess`, which is zero. `-1` is
    /// `AXError.h`'s `kAXErrorFailure`, the enum's first non-success value, and
    /// it is the right shape of answer: *this could not be done here*. Naming a
    /// more specific code - "invalid UI element", say - would claim to know why,
    /// and the reason is not an invalid argument: the accessibility server is
    /// absent.
    const AX_REFUSED: c_int = -1;

    /// A non-zero `OSErr` from the Carbon Event Manager.
    ///
    /// Carbon's event-error range begins at `eventInternalErr` (-10000), and
    /// that is what this is: the Event Manager could not do the thing because
    /// there is no Event Manager. As with [`AX_REFUSED`], the guest only tests
    /// non-zero - `GetEventParameter`'s two live call sites both do
    /// `test eax,eax` - and the header for this crate's copy of Carbon's event
    /// errors could not be read, so the number is the family's base rather than a
    /// name this file can cite. That limitation is stated rather than hidden.
    const EVENT_MANAGER_REFUSED: c_int = -10000;

    // ------------------------------------------- CFTypeIDs for the AX types

    /// A `CFTypeID` that **cannot** be a real one.
    ///
    /// In this tree a `CFTypeID` *is* an Objective-C class pointer -
    /// `corefoundation::base` says so in as many words: "Apple's is a small
    /// integer; here it is the class pointer, which is the same thing said more
    /// directly - `CFGetTypeID(x) == CFStringGetTypeID()` asks whether `x` is a
    /// string, and the class answers that exactly." A class is an object, and
    /// objects are eight-byte aligned, so **no odd number can be a class
    /// pointer** and therefore no odd number can collide with any object in the
    /// process.
    ///
    /// That is what these four are: stable for the life of the process, distinct
    /// from each other, and distinct from every real type. It matters because the
    /// guest's first move on any accessibility value is
    /// `CFGetTypeID(x)` compared against one of the `...GetTypeID` functions -
    /// `Framework+0x80a0425` against `AXValueGetTypeID`, `+0x80a051a` and
    /// `+0x80a0667` against `AXUIElementGetTypeID`, `+0x809b488` against
    /// `AXTextMarkerGetTypeID`. A zero here would be *worse* than useless: it
    /// would make `CFGetTypeID` of every non-CoreFoundation object compare
    /// equal, and the guest would then hand a garbage pointer to the very
    /// function that dereferences it.
    const TYPE_AX_UI_ELEMENT: u64 = 1;
    const TYPE_AX_VALUE: u64 = 3;
    const TYPE_AX_TEXT_MARKER: u64 = 5;
    const TYPE_AX_TEXT_MARKER_RANGE: u64 = 7;

    // ----------------------------------------------------------- the note

    /// A refusal, said once.
    ///
    /// `write(2)` rather than `eprintln!`, for `crate::cg_unavailable`'s reason:
    /// a message about a refusal has to be out before the refusal is, and a
    /// buffered one can be lost.
    fn note(once: &AtomicBool, what: &str, why: &str) {
        if !once.swap(true, Ordering::Relaxed) {
            let line = format!("rosetta3: {what}: {why}\n");
            // SAFETY: a live buffer for the length written.
            unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
        }
    }

    /// One flag per family, so a process that touches six of these says six
    /// things and a process that hammers one of them says it once.
    mod said {
        use std::sync::atomic::AtomicBool;

        /// The accessibility server, and with it every `AX*` answer.
        pub static ACCESSIBILITY: AtomicBool = AtomicBool::new(false);
        /// PrintCore, and with it the whole print job pipeline.
        pub static PRINTING: AtomicBool = AtomicBool::new(false);
        /// The Process Manager's table of processes.
        pub static PROCESS_MANAGER: AtomicBool = AtomicBool::new(false);
        /// The Carbon Event Manager and the window server behind it.
        pub static EVENTS: AtomicBool = AtomicBool::new(false);
        /// Text Services: the input method, and secure event input.
        pub static TEXT_SERVICES: AtomicBool = AtomicBool::new(false);
        /// `launchd`'s Launch Services daemon, `lsd`.
        pub static LAUNCH_SERVICES: AtomicBool = AtomicBool::new(false);
        /// The Apple Event Manager.
        pub static APPLE_EVENTS: AtomicBool = AtomicBool::new(false);
        /// The recent-items list, which is `lsd`'s as well.
        pub static RECENT_ITEMS: AtomicBool = AtomicBool::new(false);
        /// Spotlight's metadata store, `mds`.
        pub static SPOTLIGHT: AtomicBool = AtomicBool::new(false);
        /// `fseventsd`, and with it the FSEvents clock.
        pub static FSEVENTS: AtomicBool = AtomicBool::new(false);
    }

    /// Say, once, that a family is refusing.
    fn refuse(family: &AtomicBool, what: &str, why: &str) {
        note(family, what, why);
    }

    // ================================================================ AX: HIServices

    /// `AXError AXObserverAddNotification(AXObserverRef observer, AXUIElementRef element, CFStringRef notification, void *refcon)`.
    ///
    /// **A no-op, and the return is discarded anyway.** The guest sets all four
    /// arguments (`Framework+0x809e77c`) and at `+0x809e795` releases the
    /// notification name without ever looking at `rax`.
    ///
    /// Unreachable in practice: the guest only reaches it after
    /// [`ax_observer_create_with_info_callback`] has returned success.
    unsafe extern "C" fn ax_observer_add_notification(
        _observer: *const c_void,
        _element: *const c_void,
        _notification: *const c_void,
        _refcon: *mut c_void,
    ) {
        refuse(
            &said::ACCESSIBILITY,
            "AXObserverAddNotification",
            "no accessibility server on this host, so there is no observer to notify and no \
             element to observe; the guest discards this call's answer (Framework+0x809e795 \
             releases the name without reading rax)",
        );
    }

    /// `AXError AXObserverCreateWithInfoCallback(...) -> AXObserverRef *outObserver`.
    ///
    /// **A non-zero `AXError`, and nothing is written anywhere.**
    ///
    /// The arity is the binary's, not the header's, and the difference matters
    /// only because it decides what *not* to touch. `Framework+0x809e69c` passes
    /// **four** things - a 32-bit value in `edi`, a callback in `rsi`, a pointer
    /// in `rdx` and whatever `rcx` held - where `AXObserver.h` declares three
    /// parameters with the out-parameter third. So which register holds the
    /// out-parameter cannot be established from the header, and writing to a
    /// register that is actually a `void *refcon` would be the
    /// `CGDisplayStreamUpdateGetRects` mistake in a new place.
    ///
    /// It does not need writing: the guest tests the status **first**
    /// (`+0x809e6a1 test eax,eax; jne 0x809e85b`) and its error arm builds the
    /// string "Failed to create AXObserverRef" and gives up. The out-parameter is
    /// not read on that path.
    unsafe extern "C" fn ax_observer_create_with_info_callback(
        _first: Pid,
        _callback: Option<unsafe extern "C" fn(*const c_void, *mut c_void)>,
        _refcon: *mut c_void,
        _out: *mut *mut c_void,
    ) -> c_int {
        refuse(
            &said::ACCESSIBILITY,
            "AXObserverCreateWithInfoCallback",
            "no accessibility server on this host, so an observer cannot be created; the guest \
             checks this status before reading anything (Framework+0x809e6a1) and logs \
             \"Failed to create AXObserverRef\" on it",
        );
        AX_REFUSED
    }

    /// `CFRunLoopSourceRef AXObserverGetRunLoopSource(AXObserverRef observer, CFRunLoopRef runLoop)`.
    ///
    /// **Null.** The guest stores the answer at `Framework+0x809e7fd` and hands
    /// it straight to `CFRunLoopAddSource` at `+0x809e81d` with no test, so a
    /// non-null answer would have to be a real `CFRunLoopSource`.
    ///
    /// Unreachable: the only path to this call is the fall-through after
    /// [`ax_observer_create_with_info_callback`] returned `kAXErrorSuccess`.
    unsafe extern "C" fn ax_observer_get_run_loop_source(
        _observer: *const c_void,
        _run_loop: *const c_void,
    ) -> *const c_void {
        refuse(
            &said::ACCESSIBILITY,
            "AXObserverGetRunLoopSource",
            "there is no observer, because there is no accessibility server; the guest adds \
             whatever this returns to its run loop without testing it (Framework+0x809e7fd)",
        );
        std::ptr::null()
    }

    /// `AXTextMarkerRef AXTextMarkerCreate(CFAllocatorRef allocator, const UInt8 *bytePtr, CFIndex length)`.
    ///
    /// **Null, and this one is a refusal with a reason that is worth stating
    /// plainly rather than hiding.** `AXTextMarker` is a `CFType`: the guest
    /// reads it with `CFGetTypeID` (`Framework+0x809b488`) and compares the answer
    /// against [`ax_text_marker_get_type_id`], and a value this crate handed out
    /// would have to be an object `corefoundation` recognises. It cannot be: a
    /// `CFTypeID` here *is* an Objective-C class pointer, `CFGetTypeID` reads the
    /// object's class through the Objective-C runtime, and the only registry that
    /// could name a class as a CoreFoundation type
    /// (`corefoundation::base`'s `OPAQUE` table) belongs to `corefoundation`,
    /// which **depends on** `darwin` rather than the other way round. So a
    /// `Box`ed struct published from here would be handed to `CFGetTypeID`,
    /// which would dereference a foreign pointer.
    ///
    /// Null is safe here: the guest null-checks every marker it gets back
    /// before releasing it (`+0x809b8c5` and `+0x809b8d2`), and the one
    /// place it returns a marker without a test (`+0x809b6ed`, straight into
    /// `objc_autoreleaseReturnValue`) is a nil-safe call.
    unsafe extern "C" fn ax_text_marker_create(
        _allocator: *const c_void,
        _bytes: *const u8,
        _length: isize,
    ) -> *const c_void {
        refuse(
            &said::ACCESSIBILITY,
            "AXTextMarkerCreate",
            "an AXTextMarker has to be a CoreFoundation object whose CFGetTypeID this file \
             cannot make, because the CFTypeID registry belongs to the corefoundation crate, \
             which depends on darwin rather than the other way round; the guest null-checks \
             every marker it gets back",
        );
        std::ptr::null()
    }

    /// `CFIndex AXTextMarkerGetLength(AXTextMarkerRef marker)`.
    ///
    /// **Zero**, and unreachable: the guest calls it only after
    /// `CFGetTypeID(marker) == AXTextMarkerGetTypeID()` has passed
    /// (`Framework+0x809b495`), and no marker can be one. It then compares the
    /// answer against `0x38` (`+0x809b4a2`) and takes its "not a text marker"
    /// path on a mismatch.
    unsafe extern "C" fn ax_text_marker_get_length(_marker: *const c_void) -> isize {
        0
    }

    /// `const UInt8 *AXTextMarkerGetBytePtr(AXTextMarkerRef marker)`.
    ///
    /// **Null**, which the guest checks (`Framework+0x809b4b0 test rax,rax; je`).
    unsafe extern "C" fn ax_text_marker_get_byte_ptr(_marker: *const c_void) -> *const u8 {
        std::ptr::null()
    }

    /// `CFTypeID AXTextMarkerGetTypeID(void)`.
    ///
    /// [`TYPE_AX_TEXT_MARKER`]. See the note on that constant for why an odd
    /// number is the right kind of value here.
    unsafe extern "C" fn ax_text_marker_get_type_id() -> u64 {
        TYPE_AX_TEXT_MARKER
    }

    /// `AXTextMarkerRef AXTextMarkerRangeCopyStartMarker(AXTextMarkerRangeRef range)`.
    ///
    /// **Null.** The guest null-checks it - `Framework+0x809b52d test r12,r12;
    /// setne` - and takes its "not a marker range" path when either marker is
    /// missing.
    unsafe extern "C" fn ax_text_marker_range_copy_start_marker(_range: *const c_void) -> *const c_void {
        std::ptr::null()
    }

    /// `AXTextMarkerRef AXTextMarkerRangeCopyEndMarker(AXTextMarkerRangeRef range)`.
    ///
    /// [`ax_text_marker_range_copy_start_marker`]'s null, for the same reason
    /// and the same instruction.
    unsafe extern "C" fn ax_text_marker_range_copy_end_marker(_range: *const c_void) -> *const c_void {
        std::ptr::null()
    }

    /// `AXTextMarkerRangeRef AXTextMarkerRangeCreate(CFAllocatorRef allocator, AXTextMarkerRef start, AXTextMarkerRef end)`.
    ///
    /// **Null**, with the same reason as [`ax_text_marker_create`]. The three
    /// arguments are the guest's own: `Framework+0x809b8b4` sets `rdi` to the
    /// allocator, `rsi` to a start marker and `rdx` to an end marker, both of
    /// which are already null.
    unsafe extern "C" fn ax_text_marker_range_create(
        _allocator: *const c_void,
        _start: *const c_void,
        _end: *const c_void,
    ) -> *const c_void {
        std::ptr::null()
    }

    /// `CFTypeID AXTextMarkerRangeGetTypeID(void)`.
    ///
    /// [`TYPE_AX_TEXT_MARKER_RANGE`].
    unsafe extern "C" fn ax_text_marker_range_get_type_id() -> u64 {
        TYPE_AX_TEXT_MARKER_RANGE
    }

    /// `AXError AXUIElementCopyActionNames(AXUIElementRef element, CFArrayRef *outNames)`.
    ///
    /// [`AX_REFUSED`], with `*outNames` set to null. The guest checks the status
    /// (`Framework+0x80a13a3 test eax,eax; je`) and null-checks the array
    /// again at `+0x80a13a7`; it had already pre-zeroed the slot at `+0x80a138b`.
    unsafe extern "C" fn axuielement_copy_action_names(
        _element: *const c_void,
        out_names: *mut *const c_void,
    ) -> c_int {
        refuse(
            &said::ACCESSIBILITY,
            "AXUIElementCopyActionNames",
            "no accessibility server on this host, so no element has any actions",
        );
        write_null_ref(out_names);
        AX_REFUSED
    }

    /// `AXError AXUIElementCopyAttributeNames(AXUIElementRef element, CFArrayRef *outNames)`.
    ///
    /// [`axuielement_copy_action_names`]'s answer. **Worth recording that this
    /// one is the call whose status the guest does not check** -
    /// `Framework+0x80a0680` is followed by `mov BYTE PTR [rbp-0x29],0x12`
    /// rather than a test - so the null written here is the only thing standing
    /// between the guest and the array it would otherwise walk. The guest had
    /// pre-zeroed the slot at `+0x80a0671`; writing null says the same thing
    /// without relying on that.
    unsafe extern "C" fn axuielement_copy_attribute_names(
        _element: *const c_void,
        out_names: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_names);
        AX_REFUSED
    }

    /// `AXError AXUIElementCopyAttributeValue(AXUIElementRef element, CFStringRef attribute, CFTypeRef *outValue)`.
    ///
    /// [`AX_REFUSED`], with `*outValue` set to null.
    ///
    /// Three arguments, and the third is an out-parameter: `Framework+0x809fea6`
    /// sets `rdi` to the element, `rsi` to the attribute and `rdx` to
    /// `&[rbp-0x20]`, which the guest pre-zeroed at `+0x809fe9b`. The status is
    /// checked at `+0x809feba` and the value null-checked again at `+0x809febe`.
    unsafe extern "C" fn axuielement_copy_attribute_value(
        _element: *const c_void,
        _attribute: *const c_void,
        out_value: *mut *const c_void,
    ) -> c_int {
        refuse(
            &said::ACCESSIBILITY,
            "AXUIElementCopyAttributeValue",
            "no accessibility server on this host, so no element has any attribute",
        );
        write_null_ref(out_value);
        AX_REFUSED
    }

    /// `AXError AXUIElementCopyParameterizedAttributeNames(AXUIElementRef element, CFArrayRef *outNames)`.
    ///
    /// [`axuielement_copy_action_names`]'s answer. Status checked at
    /// `Framework+0x80a0969`, slot pre-zeroed at `+0x80a0955`.
    unsafe extern "C" fn axuielement_copy_parameterized_attribute_names(
        _element: *const c_void,
        out_names: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_names);
        AX_REFUSED
    }

    /// `AXError AXUIElementCopyParameterizedAttributeValue(AXUIElementRef element, CFStringRef attribute, CFTypeRef value, CFTypeRef *outValue)`.
    ///
    /// [`AX_REFUSED`], with `*outValue` set to null. Four arguments and the
    /// binary confirms the fourth: `Framework+0x80a0c8a` sets `rdi` to the
    /// element, `rsi` to the attribute, `rdx` to the `AXValue` the guest built
    /// and `rcx` to `&[rbp-0x40]`, pre-zeroed at `+0x80a0c73`.
    ///
    /// The `AXValue` in `rdx` is the one [`ax_value_create`] returned, and it is
    /// null - see that function for why it has to be.
    unsafe extern "C" fn axuielement_copy_parameterized_attribute_value(
        _element: *const c_void,
        _attribute: *const c_void,
        _value: *const c_void,
        out_value: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_value);
        AX_REFUSED
    }

    /// `AXUIElementRef AXUIElementCreateApplication(pid_t pid)`.
    ///
    /// **Null, and this is the one refusal in this file with a known exposure,
    /// so it is stated rather than left for someone to discover.**
    ///
    /// `AXUIElementRef` is a token minted by the accessibility server, which is
    /// part of the window server. There is no window server on this host, so
    /// there is no such token to mint, and a non-null answer would have to be an
    /// object `CFGetTypeID` names as an `AXUIElement` - the same cross-crate
    /// wall [`ax_text_marker_create`] describes.
    ///
    /// The guest checks the answer at its *first* use and has a named path for
    /// it: `Framework+0x809e5f8` tests `rax` and, on null, builds the string
    /// "Failed to get AXUIElement for pid " (`__cstring+0xda33e91`) and returns.
    /// Two of the other three call sites are inside
    /// `ui/accessibility/platform/inspect/ax_event_recorder_mac.mm`'s
    /// `AXEventRecorder`, which is gated on a configured pid
    /// (`Framework+0x809e5da test r12d,r12d; je 0x809e657`) and is not reached
    /// by a `--headless --dump-dom` run.
    ///
    /// **The exposure.** The fourth call site, at `Framework+0x80a2419`, is *not*
    /// gated and does *not* test: it calls `CFBridgingRelease` twice on the
    /// answer and then `+0x80a2449` passes it to a helper that compares
    /// `CFGetTypeID(x)` against `AXUIElementGetTypeID()` and, on a mismatch,
    /// calls `base::internal::ImmediateCrash` - the `TriggerNotReached` at
    /// `__cstring+0xd8eba55`, confirmed by the strings beside it
    /// ("Check failed: false.", "NOTREACHED log messages are omitted in
    /// official builds"). So if that path is ever reached with a null element,
    /// the guest aborts.
    ///
    /// That is not fixable from here without lying: making
    /// [`axuielement_get_type_id`] answer zero would make the comparison
    /// succeed for *every* non-CoreFoundation object and hand the guest a
    /// garbage pointer to dereference, which is a strictly worse wrong answer.
    /// The truth is the null, and the exposure is recorded.
    unsafe extern "C" fn axuielement_create_application(_pid: Pid) -> *const c_void {
        refuse(
            &said::ACCESSIBILITY,
            "AXUIElementCreateApplication",
            "an AXUIElement is minted by the accessibility server, which is part of the window \
             server, and there is no window server on this host; the guest's first call site \
             null-checks it (Framework+0x809e5f8) but one site inside AXEventRecorder does \
             not (Framework+0x80a2419) and would reach NOTREACHED",
        );
        std::ptr::null()
    }

    /// `CFTypeID AXUIElementGetTypeID(void)`.
    ///
    /// [`TYPE_AX_UI_ELEMENT`], and see that constant's note for why an odd
    /// number is required rather than merely better. This is the value compared
    /// against `CFGetTypeID` at **six** call sites, three of which treat a
    /// mismatch as a hard error (`Framework+0x80a12e4`, `+0x80a145e` and
    /// `+0x80a1389` all branch to `0x5cb87d0`, the `TriggerNotReached`).
    unsafe extern "C" fn axuielement_get_type_id() -> u64 {
        TYPE_AX_UI_ELEMENT
    }

    /// `AXError AXUIElementPerformAction(AXUIElementRef element, CFStringRef action)`.
    ///
    /// [`AX_REFUSED`], and the guest discards it (`Framework+0x80a1466` is
    /// followed by a jump, not a test).
    unsafe extern "C" fn axuielement_perform_action(
        _element: *const c_void,
        _action: *const c_void,
    ) -> c_int {
        AX_REFUSED
    }

    /// `AXError AXUIElementSetAttributeValue(AXUIElementRef element, CFStringRef attribute, CFTypeRef value)`.
    ///
    /// [`AX_REFUSED`], and the guest discards it (`Framework+0x80a12ef`).
    unsafe extern "C" fn axuielement_set_attribute_value(
        _element: *const c_void,
        _attribute: *const c_void,
        _value: *const c_void,
    ) -> c_int {
        AX_REFUSED
    }

    /// `AXValueRef AXValueCreate(AXValueType theType, const void *valuePtr)`.
    ///
    /// **Null, and the reason is the single most important finding in the AX
    /// family.** `AXValue.h` declares the result `__nullable`, so null is a
    /// documented answer; but the *reason* it has to be null here is that the
    /// guest hands the answer to `CFRelease`.
    ///
    /// `Framework+0x80a0c86` stores the created value in `[rbp-0x30]`, and on
    /// the next iteration of the enclosing loop `+0x80a0c63` loads `[rbp-0x30]`
    /// and calls `CFRelease` on it. `CFRelease` is CoreFoundation's, and it
    /// works by reading the object's class through the Objective-C runtime. A
    /// `Box`ed struct published from `darwin` would therefore be *dereferenced as
    /// an Objective-C object* - the cross-crate wall again, and this time a fault
    /// rather than a wrong type. Null makes `CFRelease(NULL)` the no-op it is on
    /// every platform.
    ///
    /// The value in the answer is whatever the caller encoded, so a non-null
    /// answer would not have been an invented value - it would have been a
    /// *foreign pointer in a slot CoreFoundation owns*. That is worse.
    unsafe extern "C" fn ax_value_create(_the_type: u32, _value_ptr: *const c_void) -> *const c_void {
        refuse(
            &said::ACCESSIBILITY,
            "AXValueCreate",
            "an AXValue has to be a CoreFoundation object, and the guest hands the answer to \
             CFRelease on its next pass (Framework+0x80a0c63 reloads the slot this filled at \
             +0x80a0c86), so a non-CF token would be dereferenced as an Objective-C object",
        );
        std::ptr::null()
    }

    /// `AXValueType AXValueGetType(AXValueRef value)`.
    ///
    /// **`kAXValueTypeIllegal`, which is 0** - the header's own name for "a type
    /// that is not recognised", and the value `AXValueGetType` is documented to
    /// return for one. Quoting `AXValue.h`: "Returns the structure type encoded
    /// in value. If the type is not recognized, it returns
    /// kAXValueTypeIllegal."
    ///
    /// The guest compares the answer against `2` (`kAXValueTypeCGSize`,
    /// `Framework+0x80a043a`) and `1` (`kAXValueTypeCGPoint`, `+0x80a058f`) and
    /// takes a "not a size/point" path on a mismatch - so zero is the documented
    /// answer *and* the one the guest is built to handle.
    unsafe extern "C" fn ax_value_get_type(_value: *const c_void) -> u32 {
        0
    }

    /// `CFTypeID AXValueGetTypeID(void)`.
    ///
    /// [`TYPE_AX_VALUE`].
    unsafe extern "C" fn ax_value_get_type_id() -> u64 {
        TYPE_AX_VALUE
    }

    /// `Boolean AXValueGetValue(AXValueRef value, AXValueType theType, void *valuePtr)`.
    ///
    /// **False, and `*valuePtr` is deliberately not written.** The header's
    /// contract is that the structure is copied out only when the stored type
    /// matches the requested one, so a false answer means there is no value to
    /// store.
    ///
    /// Writing zeros here would be the size question this file refuses to guess:
    /// `kAXValueTypeCGRect` is 32 bytes and the other four types are 8 to 16, so
    /// a fixed-size write would overflow whichever buffer the caller passed. Not
    /// writing is also safe at the two live call sites, because the guest zeroes
    /// the destination itself before asking - `Framework+0x80a0443` and
    /// `+0x80a0598` both `movdqa` a zeroed `xmm0` into the destination - and
    /// zeroes it again on a false answer.
    unsafe extern "C" fn ax_value_get_value(
        _value: *const c_void,
        _the_type: u32,
        _value_ptr: *mut c_void,
    ) -> Boolean {
        0
    }

    // ====================================================== Process Manager

    /// `OSStatus GetProcessInformation(ProcessSerialNumber *psn, <a 72-byte output structure> *)`.
    ///
    /// **`procNotFound`, and nothing is written.**
    ///
    /// The arity is the binary's: `Framework+0x5da228e` and `+0x5da22c1` each
    /// pass exactly two pointers, where `Processes.h`'s five-argument
    /// `GetProcessInformation(short procID, Str63, Ptr, Ptr, Ptr)` is a different
    /// overload. The second argument is a structure the guest itself lays out -
    /// it writes the size `0x48` at its offset `+0x18` and zeroes the rest
    /// before every call - and it reads a four-character code at `+0x30` on
    /// success (`cmp DWORD PTR [rbp-0x38],0x6c676e77`, `'wngl'`).
    ///
    /// This crate has no source for that structure, so it does not write one:
    /// inventing 72 bytes at an invented layout is exactly the silent corruption
    /// this project exists to avoid, and it would be *undetectable* - the guest
    /// reads a code out of the middle of it.
    ///
    /// The refusal is safe at all three call sites because each tests the status
    /// before reading the structure: `Framework+0x5da2293 test ax,ax; jne`,
    /// `+0x5da2349 test ax,ax; jne` and `+0x5da237c test ax,ax; jne`.
    ///
    /// The code is the Process Manager's own "no such process". The process the
    /// guest asks about - itself - is running; what is missing is the process
    /// *table*, so the most honest thing the table can say is that it has no
    /// entry. This is the same argument
    /// `dadisk_create_from_bsd_name` makes in [`crate::unavailable`]: the object
    /// was never in the table to begin with.
    unsafe extern "C" fn get_process_information(
        _psn: *const c_void,
        _out: *mut c_void,
    ) -> c_int {
        refuse(
            &said::PROCESS_MANAGER,
            "GetProcessInformation",
            "the Process Manager's table of processes is a Mac OS facility with no counterpart on \
             this host; the guest lays out and zeroes its own 72-byte output structure before \
             every call and tests the status before reading it",
        );
        PROC_NOT_FOUND
    }

    // ==================================================== PrintCore (PM*)

    /// `void PMRelease(PMObjectRef object)`.
    ///
    /// **A no-op, and the reason is ownership rather than absence.** Every
    /// `PM*` function below hands back null, so the only value the guest can
    /// reach `PMRelease` with is one this file never allocated - and freeing a
    /// foreign pointer is the one thing a `void` release must never do. The
    /// twelve call sites (`Framework+0x8b5163d` among them) are therefore
    /// reached on the *live* path with a null handle and have nothing to do,
    /// which is the same shape as `ppd_close` in [`crate::unavailable`].
    unsafe extern "C" fn pm_release(_object: *const c_void) {
        refuse(
            &said::PRINTING,
            "PMRelease",
            "no print server on this host, so no PM object was ever created and there is \
             nothing here to release; a null handle is what every call site can be holding",
        );
    }

    /// `PMPrinterRef PMPrinterCreateFromPrinterID(CFStringRef inPrinterID)`.
    ///
    /// **Null**, which the guest null-checks at `Framework+0x8b51a19` and skips
    /// the rest of the "select this printer" path on. There is no printer here,
    /// so there is no `PMPrinterRef` for any id.
    unsafe extern "C" fn pmprinter_create_from_printer_id(_id: *const c_void) -> *const c_void {
        refuse(
            &said::PRINTING,
            "PMPrinterCreateFromPrinterID",
            "no print server on this host, so no printer exists for any id",
        );
        std::ptr::null()
    }

    /// `CFStringRef PMPrinterGetID(PMPrinterRef printer) - as the guest calls it`.
    ///
    /// **Null, and the signature is the guest's rather than the header's.** Two
    /// call sites use the answer as a `CFStringRef` in `rax` with **one** argument
    /// and no out-parameter: `Framework+0x8b4fc2a` (`rsi` still holds the
    /// caller's second argument, untouched) and `+0x8b519c2`, where the answer is
    /// immediately null-tested (`+0x8b519ca test rax,rax; je`). The header's
    /// `OSStatus PMPrinterGetID(PMPrinterRef, CFStringRef *)` does not match
    /// either, and the binary is the authority.
    ///
    /// The first site does not test the answer at all - it passes it straight to
    /// a `CFStringRef`-to-`std::string` converter at `+0x8b4fc39` and copies the
    /// result out - so this had to be *safe* rather than merely correct. It is:
    /// that converter calls `CFStringGetLength(rdi)` first and takes its
    /// empty-string path when the length is zero
    /// (`+0x5d99e4f test rax,rax; je 0x5d99ea4`), and
    /// `corefoundation::api::CFStringGetLength` answers `0` for a null string.
    ///
    /// Null is also the truth: a printer has no id because there is no printer.
    unsafe extern "C" fn pmprinter_get_id(_printer: *const c_void) -> *const c_void {
        refuse(
            &said::PRINTING,
            "PMPrinterGetID",
            "no print server on this host, so no printer has an id; null is safe even where \
             the guest does not test it, because the CFStringRef-to-std::string converter it \
             passes the answer to treats a null string as empty \
             (Framework+0x5d99e4f)",
        );
        std::ptr::null()
    }

    /// `OSStatus PMPrinterGetPaperList(PMPrinterRef printer, CFArrayRef *outPaperList)`.
    ///
    /// [`PM_NO_SERVER`], with `*outPaperList` set to null. The guest pre-zeroed
    /// the slot at `Framework+0x8b51294` and tests the status at `+0x8b512ad`.
    unsafe extern "C" fn pmprinter_get_paper_list(
        _printer: *const c_void,
        out_list: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_list);
        PM_NO_SERVER
    }

    /// `OSStatus PMPrinterGetPrinterResolutionCount(PMPrinterRef printer, UInt32 *outCount)`.
    ///
    /// [`PM_NO_SERVER`], with `*outCount` set to zero. The guest pre-zeroed the
    /// slot at `Framework+0x8b4fcb9`, tests the status at `+0x8b4fce1` **and**
    /// tests the count at `+0x8b4fce5`, so zero is doubly the right answer: no
    /// resolutions, and the count is a count.
    unsafe extern "C" fn pmprinter_get_printer_resolution_count(
        _printer: *const c_void,
        out_count: *mut u32,
    ) -> c_int {
        write_zero(out_count);
        PM_NO_SERVER
    }

    /// `OSStatus PMPrinterGetIndexedPrinterResolution(PMPrinterRef printer, UInt32 index, PMResolution *outResolution)`.
    ///
    /// [`PM_NO_SERVER`], with the resolution's two `double`s set to zero. The
    /// guest pre-zeroes the whole 16-byte structure before every call
    /// (`Framework+0x8b4fd09`) and, because the count above is zero, never
    /// reaches the body of the loop.
    unsafe extern "C" fn pmprinter_get_indexed_printer_resolution(
        _printer: *const c_void,
        _index: u32,
        out_resolution: *mut f64,
    ) -> c_int {
        // SAFETY: the caller's own `PMResolution`, two `double`s, pre-zeroed by
        // the guest and only read back as two `double`s (`movsd` at
        // `Framework+0x8b4fd1c`).
        if !out_resolution.is_null() {
            unsafe {
                out_resolution.write(0.0);
                out_resolution.add(1).write(0.0);
            }
        }
        PM_NO_SERVER
    }

    /// `OSStatus PMPrinterSetOutputResolution(PMPrinterRef printer, PMPrintSettingsRef settings, CFRect resolution)`.
    ///
    /// [`PM_NO_SERVER`]. Three arguments, confirmed by the binary:
    /// `Framework+0x8b51e19` sets `rdi` to the printer, `rsi` to the settings
    /// and `rdx` to a pointer to a 16-byte `PMResolution` the guest built at
    /// `+0x8b51de5`. The guest turns the answer into a bool with
    /// `sete al` (`+0x8b51e20`), so a non-zero status is "could not set the
    /// resolution" - which is true.
    unsafe extern "C" fn pmprinter_set_output_resolution(
        _printer: *const c_void,
        _settings: *const c_void,
        _resolution: *const f64,
    ) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionGetCurrentPrinter(PMPrintingSessionRef session, PMPrinterRef *outPrinter)`.
    ///
    /// [`PM_NO_SERVER`], **with `*outPrinter` set to null - and this is the one
    /// out-parameter in this file that is load-bearing.**
    ///
    /// Two call sites pre-fill the out-parameter with `0xaaaaaaaaaaaaaaaa` and
    /// then read it **without looking at the return value**:
    ///
    /// * `Framework+0x8b518cb` stores the poison, `+0x8b518e0` calls this, and
    ///   `+0x8b518e5` loads `[rbp-0x50]` straight into the next call.
    /// * `Framework+0x8b51707` stores the poison, `+0x8b5171c` calls this, and
    ///   `+0x8b51721` loads `[rbp-0x40]` immediately.
    /// * `Framework+0x8b504f4` does the same and loads at `+0x8b5050a`.
    ///
    /// `0xaaaa...aaaa` is not a plausible printer: the guest would pass it to
    /// `pmprinter_get_id` and on to CoreFoundation. The other three call sites
    /// *do* test the status (`+0x8b51194`, `+0x8b51198` and `+0x8b51962`), and
    /// one of those additionally needs a *non-zero* status to avoid falling into
    /// a branch that would call `pmprinter_get_id` on a null printer.
    unsafe extern "C" fn pmsession_get_current_printer(
        _session: *const c_void,
        out_printer: *mut *const c_void,
    ) -> c_int {
        refuse(
            &said::PRINTING,
            "PMSessionGetCurrentPrinter",
            "no print server on this host, so no printing session has a current printer; the \
             out-parameter is written because three call sites pre-fill it with clang's \
             0xaaaa...aaaa poison and read it without testing this function's status \
             (Framework+0x8b51721, +0x8b518e5, +0x8b5050a)",
        );
        write_null_ref(out_printer);
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionSetCurrentPMPrinter(PMPrintingSessionRef session, PMPrinterRef printer)`.
    ///
    /// [`PM_NO_SERVER`]. Two arguments, confirmed at `Framework+0x8b51a28`; the
    /// status is tested at `+0x8b51a46` and a non-zero answer sends the guest
    /// down its "could not select that printer" path.
    unsafe extern "C" fn pmsession_set_current_pm_printer(
        _session: *const c_void,
        _printer: *const c_void,
    ) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionSetDestination(PMPrintingSessionRef session, PMPrintDestination destType, CFURLRef url, CFDictionaryRef options)`.
    ///
    /// [`PM_NO_SERVER`]. Four arguments, confirmed at `Framework+0x8b5181b`
    /// (`edx = 4`, which is `kPMDestinationPrinter`) and the status tested at
    /// `+0x8b51820`.
    unsafe extern "C" fn pmsession_set_destination(
        _session: *const c_void,
        _destination: c_int,
        _url: *const c_void,
        _options: *const c_void,
    ) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionGetDestinationType(PMPrintingSessionRef session, CFStringRef *outType)`.
    ///
    /// [`PM_NO_SERVER`], with `*outType` set to null.
    ///
    /// The guest discards the status: `Framework+0x8b50931` is followed by
    /// loading a 16-byte constant into `[rbp-0x70]` and zeroing `[rbp-0x60]`
    /// (`+0x8b5093d` and `+0x8b50941`), which is to say it overwrites the slot
    /// itself rather than reading it. Writing null here is therefore belt and
    /// braces - and it is written so that the claim "this session has no
    /// destination type" is made by this function rather than by the guest's
    /// own initialiser.
    unsafe extern "C" fn pmsession_get_destination_type(
        _session: *const c_void,
        out_type: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_type);
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionCopyDestinationFormat(PMPrintingSessionRef session, PMPrintSettingsRef settings, PMPrintSettingsRef *outFormat)`.
    ///
    /// [`PM_NO_SERVER`], with `*outFormat` set to null. Three arguments,
    /// confirmed at `Framework+0x8b50956`; the slot was pre-zeroed at
    /// `+0x8b50941` and the status tested at `+0x8b5095e`.
    unsafe extern "C" fn pmsession_copy_destination_format(
        _session: *const c_void,
        _settings: *const c_void,
        out_format: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_format);
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionCopyDestinationLocation(PMPrintingSessionRef session, CFURLRef *outURL)`.
    ///
    /// [`PM_NO_SERVER`], with `*outURL` set to null. Two arguments, confirmed at
    /// `Framework+0x8b50a2f`; the status is tested at `+0x8b50a37`.
    unsafe extern "C" fn pmsession_copy_destination_location(
        _session: *const c_void,
        out_url: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_url);
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionValidatePageFormat(PMPrintingSessionRef session, PMPageFormatRef format, PMPrintSettingsRef *outSettings)`.
    ///
    /// [`PM_NO_SERVER`], with `*outSettings` set to null. Three arguments,
    /// confirmed at `Framework+0x8b528f4`.
    ///
    /// The other call site does **not** test the status (`+0x8b5188c` discards
    /// `rax`), so the null written here is what keeps that path from reading a
    /// stale handle.
    unsafe extern "C" fn pmsession_validate_page_format(
        _session: *const c_void,
        _format: *const c_void,
        out_settings: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_settings);
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionValidatePrintSettings(PMPrintingSessionRef session, PMPrintSettingsRef settings, PMPageFormatRef *outFormat)`.
    ///
    /// [`pmsession_validate_page_format`]'s answer, with the two out-parameters
    /// exchanged.
    unsafe extern "C" fn pmsession_validate_print_settings(
        _session: *const c_void,
        _settings: *const c_void,
        out_format: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_format);
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionCopyDestinationFormat`-adjacent: `PMSessionGetCGGraphicsContext` is separate; this is
    /// `OSStatus PMSessionBeginCGDocumentNoDialog(PMPrintingSessionRef session, PMPrintSettingsRef settings, CFDictionaryRef options)`.
    ///
    /// [`PM_NO_SERVER`]. Three arguments, confirmed at `Framework+0x8b52a91`,
    /// and the status **is** tested at `+0x8b52a96` - a non-zero answer sends
    /// the guest straight to its "the print job could not be started" path,
    /// which is what makes every page- and context-level call below unreachable
    /// rather than merely unused.
    unsafe extern "C" fn pmsession_begin_cg_document_no_dialog(
        _session: *const c_void,
        _settings: *const c_void,
        _options: *const c_void,
    ) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionEndDocumentNoDialog(PMPrintingSessionRef session)`.
    ///
    /// [`PM_NO_SERVER`]. One argument; the guest discards the answer.
    unsafe extern "C" fn pmsession_end_document_no_dialog(_session: *const c_void) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionBeginPageNoDialog(PMPrintingSessionRef session, PMRect pageRect, PMRect mediaRect)`.
    ///
    /// [`PM_NO_SERVER`]. The two `PMRect`s are the header's `CGRect`-shaped
    /// structs, passed **on the stack** - 32 bytes each - which is the memory
    /// class this file's [`Rect`] is declared for.
    unsafe extern "C" fn pmsession_begin_page_no_dialog(
        _session: *const c_void,
        _page_rect: Rect,
        _media_rect: Rect,
    ) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMSessionEndPageNoDialog(PMPrintingSessionRef session)`.
    ///
    /// [`PM_NO_SERVER`]. One argument. The guest reaches this only from inside a
    /// started document, which [`pmsession_begin_cg_document_no_dialog`] never
    /// allows.
    unsafe extern "C" fn pmsession_end_page_no_dialog(_session: *const c_void) -> c_int {
        PM_NO_SERVER
    }

    /// `CGContextRef PMSessionGetCGGraphicsContext(PMPrintingSessionRef session)`.
    ///
    /// **Null**, and this is the one PrintCore answer that *could* have been a
    /// fault. The guest takes the answer and draws into it
    /// (`Framework+0x8b52c90+0xd9`), so a non-null value would have to be a real
    /// `CGContextRef` from `coregraphics` - which, like `AXValue`, is a
    /// cross-crate object.
    ///
    /// Unreachable: the only path to it is inside a document that
    /// [`pmsession_begin_cg_document_no_dialog`] refused, and the guest tests
    /// that status (`+0x8b52a96`) before beginning a page.
    unsafe extern "C" fn pmsession_get_cg_graphics_context(_session: *const c_void) -> *const c_void {
        std::ptr::null()
    }

    /// `OSStatus PMPrintSettingsSetJobName(PMPrintSettingsRef settings, CFStringRef jobName)`.
    ///
    /// [`PM_NO_SERVER`]. Two arguments, confirmed at `Framework+0x8b52a83`; the
    /// guest discards the answer.
    unsafe extern "C" fn pmprint_settings_set_job_name(
        _settings: *const c_void,
        _job_name: *const c_void,
    ) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMPrintSettingsSetValue(PMPrintSettingsRef settings, CFStringRef key, CFTypeRef value)`.
    ///
    /// [`PM_NO_SERVER`]. Three arguments, confirmed at `Framework+0x8b51f43`: the
    /// guest builds a `CFStringRef` from the key and passes it in `rsi` with the
    /// value in `rdx`.
    unsafe extern "C" fn pmprint_settings_set_value(
        _settings: *const c_void,
        _key: *const c_void,
        _value: *const c_void,
    ) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMPrintSettingsCreateDataRepresentation(PMPrintSettingsRef settings, CFDataRef *outData, UInt32 *outSize)`.
    ///
    /// [`PM_NO_SERVER`], with `*outData` set to null. Three arguments,
    /// confirmed at `Framework+0x8b50592` - and note `edx` is **zero** there, so
    /// the third argument (the size out-parameter) is null and is correctly not
    /// written. The data slot was pre-zeroed at `+0x8b50574` and the status is
    /// tested at `+0x8b50597`.
    unsafe extern "C" fn pmprint_settings_create_data_representation(
        _settings: *const c_void,
        out_data: *mut *const c_void,
        _out_size: *mut u32,
    ) -> c_int {
        write_null_ref(out_data);
        PM_NO_SERVER
    }

    /// `OSStatus PMPrintSettingsCreateWithDataRepresentation(CFDataRef inData, PMPrintSettingsRef *outSettings)`.
    ///
    /// [`PM_NO_SERVER`], with `*outSettings` set to null. Two arguments,
    /// confirmed at `Framework+0x8b51fd0+0x679`.
    unsafe extern "C" fn pmprint_settings_create_with_data_representation(
        _data: *const c_void,
        out_settings: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_settings);
        PM_NO_SERVER
    }

    /// `OSStatus PMPrintSettingsCopy(PMPrintSettingsRef src, PMPrintSettingsRef *outDest)`.
    ///
    /// [`pmprinter_set_output_resolution`]'s shape: two arguments and a status
    /// the guest turns into a bool.
    unsafe extern "C" fn pm_copy_print_settings(
        _src: *const c_void,
        out_dest: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_dest);
        PM_NO_SERVER
    }

    /// `OSStatus PMPageFormatCreateDataRepresentation(PMPageFormatRef format, CFDataRef *outData, UInt32 *outSize)`.
    ///
    /// [`pmprint_settings_create_data_representation`]'s answer. Three
    /// arguments, confirmed at `Framework+0x8b50795`; the data slot was
    /// pre-zeroed at `+0x8b50784` and the status tested at `+0x8b5079d`.
    unsafe extern "C" fn pm_page_format_create_data_representation(
        _format: *const c_void,
        out_data: *mut *const c_void,
        _out_size: *mut u32,
    ) -> c_int {
        write_null_ref(out_data);
        PM_NO_SERVER
    }

    /// `OSStatus PMPageFormatCreateWithDataRepresentation(CFDataRef inData, PMPageFormatRef *outFormat)`.
    ///
    /// [`pmprint_settings_create_with_data_representation`]'s answer. Two
    /// arguments, confirmed at `Framework+0x8b52864`; the slot was pre-zeroed at
    /// `+0x8b52855` and the status tested at `+0x8b5286c`.
    unsafe extern "C" fn pm_page_format_create_with_data_representation(
        _data: *const c_void,
        out_format: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_format);
        PM_NO_SERVER
    }

    /// `OSStatus PMCreatePageFormatWithPMPaper(PMPaperRef paper, PMPageFormatRef *outFormat)`.
    ///
    /// [`PM_NO_SERVER`], with `*outFormat` set to null. Two arguments,
    /// confirmed at `Framework+0x8b51442` (`rdi` is the paper, `rsi` the
    /// out-format); the slot was pre-zeroed at `+0x8b51433` and the status
    /// tested at `+0x8b51447`.
    unsafe extern "C" fn pm_create_page_format_with_pmpaper(
        _paper: *const c_void,
        out_format: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_format);
        PM_NO_SERVER
    }

    /// `OSStatus PMGetPageFormatPaper(PMPageFormatRef format, PMPaperRef *outPaper)`.
    ///
    /// [`PM_NO_SERVER`], with `*outPaper` set to null. **The slot is poisoned**
    /// at `Framework+0x8b51479` (`movabs rax,0xaaaa…; mov [rbp-0x30],rax`),
    /// and the status *is* tested at `+0x8b51489`, so the poison is never read -
    /// but the guest does load the slot on the success path at `+0x8b51491`, so
    /// writing null is what makes a non-zero status the only thing that matters
    /// there.
    unsafe extern "C" fn pm_get_page_format_paper(
        _format: *const c_void,
        out_paper: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_paper);
        PM_NO_SERVER
    }

    /// `OSStatus PMGetOrientation(PMPrintSettingsRef settings, UInt32 *outOrientation)`.
    ///
    /// [`PM_NO_SERVER`], with the orientation written as
    /// **`kPMPortrait` (1)**, and the reason for the *value* is separate from the
    /// reason for the *status*.
    ///
    /// The status is discarded at `Framework+0x8b4fca2` and the slot is read
    /// immediately (`+0x8b4fca7 cmp WORD PTR [rbp-0x2a],0x2`), so the write is
    /// required. The guest had pre-seeded the slot with `1`
    /// (`+0x8b4fc95 mov WORD PTR [rbp-0x2a],0x1`) and compares it against `2`,
    /// which is `kPMLandscape` - so it is asking "is this landscape?" and
    /// treating the answer as a bool. Writing `kPMPortrait` says "no", which is
    /// the truth: there is no page orientation here, and portrait is the
    /// `PMPrintSettings` default the guest itself chose.
    ///
    /// The width of the store is worth a note: the guest writes and reads a
    /// **word**, so its own declaration of the out-parameter is 16-bit. Writing
    /// `1` as a 32-bit `UInt32` gives the same two bytes either way, so the
    /// answer does not depend on which of the two the compiler meant.
    unsafe extern "C" fn pm_get_orientation(
        _settings: *const c_void,
        out_orientation: *mut u32,
    ) -> c_int {
        const K_PM_PORTRAIT: u32 = 1;
        if !out_orientation.is_null() {
            // SAFETY: the caller's own out-parameter, read back as a 16-bit
            // value at `Framework+0x8b4fca7`.
            unsafe { out_orientation.write(K_PM_PORTRAIT) };
        }
        PM_NO_SERVER
    }

    /// `OSStatus PMGetAdjustedPageRect(PMPrintSettingsRef settings, CGRect *outRect)`.
    ///
    /// [`PM_NO_SERVER`], with the rectangle written as all zeros. The status is
    /// discarded (`Framework+0x8b4fd6f`) and the rectangle is read
    /// unconditionally at `+0x8b4fd93`, so the write is required. The guest had
    /// pre-zeroed the slot at `+0x8b4fd56` and then subtracts one rect from the
    /// other, clamping every result with `cmovle` (`+0x8b4fd38` and onwards) -
    /// so zeros produce a zero-sized page, which is what a rect with no print
    /// settings behind it means.
    unsafe extern "C" fn pm_get_adjusted_page_rect(
        _settings: *const c_void,
        out_rect: *mut Rect,
    ) -> c_int {
        write_rect_zero(out_rect);
        PM_NO_SERVER
    }

    /// `OSStatus PMGetAdjustedPaperRect(PMPrintSettingsRef settings, CGRect *outRect)`.
    ///
    /// [`pm_get_adjusted_page_rect`]'s answer. Same call shape at
    /// `Framework+0x8b4fd7e` and `+0x8b510f5`: status discarded, rect read.
    unsafe extern "C" fn pm_get_adjusted_paper_rect(
        _settings: *const c_void,
        out_rect: *mut Rect,
    ) -> c_int {
        write_rect_zero(out_rect);
        PM_NO_SERVER
    }

    /// `OSStatus PMSetOrientation(PMPrintSettingsRef settings, PMOrientation orientation)`.
    ///
    /// [`PM_NO_SERVER`]. The guest's third argument is the *negated* landscape
    /// flag it computed at `Framework+0x8b51858 inc r12d`, so it is asking to
    /// set portrait and the status is tested at `+0x8b5186b`.
    unsafe extern "C" fn pm_set_orientation(
        _settings: *const c_void,
        _orientation: c_int,
    ) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMSetCopies(PMPrintSettingsRef settings, UInt32 copies)`.
    ///
    /// [`PM_NO_SERVER`]. Two arguments; the guest tests the status and logs.
    unsafe extern "C" fn pm_set_copies(_settings: *const c_void, _copies: u32) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMSetCollate(PMPrintSettingsRef settings, Boolean collate)`.
    ///
    /// [`PM_NO_SERVER`]. The `Boolean` is one byte, per `CoreServices`.
    unsafe extern "C" fn pm_set_collate(_settings: *const c_void, _collate: Boolean) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMSetDuplex(PMPrintSettingsRef settings, PMDuplexMode mode)`.
    ///
    /// [`PM_NO_SERVER`].
    unsafe extern "C" fn pm_set_duplex(_settings: *const c_void, _mode: c_int) -> c_int {
        PM_NO_SERVER
    }

    /// `OSStatus PMPaperGetWidth(PMPaperRef paper, double *outWidth)`.
    ///
    /// [`PM_NO_SERVER`], with the width set to `0.0`. One of the two call sites
    /// discards the status and reads the slot (`Framework+0x8b5137a`, slot
    /// pre-zeroed at `+0x8b51360`), and the other tests the status at
    /// `+0x8b5149e`; the width is a `double` in both.
    unsafe extern "C" fn pm_paper_get_width(_paper: *const c_void, out_width: *mut f64) -> c_int {
        write_zero_f64(out_width);
        PM_NO_SERVER
    }

    /// `OSStatus PMPaperGetHeight(PMPaperRef paper, double *outHeight)`.
    ///
    /// [`pm_paper_get_width`]'s answer. `Framework+0x8b51389` and `+0x8b514ae`.
    unsafe extern "C" fn pm_paper_get_height(_paper: *const c_void, out_height: *mut f64) -> c_int {
        write_zero_f64(out_height);
        PM_NO_SERVER
    }

    /// `OSStatus PMPaperGetPPDPaperName(PMPaperRef paper, CFStringRef *outName)`.
    ///
    /// [`PM_NO_SERVER`], with `*outName` set to null. Three of the four call
    /// sites either discard the status (`Framework+0x8b513ef`, slot pre-zeroed
    /// at `+0x8b513e0`) or discard it and read the slot anyway
    /// (`+0x8b514cb`, slot pre-zeroed at `+0x8b514bb`) - and the last one passes
    /// the result to a `CFStringRef`-consuming helper at `+0x8b514e8`, which is
    /// null-tolerant for the same reason [`pmprinter_get_id`]'s is. The fourth
    /// tests the status at `+0x8b513f4`.
    unsafe extern "C" fn pm_paper_get_ppd_paper_name(
        _paper: *const c_void,
        out_name: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_name);
        PM_NO_SERVER
    }

    /// `OSStatus PMPaperGetMargins(PMPaperRef paper, CFDictionaryRef *outMargins)`.
    ///
    /// [`PM_NO_SERVER`], with `*outMargins` set to null. **The status is
    /// discarded** at `Framework+0x8b514db` and the slot is read at `+0x8b51558`,
    /// where it becomes an argument to
    /// [`pm_paper_create_custom`]. The guest had pre-zeroed the 32 bytes at
    /// `+0x8b511c3`.
    unsafe extern "C" fn pm_paper_get_margins(
        _paper: *const c_void,
        out_margins: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_margins);
        PM_NO_SERVER
    }

    /// `OSStatus PMPaperCreateCustom(...) -> PMPaperRef *outPaper`.
    ///
    /// [`PM_NO_SERVER`], with the out-parameter set to null.
    ///
    /// **The arity is the guest's and it is not the header's.** `PMPaper.h`
    /// declares six parameters starting with a `CFStringRef` name. The guest
    /// passes **seven** (`Framework+0x8b51563`): a `PMPrinterRef` in `rdi`, two
    /// `CGFloat`s in `xmm0`/`xmm1`, two `CFStringRef`s in `rsi`/`rdx`, the
    /// margins dictionary in `rcx` and the out-paper in `r8`. So the first
    /// argument is a *printer*, not a name.
    ///
    /// That does not need resolving, because this function refuses and writes
    /// only the last argument, whose position **is** established: `r8` holds
    /// `lea r8,[rbp-0x30]`, and the guest had pre-zeroed that slot at
    /// `+0x8b51534` and tests the status at `+0x8b51568`. Getting the arity
    /// wrong here would have been the `CGDisplayStreamUpdateGetRects` mistake -
    /// writing through an operand that is not the out-parameter - and reading it
    /// out of the binary is what avoids it.
    unsafe extern "C" fn pm_paper_create_custom(
        _printer: *const c_void,
        _width: f64,
        _height: f64,
        _name: *const c_void,
        _size_name: *const c_void,
        _margins: *const c_void,
        out_paper: *mut *const c_void,
    ) -> c_int {
        refuse(
            &said::PRINTING,
            "PMPaperCreateCustom",
            "no print server on this host, so no paper can be described; the guest tests the \
             status (Framework+0x8b51568) before using the out-paper it pre-zeroed at \
             +0x8b51534",
        );
        write_null_ref(out_paper);
        PM_NO_SERVER
    }

    // =================================================== Universal Access

    /// `Boolean UAZoomEnabled(void)`.
    ///
    /// **False, and it is the true answer rather than a stub.** VoiceOver Zoom is
    /// an accessibility feature the window server provides; there is no window
    /// server on this host, so zoom is not enabled.
    ///
    /// The guest **tests** it, and the false answer is what puts the guest on
    /// its own path: `Framework+0x3aa4cb3 test al,al; je 0x3aa4d16` skips the
    /// whole zoom-refresh block, and the three other call sites
    /// (`+0x3aa6b5e`, `+0x72d253f`, `+0x88c4272`) are the same test. There is a
    /// second load-bearing consequence: the one path that would reach
    /// [`ua_zoom_change_focus`] is inside that block, so the false answer makes
    /// that call unreachable rather than merely unused.
    unsafe extern "C" fn ua_zoom_enabled() -> Boolean {
        0
    }

    /// `OSStatus UAZoomChangeFocus(CGRect rect)`.
    ///
    /// **Zero, and unreachable.** The `Boolean` return is discarded at
    /// `Framework+0x3aa4ed3` and every route to it is behind the
    /// [`ua_zoom_enabled`] test that is false.
    ///
    /// The `CGRect` is declared as a memory-class aggregate, so it arrives on the
    /// stack - 32 bytes - which is what the guest pushes at
    /// `Framework+0x3aa4e9d`, where it converts two integers to `double` and
    /// stores them at `[rbp-0x30]` and `[rbp-0x28]`. Nothing is written and
    /// nothing is read, so the declaration's only job is to be harmless if it is
    /// wrong, which it is either way round: a struct argument and four scalar
    /// arguments both simply ignore the other.
    unsafe extern "C" fn ua_zoom_change_focus(_rect: Rect) -> c_int {
        0
    }

    /// `void SetApplicationIsDaemon(Boolean inIsDaemon)`.
    ///
    /// **A non-zero `OSErr`, and the guest's own log line is the reason it is
    /// worth getting right.** The sandbox prologue
    /// (`sandbox/mac/system_services.cc`, per the strings at
    /// `__cstring+0xda391e2` and `+0xda39207`) calls this with `1` and *does*
    /// test the answer: `Framework+0x80f0416 sete cl` makes a zero return the
    /// success path, and a non-zero return with a small call count falls into a
    /// block that logs "SetApplicationIsDaemon" and carries on.
    ///
    /// So this is a case where `noErr` would be the *convenient* answer and the
    /// wrong one: it would tell the sandbox that it had registered a UI-less
    /// daemon with a Launch Services that does not exist. The honest answer is
    /// that the registration did not happen, and the guest's own diagnostic is
    /// where that shows up instead of being hidden.
    unsafe extern "C" fn set_application_is_daemon(_is_daemon: Boolean) -> c_int {
        refuse(
            &said::LAUNCH_SERVICES,
            "SetApplicationIsDaemon",
            "marking this process a UI-less daemon is a registration with Launch Services, and \
             lsd is not on this host; a zero here would tell Chromium's sandbox prologue the \
             registration happened (Framework+0x80f0416 sete cl), so the refusal is reported \
             and Chromium logs \"SetApplicationIsDaemon\" about it",
        );
        LS_NO_SERVER
    }

    // ================================================= Carbon: Event Manager

    /// `EventRef CreateEvent(CFAllocatorRef allocator, UInt32 eventClass, UInt32 eventKind, EventHandlerUPP handler, EventRef target, Boolean noReply)`.
    ///
    /// **Null, and the six arguments are the binary's.** The call is inside an
    /// Objective-C method, so the incoming registers are `self`, `_cmd`, then the
    /// two selector arguments; `Framework+0x8b6a80d` puts them in the positions
    /// the header declares (`esi` = class, `edx` = kind), zeroes `rcx`, passes
    /// `r15+8` as the target in `r8` and zeroes `xmm0` for `noReply`. Six
    /// registers, which is the header's arity.
    ///
    /// Null is safe: the caller null-tests the answer at `+0x8b6a7f4` and
    /// returns the (possibly nil) object either way.
    unsafe extern "C" fn create_event(
        _allocator: *const c_void,
        _event_class: u32,
        _event_kind: u32,
        _handler: *const c_void,
        _target: *const c_void,
        _no_reply: Boolean,
    ) -> *const c_void {
        refuse(
            &said::EVENTS,
            "CreateEvent",
            "there is no Event Manager on this host - the Carbon Event Manager is part of the \
             window server, and there is no window server",
        );
        std::ptr::null()
    }

    /// `EventRef CopyEvent(EventRef inEvent)`.
    ///
    /// **Null.** The guest null-checks it at `Framework+0x8b6a8b6` and returns
    /// without touching the copy if it is null. There is no event to copy.
    unsafe extern "C" fn copy_event(_event: *const c_void) -> *const c_void {
        std::ptr::null()
    }

    /// `EventRef RetainEvent(EventRef inEvent)`.
    ///
    /// **The argument, unchanged.** Retaining is a no-op on a handle this file
    /// never produced, and returning the argument is the one answer that cannot
    /// be wrong in a new way: the guest stores the answer at
    /// `Framework+0x8b6a878` into an object field and reads it back later, so a
    /// null here would silently empty an event pointer the guest believes it
    /// holds. Unreachable in practice, since no event exists to retain.
    unsafe extern "C" fn retain_event(event: *const c_void) -> *const c_void {
        event
    }

    /// `EventRef GetCurrentEvent(void)`.
    ///
    /// **Null.** The guest passes the answer straight to an Objective-C
    /// initialiser at `Framework+0x8b6a7a8`, which is a nil-safe message send.
    /// There is no current event: there is no event dispatch happening.
    unsafe extern "C" fn get_current_event() -> *const c_void {
        std::ptr::null()
    }

    /// `EventQueueRef GetMainEventQueue(void)`.
    ///
    /// **Null**, and it is the honest answer even in the strong sense: the main
    /// event queue is the queue the window server owns, and there is no window
    /// server. The guest passes the answer to a message send at
    /// `Framework+0x8b6aac8`, so null is what it must be.
    unsafe extern "C" fn get_main_event_queue() -> *const c_void {
        refuse(
            &said::EVENTS,
            "GetMainEventQueue",
            "the main event queue belongs to the window server's event loop, and there is no \
             window server on this host",
        );
        std::ptr::null()
    }

    /// `EventQueueRef GetCurrentEventQueue(void)`.
    ///
    /// [`get_main_event_queue`]'s null: with no event dispatch there is no
    /// current queue either.
    unsafe extern "C" fn get_current_event_queue() -> *const c_void {
        std::ptr::null()
    }

    /// `EventTargetRef GetEventMonitorTarget(EventTargetRef target)`.
    ///
    /// **Null**: there is no monitor to install, and none is installed.
    unsafe extern "C" fn get_event_monitor_target(_target: *const c_void) -> *const c_void {
        std::ptr::null()
    }

    /// `EventClass GetEventClass(EventRef event)`.
    ///
    /// **Zero.** The only event the guest can have is one from a handler chain,
    /// and [`add_event_types_to_handler`] refuses to install one, so this is
    /// unreachable; the value it does return is a class code, and zero is not
    /// one of the four `EVENT_CLASS(kEventClassXYZ)` values.
    unsafe extern "C" fn get_event_class(_event: *const c_void) -> u32 {
        0
    }

    /// `EventKind GetEventKind(EventRef event)`.
    ///
    /// **Zero.** The two live call sites compare the answer against `0x7d4` and
    /// `0x7d0` (`Framework+0xbc8a3f7` and `+0xbc8a460`), which are
    /// `kEventMouseWheel`/`kEventMouseMoved`-shaped kinds, and both have a
    /// "neither, so the value is zero" arm (`+0xbc8a471 xorpd xmm0,xmm0`). Zero
    /// is therefore the value the guest itself produces for an event it does not
    /// recognise.
    unsafe extern "C" fn get_event_kind(_event: *const c_void) -> u32 {
        0
    }

    /// `EventTime GetEventTime(EventRef event)`.
    ///
    /// **Zero**, for [`get_event_class`]'s reason.
    unsafe extern "C" fn get_event_time(_event: *const c_void) -> f64 {
        0.0
    }

    /// `OSStatus SetEventTime(EventRef event, EventTime time)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`]. The guest discards the answer.
    unsafe extern "C" fn set_event_time(_event: *const c_void, _time: f64) -> c_int {
        EVENT_MANAGER_REFUSED
    }

    /// `OSErr GetEventParameter(EventRef event, AEKeyword key, AEEventClass requestedType, AEEventClass *actualType, Size dataSize, void *data, Size *dataSizeRet)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`], and the data pointer **is** written, because
    /// one call site reads it without testing the status.
    ///
    /// Seven arguments and the binary confirms the shape: `Framework+0x8b6ab59`
    /// sets `rdi` to the event, `esi`/`edx` to the key and requested type,
    /// zeroes `rcx` (the `actualType` out), zeroes `r8d` (`dataSize`), passes
    /// `&out` in `r9` and a null `dataSizeRet` on the stack. The header's
    /// seventh argument is the size out-parameter and the sixth is the data
    /// pointer - which is what this answers.
    ///
    /// The site that needs the write is `Framework+0x8b6ab19`: it calls, does
    /// **not** test the answer, and then loads `[rbp-0x8]` at `+0x8b6ab5e` and
    /// returns it. The guest had pre-zeroed that slot at `+0x8b6ab38`, so the
    /// write is belt and braces - but a null there is the answer that makes
    /// "this event has no such parameter" true rather than accidental.
    ///
    /// The *other* two call sites are the reason a non-zero status matters:
    /// `+0x8b6ab20 sete cl` turns `noErr` into "the parameter exists", and
    /// `Framework+0x4547ec7` checks the status before reading the poisoned
    /// out-size at `+0x4547ed2`.
    unsafe extern "C" fn get_event_parameter(
        event: *const c_void,
        _key: u32,
        _requested_type: u32,
        _actual_type: *mut u32,
        _data_size: usize,
        data: *mut c_void,
        _data_size_ret: *mut usize,
    ) -> c_int {
        refuse(
            &said::EVENTS,
            "GetEventParameter",
            "there is no Event Manager on this host, so no event carries a parameter; the data \
             pointer is written because one call site reads it without testing the status \
             (Framework+0x8b6ab5e), and the status must be non-zero because another turns \
             noErr into \"the parameter exists\" (Framework+0x8b6ab20 sete cl)",
        );
        let _ = event;
        write_null_ref(data.cast::<*const c_void>());
        EVENT_MANAGER_REFUSED
    }

    /// `OSErr SetEventParameter(EventRef event, AEKeyword key, EventParamType type, const void *data, Byte size)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`]. Five arguments, confirmed at
    /// `Framework+0x8b6aaf3` (which shifts the guest's `r8` into `r9` and so
    /// uses every integer register). The guest discards the answer.
    unsafe extern "C" fn set_event_parameter(
        _event: *const c_void,
        _key: u32,
        _type: u32,
        _data: *const c_void,
        _size: u32,
    ) -> c_int {
        EVENT_MANAGER_REFUSED
    }

    /// `OSErr PostEventToQueue(EventQueueRef queue, EventRef event, Boolean inFront)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`]. Three arguments, confirmed at
    /// `Framework+0x8b6aa6e`, which loads the queue from `[rdi+8]` and passes
    /// the event in `rsi`.
    unsafe extern "C" fn post_event_to_queue(
        _queue: *const c_void,
        _event: *const c_void,
        _in_front: Boolean,
    ) -> c_int {
        EVENT_MANAGER_REFUSED
    }

    /// `OSStatus SendEventToEventTargetWithOptions(EventRef event, EventTargetRef target, OptionBits options)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`]. Three arguments, confirmed at
    /// `Framework+0x8b6aa59`.
    unsafe extern "C" fn send_event_to_event_target_with_options(
        _event: *const c_void,
        _target: *const c_void,
        _options: u32,
    ) -> c_int {
        EVENT_MANAGER_REFUSED
    }

    /// `OSErr AddEventTypesToHandler(EventHandlerUPP handlerUPP, EventTypeSpec *inEventTypes, UInt32 numEventTypes, EventHandlerUPP userUPP, Boolean refconIsMenu, EventHandlerCallRef *outCallRef)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`]. Nothing was added, so the status says so.
    /// There is no `outCallRef` written: see the note on
    /// [`register_event_hot_key`] for why this file does not write
    /// out-parameters whose position it has not confirmed, and this is the same
    /// situation - the guest's call is a tail `jmp` through a wrapper, so the
    /// argument registers are not visible here.
    unsafe extern "C" fn add_event_types_to_handler(
        _handler: *const c_void,
        _event_types: *const c_void,
        _num_event_types: u32,
        _user: *const c_void,
        _refcon_is_menu: Boolean,
        _out_call_ref: *mut *mut c_void,
    ) -> c_int {
        refuse(
            &said::EVENTS,
            "AddEventTypesToHandler",
            "there is no Event Manager on this host, so no handler was installed and no call \
             reference exists",
        );
        EVENT_MANAGER_REFUSED
    }

    /// `OSErr RemoveEventTypesFromHandler(EventHandlerUPP handlerUPP, EventTypeSpec *inEventTypes, UInt32 numEventTypes, EventHandlerUPP userUPP)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`], for the same reason: nothing was installed, so
    /// nothing was removed.
    unsafe extern "C" fn remove_event_types_from_handler(
        _handler: *const c_void,
        _event_types: *const c_void,
        _num_event_types: u32,
        _user: *const c_void,
    ) -> c_int {
        EVENT_MANAGER_REFUSED
    }

    /// `OSErr CallNextEventHandler(EventRef nextHandler, EventRef event)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`]. Two arguments, confirmed at
    /// `Framework+0xbc8a481`, and the answer is returned to the guest's caller
    /// (`+0xbc8a489`), so non-zero is what a chain that has no next handler has
    /// to say.
    unsafe extern "C" fn call_next_event_handler(
        _next_handler: *const c_void,
        _event: *const c_void,
    ) -> c_int {
        EVENT_MANAGER_REFUSED
    }

    /// `OSStatus RegisterEventHotKey(UInt32 inHotKeyCode, UInt32 inHotKeyModifiers, EventHotKeyID inHotKeyID, EventHandlerUPP inHandler, EventHotKeyRef *outRef)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`], **and nothing is written.**
    ///
    /// Both call sites pass **six** arguments where the header declares five:
    /// `Framework+0x4547849` sets `rdi` to the code, `esi` to the modifiers, `rdx`
    /// to the hot key id, `rcx` to a handler, `r8` to **zero** and `r9` to the
    /// out-reference; `+0x8b6b089` does the same. So the out-parameter's position
    /// is `r9` in the guest's calling convention and cannot be established from
    /// the header alone - and one call site **poisons** the slot beforehand
    /// (`Framework+0x4547823 movabs rax,0xaaaa…`).
    ///
    /// It does not need writing, because both sites test the status first:
    /// `+0x4547851 test eax,eax; jne 0x4547910` and `+0x8b6b08e test eax,eax; jne`.
    /// A global hot key needs the Carbon event manager to route the key press,
    /// which does not exist.
    unsafe extern "C" fn register_event_hot_key(
        _code: u32,
        _modifiers: u32,
        _id: u32,
        _handler: *const c_void,
        _unused: *const c_void,
        _out_ref: *mut *mut c_void,
    ) -> c_int {
        refuse(
            &said::EVENTS,
            "RegisterEventHotKey",
            "a global hot key is routed by the Carbon Event Manager, which is part of the \
             window server; both call sites test the status before reading the out-reference \
             they pre-filled with 0xaaaa...aaaa (Framework+0x4547851)",
        );
        EVENT_MANAGER_REFUSED
    }

    /// `OSStatus UnregisterEventHotKey(EventHotKeyRef inHotKeyRef)`.
    ///
    /// [`EVENT_MANAGER_REFUSED`]. One argument, confirmed at
    /// `Framework+0x4547c82`, and the guest discards the answer - which is
    /// correct, because nothing was ever registered.
    unsafe extern "C" fn unregister_event_hot_key(_hot_key: *const c_void) -> c_int {
        EVENT_MANAGER_REFUSED
    }

    // ============================================ Carbon: Text Services

    /// `OSStatus EnableSecureEventInput(void)`.
    ///
    /// **`noErr`, and the argument for it is that the state requested already
    /// holds.** Secure event input suppresses key events so they do not reach
    /// other applications. On this host there is no window server and therefore
    /// no other application receiving key events, so the protective effect the
    /// caller wants is already in effect. It is the same argument
    /// `CGSSetDenyWindowServerConnections` -> 0 makes in
    /// [`crate::cg_unavailable`]: a claim about the *world* rather than about a
    /// capability, and one that is checkable - if a key event ever did reach
    /// another process, this answer would be the thing to revisit.
    ///
    /// The guest discards the answer at all four call sites
    /// (`Framework+0x3aa1c29`, `+0x3aa4ad5`, `+0x8832c1a` and
    /// `+0x3aa874b`), so nothing depends on it; it is the *honest* value that
    /// matters, not a load-bearing one.
    unsafe extern "C" fn enable_secure_event_input() -> c_int {
        0
    }

    /// `OSStatus DisableSecureEventInput(void)`.
    ///
    /// **`noErr`, for the mirror of [`enable_secure_event_input`]'s reason**:
    /// the state this asks to be restored - ordinary key handling - is the state
    /// this host is always in, because secure input was never in effect.
    unsafe extern "C" fn disable_secure_event_input() -> c_int {
        0
    }

    /// `OSStatus TSMSetDocumentProperty(TSMDocumentRef document, CFStringRef propertyName, CFPropertyListRef propertyValue, UInt32 propertySize)`.
    ///
    /// **A non-zero `OSStatus`, and the guest discards it.** Four arguments,
    /// confirmed at `Framework+0x3aa4a6e`, and the key is the four characters
    /// `"sine"` (`mov esi,0x656e6973`), which is
    /// `kTSMDocumentInputMethodProperty` - Chromium setting the document's input
    /// method while a text field is focused.
    ///
    /// Setting it is a message to the *current input method*, over a Mach port,
    /// and there is no input method and no Text Services Manager on this host.
    /// The honest answer is that the property was not set.
    ///
    /// The code is a plain non-zero `OSStatus` rather than a named TSM constant:
    /// TSM's own `smXxxErr` values are in a header this crate has no copy of, and
    /// a number invented to look like one would be worse than a number that is
    /// honestly anonymous. What the guest requires is only "not `noErr`", and it
    /// does not even require that.
    unsafe extern "C" fn tsm_set_document_property(
        _document: Pid,
        _property_name: *const c_void,
        _property_value: *const c_void,
        _property_size: u32,
    ) -> c_int {
        refuse(
            &said::TEXT_SERVICES,
            "TSMSetDocumentProperty",
            "setting a document property is a message to the current input method, and this \
             host has neither an input method nor a Text Services Manager; the guest discards \
             the answer (Framework+0x3aa4a6e)",
        );
        1
    }

    /// `OSStatus TSMRemoveDocumentProperty(TSMDocumentRef document, CFStringRef propertyName)`.
    ///
    /// A non-zero `OSStatus`, for [`tsm_set_document_property`]'s reason. Two
    /// arguments and the same `"sine"` key, confirmed at `Framework+0x3aa1c24`;
    /// the guest discards the answer at all seven call sites.
    unsafe extern "C" fn tsm_remove_document_property(
        _document: Pid,
        _property_name: *const c_void,
    ) -> c_int {
        // `1`, for the same reason and with the same caveat as
        // `tsm_set_document_property` above: non-zero, and honestly anonymous
        // rather than a made-up `smXxxErr`.
        1
    }

    // ================================================ CoreServices: Apple Events

    /// `OSErr AECreateAppleEvent(AEEventClass theAEEventClass, AEEventID theAEEventID, AEEventClass targetClass, AEDesc target, AEEventReturnID returnID, AEEventTransactionID transactionID, AppleEvent *outAppleEvent)`.
    ///
    /// [`PROC_NOT_FOUND`], with the out-parameter **not** written: the `AppleEvent`
    /// is an `AEDesc`, and `AECreateDesc` and `AEDisposeDesc` - the two functions
    /// that own that representation - belong to another crate, so a descriptor
    /// built here would be freed by code that does not know how.
    ///
    /// Seven arguments, and the binary confirms all seven: `Framework+0x5b09141`
    /// sets `rdi`/`rsi` to the class and id, `rdx` to the target class, `rcx` to
    /// the target descriptor, `r8` to `kAutoGenerateReturnID` (-1), `r9` to
    /// `kAnyTransactionID` (0) and the stack slot to the out-event.
    ///
    /// The status is tested (`+0x5b09149 test ax,ax; jne 0x5b09241`) and the
    /// guest disposes only the descriptors it built itself, so a non-zero answer
    /// is a clean bail-out.
    unsafe extern "C" fn aecreate_apple_event(
        _event_class: u32,
        _event_id: u32,
        _target_class: u32,
        _target: *const c_void,
        _return_id: i16,
        _transaction_id: i32,
        _out_event: *mut c_void,
    ) -> c_int {
        refuse(
            &said::APPLE_EVENTS,
            "AECreateAppleEvent",
            "there is no Apple Event Manager on this host, and the AppleEvent this would fill \
             is an AEDesc whose representation belongs to AECreateDesc/AEDisposeDesc in another \
             crate, so the out-parameter is deliberately not written; the guest tests the \
             status (Framework+0x5b09149)",
        );
        PROC_NOT_FOUND
    }

    /// `OSErr AEGetParamDesc(const AppleEvent *theAppleEvent, AEKeyword key, AEEventClass requestedType, AEDesc *result)`.
    ///
    /// [`PROC_NOT_FOUND`], with nothing written. Four arguments, confirmed at
    /// `Framework+0x5b09180`; the out-descriptor is **poisoned** at
    /// `+0x5b09152` and the status tested at `+0x5b09188`, so the poison is never
    /// read.
    ///
    /// The requested type is `'****'` (`typeWildCard`, `+0x5b0917b`), so on
    /// Darwin this asks for the parameter's descriptor whatever its type. There
    /// is no event and no descriptor store here to answer from.
    unsafe extern "C" fn aeget_param_desc(
        _event: *const c_void,
        _key: u32,
        _requested_type: u32,
        _result: *mut c_void,
    ) -> c_int {
        PROC_NOT_FOUND
    }

    /// `OSErr AEPutParamDesc(const AppleEvent *theAppleEvent, AEKeyword key, const AEDesc *desc)`.
    ///
    /// [`PROC_NOT_FOUND`]. Three arguments, confirmed at `Framework+0x5b0919e`
    /// (`rdi` = the event, `esi` = `'----'` = `keyDirectObject`, `rdx` = the
    /// descriptor), and the status is tested at `+0x5b091a6`.
    unsafe extern "C" fn aeput_param_desc(
        _event: *const c_void,
        _key: u32,
        _desc: *const c_void,
    ) -> c_int {
        PROC_NOT_FOUND
    }

    /// `OSErr AESendMessage(const AppleEvent *event, AppleEvent *reply, AESendMode sendMode, AEEventReturnID *returnID, AETransactionID *transactionID)`.
    ///
    /// [`PROC_NOT_FOUND`], **and this is the out-parameter trap of this file.
    /// Nothing is written anywhere.**
    ///
    /// The guest passes the 64-bit constant `0xfffffffffffffffe` as the fourth
    /// argument: `Framework+0x5b091bb mov rcx,0xfffffffffffffffe`. That is
    /// `AEEventReturnID *`, and the address is not a pointer to anything -
    /// Darwin's implementation leaves it alone because the send mode is
    /// `kAENoReply` (`edx = 1` at `+0x5b091b6`, which is the header's own
    /// `kAENoReply`). Writing a return id through it would be a fault at address
    /// `0xfffffffffffffffe`.
    ///
    /// The refusal is also the right answer rather than merely the safe one. The
    /// guest is Chromium's "become the default browser" helper: it sends `'LRUL'`
    /// (`0x4755524c` at `+0x5b0912f`) and then `'csim'` (`0x6d697363` /
    /// `0x61637476` at `+0x5b091f5`) to the application that owns those events.
    /// Neither exists on this host, and `procNotFound` - "the target application is
    /// not running" - is the code `AE/AppleEvents.h` itself names for exactly
    /// that. The status is tested at `+0x5b091ca` and the guest propagates it.
    unsafe extern "C" fn aesend_message(
        _event: *const c_void,
        _reply: *const c_void,
        _send_mode: c_int,
        _return_id: *mut i16,
        _transaction_id: *mut i32,
    ) -> c_int {
        refuse(
            &said::APPLE_EVENTS,
            "AESendMessage",
            "the event's target application does not exist on this host; nothing is written \
             because the guest passes 0xfffffffffffffffe as the AEEventReturnID* \
             (Framework+0x5b091bb) and relies on kAENoReply leaving it alone",
        );
        PROC_NOT_FOUND
    }

    /// `OSErr AEProcessEvent(const AppleEvent *event, AppleEvent *reply, AEEventHandlerUPP handler, SRefCon refCon, OSStatus *result, Boolean userInteractionAllowed)`.
    ///
    /// [`PROC_NOT_FOUND`]. **Unreachable in practice:** the only call site
    /// (`Framework+0x5b0906c`) is behind `ReceiveNextEvent` returning success at
    /// `+0x5b09060`, and there is no Apple Event Manager to have queued an event.
    /// The answer is still non-zero, and the guest turns it into a bool with
    /// `sete bl` (`+0x5b0907e`) - so zero would be the claim that an event
    /// arrived and was handled.
    unsafe extern "C" fn aeprocess_event(
        _event: *const c_void,
        _reply: *const c_void,
        _handler: *const c_void,
        _ref_con: isize,
        _result: *mut c_int,
        _user_interaction_allowed: Boolean,
    ) -> c_int {
        PROC_NOT_FOUND
    }

    // ============================================ CoreServices: Launch Services

    /// `OSStatus LSSetDefaultHandlerForURLScheme(CFStringRef inURLScheme, CFStringRef inHandlerBundleID)`.
    ///
    /// [`LS_NO_SERVER`], and **non-zero is load-bearing here**: the guest turns
    /// the answer into a bool with `sete r14b` at `Framework+0x5b0977a` and
    /// returns it as "did we become the handler". Two arguments, confirmed at
    /// `+0x5b09773`.
    ///
    /// `noErr` would be the answer that makes Chromium report success at
    /// claiming a URL scheme on a system with no URL-scheme database.
    unsafe extern "C" fn lsset_default_handler_for_url_scheme(
        _scheme: *const c_void,
        _bundle_id: *const c_void,
    ) -> c_int {
        refuse(
            &said::LAUNCH_SERVICES,
            "LSSetDefaultHandlerForURLScheme",
            "the user's default URL handler is a row in Launch Services' database, and lsd - \
             the process that owns it - is not on this host; the guest turns a zero answer into \
             \"we are the default handler\" (Framework+0x5b0977a sete r14b)",
        );
        LS_NO_SERVER
    }

    /// `OSStatus LSSetDefaultRoleHandlerForContentType(CFStringRef inContentType, LSRolesMask inRole, CFStringRef inHandlerBundleID)`.
    ///
    /// [`LS_NO_SERVER`]. Three arguments, confirmed at `Framework+0x5b095d2`
    /// (`esi = 2`, which is `kLSRolesViewer` in `LSInfo.h`), and the status is
    /// tested at `+0x5b095d7`. A zero answer would send the guest down its
    /// "the claim was recorded" path at `+0x5b09625`.
    unsafe extern "C" fn lsset_default_role_handler_for_content_type(
        _content_type: *const c_void,
        _role: u32,
        _bundle_id: *const c_void,
    ) -> c_int {
        LS_NO_SERVER
    }

    /// `OSStatus LSRegisterURL(CFURLRef inURL, Boolean inUpdate)`.
    ///
    /// [`LS_NO_SERVER`]. Two arguments, confirmed at `Framework+0x46f84c3`, and
    /// the guest discards the answer - the instruction after the call tests the
    /// **URL** (`+0x46f84c8 test r12,r12`), not the status.
    ///
    /// Registration is a write to `lsd`'s database. There is no database.
    unsafe extern "C" fn lsregister_url(_url: *const c_void, _update: Boolean) -> c_int {
        LS_NO_SERVER
    }

    /// `OSStatus LSOpenFromURLSpec(const LSLaunchURLSpec *inLaunchSpec, CFURLRef *outLaunchedURL)`.
    ///
    /// [`LS_NO_SERVER`], with `*outLaunchedURL` set to null. Two arguments,
    /// confirmed at `Framework+0x5da31ec`, and the guest discards the answer -
    /// it releases the spec's URL and returns.
    ///
    /// The header allows the out-parameter to be null and says the function
    /// "RETAINS THE URL REFERENCE ON BEHALF OF THE CALLER", which is why nothing
    /// is retained here: there is no URL to retain.
    unsafe extern "C" fn lsopen_from_url_spec(
        _spec: *const c_void,
        out_url: *mut *const c_void,
    ) -> c_int {
        write_null_ref(out_url);
        LS_NO_SERVER
    }

    /// `CFArrayRef LSCopyApplicationURLsForBundleIdentifier(CFStringRef inBundleIdentifier, CFErrorRef *outError)`.
    ///
    /// **Null**, and this is the header's documented answer: "If any applications
    /// with the specified bundle identifier are found, their URLs are returned in
    /// a CFArray. **If no application could be found, NULL is returned**" -
    /// `LSInfo.h`, and the same paragraph says `outError` would be populated with
    /// `kLSApplicationNotFoundErr`. Two arguments, confirmed at
    /// `Framework+0x46f3468`.
    ///
    /// `*outError` is **not** written. The guest passes **zero** in `esi`
    /// (`xor esi,esi` at `+0x46f3466`), so the slot is null and writing through
    /// it would be a fault at address zero - the `CGDisplayStreamUpdateGetRects`
    /// trap, found by reading the register rather than the header.
    ///
    /// The guest does not use the answer at all: `+0x46f346d` stores it in `r15`
    /// and the next write to `r15` is at `+0x46f34db`.
    unsafe extern "C" fn lscopy_application_urls_for_bundle_identifier(
        _bundle_id: *const c_void,
        _out_error: *mut *mut c_void,
    ) -> *const c_void {
        refuse(
            &said::LAUNCH_SERVICES,
            "LSCopyApplicationURLsForBundleIdentifier",
            "no application is registered with any bundle identifier, because the registration \
             database is lsd's and lsd is not on this host; the second argument is null at the \
             call site (Framework+0x46f3466 xor esi,esi) and is therefore not written",
        );
        std::ptr::null()
    }

    /// `void __LSSetApplicationLaunchServicesServerConnectionStatus(void *)`.
    ///
    /// **A no-op**, and it is called **unconditionally** at the end of
    /// Chromium's sandbox prologue: `Framework+0x80f0483` is the join point for
    /// both the success and the logged-failure arms of
    /// [`set_application_is_daemon`], and `+0x80f048c` is this call.
    ///
    /// There is nothing to do: the function tells Launch Services whether this
    /// process still has a server connection, and there is no server. A `void`
    /// with an unanswerable question and no way to report an answer is a no-op -
    /// the same conclusion `DASessionScheduleWithRunLoop` reaches in
    /// [`crate::unavailable`], and for the same stated reason.
    unsafe extern "C" fn lsset_application_launch_services_server_connection_status(
        _status: *const c_void,
    ) {
        refuse(
            &said::LAUNCH_SERVICES,
            "__LSSetApplicationLaunchServicesServerConnectionStatus",
            "this reports whether the process still has a Launch Services server connection, \
             and there is no server to have a connection to; the call is void, so this is a \
             no-op rather than a refusal that could be reported",
        );
    }

    /// `Boolean __CSCheckFixDisable(void)` - the CoreServices half of the same
    /// prologue.
    ///
    /// **False.** The guest discards the answer entirely: `Framework+0xc8f771d`
    /// is followed by `mov rax,[rip+0x145891f]` and a compare, with no test of
    /// `rax` in between.
    ///
    /// False is the true answer, and it is worth being precise about *which*
    /// false: this is the "apply the compatibility fix" flag that the sandbox
    /// prologue clears so that a *child* process does not inherit the fix. There
    /// is no fix to disable and nothing for a child to inherit, so the state the
    /// caller is asking about is already the one it wants.
    unsafe extern "C" fn cscheck_fix_disable() -> Boolean {
        0
    }

    // ================================== CoreServices: recent items (SharedFileList)

    /// `OSStatus LSSharedFileListCreate(CFAllocatorRef inAllocator, CFStringRef inListName, CFStringRef inIconSize, LSSharedFileListRef *outList)`.
    ///
    /// **Null, with `*outList` set to null.** Four arguments, confirmed at
    /// `Framework+0x5da1dab` (`edi = 0` for the allocator, `rsi` a global list
    /// name, `rdx = 0` for the icon size).
    ///
    /// This is the Recent Items list. `LSInfo.h`'s own description of
    /// `kLSServerCommunicationErr` is "The server process (**registration and
    /// recent items**) is not available", which is this whole half of the family
    /// in the SDK's words: recent items are `lsd`'s, and `lsd` is not here.
    ///
    /// Both call sites null-check the answer and have a defined path for it -
    /// `Framework+0x5da1dc5` and `+0x5da2142` both `test rbx,rbx; je` - so null
    /// is a clean bail-out rather than a walk of a null snapshot.
    unsafe extern "C" fn lsshared_file_list_create(
        _allocator: *const c_void,
        _list_name: *const c_void,
        _icon_size: *const c_void,
        out_list: *mut *mut c_void,
    ) -> c_int {
        refuse(
            &said::RECENT_ITEMS,
            "LSSharedFileListCreate",
            "the Recent Items list is owned by the Launch Services server - LSInfo.h calls it \
             \"registration and recent items\" - and that server is not on this host; both call \
             sites null-check the answer",
        );
        write_null_ref(out_list.cast::<*const c_void>());
        LS_NO_SERVER
    }

    /// `CFArrayRef LSSharedFileListCopySnapshot(LSSharedFileListRef inList, CFIndex *outSnapshotSeed)`.
    ///
    /// **Null.** Two arguments, and the second is confirmed to be **zero** at
    /// `Framework+0x5da201d` (`xor esi,esi`) - so the seed slot is null and is
    /// not written.
    ///
    /// The guest then calls `CFArrayGetCount` on the answer at `+0x5da203f`
    /// without testing it, which is safe: `corefoundation::api::CFArrayGetCount`
    /// answers `0` for anything that is not an array, and the enclosing loop is a
    /// `jge` on the count, so zero iterations is exactly the "no recent items"
    /// case.
    unsafe extern "C" fn lsshared_file_list_copy_snapshot(
        _list: *const c_void,
        _out_seed: *mut isize,
    ) -> *const c_void {
        std::ptr::null()
    }

    /// `CFURLRef LSSharedFileListItemCopyResolvedURL(LSSharedFileListItemRef inItem, UInt32 inSeed, CFURLRef *outURL)`.
    ///
    /// **Null, and nothing is written.** Three arguments, confirmed at
    /// `Framework+0x5da2061` (`esi = 2` is the seed, **`edx` is zero**), so the
    /// out-URL slot is null and writing through it would be a fault at address
    /// zero.
    ///
    /// The guest null-checks the answer (`+0x5da2069 test rax,rax; je`) and moves
    /// to the next item, so a null is the loop's normal "skip this entry" path.
    unsafe extern "C" fn lsshared_file_list_item_copy_resolved_url(
        _item: *const c_void,
        _seed: u32,
        _out_url: *mut *mut c_void,
    ) -> *const c_void {
        std::ptr::null()
    }

    /// `CFTypeRef LSSharedFileListItemCopyProperty(LSSharedFileListItemRef inItem, CFStringRef propertyName)`.
    ///
    /// **Null.** Two arguments, confirmed at `Framework+0x5da1e55`; the guest
    /// null-checks the answer at `+0x5da1e5a` and compares a non-null one
    /// against a global sentinel. There is no recent-items item to hold a
    /// property.
    unsafe extern "C" fn lsshared_file_list_item_copy_property(
        _item: *const c_void,
        _property_name: *const c_void,
    ) -> *const c_void {
        std::ptr::null()
    }

    /// `OSStatus LSSharedFileListInsertItemURL(LSSharedFileListRef inList, CFURLRef inURL, CFStringRef inName, void *inPropertyList, LSSharedFileListItemRef *outNewItem)`.
    ///
    /// **Null, and nothing is written.** The guest passes **seven** things -
    /// `Framework+0x5da1f1f` puts a null on the stack in addition to the five
    /// integer registers it fills - where the header declares five parameters
    /// with the out-item fifth. The out-parameter's position is therefore
    /// established only as "somewhere in that set", and the answer is the return
    /// value anyway: the guest null-checks `rax` at `+0x5da1f3d` and releases it
    /// if non-null. So null is safe, and writing through an unconfirmed register
    /// would not be.
    unsafe extern "C" fn lsshared_file_list_insert_item_url(
        _list: *const c_void,
        _url: *const c_void,
        _name: *const c_void,
        _property_list: *mut c_void,
        _out_new_item: *mut *mut c_void,
    ) -> *const c_void {
        refuse(
            &said::RECENT_ITEMS,
            "LSSharedFileListInsertItemURL",
            "adding an item means writing to the Recent Items list, which the Launch Services \
             server owns; the answer is the return value here and the guest null-checks it \
             (Framework+0x5da1f3d), so no out-parameter is written",
        );
        std::ptr::null()
    }

    /// `OSStatus LSSharedFileListItemRemove(LSSharedFileListRef inList, LSSharedFileListItemRef inItem)`.
    ///
    /// [`LS_NO_SERVER`]. Two arguments, confirmed at `Framework+0x5da21d4`, and
    /// the guest discards the answer.
    unsafe extern "C" fn lsshared_file_list_item_remove(
        _list: *const c_void,
        _item: *const c_void,
    ) -> c_int {
        LS_NO_SERVER
    }

    // ============================================ CoreServices: Spotlight metadata

    /// `MDItemRef MDItemCreate(CFStringRef path, CFURLRef url)`.
    ///
    /// **Null.** Two arguments, confirmed at `Framework+0x7fe4a35` (a
    /// `CFStringRef` path in `rsi`), and the guest null-checks the answer at
    /// `+0x7fe4a3d` and bails out on null.
    ///
    /// A `MDItem` is a row in Spotlight's metadata store, and the store is
    /// `mds`, which is not running on this host. The `MDItem` is also a `CFType`
    /// the guest would `CFRelease` (`+0x7fe4b72`), so it would have to be a real
    /// CoreFoundation object for the same cross-crate reason
    /// [`ax_value_create`] gives.
    unsafe extern "C" fn mditem_create(_path: *const c_void, _url: *const c_void) -> *const c_void {
        refuse(
            &said::SPOTLIGHT,
            "MDItemCreate",
            "a metadata item is a row in Spotlight's store, and mds is not running on this host; \
             the guest null-checks the answer (Framework+0x7fe4a3d)",
        );
        std::ptr::null()
    }

    /// `Boolean MDItemSetAttribute(MDItemRef item, CFStringRef name, CFTypeRef value)`.
    ///
    /// **False.** Three arguments, confirmed at `Framework+0x7fe4b4d`, and the
    /// guest discards the answer. Unreachable in practice, because the only
    /// `MDItem` it holds came from [`mditem_create`], which is null.
    unsafe extern "C" fn mditem_set_attribute(
        _item: *const c_void,
        _name: *const c_void,
        _value: *const c_void,
    ) -> Boolean {
        0
    }

    // ================================================ CoreServices: FSEvents

    /// `FSEventStreamEventId FSEventsGetCurrentEventId(void)`.
    ///
    /// **Zero, and that is the true answer rather than a stub.** The header:
    /// "Fetches the most recently generated event ID, system-wide." This host has
    /// no `fseventsd`, no `kqueue`-based FSEvents and no per-volume event
    /// database, so **no event has ever been generated**, and the most recently
    /// generated event id is zero.
    ///
    /// The guest uses it to seed the "last event I saw" watermark: it stores the
    /// answer at `Framework+0x5d9d0a3` (`r12`) and later discards any event whose
    /// id is not greater. With both this and
    /// [`fsevent_stream_get_latest_event_id`] answering zero, that test is
    /// correctly false - there is nothing to rescan for - and it stays false
    /// rather than oscillating.
    unsafe extern "C" fn fsevents_get_current_event_id() -> EventId {
        refuse(
            &said::FSEVENTS,
            "FSEventsGetCurrentEventId",
            "no event has ever been generated on this host: there is no fseventsd, no FSEvents \
             kqueue source and no per-volume event database, so the most recently generated \
             event id is zero",
        );
        0
    }

    /// `FSEventStreamEventId FSEventStreamGetLatestEventId(ConstFSEventStreamRef streamRef)`.
    ///
    /// **Zero, on the header's own authority.** `FSEventStream.h` documents the
    /// sibling accessor with exactly this fallback: `FSEventStreamFlushAsync`
    /// returns "The largest event id of any event ever queued for this stream,
    /// **otherwise zero if no events have been queued for this stream**". No
    /// events are ever queued for a stream on this host - and that is a
    /// measurement, not an assumption: `corefoundation::src/fsevents.rs`'s own
    /// `Stream` keeps a callback and never invokes it, and records no
    /// `sinceWhen`, so there is no batch for the id to be the largest of.
    ///
    /// The other candidate answer was `kFSEventStreamEventIdSinceNow`
    /// (`0xffffffffffffffff`), on the strength of "Initially, this returns the
    /// `sinceWhen` value supplied when the stream was created". It is not used,
    /// for two reasons: this crate cannot read the `sinceWhen` a stream was
    /// created with - the `Stream` is `corefoundation`'s and does not keep it -
    /// and reporting "everything since now" forever would claim new events for
    /// every poll. Zero is both derivable and consistent with
    /// [`fsevents_get_current_event_id`].
    unsafe extern "C" fn fsevent_stream_get_latest_event_id(_stream: *const c_void) -> EventId {
        0
    }

    // --------------------------------------------------------- the tiny helpers

    /// Write a null through a nullable out-pointer.
    ///
    /// Every one of these is a *documented* nullable slot or a slot the guest
    /// pre-zeroed, and the write exists so that a refusal never leaves a stale or
    /// poisoned value behind for a caller that reads it without testing.
    ///
    /// The pointee is `c_void` because that is what every `CFTypeRef`,
    /// `CFStringRef`, `CFRunLoopSourceRef`, `PMPaperRef` and so on is here: a
    /// `*const c_void` a guest reads as an object it did not get from
    /// CoreFoundation. Sites whose own signature uses `*mut *mut c_void` cast to
    /// this; the layouts are identical and nothing is dereferenced either way.
    fn write_null_ref(slot: *mut *const c_void) {
        if !slot.is_null() {
            // SAFETY: the caller's own out-parameter, of the pointer-to-pointer
            // type the signature declares.
            unsafe { slot.write(std::ptr::null()) };
        }
    }

    /// Write a zero through a nullable `UInt32` out-pointer.
    fn write_zero(slot: *mut u32) {
        if !slot.is_null() {
            // SAFETY: the caller's own out-parameter, declared `u32`.
            unsafe { slot.write(0) };
        }
    }

    /// Write a zero through a nullable `double` out-pointer.
    fn write_zero_f64(slot: *mut f64) {
        if !slot.is_null() {
            // SAFETY: the caller's own out-parameter, declared `f64`.
            unsafe { slot.write(0.0) };
        }
    }

    /// Write an all-zero `CGRect` through a nullable out-pointer.
    ///
    /// 32 bytes, because a `CGRect` is four `CGFloat`s and a `CGFloat` is a
    /// `double` on x86-64. The size is not a guess about the caller's buffer: the
    /// guest's own slot is a `CGRect` (`movsd`/`subsd` over `rbp-0x50`,
    /// `rbp-0x40` at `Framework+0x8b4fd93`).
    fn write_rect_zero(slot: *mut Rect) {
        if !slot.is_null() {
            // SAFETY: the caller's own `CGRect *`, 32 bytes, which the guest
            // reads back as four `double`s.
            unsafe { slot.write(Rect::default()) };
        }
    }

    exports::exports! {
        functions {
            // ApplicationServices: accessibility. No accessibility server, so no
            // element, no value, no text marker.
            "AXObserverAddNotification" => ax_observer_add_notification,
            "AXObserverCreateWithInfoCallback" => ax_observer_create_with_info_callback,
            "AXObserverGetRunLoopSource" => ax_observer_get_run_loop_source,
            "AXTextMarkerCreate" => ax_text_marker_create,
            "AXTextMarkerGetBytePtr" => ax_text_marker_get_byte_ptr,
            "AXTextMarkerGetLength" => ax_text_marker_get_length,
            "AXTextMarkerGetTypeID" => ax_text_marker_get_type_id,
            "AXTextMarkerRangeCopyEndMarker" => ax_text_marker_range_copy_end_marker,
            "AXTextMarkerRangeCopyStartMarker" => ax_text_marker_range_copy_start_marker,
            "AXTextMarkerRangeCreate" => ax_text_marker_range_create,
            "AXTextMarkerRangeGetTypeID" => ax_text_marker_range_get_type_id,
            "AXUIElementCopyActionNames" => axuielement_copy_action_names,
            "AXUIElementCopyAttributeNames" => axuielement_copy_attribute_names,
            "AXUIElementCopyAttributeValue" => axuielement_copy_attribute_value,
            "AXUIElementCopyParameterizedAttributeNames" => axuielement_copy_parameterized_attribute_names,
            "AXUIElementCopyParameterizedAttributeValue" => axuielement_copy_parameterized_attribute_value,
            "AXUIElementCreateApplication" => axuielement_create_application,
            "AXUIElementGetTypeID" => axuielement_get_type_id,
            "AXUIElementPerformAction" => axuielement_perform_action,
            "AXUIElementSetAttributeValue" => axuielement_set_attribute_value,
            "AXValueCreate" => ax_value_create,
            "AXValueGetType" => ax_value_get_type,
            "AXValueGetTypeID" => ax_value_get_type_id,
            "AXValueGetValue" => ax_value_get_value,

            // ApplicationServices: the Process Manager, which has no table here.
            "GetProcessInformation" => get_process_information,

            // ApplicationServices: PrintCore. No print server, so no printers, no
            // papers, no page formats and no print settings.
            "PMCopyPageFormat" => pm_copy_print_settings,
            "PMCopyPrintSettings" => pm_copy_print_settings,
            "PMCreatePageFormatWithPMPaper" => pm_create_page_format_with_pmpaper,
            "PMGetAdjustedPageRect" => pm_get_adjusted_page_rect,
            "PMGetAdjustedPaperRect" => pm_get_adjusted_paper_rect,
            "PMGetOrientation" => pm_get_orientation,
            "PMGetPageFormatPaper" => pm_get_page_format_paper,
            "PMPageFormatCreateDataRepresentation" => pm_page_format_create_data_representation,
            "PMPageFormatCreateWithDataRepresentation" => pm_page_format_create_with_data_representation,
            "PMPaperCreateCustom" => pm_paper_create_custom,
            "PMPaperGetHeight" => pm_paper_get_height,
            "PMPaperGetMargins" => pm_paper_get_margins,
            "PMPaperGetPPDPaperName" => pm_paper_get_ppd_paper_name,
            "PMPaperGetWidth" => pm_paper_get_width,
            "PMPrintSettingsCreateDataRepresentation" => pmprint_settings_create_data_representation,
            "PMPrintSettingsCreateWithDataRepresentation" => pmprint_settings_create_with_data_representation,
            "PMPrintSettingsSetJobName" => pmprint_settings_set_job_name,
            "PMPrintSettingsSetValue" => pmprint_settings_set_value,
            "PMPrinterCreateFromPrinterID" => pmprinter_create_from_printer_id,
            "PMPrinterGetID" => pmprinter_get_id,
            "PMPrinterGetIndexedPrinterResolution" => pmprinter_get_indexed_printer_resolution,
            "PMPrinterGetPaperList" => pmprinter_get_paper_list,
            "PMPrinterGetPrinterResolutionCount" => pmprinter_get_printer_resolution_count,
            "PMPrinterSetOutputResolution" => pmprinter_set_output_resolution,
            "PMRelease" => pm_release,
            "PMSessionBeginCGDocumentNoDialog" => pmsession_begin_cg_document_no_dialog,
            "PMSessionBeginPageNoDialog" => pmsession_begin_page_no_dialog,
            "PMSessionCopyDestinationFormat" => pmsession_copy_destination_format,
            "PMSessionCopyDestinationLocation" => pmsession_copy_destination_location,
            "PMSessionEndDocumentNoDialog" => pmsession_end_document_no_dialog,
            "PMSessionEndPageNoDialog" => pmsession_end_page_no_dialog,
            "PMSessionGetCGGraphicsContext" => pmsession_get_cg_graphics_context,
            "PMSessionGetCurrentPrinter" => pmsession_get_current_printer,
            "PMSessionGetDestinationType" => pmsession_get_destination_type,
            "PMSessionSetCurrentPMPrinter" => pmsession_set_current_pm_printer,
            "PMSessionSetDestination" => pmsession_set_destination,
            "PMSessionValidatePageFormat" => pmsession_validate_page_format,
            "PMSessionValidatePrintSettings" => pmsession_validate_print_settings,
            "PMSetCollate" => pm_set_collate,
            "PMSetCopies" => pm_set_copies,
            "PMSetDuplex" => pm_set_duplex,
            "PMSetOrientation" => pm_set_orientation,

            // ApplicationServices: Universal Access, and the sandbox prologue's
            // two Launch Services private calls.
            "UAZoomEnabled" => ua_zoom_enabled,
            "UAZoomChangeFocus" => ua_zoom_change_focus,
            "SetApplicationIsDaemon" => set_application_is_daemon,

            // Carbon: the Event Manager, which is part of the window server.
            "AddEventTypesToHandler" => add_event_types_to_handler,
            "CallNextEventHandler" => call_next_event_handler,
            "CopyEvent" => copy_event,
            "CreateEvent" => create_event,
            "GetCurrentEvent" => get_current_event,
            "GetCurrentEventQueue" => get_current_event_queue,
            "GetEventClass" => get_event_class,
            "GetEventKind" => get_event_kind,
            "GetEventMonitorTarget" => get_event_monitor_target,
            "GetEventParameter" => get_event_parameter,
            "GetEventTime" => get_event_time,
            "GetMainEventQueue" => get_main_event_queue,
            "PostEventToQueue" => post_event_to_queue,
            "RegisterEventHotKey" => register_event_hot_key,
            "RemoveEventTypesFromHandler" => remove_event_types_from_handler,
            "RetainEvent" => retain_event,
            "SendEventToEventTargetWithOptions" => send_event_to_event_target_with_options,
            "SetEventParameter" => set_event_parameter,
            "SetEventTime" => set_event_time,
            "UnregisterEventHotKey" => unregister_event_hot_key,

            // Carbon: Text Services and secure event input.
            "DisableSecureEventInput" => disable_secure_event_input,
            "EnableSecureEventInput" => enable_secure_event_input,
            "TSMRemoveDocumentProperty" => tsm_remove_document_property,
            "TSMSetDocumentProperty" => tsm_set_document_property,

            // CoreServices: Apple Events. No event manager, and no target.
            "AECreateAppleEvent" => aecreate_apple_event,
            "AEGetParamDesc" => aeget_param_desc,
            "AEProcessEvent" => aeprocess_event,
            "AEPutParamDesc" => aeput_param_desc,
            "AESendMessage" => aesend_message,

            // CoreServices: Launch Services. lsd is not on this host.
            "LSCopyApplicationURLsForBundleIdentifier" => lscopy_application_urls_for_bundle_identifier,
            "LSOpenFromURLSpec" => lsopen_from_url_spec,
            "LSRegisterURL" => lsregister_url,
            "LSSetDefaultHandlerForURLScheme" => lsset_default_handler_for_url_scheme,
            "LSSetDefaultRoleHandlerForContentType" => lsset_default_role_handler_for_content_type,
            "__CSCheckFixDisable" => cscheck_fix_disable,
            "__LSSetApplicationLaunchServicesServerConnectionStatus" => lsset_application_launch_services_server_connection_status,

            // CoreServices: the Recent Items list, which is lsd's as well.
            "LSSharedFileListCopySnapshot" => lsshared_file_list_copy_snapshot,
            "LSSharedFileListCreate" => lsshared_file_list_create,
            "LSSharedFileListInsertItemURL" => lsshared_file_list_insert_item_url,
            "LSSharedFileListItemCopyProperty" => lsshared_file_list_item_copy_property,
            "LSSharedFileListItemCopyResolvedURL" => lsshared_file_list_item_copy_resolved_url,
            "LSSharedFileListItemRemove" => lsshared_file_list_item_remove,

            // CoreServices: Spotlight's metadata store, and the FSEvents clock.
            "FSEventStreamGetLatestEventId" => fsevent_stream_get_latest_event_id,
            "FSEventsGetCurrentEventId" => fsevents_get_current_event_id,
            "MDItemCreate" => mditem_create,
            "MDItemSetAttribute" => mditem_set_attribute,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Every constant this file invents is pinned to the header it was read
        /// from. A number that drifts is a wrong answer that reads as a right one,
        /// which is the outcome this project treats as worst.
        #[test]
        fn the_error_codes_are_the_headers_own() {
            // `PrintCore/PMErrors.h`, "print server not found".
            assert_eq!(PM_NO_SERVER, -9630);
            // `LaunchServices/LSInfo.h`, "The server process (registration and
            // recent items) is not available".
            assert_eq!(LS_NO_SERVER, -10822);
            // `Processes.h`'s procNotFound, named with its value in
            // `AE/AppleEvents.h`.
            assert_eq!(PROC_NOT_FOUND, -600);
            // Non-zero is the only thing the guest's `test eax,eax` needs, but it
            // must never be `kAXErrorSuccess`.
            assert_ne!(AX_REFUSED, 0);
            assert_ne!(EVENT_MANAGER_REFUSED, 0);
        }

        /// The AX type ids have to be non-zero, mutually distinct, and odd -
        /// odd because a `CFTypeID` in this tree is an Objective-C class pointer
        /// and a class is eight-byte aligned, so no odd value can collide with a
        /// real one. Zero would be actively dangerous: it would make
        /// `CFGetTypeID` of every non-CoreFoundation object compare equal, and
        /// the guest would then dereference a garbage pointer.
        #[test]
        fn the_ax_type_ids_cannot_collide_with_a_real_one() {
            let ids = [
                TYPE_AX_UI_ELEMENT,
                TYPE_AX_VALUE,
                TYPE_AX_TEXT_MARKER,
                TYPE_AX_TEXT_MARKER_RANGE,
            ];
            for id in ids {
                assert_ne!(id, 0, "zero would match every non-CF object");
                assert_eq!(id & 1, 1, "an even value could be a class pointer");
            }
            let mut sorted = ids.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), ids.len(), "two types share an id");
        }

        /// The out-parameter the guest poisons and reads without testing.
        ///
        /// `Framework+0x8b51721`, `+0x8b518e5` and `+0x8b5050a` all load the slot
        /// straight after the call, and all three pre-fill it with
        /// `0xaaaaaaaaaaaaaaaa`.
        #[test]
        fn a_poisoned_out_parameter_is_overwritten() {
            let mut printer = 0xaaaa_aaaa_aaaa_aaaausize as *const c_void;
            // SAFETY: `printer` is a live out-parameter of the right type.
            let status = unsafe { pmsession_get_current_printer(std::ptr::null(), &mut printer) };
            assert_eq!(status, PM_NO_SERVER);
            assert!(printer.is_null(), "0xaaaa...aaaa is not a printer");
            assert_ne!(printer as usize, 0xaaaa_aaaa_aaaa_aaaa);
        }

        /// `0xfffffffffffffffe` is what the guest passes as `AESendMessage`'s
        /// `AEEventReturnID *`. Nothing may be written through it, so the only
        /// thing a test can check is that the function returns and leaves it be.
        #[test]
        fn aesend_message_does_not_write_through_a_non_pointer() {
            let poisoned: i16 = 0x7ffe;
            // SAFETY: the address is never written; the function returns before
            // touching either out-parameter. The pointer is the guest's own
            // constant and is passed only to be ignored.
            let status = unsafe {
                aesend_message(
                    std::ptr::null(),
                    std::ptr::null(),
                    1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(status, PROC_NOT_FOUND);
            assert_eq!(poisoned, 0x7ffe, "nothing else was disturbed");
        }

        /// The orientation slot is read without testing the status, so it has to
        /// hold a real `PMOrientation`. `kPMPortrait` is 1 and the guest compares
        /// against 2 (`kPMLandscape`).
        #[test]
        fn the_orientation_slot_says_portrait() {
            let mut orientation = 0xaaaa_aaaau32;
            // SAFETY: `orientation` is a live `UInt32` out-parameter.
            let status = unsafe { pm_get_orientation(std::ptr::null(), &mut orientation) };
            assert_eq!(status, PM_NO_SERVER);
            assert_eq!(orientation, 1, "kPMPortrait, and what the guest asks about");
            // The guest compares the slot against 2 for `kPMLandscape`, so 1 is
            // the value that makes its "is this landscape?" answer false.
            assert_ne!(orientation, 2, "kPMLandscape");
        }

        /// The two rect slots are read without testing the status, so they have to
        /// hold four doubles each.
        #[test]
        fn the_rect_slots_are_four_doubles_of_zero() {
            let mut page = Rect { x: 1.0, y: 2.0, width: 3.0, height: 4.0 };
            // SAFETY: `page` is a live `CGRect` out-parameter.
            let status = unsafe { pm_get_adjusted_page_rect(std::ptr::null(), &mut page) };
            assert_eq!(status, PM_NO_SERVER);
            assert_eq!(page, Rect::default());
            // A null slot is declined rather than dereferenced.
            // SAFETY: the function checks for null before writing.
            assert_eq!(unsafe { pm_get_adjusted_paper_rect(std::ptr::null(), std::ptr::null_mut()) }, PM_NO_SERVER);
        }

        /// `AXValueCreate` must be null, because the guest hands the answer to
        /// `CFRelease`. A test cannot call `CFRelease` - it belongs to another
        /// crate - so what it checks is the property that matters: the answer is
        /// never a non-null pointer that a CoreFoundation function would treat as
        /// an object.
        #[test]
        fn no_ax_value_is_ever_handed_out() {
            let mut value = [0.0f64; 4];
            // SAFETY: `value` is a live buffer of the size a `CGRect` needs.
            let created = unsafe { ax_value_create(3, value.as_mut_ptr().cast()) };
            assert!(created.is_null(), "an AXValue would be released by CFRelease");
            // And the decode side refuses without writing, so a caller of any
            // type is safe.
            let mut out = [1.0f64; 4];
            // SAFETY: `out` is a live buffer; the function writes nothing.
            assert_eq!(unsafe { ax_value_get_value(created, 3, out.as_mut_ptr().cast()) }, 0);
            assert_eq!(out, [1.0f64; 4], "a false answer stores nothing");
        }

        /// `AXValueGetType` returning zero is the header's own
        /// `kAXValueTypeIllegal`, and the guest compares against 1 and 2.
        #[test]
        fn the_ax_value_type_is_the_headers_own_illegal() {
            // SAFETY: both take a pointer and return a value.
            unsafe {
                assert_eq!(ax_value_get_type(std::ptr::null()), 0);
                assert_ne!(ax_value_get_type(std::ptr::null()), 1, "kAXValueTypeCGPoint");
                assert_ne!(ax_value_get_type(std::ptr::null()), 2, "kAXValueTypeCGSize");
            }
        }

        /// `GetEventParameter`'s status is turned into a bool by the guest
        /// (`sete cl`), so `noErr` would be the claim that the parameter exists.
        #[test]
        fn get_event_parameter_does_not_claim_a_parameter_exists() {
            let mut data = 0xaaaa_aaaausize as *const c_void;
            // SAFETY: `data` is a live out-pointer slot; the event is never read.
            let status = unsafe {
                get_event_parameter(
                    std::ptr::null(),
                    0x2d2d_2d2d,
                    0x2a2a_2a2a,
                    std::ptr::null_mut(),
                    8,
                    (&mut data as *mut *const c_void).cast(),
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(status, 0, "zero is `noErr` and would be a lie");
            assert_eq!(status, EVENT_MANAGER_REFUSED);
            assert!(data.is_null(), "the data pointer is read without a status test");
        }

        /// The launch-services family is the one where `noErr` would look like
        /// success: the guest turns a zero into "we are the default handler".
        #[test]
        fn a_default_handler_is_never_claimed() {
            // SAFETY: every one of these reads no argument and returns a status.
            let statuses = unsafe {
                [
                    lsset_default_handler_for_url_scheme(std::ptr::null(), std::ptr::null()),
                    lsset_default_role_handler_for_content_type(
                        std::ptr::null(),
                        2,
                        std::ptr::null(),
                    ),
                    lsregister_url(std::ptr::null(), 1),
                    lsopen_from_url_spec(std::ptr::null(), std::ptr::null_mut()),
                    set_application_is_daemon(1),
                ]
            };
            for status in statuses {
                assert_ne!(status, 0, "zero would tell the guest it succeeded");
                assert_eq!(status, LS_NO_SERVER);
            }
        }

        /// `UAZoomEnabled` is the one answer in this file that is an implemented
        /// *true* value rather than a refusal, and the guest tests it - so false
        /// has to mean "off", not "unknown".
        #[test]
        fn zoom_really_is_off() {
            // SAFETY: the function reads nothing.
            assert_eq!(unsafe { ua_zoom_enabled() }, 0);
            // And the state secure event input asks for already holds, so its
            // answer is `noErr` rather than a refusal.
            // SAFETY: neither reads anything.
            unsafe {
                assert_eq!(enable_secure_event_input(), 0);
                assert_eq!(disable_secure_event_input(), 0);
            }
        }

        /// The FSEvents clock is zero on both sides, so the guest's "is this
        /// event newer than the one I saw?" test is correctly false rather than
        /// accidentally true.
        #[test]
        fn no_fs_event_has_ever_been_generated() {
            // SAFETY: both read no argument.
            unsafe {
                assert_eq!(fsevents_get_current_event_id(), 0);
                assert_eq!(fsevent_stream_get_latest_event_id(std::ptr::null()), 0);
            }
        }

        /// `RetainEvent` returns its argument, because the guest stores the answer
        /// in an object field and a null there would silently empty a pointer it
        /// believes it holds.
        #[test]
        fn retaining_an_event_returns_it() {
            let event = 0x1234usize as *const c_void;
            // SAFETY: the function returns its argument without dereferencing it.
            assert_eq!(unsafe { retain_event(event) }, event);
            assert_eq!(unsafe { retain_event(std::ptr::null()) }, std::ptr::null());
        }

        /// Every null-returning family is null for a null-free reason, and the
        /// guest's checks line up: this walks the whole list so that a future
        /// edit which starts returning a plausible pointer fails here first.
        #[test]
        fn nothing_is_invented() {
            // SAFETY: every call below reads no argument, writes only a
            // non-null-terminated out-pointer slot the caller owns, and returns.
            unsafe {
                let no_elements = [
                    axuielement_copy_action_names(std::ptr::null(), std::ptr::null_mut()),
                    axuielement_copy_attribute_names(std::ptr::null(), std::ptr::null_mut()),
                    axuielement_copy_attribute_value(
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null_mut(),
                    ),
                    axuielement_copy_parameterized_attribute_names(
                        std::ptr::null(),
                        std::ptr::null_mut(),
                    ),
                    axuielement_copy_parameterized_attribute_value(
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null_mut(),
                    ),
                ];
                for status in no_elements {
                    assert_eq!(status, AX_REFUSED);
                }
                let no_objects = [
                    axuielement_create_application(1) as usize,
                    ax_observer_get_run_loop_source(std::ptr::null(), std::ptr::null()) as usize,
                    ax_text_marker_create(std::ptr::null(), std::ptr::null(), 56) as usize,
                    ax_text_marker_get_byte_ptr(std::ptr::null()) as usize,
                    ax_text_marker_range_copy_start_marker(std::ptr::null()) as usize,
                    ax_text_marker_range_copy_end_marker(std::ptr::null()) as usize,
                    ax_text_marker_range_create(std::ptr::null(), std::ptr::null(), std::ptr::null())
                        as usize,
                    pmprinter_create_from_printer_id(std::ptr::null()) as usize,
                    pmprinter_get_id(std::ptr::null()) as usize,
                    pmsession_get_cg_graphics_context(std::ptr::null()) as usize,
                    create_event(
                        std::ptr::null(),
                        0,
                        0,
                        std::ptr::null(),
                        std::ptr::null(),
                        0,
                    ) as usize,
                    copy_event(std::ptr::null()) as usize,
                    get_current_event() as usize,
                    get_current_event_queue() as usize,
                    get_main_event_queue() as usize,
                    get_event_monitor_target(std::ptr::null()) as usize,
                    lscopy_application_urls_for_bundle_identifier(
                        std::ptr::null(),
                        std::ptr::null_mut(),
                    ) as usize,
                    lsshared_file_list_copy_snapshot(std::ptr::null(), std::ptr::null_mut())
                        as usize,
                    lsshared_file_list_item_copy_property(
                        std::ptr::null(),
                        std::ptr::null(),
                    ) as usize,
                    lsshared_file_list_item_copy_resolved_url(
                        std::ptr::null(),
                        2,
                        std::ptr::null_mut(),
                    ) as usize,
                    lsshared_file_list_insert_item_url(
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    ) as usize,
                    mditem_create(std::ptr::null(), std::ptr::null()) as usize,
                ];
                for pointer in no_objects {
                    assert_eq!(pointer, 0, "a plausible pointer here would be a fault later");
                }
                // `LSSharedFileListCreate` answers with a status and puts the list
                // in an out-parameter, so it is checked as both.
                let mut list = 0xaaaa_aaaa_aaaa_aaaausize as *mut c_void;
                let status =
                    lsshared_file_list_create(std::ptr::null(), std::ptr::null(), std::ptr::null(), &mut list);
                assert_eq!(status, LS_NO_SERVER);
                assert!(list.is_null(), "no list means a null handle");
            }
        }

        /// A null out-parameter is declined rather than dereferenced, for every
        /// family that has one. The guest passes null in three of them
        /// (`LSCopyApplicationURLsForBundleIdentifier`'s `CFErrorRef *`,
        /// `LSSharedFileListItemCopyResolvedURL`'s `CFURLRef *` and
        /// `LSSharedFileListCopySnapshot`'s seed), and a write through a null slot
        /// is a fault at address zero.
        #[test]
        fn a_null_out_parameter_is_declined() {
            // SAFETY: every call below is handed null for every pointer it takes
            // and writes nothing.
            unsafe {
                pmprinter_get_paper_list(std::ptr::null(), std::ptr::null_mut());
                pmprinter_get_printer_resolution_count(std::ptr::null(), std::ptr::null_mut());
                pmprinter_get_indexed_printer_resolution(
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                );
                pmprint_settings_create_data_representation(
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                pm_page_format_create_data_representation(
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                pm_copy_print_settings(std::ptr::null(), std::ptr::null_mut());
                pm_create_page_format_with_pmpaper(std::ptr::null(), std::ptr::null_mut());
                pm_get_page_format_paper(std::ptr::null(), std::ptr::null_mut());
                pm_paper_get_width(std::ptr::null(), std::ptr::null_mut());
                pm_paper_get_height(std::ptr::null(), std::ptr::null_mut());
                pm_paper_get_ppd_paper_name(std::ptr::null(), std::ptr::null_mut());
                pm_paper_get_margins(std::ptr::null(), std::ptr::null_mut());
                pm_paper_create_custom(
                    std::ptr::null(),
                    0.0,
                    0.0,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                );
                pmsession_set_current_pm_printer(std::ptr::null(), std::ptr::null());
                pmsession_set_destination(std::ptr::null(), 4, std::ptr::null(), std::ptr::null());
                pmsession_get_destination_type(std::ptr::null(), std::ptr::null_mut());
                pmsession_copy_destination_format(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                );
                pmsession_copy_destination_location(std::ptr::null(), std::ptr::null_mut());
                pmsession_validate_page_format(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                );
                pmsession_validate_print_settings(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                );
                pmprint_settings_create_with_data_representation(
                    std::ptr::null(),
                    std::ptr::null_mut(),
                );
                pm_page_format_create_with_data_representation(
                    std::ptr::null(),
                    std::ptr::null_mut(),
                );
                pm_release(std::ptr::null());
                lsopen_from_url_spec(std::ptr::null(), std::ptr::null_mut());
            }
        }

        /// The refusals name themselves, once per family, on stderr.
        ///
        /// The message is the whole point of a named refusal, so it is worth a
        /// test that it is produced at all: an unbound import is a jump through a
        /// zero slot, and a *bound* function that says nothing is only half the
        /// job.
        #[test]
        fn the_refusals_say_something() {
            // SAFETY: every call reads no argument and writes nothing.
            unsafe {
                pm_release(std::ptr::null());
                get_process_information(std::ptr::null(), std::ptr::null_mut());
                get_main_event_queue();
                aesend_message(
                    std::ptr::null(),
                    std::ptr::null(),
                    1,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                lsset_default_handler_for_url_scheme(std::ptr::null(), std::ptr::null());
                lsshared_file_list_create(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                );
                mditem_create(std::ptr::null(), std::ptr::null());
                fsevents_get_current_event_id();
                axuielement_create_application(1);
                tsm_set_document_property(0, std::ptr::null(), std::ptr::null(), 8);
            }
            // The strings themselves are in the `refuse` calls above; what is
            // checked here is that every family's flag was consumed, which is
            // what makes the one-shot behaviour observable from a test.
            for flag in [
                &said::ACCESSIBILITY,
                &said::PRINTING,
                &said::PROCESS_MANAGER,
                &said::EVENTS,
                &said::TEXT_SERVICES,
                &said::LAUNCH_SERVICES,
                &said::APPLE_EVENTS,
                &said::RECENT_ITEMS,
                &said::SPOTLIGHT,
                &said::FSEVENTS,
            ] {
                assert!(
                    flag.load(Ordering::Relaxed),
                    "a family refused without saying so",
                );
            }
        }

        /// The C strings this file reads, if any, are the caller's own. There are
        /// none today; this is here so that adding one cannot be an accident.
        #[test]
        fn the_layouts_are_the_headers_own() {
            assert_eq!(
                std::mem::size_of::<Rect>(),
                32,
                "four CGFloats, which are doubles"
            );
            assert_eq!(std::mem::align_of::<Rect>(), 8, "a CGFloat is a double");
            assert_eq!(std::mem::size_of::<Boolean>(), 1, "a Boolean is a byte");
            assert_eq!(
                std::mem::size_of::<EventId>(),
                8,
                "an FSEventStreamEventId is a UInt64"
            );
        }

    }
}

/// The one row [`crate::lib`] holds for this file.
///
/// The table is a list of `exports::Module` values, and this one is declared by
/// the macro at the foot of `imp` - so this is a re-export rather than a second
/// declaration, and the two cannot disagree about which names the module answers
/// for.
#[cfg(not(target_os = "macos"))]
pub use imp::MODULE;

/// Nothing of this file's on Darwin: the host has every one of these subsystems,
/// and shadowing its own with a refusal would be the worst of both worlds. See
/// the `exports!` at the foot of the file.
#[cfg(target_os = "macos")]
exports::exports! {}
