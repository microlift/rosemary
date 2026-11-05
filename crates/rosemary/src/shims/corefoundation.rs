use core::ffi::c_void;

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{Callbacks, GuestMemory, Result};
use crate::shims::bind;

/// `CFRunLoopSourceContext`: ten words, with `perform` last.
const SOURCE_CONTEXT_WORDS: usize = 10;
/// Where `perform` sits in it.
const PERFORM: usize = 9;
/// Where `version` sits, which decides whether this runtime can serve it at all.
const VERSION: usize = 0;

/// `CFRunLoopTimerContext`: five words, and no callback - the timer's is a
/// separate argument in a register, so the ordinary path handles it.
const TIMER_CONTEXT_WORDS: usize = 5;

/// `CFAllocatorContext`: nine words, with `deallocate` the eighth.
const ALLOCATOR_CONTEXT_WORDS: usize = 9;
/// Where `info` sits in it.
const ALLOCATOR_INFO: usize = 1;
/// Where `deallocate` sits in it.
const ALLOCATOR_DEALLOCATE: usize = 7;

/// Read `words` words from `va`.
fn read_words(symbol: &str, va: u64, words: usize, mem: &dyn GuestMemory) -> Result<Vec<u64>> {
    (0..words)
        .map(|i| mem.read_u64(va.wrapping_add(i as u64 * 8), symbol))
        .collect()
}

/// A structure this runtime built for the host to read, kept alive forever.
///
/// Leaked deliberately. `CFRunLoopSourceCreate` copies what it is given, but a
/// caller cannot know that of every entry point that takes a context, and a
/// structure freed while something still holds it is the one failure here that
/// would not be traceable to this file.
fn leak(words: Vec<u64>) -> u64 {
    Box::leak(words.into_boxed_slice()).as_ptr() as usize as u64
}

/// `CFRunLoopSourceRef CFRunLoopSourceCreate(CFAllocatorRef, CFIndex, CFRunLoopSourceContext *)`
pub fn run_loop_source_create(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let context_va = ctx.rdx;
    if context_va == 0 {
        return forward(symbol, ctx, &[ctx.rdi, ctx.rsi, 0]);
    }
    let mut words = read_words(symbol, context_va, SOURCE_CONTEXT_WORDS, &*mem)?;
    // A version-1 source is a mach port. Refused here rather than in
    // CoreFoundation, so the refusal names the call the guest made.
    if words[VERSION] != 0 {
        return Err(crate::abi::AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "a version-1 run loop source is a mach port, which this host has no Mach for",
        });
    }
    words[PERFORM] = callbacks.substitute(symbol, words[PERFORM])?;
    let host_context = leak(words);
    forward(symbol, ctx, &[ctx.rdi, ctx.rsi, host_context])
}

/// `CFRunLoopTimerRef CFRunLoopTimerCreate(CFAllocatorRef, CFAbsoluteTime, CFTimeInterval,
///                                         CFOptionFlags, CFIndex, CFRunLoopTimerCallBack,
///                                         CFRunLoopTimerContext *)`
///
/// The callback is a register argument and the ordinary path would substitute
/// it; the *context* still has to be copied, because its `info` word is the
/// guest's and the retain and release slots in it are guest addresses this
/// runtime must not call.
pub fn run_loop_timer_create(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    // Two doubles in xmm0/xmm1, four integers in rdi/rsi/rdx/rcx, and the
    // context on the stack is not how SysV lays this out: `CFAllocatorRef`,
    // `CFOptionFlags`, `CFIndex`, the callback and the context are the five
    // integer arguments, so they are rdi, rsi, rdx, rcx, r8.
    let (allocator, flags, order) = (ctx.rdi, ctx.rsi, ctx.rdx);
    let callback = callbacks.substitute(symbol, ctx.rcx)?;
    let context_va = ctx.r8;
    let host_context = match context_va {
        0 => 0,
        va => {
            let mut words = read_words(symbol, va, TIMER_CONTEXT_WORDS, &*mem)?;
            // The retain, release and description slots are guest code this
            // runtime is not going to call, and a null there means "no
            // callback" - which is what the caller gets rather than a crash.
            for word in words.iter_mut().skip(2) {
                *word = 0;
            }
            leak(words)
        }
    };
    let fire_date = f64::from_bits(ctx.xmm[0][0]);
    let interval = f64::from_bits(ctx.xmm[1][0]);
    let host = crate::frameworks::symbol(crate::darwin_name(symbol)).ok_or(
        crate::abi::AbiError::Unsatisfied {
            symbol: symbol.to_string(),
        },
    )?;
    // SAFETY: `host` is this tree's `CFRunLoopTimerCreate`, called with the
    // prototype above; `callback` is a trampoline and `host_context` is this
    // runtime's own memory.
    let result = unsafe {
        let f: unsafe extern "C" fn(u64, f64, f64, u64, u64, u64, u64) -> u64 =
            core::mem::transmute(host as usize as *const c_void);
        f(
            allocator,
            fire_date,
            interval,
            flags,
            order,
            callback,
            host_context,
        )
    };
    ctx.rax = result;
    Ok(())
}

/// `CFAllocatorRef CFAllocatorCreate(CFAllocatorRef, CFAllocatorContext *)`
///
/// Only `info` and `deallocate` are read by this tree's CoreFoundation, which
/// copies them; every other callback slot is guest code it never calls, so
/// it is passed as null rather than as an address the host cannot run.
pub fn allocator_create(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let context_va = ctx.rsi;
    if context_va == 0 {
        return forward(symbol, ctx, &[ctx.rdi, 0, 0]);
    }
    let words = read_words(symbol, context_va, ALLOCATOR_CONTEXT_WORDS, &*mem)?;
    let mut host_context = [0u64; ALLOCATOR_CONTEXT_WORDS];
    host_context[ALLOCATOR_INFO] = words[ALLOCATOR_INFO];
    host_context[ALLOCATOR_DEALLOCATE] =
        callbacks.substitute(symbol, words[ALLOCATOR_DEALLOCATE])?;
    let host_context_va = host_context.as_ptr() as usize as u64;
    forward(symbol, ctx, &[ctx.rdi, host_context_va, 0])
}

/// Call this tree's own implementation with the three integer arguments given.
fn forward(symbol: &str, ctx: &mut RegisterContext, args: &[u64; 3]) -> Result<()> {
    let host = crate::frameworks::symbol(crate::darwin_name(symbol)).ok_or(
        crate::abi::AbiError::Unsatisfied {
            symbol: symbol.to_string(),
        },
    )?;
    // SAFETY: `host` is this tree's own entry point for `symbol`, whose first
    // three parameters are integers; the third is a structure in this runtime's
    // memory.
    let result = unsafe {
        let f: unsafe extern "C" fn(u64, u64, u64) -> u64 =
            core::mem::transmute(host as usize as *const c_void);
        f(args[0], args[1], args[2])
    };
    ctx.rax = result;
    Ok(())
}

/// A shim that may also call into the guest.
pub type CallbackShim = super::CallbackShim;

/// Whether `name` (underscore already stripped) is one of this module's.
///
/// Only when this tree's own CoreFoundation is the one in use: with the host's,
/// a guest's context structure is the host's business and a thunk in it would be
/// this runtime interposing on a call it has no reason to.
pub fn shim_for(name: &str) -> Option<CallbackShim> {
    if !crate::frameworks::using_builtin() {
        return None;
    }
    match name {
        "CFAllocatorCreate" => Some(bind!(callback "CFAllocatorCreate" => allocator_create)),
        "CFRunLoopSourceCreate" => {
            Some(bind!(callback "CFRunLoopSourceCreate" => run_loop_source_create))
        }
        "CFRunLoopTimerCreate" => {
            Some(bind!(callback "CFRunLoopTimerCreate" => run_loop_timer_create))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Recording(std::cell::RefCell<Vec<u64>>);

    impl Callbacks for Recording {
        fn substitute(&self, _symbol: &str, va: u64) -> Result<u64> {
            self.0.borrow_mut().push(va);
            Ok(va ^ 0xffff_0000)
        }
    }

    /// Guest memory that is this process's memory.
    struct Identity;

    impl GuestMemory for Identity {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            // SAFETY: the tests only pass the address of a live local array.
            unsafe { core::ptr::copy_nonoverlapping(va as *const u8, out.as_mut_ptr(), out.len()) };
            Ok(())
        }
        fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
            Err(machish::Error::OutOfBounds {
                addr: va,
                len: data.len(),
            })
        }
    }

    #[test]
    fn allocator_create_substitutes_only_the_deallocate_callback() {
        if !crate::frameworks::using_builtin() {
            eprintln!("skipping: the host's CoreFoundation is in use");
            return;
        }
        let mut context = [0x1_0000_0000u64; ALLOCATOR_CONTEXT_WORDS];
        context[ALLOCATOR_INFO] = 0x1234;
        context[ALLOCATOR_DEALLOCATE] = 0x1_0000_5678;
        let callbacks = Recording(std::cell::RefCell::new(Vec::new()));
        let mut ctx = RegisterContext::new();
        ctx.rsi = context.as_ptr() as u64;

        allocator_create("_CFAllocatorCreate", &mut ctx, &mut Identity, &callbacks)
            .expect("create");

        assert_eq!(*callbacks.0.borrow(), vec![0x1_0000_5678]);
        assert_ne!(ctx.rax, 0);
        // SAFETY: the allocator this test created.
        unsafe { corefoundation::api::CFRelease(ctx.rax as usize as *const c_void) };
    }
}
