use super::format::{BINARY16, BINARY128};
use super::*;
use std::ffi::CStr;

/// A locale with a chosen decimal point and the C locale's spaces.
struct Fixed(&'static [u8]);

impl Conventions for Fixed {
    fn is_space(&self, c: u8) -> bool {
        matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
    }

    fn decimal_point(&self) -> &[u8] {
        self.0
    }
}

const MODES: [Rounding; 4] = [
    Rounding::Nearest,
    Rounding::Down,
    Rounding::Up,
    Rounding::TowardZero,
];

fn run_in(
    format: &Format,
    point: &'static [u8],
    text: &CStr,
    mode: Rounding,
) -> (u128, bool, usize) {
    let (encoded, end) = parse(&Text::from_cstr(text), format, &Fixed(point), || mode);
    (encoded.bits, encoded.range_error, end)
}

fn run(format: &Format, text: &CStr, mode: Rounding) -> (u128, bool, usize) {
    run_in(format, b".", text, mode)
}

/// `strtod` in round-to-nearest: the value, `ERANGE`, and the end offset.
fn double(text: &CStr) -> (f64, bool, usize) {
    double_in(text, Rounding::Nearest)
}

fn double_in(text: &CStr, mode: Rounding) -> (f64, bool, usize) {
    let (bits, range_error, end) = run(&BINARY64, text, mode);
    (f64::from_bits(bits as u64), range_error, end)
}

/// A small deterministic generator, so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// A decimal with a random number of digits, point and exponent, biased
    /// toward the shapes that reach each conversion path.
    fn decimal(&mut self, max_exponent: i64) -> CString {
        let digits = match self.below(4) {
            0 => 1 + self.below(8),
            1 => 15 + self.below(5),
            2 => 19 + self.below(12),
            _ => 1 + self.below(60),
        };
        let mut text = String::new();
        for i in 0..digits {
            let d = if i == 0 {
                1 + self.below(9)
            } else {
                self.below(10)
            };
            text.push(char::from(b'0' + d as u8));
        }
        if self.below(2) == 0 {
            text.insert(1 + self.below(digits) as usize, '.');
        }
        let exponent = self.below(2 * max_exponent as u64 + 1) as i64 - max_exponent;
        text.push_str(&format!("e{exponent}"));
        CString::new(text).unwrap()
    }
}

use std::ffi::CString;

#[test]
fn parses_ordinary_decimals_and_stops_where_darwin_does() {
    assert_eq!(double(c"1.5"), (1.5, false, 3));
    assert_eq!(double(c" \t\n-0.25e1x"), (-2.5, false, 10));
    assert_eq!(double(c"00012"), (12.0, false, 5));
    assert_eq!(double(c"1e"), (1.0, false, 1));
    assert_eq!(double(c"1e+"), (1.0, false, 1));
    assert_eq!(double(c"1e-2"), (0.01, false, 4));
    assert_eq!(double(c".5"), (0.5, false, 2));
    assert_eq!(double(c"5."), (5.0, false, 2));
    assert_eq!(double(c"0.e7"), (0.0, false, 4));
    assert_eq!(double(c"1.e1"), (10.0, false, 4));
    let (zero, range_error, end) = double(c"-0.000");
    assert!(zero == 0.0 && zero.is_sign_negative() && !range_error && end == 6);
}

#[test]
fn rejects_what_is_not_a_number() {
    for text in [
        c"", c".", c"-", c"+-1", c"- 1", c".e1", c"e1", c"in", c"na", c"x",
    ] {
        let (bits, range_error, end) = run(&BINARY64, text, Rounding::Nearest);
        assert_eq!((bits, range_error, end), (0, false, 0), "{text:?}");
    }
    // A sign alone is no number, and the zero it would have signed is +0.
    assert_eq!(run(&BINARY64, c"-.", Rounding::Nearest).0, 0);
}

#[test]
fn reads_infinity_in_either_spelling() {
    assert_eq!(double(c"inf"), (f64::INFINITY, false, 3));
    assert_eq!(double(c"-INFINITY!"), (f64::NEG_INFINITY, false, 9));
    assert_eq!(double(c"Infinit"), (f64::INFINITY, false, 3));
}

#[test]
fn keeps_nan_payloads_as_darwin_does() {
    let nan = |text| {
        let (bits, _, end) = run(&BINARY64, text, Rounding::Nearest);
        (bits as u64, end)
    };
    assert_eq!(nan(c"nan"), (0x7ff8_0000_0000_0000, 3));
    assert_eq!(nan(c"-NaN(0x1234)"), (0xfff8_0000_0000_1234, 12));
    assert_eq!(nan(c"nan(12)"), (0x7ff8_0000_0000_000c, 7));
    assert_eq!(nan(c"nan(010)"), (0x7ff8_0000_0000_0008, 8));
    // Unclosed: the payload is dropped and only "nan" is consumed.
    assert_eq!(nan(c"nan(12"), (0x7ff8_0000_0000_0000, 3));
    // Closed after junk: consumed through the parenthesis, payload dropped.
    assert_eq!(nan(c"nan(1z2)"), (0x7ff8_0000_0000_0000, 8));
    // Payload bits reach into the exponent's low byte before it is set.
    assert_eq!(nan(c"nan(0xffffffffffffffff)"), (0x7fff_ffff_ffff_ffff, 23));
    let (bits, _, _) = run(&BINARY32, c"nan(0x7fffff)", Rounding::Nearest);
    assert_eq!(bits, 0x7fff_ffff);
}

#[test]
fn parses_hex_floats() {
    assert_eq!(double(c"0x1.8p1"), (3.0, false, 7));
    assert_eq!(double(c"-0X10"), (-16.0, false, 5));
    assert_eq!(double(c"0x.8"), (0.5, false, 4));
    assert_eq!(double(c"0x1p"), (1.0, false, 3));
    // "0x" with no digits is the zero before the x.
    assert_eq!(double(c"0x"), (0.0, false, 1));
    assert_eq!(double(c"0x.p1"), (0.0, false, 1));
    assert_eq!(double(c"0x0p99999999999"), (0.0, false, 15));
    // Exact subnormals are not range errors in hex; inexact ones are.
    assert_eq!(double(c"0x1p-1074"), (f64::from_bits(1), false, 9));
    assert_eq!(double(c"0x1.8p-1074"), (f64::from_bits(2), true, 11));
    assert_eq!(double(c"0x1p1024"), (f64::INFINITY, true, 8));
}

#[test]
fn rounds_hex_floats_in_every_mode() {
    let text = c"0x1.fffffffffffff8p0";
    let expect = [
        (Rounding::Nearest, 2.0),
        (Rounding::Down, 1.9999999999999998),
        (Rounding::Up, 2.0),
        (Rounding::TowardZero, 1.9999999999999998),
    ];
    for (mode, value) in expect {
        assert_eq!(double_in(text, mode).0, value, "{mode:?}");
    }
    // Digits past 128 bits still decide a directed rounding.
    let long = c"0x1.0000000000000000000000000000000000000001p0";
    assert_eq!(double_in(long, Rounding::Up).0, 1.0000000000000002);
    assert_eq!(double_in(long, Rounding::Down).0, 1.0);
}

#[test]
fn overflow_and_underflow_follow_the_rounding_mode() {
    assert_eq!(double(c"1e400"), (f64::INFINITY, true, 5));
    assert_eq!(double_in(c"1e400", Rounding::TowardZero).0, f64::MAX);
    assert_eq!(double_in(c"1e400", Rounding::Down).0, f64::MAX);
    assert_eq!(double_in(c"-1e400", Rounding::Down).0, f64::NEG_INFINITY);
    assert_eq!(double_in(c"-1e400", Rounding::Up).0, -f64::MAX);
    assert_eq!(double(c"1e-400"), (0.0, true, 6));
    assert_eq!(
        double_in(c"1e-400", Rounding::Up),
        (f64::from_bits(1), true, 6)
    );
    assert_eq!(double_in(c"-1e-400", Rounding::Down).0, -f64::from_bits(1));
    // Decimal subnormals are range errors even when exact enough to print.
    assert_eq!(double(c"4.9e-324"), (f64::from_bits(1), true, 8));
    assert_eq!(
        double(c"2.2250738585072014e-308"),
        (f64::MIN_POSITIVE, false, 23)
    );
    // The largest finite value and the first decimal that rounds past it.
    assert_eq!(double(c"1.7976931348623157e308").0, f64::MAX);
    assert_eq!(double(c"1.797693134862315807e308").0, f64::MAX);
    assert_eq!(
        double(c"1.797693134862315808e308"),
        (f64::INFINITY, true, 24)
    );
    // An exponent too long to hold saturates rather than wrapping.
    assert_eq!(double(c"1e99999999999999999999"), (f64::INFINITY, true, 22));
    assert_eq!(double(c"1e-99999999999999999999"), (0.0, true, 23));
    assert_eq!(double(c"0e99999999999999999999"), (0.0, false, 22));
}

#[test]
fn every_digit_can_decide_the_rounding() {
    // 2^53 + 1 is a tie, which goes to even...
    assert_eq!(double(c"9007199254740993").0, 9007199254740992.0);
    // ...unless a digit far past any format's precision breaks it.
    let mut text = String::from("9007199254740993.");
    text.extend(std::iter::repeat_n('0', 5000));
    text.push('1');
    let text = CString::new(text).unwrap();
    assert_eq!(double(&text).0, 9007199254740994.0);
    assert_eq!(double(&text).2, text.as_bytes().len());
}

#[test]
fn honors_the_locale_decimal_point() {
    let comma = |text| {
        let (bits, _, end) = run_in(&BINARY64, b",", text, Rounding::Nearest);
        (f64::from_bits(bits as u64), end)
    };
    assert_eq!(comma(c"1,5"), (1.5, 3));
    assert_eq!(comma(c"1.5"), (1.0, 1));
    assert_eq!(comma(c",5"), (0.5, 2));
    assert_eq!(comma(c"0x1,8p1"), (3.0, 7));
    // U+066B, two bytes, matched whole or not at all.
    let arabic = |text| {
        let (bits, _, end) = run_in(&BINARY64, "\u{66b}".as_bytes(), text, Rounding::Nearest);
        (f64::from_bits(bits as u64), end)
    };
    assert_eq!(arabic(c"1\xd9\xab5"), (1.5, 4));
    assert_eq!(arabic(c"\xd9\xab5"), (0.5, 3));
    assert_eq!(arabic(c"1\xd9"), (1.0, 1));
    assert_eq!(arabic(c"\xd9"), (0.0, 0));
}

#[test]
fn matches_rusts_correctly_rounded_parser() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..200_000 {
        let text = rng.decimal(340);
        let expect = text.to_str().unwrap().parse::<f64>().unwrap();
        assert_eq!(double(&text).0.to_bits(), expect.to_bits(), "{text:?}");
        let expect = text.to_str().unwrap().parse::<f32>().unwrap();
        let (bits, _, _) = run(&BINARY32, &text, Rounding::Nearest);
        assert_eq!(bits as u32, expect.to_bits(), "{text:?} as f32");
    }
}

#[test]
fn fast_paths_agree_with_the_exact_path() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for format in [&BINARY16, &BINARY32, &BINARY64] {
        for _ in 0..50_000 {
            let text = rng.decimal(330);
            let Number::Decimal(decimal) = scan::scan(&Text::from_cstr(&text), &Fixed(b".")).number
            else {
                unreachable!("{text:?} is a nonzero decimal");
            };
            let magnitude = decimal.exponent + decimal.count as i64;
            if magnitude < format.min_decimal || magnitude > format.max_decimal {
                continue;
            }
            for mode in MODES {
                for negative in [false, true] {
                    let exact = decimal::tests::exactly(format, negative, &decimal, mode);
                    let full = decimal::convert(format, negative, &decimal, mode);
                    assert_eq!(full, exact, "{text:?} {mode:?} negative={negative}");
                    if let Some(bounded) = decimal::tests::bounded(format, negative, &decimal, mode)
                    {
                        assert_eq!(bounded, exact, "{text:?} {mode:?} interval");
                    }
                }
            }
        }
    }
}

#[test]
fn parses_the_wide_formats() {
    assert_eq!(
        run(&FLOAT80, c"0.1", Rounding::Nearest).0,
        0x3ffb_cccc_cccc_cccc_cccd
    );
    assert_eq!(
        run(&FLOAT80, c"0.1", Rounding::Down).0,
        0x3ffb_cccc_cccc_cccc_cccc
    );
    assert_eq!(
        run(&FLOAT80, c"-1", Rounding::Nearest).0,
        0xbfff_8000_0000_0000_0000
    );
    assert_eq!(
        run(&FLOAT80, c"inf", Rounding::Nearest).0,
        0x7fff_8000_0000_0000_0000
    );
    assert_eq!(
        run(&FLOAT80, c"1e5000", Rounding::Nearest),
        (0x7fff_8000_0000_0000_0000, true, 6)
    );
    // The least x87 subnormal, 2^-16445.
    assert_eq!(
        run(&FLOAT80, c"3.6452e-4951", Rounding::Nearest),
        (1, true, 12)
    );
    // 1e400 is finite here, which it is not as a double.
    assert!(!run(&FLOAT80, c"1e400", Rounding::Nearest).1);
    assert_eq!(
        run(&BINARY128, c"0.1", Rounding::Nearest).0,
        0x3ffb_9999_9999_9999_9999_9999_9999_999a
    );
    assert_eq!(
        run(&BINARY128, c"0x1p-16494", Rounding::Nearest),
        (1, false, 10)
    );
}

#[test]
fn parses_binary16() {
    assert_eq!(
        run(&BINARY16, c"65504", Rounding::Nearest),
        (0x7bff, false, 5)
    );
    // A tie between the largest value and the next, which is infinite.
    assert_eq!(
        run(&BINARY16, c"65520", Rounding::Nearest),
        (0x7c00, true, 5)
    );
    // Below 2^16, so toward zero it is simply the largest value.
    assert_eq!(
        run(&BINARY16, c"65520", Rounding::TowardZero),
        (0x7bff, false, 5)
    );
    assert_eq!(run(&BINARY16, c"6e-8", Rounding::Nearest), (1, true, 4));
    assert_eq!(run(&BINARY16, c"0.333", Rounding::Nearest).0, 0x3554);
}

#[test]
fn the_c_entry_points_store_the_end_and_set_errno() {
    let text = c"  12.5e-1 rest";
    let mut end = std::ptr::null_mut();
    // SAFETY: a NUL-terminated string and a writable end pointer.
    let value = unsafe { strtod(text.as_ptr(), &mut end) };
    assert_eq!(value, 1.25);
    assert_eq!(end as usize - text.as_ptr() as usize, 9);

    crate::errno::set(0);
    // SAFETY: as above, with no end pointer.
    assert_eq!(
        unsafe { strtof(c"1e39".as_ptr(), std::ptr::null_mut()) },
        f32::INFINITY
    );
    assert_eq!(crate::errno::get(), libc::ERANGE);
    crate::errno::set(0);
    // SAFETY: as above; a null locale is Darwin's C locale.
    let value = unsafe { strtod_l(c"0.5".as_ptr(), std::ptr::null_mut(), std::ptr::null_mut()) };
    assert_eq!((value, crate::errno::get()), (0.5, 0));

    // SAFETY: as above.
    let x87 = unsafe { strtold_x87(c"0.1".as_ptr(), std::ptr::null_mut()) };
    assert_eq!(x87, 0x3ffb_cccc_cccc_cccc_cccd_u128.to_le_bytes()[..10]);
}
