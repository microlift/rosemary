//! Graphics and media subsystems this host answers with the truth.
//!
//! # The five families, and what each one is
//!
//! | family | names | the honest answer |
//! |---|--:|---|
//! | `IOSurface` | 24 | **no `IOSurfaceRef` is ever published.** `IOSurfaceCreate` and `IOSurfaceLookupFromMachPort` answer null; every query answers the absence (0 / null); every mutation refuses in a Mach code |
//! | `MediaAccessibility` | 10 | this host has no system caption-appearance preference, so every out-parameter is written "absent" and the guest's own `!important` path runs |
//! | `AudioToolbox` | 11 | no audio device and no `AudioFile` implementation: `kAudioQueueErr_InvalidDevice`, `kAudioFileOperationNotSupportedError`, `kAudioFormatUnknownFormatError` |
//! | `AudioUnit` | 1 | this module holds no audio units, so a property-**info** query has no size to report |
//! | `OpenGL` | 2 | `kCGLNoPixelFormat` from `CGLChoosePixelFormat`, and a release that frees nothing |
//!
//! 48 function imports, all of them a zero-slot jump today. Every one is a
//! **bound function that refuses by name**, which is the whole point: an
//! unbound import is a jump through a zero, and that is how bring-up runs are
//! lost.
//!
//! # Why there is no `IOSurface` here at all
//!
//! An `IOSurfaceRef` is not a number this runtime can invent. It is a handle to
//! a shared-memory-backed, plane-table-carrying IOSurface *object* that IOSurface
//! itself owns; publishing a pointer to a structure this crate has no source
//! for is the `malloc_zone_t` bug of `PLAN` §7 with a different name, and it
//! fails silently or faults depending on linker layout.
//!
//! What makes null the *true* answer rather than a convenient one is the guest.
//! Both places that create a surface test the answer and have a path for it:
//!
//! * `ui/gfx/mac/io_surface.cc`'s `CreateIOSurface`, at `Framework+0xb29171`,
//!   tests at `+0xb29179` and on null logs **"Failed to allocate IOSurface of
//!   size WxH"** (the string is at `0xd66e021`) and returns null to its caller.
//! * The second creator, `[0x64332b0,0x6433760)`, returns the answer straight up
//!   to `+0x12af44f`, whose caller retains it only if non-null
//!   (`+0x12af457`) and branches on null a second time at `+0x12af47b`.
//!
//! So the invariant this file keeps is narrow and worth stating: **no non-null
//! `IOSurfaceRef` ever exists**, which is why every accessor below can be asked
//! about null and why the answer is "there is nothing to describe".
//!
//! # Where the binary contradicts a header
//!
//! Three places, all of which would be a crash or a silent wrong answer if the
//! header were followed. The binary is the authority in every case.
//!
//! 1. **`IOSurfaceSetPurgeable`'s second argument is never set by the guest.**
//!    The whole of `[0x8222dcf,0x8222dee)` is a prologue, a 32-bit poison store
//!    to `[rbp-0x4]`, `mov rdi,[rdi+0x120]`, `lea rdx,[rbp-0x4]` and the call.
//!    `esi` is never written anywhere in the function. Writing through it would
//!    be a wild write into whatever the register happened to hold, so
//!    [`iosurface_set_purgeable`] does not.
//! 2. **`IOSurfaceCreateMachPort` is called with an unset second argument and
//!    its *return value* is used as a `mach_port_t`.** `esi` is never written
//!    in `[0x7617ee0,0x7617fe3)`, and `+0x7617ff7` moves the return into `esi`
//!    for `mach_port_mod_refs` at `+0x7618001` - which is the function
//!    `0xd66df4b` names `IOSurfaceMachPortTraits::Retain`. The out-parameter is
//!    therefore not written here either, and the return is
//!    `MACH_PORT_NULL`, which is both the truth and the only safe value to hand
//!    to `mach_port_mod_refs`.
//! 3. **The `MediaAccessibility` getters return their value in a register, not
//!    through the pointer they are also handed.** `GetForegroundOpacity` is
//!    called with `rsi = &slot` at `+0x810a3d5` and the guest then does
//!    `movsd [rbp-0x80],xmm0` at `+0x810a3da` *and* `cmp QWORD PTR
//!    [rbp-0x48],0x0` at `+0x810a3e5`. The same shape appears for
//!    `GetWindowOpacity`, `GetBackgroundOpacity`,
//!    `GetRelativeCharacterSize` and `GetWindowRoundedCornerRadius` (all a
//!    `double` in `xmm0`), and `GetTextEdgeStyle` returns an `int` in `rax`
//!    that the guest uses as a jump-table index at `+0x810a70f`. So each of
//!    those returns its zero **and** writes eight zero bytes through the
//!    pointer - which is "absent" under every reading the binary supports,
//!    because a null pointer and a zero `Float64` are both eight zero bytes.
//!
//! # The out-parameter that had to be written anyway
//!
//! `AudioFormatGetPropertyInfo`'s fourth argument is read by the guest **before**
//! it branches on the status: `+0x10d8b46` computes `sete cl` from the return
//! and `+0x10d8b4b` has already loaded `DWORD PTR [rbp-0x24]` - that argument -
//! and `+0x10d8b53` ANDs the two. The guest pre-fills the slot, so leaving it
//! alone hands back a stale value read as a result. Every refusal in this file
//! therefore writes its out-parameters, and [`audio_format_get_property_info`]
//! says so at the store.
//!
//! # What is *not* here, and why
//!
//! * `AudioUnitGetPropertyInfo` is the one name here whose *right* home is
//!   another crate. `crates/coreaudio` already implements a real audio unit -
//!   `AudioComponentInstanceNew` builds one, and `AudioUnitGetProperty` answers
//!   every property with the exact size it writes. A second copy of those sizes
//!   in this file would be a silent-wrong-answer generator the moment
//!   `property.rs` changed, and the direction of the error matters: a size
//!   *larger* than the real one has the guest allocate too little and this
//!   runtime write past it. So this file refuses, and the note says where the
//!   answer belongs.
//! * `IOSurfaceGetBytesPerElementOfPlane` and the thirteen `kIOSurface*` data
//!   imports are not here: the first is not imported by any image in the
//!   bundle, and the data symbols are already covered by
//!   `ROSETTA3_FILL_UNBOUND_DATA=1`.
//! * The CGL pixel-format *object model* is not here. `CGLChoosePixelFormat`
//!   and `CGLReleasePixelFormat` are the only two `OpenGL` names this binary
//!   imports at all (the other five are `CG*` names that
//!   `cg_unavailable` already answers), so nothing can be built on the object
//!   they produce - and a whole-image scan finds exactly **four** references to
//!   the global that holds it, all four inside these two functions. See
//!   [`cgl_choose_pixel_format`].

#[cfg(not(target_os = "macos"))]
mod imp {
    //! The answers, and why each one is the true answer.
    //!
    //! Every signature below was taken from a real Apple header and then
    //! checked against the call site in `Chromium Framework`; where the two
    //! disagree the binary wins, and the note says so. The `Framework+0x…`
    //! addresses are the guest's own.

    use std::ffi::{c_int, c_void};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // ------------------------------------------------------------------ Mach

    /// `KERN_SUCCESS`, from `<mach/kern_return.h>`.
    const KERN_SUCCESS: i32 = 0;
    /// `KERN_INVALID_ARGUMENT`, the same header: the argument this runtime was
    /// handed is not usable, which for every mutation here means *it is null*.
    const KERN_INVALID_ARGUMENT: i32 = 1;
    /// `MACH_PORT_NULL`, `<mach/port.h>`: the null Mach port, and a null object
    /// reference.
    ///
    /// Spelled through [`KERN_SUCCESS`] on purpose, because that coincidence is
    /// the whole of [`iosurface_create_mach_port`]'s answer: the two constants
    /// share a value, so a zero read as a `kern_return_t` and a zero read as a
    /// `mach_port_t` are the same word, and there is no second value that would
    /// be true in both positions.
    const MACH_PORT_NULL: i32 = KERN_SUCCESS;

    /// A refusal, said once.
    ///
    /// `write(2)` rather than `eprintln!`, for `cg_unavailable`'s reason: a
    /// message about a refusal has to be out before the refusal is, and a
    /// buffered one can be lost.
    fn note(once: &AtomicBool, what: &str, why: &str) {
        if !once.swap(true, Ordering::Relaxed) {
            let line = format!("rosetta3: {what}: {why}\n");
            // SAFETY: a live buffer for the length written.
            unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
        }
    }

    /// One flag per reason, so a process that hits a family a thousand times says
    /// it once, and a process that hits two families says both.
    mod said {
        use std::sync::atomic::AtomicBool;

        /// No `IOSurface` can be created or looked up, so every query about one
        /// is about nothing.
        pub static SURFACE: AtomicBool = AtomicBool::new(false);
        /// A mutation of a surface that does not exist.
        pub static MUTATION: AtomicBool = AtomicBool::new(false);
        /// No audio queue: there is no audio input device to attach one to.
        pub static AUDIO_QUEUE: AtomicBool = AtomicBool::new(false);
        /// No `AudioFile` implementation, so the file API has nothing to act on.
        pub static AUDIO_FILE: AtomicBool = AtomicBool::new(false);
        /// A format property this runtime does not have.
        pub static AUDIO_FORMAT: AtomicBool = AtomicBool::new(false);
        /// No audio unit in this module, so a property-info query has no size.
        pub static AUDIO_UNIT: AtomicBool = AtomicBool::new(false);
        /// No system caption-appearance preference on this host.
        pub static CAPTION: AtomicBool = AtomicBool::new(false);
        /// No CGL pixel format: no `OpenGL.framework` pixel-format object here.
        pub static CGL: AtomicBool = AtomicBool::new(false);
    }

    /// What every accessor is being asked about, and why zero is the answer.
    const NO_SURFACE: &str =
        "there is no IOSurface on this host, so every query about one answers the absence \
         (the guest null-checks IOSurfaceCreate at Framework+0xb29179 and at +0x12af457, \
         logs \"Failed to allocate IOSurface of size\", and carries on without a surface)";

    /// What a mutation is refusing.
    const NO_SURFACE_TO_MUTATE: &str =
        "there is no IOSurface on this host to lock, unlock, set a value on or make a \
         Mach port for; the Mach code says the argument is not a usable surface";

    /// How many use-count calls this process has seen.
    ///
    /// Not a behaviour anything depends on: a count is here so that a test, and
    /// anyone reading a log, can see that the call was *reached* rather than
    /// that a reference was silently dropped.
    static USE_COUNTS: AtomicUsize = AtomicUsize::new(0);

    fn count_use_count() {
        USE_COUNTS.fetch_add(1, Ordering::Relaxed);
    }

    /// [`USE_COUNTS`], for a test that wants to see the calls reached.
    #[cfg(test)]
    fn use_counts() -> usize {
        USE_COUNTS.load(Ordering::Relaxed)
    }

    // -------------------------------------------------------------- IOSurface

    /// `IOSurfaceRef IOSurfaceCreate(CFDictionaryRef properties)`.
    ///
    /// **Null, and that is the whole feature detection** - see the module note
    /// for the two call sites and what the guest does with a null.
    ///
    /// The properties dictionary the guest built is not inspected: there is
    /// nothing on this host for any of its keys to configure.
    unsafe extern "C" fn iosurface_create(_properties: *const c_void) -> *mut c_void {
        note(&said::SURFACE, "IOSurfaceCreate refused", NO_SURFACE);
        std::ptr::null_mut()
    }

    /// `IOSurfaceRef IOSurfaceLookupFromMachPort(mach_port_t port)`.
    ///
    /// Null: there is no surface behind any Mach port here, because there is no
    /// surface. The guest null-checks the *port* first
    /// (`Framework+0x76181e8 test r14d,r14d; je`) and stores the answer without
    /// testing it again (`+0x761820a`), so the object it gets is one whose
    /// surface is null - and every accessor below answers that safely.
    unsafe extern "C" fn iosurface_lookup_from_mach_port(_port: i32) -> *mut c_void {
        note(
            &said::SURFACE,
            "IOSurfaceLookupFromMachPort refused",
            NO_SURFACE,
        );
        std::ptr::null_mut()
    }

    /// `kern_return_t IOSurfaceCreateMachPort(IOSurfaceRef sfc, mach_port_t *outPort)`.
    ///
    /// [`MACH_PORT_NULL`], which is `KERN_SUCCESS` read as a port and the null
    /// port read as a status. Both readings are the truth here, and the second
    /// one is not free: the guest uses the return value as a `mach_port_t` and
    /// hands it straight to `mach_port_mod_refs`
    /// (`Framework+0x7617ff7` into `+0x7618001`), so anything else would be a
    /// made-up port name in a real Mach call.
    ///
    /// **`outPort` is deliberately not written**, and that is a measurement
    /// rather than caution: the argument is `esi`, and the whole of
    /// `[0x7617ee0,0x7617fe3)` contains no store to `esi`, so the register
    /// holds whatever the previous call left there. See the module note.
    unsafe extern "C" fn iosurface_create_mach_port(
        _surface: *const c_void,
        _out_port: *mut i32,
    ) -> i32 {
        note(
            &said::MUTATION,
            "IOSurfaceCreateMachPort refused",
            NO_SURFACE_TO_MUTATE,
        );
        MACH_PORT_NULL
    }

    /// `void IOSurfaceIncrementUseCount(IOSurfaceRef sfc)`.
    ///
    /// Nothing to retain. The guest null-checks first
    /// (`Framework+0x72d059a test rdi,rdi; je`), and the handle was never
    /// produced by this runtime, so there is nothing whose lifetime could be
    /// extended.
    unsafe extern "C" fn iosurface_increment_use_count(_surface: *const c_void) {
        count_use_count();
    }

    /// `void IOSurfaceDecrementUseCount(IOSurfaceRef sfc)`.
    ///
    /// Nothing to release, and **nothing to free**: all nineteen of the guest's
    /// call sites null-check the surface first and then `CFRelease`s it itself
    /// (`Framework+0x3acfd15` into `+0x3acfd1d` is one), so freeing it here
    /// would be freeing a pointer twice and from the wrong allocator.
    unsafe extern "C" fn iosurface_decrement_use_count(_surface: *const c_void) {
        count_use_count();
    }

    /// `Boolean IOSurfaceIsInUse(IOSurfaceRef sfc)`.
    ///
    /// **False, and it is a real answer.** A surface is "in use" when something
    /// else still holds a use count on it, and this host has no surfaces at all.
    /// The guest converts the answer to 0/1 itself
    /// (`Framework+0x821e6fa test al,al; setne al`), so false is read as false.
    unsafe extern "C" fn iosurface_is_in_use(_surface: *const c_void) -> bool {
        false
    }

    /// `kern_return_t IOSurfaceSetValue(IOSurfaceRef sfc, CFStringRef key,
    /// CFTypeRef value, CFTypeID type)`.
    ///
    /// [`KERN_INVALID_ARGUMENT`]: the surface is null, so nothing was set. The
    /// guest discards the answer at all three of its call sites
    /// (`Framework+0xb291b0` is followed by a jump, `+0x8228389` by another,
    /// `+0x79183ed` by a register write), so this is a refusal that costs the
    /// guest nothing.
    ///
    /// `key`, `value` and `type` are not touched: at `+0xb291b0` only `rdi` and
    /// `rsi` are set, so the other two are whatever the previous call left.
    unsafe extern "C" fn iosurface_set_value(
        _surface: *const c_void,
        _key: *const c_void,
        _value: *const c_void,
        _value_type: u32,
    ) -> i32 {
        note(
            &said::MUTATION,
            "IOSurfaceSetValue refused",
            NO_SURFACE_TO_MUTATE,
        );
        KERN_INVALID_ARGUMENT
    }

    /// `kern_return_t IOSurfaceSetPurgeable(IOSurfaceRef sfc, IOPurgeabilityState
    /// *purgeabilityState)`.
    ///
    /// [`KERN_INVALID_ARGUMENT`], for [`iosurface_set_value`]'s reason.
    ///
    /// **`purgeabilityState` is not written**, and the module note says why:
    /// the guest never sets the register that carries it, so this pointer is
    /// whatever was in `rdx` - writing through it would be a wild write. The
    /// guest also discards the answer (`Framework+0x8222dee` is the epilogue).
    unsafe extern "C" fn iosurface_set_purgeable(
        _surface: *const c_void,
        _purgeability_state: *mut c_int,
    ) -> i32 {
        note(
            &said::MUTATION,
            "IOSurfaceSetPurgeable refused",
            NO_SURFACE_TO_MUTATE,
        );
        KERN_INVALID_ARGUMENT
    }

    /// `int64_t IOSurfaceAlignProperty(CFTypeRef surface, CFStringRef property)`.
    ///
    /// **Zero, and the guest really does use the answer**, so this is worth
    /// being precise about. `ui/gfx/mac/io_surface.cc` *chains* the calls:
    /// `((w + 1) & ~1) * align` is fed back in as the argument to the next
    /// (`Framework+0xb28dbd` computes it, `+0xb28dd1` calls, `+0xb28de4`
    /// multiplies the answer into the next argument, `+0xb28def` calls again).
    /// The chain's product becomes a `kIOSurfaceAlignBytes` entry in the
    /// properties dictionary - and that dictionary is only ever handed to
    /// [`iosurface_create`], which refuses. So the value is load-bearing and the
    /// create is not, which is the only reason zero is safe rather than merely
    /// convenient.
    ///
    /// The return is 64-bit: the guest multiplies it with `imul rsi,rax` after
    /// sign-extending a 32-bit count (`+0xb28dba movsxd rax,r14d`).
    unsafe extern "C" fn iosurface_align_property(
        _surface: *const c_void,
        _property: *const c_void,
    ) -> i64 {
        note(&said::SURFACE, "IOSurfaceAlignProperty", NO_SURFACE);
        0
    }

    /// `kern_return_t IOSurfaceLock(IOSurfaceRef sfc, IOSurfaceLockOptions
    /// options, void **outPtr)`.
    ///
    /// [`KERN_INVALID_ARGUMENT`], **not** `KERN_SUCCESS`, and the difference is
    /// the whole point of this entry. A success here would be a claim that this
    /// runtime holds a lock on memory it does not have; the guest would then ask
    /// for the base address, be told null, and write pixels through it. **Six**
    /// of the guest's ten call sites test the answer and have an error path
    /// (`Framework+0x822425b`, `+0x8221066`, `+0x8221296`, `+0x11297b0`,
    /// `+0x72d05b3`, `+0xc7d9475`), so an error is reported rather than crashed
    /// through. The four that do not test it (`+0xb2918b`, `+0x114891b`,
    /// `+0xc6d9bd3`, `+0xc7d97c4`) all reach the call on a surface they have
    /// just null-checked or that was reached through a non-null field, so none of
    /// them is entered on this host either.
    ///
    /// `outPtr` is written when it is given, and that costs nothing: all ten
    /// call sites pass `rdx = 0` (`xor edx,edx` at `+0x8224254` and every
    /// other), so on this guest nothing reads it - but a caller that *did* pass
    /// one would otherwise read whatever it had left there.
    unsafe extern "C" fn iosurface_lock(
        _surface: *const c_void,
        _options: u32,
        out_base_address: *mut *mut c_void,
    ) -> i32 {
        note(&said::MUTATION, "IOSurfaceLock refused", NO_SURFACE_TO_MUTATE);
        if !out_base_address.is_null() {
            // SAFETY: the caller's own out-parameter, declared non-null by
            // `__nonnull`. Null is the only value that is true: there is no
            // memory to hand over.
            unsafe { out_base_address.write(std::ptr::null_mut()) };
        }
        KERN_INVALID_ARGUMENT
    }

    /// `kern_return_t IOSurfaceUnlock(IOSurfaceRef sfc, IOSurfaceLockOptions
    /// options)`.
    ///
    /// [`KERN_INVALID_ARGUMENT`] for [`iosurface_lock`]'s reason, minus the
    /// out-parameter: there is none.
    unsafe extern "C" fn iosurface_unlock(_surface: *const c_void, _options: u32) -> i32 {
        note(
            &said::MUTATION,
            "IOSurfaceUnlock refused",
            NO_SURFACE_TO_MUTATE,
        );
        KERN_INVALID_ARGUMENT
    }

    /// `size_t IOSurfaceGetWidth(IOSurfaceRef sfc)`, and the eleven queries
    /// beside it.
    ///
    /// **Zero, and zero is the only value that is not a fabrication.** Each of
    /// these reads a field of a surface; with no surface there is no field, and
    /// "no width" is expressed as zero rather than as a plausible number. What
    /// the guest does with it varies and is worth recording:
    ///
    /// * `IOSurfaceGetWidth`/`GetHeight` feed size arithmetic, and the arithmetic
    ///   is guarded: `Framework+0x1129766 cmp esi,ecx; jg` rejects a surface
    ///   narrower than the buffer it was asked about, and a zero width takes that
    ///   branch.
    /// * `IOSurfaceGetID` is compared for *equality* between two surfaces
    ///   (`Framework+0x7925005 cmp r14d,eax; sete al`), so two absent surfaces
    ///   compare equal. There is no other value expressible for "no ID", and the
    ///   comparison is only reached with a surface in hand.
    /// * `IOSurfaceGetBaseAddress`/`GetBaseAddressOfPlane` are stored without a
    ///   check (`Framework+0x1129822 mov QWORD PTR [r13+r14*8+0x80],rax`) and
    ///   null is what a null base address is.
    ///
    /// The two base-address accessors return null and the rest return zero
    /// because that is what each one's return type means when there is nothing
    /// to describe; the byte counts and dimensions are `size_t` and the two
    /// four-character-code-shaped values are `UInt32`.
    unsafe extern "C" fn iosurface_get_width(_surface: *const c_void) -> usize {
        note(&said::SURFACE, "IOSurface query", NO_SURFACE);
        0
    }
    unsafe extern "C" fn iosurface_get_height(_surface: *const c_void) -> usize {
        0
    }
    unsafe extern "C" fn iosurface_get_bytes_per_row(_surface: *const c_void) -> usize {
        0
    }
    unsafe extern "C" fn iosurface_get_bytes_per_element(_surface: *const c_void) -> usize {
        0
    }
    unsafe extern "C" fn iosurface_get_alloc_size(_surface: *const c_void) -> usize {
        0
    }
    unsafe extern "C" fn iosurface_get_plane_count(_surface: *const c_void) -> usize {
        0
    }
    unsafe extern "C" fn iosurface_get_width_of_plane(_surface: *const c_void, _plane: usize) -> usize {
        0
    }
    unsafe extern "C" fn iosurface_get_height_of_plane(_surface: *const c_void, _plane: usize) -> usize {
        0
    }
    unsafe extern "C" fn iosurface_get_bytes_per_row_of_plane(
        _surface: *const c_void,
        _plane: usize,
    ) -> usize {
        0
    }
    unsafe extern "C" fn iosurface_get_id(_surface: *const c_void) -> u32 {
        0
    }
    unsafe extern "C" fn iosurface_get_pixel_format(_surface: *const c_void) -> u32 {
        0
    }
    unsafe extern "C" fn iosurface_get_base_address(_surface: *const c_void) -> *mut c_void {
        std::ptr::null_mut()
    }
    unsafe extern "C" fn iosurface_get_base_address_of_plane(
        _surface: *const c_void,
        _plane: usize,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    // ------------------------------------------------------ MediaAccessibility

    /// What the caption functions refuse with, and why eight zero bytes.
    ///
    /// Every one of the ten is a *query about a system preference* - the
    /// caption colours, opacities, size, edge style and font the user set in
    /// System Settings - and this host has no such preference to report. The
    /// guest proves it reads the answer twice and degrades on null:
    /// `Framework+0x810a3ac` poisons the slot with `0xaaaaaaaaaaaaaaaa`, and
    /// `+0x810a3df`/`+0x810a3e5` turn "the returned pointer is null" and "the
    /// out-slot is zero" into flags which, at `+0x810a421` and `+0x810a5ab`,
    /// substitute the CSS token **`!important`** (the eight bytes at
    /// `0xd88fba1`, and the inline copy at `0x810a5af`) for the value. That is
    /// Chromium's own "this system has no caption preference, let the page
    /// decide" rendering, reached for the right reason.
    const NO_CAPTION_PREFERENCE: &str =
        "this host has no system caption-appearance preference - no MediaAccessibility \
         service, no System Settings caption panel - so the value is reported absent and the \
         guest's own \"!important\" path runs";

    /// Writes the "absent" answer through a slot the guest pre-poisons.
    ///
    /// Eight zero bytes, and the width is not a guess: the guest itself stores a
    /// full `0xaaaaaaaaaaaaaaaa` quadword to the same slot
    /// (`Framework+0x810a3ac mov QWORD PTR [rbp-0x48],r12`) and reads it back
    /// as a quadword (`+0x810a3e5 cmp QWORD PTR [rbp-0x48],0x0`). Zero is
    /// "absent" under every reading the binary supports - a null pointer and a
    /// `0.0` `Float64` are the same eight bytes - which is why one write serves
    /// both the `Copy*` functions (whose second argument is a `CFStringRef *`)
    /// and the numeric getters (whose public prototype says `Float64 *`).
    fn write_absent(slot: *mut u64) {
        if !slot.is_null() {
            // SAFETY: the caller's own out-parameter, a quadword it filled
            // itself immediately before the call.
            unsafe { slot.write(0) };
        }
    }

    /// `CFStringRef MACaptionAppearanceCopyForegroundColor(CMAPHDisplayType
    /// display, CFStringRef *appearanceForegroundColor)`.
    ///
    /// Null in the return **and** absent through the pointer, and the guest wants
    /// both: it keeps the return in `r14` and `CFRelease`s it if non-null
    /// (`Framework+0x810a5eb test r14,r14; je` into `+0x810a5f0`), so a null
    /// return skips a release of something this runtime never produced.
    unsafe extern "C" fn macaption_appearance_copy_foreground_color(
        _display: c_int,
        out_colour: *mut u64,
    ) -> *const c_void {
        note(
            &said::CAPTION,
            "MACaptionAppearanceCopyForegroundColor",
            NO_CAPTION_PREFERENCE,
        );
        write_absent(out_colour);
        std::ptr::null()
    }

    /// `CFStringRef MACaptionAppearanceCopyBackgroundColor(CMAPHDisplayType, CFStringRef *)`.
    ///
    /// [`macaption_appearance_copy_foreground_color`]'s answer, for the same
    /// reason; the guest's site is `Framework+0x810a4f0`.
    unsafe extern "C" fn macaption_appearance_copy_background_color(
        _display: c_int,
        out_colour: *mut u64,
    ) -> *const c_void {
        note(
            &said::CAPTION,
            "MACaptionAppearanceCopyBackgroundColor",
            NO_CAPTION_PREFERENCE,
        );
        write_absent(out_colour);
        std::ptr::null()
    }

    /// `CFStringRef MACaptionAppearanceCopyWindowColor(CMAPHDisplayType, CFStringRef *)`.
    ///
    /// [`macaption_appearance_copy_foreground_color`]'s answer; the guest's site
    /// is `Framework+0x810abd1`.
    unsafe extern "C" fn macaption_appearance_copy_window_color(
        _display: c_int,
        out_colour: *mut u64,
    ) -> *const c_void {
        note(
            &said::CAPTION,
            "MACaptionAppearanceCopyWindowColor",
            NO_CAPTION_PREFERENCE,
        );
        write_absent(out_colour);
        std::ptr::null()
    }

    /// `CGFontDescriptorRef MACaptionAppearanceCopyFontDescriptorForStyle
    /// (CMAPHDisplayType display, CFStringRef style, CGFontDescriptorRef *outDescriptor)`.
    ///
    /// Null, and the guest does **not** null-check it: at
    /// `Framework+0x810ade1` it passes the return straight to
    /// `CTFontDescriptorCopyAttribute`. That is safe here and only because that
    /// function is already bound and declines a null descriptor
    /// (`crates/appkit/src/coretext.rs:735`), which makes the chain end in the
    /// null the guest tests at `+0x810adee`.
    ///
    /// The guest passes `rdx = 0` (`+0x810adcd xor edx,edx`), so there is no
    /// out-parameter to write and nothing is written.
    unsafe extern "C" fn macaption_appearance_copy_font_descriptor_for_style(
        _display: c_int,
        _style: *const c_void,
        _out_descriptor: *mut *const c_void,
    ) -> *const c_void {
        note(
            &said::CAPTION,
            "MACaptionAppearanceCopyFontDescriptorForStyle",
            NO_CAPTION_PREFERENCE,
        );
        std::ptr::null()
    }

    /// `OSStatus MACaptionAppearanceGetForegroundOpacity(CMAPHDisplayType, Float64 *)`
    /// - as the **binary** calls it.
    ///
    /// Returns `0.0` in `xmm0` and writes eight zero bytes, and the module note
    /// is the reason for both halves: the guest uses the register
    /// (`Framework+0x810a3da movsd QWORD PTR [rbp-0x80],xmm0`, then multiplies
    /// it by 36 and packs it into a colour at `+0x810a407`) *and* null-tests the
    /// pointer it was handed (`+0x810a3e5`). The flag wins, so the value is
    /// never used - but a guest that read it would get zero, which is also the
    /// truth: there is no opacity here to report.
    unsafe extern "C" fn macaption_appearance_get_foreground_opacity(
        _display: c_int,
        out_value: *mut u64,
    ) -> f64 {
        note(
            &said::CAPTION,
            "MACaptionAppearanceGetForegroundOpacity",
            NO_CAPTION_PREFERENCE,
        );
        write_absent(out_value);
        0.0
    }

    /// `MACaptionAppearanceGetBackgroundOpacity(CMAPHDisplayType, Float64 *)` -
    /// as the binary calls it, at `Framework+0x810a505`.
    unsafe extern "C" fn macaption_appearance_get_background_opacity(
        _display: c_int,
        out_value: *mut u64,
    ) -> f64 {
        note(
            &said::CAPTION,
            "MACaptionAppearanceGetBackgroundOpacity",
            NO_CAPTION_PREFERENCE,
        );
        write_absent(out_value);
        0.0
    }

    /// `MACaptionAppearanceGetWindowOpacity(CMAPHDisplayType, Float64 *)` - as
    /// the binary calls it, at `Framework+0x810abe6`.
    unsafe extern "C" fn macaption_appearance_get_window_opacity(
        _display: c_int,
        out_value: *mut u64,
    ) -> f64 {
        note(
            &said::CAPTION,
            "MACaptionAppearanceGetWindowOpacity",
            NO_CAPTION_PREFERENCE,
        );
        write_absent(out_value);
        0.0
    }

    /// `MACaptionAppearanceGetRelativeCharacterSize(CMAPHDisplayType, Double *)`
    /// - as the binary calls it, at `Framework+0x810a61d`.
    ///
    /// The guest scales the register by 100 and truncates to an integer
    /// (`+0x810a622 mulsd`, `+0x810a62a cvttsd2si`), which is a *percentage*;
    /// zero is not a plausible caption size, and it is never used because the
    /// null flag takes precedence at `+0x810a647`.
    unsafe extern "C" fn macaption_appearance_get_relative_character_size(
        _display: c_int,
        out_value: *mut u64,
    ) -> f64 {
        note(
            &said::CAPTION,
            "MACaptionAppearanceGetRelativeCharacterSize",
            NO_CAPTION_PREFERENCE,
        );
        write_absent(out_value);
        0.0
    }

    /// `MACaptionAppearanceGetWindowRoundedCornerRadius(CMAPHDisplayType, Float64 *)`
    /// - as the binary calls it, at `Framework+0x810acfc`.
    ///
    /// The guest formats the register with `"%fpx"`
    /// (`Framework+0x810ad0d`), so zero would print `0.000000px` - except that
    /// the null flag at `+0x810ad21` replaces the whole value with
    /// `!important` first, so nothing is printed from it.
    unsafe extern "C" fn macaption_appearance_get_window_rounded_corner_radius(
        _display: c_int,
        out_value: *mut u64,
    ) -> f64 {
        note(
            &said::CAPTION,
            "MACaptionAppearanceGetWindowRoundedCornerRadius",
            NO_CAPTION_PREFERENCE,
        );
        write_absent(out_value);
        0.0
    }

    /// `MACaptionAppearanceGetTextEdgeStyle(CMAPHDisplayType, CFStringRef *)` -
    /// as the binary calls it, at `Framework+0x810a6ff`.
    ///
    /// Returns **0**, and the return is an index: the guest loads
    /// `DWORD PTR [rcx+rax*4]` and jumps (`Framework+0x810a70f`), so a wrong
    /// value is a wrong jump rather than a wrong string. Index 0 is the lowest
    /// the table can be asked for, and it lands inside the function
    /// (`0x810b0b0 + 0` resolves to `Framework+0x810a718`), so the dispatch is
    /// in range by construction - there is no larger index to be out of range
    /// *with*.
    unsafe extern "C" fn macaption_appearance_get_text_edge_style(
        _display: c_int,
        out_style: *mut u64,
    ) -> c_int {
        note(
            &said::CAPTION,
            "MACaptionAppearanceGetTextEdgeStyle",
            NO_CAPTION_PREFERENCE,
        );
        write_absent(out_style);
        0
    }

    // ------------------------------------------------------------ AudioToolbox

    /// `kAudioQueueErr_InvalidDevice`, from `AudioQueue.h`'s own
    /// `CF_ENUM(OSStatus)`: *"The device assigned to the queue could not be
    /// located, or is not properly configured."*
    ///
    /// The truest code in AudioToolbox for this host, and the reason the whole
    /// `AudioQueue` family can share one value: an audio queue is a software
    /// object whose first job is to connect to audio hardware
    /// (`AudioQueue.h`, "Audio queues perform the following tasks: - Connect to
    /// audio hardware"), and this host has no audio input device for one to
    /// connect to. Every one of the guest's calls is checked -
    /// `AudioQueueNewInput` at `Framework+0x10d3fda`, `AllocateBuffer` at
    /// `+0x10d401a`, `EnqueueBuffer` at `+0x10d402e`, `Start` at `+0x10d47df` -
    /// and each has an error path that reports and carries on with the queue
    /// not running, which is exactly right: Chromium's audio input simply never
    /// starts.
    const AUDIO_QUEUE_NO_DEVICE: i32 = -66680;

    /// What the audio-queue refusals say.
    const NO_AUDIO_DEVICE: &str =
        "this host has no audio input device for an AudioQueue to connect to, so the queue \
         was not created, no buffer was allocated and nothing was enqueued; kAudioQueueErr_\
         InvalidDevice is AudioToolbox's own code for a queue whose device cannot be located";

    /// `OSStatus AudioQueueNewInput(const AudioStreamBasicDescription *inFormat,
    /// AudioQueueInputCallback inCallbackProc, void *inUserData, CFRunLoopRef
    /// inCallbackRunLoop, CFStringRef inCallbackRunLoopMode, UInt32 inFlags,
    /// AudioQueueRef *outAQ)`.
    ///
    /// Seven arguments, which the binary confirms: `rdi` format, `rsi` the
    /// callback at `+0x10d3fc6`, `rdx` the refcon, `ecx` and `r9d` zeroed,
    /// `r8` a global, and `outAQ` **on the stack** (`+0x10d3fc2 mov QWORD PTR
    /// [rsp],r14` where `r14 = rbx+0x40`, the queue field).
    ///
    /// `outAQ` is nulled as well as the status refused. The guest does not read
    /// it on the error path (`+0x10d3fdc je 0x10d3ffc` goes the other way), but
    /// it stores a pointer in its own object and nulling costs one store.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn audio_queue_new_input(
        _format: *const c_void,
        _callback: Option<
            unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const c_void, *mut u32, *const c_void),
        >,
        _user_data: *mut c_void,
        _run_loop: *const c_void,
        _run_loop_mode: *const c_void,
        _flags: u32,
        out_queue: *mut *mut c_void,
    ) -> i32 {
        note(&said::AUDIO_QUEUE, "AudioQueueNewInput refused", NO_AUDIO_DEVICE);
        if !out_queue.is_null() {
            // SAFETY: the caller's own out-parameter, declared `__nonnull`.
            unsafe { out_queue.write(std::ptr::null_mut()) };
        }
        AUDIO_QUEUE_NO_DEVICE
    }

    /// `OSStatus AudioQueueAllocateBuffer(AudioQueueRef inAQ, UInt32
    /// inBufferByteSize, AudioQueueBufferRef *outBuffer)`.
    ///
    /// `outBuffer` is nulled, and here the guest would otherwise have read a
    /// poison value: it fills the slot with `0xaaaaaaaaaaaaaaaa`
    /// (`Framework+0x10d3ffc`/`+0x10d4032`) and loads it straight into the
    /// `AudioQueueEnqueueBuffer` argument at `+0x10d401e`. It only gets there
    /// when the status is zero, which it never is - but a null in a slot the
    /// guest handed over is never the wrong answer.
    unsafe extern "C" fn audio_queue_allocate_buffer(
        _queue: *mut c_void,
        _byte_size: u32,
        out_buffer: *mut *mut c_void,
    ) -> i32 {
        note(
            &said::AUDIO_QUEUE,
            "AudioQueueAllocateBuffer refused",
            NO_AUDIO_DEVICE,
        );
        if !out_buffer.is_null() {
            // SAFETY: the caller's own out-parameter, declared `__nonnull`.
            unsafe { out_buffer.write(std::ptr::null_mut()) };
        }
        AUDIO_QUEUE_NO_DEVICE
    }

    /// `OSStatus AudioQueueEnqueueBuffer(AudioQueueRef inAQ, AudioQueueBufferRef
    /// inBuffer, UInt32 inNumPacketDescs, const AudioStreamPacketDescription
    /// *inPacketDescs)`.
    unsafe extern "C" fn audio_queue_enqueue_buffer(
        _queue: *mut c_void,
        _buffer: *mut c_void,
        _num_packet_descs: u32,
        _packet_descs: *const c_void,
    ) -> i32 {
        note(
            &said::AUDIO_QUEUE,
            "AudioQueueEnqueueBuffer refused",
            NO_AUDIO_DEVICE,
        );
        AUDIO_QUEUE_NO_DEVICE
    }

    /// `OSStatus AudioQueueStart(AudioQueueRef inAQ, const AudioTimeStamp *inStartTime)`.
    unsafe extern "C" fn audio_queue_start(
        _queue: *mut c_void,
        _start_time: *const c_void,
    ) -> i32 {
        note(&said::AUDIO_QUEUE, "AudioQueueStart refused", NO_AUDIO_DEVICE);
        AUDIO_QUEUE_NO_DEVICE
    }

    /// `OSStatus AudioQueueStop(AudioQueueRef inAQ, Boolean inImmediate)`.
    unsafe extern "C" fn audio_queue_stop(_queue: *mut c_void, _immediate: bool) -> i32 {
        note(&said::AUDIO_QUEUE, "AudioQueueStop refused", NO_AUDIO_DEVICE);
        AUDIO_QUEUE_NO_DEVICE
    }

    /// `OSStatus AudioQueueDispose(AudioQueueRef inAQ, Boolean inImmediate)`.
    ///
    /// Unreachable: the guest null-checks the queue it is holding
    /// (`Framework+0x10d4b82 test rdi,rdi; je`), and
    /// [`audio_queue_new_input`] never produced one. Refused rather than ignored
    /// so that a future path cannot mistake a no-op for a completed teardown.
    unsafe extern "C" fn audio_queue_dispose(_queue: *mut c_void, _immediate: bool) -> i32 {
        note(
            &said::AUDIO_QUEUE,
            "AudioQueueDispose refused",
            NO_AUDIO_DEVICE,
        );
        AUDIO_QUEUE_NO_DEVICE
    }

    /// `kAudioFileOperationNotSupportedError`, from `AudioFile.h`'s own
    /// `CF_ENUM(OSStatus)`: **`0x6F703F3F`**, which the header spells `'op??'`
    /// and gives as an integer precisely because of the trigraph.
    ///
    /// *"The operation cannot be performed."* That is the whole `AudioFile`
    /// situation: this crate implements no part of the file API, and
    /// [`audio_file_initialize_with_callbacks`] refuses, so the three calls that
    /// follow it have no file to act on. `kAudioFileUnspecifiedError` was the
    /// alternative and it says strictly less.
    const AUDIO_FILE_UNSUPPORTED: i32 = 0x6F70_3F3Fu32 as i32;

    /// What the `AudioFile` refusals say.
    const NO_AUDIO_FILE: &str =
        "this runtime implements no part of AudioToolbox's file API, and AudioFileInitialize\
         WithCallbacks refused, so there is no AudioFileID to set a property on, write packets \
         to or close; kAudioFileOperationNotSupportedError is the file API's own \"the operation \
         cannot be performed\"";

    /// `OSStatus AudioFileInitializeWithCallbacks(void *inClientData,
    /// AudioFile_ReadProc inReadFunc, AudioFile_WriteProc inWriteFunc,
    /// AudioFile_GetSizeProc inGetSizeFunc, AudioFile_SetSizeProc inSetSizeProc,
    /// AudioFileTypeID inFileType, const AudioStreamBasicDescription *inFormat,
    /// AudioFileFlags inFlags, AudioFileID *outAudioFile)`.
    ///
    /// Nine arguments, and the binary agrees: `rdi` the client data, `rsi` the
    /// read proc, `rdx` the write proc, `rcx` the get-size proc, `r8` the set-size
    /// proc, `r9d` the file type - the immediate is `0x61647473`, which is
    /// `'adts'`, i.e. `kAudioFileAAC_ADTSType` - and three more on the stack
    /// (`Framework+0x50aff7a` onwards).
    ///
    /// The return is checked (`Framework+0x50aff88 test eax,eax; jne
    /// 0x50affa3`) and the error path returns 3 from the enclosing function
    /// without ever touching the `AudioFileID` (`+0x50affa3` zeroes the success
    /// flag and `+0x50affb7` sets the return). `outAudioFile` is nulled anyway.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn audio_file_initialize_with_callbacks(
        _client_data: *mut c_void,
        _read_proc: Option<
            unsafe extern "C" fn(*mut c_void, i64, u32, *mut c_void, *mut u32) -> i32,
        >,
        _write_proc: Option<
            unsafe extern "C" fn(*mut c_void, i64, u32, *const c_void, *mut u32) -> i32,
        >,
        _get_size_proc: Option<unsafe extern "C" fn(*mut c_void) -> i64>,
        _set_size_proc: Option<unsafe extern "C" fn(*mut c_void, i64) -> i32>,
        _file_type: u32,
        _format: *const c_void,
        _flags: u32,
        out_audio_file: *mut *mut c_void,
    ) -> i32 {
        note(
            &said::AUDIO_FILE,
            "AudioFileInitializeWithCallbacks refused",
            NO_AUDIO_FILE,
        );
        if !out_audio_file.is_null() {
            // SAFETY: the caller's own out-parameter, declared `__nonnull`.
            unsafe { out_audio_file.write(std::ptr::null_mut()) };
        }
        AUDIO_FILE_UNSUPPORTED
    }

    /// `OSStatus AudioFileSetProperty(AudioFileID inAudioFile,
    /// AudioFilePropertyID inPropertyID, UInt32 inPropertyDataSize, const void
    /// *inPropertyData, Boolean inWriteBackAudioFileProperties)`.
    ///
    /// The guest is setting `kAudioFilePropertyClientMagicCookie` - the
    /// immediate at `Framework+0x50aff92` is `0x6d676963`, which is `'cigm'` -
    /// and the return is checked at `+0x50aff9f`, with the same
    /// return-3 path as the initialiser.
    unsafe extern "C" fn audio_file_set_property(
        _audio_file: *mut c_void,
        _property_id: u32,
        _data_size: u32,
        _data: *const c_void,
        _write_back: bool,
    ) -> i32 {
        note(
            &said::AUDIO_FILE,
            "AudioFileSetProperty refused",
            NO_AUDIO_FILE,
        );
        AUDIO_FILE_UNSUPPORTED
    }

    /// `OSStatus AudioFileWritePackets(AudioFileID inAudioFile, Boolean
    /// inUseCache, UInt32 inNumBytes, const AudioStreamPacketDescription
    /// *inPacketDescriptions, SInt64 inStartingPacket, UInt32 *ioNumPackets,
    /// const void *inBuffer)`.
    ///
    /// `ioNumPackets` is in/out, and zero packets were written, so it is set to
    /// zero. Which argument it is was read out of the call rather than assumed:
    /// `rdi` file, `rsi` zero, `rdx` the byte count, `rcx` the descriptions,
    /// `r8` the starting packet, `r9` the count pointer and one stack argument
    /// (`Framework+0x50b0f4d` onwards).
    unsafe extern "C" fn audio_file_write_packets(
        _audio_file: *mut c_void,
        _use_cache: bool,
        _num_bytes: u32,
        _packet_descriptions: *const c_void,
        _starting_packet: i64,
        io_num_packets: *mut u32,
        _buffer: *const c_void,
    ) -> i32 {
        note(
            &said::AUDIO_FILE,
            "AudioFileWritePackets refused",
            NO_AUDIO_FILE,
        );
        if !io_num_packets.is_null() {
            // SAFETY: the caller's own in/out count, which this call did not
            // advance.
            unsafe { io_num_packets.write(0) };
        }
        AUDIO_FILE_UNSUPPORTED
    }

    /// `OSStatus AudioFileClose(AudioFileID inAudioFile)`.
    ///
    /// Refused rather than answered `noErr`, because a zero here would report
    /// success for a close that did not close anything. The guest null-checks
    /// the handle (`Framework+0x50b10fd test rdi,rdi; je`) and **discards** the
    /// answer - `+0x50b1107` is the next instruction and it stores zero into its
    /// own field - so the two choices are indistinguishable to it and only one of
    /// them is true. Freeing the handle would be freeing a pointer this runtime
    /// never allocated.
    unsafe extern "C" fn audio_file_close(_audio_file: *mut c_void) -> i32 {
        note(&said::AUDIO_FILE, "AudioFileClose refused", NO_AUDIO_FILE);
        AUDIO_FILE_UNSUPPORTED
    }

    /// `kAudioFormatUnknownFormatError`, `'!fmt'`, which is
    /// `crates/coreaudio`'s own existing answer to exactly this question
    /// (`crates/coreaudio/src/format.rs:10`, used at `:69` for "a format
    /// property this implementation does not have").
    ///
    /// Reusing it rather than inventing a fourth code is the point: the tree
    /// already has a vocabulary for a missing format property, and a second
    /// spelling of the same refusal is a second thing to get wrong.
    const AUDIO_FORMAT_UNKNOWN: i32 = i32::from_be_bytes(*b"!fmt");

    /// What the format refusal says.
    const NO_FORMAT_PROPERTY: &str =
        "this runtime's AudioFormatGetProperty knows one property, kAudioFormatProperty_\
         FormatInfo, and not the one asked about; the size out-parameter is written as zero \
         because the guest reads it before it looks at the status";

    /// `OSStatus AudioFormatGetPropertyInfo(AudioFormatPropertyID inPropertyID,
    /// UInt32 inSpecifierSize, const void *inSpecifier, UInt32 *outDataSize, ...)`.
    ///
    /// **The arity here is the binary's, not the header's, and it is
    /// `AudioFormatGetProperty`'s.** The public prototype is
    /// `(const AudioStreamBasicDescription *inFormat, AudioFormatPropertyID
    /// inPropertyID, UInt32 *ioDataSize, void *outData, UInt32
    /// *ioDataSizeIsWritable)`, but the guest puts a *four-character code* in
    /// `rdi` (`Framework+0x10d8b1e` loads `0x636d7062`, and `+0x10d8b37`
    /// `0x636d706c`), a `4` in `rsi`, and two pointers in `rdx`/`rcx` - which
    /// is the shape of the sibling function this tree already implements at
    /// `crates/coreaudio/src/format.rs:57`, `(property, specifier_size,
    /// specifier, size, data)`. The property is chosen from the format's flags
    /// (`+0x10d8b0f cmp eax,0x10000`, which is
    /// `kAudioFormatFlagsAreAllClear`), which is only meaningful for a property
    /// ID.
    ///
    /// `*outDataSize` is written, and that is **required** rather than tidy: the
    /// guest reads it at `Framework+0x10d8b4b` - `mov eax,DWORD PTR
    /// [rbp-0x24]` - *before* it has branched on the status, and only then ANDs
    /// it with "the call succeeded" (`+0x10d8b46 sete cl`, `+0x10d8b53 and
    /// dl,cl`). The slot is the fourth argument, which is this `outDataSize`.
    /// Writing four zero bytes is also the width the guest reads, so nothing
    /// wider is written to a slot whose real width the binary does not pin down.
    unsafe extern "C" fn audio_format_get_property_info(
        _property: u32,
        _specifier_size: u32,
        _specifier: *const c_void,
        out_data_size: *mut u32,
        _out_data: *mut c_void,
    ) -> i32 {
        note(
            &said::AUDIO_FORMAT,
            "AudioFormatGetPropertyInfo refused",
            NO_FORMAT_PROPERTY,
        );
        if !out_data_size.is_null() {
            // SAFETY: the caller's own out-parameter, declared `__nonnull`, and
            // read by the guest before it reads the status.
            unsafe { out_data_size.write(0) };
        }
        AUDIO_FORMAT_UNKNOWN
    }

    // --------------------------------------------------------------- AudioUnit

    /// `kAudioUnitErr_InvalidComponent`, `-10877`, taken from
    /// `crates/coreaudio/src/status.rs`, which documents it as the code that
    /// stands in for "not a unit of this runtime's".
    ///
    /// That is literally the position this module is in: it holds no audio
    /// units, so a property-**info** query has no unit to ask and no size to
    /// report. `kAudioUnitErr_InvalidProperty` was the alternative and it is
    /// worse - the property is valid, it is the *unit* that is not here, and a
    /// code that blames the caller's property ID is a claim about the world that
    /// is not true.
    const AUDIO_UNIT_NO_UNIT: i32 = -10877;

    /// What the audio-unit refusal says, and where the real answer lives.
    const NO_AUDIO_UNIT: &str =
        "this runtime's audio units live in crates/coreaudio, which already answers every \
         AudioUnitGetProperty and knows each property's size; a second copy of those sizes here \
         could disagree with it, and a size that is too large has the guest allocate too little \
         and this runtime write past the end - so the query is refused rather than guessed";

    /// `OSStatus AudioUnitGetPropertyInfo(AudioUnit inAudioUnit,
    /// AudioUnitPropertyID inPropertyID, AudioUnitScope inScope, AudioUnitElement
    /// inElement, UInt32 *outDataSize, void *outData, UInt32
    /// *outDataSizeIsWritable)`.
    ///
    /// Seven arguments, which the binary confirms at `Framework+0x10d897f`
    /// onwards: `rdi` the unit, `esi` the property (`0x13` = 19 =
    /// `kAudioUnitProperty_AudioChannelLayout`), `edx` the scope (`2` =
    /// `kAudioUnitScope_Output`), `ecx` the element, `r8` the size pointer, `r9d`
    /// zero for `outData`, and one stack argument for
    /// `outDataSizeIsWritable`.
    ///
    /// `*outDataSize` is written zero and nothing else is. The guest reads that
    /// slot at `Framework+0x10d89ac`, which is *after* the status test at
    /// `+0x10d8999`, so it is belt and braces rather than necessity - but a
    /// zero size is the truth, and the alternative is handing back whatever the
    /// caller had there. `outDataSizeIsWritable` is left alone: the binary does
    /// not show what the stack slot holds, and a write of a guessed width is the
    /// bug this file exists to avoid.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn audio_unit_get_property_info(
        _unit: *mut c_void,
        _property: u32,
        _scope: u32,
        _element: u32,
        out_data_size: *mut u32,
        _out_data: *mut c_void,
        _out_data_size_is_writable: *mut u32,
    ) -> i32 {
        note(
            &said::AUDIO_UNIT,
            "AudioUnitGetPropertyInfo refused",
            NO_AUDIO_UNIT,
        );
        if !out_data_size.is_null() {
            // SAFETY: the caller's own out-parameter, declared `__nonnull`.
            unsafe { out_data_size.write(0) };
        }
        AUDIO_UNIT_NO_UNIT
    }

    // ------------------------------------------------------------------ OpenGL

    /// `kCGLNoPixelFormat`, from `OpenGL.h`: `(CGLPixelFormatObj)0`, which is
    /// what `CGLChoosePixelFormat` returns when it cannot find a format. A zero
    /// here is the framework's own documented failure answer, not a stand-in.
    const CGL_NO_PIXEL_FORMAT: *mut c_void = std::ptr::null_mut();

    /// `CGLPixelFormatObj CGLChoosePixelFormat(CGLPixelFormatAttribute *attrs,
    /// CGLPixelFormatObj *pixFormat, GLenum *glType)`.
    ///
    /// **`kCGLNoPixelFormat`, and the return is not even what makes this safe.**
    /// The guest throws the answer away: `Framework+0x1176f46` is followed
    /// straight by `jmp 0x1176f7f` (`+0x1176f4b`), which is the function's
    /// epilogue. What it *keeps* is the two out-parameters - it pre-initialises
    /// both to zero at `+0x1176f29`/`+0x1176f30` and tests the **pixel format
    /// global** it receives in `rsi` for null at `+0x1176f1f`, skipping the call
    /// when one is already there.
    ///
    /// Writing both out-parameters is therefore free and is what makes the
    /// answer consistent: the global stays null, and
    /// `CGLReleasePixelFormat` - the only other CGL name this binary imports, at
    /// `Framework+0x829f4b9` - then skips a release it has nothing to release.
    ///
    /// **Why not build one out of GLX.** The host does have Mesa and GLX, and
    /// `glXChooseFBConfig` is the nearest analogue; but a `CGLPixelFormatObj` is
    /// an `NSOpenGLPixelFormat`, and the guest would go on to pass it to
    /// `CGLDescribePixelFormat`, `CGLDestroyPixelFormat` or `CGLCreateContext`.
    /// None of those is imported by any image in the bundle, so there is nothing
    /// here to be a context for, and a handle into a structure with no source
    /// would be the `malloc_zone_t` bug wearing a different hat. The two names
    /// this file answers are the *whole* of the CGL surface in this binary; the
    /// five other `OpenGL` imports are `CG*` names
    /// (`CGMainDisplayID`, `CGRectGetWidth`, `CGRectGetHeight`,
    /// `CGDisplayIDToOpenGLDisplayMask`, `CGDirectDisplayCopyCurrentMetalDevice`)
    /// and belong to `cg_unavailable`.
    ///
    /// **And nothing else in the image touches the object**, which is a
    /// measurement rather than an argument. The pixel-format global is
    /// `0xe80cee0`, and a single pass over all 214 MB of `__TEXT,__text` for
    /// rip-relative references to it finds **four**, and four only:
    ///
    /// | address | instruction | role |
    /// |---|---|---|
    /// | `+0x1176f1f` | `cmp QWORD PTR [rip+…],0x0` | "is one already chosen?" |
    /// | `+0x1176f37` | `lea rsi,[rip+…]` | the `*pixFormat` argument |
    /// | `+0x829f4ad` | `mov rdi,QWORD PTR [rip+…]` | the release's argument |
    /// | `+0x829f4be` | `mov QWORD PTR [rip+…],0x0` | the guest clearing it |
    ///
    /// So a null there cannot be read by anything that expected a format, and
    /// `CGLReleasePixelFormat` has nothing to release. (Getting that number took
    /// three attempts: an exhaustive displacement scan first assumed no
    /// immediate followed the field, then allowed any immediate width, and each
    /// version produced a confident wrong count. The one that agrees with
    /// hand-disassembly decides the immediate width from the opcode.)
    ///
    /// The return is a pointer because that is what the header declares, while
    /// `abi/table.rs:830` describes the same call as
    /// `sig!("CGLChoosePixelFormat", I32, P, P, P)`. The two cannot disagree
    /// here: the only answer this function can give is null, and the low half of
    /// a null pointer read as an `I32` is the same zero. Worth recording rather
    /// than leaving to be found.
    unsafe extern "C" fn cgl_choose_pixel_format(
        _attributes: *const c_void,
        out_pixel_format: *mut *mut c_void,
        out_gl_type: *mut u32,
    ) -> *mut c_void {
        note(
            &said::CGL,
            "CGLChoosePixelFormat refused",
            "no OpenGL pixel format exists on this host: the CGL pixel-format object model is an \
             NSOpenGLPixelFormat graph, and the only two CGL names this binary imports are the \
             one that makes one and the one that releases it, so a handle would have nothing to \
             be a context for",
        );
        if !out_pixel_format.is_null() {
            // SAFETY: the caller's own out-parameter, declared `__nonnull`.
            unsafe { out_pixel_format.write(std::ptr::null_mut()) };
        }
        if !out_gl_type.is_null() {
            // SAFETY: the caller's own out-parameter, declared `__nonnull`.
            unsafe { out_gl_type.write(0) };
        }
        CGL_NO_PIXEL_FORMAT
    }

    /// `void CGLReleasePixelFormat(CGLPixelFormatObj pixFormat)`.
    ///
    /// A no-op, and *nothing is freed*. The guest null-checks before calling
    /// (`Framework+0x829f4b4 test rdi,rdi; je`) and clears its own global
    /// afterwards (`+0x829f4be`), so the whole function is reachable only with a
    /// handle this runtime would have to have produced - and it did not.
    /// Freeing a pointer from a foreign allocator is the one thing a release
    /// function must never do; the same reasoning as `cupsFreeDests` in
    /// `unavailable.rs`.
    unsafe extern "C" fn cgl_release_pixel_format(_pixel_format: *mut c_void) {
        note(
            &said::CGL,
            "CGLReleasePixelFormat: nothing to release",
            "no CGL pixel format was ever created here, so there is nothing to release and \
             nothing that may be freed",
        );
    }

    exports::exports! {
        functions {
            // IOSurface: the feature detection, and the absence it detects.
            "IOSurfaceCreate" => iosurface_create,
            "IOSurfaceLookupFromMachPort" => iosurface_lookup_from_mach_port,
            "IOSurfaceCreateMachPort" => iosurface_create_mach_port,
            "IOSurfaceIncrementUseCount" => iosurface_increment_use_count,
            "IOSurfaceDecrementUseCount" => iosurface_decrement_use_count,
            "IOSurfaceIsInUse" => iosurface_is_in_use,
            "IOSurfaceSetValue" => iosurface_set_value,
            "IOSurfaceSetPurgeable" => iosurface_set_purgeable,
            "IOSurfaceAlignProperty" => iosurface_align_property,
            "IOSurfaceLock" => iosurface_lock,
            "IOSurfaceUnlock" => iosurface_unlock,
            "IOSurfaceGetWidth" => iosurface_get_width,
            "IOSurfaceGetHeight" => iosurface_get_height,
            "IOSurfaceGetBytesPerRow" => iosurface_get_bytes_per_row,
            "IOSurfaceGetBytesPerElement" => iosurface_get_bytes_per_element,
            "IOSurfaceGetAllocSize" => iosurface_get_alloc_size,
            "IOSurfaceGetPlaneCount" => iosurface_get_plane_count,
            "IOSurfaceGetWidthOfPlane" => iosurface_get_width_of_plane,
            "IOSurfaceGetHeightOfPlane" => iosurface_get_height_of_plane,
            "IOSurfaceGetBytesPerRowOfPlane" => iosurface_get_bytes_per_row_of_plane,
            "IOSurfaceGetID" => iosurface_get_id,
            "IOSurfaceGetPixelFormat" => iosurface_get_pixel_format,
            "IOSurfaceGetBaseAddress" => iosurface_get_base_address,
            "IOSurfaceGetBaseAddressOfPlane" => iosurface_get_base_address_of_plane,

            // MediaAccessibility: ten queries about a system preference that
            // does not exist here, each writing "absent" through its pointer.
            "MACaptionAppearanceCopyForegroundColor" => macaption_appearance_copy_foreground_color,
            "MACaptionAppearanceCopyBackgroundColor" => macaption_appearance_copy_background_color,
            "MACaptionAppearanceCopyWindowColor" => macaption_appearance_copy_window_color,
            "MACaptionAppearanceCopyFontDescriptorForStyle" => macaption_appearance_copy_font_descriptor_for_style,
            "MACaptionAppearanceGetForegroundOpacity" => macaption_appearance_get_foreground_opacity,
            "MACaptionAppearanceGetBackgroundOpacity" => macaption_appearance_get_background_opacity,
            "MACaptionAppearanceGetWindowOpacity" => macaption_appearance_get_window_opacity,
            "MACaptionAppearanceGetRelativeCharacterSize" => macaption_appearance_get_relative_character_size,
            "MACaptionAppearanceGetWindowRoundedCornerRadius" => macaption_appearance_get_window_rounded_corner_radius,
            "MACaptionAppearanceGetTextEdgeStyle" => macaption_appearance_get_text_edge_style,

            // AudioToolbox: no audio device, and no file API.
            "AudioQueueNewInput" => audio_queue_new_input,
            "AudioQueueAllocateBuffer" => audio_queue_allocate_buffer,
            "AudioQueueEnqueueBuffer" => audio_queue_enqueue_buffer,
            "AudioQueueStart" => audio_queue_start,
            "AudioQueueStop" => audio_queue_stop,
            "AudioQueueDispose" => audio_queue_dispose,
            "AudioFileInitializeWithCallbacks" => audio_file_initialize_with_callbacks,
            "AudioFileSetProperty" => audio_file_set_property,
            "AudioFileWritePackets" => audio_file_write_packets,
            "AudioFileClose" => audio_file_close,
            "AudioFormatGetPropertyInfo" => audio_format_get_property_info,

            // AudioUnit: the one property-info query, refused rather than
            // guessed - crates/coreaudio owns the sizes.
            "AudioUnitGetPropertyInfo" => audio_unit_get_property_info,

            // OpenGL: the two CGL names, and only the two.
            "CGLChoosePixelFormat" => cgl_choose_pixel_format,
            "CGLReleasePixelFormat" => cgl_release_pixel_format,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A pointer the guest would never legitimately pass, used to prove that
        /// an argument this runtime does not understand is not written through.
        const POISON: *mut c_void = 0xdead_0000 as *mut c_void;

        /// The invariant the whole IOSurface family rests on, and the one the
        /// guest's two null checks make safe.
        #[test]
        fn no_iosurface_is_ever_published() {
            // SAFETY: every one of these reads nothing and returns a constant.
            unsafe {
                // The guest's own tests: `test rax,rax; je` at Framework+0xb29179
                // and the retain-only-if-non-null at +0x12af457.
                assert!(iosurface_create(std::ptr::null()).is_null());
                assert!(iosurface_lookup_from_mach_port(0).is_null());

                // A query about nothing answers nothing, in each return type's
                // own vocabulary.
                assert_eq!(iosurface_get_width(std::ptr::null()), 0);
                assert_eq!(iosurface_get_height(std::ptr::null()), 0);
                assert_eq!(iosurface_get_bytes_per_row(std::ptr::null()), 0);
                assert_eq!(iosurface_get_bytes_per_element(std::ptr::null()), 0);
                assert_eq!(iosurface_get_alloc_size(std::ptr::null()), 0);
                assert_eq!(iosurface_get_plane_count(std::ptr::null()), 0);
                assert_eq!(iosurface_get_width_of_plane(std::ptr::null(), 0), 0);
                assert_eq!(iosurface_get_height_of_plane(std::ptr::null(), 1), 0);
                assert_eq!(iosurface_get_bytes_per_row_of_plane(std::ptr::null(), 0), 0);
                assert_eq!(iosurface_get_id(std::ptr::null()), 0);
                assert_eq!(iosurface_get_pixel_format(std::ptr::null()), 0);
                assert!(iosurface_get_base_address(std::ptr::null()).is_null());
                assert!(iosurface_get_base_address_of_plane(std::ptr::null(), 0).is_null());

                // And the alignment the guest chains is zero, which is safe only
                // because the create it feeds refuses.
                assert_eq!(iosurface_align_property(std::ptr::null(), std::ptr::null()), 0);

                // "In use" is a real question with a real negative answer.
                assert!(!iosurface_is_in_use(std::ptr::null()));
            }
        }

        /// A lock must not report success: the guest would then ask for a base
        /// address, be told null, and write pixels through it.
        #[test]
        fn a_lock_that_locked_nothing_says_so() {
            let mut base = POISON;
            // SAFETY: `base` is a live, writable out-parameter of the right type.
            let kr = unsafe { iosurface_lock(std::ptr::null(), 0, &mut base) };
            assert_ne!(kr, KERN_SUCCESS, "success here would be a claim, not an answer");
            assert_eq!(kr, KERN_INVALID_ARGUMENT);
            // And the out-parameter is written even on the refusal, at the width
            // the guest reads, so a caller that does pass one reads null rather
            // than its own poison.
            assert!(base.is_null());
            // A null out-parameter is declined rather than dereferenced.
            // SAFETY: the function checks for null before writing.
            assert_eq!(
                unsafe { iosurface_lock(std::ptr::null(), 0, std::ptr::null_mut()) },
                KERN_INVALID_ARGUMENT
            );
            // SAFETY: reads nothing.
            assert_eq!(unsafe { iosurface_unlock(std::ptr::null(), 0) }, KERN_INVALID_ARGUMENT);
        }

        /// The three arguments the guest leaves unset, and the one the guest
        /// pre-poisons, are the whole of this test: writing through any of them
        /// would be a wild write into a register or a stack slot that holds
        /// `0xaaaaaaaa`.
        #[test]
        fn the_refusals_never_touch_an_argument_the_guest_leaves_unset() {
            let before = use_counts();
            // SAFETY: every call below is handed deliberately hostile arguments
            // and must return without dereferencing any of them.
            unsafe {
                // Framework+0x8222dcf: `esi` is never written in that function.
                assert_eq!(
                    iosurface_set_purgeable(std::ptr::null(), POISON.cast()),
                    KERN_INVALID_ARGUMENT
                );
                // Framework+0x7617ee0: `rsi` is never written before the call,
                // and the return is used as a mach_port_t, so it must be the
                // null port and not a status.
                assert_eq!(iosurface_create_mach_port(std::ptr::null(), POISON.cast()), MACH_PORT_NULL);
                // Framework+0xb291b0: only `rdi` and `rsi` are set.
                assert_eq!(
                    iosurface_set_value(std::ptr::null(), std::ptr::null(), POISON, 0xdead_beef),
                    KERN_INVALID_ARGUMENT
                );
                // The use counts are reached rather than dropped.
                iosurface_increment_use_count(std::ptr::null());
                iosurface_decrement_use_count(std::ptr::null());
            }
            assert_eq!(use_counts(), before + 2);
        }

        /// Every caption function writes "absent" through the slot the guest
        /// pre-filled with `0xaaaaaaaaaaaaaaaa`, and none of them reports a
        /// value. The `!important` substitution is Chromium's own.
        #[test]
        fn caption_appearance_reports_no_system_preference() {
            /// The value the guest itself writes to the slot
            /// (`Framework+0x810a3ac mov QWORD PTR [rbp-0x48],r12`, with
            /// `r12 = 0xaaaaaaaaaaaaaaaa` loaded at `+0x810a34b`).
            const POISONED: u64 = 0xaaaa_aaaa_aaaa_aaaa;
            let mut slot = POISONED;
            let at: *mut u64 = &mut slot;
            // SAFETY: `at` is a live, writable quadword - the width the guest
            // itself writes to the same slot at Framework+0x810a3ac.
            unsafe {
                assert!(macaption_appearance_copy_foreground_color(1, at).is_null());
                // The guest's test, verbatim: `cmp QWORD PTR [rbp-0x48],0x0; sete`.
                assert_eq!(slot, 0);
                assert!(slot == 0, "zero is what makes the guest substitute !important");

                slot = POISONED;
                assert!(macaption_appearance_copy_background_color(1, at).is_null());
                assert_eq!(slot, 0);
                slot = POISONED;
                assert!(macaption_appearance_copy_window_color(1, at).is_null());
                assert_eq!(slot, 0);

                // The numeric getters: value in the register, flag through the
                // pointer. Both are zero, which is the truth either way.
                slot = POISONED;
                assert_eq!(macaption_appearance_get_foreground_opacity(1, at), 0.0);
                assert_eq!(slot, 0);
                slot = POISONED;
                assert_eq!(macaption_appearance_get_background_opacity(1, at), 0.0);
                assert_eq!(slot, 0);
                slot = POISONED;
                assert_eq!(macaption_appearance_get_window_opacity(1, at), 0.0);
                assert_eq!(slot, 0);
                slot = POISONED;
                assert_eq!(macaption_appearance_get_relative_character_size(1, at), 0.0);
                assert_eq!(slot, 0);
                slot = POISONED;
                assert_eq!(macaption_appearance_get_window_rounded_corner_radius(1, at), 0.0);
                assert_eq!(slot, 0);

                // The edge style is a jump-table index, and 0 is the lowest one
                // the table can be asked for.
                slot = POISONED;
                assert_eq!(macaption_appearance_get_text_edge_style(1, at), 0);
                assert_eq!(slot, 0);

                // The font descriptor's return is handed straight to
                // CTFontDescriptorCopyAttribute, which declines a null - and the
                // guest passes no out-pointer at all here (rdx = 0).
                assert!(macaption_appearance_copy_font_descriptor_for_style(
                    1,
                    std::ptr::null(),
                    std::ptr::null_mut()
                )
                .is_null());

                // A null out-parameter is declined rather than dereferenced.
                assert!(macaption_appearance_copy_foreground_color(1, std::ptr::null_mut())
                    .is_null());
            }
        }

        /// The audio-queue status is AudioToolbox's own name for a device that
        /// cannot be located, and it is non-zero, which is what puts the guest on
        /// its error path at every one of its six checks.
        #[test]
        fn the_audio_queue_status_names_the_missing_device() {
            assert_eq!(AUDIO_QUEUE_NO_DEVICE, -66680);
            let mut queue = POISON;
            let mut buffer = POISON;
            // SAFETY: each out-parameter is a live, writable slot of the right
            // type, and the callbacks are null so nothing is called.
            unsafe {
                assert_eq!(
                    audio_queue_new_input(
                        std::ptr::null(),
                        None,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                        std::ptr::null(),
                        0,
                        &mut queue,
                    ),
                    AUDIO_QUEUE_NO_DEVICE
                );
                assert!(queue.is_null(), "no queue, so no handle to be used by mistake");

                assert_eq!(
                    audio_queue_allocate_buffer(std::ptr::null_mut(), 4096, &mut buffer),
                    AUDIO_QUEUE_NO_DEVICE
                );
                assert!(
                    buffer.is_null(),
                    "the guest pre-fills this slot with 0xaaaa... and would load it"
                );

                assert_eq!(
                    audio_queue_enqueue_buffer(std::ptr::null_mut(), std::ptr::null_mut(), 0, std::ptr::null()),
                    AUDIO_QUEUE_NO_DEVICE
                );
                assert_eq!(audio_queue_start(std::ptr::null_mut(), std::ptr::null()), AUDIO_QUEUE_NO_DEVICE);
                assert_eq!(audio_queue_stop(std::ptr::null_mut(), true), AUDIO_QUEUE_NO_DEVICE);
                assert_eq!(audio_queue_dispose(std::ptr::null_mut(), true), AUDIO_QUEUE_NO_DEVICE);

                // And every one of those is what the guest's `test eax,eax; je`
                // reads, so none of them takes a success path.
                assert_ne!(AUDIO_QUEUE_NO_DEVICE, 0);
            }
            // The AudioFile in/out packet count is an out-parameter too, and the
            // test for it lives with the rest of the file family below.
            let mut written = 7u32;
            // SAFETY: `written` is a live `UInt32 *`.
            assert_eq!(
                unsafe {
                    audio_file_write_packets(
                        std::ptr::null_mut(),
                        false,
                        0,
                        std::ptr::null(),
                        0,
                        &mut written,
                        std::ptr::null(),
                    )
                },
                AUDIO_FILE_UNSUPPORTED
            );
            assert_eq!(written, 0, "no packets were written");
        }

        /// The file and format statuses are non-zero, so the guest's
        /// `test eax,eax; jne` takes its error path - and the format one writes
        /// the size the guest reads *before* it looks at the status.
        #[test]
        fn the_audio_file_and_format_statuses_are_not_success() {
            // 'op??', spelled as an integer in AudioFile.h because of the
            // trigraph.
            assert_eq!(AUDIO_FILE_UNSUPPORTED, 0x6F70_3F3Fu32 as i32);
            assert_ne!(AUDIO_FILE_UNSUPPORTED, 0);
            let mut file = POISON;
            // SAFETY: `file` is a live, writable out-parameter.
            unsafe {
                assert_eq!(
                    audio_file_initialize_with_callbacks(
                        std::ptr::null_mut(),
                        None,
                        None,
                        None,
                        None,
                        u32::from_be_bytes(*b"adts"),
                        std::ptr::null(),
                        0,
                        &mut file,
                    ),
                    AUDIO_FILE_UNSUPPORTED
                );
                assert!(file.is_null());
                // The guest passes 'cigm' = kAudioFilePropertyClientMagicCookie.
                assert_eq!(
                    audio_file_set_property(std::ptr::null_mut(), u32::from_be_bytes(*b"cigm"), 0, std::ptr::null(), false),
                    AUDIO_FILE_UNSUPPORTED
                );
                // A close that closed nothing is not a successful close.
                assert_eq!(audio_file_close(std::ptr::null_mut()), AUDIO_FILE_UNSUPPORTED);
            }

            // '!fmt', which is crates/coreaudio's own existing code for a format
            // property it does not have.
            assert_eq!(AUDIO_FORMAT_UNKNOWN, i32::from_be_bytes(*b"!fmt"));
            let mut size = 0xaaaa_aaaau32;
            // SAFETY: `size` is a live, writable `UInt32 *`, and the specifier
            // and out-data pointers are never read.
            let status = unsafe {
                audio_format_get_property_info(
                    u32::from_be_bytes(*b"cmpb"),
                    4,
                    std::ptr::null(),
                    &mut size,
                    std::ptr::null_mut(),
                )
            };
            assert_eq!(status, AUDIO_FORMAT_UNKNOWN);
            assert_ne!(status, 0);
            // The load-bearing store: the guest reads this slot at
            // Framework+0x10d8b4b before it branches on the status, so leaving
            // the poison in place would hand back a stale value read as a size.
            assert_eq!(size, 0);
            // A null out-parameter is declined rather than dereferenced.
            // SAFETY: the function checks for null before writing.
            assert_eq!(
                unsafe {
                    audio_format_get_property_info(0, 0, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut())
                },
                AUDIO_FORMAT_UNKNOWN
            );
        }

        /// The audio-unit refusal blames the missing unit rather than the
        /// caller's property ID, and writes a size of zero instead of a guess.
        #[test]
        fn the_audio_unit_query_declines_rather_than_guessing_a_size() {
            assert_eq!(AUDIO_UNIT_NO_UNIT, -10877);
            let mut size = 0xaaaa_aaaau32;
            let mut writable = 0xaaaa_aaaau32;
            // SAFETY: both are live, writable slots; the unit and the data
            // pointer are null exactly as the guest passes them.
            let status = unsafe {
                audio_unit_get_property_info(
                    std::ptr::null_mut(),
                    19, // kAudioUnitProperty_AudioChannelLayout
                    2,  // kAudioUnitScope_Output
                    0,
                    &mut size,
                    std::ptr::null_mut(),
                    &mut writable,
                )
            };
            assert_eq!(status, AUDIO_UNIT_NO_UNIT);
            assert_ne!(status, 0, "the guest tests the status at Framework+0x10d8999");
            assert_eq!(size, 0, "no unit, so no size to report - and never a guess");
            // The writability flag is *not* written: the binary does not show
            // what the guest's stack slot holds, and a write of a guessed width
            // is the bug this file exists to avoid.
            assert_eq!(
                writable, 0xaaaa_aaaa,
                "left as the caller had it, because its width is not established"
            );
            // A null out-parameter is declined rather than dereferenced.
            // SAFETY: the function checks for null before writing.
            assert_eq!(
                unsafe {
                    audio_unit_get_property_info(
                        std::ptr::null_mut(),
                        19,
                        2,
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                AUDIO_UNIT_NO_UNIT
            );
        }

        /// `CGLChoosePixelFormat` reports the framework's own failure answer and
        /// leaves the guest's global null, so the release that follows has
        /// nothing to release.
        #[test]
        fn cgl_reports_no_pixel_format_and_frees_nothing() {
            let mut pixel_format = POISON;
            let mut gl_type = 0xaaaa_aaaau32;
            // SAFETY: both are live, writable out-parameters.
            let chosen = unsafe {
                cgl_choose_pixel_format(std::ptr::null(), &mut pixel_format, &mut gl_type)
            };
            // kCGLNoPixelFormat is null, and it is what OpenGL.h says a failed
            // choose returns.
            assert!(chosen.is_null());
            assert!(pixel_format.is_null(), "the guest's own null test at +0x1176f1f");
            assert_eq!(gl_type, 0, "there is no GL type to name");
            // Null out-parameters are declined rather than dereferenced.
            // SAFETY: the function checks for null before writing.
            assert!(unsafe {
                cgl_choose_pixel_format(std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut())
            }
            .is_null());
            // And the release frees nothing, including for a handle it is handed.
            // SAFETY: the function reads nothing and frees nothing.
            unsafe {
                cgl_release_pixel_format(std::ptr::null_mut());
                cgl_release_pixel_format(POISON);
            }
        }

        /// Every name this file claims is a *function*, and each one is
        /// published. A name in the table that resolved to nothing would be a
        /// zero slot wearing a bound-looking name.
        #[test]
        fn every_published_name_resolves_to_an_address() {
            for name in names() {
                assert!(
                    symbol(name).is_some(),
                    "{name} is published but resolves to nothing"
                );
            }
            // And the count, so a name silently dropped from the table shows up
            // here rather than as an unbound import in a log.
            assert_eq!(names().len(), 48);
        }
    }
}

/// The one row [`crate::lib`] holds for this file.
///
/// The table is a list of `exports::Module` values, and every one of them is
/// declared by the macro at the bottom of `imp` - so this is a re-export rather
/// than a second declaration, and the two cannot disagree about which names the
/// module answers for.
#[cfg(not(target_os = "macos"))]
pub use imp::MODULE;

/// Nothing of this file's on Darwin: the host has every one of these
/// subsystems, and shadowing its own with a refusal would be the worst of both
/// worlds. See the `exports!` at the foot of the file.
#[cfg(target_os = "macos")]
exports::exports! {}
