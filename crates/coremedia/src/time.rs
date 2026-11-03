//! CoreMedia time layout is exactly the 24-byte CMTime aggregate.

#![allow(non_snake_case)]

use core::cmp::Ordering;

pub const VALID: u32 = 1;
pub const HAS_BEEN_ROUNDED: u32 = 2;
pub const POSITIVE_INFINITY: u32 = 4;
pub const NEGATIVE_INFINITY: u32 = 8;
pub const INDEFINITE: u32 = 16;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CMTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}

impl CMTime {
    pub const INVALID: Self = Self {
        value: 0,
        timescale: 0,
        flags: 0,
        epoch: 0,
    };

    pub fn is_numeric(self) -> bool {
        self.flags & VALID != 0
            && self.flags & (POSITIVE_INFINITY | NEGATIVE_INFINITY | INDEFINITE) == 0
            && self.timescale > 0
    }
}

/// Compare finite numeric times exactly without converting to f64.
pub(crate) fn compare_numeric(a: CMTime, b: CMTime) -> Option<Ordering> {
    (a.is_numeric() && b.is_numeric()).then(|| {
        a.epoch.cmp(&b.epoch).then_with(|| {
            (i128::from(a.value) * i128::from(b.timescale))
                .cmp(&(i128::from(b.value) * i128::from(a.timescale)))
        })
    })
}

/// Canonical nonnumeric times have zero value, scale and epoch.
fn special(flags: u32) -> CMTime {
    CMTime {
        flags: VALID | flags,
        ..CMTime::INVALID
    }
}

/// CoreMedia treats infinity and indefinite as meaningful valid durations.
/// Opposing infinities and any invalid time still yield kCMTimeInvalid.
pub(crate) fn add_time(a: CMTime, b: CMTime) -> CMTime {
    if (a.flags & VALID) == 0 || (b.flags & VALID) == 0 {
        return CMTime::INVALID;
    }
    let flags = a.flags | b.flags;
    if flags & POSITIVE_INFINITY != 0 && flags & NEGATIVE_INFINITY != 0 {
        return CMTime::INVALID;
    }
    if flags & POSITIVE_INFINITY != 0 {
        return special(POSITIVE_INFINITY);
    }
    if flags & NEGATIVE_INFINITY != 0 {
        return special(NEGATIVE_INFINITY);
    }
    if flags & INDEFINITE != 0 {
        return special(INDEFINITE);
    }
    if !a.is_numeric() || !b.is_numeric() || (a.epoch != 0 && b.epoch != 0 && a.epoch != b.epoch) {
        return CMTime::INVALID;
    }
    // CoreMedia treats zero epochs as neutral, while adding matching nonzero
    // epochs produces a time in epoch zero.
    let epoch = if a.epoch == 0 {
        b.epoch
    } else if b.epoch == 0 {
        a.epoch
    } else {
        0
    };
    let rounded_flags = (a.flags | b.flags) & HAS_BEEN_ROUNDED;
    // Prefer an exact common timescale. CoreMedia falls back to a nanosecond
    // scale when the LCM is not representable as a CMTime timescale.
    // Continue reducing the scale if the sum's 64-bit numerator would overflow.
    if a.value == 0 {
        return CMTime {
            flags: b.flags | rounded_flags,
            epoch,
            ..b
        };
    }
    if b.value == 0 {
        return CMTime {
            flags: a.flags | rounded_flags,
            epoch,
            ..a
        };
    }
    let gcd = gcd(u64::from(a.timescale as u32), u64::from(b.timescale as u32));
    let lcm = i64::from(a.timescale) / gcd as i64 * i64::from(b.timescale);
    let mut scale = i32::try_from(lcm).unwrap_or(1_000_000_000);
    loop {
        let (lhs, lhs_rounded) = rescale_rounded(a.value, a.timescale, scale);
        let (rhs, rhs_rounded) = rescale_rounded(b.value, b.timescale, scale);
        let sum = lhs + rhs;
        // A native intermediate must itself fit in i64 before the addition;
        // cancellation cannot hide an out-of-range converted operand.
        if let (Ok(_), Ok(_), Ok(value)) =
            (i64::try_from(lhs), i64::try_from(rhs), i64::try_from(sum))
        {
            return CMTime {
                value,
                timescale: scale,
                flags: VALID
                    | ((a.flags | b.flags) & HAS_BEEN_ROUNDED)
                    | if lhs_rounded || rhs_rounded {
                        HAS_BEEN_ROUNDED
                    } else {
                        0
                    },
                epoch,
            };
        }
        if scale == 1 {
            // This is the sign of the exact rational sum, not the rounded sum.
            let exact = i128::from(a.value) * i128::from(b.timescale)
                + i128::from(b.value) * i128::from(a.timescale);
            return special(if exact < 0 {
                NEGATIVE_INFINITY
            } else {
                POSITIVE_INFINITY
            });
        }
        scale = (scale / 2).max(1);
    }
}

pub(crate) fn multiply_time(time: CMTime, count: isize) -> CMTime {
    if time.flags & VALID == 0 || count < 0 {
        return CMTime::INVALID;
    }
    if time.flags & POSITIVE_INFINITY != 0 {
        return special(POSITIVE_INFINITY);
    }
    if time.flags & NEGATIVE_INFINITY != 0 {
        return special(NEGATIVE_INFINITY);
    }
    if time.flags & INDEFINITE != 0 {
        return special(INDEFINITE);
    }
    if !time.is_numeric() {
        return CMTime::INVALID;
    }
    if count == 0 {
        return CMTime { value: 0, ..time };
    }
    let mut scale = time.timescale;
    loop {
        let (rescaled, rounded) = rescale_rounded(time.value, time.timescale, scale);
        if let Ok(value) = i64::try_from(rescaled * count as i128) {
            return CMTime {
                value,
                timescale: scale,
                flags: time.flags | if rounded { HAS_BEEN_ROUNDED } else { 0 },
                epoch: time.epoch,
            };
        }
        if scale == 1 {
            return special(if time.value < 0 {
                NEGATIVE_INFINITY
            } else {
                POSITIVE_INFINITY
            });
        }
        scale = (scale / 2).max(1);
    }
}

/// Rescale with CoreMedia's nearest-integer, ties-away-from-zero rule.
/// Multiplication uses i128 so both 64-bit numerators and 32-bit scales fit.
fn rescale_rounded(value: i64, from: i32, to: i32) -> (i128, bool) {
    let numerator = i128::from(value) * i128::from(to);
    let divisor = i128::from(from);
    let rounded = numerator % divisor != 0;
    let mut quotient = numerator / divisor;
    if (numerator % divisor).abs() * 2 >= divisor {
        quotient += numerator.signum();
    }
    (quotient, rounded)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

pub extern "C" fn CMTimeMake(value: i64, timescale: i32) -> CMTime {
    if timescale <= 0 {
        return CMTime::INVALID;
    }
    CMTime {
        value,
        timescale,
        flags: VALID,
        epoch: 0,
    }
}

pub extern "C" fn CMTimeGetSeconds(time: CMTime) -> f64 {
    if time.flags & VALID == 0 {
        return f64::NAN;
    }
    if time.flags & POSITIVE_INFINITY != 0 {
        return f64::INFINITY;
    }
    if time.flags & NEGATIVE_INFINITY != 0 {
        return f64::NEG_INFINITY;
    }
    if time.flags & INDEFINITE != 0 || time.timescale <= 0 {
        return f64::NAN;
    }
    time.value as f64 / f64::from(time.timescale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_time_math_preserves_order_epoch_and_rejects_overflow() {
        use core::cmp::Ordering;
        assert_eq!(
            compare_numeric(CMTimeMake(1, 3), CMTimeMake(2, 5)),
            Some(Ordering::Less)
        );
        assert_eq!(
            add_time(CMTimeMake(1, 3), CMTimeMake(1, 6)),
            CMTimeMake(3, 6)
        );
        assert_eq!(multiply_time(CMTimeMake(2, 3), 3), CMTimeMake(6, 3));
        assert_eq!(
            multiply_time(CMTimeMake(i64::MAX, 1), 2),
            special(POSITIVE_INFINITY)
        );
        assert_eq!(
            add_time(CMTimeMake(i64::MAX, 1), CMTimeMake(1, 1)),
            special(POSITIVE_INFINITY)
        );
        assert_eq!(
            add_time(CMTimeMake(1, i32::MAX), CMTimeMake(1, i32::MAX - 1)),
            CMTime {
                value: 0,
                timescale: 1_000_000_000,
                flags: VALID | HAS_BEEN_ROUNDED,
                epoch: 0
            }
        );
        let different_epoch = CMTime {
            epoch: 1,
            ..CMTimeMake(1, 2)
        };
        assert_eq!(
            compare_numeric(different_epoch, CMTimeMake(100, 1)),
            Some(Ordering::Greater)
        );
        assert_eq!(
            add_time(different_epoch, CMTimeMake(1, 2)),
            CMTime {
                value: 2,
                timescale: 2,
                flags: VALID,
                epoch: 1
            }
        );
        assert!(
            CMTimeGetSeconds(CMTime {
                timescale: -1,
                ..CMTimeMake(1, 1)
            })
            .is_nan()
        );
    }

    #[test]
    fn native_epoch_and_rounded_zero_addition_semantics() {
        let a = CMTimeMake(1, 2);
        let b = CMTimeMake(1, 3);
        for (ae, be, expected_epoch, valid) in [
            (0, 0, 0, true),
            (0, 7, 7, true),
            (7, 0, 7, true),
            (7, 7, 0, true),
            (7, 8, 0, false),
            (-7, -7, 0, true),
            (0, -7, -7, true),
            (8, -7, 0, false),
        ] {
            let lhs = CMTime { epoch: ae, ..a };
            let rhs = CMTime { epoch: be, ..b };
            let result = add_time(lhs, rhs);
            assert_eq!(
                result,
                if valid {
                    CMTime {
                        value: 5,
                        timescale: 6,
                        flags: VALID,
                        epoch: expected_epoch,
                    }
                } else {
                    CMTime::INVALID
                },
                "epochs {ae}+{be}"
            );
        }
        let rounded_zero = CMTime {
            value: 0,
            timescale: 2,
            flags: VALID | HAS_BEEN_ROUNDED,
            epoch: 7,
        };
        for (left, right) in [(rounded_zero, b), (b, rounded_zero)] {
            assert_eq!(
                add_time(left, right),
                CMTime {
                    value: 1,
                    timescale: 3,
                    flags: VALID | HAS_BEEN_ROUNDED,
                    epoch: 7,
                }
            );
        }
    }

    #[test]
    fn native_intermediate_overflow_reduces_timescale_before_cancellation() {
        let a = CMTimeMake(-5420539530132427298, 7);
        let b = CMTimeMake(2375011267000674195, 2);
        let expected = CMTime {
            value: 2891999904369932385,
            timescale: 7,
            flags: 3,
            epoch: 0,
        };
        assert_eq!(add_time(a, b), expected);
        assert_eq!(
            add_time(
                CMTime {
                    flags: VALID | HAS_BEEN_ROUNDED,
                    ..a
                },
                b
            ),
            expected
        );
    }

    #[test]
    fn large_timescale_additions_match_native_coremedia() {
        let max = i64::MAX;
        let sm = i32::MAX;
        let cases = [
            (
                "coprime max",
                CMTimeMake(1, sm),
                CMTimeMake(1, sm - 1),
                0,
                1_000_000_000,
                3,
            ),
            (
                "half plus third",
                CMTimeMake(1, 2),
                CMTimeMake(1, 3),
                5,
                6,
                1,
            ),
            (
                "max plus one",
                CMTimeMake(max, 1),
                CMTimeMake(1, 1),
                0,
                0,
                5,
            ),
            (
                "negative overflow",
                CMTimeMake(i64::MIN, 1),
                CMTimeMake(-1, 1),
                0,
                0,
                9,
            ),
            (
                "large small",
                CMTimeMake(max, sm),
                CMTimeMake(1, sm - 1),
                4294967298000000000,
                1_000_000_000,
                3,
            ),
            (
                "large odd",
                CMTimeMake(max, 2),
                CMTimeMake(max, 3),
                7686143364045646506,
                1,
                3,
            ),
            (
                "one third plus max",
                CMTimeMake(1, 3),
                CMTimeMake(1, sm),
                333333333,
                1_000_000_000,
                3,
            ),
            (
                "one third exact lcm",
                CMTimeMake(1, 3),
                CMTimeMake(1, sm - 1),
                715827883,
                sm - 1,
                1,
            ),
            (
                "coprime 50000",
                CMTimeMake(1, 50000),
                CMTimeMake(1, 50001),
                40000,
                1_000_000_000,
                3,
            ),
            (
                "max half plus max half",
                CMTimeMake(max, 2),
                CMTimeMake(max, 2),
                0,
                0,
                5,
            ),
            (
                "max third plus max third",
                CMTimeMake(max, 3),
                CMTimeMake(max, 3),
                6148914691236517204,
                1,
                3,
            ),
            (
                "high scale nearly max",
                CMTimeMake(max, sm),
                CMTimeMake(1, sm),
                4611686016279904254,
                1073741823,
                3,
            ),
            (
                "high scale max doubled",
                CMTimeMake(max, sm),
                CMTimeMake(max, sm),
                9223372032559808508,
                1073741823,
                3,
            ),
            (
                "same high scale exact",
                CMTimeMake(1, sm),
                CMTimeMake(1, sm),
                2,
                sm,
                1,
            ),
            (
                "half upward",
                CMTimeMake(1, 2_000_000_000),
                CMTimeMake(1, 1_999_999_999),
                2,
                1_000_000_000,
                3,
            ),
        ];
        for (label, a, b, value, timescale, flags) in cases {
            assert_eq!(
                add_time(a, b),
                CMTime {
                    value,
                    timescale,
                    flags,
                    epoch: 0
                },
                "{label}"
            );
        }
    }

    #[test]
    fn multiplication_overflow_rescales_or_returns_signed_infinity() {
        let max = i64::MAX;
        let sm = i32::MAX;
        let cases = [
            ("positive infinity", CMTimeMake(max, 1), 2, 0, 0, 5),
            ("negative infinity", CMTimeMake(i64::MIN, 1), 2, 0, 0, 9),
            (
                "half overflows after rounding",
                CMTimeMake(max, 2),
                2,
                0,
                0,
                5,
            ),
            (
                "rescale high",
                CMTimeMake(max, sm),
                2,
                9223372032559808508,
                1073741823,
                3,
            ),
            (
                "rescale third",
                CMTimeMake(max, 3),
                3,
                9223372036854775806,
                1,
                3,
            ),
            (
                "rescale nine",
                CMTimeMake(max, sm),
                9,
                5188146734492024814,
                134217727,
                3,
            ),
        ];
        for (label, time, count, value, timescale, flags) in cases {
            assert_eq!(
                multiply_time(time, count),
                CMTime {
                    value,
                    timescale,
                    flags,
                    epoch: 0
                },
                "{label}"
            );
        }
        let already_rounded = CMTime {
            flags: VALID | HAS_BEEN_ROUNDED,
            ..CMTimeMake(1, 30)
        };
        assert_eq!(
            multiply_time(already_rounded, 2).flags,
            VALID | HAS_BEEN_ROUNDED
        );
        assert_eq!(
            add_time(already_rounded, CMTimeMake(1, 30)).flags,
            VALID | HAS_BEEN_ROUNDED
        );
    }

    #[test]
    fn special_durations_follow_native_video_timing_rules() {
        let indefinite = special(INDEFINITE);
        let positive = special(POSITIVE_INFINITY);
        let negative = special(NEGATIVE_INFINITY);
        let numeric = CMTimeMake(1, 30);
        for (time, expected) in [
            (indefinite, indefinite),
            (positive, positive),
            (negative, negative),
            (CMTime::INVALID, CMTime::INVALID),
        ] {
            assert_eq!(multiply_time(time, 1), expected);
            assert_eq!(multiply_time(time, 2), expected);
        }
        for (left, right, expected) in [
            (indefinite, numeric, indefinite),
            (numeric, indefinite, indefinite),
            (positive, indefinite, positive),
            (negative, indefinite, negative),
            (positive, negative, CMTime::INVALID),
            (negative, positive, CMTime::INVALID),
            (CMTime::INVALID, positive, CMTime::INVALID),
            (numeric, CMTime::INVALID, CMTime::INVALID),
            (positive, numeric, positive),
            (numeric, negative, negative),
        ] {
            assert_eq!(add_time(left, right), expected);
        }
    }

    #[test]
    fn time_layout_flags_and_seconds() {
        assert_eq!(size_of::<CMTime>(), 24);
        assert_eq!(CMTimeGetSeconds(CMTimeMake(300, 600)), 0.5);
        assert!(CMTimeGetSeconds(CMTimeMake(42, 0)).is_nan());
        assert!(CMTimeGetSeconds(CMTime::INVALID).is_nan());
        assert_eq!(
            CMTimeGetSeconds(CMTime {
                flags: VALID | POSITIVE_INFINITY,
                ..CMTime::INVALID
            }),
            f64::INFINITY
        );
    }
}

exports::exports! {
    functions {
        "CMTimeMake" => CMTimeMake,
        "CMTimeGetSeconds" => CMTimeGetSeconds,
    }
}
