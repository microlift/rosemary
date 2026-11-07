#![cfg_attr(target_os = "macos", allow(dead_code))]

use std::ffi::c_char;
use std::ffi::c_int;
use std::ffi::c_void;

use crate::translation::DarwinTranslation;

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

/// Darwin's obsolete `struct timezone`, whose two `int` fields remain part of
/// the `settimeofday` ABI.
#[repr(C)]
#[derive(Clone, Copy)]
struct Timezone {
    minutes_west: i32,
    dst_time: i32,
}

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
            tv_usec: self.usec as _,
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

/// `time_t time(time_t *)`. Darwin's `time_t` is 64-bit, as this host's.
unsafe extern "C" fn time(out: *mut i64) -> i64 {
    // The vDSO, like `clock_gettime`'s, may fall back to a raw syscall.
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    // SAFETY: `out` is null or a writable `time_t`, as `time` requires.
    unsafe { libc::time(out.cast()) }
}

/// `int gettimeofday(struct timeval *, void *)`.
unsafe extern "C" fn gettimeofday(out: *mut Timeval, tz: *mut c_void) -> i32 {
    let mut host = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    // The vDSO, like `clock_gettime`'s, may fall back to a raw syscall.
    let rc = {
        let _host = crate::syscall::ScopedHostSyscallContext::new();
        unsafe { libc::gettimeofday(&mut host, tz.cast()) }
    };
    if rc == 0 && !out.is_null() {
        unsafe { Timeval::from_host(host).write(out) };
    }
    rc
}

/// Convert the two guest values into host-owned storage. A null pointer keeps
/// the documented "use the current time" meaning and reaches libc as null.
unsafe fn timevals(times: *const Timeval) -> Option<[libc::timeval; 2]> {
    (!times.is_null()).then(|| unsafe {
        [
            Timeval::read(times).host(),
            Timeval::read(times.add(1)).host(),
        ]
    })
}

unsafe extern "C" fn utimes(path: *const c_char, times: *const Timeval) -> i32 {
    let host = unsafe { timevals(times) };
    let times = host.as_ref().map_or(core::ptr::null(), |v| v.as_ptr());
    // The path is the guest's; see [`crate::fs`] for why it is translated.
    crate::stat::with_fallback(path, |path| {
        let rc = unsafe { libc::utimes(path, times) };
        (rc, rc == 0)
    })
}
unsafe extern "C" fn futimes(fd: i32, times: *const Timeval) -> i32 {
    let host = unsafe { timevals(times) };
    unsafe { libc::futimes(fd, host.as_ref().map_or(core::ptr::null(), |v| v.as_ptr())) }
}
unsafe extern "C" fn lutimes(path: *const c_char, times: *const Timeval) -> i32 {
    let host = unsafe { timevals(times) };
    let times = host.as_ref().map_or(core::ptr::null(), |v| v.as_ptr());
    crate::stat::with_fallback(path, |path| {
        let rc = unsafe { libc::lutimes(path, times) };
        (rc, rc == 0)
    })
}

/// Darwin's `UTIME_NOW` and `UTIME_OMIT` from `<sys/stat.h>`; Linux spells
/// them `(1 << 30) - 1` and `(1 << 30) - 2`.
const DARWIN_UTIME_NOW: i64 = -1;
const DARWIN_UTIME_OMIT: i64 = -2;

/// Darwin's `AT_SYMLINK_NOFOLLOW`.
const DARWIN_AT_SYMLINK_NOFOLLOW: c_int = 0x20;

/// Convert the two guest timespecs into host-owned storage, translating the
/// `UTIME_NOW` and `UTIME_OMIT` markers. Null keeps meaning "now" for both.
unsafe fn timespecs(times: *const crate::stat::Timespec) -> Option<[libc::timespec; 2]> {
    let host = |time: crate::stat::Timespec| {
        let nsec = match time.nsec {
            DARWIN_UTIME_NOW => libc::UTIME_NOW,
            DARWIN_UTIME_OMIT => libc::UTIME_OMIT,
            nsec => nsec,
        };
        libc::timespec {
            tv_sec: time.sec,
            tv_nsec: nsec,
        }
    };
    (!times.is_null()).then(|| unsafe {
        [
            host(times.read_unaligned()),
            host(times.add(1).read_unaligned()),
        ]
    })
}

/// `int utimensat(int fd, const char *path, const struct timespec[2], int flag)`
///
/// # Safety
///
/// `path` is a NUL-terminated string, `times` is null or two readable
/// timespecs, and `dirfd` a directory descriptor or Darwin's `AT_FDCWD`.
unsafe extern "C" fn utimensat(
    dirfd: c_int,
    path: *const c_char,
    times: *const crate::stat::Timespec,
    flag: c_int,
) -> i32 {
    if flag & !DARWIN_AT_SYMLINK_NOFOLLOW != 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let host_flag = if flag & DARWIN_AT_SYMLINK_NOFOLLOW != 0 {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    let dirfd = match dirfd {
        crate::stat::DARWIN_AT_FDCWD => libc::AT_FDCWD,
        fd => fd,
    };
    let host = unsafe { timespecs(times) };
    let times = host.as_ref().map_or(core::ptr::null(), |v| v.as_ptr());
    crate::stat::with_fallback_at(dirfd, path, |path| {
        if !crate::stat::may_write_at(dirfd, path) {
            return (-1, false);
        }
        let rc = unsafe { libc::utimensat(dirfd, path, times, host_flag) };
        (rc, rc == 0)
    })
}

/// `int futimens(int fd, const struct timespec[2])`
///
/// # Safety
///
/// `times` is null or two readable timespecs.
unsafe extern "C" fn futimens(fd: c_int, times: *const crate::stat::Timespec) -> i32 {
    let host = unsafe { timespecs(times) };
    unsafe { libc::futimens(fd, host.as_ref().map_or(core::ptr::null(), |v| v.as_ptr())) }
}

/// `int utime(const char *, const struct utimbuf *)`. Both `time_t` fields have
/// the same layout on Darwin and this host; only the path needs translating.
unsafe extern "C" fn utime(path: *const c_char, times: *const libc::utimbuf) -> i32 {
    crate::stat::with_fallback(path, |path| {
        // SAFETY: a translated path, and the caller's `utimbuf` or null.
        let rc = unsafe { libc::utime(path, times) };
        (rc, rc == 0)
    })
}

unsafe extern "C" fn settimeofday(_time: *const Timeval, _timezone: *const Timezone) -> i32 {
    crate::errno::set(libc::ENOSYS);
    -1
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
    unsafe { crate::syscall::clock_gettime(host, ts.cast()) }
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
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    // SAFETY: the caller's guarantee, and the two layouts agree.
    unsafe { libc::clock_getres(host, ts as *mut libc::timespec) }
}

/// `int clock_settime(clockid_t, const struct timespec *)`.
///
/// Darwin's libc sets only the wall clock, through `settimeofday`, so every
/// other id is `EINVAL`. The host's privilege policy decides the rest.
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
    if ts.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let ts = unsafe { (ts as *const libc::timespec).read_unaligned() };
    if !(0..1_000_000_000).contains(&ts.tv_nsec) {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) }
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn settime(clock: id::clockid_t, ts: *const c_void) -> i32 {
    let Some(host) = translate(clock) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    unsafe { libc::clock_settime(host, ts.cast()) }
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
    if unsafe { crate::syscall::clock_gettime(host, &mut ts) } != 0 {
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
    // A CPU-time clock, which the vDSO always answers with a raw syscall.
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    unsafe { host::clock() as u64 }
}

/// Use this type to decorate a mutable pointer (ie from a header)
/// that doesn't actually have safe mutability, but you want to convey
/// what the source code specifies.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct Immutable<T> {
    val: T,
}

impl<T> Immutable<T> {
    pub const fn new(val: T) -> Self {
        Self { val }
    }

    pub const fn get(&self) -> &T {
        &self.val
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Tm {
    sec: c_int,
    min: c_int,
    hour: c_int,
    mday: c_int,
    mon: c_int,
    year: c_int,
    wday: c_int,
    yday: c_int,
    isdst: c_int,
    gmtoff: i64,
    /// `char *` in the SDK, but it names a string the guest must not write.
    zone: Immutable<*mut c_char>,
}

impl Tm {
    const ZERO: Tm = Tm {
        sec: 0,
        min: 0,
        hour: 0,
        mday: 0,
        mon: 0,
        year: 0,
        wday: 0,
        yday: 0,
        isdst: 0,
        zone: Immutable::new(core::ptr::null_mut()),
        gmtoff: 0,
    };
}

impl DarwinTranslation<libc::tm> for Tm {
    fn to_host(&self) -> libc::tm {
        libc::tm {
            tm_sec: self.sec,
            tm_min: self.min,
            tm_hour: self.hour,
            tm_mday: self.mday,
            tm_mon: self.mon,
            tm_year: self.year,
            tm_wday: self.wday,
            tm_yday: self.yday,
            tm_isdst: self.isdst,
            tm_gmtoff: self.gmtoff as _,
            tm_zone: *self.zone.get() as _,
        }
    }

    fn from_host(host: &libc::tm) -> Self {
        Self {
            sec: host.tm_sec,
            min: host.tm_min,
            hour: host.tm_hour,
            mday: host.tm_mday,
            mon: host.tm_mon,
            year: host.tm_year,
            wday: host.tm_wday,
            yday: host.tm_yday,
            isdst: host.tm_isdst,
            gmtoff: host.tm_gmtoff as i64,
            zone: Immutable::new(host.tm_zone as *mut c_char),
        }
    }
}

const _: () = {
    assert!(size_of::<Tm>() == 56);
    assert!(std::mem::offset_of!(Tm, gmtoff) == 40);
    assert!(std::mem::offset_of!(Tm, zone) == 48);
};

/// The zone Darwin's `gmtime` names, where glibc's names "GMT".
const UTC: &std::ffi::CStr = c"UTC";

/// The broken-down UTC time of `*clock`, with Darwin's zone name.
///
/// # Safety
///
/// `clock` is readable.
unsafe fn utc(clock: *const libc::time_t) -> Option<Tm> {
    // SAFETY: an all-zero `struct tm` is a valid one to overwrite.
    let mut host: libc::tm = unsafe { core::mem::zeroed() };
    // SAFETY: the caller's obligation for `clock`; `host` is a live local.
    if unsafe { libc::gmtime_r(clock, &mut host) }.is_null() {
        return None;
    }
    Some(Tm {
        zone: Immutable::new(UTC.as_ptr().cast_mut()),
        ..Tm::from_host(&host)
    })
}

/// `struct tm *gmtime_r(const time_t *, struct tm *)`
///
/// # Safety
///
/// `clock` is readable and `result` writable for one Darwin `struct tm`.
unsafe extern "C" fn gmtime_r(clock: *const libc::time_t, result: *mut Tm) -> *mut Tm {
    // SAFETY: the caller's obligation for `clock`.
    let Some(tm) = (unsafe { utc(clock) }) else {
        return core::ptr::null_mut();
    };
    // SAFETY: the caller's obligation; guest memory need not be Rust-aligned.
    unsafe { result.write_unaligned(tm) };
    result
}

// Darwin keeps `gmtime`'s result per thread rather than in one static shared
// with the host's own `gmtime` and `localtime`.
thread_local! {
    static GMTIME: core::cell::UnsafeCell<Tm> = const { core::cell::UnsafeCell::new(Tm::ZERO) };
}

/// `struct tm *gmtime(const time_t *)`
///
/// # Safety
///
/// `clock` is readable.
unsafe extern "C" fn gmtime(clock: *const libc::time_t) -> *mut Tm {
    // SAFETY: the caller's obligation for `clock`.
    let Some(tm) = (unsafe { utc(clock) }) else {
        return core::ptr::null_mut();
    };
    let slot = GMTIME.with(core::cell::UnsafeCell::get);
    // SAFETY: this thread's own buffer, which lives as long as the thread.
    unsafe { slot.write(tm) };
    slot
}

exports::exports! {
    functions {
        "gmtime" => gmtime,
        "gmtime_r" => gmtime_r,
        "clock_gettime" => gettime,
        "clock_getres" => getres,
        "clock_settime" => settime,
        "clock_gettime_nsec_np" => gettime_nsec_np,
        "clock" => clock,
        "time" => time,
        "gettimeofday" => gettimeofday,
        "utime" => utime,
        "utimes" => utimes,
        "futimes" => futimes,
        "lutimes" => lutimes,
        "utimensat" => utimensat,
        "futimens" => futimens,
        "settimeofday" => settimeofday,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rosemary-clock-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        dir
    }

    fn c_path(path: &std::path::Path) -> std::ffi::CString {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::CString::new(path.as_os_str().as_bytes()).expect("no NUL")
    }

    fn mtime(path: &std::path::Path) -> i64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).expect("a file").mtime()
    }

    #[test]
    fn utimensat_sets_a_symlinks_own_time_with_darwins_nofollow() {
        let dir = scratch("utimensat-nofollow");
        // The target does not exist, as `tar` restores a link before its file.
        std::os::unix::fs::symlink("missing", dir.join("link")).expect("a symlink");
        let link = c_path(&dir.join("link"));
        let at = crate::stat::Timespec {
            sec: 1_000_000,
            nsec: 0,
        };
        let times = [at, at];
        // SAFETY: a NUL-terminated path and two timespecs.
        let rc = unsafe {
            utimensat(
                crate::stat::DARWIN_AT_FDCWD,
                link.as_ptr(),
                times.as_ptr(),
                DARWIN_AT_SYMLINK_NOFOLLOW,
            )
        };
        assert_eq!(rc, 0, "utimensat: {}", std::io::Error::last_os_error());
        assert_eq!(mtime(&dir.join("link")), 1_000_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn futimens_leaves_an_omitted_time_alone() {
        let dir = scratch("futimens-omit");
        let path = dir.join("file");
        std::fs::write(&path, b"x").expect("a file");
        let file = std::fs::File::open(&path).expect("an open file");
        let set = crate::stat::Timespec {
            sec: 2_000_000,
            nsec: 0,
        };
        let omit = crate::stat::Timespec {
            sec: 0,
            nsec: DARWIN_UTIME_OMIT,
        };
        use std::os::fd::AsRawFd;
        // SAFETY: a live descriptor and two timespecs.
        unsafe {
            assert_eq!(futimens(file.as_raw_fd(), [set, set].as_ptr()), 0);
            assert_eq!(futimens(file.as_raw_fd(), [set, omit].as_ptr()), 0);
        }
        assert_eq!(mtime(&path), 2_000_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gmtime_names_its_zone_utc() {
        let epoch: libc::time_t = 0;
        let mut tm = Tm::ZERO;
        let mut text = [0 as c_char; 16];
        // SAFETY: a readable time, a writable `struct tm` and a sized buffer.
        unsafe {
            assert!(!gmtime_r(&epoch, &mut tm).is_null());
            let host = tm.to_host();
            let n = libc::strftime(text.as_mut_ptr(), text.len(), c"%Z %Y".as_ptr(), &host);
            assert_eq!(
                &std::ffi::CStr::from_ptr(text.as_ptr()).to_bytes()[..n],
                b"UTC 1970"
            );
            let zone = *(*gmtime(&epoch)).zone.get();
            assert_eq!(std::ffi::CStr::from_ptr(zone), UTC);
        }
    }

    #[test]
    fn gmtime_leaves_the_host_static_alone() {
        let epoch: libc::time_t = 0;
        // SAFETY: a readable time; the host's static is only read here.
        unsafe {
            let host = libc::gmtime(&epoch);
            let zone = (*host).tm_zone;
            let guest = gmtime(&epoch);
            assert_ne!(guest as usize, host as usize);
            assert_eq!((*host).tm_zone, zone);
        }
    }

    #[test]
    fn gmtime_keeps_a_buffer_per_thread() {
        let epoch: libc::time_t = 0;
        // SAFETY: a readable time.
        let here = unsafe { gmtime(&epoch) } as usize;
        // SAFETY: a readable time.
        let there = std::thread::spawn(move || unsafe { (*gmtime(&86_400)).yday })
            .join()
            .expect("the thread");
        assert_eq!(there, 1);
        // SAFETY: this thread's buffer is still live.
        assert_eq!(unsafe { (*(here as *const Tm)).yday }, 0);
    }

    #[test]
    fn time_answers_and_stores_the_same_second() {
        let mut stored = 0i64;
        // SAFETY: a live local.
        let answered = unsafe { time(&mut stored) };
        assert_eq!(answered, stored);
        // SAFETY: null is allowed.
        assert!(unsafe { time(core::ptr::null_mut()) } >= answered);
    }

    #[test]
    fn utime_sets_both_times() {
        let path = std::env::temp_dir().join(format!("rosemary-utime-{}", std::process::id()));
        std::fs::write(&path, b"x").expect("a file");
        let name = std::ffi::CString::new(path.to_str().expect("utf-8")).expect("no NUL");
        let times = libc::utimbuf {
            actime: 315_550_800,
            modtime: 315_550_800,
        };
        // SAFETY: a NUL-terminated path and a live `utimbuf`.
        assert_eq!(unsafe { utime(name.as_ptr(), &times) }, 0);
        let modified = std::fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("mtime");
        let seconds = modified
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after 1970");
        assert_eq!(seconds.as_secs(), 315_550_800);
        let _ = std::fs::remove_file(&path);
    }

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
