//! The scanf family, after Darwin's `__svfscanf_l`.
//!
//! Darwin's `vsscanf` wraps its string in a read-only `FILE` and hands it to
//! the same engine as `vfscanf`; here one engine reads either [`Source`]. The
//! destinations come from a [`Destinations`], which is a `va_list` for a guest
//! that runs natively and the emulated guest's own arguments otherwise.

use std::ffi::{CStr, VaList, c_char, c_int, c_short, c_void};

use super::backend::{mbrtowc, wcrtomb};
use super::{EOF, File, Locked, STDINP, engine};

unsafe extern "C" {
    fn iswspace(wc: u32) -> c_int;
    fn wctob(wc: u32) -> c_int;
}

/// Longest integer field: Darwin's conversion buffer is 513 bytes.
const INTEGER_FIELD: usize = 512;

/// Darwin's conversion flags, which the modifiers combine into.
mod flag {
    pub const LONG: u32 = 1 << 0;
    pub const LONGDBL: u32 = 1 << 1;
    pub const SHORT: u32 = 1 << 2;
    pub const SUPPRESS: u32 = 1 << 3;
    pub const POINTER: u32 = 1 << 4;
    pub const NOSKIP: u32 = 1 << 5;
    pub const LONGLONG: u32 = 1 << 6;
    pub const INTMAXT: u32 = 1 << 7;
    pub const PTRDIFFT: u32 = 1 << 8;
    pub const SIZET: u32 = 1 << 9;
    pub const SHORTSHORT: u32 = 1 << 10;
    pub const UNSIGNED: u32 = 1 << 11;
}

/// How a caller's `long double` is laid out, which is what `%Lf` stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LongDouble {
    /// x86-64 Darwin's: the x87 80-bit format.
    X87,
    /// arm64 Darwin's: `double`.
    Double,
}

impl LongDouble {
    /// A guest running natively shares this architecture's.
    pub const NATIVE: LongDouble = if cfg!(target_arch = "x86_64") {
        LongDouble::X87
    } else {
        LongDouble::Double
    };
}

/// The pointer arguments a scan stores through, in argument order.
pub trait Destinations {
    /// The next pointer. Null if it could not be read; a null is skipped.
    fn next(&mut self) -> *mut c_void;
    /// Back to the first argument, for a `%n$` position.
    fn rewind(&mut self);
    /// The caller's `long double`.
    fn long_double(&self) -> LongDouble;
}

/// A natively running guest's `va_list`.
struct List<'a> {
    first: VaList<'a>,
    ap: VaList<'a>,
}

impl<'a> List<'a> {
    /// # Safety
    ///
    /// `ap` holds a pointer for every conversion the scan will store.
    unsafe fn new(ap: VaList<'a>) -> List<'a> {
        List {
            first: ap.clone(),
            ap,
        }
    }
}

impl Destinations for List<'_> {
    fn next(&mut self) -> *mut c_void {
        // SAFETY: `List::new`'s caller states the pointers are there.
        unsafe { self.ap.next_arg::<*mut c_void>() }
    }

    fn rewind(&mut self) {
        self.ap = self.first.clone();
    }

    fn long_double(&self) -> LongDouble {
        LongDouble::NATIVE
    }
}

/// What a scan reads.
enum Source<'a> {
    /// A stream, locked by the scan's caller.
    Stream(*mut File),
    /// A string, as `vsscanf`'s read-only `FILE` holds it.
    Text { bytes: &'a [u8], at: usize },
}

/// The input being scanned, with the count `%n` reports.
struct Input<'a> {
    source: Source<'a>,
    count: i64,
}

impl Input<'_> {
    fn get(&mut self) -> c_int {
        let c = match &mut self.source {
            // SAFETY: the scanned stream, locked by the scan's caller.
            Source::Stream(fp) => unsafe { engine::getc(*fp) },
            Source::Text { bytes, at } => match bytes.get(*at) {
                Some(&b) => {
                    *at += 1;
                    c_int::from(b)
                }
                None => EOF,
            },
        };
        if c != EOF {
            self.count += 1;
        }
        c
    }

    /// Put back `c`, which must be the byte last read.
    fn unget(&mut self, c: c_int) {
        if c == EOF {
            return;
        }
        match &mut self.source {
            // SAFETY: as [`Input::get`]; the `ungetc` buffer grows as needed.
            Source::Stream(fp) => _ = unsafe { engine::ungetc(c, *fp) },
            Source::Text { at, .. } => *at -= 1,
        }
        self.count -= 1;
    }

    fn peek(&mut self) -> c_int {
        let c = self.get();
        self.unget(c);
        c
    }

    /// Push back what `field` holds from `from` on, last byte first.
    fn unget_all(&mut self, field: &mut Vec<u8>, from: usize) {
        while field.len() > from {
            let b = field.pop().unwrap_or_default();
            self.unget(c_int::from(b));
        }
    }

    /// Skip whitespace; the next byte, left unread, or `EOF`.
    fn skip_space(&mut self) -> c_int {
        let mut c = self.get();
        while is_space(c) {
            c = self.get();
        }
        self.unget(c);
        c
    }

    /// One multibyte character; `None` at the end or on a bad sequence.
    fn get_wide(&mut self, state: &mut [u64; 16]) -> Option<u32> {
        loop {
            let c = self.get();
            if c == EOF {
                return None;
            }
            let b = c as c_char;
            let mut wc: c_int = 0;
            // SAFETY: one byte and a conversion state of any host's size.
            let n = unsafe { mbrtowc(&mut wc, &b, 1, state.as_mut_ptr().cast()) };
            match n {
                usize::MAX => return None,
                x if x == usize::MAX - 1 => continue,
                _ => return Some(wc as u32),
            }
        }
    }

    /// Put back a whole multibyte character.
    fn unget_wide(&mut self, wc: u32) {
        let mut back = [0u8; 16];
        let mut fresh = [0u64; 16];
        // SAFETY: a buffer longer than any sequence.
        let n = unsafe {
            wcrtomb(
                back.as_mut_ptr().cast(),
                wc as c_int,
                fresh.as_mut_ptr().cast(),
            )
        };
        if n != usize::MAX {
            for &b in back[..n].iter().rev() {
                self.unget(c_int::from(b));
            }
        }
    }
}

/// C's `isspace` in the C locale, which Rust's ASCII test does not match:
/// it leaves out the vertical tab.
fn is_space(c: c_int) -> bool {
    matches!(c, 0x20 | 0x09..=0x0d)
}

/// Collect an integer into `field` as Darwin's `CT_INT` scan does. The base
/// to convert it in, or `None` for a matching failure.
fn collect_integer(
    input: &mut Input,
    field: &mut Vec<u8>,
    width: usize,
    mut base: u32,
    mut prefix_ok: bool,
) -> Option<u32> {
    let width = match width {
        0 => INTEGER_FIELD,
        w => w.min(INTEGER_FIELD),
    };
    // `%i` settles its base at the first digit.
    let settle = |base: u32| if base == 0 { 10 } else { base };
    let (mut sign_ok, mut no_digits, mut no_zeros, mut signed) = (true, true, true, false);
    while field.len() < width {
        let c = input.peek();
        if c == EOF {
            break;
        }
        let c = c as u8;
        let accept = match c {
            b'0' => {
                if base == 0 {
                    base = 8;
                    prefix_ok = true;
                }
                // A zero after a zero ends the chance of a prefix.
                if !no_zeros {
                    prefix_ok = false;
                }
                (sign_ok, no_zeros, no_digits) = (false, false, false);
                true
            }
            b'1'..=b'9' => {
                base = settle(base);
                let ok = c <= b'7' || base > 8;
                if ok {
                    (sign_ok, prefix_ok, no_digits) = (false, false, false);
                }
                ok
            }
            b'a'..=b'f' | b'A'..=b'F' => {
                let ok = base > 10;
                if ok {
                    (sign_ok, prefix_ok, no_digits) = (false, false, false);
                }
                ok
            }
            b'+' | b'-' => {
                let ok = sign_ok;
                (sign_ok, signed) = (false, signed || ok);
                ok
            }
            b'x' | b'X' => {
                let ok = prefix_ok && field.len() == 1 + usize::from(signed);
                if ok {
                    (base, prefix_ok) = (16, false);
                }
                ok
            }
            _ => false,
        };
        if !accept {
            break;
        }
        field.push(c);
        input.get();
    }
    if no_digits {
        // At most a sign was taken; it is not part of anything.
        input.unget_all(field, 0);
        return None;
    }
    // "0x" with no digit after it is the number 0.
    if let Some(&x @ (b'x' | b'X')) = field.last() {
        field.pop();
        input.unget(c_int::from(x));
    }
    Some(base)
}

/// Collect a floating-point number into `field` as Darwin's `parsefloat`
/// does: read while the text could still become a number, then put back all
/// after the last point where it was one. Empty for a matching failure.
fn collect_float(input: &mut Input, field: &mut Vec<u8>, width: usize) {
    #[derive(PartialEq)]
    enum State {
        Start,
        GotSign,
        Inf,
        Nan,
        Done,
        MaybeHex,
        Digits,
        DecimalPoint,
        Fraction,
        Exponent,
        ExponentDigits,
    }
    // SAFETY: the current locale's own conventions, NUL-terminated.
    let point = unsafe { CStr::from_ptr((*libc::localeconv()).decimal_point) }.to_bytes();
    let mut state = State::Start;
    let (mut infnan, mut in_point) = (0usize, 0usize);
    let (mut mantissa, mut hex) = (false, false);
    // How much of `field` is a number by itself.
    let mut commit = 0;
    while width == 0 || field.len() < width {
        let c = input.peek();
        if c == EOF {
            break;
        }
        let c = c as u8;
        let here = field.len() + 1;
        let accept = loop {
            match state {
                State::Start => {
                    state = State::GotSign;
                    if c == b'-' || c == b'+' {
                        break true;
                    }
                }
                State::GotSign => match c {
                    b'0' => {
                        (state, commit) = (State::MaybeHex, here);
                        break true;
                    }
                    b'I' | b'i' => {
                        state = State::Inf;
                        break true;
                    }
                    b'N' | b'n' => {
                        state = State::Nan;
                        break true;
                    }
                    _ => state = State::Digits,
                },
                State::Inf => {
                    let rest = b"nfinity";
                    if infnan >= rest.len() || c.to_ascii_lowercase() != rest[infnan] {
                        break false;
                    }
                    // "inf" or "infinity".
                    if infnan == 1 || infnan == 6 {
                        commit = here;
                    }
                    infnan += 1;
                    break true;
                }
                State::Nan => {
                    let ok = match infnan {
                        0 => c.eq_ignore_ascii_case(&b'a'),
                        1 => c.eq_ignore_ascii_case(&b'n'),
                        2 => c == b'(',
                        _ => c == b')' || c.is_ascii_alphanumeric() || c == b'_',
                    };
                    if !ok {
                        break false;
                    }
                    if infnan == 1 {
                        commit = here;
                    } else if infnan > 2 && c == b')' {
                        (state, commit) = (State::Done, here);
                    }
                    infnan += 1;
                    break true;
                }
                State::Done => break false,
                State::MaybeHex => {
                    state = State::Digits;
                    if c == b'X' || c == b'x' {
                        hex = true;
                        break true;
                    }
                    mantissa = true;
                }
                State::Digits => {
                    if (hex && c.is_ascii_hexdigit()) || c.is_ascii_digit() {
                        (mantissa, commit) = (true, here);
                        break true;
                    }
                    state = State::DecimalPoint;
                }
                State::DecimalPoint => {
                    if point.get(in_point) == Some(&c) {
                        in_point += 1;
                        if in_point == point.len() {
                            state = State::Fraction;
                            if mantissa {
                                commit = here;
                            }
                        }
                        break true;
                    }
                    // Part of a multibyte decimal point, and then not.
                    if in_point != 0 {
                        break false;
                    }
                    state = State::Fraction;
                }
                State::Fraction => {
                    let marker = if hex { b'p' } else { b'e' };
                    if c.to_ascii_lowercase() == marker {
                        if !mantissa {
                            break false;
                        }
                        state = State::Exponent;
                        break true;
                    }
                    if (hex && c.is_ascii_hexdigit()) || c.is_ascii_digit() {
                        (mantissa, commit) = (true, here);
                        break true;
                    }
                    break false;
                }
                State::Exponent => {
                    state = State::ExponentDigits;
                    if c == b'-' || c == b'+' {
                        break true;
                    }
                }
                State::ExponentDigits => {
                    if c.is_ascii_digit() {
                        commit = here;
                        break true;
                    }
                    break false;
                }
            }
        };
        if !accept {
            break;
        }
        field.push(c);
        input.get();
    }
    input.unget_all(field, commit);
}

/// Parse a `%[` set after the `[`, as Darwin's `__sccl` does; the set and the
/// format after its `]`.
///
/// A `]` or `-` first is a member, and so is a `-` last or before a lower
/// byte. `a-c-e` is `a` through `e`.
// FIXME (ACCURACY): ranges are in byte order; Darwin orders them by the
// locale's collation when it has one loaded.
fn scanset(f: &[u8]) -> ([bool; 256], &[u8]) {
    let (negate, mut f) = match f.split_first() {
        Some((b'^', rest)) => (true, rest),
        _ => (false, f),
    };
    let mut set = [negate; 256];
    let Some((&first, rest)) = f.split_first() else {
        return (set, f);
    };
    f = rest;
    let mut c = first;
    loop {
        set[usize::from(c)] = !negate;
        loop {
            let Some((&n, rest)) = f.split_first() else {
                return (set, f);
            };
            f = rest;
            match n {
                b']' => return (set, f),
                b'-' => match f.first() {
                    Some(&hi) if hi != b']' && hi >= c => {
                        f = &f[1..];
                        for b in c..=hi {
                            set[usize::from(b)] = !negate;
                        }
                        c = hi;
                    }
                    _ => {
                        c = b'-';
                        break;
                    }
                },
                _ => {
                    c = n;
                    break;
                }
            }
        }
    }
}

/// Store an integer at `at` as the type `flags` names. Null is skipped.
///
/// # Safety
///
/// `at` is null or writable for that type.
unsafe fn store_int(at: *mut c_void, flags: u32, value: i64) {
    if at.is_null() {
        return;
    }
    // SAFETY (whole match): the caller's obligation. Darwin is LP64, and the
    // truncations are C's conversions.
    unsafe {
        if flags & flag::POINTER != 0 {
            at.cast::<usize>().write_unaligned(value as usize);
        } else if flags & flag::SHORTSHORT != 0 {
            at.cast::<i8>().write(value as i8);
        } else if flags & flag::SHORT != 0 {
            at.cast::<c_short>().write_unaligned(value as c_short);
        } else if flags & (flag::LONG | flag::LONGLONG | flag::INTMAXT) != 0 {
            at.cast::<i64>().write_unaligned(value);
        } else if flags & flag::PTRDIFFT != 0 {
            at.cast::<isize>().write_unaligned(value as isize);
        } else if flags & flag::SIZET != 0 {
            at.cast::<usize>().write_unaligned(value as usize);
        } else {
            at.cast::<c_int>().write_unaligned(value as c_int);
        }
    }
}

/// Parse and store a `long double` in the caller's layout.
///
/// # Safety
///
/// `text` is NUL-terminated and `at` is writable for that `long double`.
unsafe fn store_long_double(at: *mut c_void, text: *const c_char, layout: LongDouble) {
    // SAFETY: the caller's string and destination.
    unsafe {
        match layout {
            LongDouble::X87 => {
                let value = crate::strtofp::strtold_x87(text, std::ptr::null_mut());
                std::ptr::copy_nonoverlapping(value.as_ptr(), at.cast(), value.len());
            }
            LongDouble::Double => at
                .cast::<f64>()
                .write_unaligned(crate::strtofp::strtod(text, std::ptr::null_mut())),
        }
    }
}

/// How a scan ended before its format did.
enum Failure {
    /// The input ran out.
    Input,
    /// The input did not match.
    Matching,
}

/// Read `format` against `input`, storing through `out`.
///
/// # Safety
///
/// `format` is NUL-terminated and each pointer `out` yields is null or
/// writable for its conversion.
unsafe fn scan(input: &mut Input, format: *const c_char, out: &mut dyn Destinations) -> c_int {
    // SAFETY: the caller's NUL-terminated format.
    let mut f = unsafe { CStr::from_ptr(format) }.to_bytes();
    let mut field: Vec<u8> = Vec::new();
    let mut assigned = 0;
    let failure = 'scan: loop {
        let Some((&c, rest)) = f.split_first() else {
            return assigned;
        };
        f = rest;
        if is_space(c_int::from(c)) {
            input.skip_space();
            continue;
        }
        if c != b'%' {
            match input.get() {
                EOF => break Failure::Input,
                got if got == c_int::from(c) => continue,
                got => {
                    input.unget(got);
                    break Failure::Matching;
                }
            }
        }
        let spec = f;
        let (mut flags, mut width) = (0u32, 0usize);
        let conv = loop {
            let Some((&c, rest)) = f.split_first() else {
                // A format ending in a conversion, which Darwin keeps for
                // compatibility.
                return EOF;
            };
            f = rest;
            match c {
                b'$' => {
                    // Only `%N$`, one digit straight after the `%`.
                    let index = width;
                    if index < 1 || spec.len() - f.len() != 2 {
                        break 'scan Failure::Input;
                    }
                    width = 0;
                    out.rewind();
                    for _ in 1..index {
                        out.next();
                    }
                }
                b'*' => flags |= flag::SUPPRESS,
                b'j' => flags |= flag::INTMAXT,
                b'l' if flags & flag::LONG != 0 => flags = (flags & !flag::LONG) | flag::LONGLONG,
                b'l' => flags |= flag::LONG,
                b'q' => flags |= flag::LONGLONG,
                b't' => flags |= flag::PTRDIFFT,
                b'z' => flags |= flag::SIZET,
                b'L' => flags |= flag::LONGDBL,
                b'h' if flags & flag::SHORT != 0 => {
                    flags = (flags & !flag::SHORT) | flag::SHORTSHORT
                }
                b'h' => flags |= flag::SHORT,
                b'0'..=b'9' => {
                    width = width
                        .saturating_mul(10)
                        .saturating_add(usize::from(c - b'0'))
                }
                _ => break c,
            }
        };
        let suppress = flags & flag::SUPPRESS != 0;
        let (mut base, mut prefix_ok) = (10, false);
        match conv {
            b'%' => {
                if input.skip_space() == EOF {
                    break Failure::Input;
                }
                let got = input.get();
                if got != c_int::from(b'%') {
                    input.unget(got);
                    break Failure::Matching;
                }
                continue;
            }
            b'n' => {
                if !suppress {
                    // SAFETY: `%n`'s argument is an integer of this width.
                    unsafe { store_int(out.next(), flags, input.count) };
                }
                continue;
            }
            b'd' => {}
            b'i' => base = 0,
            b'o' => (base, flags) = (8, flags | flag::UNSIGNED),
            b'u' => flags |= flag::UNSIGNED,
            b'x' | b'X' => (base, prefix_ok, flags) = (16, true, flags | flag::UNSIGNED),
            b'p' => {
                (base, prefix_ok) = (16, true);
                flags |= flag::POINTER | flag::UNSIGNED;
            }
            b'a' | b'A' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {}
            b'S' => flags |= flag::LONG,
            b's' => {}
            b'C' => flags |= flag::LONG | flag::NOSKIP,
            b'c' | b'[' => flags |= flag::NOSKIP,
            _ => break Failure::Matching,
        }
        let set = match conv {
            b'[' => {
                let (set, rest) = scanset(f);
                f = rest;
                set
            }
            _ => std::array::from_fn(|c| !is_space(c as c_int)),
        };
        if input.peek() == EOF {
            break Failure::Input;
        }
        if flags & flag::NOSKIP == 0 && input.skip_space() == EOF {
            break Failure::Input;
        }
        let wide = flags & flag::LONG != 0;
        field.clear();
        // SAFETY (whole match): each destination is the next argument, of the
        // type the conversion names, which is the caller's obligation.
        match conv {
            b'c' | b'C' => {
                let width = width.max(1);
                let at = if suppress {
                    std::ptr::null_mut()
                } else {
                    out.next()
                };
                let mut n = 0;
                if wide {
                    let mut state = [0u64; 16];
                    while n < width {
                        let Some(wc) = input.get_wide(&mut state) else {
                            break;
                        };
                        if !at.is_null() {
                            unsafe { at.cast::<u32>().add(n).write_unaligned(wc) };
                        }
                        n += 1;
                    }
                } else {
                    while n < width {
                        let c = input.get();
                        if c == EOF {
                            break;
                        }
                        if !at.is_null() {
                            unsafe { at.cast::<u8>().add(n).write(c as u8) };
                        }
                        n += 1;
                    }
                }
                if n == 0 {
                    break Failure::Input;
                }
                if !suppress {
                    assigned += 1;
                }
            }
            b's' | b'S' | b'[' => {
                let width = if width == 0 { usize::MAX } else { width };
                let at = if suppress {
                    std::ptr::null_mut()
                } else {
                    out.next()
                };
                let mut n = 0;
                if wide {
                    let mut state = [0u64; 16];
                    while n < width {
                        let Some(wc) = input.get_wide(&mut state) else {
                            break;
                        };
                        let member = match conv {
                            // A character with no single-byte form is in
                            // every set.
                            // SAFETY: a plain query of the current locale.
                            b'[' => match unsafe { wctob(wc) } {
                                EOF => true,
                                b => set[b as u8 as usize],
                            },
                            // SAFETY: as above.
                            _ => (unsafe { iswspace(wc) }) == 0,
                        };
                        if !member {
                            input.unget_wide(wc);
                            break;
                        }
                        if !at.is_null() {
                            unsafe { at.cast::<u32>().add(n).write_unaligned(wc) };
                        }
                        n += 1;
                    }
                    if n > 0 && !at.is_null() {
                        unsafe { at.cast::<u32>().add(n).write_unaligned(0) };
                    }
                } else {
                    while n < width {
                        let c = input.get();
                        if c == EOF {
                            break;
                        }
                        if !set[c as usize] {
                            input.unget(c);
                            break;
                        }
                        if !at.is_null() {
                            unsafe { at.cast::<u8>().add(n).write(c as u8) };
                        }
                        n += 1;
                    }
                    if n > 0 && !at.is_null() {
                        unsafe { at.cast::<u8>().add(n).write(0) };
                    }
                }
                if n == 0 {
                    break Failure::Matching;
                }
                if !suppress {
                    assigned += 1;
                }
            }
            b'a' | b'A' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {
                collect_float(input, &mut field, width);
                if field.is_empty() {
                    break Failure::Matching;
                }
                if !suppress {
                    field.push(0);
                    let text = field.as_ptr().cast::<c_char>();
                    let at = out.next();
                    let end = std::ptr::null_mut();
                    // SAFETY: a NUL-terminated number, and a destination of
                    // the type the flags name.
                    if !at.is_null() {
                        unsafe {
                            use crate::strtofp::{strtod, strtof};
                            if flags & flag::LONGDBL != 0 {
                                store_long_double(at, text, out.long_double());
                            } else if flags & flag::LONG != 0 {
                                at.cast::<f64>().write_unaligned(strtod(text, end));
                            } else {
                                at.cast::<f32>().write_unaligned(strtof(text, end));
                            }
                        }
                    }
                    assigned += 1;
                }
            }
            _ => {
                let Some(base) = collect_integer(input, &mut field, width, base, prefix_ok) else {
                    break Failure::Matching;
                };
                if !suppress {
                    field.push(0);
                    let text = field.as_ptr().cast::<c_char>();
                    let end = std::ptr::null_mut();
                    // SAFETY: a NUL-terminated field of digits.
                    let value = unsafe {
                        match flags & flag::UNSIGNED {
                            0 => libc::strtoll(text, end, base as c_int),
                            _ => libc::strtoull(text, end, base as c_int) as i64,
                        }
                    };
                    unsafe { store_int(out.next(), flags, value) };
                    assigned += 1;
                }
            }
        }
    };
    match failure {
        Failure::Input if assigned == 0 => EOF,
        _ => assigned,
    }
}

/// Scan a stream in `loc`, if given.
///
/// # Safety
///
/// As [`scan_with`].
unsafe fn scan_stream(
    fp: *mut File,
    loc: Option<*mut c_void>,
    format: *const c_char,
    out: &mut dyn Destinations,
) -> c_int {
    let _locale = loc.map(|loc| crate::xlocale::switch(loc as libc::locale_t));
    // SAFETY: the caller's stream, format and destinations.
    unsafe {
        let _guard = Locked::new(fp);
        engine::orient(fp, -1);
        let mut input = Input {
            source: Source::Stream(fp),
            count: 0,
        };
        scan(&mut input, format, out)
    }
}

/// Scan a string in `loc`, if given.
///
/// # Safety
///
/// As [`scan_str_with`].
unsafe fn scan_str(
    text: *const c_char,
    loc: Option<*mut c_void>,
    format: *const c_char,
    out: &mut dyn Destinations,
) -> c_int {
    let _locale = loc.map(|loc| crate::xlocale::switch(loc as libc::locale_t));
    let mut input = Input {
        // SAFETY: the caller's NUL-terminated string.
        source: Source::Text {
            bytes: unsafe { CStr::from_ptr(text) }.to_bytes(),
            at: 0,
        },
        count: 0,
    };
    // SAFETY: the caller's format and destinations.
    unsafe { scan(&mut input, format, out) }
}

/// `vfscanf` for a caller that holds the destinations some other way: an
/// emulated guest's arguments.
///
/// # Safety
///
/// `stream` is one this module opened, `loc` is a `locale_t` this process
/// made, `format` is NUL-terminated, and each pointer `out` yields is null or
/// writable for its conversion.
pub unsafe fn scan_with(
    stream: *mut c_void,
    loc: Option<*mut c_void>,
    format: *const c_char,
    out: &mut dyn Destinations,
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_stream(stream.cast(), loc, format, out) }
}

/// `vsscanf` for a caller that holds the destinations some other way.
///
/// # Safety
///
/// As [`scan_with`], with a NUL-terminated `text` for the stream.
pub unsafe fn scan_str_with(
    text: *const c_char,
    loc: Option<*mut c_void>,
    format: *const c_char,
    out: &mut dyn Destinations,
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_str(text, loc, format, out) }
}

/// `int vfscanf(FILE *stream, const char *format, va_list ap)`
///
/// # Safety
///
/// `stream` is open and `ap` holds a destination for every assignment.
pub unsafe extern "C" fn vfscanf(fp: *mut File, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_stream(fp, None, format, &mut List::new(ap)) }
}

/// `int vscanf(const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vfscanf`].
pub unsafe extern "C" fn vscanf(format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdin`, and the caller's obligations.
    unsafe { scan_stream(STDINP.get(), None, format, &mut List::new(ap)) }
}

/// `int vfscanf_l(FILE *stream, locale_t loc, const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vfscanf`], and `loc` is null or a `locale_t` this process made.
pub unsafe extern "C" fn vfscanf_l(
    fp: *mut File,
    loc: *mut c_void,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_stream(fp, Some(loc), format, &mut List::new(ap)) }
}

/// `int vscanf_l(locale_t loc, const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vfscanf_l`].
pub unsafe extern "C" fn vscanf_l(loc: *mut c_void, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdin`, and the caller's obligations.
    unsafe { scan_stream(STDINP.get(), Some(loc), format, &mut List::new(ap)) }
}

/// `int vsscanf(const char *str, const char *format, va_list ap)`
///
/// # Safety
///
/// Both strings are NUL-terminated and `ap` holds a destination for every
/// assignment.
pub unsafe extern "C" fn vsscanf(text: *const c_char, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_str(text, None, format, &mut List::new(ap)) }
}

/// `int vsscanf_l(const char *str, locale_t loc, const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vsscanf`], and `loc` is null or a `locale_t` this process made.
pub unsafe extern "C" fn vsscanf_l(
    text: *const c_char,
    loc: *mut c_void,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_str(text, Some(loc), format, &mut List::new(ap)) }
}

crate::variadic! {
    /// `int fscanf(FILE *stream, const char *format, ...)`
    pub fn fscanf(fp: *mut File, format: *const c_char) -> c_int => vfscanf;
    /// `int scanf(const char *format, ...)`
    pub fn scanf(format: *const c_char) -> c_int => vscanf;
    /// `int fscanf_l(FILE *stream, locale_t loc, const char *format, ...)`
    pub fn fscanf_l(fp: *mut File, loc: *mut c_void, format: *const c_char) -> c_int
        => vfscanf_l;
    /// `int scanf_l(locale_t loc, const char *format, ...)`
    pub fn scanf_l(loc: *mut c_void, format: *const c_char) -> c_int => vscanf_l;
    /// `int sscanf(const char *str, const char *format, ...)`
    pub fn sscanf(text: *const c_char, format: *const c_char) -> c_int => vsscanf;
    /// `int sscanf_l(const char *str, locale_t loc, const char *format, ...)`
    pub fn sscanf_l(text: *const c_char, loc: *mut c_void, format: *const c_char) -> c_int
        => vsscanf_l;
}

crate::va_list! {
    pub(super) darwin_vfscanf = vfscanf, 2;
    pub(super) darwin_vscanf = vscanf, 1;
    pub(super) darwin_vfscanf_l = vfscanf_l, 3;
    pub(super) darwin_vscanf_l = vscanf_l, 2;
    pub darwin_vsscanf = vsscanf, 2;
    pub darwin_vsscanf_l = vsscanf_l, 3;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `sscanf` in this host's convention, which the published one is not on
    /// every host.
    unsafe extern "C" fn sscanf(text: *const c_char, format: *const c_char, args: ...) -> c_int {
        // SAFETY: the caller's obligations.
        unsafe { vsscanf(text, format, args) }
    }

    fn text(bytes: &[u8]) -> &str {
        CStr::from_bytes_until_nul(bytes)
            .expect("terminated")
            .to_str()
            .expect("UTF-8")
    }

    #[test]
    fn a_negated_set_stops_at_the_first_excluded_byte() {
        let (mut kind, mut id, mut w, mut h) = ([0u8; 16], 0i64, 0i32, 0i32);
        // SAFETY: a destination of each conversion's type.
        let n = unsafe {
            sscanf(
                c"type=Asset&id=134289558904675&w=384&h=216&filters=".as_ptr(),
                c"type=%[^&]&id=%lld&w=%d&h=%d".as_ptr(),
                kind.as_mut_ptr(),
                &raw mut id,
                &raw mut w,
                &raw mut h,
            )
        };
        assert_eq!(
            (n, text(&kind), id, w, h),
            (4, "Asset", 134289558904675, 384, 216)
        );
    }

    #[test]
    fn a_range_continues_through_a_second_dash() {
        let mut out = [0u8; 8];
        // SAFETY: as above.
        let n = unsafe { sscanf(c"abdez".as_ptr(), c"%[a-c-e]".as_ptr(), out.as_mut_ptr()) };
        assert_eq!((n, text(&out)), (1, "abde"));
    }

    #[test]
    fn running_out_after_only_suppressed_conversions_is_eof() {
        let mut d = 0i32;
        // SAFETY: as above.
        let n = unsafe { sscanf(c"5".as_ptr(), c"%*d%d".as_ptr(), &raw mut d) };
        assert_eq!(n, EOF);
    }

    #[test]
    fn a_position_restarts_the_arguments() {
        let (mut a, mut b) = (0i32, 0i32);
        // SAFETY: as above.
        let n = unsafe {
            sscanf(
                c"7 9".as_ptr(),
                c"%2$d %1$d".as_ptr(),
                &raw mut a,
                &raw mut b,
            )
        };
        assert_eq!((n, a, b), (2, 9, 7));
    }

    #[test]
    fn a_format_ending_in_a_bare_percent_is_eof() {
        let mut d = 0i32;
        // SAFETY: as above.
        let n = unsafe { sscanf(c"1 2".as_ptr(), c"%d %".as_ptr(), &raw mut d) };
        assert_eq!((n, d), (EOF, 1));
    }

    #[test]
    fn a_float_gives_back_what_did_not_complete_it() {
        let (mut f, mut rest) = (0f32, [0u8; 8]);
        // SAFETY: as above.
        let n = unsafe {
            sscanf(
                c"1e+x".as_ptr(),
                c"%f%s".as_ptr(),
                &raw mut f,
                rest.as_mut_ptr(),
            )
        };
        assert_eq!((n, f, text(&rest)), (2, 1.0, "e+x"));
    }

    #[test]
    fn a_hex_prefix_without_a_digit_is_zero() {
        let (mut x, mut rest) = (1u32, [0u8; 8]);
        // SAFETY: as above.
        let n = unsafe {
            sscanf(
                c"0xg".as_ptr(),
                c"%x%s".as_ptr(),
                &raw mut x,
                rest.as_mut_ptr(),
            )
        };
        assert_eq!((n, x, text(&rest)), (2, 0, "xg"));
    }
}
