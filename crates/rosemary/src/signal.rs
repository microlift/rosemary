use microlift::arch::x86_64::context::RegisterContext;
use static_init::dynamic;

use crate::abi::{AbiError, Callbacks, GuestMemory, Result};
use crate::shims::bind;

/// `sa_flags` bit asking for the three-argument handler. See the module note.
const SA_SIGINFO: u32 = 0x0040;

/// The dispositions that are not addresses.
fn is_sentinel(value: u64) -> bool {
    value == 0 || value == 1 || value as i64 == -1
}

/// `struct sigaction` as both of *Darwin's* architectures lay it out.
///
/// Not the host's: under glibc a `struct sigaction` is 152 bytes with a 128-byte
/// `sigset_t` in the middle, so these sixteen are a value to convert rather than
/// a structure to hand along. [`darwin::signal::sigaction`] does the converting,
/// on both hosts, so there is one path and it is tested on the one where the
/// answers can be checked.
#[derive(Clone, Copy, Default)]
struct SigAction {
    handler: u64,
    mask: u32,
    flags: u32,
}

impl From<SigAction> for darwin::signal::Disposition {
    fn from(a: SigAction) -> Self {
        darwin::signal::Disposition {
            handler: a.handler,
            mask: a.mask,
            flags: a.flags,
        }
    }
}

impl From<darwin::signal::Disposition> for SigAction {
    fn from(d: darwin::signal::Disposition) -> Self {
        SigAction {
            handler: d.handler,
            mask: d.mask,
            flags: d.flags,
        }
    }
}

impl SigAction {
    const SIZE: usize = 16;

    fn from_bytes(b: [u8; Self::SIZE]) -> Self {
        SigAction {
            handler: u64::from_le_bytes(b[0..8].try_into().unwrap_or_default()),
            mask: u32::from_le_bytes(b[8..12].try_into().unwrap_or_default()),
            flags: u32::from_le_bytes(b[12..16].try_into().unwrap_or_default()),
        }
    }

    fn to_bytes(self) -> [u8; Self::SIZE] {
        let mut out = [0u8; Self::SIZE];
        out[0..8].copy_from_slice(&self.handler.to_le_bytes());
        out[8..12].copy_from_slice(&self.mask.to_le_bytes());
        out[12..16].copy_from_slice(&self.flags.to_le_bytes());
        out
    }
}

/// The `SA_SIGINFO` dispositions this runtime accepted and did not install.
///
/// Keyed by signal number, holding the guest's *own* struct - the handler field
/// as the guest wrote it, never a thunk - because the only thing this map is for
/// is answering `oldact` with what the guest last said. See the module note.
fn recorded() -> &'static std::sync::Mutex<std::collections::HashMap<i32, SigAction>> {
    #[dynamic]
    static MAP: std::sync::Mutex<std::collections::HashMap<i32, SigAction>> = Default::default();
    &MAP
}

/// Say once, per signal, that a disposition was taken and will not be delivered.
fn say_undeliverable(signum: i32) {
    static SAID: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.contains(&signum) {
        return;
    }
    said.push(signum);
    eprintln!(
        "rosemary: sigaction(signal {signum}): SA_SIGINFO handler recorded but not installed \
         - a ucontext_t would need the guest's registers at an interrupted PC, which the \
         recompiler cannot report; the signal keeps its previous disposition"
    );
}

/// A guest handler on its way *out*: a thunk, unless it is a sentinel.
fn outbound(symbol: &str, handler: u64, callbacks: &dyn Callbacks) -> Result<u64> {
    if is_sentinel(handler) {
        return Ok(handler);
    }
    callbacks.substitute(symbol, handler)
}

/// A handler on its way *back in*: the guest address behind a thunk, if it is
/// one of ours.
///
/// A disposition the guest never installed - one this process set up before the
/// guest ran - is returned unchanged, because it is what the kernel will really
/// call and the guest can only ever pass it back.
fn inbound(handler: u64, callbacks: &dyn Callbacks) -> u64 {
    if is_sentinel(handler) {
        return handler;
    }
    callbacks.unsubstitute(handler)
}

/// `void (*signal(int sig, void (*handler)(int)))(int)`
pub fn signal(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let signum = ctx.rdi as u32 as i32;
    let handler = outbound(symbol, ctx.rsi, callbacks)?;

    // SAFETY: `handler` is a function pointer the host will call as arm64 -
    // which is what `outbound` just guaranteed it is, either a thunk this
    // runtime compiled or one of the kernel's own sentinels. The signal
    // *number* is Darwin's, which is why this does not go straight to `libc`.
    let previous = unsafe { darwin::signal::signal(signum, handler) };
    ctx.rax = inbound(previous, callbacks);
    Ok(())
}

/// `int sigaction(int sig, const struct sigaction *act, struct sigaction *oldact)`
pub fn sigaction(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let signum = ctx.rdi as u32 as i32;
    let (act_va, old_va) = (ctx.rsi, ctx.rdx);

    // The guest's own struct is never rewritten.
    let mut act = None;
    if act_va != 0 {
        let mut bytes = [0u8; SigAction::SIZE];
        mem.read(act_va, &mut bytes).map_err(|_| AbiError::Memory {
            symbol: symbol.to_string(),
            addr: act_va,
        })?;
        let mut a = SigAction::from_bytes(bytes);
        if a.flags & SA_SIGINFO != 0 && !is_sentinel(a.handler) {
            // Accepted, recorded, and never installed. See the module note: the
            // installation is honest, the delivery would not be.
            say_undeliverable(signum);
            let previous = recorded()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(signum, a);
            if old_va != 0 {
                let old = previous.unwrap_or_default();
                mem.write(old_va, &old.to_bytes())
                    .map_err(|_| AbiError::Memory {
                        symbol: symbol.to_string(),
                        addr: old_va,
                    })?;
            }
            crate::shims::set_int_return(ctx, 0);
            return Ok(());
        }
        a.handler = outbound(symbol, a.handler, callbacks)?;
        act = Some(a);
    }

    // A query - or an ordinary installation - for a signal whose last
    // disposition this runtime took rather than installed.
    if old_va != 0
        && let Some(previous) = recorded()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&signum)
    {
        mem.write(old_va, &previous.to_bytes())
            .map_err(|_| AbiError::Memory {
                symbol: symbol.to_string(),
                addr: old_va,
            })?;
        let rc = match act {
            None => 0,
            // SAFETY: as the call below - `a.handler` is a sentinel or a
            // trampoline this runtime compiled.
            Some(a) => unsafe { darwin::signal::sigaction(signum, Some(a.into()), None) },
        };
        crate::shims::set_int_return(ctx, rc);
        return Ok(());
    }

    let mut old = darwin::signal::Disposition::default();
    // SAFETY: `act`'s handler is a sentinel or a trampoline this runtime
    // compiled and keeps mapped. Everything about the host's own structure -
    // its size, its field order, its flag values, the width of its mask - is
    // `darwin::signal`'s business and not this shim's.
    let rc = unsafe {
        darwin::signal::sigaction(
            signum,
            act.map(Into::into),
            (old_va != 0).then_some(&mut old),
        )
    };

    if rc == 0 && old_va != 0 {
        let mut old = SigAction::from(old);
        old.handler = inbound(old.handler, callbacks);
        mem.write(old_va, &old.to_bytes())
            .map_err(|_| AbiError::Memory {
                symbol: symbol.to_string(),
                addr: old_va,
            })?;
    }
    crate::shims::set_int_return(ctx, rc);
    Ok(())
}

/// `int sigaltstack(const stack_t *ss, stack_t *oss)`
///
/// `oss` is answered with `SS_DISABLE` and a null stack, which is what the guest would
/// see if it had never asked - true, and the only self-consistent answer to give a
/// caller that is about to be told its request succeeded.
pub fn sigaltstack(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    _callbacks: &dyn Callbacks,
) -> Result<()> {
    /// `stack_t`: `{ void *ss_sp; size_t ss_size; int ss_flags; }` - 24 bytes
    /// with the same layout on x86-64 and arm64 macOS.
    const STACK_T_SIZE: usize = 24;
    /// `SS_DISABLE`.
    const SS_DISABLE: u32 = 0x0004;

    let old_va = ctx.rsi;
    if old_va != 0 {
        let mut bytes = [0u8; STACK_T_SIZE];
        bytes[16..20].copy_from_slice(&SS_DISABLE.to_le_bytes());
        mem.write(old_va, &bytes).map_err(|_| AbiError::Memory {
            symbol: symbol.to_string(),
            addr: old_va,
        })?;
    }
    crate::shims::set_int_return(ctx, 0);
    Ok(())
}

/// `kern_return_t task_set_exception_ports(task, mask, port, behavior, flavor)`
pub fn task_set_exception_ports(
    _symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    _callbacks: &dyn Callbacks,
) -> Result<()> {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rosemary: task_set_exception_ports(mask {:#x}) accepted but not honoured - a Mach \
             exception message carries this CPU's thread state for a fault inside a translation, \
             not the guest's registers",
            ctx.rsi as u32
        );
    }
    // KERN_SUCCESS. The guest asked to be told about faults and will not be.
    ctx.rax = 0;
    Ok(())
}

/// Say the exception-port sentence once, whichever entry point reached it.
fn announce_exception_ports(symbol: &str, mask: u32) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rosemary: {symbol}(mask {mask:#x}) accepted but not honoured - a Mach exception \
             message carries this CPU's thread state for a fault inside a translation, not the \
             guest's registers",
        );
    }
}

/// The `..._get_exception_ports` half of the family: `task`, `host`.
fn get_exception_ports(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    _callbacks: &dyn Callbacks,
) -> Result<()> {
    // `masksCnt` is the fourth parameter.
    write_port_count(symbol, ctx.rcx, mem)?;
    crate::shims::set_int_return(ctx, 0);
    Ok(())
}

/// The `..._swap_exception_ports` half: `task`, `host`, `thread`.
///
/// **Ten parameters**, which puts `masksCnt` - the seventh - past SysV AMD64's six
/// integer registers and onto the caller's stack.
fn swap_exception_ports(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    _callbacks: &dyn Callbacks,
) -> Result<()> {
    announce_exception_ports(symbol, ctx.rsi as u32);
    let masks_count = mem.read_u64(ctx.rsp, symbol)?;
    write_port_count(symbol, masks_count, mem)?;
    crate::shims::set_int_return(ctx, 0);
    Ok(())
}

/// `host_set_exception_ports`, which is [`task_set_exception_ports`] with a
/// `host_priv_t` in front - and which a program without host privileges could
/// not have used anyway.
fn set_exception_ports(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    _callbacks: &dyn Callbacks,
) -> Result<()> {
    announce_exception_ports(symbol, ctx.rsi as u32);
    crate::shims::set_int_return(ctx, 0);
    Ok(())
}

/// Write "no exception ports" through a `mach_msg_type_number_t *`.
fn write_port_count(symbol: &str, at: u64, mem: &mut dyn GuestMemory) -> Result<()> {
    if at == 0 {
        return Ok(());
    }
    // `mach_msg_type_number_t` is `natural_t`, a 32-bit unsigned integer on
    // both architectures.
    mem.write(at, &0u32.to_le_bytes())
        .map_err(|_| AbiError::Memory {
            symbol: symbol.to_string(),
            addr: at,
        })
}

/// One of this module's implementations, in the shape [`crate::shims`] wants.
pub type Shim = crate::shims::CallbackShim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    Some(match name {
        "signal" => bind!(callback "signal" => signal),
        "sigaction" => bind!(callback "sigaction" => sigaction),
        "sigaltstack" => bind!(callback "sigaltstack" => sigaltstack),
        "task_set_exception_ports" => {
            bind!(callback "task_set_exception_ports" => task_set_exception_ports)
        }
        "host_set_exception_ports" => {
            bind!(callback "host_set_exception_ports" => set_exception_ports)
        }
        "task_get_exception_ports" => {
            bind!(callback "task_get_exception_ports" => get_exception_ports)
        }
        "host_get_exception_ports" => {
            bind!(callback "host_get_exception_ports" => get_exception_ports)
        }
        "task_swap_exception_ports" => {
            bind!(callback "task_swap_exception_ports" => swap_exception_ports)
        }
        "host_swap_exception_ports" => {
            bind!(callback "host_swap_exception_ports" => swap_exception_ports)
        }
        "thread_swap_exception_ports" => {
            bind!(callback "thread_swap_exception_ports" => swap_exception_ports)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what crossed, and pretends a thunk is the guest address with its
    /// top bit set - enough to tell the two directions apart in an assertion.
    struct Wrapping;

    const WRAP: u64 = 0x8000_0000_0000_0000;

    impl Callbacks for Wrapping {
        fn substitute(&self, _symbol: &str, va: u64) -> Result<u64> {
            Ok(va | WRAP)
        }

        fn unsubstitute(&self, va: u64) -> u64 {
            va & !WRAP
        }
    }

    struct Bytes {
        base: u64,
        bytes: Vec<u8>,
    }

    impl GuestMemory for Bytes {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            let off = (va.wrapping_sub(self.base)) as usize;
            let end = off.checked_add(out.len());
            match end.filter(|e| *e <= self.bytes.len()) {
                Some(e) => {
                    out.copy_from_slice(&self.bytes[off..e]);
                    Ok(())
                }
                None => Err(machish::Error::OutOfBounds {
                    addr: va,
                    len: out.len(),
                }),
            }
        }

        fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
            let off = (va.wrapping_sub(self.base)) as usize;
            let end = off.checked_add(data.len());
            match end.filter(|e| *e <= self.bytes.len()) {
                Some(e) => {
                    self.bytes[off..e].copy_from_slice(data);
                    Ok(())
                }
                None => Err(machish::Error::OutOfBounds {
                    addr: va,
                    len: data.len(),
                }),
            }
        }
    }

    fn guest_memory() -> Bytes {
        Bytes {
            base: 0x3_0000_0000,
            bytes: vec![0; 64],
        }
    }

    #[test]
    fn the_struct_is_sixteen_bytes_of_the_shape_both_architectures_use() {
        assert_eq!(std::mem::size_of::<SigAction>(), SigAction::SIZE);
        // The guest's `struct sigaction` is Darwin's, so this is a cross-check against
        // the host's declaration only where the host is a Mac. Under glibc and musl
        // the host's is 152 bytes of a different shape, which is why this shim
        // converts through [`darwin::signal::sigaction`] rather than passing the
        // guest's sixteen bytes along. `darwin::divergent` refuses the name too,
        // but that is not the thing standing between the guest and the host: a
        // shim runs before the divergence check, unconditionally.
        #[cfg(target_os = "macos")]
        assert_eq!(
            std::mem::size_of::<SigAction>(),
            std::mem::size_of::<libc::sigaction>()
        );
        // Round-tripping is what the shim relies on when it rewrites one field
        // and leaves the mask and flags exactly as the guest wrote them.
        let a = SigAction {
            handler: 0x1_0000_1234,
            mask: 0xdead_beef,
            flags: 0x1234_5678,
        };
        let b = SigAction::from_bytes(a.to_bytes());
        assert_eq!((b.handler, b.mask, b.flags), (a.handler, a.mask, a.flags));
    }

    #[test]
    fn the_kernels_sentinels_are_never_wrapped_in_a_thunk() {
        // `SIG_DFL`, `SIG_IGN` and `SIG_ERR` are dispositions, not addresses.
        for sentinel in [0u64, 1, u64::MAX] {
            assert_eq!(outbound("_signal", sentinel, &Wrapping).unwrap(), sentinel);
            assert_eq!(inbound(sentinel, &Wrapping), sentinel);
        }
        assert_eq!(
            outbound("_signal", 0x1_0000_1234, &Wrapping).unwrap(),
            0x1_0000_1234 | WRAP
        );
    }

    #[test]
    fn a_handler_crosses_out_as_a_thunk_and_back_as_the_guests_own_pointer() {
        // The direction that is easy to forget.
        let mut mem = guest_memory();
        let act = SigAction {
            handler: 0x1_0000_1234,
            mask: 0,
            flags: 0,
        };
        mem.write(mem.base, &act.to_bytes()).unwrap();

        let mut ctx = RegisterContext::new();
        ctx.rdi = darwin::signal::signum::SIGUSR1 as u64;
        ctx.rsi = mem.base;
        ctx.rdx = mem.base + 16;
        sigaction("_sigaction", &mut ctx, &mut mem, &Wrapping).expect("install");
        assert_eq!(ctx.rax as i32, 0);

        // Install again, asking for the previous disposition: what comes back
        // must be the guest's own address.
        let mut ctx = RegisterContext::new();
        ctx.rdi = darwin::signal::signum::SIGUSR1 as u64;
        ctx.rsi = 0;
        ctx.rdx = mem.base + 16;
        sigaction("_sigaction", &mut ctx, &mut mem, &Wrapping).expect("query");

        let mut back = [0u8; SigAction::SIZE];
        mem.read(mem.base + 16, &mut back).unwrap();
        assert_eq!(SigAction::from_bytes(back).handler, 0x1_0000_1234);

        // Leave the process as it was found.
        // SAFETY: restoring the default disposition for the signal this test
        // installed a handler for.
        unsafe { darwin::signal::signal(darwin::signal::signum::SIGUSR1, 0) };
    }

    #[test]
    fn sa_siginfo_is_recorded_and_never_installed() {
        let mut mem = guest_memory();
        let act = SigAction {
            handler: 0x1_0000_1234,
            mask: 0,
            flags: SA_SIGINFO,
        };
        mem.write(mem.base, &act.to_bytes()).unwrap();

        let mut ctx = RegisterContext::new();
        ctx.rdi = darwin::signal::signum::SIGUSR2 as u64;
        ctx.rsi = mem.base;
        sigaction("_sigaction", &mut ctx, &mut mem, &Wrapping).expect("accepted");
        assert_eq!(ctx.rax as i32, 0, "the guest must be told it succeeded");

        // Accepted and *not installed*: the host's disposition for that signal
        // is untouched, which is the whole claim. Asked through `darwin`, whose
        // job it is to know that the host's structure is not this one.
        let mut current = darwin::signal::Disposition::default();
        // SAFETY: a query, which installs nothing.
        unsafe {
            darwin::signal::sigaction(darwin::signal::signum::SIGUSR2, None, Some(&mut current))
        };
        assert_eq!(current.handler, 0, "a disposition was installed anyway");
    }

    #[test]
    fn a_recorded_sa_siginfo_disposition_comes_back_through_oldact() {
        // A program that saves a disposition and restores it later must see what
        // it wrote, not what the host would report - which is the disposition
        // from before the guest ever spoke.
        let mut mem = guest_memory();
        let first = SigAction {
            handler: 0x1_0000_1234,
            mask: 0,
            flags: SA_SIGINFO,
        };
        mem.write(mem.base, &first.to_bytes()).unwrap();

        let mut ctx = RegisterContext::new();
        ctx.rdi = darwin::signal::signum::SIGUSR2 as u64;
        ctx.rsi = mem.base;
        sigaction("_sigaction", &mut ctx, &mut mem, &Wrapping).expect("first");

        let second = SigAction {
            handler: 0x1_0000_5678,
            mask: 0,
            flags: SA_SIGINFO,
        };
        mem.write(mem.base, &second.to_bytes()).unwrap();
        let mut ctx = RegisterContext::new();
        ctx.rdi = darwin::signal::signum::SIGUSR2 as u64;
        ctx.rsi = mem.base;
        ctx.rdx = mem.base + 16;
        sigaction("_sigaction", &mut ctx, &mut mem, &Wrapping).expect("second");

        let mut back = [0u8; SigAction::SIZE];
        mem.read(mem.base + 16, &mut back).unwrap();
        let back = SigAction::from_bytes(back);
        assert_eq!(
            back.handler, 0x1_0000_1234,
            "the guest's own address, unwrapped"
        );
        assert_eq!(back.flags, SA_SIGINFO);
    }

    #[test]
    fn sigaltstack_succeeds_and_reports_the_stack_disabled() {
        // Accepted so a runtime that installs one can start; never forwarded,
        // because the alternate stack belongs to the *host* thread.
        let mut mem = guest_memory();
        let mut ctx = RegisterContext::new();
        ctx.rdi = 0;
        ctx.rdx = 0;
        ctx.rsi = mem.base + 16;
        sigaltstack("_sigaltstack", &mut ctx, &mut mem, &Wrapping).expect("accepted");
        assert_eq!(ctx.rax as i32, 0);

        let mut back = [0u8; 24];
        mem.read(mem.base + 16, &mut back).unwrap();
        assert_eq!(u64::from_le_bytes(back[0..8].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(back[16..20].try_into().unwrap()), 0x0004);
    }

    #[test]
    fn a_handler_pointer_outside_guest_memory_fails_by_address() {
        let mut mem = guest_memory();
        let mut ctx = RegisterContext::new();
        ctx.rdi = darwin::signal::signum::SIGUSR1 as u64;
        ctx.rsi = 0x9_0000_0000;
        let err = sigaction("_sigaction", &mut ctx, &mut mem, &Wrapping).expect_err("fail");
        assert_eq!(
            err,
            AbiError::Memory {
                symbol: "_sigaction".into(),
                addr: 0x9_0000_0000
            }
        );
    }
}
