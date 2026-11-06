use microlift::arch::x86_64::context::RegisterContext;

use crate::shims::bind;
use crate::{
    abi::{GuestMemory, Result},
    shims::host::builtin_or_host,
};

/// `CFRange`: two `CFIndex`, which both ABIs return in a pair of integer
/// registers - see `crates/corefoundation/src/api.rs`'s `CFStringFind` for why
/// that is what makes this a shim rather than a `table.rs` entry.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Range {
    location: i64,
    length: i64,
}

/// `CFRange CFStringFind(CFStringRef, CFStringRef, CFStringCompareFlags)`
fn find(symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let host = builtin_or_host(symbol)?;
    // SAFETY: the symbol [`shim_for`] matched, which is `CFRange (*)(CFStringRef,
    // CFStringRef, CFOptionFlags)`, resolved out of this tree's own
    // CoreFoundation (or, on a macOS host set to use the real one, Apple's).
    let f: extern "C" fn(u64, u64, u64) -> Range =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, u64, u64) -> Range>(host) };
    let range = f(ctx.rdi, ctx.rsi, ctx.rdx);
    ctx.rax = range.location as u64;
    ctx.rdx = range.length as u64;
    Ok(())
}

/// `CFRange CFStringGetRangeOfComposedCharactersAtIndex(CFStringRef, CFIndex)`
fn range_of_composed_characters(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let host = builtin_or_host(symbol)?;
    // SAFETY: as [`find`], for `CFRange (*)(CFStringRef, CFIndex)`.
    let f: extern "C" fn(u64, u64) -> Range =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, u64) -> Range>(host) };
    let range = f(ctx.rdi, ctx.rsi);
    ctx.rax = range.location as u64;
    ctx.rdx = range.length as u64;
    Ok(())
}

pub type Shim = crate::shims::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "CFStringFind" => Some(bind!("CFStringFind" => find)),
        "CFStringGetRangeOfComposedCharactersAtIndex" => Some(
            bind!("CFStringGetRangeOfComposedCharactersAtIndex" => range_of_composed_characters),
        ),
        _ => None,
    }
}
