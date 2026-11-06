//! `NSDecimalNumber` and `NSDecimalNumberHandler`.
//!
//! An `NSDecimal` is a base-10 mantissa of at most 38 significant digits and
//! an exponent in -128...127. The arithmetic here keeps the mantissa as decimal
//! digits, works exactly, and rounds to 38 digits at the end of each operation,
//! which is the precision Apple's `NSDecimal` functions keep.
//!
//! A decimal number is also an `__NSCFNumber` holding the nearest double, so
//! code that reads it as a plain number - CoreFoundation, a formatter, a plist
//! writer - gets an answer. The exact value lives beside it, keyed by object.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use corefoundation::base::CFTypeRef;
use corefoundation::collections::{self, Number};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// The most significant digits an `NSDecimal` keeps.
const MAX_DIGITS: usize = 38;
const MAX_EXPONENT: i32 = 127;
const MIN_EXPONENT: i32 = -128;
/// `NSDecimalNoScale`.
const NO_SCALE: i16 = i16::MAX;

/// `NSRoundingMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    Plain,
    Down,
    Up,
    Bankers,
}

impl Rounding {
    fn from_raw(raw: usize) -> Rounding {
        match raw {
            1 => Rounding::Down,
            2 => Rounding::Up,
            3 => Rounding::Bankers,
            _ => Rounding::Plain,
        }
    }

    fn raw(self) -> usize {
        match self {
            Rounding::Plain => 0,
            Rounding::Down => 1,
            Rounding::Up => 2,
            Rounding::Bankers => 3,
        }
    }
}

/// `NSCalculationError`, as far as a caller here distinguishes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    LossOfPrecision,
    Underflow,
    Overflow,
    DivideByZero,
}

/// `(-1)^negative * digits * 10^exponent`, or not a number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decimal {
    negative: bool,
    /// Most significant first, no leading zeros; empty is zero.
    digits: Vec<u8>,
    exponent: i32,
    nan: bool,
}

impl Decimal {
    pub fn zero() -> Decimal {
        Decimal {
            negative: false,
            digits: Vec::new(),
            exponent: 0,
            nan: false,
        }
    }

    pub fn nan() -> Decimal {
        Decimal {
            negative: false,
            digits: Vec::new(),
            exponent: 0,
            nan: true,
        }
    }

    pub fn from_parts(mantissa: u64, exponent: i32, negative: bool) -> Decimal {
        let digits = mantissa.to_string().bytes().map(|b| b - b'0').collect();
        Decimal {
            negative,
            digits,
            exponent,
            nan: false,
        }
        .normalized()
    }

    pub fn is_nan(&self) -> bool {
        self.nan
    }

    pub fn is_zero(&self) -> bool {
        !self.nan && self.digits.is_empty()
    }

    /// No leading or trailing zeros, and zero always positive.
    fn normalized(mut self) -> Decimal {
        if self.nan {
            return Decimal::nan();
        }
        let lead = self.digits.iter().take_while(|d| **d == 0).count();
        self.digits.drain(..lead);
        while self.digits.last() == Some(&0) {
            self.digits.pop();
            self.exponent += 1;
        }
        if self.digits.is_empty() {
            return Decimal::zero();
        }
        self
    }

    /// Drop `count` trailing digits, rounding what remains by `mode`.
    fn drop_digits(&mut self, count: usize, mode: Rounding) -> bool {
        if count == 0 {
            return false;
        }
        let keep = self.digits.len().saturating_sub(count);
        let dropped: Vec<u8> = self.digits.split_off(keep);
        self.exponent += count as i32;
        let first = dropped.first().copied().unwrap_or(0);
        let rest_nonzero = dropped.iter().skip(1).any(|d| *d != 0);
        let any = first != 0 || rest_nonzero;
        let last_odd = self.digits.last().is_some_and(|d| d % 2 == 1);
        let up = match mode {
            Rounding::Plain => first >= 5,
            Rounding::Bankers => first > 5 || (first == 5 && (rest_nonzero || last_odd)),
            // Towards negative infinity, and towards positive infinity.
            Rounding::Down => any && self.negative,
            Rounding::Up => any && !self.negative,
        };
        if up {
            increment(&mut self.digits);
        }
        any
    }

    /// Fit the result of an operation into `NSDecimal`'s range.
    fn fit(mut self, mode: Rounding) -> Result<Decimal, (Error, Decimal)> {
        self = self.normalized();
        if self.nan || self.digits.is_empty() {
            return Ok(self);
        }
        let mut lost = false;
        if self.digits.len() > MAX_DIGITS {
            lost = self.drop_digits(self.digits.len() - MAX_DIGITS, mode);
            self = self.normalized();
        }
        if self.exponent < MIN_EXPONENT {
            let excess = (MIN_EXPONENT - self.exponent) as usize;
            if excess > self.digits.len() {
                return Err((Error::Underflow, Decimal::zero()));
            }
            lost |= self.drop_digits(excess, mode);
            self = self.normalized();
            if self.digits.is_empty() {
                return Err((Error::Underflow, Decimal::zero()));
            }
        }
        if self.exponent > MAX_EXPONENT {
            // Room left in the mantissa takes up what the exponent cannot.
            let excess = (self.exponent - MAX_EXPONENT) as usize;
            if self.digits.len() + excess > MAX_DIGITS {
                return Err((Error::Overflow, Decimal::nan()));
            }
            self.digits.extend(std::iter::repeat_n(0, excess));
            self.exponent = MAX_EXPONENT;
        }
        match lost {
            true => Err((Error::LossOfPrecision, self)),
            false => Ok(self),
        }
    }

    pub fn add(&self, other: &Decimal, mode: Rounding) -> Result<Decimal, (Error, Decimal)> {
        if self.nan || other.nan {
            return Ok(Decimal::nan());
        }
        let exponent = self.exponent.min(other.exponent);
        let scaled = |d: &Decimal| {
            let mut digits = d.digits.clone();
            digits.extend(std::iter::repeat_n(0, (d.exponent - exponent) as usize));
            digits
        };
        let (a, b) = (scaled(self), scaled(other));
        let (negative, digits) = match self.negative == other.negative {
            true => (self.negative, add_digits(&a, &b)),
            false => match compare_digits(&a, &b) {
                std::cmp::Ordering::Less => (other.negative, sub_digits(&b, &a)),
                _ => (self.negative, sub_digits(&a, &b)),
            },
        };
        Decimal {
            negative,
            digits,
            exponent,
            nan: false,
        }
        .fit(mode)
    }

    pub fn negated(&self) -> Decimal {
        let mut negated = self.clone();
        if !negated.is_zero() {
            negated.negative = !negated.negative;
        }
        negated
    }

    pub fn multiply(&self, other: &Decimal, mode: Rounding) -> Result<Decimal, (Error, Decimal)> {
        if self.nan || other.nan {
            return Ok(Decimal::nan());
        }
        Decimal {
            negative: self.negative != other.negative,
            digits: mul_digits(&self.digits, &other.digits),
            exponent: self.exponent + other.exponent,
            nan: false,
        }
        .fit(mode)
    }

    pub fn divide(&self, other: &Decimal, mode: Rounding) -> Result<Decimal, (Error, Decimal)> {
        if self.nan || other.nan {
            return Ok(Decimal::nan());
        }
        if other.is_zero() {
            return Err((Error::DivideByZero, Decimal::nan()));
        }
        if self.is_zero() {
            return Ok(Decimal::zero());
        }
        // Enough zeros on the dividend that the quotient has a digit past the
        // last one kept, so rounding sees it.
        let extra = (MAX_DIGITS + 2 + other.digits.len()).saturating_sub(self.digits.len());
        let mut dividend = self.digits.clone();
        dividend.extend(std::iter::repeat_n(0, extra));
        let (mut quotient, remainder) = div_digits(&dividend, &other.digits);
        // A remainder is a nonzero tail past the last digit: one more digit
        // that can only round, never be mistaken for an exact half.
        if !remainder.is_empty() {
            quotient.push(1);
        }
        let shift = if remainder.is_empty() { 0 } else { 1 };
        Decimal {
            negative: self.negative != other.negative,
            digits: quotient,
            exponent: self.exponent - other.exponent - extra as i32 - shift,
            nan: false,
        }
        .fit(mode)
    }

    pub fn power(&self, power: usize, mode: Rounding) -> Result<Decimal, (Error, Decimal)> {
        if self.nan {
            return Ok(Decimal::nan());
        }
        let mut result = Decimal::from_parts(1, 0, false);
        let mut base = self.clone();
        let mut power = power;
        let mut error = None;
        while power > 0 {
            if power & 1 == 1 {
                result = keep(result.multiply(&base, mode), &mut error)?;
            }
            power >>= 1;
            if power > 0 {
                base = keep(base.multiply(&base, mode), &mut error)?;
            }
        }
        match error {
            Some(error) => Err((error, result)),
            None => Ok(result),
        }
    }

    pub fn multiply_by_power_of_10(
        &self,
        power: i16,
        mode: Rounding,
    ) -> Result<Decimal, (Error, Decimal)> {
        if self.nan || self.is_zero() {
            return Ok(self.clone());
        }
        let mut shifted = self.clone();
        shifted.exponent += i32::from(power);
        shifted.fit(mode)
    }

    /// `NSDecimalRound`: at most `scale` digits after the decimal point.
    pub fn round(&self, scale: i16, mode: Rounding) -> Decimal {
        if self.nan || scale == NO_SCALE || self.exponent >= -i32::from(scale) {
            return self.clone();
        }
        let mut rounded = self.clone();
        let count = (-i32::from(scale) - rounded.exponent) as usize;
        if count > rounded.digits.len() {
            // Every digit is past the scale: what is left is zero or one unit.
            rounded.digits.insert(0, 0);
        }
        let count = count.min(rounded.digits.len());
        rounded.drop_digits(count, mode);
        rounded.normalized()
    }

    pub fn compare(&self, other: &Decimal) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        // Apple orders NaN below everything, and equal to itself.
        match (self.nan, other.nan) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        match self.add(&other.negated(), Rounding::Plain) {
            Ok(difference) | Err((_, difference)) => match difference.is_zero() {
                true => Ordering::Equal,
                false if difference.negative => Ordering::Less,
                false => Ordering::Greater,
            },
        }
    }

    /// Apple's `-description`: every digit, a point where one belongs, never
    /// an exponent.
    pub fn to_string_with(&self, separator: &str) -> String {
        if self.nan {
            return "NaN".to_string();
        }
        if self.digits.is_empty() {
            return "0".to_string();
        }
        let digits: String = self.digits.iter().map(|d| char::from(b'0' + d)).collect();
        let mut out = String::new();
        if self.negative {
            out.push('-');
        }
        if self.exponent >= 0 {
            out.push_str(&digits);
            out.extend(std::iter::repeat_n('0', self.exponent as usize));
        } else {
            let fraction = (-self.exponent) as usize;
            if digits.len() > fraction {
                let (whole, part) = digits.split_at(digits.len() - fraction);
                out.push_str(whole);
                out.push_str(separator);
                out.push_str(part);
            } else {
                out.push('0');
                out.push_str(separator);
                out.extend(std::iter::repeat_n('0', fraction - digits.len()));
                out.push_str(&digits);
            }
        }
        out
    }

    /// `+decimalNumberWithString:`: leading whitespace, a sign, digits with at
    /// most one `separator`, and an exponent, reading as far as that goes.
    /// Not a number when there are no digits at all.
    pub fn parse(text: &str, separator: &str) -> Decimal {
        let text = text.trim_start();
        let mut rest = text;
        let mut negative = false;
        if let Some(r) = rest.strip_prefix('-') {
            negative = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('+') {
            rest = r;
        }
        let mut digits = Vec::new();
        let mut exponent: i64 = 0;
        let mut seen_digit = false;
        let mut seen_separator = false;
        loop {
            if let Some(c) = rest.chars().next()
                && c.is_ascii_digit()
            {
                digits.push(c as u8 - b'0');
                if seen_separator {
                    exponent -= 1;
                }
                seen_digit = true;
                rest = &rest[1..];
                continue;
            }
            if !seen_separator
                && !separator.is_empty()
                && let Some(r) = rest.strip_prefix(separator)
            {
                seen_separator = true;
                rest = r;
                continue;
            }
            break;
        }
        if !seen_digit {
            return Decimal::nan();
        }
        if let Some(r) = rest.strip_prefix(['e', 'E']) {
            let (sign, r) = match r.strip_prefix('-') {
                Some(r) => (-1, r),
                None => (1, r.strip_prefix('+').unwrap_or(r)),
            };
            let written: String = r.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(value) = written.parse::<i64>() {
                exponent += sign * value;
            }
        }
        let exponent = exponent.clamp(-100_000, 100_000) as i32;
        let parsed = Decimal {
            negative,
            digits,
            exponent,
            nan: false,
        };
        match parsed.fit(Rounding::Plain) {
            Ok(value) => value,
            Err((Error::LossOfPrecision, value)) => value,
            Err((_, value)) => value,
        }
    }

    pub fn to_f64(&self) -> f64 {
        match self.nan {
            true => f64::NAN,
            false => {
                let digits: String = self.digits.iter().map(|d| char::from(b'0' + d)).collect();
                let text = format!(
                    "{}{}e{}",
                    if self.negative { "-" } else { "" },
                    if digits.is_empty() { "0" } else { &digits },
                    self.exponent
                );
                text.parse().unwrap_or(f64::NAN)
            }
        }
    }

    /// The nearest decimal to `value`, by its shortest round-trip spelling.
    pub fn from_f64(value: f64) -> Decimal {
        match value.is_finite() {
            true => Decimal::parse(&format!("{value:e}"), "."),
            false => Decimal::nan(),
        }
    }

    pub fn to_i64(&self) -> i64 {
        let truncated = self.round(
            0,
            if self.negative {
                Rounding::Up
            } else {
                Rounding::Down
            },
        );
        let text = truncated.to_string_with(".");
        text.parse::<i64>().unwrap_or_else(|_| self.to_f64() as i64)
    }

    /// The mantissa, exponent and sign for an `NSDecimal` struct: the mantissa
    /// is at most 38 digits, so it fits 128 bits.
    fn raw_parts(&self) -> (u128, i32, bool) {
        let mantissa = self
            .digits
            .iter()
            .fold(0u128, |m, d| m * 10 + u128::from(*d));
        (mantissa, self.exponent, self.negative)
    }
}

fn keep(
    result: Result<Decimal, (Error, Decimal)>,
    error: &mut Option<Error>,
) -> Result<Decimal, (Error, Decimal)> {
    match result {
        Ok(value) => Ok(value),
        Err((Error::LossOfPrecision, value)) => {
            error.get_or_insert(Error::LossOfPrecision);
            Ok(value)
        }
        Err(other) => Err(other),
    }
}

fn increment(digits: &mut Vec<u8>) {
    for digit in digits.iter_mut().rev() {
        if *digit == 9 {
            *digit = 0;
        } else {
            *digit += 1;
            return;
        }
    }
    digits.insert(0, 1);
}

fn compare_digits(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let strip = |d: &[u8]| -> usize { d.iter().take_while(|x| **x == 0).count() };
    let (a, b) = (&a[strip(a)..], &b[strip(b)..]);
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn add_digits(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let (mut i, mut j, mut carry) = (a.len(), b.len(), 0u8);
    while i > 0 || j > 0 || carry > 0 {
        let x = if i > 0 {
            i -= 1;
            a[i]
        } else {
            0
        };
        let y = if j > 0 {
            j -= 1;
            b[j]
        } else {
            0
        };
        let sum = x + y + carry;
        out.push(sum % 10);
        carry = sum / 10;
    }
    out.reverse();
    out
}

/// `a - b`, for `a >= b`.
fn sub_digits(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len());
    let (mut i, mut j, mut borrow) = (a.len(), b.len(), 0i8);
    while i > 0 {
        i -= 1;
        let y = if j > 0 {
            j -= 1;
            b[j] as i8
        } else {
            0
        };
        let mut diff = a[i] as i8 - y - borrow;
        borrow = 0;
        if diff < 0 {
            diff += 10;
            borrow = 1;
        }
        out.push(diff as u8);
    }
    out.reverse();
    let lead = out.iter().take_while(|d| **d == 0).count();
    out.drain(..lead);
    out
}

fn mul_digits(a: &[u8], b: &[u8]) -> Vec<u8> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, x) in a.iter().enumerate().rev() {
        for (j, y) in b.iter().enumerate().rev() {
            out[i + j + 1] += u32::from(*x) * u32::from(*y);
        }
    }
    for k in (1..out.len()).rev() {
        let carry = out[k] / 10;
        out[k] %= 10;
        out[k - 1] += carry;
    }
    let mut digits: Vec<u8> = out.into_iter().map(|d| d as u8).collect();
    let lead = digits.iter().take_while(|d| **d == 0).count();
    digits.drain(..lead);
    digits
}

/// Long division: `(quotient, remainder)` of two nonzero digit strings.
fn div_digits(a: &[u8], b: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut quotient = Vec::with_capacity(a.len());
    let mut remainder: Vec<u8> = Vec::new();
    for digit in a {
        remainder.push(*digit);
        let lead = remainder.iter().take_while(|d| **d == 0).count();
        remainder.drain(..lead);
        let mut count = 0;
        while compare_digits(&remainder, b) != std::cmp::Ordering::Less {
            remainder = sub_digits(&remainder, b);
            count += 1;
        }
        quotient.push(count);
    }
    let lead = quotient.iter().take_while(|d| **d == 0).count();
    quotient.drain(..lead);
    (quotient, remainder)
}

/// What an `NSDecimalNumberHandler` decides: how to round, to what scale,
/// and which errors raise.
#[derive(Clone, Copy, Debug)]
struct Behavior {
    mode: Rounding,
    scale: i16,
    raise_exactness: bool,
    raise_overflow: bool,
    raise_underflow: bool,
    raise_divide_by_zero: bool,
}

/// `+defaultDecimalNumberHandler`: plain rounding, no scale, and every error
/// but lost precision raised.
const DEFAULT_BEHAVIOR: Behavior = Behavior {
    mode: Rounding::Plain,
    scale: NO_SCALE,
    raise_exactness: false,
    raise_overflow: true,
    raise_underflow: true,
    raise_divide_by_zero: true,
};

fn exact_values() -> &'static Mutex<HashMap<usize, Decimal>> {
    static VALUES: OnceLock<Mutex<HashMap<usize, Decimal>>> = OnceLock::new();
    VALUES.get_or_init(Default::default)
}

fn handlers() -> &'static Mutex<HashMap<usize, Behavior>> {
    static HANDLERS: OnceLock<Mutex<HashMap<usize, Behavior>>> = OnceLock::new();
    HANDLERS.get_or_init(Default::default)
}

/// The behavior `+setDefaultBehavior:` installed, as the handler object.
static DEFAULT_HANDLER: Mutex<usize> = Mutex::new(0);

fn decimal_class() -> &'static objc::class::Class {
    objc::class::lookup(c"NSDecimalNumber").expect("registered in install")
}

fn handler_class() -> &'static objc::class::Class {
    objc::class::lookup(c"NSDecimalNumberHandler").expect("registered in install")
}

/// The exact value of a decimal number, or of any other number by its value.
///
/// # Safety
///
/// `object` is null or an object this runtime allocated.
pub unsafe fn value_of(object: Id) -> Option<Decimal> {
    if object.is_null() {
        return None;
    }
    if let Some(value) = exact_values()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(object as usize))
    {
        return Some(value.clone());
    }
    // SAFETY: the caller's obligation.
    match unsafe { collections::number(object as CFTypeRef) }? {
        Number::Integer(value) => Some(Decimal::parse(&value.to_string(), ".")),
        Number::Float(value) => Some(Decimal::from_f64(value)),
    }
}

/// A new decimal number holding `value`, owned by the caller.
pub fn make(value: Decimal) -> Id {
    let object = collections::new_number(Number::Float(value.to_f64())) as Id;
    // SAFETY: a fresh `__NSCFNumber`, which `NSDecimalNumber` subclasses with
    // no storage of its own.
    unsafe {
        objc::api::object_setClass(object, objc::class::ptr(decimal_class()) as *const _);
    }
    exact_values()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(object as usize, value);
    object
}

fn autoreleased(value: Decimal) -> Id {
    objc::arc::autorelease(make(value))
}

/// `+notANumber`, one object for the process.
fn not_a_number() -> Id {
    static NAN: OnceLock<usize> = OnceLock::new();
    *NAN.get_or_init(|| make(Decimal::nan()) as usize) as Id
}

unsafe extern "C" fn dealloc(receiver: Id, sel: Sel) {
    exact_values()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(receiver as usize));
    // `__NSCFNumber`'s own, which frees the double beside the value.
    let parent = collections::number_class();
    let imp = objc::class::method_implementation(parent, sel);
    // SAFETY: `-dealloc` is `void (id, SEL)`, and the receiver is an
    // `__NSCFNumber` underneath.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(imp);
        f(receiver, sel);
    }
}

fn behavior_of(handler: Id) -> Behavior {
    let handlers = handlers().lock().unwrap_or_else(|e| e.into_inner());
    let chosen = match handler.is_null() {
        true => *DEFAULT_HANDLER.lock().unwrap_or_else(|e| e.into_inner()),
        false => handler as usize,
    };
    handlers.get(&chosen).copied().unwrap_or(DEFAULT_BEHAVIOR)
}

/// Apply a handler to an operation's result: round to its scale, and either
/// answer `+notANumber` for an error or fail where it would raise.
///
/// FIXME (ACCURACY): an error the handler raises for aborts here instead:
/// there is no frame to throw an `NSException` into from this runtime.
fn finish(result: Result<Decimal, (Error, Decimal)>, behavior: Behavior, what: &str) -> Id {
    let value = match result {
        Ok(value) => value,
        Err((error, value)) => {
            let (raises, name) = match error {
                Error::LossOfPrecision => (
                    behavior.raise_exactness,
                    "NSDecimalNumberExactnessException",
                ),
                Error::Underflow => (
                    behavior.raise_underflow,
                    "NSDecimalNumberUnderflowException",
                ),
                Error::Overflow => (behavior.raise_overflow, "NSDecimalNumberOverflowException"),
                Error::DivideByZero => (
                    behavior.raise_divide_by_zero,
                    "NSDecimalNumberDivideByZeroException",
                ),
            };
            if raises {
                objc::fail!(
                    "-[NSDecimalNumber {what}] raised {name}, which this runtime cannot throw"
                );
            }
            match error {
                Error::LossOfPrecision => value,
                _ => return not_a_number(),
            }
        }
    };
    autoreleased(value.round(behavior.scale, behavior.mode))
}

/// The receiver and argument of a binary operation, each as a decimal.
unsafe fn operands(receiver: Id, other: Id) -> (Decimal, Decimal) {
    // SAFETY: the caller's obligation; nil is not a number.
    unsafe {
        (
            value_of(receiver).unwrap_or_else(Decimal::nan),
            value_of(other).unwrap_or_else(Decimal::nan),
        )
    }
}

macro_rules! binary {
    ($($name:ident, $with:ident => $op:ident, $what:literal;)*) => {$(
        unsafe extern "C" fn $with(receiver: Id, _sel: Sel, other: Id, handler: Id) -> Id {
            // SAFETY: two numbers or nil.
            let (a, b) = unsafe { operands(receiver, other) };
            let behavior = behavior_of(handler);
            finish(a.$op(&b, behavior.mode), behavior, $what)
        }

        unsafe extern "C" fn $name(receiver: Id, sel: Sel, other: Id) -> Id {
            // SAFETY: the same operands, with the default behavior.
            unsafe { $with(receiver, sel, other, std::ptr::null_mut()) }
        }
    )*};
}

binary! {
    adding, adding_with_behavior => add, "decimalNumberByAdding:";
    multiplying, multiplying_with_behavior => multiply, "decimalNumberByMultiplyingBy:";
    dividing, dividing_with_behavior => divide, "decimalNumberByDividingBy:";
}

unsafe extern "C" fn subtracting_with_behavior(
    receiver: Id,
    _sel: Sel,
    other: Id,
    handler: Id,
) -> Id {
    // SAFETY: two numbers or nil.
    let (a, b) = unsafe { operands(receiver, other) };
    let behavior = behavior_of(handler);
    finish(
        a.add(&b.negated(), behavior.mode),
        behavior,
        "decimalNumberBySubtracting:",
    )
}

unsafe extern "C" fn subtracting(receiver: Id, sel: Sel, other: Id) -> Id {
    // SAFETY: as above.
    unsafe { subtracting_with_behavior(receiver, sel, other, std::ptr::null_mut()) }
}

unsafe extern "C" fn raising_with_behavior(
    receiver: Id,
    _sel: Sel,
    power: usize,
    handler: Id,
) -> Id {
    // SAFETY: a number.
    let value = unsafe { value_of(receiver) }.unwrap_or_else(Decimal::nan);
    let behavior = behavior_of(handler);
    finish(
        value.power(power, behavior.mode),
        behavior,
        "decimalNumberByRaisingToPower:",
    )
}

unsafe extern "C" fn raising(receiver: Id, sel: Sel, power: usize) -> Id {
    // SAFETY: as above.
    unsafe { raising_with_behavior(receiver, sel, power, std::ptr::null_mut()) }
}

unsafe extern "C" fn power_of_10_with_behavior(
    receiver: Id,
    _sel: Sel,
    power: i16,
    handler: Id,
) -> Id {
    // SAFETY: a number.
    let value = unsafe { value_of(receiver) }.unwrap_or_else(Decimal::nan);
    let behavior = behavior_of(handler);
    finish(
        value.multiply_by_power_of_10(power, behavior.mode),
        behavior,
        "decimalNumberByMultiplyingByPowerOf10:",
    )
}

unsafe extern "C" fn power_of_10(receiver: Id, sel: Sel, power: i16) -> Id {
    // SAFETY: as above.
    unsafe { power_of_10_with_behavior(receiver, sel, power, std::ptr::null_mut()) }
}

unsafe extern "C" fn rounding(receiver: Id, _sel: Sel, handler: Id) -> Id {
    // SAFETY: a number.
    let value = unsafe { value_of(receiver) }.unwrap_or_else(Decimal::nan);
    let behavior = behavior_of(handler);
    autoreleased(value.round(behavior.scale, behavior.mode))
}

/// `-compare:`, which any two numbers answer by value.
unsafe extern "C" fn compare(receiver: Id, _sel: Sel, other: Id) -> isize {
    // SAFETY: two numbers or nil.
    let (a, b) = unsafe { operands(receiver, other) };
    match a.compare(&b) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

unsafe extern "C" fn is_equal_to_number(receiver: Id, sel: Sel, other: Id) -> u8 {
    if other.is_null() {
        return 0;
    }
    // SAFETY: as `-compare:`.
    u8::from(unsafe { compare(receiver, sel, other) } == 0)
}

unsafe extern "C" fn is_equal(receiver: Id, sel: Sel, other: Id) -> u8 {
    // SAFETY: anything; a non-number has no value and is not equal.
    match unsafe { value_of(other) } {
        Some(_) => unsafe { is_equal_to_number(receiver, sel, other) },
        None => 0,
    }
}

unsafe extern "C" fn hash(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: a number. It hashes as an `NSNumber` of its value, which it is
    // `-isEqual:` to.
    let value = unsafe { value_of(receiver) }.unwrap_or_else(Decimal::nan);
    crate::collections::number_hash_of(Number::Float(value.to_f64()))
}

/// The decimal separator a locale writes, `.` for nil or a locale that has
/// none on record. Accepts a dictionary too, which is what older code passes.
fn separator_of(locale: Id) -> String {
    if locale.is_null() {
        return ".".to_string();
    }
    let key = crate::string::from_str("NSDecimalSeparator");
    // SAFETY: `-objectForKey:` is `id (id, SEL, id)` on a locale and on a
    // dictionary alike.
    let separator = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(locale, objc::sel::get("objectForKey:"), key)
    };
    objc::arc::release(key);
    // SAFETY: a string, or nil.
    let text = String::from_utf8(unsafe { crate::string::bytes(separator) }).unwrap_or_default();
    if text.is_empty() {
        ".".to_string()
    } else {
        text
    }
}

unsafe extern "C" fn description_with_locale(receiver: Id, _sel: Sel, locale: Id) -> Id {
    // SAFETY: a number.
    let value = unsafe { value_of(receiver) }.unwrap_or_else(Decimal::nan);
    let text = crate::string::from_str(&value.to_string_with(&separator_of(locale)));
    objc::arc::autorelease(text)
}

unsafe extern "C" fn description(receiver: Id, sel: Sel) -> Id {
    // SAFETY: the same receiver, with no locale.
    unsafe { description_with_locale(receiver, sel, std::ptr::null_mut()) }
}

unsafe extern "C" fn double_value(receiver: Id, _sel: Sel) -> f64 {
    // SAFETY: a number.
    unsafe { value_of(receiver) }.map_or(f64::NAN, |v| v.to_f64())
}

unsafe extern "C" fn float_value(receiver: Id, sel: Sel) -> f32 {
    // SAFETY: as above.
    unsafe { double_value(receiver, sel) as f32 }
}

unsafe extern "C" fn integer_value(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: a number.
    unsafe { value_of(receiver) }.map_or(0, |v| v.to_i64())
}

unsafe extern "C" fn int_value(receiver: Id, sel: Sel) -> i32 {
    // SAFETY: as above.
    unsafe { integer_value(receiver, sel) as i32 }
}

unsafe extern "C" fn bool_value(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a number.
    u8::from(unsafe { value_of(receiver) }.is_some_and(|v| !v.is_zero()))
}

/// `-objCType`: `d`, as Apple's answers.
unsafe extern "C" fn objc_type(_receiver: Id, _sel: Sel) -> *const std::ffi::c_char {
    c"d".as_ptr()
}

/// `NSDecimal`, as Foundation lays it out: the exponent, then length,
/// isNegative, isCompact and reserved bits, then eight 16-bit mantissa words,
/// least significant first.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NSDecimal {
    exponent: i8,
    flags: [u8; 3],
    mantissa: [u16; 8],
}

impl NSDecimal {
    fn from_decimal(value: &Decimal) -> NSDecimal {
        if value.nan {
            // Length 0 and isNegative is Apple's NaN.
            return NSDecimal {
                exponent: 0,
                flags: [0x10, 0, 0],
                mantissa: [0; 8],
            };
        }
        let (mut mantissa, exponent, negative) = value.raw_parts();
        let mut words = [0u16; 8];
        let mut length = 0u8;
        for (i, word) in words.iter_mut().enumerate() {
            *word = (mantissa & 0xFFFF) as u16;
            mantissa >>= 16;
            if *word != 0 {
                length = i as u8 + 1;
            }
        }
        let flags = length | (u8::from(negative) << 4) | (1 << 5);
        NSDecimal {
            exponent: exponent as i8,
            flags: [flags, 0, 0],
            mantissa: words,
        }
    }

    fn to_decimal(self) -> Decimal {
        let length = usize::from(self.flags[0] & 0x0F).min(8);
        let negative = self.flags[0] & 0x10 != 0;
        if length == 0 {
            return match negative {
                true => Decimal::nan(),
                false => Decimal::zero(),
            };
        }
        let mantissa = self.mantissa[..length]
            .iter()
            .rev()
            .fold(0u128, |m, w| (m << 16) | u128::from(*w));
        Decimal::parse(
            &format!(
                "{}{mantissa}e{}",
                if negative { "-" } else { "" },
                self.exponent
            ),
            ".",
        )
    }
}

/// `-decimalValue`, returned through the caller's buffer as the struct is
/// twenty bytes.
unsafe extern "C" fn decimal_value(receiver: Id, _sel: Sel) -> NSDecimal {
    // SAFETY: a number.
    NSDecimal::from_decimal(&unsafe { value_of(receiver) }.unwrap_or_else(Decimal::nan))
}

/// Replace an `+alloc` placeholder with a decimal number holding `value`.
fn replace(receiver: Id, value: Decimal) -> Id {
    objc::arc::release(receiver);
    make(value)
}

unsafe extern "C" fn init_with_string_locale(receiver: Id, _sel: Sel, text: Id, locale: Id) -> Id {
    // SAFETY: a string, or nil.
    let text = String::from_utf8(unsafe { crate::string::bytes(text) }).unwrap_or_default();
    replace(receiver, Decimal::parse(&text, &separator_of(locale)))
}

unsafe extern "C" fn init_with_string(receiver: Id, sel: Sel, text: Id) -> Id {
    // SAFETY: as above, in the C locale.
    unsafe { init_with_string_locale(receiver, sel, text, std::ptr::null_mut()) }
}

unsafe extern "C" fn init_with_mantissa(
    receiver: Id,
    _sel: Sel,
    mantissa: u64,
    exponent: i16,
    negative: u8,
) -> Id {
    replace(
        receiver,
        Decimal::from_parts(mantissa, i32::from(exponent), negative != 0),
    )
}

unsafe extern "C" fn init_with_decimal(receiver: Id, _sel: Sel, value: NSDecimal) -> Id {
    replace(receiver, value.to_decimal())
}

/// The `-initWith<scalar>:` spellings `NSNumber` declares, which a decimal
/// number answers with the exact value of the scalar.
macro_rules! scalar_init {
    ($($name:ident: $ty:ty => $convert:expr;)*) => {$(
        unsafe extern "C" fn $name(receiver: Id, _sel: Sel, value: $ty) -> Id {
            let convert: fn($ty) -> Decimal = $convert;
            replace(receiver, convert(value))
        }
    )*};
}

scalar_init! {
    init_with_double: f64 => Decimal::from_f64;
    init_with_float: f32 => |v| Decimal::from_f64(f64::from(v));
    init_with_long_long: i64 => |v| Decimal::parse(&v.to_string(), ".");
    init_with_unsigned_long_long: u64 => |v| Decimal::parse(&v.to_string(), ".");
    init_with_int: i32 => |v| Decimal::parse(&v.to_string(), ".");
    init_with_bool: u8 => |v| Decimal::parse(if v != 0 { "1" } else { "0" }, ".");
}

unsafe extern "C" fn with_string_locale(_class: Id, _sel: Sel, text: Id, locale: Id) -> Id {
    // SAFETY: a string, or nil.
    let text = String::from_utf8(unsafe { crate::string::bytes(text) }).unwrap_or_default();
    autoreleased(Decimal::parse(&text, &separator_of(locale)))
}

unsafe extern "C" fn with_string(class: Id, sel: Sel, text: Id) -> Id {
    // SAFETY: as above, in the C locale.
    unsafe { with_string_locale(class, sel, text, std::ptr::null_mut()) }
}

unsafe extern "C" fn with_mantissa(
    _class: Id,
    _sel: Sel,
    mantissa: u64,
    exponent: i16,
    negative: u8,
) -> Id {
    autoreleased(Decimal::from_parts(
        mantissa,
        i32::from(exponent),
        negative != 0,
    ))
}

unsafe extern "C" fn with_decimal(_class: Id, _sel: Sel, value: NSDecimal) -> Id {
    autoreleased(value.to_decimal())
}

unsafe extern "C" fn class_zero(_class: Id, _sel: Sel) -> Id {
    autoreleased(Decimal::zero())
}

unsafe extern "C" fn class_one(_class: Id, _sel: Sel) -> Id {
    autoreleased(Decimal::from_parts(1, 0, false))
}

unsafe extern "C" fn class_not_a_number(_class: Id, _sel: Sel) -> Id {
    not_a_number()
}

unsafe extern "C" fn class_maximum(_class: Id, _sel: Sel) -> Id {
    autoreleased(Decimal::parse(
        &format!("{}e{MAX_EXPONENT}", "9".repeat(MAX_DIGITS)),
        ".",
    ))
}

unsafe extern "C" fn class_minimum(_class: Id, _sel: Sel) -> Id {
    autoreleased(Decimal::parse(
        &format!("-{}e{MAX_EXPONENT}", "9".repeat(MAX_DIGITS)),
        ".",
    ))
}

unsafe extern "C" fn default_behavior(_class: Id, _sel: Sel) -> Id {
    let chosen = *DEFAULT_HANDLER.lock().unwrap_or_else(|e| e.into_inner());
    match chosen {
        0 => default_handler(),
        handler => handler as Id,
    }
}

unsafe extern "C" fn set_default_behavior(_class: Id, _sel: Sel, handler: Id) {
    let mut chosen = DEFAULT_HANDLER.lock().unwrap_or_else(|e| e.into_inner());
    let old = std::mem::replace(&mut *chosen, objc::arc::retain(handler) as usize);
    objc::arc::release(old as Id);
}

fn new_handler(behavior: Behavior) -> Id {
    let object = objc::object::create_instance(handler_class(), 0);
    handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(object as usize, behavior);
    object
}

/// `+defaultDecimalNumberHandler`, one object for the process.
fn default_handler() -> Id {
    static DEFAULT: OnceLock<usize> = OnceLock::new();
    *DEFAULT.get_or_init(|| new_handler(DEFAULT_BEHAVIOR) as usize) as Id
}

unsafe extern "C" fn class_default_handler(_class: Id, _sel: Sel) -> Id {
    default_handler()
}

#[allow(clippy::too_many_arguments)]
fn behavior_from(
    mode: usize,
    scale: i16,
    exactness: u8,
    overflow: u8,
    underflow: u8,
    divide_by_zero: u8,
) -> Behavior {
    Behavior {
        mode: Rounding::from_raw(mode),
        scale,
        raise_exactness: exactness != 0,
        raise_overflow: overflow != 0,
        raise_underflow: underflow != 0,
        raise_divide_by_zero: divide_by_zero != 0,
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn handler_with(
    _class: Id,
    _sel: Sel,
    mode: usize,
    scale: i16,
    exactness: u8,
    overflow: u8,
    underflow: u8,
    divide_by_zero: u8,
) -> Id {
    let behavior = behavior_from(mode, scale, exactness, overflow, underflow, divide_by_zero);
    objc::arc::autorelease(new_handler(behavior))
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn handler_init(
    receiver: Id,
    _sel: Sel,
    mode: usize,
    scale: i16,
    exactness: u8,
    overflow: u8,
    underflow: u8,
    divide_by_zero: u8,
) -> Id {
    handlers().lock().unwrap_or_else(|e| e.into_inner()).insert(
        receiver as usize,
        behavior_from(mode, scale, exactness, overflow, underflow, divide_by_zero),
    );
    receiver
}

unsafe extern "C" fn handler_rounding_mode(receiver: Id, _sel: Sel) -> usize {
    behavior_of(receiver).mode.raw()
}

unsafe extern "C" fn handler_scale(receiver: Id, _sel: Sel) -> i16 {
    behavior_of(receiver).scale
}

unsafe extern "C" fn handler_dealloc(receiver: Id, _sel: Sel) {
    handlers()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(receiver as usize));
    objc::arc::finalize(receiver);
}

/// The exception names, which a program compares a caught exception's name
/// against.
pub const KEYS: &[(&str, &str)] = &[
    (
        "_NSDecimalNumberExactnessException",
        "NSDecimalNumberExactnessException",
    ),
    (
        "_NSDecimalNumberOverflowException",
        "NSDecimalNumberOverflowException",
    ),
    (
        "_NSDecimalNumberUnderflowException",
        "NSDecimalNumberUnderflowException",
    ),
    (
        "_NSDecimalNumberDivideByZeroException",
        "NSDecimalNumberDivideByZeroException",
    ),
];

pub(crate) fn install() {
    let class = corefoundation::base::register(
        c"NSDecimalNumber",
        Some(collections::number_class()),
        0,
        false,
    );
    methods!(class, {
        "dealloc" => dealloc, c"v16@0:8";
        "initWithString:" => init_with_string, c"@24@0:8@16";
        "initWithString:locale:" => init_with_string_locale, c"@32@0:8@16@24";
        "initWithMantissa:exponent:isNegative:" => init_with_mantissa, c"@32@0:8Q16s24c28";
        "initWithDecimal:" => init_with_decimal, c"@36@0:8{?=b8b4b1b1b18[8S]}16";
        "initWithDouble:" => init_with_double, c"@24@0:8d16";
        "initWithFloat:" => init_with_float, c"@20@0:8f16";
        "initWithLongLong:" => init_with_long_long, c"@24@0:8q16";
        "initWithInteger:" => init_with_long_long, c"@24@0:8q16";
        "initWithLong:" => init_with_long_long, c"@24@0:8q16";
        "initWithUnsignedLongLong:" => init_with_unsigned_long_long, c"@24@0:8Q16";
        "initWithUnsignedInteger:" => init_with_unsigned_long_long, c"@24@0:8Q16";
        "initWithInt:" => init_with_int, c"@20@0:8i16";
        "initWithBool:" => init_with_bool, c"@20@0:8c16";
        "decimalNumberByAdding:" => adding, c"@24@0:8@16";
        "decimalNumberByAdding:withBehavior:" => adding_with_behavior, c"@32@0:8@16@24";
        "decimalNumberBySubtracting:" => subtracting, c"@24@0:8@16";
        "decimalNumberBySubtracting:withBehavior:" => subtracting_with_behavior, c"@32@0:8@16@24";
        "decimalNumberByMultiplyingBy:" => multiplying, c"@24@0:8@16";
        "decimalNumberByMultiplyingBy:withBehavior:" => multiplying_with_behavior, c"@32@0:8@16@24";
        "decimalNumberByDividingBy:" => dividing, c"@24@0:8@16";
        "decimalNumberByDividingBy:withBehavior:" => dividing_with_behavior, c"@32@0:8@16@24";
        "decimalNumberByRaisingToPower:" => raising, c"@24@0:8Q16";
        "decimalNumberByRaisingToPower:withBehavior:" => raising_with_behavior, c"@32@0:8Q16@24";
        "decimalNumberByMultiplyingByPowerOf10:" => power_of_10, c"@20@0:8s16";
        "decimalNumberByMultiplyingByPowerOf10:withBehavior:" => power_of_10_with_behavior, c"@28@0:8s16@20";
        "decimalNumberByRoundingAccordingToBehavior:" => rounding, c"@24@0:8@16";
        "compare:" => compare, c"q24@0:8@16";
        "isEqualToNumber:" => is_equal_to_number, c"c24@0:8@16";
        "isEqual:" => is_equal, c"c24@0:8@16";
        "hash" => hash, c"Q16@0:8";
        "description" => description, c"@16@0:8";
        "stringValue" => description, c"@16@0:8";
        "descriptionWithLocale:" => description_with_locale, c"@24@0:8@16";
        "doubleValue" => double_value, c"d16@0:8";
        "floatValue" => float_value, c"f16@0:8";
        "longLongValue" => integer_value, c"q16@0:8";
        "longValue" => integer_value, c"q16@0:8";
        "integerValue" => integer_value, c"q16@0:8";
        "unsignedLongLongValue" => integer_value, c"Q16@0:8";
        "unsignedIntegerValue" => integer_value, c"Q16@0:8";
        "intValue" => int_value, c"i16@0:8";
        "unsignedIntValue" => int_value, c"I16@0:8";
        "boolValue" => bool_value, c"c16@0:8";
        "objCType" => objc_type, c"r*16@0:8";
        "decimalValue" => decimal_value, c"{?=b8b4b1b1b18[8S]}16@0:8";
    });
    class_methods!(class, {
        "decimalNumberWithString:" => with_string, c"@24@0:8@16";
        "decimalNumberWithString:locale:" => with_string_locale, c"@32@0:8@16@24";
        "decimalNumberWithMantissa:exponent:isNegative:" => with_mantissa, c"@32@0:8Q16s24c28";
        "decimalNumberWithDecimal:" => with_decimal, c"@36@0:8{?=b8b4b1b1b18[8S]}16";
        "zero" => class_zero, c"@16@0:8";
        "one" => class_one, c"@16@0:8";
        "notANumber" => class_not_a_number, c"@16@0:8";
        "maximumDecimalNumber" => class_maximum, c"@16@0:8";
        "minimumDecimalNumber" => class_minimum, c"@16@0:8";
        "defaultBehavior" => default_behavior, c"@16@0:8";
        "setDefaultBehavior:" => set_default_behavior, c"v24@0:8@16";
    });

    let handler = corefoundation::base::register(c"NSDecimalNumberHandler", None, 0, false);
    methods!(handler, {
        "initWithRoundingMode:scale:raiseOnExactness:raiseOnOverflow:raiseOnUnderflow:raiseOnDivideByZero:"
            => handler_init, c"@40@0:8Q16s24c28c32c36c40";
        "roundingMode" => handler_rounding_mode, c"Q16@0:8";
        "scale" => handler_scale, c"s16@0:8";
        "dealloc" => handler_dealloc, c"v16@0:8";
    });
    class_methods!(handler, {
        "defaultDecimalNumberHandler" => class_default_handler, c"@16@0:8";
        "decimalNumberHandlerWithRoundingMode:scale:raiseOnExactness:raiseOnOverflow:raiseOnUnderflow:raiseOnDivideByZero:"
            => handler_with, c"@40@0:8Q16s24c28c32c36c40";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal {
        Decimal::parse(text, ".")
    }

    fn s(value: Result<Decimal, (Error, Decimal)>) -> String {
        match value {
            Ok(v) | Err((_, v)) => v.to_string_with("."),
        }
    }

    #[test]
    fn adds_and_subtracts_exactly() {
        assert_eq!(s(d("0.1").add(&d("0.2"), Rounding::Plain)), "0.3");
        assert_eq!(s(d("1").add(&d("-1.5"), Rounding::Plain)), "-0.5");
        assert_eq!(s(d("123.45").add(&d("0.55"), Rounding::Plain)), "124");
    }

    #[test]
    fn divides_to_thirty_eight_digits() {
        assert_eq!(
            s(d("1").divide(&d("3"), Rounding::Plain)),
            "0.33333333333333333333333333333333333333"
        );
        assert_eq!(
            s(d("2").divide(&d("3"), Rounding::Plain)),
            "0.66666666666666666666666666666666666667"
        );
        assert_eq!(s(d("10").divide(&d("4"), Rounding::Plain)), "2.5");
        assert!(matches!(
            d("1").divide(&d("0"), Rounding::Plain),
            Err((Error::DivideByZero, _))
        ));
    }

    #[test]
    fn multiplies_powers_and_rounds() {
        assert_eq!(s(d("1.5").multiply(&d("-4"), Rounding::Plain)), "-6");
        assert_eq!(s(d("2").power(10, Rounding::Plain)), "1024");
        assert_eq!(
            d("2.675").round(2, Rounding::Plain).to_string_with("."),
            "2.68"
        );
        assert_eq!(
            d("2.665").round(2, Rounding::Bankers).to_string_with("."),
            "2.66"
        );
        assert_eq!(
            d("-1.21").round(1, Rounding::Down).to_string_with("."),
            "-1.3"
        );
        assert_eq!(
            d("0.004").round(2, Rounding::Plain).to_string_with("."),
            "0"
        );
        assert_eq!(
            d("0.006").round(2, Rounding::Plain).to_string_with("."),
            "0.01"
        );
    }

    #[test]
    fn parses_and_prints_apples_spelling() {
        assert_eq!(d("  -0012.3400").to_string_with("."), "-12.34");
        assert_eq!(d("1e3").to_string_with("."), "1000");
        assert_eq!(d("1.5E-3").to_string_with("."), "0.0015");
        assert!(d("abc").is_nan());
        assert_eq!(d("12abc").to_string_with("."), "12");
        assert_eq!(Decimal::parse("3,5", ",").to_string_with(","), "3,5");
        assert_eq!(d("0.1").to_f64(), 0.1);
    }

    #[test]
    fn round_trips_the_nsdecimal_struct() {
        for text in [
            "0",
            "1",
            "-12.5",
            "123456789012345678901234567890",
            "0.000001",
        ] {
            let value = d(text);
            assert_eq!(
                NSDecimal::from_decimal(&value).to_decimal(),
                value,
                "{text}"
            );
        }
        assert!(
            NSDecimal::from_decimal(&Decimal::nan())
                .to_decimal()
                .is_nan()
        );
    }
}
