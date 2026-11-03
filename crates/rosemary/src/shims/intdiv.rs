use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{AbiError, GuestMemory, Result};
use crate::shims::bind;

/// One of this module's implementations, in the shape [`crate::shims`] wants.
pub type Shim = crate::shims::Shim;

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<Shim> {
    match name {
        "div" => Some(bind!("div" => div)),
        // `lldiv` and `imaxdiv` are `ldiv` under other names on an LP64
        // platform.
        "ldiv" => Some(bind!("ldiv" => ldiv)),
        "lldiv" => Some(bind!("lldiv" => ldiv)),
        "imaxdiv" => Some(bind!("imaxdiv" => ldiv)),
        // The compiler-rt 128-bit helpers.
        "__udivti3" => Some(bind!("__udivti3" => udivti3)),
        "__umodti3" => Some(bind!("__umodti3" => umodti3)),
        "__divti3" => Some(bind!("__divti3" => divti3)),
        "__modti3" => Some(bind!("__modti3" => modti3)),
        "__udivmodti4" => Some(bind!("__udivmodti4" => udivmodti4)),
        _ => None,
    }
}

/// The two `__int128` arguments SysV puts in `rdi:rsi` and `rdx:rcx`.
fn operands(ctx: &RegisterContext) -> (u128, u128) {
    let a = u128::from(ctx.rdi) | (u128::from(ctx.rsi) << 64);
    let b = u128::from(ctx.rdx) | (u128::from(ctx.rcx) << 64);
    (a, b)
}

/// A 128-bit result, in the `rax:rdx` pair SysV returns one in.
fn result(ctx: &mut RegisterContext, value: u128) {
    ctx.rax = value as u64;
    ctx.rdx = (value >> 64) as u64;
}

/// `unsigned __int128 __udivti3(unsigned __int128 a, unsigned __int128 b)`
fn udivti3(symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let (a, b) = operands(ctx);
    let Some(q) = a.checked_div(b) else {
        return Err(would_have_trapped(symbol));
    };
    result(ctx, q);
    Ok(())
}

/// `unsigned __int128 __umodti3(unsigned __int128 a, unsigned __int128 b)`
fn umodti3(symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let (a, b) = operands(ctx);
    let Some(r) = a.checked_rem(b) else {
        return Err(would_have_trapped(symbol));
    };
    result(ctx, r);
    Ok(())
}

/// `__int128 __divti3(__int128 a, __int128 b)` - signed, truncating toward
/// zero, and `checked_div` refuses the one overflow (`i128::MIN / -1`) for the
/// same reason `ldiv` does.
fn divti3(symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let (a, b) = operands(ctx);
    let Some(q) = (a as i128).checked_div(b as i128) else {
        return Err(would_have_trapped(symbol));
    };
    result(ctx, q as u128);
    Ok(())
}

/// `__int128 __modti3(__int128 a, __int128 b)`
fn modti3(symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let (a, b) = operands(ctx);
    let Some(r) = (a as i128).checked_rem(b as i128) else {
        return Err(would_have_trapped(symbol));
    };
    result(ctx, r as u128);
    Ok(())
}

/// `unsigned __int128 __udivmodti4(unsigned __int128 a, unsigned __int128 b, unsigned
/// __int128 *rem)` - the quotient returned, the remainder written through the pointer
/// in `r8`, which is the register after the two pairs.
fn udivmodti4(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let (a, b) = operands(ctx);
    let (Some(q), Some(r)) = (a.checked_div(b), a.checked_rem(b)) else {
        return Err(would_have_trapped(symbol));
    };
    let at = ctx.r8;
    if at != 0 {
        mem.write(at, &r.to_le_bytes())
            .map_err(|_| AbiError::Memory {
                symbol: symbol.to_string(),
                addr: at,
            })?;
    }
    result(ctx, q);
    Ok(())
}

/// `div_t div(int numer, int denom)` - eight bytes, so one eightbyte, so `rax`.
fn div(symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let (numer, denom) = (ctx.rdi as i32, ctx.rsi as i32);
    let (quot, rem) = match (numer.checked_div(denom), numer.checked_rem(denom)) {
        (Some(q), Some(r)) => (q, r),
        _ => return Err(would_have_trapped(symbol)),
    };
    ctx.rax = u64::from(quot as u32) | (u64::from(rem as u32) << 32);
    Ok(())
}

/// `ldiv_t ldiv(long numer, long denom)` - sixteen bytes, so `rax` and `rdx`.
fn ldiv(symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let (numer, denom) = (ctx.rdi as i64, ctx.rsi as i64);
    let (quot, rem) = match (numer.checked_div(denom), numer.checked_rem(denom)) {
        (Some(q), Some(r)) => (q, r),
        _ => return Err(would_have_trapped(symbol)),
    };
    ctx.rax = quot as u64;
    ctx.rdx = rem as u64;
    Ok(())
}

/// The refusal for the two argument pairs `idiv` itself would have faulted on.
fn would_have_trapped(symbol: &str) -> AbiError {
    AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "a divisor of zero, or a quotient too large to represent - the \
                 division the guest asked for is the one its own hardware traps on",
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

    fn call(name: &str, numer: u64, denom: u64) -> (u64, u64) {
        let mut ctx = RegisterContext::new();
        ctx.rdi = numer;
        ctx.rsi = denom;
        shim_for(name).expect("shimmed")(&mut ctx, &mut NoMemory).expect("divides");
        (ctx.rax, ctx.rdx)
    }

    /// Call a 128-bit helper the way SysV would: `rdi:rsi`, then `rdx:rcx`.
    fn call128(name: &str, a: u128, b: u128) -> (u128, RegisterContext) {
        let mut ctx = RegisterContext::new();
        ctx.rdi = a as u64;
        ctx.rsi = (a >> 64) as u64;
        ctx.rdx = b as u64;
        ctx.rcx = (b >> 64) as u64;
        shim_for(name).expect("shimmed")(&mut ctx, &mut NoMemory).expect("divides");
        (u128::from(ctx.rax) | (u128::from(ctx.rdx) << 64), ctx)
    }

    #[test]
    fn a_128_bit_quotient_spans_the_register_pair() {
        // A value that does not fit in 64 bits, so a shim that only looked at
        // `rdi` and `rdx` would get a different answer rather than a wrong-
        // looking one.
        let a = 1u128 << 100;
        let (q, _) = call128("__udivti3", a, 3);
        assert_eq!(q, a / 3);
        let (r, _) = call128("__umodti3", a, 3);
        assert_eq!(r, a % 3);
    }

    #[test]
    fn the_signed_forms_truncate_toward_zero() {
        let a = -(1i128 << 100);
        let (q, _) = call128("__divti3", a as u128, 3);
        assert_eq!(q as i128, a / 3);
        let (r, _) = call128("__modti3", a as u128, 3);
        assert_eq!(r as i128, a % 3);
    }

    #[test]
    fn a_zero_divisor_is_refused_rather_than_answered() {
        for name in [
            "__udivti3",
            "__umodti3",
            "__divti3",
            "__modti3",
            "__udivmodti4",
        ] {
            let mut ctx = RegisterContext::new();
            ctx.rdi = 1;
            assert!(
                shim_for(name).expect("shimmed")(&mut ctx, &mut NoMemory).is_err(),
                "a zero divisor answered"
            );
        }
        // And the one signed overflow, which `idiv` traps on too.
        let mut ctx = RegisterContext::new();
        ctx.rsi = 1u64 << 63; // i128::MIN
        ctx.rdx = u64::MAX;
        ctx.rcx = u64::MAX; // -1
        assert!(divti3("__divti3", &mut ctx, &mut NoMemory).is_err());
    }

    #[test]
    fn a_div_t_arrives_whole_in_rax() {
        // 17 / 5 = 3 remainder 2, packed low-half-first.
        let (rax, _) = call("div", 17, 5);
        assert_eq!(rax & 0xffff_ffff, 3);
        assert_eq!(rax >> 32, 2);
    }

    #[test]
    fn division_truncates_toward_zero_as_c_says() {
        // Not floor division: -7 / 2 is -3 remainder -1, and the sign of the
        // remainder follows the numerator.
        let (rax, _) = call("div", (-7i32) as u32 as u64, 2);
        assert_eq!(rax as u32 as i32, -3);
        assert_eq!((rax >> 32) as u32 as i32, -1);

        let (quot, rem) = call("ldiv", (-7i64) as u64, 2);
        assert_eq!(quot as i64, -3);
        assert_eq!(rem as i64, -1);
    }

    #[test]
    fn an_ldiv_t_is_two_registers() {
        let (quot, rem) = call("ldiv", 1_000_000_000_007, 1_000_000);
        assert_eq!(quot, 1_000_000);
        assert_eq!(rem, 7);
    }

    #[test]
    fn a_division_the_hardware_would_trap_on_is_refused_rather_than_answered() {
        let mut ctx = RegisterContext::new();
        ctx.rdi = 1;
        ctx.rsi = 0;
        let before = ctx.rax;
        assert!(ldiv("_ldiv", &mut ctx, &mut NoMemory).is_err());
        assert_eq!(ctx.rax, before, "a refusal leaves the context untouched");

        // The other `#DE`: the quotient of the most negative value and -1 is
        // one past the top of the range.
        ctx.rdi = i64::MIN as u64;
        ctx.rsi = (-1i64) as u64;
        assert!(ldiv("_ldiv", &mut ctx, &mut NoMemory).is_err());
    }

    #[test]
    fn the_three_lp64_spellings_are_one_implementation() {
        for name in ["ldiv", "lldiv", "imaxdiv"] {
            assert!(shim_for(name).is_some(), "{name}");
        }
        assert!(shim_for("div").is_some());
        assert!(shim_for("fdiv").is_none());
    }
}
