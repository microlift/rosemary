#![cfg_attr(target_os = "macos", allow(dead_code))]

#[cfg(not(target_os = "macos"))]
use std::ffi::c_char;
use std::ffi::c_void;

/// Darwin's 64-bit `struct timeval` from `<sys/_types/_timeval.h>`.
///
/// `tv_usec` is `__darwin_suseconds_t` (`int32_t`); `_pad` is the four bytes
/// of tail padding required to align an array element to eight bytes.  It is
/// guest representation only: no host libc call receives this structure.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Timeval {
    sec: i64,
    usec: i32,
    _pad: i32,
}

#[cfg(target_os = "macos")]
impl Timeval {
    /// A host value converted to the guest's padded Darwin representation.
    fn from_host(value: libc::timeval) -> Timeval {
        Timeval {
            sec: value.tv_sec,
            usec: value.tv_usec,
            _pad: 0,
        }
    }
}

/// Darwin's obsolete `struct timezone`, whose two `int` fields remain part of
/// the `settimeofday` ABI.
#[repr(C)]
#[derive(Clone, Copy)]
struct Timezone {
    minutes_west: i32,
    dst_time: i32,
}

impl Timezone {
    /// # Safety
    ///
    /// `value` is readable for one Darwin `timezone`.
    unsafe fn read(value: *const Self) -> Self {
        unsafe { value.read_unaligned() }
    }
}

#[cfg(not(target_os = "macos"))]
impl Timeval {
    /// A host value converted to the Darwin representation.
    pub(crate) fn from_host(value: libc::timeval) -> Timeval {
        Timeval {
            sec: value.tv_sec,
            usec: value.tv_usec as i32,
            _pad: 0,
        }
    }

    /// Read a potentially unaligned guest value without imposing Rust's
    /// alignment requirements on guest memory.
    ///
    /// # Safety
    ///
    /// `value` is readable for one Darwin `timeval`.
    pub(crate) unsafe fn read(value: *const Self) -> Self {
        unsafe { value.read_unaligned() }
    }

    /// A host-owned timeval suitable for passing to libc.
    pub(crate) fn host(self) -> libc::timeval {
        libc::timeval {
            tv_sec: self.sec,
            tv_usec: i64::from(self.usec),
        }
    }

    /// Write to a potentially unaligned guest buffer.
    ///
    /// # Safety
    ///
    /// `out` is writable for one Darwin `timeval`.
    pub(crate) unsafe fn write(self, out: *mut Self) {
        unsafe { out.write_unaligned(self) }
    }
}

/// `int gettimeofday(struct timeval *, void *)`.
unsafe extern "C" fn gettimeofday(out: *mut Timeval, tz: *mut c_void) -> i32 {
    let mut host = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let rc = unsafe { libc::gettimeofday(&mut host, tz.cast()) };
    if rc == 0 && !out.is_null() {
        unsafe { out.write(Timeval::from_host(host)) };
    }
    rc
}

#[cfg(not(target_os = "macos"))]
unsafe extern "C" {
    #[link_name = "utimes"]
    fn host_utimes(path: *const c_char, times: *const libc::timeval) -> i32;
    #[link_name = "futimes"]
    fn host_futimes(fd: i32, times: *const libc::timeval) -> i32;
    #[link_name = "lutimes"]
    fn host_lutimes(path: *const c_char, times: *const libc::timeval) -> i32;
    #[link_name = "settimeofday"]
    fn host_settimeofday(time: *const libc::timeval, timezone: *const Timezone) -> i32;
}

/// Convert the two guest values into host-owned storage. A null pointer keeps
/// the documented "use the current time" meaning and reaches libc as null.
#[cfg(not(target_os = "macos"))]
unsafe fn timevals(times: *const Timeval) -> Option<[libc::timeval; 2]> {
    (!times.is_null()).then(|| unsafe {
        [
            Timeval::read(times).host(),
            Timeval::read(times.add(1)).host(),
        ]
    })
}

#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn utimes(path: *const c_char, times: *const Timeval) -> i32 {
    let host = unsafe { timevals(times) };
    let times = host.as_ref().map_or(core::ptr::null(), |v| v.as_ptr());
    // The path is the guest's; see [`crate::fs`] for why it is translated.
    crate::stat::with_fallback(path, |path| {
        let rc = unsafe { host_utimes(path, times) };
        (rc, rc == 0)
    })
}
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn futimes(fd: i32, times: *const Timeval) -> i32 {
    let host = unsafe { timevals(times) };
    unsafe { host_futimes(fd, host.as_ref().map_or(core::ptr::null(), |v| v.as_ptr())) }
}
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn lutimes(path: *const c_char, times: *const Timeval) -> i32 {
    let host = unsafe { timevals(times) };
    let times = host.as_ref().map_or(core::ptr::null(), |v| v.as_ptr());
    crate::stat::with_fallback(path, |path| {
        let rc = unsafe { host_lutimes(path, times) };
        (rc, rc == 0)
    })
}

/// `int settimeofday(const struct timeval *, const struct timezone *)`.
///
/// This is bridged rather than denied: changing host time is the host analogue
/// of the Darwin operation, still subject to the host's privilege policy.
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn settimeofday(time: *const Timeval, timezone: *const Timezone) -> i32 {
    let host_time = (!time.is_null()).then(|| unsafe { Timeval::read(time).host() });
    let host_timezone = (!timezone.is_null()).then(|| unsafe { Timezone::read(timezone) });
    unsafe {
        host_settimeofday(
            host_time
                .as_ref()
                .map_or(core::ptr::null(), core::ptr::from_ref),
            host_timezone
                .as_ref()
                .map_or(core::ptr::null(), core::ptr::from_ref),
        )
    }
}

/// `clockid_t`, from Darwin's `<time.h>`.
pub mod id {
    #[allow(non_camel_case_types)]
    pub type clockid_t = i32;

    pub const REALTIME: clockid_t = 0;
    pub const MONOTONIC: clockid_t = 6;
    pub const MONOTONIC_RAW: clockid_t = 4;
    pub const MONOTONIC_RAW_APPROX: clockid_t = 5;
    pub const UPTIME_RAW: clockid_t = 8;
    pub const UPTIME_RAW_APPROX: clockid_t = 9;
    pub const PROCESS_CPUTIME_ID: clockid_t = 12;
    pub const THREAD_CPUTIME_ID: clockid_t = 16;
}

/// The host clocks the mapping picks from, named by the property being preserved
/// rather than by the host's spelling - a host without `CLOCK_BOOTTIME` still has to
/// answer, and answering with the nearest clock is what these names make explicit.
#[cfg(target_os = "linux")]
mod host {
    use libc::clockid_t;

    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    pub const WITH_SLEEP: clockid_t = libc::CLOCK_BOOTTIME;
    pub const WITHOUT_SLEEP: clockid_t = libc::CLOCK_MONOTONIC;
    pub const WITHOUT_SLEEP_COARSE: clockid_t = libc::CLOCK_MONOTONIC_COARSE;
    pub const PROCESS_CPU: clockid_t = libc::CLOCK_PROCESS_CPUTIME_ID;
    pub const THREAD_CPU: clockid_t = libc::CLOCK_THREAD_CPUTIME_ID;

    unsafe extern "C" {
        pub fn clock() -> libc::clock_t;
    }
}

/// As above, for a host with no clock that counts a sleep and none that is coarse.
/// Both fall back to plain monotonic, which differs only across a suspend.
#[cfg(not(target_os = "linux"))]
mod host {
    use libc::clockid_t;

    pub const REALTIME: clockid_t = libc::CLOCK_REALTIME;
    pub const WITH_SLEEP: clockid_t = libc::CLOCK_MONOTONIC;
    pub const WITHOUT_SLEEP: clockid_t = libc::CLOCK_MONOTONIC;
    pub const WITHOUT_SLEEP_COARSE: clockid_t = libc::CLOCK_MONOTONIC;
    pub const PROCESS_CPU: clockid_t = libc::CLOCK_PROCESS_CPUTIME_ID;
    pub const THREAD_CPU: clockid_t = libc::CLOCK_THREAD_CPUTIME_ID;

    unsafe extern "C" {
        pub fn clock() -> libc::clock_t;
    }
}

/// The host clock a guest's `clockid_t` names, or `None` if it names none.
///
/// An unrecognised id is refused rather than passed through: the host's own numbering
/// starts at zero too, so a wrong id is a *working* clock rather than an error.
fn translate(guest: id::clockid_t) -> Option<libc::clockid_t> {
    Some(match guest {
        id::REALTIME => host::REALTIME,
        id::MONOTONIC | id::MONOTONIC_RAW => host::WITH_SLEEP,
        // No host clock is both coarse and sleep-counting; sleep-counting wins, and
        // the cost is only that the read is not the cheap one.
        id::MONOTONIC_RAW_APPROX => host::WITH_SLEEP,
        id::UPTIME_RAW => host::WITHOUT_SLEEP,
        id::UPTIME_RAW_APPROX => host::WITHOUT_SLEEP_COARSE,
        id::PROCESS_CPUTIME_ID => host::PROCESS_CPU,
        id::THREAD_CPUTIME_ID => host::THREAD_CPU,
        _ => return None,
    })
}

/// `int clock_gettime(clockid_t, struct timespec *)`.
///
/// # Safety
///
/// `ts` is null or a writable `struct timespec *`, which is two eight-byte words on
/// Darwin and on this host alike.
unsafe extern "C" fn gettime(clock: id::clockid_t, ts: *mut c_void) -> i32 {
    let Some(host) = translate(clock) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    // SAFETY: the caller's guarantee, and the two layouts agree.
    unsafe { libc::clock_gettime(host, ts as *mut libc::timespec) }
}

/// `int clock_getres(clockid_t, struct timespec *)`.
///
/// # Safety
///
/// As [`gettime`].
unsafe extern "C" fn getres(clock: id::clockid_t, ts: *mut c_void) -> i32 {
    let Some(host) = translate(clock) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    // SAFETY: the caller's guarantee, and the two layouts agree.
    unsafe { libc::clock_getres(host, ts as *mut libc::timespec) }
}

/// `int clock_settime(clockid_t, const struct timespec *)`.
///
/// Darwin's libc sets only the wall clock, through `settimeofday`, so every
/// other id is `EINVAL` and the nanoseconds are truncated to microseconds. As
/// with [`settimeofday`], the host's privilege policy decides the rest.
///
/// # Safety
///
/// `ts` is a readable `struct timespec *`.
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn settime(clock: id::clockid_t, ts: *const c_void) -> i32 {
    if clock != id::REALTIME {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: the caller's guarantee, and the two layouts agree.
    let ts = unsafe { (ts as *const libc::timespec).read_unaligned() };
    let tv = libc::timeval {
        tv_sec: ts.tv_sec,
        tv_usec: ts.tv_nsec / 1000,
    };
    unsafe { host_settimeofday(&tv, core::ptr::null()) }
}

/// `uint64_t clock_gettime_nsec_np(clockid_t)`, which has no host spelling at all.
///
/// Zero is the failure Darwin reports, with `errno` set; there is no other value a
/// `uint64_t` return can reserve.
unsafe extern "C" fn gettime_nsec_np(clock: id::clockid_t) -> u64 {
    let Some(host) = translate(clock) else {
        crate::errno::set(libc::EINVAL);
        return 0;
    };
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes the `timespec` it is given and nothing else.
    if unsafe { libc::clock_gettime(host, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(ts.tv_nsec as u64)
}

/// `clock_t clock(void)`.
///
/// Darwin's `clock_t` is an unsigned 64-bit value. POSIX hosts report the same
/// microsecond CPU-time ticks, so only the signedness of the host representation
/// needs adapting. Casting also preserves the `(clock_t)-1` failure sentinel.
unsafe extern "C" fn clock() -> u64 {
    unsafe { host::clock() as u64 }
}

#[cfg(target_os = "macos")]
exports::exports! {}

#[cfg(not(target_os = "macos"))]
exports::exports! {
    functions {
        "clock_gettime" => gettime,
        "clock_getres" => getres,
        "clock_settime" => settime,
        "clock_gettime_nsec_np" => gettime_nsec_np,
        "clock" => clock,
        "gettimeofday" => gettimeofday,
        "utimes" => utimes,
        "futimes" => futimes,
        "lutimes" => lutimes,
        "settimeofday" => settimeofday,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// macOS SDK `<sys/_types/_timeval.h>` declares `long` then `int32_t`.
    #[test]
    fn timeval_matches_the_64_bit_darwin_sdk_abi() {
        assert_eq!(core::mem::size_of::<Timeval>(), 16);
        assert_eq!(core::mem::offset_of!(Timeval, sec), 0);
        assert_eq!(core::mem::offset_of!(Timeval, usec), 8);
    }

    /// The collision this module exists for: Darwin's monotonic id is the host's
    /// coarse monotonic, so the mapping must not be the identity.
    #[test]
    #[cfg(target_os = "linux")]
    fn darwin_monotonic_is_not_the_host_s_number() {
        assert_eq!(id::MONOTONIC, libc::CLOCK_MONOTONIC_COARSE as id::clockid_t);
        assert_eq!(translate(id::MONOTONIC), Some(libc::CLOCK_BOOTTIME));
    }

    #[test]
    fn an_unknown_clock_is_refused_rather_than_read() {
        assert_eq!(translate(99), None);
        // SAFETY: no memory is touched on the refusing path.
        assert_eq!(unsafe { gettime_nsec_np(99) }, 0);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        crate::errno::set(0);
    }

    /// Only the wall clock is settable on Darwin; the refusal must come before
    /// the time is read, let alone reaches the host.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn only_the_wall_clock_can_be_set() {
        // SAFETY: the refusing path does not read the pointer.
        assert_eq!(unsafe { settime(id::MONOTONIC, core::ptr::null()) }, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        crate::errno::set(0);
    }

    #[test]
    fn every_named_clock_reads() {
        for clock in [
            id::REALTIME,
            id::MONOTONIC,
            id::MONOTONIC_RAW,
            id::MONOTONIC_RAW_APPROX,
            id::UPTIME_RAW,
            id::UPTIME_RAW_APPROX,
            id::PROCESS_CPUTIME_ID,
            id::THREAD_CPUTIME_ID,
        ] {
            // SAFETY: a live local of the right type.
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: `ts` is a writable `struct timespec`.
            let rc = unsafe { gettime(clock, &mut ts as *mut _ as *mut c_void) };
            assert_eq!(rc, 0, "clock {clock} must read");
        }
    }

    #[test]
    fn clock_returns_cpu_time_instead_of_treating_it_as_a_clock_id() {
        // SAFETY: `clock` has no arguments and touches no caller memory.
        let first = unsafe { clock() };
        let second = unsafe { clock() };
        assert_ne!(first, u64::MAX, "clock must not fail");
        assert!(second >= first, "process CPU time must not run backwards");
    }

    /// The wall clock and a monotonic clock must not be confused for one another:
    /// the first is a Unix timestamp and the second a much smaller uptime.
    #[test]
    fn realtime_and_uptime_are_different_timelines() {
        // SAFETY: neither touches caller memory.
        let (real, up) = unsafe {
            (
                gettime_nsec_np(id::REALTIME),
                gettime_nsec_np(id::UPTIME_RAW),
            )
        };
        assert!(
            real > up,
            "the wall clock counts from 1970 and uptime from boot"
        );
    }
}
