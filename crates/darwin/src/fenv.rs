//! The `<fenv.h>` calls whose argument is a whole `fenv_t`.
//!
//! Darwin's x86-64 `fenv_t` is sixteen bytes holding the x87 control and status
//! words and `MXCSR`; glibc's is the twenty-eight byte `fnstenv` image plus
//! `MXCSR`. A host `fegetenv` writes past the end of a guest's environment, which
//! is the stack canary when the guest keeps it in a local. The calls taking only
//! exception bits or a rounding mode agree in constants and pass through.

use core::arch::asm;
use core::ffi::c_int;

/// Darwin's `FE_ALL_EXCEPT`, which unlike glibc's includes `FE_DENORMALOPERAND`.
const FE_ALL_EXCEPT: u16 = 0x3f;
/// The `MXCSR` exception masks sit this far above the matching status flags.
const MXCSR_MASK_SHIFT: u32 = 7;
/// `MXCSR`'s flush-to-zero and denormals-are-zero bits.
const MXCSR_FTZ_DAZ: u32 = 0x8040;
const X87_CONTROL_DEFAULT: u16 = 0x037f;
const MXCSR_DEFAULT: u32 = 0x1f80;

/// Darwin's x86-64 `fenv_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fenv {
    control: u16,
    status: u16,
    mxcsr: u32,
    reserved: [u8; 8],
}

/// What `fnstenv` stores in 64-bit mode.
#[repr(C)]
#[derive(Default)]
struct X87Env {
    control: u16,
    _reserved1: u16,
    status: u16,
    _reserved2: u16,
    _rest: [u32; 5],
}

/// `FE_DFL_ENV` and its sibling are addresses of libSystem data, not sentinels.
static FE_DFL_ENV: Fenv = Fenv {
    control: X87_CONTROL_DEFAULT,
    status: 0,
    mxcsr: MXCSR_DEFAULT,
    reserved: [0; 8],
};

static FE_DFL_DISABLE_SSE_DENORMS_ENV: Fenv = Fenv {
    mxcsr: MXCSR_DEFAULT | MXCSR_FTZ_DAZ,
    ..FE_DFL_ENV
};

fn store_x87() -> X87Env {
    let mut env = X87Env::default();
    // SAFETY: `fnstenv` writes exactly the 28 bytes of an `X87Env`. It also masks
    // every x87 exception, which is why each caller loads an environment back.
    unsafe { asm!("fnstenv [{}]", in(reg) &raw mut env, options(nostack)) };
    env
}

fn load_x87(env: &X87Env) {
    // SAFETY: an environment `fnstenv` produced, possibly with its control and
    // status words replaced.
    unsafe { asm!("fldenv [{}]", in(reg) env, options(nostack, readonly)) };
}

fn mxcsr() -> u32 {
    let mut value = 0u32;
    // SAFETY: stores four bytes into `value`.
    unsafe { asm!("stmxcsr [{}]", in(reg) &raw mut value, options(nostack)) };
    value
}

fn set_mxcsr(value: u32) {
    // SAFETY: loads four bytes from `value`. Reserved bits set would fault, and
    // only a guest's own corrupted environment can carry them.
    unsafe { asm!("ldmxcsr [{}]", in(reg) &value, options(nostack, readonly)) };
}

fn current() -> Fenv {
    let x87 = store_x87();
    load_x87(&x87);
    Fenv {
        control: x87.control,
        status: x87.status,
        mxcsr: mxcsr(),
        reserved: [0; 8],
    }
}

fn install(env: &Fenv) {
    let mut x87 = store_x87();
    x87.control = env.control;
    x87.status = env.status;
    load_x87(&x87);
    set_mxcsr(env.mxcsr);
}

/// `int fegetenv(fenv_t *envp)`.
pub unsafe extern "C" fn fegetenv(envp: *mut Fenv) -> c_int {
    // SAFETY: the caller passes a writable `fenv_t`.
    unsafe { envp.write(current()) };
    0
}

/// `int fesetenv(const fenv_t *envp)`.
pub unsafe extern "C" fn fesetenv(envp: *const Fenv) -> c_int {
    // SAFETY: the caller passes a readable `fenv_t`, `FE_DFL_ENV` included.
    install(unsafe { &*envp });
    0
}

/// `int feholdexcept(fenv_t *envp)`: save, then clear every flag and mask every
/// exception.
pub unsafe extern "C" fn feholdexcept(envp: *mut Fenv) -> c_int {
    let saved = current();
    // SAFETY: as `fegetenv`.
    unsafe { envp.write(saved) };
    install(&Fenv {
        control: saved.control | FE_ALL_EXCEPT,
        status: saved.status & !FE_ALL_EXCEPT,
        mxcsr: (saved.mxcsr | (u32::from(FE_ALL_EXCEPT) << MXCSR_MASK_SHIFT))
            & !u32::from(FE_ALL_EXCEPT),
        reserved: [0; 8],
    });
    0
}

/// `int feupdateenv(const fenv_t *envp)`: install `envp`, then raise what was
/// raised before it.
pub unsafe extern "C" fn feupdateenv(envp: *const Fenv) -> c_int {
    let now = current();
    let raised = (now.status | now.mxcsr as u16) & FE_ALL_EXCEPT;
    // SAFETY: as `fesetenv`.
    install(unsafe { &*envp });
    let mut x87 = store_x87();
    x87.status |= raised;
    load_x87(&x87);
    // Delivers the trap for any flag the restored environment leaves unmasked.
    // SAFETY: `fwait` has no operands.
    unsafe { asm!("fwait", options(nostack, nomem)) };
    0
}

exports::exports! {
    functions {
        "fegetenv" => fegetenv,
        "fesetenv" => fesetenv,
        "feholdexcept" => feholdexcept,
        "feupdateenv" => feupdateenv,
    }
    data {
        "_FE_DFL_ENV" => &raw const FE_DFL_ENV as usize as u64,
        "_FE_DFL_DISABLE_SSE_DENORMS_ENV" => &raw const FE_DFL_DISABLE_SSE_DENORMS_ENV as usize as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_environment_is_sixteen_bytes() {
        assert_eq!(size_of::<Fenv>(), 16);
        assert_eq!(size_of::<X87Env>(), 28);
    }

    /// The overrun that smashed the JVM's stack canary.
    #[test]
    fn fegetenv_stays_inside_the_callers_environment() {
        let mut buffer = [0xa5u8; 32];
        // SAFETY: the buffer holds a `Fenv`; `u8` imposes no alignment `Fenv`
        // needs beyond what the array start gives on this target.
        unsafe { fegetenv(buffer.as_mut_ptr().cast()) };
        assert!(buffer[16..].iter().all(|&b| b == 0xa5));
    }

    #[test]
    fn a_saved_rounding_mode_comes_back() {
        const ROUND_UP_X87: u16 = 0x0800;
        const ROUND_UP_SSE: u32 = 0x4000;
        let mut saved = FE_DFL_ENV;
        // SAFETY: valid environments on both sides.
        unsafe {
            fegetenv(&mut saved);
            fesetenv(&Fenv {
                control: saved.control | ROUND_UP_X87,
                mxcsr: saved.mxcsr | ROUND_UP_SSE,
                ..saved
            });
            assert_eq!(current().mxcsr & ROUND_UP_SSE, ROUND_UP_SSE);
            fesetenv(&saved);
        }
        assert_eq!(current(), saved);
    }

    #[test]
    fn holding_masks_and_clears_and_updating_restores() {
        let mut saved = FE_DFL_ENV;
        // SAFETY: valid environments on both sides.
        unsafe {
            feholdexcept(&mut saved);
            let held = current();
            assert_eq!(held.control & FE_ALL_EXCEPT, FE_ALL_EXCEPT);
            assert_eq!(held.mxcsr & u32::from(FE_ALL_EXCEPT), 0);
            feupdateenv(&saved);
        }
        assert_eq!(current().control, saved.control);
    }
}
