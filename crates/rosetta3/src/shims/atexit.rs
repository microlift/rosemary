use core::ffi::{c_int, c_void};

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{Callbacks, GuestMemory, Result};

unsafe extern "C" {
    /// libsystem's own, spelled `___cxa_atexit` in the Mach-O.
    fn __cxa_atexit(func: *const (), arg: *mut c_void, dso: *const ()) -> c_int;
}

/// The DSO handle every guest destructor is registered against.
static DSO_HANDLE: u8 = 0;

/// `int __cxa_atexit(void (*func)(void *), void *arg, void *dso_handle)`
pub fn cxa_atexit(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    // `arg` is passed through untouched.
    let handler = callbacks.substitute(symbol, ctx.rdi)?;
    let arg = ctx.rsi as *mut c_void;

    // SAFETY: `handler` is either null or an arm64 trampoline this runtime
    // compiled and keeps mapped for the VM's lifetime - which is what
    // `substitute` guarantees and what the published VM makes true past `main`.
    // The DSO handle is our own static's address, so the host is never handed a
    // guest one.
    let rc = unsafe {
        __cxa_atexit(
            handler as *const (),
            arg,
            &raw const DSO_HANDLE as *const (),
        )
    };
    super::set_int_return(ctx, rc);
    Ok(())
}

/// `int atexit(void (*func)(void))`, which is `__cxa_atexit` with no argument.
///
/// A shim rather than a passthrough for two reasons, and either would be enough:
/// the handler is a *guest* address that has to become a trampoline, and glibc
/// does not export `atexit` dynamically at all - it lives in `libc_nonshared.a`,
/// so there is nothing for a `dlsym` to find. See `darwin::passthrough`.
///
/// The handler is registered through the three-argument form with a null
/// argument. A function compiled to take none ignores the register it arrives
/// in, under both ABIs in play here.
///
/// # Which of those two reasons is true depends on the backend, and this module
/// is only reached on one of them
///
/// The first reason - that the handler must become a trampoline - is an
/// **emulated**-backend fact. Under emulation the guest's function pointers are
/// not the host's, so [`crate::abi::Callbacks::substitute`] has to compile one;
/// under **native** execution the guest *is* host code, a guest function pointer
/// is already a callable host address, and the substitution would be the
/// identity.
///
/// The second reason is true on both, and is the one that cannot be fixed by
/// publication either way: `atexit` is in `libc_nonshared.a`, so a
/// `SyntheticDylib` that defined the name would be shadowed by nothing but
/// would also be the *only* thing serving it, and `dlsym` will never find it
/// however the runtime is wired. `__cxa_atexit` is the opposite case and *is*
/// exported, so it is publishable.
///
/// ## The whole module is unreachable on native
///
/// [`crate::shims::dispatch`] is called from exactly one place,
/// `abi::call_import_inner`, which is reached only from the `Vm` run loop. On
/// the native backend `exec::native::Native::load` binds every import straight to
/// a host address and calls `crate::trampoline::disarm()`, so no guest call ever
/// arrives at `dispatch` - `crates/rosetta3/src/fault.rs`'s `say_thread` says so
/// at the same site and for the same reason. So on native the guest's
/// `___cxa_atexit` reaches glibc's own copy with the guest's `dso_handle`.
///
/// Measured, and the measurement is *not* a misbehaviour report: the guest
/// imports `___cxa_atexit` from 14 of 14 Mach-Os in the bundle (251 call sites
/// in the Framework) and `__cxa_finalize` from **none**, so nothing on this
/// guest ever compares a DSO handle against the one glibc stored. That is why
/// the asymmetry is worth recording rather than fixing blind: the substitution
/// this module performs is inert for this program, and a native publication that
/// "fixed" it would be changing code that was not wrong. What it would *not* do
/// is make anything more correct, which is the honest summary.
pub fn atexit(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let handler = callbacks.substitute(symbol, ctx.rdi)?;
    // SAFETY: as `cxa_atexit`, whose obligations these are.
    let rc = unsafe {
        __cxa_atexit(
            handler as *const (),
            core::ptr::null_mut(),
            &raw const DSO_HANDLE as *const (),
        )
    };
    super::set_int_return(ctx, rc);
    Ok(())
}

/// One of this module's implementations, in the shape [`crate::shims`] wants.
pub type Shim = crate::signal::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
///
/// Mach-O spellings, because that is what [`crate::shims`] reduces to: the
/// guest's import is `___cxa_atexit` and `_atexit`, and one underscore comes off
/// before this is asked. A row written `__cxa_atexit` here would match nothing,
/// and nothing would say so - see the note on [`atexit`] about which backend
/// reaches this at all.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "__cxa_atexit" => Some(cxa_atexit),
        "atexit" => Some(atexit),
        // `__cxa_thread_atexit` is *not* here.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handler that is really callable, because this test's registrations stay in the
    /// *test process's* exit list and run for real when it exits.
    extern "C" fn harmless(_arg: *mut c_void) {}

    /// Answers with a real host function's address, and records what it was
    /// asked to substitute.
    struct Recording(std::cell::Cell<u64>);

    impl Callbacks for Recording {
        fn substitute(&self, _symbol: &str, va: u64) -> Result<u64> {
            self.0.set(va);
            Ok(harmless as *const () as u64)
        }
    }

    struct NoMemory;

    impl GuestMemory for NoMemory {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            Err(machish::Error::OutOfBounds {
                addr: va,
                len: out.len(),
            })
        }

        fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
            Err(machish::Error::OutOfBounds {
                addr: va,
                len: data.len(),
            })
        }
    }

    #[test]
    fn the_handler_crosses_out_as_a_thunk_and_the_host_accepts_it() {
        let callbacks = Recording(std::cell::Cell::new(0));
        let mut ctx = RegisterContext::new();
        ctx.rdi = 0x1_0000_1234; // the guest's destructor
        ctx.rsi = 0x2_0000_5678; // the object it destroys
        ctx.rdx = 0x1_0000_0000; // the guest's `___dso_handle`

        cxa_atexit("___cxa_atexit", &mut ctx, &mut NoMemory, &callbacks).expect("register");

        // What crossed out is the guest's own function pointer, and what the
        // host got back is a success.
        assert_eq!(callbacks.0.get(), 0x1_0000_1234);
        assert_eq!(ctx.rax as i32, 0);
    }

    #[test]
    fn the_handle_registered_belongs_to_an_image_the_host_knows() {
        // The property that makes substituting the guest's handle worth doing.
        let mut info = unsafe { std::mem::zeroed::<libc::Dl_info>() };
        // SAFETY: a live static's address and a live local `Dl_info`.
        let found = unsafe { libc::dladdr((&raw const DSO_HANDLE).cast(), &raw mut info) };
        assert_ne!(
            found, 0,
            "the runtime's own DSO handle is not in any loaded image"
        );
    }
}
