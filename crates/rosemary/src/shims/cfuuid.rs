use microlift::arch::x86_64::context::RegisterContext;

use crate::shims::bind;
use crate::{
    abi::{GuestMemory, Result},
    shims::host::builtin_or_host,
};

/// Sixteen bytes as the two ABIs carry them: a pair of doublewords.
///
/// `#[repr(C)]` and exactly two `u64`s, so Rust returns it in `x0`/`x1` and passes it
/// in the next two integer registers, which is what the host's own declaration does
/// with the `unsigned char [16]` it really is.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Bytes {
    lo: u64,
    hi: u64,
}

/// `CFUUIDBytes CFUUIDGetUUIDBytes(CFUUIDRef)`
fn get_uuid_bytes(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = builtin_or_host(symbol)?;
    // SAFETY: the symbol [`shim_for`] matched, which is
    // `CFUUIDBytes (*)(CFUUIDRef)`, resolved out of a CoreFoundation this
    // process has loaded. The `CFUUIDRef` is a host object the guest received
    // from `CFUUIDCreate`.
    let f: extern "C" fn(u64) -> Bytes =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64) -> Bytes>(host) };
    let bytes = f(ctx.rdi);
    ctx.rax = bytes.lo;
    ctx.rdx = bytes.hi;
    Ok(())
}

/// `CFUUIDRef CFUUIDCreateFromUUIDBytes(CFAllocatorRef, CFUUIDBytes)`
fn create_from_uuid_bytes(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = builtin_or_host(symbol)?;
    // SAFETY: as above, for `CFUUIDRef (*)(CFAllocatorRef, CFUUIDBytes)`.
    let f: extern "C" fn(u64, Bytes) -> u64 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, Bytes) -> u64>(host) };
    ctx.rax = f(
        ctx.rdi,
        Bytes {
            lo: ctx.rsi,
            hi: ctx.rdx,
        },
    );
    Ok(())
}

pub type Shim = crate::shims::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "CFUUIDGetUUIDBytes" => Some(bind!("CFUUIDGetUUIDBytes" => get_uuid_bytes)),
        "CFUUIDCreateFromUUIDBytes" => {
            Some(bind!("CFUUIDCreateFromUUIDBytes" => create_from_uuid_bytes))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The round trip, through the host's own CoreFoundation.
    ///
    /// Checked rather than asserted, because the whole claim is that sixteen bytes
    /// survive a pair of registers in both directions.
    #[test]
    fn sixteen_bytes_survive_the_round_trip() {
        let Some(create) = crate::host::symbol("CFUUIDCreateFromUUIDBytes") else {
            return;
        };
        let Some(get) = crate::host::symbol("CFUUIDGetUUIDBytes") else {
            return;
        };
        let want = Bytes {
            lo: 0x0011_2233_4455_6677,
            hi: 0x8899_aabb_ccdd_eeff,
        };
        // SAFETY: both are the CoreFoundation entry points named, resolved from
        // this process's own image list.
        let (create, get) = unsafe {
            (
                core::mem::transmute::<u64, extern "C" fn(u64, Bytes) -> u64>(create),
                core::mem::transmute::<u64, extern "C" fn(u64) -> Bytes>(get),
            )
        };
        let uuid = create(0, want);
        assert_ne!(uuid, 0, "CoreFoundation made no UUID");
        let back = get(uuid);
        assert_eq!(back.lo, want.lo, "the low eight bytes changed");
        assert_eq!(back.hi, want.hi, "the high eight bytes changed");
    }
}
