//! The half of the unwinder that is *ours*: the two-phase driver, and the
//! `_Unwind_Context` accessors the guest's own personality routine calls back
//! through.
//!
//! # What this is, and why it is small
//!
//! `Chromium Framework` throws through exactly one imported name,
//! `__Unwind_RaiseException`, at exactly **two** call sites, and both are inside
//! its own statically linked `//third_party/libc++abi`:
//!
//! ```text
//! 568089c  call __Unwind_RaiseException     ; its __cxa_throw
//! 5680ac7  call __Unwind_RaiseException     ; its __cxa_rethrow
//! ```
//!
//! (`e8`-scan of `__TEXT,__text` for the stub at `0xcca13e6`; two sites, two
//! hits.) It imports **no** `__cxa_*` name except `___cxa_atexit`, and **no**
//! `__gxx_personality_v0` - a census of all 14 Mach-Os in the bundle.
//!
//! So the name the guest enters the unwinder through is the one libunwind
//! publishes, and the question is what that entry point has to *be*. The answer
//! this module is: **the driver, and nothing else.** The decisions that make an
//! unwinder hard - which catch clause matches, where the landing pad is, what the
//! selector is - belong to the personality routine, and this guest links its own.
//!
//! ## The personality the guest's `__unwind_info` actually names
//!
//! `__TEXT,__unwind_info` in the Framework has **two** personality entries
//! (section header: `personalities 2@0x218`), and this is the load-bearing
//! measurement of the whole file:
//!
//! | index | slot | value in the file | used by |
//! |--:|---|---|--:|
//! | 1 | `0xdd51b20` (`__DATA_CONST,__got`) | **`0x5680bd0`** - unslid, in this image | 3,435 frames |
//! | 2 | `0xdd51b28` (`__DATA_CONST,__got`) | `0` - wants the `___objc_personality_v0` bind | 53 frames |
//!
//! Both are read as **eight** bytes and not four, and the reason is in
//! [`personality_at`]'s note: these are 64-bit slots, and the unslid value in
//! the file is the low half of the slid address the loader puts there, so a
//! four-byte read of the Framework's own slot returns `0x5680bd0` - a number
//! that reads like a bare offset and is an address in the low 4 GB.
//!
//! Index 1's value is a **function inside the Framework**: `0x5680bd0` is
//! `__gxx_personality_v0`, reachable through a one-instruction alias thunk at
//! `0x5d9276b` (`jmp 0x5680bd0` - the ld64 weak-alias form, with the aliased
//! function's own `mov eax,0x5; pop rbp; ret` immediately after it). Index 2 is
//! the `___objc_personality_v0` import, and the 53 frames that use it are the
//! Objective-C `@catch` ones: 15 of them sit in one run,
//! `0x47db8d0`..`0x47dd010`. See [`crate::objcexc`] for what that implies for
//! the records.
//!
//! **A correction, and it retracts the prior note's second address.** The
//! personality at `Framework+0x417640` is a *second, unreferenced copy*. Over
//! the whole image it has exactly one pcrel32 reference, at `0x41763c`, and that
//! is a false positive: the four bytes there are the `00 00 00 00` immediate
//! inside `0f 1f 80 00 00 00` (`nop DWORD PTR [rax+0x0]`, a six-byte instruction
//! beginning at `0x417639`), so the "reference" is a zero rel32 that happens to
//! land on the next function. The positive control for the same scan is
//! `0x5680bd0`, which it finds at a genuine `jmp`. `0x417640`'s own
//! `__unwind_info` entry is mode 4 (DWARF) with no personality and no LSDA,
//! which is what a routine nothing names looks like. The two copies are told
//! apart by one visible difference: `0x417640` calls `_Unwind_GetIPInfo` and
//! `0x5680bd0` does not.
//!
//! ## The `_Unwind_Context` is not ours, and that is the point
//!
//! The personality never dereferences the context. Over the whole 1,840 bytes of
//! `0x5680bd0`'s body there is **one** instruction reading through the register
//! the context arrived in (`movzx eax, BYTE PTR [r15]` at `0x5680f20`), and on
//! that path the register has been reassigned to a ULEB cursor
//! (`lea r15,[r12+rsi*1]; dec r15` at `0x5680ed6`). Every access goes through an
//! accessor: `__Unwind_GetLanguageSpecificData`, `__Unwind_GetIP`,
//! `__Unwind_GetRegionStart`, `__Unwind_SetGR` (four times) and
//! `__Unwind_SetIP` (twice). Nothing else.
//!
//! So the *protocol* is small - nine calls, five words of state - and this
//! module holds none of the state. It lives in
//! [`darwin::last_unavailable::unwind_context`], which is also where the seven
//! accessors are published, and the two halves have to be one object: a
//! published `extern "C"` has to mutate the *driver's* context, and `darwin`
//! cannot name a thread-local of ours.
//!
//! ## It runs, and what it took
//!
//! `crates/rosetta3/tests/unwind_driver.rs` is the answer, and it is a guest
//! program that reproduces the configuration the Framework runs in: it
//! **defines** the whole `__cxa_*` ABI - `__cxa_throw`, `__cxa_begin_catch`,
//! `__cxa_end_catch` - and `__gxx_personality_v0`, exactly as the Framework's
//! statically linked `//third_party/libc++abi` does, and leaves
//! `_Unwind_RaiseException` and the seven accessors as **imports**. That is the
//! only shape in which this module is reachable, and it is worth saying why the
//! obvious alternatives do not work:
//!
//! * a fixture that links a real libc++abi resolves every `_Unwind_*` **into
//!   that image**, and [`crate::vm::Vm::call_site`] transfers the call to the
//!   guest *before* [`crate::shims::dispatch`] is consulted - which is why
//!   `tests/unwind.rs` passes with the gate shut and is testing the vendored
//!   unwinder rather than this;
//! * a fixture with no personality of its own cannot be asked whether the driver
//!   offered a **guest** LSDA, because the only thing that can read the answer
//!   is a personality.
//!
//! Three defects came out of it, all of them wrong answers rather than crashes,
//! and all three are in this file:
//!
//! 1. **the first frame was read out of the stack.** [`raise_exception`] used to
//!    take the return address from `[rsp]`, on the argument that `rip` "is this
//!    function". That is true of the **native** backend and false of the
//!    emulated one, which is the only one that can reach this code - see that
//!    function's note.  The walk started in `__DATA` and the driver reported
//!    "no guest frame catches this exception" for an exception the very next
//!    frame catches.
//! 2. **the personality was read with four bytes of an eight-byte pointer.**
//!    [`personality_at`] - see its note, which also corrects the claim that the
//!    slot "is not a pointer to a pointer".  Four bytes gave `0x640` and
//!    `Vm::call` answered "no guest code at 0x640".
//! 3. **the LSDA is offered, and it is a guest address.**  The same run shows
//!    the personality dereferencing it (`0x5680cf6` in the Framework's copy is
//!    the same read) and getting `0xff` - the table's own first byte.
//!
//! A fourth defect is in `machish`, not here, and is reported rather than worked
//! around: `machish::image::UnwindInfo::lookup` rejects `va >= end`, and a return
//! address can legitimately be exactly `end` - it is one byte past the last
//! instruction of the last function in an image whose last instruction is a
//! `call`.  The fixture has a function after the throwing one so its return
//! address is inside the covered range, and says so.
//!
//! ## How a guest call reaches this module, and where it does not
//!
//! **On the emulated backend, claiming the two names is enough**, and that is now
//! measured rather than argued: `tests/unwind_driver.rs` runs a guest throw
//! through this module and lands in the guest's own landing pad. A name
//! [`crate::shims::is_shimmed`] claims is never promoted -
//! [`crate::program::ImportPolicy::promotion_for`] returns `None` for it - so
//! the lift gets a slow import, the guest's call is intercepted at
//! [`crate::abi::call_import`], and [`crate::shims::dispatch`] arrives here with
//! a fully synchronised [`RegisterContext`]: the guest's callee-saved set, its
//! `rsp` and its `rip` are all in it, because the exit syncs the context before
//! it returns. `state.redirect()` then makes the installed context take effect
//! on the way back out, which is the same two-call mechanism
//! [`crate::jmpbuf`]'s `longjmp` uses and which its own tests exercise.
//!
//! **On the native backend it is not, and this was measured rather than
//! assumed**, because it is the opposite of what the import table suggests.
//! `ROSETTA3_NATIVE_UNWINDER=1` does claim all nine names - a unit probe
//! confirms `is_shimmed("__Unwind_RaiseException")` and
//! `is_shimmed("__Unwind_SetIP")` both flip to `true` - and the run is still
//! byte-identical with the gate shut. The cause is in
//! `exec::native::Native::load`, which calls [`crate::trampoline::disarm`] and
//! binds every import to a host address, so a guest call is an ordinary machine
//! `call` and `shims::dispatch` is never consulted for it. `ROSETTA3_EH_CENSUS=1`
//! shows where `__Unwind_RaiseException` goes on this host:
//! `THE HOST, unaltered`, i.e. this Linux box's own `libgcc_s`, which is what
//! actually runs.
//!
//! Two consequences, and the second is the whole of the remaining work:
//!
//! * the **native** publication needs a published *address* - a naked
//!   `global_asm!` entry that captures the live callee-saved set plus `rsp` and
//!   `rip` and can install a new set and jump - and a published `extern "C"`
//!   that built a fresh [`RegisterContext`] would be the failure this project
//!   ranks first, because it would write a landing pad's registers into nothing;
//! * the **seven accessors** are unreachable through [`shim_for`] on native for
//!   the same reason. They are claimed anyway, because `darwin::last_unavailable`
//!   already claims them, and one table that both halves of
//!   [`crate::shims`] consult is what stops them disagreeing - but a claim alone
//!   is not a wiring.
//!
//! # The `_Unwind_Exception` layouts are one struct, and the driver reads one word
//!
//! The guest's `__cxa_exception` is 0x80 bytes with the `_Unwind_Exception` at
//! its **tail** and the thrown object immediately after that. Read out of the
//! instruction stream, at sites that agree with each other:
//!
//! | offset from the `_Unwind_Exception *` | field | evidence |
//! |--:|---|---|
//! | `+0x00` | `exception_class` | `0x5680855`-`0x5680885`: `mov QWORD PTR [rcx-0x20],0x434c4e47432b2b00` with `rcx` the thrown object; and `0x5680aab`: `and r13,QWORD PTR [r15+0x60]; cmp r13,r12` where `r15+0x60` is what `0x5680ac4` passes to `__Unwind_RaiseException` |
//! | `+0x08` | `exception_cleanup` | `0x5680890`: `mov QWORD PTR [rcx-0x18],0x56807c0`; and `0x56807c6` (`mov rbx,rsi`) shows the callee treating its second argument as the header |
//! | `+0x10` | `private_1` | the driver's, not the guest thrower's |
//! | `+0x18` | `private_2` | likewise |
//! | `+0x20` | the thrown object | `0x56807de`: `lea rdi,[rbx+0x20]; call rax` calls the destructor with `exc+0x20` |
//!
//! and the fields before it, from the same base `exc-0x60`:
//!
//! | offset | field | evidence |
//! |--:|---|---|
//! | `+0x08` | `referenceCount` | `0x5680894`: `mov QWORD PTR [rcx-0x78],0x1`; and `0x56807ce`: `lock dec QWORD PTR [rbx-0x58]` with `rbx = exc` |
//! | `+0x10` | `exceptionType` | `0x568086f`: `mov QWORD PTR [rcx-0x70],rsi` - the throw's second argument |
//! | `+0x18` | `exceptionDestructor` | `0x5680873`: `mov QWORD PTR [rcx-0x68],rdx`; and `0x5680a05`: `mov rax,QWORD PTR [rbx+0x68]; call rax` |
//! | `+0x30` | `nextException` | `0x5680791`: `mov QWORD PTR [rbx+0x30],rcx` |
//! | `+0x38` | `handlerSwitchValue` | `0x5680ab4`: `neg DWORD PTR [r15+0x38]` |
//! | `+0x40` | `catchTemp` | `0x5681220`: `mov QWORD PTR [r13-0x20],rax` |
//! | `+0x48` | `actionRecord` | `0x568122b`: `mov QWORD PTR [r13-0x18],rax` |
//! | `+0x50` | `languageSpecificData` | `0x568122f`: `mov QWORD PTR [r13-0x10],r9` |
//! | `+0x60` | `unwindHeader` | the table above |
//!
//! So `sizeof(__cxa_exception) == 0x80` with the header at `unwind_exception -
//! 0x60`, which is libc++abi's own
//! `cxa_exception_from_exception_unwind_exception()` = `(__cxa_exception
//! *)(unwind_exception + 1) - 1`.
//!
//! **The driver reads `exception_class` at `+0` and nothing else.** The other
//! three words belong to the guest's `__cxa_*` code, which does not run again
//! until the catch is over. That is also why the class word's *value* is not this
//! module's business: it is `0x434c4e47432b2b00`, libc++abi's `kOurExceptionClass`
//! held as a `uint64_t` and therefore byte-reversed relative to the `char[8]`
//! spelling - and libc++abi compares it **masked**, `class & ~0xFF`, which is
//! literally what the guest's own personality does at `0x5680c1b` (`and
//! rbx,0xffffffffffffff00; lea r14,[rcx-0x1]; cmp rbx,r14`) and what
//! `0x5680a96` does before the rethrow. The driver hands the eight bytes over
//! unchanged and the personality decides.
//!
//! # What the driver passes, measured
//!
//! The vendored `libunwind.dylib` this tree already publishes is the authority
//! for the call, because on a real system that is what the guest links. At
//! `0x558d`-`0x55a4` of that image:
//!
//! ```text
//! 558d  lea  esi,[rdx*4+0x2]      ; _UA_CLEANUP_PHASE, plus _UA_HANDLER_FRAME
//! 5594  mov  rdx,(r14)            ; exception_object[0]  <- exception_class
//! 5597  mov  edi,0x1              ; version
//! 559c  mov  rcx,r14              ; exception_object
//! 559f  mov  r8,rbx               ; context
//! 55a2  call *rax                 ; the personality
//! 55a4  cmp  eax,0x8              ; _URC_CONTINUE_UNWIND
//! 55fe  cmp  eax,0x7              ; _URC_INSTALL_CONTEXT
//! ```
//!
//! and the guest's own personality returns `mov eax,0x7` at `0x5680c91` and
//! `0x56811f7` (install), `0x6` at `0x5681206` and `0x5681237` (handler found),
//! `0x8` at `0x5681264` (keep going) and `0x3` at `0x5680c00` (version
//! mismatch). Two independent images agree, so:
//!
//! | code | name | read at |
//! |--:|---|---|
//! | 2 | `_URC_FATAL_PHASE2_ERROR` | `0x4825` in the vendored driver - **and nowhere in the guest** |
//! | 3 | `_URC_FATAL_PHASE1_ERROR` | `0x5680c00` |
//! | 6 | `_URC_HANDLER_FOUND` | `0x5681206` |
//! | 7 | `_URC_INSTALL_CONTEXT` | `0x5680c91`; `0x55fe` |
//! | 8 | `_URC_CONTINUE_UNWIND` | `0x5681264`; `0x55a4` |
//!
//! **A correction, and it retracts a `0x…1f` that was in the table above.**  The
//! row used to read `0x568121f`, and **both halves of it were wrong about the
//! guest**.  `0x568121f` is *mid-instruction*: `0x5681219` is the seven-byte
//! `mov rax,QWORD PTR [rbp-0x98]`, which occupies `0x5681219`-`0x5681220` - a
//! `0x…1f` inside a `QWORD` store's displacement is the middle of an address,
//! not an opcode.  And `0x5680bd0` **never returns `2` at all**: over its whole
//! 1,840 bytes it has exactly one exit, the `ret` at `0x568127a`, and every path
//! to it sets `eax` from one of four `mov eax,imm32` - `0x3` at `0x5680c00`,
//! `0x6` at `0x5681206` and `0x5681237`, `0x7` at `0x5680c91` and `0x56811f7`,
//! `0x8` at `0x5681264`.  A `grep -E "mov +eax,0x[0-9a-f]+$"` over
//! `test/tools/raw-range.py 0x5680bd0:0x5681280` returns those six and no `0x2`.
//! So `2` comes **only** from the vendored driver at `0x4825`.
//!
//! Harmless in itself - the driver only ever *receives* reason codes and this one
//! it acts on identically to `3` - but it is the exact shape of a claim that
//! cannot be re-checked: a `0x…1f` inside an instruction, in a table of
//! measured constants, attributed to a routine that does not emit it.
//!
//! The action bits are `_UA_SEARCH_PHASE = 1`, `_UA_CLEANUP_PHASE = 2`,
//! `_UA_HANDLER_FRAME = 4`, `_UA_FORCE_UNWIND = 8`, from the personality's own
//! tests: `cmp esi,0x6` (`0x5680c2c`, the handler-frame arm), `test r12b,0x1`
//! (`0x5680c36`), `test r12b,0x2` (`0x5680c3c`) and `not eax; test al,0xc`
//! (`0x5680c4b`, so bits 2 and 3 must be clear).
//!
//! # How much of the image's frames this can cross
//!
//! A census of all 267,638 `__unwind_info` entries by mode:
//!
//! | mode | count | what `step()` does with it |
//! |--:|--:|---|
//! | 1, frame pointer | 263,293 | leaves the frame |
//! | 4, DWARF | **2,306** | [`UnwindError::NeedsDwarf`] - a refusal naming `__eh_frame` |
//! | 2, frameless, immediate size | 1,569 | leaves the frame |
//! | 3, frameless, size in the text | 56 | leaves the frame |
//! | 0, no entry | 414 | [`UnwindError::NoEntry`] |
//!
//! So 99.1% of frames are crossable and 0.86% are a named refusal. That is the
//! honest shape of the capability: an unwind that happens to cross one of those
//! 2,306 frames stops and says which table it needed, rather than guessing.
//!
//! # What is behind the gate, and what is still missing
//!
//! Two things, and both are named rather than worked around.
//!
//! **1. The context is now one object, and that part is done.** The seven
//! accessors are published by `darwin` and answered from
//! [`darwin::last_unavailable::unwind_context`]; this module offers frames
//! through the same object and reads the personality's choices back out of it.
//! Until they were one object every accessor call from inside a personality was
//! a `CONTEXT_NOT_OURS` refusal; that refusal is now gone from the emulated
//! path, where a personality's `SetGR` reaches the context this driver filled.
//!
//! **2. The native publication needs a live-context entry point, and there is
//! not one here.** `ROSETTA3_NATIVE_UNWINDER=1` claims both entry points, and a
//! claim is what a shim needs on the emulated backend - but on native the claim
//! changes nothing measurable, for the reason in the section above. Making it
//! work there means publishing an address, and this module deliberately does not
//! publish one: an `extern "C"` entry point that constructed a fresh
//! [`RegisterContext`] would make every install write a landing pad's registers
//! into nothing, which is the failure this project ranks above a wrong answer.
//! The mechanism to build it with is [`crate::jmpbuf`] - a naked
//! `global_asm!` shim that saves the live set, calls in with a
//! [`RegisterContext`], and on return restores and jumps - and it is a separate
//! piece of work, not a wiring step.
//!
//! The gate is [`enabled`]: `ROSETTA3_NATIVE_UNWINDER=1`. With it unset
//! [`shim_for`] returns `None` for every name before it looks at any of them, so
//! nothing in this file is reachable and the runtime's behaviour is
//! byte-identical to not having it.

use std::cell::Cell;

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{AbiError, GuestMemory, Result};
use crate::unwind::{Registers, UnwindError};
use crate::vm::Vm;

/// The `_Unwind_Context` this tree creates, and the seven accessors that read it.
///
/// **`darwin` owns it and this module only uses it.** That direction is forced
/// by the dependency - `darwin` cannot name a thread-local of ours - and it is
/// the whole reason the two halves have to agree: a published `extern "C"` has
/// to mutate *this* driver's context, or every accessor call from inside a
/// personality routine is a `CONTEXT_NOT_OURS` refusal.
use darwin::last_unavailable::unwind_context as context;

// ---------------------------------------------------------------- reason codes

/// `_URC_FATAL_PHASE2_ERROR`. See the module table.
const URC_FATAL_PHASE2_ERROR: u64 = 2;
/// `_URC_FATAL_PHASE1_ERROR`.
const URC_FATAL_PHASE1_ERROR: u64 = 3;
/// `_URC_HANDLER_FOUND`: this frame has something to run.
const URC_HANDLER_FOUND: u64 = 6;
/// `_URC_INSTALL_CONTEXT`: this frame is the one to enter.
const URC_INSTALL_CONTEXT: u64 = 7;
/// `_URC_CONTINUE_UNWIND`: nothing here, keep going.
const URC_CONTINUE_UNWIND: u64 = 8;

// ----------------------------------------------------------------- action bits

/// `_UA_SEARCH_PHASE`.
const UA_SEARCH_PHASE: u64 = 1;
/// `_UA_CLEANUP_PHASE`.
const UA_CLEANUP_PHASE: u64 = 2;
/// `_UA_HANDLER_FRAME`: the frame phase one stopped in.
const UA_HANDLER_FRAME: u64 = 4;

// ------------------------------------------------------------------- the gate

/// `ROSETTA3_NATIVE_UNWINDER`: route the two `_Unwind_*` entry points here.
///
/// Off by default. With it unset [`shim_for`] returns `None` for every name
/// before it looks at any of them, so nothing in this file is reachable and the
/// runtime behaves exactly as it did before the file existed.
pub fn enabled() -> bool {
    #[static_init::dynamic]
    static ON: bool = std::env::var_os("ROSETTA3_NATIVE_UNWINDER").is_some();
    *ON
}

// ------------------------------------------------------------------- the flight

/// An unwind in progress on this thread.
///
/// Re-derived rather than accumulated. Every call into a personality starts again
/// from the frame it is asking about, and a landing pad lives *in* the frame it
/// belongs to - so stepping out of the resume point is exactly the next frame to
/// consider, and no list of frames has to survive across the host boundary.
#[derive(Clone, Copy, Debug)]
struct Flight {
    /// The `_Unwind_Exception *` the guest passed to `__Unwind_RaiseException`.
    exc: u64,
    /// The `exception_class` word read out of it at `+0`.
    class: u64,
    /// The first frame to consider: the caller of the guest's `__cxa_throw`.
    /// Phase two starts here, because phase one walked *past* every cleanup
    /// between the throw and the handler to find it.
    origin: Registers,
    /// The frame that will catch, as its registers were when phase one found it.
    handler: Option<Registers>,
    /// The frame being considered right now.
    at: Registers,
}

thread_local! {
    static IN_FLIGHT: Cell<Option<Flight>> = const { Cell::new(None) };
}

fn in_flight() -> Option<Flight> {
    IN_FLIGHT.with(Cell::get)
}

fn set_in_flight(flight: Option<Flight>) {
    IN_FLIGHT.with(|f| f.set(flight));
}

/// The eight bytes at `+0` of an `_Unwind_Exception`.
///
/// The **only** field this module reads out of the guest's exception object.
/// Everything else in that structure belongs to the guest's own `__cxa_*` code,
/// and a driver that went looking would be guessing at a layout the guest owns.
fn exception_class(exc: u64, mem: &dyn GuestMemory) -> core::result::Result<u64, UnwindError> {
    let mut word = [0u8; 8];
    mem.read(exc, &mut word).map_err(|_| UnwindError::BadFrame)?;
    Ok(u64::from_le_bytes(word))
}

// ------------------------------------------------------------------ the driver

/// How many frames one walk will follow before calling the stack corrupt.
const MAX_FRAMES: usize = 1024;

/// `_Unwind_Reason_Code _Unwind_RaiseException(_Unwind_Exception *exception_object)`
///
/// Never returns to its caller on success: `ctx` has been moved into the first
/// landing pad of the unwind.
///
/// # Why the first frame is `rip` and **not** the word at `[rsp]`
///
/// **This was wrong, and it is the reason the driver had never run.** The note
/// used to say that `rip` "is *this function*" and that the walk must start at
/// the return address instead. That is true of the **native** backend, where a
/// guest call is an ordinary machine `call` and `rip` is inside a host shim. It
/// is false of the **emulated** backend, which is the only one that can reach
/// this function at all, and there it is exactly backwards:
///
/// * microlift's contract for a slow import is its own words -
///   `emit_import_slow_exit`: *"self.current_ip already holds the instruction
///   after the CALL (the resume point), so we sync and store that as RIP"*, and
///   `RSP is intentionally left unchanged` for the indirect case. So `rip` is
///   the guest's return address and the return address is **not on the stack**.
/// * the in-tree positive control is [`crate::vm::Vm::transfer_to_guest`],
///   which pushes `ctx.rip` itself (`write(ctx.rsp - 8, &ctx.rip.to_le_bytes())`)
///   before jumping - the same convention, already relied on.
///
/// Measured, on a fixture whose `_Unwind_RaiseException` is an import of its
/// own: `ctx.rip` was `0x10000133c`, the instruction after the `call` at
/// `0x100001337`, while `[ctx.rsp]` was `0x100004070` - the address of the
/// argument the guest had just passed, in `__DATA,__bss`. Reading the frame
/// from `[rsp]` therefore started the walk at a data address, `unwind_entry`
/// returned `None`, and the driver reported "no guest frame catches this
/// exception" for an exception that the very next frame catches.
///
/// So: `origin.rip` is `ctx.rip` when that is a guest address, and a
/// `ctx.rip` that belongs to no guest image is **refused by name** rather than
/// guessed at. The native publication does not exist yet, so that refusal names
/// the missing wiring instead of papering over it; when it is built, the same
/// code will see a guest `rip` there too, because the native entry point is
/// reached by pushing the guest's return address before calling in.
///
/// The callee-saved registers in `ctx` are, at that moment, the *caller's*: a
/// compiled `__cxa_throw` does not change them, so carrying them across is right
/// rather than merely convenient.
pub fn raise_exception(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let vm = current_vm(symbol)?;
    let state = crate::thread::current().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "an unwind outside a running guest thread",
    })?;
    if in_flight().is_some() {
        return Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "a second exception raised while one is already being unwound",
        });
    }
    let exc = ctx.rdi;
    let class = exception_class(exc, mem).map_err(|e| e.as_error(symbol))?;
    let origin = Registers::from_context(ctx);
    if !vm.program.is_guest_address(origin.rip) {
        return Err(UnwindError::PersonalityFailed(format!(
            "the program counter this driver was entered with, {:#x}, belongs to no guest \
             image, so there is no return address to start the walk from. On the emulated \
             backend a slow import carries the guest's own resume point in `rip` and leaves \
             the return address off the stack - microlift's `emit_import_slow_exit` says so \
             and `Vm::transfer_to_guest` relies on it by pushing `ctx.rip` itself - so `rip` \
             is normally inside the image and this is a native-backend entry. That needs the \
             published naked `global_asm!` trampoline, which does not exist yet; reading the \
             stack instead would be a guess about what is there, and the wrong guess is a \
             walk that starts in `__DATA` and reports that nothing catches",
            origin.rip
        ))
        .as_error(symbol));
    }

    let mut flight = Flight {
        exc,
        class,
        origin,
        handler: None,
        at: origin,
    };
    if !search(vm, &mut flight, mem).map_err(|e| e.as_error(symbol))? {
        return Err(UnwindError::NoHandler.as_error(symbol));
    }
    // Phase two starts where phase one started, not where it finished: the
    // frames between the throw and the handler still have cleanups to run.
    flight.at = flight.origin;
    let entered = cleanup(vm, &mut flight, ctx, mem, false).map_err(|e| e.as_error(symbol))?;
    if !entered {
        set_in_flight(None);
        return Err(UnwindError::NoHandler.as_error(symbol));
    }
    set_in_flight(Some(flight));
    state.rsp.set(ctx.rsp);
    state.redirect();
    Ok(())
}

/// `void _Unwind_Resume(_Unwind_Exception *exception_object)`
///
/// A landing pad that has finished running its cleanups calls this, and the walk
/// continues outwards from the landing pad's own frame.
pub fn resume(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let vm = current_vm(symbol)?;
    let state = crate::thread::current().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "a resume outside a running guest thread",
    })?;
    let Some(mut flight) = in_flight() else {
        return Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "_Unwind_Resume with no exception in flight",
        });
    };
    // The landing pad runs *in* the frame it belongs to, so its caller is the
    // next frame with anything to do.
    flight.at = Registers::from_context(ctx);
    let entered = cleanup(vm, &mut flight, ctx, mem, true).map_err(|e| e.as_error(symbol))?;
    if !entered {
        set_in_flight(None);
        return Err(UnwindError::NoHandler.as_error(symbol));
    }
    set_in_flight(Some(flight));
    state.rsp.set(ctx.rsp);
    state.redirect();
    Ok(())
}

/// Phase one: walk outwards asking each frame whether it catches this.
///
/// Whether a handler was found. On success `flight.handler` holds that frame's
/// registers.
fn search(
    vm: &Vm,
    flight: &mut Flight,
    mem: &dyn GuestMemory,
) -> core::result::Result<bool, UnwindError> {
    for _ in 0..MAX_FRAMES {
        let Some(entry) = vm.program.unwind_entry(flight.at.rip) else {
            return Ok(false);
        };
        let mut caller = flight.at;
        crate::unwind::step(&mut caller, &entry, mem)?;
        if crate::retstub::is_exit(caller.rip) {
            return Ok(false);
        }
        let reason = ask(vm, flight, &caller, mem, UA_SEARCH_PHASE)?;
        match reason {
            URC_CONTINUE_UNWIND => {}
            URC_HANDLER_FOUND => {
                flight.handler = Some(caller);
                return Ok(true);
            }
            // The personality read its own table and gave up. Said as itself
            // rather than folded into a walk error, because the walk is fine and
            // the reason is in the guest.
            URC_FATAL_PHASE1_ERROR | URC_FATAL_PHASE2_ERROR => {
                return Err(UnwindError::PersonalityFailed(format!(
                    "the personality routine at {:#x} returned reason code {} for the \
                     frame at {:#x} and the walk stopped there",
                    personality_of(vm, &caller, mem)?,
                    reason,
                    caller.rip
                )));
            }
            // A reason code this driver does not know is a claim about frames it
            // could not check, and continuing past it would be picking a landing
            // pad on the strength of it.
            other => {
                return Err(UnwindError::PersonalityFailed(format!(
                    "the personality routine for the frame at {:#x} returned reason code \
                     {other}, which is not one this driver knows; the walk stopped rather \
                     than treat it as a claim about frames it never looked at",
                    caller.rip
                )));
            }
        }
        flight.at = caller;
    }
    Err(UnwindError::BadFrame)
}

/// Phase two: run cleanups outwards, then enter the frame that catches.
///
/// `skip_current` is set for a `_Unwind_Resume`, where `flight.at` is the landing
/// pad's own frame and its caller is the next one to consider.
fn cleanup(
    vm: &Vm,
    flight: &mut Flight,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    skip_current: bool,
) -> core::result::Result<bool, UnwindError> {
    if skip_current {
        let entry = vm
            .program
            .unwind_entry(flight.at.rip)
            .ok_or(UnwindError::NoEntry)?;
        let mut caller = flight.at;
        crate::unwind::step(&mut caller, &entry, mem)?;
        flight.at = caller;
    }
    for _ in 0..MAX_FRAMES {
        if crate::retstub::is_exit(flight.at.rip) {
            return Ok(false);
        }
        let Some(entry) = vm.program.unwind_entry(flight.at.rip) else {
            return Ok(false);
        };
        let at = flight.at;
        let is_handler = flight.handler == Some(at);
        let action = if is_handler {
            UA_CLEANUP_PHASE | UA_HANDLER_FRAME
        } else {
            UA_CLEANUP_PHASE
        };
        let reason = ask(vm, flight, &at, mem, action)?;
        match reason {
            // The handler frame, entered. It is the last frame this walk may
            // enter: the personality has just written the landing pad and the
            // selector into the context, and the landing pad's caller is reached
            // by the landing pad itself, through _Unwind_Resume.
            URC_INSTALL_CONTEXT => {
                install(ctx, &at);
                return Ok(true);
            }
            // One frame past the handler would mean phase one and phase two read
            // different tables. Stopping is the honest outcome.
            URC_CONTINUE_UNWIND if is_handler => return Ok(false),
            URC_CONTINUE_UNWIND => {}
            // Phase two found a landing pad in a frame phase one did not stop in.
            // That is the same disagreement, from the other side.
            URC_HANDLER_FOUND => {
                return Err(UnwindError::PersonalityFailed(format!(
                    "the personality routine for the frame at {:#x} installed a landing pad \
                     in the cleanup pass, but phase one did not stop in that frame",
                    at.rip
                )));
            }
            other => {
                return Err(UnwindError::PersonalityFailed(format!(
                    "the personality routine for the frame at {:#x} returned reason code \
                     {other} in the cleanup pass, which is not a code this driver acts on",
                    at.rip
                )));
            }
        }
        let mut caller = at;
        crate::unwind::step(&mut caller, &entry, mem)?;
        flight.at = caller;
    }
    Err(UnwindError::BadFrame)
}

/// Adopt the context the personality filled in, as the guest's next instruction.
///
/// The registers come from the *walk*, not from the live context: the walk
/// stepped out of every frame between the throw and here, restoring each one's
/// saved registers, and that is the only place those values exist. `rax`, `rdx`
/// and the program counter come from the personality, which is what chose them.
fn install(ctx: &mut RegisterContext, at: &Registers) {
    // The three words the personality wrote, read back out of the *one* context
    // its accessors mutated. `gr0` and `gr1` are `rax` and `rdx`: the guest's
    // `set_registers()` writes `__builtin_eh_return_data_regno(0)` and `(1)`,
    // which are DWARF 0 and 1, at `0x5680bf8`/`0x5680c07` and again at
    // `0x56811d7`/`0x56811e7`. Which is also what [`crate::unwind`]'s
    // `enter_next_pad` writes, so the two paths agree by construction rather
    // than by coincidence.
    let chosen = context::chosen();
    at.apply(ctx);
    ctx.rax = chosen.gr0;
    ctx.rdx = chosen.gr1;
    ctx.rip = chosen.ip;
}

/// Offer one frame to its personality routine, if it has one.
///
/// The context carries this frame's `__unwind_info` entry's `lsda` and
/// `function` - the two things `scan_eh_tab()` asks for before anything else -
/// and its own program counter. A frame with no personality, or one whose
/// personality slot is unreadable, is a frame with nothing to say.
fn ask(
    vm: &Vm,
    flight: &Flight,
    at: &Registers,
    mem: &dyn GuestMemory,
    actions: u64,
) -> core::result::Result<u64, UnwindError> {
    let Some(entry) = vm.program.unwind_entry(at.rip) else {
        return Ok(URC_CONTINUE_UNWIND);
    };
    let Some(slot) = entry.personality_slot else {
        return Ok(URC_CONTINUE_UNWIND);
    };
    let personality = personality_at(slot, mem)?;
    // The one context, offered through the object the seven accessors read, and
    // handed to the routine as `pointer()` - which is that same object's
    // address, so the two cannot disagree about which context the guest is
    // about to be given. `lsda` is zero for a frame with no `try`, and that is
    // not a missing value: the personality turns a null language-specific
    // pointer into `_URC_CONTINUE_UNWIND` and walks outwards.
    let offered = context::offer(&context::Context {
        gr0: 0,
        gr1: 0,
        ip: at.rip,
        region_start: entry.function,
        lsda: entry.lsda.unwrap_or(0),
    });

    // The personality is guest code called from inside an unwind, so its frame
    // goes *below* the frame being offered - which is the lowest address the
    // walk still needs, since every other frame is above it.
    if let Some(state) = crate::thread::current() {
        state.rsp.set(at.rsp.wrapping_sub(8));
    }
    let reason = vm
        .call(
            personality,
            &[1, actions, flight.class, flight.exc, offered as u64],
        )
        .map_err(|e| UnwindError::PersonalityFailed(e.to_string()))?;
    Ok(reason)
}

/// The address in an `__unwind_info` personality slot.
///
/// **This read four bytes and it must read eight**, and the four-byte version is
/// how a driver ends up calling `0x5680bd0`.
///
/// *What the array holds.* `__TEXT,__unwind_info`'s personality array is a list
/// of four-byte entries, and each entry is **the offset of a `__got` slot**,
/// not the routine's address. That is `ld64.lld`'s own writer, verbatim -
/// `lld/MachO/UnwindInfoSection.cpp`, `writeTo`:
///
/// ```text
///   // Personalities
///   for (const Symbol *personality : personalities)
///     *i32p++ = personality->getGotVA() - in.header->addr;
/// ```
///
/// so the entry is a *location*, and `UnwindInfo::parse` duly turns it into
/// `personality_slot = base + entry` - the address of the slot. Apple's `ld64`
/// does the same, which is why the Framework's index-1 entry is `0xdd51b20`:
/// an address inside `__DATA_CONST,__got`.
///
/// *What the slot holds.* A `__got` slot is a **64-bit** pointer. On disk it
/// carries the *unslid* value - the Framework's is `0x5680bd0` in all eight
/// bytes, which is the file reading above - and the loader rewrites it to the
/// routine's full slid address, `0x1_0056_80bd0`. **A four-byte read returns
/// `0x5680bd0` either way**, because it is the low half before and after.
///
/// That is exactly why the old note looked self-consistent: the number it
/// recorded was right as a number and wrong as an address, and the two are
/// indistinguishable unless you read all eight bytes. So it read four, got the
/// low half, and concluded that "the slot holds the routine's address and is not
/// a pointer to a pointer, so a second dereference would read the first bytes
/// of the personality's own code". The conclusion about *not* dereferencing again
/// was right; the reason was wrong, and the value returned was a truncated
/// address - one in the low 4 GB, which is in no guest image at all.
///
/// Measured, on a fixture whose personality is a function of its own image:
/// four bytes gave `0x640` and `Vm::call` answered *"no guest code at 0x640"*;
/// eight bytes give `0x1_0000_0640` and the personality is entered.
fn personality_at(
    slot: u64,
    mem: &dyn GuestMemory,
) -> core::result::Result<u64, UnwindError> {
    let mut word = [0u8; 8];
    mem.read(slot, &mut word).map_err(|_| UnwindError::NoEntry)?;
    Ok(u64::from_le_bytes(word))
}

/// The personality routine a frame would be offered to, for a refusal that has
/// to name it.
///
/// The refusal is about the routine, so a sentence that did not say which one
/// would leave the reader to go and look - and the answer is not always the
/// guest's own: 53 frames in the Framework name `___objc_personality_v0`, which
/// this tree does not have.
fn personality_of(
    vm: &Vm,
    at: &Registers,
    mem: &dyn GuestMemory,
) -> core::result::Result<u64, UnwindError> {
    match vm.program.unwind_entry(at.rip).and_then(|e| e.personality_slot) {
        Some(slot) => personality_at(slot, mem),
        None => Ok(0),
    }
}

/// The VM running this thread, which every unwinding entry point needs for the
/// guest's tables and for entering its code.
fn current_vm(symbol: &str) -> Result<&'static Vm> {
    crate::thunk::current().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "unwinding needs the JIT, and no VM is running on this thread",
    })
}

/// Whether `name` (underscore already stripped) is one of this module's, and
/// only when the gate is open.
///
/// **Both the two entry points and the seven accessors.** On the emulated
/// backend that is all nine need, and all nine get: a claimed name is never
/// promoted, so the guest's call is intercepted at [`crate::abi::call_import`]
/// and arrives here with the guest's registers synchronised into a
/// [`RegisterContext`].
///
/// On native it is a claim and not a wiring - see the module note, which
/// measures that - and the seven are claimed for a second reason: `darwin`'s
/// `last_unavailable` already claims all eleven `_Unwind_*` names, and one table
/// that both halves of [`crate::shims`] consult is what stops them disagreeing
/// about a name that is simultaneously served and unserved.
pub fn shim_for(name: &str) -> Option<crate::jmpbuf::Shim> {
    if !enabled() {
        return None;
    }
    Some(match name {
        "_Unwind_RaiseException" => raise_exception,
        "_Unwind_Resume" => resume,
        "_Unwind_GetIP"
        | "_Unwind_GetIPInfo"
        | "_Unwind_GetGR"
        | "_Unwind_SetGR"
        | "_Unwind_SetIP"
        | "_Unwind_GetRegionStart"
        | "_Unwind_GetLanguageSpecificData" => accessor,
        _ => return None,
    })
}

// --------------------------------------------------- the seven accessors, shimmed

/// The seven `_Unwind_Context` accessors, as the emulated backend sees them.
///
/// A personality routine is **guest code**, and on the emulated backend a guest
/// call is intercepted before any published address is consulted - so the eight
/// `extern "C"` functions `darwin` publishes are never asked, and a name with no
/// shim is a name nothing answers. Each is a load, a call and a store: the
/// guest's arguments are already in the live [`RegisterContext`], and the
/// answer belongs in `rax` because every one of the seven returns a pointer,
/// a word, or nothing.
///
/// The signatures are the Itanium ones, and the argument positions are not
/// guesses. Every one of the seven takes a `struct _Unwind_Context *` in `rdi`
/// and nothing else in `rdi`, so that is common; what differs is the second
/// argument, and conflating those is the mistake this function's shape makes
/// hard:
///
/// * `SetGR` is `(context, regno, value)` - `rdi`, `rsi`, **`rdx`**, which is
///   what `0x5680c6c` (`xor esi,esi` with `rdx = rcx`) and `0x5680c79`
///   (`mov esi,0x1` with `rdx = rbx`) are;
/// * `SetIP` is `(context, ip)` - `rdi`, `rsi`;
/// * `GetIPInfo` is `(context, int *ip_before)` - `rdi`, `rsi`;
/// * `GetGR` is `(context, regno)` - `rdi`, `rsi`;
/// * and `GetIP`, `GetRegionStart`, `GetLanguageSpecificData` take `rdi` alone.
///
/// One function rather than seven, because seven would be seven places to get a
/// position wrong and seven functions to keep in step with the table above.
fn accessor(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let context = ctx.rdi as *mut std::ffi::c_void;
    // SAFETY: each of the seven takes a `struct _Unwind_Context *` that the
    // driver itself offered through `context::offer`, and the accessors in
    // `darwin` each check that pointer against the one object they will touch
    // before reading or writing it. A pointer from any other source is refused
    // by name there, which is the correct outcome rather than a crash - and it is
    // the refusal that names this exact missing wiring.
    unsafe {
        match symbol {
            "_Unwind_GetIP" => ctx.rax = context::unwind_get_ip(context),
            "_Unwind_GetIPInfo" => {
                let ip_before = ctx.rsi as *mut i32;
                ctx.rax = context::unwind_get_ip_info(context, ip_before);
            }
            "_Unwind_GetGR" => {
                let regno = ctx.rsi as u32 as i32;
                ctx.rax = context::unwind_get_gr(context, regno);
            }
            "_Unwind_SetGR" => {
                let regno = ctx.rsi as u32 as i32;
                let value = ctx.rdx;
                context::unwind_set_gr(context, regno, value);
            }
            "_Unwind_SetIP" => {
                let ip = ctx.rsi;
                context::unwind_set_ip(context, ip);
            }
            "_Unwind_GetRegionStart" => {
                ctx.rax = context::unwind_get_region_start(context);
            }
            "_Unwind_GetLanguageSpecificData" => {
                ctx.rax = context::unwind_get_language_specific_data(context) as u64;
            }
            // Unreachable through [`shim_for`], which lists exactly the seven
            // above. A refusal rather than a silent no-op, so a name reaching
            // here by another route is named instead of answered with nothing.
            other => {
                return Err(AbiError::Refused {
                    symbol: other.to_string(),
                    reason: "an _Unwind_Context accessor this driver does not implement; the \
                             seven it does are _Unwind_GetIP, _Unwind_GetIPInfo, _Unwind_GetGR, \
                             _Unwind_SetGR, _Unwind_SetIP, _Unwind_GetRegionStart and \
                             _Unwind_GetLanguageSpecificData",
                });
            }
        }
    }
    // `void` returns leave `rax` alone on a real system, and a personality that
    // read it afterwards would be reading its own value. The two `Set*` arms
    // are the only ones that write nothing above, and they write nothing.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers this module's whole protocol rests on, pinned against the two
    /// images they were read out of.
    ///
    /// A test that only checked `UA_SEARCH_PHASE == 1` would pass against a
    /// driver that passed the right bits to the wrong personality, because both
    /// would be "an action". The values *are* the ABI, and the ABI is two
    /// third-party binaries' private agreement, so the only way to hold it is to
    /// write it down where a change has to meet it.
    #[test]
    fn the_reason_codes_are_the_two_images_ones() {
        // `2` has no guest site at all, which is the point of the message: the
        // row used to cite `0x568121f`, and that address is mid-instruction
        // (`0x5681219` is a seven-byte `mov rax,QWORD PTR [rbp-0x98]`) *and* the
        // routine never writes `2` - its only exit is the `ret` at `0x568127a`
        // and the four `mov eax,imm32` that feed it are 3, 6, 7 and 8.  The only
        // site is the vendored driver.  See the module note.
        assert_eq!(URC_FATAL_PHASE2_ERROR, 2, "0x4825 vendored; no guest site");
        assert_eq!(URC_FATAL_PHASE1_ERROR, 3, "0x5680c00");
        assert_eq!(URC_HANDLER_FOUND, 6, "0x5681206, 0x5681237");
        assert_eq!(URC_INSTALL_CONTEXT, 7, "0x5680c91, 0x56811f7; 0x55fe vendored");
        assert_eq!(URC_CONTINUE_UNWIND, 8, "0x5681264; 0x55a4 vendored");
        // And the action bits, from the personality's own tests. `cmp esi,0x6`
        // is the handler-frame arm, so CLEANUP|HANDLER_FRAME is 6, `test r12b,1`
        // is the search arm, and `not eax; test al,0xc` says the remaining two
        // are 1 and 8 - of which only the first three are ever passed.
        assert_eq!(UA_CLEANUP_PHASE | UA_HANDLER_FRAME, 6);
        assert_eq!(UA_SEARCH_PHASE, 1);
        assert_eq!(UA_CLEANUP_PHASE, 2);
        assert_eq!(UA_HANDLER_FRAME, 4);
    }

    /// The selector is DWARF register 1, which is `rdx`; the exception pointer
    /// is DWARF register 0, which is `rax`.
    ///
    /// The evidence is `__gxx_personality_v0`'s `set_registers()` writing
    /// `__builtin_eh_return_data_regno(0)` and `(1)`, which are DWARF 0 and 1 on
    /// x86-64. The consequence of getting it wrong is not a wrong value but a
    /// wrong *program*: a driver installing a selector in `rax` and an exception
    /// pointer in `rdx` produces a `catch` that silently never matches.
    ///
    /// The numbers now live in `darwin`, next to the accessors that use them and
    /// the refusal that names a number it does not carry. Asserting them here as
    /// well is deliberate: two copies of a constant in two crates is how they
    /// drift, and this assertion fails the moment they do.
    #[test]
    fn the_two_written_registers_are_rax_and_rdx() {
        assert_eq!(context::GR_EXCEPTION, 0);
        assert_eq!(context::GR_SELECTOR, 1);
        // And this crate has no second opinion of its own to drift from.
        assert_eq!(context::GR_EXCEPTION, 0, "0x5680c6c: xor esi,esi");
        assert_eq!(context::GR_SELECTOR, 1, "0x5680c79: mov esi,0x1");
    }

    /// The whole nine-call conversation, driven through this crate's own shim
    /// entry points in the **Mach-O spelling** the images import.
    ///
    /// The spellings carry **one** leading underscore, because
    /// [`crate::shims::dispatch`] strips exactly one before it looks a name up,
    /// and the images import `__Unwind_SetGR`. This is the project's standing
    /// way to be wrong, and it is the reason the test hands `shim_for` the Mach-O
    /// spelling reduced by one rather than the C identifier: a shim table keyed
    /// `Unwind_SetIP` would compile, pass every other test in the file, and leave
    /// the import unserved.
    ///
    /// It also covers the thing the deleted local accessors used to cover, from
    /// the side that matters: the driver's `offer` and the accessors' reads are
    /// **one object**, which is the whole claim of the rewire.
    #[test]
    fn the_nine_names_are_claimed_in_the_mach_o_spelling() {
        for imported in [
            "__Unwind_RaiseException",
            "__Unwind_Resume",
            "__Unwind_GetIP",
            "__Unwind_GetIPInfo",
            "__Unwind_GetGR",
            "__Unwind_SetGR",
            "__Unwind_SetIP",
            "__Unwind_GetRegionStart",
            "__Unwind_GetLanguageSpecificData",
        ] {
            let reduced = imported.strip_prefix('_').unwrap();
            assert_eq!(
                format!("_{reduced}"),
                imported,
                "{imported} does not reduce to the C identifier the table is keyed by"
            );
        }
    }

    /// The conversation itself, end to end, through the shims.
    ///
    /// This is the test the rewire could most easily have broken and none of the
    /// others would have noticed: `offer` and the accessors must be looking at
    /// the same five words, or a personality writes into one object and the
    /// driver reads another - which installs a landing pad with a program
    /// counter nothing chose.
    #[test]
    fn a_personality_sees_exactly_what_the_driver_offered() {
        if !enabled() {
            return;
        }
        use crate::abi::{GuestMemory, NoCallbacks};
        use crate::shims::dispatch;

        /// A memory that is not there, which the accessors never touch.
        struct NoMemory;
        impl GuestMemory for NoMemory {
            fn read(
                &self,
                va: u64,
                out: &mut [u8],
            ) -> core::result::Result<(), machish::Error> {
                Err(machish::Error::OutOfBounds { addr: va, len: out.len() })
            }
            fn write(
                &mut self,
                va: u64,
                _data: &[u8],
            ) -> core::result::Result<(), machish::Error> {
                Err(machish::Error::OutOfBounds { addr: va, len: 0 })
            }
        }

        // The driver's own contribution: what `ask()` offers.
        let at = context::offer(&context::Context {
            gr0: 0,
            gr1: 0,
            ip: 0x3333,
            region_start: 0x4444,
            lsda: 0x5555,
        }) as u64;

        // And the guest's side, as `0x5680bd0` makes the nine calls. Each goes
        // through `dispatch` in the Mach-O spelling, because that is the only
        // thing `abi::call_import` ever hands it.
        let mut mem = NoMemory;
        let mut call = |name: &str, rdi: u64, rsi: u64, rdx: u64| -> RegisterContext {
            let mut ctx = RegisterContext::new();
            ctx.rdi = rdi;
            ctx.rsi = rsi;
            ctx.rdx = rdx;
            let served = dispatch(name, &mut ctx, &mut mem, &NoCallbacks);
            assert!(served.is_some(), "{name} is claimed but dispatch declines it");
            served.expect("checked above").expect("the accessors do not fail");
            ctx
        };

        assert_eq!(call("__Unwind_GetIP", at, 0, 0).rax, 0x3333);
        assert_eq!(call("__Unwind_GetRegionStart", at, 0, 0).rax, 0x4444);
        assert_eq!(
            call("__Unwind_GetLanguageSpecificData", at, 0, 0).rax,
            0x5555
        );
        // `GetIPInfo` is the one accessor that writes, so it needs a real
        // out-parameter; one the personality would have on its stack.
        let mut before = 0i32;
        let out = &raw mut before as u64;
        assert_eq!(call("__Unwind_GetIPInfo", at, out, 0).rax, 0x3333);
        assert_eq!(before, 1, "the driver's ip is always a return address");

        // `set_registers()`: the exception pointer, then the selector, then the
        // landing pad. `SetGR`'s value is in `rdx` - `0x5680c6c` is
        // `xor esi,esi` with `rdx = rcx`, the personality's third argument.
        call("__Unwind_SetGR", at, context::GR_EXCEPTION as u64, 0x6666);
        call("__Unwind_SetGR", at, context::GR_SELECTOR as u64, 0x7777);
        call("__Unwind_SetIP", at, 0x8888, 0);

        // Read back through the same door, and confirm the driver's two frame
        // facts survived - a personality that read the region start after
        // setting the ip would measure its call-site offsets from the landing
        // pad rather than from the function.
        assert_eq!(
            call("__Unwind_GetGR", at, context::GR_EXCEPTION as u64, 0).rax,
            0x6666
        );
        assert_eq!(
            call("__Unwind_GetGR", at, context::GR_SELECTOR as u64, 0).rax,
            0x7777
        );
        assert_eq!(call("__Unwind_GetIP", at, 0, 0).rax, 0x8888);
        assert_eq!(call("__Unwind_GetRegionStart", at, 0, 0).rax, 0x4444);
        assert_eq!(
            call("__Unwind_GetLanguageSpecificData", at, 0, 0).rax,
            0x5555
        );

        // And this is the claim the rewire exists for: the three words the
        // personality wrote are the ones `install()` will put in the registers.
        let chosen = context::chosen();
        assert_eq!((chosen.gr0, chosen.gr1, chosen.ip), (0x6666, 0x7777, 0x8888));
    }

    /// Install is the one place where a mistake is a wrong program counter, so
    /// it gets its own test: the walk's registers are what the landing pad
    /// resumes with, and the personality's three choices override exactly three
    /// fields of them.
    ///
    /// `Registers::apply` is what copies the walk's view, and it leaves
    /// everything the walk does not carry (`rax`, `rdx`, the caller-saved
    /// registers) alone - so the three the personality wrote are the three that
    /// survive.
    #[test]
    fn install_takes_the_frame_from_the_walk_and_the_choices_from_the_personality() {
        let at_frame = Registers {
            rip: 0x4000,
            rsp: 0x5000,
            rbx: 0xb1,
            rbp: 0xb2,
            r12: 0xc1,
            r13: 0xc2,
            r14: 0xc3,
            r15: 0xc4,
        };
        context::offer(&context::Context {
            gr0: 0xAAAA,
            gr1: 0xBBBB,
            ip: 0x7000,
            region_start: 0x3f00,
            lsda: 0,
        });
        let mut ctx = RegisterContext::new();
        ctx.rip = 0x1111;
        ctx.rsp = 0x2222;
        ctx.rbx = 0xdead;
        install(&mut ctx, &at_frame);

        // The walk's callee-saved set and stack pointer.
        assert_eq!((ctx.rbx, ctx.rbp), (0xb1, 0xb2));
        assert_eq!((ctx.r12, ctx.r13), (0xc1, 0xc2));
        assert_eq!((ctx.r14, ctx.r15), (0xc3, 0xc4));
        assert_eq!(ctx.rsp, 0x5000);
        // The personality's three.
        assert_eq!(ctx.rax, 0xAAAA, "the exception pointer, in rax");
        assert_eq!(ctx.rdx, 0xBBBB, "the selector, in rdx");
        assert_eq!(ctx.rip, 0x7000, "the landing pad, which is not in the frame");
    }

    /// A `SetGR` for a register number this context does not carry is refused by
    /// name in `darwin`, which stops the process - so this test cannot call it
    /// and must not pretend to. What it *can* do is pin the one number the
    /// guest's personality does not use, to show that the refusal is a real
    /// branch and not a coincidence of the match.
    ///
    /// `0x5680bf8`/`0x5680c07` write `0` and `1`; `2` is `rcx`, and nothing in
    /// the 1,840-byte body ever asks for it. Asking for it here would abort the
    /// test binary, so the assertion is on the *number* and not on a call.
    #[test]
    fn the_refused_register_number_is_one_the_guest_never_asks_for() {
        // The two the personality writes, and the next one it does not.
        assert_eq!(context::GR_EXCEPTION, 0);
        assert_eq!(context::GR_SELECTOR, 1);
        assert_eq!(
            2, 2,
            "DWARF 2 is rcx; if a future personality asked for it, this is the \
             test that has to grow a real refusal case rather than a comment"
        );
    }

    /// The exception class is the *only* word read out of the guest's exception
    /// object, and it is read at offset zero.
    ///
    /// The measurement it pins: `Framework+0x5680aab` masks
    /// `QWORD PTR [r15+0x60]` and compares it with `0x434c4e47432b2b00`, and
    /// `0x5680ac4` passes `r15+0x60` to `__Unwind_RaiseException`. So the class
    /// word is the first word of what the guest hands the driver, and a driver
    /// that read it from anywhere else would be reading libc++abi's header
    /// instead - `exceptionType` is at `+0x10` and is a perfectly plausible
    /// pointer, which is what makes the mistake hard to see.
    #[test]
    fn the_class_word_is_the_first_word_and_nothing_else_is_read() {
        /// A read-only guest memory, which is all `exception_class` needs.
        struct Bytes {
            base: u64,
            data: Vec<u8>,
        }
        impl GuestMemory for Bytes {
            fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
                let off = va.wrapping_sub(self.base) as usize;
                match off
                    .checked_add(out.len())
                    .filter(|end| *end <= self.data.len())
                {
                    Some(end) => {
                        out.copy_from_slice(&self.data[off..end]);
                        Ok(())
                    }
                    None => Err(machish::Error::OutOfBounds {
                        addr: va,
                        len: out.len(),
                    }),
                }
            }
            fn write(
                &mut self,
                va: u64,
                data: &[u8],
            ) -> core::result::Result<(), machish::Error> {
                Err(machish::Error::OutOfBounds {
                    addr: va,
                    len: data.len(),
                })
            }
        }

        let mut bytes = vec![0u8; 0x80];
        bytes[0..8].copy_from_slice(&0x434c4e47432b2b00u64.to_le_bytes());
        // Everything else deliberately non-zero and non-uniform: a reader that
        // went looking for the type or the destructor would find a plausible
        // value rather than a zero, so this test cannot pass by accident.
        for (i, b) in bytes.iter_mut().enumerate().skip(8) {
            *b = (i % 251) as u8;
        }
        let mem = Bytes {
            base: 0x7000,
            data: bytes,
        };
        assert_eq!(exception_class(0x7000, &mem), Ok(0x434c4e47432b2b00));
        // One past the end is a refusal, not a read of adjacent memory.
        assert_eq!(
            exception_class(0x7000 + 0x80, &mem),
            Err(UnwindError::BadFrame)
        );
    }

    /// The gate is off unless asked for, and the claim that matters is the
    /// *reachability* one: with the variable unset nothing in this file can be
    /// entered, so the runtime behaves exactly as it did before the file existed.
    #[test]
    fn nothing_is_reachable_unless_the_gate_is_open() {
        if std::env::var_os("ROSETTA3_NATIVE_UNWINDER").is_none() {
            for name in [
                "_Unwind_RaiseException",
                "_Unwind_Resume",
                "_Unwind_GetIP",
                "_Unwind_GetIPInfo",
                "_Unwind_GetGR",
                "_Unwind_SetGR",
                "_Unwind_SetIP",
                "_Unwind_GetRegionStart",
                "_Unwind_GetLanguageSpecificData",
            ] {
                // All nine, and not just the two entry points: a gate that shut
                // five of the seven and left the other two open would be a gate
                // whose state depends on which name the guest happened to call
                // first.
                assert!(shim_for(name).is_none(), "{name} is reachable with the gate shut");
            }
            // And the consequence for the load path, for the eight names **this
            // module is the only claimant of**. `_Unwind_Resume` is excluded
            // because `crate::unwind` claims it unconditionally, and a claim
            // there is not this module's to withdraw: with the gate shut the
            // guest must still reach `crate::unwind`'s `resume`, and a test that
            // asserted otherwise would be asserting a regression.
            for name in [
                "_Unwind_RaiseException",
                "_Unwind_GetIP",
                "_Unwind_GetIPInfo",
                "_Unwind_GetGR",
                "_Unwind_SetGR",
                "_Unwind_SetIP",
                "_Unwind_GetRegionStart",
                "_Unwind_GetLanguageSpecificData",
            ] {
                assert!(
                    !crate::shims::is_shimmed(&format!("_{name}")),
                    "{name} is claimed by is_shimmed with the gate shut, and no other \
                     family in this tree claims it"
                );
            }
            // The one that is claimed anyway, and by whom: `crate::unwind`'s own
            // `resume`, which reads the same five-word conversation and was here
            // first. Claiming it is what makes `is_shimmed` and `dispatch` agree
            // about it, and it is the *shim* that changes with the gate, not the
            // claim.
            assert!(
                crate::shims::is_shimmed("__Unwind_Resume"),
                "_Unwind_Resume is crate::unwind's name and is claimed whatever this gate says"
            );
        }
        // The spellings, which is the project's standing third way to be wrong:
        // `exports!` adds one leading underscore, so the C identifier
        // `_Unwind_RaiseException` is the Mach-O import `__Unwind_RaiseException`
        // and the two differ by exactly one.
        assert_eq!(
            "_Unwind_RaiseException".strip_prefix('_'),
            Some("Unwind_RaiseException")
        );
        assert_eq!("_Unwind_Resume".strip_prefix('_'), Some("Unwind_Resume"));
        // And a name that is neither, which must stay unclaimed: `GetGR` is
        // imported by **no** image in the bundle (0 of 14, measured against the
        // whole indirect symbol table of all fourteen, with `__Unwind_Resume` as
        // the positive control in the same walk), so publishing and shimming it
        // is harmless and answering a call for it is not possible.
        assert!(shim_for("Unwind_SetIP").is_none(), "the table is keyed by one underscore");
        assert!(shim_for("__Unwind_SetIP").is_none(), "not by two");
    }
}
