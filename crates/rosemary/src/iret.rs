//! `IRETQ` in user mode, which the recompiler leaves to [`crate::interp`]. Wine
//! returns into a whole saved context with it: the PC, flags and stack at once.

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::GuestMemory;
use crate::vm::VmError;

/// The frame `IRETQ` pops: `RIP`, `CS`, `RFLAGS`, `RSP`, `SS`.
const FRAME_WORDS: usize = 5;

/// The `RFLAGS` bits the context keeps.
mod flag {
    pub const CF: u64 = 1 << 0;
    pub const PF: u64 = 1 << 2;
    pub const AF: u64 = 1 << 4;
    pub const ZF: u64 = 1 << 6;
    pub const SF: u64 = 1 << 7;
    /// The trap flag, which would single-step what follows.
    pub const TF: u64 = 1 << 8;
    pub const DF: u64 = 1 << 10;
    pub const OF: u64 = 1 << 11;
}

/// Put the arithmetic flags and `DF` from an `RFLAGS` image into the context.
fn set_flags(ctx: &mut RegisterContext, rflags: u64) {
    let bit = |mask: u64| u8::from(rflags & mask != 0);
    ctx.cf = bit(flag::CF);
    ctx.set_pf(rflags & flag::PF != 0);
    ctx.set_af(rflags & flag::AF != 0);
    ctx.zf = bit(flag::ZF);
    ctx.sf = bit(flag::SF);
    ctx.df = bit(flag::DF);
    ctx.of = bit(flag::OF);
}

/// Perform the `IRETQ` at the context's PC, leaving the PC where it returns to.
///
/// A frame that changes the code or stack segment is refused: returning to
/// another code segment is how a 64-bit thread enters 32-bit code, which the
/// recompiler does not run.
pub fn service(ctx: &mut RegisterContext, mem: &dyn GuestMemory) -> Result<(), VmError> {
    let pc = ctx.rip;
    let refused = |what: String| VmError::Interpret { pc, what };
    let mut frame = [0u8; FRAME_WORDS * 8];
    mem.read(ctx.rsp, &mut frame)
        .map_err(|_| refused(format!("a frame at {:#x} that is not mapped", ctx.rsp)))?;
    let word = |i: usize| u64::from_le_bytes(frame[i * 8..i * 8 + 8].try_into().unwrap());
    let (rip, cs, rflags, rsp, ss) = (word(0), word(1), word(2), word(3), word(4));
    if cs as u16 != ctx.cs || ss as u16 != ctx.ss {
        return Err(refused(format!(
            "a return to segments {cs:#x}:{ss:#x} rather than this thread's own"
        )));
    }
    if rflags & flag::TF != 0 {
        return Err(refused("a return with the trap flag set".into()));
    }
    set_flags(ctx, rflags);
    ctx.rsp = rsp;
    ctx.rip = rip;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stack(Vec<u8>, u64);

    impl GuestMemory for Stack {
        fn read(&self, va: u64, out: &mut [u8]) -> Result<(), machish::Error> {
            let at = (va - self.1) as usize;
            out.copy_from_slice(&self.0[at..at + out.len()]);
            Ok(())
        }

        fn write(&mut self, va: u64, data: &[u8]) -> Result<(), machish::Error> {
            Err(machish::Error::OutOfBounds {
                addr: va,
                len: data.len(),
            })
        }
    }

    fn frame(ctx: &RegisterContext, rip: u64, cs: u64, rflags: u64, rsp: u64) -> Stack {
        let words = [rip, cs, rflags, rsp, u64::from(ctx.ss)];
        Stack(words.iter().flat_map(|w| w.to_le_bytes()).collect(), 0x1000)
    }

    #[test]
    fn returns_to_the_frame_with_its_flags_and_stack() {
        let mut ctx = RegisterContext::new();
        ctx.rsp = 0x1000;
        let stack = frame(
            &ctx,
            0x4000,
            u64::from(ctx.cs),
            flag::ZF | flag::CF | 0x202,
            0x8000,
        );
        service(&mut ctx, &stack).unwrap();
        assert_eq!((ctx.rip, ctx.rsp), (0x4000, 0x8000));
        assert_eq!((ctx.zf, ctx.cf, ctx.sf), (1, 1, 0));
    }

    #[test]
    fn a_change_of_code_segment_is_refused() {
        let mut ctx = RegisterContext::new();
        ctx.rsp = 0x1000;
        let stack = frame(&ctx, 0x4000, 0x23, 0x202, 0x8000);
        assert!(service(&mut ctx, &stack).is_err());
        assert_eq!(ctx.rip, 0, "nothing is popped");
    }
}
