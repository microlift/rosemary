//! The `_Unwind_Context` this tree creates, and the seven accessors a
//! personality routine calls back through.
//!
//! These seven names used to be refusals in
//! [`crate::last_unavailable`], and the reason they were is worth keeping
//! because it is what makes them answerable *now*:
//!
//! > Nine of the twelve `_Unwind_*` names are one load and a `ret` in the
//! > host's `libgcc_s.so.1` — `_Unwind_GetIP` is
//! > `mov rax,[rdi+0x98]; ret` — and **not one of them calls
//! > `_Unwind_Find_FDE`**. They do not search unwind data; they report what an
//! > unwinder *wrote*. And the field they report from is **libgcc's**
//! > `struct _Unwind_Context` layout, which is not the one LLVM's
//! > `UnwindContext` uses.
//!
//! That argument is still true and is still the reason these rows must point at
//! **this file's** functions and must not be left to fall through to `dlsym`.
//! What has changed is the premise underneath it. The old premise was "no
//! unwinder ever walked, so there is no context": a host unwinder had been
//! asked the question and had nothing. **This tree now walks** —
//! `crates/rosetta3/src/unwind_native.rs` parses each guest image's
//! `__TEXT,__unwind_info`, steps a frame from it, and asks **that frame's own
//! personality routine** whether it catches. So there *is* a context, this crate
//! creates it, this crate fills it, and only these accessors ever read it.
//!
//! # Why the layout is five words and not libunwind's kilobytes
//!
//! Because the context is **opaque to the party that uses it**, and that is a
//! measurement rather than an assumption. The personality routine in use is
//! `__gxx_personality_v0` at `Framework+0x5680bd0`, 1,840 bytes, and over all of
//! it:
//!
//! * **zero** memory operands are based on the register the context arrived in.
//!   It is `r8`, moved to `r15` at `0x5680c0b`, spilled to `[rbp-0xb0]` at
//!   `0x5680c4f` and `0x5680d41`, reloaded into `rbx` at `0x56811c8` and passed
//!   as `rdi` to each accessor. An `e8`/operand scan of the whole body finds
//!   **no** `[r8…]`, **no** `[r15…]` except one, and **no** `[rbx…]` at all.
//! * the single exception, `movzx eax, BYTE PTR [r15]` at `0x5680f20`, is a
//!   ULEB128 decode loop (`and edx,0x7f; shl rdx,cl; inc r15; or r12,rdx;
//!   add rcx,7; test al,al; js 0x5680f20`) over the `__gcc_except_tab`, and
//!   `r15` was reassigned to a cursor at `0x5680ed6` (`lea r15,[r12+rsi*1];
//!   dec r15`).
//!
//! So the whole protocol is nine calls: `SetGR` ×4, `SetIP` ×2,
//! `GetLanguageSpecificData`, `GetIP`, `GetRegionStart` — counted by scanning
//! the body for `call` against the twelve `__TEXT,__stubs` addresses. The
//! guest's *other* copy of the same routine, `0x417640`, adds `GetIPInfo` and
//! changes nothing else. `GetGR` and `GetIPInfo` are imported and never called
//! by the personality in use; both are published because the image imports them
//! and a name the loader binds should be bound to an answer rather than to
//! `libgcc_s`'s.
//!
//! # What the context therefore holds
//!
//! Five words, and each is measured out of a call site rather than out of a
//! header — because the two layouts differ and this crate's is the one that
//! matters:
//!
//! | field | read by | evidence |
//! |---|---|---|
//! | `gr0` | `SetGR(ctx, 0, exception_object)` | `0x5680c6c` is `xor esi,esi` with `rdx = rcx`, the personality's third argument |
//! | `gr1` | `SetGR(ctx, 1, selector)` | `0x5680c79` is `mov esi,0x1` with `rdx = rbx`, i.e. `[r13-0x24]`, the handler switch value |
//! | `ip` | `SetIP(ctx, landing_pad)` | `0x5680c89` is `mov rsi,r14` with `r14 = [r13-0x10]`, the address the personality found |
//! | `region_start` | `GetRegionStart(ctx)` | `0x5680cdd`, result in `r9`; it is the base the call-site table's offsets are from |
//! | `lsda` | `GetLanguageSpecificData(ctx)` | `0x5680cb9`, result in `rax`, tested for null at `0x5680cbe` and then **dereferenced** at `0x5680cf6` (`movzx eax, BYTE PTR [r13+0x0]`) |
//!
//! **`lsda` is a guest address and has to be one.** The personality reads its
//! first byte and walks it as a ULEB table, so a null is the only value that
//! means "no handler table here" — `0x5680cbe` turns a null into
//! `_URC_CONTINUE_UNWIND` at `0x5681264` — and any non-null value must be
//! readable guest memory. Pointing it at one of this struct's own fields would
//! hand the personality five bytes of zeroes to parse as a type table.
//!
//! # `ip` is a return address, and `GetIPInfo` says so
//!
//! `GetIP` returns the frame's program counter **as the unwinder recorded it**,
//! which for every frame a personality is asked about is a return address. The
//! Itanium rule is that the *caller* turns that into a call-site offset by
//! subtracting one, and the guest's own second copy shows exactly that
//! arithmetic: `0x41768e` is `cmp DWORD PTR [rbp-0x2c],0x1; sbb r12,0x0` —
//! `ip - 1` when `ip_before` is 1, `ip` when it is 0. So `GetIPInfo` writes
//! `1`, and `GetIP` must not pre-decrement: a driver that subtracted here would
//! have the personality subtract again and land one byte before every call.
//!
//! # A register number this context does not carry is refused, not dropped
//!
//! DWARF 0 and 1 are `rax` and `rdx` on x86-64, and they are the only two the
//! personality writes, because a landing pad's contract is those two
//! (`__builtin_eh_return_data_regno(0)` and `(1)`). Dropping a write to any
//! other number is not a lost value — it is a landing pad resuming with a
//! program counter and a `catch` selector that **nothing chose**, which is the
//! failure this project ranks above a wrong answer. So an unknown number stops
//! the process and says which number it was.
//!
//! # A context pointer this crate did not create is refused, not read
//!
//! [`offered_on`] is the only [`Context`] pointer the accessors will touch, and
//! every one of them checks. That check is not defensive noise: it is what makes
//! this file safe to land **before** the driver is pointed at it. Until
//! `crates/rosetta3/src/unwind_native.rs` offers frames through [`offer`], a
//! personality routine is handed a context pointer this crate never made, and
//! the truthful response to that is a refusal naming the missing wiring — not a
//! read of five words of whatever the caller had.
//!
//! It is also the check that keeps the host's layout out of reach. If these
//! names were left unbound, `darwin::symbol` would fall through to `dlsym`, the
//! loader would bind `libgcc_s.so.1`, and `_Unwind_GetIP` would be
//! `mov rax,[rdi+0x98]; ret` against a struct whose `+0x98` is a couple of
//! kilobytes past anything this file allocates.

use std::cell::Cell;
use std::ffi::c_void;

use super::imp::{refuse, said_unwind};

/// The DWARF register number of `rax` on x86-64, which is what a landing pad
/// receives its exception pointer in.
pub const GR_EXCEPTION: i32 = 0;

/// The DWARF register number of `rdx` on x86-64, which is what a landing pad
/// receives its `catch` selector in.
///
/// The pairing is not a choice: `__gxx_personality_v0`'s `set_registers()`
/// writes `__builtin_eh_return_data_regno(0)` and `__builtin_eh_return_data_regno(1)`,
/// and the guest's copy writes exactly this pair at `0x5680c6c`/`0x5680c79` and
/// again at `0x56811d2`/`0x56811df`. Reversing them installs a landing pad with
/// a selector in `rax` and an exception pointer in `rdx`, and the `catch` then
/// silently never matches.
pub const GR_SELECTOR: i32 = 1;

/// What a personality routine can see of the frame being offered to it.
///
/// Five words, and the module note is the argument for why five is enough. The
/// layout is this crate's because this crate creates the context; a
/// `_Unwind_Context` is opaque to everyone except its creator and the
/// accessors, and both of those are here.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// DWARF 0. The exception pointer, which a landing pad receives in `rax`.
    pub gr0: u64,
    /// DWARF 1. The selector, which a landing pad receives in `rdx`.
    pub gr1: u64,
    /// Where this frame is executing. A return address, always — see the note on
    /// `GetIPInfo`.
    pub ip: u64,
    /// Where this frame's code starts: the base its call-site offsets are from.
    pub region_start: u64,
    /// This frame's `__gcc_except_tab`, a **guest** address, or zero for a frame
    /// with no `try`.
    pub lsda: u64,
}

thread_local! {
    /// The context of the frame currently being offered to a personality.
    ///
    /// A thread-local rather than a `Box` so the pointer handed to guest code
    /// cannot dangle: the guest only ever passes it back, and every access goes
    /// through an accessor here, which checks the pointer before touching it.
    static CURRENT: Cell<Context> = const { Cell::new(Context {
        gr0: 0,
        gr1: 0,
        ip: 0,
        region_start: 0,
        lsda: 0,
    }) };
}

/// The address to hand a personality routine as its `context` argument.
///
/// The seventh word of the conversation, and the only one that costs anything:
/// it is the address of a thread-local, so it is stable for as long as the
/// thread is and no allocation can be freed out from under the guest.
pub fn pointer() -> *mut c_void {
    CURRENT.with(|c| c as *const Cell<Context> as *mut c_void)
}

/// Offer one frame to its personality routine.
///
/// The driver calls this immediately before entering the routine, with the
/// frame's own three facts: where it is executing, where its code starts, and
/// which `__gcc_except_tab` (if any) its `try` clauses are described by. It
/// returns the same address [`pointer`] hands out, so the two cannot disagree
/// about which context the guest is about to be given.
///
/// A frame with no `try` has `lsda == 0`, and that is not a missing value: the
/// personality turns a null language-specific pointer into
/// `_URC_CONTINUE_UNWIND` (`0x5680cbe` → `0x5681264`) and walks outwards, which
/// is the correct outcome for a frame that cannot catch anything.
pub fn offer(frame: &Context) -> *mut c_void {
    CURRENT.with(|c| c.set(*frame));
    pointer()
}

/// The three choices a personality routine made, as the context now holds them.
///
/// The driver's install path reads this and nothing else, because these are the
/// only fields a personality is allowed to change: it writes `gr0`, `gr1` and
/// `ip` and reads `region_start` and `lsda`.
pub fn chosen() -> Context {
    CURRENT.with(Cell::get)
}

/// Offer `frame` and run `f` against the context, as a personality would.
///
/// The one place a write is allowed to happen, so the pointer check and the
/// read-modify-write cannot come apart.
fn with_context<R>(at: *mut c_void, f: impl FnOnce(&mut Context) -> R) -> R {
    let at = at as u64;
    CURRENT.with(|c| {
        if at != c as *const Cell<Context> as u64 {
            refuse(
                &said_unwind::CONTEXT_NOT_OURS,
                "a personality routine was handed a context pointer this crate did not \
                 create, and no accessor can answer it. The driver in \
                 crates/rosetta3/src/unwind_native.rs has to offer each frame through \
                 darwin::last_unavailable::unwind_context::offer() and read the \
                 personality's choices back through chosen(); until it does, the \
                 context it hands the guest is not the one these accessors read.",
            );
        }
        let mut ctx = c.get();
        let out = f(&mut ctx);
        c.set(ctx);
        out
    })
}

/// The frame currently being offered, for the accessors that only read.
///
/// A *copy*, taken through [`with_context`] rather than around it, so that a
/// reader runs the same pointer check a writer does. A personality that reads
/// four fields and writes three must not be able to skip the check by only
/// reading, and it must not be able to lose the three the writer installed to a
/// reader's stale copy.
fn reading(at: *mut c_void) -> Context {
    with_context(at, |c| *c)
}

// ------------------------------------------------------------------ the accessors

/// `_Unwind_Ptr _Unwind_GetIP(struct _Unwind_Context *context)`
///
/// The frame's program counter as the unwinder recorded it, which is a return
/// address. The caller turns it into a call-site offset; see the module note.
pub unsafe extern "C" fn unwind_get_ip(context: *mut c_void) -> u64 {
    reading(context).ip
}

/// `_Unwind_Ptr _Unwind_GetIPInfo(struct _Unwind_Context *context, int *ip_before)`
///
/// The same value, plus the answer to "is this the instruction *after* a call?".
/// It writes `1` because every frame this driver offers a personality is a
/// frame the walk *stepped into*, so its `ip` is a return address by
/// construction — and saying so is what makes the personality search from
/// `ip - 1` rather than from `ip`.
///
/// `ip_before` is optional on a real system, so a null one is not written: the
/// caller that passes null is a caller that does not want the answer.
pub unsafe extern "C" fn unwind_get_ip_info(context: *mut c_void, ip_before: *mut i32) -> u64 {
    let ip = reading(context).ip;
    if !ip_before.is_null() {
        // SAFETY: the caller's own out-parameter, which it passed non-null.
        unsafe { *ip_before = 1 };
    }
    ip
}

/// `_Unwind_Word _Unwind_GetGR(struct _Unwind_Context *context, int regno)`
///
/// Nothing in this image calls it — an `e8`-scan of `__TEXT,__text` against its
/// `__TEXT,__stubs` address finds **zero** sites, with `__Unwind_Resume` (3,581)
/// and `___cxa_atexit` (251) as the positive controls in the same scan — so
/// which numbers a caller might ask for is not measurable from the guest. The
/// two the personality writes are carried; a third is refused by name rather
/// than answered with a zero, because a zero here is a register nothing wrote.
pub unsafe extern "C" fn unwind_get_gr(context: *mut c_void, regno: i32) -> u64 {
    match regno {
        GR_EXCEPTION => reading(context).gr0,
        GR_SELECTOR => reading(context).gr1,
        other => refuse_register(other, "_Unwind_GetGR"),
    }
}

/// `void _Unwind_SetGR(struct _Unwind_Context *context, int regno, _Unwind_Word val)`
///
/// What `__gxx_personality_v0`'s `set_registers()` calls, four times per install,
/// with the exception pointer and the selector.
pub unsafe extern "C" fn unwind_set_gr(context: *mut c_void, regno: i32, value: u64) {
    with_context(context, |c| match regno {
        GR_EXCEPTION => c.gr0 = value,
        GR_SELECTOR => c.gr1 = value,
        other => refuse_register(other, "_Unwind_SetGR"),
    });
}

/// `void _Unwind_SetIP(struct _Unwind_Context *context, _Unwind_Ptr ip)`
///
/// The unwinder being told **where to resume**, so a value that is merely wrong
/// is a jump.
pub unsafe extern "C" fn unwind_set_ip(context: *mut c_void, ip: u64) {
    with_context(context, |c| c.ip = ip);
}

/// `_Unwind_Ptr _Unwind_GetRegionStart(struct _Unwind_Context *context)`
///
/// The name that looks most like a stack-trace helper and is the least like
/// one: it is the *function* a personality routine measures its call-site
/// offsets from, and a personality given the wrong function start finds the
/// wrong landing pad.
pub unsafe extern "C" fn unwind_get_region_start(context: *mut c_void) -> u64 {
    reading(context).region_start
}

/// `void *_Unwind_GetLanguageSpecificData(struct _Unwind_Context *context)`
///
/// A **guest** pointer to this frame's `__gcc_except_tab`, or null. The
/// personality dereferences the answer immediately — `0x5680cf6` is
/// `movzx eax, BYTE PTR [r13+0x0]`, and `0x4176b7` is the same read in the
/// image's other copy — and uses the first byte to choose between the
/// exception-table and the `0xff` clean-up form. This is where a personality
/// routine picks which ABI it is speaking, and it is why the answer cannot be
/// one of this struct's own fields.
pub unsafe extern "C" fn unwind_get_language_specific_data(context: *mut c_void) -> *mut c_void {
    reading(context).lsda as *mut c_void
}

/// A DWARF register number this context does not carry.
///
/// `-> !` in both arms, and the reason is the module's: a dropped `SetGR` is a
/// landing pad resuming with a program counter and a selector that nothing
/// chose, and a `GetGR` that answered `0` is a claim about a register nobody
/// wrote. Neither is a "no answer"; both are instructions.
fn refuse_register(regno: i32, name: &str) -> ! {
    refuse(
        &said_unwind::REGISTER_NOT_CARRIED,
        &format!(
            "{name}: the personality asked for DWARF register {regno}. This context carries \
             only {GR_EXCEPTION} (rax) and {GR_SELECTOR} (rdx), because those are the only \
             two the personality routine in use writes - __gxx_personality_v0 at \
             Framework+0x5680bd0 calls SetGR with esi=0 at 0x5680c6c and esi=1 at \
             0x5680c79, and again at 0x56811d2 and 0x56811df. A value in any other \
             register would be a landing pad resuming with something nothing chose, so \
             the write is refused rather than dropped."
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conversation, driven exactly as the personality drives it.
    ///
    /// Through the `extern "C"` entry points rather than the Rust functions,
    /// because those are what guest code calls and a signature mistake there is
    /// invisible from inside the crate. The order is the order of
    /// `Framework+0x5680bd0`: read the language-specific data, read the ip, read
    /// the region start, then — on the install path — write both registers and
    /// the ip.
    #[test]
    fn a_personality_sees_exactly_what_the_driver_offered() {
        let at = offer(&Context {
            gr0: 0,
            gr1: 0,
            ip: 0x3333,
            region_start: 0x4444,
            lsda: 0x5555,
        });

        // SAFETY: the pointer `offer` just returned, which is what the accessors
        // require, and one real out-parameter.
        unsafe {
            // The null is not an error: it is `_URC_CONTINUE_UNWIND`, and a frame
            // with no `try` is the common case.
            assert_eq!(unwind_get_language_specific_data(at) as u64, 0x5555);
            assert_eq!(unwind_get_ip(at), 0x3333);
            assert_eq!(unwind_get_region_start(at), 0x4444);

            let mut before = 0i32;
            assert_eq!(unwind_get_ip_info(at, &raw mut before), 0x3333);
            assert_eq!(before, 1, "the driver's ip is always a return address");

            // `set_registers()`: the exception pointer, then the selector.
            unwind_set_gr(at, GR_EXCEPTION, 0x6666);
            unwind_set_gr(at, GR_SELECTOR, 0x7777);
            unwind_set_ip(at, 0x8888);
            assert_eq!(unwind_get_gr(at, GR_EXCEPTION), 0x6666);
            assert_eq!(unwind_get_gr(at, GR_SELECTOR), 0x7777);
            assert_eq!(unwind_get_ip(at), 0x8888);

            // The frame facts are the driver's, and a `SetIP` must not have
            // disturbed them: a personality that read the region start after
            // setting the ip would measure its call-site offsets from the landing
            // pad rather than from the function.
            assert_eq!(unwind_get_region_start(at), 0x4444);
            assert_eq!(unwind_get_language_specific_data(at) as u64, 0x5555);
        }
    }

    /// `chosen()` is the only path back to the driver, and it is the one that
    /// decides a program counter — so it is pinned on its own.
    #[test]
    fn what_the_personality_wrote_is_what_the_driver_would_install() {
        let at = offer(&Context {
            gr0: 0,
            gr1: 0,
            ip: 0x3333,
            region_start: 0x4444,
            lsda: 0x5555,
        });
        // SAFETY: the pointer `offer` returned, and register numbers 0 and 1.
        unsafe {
            unwind_set_gr(at, GR_EXCEPTION, 0xdead_beef);
            unwind_set_gr(at, GR_SELECTOR, 0x0bad_f00d);
            unwind_set_ip(at, 0xfeed_face);
        }
        let chosen = chosen();
        assert_eq!(chosen.gr0, 0xdead_beef, "the exception pointer, in rax");
        assert_eq!(chosen.gr1, 0x0bad_f00d, "the selector, in rdx");
        assert_eq!(chosen.ip, 0xfeed_face, "the landing pad");
        // And the two the driver supplied are still the driver's: a personality
        // that could change the region start would be able to move the frame it
        // is being asked about.
        assert_eq!(chosen.region_start, 0x4444);
        assert_eq!(chosen.lsda, 0x5555);
    }

    /// A frame with no `try` answers zero from the language-specific accessor,
    /// which the personality turns into `_URC_CONTINUE_UNWIND`.
    ///
    /// Zero is the whole claim: it is not a stand-in for a table this crate
    /// could not find, it is what a frame with no `try` has.
    #[test]
    fn a_frame_with_no_try_has_no_language_specific_data() {
        let at = offer(&Context {
            ip: 0x10,
            region_start: 0x20,
            ..Context::default()
        });
        // SAFETY: the pointer `offer` returned.
        assert!(unsafe { unwind_get_language_specific_data(at) }.is_null());
    }

    /// `ip_before` is optional, and a null one must not be written.
    #[test]
    fn the_ip_info_out_parameter_is_optional() {
        let at = offer(&Context {
            ip: 0xabcd,
            ..Context::default()
        });
        // SAFETY: the pointer `offer` returned, and a null out-parameter, which
        // the accessor checks before writing.
        assert_eq!(
            unsafe { unwind_get_ip_info(at, core::ptr::null_mut()) },
            0xabcd
        );
    }

    /// The two register numbers, pinned to the DWARF numbering and to the guest's
    /// own two `mov esi` instructions.
    #[test]
    fn the_written_registers_are_rax_and_rdx_in_dwarfs_numbering() {
        assert_eq!(GR_EXCEPTION, 0, "0x5680c6c: xor esi,esi");
        assert_eq!(GR_SELECTOR, 1, "0x5680c79: mov esi,0x1");
    }

    /// The context is five words of `u64` with no padding, so the address the
    /// guest is handed and the fields the accessors read are the same object.
    ///
    /// A `repr(C)` struct of five `u64`s is 40 bytes and 8-aligned; the
    /// assertion is here so that adding a field, or changing one to something
    /// narrower, is a compile error rather than a guest reading a shifted word.
    #[test]
    fn the_context_is_five_unpadded_words() {
        assert_eq!(std::mem::size_of::<Context>(), 40);
        assert_eq!(std::mem::align_of::<Context>(), 8);
        // And the thread-local's address is the one handed out, so the guest's
        // pointer is stable across a whole offer-and-answer conversation.
        let at = pointer();
        let again = offer(&Context::default());
        assert_eq!(at, again, "the context pointer moved under the guest");
    }
}
