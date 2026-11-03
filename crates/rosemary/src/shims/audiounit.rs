use core::ffi::c_void;

use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{Callbacks, GuestMemory, Result};

use super::{bad_pointer, set_int_return};
use crate::shims::bind;

/// `kAudioUnitProperty_SetRenderCallback`, the one a program that plays audio
/// installs.
const SET_RENDER_CALLBACK: u32 = 23;
/// `kAudioUnitProperty_SetExternalBuffer` is *not* here; this is the deprecated
/// `kAudioUnitProperty_SetInputCallback`, which older code still sets and which
/// carries the identical structure.
const SET_INPUT_CALLBACK: u32 = 22;
/// `kAudioOutputUnitProperty_SetInputCallback`, the output-unit spelling.
const OUTPUT_SET_INPUT_CALLBACK: u32 = 2005;

/// `sizeof(AURenderCallbackStruct)`: a function pointer and a cookie.
const CALLBACK_STRUCT_SIZE: u32 = 16;

/// `kAudioUnitErr_InvalidPropertyValue`, which is what the framework answers a
/// caller whose property data is the wrong size - so a guest handing over a
/// structure this shim would misread gets its own framework's error rather than
/// a silently truncated read.
const INVALID_PROPERTY_VALUE: i32 = -10851;

/// The framework's own entry point, found by `dlsym` rather than linked.
///
/// Deliberately not `#[link(name = "AudioToolbox", kind = "framework")]`.
type SetPropertyFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void, u32) -> i32;

fn host_set_property() -> Option<SetPropertyFn> {
    static FOUND: std::sync::OnceLock<Option<SetPropertyFn>> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        let name = c"AudioUnitSetProperty";
        // SAFETY: a NUL-terminated name and a well-known pseudo-handle; the
        // result is either null or the framework's own function, and the type
        // is the one AudioToolbox.h declares.
        let sym = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        (!sym.is_null()).then(|| unsafe { core::mem::transmute::<*mut c_void, SetPropertyFn>(sym) })
    })
}

/// Whether `id` names a property whose data begins with a callback pointer.
fn carries_a_callback(id: u32) -> bool {
    matches!(
        id,
        SET_RENDER_CALLBACK | SET_INPUT_CALLBACK | OUTPUT_SET_INPUT_CALLBACK
    )
}

/// `OSStatus AudioUnitSetProperty(AudioUnit, AudioUnitPropertyID,
/// AudioUnitScope, AudioUnitElement, const void *inData, UInt32 inDataSize)`
pub fn set_property(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    callbacks: &dyn Callbacks,
) -> Result<()> {
    let (unit, id, scope) = (ctx.rdi, ctx.rsi as u32, ctx.rdx as u32);
    let (element, data, size) = (ctx.rcx as u32, ctx.r8, ctx.r9 as u32);

    // Every other property is bytes, and bytes are identity-mapped.
    if !carries_a_callback(id) || data == 0 {
        // SAFETY: `data` is either null or a guest address, which under identity
        // mapping is a readable host address for the `size` bytes the guest
        // says it is.
        let rc = unsafe {
            apply(
                symbol,
                unit,
                id,
                scope,
                element,
                data as *const c_void,
                size,
            )?
        };
        set_int_return(ctx, rc);
        return Ok(());
    }

    if size < CALLBACK_STRUCT_SIZE {
        set_int_return(ctx, INVALID_PROPERTY_VALUE);
        return Ok(());
    }

    let mut buf = [0u8; CALLBACK_STRUCT_SIZE as usize];
    mem.read(data, &mut buf)
        .map_err(|_| bad_pointer(symbol, data))?;
    let proc_va = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let ref_con = u64::from_le_bytes(buf[8..16].try_into().unwrap());

    // The cookie is passed through untouched: it is the guest's own pointer and
    // the *guest's* callback is what will dereference it.
    let substituted: [u64; 2] = [callbacks.substitute(symbol, proc_va)?, ref_con];

    // SAFETY: `unit` is an `AudioUnit` the guest received from
    // `AudioComponentInstanceNew`, and `substituted` is a live local of exactly
    // the size being declared. The implementation copies it before returning.
    let rc = unsafe {
        apply(
            symbol,
            unit,
            id,
            scope,
            element,
            substituted.as_ptr().cast(),
            CALLBACK_STRUCT_SIZE,
        )?
    };
    set_int_return(ctx, rc);
    Ok(())
}

/// Perform the call, on whichever implementation this run's frameworks are.
///
/// # Safety
///
/// `data` must be `size` readable bytes, and a callback property's must already
/// hold a *host*-callable function pointer.
unsafe fn apply(
    symbol: &str,
    unit: u64,
    id: u32,
    scope: u32,
    element: u32,
    data: *const c_void,
    size: u32,
) -> Result<i32> {
    if crate::frameworks::using_builtin() {
        // SAFETY: the caller's obligation, and `unit` is a handle
        // `coreaudio::AudioComponentInstanceNew` handed out - which it checks.
        return Ok(unsafe {
            coreaudio::property::AudioUnitSetProperty(
                unit as *mut c_void,
                id,
                scope,
                element,
                data,
                size,
            )
        });
    }
    // The guest cannot have reached this call without the framework being
    // loaded, so a miss is a fact about the host rather than about the guest -
    // reported by name, like any other unsatisfied import.
    let Some(set) = host_set_property() else {
        return Err(crate::abi::AbiError::Unsatisfied {
            symbol: symbol.to_string(),
        });
    };
    Ok(unsafe { set(unit as *mut c_void, id, scope, element, data, size) })
}

/// One of this module's implementations, in the shape [`crate::shims`] wants -
/// the wider one, carrying [`Callbacks`].
pub type Shim = crate::signal::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
///
/// `AudioUnitGetProperty` is deliberately absent.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "AudioUnitSetProperty" => Some(bind!(callback "AudioUnitSetProperty" => set_property)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shim finds the framework with `RTLD_DEFAULT`, which in a real run is
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

    struct Recording(std::cell::RefCell<Vec<u64>>);

    impl Callbacks for Recording {
        fn substitute(&self, _symbol: &str, va: u64) -> Result<u64> {
            self.0.borrow_mut().push(va);
            Ok(va ^ 0xffff_0000)
        }
    }

    /// Guest memory that is this process's memory, which identity mapping makes
    /// true of a real guest too.
    struct Identity;

    impl GuestMemory for Identity {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            if va == 0 {
                return Err(machish::Error::OutOfBounds {
                    addr: va,
                    len: out.len(),
                });
            }
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

    fn ctx_for(id: u32, callback: &[u64; 2], size: u32) -> RegisterContext {
        let mut ctx = RegisterContext::new();
        // A null unit.
        ctx.rdi = 0;
        ctx.rsi = u64::from(id);
        ctx.rdx = 0;
        ctx.rcx = 0;
        ctx.r8 = callback.as_ptr() as u64;
        ctx.r9 = u64::from(size);
        ctx
    }

    #[test]
    fn the_render_callback_crosses_out_and_the_cookie_does_not() {
        if !framework_loaded() {
            eprintln!("skipping: AudioToolbox is not present");
            return;
        }
        for id in [
            SET_RENDER_CALLBACK,
            SET_INPUT_CALLBACK,
            OUTPUT_SET_INPUT_CALLBACK,
        ] {
            let cb = [0x1_0000_1234u64, 0x2_0000_5678];
            let callbacks = Recording(std::cell::RefCell::new(Vec::new()));
            let mut ctx = ctx_for(id, &cb, CALLBACK_STRUCT_SIZE);

            set_property("_AudioUnitSetProperty", &mut ctx, &mut Identity, &callbacks)
                .expect("set");

            assert_eq!(
                *callbacks.0.borrow(),
                vec![0x1_0000_1234],
                "property {id}: the proc, and only the proc, must be substituted"
            );
            // The guest's own structure is untouched - the framework copies.
            assert_eq!(cb, [0x1_0000_1234, 0x2_0000_5678]);
        }
    }

    #[test]
    fn an_ordinary_property_is_passed_straight_through() {
        if !framework_loaded() {
            eprintln!("skipping: AudioToolbox is not present");
            return;
        }
        let data = [44100.0f64.to_bits(), 0];
        let callbacks = Recording(std::cell::RefCell::new(Vec::new()));
        // `kAudioUnitProperty_SampleRate` is 2, and carries no pointer.
        let mut ctx = ctx_for(2, &data, 8);

        set_property("_AudioUnitSetProperty", &mut ctx, &mut Identity, &callbacks).expect("set");

        assert!(
            callbacks.0.borrow().is_empty(),
            "a property with no callback in it must not be rewritten"
        );
    }

    #[test]
    fn a_callback_structure_too_small_to_hold_one_is_refused() {
        if !framework_loaded() {
            eprintln!("skipping: AudioToolbox is not present");
            return;
        }
        let cb = [0x1_0000_1234u64, 0];
        let callbacks = Recording(std::cell::RefCell::new(Vec::new()));
        let mut ctx = ctx_for(SET_RENDER_CALLBACK, &cb, 8);

        set_property("_AudioUnitSetProperty", &mut ctx, &mut Identity, &callbacks).expect("set");

        assert_eq!(ctx.rax as i32, INVALID_PROPERTY_VALUE);
        assert!(callbacks.0.borrow().is_empty());
    }
}
