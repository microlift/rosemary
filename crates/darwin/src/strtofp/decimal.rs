//! Decimal significand times a power of ten, correctly rounded.
//!
//! Three methods, fastest first. Each either answers exactly or declines; the
//! last always answers, so the earlier ones change speed and never results.

use super::big::Big;
use super::format::{Binary, Encoded, Format, Native, Rounding};
use super::scan::Decimal;
use super::tables;

pub(super) fn convert(
    format: &Format,
    negative: bool,
    decimal: &Decimal,
    mode: Rounding,
) -> Encoded {
    // The value is below 10^magnitude and at least a tenth of it.
    let magnitude = decimal.exponent + decimal.count as i64;
    if magnitude < format.min_decimal {
        return format.underflow(negative, mode);
    }
    if magnitude > format.max_decimal {
        return format.overflow(negative, mode);
    }
    if mode == Rounding::Nearest
        && let Some(bits) = hardware(format, decimal)
    {
        return Encoded {
            bits: bits | (u128::from(negative) << (format.width - 1)),
            range_error: false,
        };
    }
    interval(format, negative, decimal, mode)
        .unwrap_or_else(|| exact(format, negative, decimal, mode))
}

/// Clinger's observation: when the significand and the power of ten are
/// both exact in the host type, one hardware operation rounds correctly.
/// Only in round-to-nearest, the one mode Rust's float arithmetic promises.
fn hardware(format: &Format, decimal: &Decimal) -> Option<u128> {
    const F64_POWERS: [f64; 23] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
        1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
    ];
    const F32_POWERS: [f32; 11] = [1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10];
    if decimal.count > 19 {
        return None;
    }
    let (digits, exponent) = (decimal.leading, decimal.exponent);
    let power = usize::try_from(exponent.unsigned_abs()).ok()?;
    match format.native {
        Native::F64 if exponent == 0 => Some((digits as f64).to_bits().into()),
        Native::F64 if digits <= 1 << 53 => {
            let ten = *F64_POWERS.get(power)?;
            let value = if exponent < 0 {
                digits as f64 / ten
            } else {
                digits as f64 * ten
            };
            Some(value.to_bits().into())
        }
        Native::F32 if exponent == 0 => Some((digits as f32).to_bits().into()),
        Native::F32 if digits <= 1 << 24 => {
            let ten = *F32_POWERS.get(power)?;
            let value = if exponent < 0 {
                digits as f32 / ten
            } else {
                digits as f32 * ten
            };
            Some(value.to_bits().into())
        }
        _ => None,
    }
}

/// Bound the product from both sides with 64-bit arithmetic. Rounding is
/// monotonic, so when both bounds round to the same result - including the
/// same `ERANGE` - so does everything between them. That settles nearly
/// every input with at most 53 significant bits.
fn interval(format: &Format, negative: bool, decimal: &Decimal, mode: Rounding) -> Option<Encoded> {
    if format.sig_bits > 53 {
        return None;
    }
    // Past 19 digits, the leading ones stand for the rest to within one.
    let (power, width) = match decimal.count {
        0..=19 => (decimal.exponent, 12u128),
        count => (decimal.exponent + count as i64 - 19, 80),
    };
    let (ten, ten_exp) = tables::ten_to_the(i32::try_from(power).ok()?)?;
    let digits_zeros = decimal.leading.leading_zeros();
    let digits = decimal.leading << digits_zeros;
    let product = ((u128::from(ten) * u128::from(digits)) >> 64) as u64;
    // At most two bits short, since both factors are at least 2^62.
    let product_zeros = product.leading_zeros();
    let low = product << product_zeros;
    // The truncations above cost at most three units before the shift, or
    // twenty with the digits past 19 unknown.
    let exp = i64::from(ten_exp) - i64::from(digits_zeros) - i64::from(product_zeros);
    let high = u128::from(low) + width;
    let top = exp + 64;
    if format.min_exp < top && top < format.max_exp && high >> 64 == 0 {
        return normal_interval(format, negative, low, high as u64, top, mode);
    }
    // FIXME (ACCURACY): Darwin's binary64 interval path sets `ERANGE` for a
    // result that rounds up from below 2^-1022 to exactly -DBL_MIN, but not
    // for +DBL_MIN: its subnormal branch tests `raw != 0x0010000000000000`
    // with the sign bit already in `raw`. Its slow path, which decides the
    // same inputs when the interval does not, sets no `ERANGE` for either
    // sign, and this follows the slow path. Reproducing it needs Apple's
    // exact decision of when that branch settles a value rather than falling
    // through: `adjustedBinaryExponent <= minBinaryExp`, `shift = 65 -
    // binaryExponent + (minBinaryExp - sigBits) < 64`, its rounding offsets
    // (`lro`/`uro` from the mode and sign, widened by `intervalWidth` 12 or
    // 80), and `upperSignificand == lowerSignificand`. Example:
    // "-2.2250738585072011e-308" in FE_DOWNWARD gives -DBL_MIN with `ERANGE`
    // there and without it here. Binary32 has no such branch (its subnormals
    // always fall through to the slow path).
    let bound = |sig| {
        let value = Binary {
            sig,
            exp,
            sticky: false,
        };
        format.round(negative, value, mode, false)
    };
    let lower = bound(low.into());
    (lower == bound(high)).then_some(lower)
}

/// [`interval`] where the result is certainly normal, in 64-bit arithmetic:
/// this is the common case and [`Format::round`] is general enough to be
/// slow for it. `low` and `high` are normalized with their value in
/// `[2^(top-1), 2^top)`.
fn normal_interval(
    format: &Format,
    negative: bool,
    low: u64,
    high: u64,
    top: i64,
    mode: Rounding,
) -> Option<Encoded> {
    let drop = 64 - format.sig_bits;
    let half = 1u64 << (drop - 1);
    let round = |sig: u64| {
        let kept = sig >> drop;
        let rest = sig & ((1 << drop) - 1);
        let up = match mode {
            Rounding::Nearest => rest > half || (rest == half && kept & 1 == 1),
            _ => rest != 0 && mode.directed_away(negative),
        };
        kept + u64::from(up)
    };
    let kept = round(low);
    if kept != round(high) {
        return None;
    }
    // Adding the significand with its leading bit carries that bit into the
    // exponent field, so a round up to the next power of two needs no case.
    let field = (top - format.min_exp - 1) as u128;
    let stored = format.sig_bits - 1;
    let sign = u128::from(negative) << (format.width - 1);
    Some(Encoded {
        bits: sign | ((field << stored) + u128::from(kept)),
        range_error: false,
    })
}

/// The exact value with big integers.
fn exact(format: &Format, negative: bool, decimal: &Decimal, mode: Rounding) -> Encoded {
    // Digits past `midpoint_digits` cannot move the result across a rounding
    // boundary, so they collapse to one digit that says whether any is nonzero.
    let limit = format.midpoint_digits;
    let mut digits = decimal.digits.iter().filter(|c| c.is_ascii_digit());
    let mut significand = Big::default();
    let (mut chunk, mut chunk_len) = (0u64, 0);
    for &c in digits.by_ref().take(limit) {
        chunk = chunk * 10 + u64::from(c - b'0');
        chunk_len += 1;
        if chunk_len == 19 {
            significand.mul_add(10u64.pow(chunk_len), chunk);
            (chunk, chunk_len) = (0, 0);
        }
    }
    if chunk_len > 0 {
        significand.mul_add(10u64.pow(chunk_len), chunk);
    }
    let mut used = decimal.count.min(limit);
    if decimal.count > limit {
        let nonzero = digits.any(|&c| c != b'0');
        significand.mul_add(10, nonzero.into());
        used += 1;
    }
    let power = decimal.exponent + (decimal.count - used) as i64;

    let value = if power >= 0 {
        significand.mul_pow5(power as u32);
        let (sig, shift, sticky) = significand.top128();
        Binary {
            sig,
            exp: i64::from(shift) + power,
            sticky,
        }
    } else {
        // FIXME (ACCURACY): Darwin's slow path can corrupt this division and
        // return a wrong last bit. `generalSlowpath` carves the numerator and
        // the 5^k denominator out of one buffer of 32-bit words, numerator
        // first, sized `numeratorWordsNeeded = ceil(max(significandDigits *
        // 1701 >> 9, exponentBits + sigBits + 2) / 32) + 2`. `divideMPByMP`
        // then normalizes both by the divisor's leading zeros and may add a
        // high word to the numerator, and writes the quotient downward from
        // `numerator.msw`. When the numerator has grown to fill its share,
        // `numerator.msw == denominator.lsw` (the `assert` there fails in a
        // debug build), and the first quotient word overwrites the divisor's
        // lowest word, which the remaining Algorithm D steps still read. The
        // result is typically a spurious nonzero remainder: " 00009." then 317
        // zeros in FE_UPWARD gives 0x1.2000000000001p+3 instead of 9, and
        // "9010309969950787." then 331 zeros trips the assert in every mode.
        // It needs many digits (so the interval path declines) and a negative
        // power of ten. To match, run this division as Darwin lays it out:
        // 32-bit words in one buffer with those sizes, quotient overlapping
        // the divisor, for each format's `midpoint_digits` truncation.
        let k = power.unsigned_abs() as u32;
        let mut five = Big::from_u64(1);
        five.mul_pow5(k);
        // Enough numerator bits for a quotient of at least 129.
        let scale = (five.bit_len() + 129).saturating_sub(significand.bit_len());
        significand.shl(scale);
        let (quotient, remainder) = significand.div_rem(&five);
        let (sig, shift, sticky) = quotient.top128();
        Binary {
            sig,
            exp: i64::from(shift) - i64::from(scale) - i64::from(k),
            sticky: sticky || remainder,
        }
    };
    format.round(negative, value, mode, false)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// The slow path alone, so tests can check the fast ones against it.
    pub fn exactly(format: &Format, negative: bool, decimal: &Decimal, mode: Rounding) -> Encoded {
        exact(format, negative, decimal, mode)
    }

    pub fn bounded(
        format: &Format,
        negative: bool,
        decimal: &Decimal,
        mode: Rounding,
    ) -> Option<Encoded> {
        interval(format, negative, decimal, mode)
    }
}
