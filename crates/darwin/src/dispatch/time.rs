/// `DISPATCH_TIME_NOW`: read the clock rather than using the base given.
pub const NOW: u64 = 0;
/// `DISPATCH_TIME_FOREVER`: a deadline that never arrives.
pub const FOREVER: u64 = u64::MAX;

/// The clock `dispatch_time_t` counts on, in nanoseconds.
pub fn now() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes the `timespec` it is given and nothing else.
    if unsafe { crate::syscall::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(ts.tv_nsec as u64)
}

/// `dispatch_time_t dispatch_time(dispatch_time_t when, int64_t delta)`.
///
/// `delta` is signed nanoseconds, and saturates rather than wrapping: a deadline that
/// wrapped past zero would be read as long past and fire immediately, which is the
/// one failure a caller cannot see coming.
pub unsafe extern "C" fn time(when: u64, delta: i64) -> u64 {
    if when == FOREVER {
        return FOREVER;
    }
    let base = match when {
        NOW => now(),
        _ => when,
    };
    match delta >= 0 {
        true => base
            .checked_add(delta as u64)
            .filter(|v| *v <= i64::MAX as u64)
            .unwrap_or(FOREVER),
        false => base.saturating_sub(delta.unsigned_abs()).max(1),
    }
}

/// Nanoseconds from now until `deadline`, or `None` for a deadline that never
/// arrives. Zero for one already past.
pub fn remaining(deadline: u64) -> Option<u64> {
    if deadline == FOREVER {
        return None;
    }
    Some(deadline.saturating_sub(now()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_is_absolute_and_forever_is_infectious() {
        let soon = unsafe { time(NOW, 50_000_000) };
        assert!(soon > now(), "a future deadline is ahead of the clock");
        assert_eq!(unsafe { time(FOREVER, 5) }, FOREVER);
        assert_eq!(remaining(FOREVER), None);
    }

    /// A negative delta larger than the base must not wrap into the distant future.
    #[test]
    fn a_deadline_saturates_rather_than_wrapping() {
        assert_eq!(unsafe { time(10, -100) }, 1);
        assert_eq!(unsafe { time(u64::MAX - 1, i64::MAX) }, u64::MAX);
    }

    #[test]
    fn a_past_deadline_has_no_time_left() {
        assert_eq!(remaining(1), Some(0));
    }
}
