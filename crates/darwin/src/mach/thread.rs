#![cfg(not(target_os = "macos"))]
//! `thread_get_state`, and the two calls that give it meaning.
//!
//! ## The shape of the question
//!
//! `thread_get_state(target, flavor, out_state, out_stateCnt)` asks for a
//! thread's *machine state*: the registers a thread was holding when the kernel
//! last stopped it. It is the one Mach call whose answer is not a fact about
//! the operating system's data structures but a fact about the CPU, and a
//! caller that gets it wrong does not find out - it walks the wrong stack. A
//! stack unwinder reading a `rip` that belongs to another thread produces frames
//! that look entirely reasonable and are entirely fiction.
//!
//! So the only answer this runtime gives is one it can prove.
//!
//! ## Which namespace a `target` is in
//!
//! There are two, they are disjoint, and they both call themselves a thread
//! port:
//!
//! * [`crate::pthread::id`] - what `pthread_mach_thread_np(pthread_self())`
//!   hands out, which is a kernel thread id on this host and a Mach port on a
//!   Mac. [`crate::pthread::mach_thread_np`] is the same value.
//! * [`super::task::mach_thread_self`] - what `mach_thread_self()` hands out,
//!   a name from [`super::space`]'s table, which starts at `0x103` and knows
//!   nothing about threads.
//!
//! Chromium's profiler uses the first, demonstrably: `base::profiler`'s
//! `SuspendableThreadDelegateMac` (Chromium `130.0.6723.91`,
//! `base/profiler/suspendable_thread_delegate_mac.cc`) is handed a
//! `SamplingProfilerThreadToken` whose `id` is
//! `pthread_mach_thread_np(pthread_self())`, and its constructor immediately
//! asks for that thread's state.
//!
//! So that is the namespace [`thread_get_state`] accepts, and it accepts it for
//! **one** value in it: the calling thread's own. A name from the other
//! namespace names no thread this runtime keeps registers for, and the honest
//! answer there is a refusal - see [`admit`]. Reconciling the two namespaces
//! belongs in `super::space`, and a cross-namespace answer here would be a
//! plausible wrong one.
//!
//! ## Why the answer can be real
//!
//! Darwin's kernel traps into the kernel *after* the caller has written the
//! arguments, so the state it hands back has `rdi`, `rsi`, `rdx` and `rcx`
//! holding the arguments, `rip` holding the return address, and `rsp` holding
//! the stack pointer the caller had at the `call`. A callee that reproduces
//! those four registers and the rest of the frame around them reproduces the
//! kernel's answer exactly - which is why this module's entry point is
//! assembly, and why the register-save area it builds is a copy of the
//! registers as they were on its *first* instruction. `rbp` is the reason a
//! `#[inline(never)]` Rust function cannot do it: a frame pointer would be
//! pushed before any Rust could read it.
//!
//! ## What is not answered
//!
//! [`thread_suspend`] and [`thread_resume`], and *why* they are refused rather
//! than approximated, are at the bottom of this file. Reading another thread's
//! registers on this host needs `PTRACE_ATTACH` plus `PTRACE_GETREGS`, and
//! `yama/ptrace_scope` is `1` here, which refuses a `PTRACE_ATTACH` to a sibling
//! thread of the tracer's own process. That is a property of the host, not of
//! this code, and it is checked rather than assumed:
//!
//! ```text
//! $ cat /proc/sys/kernel/yama/ptrace_scope
//! 1
//! $ ./pt2          # attach to a sibling thread / to a forked child
//! self ATTACH -> -1 errno 1 (Operation not permitted)
//! child ATTACH -> 0 errno 0 (Success)
//! ```
//!
//! A refusal is the right answer rather than a stop: the caller here is
//! Chromium's stack sampler, whose own `base::profiler::StackSampler::
//! RecordStackFrames` records an empty frame list and returns when
//! `StackCopier::CopyStack` fails, and whose `ScopedSuspendThread` records
//! `MACH_PORT_NULL` when `thread_suspend` does not return `KERN_SUCCESS`. Both
//! are the paths this runtime's host already reaches every time a thread fails
//! to suspend, and neither is a fault.

use super::kern::{KERN_FAILURE, KERN_INVALID_ADDRESS, KERN_INVALID_ARGUMENT, KERN_SUCCESS};
use super::space::Name;

/// The `THREAD_STATE_*` flavour numbers, from `osfmk/mach/i386/thread_status.h`.
///
/// Only [`X86_THREAD_STATE64`] is answered. The rest are named so that a
/// refusal can say which flavour it was refusing rather than only that it was.
pub const X86_THREAD_STATE32: i32 = 1;
pub const X86_FLOAT_STATE32: i32 = 2;
pub const X86_EXCEPTION_STATE32: i32 = 3;
pub const X86_THREAD_STATE64: i32 = 4;
pub const X86_FLOAT_STATE64: i32 = 5;
pub const X86_EXCEPTION_STATE64: i32 = 6;
pub const X86_THREAD_STATE: i32 = 7;
pub const X86_FLOAT_STATE: i32 = 8;
pub const X86_EXCEPTION_STATE: i32 = 9;
pub const X86_DEBUG_STATE32: i32 = 10;
pub const X86_DEBUG_STATE64: i32 = 11;
pub const X86_DEBUG_STATE: i32 = 12;
pub const THREAD_STATE_NONE: i32 = 13;

/// `THREAD_STATE_REGS64`, the machine-independent spelling of
/// [`X86_THREAD_STATE64`] on Intel, and the same number again as
/// `INTEL_THREAD_STATE64`.
///
/// The guest passes the architecture-specific spelling - Chromium's
/// `GetThreadContextImpl` is `thread_get_state(target, kThreadStateFlavor, …)`
/// with `kThreadStateFlavor = x86_THREAD_STATE64` - but a guest that reached for
/// the portable one would be asking for the same thing and must not be told it
/// asked for something else. Hence one constant, three names.
pub const THREAD_STATE_REGS64: i32 = X86_THREAD_STATE64;

/// `x86_THREAD_STATE64_COUNT`, in 32-bit words - `sizeof` the structure divided
/// by `sizeof(int)`, so 168 bytes is 42 of them and not 40 or 44.
pub const X86_THREAD_STATE64_COUNT: u32 = 42;

/// The flavour a refusal names, or `None` for a number the header does not
/// define.
fn flavour(flavor: i32) -> Option<&'static str> {
    Some(match flavor {
        X86_THREAD_STATE32 => "x86_THREAD_STATE32",
        X86_FLOAT_STATE32 => "x86_FLOAT_STATE32",
        X86_EXCEPTION_STATE32 => "x86_EXCEPTION_STATE32",
        X86_THREAD_STATE64 => "x86_THREAD_STATE64",
        X86_FLOAT_STATE64 => "x86_FLOAT_STATE64",
        X86_EXCEPTION_STATE64 => "x86_EXCEPTION_STATE64",
        X86_THREAD_STATE => "x86_THREAD_STATE",
        X86_FLOAT_STATE => "x86_FLOAT_STATE",
        X86_EXCEPTION_STATE => "x86_EXCEPTION_STATE",
        X86_DEBUG_STATE32 => "x86_DEBUG_STATE32",
        X86_DEBUG_STATE64 => "x86_DEBUG_STATE64",
        X86_DEBUG_STATE => "x86_DEBUG_STATE",
        THREAD_STATE_NONE => "THREAD_STATE_NONE",
        _ => return None,
    })
}

/// `x86_thread_state64_t`, which is `_STRUCT_X86_THREAD_STATE64` in
/// `osfmk/mach/i386/_structs.h`:
///
/// ```c
/// _STRUCT_X86_THREAD_STATE64
/// {
///         __uint64_t __rax;   __uint64_t __rbx;   __uint64_t __rcx;   __uint64_t __rdx;
///         __uint64_t __rdi;   __uint64_t __rsi;   __uint64_t __rbp;   __uint64_t __rsp;
///         __uint64_t __r8;    __uint64_t __r9;    __uint64_t __r10;   __uint64_t __r11;
///         __uint64_t __r12;   __uint64_t __r13;   __uint64_t __r14;   __uint64_t __r15;
///         __uint64_t __rip;   __uint64_t __rflags;
///         __uint64_t __cs;    __uint64_t __fs;    __uint64_t __gs;
/// };
/// ```
///
/// **The last three are `cs`, `fs` and `gs` - there is no `ss` and no `efs` or
/// `egs`.** `ss` lives in `x86_thread_full_state64_t` (flavour 23) and so do
/// `ds` and `es`; `efs`/`egs` are the 32-bit structure's spelling of the
/// segment *selectors*, which on x86-64 are 64 bits wide, and `xnu`'s
/// conversion to this layout says so directly (`osfmk/i386/pcb.c`,
/// `thread_get_state`):
///
/// ```c
///         state->uts.ts64.rflags = saved_state->isf.rflags;
///         state->uts.ts64.cs = saved_state->isf.cs;
///         state->uts.ts64.fs = saved_state->fs & 0xffff;
///         state->uts.ts64.gs = saved_state->gs & 0xffff;
/// ```
///
/// so `fs` and `gs` here are **selectors, not bases** - on this host both are
/// zero, because in long mode the bases live in `IA32_FS_BASE`/`IA32_GS_BASE`
/// and only `gsbase` (flavour 23) carries one to a caller. `rosetta3`'s own
/// sampler reads `__rip` and nothing else, and
/// `base::profiler::RegisterContext` is this structure, so Chromium reads
/// `__rbx`, `__rbp`, `__rsp` and `__r12`-`__r15` (the seven registers its
/// unwinder rewrites), `__rsp` and `__rip`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ThreadState64 {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
    pub cs: u64,
    pub fs: u64,
    pub gs: u64,
}

/// Every field's byte offset, in declaration order.
///
/// The assembly below writes the structure by offset and cannot spell a
/// `const` operand from a table, so each is handed to the assembler as its own
/// `const` operand and this table is what pins them to the header.
macro_rules! count_idents {
    ($head:ident $(, $tail:ident)*) => { 1 + count_idents!($($tail),*) };
    () => { 0 };
}
macro_rules! offsets {
    ($($field:ident),* $(,)?) => {
        /// Byte offset of each field, in the order the header declares them.
        const OFFSETS: [usize; count_idents!($($field),*)] = [$(
            std::mem::offset_of!(ThreadState64, $field)
        ),*];
    };
}
offsets!(
    rax, rbx, rcx, rdx, rdi, rsi, rbp, rsp, r8, r9, r10, r11, r12, r13, r14, r15, rip, rflags, cs,
    fs, gs,
);

const _: () = {
    // Twenty-one natural-width words, and `x86_THREAD_STATE64_COUNT` of them.
    assert!(size_of::<ThreadState64>() == 168);
    assert!(X86_THREAD_STATE64_COUNT as usize * 4 == size_of::<ThreadState64>());
    // The structure is a packed sequence of eight-byte fields, so the offsets
    // the assembler writes at must be exactly `8 * index` - which is the check
    // that a field added, removed or reordered above is caught here rather than
    // by a guest's unwinder.
    let mut i = 0;
    while i < OFFSETS.len() {
        assert!(OFFSETS[i] == 8 * i);
        i += 1;
    }
    // The three the runtime and its guest both reach for by name. `rip` is the
    // seventeenth eight-byte field, so byte 128 - which is `rosetta3`'s own
    // `profile::PC_WORD * 8` and word 32 of the 42.
    assert!(std::mem::offset_of!(ThreadState64, rsp) == 0x38);
    assert!(std::mem::offset_of!(ThreadState64, rip) == 0x80);
    assert!(std::mem::offset_of!(ThreadState64, rflags) == 0x88);
    assert!(std::mem::offset_of!(ThreadState64, cs) == 0x90);
    assert!(std::mem::offset_of!(ThreadState64, fs) == 0x98);
    assert!(std::mem::offset_of!(ThreadState64, gs) == 0xa0);
};

/// Why a call was refused, said once so that a profiler which asks a thousand
/// times a second does not say it a thousand times.
///
/// The **return value is the caller's**, not this function's: the code a
/// refusal answers with is part of the contract and differs between the reasons
/// - `xnu` calls a null out-parameter an address, a short count an argument, and
/// a thread it cannot report a plain failure. So this only says, and each site
/// below returns its own number.
///
/// `write(2)` rather than `eprintln!`, for the reason `crate::malloc`'s
/// `abort_naming` gives: a message about a refusal has to be out before the
/// refusal is, and a buffered one can be lost. It does allocate to build the
/// line, which is safe here for a reason worth stating rather than leaving to be
/// assumed: this runtime suspends nothing, so no thread is ever stopped while
/// holding this tree's allocator lock. Were that to change, this would have to
/// become a fixed-size stack buffer.
fn note(symbol: &str, said: &std::sync::atomic::AtomicBool, why: &str) {
    use std::sync::atomic::Ordering;

    if !said.swap(true, Ordering::Relaxed) {
        let line = format!("rosetta3: {symbol} refused: {why}\n");
        // SAFETY: a live buffer for the length written.
        unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
    }
}

/// The decision [`THREAD_GET_STATE`] reaches before it publishes anything.
///
/// # Safety
///
/// Reached only from the assembly below, with the four arguments the caller
/// passed, and with every callee-saved register already in the spill frame the
/// assembly built. It writes to `out` and `*count` only after both have been
/// checked, and the assembly writes `out` only after this returns
/// `KERN_SUCCESS`, so a refusal leaves the caller's buffer exactly as it found
/// it.
unsafe extern "C" fn admit(target: Name, flavor: i32, out: *mut u32, count: *mut u32) -> i32 {
    static SAID_FLAVOUR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static SAID_SHORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static SAID_TARGET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    // `xnu` rejects a null `old_state` and a null count as an address, not an
    // argument, and the two are different numbers a caller may switch on.
    if out.is_null() || count.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    if flavor != X86_THREAD_STATE64 {
        note(
            "thread_get_state",
            &SAID_FLAVOUR,
            &format!(
                "flavor {flavor} ({}) - only x86_THREAD_STATE64 ({X86_THREAD_STATE64}), \
                 {X86_THREAD_STATE64_COUNT} words, is answered, and answering another with this \
                 one's layout would hand a guest a frame it cannot read",
                flavour(flavor).unwrap_or("not a thread-state flavour"),
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    // The count is in and out: the caller states its buffer's capacity in
    // 32-bit words, and `xnu` returns `KERN_INVALID_ARGUMENT` for a short one
    // rather than filling the part that fits.
    // SAFETY: `count` is the caller's writable `mach_msg_type_number_t`.
    let offered = unsafe { count.read() };
    if offered < X86_THREAD_STATE64_COUNT {
        note(
            "thread_get_state",
            &SAID_SHORT,
            &format!(
                "the caller offered {offered} of the {X86_THREAD_STATE64_COUNT} 32-bit words \
                 x86_THREAD_STATE64 is {X86_THREAD_STATE64_COUNT} words of"
            ),
        );
        return KERN_INVALID_ARGUMENT;
    }
    // The one thread whose registers are this thread's. See the module note for
    // why that is the calling thread and not some other.
    let mine = crate::pthread::id();
    if target != mine {
        note(
            "thread_get_state",
            &SAID_TARGET,
            &format!(
                "target {target:#x} does not name this thread ({mine:#x}) - this runtime \
                 reports the calling thread's own registers and no other thread's, \
                 because a second thread's state cannot be read on this host",
            ),
        );
        return KERN_FAILURE;
    }
    KERN_SUCCESS
}

core::arch::global_asm!(
    ".text",
    ".globl rosetta3_thread_get_state",
    ".hidden rosetta3_thread_get_state",
    ".type rosetta3_thread_get_state, @function",
    "rosetta3_thread_get_state:",
    // 176 bytes of register save area, rounded up to 184 so that `rsp` is
    // sixteen-byte aligned across the `call` below, as the SysV AMD64 ABI
    // requires at a call site.
    "sub rsp, 184",
    // The general-purpose registers, as they were on this function's first
    // instruction. A Rust prologue would have taken `rbp` by then.
    "mov [rsp + 0x00], rax",
    "mov [rsp + 0x08], rbx",
    "mov [rsp + 0x10], rcx",
    "mov [rsp + 0x18], rdx",
    "mov [rsp + 0x20], rdi",
    "mov [rsp + 0x28], rsi",
    "mov [rsp + 0x30], rbp",
    "mov [rsp + 0x38], r8",
    "mov [rsp + 0x40], r9",
    "mov [rsp + 0x48], r10",
    "mov [rsp + 0x50], r11",
    "mov [rsp + 0x58], r12",
    "mov [rsp + 0x60], r13",
    "mov [rsp + 0x68], r14",
    "mov [rsp + 0x70], r15",
    // `rsp` as the caller left it, which is the frame this call was made from,
    // and the return address the `call` pushed just below it.
    "mov rax, rsp",
    "add rax, 184",
    "mov [rsp + 0x78], rax",
    "mov rax, [rsp + 184]",
    "mov [rsp + 0x80], rax",
    // The flags, and the three segment selectors. Read as selectors, because
    // that is what the structure holds - see [`ThreadState64`]. `mov r64, Sreg`
    // rather than the other way round: only this direction is encodable in
    // 64-bit mode, and the other traps with `SIGILL`.
    "pushfq",
    "pop rax",
    "mov [rsp + 0x88], rax",
    "mov rax, cs",
    "mov [rsp + 0x90], rax",
    "mov rax, fs",
    "mov [rsp + 0x98], rax",
    "mov rax, gs",
    "mov [rsp + 0xa0], rax",
    // The decision. It may print, and nothing published so far can be taken
    // back, which is why it runs before the copy rather than after it.
    "call {admit}",
    "test eax, eax",
    "jne refused",
    // `rdx` for the base of every store, out of the spill frame: `admit` is
    // free to have overwritten it.
    "mov r8, [rsp + 0x18]",
    "mov rax, [rsp + 0x00]",
    "mov [r8 + {rax}], rax",
    "mov rax, [rsp + 0x08]",
    "mov [r8 + {rbx}], rax",
    "mov rax, [rsp + 0x10]",
    "mov [r8 + {rcx}], rax",
    "mov rax, [rsp + 0x18]",
    "mov [r8 + {rdx}], rax",
    "mov rax, [rsp + 0x20]",
    "mov [r8 + {rdi}], rax",
    "mov rax, [rsp + 0x28]",
    "mov [r8 + {rsi}], rax",
    "mov rax, [rsp + 0x30]",
    "mov [r8 + {rbp}], rax",
    // The caller's stack pointer, which is this frame's plus the eight bytes
    // the `call` pushed - the same value `xnu` copies out of the interrupt
    // frame's `rsp`.
    "mov rax, [rsp + 0x78]",
    "sub rax, 8",
    "mov [r8 + {rsp}], rax",
    "mov rax, [rsp + 0x38]",
    "mov [r8 + {r8}], rax",
    "mov rax, [rsp + 0x40]",
    "mov [r8 + {r9}], rax",
    "mov rax, [rsp + 0x48]",
    "mov [r8 + {r10}], rax",
    "mov rax, [rsp + 0x50]",
    "mov [r8 + {r11}], rax",
    "mov rax, [rsp + 0x58]",
    "mov [r8 + {r12}], rax",
    "mov rax, [rsp + 0x60]",
    "mov [r8 + {r13}], rax",
    "mov rax, [rsp + 0x68]",
    "mov [r8 + {r14}], rax",
    "mov rax, [rsp + 0x70]",
    "mov [r8 + {r15}], rax",
    "mov rax, [rsp + 0x80]",
    "mov [r8 + {rip}], rax",
    "mov rax, [rsp + 0x88]",
    "mov [r8 + {rflags}], rax",
    "mov rax, [rsp + 0x90]",
    "mov [r8 + {cs}], rax",
    "mov rax, [rsp + 0x98]",
    "mov [r8 + {fs}], rax",
    "mov rax, [rsp + 0xa0]",
    "mov [r8 + {gs}], rax",
    // And the in-out count becomes the count of what was written, which is the
    // kernel's own habit: `*count = x86_THREAD_STATE64_COUNT;`.
    "mov rcx, [rsp + 0x10]",
    "mov dword ptr [rcx], 42",
    "xor eax, eax",
    "refused:",
    "add rsp, 184",
    "ret",
    ".size rosetta3_thread_get_state, . - rosetta3_thread_get_state",
    admit = sym admit,
    rax = const OFFSETS[0],
    rbx = const OFFSETS[1],
    rcx = const OFFSETS[2],
    rdx = const OFFSETS[3],
    rdi = const OFFSETS[4],
    rsi = const OFFSETS[5],
    rbp = const OFFSETS[6],
    rsp = const OFFSETS[7],
    r8 = const OFFSETS[8],
    r9 = const OFFSETS[9],
    r10 = const OFFSETS[10],
    r11 = const OFFSETS[11],
    r12 = const OFFSETS[12],
    r13 = const OFFSETS[13],
    r14 = const OFFSETS[14],
    r15 = const OFFSETS[15],
    rip = const OFFSETS[16],
    rflags = const OFFSETS[17],
    cs = const OFFSETS[18],
    fs = const OFFSETS[19],
    gs = const OFFSETS[20],
);

unsafe extern "C" {
    /// The entry point above, which [`THREAD_GET_STATE`] is.
    fn rosetta3_thread_get_state(
        target: Name,
        flavor: i32,
        out_state: *mut u32,
        out_state_count: *mut u32,
    ) -> i32;
}

/// `kern_return_t thread_get_state(thread_act_t, thread_state_flavor_t,
/// thread_state_t, mach_msg_type_number_t *)`, as the [`exports!`] table
/// publishes it.
///
/// A function-pointer constant rather than a `fn` item, because the address
/// that has to be published is one the assembler produced - the same shape
/// `crate::malloc`'s `abort_report` trampoline takes, and for the same reason.
pub const THREAD_GET_STATE: unsafe extern "C" fn(Name, i32, *mut u32, *mut u32) -> i32 =
    rosetta3_thread_get_state;

/// `kern_return_t thread_suspend(thread_act_t)` - refused, with its reason.
///
/// **The obvious implementation is the wrong one.** Suspending the *calling*
/// thread is what Mach does allow, and it is what a self-sampling profiler asks
/// for; but the code that would read the state afterwards is the code that was
/// stopped, so a self-suspend here is a hang rather than a sample. Suspending
/// a *different* thread needs that thread stopped, and there is no way to stop
/// another thread of this process on this host: `ptrace(PTRACE_ATTACH)` is
/// refused for a sibling thread under `yama/ptrace_scope == 1`, and
/// `/proc/<tid>/…` and `process_vm_readv` do not reach registers.
///
/// So every target is `KERN_FAILURE`, said once and by name. That is a code the
/// caller already handles: Chromium's `SuspendableThreadDelegateMac::
/// ScopedSuspendThread` stores `MACH_PORT_NULL` for any `thread_suspend` that
/// is not `KERN_SUCCESS`, `WasSuccessful()` then answers false, and
/// `base::profiler::StackCopierMac`'s copy returns false, which
/// `StackSampler::RecordStackFrames` turns into one sample with no frames. The
/// `MACH_CHECK(kr == KERN_SUCCESS, kr) << "thread_resume"` in
/// `~ScopedSuspendThread` is unreachable from here, because the destructor
/// returns before the resume when the suspend did not succeed.
pub extern "C" fn thread_suspend(target: Name) -> i32 {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let mine = crate::pthread::id();
    note(
        "thread_suspend",
        &SAID,
        &match target == mine {
            // The one case where the reason is not the host.
            true => format!(
                "target {target:#x} is this thread - a self-suspend would stop the code that \
                 was going to read the state, and Mach's answer of KERN_SUCCESS would be a \
                 deadlock here rather than a sample"
            ),
            false => format!(
                "target {target:#x} is another thread - stopping it needs ptrace, and \
                 /proc/sys/kernel/yama/ptrace_scope is {scope} on this host, which refuses \
                 PTRACE_ATTACH to a sibling thread of the tracer's own process",
                scope = yama_scope().unwrap_or_else(|| "unreadable".into())
            ),
        },
    );
    KERN_FAILURE
}

/// `kern_return_t thread_resume(thread_act_t)` - refused, with its reason.
///
/// Always: this runtime never suspends a thread, so there is never a
/// suspension to undo. `xnu` answers `KERN_FAILURE` for a thread that is not
/// suspended too, so the number is the one a caller already handles, and a
/// caller that pairs it with a successful [`thread_suspend`] - which cannot
/// happen - is the only way to reach the `MACH_CHECK` on the far side.
pub extern "C" fn thread_resume(target: Name) -> i32 {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    note(
        "thread_resume",
        &SAID,
        &format!(
            "target {target:#x} - no thread is suspended by this runtime, because \
             thread_suspend is refused, so there is no suspension to undo"
        ),
    );
    KERN_FAILURE
}

/// `yama/ptrace_scope`, or `None` where the file is not there.
///
/// Read on every refusal rather than cached, because the whole argument for
/// refusing `thread_suspend` rests on it and a reader who wants to check has to
/// be able to change it and see the message change with it.
fn yama_scope() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .ok()
        .map(|text| text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A buffer poisoned the way Chromium poisons it, which is the question this
    /// module exists to answer: the guest fills its `x86_thread_state64_t` with
    /// `0xaa` bytes before asking (`-ftrivial-auto-var-init=pattern` expands to
    /// the eleven `movaps` at `Chromium Framework+0x5dadf1f` and the
    /// `movabs rax, 0xaaaa…` at `+0x5dadf11`, whose immediate is still in `rax`
    /// when the call is made), and every word of the answer has to have been
    /// written for the walk that follows to mean anything.
    const POISON: u64 = 0xaaaa_aaaa_aaaa_aaaa;

    /// Read the state a guest would read, through the published address.
    ///
    /// # Safety
    ///
    /// `out` is 21 eight-byte words, which is the structure.
    unsafe fn asked(target: Name, flavor: i32, out: &mut [u64; 21], count: &mut u32) -> i32 {
        // SAFETY: the buffer is 21 words, which is the flavour's whole layout,
        // and `count` is a live `u32`.
        unsafe { THREAD_GET_STATE(target, flavor, out.as_mut_ptr().cast::<u32>(), count) }
    }

    /// The structure is the one the header declares, and `x86_THREAD_STATE64_COUNT`
    /// is its size in 32-bit words. Chromium's `RegisterContext` *is* this type
    /// (`register_context.h`, `IS_MAC && ARCH_CPU_X86_64`), so a field at the
    /// wrong offset is a register the unwinder rewrites from the wrong place.
    #[test]
    fn the_layout_is_the_one_the_header_declares() {
        assert_eq!(size_of::<ThreadState64>(), 168);
        assert_eq!(X86_THREAD_STATE64_COUNT, 42);
        assert_eq!(X86_THREAD_STATE64_COUNT as usize * 4, 168);
        // `rip` at 128 and `rsp` at 56: the two a sampler and an unwinder read,
        // and the two `rosetta3`'s own `profile` module reads (`PC_WORD * 8` is
        // 128 there, in a `u32` array, so word 32 of 42).
        assert_eq!(std::mem::offset_of!(ThreadState64, rip), 0x80);
        assert_eq!(std::mem::offset_of!(ThreadState64, rsp), 0x38);
        // The seven Chromium rewrites: `__rbx`, `__rbp`, `__rsp`, `__r12`-`__r15`
        // (`GetRegistersToRewrite`).
        for field in [1usize, 6, 7, 12, 13, 14, 15] {
            assert_eq!(OFFSETS[field], 8 * field);
        }
        // Not `ss`, and not `efs`/`egs`: the last three are the selectors.
        assert_eq!(OFFSETS[18..], [0x90, 0x98, 0xa0]);
        // The machine-independent spelling of the same flavour.
        assert_eq!(THREAD_STATE_REGS64, X86_THREAD_STATE64);
    }

    /// **What the guest observes.** The words come back with the poison gone,
    /// the `rip` word is a return address into the very function that asked, and
    /// the `rsp` word is the stack pointer that function had at the call.
    ///
    /// Two call sites rather than one, because "the `rip` word is an address"
    /// is also true of a constant: what distinguishes the right answer from a
    /// plausible one is that it is the *caller's* return address, so it must land
    /// inside the helper that made the call and must differ between two helpers.
    #[test]
    fn a_guest_gets_its_own_rip_and_rsp_back() {
        // Two call sites, and each makes the call itself rather than through a
        // helper - so the address the state reports as `rip` really is a return
        // address into the function whose bounds the test knows. One call site
        // would not do: "the `rip` word is an address" is also true of a
        // constant, and what tells the right answer from a plausible one is that
        // it is the *caller's* return address, so it has to land inside the
        // caller and has to differ between two callers.
        #[inline(never)]
        unsafe fn ask_here(out: &mut [u64; 21], count: &mut u32) -> i32 {
            let padding = [0u8; 64];
            core::hint::black_box(&padding);
            // SAFETY: 21 words, which is the flavour's whole layout, and a live
            // count.
            let rc = unsafe {
                THREAD_GET_STATE(
                    crate::pthread::id(),
                    X86_THREAD_STATE64,
                    out.as_mut_ptr().cast::<u32>(),
                    count,
                )
            };
            // Work *after* the call, so that it cannot become a tail call: a
            // sibcall would make the state report the *test's* return address
            // rather than this function's, and the whole point is whose it is.
            core::hint::black_box(&padding);
            rc
        }
        #[inline(never)]
        unsafe fn ask_there(out: &mut [u64; 21], count: &mut u32) -> i32 {
            let padding = [0x5au8; 64];
            core::hint::black_box(&padding);
            // SAFETY: as above.
            let rc = unsafe {
                THREAD_GET_STATE(
                    crate::pthread::id(),
                    X86_THREAD_STATE64,
                    out.as_mut_ptr().cast::<u32>(),
                    count,
                )
            };
            core::hint::black_box(&padding);
            rc
        }

        let here = ask_here as *const () as u64;
        let there = ask_there as *const () as u64;
        // Chromium's own fill is a *uniform* `0xaa…`, because that is what
        // `-ftrivial-auto-var-init=pattern` writes. A uniform fill cannot prove
        // the callee wrote all 42 words, because a register it read might
        // itself have held `0xaa…` - and in this test one does, which is exactly
        // why the test below fills each word differently as well.
        let mut a = [POISON; 21];
        let mut b = [POISON; 21];
        let mut count_a = X86_THREAD_STATE64_COUNT;
        let mut count_b = X86_THREAD_STATE64_COUNT;

        // SAFETY: two live 21-word buffers and two live counts.
        let (rc_a, rc_b) = unsafe {
            (
                ask_here(&mut a, &mut count_a),
                ask_there(&mut b, &mut count_b),
            )
        };

        assert_eq!(rc_a, KERN_SUCCESS, "asking about itself must succeed");
        assert_eq!(rc_b, KERN_SUCCESS);
        assert_eq!(count_a, X86_THREAD_STATE64_COUNT, "the count is in and out");
        assert_eq!(count_b, X86_THREAD_STATE64_COUNT);

        // The two words a stack walk starts from, and the two it cannot do
        // without: they may not be the poison, because a poison `rip` is a walk
        // into `0xaaaa…` and a poison `rsp` is a read of `0xaaaa…`. The other
        // nineteen are *not* checked against `0xaa…` here, because a
        // callee-saved register the callee faithfully reports can itself have
        // held `0xaa…` - in this test `__r13` does - and `every_word_of_the_
        // answer_was_written_by_the_callee` is the check that does not have that
        // hole.
        for (name, state) in [("ask_here", &a), ("ask_there", &b)] {
            assert_ne!(state[16], POISON, "{name}: __rip is still the poison");
            assert_ne!(state[7], POISON, "{name}: __rsp is still the poison");
        }

        // `rip` is a return address into the code that asked, not into the
        // callee and not a fixed value.
        let (rip_a, rip_b) = (a[16], b[16]);
        assert!(
            (here..here + 4096).contains(&rip_a),
            "rip {rip_a:#x} is not inside ask_here, which is at {here:#x}"
        );
        assert!(
            (there..there + 4096).contains(&rip_b),
            "rip {rip_b:#x} is not inside ask_there, which is at {there:#x}"
        );
        assert_ne!(
            rip_a, rip_b,
            "one fixed value cannot be both return addresses"
        );

        // `rsp` is the frame that asked: at or below its own locals, within a
        // frame of them, and pointing at nothing that looks like code.
        let local = &count_a as *const _ as u64;
        let stack_of_here = a[7];
        assert!(
            stack_of_here <= local && local - stack_of_here < 64 * 1024,
            "rsp {stack_of_here:#x} is not this frame's stack pointer ({local:#x})"
        );
        assert!(
            !((here..here + 4096).contains(&stack_of_here)),
            "rsp {stack_of_here:#x} is inside the code that asked"
        );

        // The four argument registers hold the arguments, which is what the
        // kernel saves: it traps *after* the caller wrote them. `rdi` is the
        // target and `rsi` the flavour, so those two are checkable exactly.
        assert_eq!(a[4], u64::from(crate::pthread::id()), "__rdi is the target");
        assert_eq!(
            a[5],
            u64::from(X86_THREAD_STATE64 as u32),
            "__rsi is the flavour"
        );
        // `rdx` is the out-parameter and `rcx` the count, and the 42 now in
        // `*count` is the proof that the callee wrote through `rcx`.
        assert_eq!(a[3], a.as_ptr() as u64, "__rdx is the out-parameter");
        assert_eq!(
            a[2], &count_a as *const _ as u64,
            "__rcx is the count pointer"
        );
        // And the flags are this thread's, not a constant: bit 1 is always set
        // and the carry is clear after a call.
        assert_eq!(a[17] & 0x2, 0x2, "rflags is not this thread's");
    }

    /// **Every** word was written, provable, by filling each one differently.
    ///
    /// This is the direct answer to the question this module was written for: a
    /// `thread_get_state` that returned `KERN_SUCCESS` without touching the
    /// buffer - which is what a pass-through to something that does not know
    /// what `x86_THREAD_STATE64` is would do - leaves the caller's bytes in
    /// place, and every word here is the word the caller wrote. Chromium's own
    /// poison is uniform `0xaa…`, which cannot show this on its own because a
    /// register the callee reads may itself have held `0xaa…`; a per-word fill
    /// can, because twenty-one distinct values cannot all be registers.
    #[test]
    fn every_word_of_the_answer_was_written_by_the_callee() {
        let mut out = [0u64; 21];
        let mut count = X86_THREAD_STATE64_COUNT;
        for (word, value) in out.iter_mut().enumerate() {
            // A distinct, improbable value per word: the low half counts, the
            // high half is the poison so a word that is merely *unchanged* is
            // also visible.
            *value = 0xaaaa_0000_0000_0000 | word as u64;
        }
        let before = out;
        // SAFETY: a live 21-word buffer and a live count.
        let rc = unsafe {
            asked(
                crate::pthread::id(),
                X86_THREAD_STATE64,
                &mut out,
                &mut count,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        for (word, (was, now)) in before.iter().zip(out.iter()).enumerate() {
            assert_ne!(
                *now,
                *was,
                "word {word} at offset {:#x} is the value the caller wrote, so the callee \
                 did not write it",
                8 * word
            );
        }
    }

    /// The refusals, each named for what it was given, and none of them touching
    /// the caller's buffer. The last half of that is the important half: a
    /// refusal that leaves a half-written structure behind is worse than no
    /// answer, because `x86_THREAD_STATE64` is ignored by callers that only want
    /// the `rip`.
    #[test]
    fn every_refusal_leaves_the_buffers_the_guest_wrote() {
        let mut out = [POISON; 21];
        let mut count = X86_THREAD_STATE64_COUNT;

        // A flavour this runtime does not answer.
        let rc = // SAFETY: a live buffer and count.
            unsafe { asked(crate::pthread::id(), X86_FLOAT_STATE64, &mut out, &mut count) };
        assert_eq!(
            rc, KERN_INVALID_ARGUMENT,
            "a float state is not a register state"
        );
        assert!(
            out.iter().all(|w| *w == POISON),
            "the flavour refusal wrote to out"
        );
        assert_eq!(count, X86_THREAD_STATE64_COUNT, "and left the count alone");

        // A buffer too short for the flavour, which `xnu` calls an argument and
        // not an address.
        let mut short = 41u32;
        // SAFETY: a live buffer, and a live count of 41.
        let rc = unsafe {
            THREAD_GET_STATE(
                crate::pthread::id(),
                X86_THREAD_STATE64,
                out.as_mut_ptr().cast::<u32>(),
                &mut short,
            )
        };
        assert_eq!(rc, KERN_INVALID_ARGUMENT);
        assert!(
            out.iter().all(|w| *w == POISON),
            "the short-count refusal wrote to out"
        );

        // Null out-parameters are addresses, not arguments.
        // SAFETY: a null `old_state`, which is what is being tested.
        let rc = unsafe {
            THREAD_GET_STATE(
                crate::pthread::id(),
                X86_THREAD_STATE64,
                std::ptr::null_mut(),
                &mut count,
            )
        };
        assert_eq!(rc, KERN_INVALID_ADDRESS);
        // SAFETY: a null count, which is what is being tested.
        let rc = unsafe {
            THREAD_GET_STATE(
                crate::pthread::id(),
                X86_THREAD_STATE64,
                out.as_mut_ptr().cast::<u32>(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, KERN_INVALID_ADDRESS);
        assert!(out.iter().all(|w| *w == POISON));

        // A thread this runtime cannot read. Three of them, and the two
        // namespaces are the point: `MACH_PORT_NULL`, a name from
        // `mach::space`, and a live-looking kernel thread id that is not this
        // thread's. All three are `KERN_FAILURE`, and none of them publishes a
        // register.
        let foreign = super::super::task::mach_thread_self();
        assert_ne!(foreign, 0, "`mach_thread_self` always names something");
        for target in [
            0u32,
            foreign,
            foreign.wrapping_add(4),
            crate::pthread::id() + 1,
        ] {
            let mut out = [POISON; 21];
            let mut count = X86_THREAD_STATE64_COUNT;
            // SAFETY: a live buffer and count.
            let rc = unsafe { asked(target, X86_THREAD_STATE64, &mut out, &mut count) };
            assert_eq!(
                rc, KERN_FAILURE,
                "target {target:#x} was answered instead of refused"
            );
            assert!(
                out.iter().all(|w| *w == POISON),
                "target {target:#x}: a refusal published registers"
            );
            assert_eq!(count, X86_THREAD_STATE64_COUNT);
        }
    }

    /// The name has to reach the guest as an *address*, or none of the above is
    /// callable. `thread_` is not a denied prefix, so the only thing that can
    /// publish it is this crate - and what it publishes must not be the host's,
    /// which has no such function and would bind to zero.
    #[test]
    fn the_state_call_is_published_rather_than_left_to_the_host() {
        let ours = crate::symbol("thread_get_state")
            .expect("the guest's import slot for this name would be left zero");
        assert_eq!(ours, THREAD_GET_STATE as usize as u64);
        assert_ne!(
            ours,
            crate::host_symbol("thread_get_state").unwrap_or(0),
            "the guest's slot would hold the host's, which is not this function"
        );
        assert!(
            !crate::divergent::contains("thread_get_state"),
            "a name this crate answers for cannot also be refused by family"
        );
        for name in ["thread_suspend", "thread_resume"] {
            assert!(crate::symbol(name).is_some(), "{name} is not published");
            assert!(
                !crate::divergent::contains(name),
                "{name} is refused by family"
            );
        }
    }

    /// The suspension pair refuses, and says why. A `KERN_SUCCESS` from
    /// `thread_suspend` for the calling thread would be a deadlock here rather
    /// than a sample, so the number matters as much as the message.
    #[test]
    fn suspension_is_refused_with_the_reason_named() {
        let mine = crate::pthread::id();
        assert_eq!(
            thread_suspend(mine),
            KERN_FAILURE,
            "a self-suspend must refuse"
        );
        assert_eq!(thread_suspend(mine + 1), KERN_FAILURE);
        assert_eq!(thread_suspend(0), KERN_FAILURE);
        assert_eq!(thread_resume(mine), KERN_FAILURE);
        assert_eq!(thread_resume(0), KERN_FAILURE);
    }
}
