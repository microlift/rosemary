use core::ffi::{c_int, c_void};

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{Callbacks, GuestMemory, Result};
use crate::shims::bind;

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

    // Ahead of the handler, so stdio's flush runs after it as Darwin's does.
    darwin::stdio::register_cleanup();
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
pub fn atexit(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let handler = callbacks.substitute(symbol, ctx.rdi)?;
    // Ahead of the handler, so stdio's flush runs after it as Darwin's does.
    darwin::stdio::register_cleanup();
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
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "__cxa_atexit" => Some(bind!(callback "__cxa_atexit" => cxa_atexit)),
        "atexit" => Some(bind!(callback "atexit" => atexit)),
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
