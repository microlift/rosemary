use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{GuestMemory, Result};
use crate::shims::bind;

/// `__float2 __sincosf_stret(float x)`
///
/// The argument arrives in `xmm0[31:0]` and both results leave in `xmm0`, low
/// half first - one register in, one register out, because eight bytes of
/// floating point is a single SSE eightbyte on the way in and on the way back.
pub fn sincosf_stret(
    _symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let x = f32::from_bits(ctx.xmm[0][0] as u32);
    let (sin, cos) = darwin::math::sincosf(x);
    ctx.xmm[0] = [
        u64::from(sin.to_bits()) | (u64::from(cos.to_bits()) << 32),
        0,
    ];
    Ok(())
}

/// `__double2 __sincos_stret(double x)`
///
/// Sixteen bytes, so SysV AMD64 splits it across `xmm0` and `xmm1` rather than
/// calling it memory - the boundary that makes this the *last* aggregate return
/// expressible in registers at all.
pub fn sincos_stret(
    _symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let x = f64::from_bits(ctx.xmm[0][0]);
    let (sin, cos) = darwin::math::sincos(x);
    ctx.xmm[0] = [sin.to_bits(), 0];
    ctx.xmm[1] = [cos.to_bits(), 0];
    Ok(())
}

/// `double _Complex __divdc3(double a, double b, double c, double d)`.
pub fn divdc3(_symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let (a, b, c, d) = (
        f64::from_bits(ctx.xmm[0][0]),
        f64::from_bits(ctx.xmm[1][0]),
        f64::from_bits(ctx.xmm[2][0]),
        f64::from_bits(ctx.xmm[3][0]),
    );

    let (real, imag) = darwin::math::divdc3(a, b, c, d);

    ctx.xmm[0] = [real.to_bits(), 0];
    ctx.xmm[1] = [imag.to_bits(), 0];
    Ok(())
}

/// `float _Complex __mulsc3(float a, float b, float c, float d)`.
///
/// [`divdc3`]'s sibling one size down and in the other operation, and its register
/// story is the one interesting thing about it.
pub fn mulsc3(_symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let half = |reg: usize, high: bool| {
        let word = ctx.xmm[reg][0];
        f32::from_bits(if high {
            (word >> 32) as u32
        } else {
            word as u32
        })
    };
    let (a, b) = (half(0, false), half(0, true));
    let (c, d) = (half(1, false), half(1, true));

    let (real, imag) = darwin::math::mulsc3(a, b, c, d);

    ctx.xmm[0] = [
        u64::from(real.to_bits()) | (u64::from(imag.to_bits()) << 32),
        0,
    ];
    Ok(())
}

/// One of this module's implementations, in the shape [`crate::shims`] wants.
pub type Shim = crate::shims::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "__divdc3" => Some(bind!("__divdc3" => divdc3)),
        "__mulsc3" => Some(bind!("__mulsc3" => mulsc3)),
        "__sincosf_stret" => Some(bind!("__sincosf_stret" => sincosf_stret)),
        "__sincos_stret" => Some(bind!("__sincos_stret" => sincos_stret)),
        _ => None,
    }
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

    /// The placement is the claim, so it is read back the way the *guest* would:
    /// the low eightbyte of `xmm0` as two packed floats.
    #[test]
    fn both_floats_come_back_in_one_register_sine_first() {
        for x in [0.0f32, 0.5, -1.25, 3.0, 1e6] {
            let mut ctx = RegisterContext::new();
            ctx.xmm[0] = [u64::from(x.to_bits()), 0];
            sincosf_stret("___sincosf_stret", &mut ctx, &mut NoMemory).unwrap();

            let packed = ctx.xmm[0][0];
            let sin = f32::from_bits(packed as u32);
            let cos = f32::from_bits((packed >> 32) as u32);
            assert_eq!(sin, x.sin(), "sine belongs in the low half for {x}");
            assert_eq!(cos, x.cos(), "cosine belongs in the high half for {x}");
            assert_eq!(ctx.xmm[0][1], 0, "the high eightbyte is not part of it");
        }
    }

    #[test]
    fn the_double_form_uses_two_registers() {
        for x in [0.0f64, 0.5, -1.25, 3.0, 1e6] {
            let mut ctx = RegisterContext::new();
            ctx.xmm[0] = [x.to_bits(), 0];
            sincos_stret("___sincos_stret", &mut ctx, &mut NoMemory).unwrap();

            assert_eq!(f64::from_bits(ctx.xmm[0][0]), x.sin());
            assert_eq!(f64::from_bits(ctx.xmm[1][0]), x.cos());
        }
    }

    #[test]
    fn complex_division_uses_four_inputs_and_two_outputs() {
        let mut ctx = RegisterContext::new();
        for (slot, value) in [2.0_f64, 3.0, 4.0, -5.0].into_iter().enumerate() {
            ctx.xmm[slot] = [value.to_bits(), 0];
        }
        divdc3("___divdc3", &mut ctx, &mut NoMemory).unwrap();

        assert!((f64::from_bits(ctx.xmm[0][0]) + 7.0 / 41.0).abs() < 1e-15);
        assert!((f64::from_bits(ctx.xmm[1][0]) - 22.0 / 41.0).abs() < 1e-15);
        assert_eq!(ctx.xmm[0][1], 0);
        assert_eq!(ctx.xmm[1][1], 0);
    }

    #[test]
    fn complex_division_scales_a_large_denominator() {
        let mut ctx = RegisterContext::new();
        for (slot, value) in [1.0_f64, 1.0, 1e200, 1e200].into_iter().enumerate() {
            ctx.xmm[slot] = [value.to_bits(), 0];
        }
        divdc3("___divdc3", &mut ctx, &mut NoMemory).unwrap();

        let real = f64::from_bits(ctx.xmm[0][0]);
        let imag = f64::from_bits(ctx.xmm[1][0]);
        assert!((real - 1e-200).abs() < 1e-215);
        assert!(imag.abs() < 1e-215);
    }

    /// The Mach-O spelling is what reaches [`shim_for`] after one underscore is
    /// stripped, and getting that wrong means the shim silently never fires.
    #[test]
    fn the_mach_o_spelling_resolves() {
        for symbol in ["___divdc3", "___sincosf_stret", "___sincos_stret"] {
            let name = symbol.strip_prefix('_').unwrap();
            assert!(shim_for(name).is_some(), "{symbol} is not claimed");
            assert!(crate::shims::is_shimmed(symbol), "{symbol} may be inlined");
        }
    }
}
