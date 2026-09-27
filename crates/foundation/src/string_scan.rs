//! `-intValue`, `-integerValue`, `-doubleValue` and `-boolValue`, as pure
//! functions of a string's UTF-16 code units.
//!
//! **Why this is a separate file, and why the sources are named.** These four
//! selectors are the ones where a *plausible* answer and the *right* answer are
//! different numbers, and where nothing downstream can tell them apart. Chromium
//! sends `-intValue` 41 times and `-boolValue` 31 times in the Framework image,
//! and the results are `test`ed or compared into branches: an implementation
//! that returns `0` where the real one returns `INT_MAX`, or `NO` where it
//! returns `YES`, produces a run that succeeds and a browser that is quietly
//! misconfigured. So each function below is a line-by-line port of the parser
//! Apple actually runs, and the port's structure is kept - saturating
//! accumulator, two-stage promotion, character set bitmap, `strtod` - because
//! every one of those is a case where a "reasonable" reimplementation is
//! wrong.
//!
//! The two sources are:
//!
//! * `__CFStringScanInteger`, `__CFStringScanDouble` and the `__CFNumberSet`
//!   bitmap in **`CFStringScanner.c`**, from Apple's open-source CoreFoundation
//!   (`opensource-apple/CF`, APSL 2.0). `CFStringGetIntValue` and
//!   `CFStringGetDoubleValue` in `CFString.c` are thin wrappers over them, and
//!   `__NSCFString` *is* a CoreFoundation string, so this is not a
//!   reimplementation from a spec - it is the code.
//! * the `-boolValue` clause, which is `NSString.h`'s own comment:
//!
//!   ```c
//!   /* The following convenience methods all skip initial space characters
//!      (whitespaceSet) and ignore trailing characters. NSScanner can be used
//!      for more "exact" parsing of numbers. */
//!   - (double)doubleValue;
//!   - (float)floatValue;
//!   - (int)intValue;
//!   - (NSInteger)integerValue;
//!   - (long long)longLongValue;
//!   - (BOOL)boolValue NS_AVAILABLE(10_5, 2_0);  // Skips initial space
//!       characters (whitespaceSet), or optional -/+ sign followed by zeroes.
//!       Returns YES on encountering one of "Y", "y", "T", "t", or a digit
//!       1-9. It ignores any trailing characters.
//!   ```
//!
//! **Every argument is a UTF-16 code unit, not a byte.** The scanners read
//! `UniChar`s out of a `CFStringInlineBuffer`, and `-length` counts code units,
//! so an index or a limit the caller passes is in the same unit. Decoding to
//! UTF-8 first would change the answer for anything outside ASCII and would
//! make a truncated multi-byte sequence a parse error rather than a non-digit.

/// Which width `-intValue` saturates at, and which `-integerValue` and
/// `-longLongValue` saturate at.
///
/// `CFStringGetIntValue` calls the scanner with `doLonglong` **false**, so
/// `-intValue` accumulates in a C `int` and clamps at `INT_MAX`/`INT_MIN`. It
/// does *not* accumulate in 64 bits and truncate: on a 64-bit platform
/// `[@"99999999999" intValue]` is `2147483647`, not `1215752191`. This is the
/// question a "parse as i64 then cast" implementation gets wrong, and it is
/// why the width is a parameter of the port rather than a cast at the end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Width {
    /// `-intValue`: a 32-bit accumulator, saturating at `INT_MIN`/`INT_MAX`.
    Int,
    /// `-integerValue` and `-longLongValue`: a 64-bit accumulator, saturating
    /// at `i64::MIN`/`i64::MAX`. The 32-bit accumulator is still used first,
    /// and promoted on the first digit that does not fit in it.
    LongLong,
}

/// `+[NSCharacterSet whitespaceCharacterSet]`, which is what `NSString.h`
/// names in `-boolValue`'s own comment ("Skips initial space characters
/// (whitespaceSet)").
///
/// General Category `Zs` plus `U+0009`. **Not**
/// `+[NSCharacterSet whitespaceAndNewlineCharacterSet]`: the header names
/// `whitespaceSet`, and a leading newline is therefore *not* skipped, so
/// `[@"\n5" intValue]` is `0` and not `5`.
///
/// This is not the same as Rust's `str::trim_start`, whose predicate is the
/// whole Unicode `White_Space` property: that set also contains
/// `U+000A`..`U+000D`, `U+0085`, `U+2028` and `U+2029`, and using it would make
/// this crate answer `5` where Foundation answers `0`.
pub fn is_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x0009
            | 0x0020
            | 0x00A0
            | 0x1680
            | 0x2000..=0x200A
            | 0x202F
            | 0x205F
            | 0x3000
    )
}

/// `__CFCharacterIsADigit`.
fn is_digit(unit: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&unit)
}

/// `__CFStringGetCharacterFromInlineBufferAux` past the end: 0, which is not a
/// digit, not whitespace and not in any number set, so every loop below stops
/// where CF's would.
fn at(units: &[u16], index: usize) -> u16 {
    units.get(index).copied().unwrap_or(0)
}

/// `__CFStringGetFirstNonSpaceCharacterFromInlineBuffer`: skip whitespace,
/// leave `index` on the first unit that is not whitespace, and return it.
fn next_non_space(units: &[u16], index: &mut usize) -> u16 {
    let mut unit = at(units, *index);
    while is_whitespace(unit) {
        *index += 1;
        unit = at(units, *index);
    }
    unit
}

/// `__CFStringScanInteger`: the leading integer of `units`, or `None` when no
/// conversion is possible - which every caller turns into zero, because
/// `CFStringGetIntValue` is `success ? result : 0`.
///
/// The three clauses of the contract, in the order CF applies them:
///
/// 1. **skip initial `whitespaceSet` characters**;
/// 2. **an optional `-` or `+`, and then whitespace is skipped again** -
///    `[@"- 5" integerValue]` is `-5`, because the sign is consumed and the
///    scanner then asks for the first non-space character again;
/// 3. **digits, accumulating, and saturating rather than wrapping**.
///
/// Clause 3 is the one an implementation gets wrong by accident. The C code
/// checks for overflow *before* the multiply and, on overflow, either promotes
/// the 32-bit accumulator to 64-bit (`doLonglong`) or clamps and *keeps
/// consuming digits* so that `-5` is reported as `-INT_MAX` and not as
/// `-INT_MAX` shifted by the remaining digits. The promotion deliberately does
/// not consume its digit: the loop re-reads it into the wide accumulator.
pub fn scan_integer(units: &[u16], width: Width) -> Option<i64> {
    const INT_MAX: i64 = i32::MAX as i64;
    const LLONG_MAX: i64 = i64::MAX;

    let mut index = 0usize;
    let mut unit = next_non_space(units, &mut index);
    let mut negative = false;
    if unit == b'-' as u16 || unit == b'+' as u16 {
        negative = unit == b'-' as u16;
        index += 1;
        unit = next_non_space(units, &mut index);
    }
    // "No digits, bail out..." - and the caller answers zero, which is *not*
    // the same answer as a parsed zero only in the sense that a caller cannot
    // see the difference. That is the point: Foundation has no error here.
    if !is_digit(unit) {
        return None;
    }

    let mut narrow: i64 = 0;
    let mut wide: i64 = 0;
    let mut promoted = false;
    loop {
        let digit = i64::from(unit - b'0' as u16);
        // The C overflow test writes the candidate as `digit - (neg ? 1 : 0)`:
        // a negative number is one closer to its limit, which is why the sign
        // is in the test and not in the accumulator.
        let candidate = digit - i64::from(negative);
        if promoted {
            if wide >= LLONG_MAX / 10 && (wide > LLONG_MAX / 10 || candidate >= LLONG_MAX - wide * 10)
            {
                wide = if negative { i64::MIN } else { LLONG_MAX };
                negative = false;
                // Skip the remaining digits: the value is already decided, and
                // letting them shift it is the wrapping this exists to prevent.
                // C's `while (isDigit(ch = buf[++i]));` leaves `i` on the first
                // non-digit, which is what the `index += 1` before the loop and
                // the walk reproduce.
                index += 1;
                while is_digit(at(units, index)) {
                    index += 1;
                }
                break;
            }
            wide = wide * 10 + digit;
            index += 1;
            unit = at(units, index);
        } else if narrow >= INT_MAX / 10 && (narrow > INT_MAX / 10 || candidate >= INT_MAX - narrow * 10)
        {
            if width == Width::LongLong {
                // Promote, and re-read *this* digit in the wide accumulator: the
                // C `do { } while` re-tests its condition with `ch` unchanged,
                // so the digit that caused the promotion is the first one the
                // 64-bit accumulator sees.
                wide = narrow;
                promoted = true;
            } else {
                narrow = if negative { i32::MIN as i64 } else { INT_MAX };
                negative = false;
                index += 1;
                while is_digit(at(units, index)) {
                    index += 1;
                }
                break;
            }
        } else {
            narrow = narrow * 10 + digit;
            index += 1;
            unit = at(units, index);
        }
        if !is_digit(unit) {
            break;
        }
    }

    Some(if width == Width::LongLong {
        if !promoted {
            wide = narrow;
        }
        if negative {
            -wide
        } else {
            wide
        }
    } else if negative {
        -narrow
    } else {
        narrow
    })
}

/// The bitmap `__CFStringScanDouble` collects through, transcribed from
/// `CFStringScanner.c`.
///
/// It admits exactly fifteen characters: `( ) * + , - . /`, the digits `0`-`9`,
/// and `E`/`e`. That is
///
/// ```text
/// ( ) * + , - . /  0 1 2 3 4 5 6 7  8 9  E  e
/// ```
///
/// and nothing else, which is a much narrower set than "looks like a number"
/// suggests. Four consequences are worth stating, because each is a case where
/// a reimplementation diverges:
///
/// * **`x` is absent**, so `strtod`'s C99 hexadecimal form is unreachable:
///   `[@"0x10" doubleValue]` is `0`, not `16`.
/// * **`i`, `n` and `a` are absent**, so `strtod`'s `inf`, `infinity` and `nan`
///   are unreachable: `[@"inf" doubleValue]` is `0`, not `+inf`.
/// * **space is absent**, so a run can never contain one and `strtod` never has
///   to stop at a space: `[@"3 14" doubleValue]` is `3`. Note that this is
///   *not* the same as "whitespace is skipped" - it is skipped by the separate
///   leading `__CFStringGetFirstNonSpaceCharacterFromInlineBuffer` call, and only
///   the `whitespaceSet` characters, so `[@"\n3.14" doubleValue]` is `0`.
/// * **`#`, `%` and `&` are absent**, which is easy to assume otherwise from the
///   row that holds them looking like a printable run.
///
/// The transcription was checked by extracting the sixteen initialisers out of
/// the upstream file line by line and diffing the row values, after a first
/// hand-copy silently dropped a row and shifted every row after it - which would
/// have made `[@"2" doubleValue]` answer `0` and every digit from `2` onwards
/// unreachable. A self-consistent table passes any test; only the diff against
/// the source finds that.
const NUMBER_SET: [u8; 16] = [
    0x00, // nul soh stx etx eot enq ack bel
    0x00, // bs  ht  nl  vt  np  cr  so  si
    0x00, // dle dc1 dc2 dc3 dc4 nak syn etb
    0x00, // can em  sub esc fs  gs  rs  us
    0x00, // sp  !   "   #   $   %   &   '
    0x68, // (   )   *   +   ,   -   .   /
    0xFF, // 0   1   2   3   4   5   6   7
    0x03, // 8   9   :   ;   <   =   >   ?
    0x20, // @   A   B   C   D   E   F   G
    0x00, // H   I   J   K   L   M   N   O
    0x00, // P   Q   R   S   T   U   V   W
    0x00, // X   Y   Z   [   \   ]   ^   _
    0x20, // `   a   b   c   d   e   f   g
    0x00, // h   i   j   k   l   m   n   o
    0x00, // p   q   r   s   t   u   v   w
    0x00, // x   y   z   {   |   }   ~   del
];

/// `(NUMBER_SET[ch >> 3] & (1 << (ch & 7))) != 0`, for the ASCII range only -
/// the C test is guarded by `ch < 128`.
fn in_number_set(byte: u8) -> bool {
    NUMBER_SET[usize::from(byte) >> 3] & (1 << (byte & 7)) != 0
}

// libc's `strtod`.  This is the call CFStringScanner.c makes, in the C locale -
// see `strtod_c` below for why that is spelled this way and not `strtod_l`.
unsafe extern "C" {
    fn strtod(nptr: *const std::ffi::c_char, endptr: *mut *mut std::ffi::c_char) -> f64;
}

/// `strtod` in the C locale, on a run already restricted to `NUMBER_SET`.
///
/// Returns the value and how many bytes it consumed; **zero consumed is the
/// "no conversion" case** and the caller answers `0.0` for it, which is what
/// `CFStringGetDoubleValue`'s `success ? result : 0.0` does.
///
/// **Why libc's `strtod` and not a reimplementation of it.** CFStringScanner.c
/// makes the call
///
/// ```c
/// result = strtod_l(charPtr, &endCharPtr, NULL);
/// ```
///
/// and the `NULL` is the point: it pins the parse to the **C locale** so the
/// answer does not depend on the user's locale, which is the same reason
/// `NSString.h` says these convenience methods "are not locale-aware". So the
/// contract is "the C library's `strtod` on this byte run", and the honest way
/// to implement it is to make that call. A hand-written grammar for `strtod`'s
/// C99 number syntax was tried first and is *not* equivalent: it cannot produce
/// a subnormal, so it answered `0` for `1e-320`, `5e-324` and
/// `2.2250738585072011e-308`, where the real function answers the smallest
/// representable values. Those three were found by diffing against libc over a
/// 124-input corpus (`test/tools/strtod-diff.md`), not by reading the source.
///
/// `strtod_l` itself is not callable here: on glibc it dereferences the locale
/// argument, so CF's own `strtod_l(buf, &end, NULL)` **segfaults** on this host
/// (verified, `gcc -O0` + a two-line program). Plain `strtod` is the same
/// function in the same locale, because a Rust process does not call
/// `setlocale` and therefore is in the C locale already - which is precisely
/// what the `NULL` was asking for.
///
/// The C99 hexadecimal and the `inf`/`infinity`/`nan` forms are unreachable
/// here, not unimplemented: `x`, `i`, `n` and `a` are not in `NUMBER_SET`, so no
/// run this function can be handed contains the letter any of them needs. That is
/// a *narrowing* of `strtod`, and it is what makes `[@"0x10" doubleValue]` `0`
/// rather than `16`. Space is not in the set either, so the run never starts
/// with whitespace and `strtod`'s own leading-whitespace rule never fires -
/// which is why the one place whitespace matters is the separate
/// `whitespaceSet` skip in [`number_run`].
///
/// No NUL check is needed before the terminator: every byte in `NUMBER_SET` is
/// printable ASCII, so a collected run cannot contain a `\0`.
pub fn strtod_c(run: &[u8]) -> (f64, usize) {
    let mut buffer = run.to_vec();
    buffer.push(0);
    let mut end: *mut std::ffi::c_char = std::ptr::null_mut();
    // SAFETY: `buffer` is a live, NUL-terminated byte buffer, which is the whole
    // of `strtod`'s contract on the pointer, and `end` is a writable slot.
    let value = unsafe { strtod(buffer.as_ptr().cast(), &mut end) };
    let consumed = unsafe { end.offset_from(buffer.as_ptr().cast()) } as usize;
    (value, consumed)
}

pub fn scan_double(units: &[u16]) -> f64 {
    let run = number_run(units);
    let (value, consumed) = strtod_c(&run);
    if consumed == 0 {
        0.0
    } else {
        value
    }
}

/// The run `__CFStringScanDouble` hands to `strtod`: initial `whitespaceSet`
/// skipped, then the longest run of `NUMBER_SET` characters.
///
/// Split out from [`scan_double`] because it is the half of the contract a
/// differential test against libc can check directly: libc does not have
/// `__CFNumberSet`, so the collection has to be pinned by this crate, and only
/// the `strtod` that follows it can be pinned against the real thing. The two
/// steps are separate contracts, and conflating them is how
/// `[@"\n3.14" doubleValue]` becomes `3.14` instead of `0`.
pub fn number_run(units: &[u16]) -> Vec<u8> {
    let mut index = 0usize;
    next_non_space(units, &mut index);
    let mut run: Vec<u8> = Vec::new();
    while let Some(unit) = units.get(index).copied() {
        let byte = unit as u32;
        if byte >= 128 || !in_number_set(byte as u8) {
            break;
        }
        run.push(byte as u8);
        index += 1;
    }
    run
}

/// `-boolValue`, clause by clause, from the comment quoted at the top of this
/// file. There is no `__CFStringScanBoolean`: this is Foundation's own, and the
/// header is the only contract for it.
///
/// 1. **skip initial `whitespaceSet` characters**;
/// 2. **`t`, `T`, `y` or `Y` now means yes**;
/// 3. **consume one optional `+` or `-`**;
/// 4. **consume any following zeroes**;
/// 5. **yes iff the next character is `1`..=`9`**, and no otherwise.
///
/// The zeroes in clause 4 are what make the method's two documented halves fit
/// together: they are consumed whether or not a sign was, so `[@"0" boolValue]`
/// is `NO` (the zeroes are eaten, nothing is left) *and* `[@"01" boolValue]` is
/// `YES` (the `0` is eaten, the `1` answers). Trailing characters are ignored,
/// which is what makes this a whole-string test on a *prefix*.
///
/// The consequence for this runtime's measured path: every one of the five
/// Chromium Helper bundles carries `<key>LSUIElement</key><string>1</string>`,
/// and the guest reads it as
/// `[[[NSBundle mainBundle] infoDictionary] objectForKey:@"LSUIElement"] boolValue`
/// (`Framework+0x5d929e0`, called from `Framework+0x8912176`, whose result is
/// `test`ed at `+0x891217b`). Clause 5 answers **`YES`**. An implementation that
/// answered `NO` would be returning the same byte as a correct `NO`, would
/// record "this is not a background-only app" for a bundle that is one, and the
/// run would go on to succeed.
pub fn bool_value(units: &[u16]) -> bool {
    let mut index = 0usize;
    let unit = next_non_space(units, &mut index);
    if matches!(unit, c if c == b't' as u16
        || c == b'T' as u16
        || c == b'y' as u16
        || c == b'Y' as u16)
    {
        return true;
    }
    if unit == b'+' as u16 || unit == b'-' as u16 {
        index += 1;
    }
    while at(units, index) == b'0' as u16 {
        index += 1;
    }
    let unit = at(units, index);
    (b'1' as u16..=b'9' as u16).contains(&unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// The one input the measured run depends on, and the whole neighbourhood
    /// around it, because the answer is a byte and the wrong `NO` is invisible.
    #[test]
    fn bool_value_is_the_plist_spelling_of_true() {
        for (text, expected) in [
            ("1", true),      // <string>1</string>, the LSUIElement of all five helpers
            ("0", false),     //
            ("01", true),     // zeroes eaten, then a 1-9
            ("00", false),    // zeroes eaten, nothing left
            ("+1", true),     // sign, no zeroes, a 1-9
            ("-1", true),     // the sign does not make it false
            ("+0", false),    //
            ("  1", true),    // initial whitespaceSet
            ("\t9", true),    //
            ("\n1", false),   // a newline is NOT in whitespaceSet
            ("y", true),
            ("Y", true),
            ("t", true),
            ("T", true),
            ("true", true),   // trailing characters ignored
            ("Txyz", true),
            ("yes please", true),
            ("no", false),
            ("f", false),
            ("false", false),
            ("", false),
            ("   ", false),
            ("2", true),
            ("9", true),
            ("x1", false),    // a 1 that is not at the front
            ("0x1", false),
            ("-0", false),
        ] {
            assert_eq!(bool_value(&units(text)), expected, "boolValue({text:?})");
        }
    }

    /// `-intValue` saturates in 32 bits and `-integerValue` in 64. The pair is
    /// one test because the difference between them *is* the thing being pinned.
    #[test]
    fn integer_accessors_saturate_at_their_own_width() {
        // Inputs with no conversion at all.  `CFStringGetIntValue` is
        // `success ? result : 0`, so `None` becomes 0 one call up - and that is
        // the only reason the two are different things, so it is pinned here at
        // the level where the distinction exists.
        for text in ["", "abc", "-", "+", "   ", "-  +"] {
            assert_eq!(scan_integer(&units(text), Width::Int), None, "{text:?}");
            assert_eq!(
                scan_integer(&units(text), Width::LongLong),
                None,
                "{text:?}"
            );
        }
        for (text, narrow, wide) in [
            ("1", 1, 1),
            ("-1", -1, -1),
            (" 12abc", 12, 12),

            ("- 5", -5, -5),        // whitespace is skipped again after the sign
            ("-   5", -5, -5),
            ("0007", 7, 7),
            // 2147483647 is INT_MAX; one more digit is past it.
            ("2147483647", 2147483647, 2147483647),
            ("2147483648", 2147483647, 2147483648),
            ("-2147483648", -2147483648, -2147483648),
            ("-2147483649", -2147483648, -2147483649),
            // 99999999999 wraps to 1215752191 if the width is not honoured.
            ("99999999999", 2147483647, 99999999999),
            ("-99999999999", -2147483648, -99999999999),
            ("9223372036854775807", 2147483647, 9223372036854775807),
            // One past LLONG_MAX, which saturates rather than wrapping negative.
            ("9223372036854775808", 2147483647, 9223372036854775807),
            ("99999999999999999999", 2147483647, 9223372036854775807),
            ("-99999999999999999999", -2147483648, -9223372036854775808),
            // The 32-bit accumulator is used first and promoted on the first
            // digit that does not fit, so the crossover itself is a case.
            ("21474836480", 2147483647, 21474836480),
        ] {
            assert_eq!(
                scan_integer(&units(text), Width::Int),
                Some(narrow),
                "intValue({text:?})"
            );
            assert_eq!(
                scan_integer(&units(text), Width::LongLong),
                Some(wide),
                "integerValue({text:?})"
            );
        }
    }

    /// What the collection-then-`strtod` shape buys, and what it costs.
    #[test]
    fn double_value_collects_a_run_before_it_parses() {
        for (text, expected) in [
            ("0", 0.0),
            ("3.14", 3.14),
            ("  3.14", 3.14),        // initial whitespaceSet is skipped
            ("\t-2.5", -2.5),
            ("1.", 1.0),             // strtod consumes the '.'
            (".5", 0.5),
            ("-.5", -0.5),
            ("1e", 1.0),             // an exponent with no digit is not part of it
            ("1e+", 1.0),
            ("1e5", 100000.0),
            ("1E5", 100000.0),
            ("1.5e-3", 0.0015),
            ("1e999", f64::INFINITY), // HUGE_VAL, not an error
            ("0x10", 0.0),           // 'x' is not in __CFNumberSet
            ("inf", 0.0),            // 'i' is not in __CFNumberSet
            ("nan", 0.0),            // 'n' is not in __CFNumberSet
            ("-inf", 0.0),
            ("NaN", 0.0),            // 'N' is not in __CFNumberSet
            ("abc", 0.0),
            ("", 0.0),
            (".", 0.0),              // a '.' alone converts nothing
            ("12abc", 12.0),
            ("1.2.3", 1.2),         // the run is "1.2.3"; strtod stops at the 2nd '.'
            ("3 14", 3.0),           // space is NOT in the set: the run is "3"
            ("\n3.14", 0.0),         // a newline is not whitespaceSet either
            ("1#2", 1.0),            // nor is '#'
            ("1&2", 1.0),            // nor is '&'
            ("1%2", 1.0),            // nor is '%'
            ("1,5", 1.0),            // nor is ',' - so no thousands separators
            ("1/2", 1.0),            // nor is '/'
            ("()", 0.0),             // nor '(', so an empty run and no conversion
            (",5", 0.0),             // ',' is not in the set: the run is empty
            ("+2", 2.0),
            ("-0", -0.0),
            ("--1", 0.0),            // one sign only
        ] {
            assert_eq!(scan_double(&units(text)), expected, "doubleValue({text:?})");
        }
    }

    /// A non-ASCII unit is a non-digit, which is a claim about *where* the scan
    /// stops, not only about the value. `-length` counts code units, so the
    /// scan has to be in them.
    #[test]
    fn the_scan_is_over_code_units_not_bytes() {
        // U+FF11 FULLWIDTH DIGIT ONE is not an ASCII digit, so there is no
        // conversion at all - the IMP turns that into 0, the scanner says None.
        assert_eq!(scan_integer(&units("\u{ff11}7"), Width::Int), None);
        assert_eq!(scan_double(&units("\u{ff11}.5")), 0.0);
        // A supplementary character before an ASCII digit: its surrogate half is
        // not a digit, so the same.
        assert_eq!(scan_integer(&units("\u{1f600}5"), Width::Int), None);
        // ...and after one, so the run ends at it.
        assert_eq!(scan_integer(&units("12\u{1f600}"), Width::Int), Some(12));
        // U+00A0 NO-BREAK SPACE is in whitespaceSet, unlike U+2028.
        assert!(is_whitespace(0x00A0));
        assert!(!is_whitespace(0x2028));
        assert!(!is_whitespace(0x000A));
    }
}
