//! Darwin's `strtod` family, after Apple's `strtofp.c` (Tim Kientzle's
//! "ffpp_strtofp"), so that a guest parses exactly what macOS would: the same
//! value in every rounding mode, the same end pointer, and the same `errno`.
//!
//! The host's own `strtod` differs in the details a program can see: hex
//! float rounding, NaN payloads, when `ERANGE` is set, and which locale is
//! consulted for the decimal point. The only things taken from the host are
//! the decimal point, `isspace` and the rounding mode, through POSIX calls
//! every host has.
//!
//! Checked against Apple's C over millions of random inputs in every rounding
//! mode. It differs only where that code has bugs, which Darwin's answer still
//! defines and this does not yet reproduce; each is a `FIXME (ACCURACY)` at
//! the point where it would go:
//!
//! * the binary64 fast path's sign-dependent `ERANGE` near the least normal
//!   (`decimal::interval`);
//! * a float80 hex subnormal shifted by 64 bits (`parse`);
//! * the slow path's undersized numerator (`decimal::exact`).

mod big;
mod decimal;
mod format;
mod scan;
mod tables;

use format::{BINARY32, BINARY64, Encoded, FLOAT80, Format, Rounding};
use scan::{Conventions, Number, Text};
use std::cell::OnceCell;
use std::ffi::c_char;

/// Parse one number from `text`: the result and the offset just past it.
fn parse(
    text: &Text,
    format: &Format,
    conventions: &impl Conventions,
    mode: impl FnOnce() -> Rounding,
) -> (Encoded, usize) {
    let scanned = scan::scan(text, conventions);
    let negative = scanned.negative;
    let exact = |bits| Encoded {
        bits,
        range_error: false,
    };
    let encoded = match scanned.number {
        Number::Invalid => exact(format.zero(false)),
        Number::Zero => exact(format.zero(negative)),
        Number::Infinity => exact(format.infinity(negative)),
        Number::Nan(payload) => exact(format.nan(negative, payload)),
        // FIXME (ACCURACY): Darwin's float80 hex parse is wrong for values in
        // [2^-16383, 2^-16382), the top binade of x87 subnormals. In
        // `hexFloat`, `fractionBits = 127 - (base2Exponent - minBinaryExp +
        // sigBits - 1)` is exactly 64 there, and the `fractionBits >= 64`
        // branch then computes `significand_msw << (128 - fractionBits)` and
        // `significand_lsw << (128 - fractionBits)`: shifts of a `uint64_t` by
        // 64, which C leaves undefined. x86-64 `shl` masks the count to 0, so
        // with the significand normalized to 127 bits in (msw, lsw) the
        // discarded fraction reads as `msw | lsw`, ORed with 1 if `lsw != 0`.
        // `msw` is never zero, so every such input - even an exact one like
        // "0x.fcb3p-16382" - sets `ERANGE`, and FE_UPWARD / FE_DOWNWARD add one
        // unit away from zero on the side they round toward; FE_TONEAREST
        // rounds up when that fraction exceeds 2^63 or ties with an odd
        // significand. To match, round this one binade with that fraction for
        // FLOAT80 only. Measured against the C built by GCC on x86-64 Linux;
        // confirm the shift really masks in Apple's clang build of Libc before
        // copying it.
        Number::Hex(value) => format.round(negative, value, mode(), true),
        Number::Decimal(decimal) => decimal::convert(format, negative, &decimal, mode()),
    };
    (encoded, scanned.end)
}

/// Whose decimal point a parse uses.
#[derive(Clone, Copy)]
enum Locale {
    /// Darwin's null `locale_t`, which its `_l` functions read as "C".
    C,
    /// The calling thread's, as the plain functions use.
    Current,
    /// An explicit `locale_t`, possibly `LC_GLOBAL_LOCALE`.
    Explicit(libc::locale_t),
}

impl Locale {
    fn of(loc: libc::locale_t) -> Locale {
        if loc.is_null() {
            Locale::C
        } else {
            Locale::Explicit(loc)
        }
    }
}

/// The host's answers for one call.
struct Host<'a> {
    locale: Locale,
    point: OnceCell<&'a [u8]>,
}

impl Conventions for Host<'_> {
    fn is_space(&self, c: u8) -> bool {
        // Darwin asks the current locale even of an `_l` call.
        // SAFETY: `isspace` is defined for every `unsigned char` value.
        unsafe { libc::isspace(c.into()) != 0 }
    }

    fn decimal_point(&self) -> &[u8] {
        self.point.get_or_init(|| {
            // `nl_langinfo` answers for the thread's locale, so an explicit one
            // is selected for the question rather than relying on each host
            // having `nl_langinfo_l` and accepting `LC_GLOBAL_LOCALE` in it.
            let _switched = match self.locale {
                Locale::C => return b".",
                Locale::Current => None,
                Locale::Explicit(loc) => crate::xlocale::switch(loc),
            };
            // SAFETY: a query. The string belongs to the locale, which the
            // caller keeps alive for the call, and nothing else asks
            // `nl_langinfo` before the parse is done with it. Measured by hand
            // because it is a byte or two, where a `strlen` call costs more
            // than the rest of a short parse.
            unsafe {
                let point = libc::nl_langinfo(libc::RADIXCHAR).cast::<u8>();
                if point.is_null() {
                    return b".";
                }
                let mut len = 0;
                while *point.add(len) != 0 {
                    len += 1;
                }
                std::slice::from_raw_parts(point, len)
            }
        })
    }
}

/// The shared body of every entry point: parse, set `errno`, store the end.
///
/// # Safety
///
/// `nptr` is NUL-terminated; `endptr` is null or writable; `locale` names a
/// live `locale_t` if it names one at all.
unsafe fn strto(
    format: &Format,
    nptr: *const c_char,
    endptr: *mut *mut c_char,
    locale: Locale,
) -> u128 {
    // SAFETY: the caller's string.
    let text = unsafe { Text::new(nptr.cast()) };
    let host = Host {
        locale,
        point: OnceCell::new(),
    };
    let (encoded, end) = parse(&text, format, &host, Rounding::current);
    if encoded.range_error {
        crate::errno::set(libc::ERANGE);
    }
    if !endptr.is_null() {
        // SAFETY: `end` is within the string; `endptr` is the caller's.
        unsafe { *endptr = nptr.add(end).cast_mut() };
    }
    encoded.bits
}

/// `double strtod(const char *, char **)`
///
/// # Safety
///
/// `nptr` is NUL-terminated and `endptr` is null or writable.
pub unsafe extern "C" fn strtod(nptr: *const c_char, endptr: *mut *mut c_char) -> f64 {
    // SAFETY: the caller's obligation.
    f64::from_bits(unsafe { strto(&BINARY64, nptr, endptr, Locale::Current) } as u64)
}

/// `double strtod_l(const char *, char **, locale_t)`
///
/// # Safety
///
/// As [`strtod`], and `loc` is null or a live `locale_t`.
pub unsafe extern "C" fn strtod_l(
    nptr: *const c_char,
    endptr: *mut *mut c_char,
    loc: libc::locale_t,
) -> f64 {
    // SAFETY: the caller's obligation.
    f64::from_bits(unsafe { strto(&BINARY64, nptr, endptr, Locale::of(loc)) } as u64)
}

/// `float strtof(const char *, char **)`
///
/// # Safety
///
/// As [`strtod`].
pub unsafe extern "C" fn strtof(nptr: *const c_char, endptr: *mut *mut c_char) -> f32 {
    // SAFETY: the caller's obligation.
    f32::from_bits(unsafe { strto(&BINARY32, nptr, endptr, Locale::Current) } as u32)
}

/// `float strtof_l(const char *, char **, locale_t)`
///
/// # Safety
///
/// As [`strtod_l`].
pub unsafe extern "C" fn strtof_l(
    nptr: *const c_char,
    endptr: *mut *mut c_char,
    loc: libc::locale_t,
) -> f32 {
    // SAFETY: the caller's obligation.
    f32::from_bits(unsafe { strto(&BINARY32, nptr, endptr, Locale::of(loc)) } as u32)
}

/// `double atof(const char *)`, which Darwin defines as `strtod` with no end
/// pointer, `errno` included.
///
/// # Safety
///
/// `nptr` is NUL-terminated.
pub unsafe extern "C" fn atof(nptr: *const c_char) -> f64 {
    // SAFETY: the caller's obligation.
    unsafe { strtod(nptr, std::ptr::null_mut()) }
}

/// `long double strtold(const char *, char **)` where `long double` is
/// `double`, as it is on arm64 Darwin.
///
/// # Safety
///
/// As [`strtod`].
#[cfg(target_arch = "aarch64")]
pub unsafe extern "C" fn strtold(nptr: *const c_char, endptr: *mut *mut c_char) -> f64 {
    // SAFETY: the caller's obligation.
    unsafe { strtod(nptr, endptr) }
}

/// `long double strtold_l(const char *, char **, locale_t)`, as [`strtold`].
///
/// # Safety
///
/// As [`strtod_l`].
#[cfg(target_arch = "aarch64")]
pub unsafe extern "C" fn strtold_l(
    nptr: *const c_char,
    endptr: *mut *mut c_char,
    loc: libc::locale_t,
) -> f64 {
    // SAFETY: the caller's obligation.
    unsafe { strtod_l(nptr, endptr, loc) }
}

/// x86-64 Darwin's `strtold`, as the ten bytes of an x87 extended value.
///
/// Not exported: Rust cannot return a value in `st(0)`, so this serves the
/// callers inside the crate that store one.
///
/// # Safety
///
/// As [`strtod`].
pub(crate) unsafe fn strtold_x87(nptr: *const c_char, endptr: *mut *mut c_char) -> [u8; 10] {
    // SAFETY: the caller's obligation.
    let bits = unsafe { strto(&FLOAT80, nptr, endptr, Locale::Current) };
    let mut out = [0; 10];
    out.copy_from_slice(&bits.to_le_bytes()[..10]);
    out
}

exports::exports! {
    functions {
        "atof" => atof,
        "strtod" => strtod,
        "strtod_l" => strtod_l,
        "strtof" => strtof,
        "strtof_l" => strtof_l,
        #[cfg(target_arch = "aarch64")]
        "strtold" => strtold,
        #[cfg(target_arch = "aarch64")]
        "strtold_l" => strtold_l,
    }
}

#[cfg(test)]
mod tests;
