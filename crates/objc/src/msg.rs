use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::object::Id;
use crate::sel::Sel;

/// The `IMP` to jump to for `(receiver, sel)`, or the "unrecognised" trampoline.
///
/// Called from [`msg_send`]'s prologue with the receiver, the selector, and
/// where on its frame the receiver is spilled; the rest of the arguments are
/// still spilled there too. A forwarded message is sent on by rewriting that
/// slot, which is what the trampoline restores the receiver from.
///
/// # Safety
///
/// `receiver` must be a non-null object pointer, and `slot` null or the
/// trampoline's saved receiver.
unsafe extern "C" fn lookup(receiver: Id, sel: Sel, slot: *mut Id) -> *const c_void {
    let refused = match logging_unrecognised() {
        true => unrecognised_logged as *const c_void,
        false => unrecognised as *const c_void,
    };
    let generation = crate::class::generation();
    // SAFETY: the caller's obligations.
    let resolved = unsafe { resolve(receiver, sel, slot) };
    if let Some(imp) = resolved {
        // SAFETY: as above.
        unsafe { sends::remember(receiver, sel, slot, imp, generation) };
    }
    resolved.unwrap_or(refused)
}

/// Every send [`msg_send`] resolved, cached by the receiver's class and the
/// selector, which its x86-64 prologue probes before it saves a register.
///
/// A message from a game is a Metal or Foundation call, tens of thousands a
/// frame; spilling every argument register to ask Rust was most of their cost.
/// An entry is a sequence lock: odd while it is written, so a probe which reads
/// the same even sequence before and after its loads saw a whole entry. Writers
/// take [`WRITER`]; the probe takes nothing.
mod sends {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU64;

    /// log2 of the entry count.
    pub(super) const BITS: u32 = 12;
    /// The probe's `imul` takes a sign-extended 32-bit immediate.
    pub(super) const MULTIPLIER: i32 = -1_640_531_535;

    #[repr(C, align(32))]
    pub(super) struct Entry {
        sequence: AtomicU64,
        class: AtomicUsize,
        sel: AtomicUsize,
        imp: AtomicUsize,
    }

    #[allow(clippy::declare_interior_mutable_const)]
    const EMPTY: Entry = Entry {
        sequence: AtomicU64::new(0),
        class: AtomicUsize::new(0),
        sel: AtomicUsize::new(0),
        imp: AtomicUsize::new(0),
    };

    pub(super) static TABLE: [Entry; 1 << BITS] = [EMPTY; 1 << BITS];
    /// Held by a writer, and `true` while any entry may be filled.
    static WRITER: Mutex<bool> = Mutex::new(false);

    /// The entry the probe reads for `(class, sel)`: the same arithmetic as
    /// its assembly.
    pub(super) fn index(class: usize, sel: usize) -> usize {
        let key = ((class as u64) << 7) ^ sel as u64;
        (key.wrapping_mul(MULTIPLIER as i64 as u64) >> (64 - BITS)) as usize
    }

    /// The `IMP` cached for `(class, sel)`, read as the probe reads it.
    #[cfg(test)]
    pub(super) fn cached(class: usize, sel: usize) -> Option<usize> {
        let entry = &TABLE[index(class, sel)];
        let before = entry.sequence.load(Ordering::Acquire);
        let found = (entry.class.load(Ordering::Acquire) == class
            && entry.sel.load(Ordering::Acquire) == sel)
            .then(|| entry.imp.load(Ordering::Acquire));
        (before.is_multiple_of(2) && entry.sequence.load(Ordering::Acquire) == before)
            .then_some(found)
            .flatten()
    }

    /// Cache what [`resolve`] found for a direct send, unless the receiver was
    /// forwarded, its class is still being initialized, a method table changed
    /// since `generation`, or hardening is on (the probe checks nothing).
    ///
    /// # Safety
    ///
    /// As [`lookup`].
    pub(super) unsafe fn remember(
        receiver: Id,
        sel: Sel,
        slot: *mut Id,
        imp: *const c_void,
        generation: usize,
    ) {
        if !cfg!(target_arch = "x86_64") || crate::debug::enabled() {
            return;
        }
        // SAFETY: the trampoline's spill slot, by the caller.
        if !slot.is_null() && unsafe { *slot } != receiver {
            return;
        }
        // SAFETY: a live object, whose first word is what the probe reads.
        let isa = unsafe { *(receiver as *const usize) };
        // SAFETY: `resolve` just found a method on this class.
        let Some(class) = (unsafe { crate::class::from_ptr(isa as *const crate::class::Class) })
        else {
            return;
        };
        if !crate::class::is_initialized(class) {
            return;
        }
        // A send which finds the lock taken skips the cache rather than
        // queue behind it: two hot sends sharing an entry would otherwise
        // serialize every thread sending either.
        let Ok(mut filled) = WRITER.try_lock() else {
            return;
        };
        if crate::class::generation() != generation {
            return;
        }
        *filled = true;
        let entry = &TABLE[index(isa, sel as usize)];
        let sequence = entry.sequence.load(Ordering::Relaxed);
        entry.sequence.store(sequence + 1, Ordering::Release);
        entry.class.store(isa, Ordering::Release);
        entry.sel.store(sel as usize, Ordering::Release);
        entry.imp.store(imp as usize, Ordering::Release);
        entry.sequence.store(sequence + 2, Ordering::Release);
    }

    pub(super) fn forget() {
        let mut filled = WRITER.lock().unwrap_or_else(|e| e.into_inner());
        // Classes register methods by the thousand before anything is sent.
        if !std::mem::take(&mut *filled) {
            return;
        }
        for entry in &TABLE {
            let sequence = entry.sequence.load(Ordering::Relaxed);
            entry.sequence.store(sequence + 1, Ordering::Release);
            entry.class.store(0, Ordering::Release);
            entry.sequence.store(sequence + 2, Ordering::Release);
        }
    }
}

/// Drop every cached send, for a method table which changed.
pub(crate) fn forget_cached_sends() {
    sends::forget();
}

/// The method `sel` resolves to for `receiver`, after the class has had its
/// `+resolveInstanceMethod:` (or `+resolveClassMethod:`) chance to add one,
/// and after `-forwardingTargetForSelector:` has had its chance to name
/// another receiver - which is then written to `slot`.
///
/// # Safety
///
/// As [`lookup`].
unsafe fn resolve(receiver: Id, sel: Sel, slot: *mut Id) -> Option<*const c_void> {
    let mut receiver = receiver;
    // Bounded: two proxies naming each other must not hang the send.
    for _ in 0..8 {
        // SAFETY: a live object, by the caller and by each forwarding target.
        let class = unsafe { crate::object::class_of(receiver) }?;
        crate::class::ensure_initialized(class);
        let found = class
            .method(sel)
            .or_else(|| unsafe { resolve_dynamically(class, receiver, sel) });
        if let Some(method) = found {
            if !slot.is_null() {
                // SAFETY: the trampoline's spill slot, by the caller.
                unsafe { *slot = receiver };
            }
            return Some(method.implementation());
        }
        let target = unsafe { forwarding_target(class, receiver, sel) };
        if target.is_null() || target == receiver || slot.is_null() {
            return None;
        }
        receiver = target;
    }
    None
}

/// `+resolveInstanceMethod:`, or `+resolveClassMethod:` for a class
/// receiver: the class may add the method now, and the lookup is tried again.
///
/// # Safety
///
/// `receiver` is a live object of `class`.
unsafe fn resolve_dynamically(
    class: &'static crate::class::Class,
    receiver: Id,
    sel: Sel,
) -> Option<&'static crate::class::Method> {
    let (resolver, asked) = match class.is_meta() {
        true => (crate::sel::get("resolveClassMethod:"), receiver),
        false => (
            crate::sel::get("resolveInstanceMethod:"),
            crate::class::ptr(class) as Id,
        ),
    };
    // Only a class that implements the resolver is asked: the default
    // answers no, and asking through a send would recurse here for a class
    // that has none.
    let meta = match class.is_meta() {
        true => class,
        false => class.metaclass(),
    };
    let method = meta.method(resolver)?;
    if method.implementation() == crate::nsobject::default_resolver() {
        return None;
    }
    // SAFETY: `BOOL (id, SEL, SEL)`, the resolver's prototype.
    let added = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Sel) -> u8 =
            std::mem::transmute(method.implementation());
        f(asked, resolver, sel)
    };
    match added {
        0 => None,
        _ => class.method(sel),
    }
}

/// What `-forwardingTargetForSelector:` names for `sel`, or nil.
///
/// # Safety
///
/// As [`resolve_dynamically`].
unsafe fn forwarding_target(class: &'static crate::class::Class, receiver: Id, sel: Sel) -> Id {
    let forward = crate::sel::get("forwardingTargetForSelector:");
    let Some(method) = class.method(forward) else {
        return std::ptr::null_mut();
    };
    if method.implementation() == crate::nsobject::default_forwarding_target() {
        return std::ptr::null_mut();
    }
    // SAFETY: `id (id, SEL, SEL)`, the method's prototype.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Sel) -> Id =
            std::mem::transmute(method.implementation());
        f(receiver, forward, sel)
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

/// The class and selector of a message nothing answers, as one line, with a
/// class receiver called a class: a missing `+` method otherwise reads as a
/// missing instance method.
///
/// # Safety
///
/// `receiver` is null or a live object.
pub(crate) unsafe fn describe_unrecognised(receiver: Id, sel: Sel) -> String {
    let class = unsafe { crate::object::class_of(receiver) };
    let name = class.map_or("nil", |c| c.name_str());
    let kind = match class.is_some_and(|c| c.is_meta()) {
        true => "class",
        false => "instance of",
    };
    let selector = match sel.is_null() {
        true => std::borrow::Cow::Borrowed("<null selector>"),
        // SAFETY: a registered selector is a NUL-terminated interned name.
        false => unsafe { std::ffi::CStr::from_ptr(sel) }.to_string_lossy(),
    };
    format!("unrecognized selector sent to {kind} {name}: {selector}")
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
unsafe extern "C" fn lookup_stret(receiver: Id, sel: Sel, slot: *mut Id) -> *const c_void {
    let refused = match logging_unrecognised() {
        true => unrecognised_stret_logged as *const c_void,
        false => unrecognised_stret as *const c_void,
    };
    // SAFETY: the caller's obligations.
    unsafe { resolve(receiver, sel, slot) }.unwrap_or(refused)
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
        "add     x2, sp, #128",
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
        // The cached send, probed with r10, r11 and rax, which carries a
        // variadic call's vector count and so is saved around the probe.
        "push    rax",
        "mov     rax, [rdi]",
        // A forgotten or empty entry's class is zero.
        "test    rax, rax",
        "jz      8f",
        "mov     r11, rax",
        "shl     r11, 7",
        "xor     r11, rsi",
        "imul    r11, r11, {multiplier}",
        "shr     r11, {shift}",
        "shl     r11, 5",
        "lea     r10, [rip + {table}]",
        "add     r10, r11",
        "mov     r11, [r10]",
        "test    r11, 1",
        "jnz     8f",
        "cmp     rax, [r10 + 8]",
        "jne     8f",
        "cmp     rsi, [r10 + 16]",
        "jne     8f",
        "mov     rax, [r10 + 24]",
        "cmp     r11, [r10]",
        "jne     8f",
        "mov     r11, rax",
        "pop     rax",
        "jmp     r11",
        "8:",
        "pop     rax",
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
        "lea     rdx, [rsp + 128]",
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
        table = sym sends::TABLE,
        multiplier = const sends::MULTIPLIER,
        shift = const 64 - sends::BITS,
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
        "lea     rdx, [rsp + 136]",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `objc_msgSend` for a message taking no arguments and answering an
    /// integer, called as a guest calls it.
    fn send_answer(receiver: Id, sel: Sel) -> u64 {
        // SAFETY: the trampoline takes any message; this one's signature.
        let send: unsafe extern "C" fn(Id, Sel) -> u64 = unsafe { std::mem::transmute(entry()) };
        // SAFETY: a live receiver whose method has this signature.
        unsafe { send(receiver, sel) }
    }

    unsafe extern "C" fn answer_one(_receiver: Id, _sel: Sel) -> u64 {
        1
    }

    unsafe extern "C" fn answer_two(_receiver: Id, _sel: Sel) -> u64 {
        2
    }

    /// A fresh NSObject subclass answering `sel` with `imp`.
    fn class_answering(
        name: &std::ffi::CStr,
        sel: Sel,
        imp: unsafe extern "C" fn(Id, Sel) -> u64,
    ) -> &'static crate::class::Class {
        crate::bootstrap();
        let nsobject = crate::class::lookup(c"NSObject").expect("NSObject");
        let class = crate::class::allocate_pair(Some(nsobject), name, 0).expect("an unused name");
        assert!(class.add_method(sel, imp as crate::class::Imp, c"Q16@0:8"));
        crate::class::register_pair(class);
        class
    }

    fn isa(object: Id) -> usize {
        // SAFETY: a live object, whose first word is its class.
        unsafe { *(object as *const usize) }
    }

    #[test]
    fn a_cached_send_follows_a_replaced_implementation() {
        let sel = crate::sel::get("rosemaryCachedAnswer");
        let class = class_answering(c"RosemaryCachedSendReplaced", sel, answer_one);
        let instance = crate::object::create_instance(class, 0);
        assert_eq!(send_answer(instance, sel), 1);
        assert_eq!(send_answer(instance, sel), 1);
        if cfg!(target_arch = "x86_64") {
            assert_eq!(
                sends::cached(isa(instance), sel as usize),
                Some(answer_one as *const () as usize)
            );
        }
        class.replace_method(sel, answer_two as crate::class::Imp, c"Q16@0:8");
        assert_eq!(send_answer(instance, sel), 2);
    }

    #[test]
    fn classes_sharing_a_selector_each_reach_their_own_method() {
        let sel = crate::sel::get("rosemaryCachedShared");
        let first = crate::object::create_instance(
            class_answering(c"RosemaryCachedSharedOne", sel, answer_one),
            0,
        );
        let second = crate::object::create_instance(
            class_answering(c"RosemaryCachedSharedTwo", sel, answer_two),
            0,
        );
        for _ in 0..100 {
            assert_eq!(send_answer(first, sel), 1);
            assert_eq!(send_answer(second, sel), 2);
        }
    }

    unsafe extern "C" fn forward_to_target(_receiver: Id, _sel: Sel, _asked: Sel) -> Id {
        FORWARD_TARGET.load(Ordering::Acquire) as Id
    }

    static FORWARD_TARGET: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn a_forwarded_send_is_not_cached_for_the_forwarder() {
        let sel = crate::sel::get("rosemaryCachedForwarded");
        let target = crate::object::create_instance(
            class_answering(c"RosemaryCachedForwardTarget", sel, answer_two),
            0,
        );
        FORWARD_TARGET.store(target as usize, Ordering::Release);
        let nsobject = crate::class::lookup(c"NSObject").expect("NSObject");
        let forwarder_class =
            crate::class::allocate_pair(Some(nsobject), c"RosemaryCachedForwarder", 0)
                .expect("an unused name");
        assert!(forwarder_class.add_method(
            crate::sel::get("forwardingTargetForSelector:"),
            forward_to_target as crate::class::Imp,
            c"@24@0:8:16",
        ));
        crate::class::register_pair(forwarder_class);
        let forwarder = crate::object::create_instance(forwarder_class, 0);
        assert_eq!(send_answer(forwarder, sel), 2);
        assert_eq!(send_answer(forwarder, sel), 2);
        assert_eq!(sends::cached(isa(forwarder), sel as usize), None);
    }

    #[test]
    fn sends_racing_replaced_implementations_reach_one_of_them() {
        let sel = crate::sel::get("rosemaryCachedRace");
        let class = class_answering(c"RosemaryCachedRace", sel, answer_one);
        let instance = crate::object::create_instance(class, 0) as usize;
        let sel_address = sel as usize;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let senders: Vec<_> = (0..4)
            .map(|_| {
                let stop = std::sync::Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut sent = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let answer = send_answer(instance as Id, sel_address as Sel);
                        assert!(answer == 1 || answer == 2, "a send reached {answer}");
                        sent += 1;
                    }
                    sent
                })
            })
            .collect();
        for round in 0..20_000 {
            let imp = match round % 2 {
                0 => answer_two as crate::class::Imp,
                _ => answer_one as crate::class::Imp,
            };
            class.replace_method(sel, imp, c"Q16@0:8");
        }
        stop.store(true, Ordering::Relaxed);
        for sender in senders {
            assert!(sender.join().expect("a sender") > 0);
        }
    }

    /// Dispatch cost of a send whose method is found. Run with `cargo test
    /// --release -p objc -- --ignored --nocapture send_cost`.
    #[test]
    #[ignore = "a benchmark, not a check"]
    fn send_cost() {
        const SENDS: u64 = 20_000_000;
        let sel = crate::sel::get("rosemaryBenchmarkAnswer");
        let instance = crate::object::create_instance(
            class_answering(c"RosemaryBenchmarkSend", sel, answer_one),
            0,
        );
        let start = std::time::Instant::now();
        let mut total = 0;
        for _ in 0..SENDS {
            total += send_answer(std::hint::black_box(instance), sel);
        }
        assert_eq!(total, SENDS);
        eprintln!(
            "send: {:.2} ns each",
            start.elapsed().as_nanos() as f64 / SENDS as f64
        );
    }

    /// A missing `+` method names the class as the receiver, not an instance.
    #[test]
    fn unrecognised_names_a_class_receiver_as_a_class() {
        crate::bootstrap();
        let nsobject = crate::class::lookup(c"NSObject").expect("NSObject");
        let sel = crate::sel::get("noSuchMessage");
        let instance = crate::object::create_instance(nsobject, 0);
        let class = crate::class::ptr(nsobject) as Id;
        // SAFETY: a live instance and a registered class.
        let (to_instance, to_class) = unsafe {
            (
                describe_unrecognised(instance, sel),
                describe_unrecognised(class, sel),
            )
        };
        assert_eq!(
            to_instance,
            "unrecognized selector sent to instance of NSObject: noSuchMessage"
        );
        assert_eq!(
            to_class,
            "unrecognized selector sent to class NSObject: noSuchMessage"
        );
    }
}
