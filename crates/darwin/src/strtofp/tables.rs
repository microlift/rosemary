//! Powers of ten as 64-bit significands that never exceed the true value,
//! for the interval path. Any power in range is one exact factor times one
//! coarse factor, which keeps the table to thirty entries.

/// `floor(log2(10^p)) + 1`: the exponent of 10^p with its significand read
/// as a fraction in [1/2, 1).
const fn binary_exponent(p: i32) -> i32 {
    (((p as i64) * 55_732_705) >> 24) as i32 + 1
}

/// 10^0 through 10^27, which are exact in 64 bits because 5^27 is.
const EXACT: [u64; 28] = {
    let mut table = [0; 28];
    let mut five = 1u64;
    let mut i = 0;
    while i < table.len() {
        table[i] = five << five.leading_zeros();
        if i + 1 < table.len() {
            five *= 5;
        }
        i += 1;
    }
    table
};

const STEP: i32 = 28;
const FIRST: i32 = -15;

/// 10^(28k) for k in -15..=14, rounded down.
const COARSE: [u64; 30] = [
    0xdd5a2c3eab3097cb, // 10^-420
    0xdf82365c497b5453, // 10^-392
    0xe1afa13afbd14d6d, // 10^-364
    0xe3e27a444d8d98b7, // 10^-336
    0xe61acf033d1a45df, // 10^-308
    0xe858ad248f5c22c9, // 10^-280
    0xea9c227723ee8bcb, // 10^-252
    0xece53cec4a314ebd, // 10^-224
    0xef340a98172aace4, // 10^-196
    0xf18899b1bc3f8ca1, // 10^-168
    0xf3e2f893dec3f126, // 10^-140
    0xf64335bcf065d37d, // 10^-112
    0xf8a95fcf88747d94, // 10^-84
    0xfb158592be068d2e, // 10^-56
    0xfd87b5f28300ca0d, // 10^-28
    0x8000000000000000, // 10^0
    0x813f3978f8940984, // 10^28
    0x82818f1281ed449f, // 10^56
    0x83c7088e1aab65db, // 10^84
    0x850fadc09923329e, // 10^112
    0x865b86925b9bc5c2, // 10^140
    0x87aa9aff79042286, // 10^168
    0x88fcf317f22241e2, // 10^196
    0x8a5296ffe33cc92f, // 10^224
    0x8bab8eefb6409c1a, // 10^252
    0x8d07e33455637eb2, // 10^280
    0x8e679c2f5e44ff8f, // 10^308
    0x8fcac257558ee4e6, // 10^336
    0x91315e37db165aa9, // 10^364
    0x929b7871de7f22b9, // 10^392
];

/// `(s, e)` with `s * 2^(e-64) <= 10^p < (s + 2) * 2^(e-64)`, or `None` for
/// a power outside the table.
pub(super) fn ten_to_the(p: i32) -> Option<(u64, i32)> {
    let coarse = p.div_euclid(STEP);
    let exact = p.rem_euclid(STEP);
    let c = *COARSE.get(usize::try_from(coarse - FIRST).ok()?)?;
    let x = EXACT[exact as usize];
    let s = ((u128::from(c) * u128::from(x)) >> 64) as u64;
    Some((s, binary_exponent(coarse * STEP) + binary_exponent(exact)))
}

#[cfg(test)]
mod tests {
    use super::super::big::Big;
    use super::*;

    /// Whether `s * 2^(e-64) <= 10^p < (s + width) * 2^(e-64)`, decided
    /// exactly by moving every factor to the side where it is an integer.
    fn brackets(p: i32, s: u64, e: i32, width: u64) -> bool {
        let scaled = |m: u128| {
            let mut x = Big::from_u128(m);
            let mut ten = Big::from_u64(1);
            if p < 0 {
                x.mul_pow5(p.unsigned_abs());
            } else {
                ten.mul_pow5(p as u32);
            }
            let k = e - 64 - p;
            if k >= 0 {
                x.shl(k as u32);
            } else {
                ten.shl(k.unsigned_abs());
            }
            (x, ten)
        };
        let (low, ten) = scaled(u128::from(s));
        let (high, _) = scaled(u128::from(s) + u128::from(width));
        low <= ten && ten < high
    }

    #[test]
    fn the_coarse_powers_are_rounded_down() {
        for (k, &entry) in COARSE.iter().enumerate() {
            let p = (k as i32 + FIRST) * STEP;
            assert!(entry >> 63 == 1, "10^{p} is normalized");
            assert!(brackets(p, entry, binary_exponent(p), 1), "10^{p}");
        }
    }

    #[test]
    fn every_power_in_range_is_bracketed() {
        for p in -420..=419 {
            let (s, e) = ten_to_the(p).unwrap();
            assert!(brackets(p, s, e, 2), "10^{p}");
        }
        assert!(ten_to_the(-421).is_none());
        assert!(ten_to_the(420).is_none());
    }
}
