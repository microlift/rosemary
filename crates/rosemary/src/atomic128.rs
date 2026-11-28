use iced_x86::{Instruction, Register};

use microlift::arch::x86_64::context::RegisterContext;

use crate::vm::VmError;

/// Perform the `CMPXCHG16B` the recompiler handed to [`crate::interp`].
///
/// The PC is left alone: the interpreter steps it over the instruction.
pub fn service(ctx: &mut RegisterContext, instruction: &Instruction) -> Result<(), VmError> {
    let at = ctx.rip;
    let refused = |what: String| VmError::Interpret { pc: at, what };
    let address = effective_address(instruction, ctx)
        .ok_or_else(|| refused("an operand this runtime cannot address".into()))?;
    if address % 16 != 0 {
        return Err(refused(format!(
            "a misaligned operand at {address:#x}, which is a #GP on the guest's own CPU"
        )));
    }

    let expected = u128::from(ctx.rax) | (u128::from(ctx.rdx) << 64);
    let desired = u128::from(ctx.rbx) | (u128::from(ctx.rcx) << 64);

    // SAFETY: `address` is sixteen-byte aligned and inside the guest's own
    // mapped memory, which is identity-mapped into this process and writable -
    // a `lock cmpxchg16b` the guest issued is a write it is entitled to make.
    // Concurrency is the point rather than a hazard: every guest `CMPXCHG16B`
    // reaches this function, so the exclusive monitor sees all of them.
    let found = unsafe { compare_exchange(address as usize as *mut u128, expected, desired) };

    match found == expected {
        true => ctx.zf = 1,
        false => {
            ctx.zf = 0;
            ctx.rax = found as u64;
            ctx.rdx = (found >> 64) as u64;
        }
    }
    Ok(())
}

/// A real 128-bit compare-and-swap, returning what was in memory.
///
/// `ldaxp`/`stlxp` rather than LSE's `caspal`:
///
/// # Safety
///
/// `at` must be a valid, sixteen-byte-aligned, writable address for the duration of the
/// call.
#[cfg(target_arch = "aarch64")]
unsafe fn compare_exchange(at: *mut u128, expected: u128, desired: u128) -> u128 {
    let (expect_lo, expect_hi) = (expected as u64, (expected >> 64) as u64);
    let (desire_lo, desire_hi) = (desired as u64, (desired >> 64) as u64);
    let (mut found_lo, mut found_hi): (u64, u64);
    // SAFETY: the caller's guarantee about `at`. The loop is the standard
    // exclusive-pair CAS: load the pair with acquire, compare, and either store
    // the new pair with release or break out having *cleared* the monitor with
    // `clrex` - leaving it set would make an unrelated later `stxp` on this core
    // succeed when it should not.
    unsafe {
        std::arch::asm!(
            "2:",
            "ldaxp {found_lo}, {found_hi}, [{at}]",
            "cmp {found_lo}, {expect_lo}",
            "ccmp {found_hi}, {expect_hi}, #0, eq",
            "b.ne 3f",
            "stlxp {status:w}, {desire_lo}, {desire_hi}, [{at}]",
            "cbnz {status:w}, 2b",
            "b 4f",
            "3:",
            "clrex",
            "4:",
            at = in(reg) at,
            expect_lo = in(reg) expect_lo,
            expect_hi = in(reg) expect_hi,
            desire_lo = in(reg) desire_lo,
            desire_hi = in(reg) desire_hi,
            found_lo = out(reg) found_lo,
            found_hi = out(reg) found_hi,
            status = out(reg) _,
            options(nostack),
        );
    }
    u128::from(found_lo) | (u128::from(found_hi) << 64)
}

/// The same operation on the host that has the guest's own instruction.
///
/// The guest only reaches here when [`Atomic128Mode::Interpret`] was chosen, which
/// on an x86-64 host means the CPU lacks `CX16` - so this is the servicing path for
/// a feature the lifted code could not use inline, not a translation.
///
/// [`Atomic128Mode::Interpret`]: microlift::core::Atomic128Mode
///
/// # Safety
///
/// As the aarch64 arm.
#[cfg(target_arch = "x86_64")]
unsafe fn compare_exchange(at: *mut u128, expected: u128, desired: u128) -> u128 {
    let (expect_lo, expect_hi) = (expected as u64, (expected >> 64) as u64);
    let (desire_lo, desire_hi) = (desired as u64, (desired >> 64) as u64);
    let (found_lo, found_hi): (u64, u64);
    // SAFETY: the caller's guarantee about `at`. `rbx` is half the operand pair
    // but LLVM will not hand it over as an operand, so it is saved and restored
    // around the instruction; nothing between the two moves can observe it gone.
    // The other operands are pinned to explicit registers precisely so that the
    // allocator cannot put one of them in `rbx` behind our back.
    unsafe {
        std::arch::asm!(
            "mov r8, rbx",
            "mov rbx, rdi",
            "lock cmpxchg16b [rsi]",
            "mov rbx, r8",
            in("rsi") at,
            in("rdi") desire_lo,
            out("r8") _,
            inout("rax") expect_lo => found_lo,
            inout("rdx") expect_hi => found_hi,
            in("rcx") desire_hi,
            options(nostack),
        );
    }
    u128::from(found_lo) | (u128::from(found_hi) << 64)
}

/// The address the memory operand names, from the guest's own registers.
pub(crate) fn effective_address(instruction: &Instruction, ctx: &RegisterContext) -> Option<u64> {
    if instruction.segment_prefix() != Register::None {
        return None;
    }
    let base = match instruction.memory_base() {
        Register::None => 0,
        // RIP-relative, where iced has already folded the displacement against
        // the IP the decoder was given - which is why [`crate::interp`] decodes
        // at the instruction's own address.
        Register::RIP => return Some(instruction.memory_displacement64()),
        reg => read(reg, ctx)?,
    };
    let index = match instruction.memory_index() {
        Register::None => 0,
        reg => read(reg, ctx)?.wrapping_mul(u64::from(instruction.memory_index_scale())),
    };
    Some(
        base.wrapping_add(index)
            .wrapping_add(instruction.memory_displacement64()),
    )
}

/// One 64-bit register out of the guest's context.
fn read(reg: Register, ctx: &RegisterContext) -> Option<u64> {
    Some(match reg {
        Register::RAX => ctx.rax,
        Register::RCX => ctx.rcx,
        Register::RDX => ctx.rdx,
        Register::RBX => ctx.rbx,
        Register::RSP => ctx.rsp,
        Register::RBP => ctx.rbp,
        Register::RSI => ctx.rsi,
        Register::RDI => ctx.rdi,
        Register::R8 => ctx.r8,
        Register::R9 => ctx.r9,
        Register::R10 => ctx.r10,
        Register::R11 => ctx.r11,
        Register::R12 => ctx.r12,
        Register::R13 => ctx.r13,
        Register::R14 => ctx.r14,
        Register::R15 => ctx.r15,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sixteen-byte-aligned storage, which `compare_exchange` requires and a
    /// plain `u128` local does not promise.
    #[repr(align(16))]
    struct Aligned(u128);

    #[test]
    fn a_matching_pair_is_swapped_and_a_mismatched_one_is_read_back() {
        let mut cell = Aligned(0x1111_2222_3333_4444_5555_6666_7777_8888);
        // SAFETY: `cell` is live, aligned by its own `repr`, and this thread is
        // the only one touching it.
        let found = unsafe { compare_exchange(&mut cell.0, cell.0, 0xdead) };
        assert_eq!(found, 0x1111_2222_3333_4444_5555_6666_7777_8888);
        assert_eq!(cell.0, 0xdead);

        let found = unsafe { compare_exchange(&mut cell.0, 0, 0xbeef) };
        assert_eq!(found, 0xdead, "a failed CAS reports what was there");
        assert_eq!(cell.0, 0xdead, "and leaves it alone");
    }

    #[test]
    fn the_swap_is_atomic_across_threads() {
        // The whole point, and the property `Atomic128Mode::Emulated` does not
        // have.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        let cell = Arc::new(AtomicU64::new(0));
        let boxed: Arc<std::sync::Mutex<Box<Aligned>>> =
            Arc::new(std::sync::Mutex::new(Box::new(Aligned(0))));
        let at = {
            let guard = boxed.lock().unwrap();
            (&guard.0 as *const u128) as usize
        };
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let cell = Arc::clone(&cell);
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        loop {
                            // SAFETY: `at` is the boxed, aligned cell, which
                            // outlives every thread here because `boxed` is
                            // joined below. Racing on it is the test.
                            let current = unsafe { compare_exchange(at as *mut u128, 0, 0) };
                            let next = current.wrapping_add(1);
                            let found = unsafe { compare_exchange(at as *mut u128, current, next) };
                            if found == current {
                                break;
                            }
                        }
                    }
                    cell.fetch_add(1, Ordering::Relaxed);
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a worker");
        }
        let guard = boxed.lock().unwrap();
        assert_eq!(
            guard.0, 8000,
            "an increment was lost, so the CAS is not atomic"
        );
    }

    #[test]
    fn a_failed_compare_leaves_the_monitor_clear() {
        // `clrex` on the mismatch path.
        let mut cell = Aligned(0);
        for i in 0..1000u128 {
            // SAFETY: `cell` is live, aligned, and thread-local here.
            let found = unsafe { compare_exchange(&mut cell.0, i + 500, 0xbad) };
            assert_eq!(found, i, "iteration {i}");
            assert_eq!(cell.0, i, "a mismatched CAS must not store");
            unsafe { compare_exchange(&mut cell.0, i, i + 1) };
        }
        assert_eq!(cell.0, 1000);
    }
}
