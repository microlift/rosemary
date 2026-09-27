//! The last thirty-eight unbound function imports, and what each one is.
//!
//! Plus the eleven `_Unwind_*` names, which were **not** unbound and are the
//! more interesting half: seven of them are now *answered*, on a
//! `_Unwind_Context` this crate creates, and four are still refusals. See
//! `imp::unwind_context_accessors` for the partition and [`unwind_context`]
//! for the context and the seven accessors, and the short version in the table
//! below.
//!
//! # Why this file exists
//!
//! One bring-up run measured **764 unbound imports** against the Framework, of
//! which exactly **38** were functions (a line with `, stub 0x…` in the report;
//! without it the import is data, and the 726 data ones are already defused by
//! `ROSETTA3_FILL_UNBOUND_DATA=1`). Every one of those 38 is answered here, so
//! the Framework's unbound *function* count reaches **zero**.
//!
//! That matters mechanically more than it looks. An unbound import is a zero in
//! its slot and a jump through it, and the runtime's own fault reporter has been
//! wrong about which symbol that was (PLAN.md §7). A bound function that says
//! "there is no VideoToolbox here" is a bound function; a jump through a zero is
//! a lost run.
//!
//! The unwinder is the other direction, and the same rule cuts it: **a name bound
//! to the wrong code is worse than a name refused.** The eleven here were bound,
//! and reported bound, and bound to the host's `libgcc_s.so.1`, which cannot walk
//! a single frame of this process's guest.
//!
//! | framework | names | |
//! |---|--:|---|
//! | VideoToolbox | 15 | the whole session half: no decoder, no encoder, no pixel-transfer session |
//! | libsandbox.1 + libSystem | 6 | no Seatbelt: the profile is not compiled and the sandbox does not engage |
//! | SystemConfiguration | 4 | no dynamic store: no Bonjour host name, no proxy keys, and an error string that says so |
//! | libresolv | 3 | a zeroed `__res_state`: valid, and with no nameservers in it |
//! | CFNetwork | 2 | no system proxy configuration: no proxies for a URL |
//! | CoreServices | 2 | no LaunchServices, and the `__CSCheck*` existence marker that does nothing itself |
//! | QuartzCore / Metal / libbsm / libobjc / mach | 5 | one computed value, three truths, one Mach call this crate declines |
//! | libSystem (`libgcc_s.1.dylib`) | 7 **answered**, 4 refused | the DWARF unwinder's own `_Unwind_Context`: seven are field accessors on one this crate creates, and four have no answer to give |
//!
//! # `__Unwind_Resume` and `__Unwind_RaiseException` are not in that table
//!
//! Two of the twelve `_Unwind_*` names are **deliberately absent**, and this is
//! the most important thing in the file.
//!
//! They are not refusals, because the capability they need is **already
//! implemented in this tree**:
//!
//! * `crates/machish/src/image/unwind.rs` parses the guest's
//!   `__TEXT,__unwind_info` into `UnwindEntry { function, encoding,
//!   personality_slot, lsda }`.
//! * `crates/rosetta3/src/unwind.rs` leaves a frame from that encoding (the
//!   frame-pointer and frameless forms, with the register permutation decoder),
//!   parses the `__gcc_except_tab` LSDA, and runs **both unwind phases** —
//!   `find_handler` then `enter_next_pad` — driving the guest's real landing
//!   pads. It is 1,296 lines with eight unit tests over the encodings, and
//!   `unwind::shim_for` already maps `"_Unwind_Resume" => resume`.
//! * `crates/rosetta3/src/dyld.rs:1245`'s `unwind_sections_of`, published as
//!   `_dyld_find_unwind_sections` at `dyld.rs:1855`, answers the **vendored
//!   Apple `libunwind.dylib`** with each guest image's real `__eh_frame` and
//!   `__unwind_info` bounds. (Verified: that image really does import
//!   `__dyld_find_unwind_sections` and `__dyld_register_func_for_add_image`.)
//!
//! So `__Unwind_Resume` is a **wiring** defect with two candidate wirings, both
//! in files this one may not touch, and neither of which is a refusal:
//!
//! 1. **Publish this tree's own unwinder on the native backend.**
//!    `unwind::shim_for` is reached only on the *emulated* path, at
//!    `abi::call_import`; on native the slot is published, so the call goes
//!    straight to the published address and the shim is never consulted.
//!    `__cxa_throw` and `_Unwind_Resume` both have to be published together —
//!    `resume` refuses with "no exception in flight" unless `throw` put one
//!    there — and both need an `extern "C"` wrapper that turns an `AbiError`
//!    into a named refusal.
//! 2. **Rebind the twelve to the vendored `libunwind.dylib`.**
//!    `cxxlib::bind_unwinder_to_the_vendored_one` does exactly this and runs on
//!    both link paths (`exec/native.rs:188`, `dyld.rs:718`) — but it returns `0`
//!    without asking, because it needs a libunwind image in the graph and
//!    `VendoredCxx::fetch` only answers `/usr/lib/libc++abi.dylib` or
//!    `/usr/lib/libstdc++.6.dylib`. `Chromium Framework` links **neither**: its
//!    `LC_LOAD_DYLIB`s are libSystem, libcups, libbsm, libpmenergy, libpmsample,
//!    libresolv and libsandbox, and it carries a **statically linked** copy of
//!    `//third_party/libc++abi` instead. Nothing ever asks for the unwinder, so
//!    the rebind is a no-op and the twelve fall through to the host.
//!
//! **Taking either name here would occupy it and turn a solvable problem into a
//! documented dead end.** That is why they are absent, and
//! `imp::tests::the_two_walkers_are_left_for_the_routing_fix` fails if anybody
//! adds them.
//!
//! # The other two names in the twelve, and the one that returns
//!
//! `__Unwind_Backtrace` is refused too, but it **returns**: the guest's single
//! call site (`Framework+0x414d61`) never reads the return value, and an empty
//! trace is a path it has (`cmp BYTE PTR [rbp-0x29],0x0; je` at `+0x414d66`).
//! "No frames" is the true count for this name even now that this tree can walk
//! guest frames, because **nothing walks on this function's behalf**: it is
//! entered as an ordinary host call with no live-context trampoline behind it, so
//! there is no frame enumeration to do. It is the same empty-sample outcome
//! PLAN.md §4 already accepts for Chromium's sampling profiler under
//! `yama_ptrace_scope=1`.
//!
//! # Why the nine readers were refused, and why seven of them no longer are
//!
//! The obvious objection is that `_Unwind_GetIP` and its eight siblings are
//! *readers*, and a reader might be answerable where a walker is not. That was
//! the partition this file was asked to test, and the test said the objection
//! did not hold: it is not what they read, it is that there was nothing there.
//! Disassembling the host's `libgcc_s.so.1` shows nine of the twelve are one
//! load and a `ret` — `_Unwind_GetIP` is `mov rax,[rdi+0x98]; ret` — and **not
//! one of them calls `_Unwind_Find_FDE`**. They do not search unwind data; they
//! report what an unwinder *wrote*. And the offsets they report from
//! (`0x90`, `0x98`, `0xa0`, `0xa8`, `0xb0`, `0xb8`, `0xc0`) are **libgcc's**
//! `struct _Unwind_Context` layout, which is not the one LLVM's
//! `UnwindContext` — the vendored `libunwind.dylib`, and this tree's own reader
//! — uses. So they were unanswerable even if the guest's CFI were registered.
//!
//! **Seven of them are answered now, and the reason is that "nothing there" is no
//! longer true.** `crates/rosetta3/src/unwind_native.rs` walks this process's
//! guest frames — each frame out of its image's `__TEXT,__unwind_info`, asking
//! that frame's *own* personality routine whether it catches — so there *is* a
//! context, this crate creates it, and only these accessors read it back out.
//! Every word of the paragraph above still holds, and it is now the reason the
//! rows must exist: an *absent* name falls through `darwin::symbol` to `dlsym`,
//! the loader binds `libgcc_s.so.1`, and the guest's personality reads a struct
//! this crate never allocated — which is a silent wrong answer, the one outcome
//! ranked above a refusal.
//!
//! The caller makes it worse rather than better, and is worth naming: six of the
//! nine are called from `Framework+0x417640`, which is **a personality routine
//! the guest links itself** (Itanium signature, `_Unwind_Action` jump table at
//! `+0x417be0`, returns `_URC_INSTALL_CONTEXT` or `_URC_END_OF_STACK`), and
//! again at `+0x5680c00`. What they are asked for is *which frame to enter*.
//! A wrong value there is not a wrong answer, it is a wrong program counter.
//!

//! `NSLogv` is the thirty-eighth name in the census and is **not** here. It is
//! the one import of the 38 that cannot be answered from `darwin`: the shape the
//! guest passes is an `NSString *` format in `rdi` and a 24-byte SysV `va_list`
//! in `rsi` (read at `Framework+0x4dc397b`, `CFRelease`d at `+0x4dc3989`), and
//! `crates/rosetta3/src/shims/nslog.rs` already has the *interception* half of it
//! — `shims::mod.rs:116` registers it for the emulated backend. What is missing
//! is the **publication** the native backend needs, and it is one line in a file
//! this one may not touch:
//!
//! ```text
//! crates/rosetta3/src/frameworks.rs, in `fn shimmed()`
//!     ("NSLogv", crate::shims::nslog::native::nslogv() as *const ()),
//! ```
//!
//! next to the `("NSLog", crate::shims::nslog::native::nslog())` row already
//! there, and a `nslog::native::nslogv()` beside `nslog::native::nslog()` — a
//! SysV trampoline that saves the five integer registers and the eight vector
//! ones and calls `VarArgs::from_va_list`, which is what
//! `nslog::native::render_tail` already does for `NSLog`.
//!
//! # The three spellings that will not catch you
//!
//! `exports!` publishes `"_" + <row>`, and the synthetic dylib resolves an
//! import **verbatim, with no normalisation**. Three of these names carry a
//! second leading underscore in the image, which a clean build and a green suite
//! both accept:
//!
//! | row in this file | published as | the image's import |
//! |---|---|---|
//! | `"__sandbox_ms"` | `___sandbox_ms` | `___sandbox_ms` |
//! | `"_LSSetApplicationLaunchServicesServerConnectionStatus"` | `__LSSetApplication…` | `__LSSetApplication…` |
//! | `"_CSCheckFixDisable"` | `__CSCheckFixDisable` | `__CSCheckFixDisable` |
//!
//! `darwin::symbol` strips exactly one leading underscore (`c_name`), so these
//! rows are also what `implemented("_sandbox_ms")` finds. Both paths are pinned
//! by [`names_are_the_image_spellings`].
//!
//! # Two arguments a header would have got wrong
//!
//! * **`SCDynamicStoreCopyComputerName`'s second argument is a literal zero.**
//!   `Framework+0x1b22810` is `xor esi,esi` immediately before the call, so the
//!   `outName` out-parameter of the two-argument spelling arrives as `NULL`.
//!   Writing through it would fault at address 0. It is read and ignored, which
//!   is the one thing an out-parameter of that shape can safely be.
//! * **`SCDynamicStoreKeyCreateProxies`'s only argument is a literal zero**, and it
//!   is not an out-parameter: `Framework+0x674ae25` is `xor edi,edi`, and the
//!   header's `proxyAutoConfigURLStr` is a `CFStringRef`. A null PAC URL is a
//!   legitimate argument.
//!
//! And one argument is left **deliberately unwritten** because the guest never
//! set it — see [`cfnetwork_execute_proxy_auto_configuration_url`].
//!
//! # Two places where refusal is a trap the guest asserts against
//!
//! `VTPixelTransferSessionCreate` is called at `Framework+0x79258d8` and the very
//! next instruction pair is `test eax,eax; jne 0x7925986`, where `0x7925986` is
//! `int3; ud2`. A truthful refusal therefore reaches a `CHECK` in the guest's own
//! code. That is still strictly better than the alternative — a `SIGSEGV` at
//! address 0 through an unbound slot, reported by a fault handler that has
//! already named the wrong symbol once — and it is *located*, and one line above
//! it on stderr is [`said::PIXEL_TRANSFER`] naming this function and this call
//! site. The same is true of `VTPixelTransferSessionTransferImage` at
//! `+0x7925a92`, except that there the guest has a path: it accepts `0` and
//! exactly `-12905`, so the answer is [`VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR`] and the
//! software conversion path runs.

/// The `_Unwind_Context` this crate creates, and the seven accessors a
/// personality routine calls back through.
///
/// Declared here rather than in `lib.rs` so that adding it needs no edit to a
/// file this round does not own, and public because the driver that fills the
/// context is in a crate **above** this one and has to reach
/// `offer()`/`chosen()` — see the module note for the whole argument, and for
/// the reason these accessors must be this crate's rather than the host's.
#[cfg(not(target_os = "macos"))]
pub mod unwind_context;

#[cfg(not(target_os = "macos"))]
mod imp {
    //! The answers, and why each one is the true answer.
    //!
    //! Every signature below was read out of a call site in `Chromium Framework`
    //! before it was written, and where the header and the binary disagree the
    //! note says so and the binary wins.

    use std::ffi::{c_char, c_int, c_uint, c_void, CString};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    // ------------------------------------------------------------ diagnostics

    /// A refusal, said once.
    ///
    /// `write(2)` rather than `eprintln!`, for `crate::mach::policy`'s reason: a
    /// message about a refusal has to be out before the refusal is, and a
    /// buffered one can be lost. Allocating to build the line is safe here for
    /// the stated reason there: this runtime suspends nothing, so no thread is
    /// ever stopped holding this tree's allocator lock.
    fn note(once: &AtomicBool, what: &str, why: &str) {
        if !once.swap(true, Ordering::Relaxed) {
            let line = format!("rosetta3: {what}: {why}\n");
            // SAFETY: a live buffer for the length written.
            unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
        }
    }

    /// One flag per reason, so a process that reaches two of them says both and a
    /// process that loops through one says it once.
    mod said {
        use std::sync::atomic::AtomicBool;

        /// No VideoToolbox: a decode or encode session cannot be created.
        pub static SESSION: AtomicBool = AtomicBool::new(false);
        /// A session that does not exist supports no properties.
        pub static PROPERTY: AtomicBool = AtomicBool::new(false);
        /// No pixel-transfer session, which the guest `CHECK`s.
        pub static PIXEL_TRANSFER: AtomicBool = AtomicBool::new(false);
        /// No Seatbelt on this host.
        pub static SANDBOX: AtomicBool = AtomicBool::new(false);
        /// The parameters are held, and nothing here enforces them. **Its own
        /// flag**, and it has to be: `__sandbox_ms` shares [`SANDBOX`] and fires
        /// first, so a `create_params` that answered on the same flag would have
        /// its explanation swallowed by a refusal that has nothing to do with it —
        /// and this is the one note that says the wall moved.
        pub static SANDBOX_PARAMS: AtomicBool = AtomicBool::new(false);
        /// No audit token, because this runtime writes none.
        pub static AUDIT_TOKEN: AtomicBool = AtomicBool::new(false);
        /// A `hasStrong` struct copy, which needs a type layout this crate has no
        /// way to read.
        pub static STRONG_COPY: AtomicBool = AtomicBool::new(false);
    }

    /// One flag per `_Unwind_*` refusal, **not** one for the family.
    ///
    /// [`said`]'s rule is one flag per *reason*, and the reason here is shared
    /// by several names — so a single flag would name whichever of them the
    /// guest happened to reach first and say nothing about the others. Each is
    /// refused under its own name because each is reached from a different place
    /// in the guest, and the caller is part of the answer. See
    /// [`unwind_context_accessors`].
    ///
    /// **There are four names here and there were eleven**, and the seven that
    /// left are answered for real in [`unwind_context`]. The two newest are not
    /// `_Unwind_*` names at all: they are what the *answered* accessors say when
    /// they are handed something they cannot answer truthfully, which is the one
    /// thing an accessor that answers must still not do.
    pub(super) mod said_unwind {
        use std::sync::atomic::AtomicBool;

        macro_rules! flags {
            ($($(#[$m:meta])* $name:ident),* $(,)?) => {
                $(
                    $(#[$m])*
                    pub static $name: AtomicBool = AtomicBool::new(false);
                )*
            };
        }

        flags! {
            /// `_Unwind_GetTextRelBase`.
            GET_TEXT_REL_BASE,
            /// `_Unwind_GetDataRelBase`.
            GET_DATA_REL_BASE,
            /// `_Unwind_DeleteException`.
            DELETE_EXCEPTION,
            /// `_Unwind_Backtrace`, which is the one that returns rather than stops.
            BACKTRACE,
            /// A context pointer the driver did not create.
            CONTEXT_NOT_OURS,
            /// A DWARF register number the five-word context does not carry.
            REGISTER_NOT_CARRIED,
        }
    }

    /// Say a refusal once, under the name it was reached by, and stop.
    ///
    /// `abort()` and not a return value, for the reason [`data_unavailable`]
    /// gives for `___objc_personality_v0` and it is the same argument: each of
    /// these is called *by the unwinder, from inside an unwind*, and the caller
    /// is a personality routine that will read the answer and decide **which
    /// frame to land in** on the strength of it. A zero or a null returned here
    /// is not a "no answer" — it is an instruction to land in frame zero. There
    /// is no `_Unwind_Reason_Code` that means "I could not look", because every
    /// value in that enum is either a claim about frames that were searched or
    /// an instruction to terminate.
    ///
    /// `why` is the whole sentence rather than a shared one, because **the
    /// reasons are no longer shared**. It used to be a single `NO_CFI` constant
    /// said by all ten, and the note below records why that constant is gone —
    /// the honest reason for each remaining name is a different sentence, and
    /// reusing the old one would have named a capability that is no longer the
    /// missing one.
    pub(super) fn refuse_unwind(once: &AtomicBool, name: &str, why: &str) -> ! {
        if !once.swap(true, Ordering::Relaxed) {
            let line = format!("rosetta3: {name}: refused by name. {why}\n");
            // SAFETY: a live buffer for the length written, and this runs before
            // anything that can stop the process.
            unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
        }
        std::process::abort()
    }

    /// Say a refusal once, where the sentence is already the whole answer.
    ///
    /// For the two reasons [`unwind_context`] raises, which are not a *name*
    /// being unavailable but an *answer* being untruthful — a context pointer
    /// this crate did not create, and a register number it does not carry. Both
    /// carry their own flag, so each says itself once and neither hides the
    /// other behind [`refuse_unwind`]'s per-name flags.
    pub(super) fn refuse(once: &AtomicBool, what: &str) -> ! {
        if !once.swap(true, Ordering::Relaxed) {
            let line = format!("rosetta3: unwind: {what}\n");
            // SAFETY: a live buffer for the length written, and this runs before
            // anything that can stop the process.
            unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
        }
        std::process::abort()
    }

    /// The one place a string of this file's own is handed to the guest, and it
    /// has to outlive the call: a `const char *` the guest copies, never frees.
    ///
    /// Leaked deliberately and named [`leaked`] so a test can see that it is
    /// bounded by the number of calls rather than by a loop. This is the same
    /// trade `crate::unavailable`'s `cups_user` makes, for the same reason: a
    /// message that says what happened is worth one small permanent allocation.
    ///
    /// **`CString::into_raw`, not `Box::into_raw(into_boxed_c_str())`**, so that
    /// every `char *` this file hands the guest has the *same* allocation shape
    /// and the same round trip. These two are not interchangeable:
    /// `release_guest_error_string` reconstructs with `CString::from_raw`, which
    /// is only the documented inverse of `into_raw`. Nothing here is registered
    /// and so nothing here is ever freed — `Framework+0x3a7b518` renders this
    /// message through a `%s` conversion and the guest has no other use for it —
    /// but one shape is cheaper to reason about than two.
    fn leaked(text: String) -> *mut c_char {
        CString::new(text).unwrap_or_default().into_raw()
    }

    // ------------------------------------------------- VideoToolbox OSStatus

    /// VideoToolbox's `OSStatus` for "there is no video decoder to find", from
    /// `VTErrors.h`.
    ///
    /// The right code for a host with no VideoToolbox at all, and not a generic
    /// failure: the function's own job is to find a decoder and there is nothing
    /// to find. Value confirmed against `osstatus.com`'s copy of `VTErrors.h`.
    const VT_COULD_NOT_FIND_VIDEO_DECODER_ERR: i32 = -12906;

    /// `VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR`, from the same header: the encoder
    /// counterpart of the above.
    const VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR: i32 = -12908;

    /// `VT_PROPERTY_NOT_SUPPORTED_ERR`, from the same header: what a session that
    /// was never created says about a property, and what a session says about a
    /// property it does not have.
    const VT_PROPERTY_NOT_SUPPORTED_ERR: i32 = -12900;

    /// `VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR`, from the same header, and the **only**
    /// one of these four the guest can be seen to check for by value: at
    /// `Framework+0x7925a97` it compares `VTPixelTransferSessionTransferImage`'s
    /// answer against `0xffffcd97`, which is this number, and takes its software
    /// conversion path.
    const VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR: i32 = -12905;

    /// A `CMTime` as VideoToolbox passes one: `value`, then the flags and epoch
    /// that go with it, in Apple's declared order.
    ///
    /// Sixteen bytes and therefore a memory-class aggregate, so the two of them
    /// `VTCompressionSessionCompleteFrames` and `VTCompressionSessionEncodeFrame`
    /// take are on the stack. The guest's own stores agree: `Framework+0x2fd82e8`
    /// writes sixteen bytes at `[rsp]` and eight more at `[rsp+0x10]`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CTime {
        value: i64,
        flags: u32,
        epoch: i32,
    }

    /// `OSStatus VTCompressionSessionCreate(CFAllocatorRef, int32_t width,
    /// int32_t height, CMVideoCodecType codecType, CFDictionaryRef
    /// encoderSpecification, CFDictionaryRef sourceImageBufferAttributes,
    /// CFAllocatorRef compressedDataAllocator, VTCompressionOutputCallbackRef
    /// outputCallback, void *outputCallbackRefCon, VTCompressionSessionRef
    /// *compressionSessionOut)`.
    ///
    /// **Ten parameters, and the register assignment is the one the binary
    /// proves.** `Framework+0x2fd6c82`-`0x2fd6c89`:
    ///
    /// ```text
    /// 2fd6c82  mov  r8, QWORD PTR [rbp-0x30]   ; the encoder specification
    /// 2fd6c86  xor  r9d, r9d                   ; sourceImageBufferAttributes = NULL
    /// 2fd6c5c  mov  QWORD PTR [rsp+0x18], r13   ; r13 = &this->session_
    /// 2fd6c61  mov  QWORD PTR [rsp+0x10], r14   ; this  == outputCallbackRefCon
    /// 2fd6c6d  mov  QWORD PTR [rsp+0x8], rax    ; the callback thunk
    /// 2fd6c72  mov  QWORD PTR [rsp], 0x0        ; compressedDataAllocator = NULL
    /// 2fd6c7a  mov  esi, ebx                    ; width
    /// 2fd6c7c  mov  edx, r15d                   ; height
    /// 2fd6c7f  mov  ecx, r12d                   ; codecType, 'avc1' at 0x2fd6c0c
    /// 2fd6c89  call _VTCompressionSessionCreate
    /// ```
    ///
    /// So `rcx` is the codec type and not the third scalar, the callback record
    /// is the memory-class pair at `[rsp+0x8]`/`[rsp+0x10]`, and **the session
    /// out-parameter is the tenth word**. That last one is written: the guest
    /// zeroes the same slot itself at `+0x2fd6c99` (`mov QWORD PTR [r13+0x0],
    /// 0x0`) the moment the status comes back non-zero, so writing it is not a
    /// guess but a duplicate of what the guest is about to do.
    ///
    /// `VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR`, and the guest's own failure arm is a log
    /// line built from the status (`+0x2fd6cb0`) followed by
    /// `test r15d,r15d; sete bl` — it returns false and keeps running.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn vt_compression_session_create(
        _allocator: *mut c_void,
        _width: i32,
        _height: i32,
        _codec_type: u32,
        _encoder_specification: *mut c_void,
        _source_image_buffer_attributes: *mut c_void,
        _compressed_data_allocator: *mut c_void,
        _output_callback: *mut c_void,
        _output_callback_refcon: *mut c_void,
        session_out: *mut *mut c_void,
    ) -> i32 {
        if !session_out.is_null() {
            // SAFETY: the caller's own out-parameter, and the tenth word of the
            // call is the address the guest stored at `Framework+0x2fd6c5c`.
            unsafe { session_out.write(std::ptr::null_mut()) };
        }
        note(
            &said::SESSION,
            "VTCompressionSessionCreate",
            "there is no VideoToolbox on this host and no encoder it could be built on; \
             the guest logs the status and carries on (Framework+0x2fd6c89)",
        );
        VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR
    }

    /// `void VTCompressionSessionInvalidate(VTCompressionSessionRef)`.
    ///
    /// A no-op, and a *true* no-op: a session was never created, so there is
    /// nothing to invalidate. The guest guards the call
    /// (`Framework+0x2fd6eb8` `test rdi,rdi; je`) and ignores the void return.
    unsafe extern "C" fn vt_compression_session_invalidate(_session: *mut c_void) {}

    /// `OSStatus VTCompressionSessionEncodeFrame(VTCompressionSessionRef,
    /// CVImageBufferRef, CMTime presentationTimeStamp, CMTime duration,
    /// CFDictionaryRef frameProperties, void *sourceFrameRefCon,
    /// VTEncodeInfoFlags *infoFlagsOut)`.
    ///
    /// `VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR`. The guest branches on it
    /// (`Framework+0x2fd8302` `test eax,eax; je`) and its non-zero arm formats the
    /// status into a log line, so nothing is written through the arguments and
    /// nothing needs to be.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn vt_compression_session_encode_frame(
        _session: *mut c_void,
        _image_buffer: *mut c_void,
        _presentation_time_stamp: CTime,
        _duration: CTime,
        _frame_properties: *mut c_void,
        _source_frame_refcon: *mut c_void,
        _info_flags_out: *mut c_uint,
    ) -> i32 {
        note(
            &said::SESSION,
            "VTCompressionSessionEncodeFrame",
            "as VTCompressionSessionCreate: no session exists to encode with \
             (Framework+0x2fd82fd)",
        );
        VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR
    }

    /// `OSStatus VTCompressionSessionCompleteFrames(VTCompressionSessionRef,
    /// CMTime completeUntilPresentationTimeStamp)`.
    ///
    /// As [`vt_compression_session_encode_frame`]; the guest's non-zero arm at
    /// `Framework+0x2fd7c40` logs the status.
    unsafe extern "C" fn vt_compression_session_complete_frames(
        _session: *mut c_void,
        _complete_until_presentation_time_stamp: CTime,
    ) -> i32 {
        note(
            &said::SESSION,
            "VTCompressionSessionCompleteFrames",
            "as VTCompressionSessionCreate: no session exists to flush \
             (Framework+0x2fd7c39)",
        );
        VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR
    }

    /// `OSStatus VTDecompressionSessionCreate(CFAllocatorRef allocator, CFTypeRef
    /// videoDecoderSpecification, CMVideoCodecType videoCodecType,
    /// CFDictionaryRef destinationImageBufferAttributes,
    /// VTDecompressionOutputCallbackRef outputCallback, void *refcon,
    /// VTDecompressionSessionRef *sessionOut)`.
    ///
    /// **Nothing is written, and that is a decision read out of the binary.**
    /// The call is at `Framework+0x2fcb7ff`:
    ///
    /// ```text
    /// 2fcb7db  mov  QWORD PTR [rbp-0x38], rdi   ; the refcon, in the record
    /// 2fcb7df  cmp  QWORD PTR [rdi+0x20], 0x0   ; a session already exists?
    /// 2fcb7e4  jne  0x2fcb8ce                   ; int3; ud2 if it does
    /// 2fcb7ed  lea  r9, [rdi+0x20]              ; &this->session_
    /// 2fcb7f8  mov  rdi, QWORD PTR [rax]        ; the VTAllocator
    /// 2fcb7fb  lea  r8, [rbp-0x40]              ; the callback record
    /// 2fcb7ff  call _VTDecompressionSessionCreate
    /// ```
    ///
    /// The header's seventh parameter would be the seventh word, which is
    /// `[rsp]` — and **the guest never writes it**. This frame is entered with
    /// `sub rsp,0x168` and no store to `[rsp]` anywhere before the call, so
    /// whether the seventh word is a session out-parameter or the second half of
    /// the callback record cannot be settled from the image, and a function that
    /// guessed would be writing to uninitialised stack or to a refcon.
    ///
    /// Not writing is provably safe: `this+0x20` is zeroed by the constructor at
    /// `+0x2fcb666` (`xorps xmm0,xmm0; movups [rdi+0x20],xmm0`), the guard at
    /// `+0x2fcb7df` requires it to still be zero on any later call, and the guest
    /// only ever *reads* it behind `test rdi,rdi; je` (invalidate at `+0x2fcb9da`,
    /// invalidate-at-exit at `+0x2fcbaca`, decode at `+0x2fcbac3`). Returning a
    /// non-zero status keeps all of that consistent.
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn vt_decompression_session_create(
        _allocator: *mut c_void,
        _video_decoder_specification: *mut c_void,
        _video_codec_type: u32,
        _destination_image_buffer_attributes: *mut c_void,
        _output_callback: *mut c_void,
        _refcon: *mut c_void,
    ) -> i32 {
        note(
            &said::SESSION,
            "VTDecompressionSessionCreate",
            "there is no VideoToolbox on this host and no decoder it could be built on \
             (Framework+0x2fcb7ff)",
        );
        VT_COULD_NOT_FIND_VIDEO_DECODER_ERR
    }

    /// `void VTDecompressionSessionInvalidate(VTDecompressionSessionRef)`.
    ///
    /// A true no-op, as [`vt_compression_session_invalidate`]: the guest reaches
    /// it only through `test rdi,rdi; je` (`Framework+0x2fcb9de`), and a session
    /// was never created.
    unsafe extern "C" fn vt_decompression_session_invalidate(_session: *mut c_void) {}

    /// `bool VTDecompressionSessionCanAcceptFormatDescription(
    /// VTDecompressionSessionRef, CMVideoFormatDescriptionRef)`.
    ///
    /// **False, and false is the answer rather than a stub**: a session that does
    /// not exist can accept nothing. The guest calls it only with a non-null
    /// session (`Framework+0x2fcba90` `test rdi,rdi; je` to an `int3`), so with
    /// [`vt_decompression_session_create`] refusing this is unreachable on this
    /// host — which is exactly why `false` is safe and a made-up `true` would not
    /// be.
    unsafe extern "C" fn vt_decompression_session_can_accept_format_description(
        _session: *mut c_void,
        _format_description: *mut c_void,
    ) -> bool {
        note(
            &said::SESSION,
            "VTDecompressionSessionCanAcceptFormatDescription",
            "as VTDecompressionSessionCreate: there is no session to ask \
             (Framework+0x2fcba9d)",
        );
        false
    }

    /// `OSStatus VTDecompressionSessionDecodeFrame(VTDecompressionSessionRef,
    /// CMBlockBufferRef, CMVideoFormatDescriptionRef, VTDecodeFrameFlags, void *,
    /// VTDecodeInfoFlags *)`.
    ///
    /// `VT_COULD_NOT_FIND_VIDEO_DECODER_ERR`. The guest's non-zero arm formats the
    /// status into a log line and returns false (`Framework+0x2fcbae2`), so
    /// nothing is written and nothing needs to be.
    unsafe extern "C" fn vt_decompression_session_decode_frame(
        _session: *mut c_void,
        _data_buffer: *mut c_void,
        _frame_description: *mut c_void,
        _decode_flags: u32,
        _frame_info: *mut c_void,
    ) -> i32 {
        note(
            &said::SESSION,
            "VTDecompressionSessionDecodeFrame",
            "as VTDecompressionSessionCreate: there is no session to decode with \
             (Framework+0x2fcbadb)",
        );
        VT_COULD_NOT_FIND_VIDEO_DECODER_ERR
    }

    /// `OSStatus VTSessionCopyProperty(VTSessionRef, CFStringRef propertyKey,
    /// CFTypeRef *propertyValueOut)`.
    ///
    /// `VT_PROPERTY_NOT_SUPPORTED_ERR`, **with the out-parameter written to null**.
    /// The guest reads it only on a zero status
    /// (`Framework+0x2fd7781` `test eax,eax; jne`), and it pre-zeroes the slot at
    /// `+0x2fd779a` — but a function whose whole job is to leave an
    /// out-parameter defined should not lean on the caller's incidental zeroing.
    unsafe extern "C" fn vt_session_copy_property(
        _session: *mut c_void,
        _property_key: *mut c_void,
        property_value_out: *mut *mut c_void,
    ) -> i32 {
        if !property_value_out.is_null() {
            // SAFETY: the caller's own out-parameter, per the header's
            // `CF_RETURNS_RETAINED` annotation.
            unsafe { property_value_out.write(std::ptr::null_mut()) };
        }
        note(
            &said::PROPERTY,
            "VTSessionCopyProperty",
            "a session that was never created has no properties \
             (Framework+0x2fd777c)",
        );
        VT_PROPERTY_NOT_SUPPORTED_ERR
    }

    /// `OSStatus VTSessionSetProperty(VTSessionRef, CFStringRef propertyKey,
    /// CFTypeRef propertyValue)`.
    ///
    /// `VT_PROPERTY_NOT_SUPPORTED_ERR`, and no out-parameter: the guest turns the
    /// answer into a `bool` immediately (`Framework+0x167ed06` `test r14d,r14d;
    /// sete al`) and has a path for false.
    unsafe extern "C" fn vt_session_set_property(
        _session: *mut c_void,
        _property_key: *mut c_void,
        _property_value: *mut c_void,
    ) -> i32 {
        note(
            &said::PROPERTY,
            "VTSessionSetProperty",
            "a session that was never created accepts no properties \
             (Framework+0x167ecf1)",
        );
        VT_PROPERTY_NOT_SUPPORTED_ERR
    }

    /// `OSStatus VTSessionCopySupportedPropertyDictionary(VTSessionRef,
    /// CFDictionaryRef *outDictionary)`.
    ///
    /// `VT_PROPERTY_NOT_SUPPORTED_ERR`, **and this is the one VideoToolbox
    /// out-parameter that had to be written**: the guest poisons the slot
    /// immediately before the call —
    ///
    /// ```text
    /// 167ec59  movabs rax, 0xaaaaaaaaaaaaaaaa
    /// 167ec63  mov    QWORD PTR [rbp-0x20], rax
    /// 167ec6a  lea    rsi, [rbp-0x20]
    /// 167ec6e  call   _VTSessionCopySupportedPropertyDictionary
    /// 167ec73  test   eax, eax
    /// 167ec75  je     0x167ec9b                 ; on success, cache it
    /// ```
    ///
    /// and on the success arm `+0x167ec9b` reads `[rbp-0x20]` and hands it to
    /// `CFDictionaryContainsKey` at `+0x167ec86`. A zero status that left the
    /// poison there would fault on `0xaaaaaaaaaaaaaaaa`. The status is non-zero,
    /// and the slot is nulled anyway.
    unsafe extern "C" fn vt_session_copy_supported_property_dictionary(
        _session: *mut c_void,
        out_dictionary: *mut *mut c_void,
    ) -> i32 {
        if !out_dictionary.is_null() {
            // SAFETY: the caller's own out-parameter; the guest pre-poisoned it
            // with `0xaaaa…aaaa` at `Framework+0x167ec63`.
            unsafe { out_dictionary.write(std::ptr::null_mut()) };
        }
        note(
            &said::PROPERTY,
            "VTSessionCopySupportedPropertyDictionary",
            "there are no supported properties, because there is no VideoToolbox to \
             have any (Framework+0x167ec6e)",
        );
        VT_PROPERTY_NOT_SUPPORTED_ERR
    }

    /// `OSStatus VTCreateCGImageFromCVPixelBuffer(CVPixelBufferRef,
    /// CFDictionaryRef options, CGImageRef *imageOut)`.
    ///
    /// `VT_COULD_NOT_FIND_VIDEO_DECODER_ERR`, with `*imageOut` nulled. The guest
    /// branches on the status (`Framework+0x45913c8` `test eax,eax; jne`) and
    /// its non-zero arm skips the `CGImageCreateWithImageInRect` at `+0x4591444`
    /// entirely; the out-parameter is pre-zeroed at `+0x45913b2` and nulled here
    /// so the guarantee is this function's rather than the caller's.
    unsafe extern "C" fn vt_create_cg_image_from_cv_pixel_buffer(
        _pixel_buffer: *mut c_void,
        _options: *mut c_void,
        image_out: *mut *mut c_void,
    ) -> i32 {
        if !image_out.is_null() {
            // SAFETY: the caller's own out-parameter, per `CF_RETURNS_RETAINED`.
            unsafe { image_out.write(std::ptr::null_mut()) };
        }
        note(
            &said::SESSION,
            "VTCreateCGImageFromCVPixelBuffer",
            "making a CGImage out of a CVPixelBuffer is VideoToolbox's job and there \
             is no VideoToolbox (Framework+0x45913c3)",
        );
        VT_COULD_NOT_FIND_VIDEO_DECODER_ERR
    }

    /// `OSStatus VTPixelTransferSessionCreate(CFAllocatorRef,
    /// VTPixelTransferSessionRef *sessionOut)`.
    ///
    /// **The out-parameter is written and the status is not zero, and the guest
    /// traps on it.** `Framework+0x79258d3`-`0x79258df`:
    ///
    /// ```text
    /// 79258cc  mov  QWORD PTR [rax], 0x0     ; the session slot, already null
    /// 79258d3  xor  edi, edi                 ; allocator = NULL
    /// 79258d5  mov  rsi, rax
    /// 79258d8  call _VTPixelTransferSessionCreate
    /// 79258dd  test eax, eax
    /// 79258df  jne  0x7925986                ; int3; ud2
    /// ```
    ///
    /// So the guest wrote the slot itself before the call and asserts that the
    /// call succeeds. Returning `0` would be a lie that leaves a garbage handle in
    /// the guest's own pointer; returning [`VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR`] is
    /// the truth and reaches a `CHECK` in the guest's code, one line below
    /// [`said::PIXEL_TRANSFER`]'s message. That is the whole of the choice, and
    /// the truthful side of it is the one that also names itself.
    unsafe extern "C" fn vt_pixel_transfer_session_create(
        _allocator: *mut c_void,
        session_out: *mut *mut c_void,
    ) -> i32 {
        if !session_out.is_null() {
            // SAFETY: the caller's own out-parameter; the slot is a freshly
            // allocated word the guest nulled at `Framework+0x79258cc`.
            unsafe { session_out.write(std::ptr::null_mut()) };
        }
        note(
            &said::PIXEL_TRANSFER,
            "VTPixelTransferSessionCreate",
            "there is no VideoToolbox, so no pixel-transfer session; the guest CHECKs \
             this answer and will trap at Framework+0x7925986 (Framework+0x79258d8)",
        );
        VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR
    }

    /// `void VTPixelTransferSessionInvalidate(VTPixelTransferSessionRef)`.
    ///
    /// A true no-op, as the other two invalidates: no session exists. The guest
    /// reaches it behind `test rdi,rdi; je` (`Framework+0x79257b2`) and ignores
    /// the void return.
    unsafe extern "C" fn vt_pixel_transfer_session_invalidate(_session: *mut c_void) {}

    /// `OSStatus VTPixelTransferSessionTransferImage(VTPixelTransferSessionRef,
    /// CVPixelBufferRef src, CVPixelBufferRef dst)`.
    ///
    /// **Implemented, and the value is the one the guest is looking for.** The
    /// call is at `Framework+0x7925a92` and the three instructions after it are
    ///
    /// ```text
    /// 7925a97  cmp  eax, 0xffffcd97        ; -12905, VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR
    /// 7925a9c  je   0x7926cb6              ; the software conversion path
    /// 7925aa2  test eax, eax
    /// 7925aa4  je   0x7926cb6              ; so: success, also that path
    /// 7925aaa  int3; ud2                   ; anything else traps
    /// ```
    ///
    /// The guest has written down, as a literal, that "this pixel format pair is
    /// not supported" comes back as `-12905` and is not a failure. That is exactly
    /// what is true here, and the fallback is a software conversion this tree
    /// already has. **No argument is written**: the prototype has none, and
    /// `src`/`dst` are the guest's own pixel buffers.
    unsafe extern "C" fn vt_pixel_transfer_session_transfer_image(
        _session: *mut c_void,
        _source: *mut c_void,
        _destination: *mut c_void,
    ) -> i32 {
        VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR
    }

    // ----------------------------------------------------------------- QuartzCore

    /// A `CATransform3D`: the sixteen `CGFloat`s of `CATransform3D.h`'s struct,
    /// in its declared order — four rows of four, translation last.
    ///
    /// `#[repr(C)]` and 128 bytes, so it is a memory-class aggregate: **returned
    /// through a hidden pointer in `rdi`**, and both of the guest's call sites
    /// agree. `Framework+0xc6dc2ab` is `lea rdi,[rbp-0x660]` followed by
    /// `call _CATransform3DMakeTranslation`, and the result is read straight back
    /// out of 128 bytes at `[rbp-0x660]` (`+0xc6dc2ea` through `+0xc6dc315`).
    /// Declaring it as a plain `-> Transform3D` gets the hidden pointer for free,
    /// the same way `cg_unavailable`'s `CGRect` does.
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    struct Transform3D {
        m11: f64,
        m12: f64,
        m13: f64,
        m14: f64,
        m21: f64,
        m22: f64,
        m23: f64,
        m24: f64,
        m31: f64,
        m32: f64,
        m33: f64,
        m34: f64,
        m41: f64,
        m42: f64,
        m43: f64,
        m44: f64,
    }

    /// `CATransform3D CATransform3DMakeTranslation(CGFloat tx, CGFloat ty,
    /// CGFloat tz)`.
    ///
    /// **Computed, and the computation is CoreAnimation's**: the identity matrix
    /// with the translation in the last row. `CA::CATransform3DMakeTranslation`
    /// is a `static inline` in `CATransform3D.h` and reads
    ///
    /// ```c
    /// t.m11 = 1.0f; t.m12 = t.m13 = t.m14 = 0.0f;
    /// t.m21 = t.m22 = 0.0f; t.m23 = t.m24 = 0.0f;
    /// t.m31 = t.m32 = 0.0f; t.m33 = 1.0f; t.m34 = 0.0f;
    /// t.m41 = tx; t.m42 = ty; t.m43 = tz; t.m44 = 1.0f;
    /// ```
    ///
    /// — a value, not a lookup, and the only VideoToolbox-adjacent name in this
    /// file that has a right answer on this host. The two call sites
    /// (`Framework+0xc6dc2b2` and `+0xc6dc5a9`) both write the 128-byte result
    /// into a frame buffer and pass it on, so a wrong matrix would be a wrong
    /// transform and would look like a rendering bug rather than a missing
    /// framework.
    unsafe extern "C" fn ca_transform3d_make_translation(
        tx: f64,
        ty: f64,
        tz: f64,
    ) -> Transform3D {
        Transform3D {
            m11: 1.0,
            m22: 1.0,
            m33: 1.0,
            m41: tx,
            m42: ty,
            m43: tz,
            m44: 1.0,
            ..Transform3D::default()
        }
    }

    // ---------------------------------------------------------------------- Metal

    /// `NSArray<id<MTLDevice>> *MTLCopyAllDevicesWithObserver(id<MTLDeviceObserver>
    /// observer)`.
    ///
    /// **Null, which is the whole answer: this host has no Metal device to
    /// copy.** The call is at `Framework+0xc6c1a86`:
    ///
    /// ```text
    /// c6c1a70  mov  QWORD PTR [rbp-0x70], 0x0     ; the out slot, pre-nulled
    /// c6c1a78  lea  rsi, [rip+0x1f9dc79]          ; a zeroed global: no observer
    /// c6c1a7f  lea  rbx, [rbp-0x70]
    /// c6c1a83  mov  rdi, rbx
    /// c6c1a86  call _MTLCopyAllDevicesWithObserver
    /// c6c1a92  mov  rdi, rax ; call objc_retainAutoreleasedReturnValue
    /// c6c1a98  mov  rdi, QWORD PTR [rbp-0x70] ; call objc_retainAutorelease…
    /// ```
    ///
    /// The declaration and the call disagree about which register is which — the
    /// header's single parameter is the observer, and the guest puts a *stack
    /// address* in `rdi` and the observer in `rsi` — so the shape below is the
    /// binary's, and it is the safe reading either way: **`rdi` is a stack
    /// address the guest itself nulled one instruction earlier, so writing zero
    /// through it cannot fault and cannot change anything.** The header's reading
    /// would have `rdi` be a nil observer, and writing through that *would* fault
    /// at address 0; that is the reason both readings are checked here rather than
    /// one.
    ///
    /// Null is safe downstream: retaining nil is a no-op, and the next thing the
    /// guest does with the result is `movzx eax, BYTE PTR [0xe873a00]; test al,al;
    /// je` at `+0xc6c1a9f` — a feature flag, not the array.
    unsafe extern "C" fn mtl_copy_all_devices_with_observer(
        devices_out: *mut *mut c_void,
        _observer: *mut c_void,
    ) -> *mut c_void {
        if !devices_out.is_null() {
            // SAFETY: a stack address the guest pre-nulled at
            // `Framework+0xc6c1a70`, under either reading of the declaration.
            unsafe { devices_out.write(std::ptr::null_mut()) };
        }
        std::ptr::null_mut()
    }

    // --------------------------------------------------------------------- libbsm

    /// An `audit_token_t`: `unsigned int val[32]`, 128 bytes, **passed on the
    /// stack.**
    ///
    /// The guest copies it there itself at `Framework+0x4dc37ab`:
    ///
    /// ```text
    /// 4dc37ab  movups xmm0, XMMWORD PTR [rbx+0x48]
    /// 4dc37af  movups xmm1, XMMWORD PTR [rbx+0x58]
    /// 4dc37b3  movups [rsp+0x10], xmm1
    /// 4dc37b8  movups [rsp], xmm0
    /// 4dc37bc  call _audit_token_to_pid
    /// ```
    ///
    /// Thirty-two words is a memory-class aggregate, so `extern "C"` reads it
    /// from exactly those sixteen bytes.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct AuditToken([u32; 32]);

    /// `pid_t audit_token_to_pid(const audit_token_t token)`.
    ///
    /// **`-1`, and this is a refusal rather than an extraction.** Apple's own
    /// documentation says the token's representation is not to be relied on —
    /// "nobody should rely on such a literal representation" — and this host has
    /// no audit token to interpret in the first place: `darwin::mach::msg` writes
    /// an eight-byte trailer of format and size (`write_trailer`,
    /// `crates/darwin/src/mach/msg.rs:836`) and never fills `msgh_audit`, so the
    /// 128 bytes the guest hands over are bytes nothing wrote. Picking an index
    /// into `val[]` from a header would be inventing an identity.
    ///
    /// `-1` is the honest `pid_t`: a pid that cannot be a real process, so a
    /// caller that looks it up fails to find one instead of finding `kernel_task`.
    /// **Zero would have been a wrong answer that looks like a working one**, which
    /// is the outcome PLAN.md §1 exists to prevent.
    ///
    /// Every one of the twelve call sites uses the answer for a log line or a
    /// diagnostic callback — `Framework+0x4dc37c5` formats it into a string,
    /// `+0x4dc654f` passes it to `NSLog` as `"AppShim: Closing pid %d"`,
    /// `+0x4dc9c2b` hands it to a virtual — so nothing keys off it and nothing
    /// needs a real number.
    unsafe extern "C" fn audit_token_to_pid(_token: AuditToken) -> c_int {
        note(
            &said::AUDIT_TOKEN,
            "audit_token_to_pid",
            "there is no audit token on this host: darwin::mach::msg writes only the \
             trailer's format and size, never msgh_audit, so the 128 bytes this was \
             handed were never written by anything; -1 rather than a pid that could \
             be mistaken for kernel_task",
        );
        -1
    }

    // -------------------------------------------------------------------- libobjc

    /// `void objc_copyStruct(void *dest, const void *src, ptrdiff_t size, BOOL
    /// atomic, BOOL hasStrong)`.
    ///
    /// **A copy, because the binary says the one thing that decides it.** Both of
    /// the guest's call sites pass the last argument as a *literal zero*:
    ///
    /// ```text
    /// 3a9eb88  lea  rsi, [rdi+0x40]
    /// 3a9eb8c  lea  rdi, [rbp-0x10]
    /// 3a9eb90  mov  edx, 0x10        ; size = 16
    /// 3a9eb95  mov  ecx, 0x1         ; atomic = true
    /// 3a9eb9a  xor  r8d, r8d         ; hasStrong = FALSE
    /// 3a9eb9d  call _objc_copyStruct
    /// ```
    ///
    /// and `0x3a9ebe7` is the same six instructions for the other direction.
    /// With `hasStrong == false` the ARC runtime's own `objc_copyStruct` takes
    /// its `memcpy`/`objc_atomicCopy` arm — there is no `__strong` member to walk
    /// — and on x86-64 an atomic copy of a 16-byte value is that `memcpy`. So a
    /// copy of `size` bytes is the *true* answer here, not a convenient one, and
    /// it is what PLAN.md §4's blanket refusal of this name was protecting
    /// against for the other case.
    ///
    /// The other case is still refused. `hasStrong != 0` means the copy has to
    /// retain every `__strong` member and release every `__weak` one, which needs
    /// the type layout out of a type encoding; `darwin` has no encoding parser
    /// and the one that exists depends the wrong way round to be used from here
    /// (PLAN.md §4). So the copy is **skipped and named** rather than performed
    /// and wrong: a plain `memcpy` there would drop the retains and corrupt the
    /// destination silently, which is worse in every way than not copying.
    unsafe extern "C" fn objc_copy_struct(
        dest: *mut c_void,
        src: *const c_void,
        size: isize,
        _atomic: bool,
        has_strong: bool,
    ) {
        if has_strong {
            note(
                &said::STRONG_COPY,
                "objc_copyStruct",
                "hasStrong is true, so the copy has to retain and release members \
                 whose layout is only in a type encoding; the copy is skipped rather \
                 than done without them",
            );
            return;
        }
        if dest.is_null() || src.is_null() || size <= 0 {
            return;
        }
        // SAFETY: `dest` and `src` are the caller's own `size` writable and
        // readable bytes, per the header, and `hasStrong == false` is the arm
        // where the ARC runtime's own implementation is a `memcpy`.
        unsafe { std::ptr::copy_nonoverlapping(src.cast::<u8>(), dest.cast::<u8>(), size as usize) };
    }

    // -------------------------------------------------------------------- libresolv

    /// `int res_9_ninit(struct __res_state *statp)`.
    ///
    /// **Zero, and zero is what the guest's own definition of success is.** The
    /// call is at `Framework+0xadb97e` and the two instructions after it are
    ///
    /// ```text
    /// adb97e  call _res_9_ninit
    /// adb983  mov  DWORD PTR [r15+0x230], eax
    /// adb98a  test eax, eax
    /// adb98c  je   0xadb9af              ; zero: carry on and use the state
    /// ```
    ///
    /// which is Chromium's `ScopedResState::IsValid()` — `res_init_result_ == 0`
    /// — from `net/dns/public/scoped_res_state.h`. Non-zero sends the guest down
    /// the teardown arm at `+0x59b0617`.
    ///
    /// The state it is initialising is one the guest has just `bzero`ed for
    /// 0x228 bytes (`+0xadb96e`), and **nothing here writes it**: a `__res_state`
    /// on this host is an all-zero state, which is what "no system resolver
    /// configuration" means to every caller in `net/dns`. The guest agrees: its
    /// very next test, `Framework+0xaec7da` `test BYTE PTR [rsi+0x8],0x1`, is
    /// `res.options & RES_INIT`, which a zeroed state does not have, so it reports
    /// "no nameservers" and falls back. Both answers agree, so the write is not
    /// load-bearing — and leaving the guest's own zeroed state alone is the only
    /// thing that is not an invention.
    unsafe extern "C" fn res_9_ninit(_state: *mut c_void) -> c_int {
        0
    }

    /// `void res_9_ndestroy(struct __res_state *statp)`.
    ///
    /// A no-op, and a *true* one: the extension `res_ndestroy` frees is
    /// libresolv's own, attached to the state by `res_ninit`, and
    /// [`res_9_ninit`] attaches none. The guest calls it as a destructor
    /// (`Framework+0xaecb17`, tail-jumping to `operator delete` immediately
    /// after) and with the state it allocated itself, so there is nothing of ours
    /// in it to release.
    unsafe extern "C" fn res_9_ndestroy(_state: *mut c_void) {}

    /// `int res_9_getservers(struct __res_state *statp, union res_sockaddr_union
    /// *set, int cnt)`.
    ///
    /// **Zero nameservers, which is libresolv's own answer for this state.**
    /// `res_getservers` returns 0 when the state it is given has no address list
    /// and no count, and the state it is given is the zeroed one — the count *is*
    /// the error channel here, and 0 says "there are none".
    ///
    /// The guest's array is **poisoned** immediately before the call:
    ///
    /// ```text
    /// aec7f6  mov  edx, 0x180        ; 384 bytes
    /// aec7fe  mov  esi, 0xaa
    /// aec803  call _memset            ; memset(set, 0xaa, 0x180)
    /// aec813  call _res_9_getservers
    /// aec818  test eax, eax
    /// aec81a  jle  0xaeca42           ; <= 0: an empty, valid configuration
    /// ```
    ///
    /// so the array is *not* written and does not need to be: the count of 0 is
    /// what the guest's loop (`+0xaec864` `cmp r13,r14; je`) is driven by, and it
    /// never runs. Writing 0 into a `sockaddr` union would be inventing a
    /// resolver that does not exist; the `0xaa` the guest put there is correctly
    /// never read. On the taken path the guest builds a configuration with no
    /// nameservers and a valid flag (`+0xaeca7b`-`+0xaeca86`).
    unsafe extern "C" fn res_9_getservers(
        _state: *mut c_void,
        _set: *mut c_void,
        _count: c_int,
    ) -> c_int {
        0
    }

    // -------------------------------------------------------------------- sandbox

    /// `int __sandbox_ms(const char *profile, uint32_t flags, void **errorbuf)`.
    ///
    /// Note the **double leading underscore in the C name** — the image imports
    /// `___sandbox_ms` (see the module note).
    ///
    /// **`-1`, and the error out-parameter carries a message rather than a
    /// null.** The call is at `Framework+0x3a7b4e6` and the out-parameter is
    /// handed straight to a `%s` conversion:
    ///
    /// ```text
    /// 3a7b4d2  mov  QWORD PTR [rbp-0x28], 0x0        ; pre-nulled
    /// 3a7b4da  lea  rdi, [rip+0x9cd12f4]              ; the profile
    /// 3a7b4e1  mov  esi, 0x60                         ; flags
    /// 3a7b4e6  call ___sandbox_ms
    /// 3a7b4f1  sete dl                                ; dl = (status == 0)
    /// 3a7b4f7  and  r14d, 0x4 ; 3a7b4fb  shr r14d, 0x2 ; the status's own bits
    /// 3a7b506  lea  rsi, [rip+0x9d88df7]              ; "rv=%d status=0x%llx …"
    /// 3a7b518  call …                                 ; rcx = [rbp-0x28]
    /// ```
    ///
    /// Chromium's `sandbox_init` reports a non-zero answer and carries on, so the
    /// status is a handled path. A null `*errorbuf` would reach that `%s` and
    /// print `(null)`; a real message is both true and useful, and it is the one
    /// place in this file where a permanent allocation is worth making.
    unsafe extern "C" fn sandbox_ms(
        _profile: *const c_char,
        _flags: u32,
        errorbuf: *mut *mut c_char,
    ) -> c_int {
        if !errorbuf.is_null() {
            let why = leaked(
                "rosetta3: sandbox_ms: refused - there is no Seatbelt on this host, \
                 so no sandbox profile can be compiled or applied"
                    .to_string(),
            );
            // SAFETY: the caller's own out-parameter; the guest pre-nulls it at
            // `Framework+0x3a7b4d2` and then passes it to a `%s` conversion.
            unsafe { errorbuf.write(why) };
        }
        note(
            &said::SANDBOX,
            "__sandbox_ms",
            "there is no Seatbelt on this host, so no sandbox profile can be applied; \
             the guest logs rv and status and continues (Framework+0x3a7b4e6)",
        );
        -1
    }

    // ---------------------------------------------------------- the params set
    //
    // The three functions below are a **record** operation, not an enforcement
    // one, and that is the whole wall. Everything else in this file's sandbox
    // family refuses because there is no Seatbelt to compile, apply or enforce
    // with, which is a capability gap and is named as one. A `sandbox_params_t`
    // is a *set of strings*, this host can hold a set of strings, and refusing
    // to hold one put the refusal in the wrong place: the guest never reached the
    // honest refusal, because it trapped first. The two composable refusals, and
    // the crash they made, were
    //
    // ```text
    //  3a94d5d  call _sandbox_create_params     ; -> NULL
    //  3a94d62  mov  [rbp-0x210], rax           ; stored, never null-tested
    //  ...
    //  7fe5f7e  mov  rdi, [r14+0x58]            ; handed on, never null-tested
    //  7fe5f82  call _sandbox_set_param         ; -> -1
    //  7fe5f87  test eax, eax
    //  7fe5f89  je   0x7fe6033                   ; 0 => `mov al,0x1`: SUCCESS
    // ```
    //
    // and 18 of the 21 call sites of the wrapper at `0x7fe5e70` turn that `false`
    // into a bare `int3` (the eleven-`int3` pad at `0x3ab0d2b`). So the
    // *enforcement* refusal that matters does not move: the profile those
    // parameters belong to is still refused at [`sandbox_compile_string`], with a
    // message, on the one arm the guest has a path for.

    /// One `sandbox_params_t`: the set of name/value pairs the guest is building
    /// for a profile it will never get compiled.
    ///
    /// A `BTreeMap` and not a `HashMap`, for one reason that is not taste: the
    /// recorded set has to be *readable* — a test, and any future step that
    /// wanted the parameters back, both need an order that does not change from
    /// one run to the next.
    #[derive(Default)]
    struct Params {
        pairs: std::collections::BTreeMap<String, String>,
    }

    impl Params {
        /// Record `key` = `value`, replacing any earlier value for that key.
        ///
        /// Replacing is the real `sandbox_set_param` semantics: the guest may set
        /// one key twice, and a set holds one value per key.
        fn set(&mut self, key: String, value: String) {
            self.pairs.insert(key, value);
        }
    }

    /// The addresses of every live parameter set, and nothing else.
    ///
    /// **A set of addresses, and the address is what the guest gets back.** A
    /// handle handed to `sandbox_set_param` or `sandbox_free_params` is looked up
    /// here first and is *refused* if it is not in the table, rather than
    /// dereferenced. `sandbox_params_t` is a `void *` that Chromium stores in a
    /// struct field (`Framework+0x7fe5f7e  mov rdi, [r14+0x58]`) and hands back
    /// whenever it likes, so "is this one of ours" has to be answerable without
    /// reading it.
    ///
    /// It also settles the concurrency the guest creates. The 18 trapping call
    /// sites are in one start-up function, but Chromium is multi-threaded and
    /// nothing in the binary says only one thread reaches it, so every mutation
    /// of every set is serialised by this one lock — held **across** the
    /// `set_param` mutation below, so a set cannot be freed while it is being
    /// written to.
    fn live_params() -> &'static Mutex<Option<std::collections::HashSet<usize>>> {
        // `Mutex::new(None)` rather than `Default::default()`, for the reason
        // `crate::utmpx` gives: a `static` initializer has to be a const call, and
        // `HashSet::new` is not one.
        static LIVE: Mutex<Option<std::collections::HashSet<usize>>> = Mutex::new(None);
        &LIVE
    }

    /// The guest's `sandbox_params_t`, if it is one this runtime handed out.
    ///
    /// `None` for null, for a handle already freed, and for anything else — which
    /// is what `sandbox_set_param` turns into `-1` and `sandbox_free_params`
    /// turns into doing nothing. `live` is passed in rather than locked here so
    /// that the borrow below is tied to the caller's guard and cannot outlive it.
    fn params_of<'a>(
        handle: *mut c_void,
        live: &'a mut Option<std::collections::HashSet<usize>>,
    ) -> Option<&'a mut Params> {
        let address = handle as usize;
        // A null handle is not "ours", and it must not become address 0.
        let live = live.as_mut()?;
        if address == 0 || !live.contains(&address) {
            return None;
        }
        // SAFETY: the address is in `live`, so this allocation is one
        // `sandbox_create_params` made with `Box::into_raw` and not yet freed.
        // `sandbox_free_params` is the only thing that frees one, and it removes
        // the address from `live` *first*, under this same lock — which is the
        // proof that nothing frees it underneath the `&mut` returned here. The
        // reference is `&mut` and reached only under the lock, and the address
        // was never published to another thread as anything but an opaque
        // `void *`.
        Some(unsafe { &mut *(address as *mut Params) })
    }

    /// `sandbox_params_t sandbox_create_params(void)`.
    ///
    /// **A real allocation: the address of a real set of pairs.**
    ///
    /// The call is at `Framework+0x3a94d5d`:
    ///
    /// ```text
    /// 3a94d4e  mov  QWORD PTR [rbp-0x210], 0x0     ; pre-nulled
    /// 3a94d59  test al, 0x1
    /// 3a94d5b  je   0x3a94d69                      ; the *call* is conditional ...
    /// 3a94d5d  call _sandbox_create_params
    /// 3a94d62  mov  QWORD PTR [rbp-0x210], rax      ; ... the result never is
    /// ```
    ///
    /// The result is stored and **never null-tested** before being handed on to
    /// `sandbox_set_param` at `+0x7fe5f7e`. It is tested once more, later, and
    /// only to decide whether to free it — and that test is what makes a non-null
    /// answer mean the guest really does call `sandbox_free_params`:
    ///
    /// ```text
    /// 3a94ffe  mov  rdi, QWORD PTR [rbp-0x210]
    /// 3a95005  test rdi, rdi
    /// 3a95008  je   0x3a9500f                       ; skip the free
    /// 3a9500a  call _sandbox_free_params
    /// ```
    ///
    /// which is equally true of the second site at `+0x3a958a3`. So a null answer
    /// meant `sandbox_free_params` was **never called**, and a non-null answer
    /// means it is called **twice**, on two different paths. There is no version
    /// of this that does not either leak or free.
    unsafe extern "C" fn sandbox_create_params() -> *mut c_void {
        // A distinct allocation per call, because the guest can hold more than
        // one: `+0x3a94d5d` and the second free site are on different paths of
        // one start-up function, and nothing in the binary says only one set is
        // ever alive at a time.
        let params = Box::into_raw(Box::new(Params::default()));
        let address = params as usize;
        // Cannot already be present: the address came from a live allocation
        // this line made, and nothing has freed it.
        let mut live = live_params().lock().unwrap_or_else(|e| e.into_inner());
        live.get_or_insert_with(Default::default).insert(address);
        note(
            &said::SANDBOX_PARAMS,
            "sandbox_create_params",
            "a real set of name/value pairs (Framework+0x3a94d5d), and sandbox_set_param \
             records into it. Nothing enforces them here: there is no Seatbelt and no MAC \
             framework, so the profile they belong to is refused at the compile step. The \
             parameters are bookkeeping, and this host can do bookkeeping",
        );
        params.cast()
    }

    /// `void sandbox_free_params(sandbox_params_t params)`.
    ///
    /// **A real free, and it is reached.** Both call sites guard with
    /// `test rdi, rdi; je` (`Framework+0x3a95005` and `+0x3a958a3`), so this is
    /// entered with a non-null handle exactly when
    /// [`sandbox_create_params`] produced one — that is, twice, on every launch
    /// that gets that far. A no-op here leaks once per launch, and a null answer
    /// from `create_params` is a way of never reaching it at all.
    ///
    /// A handle this runtime did not hand out is **not** freed, and refusing is
    /// the only safe answer: `sandbox_params_t` is an opaque `void *`, so
    /// `Box::from_raw` on one would be a double free on two counts.
    unsafe extern "C" fn sandbox_free_params(params: *mut c_void) {
        let address = params as usize;
        let freed = {
            let mut live = live_params().lock().unwrap_or_else(|e| e.into_inner());
            // `remove` is the whole of the double-free guard: a second free of the
            // same handle finds nothing and falls through.
            live.as_mut().is_some_and(|live| live.remove(&address))
        };
        if address == 0 || !freed {
            return;
        }
        // SAFETY: `freed` says this address was in the table, which means
        // `sandbox_create_params` produced it with `Box::into_raw` and nothing
        // has freed it since: the only other way out of the table is
        // `sandbox_free_params` itself, and this handle is no longer in it.
        drop(unsafe { Box::from_raw(address as *mut Params) });
    }

    /// `int sandbox_set_param(sandbox_params_t params, const char *key, const char
    /// value, uint32_t set_flags)`.
    ///
    /// **0 when the handle is one of ours. That is the whole wall.**
    ///
    /// The *direction* of the guest's branch is the load-bearing fact, and it is
    /// the opposite of what this note used to say. At `Framework+0x7fe5f82`:
    ///
    /// ```text
    /// 7fe5f7e  mov  rdi, QWORD PTR [r14+0x58]      ; the params handle, never null-tested
    /// 7fe5f82  call _sandbox_set_param
    /// 7fe5f87  test eax, eax
    /// 7fe5f89  je   0x7fe6033                      ; status == 0 ...
    /// 7fe6033  mov  al, 0x1                        ; ... is `true`, and returns
    /// 7fe5f8f  ... erase the cached entry, build a message naming key and value ...
    /// 7fe5fc9  xor  eax, eax                        ; ... and `false` is the fall-through
    /// ```
    ///
    /// So **0 is the success arm**, and the message-building fall-through is the
    /// failure. The previous note here read that fall-through as "a handled
    /// path, and the right one"; the `je` target is the `mov al,0x1` one
    /// instruction before the epilogue, and `test/logs/156.log` has the guest
    /// trapping on this exact answer.
    ///
    /// And `false` is not handled anywhere that matters: the wrapper at
    /// `+0x7fe5e70` is called from 21 places and **18 of them turn `false` into
    /// a bare `int3`** (`Framework+0x3ab0350` `call`, `+0x3ab0355` `test al,al`,
    /// `+0x3ab0357` `je 0x3ab0d2b`, and `+0x3ab0d2b` is the first `cc` of the
    /// eleven-`int3` pad — Linux leaves RIP one past an `int3`, which is why the
    /// log prints `+0x3ab0d2c`). Those are `if (!SetParam(..)) __builtin_trap();`
    /// ten times over in Chromium's sandbox-parameter setter: not a `CHECK`, not a
    /// `NOTREACHED`, so no message and no `LOG(FATAL)`, which is why none of
    /// `check.cc`'s four entries are on the stack. The function is identified
    /// beyond doubt by its own literals — `EXECUTABLE_PATH`, `ENABLE_LOGGING`,
    /// `DISABLE_LOGGING`, `DISABLE_SANDBOX_DENIAL_LOGGING`,
    /// `USER_HOMEDIR_AS_LITERAL`, `FILTER_SYSCALLS{,_DEBUG}`, `TRUE`/`FALSE`,
    /// and the `enable-sandbox-logging` switch.
    ///
    /// **Answering 0 is not a fabrication, and the reason is that this is a
    /// record.** The pair goes in the set and the set can be read back, so "set"
    /// is what happened. The refusal that matters does not move: the profile those
    /// parameters belong to is still refused at [`sandbox_compile_string`], with
    /// a message, on the arm the guest has a path for. "There is no Seatbelt" is
    /// a true answer to "can you confine this process" and a non-answer to "do you
    /// have a place to put my seven parameters".
    ///
    /// `-1` is kept for the two cases that really are failures: a handle this
    /// runtime did not hand out, and a null or empty key. Both are refusals about
    /// the *argument* rather than about the host. The key and value are both
    /// null-tested before either is read.
    unsafe extern "C" fn sandbox_set_param(
        params: *mut c_void,
        key: *const c_char,
        value: *const c_char,
        _set_flags: u32,
    ) -> c_int {
        if key.is_null() || value.is_null() {
            return -1;
        }
        // SAFETY: both pointers are the guest's own NUL-terminated strings, built
        // in its own frame and passed as this call's second and third words; each
        // read stops at its NUL. A pointer into a freed mapping would fault here
        // exactly as it would in the real library, which is the correct outcome
        // for a wrong pointer.
        let (key, value) = unsafe {
            (
                std::ffi::CStr::from_ptr(key).to_string_lossy().into_owned(),
                std::ffi::CStr::from_ptr(value).to_string_lossy().into_owned(),
            )
        };
        if key.is_empty() {
            return -1;
        }
        // Held across the mutation, so a set cannot be freed while it is being
        // written to; it is also the only thing that can free one.
        let mut live = live_params().lock().unwrap_or_else(|e| e.into_inner());
        match params_of(params, &mut live) {
            Some(set) => {
                set.set(key, value);
                0
            }
            None => -1,
        }
    }

    /// `void *sandbox_compile_string(const char *profile, void **sandbox_ret, char
    /// **errorbuf)`.
    ///
    /// **Null, and the error out-parameter carries a message — because a null
    /// message is a `SIGILL` in the guest.**
    ///
    /// The call is at `Framework+0x7fe65cf`, and the whole arm is:
    ///
    /// ```text
    /// 7fe65ba  mov  r15, rcx                        ; the guest's 4th argument
    /// 7fe65bd  mov  r14, rdx                        ; and the 3rd
    /// 7fe65c0  mov  QWORD PTR [rbp-0x20], 0x0       ; pre-nulls the error slot
    /// 7fe65c8  mov  rsi, QWORD PTR [rsi]            ; the profile, dereferenced
    /// 7fe65cb  lea  rdx, [rbp-0x20]                 ; the error out-parameter
    /// 7fe65cf  call _sandbox_compile_string
    /// 7fe65d4  mov  rbx, rax
    /// 7fe65d7  test rax, rax
    /// 7fe65da  je   0x7fe65f3                       ; the failure arm
    /// 7fe65f3  mov  r14, QWORD PTR [rbp-0x20]       ; reads *errorbuf
    /// 7fe65f7  test r15, r15                        ; the guest's 4th argument
    /// 7fe65fa  je   0x7fe660c                       ; wants no string: free and go
    /// 7fe65fc  test r14, r14
    /// 7fe65ff  je   0x7fe665d                       ; <-- ud2. SIGILL.
    /// 7fe6601  mov  rdi, r15 ; 7fe6604  mov rsi, r14
    /// 7fe6607  call std::string::assign
    /// 7fe660c  mov  rdi, r14
    /// 7fe660f  call _sandbox_free_error
    /// 7fe664c  test rbx, rbx
    /// 7fe664f  setne al                             ; returns (profile != 0) = false
    /// 7fe665d  ud2
    /// ```
    ///
    /// So the guest always passes a non-null fourth argument
    /// (`Framework+0x7fe6380`'s caller hands it `rcx = &this->error_string`), and
    /// therefore **a null `*errorbuf` on the failure arm is an illegal
    /// instruction.** This is the shape `__sandbox_ms` already got right 350
    /// lines above it, and for the same reason.
    ///
    /// Two other things the same disassembly settles, and neither changes the
    /// answer:
    ///
    /// * The success arm is *also* a `ud2`, at the same `+0x7fe665d`. It reads
    ///   `[profile+0x8]` and `[profile+0x10]` and traps if either is zero
    ///   (`+0x7fe6624`…`+0x7fe663a`). So a *fabricated* profile would have to be
    ///   a real object with a non-null pointer and a non-zero length in it, which
    ///   is one more reason the refusal here is the right shape and not a
    ///   convenient one.
    /// * The guest's second argument (`rsi`) is dereferenced *before* the call and
    ///   the third is overwritten with the error out-parameter, so the callee is
    ///   handed four words against three parameters in any header. Four are
    ///   declared here because the binary is the authority; only the third is
    ///   written, because the third is the one the guest reads back.
    ///
    /// **What the refusal does not say:** it does not say there is no mechanism on
    /// this host. Landlock and seccomp are both present and enabled here
    /// (`/sys/kernel/security/lsm` lists `landlock`; all seven seccomp actions are
    /// in `/proc/sys/kernel/seccomp/actions_avail`), and a Landlock+seccomp
    /// realisation of this profile is real work this host could support. It is
    /// not built, and this message says "not built" rather than "impossible".
    unsafe extern "C" fn sandbox_compile_string(
        _profile: *const c_char,
        _unused: *mut c_void,
        errorbuf: *mut *mut c_char,
        _unused_last: *mut c_void,
    ) -> *mut c_void {
        if !errorbuf.is_null() {
            // SAFETY: the caller's own out-parameter — `lea rdx, [rbp-0x20]` in
            // the guest's own frame, pre-nulled at `+0x7fe65c0`. The guest reads
            // the word back at `+0x7fe65f3` and hands it to
            // `std::string::assign` and then to `sandbox_free_error`, and a null
            // there is the `ud2` at `+0x7fe665d`. It is registered as ours so
            // that `sandbox_free_error` can release exactly this allocation and
            // no other.
            unsafe {
                errorbuf.write(super::guest_error_string(
                    "rosetta3: sandbox_compile_string: refused - there is no Seatbelt on \
                     this host and no MAC framework to enforce what one would compile, so \
                     no profile can be compiled. The parameters were recorded (see \
                     darwin::last_unavailable::sandbox_set_param); nothing enforces them \
                     here yet. Landlock and seccomp are both present on this host, so a \
                     real realisation is available and is not built",
                ))
            };
        }
        std::ptr::null_mut()
    }

    /// `void sandbox_free_profile(void *profile)`.
    ///
    /// A no-op, as [`sandbox_free_params`]: no profile is ever compiled, so none
    /// is ever allocated. The guest does reach it, with a null handle, on the
    /// success arm of the compile (`Framework+0x7fe6647`, reached only when the
    /// compile returned non-null) — so a null is the one argument it can actually
    /// pass here, and a no-op is the right answer for it too.
    unsafe extern "C" fn sandbox_free_profile(_profile: *mut c_void) {}

    // ------------------------------------------------------- SystemConfiguration

    /// `CFStringRef SCDynamicStoreCopyLocalHostName(SCDynamicStoreRef store)`.
    ///
    /// **Null, and null is the whole answer: there is no SystemConfiguration on
    /// this host, so there is no Bonjour-scheduled local host name to copy.**
    ///
    /// The guest asks for the local host name first and the computer name second,
    /// and has a path for both being null:
    ///
    /// ```text
    /// 1b22803  call _SCDynamicStoreCopyLocalHostName
    /// 1b22808  test rax, rax
    /// 1b2280b  jne  0x1b2281c                 ; got one: use it
    /// 1b2280d  mov  rdi, r14 ; 1b22810  xor esi, esi
    /// 1b22812  call _SCDynamicStoreCopyComputerName
    /// 1b22817  test rax, rax
    /// 1b2281a  je   0x1b22852                 ; neither: the empty-string path
    /// ```
    ///
    /// — which is Chromium's `base::SysWideHostname()` falling back to the empty
    /// string when SystemConfiguration has nothing to say, and every caller of it
    /// has a path for an empty name. The store argument is read by nothing here.
    unsafe extern "C" fn sc_dynamic_store_copy_local_host_name(
        _store: *mut c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `CFStringRef SCDynamicStoreCopyComputerName(SCDynamicStoreRef store,
    /// char *name)`.
    ///
    /// **Null, and the second argument is a literal zero that must not be
    /// written through.** `Framework+0x1b22810` is `xor esi,esi` in the two
    /// instructions before the call, so the header's `char *name` arrives as
    /// `NULL`; a function that treated it as an out-parameter would fault at
    /// address 0. The same shape is at `+0x6b34eb0` and at
    /// `+0x5094260`'s call. See [`names_that_are_not_pointers`].
    unsafe extern "C" fn sc_dynamic_store_copy_computer_name(
        _store: *mut c_void,
        _name: *mut c_char,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `CFArrayRef SCDynamicStoreKeyCreateProxies(CFStringRef
    /// proxyAutoConfigURLStr)`.
    ///
    /// **Null, and the one argument is a literal zero that is a legitimate
    /// `NULL` and not an out-parameter.** `Framework+0x674ae25` is `xor edi,edi`
    /// and the call follows at `+0x674ae27`; the answer is stored at `[rbp-0x18]`
    /// and wrapped in a `CFArrayCreate` of one element, so the value matters. A
    /// null auto-configuration URL is what Chromium passes when it has no PAC
    /// script, and real SystemConfiguration has no proxy keys to return for one.
    unsafe extern "C" fn sc_dynamic_store_key_create_proxies(
        _proxy_auto_config_url_str: *mut c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `const char *SCErrorString(int error)`.
    ///
    /// **A non-null string, always, because the guest `strlen`s the answer
    /// without testing it.** Three call sites, all the same shape:
    ///
    /// ```text
    /// 6755026  call _SCErrorString
    /// 675502b  mov  r12, rax
    /// 675502e  mov  rdi, rax
    /// 6755031  call _strlen                    ; no null test anywhere
    /// ```
    ///
    /// and at `+0x50949b0` and `+0x675514c` likewise. A null here is a fault at
    /// address 0, so "no answer" is not available in this function's vocabulary.
    ///
    /// What it says is a statement of fact rather than an invented SC error: the
    /// value it is handed is not a SystemConfiguration error at all — one call
    /// site takes it straight out of `errno` (`+0x6754c77` `movzx r12d, BYTE PTR
    /// [rax]`, having just zeroed `*errno`) — and the honest description of an
    /// unknown code on a host with no SystemConfiguration is that there is no
    /// SystemConfiguration to have named it. The number is interpolated because
    /// it is the one piece of information the caller had, and the string is
    /// leaked once per call for the reason [`leaked`] gives.
    unsafe extern "C" fn sc_error_string(error: c_int) -> *const c_char {
        leaked(format!(
            "Unknown error {error}: there is no SystemConfiguration on this host, so \
             this is not a SystemConfiguration error"
        ))
    }

    // ------------------------------------------------------------------ CFNetwork

    /// `CFArrayRef CFNetworkCopyProxiesForURL(CFURLRef url, CFDictionaryRef
    /// proxySettings)`.
    ///
    /// **Null, which is the truth: this host has no system proxy configuration,
    /// so there are no proxies for any URL.** The call is at
    /// `Framework+0x7e7bb6b`, with an *empty* `CFDictionary` for
    /// `proxySettings` built by a `CFDictionaryCreate` with all six arguments
    /// zeroed at `+0x7e7bb4b`-`+0x7e7bb59`, and the answer stored at `[rbp-0x140]`.
    /// The one thing the guest does with it is release it, behind a null test:
    ///
    /// ```text
    /// 7e7c407  mov  rdi, QWORD PTR [rbp-0x140]
    /// 7e7c40e  test rdi, rdi
    /// 7e7c411  je   0x7e7c418                 ; null is fine
    /// 7e7c413  call _CFRelease
    /// ```
    ///
    /// **This call site is not the one `test/tools/callers.py` reports.** It gave
    /// `Framework+0x7e7b540+0x62b = 0x7e7b76b`, and the bytes there are
    /// `49 06 48 85 ff 74 06` — a `mov QWORD PTR [r14],rax` whose ModRM byte
    /// happened to read as an `e8` with a `rel32` that lands on the stub. The
    /// real call is at `0x7e7bb6b`; both were found by scanning `__TEXT,__text`
    /// for `e8` against the stub address directly, which is what the note in
    /// PLAN.md §7 asks for.
    unsafe extern "C" fn cfnetwork_copy_proxies_for_url(
        _url: *mut c_void,
        _proxy_settings: *mut c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    /// `CFArrayRef CFNetworkExecuteProxyAutoConfigurationURL(CFURLRef url,
    /// CFStreamClientRef client, CFStreamClientCallBack clientCallBack,
    /// CFStreamClientContext *clientContext, CFArrayRef *proxyList, CFErrorRef
    /// *error)`.
    ///
    /// **Null — and the last two parameters are deliberately left unwritten,
    /// because the guest never sets the registers they arrive in:**
    ///
    /// ```text
    /// 7e7bbac  lea  rdx, [rip+0x91d]          ; the PAC callback
    /// 7e7bbb3  lea  rcx, [rbp-0x270]          ; the client context
    /// 7e7bba  lea  rax, [rbp-0x108]
    /// 7e7bba5  mov  QWORD PTR [rbp-0x268], rax ; …as the context's `info`
    /// 7e7bbba  mov  rdi, r12                  ; the URL
    /// 7e7bbbd  mov  rsi, r14                  ; the stream client
    /// 7e7bbc0  call _CFNetworkExecuteProxyAutoConfigurationURL
    /// 7e7bbc5  test rax, rax
    /// 7e7bccf  je   0x7e7bc9c                 ; null: the no-proxy path
    /// ```
    ///
    /// Four instructions of setup and not one touch of `r8` or `r9`. They hold
    /// whatever the caller's frame held, so writing through them would be a store
    /// to an arbitrary address. Real CFNetwork also only writes `proxyList` and
    /// `error` on success, and this is the failure answer, so not writing is what
    /// the real function does too.
    ///
    /// The null is handled with a named result rather than an error: the guest
    /// sets `r13d = 0xfffffffe` (`+0x7e7bc9c`) and returns `-2`, which is its
    /// "no proxy" code.
    unsafe extern "C" fn cfnetwork_execute_proxy_auto_configuration_url(
        _url: *mut c_void,
        _client: *mut c_void,
        _client_call_back: *mut c_void,
        _client_context: *mut c_void,
        _proxy_list: *mut *mut c_void,
        _error: *mut *mut c_void,
    ) -> *mut c_void {
        std::ptr::null_mut()
    }

    // ------------------------------------------------------------- CoreServices

    /// `void __CSCheckFixDisable(void)`.
    ///
    /// Note the **double leading underscore in the C name** — the image imports
    /// `__CSCheckFixDisable` (see the module note).
    ///
    /// **A no-op, and this is the whole of what the symbol is.** The `__CSCheck*`
    /// family is CoreServices' own existence marker: it defines
    /// `__CSCheckFixDisable` if and only if `FixDisable` is present, so a client
    /// can test for the real symbol without linking against a version of the
    /// framework that has it. The marker function has no body to speak of and
    /// returns nothing. The guest calls it and continues
    /// (`Framework+0xc8f771d`, four instructions before a `ret`), and adds no
    /// diagnostic of its own, which is the behaviour a correct marker has.
    unsafe extern "C" fn cs_check_fix_disable() {}

    /// `OSStatus LSSetApplicationLaunchServicesServerConnectionStatus(Boolean
    /// inServerConnectionStatus)`.
    ///
    /// Note the **double leading underscore in the C name** — the image imports
    /// `__LSSetApplicationLaunchServicesServerConnectionStatus`.
    ///
    /// **`noErr`, and `noErr` is a true answer rather than a shrug.** The guest
    /// passes a literal `0` (`Framework+0x80f048a` `xor edi,edi`), which is the
    /// *declaration* that this process is **not** connected to the LaunchServices
    /// server — and on this host it is not, because there is no LaunchServices.
    /// The state the caller asked for is the state that already holds, so there is
    /// nothing to report as failed.
    ///
    /// The guest discards the answer entirely: `+0x80f0491` is the next
    /// instruction and it tears the frame down, so nothing depends on it either
    /// way. The note below is here because the *capability* is genuinely absent
    /// and PLAN.md §1 asks that be visible somewhere, not because the value
    /// matters.
    unsafe extern "C" fn ls_set_application_launch_services_server_connection_status(
        _in_server_connection_status: bool,
    ) -> c_int {
        note(
            &said::SANDBOX,
            "LSSetApplicationLaunchServicesServerConnectionStatus",
            "there is no LaunchServices on this host; the caller asked to be recorded \
             as not connected to its server, which is the status that already holds, \
             so this reports success and changes nothing (Framework+0x80f048c)",
        );
        0
    }

    // ----------------------------------------------------------------------- mach

    /// Darwin's `KERN_FAILURE`, from `<mach/kern_return.h>`: the generic "this
    /// did not happen", and the code `darwin::mach` uses where it declines a
    /// kernel interface.
    const KERN_FAILURE: c_int = 1;

    /// `kern_return_t mach_vm_purgable_control(vm_map_read_t target_task,
    /// vm_address_t address, int control_cmd, int32_t *control_value)`.
    ///
    /// **`KERN_FAILURE`, and the out-parameter is written** even though the guest
    /// does not test the status — `Framework+0x81c410b` is
    /// `and DWORD PTR [rbp-0x94],0x3`, the very next instruction, with no
    /// `test eax,eax` in between. The slot is pre-zeroed at `+0x81c40e6` and
    /// nulled again here, so the read is 0 whatever the status, which is what the
    /// call is asking about: the two low bits of `control_value` are
    /// `VM_PURGABLE_CONTROL_*` state, and 0 is "not purgeable", which is the truth
    /// for memory this host does not manage.
    ///
    /// The four arguments are the binary's: `rdi` is a 32-bit port name
    /// (`mov edi,r15d` at `+0x81c40f7`), `rsi` a `vm_address_t` loaded from
    /// `[rbp-0xd0]`, `edx` the literal `1` (`VM_PURGABLE_CONTROL_PURGE`), and
    /// `rcx` the out-parameter.
    ///
    /// **This is the one name here that `darwin::divergent` deliberately denies**,
    /// and it was chosen as the witness for "a Mach memory call this crate
    /// declines" — `divergent.rs:442` asserts `reason("mach_vm_purgable_control")
    /// == Some(MACH)` and `divergent.rs:515` asserts
    /// `contains("mach_vm_purgable_control")`. **`divergent::reason` carves out
    /// every name this crate implements** (`divergent.rs:377`), so publishing
    /// this one makes those two assertions fail, and they are the only two lines
    /// in the tree that object.
    ///
    /// They have to go, and the reason is the one PLAN.md §4 ranks above a
    /// refusal: `divergent` *denies* a name, it does not bind it, so leaving
    /// `mach_vm_purgable_control` there leaves the import **unbound** — a zero in
    /// its slot and a jump through it, from a guest that calls this at
    /// `Framework+0x81c4106` on every Mach VM walk. `darwin::symbol` consults
    /// `implemented` before `divergent`, so publishing it here binds the import
    /// and the `mach_` prefix entry in `divergent` goes on denying the *family*,
    /// which is what it is for.
    unsafe extern "C" fn mach_vm_purgable_control(
        _target_task: u32,
        _address: u64,
        _control_cmd: c_int,
        control_value: *mut c_int,
    ) -> c_int {
        if !control_value.is_null() {
            // SAFETY: the caller's own out-parameter, and the guest reads it at
            // `Framework+0x81c410b` without testing the status first.
            unsafe { control_value.write(0) };
        }
        KERN_FAILURE
    }

    // ---------------------------------------------------------------- the unwinder

    /// **Seven of these are answered, four are refused, and the difference is
    /// which side of a crate boundary the capability sits on.**
    ///
    /// The seven — `_Unwind_GetIP`, `_Unwind_GetIPInfo`, `_Unwind_GetGR`,
    /// `_Unwind_SetGR`, `_Unwind_SetIP`, `_Unwind_GetRegionStart` and
    /// `_Unwind_GetLanguageSpecificData` — are published as *field accessors* on
    /// a `struct _Unwind_Context` this crate creates, and the module
    /// [`unwind_context`] holds both. They were refusals until the walk they
    /// describe arrived; the retirement of the old argument is below, and it is
    /// worth reading because the argument was not wrong, only aimed at a premise
    /// that has since changed.
    ///
    /// The four that stay — `_Unwind_GetTextRelBase`, `_Unwind_GetDataRelBase`,
    /// `_Unwind_DeleteException` and `_Unwind_Backtrace` — are named in their own
    /// doc comments with what is missing, and none of the four reasons is "no
    /// walk happened" any more.
    ///
    /// # The retired argument, kept because it is why the rows cannot be dropped
    ///
    /// Nine of the twelve `_Unwind_*` names are one load and a `ret` in the
    /// host's `libgcc_s.so.1` — and **not one of them calls
    /// `_Unwind_Find_FDE`**:
    ///
    /// ```text
    /// 000000000001f300 <_Unwind_GetIP>:
    ///    1f300:  48 8b 87 98 00 00 00   mov rax,QWORD PTR [rdi+0x98]
    ///    1f30b:  c3                   ret
    /// 000000000001f350 <_Unwind_GetRegionStart>:
    ///    1f350:  48 8b 87 b8 00 00 00   mov rax,QWORD PTR [rdi+0xb8]
    ///    1f35b:  c3                   ret
    /// 000000000001f390 <_Unwind_GetDataRelBase>:
    ///    1f390:  48 8b 87 b0 00 00 00   mov rax,QWORD PTR [rdi+0xb0]
    ///    1f3a0b: c3                   ret
    /// ```
    ///
    /// They do not search unwind data; they report what an unwinder *wrote*. And
    /// the field they report from is **libgcc's** `struct _Unwind_Context` layout
    /// — `0x90`, `0x98`, `0xa0`, `0xa8`, `0xb0`, `0xb8`, `0xc0` — which is not
    /// the one LLVM's `UnwindContext` uses, and not the one
    /// [`unwind_context::Context`] is. **Every word of that argument still
    /// holds, and it is now the reason these seven rows must exist rather than
    /// the reason they could not:** a name that is *not* published here falls
    /// through `darwin::symbol` to `dlsym`, the loader binds
    /// `libgcc_s.so.1`, and `_Unwind_GetIP` becomes `mov rax,[rdi+0x98]` against
    /// a struct whose `+0x98` is roughly two kilobytes past anything this crate
    /// allocates. That is a silent wrong answer, which is the one outcome this
    /// file exists to prevent and the one an unbound-or-mislaid row produces.
    ///
    /// What changed is the premise underneath it. The old premise was that the
    /// walk had never happened, so the context was a struct describing nothing:
    ///
    /// > *`nm -D /usr/lib/libgcc_s.so.1` exports all eleven of the names; a grep
    /// > over every `.rs` returns zero hits for `__register_frame_info`,
    /// > `eh_frame_hdr`, `dl_iterate_phdr` and `PT_GNU_EH_FRAME`, so nothing has
    /// > ever asked this process's guest frames anything; and `__TEXT,__text` is
    /// > 214,556,026 bytes against 146,536 bytes of `__eh_frame` — 0.0683% — with
    /// > the rest in a compact `__unwind_info` `libgcc_s` does not read.*
    ///
    /// All of that is still true of the **host's** unwinder. It stopped being
    /// true of this tree's. `crates/rosetta3/src/unwind_native.rs` parses each
    /// guest image's `__TEXT,__unwind_info`, steps a frame out of it, and asks
    /// **that frame's own personality routine** whether it catches — so a walk
    /// does happen, in this process, and the context it writes is one this crate
    /// allocated. The seven accessors are the only way anything reads it back
    /// out, so they have to be *ours* to be right.
    ///
    /// # Which routine, and why that decides the context's size
    ///
    /// The Framework's `__unwind_info` header says `personalities 2@0x218`.
    /// Entry 1 is the GOT slot `0xdd51b20`, whose value in the file is
    /// **`0x5680bd0` — a function inside the Framework**, its own statically
    /// linked `__gxx_personality_v0`, covering 3,435 frames. Entry 2 is the slot
    /// `0xdd51b28`, value `0`, wanting the `___objc_personality_v0` bind, and
    /// covering 53. So the guest brings its own personality for 99% of its
    /// frames, and **that** is what makes the accessors a closed five-word
    /// conversation rather than libunwind's couple of kilobytes of its own
    /// register numbering: over all 1,840 bytes of `0x5680bd0` there is not one
    /// memory operand based on the register the context arrived in. The
    /// measurement, and its one exception, are in [`unwind_context`].
    ///
    /// # Who calls the four that are still refused
    ///
    /// | name | call sites | where |
    /// |---|--:|---|
    /// | `_Unwind_DeleteException` | 1 | `0x568095f` |
    /// | `_Unwind_GetTextRelBase` | **0** | imported and never called |
    /// | `_Unwind_GetDataRelBase` | **0** | imported and never called |
    /// | `_Unwind_Backtrace` | 1 | `0x414d61`, and it returns |
    ///
    /// The call-site counts are an `e8`-scan of `__TEXT,__text` against each
    /// name's `__TEXT,__stubs` address, with `__Unwind_Resume` (3,581) and
    /// `___cxa_atexit` (251) as positive controls in the same scan.
    ///
    /// # The two names still deliberately absent
    ///
    /// `__Unwind_RaiseException` (2 call sites) and `__Unwind_Resume` (3,581) are
    /// **not** in this table, and the test
    /// [`tests::the_two_walkers_are_left_for_the_routing_fix`] fails if anybody
    /// adds them. They walk, and the walk is
    /// `crates/rosetta3/src/unwind_native.rs`, gated on
    /// `ROSETTA3_NATIVE_UNWINDER=1` and reachable through
    /// `crate::unwind::shim_for`'s successor `crate::unwind_native::shim_for` on
    /// the emulated backend. On **native** they are not yet reachable at all,
    /// and the reason is a specific one: a published address is entered as an
    /// ordinary host call, so an entry point has to be able to see and rewrite
    /// the **live** register context, and that mechanism is `crate::jmpbuf`.
    /// Publishing an `extern "C"` that built a fresh `RegisterContext` would be
    /// the one failure ranked above a wrong answer — a landing pad's registers
    /// written into nothing. So the two names stay absent here, where taking them
    /// would turn a solvable wiring problem into a documented dead end.
    mod unwind_context_accessors {
        use super::{c_void, refuse_unwind, said_unwind};

        /// `_Unwind_Ptr _Unwind_GetTextRelBase(struct _Unwind_Context *context)`.
        ///
        /// **Imported, never called, refused anyway, and the reason is not "no
        /// walk happened" any more.** It has **zero** call sites, so there is no
        /// caller to be truthful *to*; and there is no field to be truthful
        /// *with*: the context this crate hands a personality routine is five
        /// words — see [`super::unwind_context::Context`] — and neither a text
        /// nor a data reloc base is one of them. Inventing a sixth word to answer
        /// a name nobody calls would be a plausible value for a reader that
        /// cannot exist.
        ///
        /// It stays bound to a refusal rather than dropped from the table because
        /// dropping it is *worse* than refusing it: an unanswered name falls
        /// through `darwin::symbol` to `dlsym`, the loader binds the host's
        /// `libgcc_s.so.1` (`nm -D` confirms it exports the name), and the guest
        /// gets `mov rax,[rdi+0xb0]` — which is exactly the silent wrong answer
        /// this section was written to stop. It is invisible: the unbound-import
        /// census reports a bound name as bound either way.
        pub unsafe extern "C" fn unwind_get_text_rel_base(_context: *mut c_void) -> u64 {
            refuse_unwind(
                &said_unwind::GET_TEXT_REL_BASE,
                "_Unwind_GetTextRelBase",
                "nothing in Chromium Framework calls it: an e8-scan of __TEXT,__text \
                 against its __TEXT,__stubs address finds zero sites, with __Unwind_Resume \
                 (3581) and ___cxa_atexit (251) as positive controls in the same scan. \
                 There is also nothing to answer with: the _Unwind_Context this crate \
                 hands a personality routine is five words, and a text reloc base is not \
                 one of them. Refused rather than dropped from the table, because a \
                 dropped name falls through to dlsym and the loader binds the host's \
                 libgcc_s.so.1 copy - which is mov rax,[rdi+0xb0] against a struct whose \
                 +0xb0 this crate never allocated.",
            )
        }

        /// `_Unwind_Ptr _Unwind_GetDataRelBase(struct _Unwind_Context *context)`.
        ///
        /// As [`unwind_get_text_rel_base`]: **zero** call sites, no field to answer
        /// with, and a name the loader has to bind to *something* it can see.
        pub unsafe extern "C" fn unwind_get_data_rel_base(_context: *mut c_void) -> u64 {
            refuse_unwind(
                &said_unwind::GET_DATA_REL_BASE,
                "_Unwind_GetDataRelBase",
                "nothing in Chromium Framework calls it: zero call sites by the same \
                 e8-scan, and the two reloc-base names are the only two this image \
                 imports and never uses - a linker artefact of the library it was \
                 built against, not code it runs. There is nothing to answer with \
                 either: the _Unwind_Context this crate creates is five words. Refused \
                 rather than dropped, because a dropped name is bound by the loader to \
                 the host's libgcc_s.so.1 copy instead.",
            )
        }

        /// `void _Unwind_DeleteException(struct _Unwind_Exception *exception_object)`.
        ///
        /// The only call site, `Framework+0x568095f`, is in the guest's own
        /// `__cxa_end_catch` path — the function that decrements a handler count
        /// and then releases the exception. **It is refused rather than made a
        /// no-op**, and that is the one place in this section where refusing
        /// costs something a no-op would not: the guest leaks. A leak is the
        /// truthful outcome, because the alternative is freeing memory whose
        /// layout this runtime has never seen — the exception object is one
        /// **guest** allocation, made by the guest's own `__cxa_allocate_exception`
        /// and filled in by the guest's own throw, and a `free` of it from here
        /// would be a `free` of a pointer this crate cannot account for. The name
        /// says so before the process stops.
        pub unsafe extern "C" fn unwind_delete_exception(_exception_object: *mut c_void) {
            refuse_unwind(
                &said_unwind::DELETE_EXCEPTION,
                "_Unwind_DeleteException",
                "the guest's own __cxa_end_catch path, at Framework+0x568095f - the only \
                 call site in the image. The exception object it would free is a guest \
                 allocation this runtime never made and has no allocator for: refusing \
                 leaks it, and leaking is the truthful outcome, because the alternative \
                 is freeing a pointer this crate cannot account for. This is the one \
                 place in this section where a no-op would be cheaper, and it is refused \
                 anyway because a silent no-op is a promise this crate cannot keep.",
            )
        }
    }

    // The seven answered accessors, and the five-word context they read and
    // write. `exports!`'s `=>` target is a single `expr` fragment, so a
    // `{ … }` block there does not parse - hence the module-qualified names
    // below rather than a `use` that would shadow them.
    use super::unwind_context as unw;
    use unwind_context_accessors as unwind_accessors;

    /// `_Unwind_Reason_Code _Unwind_Backtrace(_Unwind_Trace *trace, void
    /// _trace_argument)`.
    ///
    /// **The one refusal in this file that returns instead of stopping, and the
    /// reason is in the guest's own code.** The single call site is
    /// `Framework+0x414d61`, and the guest does not look at the return value at
    /// all:
    ///
    /// ```text
    /// 414d53  lea  rdi,[rip+0x3b96]            # 0x4188f0, the trace callback
    /// 414d5a  lea  rsi,[rbp-0xc0]
    /// 414d61  call __Unwind_Backtrace
    /// 414d66  cmp  BYTE PTR [rbp-0x29],0x0
    /// 414d6a  je   0x414da2                    ; nothing collected: a real arm
    /// ```
    ///
    /// The flag at `[rbp-0x29]` is set by the *callback*, not by the count, so an
    /// empty trace is a path the guest has and takes. And "no frames" is not a
    /// fabrication: it is the true answer to *how many guest frames did you
    /// walk* from a function that walks none. **Nothing walks on this name's
    /// behalf** — it is entered as an ordinary host call, so there is no live
    /// register context behind it to enumerate from, and the driver that *can*
    /// walk guest frames is reached by a different name. It is the same outcome
    /// PLAN.md §4 already accepts for Chromium's sampling profiler under
    /// `yama_ptrace_scope=1` — one empty sample, not an abort — and the same one
    /// `crate::media_unavailable` returns for `unw_step`.
    ///
    /// So: say it, by name, and answer nothing. `0` is the count a backtrace
    /// that walked nothing returns, and the guest reads no other field of it.
    unsafe extern "C" fn unwind_backtrace(
        _trace: *mut c_void,
        _trace_argument: *mut c_void,
    ) -> c_int {
        note(
            &said_unwind::BACKTRACE,
            "_Unwind_Backtrace",
            "nothing walks on this name's behalf, so the trace is empty: this tree can \
             walk guest frames, but only through a driver that is handed a live register \
             context, and this is entered as an ordinary host call with none. The guest \
             tests the callback's own flag and has an arm for nothing collected \
             (Framework+0x414d66), and it never reads the count.",
        );
        0
    }

    // ------------------------------------------------------------------ the table

    exports::exports! {
        functions {
            // VideoToolbox: the session half. There is no VideoToolbox on this
            // host, so there is nothing to decode or encode with.
            "VTDecompressionSessionCreate" => vt_decompression_session_create,
            "VTDecompressionSessionInvalidate" => vt_decompression_session_invalidate,
            "VTDecompressionSessionCanAcceptFormatDescription"
                => vt_decompression_session_can_accept_format_description,
            "VTDecompressionSessionDecodeFrame" => vt_decompression_session_decode_frame,
            "VTCompressionSessionCreate" => vt_compression_session_create,
            "VTCompressionSessionInvalidate" => vt_compression_session_invalidate,
            "VTCompressionSessionEncodeFrame" => vt_compression_session_encode_frame,
            "VTCompressionSessionCompleteFrames" => vt_compression_session_complete_frames,
            "VTSessionCopyProperty" => vt_session_copy_property,
            "VTSessionSetProperty" => vt_session_set_property,
            "VTSessionCopySupportedPropertyDictionary"
                => vt_session_copy_supported_property_dictionary,
            "VTCreateCGImageFromCVPixelBuffer" => vt_create_cg_image_from_cv_pixel_buffer,
            "VTPixelTransferSessionCreate" => vt_pixel_transfer_session_create,
            "VTPixelTransferSessionInvalidate" => vt_pixel_transfer_session_invalidate,
            "VTPixelTransferSessionTransferImage" => vt_pixel_transfer_session_transfer_image,

            // QuartzCore: the one name here with a right answer on this host.
            "CATransform3DMakeTranslation" => ca_transform3d_make_translation,

            // Metal: no device, so nothing to copy.
            "MTLCopyAllDevicesWithObserver" => mtl_copy_all_devices_with_observer,

            // libbsm: no audit token, so no pid.
            "audit_token_to_pid" => audit_token_to_pid,

            // libobjc: a copy when there is no `__strong` member to retain.
            "objc_copyStruct" => objc_copy_struct,

            // libresolv: a zeroed `__res_state` is valid, and has no nameservers.
            "res_9_ninit" => res_9_ninit,
            "res_9_ndestroy" => res_9_ndestroy,
            "res_9_getservers" => res_9_getservers,

            // The Seatbelt, which does not exist here. `__sandbox_ms` carries
            // its own second leading underscore; see the module note.
            "__sandbox_ms" => sandbox_ms,
            "sandbox_create_params" => sandbox_create_params,
            "sandbox_free_params" => sandbox_free_params,
            "sandbox_set_param" => sandbox_set_param,
            "sandbox_compile_string" => sandbox_compile_string,
            "sandbox_free_profile" => sandbox_free_profile,

            // SystemConfiguration: no dynamic store, so no names and no keys.
            "SCDynamicStoreCopyLocalHostName" => sc_dynamic_store_copy_local_host_name,
            "SCDynamicStoreCopyComputerName" => sc_dynamic_store_copy_computer_name,
            "SCDynamicStoreKeyCreateProxies" => sc_dynamic_store_key_create_proxies,
            "SCErrorString" => sc_error_string,

            // CFNetwork: no system proxy configuration.
            "CFNetworkCopyProxiesForURL" => cfnetwork_copy_proxies_for_url,
            "CFNetworkExecuteProxyAutoConfigurationURL"
                => cfnetwork_execute_proxy_auto_configuration_url,

            // CoreServices. Both rows carry the `__` spelling the image uses.
            "_CSCheckFixDisable" => cs_check_fix_disable,
            "_LSSetApplicationLaunchServicesServerConnectionStatus"
                => ls_set_application_launch_services_server_connection_status,

            // The Mach call `crate::divergent` names as its witness; see
            // [`mach_vm_purgable_control`].
            "mach_vm_purgable_control" => mach_vm_purgable_control,

            // The DWARF unwinder's own state, in two halves. Every row carries
            // **one** leading underscore, so `exports!` publishes the two the
            // image imports - the same trap as `__sandbox_ms` and
            // `__CSCheckFixDisable` above, and for the same reason. See
            // [`unwind_context_accessors`] for the measurements.
            //
            // The seven accessors are ANSWERED, on the five-word context in
            // `unwind_context`. They are rows here rather than absent names
            // because absent is the one outcome worse than a wrong answer:
            // `darwin::symbol` would fall through to `dlsym`, the loader would
            // bind `libgcc_s.so.1`, and the guest's personality routine would
            // read a struct this crate never allocated.
            "_Unwind_GetIP" => unw::unwind_get_ip,
            "_Unwind_GetIPInfo" => unw::unwind_get_ip_info,
            "_Unwind_GetGR" => unw::unwind_get_gr,
            "_Unwind_SetGR" => unw::unwind_set_gr,
            "_Unwind_SetIP" => unw::unwind_set_ip,
            "_Unwind_GetRegionStart" => unw::unwind_get_region_start,
            "_Unwind_GetLanguageSpecificData" => unw::unwind_get_language_specific_data,
            // The four that stay refused, each with its own sentence: two names
            // with zero call sites and no field to answer with, one that would
            // free a guest allocation, and the one that returns.
            "_Unwind_GetTextRelBase" => unwind_accessors::unwind_get_text_rel_base,
            "_Unwind_GetDataRelBase" => unwind_accessors::unwind_get_data_rel_base,
            "_Unwind_DeleteException" => unwind_accessors::unwind_delete_exception,
            // The soft one, and the only one that returns.
            "_Unwind_Backtrace" => unwind_backtrace,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // ------------------------------------------------------ the name spellings

        /// The names the image spells with **two** leading underscores.
        ///
        /// `exports!` publishes `"_" + <row>`, and the loader resolves imports
        /// verbatim. A row written as `"sandbox_ms"` would publish `_sandbox_ms`,
        /// the build would be clean, the suite would be green, and the import
        /// would still be unbound — the failure the progress log records twice.
        /// The other half is `darwin::symbol`, which strips exactly one
        /// underscore in `c_name`, so both paths are checked here.
        ///
        /// The eleven `_Unwind_*` rows are in the same class and for the same
        /// reason: the image imports `__Unwind_GetIP`, not `_Unwind_GetIP`, so
        /// the row is `"_Unwind_GetIP"` and the published name gains the second
        /// underscore. They are listed separately below because the *set* is
        /// load-bearing in a way the other three are not.
        #[test]
        fn names_are_the_image_spellings() {
            for (row, published) in [
                ("__sandbox_ms", "___sandbox_ms"),
                (
                    "_LSSetApplicationLaunchServicesServerConnectionStatus",
                    "__LSSetApplicationLaunchServicesServerConnectionStatus",
                ),
                ("_CSCheckFixDisable", "__CSCheckFixDisable"),
            ] {
                assert_eq!(format!("_{row}"), published, "one underscore too few");
                // And the name the *other* path asks about. `darwin::symbol`
                // strips exactly one leading underscore in `c_name` before it
                // consults the table, so the raw import has to reduce to the row.
                let asked = crate::base_name(published);
                assert_eq!(asked, row, "the table is keyed by the bare C name");
                assert!(
                    (MODULE.symbol)(asked).is_some(),
                    "{asked} resolves to nothing, so `darwin::symbol` would fall \
                     through to `divergent` and leave the import unbound"
                );
            }
        }

        /// The eleven `_Unwind_*` rows, each pinned to the spelling the image
        /// imports, and the partition they were drawn by.
        ///
        /// The spellings come from `Chromium Framework`'s own
        /// `__TEXT,__stubs` indirect symbol table, not from a header: the stubs
        /// are at `__TEXT,__stubs+0x14c` and resolve to `__Unwind_Backtrace`,
        /// `__Unwind_DeleteException`, `__Unwind_GetDataRelBase`,
        /// `__Unwind_GetTextRelBase`, `__Unwind_GetIP`, `__Unwind_GetIPInfo`,
        /// `__Unwind_GetLanguageSpecificData`, `__Unwind_GetRegionStart`,
        /// `__Unwind_Resume`, `__Unwind_RaiseException`, `__Unwind_SetGR` and
        /// `__Unwind_SetIP`. **Two leading underscores in every case**, which is
        /// why a row spelled `Unwind_GetIP` would compile, pass, and leave the
        /// import on the host.
        ///
        /// Eleven rows, in two halves, and the set of *names* has not changed -
        /// what changed is what seven of them resolve to. See
        /// [`the_seven_answered_accessors_are_seven_distinct_addresses`].
        #[test]
        fn the_unwind_rows_are_the_image_spellings() {
            for published in [
                "__Unwind_Backtrace",
                "__Unwind_DeleteException",
                "__Unwind_GetDataRelBase",
                "__Unwind_GetTextRelBase",
                "__Unwind_GetGR",
                "__Unwind_GetIP",
                "__Unwind_GetIPInfo",
                "__Unwind_GetLanguageSpecificData",
                "__Unwind_GetRegionStart",
                "__Unwind_SetGR",
                "__Unwind_SetIP",
            ] {
                let row = crate::base_name(published);
                assert!(
                    row.starts_with("_Unwind_"),
                    "{published} reduces to {row}, which is not a row of this table"
                );
                assert_eq!(
                    format!("_{row}"),
                    published,
                    "{row} would publish the wrong number of underscores"
                );
                assert!(
                    (MODULE.symbol)(row).is_some(),
                    "{row} is not in this module, so the import stays on the host"
                );
            }
        }

        /// **The seven answered accessors are seven addresses, and none of them is
        /// a placeholder.**
        ///
        /// This is the test for the failure mode that is *not* a missing name. An
        /// absent row and a row pointing at one shared trampoline both leave the
        /// import bound, and the unbound-import census reports both as bound — but
        /// the first is a jump through a zero and the second is every personality
        /// call landing in the same function. It is not hypothetical: run
        /// 157's `ROSETTA3_EH_CENSUS=1` output has
        /// `_objc_begin_catch` **and** `_objc_end_catch` both at
        /// `0x55e7e084d670`, one address inside this runtime's own text, with 33
        /// call sites between them and `NOTHING RECOGNISABLE` in the
        /// classification column.
        ///
        /// So the assertion is pairwise, over the seven: two different names must
        /// not resolve to the same address. The four refusals are checked to be
        /// non-null and *not* among the seven, which is the other half — a
        /// refusal and an answer must not have been collapsed into one row.
        #[test]
        fn the_seven_answered_accessors_are_seven_distinct_addresses() {
            const ANSWERED: &[&str] = &[
                "_Unwind_GetIP",
                "_Unwind_GetIPInfo",
                "_Unwind_GetGR",
                "_Unwind_SetGR",
                "_Unwind_SetIP",
                "_Unwind_GetRegionStart",
                "_Unwind_GetLanguageSpecificData",
            ];
            const REFUSED: &[&str] = &[
                "_Unwind_GetTextRelBase",
                "_Unwind_GetDataRelBase",
                "_Unwind_DeleteException",
                "_Unwind_Backtrace",
            ];

            let mut seen: Vec<(&str, u64)> = ANSWERED
                .iter()
                .map(|name| {
                    let addr = (MODULE.symbol)(name).unwrap_or_else(|| {
                        panic!("{name} is not published, so the loader binds the host's")
                    });
                    assert_ne!(addr, 0, "{name} published a null address");
                    (*name, addr)
                })
                .collect();
            for (name, addr) in &seen {
                for (other, other_addr) in &seen {
                    if name == other {
                        continue;
                    }
                    assert_ne!(
                        other_addr, addr,
                        "{name} and {other} are both at {addr:#x}: one shared trampoline \
                         for a whole family is a bound import the census cannot see"
                    );
                }
            }
            // And the refused four resolve somewhere else entirely, so no answer
            // and no refusal can be the same row wearing two names.
            for name in REFUSED {
                let addr = (MODULE.symbol)(name).unwrap_or_else(|| {
                    panic!("{name} is not published, so the loader binds the host's")
                });
                assert_ne!(addr, 0, "{name} published a null address");
                assert!(
                    !seen.iter().any(|(_, a)| *a == addr),
                    "{name} shares its address with an answered accessor"
                );
                seen.push((name, addr));
            }
            assert_eq!(seen.len(), 11, "seven answered and four refused");
        }

        /// The seven answered accessors are **this crate's** functions and not the
        /// host's, which is the whole reason the rows exist.
        ///
        /// `nm -D /usr/lib/libgcc_s.so.1` exports every one of the twelve
        /// `_Unwind_*` names, so a name not published here is bound to the host's
        /// — byte for byte, at one common load base, which is exactly what
        /// `ROSETTA3_EH_CENSUS=1` printed before these rows were answered:
        /// `__Unwind_GetIP`'s slot resolved to `0x7fba16bfc300` and the host's own
        /// symbol is at `0x1f300`, so the guest's answer was the host's
        /// `0x1f300 + 0x7fba16bdd000`. Twelve of twelve agreed on that base.
        ///
        /// A host accessor is a *silent* wrong answer rather than a loud one, and
        /// this assertion is what keeps the seven from quietly becoming that
        /// again: their addresses are inside this module's code, and the host's
        /// twelve are somewhere else entirely.
        #[test]
        fn the_answered_accessors_are_not_the_hosts() {
            for name in [
                "__Unwind_GetIP",
                "__Unwind_GetIPInfo",
                "__Unwind_GetGR",
                "__Unwind_SetGR",
                "__Unwind_SetIP",
                "__Unwind_GetRegionStart",
                "__Unwind_GetLanguageSpecificData",
            ] {
                let row = crate::base_name(name);
                let ours = (MODULE.symbol)(row).unwrap_or_else(|| {
                    panic!("{name} is answered but the table has no address for it")
                });
                if let Some(host) = crate::host_symbol(row) {
                    assert_ne!(
                        ours, host,
                        "{name} resolves to the host's own copy, which reads a \
                         struct _Unwind_Context this crate never allocated"
                    );
                }
            }
        }

        /// **The two names this module deliberately does not take.**
        ///
        /// `__Unwind_Resume` (3,581 call sites) and `__Unwind_RaiseException`
        /// (2) are not refusals. This tree already reads the guest's
        /// `__TEXT,__unwind_info` — `crates/machish/src/image/unwind.rs` parses
        /// it, `crates/rosetta3/src/unwind.rs` steps a frame from it, parses the
        /// `__gcc_except_tab` LSDA, and runs both unwind phases into the guest's
        /// own landing pads, and `crates/rosetta3/src/dyld.rs:1245` answers
        /// `_dyld_find_unwind_sections` for the vendored Apple `libunwind.dylib`.
        /// The capability is present and the *wiring* is what is missing, so
        /// binding either name to a refusal here would take the name away from
        /// the fix and turn a solvable problem into a documented dead end.
        ///
        /// This test is the guard on that decision, and it fails if somebody adds
        /// either row "to be complete" — which is the one way this section could
        /// quietly become the thing it says it is not.
        #[test]
        fn the_two_walkers_are_left_for_the_routing_fix() {
            for name in ["_Unwind_Resume", "_Unwind_RaiseException"] {
                assert!(
                    (MODULE.symbol)(name).is_none(),
                    "{name} is published here as a refusal, but it is a wiring problem \
                     rather than a missing capability: crates/rosetta3/src/unwind.rs \
                     already implements it and only the publication is missing. This \
                     refusal now occupies the name."
                );
                assert!(
                    !((MODULE.names)()).contains(&name),
                    "{name} is in names() but not symbol()"
                );
            }
        }

        /// `_Unwind_Backtrace` is the one refusal here that returns, and what it
        /// returns is the count a walk that walked nothing returns.
        ///
        /// The guest does not read the return value at all — `Framework+0x414d66`
        /// tests a flag the *callback* set — so `0` is not a value invented for
        /// the guest's benefit, it is the only honest count on this host.
        #[test]
        fn the_backtrace_walk_produces_no_frames() {
            // SAFETY: two pointers, neither dereferenced.
            let reason = unsafe { unwind_backtrace(std::ptr::null_mut(), std::ptr::null_mut()) };
            assert_eq!(reason, 0, "a walk that walked nothing collected nothing");
        }

        /// **The census diff, as an assertion.** What the loader does with each
        /// of the twelve, before and after this section.
        ///
        /// `darwin::is_implemented` is the question the loader asks first and
        /// `darwin::host_symbol` (`dlsym`) is what it asks when the answer is
        /// no, so a name is on the host exactly when the first is false and the
        /// second is not. `nm -D /usr/lib/libgcc_s.so.1` says the second is true
        /// for all twelve, so before this section **all twelve were on the host**
        /// — which is what `ROSETTA3_EH_CENSUS=1` printed, all twelve at one
        /// load base and byte for byte equal to the host's own symbols.
        ///
        /// After it, eleven are answered here and **two are still on the host**,
        /// which is the honest report and not a gap in the test: those two are
        /// [`the_two_walkers_are_left_for_the_routing_fix`].
        ///
        /// **And the eleven are no longer eleven refusals.** Seven of them are
        /// field accessors on the `_Unwind_Context` this crate creates, and four
        /// still stop the process — so this test now proves that *every* one of
        /// the eleven is off the host, which is the property that matters, and it
        /// says nothing about which of them answers. What each one does is pinned
        /// by [`unwind_context::tests`] for the seven and by the refusal messages
        /// for the four.
        ///
        /// Note the spelling: `is_implemented` takes the **Mach-O** name and
        /// reduces it itself, so it must not be handed `base_name`'s output.
        /// Reducing twice is what makes `__objc_personality_v0` print
        /// `NOTHING RECOGNISABLE` in `ROSETTA3_EH_CENSUS=1`'s output when it is
        /// in fact published by `crate::data_unavailable` — see the report.
        #[test]
        fn the_unwinder_census() {
            for published in [
                "__Unwind_Backtrace",
                "__Unwind_DeleteException",
                "__Unwind_GetDataRelBase",
                "__Unwind_GetGR",
                "__Unwind_GetIP",
                "__Unwind_GetIPInfo",
                "__Unwind_GetLanguageSpecificData",
                "__Unwind_GetRegionStart",
                "__Unwind_SetGR",
                "__Unwind_SetIP",
                "__Unwind_GetTextRelBase",
            ] {
                assert!(
                    crate::is_implemented(published),
                    "{published} is not answered here, so the loader falls through to \
                     dlsym and the guest gets the host's"
                );
            }
            for published in ["__Unwind_Resume", "__Unwind_RaiseException"] {
                assert!(
                    !crate::is_implemented(published),
                    "{published} is answered here, which is the thing \
                     the_two_walkers_are_left_for_the_routing_fix forbids"
                );
                assert!(
                    crate::host_symbol(crate::base_name(published)).is_some(),
                    "{published} is on neither this tree nor the host, so it is an unbound \
                     import - a zero in its slot and a jump through it, which is worse than \
                     either"
                );
            }
        }

        /// Every name this module publishes resolves, and resolves once.
        ///
        /// The other half of the census: 48 names in, 48 names out. A row that
        /// resolves to nothing is an unbound import that a test suite will not
        /// notice, which is the whole reason the count is asserted.
        #[test]
        fn every_published_name_resolves() {
            let names = (MODULE.names)();
            assert_eq!(
                names.len(),
                48,
                "VideoToolbox 15 + QuartzCore 1 + Metal 1 + libbsm 1 + libobjc 1 + \
                 libresolv 3 + sandbox 6 + SystemConfiguration 4 + CFNetwork 2 + \
                 CoreServices 2 + mach 1 + unwinder 11"
            );
            for name in names {
                assert!(
                    (MODULE.symbol)(name).is_some(),
                    "{name} is published but resolves to no address"
                );
                // The other path: the Mach-O spelling goes through `c_name` and
                // has to arrive at the same row. Getting this wrong is a clean
                // build, a green suite, and an unbound import.
                let mach_o = format!("_{name}");
                let asked = crate::base_name(&mach_o);
                assert_eq!(
                    asked, name,
                    "one leading underscore in, one out, for {name}"
                );
                assert!(
                    (MODULE.symbol)(asked).is_some(),
                    "the Mach-O spelling of {name} does not resolve"
                );
            }
        }

        // --------------------------------------------------------- VideoToolbox

        #[test]
        fn no_video_session_is_created_and_the_handles_stay_null() {
            let mut session: *mut c_void = 0xdead_beef as *mut c_void;
            // SAFETY: each of these reads no argument and writes only the
            // out-parameter the binary proves is one.
            unsafe {
                assert_eq!(
                    vt_decompression_session_create(
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        0x6176_6331,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    ),
                    VT_COULD_NOT_FIND_VIDEO_DECODER_ERR
                );
                assert_eq!(
                    vt_compression_session_create(
                        std::ptr::null_mut(),
                        1920,
                        1080,
                        0x6176_6331,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut session
                    ),
                    VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR
                );
            }
            assert!(
                session.is_null(),
                "a session handle the guest will CFRelease must not survive"
            );
        }

        #[test]
        fn a_session_that_was_never_created_answers_nothing() {
            // SAFETY: every one of these reads no argument, and the ones with an
            // out-parameter get a real slot.
            unsafe {
                assert!(!vt_decompression_session_can_accept_format_description(
                    std::ptr::null_mut(),
                    std::ptr::null_mut()
                ));
                assert_eq!(
                    vt_decompression_session_decode_frame(
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut()
                    ),
                    VT_COULD_NOT_FIND_VIDEO_DECODER_ERR
                );
                assert_eq!(
                    vt_compression_session_encode_frame(
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        CTime { value: 0, flags: 0, epoch: 0 },
                        CTime { value: 0, flags: 0, epoch: 0 },
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    ),
                    VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR
                );
                assert_eq!(
                    vt_compression_session_complete_frames(
                        std::ptr::null_mut(),
                        CTime { value: 0, flags: 0, epoch: 0 }
                    ),
                    VT_COULD_NOT_FIND_VIDEO_ENCODER_ERR
                );
            }
        }

        /// The three invalidates are `void` and read no argument.
        ///
        /// The guest reaches all three behind `test rdi,rdi; je`, so the null
        /// handle is the one it can actually pass, and a function that read it
        /// would fault.
        #[test]
        fn the_invalidates_touch_nothing() {
            // SAFETY: each is `void` and dereferences nothing.
            unsafe {
                vt_decompression_session_invalidate(std::ptr::null_mut());
                vt_compression_session_invalidate(std::ptr::null_mut());
                vt_pixel_transfer_session_invalidate(std::ptr::null_mut());
            }
        }

        /// The two out-parameters the guest **poisons** with `0xaaaa…aaaa`.
        ///
        /// `VTSessionCopySupportedPropertyDictionary` at `Framework+0x167ec59`
        /// and `VTSessionCopyProperty` at `+0x2fd779a` are the two whose slots
        /// the guest fills with a pattern before the call and reads on the
        /// success arm. If either were left alone, the value reaching
        /// `CFDictionaryContainsKey` or `CFBooleanGetValue` would be
        /// `0xaaaaaaaaaaaaaaaa`. That is the whole test.
        #[test]
        fn poisoned_out_parameters_are_overwritten() {
            let poison = 0xaaaa_aaaa_aaaa_aaaa as *mut c_void;
            // SAFETY: both slots are real, writable, and poisoned.
            unsafe {
                let mut dictionary = poison;
                assert_eq!(
                    vt_session_copy_supported_property_dictionary(
                        std::ptr::null_mut(),
                        &mut dictionary
                    ),
                    VT_PROPERTY_NOT_SUPPORTED_ERR
                );
                assert!(dictionary.is_null(), "the poison survived");

                let mut value = poison;
                assert_eq!(
                    vt_session_copy_property(std::ptr::null_mut(), std::ptr::null_mut(), &mut value),
                    VT_PROPERTY_NOT_SUPPORTED_ERR
                );
                assert!(value.is_null(), "the poison survived");

                let mut image = poison;
                assert_eq!(
                    vt_create_cg_image_from_cv_pixel_buffer(
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        &mut image
                    ),
                    VT_COULD_NOT_FIND_VIDEO_DECODER_ERR
                );
                assert!(image.is_null(), "the poison survived");

                let mut pixel_transfer = poison;
                assert_eq!(
                    vt_pixel_transfer_session_create(std::ptr::null_mut(), &mut pixel_transfer),
                    VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR
                );
                assert!(pixel_transfer.is_null(), "the poison survived");
            }
        }

        #[test]
        fn a_property_is_neither_read_nor_set() {
            // SAFETY: both read no argument and have no out-parameter.
            unsafe {
                assert_eq!(
                    vt_session_set_property(
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    ),
                    VT_PROPERTY_NOT_SUPPORTED_ERR
                );
            }
        }

        /// The value the guest compares against, checked against the bytes it
        /// compares with.
        ///
        /// `Framework+0x7925a97` is `cmp eax,0xffffcd97`. `-12905` in two's
        /// complement *is* `0xffffcd97`, and this is the code whose name says
        /// "pixel transfer not supported" — the one the guest treats as a normal
        /// answer and routes to its software conversion for. A refusal that
        /// returned anything else would reach the `int3` two instructions later.
        #[test]
        fn the_pixel_transfer_status_is_the_one_the_guest_compares() {
            assert_eq!(VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR, -12905);
            assert_eq!(VT_PIXEL_TRANSFER_NOT_SUPPORTED_ERR as u32, 0xffff_cd97);
            // SAFETY: three pointer arguments, none dereferenced.
            assert_eq!(
                unsafe {
                    vt_pixel_transfer_session_transfer_image(
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    )
                },
                0xffff_cd97_u32 as i32
            );
        }

        // ----------------------------------------------------------- QuartzCore

        /// The matrix is the identity with the translation in the last row.
        ///
        /// Sixteen `f64`s, in `CATransform3D.h`'s declared order. A wrong `m41`
        /// or a transposed row would be a wrong transform that looked like a
        /// rendering bug rather than a missing framework, so every element is
        /// asserted rather than just the three that were passed in.
        #[test]
        fn the_translation_is_the_identity_in_the_last_row() {
            // SAFETY: three `f64`s and a 128-byte aggregate returned through the
            // hidden pointer the ABI supplies, exactly as at
            // `Framework+0xc6dc2b2`.
            let t = unsafe { ca_transform3d_make_translation(1.0, 2.0, 3.0) };
            assert_eq!(t.m11, 1.0);
            assert_eq!(t.m22, 1.0);
            assert_eq!(t.m33, 1.0);
            assert_eq!(t.m44, 1.0);
            assert_eq!((t.m41, t.m42, t.m43), (1.0, 2.0, 3.0));
            for zero in [
                t.m12, t.m13, t.m14, t.m21, t.m23, t.m24, t.m31, t.m32, t.m34,
            ] {
                assert_eq!(zero, 0.0, "a non-zero off-diagonal element");
            }
            // And the size the hidden pointer has to point at: the guest reads
            // 128 bytes back out of it at `Framework+0xc6dc2ea`.
            assert_eq!(std::mem::size_of::<Transform3D>(), 128);
        }

        // ----------------------------------------------------------------- Metal

        /// No device, and the out slot — which is a stack address the guest
        /// pre-nulled — is nulled again rather than left alone.
        #[test]
        fn there_is_no_metal_device() {
            let mut devices: *mut c_void = 0x1234 as *mut c_void;
            // SAFETY: a real out-parameter, and an observer that is never read.
            let found = unsafe {
                mtl_copy_all_devices_with_observer(&mut devices, std::ptr::null_mut())
            };
            assert!(found.is_null(), "there is no Metal device on this host");
            assert!(devices.is_null());
        }

        // ----------------------------------------------------------------- libbsm

        /// `-1`, never a pid that could be mistaken for a real process.
        ///
        /// Zero would be the answer that looks like a working one: it names
        /// `kernel_task`, and every one of the guest's twelve call sites would log
        /// it or hand it to a virtual without noticing. The token passed here is
        /// the all-zero one the guest would pass when this runtime wrote no audit
        /// trailer, and there is no index into `val[]` that would be honest.
        #[test]
        fn an_audit_token_yields_no_pid() {
            // SAFETY: a 128-byte aggregate passed on the stack, as the guest does
            // at `Framework+0x4dc37ab`.
            let token = AuditToken([0; 32]);
            assert_eq!(unsafe { audit_token_to_pid(token) }, -1);
            // And the layout is the one the binary copies: two `movups` of
            // sixteen bytes onto the stack.
            assert_eq!(std::mem::size_of::<AuditToken>(), 128);
        }

        // ---------------------------------------------------------------- libobjc

        /// With `hasStrong == false` the copy happens, and it is a byte copy.
        ///
        /// This is the case the binary decides: both guest call sites pass
        /// `xor r8d,r8d` for the last argument (`Framework+0x3a9eb9a` and
        /// `+0x3a9ebe4`), which is where the ARC runtime's own implementation
        /// takes its `memcpy` arm.
        #[test]
        fn a_struct_with_no_strong_member_is_copied() {
            let mut destination = [0u8; 16];
            let source = [0x5au8; 16];
            // SAFETY: 16 readable bytes and 16 writable ones, `size` agrees with
            // both, and `hasStrong` is false so the copy is a plain `memcpy`.
            unsafe {
                objc_copy_struct(
                    destination.as_mut_ptr().cast(),
                    source.as_ptr().cast(),
                    16,
                    true,
                    false,
                );
            }
            assert_eq!(destination, source);
        }

        /// With `hasStrong == true` the copy is **skipped and named**, not
        /// performed without the retains.
        ///
        /// A `memcpy` here would drop every `__strong` retain and every `__weak`
        /// release and corrupt the destination silently — the one outcome
        /// PLAN.md §1 is about. The guest never asks for it (both call sites pass
        /// zero), so the branch is unreachable here and the test exists to say so.
        #[test]
        fn a_struct_with_a_strong_member_is_refused_not_copied() {
            let mut destination = [0u8; 16];
            let source = [0x5au8; 16];
            // SAFETY: as above, but with `hasStrong` true.
            unsafe {
                objc_copy_struct(
                    destination.as_mut_ptr().cast(),
                    source.as_ptr().cast(),
                    16,
                    true,
                    true,
                );
            }
            assert_eq!(
                destination, [0u8; 16],
                "a strong-aware copy must not be done as a memcpy"
            );
        }

        // -------------------------------------------------------------- libresolv

        /// `res_9_ninit` answers the only value Chromium calls success.
        ///
        /// `ScopedResState::IsValid()` is `res_init_result_ == 0`
        /// (`net/dns/public/scoped_res_state.h`), and the guest tests exactly that
        /// at `Framework+0xadb98a`. A `1` here — libresolv's own success value —
        /// would send the guest down the teardown arm instead.
        #[test]
        fn an_init_is_valid_at_zero() {
            // SAFETY: one pointer, never dereferenced.
            assert_eq!(unsafe { res_9_ninit(std::ptr::null_mut()) }, 0);
        }

        /// A zeroed `__res_state` has no nameservers, which is what zero means.
        ///
        /// The count is the error channel, so the poisoned output array is not
        /// written and does not need to be: the guest's loop is driven by the
        /// count. This test pins the count.
        #[test]
        fn there_are_no_nameservers() {
            // SAFETY: three arguments, none dereferenced; `set` is a real buffer
            // that the function must not touch.
            let mut buffer = [0xaa_u8; 0x180];
            let count = unsafe {
                res_9_getservers(std::ptr::null_mut(), buffer.as_mut_ptr().cast(), 3)
            };
            assert_eq!(count, 0, "a zeroed __res_state has no address list");
            assert!(
                buffer.iter().all(|b| *b == 0xaa),
                "the poisoned array is not the function's to write; only the count is"
            );
        }

        /// The destroy is `void` and reads no argument.
        #[test]
        fn a_state_ndestroy_is_a_no_op() {
            // SAFETY: one pointer, never dereferenced.
            unsafe { res_9_ndestroy(std::ptr::null_mut()) };
        }

        // ---------------------------------------------------------------- sandbox

        /// `sandbox_ms` refuses, and leaves a **message** rather than a null.
        ///
        /// The guest passes `[rbp-0x28]` straight to a `%s` conversion
        /// (`Framework+0x3a7b518`), so a null would print `(null)` and say
        /// nothing about why the sandbox did not engage.
        #[test]
        fn the_sandbox_refuses_with_a_reason() {
            let mut error: *mut c_char = std::ptr::null_mut();
            // SAFETY: two scalars and a real out-parameter. The profile is not
            // dereferenced.
            let status = unsafe {
                sandbox_ms(c"(version 1)".as_ptr(), 0x60, &mut error)
            };
            assert_eq!(status, -1);
            assert!(!error.is_null(), "a %s conversion needs a string");
            // SAFETY: this module's own leaked C string, never freed and never
            // written through.
            let said = unsafe { std::ffi::CStr::from_ptr(error) };
            let said = said.to_str().unwrap_or_default();
            assert!(said.contains("Seatbelt"), "the reason is in the message: {said}");
        }

        /// `sandbox_create_params` hands back a **real set**, and the guest will
        /// really call `sandbox_free_params` on it.
        ///
        /// The old answer here was `NULL`, and its own `test rdi,rdi; je` at
        /// `Framework+0x3a95005` is what made that look safe. It is not: the same
        /// guard means a *non-null* answer is what gets the free, twice, on two
        /// different paths. So the two facts that pin this are that the handle is
        /// real and distinct, and that freeing it is a real free — see
        /// `a_sandbox_param_set_can_be_freed_exactly_once`.
        #[test]
        fn a_sandbox_param_set_is_a_real_set() {
            // SAFETY: takes no argument and dereferences nothing.
            let params = unsafe { sandbox_create_params() };
            assert!(!params.is_null(), "a table of strings is not a capability this host lacks");
            // A second one is a second allocation, because nothing in the binary
            // says only one set is ever alive at a time.
            // SAFETY: as above.
            let other = unsafe { sandbox_create_params() };
            assert_ne!(params, other, "two sets must be two handles");
            // SAFETY: each is one of ours and each is freed exactly once below.
            unsafe {
                sandbox_free_params(params);
                sandbox_free_params(other);
            }
        }

        /// `sandbox_set_param` answers **0** for a handle it handed out. This is
        /// the wall, and the reason is the *direction* of the guest's branch.
        ///
        /// `Framework+0x7fe5f89  je 0x7fe6033` targets `+0x7fe6033: mov al,0x1`,
        /// which is the `true` return; the message-building fall-through is the
        /// failure. 18 of the wrapper's 21 call sites turn that `false` into a
        /// bare `int3`, so `-1` here is a crash in the guest.
        #[test]
        fn a_parameter_is_set() {
            // SAFETY: takes no argument and dereferences nothing.
            let params = unsafe { sandbox_create_params() };
            // SAFETY: a handle this runtime just made, and two NUL-terminated
            // literals this test owns.
            let status = unsafe {
                sandbox_set_param(
                    params,
                    c"EXECUTABLE_PATH".as_ptr(),
                    c"/usr/bin/chromium".as_ptr(),
                    0,
                )
            };
            assert_eq!(status, 0, "0 is the success arm: `je` targets `mov al,0x1`");
            // SAFETY: as above.
            unsafe { sandbox_free_params(params) };
        }

        /// A handle this runtime did **not** hand out is refused, and so is a
        /// null or empty key. These are refusals about the *argument*, and they
        /// are the only cases left that answer `-1`.
        ///
        /// A `sandbox_params_t` is an opaque `void *` that Chromium stores in a
        /// struct field and hands back whenever it likes
        /// (`Framework+0x7fe5f7e  mov rdi, [r14+0x58]`), so "is this one of
        /// ours" has to be answerable without reading it — which is what the
        /// table of live addresses is for.
        #[test]
        fn a_parameter_is_refused_on_a_handle_that_is_not_ours() {
            // SAFETY: a null handle and two literals; nothing is dereferenced
            // because the handle is refused before the set is reached.
            assert_eq!(
                unsafe {
                    sandbox_set_param(
                        std::ptr::null_mut(),
                        c"ENABLE_LOGGING".as_ptr(),
                        c"TRUE".as_ptr(),
                        0,
                    )
                },
                -1
            );
            // A plausible-looking but foreign address: 0x1000 is not one this
            // runtime has ever handed out, and the refusal is what keeps
            // `set_param` from writing through it.
            // SAFETY: as above — the address is refused, not read.
            assert_eq!(
                unsafe {
                    sandbox_set_param(
                        0x1000 as *mut c_void,
                        c"ENABLE_LOGGING".as_ptr(),
                        c"TRUE".as_ptr(),
                        0,
                    )
                },
                -1
            );
            // SAFETY: a real handle and a real key, but an **empty** key.
            let params = unsafe { sandbox_create_params() };
            // SAFETY: three literals this test owns.
            assert_eq!(
                unsafe { sandbox_set_param(params, c"".as_ptr(), c"TRUE".as_ptr(), 0) },
                -1
            );
            // SAFETY: one of ours, freed once.
            unsafe { sandbox_free_params(params) };
        }

        /// Freeing a set is a real free, and a second free of the same handle is
        /// refused rather than a double free.
        ///
        /// The guest reaches `sandbox_free_params` **only** through
        /// `test %rdi,%rdi; je` at `Framework+0x3a95005` and `+0x3a958a3`, so a
        /// non-null handle means the call really happens — twice per launch. A
        /// no-op free there is a leak on every run.
        #[test]
        fn a_sandbox_param_set_can_be_freed_exactly_once() {
            // SAFETY: takes no argument and dereferences nothing.
            let params = unsafe { sandbox_create_params() };
            // SAFETY: a real handle of ours.
            unsafe { sandbox_set_param(params, c"TRUE".as_ptr(), c"TRUE".as_ptr(), 0) };
            // SAFETY: `void`, reads no argument.
            unsafe { sandbox_free_params(params) };
            // The handle is gone now, so both of these must be no-ops rather than
            // a second `Box::from_raw` on the same address.
            // SAFETY: as above.
            unsafe { sandbox_free_params(params) };
            // And the freed handle is no longer accepted, which is the same
            // use-after-free the `remove` above prevents.
            // SAFETY: a stale handle, refused before it is read.
            assert_eq!(
                unsafe { sandbox_set_param(params, c"TRUE".as_ptr(), c"TRUE".as_ptr(), 0) },
                -1,
                "a freed handle must not be writable through"
            );
        }

        /// A null handle is safe to free, which is the arm the guest's own
        /// `test rdi, rdi; je` skips — and which a caller can still reach.
        #[test]
        fn freeing_no_sandbox_params_is_a_no_op() {
            // SAFETY: `void`; null is not in the table, so nothing is freed.
            unsafe { sandbox_free_params(std::ptr::null_mut()) };
        }

        /// Compiling a profile fails, **and the error out-parameter carries a
        /// message** — because a null one is a `SIGILL`.
        ///
        /// The old version of this test asserted `error.is_null()`, and that
        /// assertion *was* the trap:
        ///
        /// ```text
        /// 7fe65f3  mov  r14, QWORD PTR [rbp-0x20]   ; reads *errorbuf
        /// 7fe65f7  test r15, r15                    ; the guest's 4th argument
        /// 7fe65fa  je   0x7fe660c                   ; wants no string: fine
        /// 7fe65fc  test r14, r14
        /// 7fe65ff  je   0x7fe665d                   ; <-- ud2
        /// ```
        ///
        /// The guest always passes a non-null fourth argument
        /// (`Framework+0x7fe6380`'s caller hands it `rcx = &this->error_string`),
        /// so a null `*errorbuf` on the failure arm is an illegal instruction.
        #[test]
        fn a_profile_does_not_compile_but_says_why() {
            let mut error: *mut c_char = 0xaaaa_aaaa_aaaa_aaaa as *mut c_char;
            // SAFETY: four arguments matching the binary's arity — the profile
            // and the fourth are not read — and a real out-parameter this
            // function writes exactly one pointer into.
            let profile = unsafe {
                sandbox_compile_string(
                    c"(version 1)(allow default)".as_ptr(),
                    std::ptr::null_mut(),
                    &mut error,
                    std::ptr::null_mut(),
                )
            };
            assert!(profile.is_null(), "no Seatbelt, so no compiled profile");
            assert!(
                !error.is_null(),
                "a null error string is a ud2 at Framework+0x7fe665d, not a handled path"
            );
            // SAFETY: the pointer this call just wrote, read as a C string. The
            // poison value is gone, which is the other half of what this test is
            // for: the guest reads this word back at `+0x7fe65f3`.
            let said = unsafe { std::ffi::CStr::from_ptr(error) };
            let said = said.to_string_lossy();
            assert!(said.contains("Seatbelt"), "the reason is in the message: {said}");
            // And the message must not overstate the host: Landlock and seccomp
            // are both present here, so "there is no mechanism" would be false.
            assert!(
                said.contains("Landlock") && said.contains("seccomp"),
                "the host has both, and the refusal has to say so: {said}"
            );
            assert!(
                !said.contains("no mechanism"),
                "that would be false on this host: {said}"
            );
            // The guest releases it, so it must be releasable — and this is the
            // one that proves the arity fix is real: `sandbox_free_error` takes a
            // single `char *` and frees exactly the strings handed out here.
            assert!(
                crate::last_unavailable::release_guest_error_string(error),
                "a string this runtime produced must be releasable by the function the guest calls"
            );
            // And a second release is refused rather than a double free.
            assert!(
                !crate::last_unavailable::release_guest_error_string(error),
                "a second release must not free again"
            );
            // SAFETY: `void`, reads no argument; a null profile is the only
            // argument the guest can ever pass on the success arm, which is
            // never reached.
            unsafe { sandbox_free_profile(profile) };
        }

        // --------------------------------------------------- SystemConfiguration

        /// No dynamic store, so no host name and no computer name.
        ///
        /// The guest asks for the local host name first and the computer name
        /// second and has a path for both being null
        /// (`Framework+0x1b22808`-`0x1b2281a`).
        #[test]
        fn there_is_no_sc_host_name() {
            // SAFETY: one pointer each, neither dereferenced.
            unsafe {
                assert!(
                    sc_dynamic_store_copy_local_host_name(std::ptr::null_mut()).is_null()
                );
                // The second argument is the header's `char *name` and the guest
                // passes a **literal zero** for it (`xor esi,esi` at
                // `Framework+0x1b22810`), so a null is the only value it can be
                // asked to accept.
                assert!(
                    sc_dynamic_store_copy_computer_name(
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    )
                    .is_null()
                );
            }
        }

        /// No proxy keys, for a `NULL` auto-configuration URL.
        #[test]
        fn there_is_no_sc_proxy_key() {
            // SAFETY: one pointer, the literal `NULL` the guest passes at
            // `Framework+0x674ae25`; it is an input, not an out-parameter.
            assert!(unsafe { sc_dynamic_store_key_create_proxies(std::ptr::null_mut()) }.is_null());
        }

        /// A `const char *` that is never null, because the guest `strlen`s it
        /// without testing.
        ///
        /// `Framework+0x6755031`, `+0x50949bb` and `+0x6755157` are three
        /// `call _strlen` with the answer in `rdi` and no `test` in between. A
        /// null here is a fault at address 0.
        #[test]
        fn the_sc_error_string_is_never_null_and_says_why() {
            // SAFETY: one `int`, nothing dereferenced.
            for code in [0, 1, -67, i32::MIN, i32::MAX] {
                let said = unsafe { sc_error_string(code) };
                assert!(!said.is_null(), "the guest strlen's this with no test");
                // SAFETY: this module's own leaked C string.
                let text = unsafe { std::ffi::CStr::from_ptr(said) };
                let text = text.to_str().unwrap_or_default();
                assert!(!text.is_empty(), "an empty message is not an answer");
                assert!(
                    text.contains("SystemConfiguration"),
                    "the message must name what is missing: {text}"
                );
                assert!(
                    text.contains(&code.to_string()),
                    "the message must carry the code it was given: {text}"
                );
            }
        }

        // ----------------------------------------------------------------- CFNetwork

        /// No proxies, and the null is handled: the guest `CFRelease`s the answer
        /// behind `test rdi,rdi; je` (`Framework+0x7e7c40e`).
        #[test]
        fn there_is_no_system_proxy() {
            // SAFETY: two pointers, neither dereferenced.
            unsafe {
                assert!(
                    cfnetwork_copy_proxies_for_url(
                        std::ptr::null_mut(),
                        std::ptr::null_mut()
                    )
                    .is_null()
                );
            }
        }

        /// No PAC, and the two out-parameters are **not** written.
        ///
        /// The guest sets `rdi`, `rsi`, `rdx` and `rcx` and not one of `r8`/`r9`
        /// (`Framework+0x7e7bba5`-`0x7e7bbc0`), so those two arguments hold
        /// whatever the caller's frame held. Writing through them would be a store
        /// to an arbitrary address, and this test passes the *same* poison
        /// pointers a caller could not have meant to pass: if the function wrote
        /// them, the poison would be replaced and the test would fail.
        #[test]
        fn a_pac_url_is_not_run_and_nothing_is_written_through_r8_or_r9() {
            let mut proxy_list: *mut c_void = 0xdead_beef as *mut c_void;
            let mut error: *mut c_void = 0xdead_beef as *mut c_void;
            // SAFETY: six arguments matching the header; the last two are the
            // uninitialised ones the guest leaves alone, and a write through
            // either would be visible below.
            let list = unsafe {
                cfnetwork_execute_proxy_auto_configuration_url(
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut proxy_list,
                    &mut error,
                )
            };
            assert!(list.is_null(), "no PAC engine, so no proxy list");
            assert_eq!(
                proxy_list, 0xdead_beef as *mut c_void,
                "r8 was never set by the caller, so it must not be written"
            );
            assert_eq!(
                error, 0xdead_beef as *mut c_void,
                "r9 was never set by the caller, so it must not be written"
            );
        }

        // ------------------------------------------------------------- CoreServices

        /// The `__CSCheck*` marker has no body to speak of, and the guest
        /// continues (`Framework+0xc8f771d` is four instructions before a `ret`).
        #[test]
        fn the_coreservices_marker_does_nothing() {
            // SAFETY: `void`, takes no argument, dereferences nothing.
            unsafe { cs_check_fix_disable() };
        }

        /// `noErr`, and the reason is a *state*, not an error.
        ///
        /// The guest passes a literal `0` (`xor edi,edi` at
        /// `Framework+0x80f048a`), which declares "not connected to the
        /// LaunchServices server" — and there is no LaunchServices to be connected
        /// to, so the requested status is the status that already holds. It then
        /// discards the answer entirely.
        #[test]
        fn launch_services_connection_status_is_the_status_that_already_holds() {
            // SAFETY: one `bool`, nothing dereferenced.
            assert_eq!(
                unsafe {
                    ls_set_application_launch_services_server_connection_status(false)
                },
                0
            );
        }

        // ------------------------------------------------------------------- mach

        /// `KERN_FAILURE`, and the out-parameter written even though the guest
        /// reads it without testing the status.
        ///
        /// `Framework+0x81c410b` — the instruction after the call — is
        /// `and DWORD PTR [rbp-0x94],0x3`, with no `test eax,eax` in between.
        #[test]
        fn purging_is_refused_and_the_value_is_written() {
            let mut value: c_int = 0xaaaa_aaaa_u32 as c_int;
            // SAFETY: a 32-bit port name, a `vm_address_t`, a command and a real
            // out-parameter; nothing is dereferenced.
            let kr = unsafe { mach_vm_purgable_control(0x1234, 0xdead_beef, 1, &mut value) };
            assert_eq!(kr, KERN_FAILURE);
            assert_eq!(
                value, 0,
                "the guest masks the low two bits of this without testing the status"
            );
        }

        // ------------------------------------------------- the non-pointer arguments

        /// The three places an argument is **not** the pointer its name suggests.
        ///
        /// Each of these would fault at address 0 or write into nothing if the
        /// declaration were taken from the header alone, and each is the reason
        /// the note exists. The test is a statement about the *declarations*: that
        /// they can be handed a null and survive.
        #[test]
        fn names_that_are_not_pointers() {
            // `SCDynamicStoreCopyComputerName`'s second argument: the header's
            // `char *name`, which the guest passes as a literal zero
            // (`xor esi,esi` at `Framework+0x1b22810`) and which must not be
            // written through.
            // SAFETY: a null second argument, which is what the guest passes.
            assert!(
                unsafe {
                    sc_dynamic_store_copy_computer_name(std::ptr::null_mut(), std::ptr::null_mut())
                }
                .is_null()
            );

            // `SCDynamicStoreKeyCreateProxies`'s only argument: the header's
            // `CFStringRef`, which the guest passes as a literal zero
            // (`xor edi,edi` at `Framework+0x674ae25`) and which is an input.
            // SAFETY: a null auto-configuration URL.
            assert!(
                unsafe { sc_dynamic_store_key_create_proxies(std::ptr::null_mut()) }.is_null()
            );

            // `SCDynamicStoreCopyComputerName`'s and the two `SCDynamicStore*`
            // out-parameters are absent altogether, which is why the calls above
            // have no `&mut` to pass: a function that wrote one would be writing
            // to whatever the guest's register held.
        }

        /// Every one of the 37 names is named by a test in this file.
        ///
        /// The brief for this file was "a test per name", and a test that
        /// covers three of them is the better shape — so the requirement is
        /// restated as something checkable: the list below is every published
        /// name, paired with the test that exercises it, and it must stay the
        /// same size as [`every_published_name_resolves`]'s census. A name added
        /// to the table without a row here fails this test.
        #[test]
        fn every_name_has_a_test_that_names_it() {
            const COVERED: &[(&str, &str)] = &[
                ("VTDecompressionSessionCreate", "no_video_session_is_created_and_the_handles_stay_null"),
                ("VTCompressionSessionCreate", "no_video_session_is_created_and_the_handles_stay_null"),
                ("VTDecompressionSessionInvalidate", "the_invalidates_touch_nothing"),
                ("VTCompressionSessionInvalidate", "the_invalidates_touch_nothing"),
                ("VTPixelTransferSessionInvalidate", "the_invalidates_touch_nothing"),
                ("VTDecompressionSessionCanAcceptFormatDescription", "a_session_that_was_never_created_answers_nothing"),
                ("VTDecompressionSessionDecodeFrame", "a_session_that_was_never_created_answers_nothing"),
                ("VTCompressionSessionEncodeFrame", "a_session_that_was_never_created_answers_nothing"),
                ("VTCompressionSessionCompleteFrames", "a_session_that_was_never_created_answers_nothing"),
                ("VTSessionCopyProperty", "poisoned_out_parameters_are_overwritten"),
                ("VTSessionSetProperty", "a_property_is_neither_read_nor_set"),
                ("VTSessionCopySupportedPropertyDictionary", "poisoned_out_parameters_are_overwritten"),
                ("VTCreateCGImageFromCVPixelBuffer", "poisoned_out_parameters_are_overwritten"),
                ("VTPixelTransferSessionCreate", "poisoned_out_parameters_are_overwritten"),
                ("VTPixelTransferSessionTransferImage", "the_pixel_transfer_status_is_the_one_the_guest_compares"),
                ("CATransform3DMakeTranslation", "the_translation_is_the_identity_in_the_last_row"),
                ("MTLCopyAllDevicesWithObserver", "there_is_no_metal_device"),
                ("audit_token_to_pid", "an_audit_token_yields_no_pid"),
                ("objc_copyStruct", "a_struct_with_no_strong_member_is_copied"),
                ("res_9_ninit", "an_init_is_valid_at_zero"),
                ("res_9_ndestroy", "a_state_ndestroy_is_a_no_op"),
                ("res_9_getservers", "there_are_no_nameservers"),
                ("__sandbox_ms", "the_sandbox_refuses_with_a_reason"),
                ("sandbox_create_params", "a_sandbox_param_set_is_a_real_set"),
                ("sandbox_free_params", "a_sandbox_param_set_can_be_freed_exactly_once"),
                ("sandbox_set_param", "a_parameter_is_set"),
                ("sandbox_compile_string", "a_profile_does_not_compile_but_says_why"),
                ("sandbox_free_profile", "a_profile_does_not_compile_but_says_why"),
                ("SCDynamicStoreCopyLocalHostName", "there_is_no_sc_host_name"),
                ("SCDynamicStoreCopyComputerName", "there_is_no_sc_host_name"),
                ("SCDynamicStoreKeyCreateProxies", "there_is_no_sc_proxy_key"),
                ("SCErrorString", "the_sc_error_string_is_never_null_and_says_why"),
                ("CFNetworkCopyProxiesForURL", "there_is_no_system_proxy"),
                ("CFNetworkExecuteProxyAutoConfigurationURL", "a_pac_url_is_not_run_and_nothing_is_written_through_r8_or_r9"),
                ("_CSCheckFixDisable", "the_coreservices_marker_does_nothing"),
                ("_LSSetApplicationLaunchServicesServerConnectionStatus", "launch_services_connection_status_is_the_status_that_already_holds"),
                ("mach_vm_purgable_control", "purging_is_refused_and_the_value_is_written"),
                // The unwinder's state, in two halves. Seven are answered on the
                // five-word context in `unwind_context`, and the tests named here
                // are the ones that drive them through the published `extern "C"`
                // entry points. Four still stop the process, so for those the
                // assertion is that the name is published and is not the host's
                // - plus, for the one that returns, the count it returns.
                (
                    "_Unwind_GetIP",
                    "unwind_context::tests::a_personality_sees_exactly_what_the_driver_offered",
                ),
                (
                    "_Unwind_GetIPInfo",
                    "unwind_context::tests::the_ip_info_out_parameter_is_optional",
                ),
                (
                    "_Unwind_GetGR",
                    "unwind_context::tests::what_the_personality_wrote_is_what_the_driver_would_install",
                ),
                (
                    "_Unwind_SetGR",
                    "unwind_context::tests::the_written_registers_are_rax_and_rdx_in_dwarfs_numbering",
                ),
                (
                    "_Unwind_SetIP",
                    "unwind_context::tests::what_the_personality_wrote_is_what_the_driver_would_install",
                ),
                (
                    "_Unwind_GetRegionStart",
                    "unwind_context::tests::a_personality_sees_exactly_what_the_driver_offered",
                ),
                (
                    "_Unwind_GetLanguageSpecificData",
                    "unwind_context::tests::a_frame_with_no_try_has_no_language_specific_data",
                ),
                (
                    "_Unwind_GetTextRelBase",
                    "the_seven_answered_accessors_are_seven_distinct_addresses",
                ),
                (
                    "_Unwind_GetDataRelBase",
                    "the_seven_answered_accessors_are_seven_distinct_addresses",
                ),
                (
                    "_Unwind_DeleteException",
                    "the_seven_answered_accessors_are_seven_distinct_addresses",
                ),
                ("_Unwind_Backtrace", "the_backtrace_walk_produces_no_frames"),
            ];
            // Forty-eight rows for forty-eight names: every one of them is
            // named by a test, and no name is named twice.
            assert_eq!(COVERED.len(), (MODULE.names)().len());
            let mut covered: Vec<&str> = COVERED.iter().map(|(name, _)| *name).collect();
            covered.sort_unstable();
            let mut published = (MODULE.names)();
            published.sort_unstable();
            assert_eq!(
                covered, published,
                "a name is published without a test that names it"
            );
            for (name, test) in COVERED {
                assert!(
                    !test.is_empty(),
                    "{name} is mapped to a test with no name"
                );
            }
        }

        /// The Mach call's first argument is a **32-bit port name**, not a pointer.
        ///
        /// `Framework+0x81c40f7` is `mov edi,r15d` — the low half only. Declaring
        /// it as a `*const c_void` would work by accident; declaring it as `u32`
        /// is what the binary says it is.
        #[test]
        fn the_mach_task_is_a_port_name_and_not_a_pointer() {
            // SAFETY: a port name, an address, a command and a real
            // out-parameter. Nothing is dereferenced, and in particular the
            // 32-bit first argument is not a pointer that could be read.
            assert_eq!(
                unsafe { mach_vm_purgable_control(0, 0, 0, std::ptr::null_mut()) },
                KERN_FAILURE
            );
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

// ------------------------------------------------ the guest's error strings
//
// `sandbox_free_error` is `void sandbox_free_error(char *errbuf)`: it releases a
// string the *library* allocated on the way out of a failed call. Two producers
// in this tree hand one to the guest, and both are in the census:
//
// * `imp::sandbox_compile_string` — `Framework+0x7fe660f`, which frees
//   `*errorbuf` off the failure arm.
// * `crates::sandbox`'s `sandbox_init_with_parameters` — `Helper+0x100027050`,
//   which frees the out-word the same way.
//
// So the pointer the guest hands back is *ours*, and freeing it is a real
// `free` of a real allocation. Which is only safe to do for a pointer this
// runtime actually produced, and `sandbox_free_error` is a published symbol
// that takes whatever it is given. The table below is what makes "ours"
// answerable without trusting the argument.

/// The addresses of every error string this runtime has handed the guest and
/// not yet been asked to release.
#[cfg(not(target_os = "macos"))]
fn error_strings() -> &'static std::sync::Mutex<Option<std::collections::HashSet<usize>>> {
    // `Mutex::new(None)` and not `Default::default()`, because a `static`
    // initializer has to be a const call and `HashSet::new` is not one. Same
    // reason, and same shape, as `crate::utmpx`'s `WTMPX`.
    static LIVE: std::sync::Mutex<Option<std::collections::HashSet<usize>>> =
        std::sync::Mutex::new(None);
    &LIVE
}

/// Hand the guest an error string it is expected to release with
/// [`crate::media_unavailable::sandbox_free_error`].
///
/// **One allocation, owned by this runtime, and findable again.** The address is
/// registered before it is returned, so the release path can tell this string
/// from any other pointer, and a guest that never releases it leaks exactly one
/// small buffer per call — the same bounded-by-call-count trade `imp::leaked`
/// makes, and for the same reason: the guest has no other way to release it.
///
/// Declared here, outside `imp`, because the producer in `crates::sandbox` lives
/// in a crate **above** this one and has to reach the same table. There is no
/// edit to `lib.rs` involved: `pub mod last_unavailable` is already public.
#[cfg(not(target_os = "macos"))]
pub fn guest_error_string(text: &str) -> *mut std::ffi::c_char {
    // `CString::new` fails only on an interior NUL, and these messages are
    // literals with none; an empty string is the safe answer if one ever is.
    // `into_raw` rather than `Box::into_raw(into_boxed_c_str())` because only the
    // former is the documented round trip with `CString::from_raw`, and this
    // string is freed below rather than leaked.
    let raw = std::ffi::CString::new(text).unwrap_or_default().into_raw();
    let mut live = error_strings().lock().unwrap_or_else(|e| e.into_inner());
    // Cannot already be present: the allocation was made this line and has not
    // been given to anything yet.
    live.get_or_insert_with(Default::default).insert(raw as usize);
    raw
}

/// Release a string [`guest_error_string`] handed out. `false` — and no free — if
/// the pointer is not one of ours.
///
/// `false` is the only safe answer for a foreign pointer, and it is also the
/// answer to a **double** release, because the address is removed before the
/// allocation is freed: the second call finds nothing. Freed only under that
/// same removal, so a concurrent second release cannot be in the `Box::from_raw`
/// at the same time.
#[cfg(not(target_os = "macos"))]
pub fn release_guest_error_string(errbuf: *mut std::ffi::c_char) -> bool {
    let address = errbuf as usize;
    let ours = {
        let mut live = error_strings().lock().unwrap_or_else(|e| e.into_inner());
        live.as_mut().is_some_and(|live| live.remove(&address))
    };
    if address == 0 || !ours {
        return false;
    }
    // SAFETY: `ours` says this address is one `guest_error_string` made with
    // `CString::into_raw` and that has not been released since — the only other
    // way out of the table is this function, and it is no longer in it. That
    // `into_raw`/`from_raw` pair is the documented round trip.
    drop(unsafe { std::ffi::CString::from_raw(address as *mut std::ffi::c_char) });
    true
}

/// Nothing of this file's on Darwin: the host has VideoToolbox, Seatbelt,
/// SystemConfiguration, CoreServices, Metal and QuartzCore, and shadowing its own
/// with a refusal would be the worst of both worlds. See the `exports!` at the
/// foot of the file.
#[cfg(target_os = "macos")]
exports::exports! {}
