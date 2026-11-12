use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic};

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::GuestMemory;
use crate::vm::VmError;

/// Longest x86-64 instruction, and so the widest window a decode needs.
const MAX_LEN: usize = 15;

/// Perform `count` guest instructions from the context's PC, leaving it past the
/// last of them.
pub fn run(ctx: &mut RegisterContext, mem: &dyn GuestMemory, count: u64) -> Result<(), VmError> {
    for _ in 0..count {
        step(ctx, mem)?;
    }
    Ok(())
}

/// Perform the one instruction at the context's PC.
fn step(ctx: &mut RegisterContext, mem: &dyn GuestMemory) -> Result<(), VmError> {
    let at = ctx.rip;
    let instruction = decode_at(at, mem).ok_or(VmError::Interpret {
        pc: at,
        what: "nothing decodes there".into(),
    })?;
    match instruction.mnemonic() {
        Mnemonic::Cmpxchg16b => crate::atomic128::service(ctx, &instruction)?,
        // `Debug` is the enum variant, which is the mnemonic *plus* its operand
        // shape - `Vsqrtsd_xmm_xmm_xmmm64` rather than just `vsqrtsd` - and so
        // names the lowering the recompiler is actually missing.
        other => {
            return Err(VmError::Interpret {
                pc: at,
                what: format!("{other:?} ({:?})", instruction.code()),
            });
        }
    }
    ctx.rip = instruction.next_ip();
    Ok(())
}

/// The instruction at `pc`, decoded out of guest memory.
///
/// The window shrinks rather than failing outright, because the last instruction
/// of a mapping has fewer than [`MAX_LEN`] readable bytes behind it and is still
/// an instruction.
fn decode_at(pc: u64, mem: &dyn GuestMemory) -> Option<Instruction> {
    let mut window = [0u8; MAX_LEN];
    let mut len = MAX_LEN;
    while mem.read(pc, &mut window[..len]).is_err() {
        len -= 1;
        if len == 0 {
            return None;
        }
    }
    let mut decoder = Decoder::with_ip(64, &window[..len], pc, DecoderOptions::NONE);
    let instruction = decoder.decode();
    (!instruction.is_invalid()).then_some(instruction)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guest memory that is one buffer at one address.
    struct Bytes {
        at: u64,
        bytes: Vec<u8>,
    }

    impl GuestMemory for Bytes {
        fn read(&self, va: u64, out: &mut [u8]) -> Result<(), machish::Error> {
            let oob = machish::Error::OutOfBounds {
                addr: va,
                len: out.len(),
            };
            let start = va.wrapping_sub(self.at) as usize;
            let src = start
                .checked_add(out.len())
                .and_then(|end| self.bytes.get(start..end))
                .ok_or(oob)?;
            out.copy_from_slice(src);
            Ok(())
        }

        fn write(&mut self, va: u64, data: &[u8]) -> Result<(), machish::Error> {
            Err(machish::Error::OutOfBounds {
                addr: va,
                len: data.len(),
            })
        }
    }

    #[test]
    fn an_instruction_at_the_end_of_a_mapping_still_decodes() {
        // `lock cmpxchg16b [rdi]`, five bytes and nothing readable past them.
        let mem = Bytes {
            at: 0x1000,
            bytes: vec![0xF0, 0x48, 0x0F, 0xC7, 0x0F],
        };
        let instruction = decode_at(0x1000, &mem).expect("a decode");
        assert_eq!(instruction.mnemonic(), Mnemonic::Cmpxchg16b);
        assert_eq!(instruction.next_ip(), 0x1005);
    }

    /// Sixteen-byte-aligned storage, which the CAS requires and a plain `u128`
    /// local does not promise.
    #[repr(align(16))]
    struct Aligned(u128);

    #[test]
    fn a_cmpxchg16b_is_performed_and_stepped_over() {
        let mut cell = Aligned(0);
        // `lock cmpxchg16b [rdi]`, the one instruction this module performs.
        let mem = Bytes {
            at: 0x1000,
            bytes: vec![0xF0, 0x48, 0x0F, 0xC7, 0x0F],
        };
        let mut ctx = RegisterContext::new();
        ctx.rip = 0x1000;
        ctx.rdi = &mut cell.0 as *mut u128 as u64;
        // The pair the guest expects to find, which is what is there.
        ctx.rax = 0;
        ctx.rdx = 0;
        ctx.rbx = 7;
        ctx.rcx = 9;

        run(&mut ctx, &mem, 1).expect("the swap");
        assert_eq!(cell.0, u128::from(7u64) | (u128::from(9u64) << 64));
        assert_eq!(ctx.zf, 1, "a matching pair sets ZF");
        assert_eq!(ctx.rip, 0x1005, "and the PC is past the instruction");

        // And again, now that the pair no longer matches.
        ctx.rip = 0x1000;
        run(&mut ctx, &mem, 1).expect("the compare");
        assert_eq!(ctx.zf, 0);
        assert_eq!((ctx.rax, ctx.rdx), (7, 9), "which reads the pair back");
    }

    #[test]
    fn a_run_is_performed_in_order_up_to_a_refused_instruction() {
        let mut cell = Aligned(0);
        // Two `lock cmpxchg16b [rdi]`, then a `ud0` this module has no case for.
        let mem = Bytes {
            at: 0x1000,
            bytes: vec![
                0xF0, 0x48, 0x0F, 0xC7, 0x0F, 0xF0, 0x48, 0x0F, 0xC7, 0x0F, 0x0F, 0xFF, 0xC0,
            ],
        };
        let mut ctx = RegisterContext::new();
        ctx.rip = 0x1000;
        ctx.rdi = &mut cell.0 as *mut u128 as u64;
        ctx.rbx = 7;
        ctx.rcx = 9;

        let err = run(&mut ctx, &mem, 3).expect_err("no case for the third");
        let VmError::Interpret { pc, .. } = err else {
            panic!("wrong error: {err}");
        };
        assert_eq!(pc, 0x100A);
        assert_eq!(ctx.rip, 0x100A, "the PC stops at the refused instruction");
        assert_eq!(cell.0, u128::from(7u64) | (u128::from(9u64) << 64));
        assert_eq!(
            (ctx.zf, ctx.rax, ctx.rdx),
            (0, 7, 9),
            "the second swap saw what the first one stored"
        );
    }

    #[test]
    fn an_instruction_the_host_does_not_perform_is_named() {
        // `ud0`, which decodes and which this module has no case for.
        let mem = Bytes {
            at: 0x2000,
            bytes: vec![0x0F, 0xFF, 0xC0],
        };
        let mut ctx = RegisterContext::new();
        ctx.rip = 0x2000;
        let err = step(&mut ctx, &mem).expect_err("no case for it");
        let VmError::Interpret { pc, what } = err else {
            panic!("wrong error: {err}");
        };
        assert_eq!(pc, 0x2000);
        assert!(what.contains("Ud0"), "{what}");
        assert_eq!(ctx.rip, 0x2000, "a refused instruction is not stepped over");
    }
}
