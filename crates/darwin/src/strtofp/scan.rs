//! The textual grammar: where a number starts and ends, and what kind it is.
//! Nothing here depends on the target format.

use super::format::Binary;
use std::cell::Cell;
use std::marker::PhantomData;
use std::ops::Range;

/// A NUL-terminated string read on demand.
///
/// `strtod` is routinely called on a pointer into a large buffer, so taking
/// its length first would make a loop over that buffer quadratic. The
/// terminator is instead looked for a window at a time, only as far as the
/// parse reads, and every read is checked against what has been seen - so
/// no lookahead can pass the terminator.
pub(super) struct Text<'a> {
    start: *const u8,
    /// Every byte before this offset is nonzero.
    known: Cell<usize>,
    /// Whether the byte at `known` is the terminator.
    ended: Cell<bool>,
    _bytes: PhantomData<&'a [u8]>,
}

impl<'a> Text<'a> {
    /// How far one look for the terminator goes, which bounds the work a
    /// short number does in a long buffer.
    const WINDOW: usize = 64;

    /// # Safety
    ///
    /// `start` is a NUL-terminated string that outlives `'a`.
    pub unsafe fn new(start: *const u8) -> Text<'a> {
        Text {
            start,
            known: Cell::new(0),
            ended: Cell::new(false),
            _bytes: PhantomData,
        }
    }

    #[cfg(test)]
    pub fn from_cstr(text: &'a std::ffi::CStr) -> Text<'a> {
        // SAFETY: a `CStr` is NUL-terminated and borrowed for `'a`.
        unsafe { Text::new(text.as_ptr().cast()) }
    }

    /// The byte at `at`, or NUL anywhere at or past the terminator.
    #[inline]
    pub fn at(&self, at: usize) -> u8 {
        if at < self.known.get() || (!self.ended.get() && self.reach(at)) {
            // SAFETY: before the first NUL, so inside the caller's string.
            unsafe { *self.start.add(at) }
        } else {
            0
        }
    }

    /// Look for the terminator until `at` is known to be before it.
    #[inline(never)]
    fn reach(&self, at: usize) -> bool {
        let mut known = self.known.get();
        while known <= at && !self.ended.get() {
            // SAFETY: `known` is at most the terminator's offset, and
            // `strnlen` reads no further than the terminator.
            let len = unsafe { libc::strnlen(self.start.add(known).cast(), Self::WINDOW) };
            known += len;
            self.ended.set(len < Self::WINDOW);
        }
        self.known.set(known);
        at < known
    }

    /// The bytes from `from` that `keep` accepts, up to the first it does not.
    pub fn run(&self, from: usize, keep: impl Fn(u8) -> bool) -> &'a [u8] {
        let mut end = from;
        loop {
            let known = self.known.get();
            if end < known {
                let window = self.slice(end..known);
                if let Some(len) = window.iter().position(|&c| !keep(c)) {
                    return self.slice(from..end + len);
                }
                end = known;
            }
            if self.ended.get() || !self.reach(end) {
                return if from < end {
                    self.slice(from..end)
                } else {
                    &[]
                };
            }
        }
    }

    /// Bytes already read, none of them NUL.
    pub fn slice(&self, range: Range<usize>) -> &'a [u8] {
        assert!(range.start <= range.end && range.end <= self.known.get());
        // SAFETY: inside the known prefix, so readable and part of the
        // caller's string.
        unsafe { std::slice::from_raw_parts(self.start.add(range.start), range.len()) }
    }

    fn matches_ignoring_case(&self, at: usize, word: &[u8]) -> bool {
        word.iter()
            .enumerate()
            .all(|(i, &c)| self.at(at + i).eq_ignore_ascii_case(&c))
    }
}

/// What a locale contributes to the grammar.
pub(super) trait Conventions {
    fn is_space(&self, c: u8) -> bool;
    /// The decimal point, which may be several bytes. Asked for only when the
    /// next byte could be one, since a host can be slow to answer.
    fn decimal_point(&self) -> &[u8];
}

pub(super) struct Scanned<'a> {
    pub negative: bool,
    pub number: Number<'a>,
    /// Offset of the first byte not consumed.
    pub end: usize,
}

pub(super) enum Number<'a> {
    /// Not a number at all; the result is +0 and nothing is consumed.
    Invalid,
    Zero,
    Infinity,
    Nan(u128),
    /// A nonzero hex float, exactly or with a sticky bit.
    Hex(Binary),
    Decimal(Decimal<'a>),
}

/// A nonzero decimal significand and its power of ten.
pub(super) struct Decimal<'a> {
    /// From the first nonzero digit to the last digit. It may contain the
    /// decimal point, which readers skip as a non-digit.
    pub digits: &'a [u8],
    /// The number of digits in `digits`.
    pub count: usize,
    /// The first `min(count, 19)` digits.
    pub leading: u64,
    /// The value is `digits * 10^exponent`, reading every digit.
    pub exponent: i64,
}

/// Every digit read for a significand counts; an exponent saturates here,
/// which is far past any format's range.
const EXPONENT_LIMIT: u64 = 99_999_999;

fn digit(c: u8) -> Option<u64> {
    c.is_ascii_digit().then(|| u64::from(c - b'0'))
}

/// Append decimal digits to `value`, wrapping: past 19 digits it is
/// recomputed from the text.
///
/// Eight at a time where it can, which breaks the one-digit-at-a-time
/// multiply chain that otherwise dominates a parse.
fn accumulate(mut value: u64, digits: &[u8]) -> u64 {
    let mut chunks = digits.chunks_exact(8);
    for chunk in &mut chunks {
        let chunk = u64::from_le_bytes(chunk.try_into().unwrap());
        value = value
            .wrapping_mul(100_000_000)
            .wrapping_add(eight_digits(chunk));
    }
    chunks.remainder().iter().fold(value, |value, &c| {
        value.wrapping_mul(10).wrapping_add(u64::from(c - b'0'))
    })
}

/// Eight ASCII digits, first digit in the low byte, as their value: pairs,
/// then quads, then the whole, each step one multiply.
fn eight_digits(chunk: u64) -> u64 {
    const MASK: u64 = 0x0000_00ff_0000_00ff;
    let v = chunk - 0x3030_3030_3030_3030;
    let v = v * 10 + (v >> 8);
    let high = (v & MASK).wrapping_mul(100 + (1_000_000 << 32));
    let low = ((v >> 16) & MASK).wrapping_mul(1 + (10_000 << 32));
    high.wrapping_add(low) >> 32
}

fn hex_digit(c: u8) -> Option<u64> {
    (c as char).to_digit(16).map(u64::from)
}

/// The length of the decimal point at `at`, if there is one.
fn point_at(text: &Text, at: usize, conventions: &impl Conventions) -> Option<usize> {
    let point = conventions.decimal_point();
    let matched = !point.is_empty() && (0..point.len()).all(|i| text.at(at + i) == point[i]);
    matched.then_some(point.len())
}

pub(super) fn scan<'a>(text: &Text<'a>, conventions: &impl Conventions) -> Scanned<'a> {
    let invalid = Scanned {
        negative: false,
        number: Number::Invalid,
        end: 0,
    };
    let mut p = 0;
    let mut negative = false;
    loop {
        match text.at(p) {
            b'0'..=b'9' => break,
            b'-' => {
                negative = true;
                p += 1;
                break;
            }
            b'+' => {
                p += 1;
                break;
            }
            c if c != 0 && (c == b' ' || conventions.is_space(c)) => p += 1,
            _ => break,
        }
    }
    let done = |number, end| Scanned {
        negative,
        number,
        end,
    };

    match text.at(p) {
        b'0' if matches!(text.at(p + 1), b'x' | b'X') => {
            let (number, end) = hex(text, p, conventions);
            return done(number, end);
        }
        b'i' | b'I' if text.matches_ignoring_case(p, b"inf") => {
            let end = if text.matches_ignoring_case(p + 3, b"inity") {
                p + 8
            } else {
                p + 3
            };
            return done(Number::Infinity, end);
        }
        b'n' | b'N' if text.matches_ignoring_case(p, b"nan") => {
            let (payload, end) = nan(text, p + 3);
            return done(Number::Nan(payload), end);
        }
        _ => {}
    }

    let zeros = text.run(p, |c| c == b'0').len();
    p += zeros;
    let mut first = p;
    let integer = text.run(p, |c| c.is_ascii_digit());
    let mut leading = accumulate(0, integer);
    p += integer.len();
    let mut count = integer.len();
    let mut exponent = 0i64;

    let point = if count == 0 && zeros == 0 {
        // With nothing before it, the point is the start of the number and
        // must be followed by a digit.
        match point_at(text, p, conventions) {
            Some(len) if digit(text.at(p + len)).is_some() => Some(len),
            _ => return invalid,
        }
    } else if matches!(text.at(p), 0 | b' ' | b'e' | b'E') {
        None
    } else {
        point_at(text, p, conventions)
    };
    if let Some(len) = point {
        p += len;
        let fraction = p;
        if count == 0 {
            p += text.run(p, |c| c == b'0').len();
            first = p;
        }
        let digits = text.run(p, |c| c.is_ascii_digit());
        leading = accumulate(leading, digits);
        p += digits.len();
        count += digits.len();
        exponent = -((p - fraction) as i64);
    }
    let significand = first..p;

    if matches!(text.at(p), b'e' | b'E') {
        let mut q = p + 1;
        let sign = match text.at(q) {
            b'-' => {
                q += 1;
                -1
            }
            b'+' => {
                q += 1;
                1
            }
            _ => 1,
        };
        let digits = text.run(q, |c| c.is_ascii_digit());
        if !digits.is_empty() {
            let value = digits.iter().fold(0, |value, &c| {
                (value * 10 + u64::from(c - b'0')).min(EXPONENT_LIMIT)
            });
            exponent += sign * value as i64;
            p = q + digits.len();
        }
    }

    if count == 0 {
        return done(Number::Zero, p);
    }
    let digits = text.slice(significand);
    if count > 19 {
        leading = digits
            .iter()
            .filter_map(|&c| digit(c))
            .take(19)
            .fold(0, |acc, d| acc * 10 + d);
    }
    let decimal = Decimal {
        digits,
        count,
        leading,
        exponent,
    };
    done(Number::Decimal(decimal), p)
}

/// A hex float from the `0` of its `0x`.
fn hex<'a>(text: &Text<'a>, start: usize, conventions: &impl Conventions) -> (Number<'a>, usize) {
    // Room to shift in one more digit without losing a bit.
    const FULL: u128 = 1 << 123;
    let mut p = start + 2;
    let mut sig = 0u128;
    let mut exp = 0i64;
    let mut sticky = false;

    let integer = text.run(p, |c| c.is_ascii_hexdigit());
    for d in integer.iter().filter_map(|&c| hex_digit(c)) {
        if sig < FULL {
            sig = (sig << 4) | u128::from(d);
        } else {
            sticky |= d != 0;
            exp += 4;
        }
    }
    p += integer.len();
    let mut digits = integer.len();
    if let Some(len) = point_at(text, p, conventions) {
        p += len;
        let fraction = text.run(p, |c| c.is_ascii_hexdigit());
        for d in fraction.iter().filter_map(|&c| hex_digit(c)) {
            if sig < FULL {
                sig = (sig << 4) | u128::from(d);
                exp -= 4;
            } else {
                sticky |= d != 0;
            }
        }
        p += fraction.len();
        digits += fraction.len();
    }

    if matches!(text.at(p), b'p' | b'P') {
        let mut q = p + 1;
        let negative = match text.at(q) {
            b'-' => {
                q += 1;
                true
            }
            b'+' => {
                q += 1;
                false
            }
            _ => false,
        };
        if digit(text.at(q)).is_some() {
            let mut value = 0i64;
            while let Some(d) = digit(text.at(q)) {
                // Darwin's clamp, which lets one digit past the limit in.
                value = if value > EXPONENT_LIMIT as i64 {
                    EXPONENT_LIMIT as i64
                } else {
                    value * 10 + d as i64
                };
                q += 1;
            }
            exp += if negative { -value } else { value };
            p = q;
        }
    }

    match (sig, digits) {
        // "0x" alone is the number 0 followed by an unparsed "x".
        (0, 0) => (Number::Zero, start + 1),
        (0, _) => (Number::Zero, p),
        _ => (Number::Hex(Binary { sig, exp, sticky }), p),
    }
}

/// A NaN's optional `(payload)`, from just after `nan`. Darwin reads the
/// payload as a C integer literal, modulo 2^128, and ignores one that is
/// not closed by its own parenthesis.
fn nan(text: &Text, after: usize) -> (u128, usize) {
    if text.at(after) != b'(' {
        return (0, after);
    }
    let mut p = after + 1;
    let mut base = 10;
    if text.at(p) == b'0' {
        p += 1;
        if text.at(p) == b'x' {
            base = 16;
            p += 1;
        } else {
            base = 8;
        }
    }
    let mut payload = 0u128;
    while let Some(d) = hex_digit(text.at(p)).filter(|&d| d < base) {
        payload = payload.wrapping_mul(base.into()).wrapping_add(d.into());
        p += 1;
    }
    if text.at(p) == b')' {
        return (payload, p + 1);
    }
    while !matches!(text.at(p), 0 | b')') {
        p += 1;
    }
    match text.at(p) {
        b')' => (0, p + 1),
        _ => (0, after),
    }
}
