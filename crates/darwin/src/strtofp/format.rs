//! The target formats, and the one rounding step every parse ends in.
//!
//! Each path - hex, the fast decimal paths, the exact slow path - reduces its
//! input to a [`Binary`]: enough leading bits of the value plus a flag for
//! whether anything below them is nonzero. Rounding that to a format is then
//! a single function, so the subnormal, overflow and `ERANGE` rules exist once
//! rather than once per path.

/// The rounding direction, as `fegetround` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    Nearest,
    Down,
    Up,
    TowardZero,
}

impl Rounding {
    /// The calling thread's rounding mode, read from the register the host's
    /// `fegetround` reads, which here is cheaper than the call.
    pub fn current() -> Rounding {
        #[cfg(target_arch = "x86_64")]
        let (mode, [down, up, toward_zero]) = {
            // glibc and Darwin both answer from the x87 control word.
            let mut word = 0u16;
            // SAFETY: stores the control word and changes nothing.
            unsafe {
                std::arch::asm!("fnstcw [{}]", in(reg) &mut word, options(nostack, preserves_flags))
            };
            (u32::from(word) & 0xc00, [0x400, 0x800, 0xc00])
        };
        #[cfg(target_arch = "aarch64")]
        let (mode, [down, up, toward_zero]) = {
            let fpcr: u64;
            // SAFETY: reads FPCR, which every EL0 thread may.
            unsafe {
                std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags))
            };
            ((fpcr & 0xc0_0000) as u32, [0x80_0000, 0x40_0000, 0xc0_0000])
        };
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        let (mode, [down, up, toward_zero]) = {
            #[link(name = "m")]
            unsafe extern "C" {
                fn fegetround() -> i32;
            }
            // SAFETY: reads the thread's floating-point control state.
            let mode = unsafe { fegetround() } as u32;
            #[cfg(target_arch = "riscv64")]
            const MODES: [u32; 3] = [2, 3, 1];
            (mode, MODES)
        };
        match mode {
            m if m == down => Rounding::Down,
            m if m == up => Rounding::Up,
            m if m == toward_zero => Rounding::TowardZero,
            _ => Rounding::Nearest,
        }
    }

    /// Whether a value that lies strictly between two representable values
    /// goes to the one further from zero, for directed modes.
    pub fn directed_away(self, negative: bool) -> bool {
        match self {
            Rounding::Down => negative,
            Rounding::Up => !negative,
            Rounding::Nearest | Rounding::TowardZero => false,
        }
    }
}

/// A nonzero value `(sig + e) * 2^exp`, where `e` is in (0, 1) when `sticky`
/// and 0 otherwise.
#[derive(Clone, Copy, Debug)]
pub struct Binary {
    pub sig: u128,
    pub exp: i64,
    pub sticky: bool,
}

/// A finished result: the format's bit pattern, and whether the parse is an
/// `ERANGE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Encoded {
    pub bits: u128,
    pub range_error: bool,
}

/// How a format's values are laid out. The exponent bounds follow the
/// convention that a value in `[2^(e-1), 2^e)` has exponent `e`.
#[derive(Debug)]
pub struct Format {
    /// Significand bits, the leading one included whether stored or not.
    pub sig_bits: u32,
    pub min_exp: i64,
    pub max_exp: i64,
    /// Total width, which places the sign bit.
    pub width: u32,
    /// x87 stores its leading significand bit; the IEEE formats do not.
    pub explicit_lead: bool,
    /// Decimal magnitudes outside these are certain to underflow or
    /// overflow, which keeps the exact arithmetic bounded.
    pub min_decimal: i64,
    pub max_decimal: i64,
    /// The most significant digits any exact midpoint between two adjacent
    /// values needs. Digits past these only ever matter as "nonzero or not".
    pub midpoint_digits: usize,
    /// The bits a NaN keeps of its parsed payload, and the bits it then sets.
    /// Darwin keeps whole bytes, so a payload can reach the exponent's low
    /// bits before they are set over it.
    pub nan_kept: u128,
    pub nan_set: u128,
    /// The host type with the same layout, which the hardware fast path needs.
    pub native: Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Native {
    None,
    F32,
    F64,
}

/// No guest entry point takes these two yet; the tests use them to cover the
/// narrowest and widest IEEE layouts.
#[cfg(test)]
pub const BINARY16: Format = Format {
    sig_bits: 11,
    min_exp: -14,
    max_exp: 16,
    width: 16,
    explicit_lead: false,
    min_decimal: -7,
    max_decimal: 5,
    midpoint_digits: 22,
    nan_kept: 0xffff,
    nan_set: 0x7e00,
    native: Native::None,
};

pub const BINARY32: Format = Format {
    sig_bits: 24,
    min_exp: -126,
    max_exp: 128,
    width: 32,
    explicit_lead: false,
    min_decimal: -46,
    max_decimal: 40,
    midpoint_digits: 113,
    nan_kept: 0x00ff_ffff,
    nan_set: 0x7fc0_0000,
    native: Native::F32,
};

pub const BINARY64: Format = Format {
    sig_bits: 53,
    min_exp: -1022,
    max_exp: 1024,
    width: 64,
    explicit_lead: false,
    min_decimal: -325,
    max_decimal: 310,
    midpoint_digits: 768,
    nan_kept: 0x00ff_ffff_ffff_ffff,
    nan_set: 0x7ff8_0000_0000_0000,
    native: Native::F64,
};

pub const FLOAT80: Format = Format {
    sig_bits: 64,
    min_exp: -16382,
    max_exp: 16384,
    width: 80,
    explicit_lead: true,
    min_decimal: -5000,
    max_decimal: 5000,
    midpoint_digits: 11515,
    nan_kept: u64::MAX as u128,
    nan_set: 0x7fff_c000_0000_0000_0000,
    native: Native::None,
};

#[cfg(test)]
pub const BINARY128: Format = Format {
    sig_bits: 113,
    min_exp: -16382,
    max_exp: 16384,
    width: 128,
    explicit_lead: false,
    min_decimal: -5000,
    max_decimal: 5000,
    midpoint_digits: 11564,
    nan_kept: (1 << 112) - 1,
    nan_set: 0x7fff_8000 << 96,
    native: Native::None,
};

impl Format {
    fn sign(&self, negative: bool) -> u128 {
        u128::from(negative) << (self.width - 1)
    }

    /// Bits of the significand below the exponent field.
    fn stored_bits(&self) -> u32 {
        if self.explicit_lead {
            self.sig_bits
        } else {
            self.sig_bits - 1
        }
    }

    fn pack(&self, negative: bool, field: i64, sig: u128) -> u128 {
        let stored = self.stored_bits();
        let mask = (1u128 << stored) - 1;
        self.sign(negative) | ((field as u128) << stored) | (sig & mask)
    }

    pub fn zero(&self, negative: bool) -> u128 {
        self.sign(negative)
    }

    pub fn infinity(&self, negative: bool) -> u128 {
        let lead = if self.explicit_lead {
            1 << (self.sig_bits - 1)
        } else {
            0
        };
        self.pack(negative, self.max_exp - self.min_exp + 1, lead)
    }

    pub fn nan(&self, negative: bool, payload: u128) -> u128 {
        self.sign(negative) | (payload & self.nan_kept) | self.nan_set
    }

    /// A magnitude past the largest finite value: infinity, unless the mode
    /// rounds toward zero from it.
    pub fn overflow(&self, negative: bool, mode: Rounding) -> Encoded {
        let toward_zero = match mode {
            Rounding::Nearest => false,
            Rounding::TowardZero => true,
            Rounding::Down | Rounding::Up => !mode.directed_away(negative),
        };
        let bits = if toward_zero {
            self.pack(negative, self.max_exp - self.min_exp, u128::MAX)
        } else {
            self.infinity(negative)
        };
        Encoded {
            bits,
            range_error: true,
        }
    }

    /// A nonzero magnitude below half the least subnormal: zero, unless the
    /// mode rounds away from it. Always `ERANGE`, so that a caller can tell
    /// `1e-999` from a written zero.
    pub fn underflow(&self, negative: bool, mode: Rounding) -> Encoded {
        Encoded {
            bits: self.sign(negative) | u128::from(mode.directed_away(negative)),
            range_error: true,
        }
    }

    /// Round `value` to this format.
    ///
    /// A subnormal result is an `ERANGE`; `exact_subnormal_ok` exempts the
    /// ones that are exact, which Darwin does for hex input and not for
    /// decimal.
    pub fn round(
        &self,
        negative: bool,
        value: Binary,
        mode: Rounding,
        exact_subnormal_ok: bool,
    ) -> Encoded {
        debug_assert!(value.sig != 0);
        let shift = value.sig.leading_zeros();
        let sig = value.sig << shift;
        // The value is in [2^(top-1), 2^top).
        let mut top = value.exp + 128 - i64::from(shift);
        let sig_bits = i64::from(self.sig_bits);
        if top > self.max_exp {
            return self.overflow(negative, mode);
        }
        if top <= self.min_exp - sig_bits {
            return self.underflow(negative, mode);
        }

        let subnormal = top <= self.min_exp;
        let keep = if subnormal {
            (top - (self.min_exp - sig_bits + 1)) as u32
        } else {
            self.sig_bits
        };
        // `rest` holds the discarded bits, left-aligned.
        let (kept, rest) = match keep {
            0 => (0, sig),
            _ => (sig >> (128 - keep), sig << keep),
        };
        let inexact = rest != 0 || value.sticky;
        let half = 1u128 << 127;
        let up = match mode {
            Rounding::Nearest => rest > half || (rest == half && (value.sticky || kept & 1 == 1)),
            _ => inexact && mode.directed_away(negative),
        };
        let mut kept = kept + u128::from(up);

        let mut range_error = false;
        let field = if subnormal {
            if kept >> (self.sig_bits - 1) != 0 {
                // Rounded up into the least normal.
                1
            } else {
                range_error = inexact || !exact_subnormal_ok;
                0
            }
        } else {
            if kept >> self.sig_bits != 0 {
                kept >>= 1;
                top += 1;
                if top > self.max_exp {
                    return self.overflow(negative, mode);
                }
            }
            top - self.min_exp
        };
        Encoded {
            bits: self.pack(negative, field, kept),
            range_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exact(sig: u128, exp: i64) -> Binary {
        Binary {
            sig,
            exp,
            sticky: false,
        }
    }

    #[test]
    fn special_values_match_the_host_types() {
        assert_eq!(BINARY64.infinity(false), f64::INFINITY.to_bits().into());
        assert_eq!(BINARY32.infinity(true), f32::NEG_INFINITY.to_bits().into());
        assert_eq!(
            BINARY64.overflow(false, Rounding::TowardZero).bits,
            f64::MAX.to_bits().into()
        );
        assert_eq!(FLOAT80.infinity(false), 0x7fff_8000_0000_0000_0000);
        assert_eq!(BINARY128.infinity(true), 0xffff << 112);
        assert_eq!(
            FLOAT80.overflow(true, Rounding::Up).bits,
            0xfffe_ffff_ffff_ffff_ffff
        );
    }

    #[test]
    fn rounds_ties_to_even() {
        // 2^53 + 1 is a tie between 2^53 and 2^53 + 2.
        let tie = BINARY64.round(false, exact((1 << 53) + 1, 0), Rounding::Nearest, false);
        assert_eq!(tie.bits, 9007199254740992f64.to_bits().into());
        let tie = BINARY64.round(false, exact((1 << 53) + 3, 0), Rounding::Nearest, false);
        assert_eq!(tie.bits, 9007199254740996f64.to_bits().into());
        let above = Binary {
            sig: (1 << 53) + 1,
            exp: 0,
            sticky: true,
        };
        let above = BINARY64.round(false, above, Rounding::Nearest, false);
        assert_eq!(above.bits, 9007199254740994f64.to_bits().into());
    }

    #[test]
    fn subnormals_report_range_errors_by_origin() {
        let least = exact(1, -1074);
        let decimal = BINARY64.round(false, least, Rounding::Nearest, false);
        assert_eq!(
            decimal,
            Encoded {
                bits: 1,
                range_error: true
            }
        );
        let hex = BINARY64.round(false, least, Rounding::Nearest, true);
        assert_eq!(
            hex,
            Encoded {
                bits: 1,
                range_error: false
            }
        );
    }

    #[test]
    fn a_subnormal_can_round_up_to_the_least_normal() {
        // Just below 2^-1022.
        let value = Binary {
            sig: (1 << 53) - 1,
            exp: -1022 - 53,
            sticky: false,
        };
        let up = BINARY64.round(false, value, Rounding::Nearest, false);
        assert_eq!(up.bits, f64::MIN_POSITIVE.to_bits().into());
        assert!(!up.range_error);
    }

    #[test]
    fn directed_modes_round_away_only_on_their_side() {
        let third = Binary {
            sig: 0x5555_5555_5555_5555_5555_5555_5555_5555,
            exp: -128,
            sticky: true,
        };
        let down = BINARY32.round(false, third, Rounding::Down, false).bits;
        let up = BINARY32.round(false, third, Rounding::Up, false).bits;
        assert_eq!(up, down + 1);
        let neg_down = BINARY32.round(true, third, Rounding::Down, false).bits;
        let neg_up = BINARY32.round(true, third, Rounding::Up, false).bits;
        assert_eq!(neg_down, neg_up + 1);
    }
}
