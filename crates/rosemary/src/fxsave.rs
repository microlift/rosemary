//! `FXSAVE64` and `FXRSTOR64`, which the recompiler leaves to [`crate::interp`].
//! Wine's syscall dispatcher saves and restores the FPU state with them on a
//! CPU that does not advertise `XSAVE`.

use iced_x86::Instruction;

use microlift::arch::x86_64::context::{MXCSR_MASK, RegisterContext};

use crate::abi::GuestMemory;
use crate::vm::VmError;

/// The 512-byte area, as the SDM lays out its 64-bit form.
const AREA: usize = 512;
const FCW: usize = 0;
const FSW: usize = 2;
const FTW: usize = 4;
const MXCSR: usize = 24;
const MXCSR_MASK_AT: usize = 28;
const ST0: usize = 32;
const XMM0: usize = 160;
/// Each x87 and XMM register takes sixteen bytes.
const SLOT: usize = 16;
/// The area must be sixteen-byte aligned, or the instruction is a `#GP`.
const ALIGN: u64 = 16;

/// `TOP`, bits 11-13 of the status word.
const FSW_TOP_SHIFT: u16 = 11;
const FSW_TOP: u16 = 0b111 << FSW_TOP_SHIFT;

/// The FPU state as `FXSAVE64` would store it.
///
/// FIXME (ACCURACY): the last instruction and data pointers and the opcode are
/// stored as zero, since the recompiler does not track them.
pub fn image(ctx: &RegisterContext) -> [u8; AREA] {
    let mut area = [0u8; AREA];
    let sw = (ctx.fpu_sw & !FSW_TOP) | ((ctx.fpu_top & 0b111) << FSW_TOP_SHIFT);
    area[FCW..FCW + 2].copy_from_slice(&ctx.fpu_cw.to_le_bytes());
    area[FSW..FSW + 2].copy_from_slice(&sw.to_le_bytes());
    area[FTW] = ctx.fpu_tag as u8;
    area[MXCSR..MXCSR + 4].copy_from_slice(&ctx.mxcsr.to_le_bytes());
    area[MXCSR_MASK_AT..MXCSR_MASK_AT + 4].copy_from_slice(&MXCSR_MASK.to_le_bytes());
    // The area holds the stack in `ST(i)` order; the context, physically.
    for i in 0..8 {
        let [mantissa, sign_exp] = ctx.fpr[(usize::from(ctx.fpu_top) + i) & 7];
        let at = ST0 + i * SLOT;
        area[at..at + 8].copy_from_slice(&mantissa.to_le_bytes());
        area[at + 8..at + 10].copy_from_slice(&(sign_exp as u16).to_le_bytes());
    }
    for (i, [lo, hi]) in ctx.xmm.iter().enumerate() {
        let at = XMM0 + i * SLOT;
        area[at..at + 8].copy_from_slice(&lo.to_le_bytes());
        area[at + 8..at + 16].copy_from_slice(&hi.to_le_bytes());
    }
    area
}

/// Load the FPU state from an area as `FXRSTOR64` does, or say why the guest's
/// CPU would have refused it.
pub fn restore(ctx: &mut RegisterContext, area: &[u8; AREA]) -> Result<(), &'static str> {
    let word = |at: usize| u16::from_le_bytes([area[at], area[at + 1]]);
    let mxcsr = u32::from_le_bytes(area[MXCSR..MXCSR + 4].try_into().unwrap());
    if mxcsr & !MXCSR_MASK != 0 {
        return Err("reserved MXCSR bits set, which is a #GP");
    }
    let sw = word(FSW);
    ctx.fpu_cw = word(FCW);
    ctx.fpu_top = (sw & FSW_TOP) >> FSW_TOP_SHIFT;
    ctx.fpu_sw = sw & !FSW_TOP;
    ctx.fpu_tag = u16::from(area[FTW]);
    ctx.mxcsr = mxcsr;
    for i in 0..8 {
        let at = ST0 + i * SLOT;
        let mantissa = u64::from_le_bytes(area[at..at + 8].try_into().unwrap());
        let sign_exp = u64::from(word(at + 8));
        ctx.fpr[(usize::from(ctx.fpu_top) + i) & 7] = [mantissa, sign_exp];
    }
    for (i, xmm) in ctx.xmm.iter_mut().enumerate() {
        let at = XMM0 + i * SLOT;
        *xmm = [
            u64::from_le_bytes(area[at..at + 8].try_into().unwrap()),
            u64::from_le_bytes(area[at + 8..at + 16].try_into().unwrap()),
        ];
    }
    Ok(())
}

/// Perform the `FXSAVE64` or `FXRSTOR64` at the context's PC, which the
/// interpreter then steps over.
pub fn service(
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    instruction: &Instruction,
    save: bool,
) -> Result<(), VmError> {
    let pc = ctx.rip;
    let refused = |what: String| VmError::Interpret { pc, what };
    let address = crate::atomic128::effective_address(instruction, ctx)
        .ok_or_else(|| refused("an operand this runtime cannot address".into()))?;
    if address % ALIGN != 0 {
        return Err(refused(format!(
            "a misaligned area at {address:#x}, which is a #GP on the guest's own CPU"
        )));
    }
    let unmapped = |_| refused(format!("an area at {address:#x} that is not mapped"));
    if save {
        return mem.write(address, &image(ctx)).map_err(unmapped);
    }
    let mut area = [0u8; AREA];
    mem.read(address, &mut area).map_err(unmapped)?;
    restore(ctx, &area).map_err(|why| refused(why.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restored_image_is_the_state_it_was_taken_from() {
        let mut ctx = RegisterContext::new();
        ctx.fpu_top = 5;
        ctx.fpu_sw = 0x4100;
        ctx.fpu_tag = 0b1110_0000;
        ctx.mxcsr = 0x1FA0;
        ctx.fpr[5] = [0x8000_0000_0000_0000, 0x3FFF];
        ctx.xmm[3] = [1, 2];
        let area = image(&ctx);
        assert_eq!(
            (u16::from_le_bytes([area[FSW], area[FSW + 1]]) & FSW_TOP) >> 11,
            5
        );
        assert_eq!(
            area[ST0..ST0 + 8],
            0x8000_0000_0000_0000u64.to_le_bytes(),
            "ST(0) first"
        );

        let mut back = RegisterContext::new();
        restore(&mut back, &area).unwrap();
        assert_eq!(
            (back.fpu_top, back.fpu_sw, back.fpu_tag),
            (5, 0x4100, 0b1110_0000)
        );
        assert_eq!(
            (back.mxcsr, back.fpr[5], back.xmm[3]),
            (0x1FA0, ctx.fpr[5], [1, 2])
        );
    }

    #[test]
    fn reserved_mxcsr_bits_are_refused() {
        let mut area = image(&RegisterContext::new());
        area[MXCSR + 2] = 1;
        assert!(restore(&mut RegisterContext::new(), &area).is_err());
    }
}
