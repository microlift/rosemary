use core::ffi::c_void;

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{AbiError, Callbacks, GuestMemory, Result};

use super::bad_pointer;
use super::host::host_or_refuse;

/// Byte offset of `invoke` in a `Block_layout`: `isa`, `flags`, `reserved`.
const INVOKE_OFFSET: u64 = 16;
/// Byte offset of `descriptor`, immediately after `invoke`.
const DESCRIPTOR_OFFSET: u64 = 24;
/// Byte offset of `flags`, which says which optional descriptor fields exist.
const FLAGS_OFFSET: u64 = 8;
/// Byte offsets of `copy` and `dispose` in a `Block_descriptor_2`, which
/// follows `Block_descriptor_1`'s `reserved` and `size`.
const COPY_OFFSET: u64 = 16;
const DISPOSE_OFFSET: u64 = 24;
/// `BLOCK_HAS_COPY_DISPOSE`: the descriptor carries the two helper pointers.
const BLOCK_HAS_COPY_DISPOSE: u32 = 1 << 25;

/// Replace every guest code pointer reachable from the block at `block` with a thunk,
/// in place.
pub(crate) fn substitute_block(
    symbol: &str,
    block: u64,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    substitute_pointer(symbol, block.wrapping_add(INVOKE_OFFSET), mem, callbacks)?;

    let flags = read_u32(symbol, mem, block.wrapping_add(FLAGS_OFFSET))?;
    if flags & BLOCK_HAS_COPY_DISPOSE == 0 {
        return Ok(());
    }
    let descriptor = read_u64(symbol, mem, block.wrapping_add(DESCRIPTOR_OFFSET))?;
    if descriptor == 0 {
        return Err(bad_pointer(symbol, block.wrapping_add(DESCRIPTOR_OFFSET)));
    }
    for offset in [COPY_OFFSET, DISPOSE_OFFSET] {
        substitute_pointer(symbol, descriptor.wrapping_add(offset), mem, callbacks)?;
    }
    Ok(())
}

/// Read one guest code pointer, thunk it, and write the thunk back.
fn substitute_pointer(
    symbol: &str,
    at: u64,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let guest = read_u64(symbol, mem, at)?;
    let host = callbacks.substitute(symbol, guest)?;
    if host != guest {
        mem.write(at, &host.to_le_bytes())
            .map_err(|_| bad_pointer(symbol, at))?;
    }
    Ok(())
}

/// The selected runtime's three `_NSConcrete*Block` classes.
fn block_classes() -> &'static [u64] {
    static CLASSES: std::sync::OnceLock<Vec<u64>> = std::sync::OnceLock::new();
    CLASSES.get_or_init(|| {
        let builtin = crate::objc::host::block_data();
        [
            "_NSConcreteStackBlock",
            "_NSConcreteGlobalBlock",
            "_NSConcreteMallocBlock",
        ]
        .iter()
        .filter_map(|name| {
            builtin
                .iter()
                .find(|(n, _)| n.strip_prefix('_') == Some(name))
                .map(|&(_, addr)| addr)
                .or_else(|| crate::host::symbol(name))
        })
        .filter(|&addr| addr != 0)
        .collect()
    })
}

/// Whether `object` is a block literal, judged by its `isa`.
///
/// For an argument typed as a plain `id`: the runtime copies a block it is
/// asked to retain, which runs the block's copy helper, so the helpers must be
/// thunked before the host sees it - see [`substitute_block`].
pub(crate) fn is_block(object: u64, mem: &dyn GuestMemory) -> bool {
    // Objects are aligned; a tagged pointer or a misaligned value is not one,
    // and is not worth the mapping check a read would cost.
    if object == 0 || object % 8 != 0 {
        return false;
    }
    let mut isa = [0u8; 8];
    if mem.read(object, &mut isa).is_err() {
        return false;
    }
    let isa = u64::from_le_bytes(isa);
    isa != 0 && block_classes().contains(&isa)
}

fn read_u64(symbol: &str, mem: &mut dyn GuestMemory, at: u64) -> Result<u64> {
    let mut buf = [0u8; 8];
    mem.read(at, &mut buf)
        .map_err(|_| bad_pointer(symbol, at))?;
    Ok(u64::from_le_bytes(buf))
}

fn read_u32(symbol: &str, mem: &mut dyn GuestMemory, at: u64) -> Result<u32> {
    let mut buf = [0u8; 4];
    mem.read(at, &mut buf)
        .map_err(|_| bad_pointer(symbol, at))?;
    Ok(u32::from_le_bytes(buf))
}

/// Call a host block-taking entry point after rewriting the guest block.
///
/// This deliberately does not hold any rosemary lock while the host is running the
/// block.
fn call_block(symbol: &str, host: u64, args: &[u64]) {
    // SAFETY: all block-taking functions handled here return void
    // and receive only integer/pointer arguments. The host ABI ignores the
    // unused argument registers, and the caller supplies the exact prefix for
    // the selected function.
    unsafe {
        match args {
            [a, b] => core::mem::transmute::<u64, extern "C" fn(u64, u64)>(host)(*a, *b),
            [a, b, c] => {
                core::mem::transmute::<u64, extern "C" fn(u64, u64, u64)>(host)(*a, *b, *c)
            }
            _ => unreachable!("unsupported block dispatch arity for {symbol}"),
        }
    }
}

/// `void dispatch_async(dispatch_queue_t queue, dispatch_block_t block)`
fn dispatch_async(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (queue, block) = (ctx.rdi, ctx.rsi);
    if block == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    substitute_block(symbol, block, mem, callbacks)?;
    let host = host_or_refuse(symbol)?;
    call_block(symbol, host, &[queue, block]);
    Ok(())
}

/// `void dispatch_sync(dispatch_queue_t queue, dispatch_block_t block)`
fn dispatch_sync(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (queue, block) = (ctx.rdi, ctx.rsi);
    if block == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    substitute_block(symbol, block, mem, callbacks)?;
    call_block(symbol, host_or_refuse(symbol)?, &[queue, block]);
    Ok(())
}

/// `void dispatch_after(dispatch_time_t when, dispatch_queue_t queue,
/// dispatch_block_t block)`
fn dispatch_after(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (when, queue, block) = (ctx.rdi, ctx.rsi, ctx.rdx);
    if block == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    substitute_block(symbol, block, mem, callbacks)?;
    call_block(symbol, host_or_refuse(symbol)?, &[when, queue, block]);
    Ok(())
}

/// `void dispatch_once(dispatch_once_t *predicate, dispatch_block_t block)`
fn dispatch_once(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (predicate, block) = (ctx.rdi, ctx.rsi);
    if predicate == 0 || block == 0 {
        return Err(bad_pointer(
            symbol,
            if predicate == 0 { predicate } else { block },
        ));
    }
    substitute_block(symbol, block, mem, callbacks)?;
    call_block(symbol, host_or_refuse(symbol)?, &[predicate, block]);
    Ok(())
}

/// `void *_Block_copy(const void *block)`
fn block_copy(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let block = ctx.rdi;
    // A null block is legal here in a way it is not for `dispatch_async`.
    if block == 0 {
        ctx.rax = 0;
        return Ok(());
    }
    substitute_block(symbol, block, mem, callbacks)?;
    // The selected Objective-C runtime's, not `host_or_refuse`'s: a block
    // copied by one runtime carries that runtime's `_NSConcreteMallocBlock` in
    // its `isa`, and whichever runtime will be asked to retain it later has to
    // be the one that wrote it. `objc::block` is the answer where there is no
    // host libclosure at all.
    let host = crate::objc::host::symbol(symbol).ok_or_else(|| AbiError::NoShim {
        symbol: symbol.to_string(),
    })?;
    // SAFETY: `host` is a `_Block_copy`, whose prototype is one pointer in and
    // one pointer out. The block's code pointers are thunks by the line above,
    // so the copy helper this call runs is native.
    let f: extern "C" fn(u64) -> u64 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64) -> u64>(host) };
    ctx.rax = f(block);
    Ok(())
}

/// `void dispatch_group_notify(dispatch_group_t group, dispatch_queue_t queue,
/// dispatch_block_t block)`
fn dispatch_group_notify(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (group, queue, block) = (ctx.rdi, ctx.rsi, ctx.rdx);
    if block == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    substitute_block(symbol, block, mem, callbacks)?;
    call_block(symbol, host_or_refuse(symbol)?, &[group, queue, block]);
    Ok(())
}

/// `void dispatch_group_async(dispatch_group_t, dispatch_queue_t, dispatch_block_t)`
fn dispatch_group_async(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (group, queue, block) = (ctx.rdi, ctx.rsi, ctx.rdx);
    if block == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    substitute_block(symbol, block, mem, callbacks)?;
    call_block(symbol, host_or_refuse(symbol)?, &[group, queue, block]);
    Ok(())
}

/// `CFRunLoopObserverRef CFRunLoopObserverCreateWithHandler(CFAllocatorRef allocator,
/// CFOptionFlags activities, Boolean repeats, CFIndex order, void
/// (^block)(CFRunLoopObserverRef, CFRunLoopActivity))`
fn run_loop_observer_create_with_handler(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let block = ctx.r8;
    if block == 0 {
        return Err(bad_pointer(symbol, 0));
    }
    substitute_block(symbol, block, mem, callbacks)?;
    let host = host_or_refuse(symbol)?;
    // SAFETY: the symbol [`shim_for`] matched, whose declaration is the one
    // above. `CFOptionFlags` and `CFIndex` are 64-bit and `Boolean` a byte, so
    // every argument is one integer register; the block's code pointers are
    // thunks by the line above.
    let f: extern "C" fn(u64, u64, u8, i64, u64) -> u64 =
        unsafe { core::mem::transmute::<u64, extern "C" fn(u64, u64, u8, i64, u64) -> u64>(host) };
    ctx.rax = f(ctx.rdi, ctx.rsi, ctx.rdx as u8, ctx.rcx as i64, block);
    Ok(())
}

/// `dispatch_data_t dispatch_data_create(const void *buffer, size_t size,
/// dispatch_queue_t queue, dispatch_block_t destructor)`
fn dispatch_data_create(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let destructor = ctx.rcx;
    if destructor != 0
        && !(darwin::libc_backend::using_builtin()
            && destructor == darwin::dispatch::data::destructor_free())
    {
        substitute_block(symbol, destructor, mem, callbacks)?;
    }
    let host = host_or_refuse(symbol)?;
    // SAFETY: the symbol matched the exact four-pointer/integer libdispatch
    // entry point. The data buffer and queue are host-visible identity-mapped
    // values; the destructor has been rewritten to a native thunk above.
    let f: extern "C" fn(*const c_void, u64, u64, u64) -> u64 = unsafe {
        core::mem::transmute::<u64, extern "C" fn(*const c_void, u64, u64, u64) -> u64>(host)
    };
    ctx.rax = f(ctx.rdi as *const c_void, ctx.rsi, ctx.rdx, destructor);
    Ok(())
}

/// `void nw_path_monitor_set_update_handler(nw_path_monitor_t, block)`, its
/// cancel-handler twin, and `void nw_path_enumerate_interfaces(nw_path_t,
/// block)`: an object, then a block that is copied for later or run before
/// the call returns. A nil block is legal for the two setters.
fn object_and_block(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (object, block) = (ctx.rdi, ctx.rsi);
    if block != 0 {
        substitute_block(symbol, block, mem, callbacks)?;
    }
    call_block(symbol, host_or_refuse(symbol)?, &[object, block]);
    Ok(())
}

/// One of this module's implementations, in the shape [`crate::shims`] wants -
/// the wider one, carrying [`Callbacks`].
pub type Shim = crate::signal::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
///
/// Block-taking dispatch entry points must all rewrite the block before handing it to
/// libdispatch.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "dispatch_async" => Some(dispatch_async),
        "dispatch_sync" => Some(dispatch_sync),
        "dispatch_after" => Some(dispatch_after),
        "dispatch_once" => Some(dispatch_once),
        "dispatch_data_create" => Some(dispatch_data_create),
        "dispatch_group_notify" => Some(dispatch_group_notify),
        "dispatch_group_async" => Some(dispatch_group_async),
        "CFRunLoopObserverCreateWithHandler" => Some(run_loop_observer_create_with_handler),
        "nw_path_monitor_set_update_handler"
        | "nw_path_monitor_set_cancel_handler"
        | "nw_path_enumerate_interfaces" => Some(object_and_block),
        // Both spellings.
        "_Block_copy" | "Block_copy" => Some(block_copy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout claim, which is what a wrong answer would violate silently.
    #[test]
    fn the_offsets_are_the_blocks_abi_layout() {
        #[repr(C)]
        struct BlockLayout {
            isa: u64,
            flags: u32,
            reserved: u32,
            invoke: u64,
            descriptor: u64,
        }
        let block = BlockLayout {
            isa: 0,
            flags: 0,
            reserved: 0,
            invoke: 0,
            descriptor: 0,
        };
        let base = &block as *const BlockLayout as u64;
        assert_eq!(&block.flags as *const u32 as u64 - base, FLAGS_OFFSET);
        assert_eq!(&block.invoke as *const u64 as u64 - base, INVOKE_OFFSET);
        assert_eq!(
            &block.descriptor as *const u64 as u64 - base,
            DESCRIPTOR_OFFSET
        );
    }

    /// The same for the descriptor, whose optional half is where the two
    /// helpers live: `reserved` and `size` come first.
    #[test]
    fn the_helper_offsets_follow_reserved_and_size() {
        #[repr(C)]
        struct Descriptor {
            reserved: u64,
            size: u64,
            copy: u64,
            dispose: u64,
        }
        let d = Descriptor {
            reserved: 0,
            size: 0,
            copy: 0,
            dispose: 0,
        };
        let base = &d as *const Descriptor as u64;
        assert_eq!(&d.copy as *const u64 as u64 - base, COPY_OFFSET);
        assert_eq!(&d.dispose as *const u64 as u64 - base, DISPOSE_OFFSET);
    }
}
