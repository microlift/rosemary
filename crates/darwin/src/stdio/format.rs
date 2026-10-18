//! The variadic formatters.
//!
//! The printf family renders with the host's formatter, which shares the
//! guest's calling convention and `va_list`, and writes the result through the
//! stream. The scanf family reads through the stream itself and converts
//! numbers with the host's `strto*`.

use std::ffi::{VaList, c_char, c_int, c_long, c_short, c_void};

use super::backend::{mbrtowc, wcrtomb};
use super::{EOF, File, Locked, STDINP, STDOUTP, engine};

unsafe extern "C" {
    fn vasprintf(out: *mut *mut c_char, format: *const c_char, ap: VaList) -> c_int;
    fn vswprintf(out: *mut u32, n: usize, format: *const u32, ap: VaList) -> c_int;
    fn iswspace(wc: u32) -> c_int;
}

/// The thread switched into a guest's `locale_t` until this is dropped.
struct InLocale(Option<libc::locale_t>);

impl InLocale {
    fn new(loc: Option<*mut c_void>) -> InLocale {
        let Some(loc) = loc else {
            return InLocale(None);
        };
        let loc = match loc.is_null() {
            // Darwin's null `locale_t` is the C locale; the host's is not.
            true => c_locale(),
            false => loc as libc::locale_t,
        };
        // SAFETY: a `locale_t` the guest obtained from this process, or C's.
        InLocale(Some(unsafe { libc::uselocale(loc) }))
    }
}

impl Drop for InLocale {
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            // SAFETY: the value `uselocale` returned, which it accepts back.
            unsafe { libc::uselocale(previous) };
        }
    }
}

/// A `locale_t` for "C", made once.
fn c_locale() -> libc::locale_t {
    static C: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *C.get_or_init(|| {
        // SAFETY: a fresh locale from a constant name, kept for the process.
        unsafe {
            libc::newlocale(
                crate::locale::all_mask(),
                c"C".as_ptr(),
                std::ptr::null_mut(),
            ) as usize
        }
    }) as libc::locale_t
}

/// Render and write; the length, or -1.
unsafe fn emit(
    fp: *mut File,
    loc: Option<*mut c_void>,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    let mut out: *mut c_char = std::ptr::null_mut();
    // Only the rendering is in the guest's locale; the write is not.
    let n = {
        let _locale = InLocale::new(loc);
        // SAFETY: the caller's format and arguments.
        unsafe { vasprintf(&mut out, format, ap) }
    };
    if n < 0 {
        return -1;
    }
    // SAFETY: the caller's stream and the host's rendering of `n` bytes.
    let put = unsafe {
        let _guard = Locked::new(fp);
        engine::orient(fp, -1);
        let put = engine::write_all(fp, out.cast(), n as usize);
        libc::free(out.cast());
        put
    };
    match put == n as usize {
        true => n,
        false => -1,
    }
}

/// `int vfprintf(FILE *stream, const char *format, va_list ap)`
///
/// # Safety
///
/// `stream` is open and `ap` matches `format`.
pub unsafe extern "C" fn vfprintf(fp: *mut File, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { emit(fp, None, format, ap) }
}

/// `int vprintf(const char *format, va_list ap)`
///
/// # Safety
///
/// `ap` matches `format`.
pub unsafe extern "C" fn vprintf(format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdout`, and the caller's obligations.
    unsafe { emit(STDOUTP.get(), None, format, ap) }
}

/// `int fprintf(FILE *stream, const char *format, ...)`
///
/// # Safety
///
/// As [`vfprintf`].
pub unsafe extern "C" fn fprintf(fp: *mut File, format: *const c_char, args: ...) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { emit(fp, None, format, args) }
}

/// `int printf(const char *format, ...)`
///
/// # Safety
///
/// As [`vprintf`].
pub unsafe extern "C" fn printf(format: *const c_char, args: ...) -> c_int {
    // SAFETY: the guest's `stdout`, and the caller's obligations.
    unsafe { emit(STDOUTP.get(), None, format, args) }
}

/// `int vfprintf_l(FILE *stream, locale_t loc, const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vfprintf`], and `loc` is null or a `locale_t` this process made.
pub unsafe extern "C" fn vfprintf_l(
    fp: *mut File,
    loc: *mut c_void,
    format: *const c_char,
    ap: VaList,
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { emit(fp, Some(loc), format, ap) }
}

/// `int vprintf_l(locale_t loc, const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vfprintf_l`].
pub unsafe extern "C" fn vprintf_l(loc: *mut c_void, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdout`, and the caller's obligations.
    unsafe { emit(STDOUTP.get(), Some(loc), format, ap) }
}

/// `int fprintf_l(FILE *stream, locale_t loc, const char *format, ...)`
///
/// # Safety
///
/// As [`vfprintf_l`].
pub unsafe extern "C" fn fprintf_l(
    fp: *mut File,
    loc: *mut c_void,
    format: *const c_char,
    args: ...
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { emit(fp, Some(loc), format, args) }
}

/// `int printf_l(locale_t loc, const char *format, ...)`
///
/// # Safety
///
/// As [`vfprintf_l`].
pub unsafe extern "C" fn printf_l(loc: *mut c_void, format: *const c_char, args: ...) -> c_int {
    // SAFETY: the guest's `stdout`, and the caller's obligations.
    unsafe { emit(STDOUTP.get(), Some(loc), format, args) }
}

/// `int vfwprintf(FILE *stream, const wchar_t *format, va_list ap)`
///
/// # Safety
///
/// `stream` is open and `ap` matches `format`.
pub unsafe extern "C" fn vfwprintf(fp: *mut File, format: *const u32, ap: VaList) -> c_int {
    // `vswprintf` has no "how long would it have been" answer, so the buffer
    // grows until the rendering fits. A failure at the cap is an encoding
    // error rather than a long string.
    let mut buf: Vec<u32> = vec![0; 256];
    let n = loop {
        // SAFETY: a buffer of `buf.len()` wide characters and the caller's
        // format, with a copy of the arguments for each attempt.
        let n = unsafe { vswprintf(buf.as_mut_ptr(), buf.len(), format, ap.clone()) };
        if n >= 0 {
            break n as usize;
        }
        if buf.len() >= 1 << 24 {
            return -1;
        }
        buf.resize(buf.len() * 4, 0);
    };
    // SAFETY: the caller's stream.
    unsafe {
        let _guard = Locked::new(fp);
        engine::orient(fp, 1);
        match super::wide::write_wide(fp, &buf[..n]) {
            true => n as c_int,
            false => -1,
        }
    }
}

/// `int vwprintf(const wchar_t *format, va_list ap)`
///
/// # Safety
///
/// `ap` matches `format`.
pub unsafe extern "C" fn vwprintf(format: *const u32, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdout`, and the caller's obligations.
    unsafe { vfwprintf(STDOUTP.get(), format, ap) }
}

/// `int fwprintf(FILE *stream, const wchar_t *format, ...)`
///
/// # Safety
///
/// As [`vfwprintf`].
pub unsafe extern "C" fn fwprintf(fp: *mut File, format: *const u32, args: ...) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { vfwprintf(fp, format, args) }
}

/// `int wprintf(const wchar_t *format, ...)`
///
/// # Safety
///
/// As [`vwprintf`].
pub unsafe extern "C" fn wprintf(format: *const u32, args: ...) -> c_int {
    // SAFETY: the guest's `stdout`, and the caller's obligations.
    unsafe { vfwprintf(STDOUTP.get(), format, args) }
}

/// Longest numeric field collected; longer input stops the field there.
const FIELD: usize = 511;

#[derive(Clone, Copy, PartialEq)]
enum Length {
    None,
    Char,
    Short,
    Long,
    LongLong,
    IntMax,
    Size,
    PtrDiff,
    LongDouble,
}

/// The stream being scanned, with the count `%n` reports.
struct Input {
    fp: *mut File,
    count: i64,
}

impl Input {
    fn get(&mut self) -> c_int {
        // SAFETY: the scanned stream, locked by `scan`'s caller.
        let c = unsafe { engine::getc(self.fp) };
        if c != EOF {
            self.count += 1;
        }
        c
    }

    fn unget(&mut self, c: c_int) {
        if c == EOF {
            return;
        }
        // SAFETY: as [`Input::get`]; the `ungetc` buffer grows as needed.
        unsafe { engine::ungetc(c, self.fp) };
        self.count -= 1;
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
}

/// C's `isspace` in the C locale, which Rust's ASCII test does not match:
/// it leaves out the vertical tab.
fn is_space(c: c_int) -> bool {
    matches!(c, 0x20 | 0x09..=0x0d)
}

fn is_digit_in(c: c_int, base: u32) -> bool {
    c != EOF && (c as u8 as char).is_digit(base)
}

/// Collect an integer in `base` (0 for C's prefixed rule) into `field`.
/// The base it was read in, or `None` for a matching failure.
fn collect_integer(
    input: &mut Input,
    field: &mut Vec<u8>,
    width: usize,
    mut base: u32,
) -> Option<u32> {
    let mut c = input.get();
    if (c == c_int::from(b'+') || c == c_int::from(b'-')) && field.len() < width {
        field.push(c as u8);
        c = input.get();
    }
    let mut digits = false;
    if (base == 0 || base == 16) && c == c_int::from(b'0') && field.len() < width {
        field.push(b'0');
        digits = true;
        c = input.get();
        if (c == c_int::from(b'x') || c == c_int::from(b'X')) && field.len() < width {
            let x = c;
            c = input.get();
            if field.len() + 1 < width && is_digit_in(c, 16) {
                field.push(x as u8);
                base = 16;
            } else {
                // "0x" with no digit after it is the number 0.
                input.unget(c);
                input.unget(x);
                return Some(if base == 0 { 8 } else { base });
            }
        } else if base == 0 {
            base = 8;
        }
    }
    if base == 0 {
        base = 10;
    }
    while field.len() < width && is_digit_in(c, base) {
        field.push(c as u8);
        digits = true;
        c = input.get();
    }
    input.unget(c);
    if !digits {
        input.unget_all(field, 0);
        return None;
    }
    Some(base)
}

/// Take `word` case-insensitively, or push back and refuse.
fn match_word(input: &mut Input, field: &mut Vec<u8>, width: usize, word: &[u8]) -> bool {
    let start = field.len();
    for &w in word {
        let c = match field.len() < width {
            true => input.get(),
            false => EOF,
        };
        if c == EOF || (c as u8).to_ascii_lowercase() != w {
            input.unget(c);
            input.unget_all(field, start);
            return false;
        }
        field.push(c as u8);
    }
    true
}

/// Collect a floating-point number into `field`; false for a matching
/// failure.
fn collect_float(input: &mut Input, field: &mut Vec<u8>, width: usize) -> bool {
    // SAFETY: the current locale's own conventions, NUL-terminated.
    let point = c_int::from(unsafe { *(*libc::localeconv()).decimal_point } as u8);
    let mut c = input.get();
    if (c == c_int::from(b'+') || c == c_int::from(b'-')) && field.len() < width {
        field.push(c as u8);
        c = input.get();
    }
    if matches!(c as u8, b'i' | b'I' | b'n' | b'N') && c != EOF {
        input.unget(c);
        if match_word(input, field, width, b"inf") {
            match_word(input, field, width, b"inity");
            return true;
        }
        if match_word(input, field, width, b"nan") {
            let before = field.len();
            let open = match field.len() < width {
                true => input.get(),
                false => EOF,
            };
            if open == c_int::from(b'(') {
                field.push(b'(');
                let mut d = input.get();
                while field.len() < width
                    && d != EOF
                    && ((d as u8).is_ascii_alphanumeric() || d == c_int::from(b'_'))
                {
                    field.push(d as u8);
                    d = input.get();
                }
                if field.len() < width && d == c_int::from(b')') {
                    field.push(b')');
                } else {
                    input.unget(d);
                    input.unget_all(field, before);
                }
            } else {
                input.unget(open);
            }
            return true;
        }
        input.unget_all(field, 0);
        return false;
    }
    let (mut hex, mut digits) = (false, false);
    if c == c_int::from(b'0') && field.len() < width {
        field.push(b'0');
        digits = true;
        c = input.get();
        if (c == c_int::from(b'x') || c == c_int::from(b'X')) && field.len() < width {
            field.push(c as u8);
            hex = true;
            c = input.get();
        }
    }
    let base = if hex { 16 } else { 10 };
    while field.len() < width && is_digit_in(c, base) {
        field.push(c as u8);
        digits = true;
        c = input.get();
    }
    if c == point && field.len() < width {
        field.push(c as u8);
        c = input.get();
        while field.len() < width && is_digit_in(c, base) {
            field.push(c as u8);
            digits = true;
            c = input.get();
        }
    }
    if !digits {
        input.unget(c);
        input.unget_all(field, 0);
        return false;
    }
    let marker = if hex { b'p' } else { b'e' };
    if c != EOF && (c as u8).to_ascii_lowercase() == marker && field.len() < width {
        let mark = field.len();
        field.push(c as u8);
        c = input.get();
        if (c == c_int::from(b'+') || c == c_int::from(b'-')) && field.len() < width {
            field.push(c as u8);
            c = input.get();
        }
        if field.len() < width && is_digit_in(c, 10) {
            while field.len() < width && is_digit_in(c, 10) {
                field.push(c as u8);
                c = input.get();
            }
        } else {
            // An exponent marker with no digits is not part of the number.
            input.unget(c);
            input.unget_all(field, mark + 1);
            c = c_int::from(field.pop().unwrap_or_default());
        }
    }
    input.unget(c);
    true
}

/// Parse a `%[` scanset after the `[`; the rest of the format after `]`.
fn scanset(mut f: &[u8]) -> ([bool; 256], &[u8]) {
    let mut set = [false; 256];
    let negate = f.first() == Some(&b'^');
    if negate {
        f = &f[1..];
    }
    if f.first() == Some(&b']') {
        set[usize::from(b']')] = true;
        f = &f[1..];
    }
    while let Some(&lo) = f.first() {
        if lo == b']' {
            break;
        }
        match f {
            [_, b'-', hi, ..] if *hi != b']' && *hi >= lo => {
                for c in lo..=*hi {
                    set[usize::from(c)] = true;
                }
                f = &f[3..];
            }
            _ => {
                set[usize::from(lo)] = true;
                f = &f[1..];
            }
        }
    }
    if negate {
        for b in set.iter_mut() {
            *b = !*b;
        }
    }
    (set, f.strip_prefix(b"]").unwrap_or(f))
}

/// Store an integer at `at`, at the width `length` names. Null is skipped.
///
/// # Safety
///
/// `at` is null or writable for the type `length` names.
unsafe fn store_int(at: *mut c_void, length: Length, value: i64) {
    if at.is_null() {
        return;
    }
    // SAFETY: the caller's obligation. Truncation is C's conversion.
    unsafe {
        match length {
            Length::Char => *at.cast::<i8>() = value as i8,
            Length::Short => *at.cast::<c_short>() = value as c_short,
            Length::Long => *at.cast::<c_long>() = value as c_long,
            Length::LongLong | Length::LongDouble | Length::IntMax => *at.cast::<i64>() = value,
            Length::Size => *at.cast::<usize>() = value as usize,
            Length::PtrDiff => *at.cast::<isize>() = value as isize,
            Length::None => *at.cast::<c_int>() = value as c_int,
        }
    }
}

/// Parse and store a `long double`, which is the host's: the guest runs
/// natively, so it shares this architecture's.
///
/// # Safety
///
/// `text` is NUL-terminated and `at` is writable for a `long double`.
unsafe fn store_long_double(at: *mut u8, text: *const c_char) {
    if cfg!(target_arch = "x86_64") {
        // SAFETY: the caller's string; an x87 value is ten bytes.
        unsafe {
            let value = crate::strtofp::strtold_x87(text, std::ptr::null_mut());
            std::ptr::copy_nonoverlapping(value.as_ptr(), at, value.len());
        }
    } else {
        // SAFETY: the caller's obligation; `long double` is `double` here.
        unsafe {
            at.cast::<f64>()
                .write_unaligned(crate::strtofp::strtod(text, std::ptr::null_mut()))
        };
    }
}

/// Where scanf's results go: every conversion's argument is a pointer, so the
/// tail is a source of them, whether a `va_list` or an emulated guest's
/// registers. A null one is skipped.
pub type Destinations<'a> = &'a mut dyn FnMut() -> *mut c_void;

/// Read `format` against the stream, storing through `next`'s pointers. The
/// caller holds the lock.
unsafe fn scan(fp: *mut File, format: *const c_char, next: Destinations) -> c_int {
    let mut input = Input { fp, count: 0 };
    let (mut assigned, mut conversions) = (0, 0);
    // SAFETY: the caller's NUL-terminated format.
    let mut f = unsafe { std::ffi::CStr::from_ptr(format) }.to_bytes();
    let mut field: Vec<u8> = Vec::with_capacity(FIELD + 1);
    // How the scan ended early: `Err(true)` is an input failure, `Err(false)` a
    // matching one.
    let end: Result<(), bool> = 'scan: loop {
        let Some(&first) = f.first() else {
            break Ok(());
        };
        if is_space(c_int::from(first)) {
            while f.first().is_some_and(|&b| is_space(c_int::from(b))) {
                f = &f[1..];
            }
            input.skip_space();
            continue;
        }
        if first != b'%' || f.get(1) == Some(&b'%') {
            if first == b'%' {
                f = &f[1..];
                input.skip_space();
            }
            let c = input.get();
            if c == EOF {
                break Err(true);
            }
            if c != c_int::from(f[0]) {
                input.unget(c);
                break Err(false);
            }
            f = &f[1..];
            continue;
        }
        f = &f[1..];
        let suppress = f.first() == Some(&b'*');
        if suppress {
            f = &f[1..];
        }
        let mut width = 0usize;
        while let Some(&d) = f.first().filter(|d| d.is_ascii_digit()) {
            width = width
                .saturating_mul(10)
                .saturating_add(usize::from(d - b'0'));
            f = &f[1..];
        }
        let (length, skip) = match f {
            [b'h', b'h', ..] => (Length::Char, 2),
            [b'h', ..] => (Length::Short, 1),
            [b'l', b'l', ..] => (Length::LongLong, 2),
            [b'l', ..] => (Length::Long, 1),
            [b'q', ..] => (Length::LongLong, 1),
            [b'j', ..] => (Length::IntMax, 1),
            [b'z', ..] => (Length::Size, 1),
            [b't', ..] => (Length::PtrDiff, 1),
            [b'L', ..] => (Length::LongDouble, 1),
            _ => (Length::None, 0),
        };
        f = &f[skip..];
        let Some(&conv) = f.first() else {
            break Err(false);
        };
        f = &f[1..];
        if !matches!(conv, b'c' | b'C' | b'[' | b'n') && input.skip_space() == EOF {
            break Err(true);
        }
        field.clear();
        // SAFETY (whole match): each destination is the next argument, of the
        // type the conversion names, which is the caller's obligation.
        match conv {
            b'n' => {
                if !suppress {
                    unsafe { store_int(next(), length, input.count) };
                }
            }
            b'd' | b'i' | b'o' | b'u' | b'x' | b'X' | b'p' => {
                let base = match conv {
                    b'd' | b'u' => 10,
                    b'i' => 0,
                    b'o' => 8,
                    _ => 16,
                };
                let limit = if width == 0 { FIELD } else { width.min(FIELD) };
                let Some(base) = collect_integer(&mut input, &mut field, limit, base) else {
                    break Err(false);
                };
                field.push(0);
                if !suppress {
                    let text = field.as_ptr().cast::<c_char>();
                    // SAFETY: a NUL-terminated field of digits.
                    unsafe {
                        let value = match conv {
                            b'd' | b'i' => libc::strtoll(text, std::ptr::null_mut(), base as c_int),
                            _ => libc::strtoull(text, std::ptr::null_mut(), base as c_int) as i64,
                        };
                        let at = next();
                        match conv {
                            b'p' if !at.is_null() => *at.cast::<usize>() = value as usize,
                            b'p' => {}
                            _ => store_int(at, length, value),
                        }
                    }
                    assigned += 1;
                }
                conversions += 1;
            }
            b'a' | b'A' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {
                let limit = if width == 0 { FIELD } else { width.min(FIELD) };
                if !collect_float(&mut input, &mut field, limit) {
                    break Err(false);
                }
                field.push(0);
                if !suppress {
                    let text = field.as_ptr().cast::<c_char>();
                    let at = next();
                    // SAFETY: a NUL-terminated number, parsed at the width
                    // `length` names and stored unless the destination is null.
                    unsafe {
                        use crate::strtofp::{strtod, strtof};
                        let end = std::ptr::null_mut();
                        match length {
                            _ if at.is_null() => _ = strtod(text, end),
                            Length::LongDouble => store_long_double(at.cast(), text),
                            Length::Long => *at.cast::<f64>() = strtod(text, end),
                            _ => *at.cast::<f32>() = strtof(text, end),
                        }
                    }
                    assigned += 1;
                }
                conversions += 1;
            }
            b'c' | b'C' => {
                let width = width.max(1);
                let mut n = 0;
                if conv == b'C' || length == Length::Long {
                    let out = match suppress {
                        true => std::ptr::null_mut(),
                        false => next().cast::<u32>(),
                    };
                    let mut state = [0u64; 16];
                    while n < width {
                        let Some(wc) = input.get_wide(&mut state) else {
                            break;
                        };
                        if !out.is_null() {
                            unsafe { *out.add(n) = wc };
                        }
                        n += 1;
                    }
                } else {
                    let out = match suppress {
                        true => std::ptr::null_mut(),
                        false => next().cast::<u8>(),
                    };
                    while n < width {
                        let c = input.get();
                        if c == EOF {
                            break;
                        }
                        if !out.is_null() {
                            unsafe { *out.add(n) = c as u8 };
                        }
                        n += 1;
                    }
                }
                if n == 0 {
                    break Err(true);
                }
                if !suppress {
                    assigned += 1;
                }
                conversions += 1;
            }
            b's' | b'S' | b'[' => {
                let set = match conv {
                    b'[' => {
                        let (set, rest) = scanset(f);
                        f = rest;
                        set
                    }
                    _ => std::array::from_fn(|c| !is_space(c as c_int)),
                };
                let mut n = 0;
                if conv == b'S' || length == Length::Long {
                    let out = match suppress {
                        true => std::ptr::null_mut(),
                        false => next().cast::<u32>(),
                    };
                    let mut state = [0u64; 16];
                    while width == 0 || n < width {
                        let Some(wc) = input.get_wide(&mut state) else {
                            break;
                        };
                        let in_set = match (wc < 256, conv) {
                            (true, _) => set[wc as usize],
                            (false, b'[') => set[0],
                            // SAFETY: a plain query of the current locale.
                            (false, _) => (unsafe { iswspace(wc) }) == 0,
                        };
                        if !in_set {
                            // Put the whole multibyte sequence back.
                            let mut back = [0u8; 16];
                            let mut fresh = [0u64; 16];
                            // SAFETY: a buffer longer than any sequence.
                            let m = unsafe {
                                wcrtomb(
                                    back.as_mut_ptr().cast(),
                                    wc as c_int,
                                    fresh.as_mut_ptr().cast(),
                                )
                            };
                            if m != usize::MAX {
                                for &b in back[..m].iter().rev() {
                                    input.unget(c_int::from(b));
                                }
                            }
                            break;
                        }
                        if !out.is_null() {
                            unsafe { *out.add(n) = wc };
                        }
                        n += 1;
                    }
                    if n > 0 && !out.is_null() {
                        unsafe { *out.add(n) = 0 };
                    }
                } else {
                    let out = match suppress {
                        true => std::ptr::null_mut(),
                        false => next().cast::<u8>(),
                    };
                    while width == 0 || n < width {
                        let c = input.get();
                        if c == EOF {
                            break;
                        }
                        if !set[c as usize] {
                            input.unget(c);
                            break;
                        }
                        if !out.is_null() {
                            unsafe { *out.add(n) = c as u8 };
                        }
                        n += 1;
                    }
                    if n > 0 && !out.is_null() {
                        unsafe { *out.add(n) = 0 };
                    }
                }
                if n == 0 {
                    // Nothing matched: an input failure only if that was the end.
                    let c = input.get();
                    input.unget(c);
                    break 'scan Err(c == EOF);
                }
                if !suppress {
                    assigned += 1;
                }
                conversions += 1;
            }
            _ => break Err(false),
        }
    };
    match end {
        Err(true) if conversions == 0 => EOF,
        _ => assigned,
    }
}

/// Lock the stream and scan it, in `loc` if given.
unsafe fn scan_locked(
    fp: *mut File,
    loc: Option<*mut c_void>,
    format: *const c_char,
    next: Destinations,
) -> c_int {
    let _locale = InLocale::new(loc);
    // SAFETY: the caller's stream, format and destinations.
    unsafe {
        let _guard = Locked::new(fp);
        engine::orient(fp, -1);
        scan(fp, format, next)
    }
}

/// [`scan_locked`] with a `va_list`'s pointers.
unsafe fn scan_list(
    fp: *mut File,
    loc: Option<*mut c_void>,
    format: *const c_char,
    mut ap: VaList,
) -> c_int {
    // SAFETY: the caller states every conversion has a pointer argument.
    let mut next = || unsafe { ap.next_arg::<*mut c_void>() };
    // SAFETY: the caller's obligations.
    unsafe { scan_locked(fp, loc, format, &mut next) }
}

/// `vfscanf` for a caller that holds the destinations some other way: an
/// emulated guest's registers.
///
/// # Safety
///
/// `stream` is one this module opened, `format` is NUL-terminated, and each
/// pointer `next` yields is null or writable for its conversion.
pub unsafe fn scan_with(
    fp: *mut c_void,
    loc: Option<*mut c_void>,
    format: *const c_char,
    next: Destinations,
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_locked(fp.cast(), loc, format, next) }
}

/// `int vfscanf(FILE *stream, const char *format, va_list ap)`
///
/// # Safety
///
/// `stream` is open and `ap` holds a destination for every assignment.
pub unsafe extern "C" fn vfscanf(fp: *mut File, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_list(fp, None, format, ap) }
}

/// `int vscanf(const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vfscanf`].
pub unsafe extern "C" fn vscanf(format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdin`, and the caller's obligations.
    unsafe { scan_list(STDINP.get(), None, format, ap) }
}

/// `int fscanf(FILE *stream, const char *format, ...)`
///
/// # Safety
///
/// As [`vfscanf`].
pub unsafe extern "C" fn fscanf(fp: *mut File, format: *const c_char, args: ...) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_list(fp, None, format, args) }
}

/// `int scanf(const char *format, ...)`
///
/// # Safety
///
/// As [`vfscanf`].
pub unsafe extern "C" fn scanf(format: *const c_char, args: ...) -> c_int {
    // SAFETY: the guest's `stdin`, and the caller's obligations.
    unsafe { scan_list(STDINP.get(), None, format, args) }
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
    unsafe { scan_list(fp, Some(loc), format, ap) }
}

/// `int vscanf_l(locale_t loc, const char *format, va_list ap)`
///
/// # Safety
///
/// As [`vfscanf_l`].
pub unsafe extern "C" fn vscanf_l(loc: *mut c_void, format: *const c_char, ap: VaList) -> c_int {
    // SAFETY: the guest's `stdin`, and the caller's obligations.
    unsafe { scan_list(STDINP.get(), Some(loc), format, ap) }
}

/// `int fscanf_l(FILE *stream, locale_t loc, const char *format, ...)`
///
/// # Safety
///
/// As [`vfscanf_l`].
pub unsafe extern "C" fn fscanf_l(
    fp: *mut File,
    loc: *mut c_void,
    format: *const c_char,
    args: ...
) -> c_int {
    // SAFETY: the caller's obligations.
    unsafe { scan_list(fp, Some(loc), format, args) }
}

/// `int scanf_l(locale_t loc, const char *format, ...)`
///
/// # Safety
///
/// As [`vfscanf_l`].
pub unsafe extern "C" fn scanf_l(loc: *mut c_void, format: *const c_char, args: ...) -> c_int {
    // SAFETY: the guest's `stdin`, and the caller's obligations.
    unsafe { scan_list(STDINP.get(), Some(loc), format, args) }
}
