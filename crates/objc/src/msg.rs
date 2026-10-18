use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::object::Id;
use crate::sel::Sel;

/// The `IMP` to jump to for `(receiver, sel)`, or the "unrecognised" trampoline.
///
/// Called from [`msg_send`]'s prologue with the receiver and selector only; the
/// rest of the arguments are still spilled on its frame.
///
/// # Safety
///
/// `receiver` must be a non-null object pointer.
unsafe extern "C" fn lookup(receiver: Id, sel: Sel) -> *const c_void {
    let refused = match logging_unrecognised() {
        true => unrecognised_logged as *const c_void,
        false => unrecognised as *const c_void,
    };
    let Some(class) = (unsafe { crate::object::class_of(receiver) }) else {
        return refused;
    };
    match class.method(sel) {
        Some(method) => method.implementation(),
        None => refused,
    }
}

/// What a message no class answers lands on.
///
/// A named abort rather than a jump to null: the selector and the class are the
/// whole of the diagnostic, and a null call would print neither.
unsafe extern "C" fn unrecognised(receiver: Id, sel: Sel) -> ! {
    // SAFETY: reached only from `lookup`, which was given a live receiver.
    let line = unsafe { describe_unrecognised(receiver, sel) };
    crate::fail!("{line}");
}

/// The `IMP` a message nothing answers resolves to, for a caller that asked for
/// one rather than sent the message - `-methodForSelector:` and the
/// `-performSelector:` family.
///
/// Never null, and that is the point: a call site handed nil jumps to zero, and
/// a null jump reports neither the class nor the selector. This is Apple's
/// `_objc_msgForward` position, minus the forwarding machinery - the only thing
/// left to do with the message is name it.
pub fn forward() -> *const c_void {
    match logging_unrecognised() {
        true => unrecognised_logged as *const c_void,
        false => unrecognised as *const c_void,
    }
}

/// The class and selector of a message nothing answers, as one line.
///
/// # Safety
///
/// `receiver` is null or a live object.
unsafe fn describe_unrecognised(receiver: Id, sel: Sel) -> String {
    let class = unsafe { crate::object::class_of(receiver) };
    let name = class.map_or("nil", |c| c.name_str());
    let selector = match sel.is_null() {
        true => std::borrow::Cow::Borrowed("<null selector>"),
        // SAFETY: a registered selector is a NUL-terminated interned name.
        false => unsafe { std::ffi::CStr::from_ptr(sel) }.to_string_lossy(),
    };
    format!("unrecognized selector sent to instance of {name}: {selector}")
}

/// `ROSEMARY_LOG_UNRECOGNIZED=1`: report each unanswered message once and answer
/// nil, instead of aborting on the first one.
///
/// A diagnostic and not a mode, and the difference matters: answering nil to a
/// message the class does not implement is exactly the silent-wrong-answer this
/// runtime exists to avoid, and a program run this way is a program whose later
/// behaviour cannot be trusted. What it buys is the *whole* list in one run
/// rather than one name per rebuild - which is the only affordable way to find
/// out which of forty-odd missing classes a large program actually uses.
pub fn logging_unrecognised() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ROSEMARY_LOG_UNRECOGNIZED").is_some())
}

/// [`unrecognised`] under [`logging_unrecognised`]: one line per distinct
/// message, then nil.
unsafe extern "C" fn unrecognised_logged(receiver: Id, sel: Sel) -> usize {
    use std::sync::Mutex;
    static SEEN: Mutex<Option<std::collections::BTreeSet<String>>> = Mutex::new(None);

    let line = unsafe { describe_unrecognised(receiver, sel) };
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.get_or_insert_default().insert(line.clone()) {
        eprintln!("rosemary: objc: {line} - answering nil (ROSEMARY_LOG_UNRECOGNIZED is set)");
    }
    0
}

/// [`unrecognised`] for the `_stret` ABI, where the argument registers are one
/// place along and the first of them is the caller's return buffer.
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn unrecognised_stret(_buffer: *mut c_void, receiver: Id, sel: Sel) -> ! {
    // SAFETY: reached only from `lookup_stret`, which was given a live receiver.
    unsafe { unrecognised(receiver, sel) }
}

/// [`unrecognised_logged`] for the `_stret` ABI: the buffer is left as the
/// caller set it, which is what messaging nil does to it too.
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn unrecognised_stret_logged(_buffer: *mut c_void, receiver: Id, sel: Sel) {
    // SAFETY: reached only from `lookup_stret`, which was given a live receiver.
    unsafe { unrecognised_logged(receiver, sel) };
}

/// [`lookup`] for the `_stret` ABI.
///
/// # Safety
///
/// As [`lookup`].
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn lookup_stret(receiver: Id, sel: Sel) -> *const c_void {
    let refused = match logging_unrecognised() {
        true => unrecognised_stret_logged as *const c_void,
        false => unrecognised_stret as *const c_void,
    };
    let Some(class) = (unsafe { crate::object::class_of(receiver) }) else {
        return refused;
    };
    match class.method(sel) {
        Some(method) => method.implementation(),
        None => refused,
    }
}

/// `objc_msgSend`'s address.
pub fn entry() -> *const c_void {
    msg_send as *const c_void
}

/// `objc_msgSend_stret`'s address: the entry point for a message whose return
/// value the ABI hands back through a caller-provided buffer rather than in
/// registers.
///
/// Separate from [`entry`] on x86-64 because the buffer occupies the first
/// argument register, so the receiver is in the second and the selector in the
/// third - a message sent through [`entry`] would look up a method on whatever
/// the caller's return slot happened to contain. On AArch64 the buffer travels
/// in `x8`, which displaces nothing, so there is one entry point and this is it.
///
/// Which of the two a message needs is the return type's, not the selector's:
/// anything the System V ABI classifies as MEMORY - `NSRect`, at four doubles -
/// comes here, and `NSPoint` and `NSSize` at two do not.
pub fn entry_stret() -> *const c_void {
    #[cfg(target_arch = "x86_64")]
    {
        msg_send_stret as *const c_void
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        msg_send as *const c_void
    }
}

/// The trampoline itself.
///
/// Naked rather than assembled separately so that the symbol is Rust's to name:
/// this runtime must never export `objc_msgSend` under that spelling, or a Mac
/// could not load it alongside the host's own libobjc - which is exactly the
/// arrangement that lets both be tested here.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn msg_send() {
    // x0 = receiver, x1 = selector, x2-x7 and v0-v7 the rest, x8 an indirect
    // return buffer. All of them have to survive the lookup call.
    core::arch::naked_asm!(
        "cbz     x0, 9f",
        "stp     x29, x30, [sp, #-16]!",
        "mov     x29, sp",
        "sub     sp, sp, #208",
        "stp     q0, q1, [sp, #0]",
        "stp     q2, q3, [sp, #32]",
        "stp     q4, q5, [sp, #64]",
        "stp     q6, q7, [sp, #96]",
        "stp     x0, x1, [sp, #128]",
        "stp     x2, x3, [sp, #144]",
        "stp     x4, x5, [sp, #160]",
        "stp     x6, x7, [sp, #176]",
        "str     x8,      [sp, #192]",
        "bl      {lookup}",
        "mov     x9, x0",
        "ldp     q0, q1, [sp, #0]",
        "ldp     q2, q3, [sp, #32]",
        "ldp     q4, q5, [sp, #64]",
        "ldp     q6, q7, [sp, #96]",
        "ldp     x0, x1, [sp, #128]",
        "ldp     x2, x3, [sp, #144]",
        "ldp     x4, x5, [sp, #160]",
        "ldp     x6, x7, [sp, #176]",
        "ldr     x8,      [sp, #192]",
        "add     sp, sp, #208",
        "ldp     x29, x30, [sp], #16",
        "br      x9",
        // Messaging nil is the language, not an error: the return registers are
        // zeroed and the send is over. x0 is already zero, which is the test.
        "9:",
        "mov     x1, #0",
        "movi    d0, #0",
        "movi    d1, #0",
        "movi    d2, #0",
        "movi    d3, #0",
        "ret",
        lookup = sym lookup,
    )
}

/// [`msg_send`] for x86-64, where this runtime is cross-built.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn msg_send() {
    // rdi = receiver, rsi = selector, rdx/rcx/r8/r9 and xmm0-7 the rest, and rax
    // the vector-register count a variadic call site set.
    core::arch::naked_asm!(
        "test    rdi, rdi",
        "je      9f",
        "push    rbp",
        "mov     rbp, rsp",
        "sub     rsp, 192",
        "movaps  [rsp + 0], xmm0",
        "movaps  [rsp + 16], xmm1",
        "movaps  [rsp + 32], xmm2",
        "movaps  [rsp + 48], xmm3",
        "movaps  [rsp + 64], xmm4",
        "movaps  [rsp + 80], xmm5",
        "movaps  [rsp + 96], xmm6",
        "movaps  [rsp + 112], xmm7",
        "mov     [rsp + 128], rdi",
        "mov     [rsp + 136], rsi",
        "mov     [rsp + 144], rdx",
        "mov     [rsp + 152], rcx",
        "mov     [rsp + 160], r8",
        "mov     [rsp + 168], r9",
        "mov     [rsp + 176], rax",
        "call    {lookup}",
        "mov     r11, rax",
        "movaps  xmm0, [rsp + 0]",
        "movaps  xmm1, [rsp + 16]",
        "movaps  xmm2, [rsp + 32]",
        "movaps  xmm3, [rsp + 48]",
        "movaps  xmm4, [rsp + 64]",
        "movaps  xmm5, [rsp + 80]",
        "movaps  xmm6, [rsp + 96]",
        "movaps  xmm7, [rsp + 112]",
        "mov     rdi, [rsp + 128]",
        "mov     rsi, [rsp + 136]",
        "mov     rdx, [rsp + 144]",
        "mov     rcx, [rsp + 152]",
        "mov     r8,  [rsp + 160]",
        "mov     r9,  [rsp + 168]",
        "mov     rax, [rsp + 176]",
        "add     rsp, 192",
        "pop     rbp",
        "jmp     r11",
        "9:",
        "xor     eax, eax",
        "xor     edx, edx",
        "xorps   xmm0, xmm0",
        "xorps   xmm1, xmm1",
        "ret",
        lookup = sym lookup,
    )
}

/// [`msg_send`] for a message returning through a buffer, which on x86-64 puts
/// the buffer in `rdi` and pushes the receiver and selector to `rsi` and `rdx`.
///
/// The lookup call is the only thing that differs: it has to be handed `rsi` and
/// `rdx` rather than `rdi` and `rsi`. Everything else - the spill, the restore,
/// the tail jump - is the same, because the implementation is entered with the
/// registers the call site set, buffer included.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn msg_send_stret() {
    core::arch::naked_asm!(
        "test    rsi, rsi",
        "je      9f",
        "push    rbp",
        "mov     rbp, rsp",
        "sub     rsp, 192",
        "movaps  [rsp + 0], xmm0",
        "movaps  [rsp + 16], xmm1",
        "movaps  [rsp + 32], xmm2",
        "movaps  [rsp + 48], xmm3",
        "movaps  [rsp + 64], xmm4",
        "movaps  [rsp + 80], xmm5",
        "movaps  [rsp + 96], xmm6",
        "movaps  [rsp + 112], xmm7",
        "mov     [rsp + 128], rdi",
        "mov     [rsp + 136], rsi",
        "mov     [rsp + 144], rdx",
        "mov     [rsp + 152], rcx",
        "mov     [rsp + 160], r8",
        "mov     [rsp + 168], r9",
        "mov     [rsp + 176], rax",
        // The receiver and selector, one register along from where the lookup
        // takes them.
        "mov     rdi, rsi",
        "mov     rsi, rdx",
        "call    {lookup}",
        "mov     r11, rax",
        "movaps  xmm0, [rsp + 0]",
        "movaps  xmm1, [rsp + 16]",
        "movaps  xmm2, [rsp + 32]",
        "movaps  xmm3, [rsp + 48]",
        "movaps  xmm4, [rsp + 64]",
        "movaps  xmm5, [rsp + 80]",
        "movaps  xmm6, [rsp + 96]",
        "movaps  xmm7, [rsp + 112]",
        "mov     rdi, [rsp + 128]",
        "mov     rsi, [rsp + 136]",
        "mov     rdx, [rsp + 144]",
        "mov     rcx, [rsp + 152]",
        "mov     r8,  [rsp + 160]",
        "mov     r9,  [rsp + 168]",
        "mov     rax, [rsp + 176]",
        "add     rsp, 192",
        "pop     rbp",
        "jmp     r11",
        // Messaging nil, where the only thing to hand back is the buffer address
        // a struct return owes its caller in `rax`. The buffer itself is left
        // alone, as Apple's own `_stret` leaves it: its size is not something
        // this entry point is told, so there is nothing here that could zero it.
        // A caller that might be holding nil has to check, and this tree's do.
        "9:",
        "mov     rax, rdi",
        "ret",
        lookup = sym lookup_stret,
    )
}

/// `struct objc_super`, the first argument of every `Super` spelling.
///
/// `class` is what the search starts *at* for `objc_msgSendSuper` and the class
/// whose superclass it starts at for `objc_msgSendSuper2`. Both slots are filled
/// from a `__objc_superrefs` entry, which is why those have to name a class this
/// runtime knows: see `rosemary::objc::rewrite_refs`.
#[repr(C)]
struct Super {
    receiver: Id,
    class: *const crate::class::Class,
}

/// The `IMP` for a `[super ...]` send, searching from `super.class` itself.
///
/// # Safety
///
/// `sup` must point at a live `objc_super` whose receiver is non-null.
unsafe extern "C" fn lookup_super(sup: *const Super, sel: Sel) -> *const c_void {
    let sup = unsafe { &*sup };
    // SAFETY: `class` is a class pointer this runtime handed out, or null.
    match unsafe { crate::class::from_ptr(sup.class) } {
        Some(class) => imp_or_unrecognised(class, sel),
        None => unrecognised as *const c_void,
    }
}

/// [`lookup_super`] for `objc_msgSendSuper2`, where the search starts one class
/// higher: clang names the class the method is *in*, so that a category or a
/// subclass compiled separately still resolves against the right superclass.
///
/// # Safety
///
/// As [`lookup_super`].
unsafe extern "C" fn lookup_super2(sup: *const Super, sel: Sel) -> *const c_void {
    let sup = unsafe { &*sup };
    match unsafe { crate::class::from_ptr(sup.class) }.and_then(|c| c.superclass()) {
        Some(class) => imp_or_unrecognised(class, sel),
        None => unrecognised as *const c_void,
    }
}

fn imp_or_unrecognised(class: &'static crate::class::Class, sel: Sel) -> *const c_void {
    match class.method(sel) {
        Some(method) => method.implementation(),
        None => unrecognised as *const c_void,
    }
}

/// `objc_msgSendSuper`'s address.
pub fn entry_super() -> *const c_void {
    msg_send_super as *const c_void
}

/// `objc_msgSendSuper2`'s address.
pub fn entry_super2() -> *const c_void {
    msg_send_super2 as *const c_void
}

/// `objc_msgSendSuper_stret`'s address. As [`entry_stret`], one entry point on
/// AArch64 and a second one on x86-64.
pub fn entry_super_stret() -> *const c_void {
    #[cfg(target_arch = "x86_64")]
    {
        msg_send_super_stret as *const c_void
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        msg_send_super as *const c_void
    }
}

/// `objc_msgSendSuper2_stret`'s address.
pub fn entry_super2_stret() -> *const c_void {
    #[cfg(target_arch = "x86_64")]
    {
        msg_send_super2_stret as *const c_void
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        msg_send_super2 as *const c_void
    }
}

/// The two `Super` trampolines, which differ only in which lookup they call.
///
/// Each is [`msg_send`] with two changes: the receiver is `[x0]`/`[rdi]` rather
/// than the first argument itself, and it is put back into the argument register
/// after the lookup - the implementation is entered as if it had been sent
/// directly.
macro_rules! super_trampoline {
    ($name:ident, $lookup:ident) => {
        #[cfg(target_arch = "aarch64")]
        #[unsafe(naked)]
        unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "cbz     x0, 9f",
                "ldr     x9, [x0]",
                "cbz     x9, 9f",
                "stp     x29, x30, [sp, #-16]!",
                "mov     x29, sp",
                "sub     sp, sp, #208",
                "stp     q0, q1, [sp, #0]",
                "stp     q2, q3, [sp, #32]",
                "stp     q4, q5, [sp, #64]",
                "stp     q6, q7, [sp, #96]",
                "stp     x0, x1, [sp, #128]",
                "stp     x2, x3, [sp, #144]",
                "stp     x4, x5, [sp, #160]",
                "stp     x6, x7, [sp, #176]",
                "str     x8,      [sp, #192]",
                "bl      {lookup}",
                "mov     x9, x0",
                "ldp     q0, q1, [sp, #0]",
                "ldp     q2, q3, [sp, #32]",
                "ldp     q4, q5, [sp, #64]",
                "ldp     q6, q7, [sp, #96]",
                "ldp     x0, x1, [sp, #128]",
                "ldp     x2, x3, [sp, #144]",
                "ldp     x4, x5, [sp, #160]",
                "ldp     x6, x7, [sp, #176]",
                "ldr     x8,      [sp, #192]",
                "add     sp, sp, #208",
                "ldp     x29, x30, [sp], #16",
                // x0 still holds the `objc_super`; the callee wants its receiver.
                "ldr     x0, [x0]",
                "br      x9",
                "9:",
                "mov     x0, #0",
                "mov     x1, #0",
                "movi    d0, #0",
                "movi    d1, #0",
                "movi    d2, #0",
                "movi    d3, #0",
                "ret",
                lookup = sym $lookup,
            )
        }

        #[cfg(target_arch = "x86_64")]
        #[unsafe(naked)]
        unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "test    rdi, rdi",
                "je      9f",
                "cmp     qword ptr [rdi], 0",
                "je      9f",
                "push    rbp",
                "mov     rbp, rsp",
                "sub     rsp, 192",
                "movaps  [rsp + 0], xmm0",
                "movaps  [rsp + 16], xmm1",
                "movaps  [rsp + 32], xmm2",
                "movaps  [rsp + 48], xmm3",
                "movaps  [rsp + 64], xmm4",
                "movaps  [rsp + 80], xmm5",
                "movaps  [rsp + 96], xmm6",
                "movaps  [rsp + 112], xmm7",
                "mov     [rsp + 128], rdi",
                "mov     [rsp + 136], rsi",
                "mov     [rsp + 144], rdx",
                "mov     [rsp + 152], rcx",
                "mov     [rsp + 160], r8",
                "mov     [rsp + 168], r9",
                "mov     [rsp + 176], rax",
                "call    {lookup}",
                "mov     r11, rax",
                "movaps  xmm0, [rsp + 0]",
                "movaps  xmm1, [rsp + 16]",
                "movaps  xmm2, [rsp + 32]",
                "movaps  xmm3, [rsp + 48]",
                "movaps  xmm4, [rsp + 64]",
                "movaps  xmm5, [rsp + 80]",
                "movaps  xmm6, [rsp + 96]",
                "movaps  xmm7, [rsp + 112]",
                "mov     rdi, [rsp + 128]",
                "mov     rsi, [rsp + 136]",
                "mov     rdx, [rsp + 144]",
                "mov     rcx, [rsp + 152]",
                "mov     r8,  [rsp + 160]",
                "mov     r9,  [rsp + 168]",
                "mov     rax, [rsp + 176]",
                "add     rsp, 192",
                "pop     rbp",
                // As above: the callee is entered on the receiver.
                "mov     rdi, [rdi]",
                "jmp     r11",
                "9:",
                "xor     eax, eax",
                "xor     edx, edx",
                "xorps   xmm0, xmm0",
                "xorps   xmm1, xmm1",
                "ret",
                lookup = sym $lookup,
            )
        }
    };
}

super_trampoline!(msg_send_super, lookup_super);
super_trampoline!(msg_send_super2, lookup_super2);

/// The `_stret` spellings of the two above, which exist only on x86-64: the
/// return buffer takes `rdi`, so the `objc_super` is in `rsi` and the selector
/// in `rdx`.
#[cfg(target_arch = "x86_64")]
macro_rules! super_stret_trampoline {
    ($name:ident, $lookup:ident) => {
        #[unsafe(naked)]
        unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                "test    rsi, rsi",
                "je      9f",
                "cmp     qword ptr [rsi], 0",
                "je      9f",
                "push    rbp",
                "mov     rbp, rsp",
                "sub     rsp, 192",
                "movaps  [rsp + 0], xmm0",
                "movaps  [rsp + 16], xmm1",
                "movaps  [rsp + 32], xmm2",
                "movaps  [rsp + 48], xmm3",
                "movaps  [rsp + 64], xmm4",
                "movaps  [rsp + 80], xmm5",
                "movaps  [rsp + 96], xmm6",
                "movaps  [rsp + 112], xmm7",
                "mov     [rsp + 128], rdi",
                "mov     [rsp + 136], rsi",
                "mov     [rsp + 144], rdx",
                "mov     [rsp + 152], rcx",
                "mov     [rsp + 160], r8",
                "mov     [rsp + 168], r9",
                "mov     [rsp + 176], rax",
                "mov     rdi, rsi",
                "mov     rsi, rdx",
                "call    {lookup}",
                "mov     r11, rax",
                "movaps  xmm0, [rsp + 0]",
                "movaps  xmm1, [rsp + 16]",
                "movaps  xmm2, [rsp + 32]",
                "movaps  xmm3, [rsp + 48]",
                "movaps  xmm4, [rsp + 64]",
                "movaps  xmm5, [rsp + 80]",
                "movaps  xmm6, [rsp + 96]",
                "movaps  xmm7, [rsp + 112]",
                "mov     rdi, [rsp + 128]",
                "mov     rsi, [rsp + 136]",
                "mov     rdx, [rsp + 144]",
                "mov     rcx, [rsp + 152]",
                "mov     r8,  [rsp + 160]",
                "mov     r9,  [rsp + 168]",
                "mov     rax, [rsp + 176]",
                "add     rsp, 192",
                "pop     rbp",
                "mov     rsi, [rsi]",
                "jmp     r11",
                // Messaging nil, as in `msg_send_stret`: the buffer is left
                // alone and its address is what the caller is owed.
                "9:",
                "mov     rax, rdi",
                "ret",
                lookup = sym $lookup,
            )
        }
    };
}

#[cfg(target_arch = "x86_64")]
super_stret_trampoline!(msg_send_super_stret, lookup_super);
#[cfg(target_arch = "x86_64")]
super_stret_trampoline!(msg_send_super2_stret, lookup_super2);

/// Guest `objc_class` structures standing for classes of this runtime, keyed by
/// address, with the range they span so the common case is two compares.
///
/// A pre-10.6 compiler names a class from its own image by the address of its
/// `objc_class` in code (`lea`), not through a `__objc_classrefs` slot a loader
/// could rewrite, so the receiver of `[MyClass alloc]` or the class of a
/// `[super ...]` is a structure this runtime never made. That toolchain only
/// ever sends through the `_fixup` entry points, so they are the one place the
/// translation is needed - and the only place it costs anything.
static ALIASES: std::sync::RwLock<Option<std::collections::HashMap<usize, usize>>> =
    std::sync::RwLock::new(None);
static ALIAS_LO: AtomicUsize = AtomicUsize::new(usize::MAX);
static ALIAS_HI: AtomicUsize = AtomicUsize::new(0);

/// Record that the guest `objc_class` at `guest` stands for `class`.
pub fn alias_class(guest: usize, class: *const c_void) {
    ALIASES
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(guest, class as usize);
    ALIAS_LO.fetch_min(guest, Ordering::AcqRel);
    ALIAS_HI.fetch_max(guest + 1, Ordering::AcqRel);
}

/// The class `addr` stands for, or `addr` itself.
pub fn unalias_class(addr: usize) -> usize {
    ALIASES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&addr).copied())
        .unwrap_or(addr)
}

extern "C" fn unalias_class_c(addr: usize) -> usize {
    unalias_class(addr)
}

/// The `_fixup` spellings a pre-10.9 x86-64 toolchain calls through a
/// `message_ref_t` (`{IMP, SEL}`): the selector register holds the ref, not the
/// `SEL`. Apple's runtime rewrites each ref's `IMP` to a trampoline that loads
/// the `SEL` and carries on; binding the import to that trampoline directly is
/// the same thing without the rewrite. The `SEL` half is uniqued at load.
///
/// `$class` is the operand holding the receiver (or `objc_super.class`), which
/// is translated through [`ALIASES`] when it falls in their range.
macro_rules! fixup_trampoline {
    ($name:ident, $sel:literal, $class:literal, $target:ident) => {
        #[cfg(target_arch = "x86_64")]
        #[unsafe(naked)]
        pub(crate) unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                concat!("mov ", $sel, ", [", $sel, " + 8]"),
                concat!("mov r11, ", $class),
                "cmp r11, [rip + {lo}]",
                "jb {target}",
                "cmp r11, [rip + {hi}]",
                "jae {target}",
                "push rbp",
                "mov rbp, rsp",
                "sub rsp, 192",
                "movaps [rsp + 0], xmm0",
                "movaps [rsp + 16], xmm1",
                "movaps [rsp + 32], xmm2",
                "movaps [rsp + 48], xmm3",
                "movaps [rsp + 64], xmm4",
                "movaps [rsp + 80], xmm5",
                "movaps [rsp + 96], xmm6",
                "movaps [rsp + 112], xmm7",
                "mov [rsp + 128], rdi",
                "mov [rsp + 136], rsi",
                "mov [rsp + 144], rdx",
                "mov [rsp + 152], rcx",
                "mov [rsp + 160], r8",
                "mov [rsp + 168], r9",
                "mov [rsp + 176], rax",
                "mov rdi, r11",
                "call {unalias}",
                "mov r11, rax",
                "movaps xmm0, [rsp + 0]",
                "movaps xmm1, [rsp + 16]",
                "movaps xmm2, [rsp + 32]",
                "movaps xmm3, [rsp + 48]",
                "movaps xmm4, [rsp + 64]",
                "movaps xmm5, [rsp + 80]",
                "movaps xmm6, [rsp + 96]",
                "movaps xmm7, [rsp + 112]",
                "mov rdi, [rsp + 128]",
                "mov rsi, [rsp + 136]",
                "mov rdx, [rsp + 144]",
                "mov rcx, [rsp + 152]",
                "mov r8,  [rsp + 160]",
                "mov r9,  [rsp + 168]",
                "mov rax, [rsp + 176]",
                "add rsp, 192",
                "pop rbp",
                concat!("mov ", $class, ", r11"),
                "jmp {target}",
                target = sym $target,
                lo = sym ALIAS_LO,
                hi = sym ALIAS_HI,
                unalias = sym unalias_class_c,
            )
        }
    };
}

// `objc_super` is `{receiver, class}`; a super send's class is its second word.
fixup_trampoline!(msg_send_fixup, "rsi", "rdi", msg_send);
fixup_trampoline!(msg_send_stret_fixup, "rdx", "rsi", msg_send_stret);
fixup_trampoline!(
    msg_send_super2_fixup,
    "rsi",
    "qword ptr [rdi + 8]",
    msg_send_super2
);
fixup_trampoline!(
    msg_send_super2_stret_fixup,
    "rdx",
    "qword ptr [rsi + 8]",
    msg_send_super2_stret
);

/// An `IMP` that sends the same message, every argument untouched, to the
/// object in the receiver's first instance word (the one after `isa`).
///
/// For a companion object that answers for another: AppKit's `-cell` here is
/// state its control already owns, so a cell's methods are its control's.
/// Not for a method returning through a buffer, whose receiver is elsewhere.
pub fn entry_forward_to_first_word() -> *const c_void {
    forward_to_first_word as *const c_void
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn forward_to_first_word() {
    core::arch::naked_asm!(
        "mov rdi, [rdi + 8]",
        "jmp {send}",
        send = sym msg_send,
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn forward_to_first_word() {
    core::arch::naked_asm!(
        "ldr x0, [x0, #8]",
        "b {send}",
        send = sym msg_send,
    )
}
