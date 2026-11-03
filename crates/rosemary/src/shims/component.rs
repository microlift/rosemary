use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{GuestMemory, Result};

use super::set_int_return;
use crate::shims::bind;

/// One of this module's implementations, in the shape [`crate::shims`] wants.
pub type Shim = crate::shims::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "FindNextComponent" => Some(bind!("FindNextComponent" => find_next_component)),
        "OpenAComponent" => Some(bind!("OpenAComponent" => open_a_component)),
        "CloseComponent" => Some(bind!("CloseComponent" => close_component)),
        _ => None,
    }
}

/// The three modern entry points, found by `dlsym` rather than linked - the same care
/// [`audiounit`](super::audiounit) takes and for the same reason.
fn host_fn<T: Copy>(name: &core::ffi::CStr, cache: &std::sync::OnceLock<Option<T>>) -> Option<T> {
    *cache.get_or_init(|| {
        // SAFETY: a NUL-terminated name and a well-known pseudo-handle. The
        // result is either null or the framework's own function, and `T` is the
        // type `AudioComponent.h` declares for it.
        let sym = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        (!sym.is_null())
            .then(|| unsafe { core::mem::transmute_copy::<*mut core::ffi::c_void, T>(&sym) })
    })
}

type FindNextFn = unsafe extern "C" fn(*mut core::ffi::c_void, *const u8) -> *mut u8;
type InstanceNewFn = unsafe extern "C" fn(*mut u8, *mut *mut u8) -> i32;
type InstanceDisposeFn = unsafe extern "C" fn(*mut u8) -> i32;

/// The refusal for a host that does not export the modern registry, which no
/// macOS running arm64 code is - but a missing symbol answered with a null call
/// is the one outcome worse than saying so.
fn no_audio_component(symbol: &str) -> crate::abi::AbiError {
    crate::abi::AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "AudioToolbox does not export the AudioComponent registry, so the \
                 Component Manager call it stands in for cannot be answered",
    }
}

/// `Component FindNextComponent(Component aComponent, ComponentDescription *looking)`
fn find_next_component(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    if builtin() {
        // SAFETY: null or a readable descriptor in guest memory, which is
        // identity-mapped; `coreaudio` copies it and dereferences nothing else.
        ctx.rax = unsafe {
            coreaudio::AudioComponentFindNext(
                ctx.rdi as *mut core::ffi::c_void,
                ctx.rsi as *const _,
            )
        } as u64;
        return Ok(());
    }
    static FOUND: std::sync::OnceLock<Option<FindNextFn>> = std::sync::OnceLock::new();
    let Some(f) = host_fn(c"AudioComponentFindNext", &FOUND) else {
        return Err(no_audio_component(symbol));
    };
    // SAFETY: both arguments are opaque to this runtime - a component handle
    // the previous call returned, and a twenty-byte descriptor in guest memory,
    // which is identity-mapped. Neither is dereferenced here.
    let found = unsafe { f(ctx.rdi as *mut core::ffi::c_void, ctx.rsi as *const u8) };
    ctx.rax = found as u64;
    Ok(())
}

/// Whether this run's AudioUnit registry is the one in `crates/coreaudio`.
fn builtin() -> bool {
    crate::frameworks::using_builtin()
}

/// `OSErr OpenAComponent(Component aComponent, ComponentInstance *ci)`
fn open_a_component(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    if builtin() {
        // SAFETY: a component `find_next_component` handed out, and a
        // pointer-sized writable slot in guest memory.
        set_int_return(ctx, unsafe {
            coreaudio::AudioComponentInstanceNew(
                ctx.rdi as *mut core::ffi::c_void,
                ctx.rsi as *mut *mut core::ffi::c_void,
            )
        });
        return Ok(());
    }
    static FOUND: std::sync::OnceLock<Option<InstanceNewFn>> = std::sync::OnceLock::new();
    let Some(f) = host_fn(c"AudioComponentInstanceNew", &FOUND) else {
        return Err(no_audio_component(symbol));
    };
    // SAFETY: `rdi` is a component this process's own AudioToolbox handed out,
    // and `rsi` is a pointer-sized slot in guest memory for the instance.
    let status = unsafe { f(ctx.rdi as *mut u8, ctx.rsi as *mut *mut u8) };
    set_int_return(ctx, status);
    Ok(())
}

/// `OSErr CloseComponent(ComponentInstance aComponentInstance)`
fn close_component(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    if builtin() {
        // SAFETY: an instance `open_a_component` produced.
        set_int_return(ctx, unsafe {
            coreaudio::AudioComponentInstanceDispose(ctx.rdi as *mut core::ffi::c_void)
        });
        return Ok(());
    }
    static FOUND: std::sync::OnceLock<Option<InstanceDisposeFn>> = std::sync::OnceLock::new();
    let Some(f) = host_fn(c"AudioComponentInstanceDispose", &FOUND) else {
        return Err(no_audio_component(symbol));
    };
    // SAFETY: the instance `OpenAComponent` produced, which is an
    // `AudioComponentInstance` because this module made it one.
    let status = unsafe { f(ctx.rdi as *mut u8) };
    set_int_return(ctx, status);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The shim finds AudioToolbox with `RTLD_DEFAULT`, which in a real run is
    /// satisfied by the guest's own import having loaded it.
    fn framework_loaded() -> bool {
        static LOADED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *LOADED.get_or_init(|| {
            let path = c"/System/Library/Frameworks/AudioToolbox.framework/AudioToolbox";
            // SAFETY: a NUL-terminated path; the handle is deliberately leaked,
            // as every `dlopen` in this runtime is.
            !unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) }.is_null()
        })
    }

    /// `{ 'auou', 'def ', 'appl', 0, 0 }` - the default output unit, laid out as
    /// the guest's twenty-byte `ComponentDescription`.
    fn default_output_description() -> [u32; 5] {
        [
            u32::from_be_bytes(*b"auou"),
            u32::from_be_bytes(*b"def "),
            u32::from_be_bytes(*b"appl"),
            0,
            0,
        ]
    }

    #[test]
    fn the_default_output_unit_is_found_where_the_legacy_call_finds_nothing() {
        // The finding this module exists for, asserted rather than described.
        if !framework_loaded() {
            return;
        }
        let desc = default_output_description();
        let mut ctx = RegisterContext::new();
        ctx.rdi = 0;
        ctx.rsi = desc.as_ptr() as u64;
        find_next_component("_FindNextComponent", &mut ctx, &mut NoMemory).unwrap();
        if ctx.rax == 0 && !builtin() {
            // A host without an output device may have no default component.
            return;
        }
        assert_ne!(ctx.rax, 0, "AudioComponentFindNext should find auou/def ");
    }

    #[test]
    fn a_component_opens_and_closes() {
        if !framework_loaded() {
            return;
        }
        let desc = default_output_description();
        let mut ctx = RegisterContext::new();
        ctx.rdi = 0;
        ctx.rsi = desc.as_ptr() as u64;
        find_next_component("_FindNextComponent", &mut ctx, &mut NoMemory).unwrap();
        let component = ctx.rax;
        if component == 0 {
            return;
        }

        let mut instance: *mut u8 = core::ptr::null_mut();
        ctx.rdi = component;
        ctx.rsi = (&raw mut instance) as u64;
        open_a_component("_OpenAComponent", &mut ctx, &mut NoMemory).unwrap();
        assert_eq!(ctx.rax as i32, 0, "OpenAComponent should succeed");
        assert!(!instance.is_null(), "and should write the instance out");

        ctx.rdi = instance as u64;
        close_component("_CloseComponent", &mut ctx, &mut NoMemory).unwrap();
        assert_eq!(ctx.rax as i32, 0);
    }
}
