//! The Date, Time and Measurement Utilities conversions between Core
//! Foundation's absolute time and the File Manager's `UTCDateTime`.
//!
//! <https://developer.apple.com/documentation/coreservices/1435838-ucconvertcfabsolutetimetoutcdat>

/// `kCFAbsoluteTimeIntervalSince1904`: from 1904-01-01, the `UTCDateTime`
/// epoch, to 2001-01-01, Core Foundation's.
const SINCE_1904: f64 = 3_061_152_000.0;

/// `paramErr`.
const PARAM_ERR: i32 = -50;
/// `kUTCUnderflowErr`: a time before 1904.
const UTC_UNDERFLOW_ERR: i32 = -8850;
/// `kUTCOverflowErr`: a time past what 48 bits of seconds count.
const UTC_OVERFLOW_ERR: i32 = -8851;

/// `UTCDateTime`, packed to two bytes as `<MacTypes.h>` declares it.
#[repr(C, packed(2))]
#[derive(Clone, Copy, Default)]
pub struct UtcDateTime {
    pub high_seconds: u16,
    pub low_seconds: u32,
    pub fraction: u16,
}

const _: () = assert!(size_of::<UtcDateTime>() == 8);

fn to_utc(time: f64) -> Result<UtcDateTime, i32> {
    let since = time + SINCE_1904;
    if since.is_nan() || since < 0.0 {
        return Err(UTC_UNDERFLOW_ERR);
    }
    let seconds = since.floor();
    if seconds >= (1u64 << 48) as f64 {
        return Err(UTC_OVERFLOW_ERR);
    }
    let whole = seconds as u64;
    let fraction = (((since - seconds) * 65536.0) as u32).min(0xffff) as u16;
    Ok(UtcDateTime {
        high_seconds: (whole >> 32) as u16,
        low_seconds: whole as u32,
        fraction,
    })
}

fn from_utc(utc: UtcDateTime) -> f64 {
    let whole = (u64::from(utc.high_seconds) << 32) | u64::from(utc.low_seconds);
    whole as f64 + f64::from(utc.fraction) / 65536.0 - SINCE_1904
}

/// `OSStatus UCConvertCFAbsoluteTimeToUTCDateTime(CFAbsoluteTime, UTCDateTime *)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn UCConvertCFAbsoluteTimeToUTCDateTime(
    time: f64,
    out: *mut UtcDateTime,
) -> i32 {
    if out.is_null() {
        return PARAM_ERR;
    }
    match to_utc(time) {
        Ok(utc) => {
            // SAFETY: the caller's `UTCDateTime`, which is two-byte aligned.
            unsafe { out.write_unaligned(utc) };
            0
        }
        Err(status) => status,
    }
}

/// `OSStatus UCConvertUTCDateTimeToCFAbsoluteTime(const UTCDateTime *,
/// CFAbsoluteTime *)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn UCConvertUTCDateTimeToCFAbsoluteTime(
    utc: *const UtcDateTime,
    out: *mut f64,
) -> i32 {
    if utc.is_null() || out.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's pointers.
    unsafe { out.write_unaligned(from_utc(utc.read_unaligned())) };
    0
}

/// `OSStatus UCConvertSecondsToCFAbsoluteTime(UInt32, CFAbsoluteTime *)`:
/// seconds since 1904, the classic Mac OS clock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn UCConvertSecondsToCFAbsoluteTime(seconds: u32, out: *mut f64) -> i32 {
    if out.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's pointer.
    unsafe { out.write_unaligned(f64::from(seconds) - SINCE_1904) };
    0
}

/// `OSStatus UCConvertCFAbsoluteTimeToSeconds(CFAbsoluteTime, UInt32 *)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn UCConvertCFAbsoluteTimeToSeconds(time: f64, out: *mut u32) -> i32 {
    if out.is_null() {
        return PARAM_ERR;
    }
    let since = (time + SINCE_1904).floor();
    if since.is_nan() || since < 0.0 {
        return UTC_UNDERFLOW_ERR;
    }
    if since > f64::from(u32::MAX) {
        return UTC_OVERFLOW_ERR;
    }
    // SAFETY: the caller's pointer.
    unsafe { out.write_unaligned(since as u32) };
    0
}

/// `OSStatus UCConvertCFAbsoluteTimeToLongDateTime(CFAbsoluteTime,
/// LongDateTime *)`. `LongDateTime` is whole seconds since 1904, with no
/// fractional part - the sixty-four-bit successor to the `UInt32` seconds
/// [`UCConvertCFAbsoluteTimeToSeconds`] answers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn UCConvertCFAbsoluteTimeToLongDateTime(time: f64, out: *mut i64) -> i32 {
    if out.is_null() {
        return PARAM_ERR;
    }
    let since = (time + SINCE_1904).floor();
    if since.is_nan() || since < 0.0 {
        return UTC_UNDERFLOW_ERR;
    }
    // Every `f64` integer up to 2^53 round-trips through `i64` exactly; past
    // that, the cast below would itself be the lossy step, not the Mac epoch.
    if since > (1_i64 << 53) as f64 {
        return UTC_OVERFLOW_ERR;
    }
    // SAFETY: the caller's pointer.
    unsafe { out.write_unaligned(since as i64) };
    0
}

/// `OSStatus GetUTCDateTime(UTCDateTime *utcDateTime, OptionBits options)`.
///
/// `options` only ever named `kUTCDefaultOptions`, so there is nothing to
/// branch on.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn GetUTCDateTime(out: *mut UtcDateTime, _options: u32) -> i32 {
    if out.is_null() {
        return PARAM_ERR;
    }
    let now = unsafe { corefoundation::api::CFAbsoluteTimeGetCurrent() };
    match to_utc(now) {
        Ok(utc) => {
            // SAFETY: the caller's `UTCDateTime`.
            unsafe { out.write_unaligned(utc) };
            0
        }
        Err(status) => status,
    }
}

exports::exports! {
    functions {
        "UCConvertCFAbsoluteTimeToUTCDateTime" => UCConvertCFAbsoluteTimeToUTCDateTime,
        "UCConvertUTCDateTimeToCFAbsoluteTime" => UCConvertUTCDateTimeToCFAbsoluteTime,
        "UCConvertSecondsToCFAbsoluteTime" => UCConvertSecondsToCFAbsoluteTime,
        "UCConvertCFAbsoluteTimeToLongDateTime" => UCConvertCFAbsoluteTimeToLongDateTime,
        "GetUTCDateTime" => GetUTCDateTime,
        "UCConvertCFAbsoluteTimeToSeconds" => UCConvertCFAbsoluteTimeToSeconds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_core_foundation_epoch_is_its_offset_from_1904() {
        let mut utc = UtcDateTime::default();
        assert_eq!(
            unsafe { UCConvertCFAbsoluteTimeToUTCDateTime(0.5, &mut utc) },
            0
        );
        let (high, low, fraction) = (utc.high_seconds, utc.low_seconds, utc.fraction);
        assert_eq!((high, low, fraction), (0, 3_061_152_000, 0x8000));
        let mut back = 0.0;
        assert_eq!(
            unsafe { UCConvertUTCDateTimeToCFAbsoluteTime(&utc, &mut back) },
            0
        );
        assert_eq!(back, 0.5);
    }

    #[test]
    fn times_before_1904_underflow_and_nulls_are_refused() {
        let mut utc = UtcDateTime::default();
        assert_eq!(
            unsafe { UCConvertCFAbsoluteTimeToUTCDateTime(-SINCE_1904 - 1.0, &mut utc) },
            UTC_UNDERFLOW_ERR
        );
        assert_eq!(
            unsafe { UCConvertCFAbsoluteTimeToUTCDateTime(0.0, std::ptr::null_mut()) },
            PARAM_ERR
        );
        let mut seconds = 0;
        assert_eq!(
            unsafe { UCConvertCFAbsoluteTimeToSeconds(1.0, &mut seconds) },
            0
        );
        assert_eq!(seconds, 3_061_152_001);
    }

    #[test]
    fn long_date_time_is_whole_seconds_since_1904() {
        let mut long = 0_i64;
        assert_eq!(
            unsafe { UCConvertCFAbsoluteTimeToLongDateTime(1.5, &mut long) },
            0
        );
        assert_eq!(long, 3_061_152_001);
        assert_eq!(
            unsafe { UCConvertCFAbsoluteTimeToLongDateTime(-SINCE_1904 - 1.0, &mut long) },
            UTC_UNDERFLOW_ERR
        );
    }

    /// The current time, read back through the CF-epoch conversion this file
    /// already answers, lands within a tight window of `to_utc`'s own answer -
    /// which is also how it is implemented, so this mostly guards against the
    /// two drifting apart.
    #[test]
    fn get_utc_date_time_answers_roughly_now() {
        let mut utc = UtcDateTime::default();
        assert_eq!(unsafe { GetUTCDateTime(&mut utc, 0) }, 0);
        let mut back = 0.0;
        assert_eq!(
            unsafe { UCConvertUTCDateTimeToCFAbsoluteTime(&utc, &mut back) },
            0
        );
        let now = unsafe { corefoundation::api::CFAbsoluteTimeGetCurrent() };
        assert!((now - back).abs() < 5.0, "{back} is not close to {now}");
        assert_eq!(
            unsafe { GetUTCDateTime(std::ptr::null_mut(), 0) },
            PARAM_ERR
        );
    }
}
