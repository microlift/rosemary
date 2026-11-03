use microlift::arch::x86_64::context::RegisterContext;

use super::bad_pointer;
use crate::abi::{GuestMemory, Result};
use crate::shims::bind;

const GUEST_PAGE: u64 = 4096;

/// Probe the pages the guest will occupy after its following `sub rsp, rax`.
///
/// Reading is sufficient.
pub fn chkstk(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let size = ctx.rax;
    if size == 0 {
        return Ok(());
    }

    let mut distance = GUEST_PAGE;
    while distance < size {
        probe_at(symbol, ctx.rsp, distance, mem)?;
        distance = distance
            .checked_add(GUEST_PAGE)
            .ok_or_else(|| bad_pointer(symbol, ctx.rsp))?;
    }
    probe_at(symbol, ctx.rsp, size, mem)
}

fn probe_at(symbol: &str, rsp: u64, distance: u64, mem: &mut dyn GuestMemory) -> Result<()> {
    let address = rsp
        .checked_sub(distance)
        .ok_or_else(|| bad_pointer(symbol, rsp))?;
    let mut byte = [0u8; 1];
    mem.read(address, &mut byte)
        .map_err(|_| bad_pointer(symbol, address))
}

pub type Shim = crate::shims::Shim;

pub fn shim_for(name: &str) -> Option<Shim> {
    (name == "___chkstk_darwin").then_some(bind!("___chkstk_darwin" => chkstk))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::GuestMemory;

    struct Memory {
        reads: std::cell::RefCell<Vec<u64>>,
    }

    impl GuestMemory for Memory {
        fn read(&self, va: u64, _out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            self.reads.borrow_mut().push(va);
            Ok(())
        }

        fn write(&mut self, va: u64, _data: &[u8]) -> core::result::Result<(), machish::Error> {
            let _ = va;
            Ok(())
        }
    }

    #[test]
    fn preserves_the_size_in_rax_and_claims_the_macho_spelling() {
        assert!(shim_for("___chkstk_darwin").is_some());
        let mut ctx = RegisterContext::new();
        ctx.rax = 8192;
        ctx.rsp = 0x20_000;
        let mem_reads = std::cell::RefCell::new(Vec::new());
        let mut mem = Memory { reads: mem_reads };
        chkstk("___chkstk_darwin", &mut ctx, &mut mem).expect("mapped guest stack");
        assert_eq!(ctx.rax, 8192);
        assert_eq!(&*mem.reads.borrow(), &[0x1f000, 0x1e000]);
    }
}
