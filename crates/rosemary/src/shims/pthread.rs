use microlift::arch::x86_64::context::RegisterContext;
use static_init::dynamic;

use crate::abi::{Callbacks, GuestMemory, Result};
use crate::thread::{GuestStart, StackRequest};

use super::set_int_return;
use crate::shims::bind;

/// `EINVAL`, the one failure this shim produces without asking the host.
const EINVAL: i32 = 22;

/// The stack the *host* thread gets: the runtime's own frames, and the JIT's.
///
/// The main thread's size, and deliberately not the guest's request.
const HOST_THREAD_STACK: usize = crate::stack::DEFAULT_STACK;

/// `int pthread_create(pthread_t *thread, const pthread_attr_t *attr,
/// void *(*start_routine)(void *), void *arg)`
pub fn pthread_create(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (out, attr, start, arg) = (ctx.rdi, ctx.rsi, ctx.rdx, ctx.rcx);
    if out == 0 {
        set_int_return(ctx, EINVAL);
        return Ok(());
    }

    // The guest's attributes, read before anything is created. Read rather than
    // byte-copied: builtin mode owns the guest layout, so what crosses is what
    // the set says. See
    // [`darwin::pthread::Attributes`].
    //
    // SAFETY: `attr` is null or the guest's own `pthread_attr_t`, which is
    // identity-mapped and so directly addressable.
    let Some(attributes) = (unsafe { darwin::pthread::Attributes::of_guest(attr) }) else {
        set_int_return(ctx, EINVAL);
        return Ok(());
    };
    // Whatever the guest asked for describes the *guest's* stack; the host thread that
    // runs its code gets this runtime's own, which is what `to_host_attr` is told.
    let stack = stack_request(&attributes);
    let mut host_attr = match attributes.to_host_attr(HOST_THREAD_STACK) {
        Ok(attr) => attr,
        Err(rc) => {
            set_int_return(ctx, rc);
            return Ok(());
        }
    };

    // The start routine is guest code and crosses out as a thunk, exactly as
    // `qsort`'s comparator does.
    let entry = callbacks.substitute(symbol, start)?;
    if entry == 0 {
        set_int_return(ctx, EINVAL);
        return Ok(());
    }

    // SAFETY: `host_attr` is initialised and outlives the call, which is all
    // `pthread_create` needs - it copies what it wants out of an attribute set.
    let result = crate::thread::spawn(GuestStart::new(entry, arg, stack), unsafe {
        &*host_attr.as_mut_ptr()
    });

    match result {
        Ok(thread) => {
            mem.write_u64(
                out,
                darwin::pthread::handle::for_host(thread as u64),
                symbol,
            )?;
            set_int_return(ctx, 0);
        }
        Err(rc) => set_int_return(ctx, rc),
    }
    Ok(())
}

/// What the guest's attributes say about the stack its *own* frames go on.
fn stack_request(attributes: &darwin::pthread::Attributes) -> StackRequest {
    match (attributes.stack_top, attributes.stack_size) {
        // A supplied stack of no size is not a stack; fall back rather than hand a
        // thread a zero-byte range to run on.
        (Some(top), Some(size)) if top != 0 && size != 0 => StackRequest::Supplied {
            top,
            size: size as u64,
        },
        (_, None) => StackRequest::Default,
        (_, Some(size)) => StackRequest::Sized(size),
    }
}

/// `int pthread_once(pthread_once_t *, void (*)(void))`.
///
/// A shim for two reasons at once. The routine is **guest code**, so reaching it needs
/// a thunk; and a `pthread_once_t` is sixteen bytes on Darwin and four under musl, so
/// the control word cannot be handed to the host at all. See [`darwin::pthread::once`],
/// which owns the once-and-only-once and the waiting.
///
/// Only registered in builtin libc mode. In macOS native mode the
/// host's `pthread_once` reads the same sixteen bytes the guest wrote, and the generic
/// marshaller already gives it a thunk - see the `cbsig!` entry in [`crate::abi::table`].
fn pthread_once(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (control, routine) = (ctx.rdi, ctx.rsi);
    if routine == 0 {
        set_int_return(ctx, EINVAL);
        return Ok(());
    }
    let entry = callbacks.substitute(symbol, routine)?;
    // SAFETY: `entry` is a thunk this runtime handed out for a guest function of no
    // arguments and no result, which is what `pthread_once` promises its routine is.
    let call: extern "C" fn() = unsafe { core::mem::transmute(entry as usize as *const ()) };
    // SAFETY: `control` is the guest's own `pthread_once_t`, identity-mapped and so
    // directly addressable, and eight-byte aligned as its leading `long` makes it.
    let rc = unsafe { darwin::pthread::once(control, || call()) };
    set_int_return(ctx, rc);
    Ok(())
}

/// `int pthread_key_create(pthread_key_t *, void (*)(void *))`.
///
/// A `pthread_key_t` is an `unsigned long` on Darwin and an `unsigned` under musl, so
/// letting the host write through the guest's pointer fills four of the eight bytes
/// the guest will read and leaves the other four holding whatever was there. The key
/// is written out as a full eight bytes here instead.
fn pthread_key_create(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (out, destructor) = (ctx.rdi, ctx.rsi);
    if out == 0 {
        set_int_return(ctx, EINVAL);
        return Ok(());
    }
    // A null destructor is the common case and stays null; anything else is guest
    // code and crosses out as a thunk.
    let entry = match destructor {
        0 => 0,
        va => callbacks.substitute(symbol, va)?,
    };
    // SAFETY: `entry` is either null or a thunk this runtime handed out for a guest
    // function taking one pointer - which is what a destructor is.
    let destructor = (entry != 0).then(|| unsafe {
        core::mem::transmute::<usize, darwin::tls::key::Destructor>(entry as usize)
    });
    // A Darwin key, so the number the guest holds indexes the array its `%gs`
    // addresses. See [`darwin::tls::key`].
    let rc = match darwin::tls::key::Key::create(destructor) {
        Ok(key) => {
            mem.write_u64(out, u64::from(key.index()), symbol)?;
            0
        }
        Err(rc) => rc,
    };
    set_int_return(ctx, rc);
    Ok(())
}

pub type Shim = crate::signal::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    // The two whose object this runtime owns rather than the host, and which are
    // therefore shims only where that is true. See [`pthread_once`].
    if darwin::libc_backend::using_builtin() {
        match name {
            "pthread_once" => return Some(bind!(callback "pthread_once" => pthread_once)),
            "pthread_key_create" => {
                return Some(bind!(callback "pthread_key_create" => pthread_key_create));
            }
            _ => {}
        }
    }
    match name {
        "pthread_create" => Some(bind!(callback "pthread_create" => pthread_create)),
        // Off unless asked for: this turns every lock in the program into a VM
        // exit, which is not a price to pay for a diagnostic nobody is reading.
        "pthread_mutex_lock" if debug_mutex() => {
            Some(bind!(callback "pthread_mutex_lock" => mutex_lock))
        }
        _ => None,
    }
}

/// Whether `ROSEMARY_DEBUG_MUTEX` asked for the lock diagnostic. Read once, for
/// [`crate::trace::watching`]'s reason.
fn debug_mutex() -> bool {
    #[dynamic]
    static ON: bool = std::env::var_os("ROSEMARY_DEBUG_MUTEX").is_some();
    *ON
}

/// `int pthread_mutex_lock(pthread_mutex_t *)`, reporting a failure by address.
fn mutex_lock(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    _callbacks: &dyn Callbacks,
) -> Result<()> {
    let at = ctx.rdi;
    // SAFETY: the mutex is identity-mapped guest memory. Use the implementation
    // selected for its initialization so the diagnostic reads the same layout.
    let rc = unsafe {
        if darwin::libc_backend::using_builtin() {
            darwin::pthread::sync::mutex_lock(at)
        } else {
            libc::pthread_mutex_lock(at as *mut libc::pthread_mutex_t)
        }
    };
    if rc != 0 {
        let mut head = [0u8; 16];
        let read = mem.read(at, &mut head).is_ok();
        eprintln!(
            "rosemary: {symbol}({at:#x}) failed with {rc}; first 16 bytes {}",
            match read {
                true => head
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                false => "unreadable".to_string(),
            }
        );
    }
    ctx.rax = rc as u32 as u64;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use darwin::pthread::Attributes;

    /// Reading a guest's attribute set is [`darwin::pthread`]'s job and is tested
    /// there, on both hosts. What is this module's is the last step: turning what the
    /// guest asked for into where its frames actually go.
    fn asked(stack_top: Option<u64>, stack_size: Option<usize>) -> Attributes {
        Attributes {
            stack_top,
            stack_size,
            ..Attributes::default()
        }
    }

    #[test]
    fn a_guest_that_asked_for_nothing_gets_the_default_stack() {
        assert_eq!(stack_request(&asked(None, None)), StackRequest::Default);
    }

    #[test]
    fn a_size_alone_is_a_size_and_not_an_address() {
        assert_eq!(
            stack_request(&asked(None, Some(1024 * 1024))),
            StackRequest::Sized(1024 * 1024)
        );
    }

    /// A guest that supplied memory runs its frames on that memory, named by its
    /// **top** - the end Darwin's `pthread_attr_getstackaddr` reports.
    #[test]
    fn a_supplied_stack_is_used_where_the_guest_put_it() {
        let (top, size) = (0x2_0010_0000u64, 256 * 1024);
        assert_eq!(
            stack_request(&asked(Some(top), Some(size))),
            StackRequest::Supplied {
                top,
                size: size as u64
            }
        );
    }

    /// A supplied stack of no size is not a stack. Falling back beats handing a
    /// thread a zero-byte range to run on.
    #[test]
    fn a_supplied_stack_with_no_size_falls_back() {
        assert_eq!(
            stack_request(&asked(Some(0x2_0000_0000), Some(0))),
            StackRequest::Sized(0)
        );
        assert_eq!(stack_request(&asked(Some(0), None)), StackRequest::Default);
    }

    /// The host thread gets this runtime's own stack whatever the guest asked for:
    /// the guest's answer describes the guest's frames, and the JIT's go elsewhere.
    #[test]
    fn the_host_thread_does_not_inherit_the_guests_stack_request() {
        let mut attributes = asked(Some(0x2_0000_0000), Some(64 * 1024));
        attributes.detached = true;
        let mut host = attributes
            .to_host_attr(HOST_THREAD_STACK)
            .expect("a host attribute set");

        let mut size = 0usize;
        // SAFETY: `host` is an initialised attribute set and `size` a live local.
        unsafe { libc::pthread_attr_getstacksize(host.as_mut_ptr(), &mut size) };
        assert_eq!(
            size, HOST_THREAD_STACK,
            "the host thread gets the runtime's own"
        );

        let mut detach = 0;
        unsafe {
            darwin::pthread::raw::pthread_attr_getdetachstate(host.as_mut_ptr(), &mut detach)
        };
        assert_ne!(
            detach, 0,
            "and a detached guest thread is a detached host thread, in the host's own \
             numbering - which is not Darwin's"
        );
    }
}
