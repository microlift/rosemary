//! The overlay's seam into a guest the native backend runs.
//!
//! An emulated guest's message sends all pass through [`crate::objc::dispatch`],
//! which is where [`super::hook::before_send`] sees a present. A native guest
//! calls the runtime directly, so the seam has to be in its import slots instead:
//! its `objc_msgSend` bindings are pointed at [`send`], which filters on the
//! selector and falls through to the runtime's own.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering::Relaxed};

use machish::memory::Regions;
use machish::report::LinkReport;

/// Selector bits the filter is indexed by.
const FILTER_BITS: u32 = 12;
const FILTER_BUCKETS: usize = 1 << FILTER_BITS;
/// Selectors are pointers into aligned storage, so their lowest bits are always
/// zero and would only crowd the filter.
const FILTER_SHIFT: u32 = 3;

/// Nonzero in every bucket a watched selector falls in.
///
/// Read by [`send`] on every message the guest sends, so it has to be a load and
/// not a search. A collision costs one call into [`sent`], which compares exactly.
static FILTER: [AtomicU8; FILTER_BUCKETS] = [const { AtomicU8::new(0) }; FILTER_BUCKETS];

/// The selected runtime's `objc_msgSend`, which [`send`] always ends in.
static REAL: AtomicU64 = AtomicU64::new(0);

fn bucket(selector: u64) -> usize {
    (selector >> FILTER_SHIFT) as usize & (FILTER_BUCKETS - 1)
}

/// Fill [`FILTER`] and [`REAL`], once, and return the runtime's `objc_msgSend`.
fn armed_send() -> Option<u64> {
    static REAL_SEND: OnceLock<Option<u64>> = OnceLock::new();
    *REAL_SEND.get_or_init(|| {
        let real = crate::objc::host::symbol("objc_msgSend")?;
        for name in super::hook::WATCHED {
            FILTER[bucket(super::objc::sel(name) as u64)].store(1, Relaxed);
        }
        REAL.store(real, Relaxed);
        Some(real)
    })
}

/// Point `report`'s `objc_msgSend` and `CGLFlushDrawable` bindings at the
/// overlay, if `ROSEMARY_GUI` asked for it. Returns how many moved.
///
/// Only bindings that resolved to the runtime's own function move: anything else
/// is something the guest or an interposition chose, and is left alone.
pub fn bind<M: Regions>(report: &mut LinkReport, memory: &mut M) -> Result<usize, machish::Error> {
    if !super::enabled() {
        return Ok(0);
    }
    let Some(real) = armed_send() else {
        super::say_once("no objc_msgSend in the selected runtime, so no present can be seen");
        return Ok(0);
    };
    let flush = crate::shims::function("CGLFlushDrawable");
    let mut rebound = 0;
    for image in &mut report.images {
        for binding in &mut image.bindings {
            let to = match binding.symbol.as_str() {
                "_objc_msgSend" if binding.resolved == real => send as *const () as u64,
                "_CGLFlushDrawable" => {
                    match flush.and_then(|shim| shim.promotion(binding.resolved)) {
                        Some(promotion) => promotion.address,
                        None => continue,
                    }
                }
                _ => continue,
            };
            memory.write(binding.slot_va, &to.to_le_bytes())?;
            binding.resolved = to;
            // The tag names the provider that answered, and none of them did.
            binding.tag = 0;
            rebound += 1;
        }
    }
    if super::debugging() && rebound != 0 {
        eprintln!(
            "rosemary: gui  {rebound} native binding(s) in {} now pass the overlay",
            report.root().map_or("<unnamed>", |image| image
                .name
                .rsplit('/')
                .next()
                .unwrap_or(&image.name)),
        );
    }
    Ok(rebound)
}

/// A send the filter let through: the same seam the emulator's dispatch calls.
extern "C" fn sent(receiver: u64, selector: u64, argument: u64) {
    super::guarded(|| super::hook::before_send(selector, receiver, argument));
}

/// Bytes [`send`] saves on the arm64 slow path: `x0`-`x8`, padded, then `q0`-`q7`.
#[cfg(target_arch = "aarch64")]
const SAVED: usize = 80 + 8 * 16;

/// `objc_msgSend` for a native guest with the overlay on.
///
/// Every register a send may carry an argument in, and the stack, reach the
/// runtime's `objc_msgSend` exactly as the guest left them: `x16` and `x17` are
/// the only registers touched on the way through, and AAPCS64 gives those to
/// the linker for exactly this.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn send() {
    core::arch::naked_asm!(
        "adrp x16, {filter}@PAGE",
        "add x16, x16, {filter}@PAGEOFF",
        "ubfx x17, x1, #{shift}, #{bits}",
        "ldrb w17, [x16, x17]",
        "cbnz w17, 3f",
        "2:",
        "adrp x16, {real}@PAGE",
        "ldr x16, [x16, {real}@PAGEOFF]",
        "br x16",
        "3:",
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "sub sp, sp, #{saved}",
        "stp x0, x1, [sp, #0]",
        "stp x2, x3, [sp, #16]",
        "stp x4, x5, [sp, #32]",
        "stp x6, x7, [sp, #48]",
        "str x8, [sp, #64]",
        "stp q0, q1, [sp, #80]",
        "stp q2, q3, [sp, #112]",
        "stp q4, q5, [sp, #144]",
        "stp q6, q7, [sp, #176]",
        // Receiver, selector and first argument are already where `sent` reads them.
        "bl {sent}",
        "ldp x0, x1, [sp, #0]",
        "ldp x2, x3, [sp, #16]",
        "ldp x4, x5, [sp, #32]",
        "ldp x6, x7, [sp, #48]",
        "ldr x8, [sp, #64]",
        "ldp q0, q1, [sp, #80]",
        "ldp q2, q3, [sp, #112]",
        "ldp q4, q5, [sp, #144]",
        "ldp q6, q7, [sp, #176]",
        "mov sp, x29",
        "ldp x29, x30, [sp], #16",
        "b 2b",
        filter = sym FILTER,
        real = sym REAL,
        sent = sym sent,
        shift = const FILTER_SHIFT,
        bits = const FILTER_BITS,
        saved = const SAVED,
    );
}

/// Bytes [`send`] saves on the x86-64 slow path: six argument registers and
/// `rax`, padded, then `xmm0`-`xmm7`.
#[cfg(target_arch = "x86_64")]
const SAVED: usize = 64 + 8 * 16;

/// The x86-64 form of the arm64 [`send`]. `r10` and `r11` are the scratch
/// registers, which SysV leaves to the callee and `objc_msgSend` itself uses.
/// `rax` is saved because a variadic send carries its vector count in `al`.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn send() {
    core::arch::naked_asm!(
        "mov r11, rsi",
        "shr r11, {shift}",
        "and r11, {mask}",
        "lea r10, [rip + {filter}]",
        "cmp byte ptr [r10 + r11], 0",
        "jne 3f",
        "2:",
        "jmp qword ptr [rip + {real}]",
        "3:",
        "push rbp",
        "mov rbp, rsp",
        "sub rsp, {saved}",
        "mov [rsp], rdi",
        "mov [rsp + 8], rsi",
        "mov [rsp + 16], rdx",
        "mov [rsp + 24], rcx",
        "mov [rsp + 32], r8",
        "mov [rsp + 40], r9",
        "mov [rsp + 48], rax",
        "movaps [rsp + 64], xmm0",
        "movaps [rsp + 80], xmm1",
        "movaps [rsp + 96], xmm2",
        "movaps [rsp + 112], xmm3",
        "movaps [rsp + 128], xmm4",
        "movaps [rsp + 144], xmm5",
        "movaps [rsp + 160], xmm6",
        "movaps [rsp + 176], xmm7",
        // Receiver, selector and first argument are already where `sent` reads them.
        "call {sent}",
        "mov rdi, [rsp]",
        "mov rsi, [rsp + 8]",
        "mov rdx, [rsp + 16]",
        "mov rcx, [rsp + 24]",
        "mov r8, [rsp + 32]",
        "mov r9, [rsp + 40]",
        "mov rax, [rsp + 48]",
        "movaps xmm0, [rsp + 64]",
        "movaps xmm1, [rsp + 80]",
        "movaps xmm2, [rsp + 96]",
        "movaps xmm3, [rsp + 112]",
        "movaps xmm4, [rsp + 128]",
        "movaps xmm5, [rsp + 144]",
        "movaps xmm6, [rsp + 160]",
        "movaps xmm7, [rsp + 176]",
        "mov rsp, rbp",
        "pop rbp",
        "jmp 2b",
        filter = sym FILTER,
        real = sym REAL,
        sent = sym sent,
        shift = const FILTER_SHIFT,
        mask = const FILTER_BUCKETS - 1,
        saved = const SAVED,
    );
}

#[cfg(test)]
mod tests {
    use super::super::objc::{Id, Sel, class, sel};
    use super::*;

    /// `BOOL (*)(id, SEL, id)`, which is `-isEqual:`. `BOOL` is a byte on both
    /// architectures, signed on one of them.
    type IsEqual = unsafe extern "C" fn(Id, Sel, Id) -> u8;

    /// A send the filter stops still reaches the runtime, with the arguments
    /// the guest gave it.
    #[test]
    fn a_filtered_send_reaches_the_runtime_with_its_arguments() {
        armed_send().expect("every macOS Objective-C runtime has objc_msgSend");
        let object = class("NSObject");
        assert!(!object.is_null(), "no NSObject in the selected runtime");
        let is_equal = sel("isEqual:");
        // Marked here rather than hoped for, so the slow path is certainly the
        // one taken. A stray mark costs only a call into `sent`.
        FILTER[bucket(is_equal as u64)].store(1, Relaxed);
        // SAFETY: `send` takes exactly `objc_msgSend`'s arguments, and
        // `+[NSObject isEqual:]` is the root class's instance method, which its
        // metaclass inherits.
        let send: IsEqual = unsafe { std::mem::transmute(send as *const ()) };
        assert_ne!(unsafe { send(object, is_equal, object) }, 0);
        assert_eq!(unsafe { send(object, is_equal, std::ptr::null_mut()) }, 0);
    }
}
