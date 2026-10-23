//! xnu kevent conformance: the timer cases of Apple's xnu test suite
//! (apple-oss-distributions/xnu at xnu-12377.1.9, `tests/`) -
//! `kqueue_timer_tests.c` one call at a time, and `kevent_continuous_time.c`
//! with upstream's default of no system sleep - run against the `kqueue`
//! and `kevent64` a guest binds.
//!
//! Upstream marks the timers shorter than a millisecond `T_MAYFAIL`: their
//! 30%-or-50µs tolerance is tighter than a loaded machine keeps. They are
//! still counted here, and named as such.
//!
//! ```text
//! cargo test -p darwin --test xnu_kevent_conformance -- --nocapture
//! ```
#![cfg(not(target_os = "macos"))]

mod conformance;

use std::ffi::{c_int, c_uint, c_void};

use conformance::{Case, Kind, Stop, T, errno, imports};

const UPSTREAM: &str = "xnu-12377.1.9 tests/";

/// `struct kevent64_s`
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct Kevent64 {
    ident: u64,
    filter: i16,
    flags: u16,
    fflags: u32,
    data: i64,
    udata: u64,
    ext: [u64; 2],
}

/// Darwin's `struct timespec` and `struct timeval` (`suseconds_t` is 32 bits).
#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[repr(C)]
#[derive(Default)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i32,
}

#[repr(C)]
#[derive(Default)]
struct Timebase {
    numer: u32,
    denom: u32,
}

const EVFILT_TIMER: i16 = -7;
const EV_ADD: u16 = 0x0001;
const EV_DELETE: u16 = 0x0002;
const EV_ONESHOT: u16 = 0x0010;
const EV_RECEIPT: u16 = 0x0040;
const EV_ERROR: u16 = 0x4000;

const NOTE_SECONDS: u32 = 0x0000_0001;
const NOTE_USECONDS: u32 = 0x0000_0002;
const NOTE_NSECONDS: u32 = 0x0000_0004;
const NOTE_ABSOLUTE: u32 = 0x0000_0008;
const NOTE_MACH_CONTINUOUS_TIME: u32 = 0x0000_0080;
const NOTE_MACHTIME: u32 = 0x0000_0100;

const USEC_PER_SEC: u64 = 1_000_000;
const NSEC_PER_USEC: u64 = 1_000;
const NSEC_PER_MSEC: u64 = 1_000_000;

const TASK_OVERRIDE_QOS_POLICY: c_uint = 9;
const LATENCY_QOS_TIER_0: c_int = (0xFF << 16) | 1;
const THROUGHPUT_QOS_TIER_0: c_int = (0xFE << 16) | 1;
const TASK_QOS_POLICY_COUNT: c_uint = 2;

type KqueueFn = unsafe extern "C" fn() -> c_int;
type Kevent64Fn = unsafe extern "C" fn(
    c_int,
    *const Kevent64,
    c_int,
    *mut Kevent64,
    c_int,
    c_uint,
    *const Timespec,
) -> c_int;
type GettimeofdayFn = unsafe extern "C" fn(*mut Timeval, *mut c_void) -> c_int;
type U64Fn = unsafe extern "C" fn() -> u64;
type TimebaseFn = unsafe extern "C" fn(*mut Timebase) -> c_int;
type UsleepFn = unsafe extern "C" fn(c_uint) -> c_int;
type SleepFn = unsafe extern "C" fn(c_uint) -> c_uint;
type CloseFn = unsafe extern "C" fn(c_int) -> c_int;
type TaskPolicySetFn = unsafe extern "C" fn(c_uint, c_uint, *const c_int, c_uint) -> c_int;

/// What upstream's file-scope state and helpers are built on, bound once per
/// case as a guest's imports are.
struct Env {
    kq: c_int,
    kevent64: Kevent64Fn,
    gettimeofday: GettimeofdayFn,
    absolute: U64Fn,
    continuous: U64Fn,
    timebase: Timebase,
    usleep: UsleepFn,
    sleep: SleepFn,
    close: CloseFn,
}

impl Env {
    /// `kqueue_timer_tests`'s preamble: `disable_timer_coalescing`,
    /// `mach_timebase_info`, and the queue.
    fn new(t: &mut T, coalescing: bool) -> Result<Env, Stop> {
        imports! {
            kqueue: KqueueFn = "_kqueue";
            kevent64: Kevent64Fn = "_kevent64";
            gettimeofday: GettimeofdayFn = "_gettimeofday";
            absolute: U64Fn = "_mach_absolute_time";
            continuous: U64Fn = "_mach_continuous_time";
            timebase_info: TimebaseFn = "_mach_timebase_info";
            usleep: UsleepFn = "_usleep";
            sleep: SleepFn = "_sleep";
            close: CloseFn = "_close";
            task_policy_set: TaskPolicySetFn = "_task_policy_set";
            task_self: *const c_uint = "_mach_task_self_";
        }
        if coalescing {
            let qos = [LATENCY_QOS_TIER_0, THROUGHPUT_QOS_TIER_0];
            // SAFETY: `mach_task_self_` is the task's port, and the policy is
            // the two `integer_t`s `TASK_QOS_POLICY_COUNT` says.
            let kr = unsafe {
                task_policy_set.get()?(
                    *task_self.get()?,
                    TASK_OVERRIDE_QOS_POLICY,
                    qos.as_ptr(),
                    TASK_QOS_POLICY_COUNT,
                )
            };
            t.check(kr == 0, || {
                format!("task_policy_set(... TASK_OVERRIDE_QOS_POLICY ...) failed: {kr}")
            });
        }
        let mut timebase = Timebase::default();
        // SAFETY: a writable `mach_timebase_info_data_t`.
        unsafe { timebase_info.get()?(&mut timebase) };
        // SAFETY: no arguments.
        let kq = unsafe { kqueue.get()?() };
        t.require(kq > 0, || {
            format!("kqueue() returned {kq}, errno {}", errno())
        })?;
        Ok(Env {
            kq,
            kevent64: kevent64.get()?,
            gettimeofday: gettimeofday.get()?,
            absolute: absolute.get()?,
            continuous: continuous.get()?,
            timebase,
            usleep: usleep.get()?,
            sleep: sleep.get()?,
            close: close.get()?,
        })
    }

    fn nanos_to_abs(&self, nanos: u64) -> u64 {
        nanos * self.timebase.denom as u64 / self.timebase.numer as u64
    }

    fn abs_to_nanos(&self, abs: u64) -> u64 {
        abs * self.timebase.numer as u64 / self.timebase.denom as u64
    }

    fn now_us(&self) -> u64 {
        let mut tv = Timeval::default();
        // SAFETY: a writable `struct timeval`.
        unsafe { (self.gettimeofday)(&mut tv, std::ptr::null_mut()) };
        tv.tv_sec as u64 * USEC_PER_SEC + tv.tv_usec as u64
    }

    fn kevent(
        &self,
        changes: Option<&Kevent64>,
        events: Option<&mut Kevent64>,
        timeout: Option<&Timespec>,
    ) -> c_int {
        let (cp, cn) = match changes {
            Some(c) => (c as *const Kevent64, 1),
            None => (std::ptr::null(), 0),
        };
        let (ep, en) = match events {
            Some(e) => (e as *mut Kevent64, 1),
            None => (std::ptr::null_mut(), 0),
        };
        let tp = timeout.map_or(std::ptr::null(), |t| t as *const Timespec);
        // SAFETY: one change and one event at most, each a live local.
        unsafe { (self.kevent64)(self.kq, cp, cn, ep, en, 0, tp) }
    }

    /// `do_simple_kevent`: register `kev`, wait for it, and say whether it
    /// fired within 30% or 50µs of `expected` microseconds.
    fn simple(&self, t: &mut T, kev: &mut Kevent64, expected: u64) -> bool {
        let timeout = Timespec {
            tv_sec: (expected / USEC_PER_SEC) as i64 + 1,
            tv_nsec: ((expected % USEC_PER_SEC) * 1000) as i64,
        };
        let change = *kev;
        let before = self.now_us();
        let ret = self.kevent(Some(&change), Some(kev), Some(&timeout));
        let after = self.now_us();
        if ret < 1 || kev.flags & EV_ERROR != 0 {
            t.note(format!(
                "kevent returned {ret}, error {}",
                if ret == -1 { errno() as i64 } else { kev.data }
            ));
            return false;
        }
        let elapsed = after as i64 - before as i64;
        let delta = (elapsed - expected as i64).unsigned_abs();
        let ok = !(delta as f64 > 30.0 * expected as f64 / 100.0 && delta > 50);
        t.note(format!("expected {expected} usec, measured {elapsed} usec"));
        ok
    }
}

/// `test_absolute_kevent(time, scale)`.
fn absolute(t: &mut T, time: i64, scale: u32) -> Result<(), Stop> {
    let env = Env::new(t, true)?;
    let nowus = env.now_us();
    let timescale: u64 = match scale {
        NOTE_MACHTIME => 0,
        NOTE_SECONDS => USEC_PER_SEC,
        NOTE_USECONDS => 1,
        0 => 1000,
        _ => return Err(Stop::Fail(format!("scale {scale:#x} not recognized"))),
    };
    let (mut expected, deadline) = match scale {
        NOTE_MACHTIME => (
            env.abs_to_nanos(time as u64) / NSEC_PER_USEC,
            // SAFETY: no arguments.
            unsafe { (env.absolute)() } as i64 + time,
        ),
        // C's `long * uint64_t`, which wraps; a negative time's expectation
        // is zeroed below.
        _ => (
            (time as u64).wrapping_mul(timescale),
            (nowus / timescale) as i64 + time,
        ),
    };
    if time < 0 {
        expected = 0;
    }
    let mut kev = Kevent64 {
        ident: 1,
        filter: EVFILT_TIMER,
        flags: EV_ADD,
        fflags: NOTE_ABSOLUTE | scale,
        data: deadline,
        ..Default::default()
    };
    let ok = env.simple(t, &mut kev, expected);
    t.check(ok, || {
        format!("test_absolute_kevent time:{time}, scale:{scale:#x}")
    });
    Ok(())
}

/// `test_oneshot_kevent(time, scale)`.
fn oneshot(t: &mut T, time: i64, scale: u32) -> Result<(), Stop> {
    let env = Env::new(t, true)?;
    let mut expected = match scale {
        NOTE_MACHTIME => env.abs_to_nanos(time as u64) / NSEC_PER_USEC,
        NOTE_SECONDS => (time as u64).wrapping_mul(USEC_PER_SEC),
        NOTE_USECONDS => time as u64,
        NOTE_NSECONDS => time as u64 / 1000,
        0 => (time as u64).wrapping_mul(1000),
        _ => return Err(Stop::Fail(format!("scale {scale:#x} not recognized"))),
    };
    if time < 0 {
        expected = 0;
    }
    let mut kev = Kevent64 {
        ident: 2,
        filter: EVFILT_TIMER,
        flags: EV_ADD | EV_ONESHOT,
        fflags: scale,
        data: time,
        ..Default::default()
    };
    let ok = env.simple(t, &mut kev, expected);
    t.check(ok, || {
        format!("test_oneshot_kevent time:{time}, scale:{scale:#x}")
    });
    Ok(())
}

/// `test_interval_kevent(usec)`: the timer goes ding multiple times.
fn interval(t: &mut T, usec: i64) -> Result<(), Stop> {
    let env = Env::new(t, true)?;
    let failure_timeout = Timespec {
        tv_sec: 10,
        tv_nsec: 0,
    };
    let test_duration_us = USEC_PER_SEC;
    let expected_pops = match usec < 0 {
        true => 1,
        false => test_duration_us / usec as u64,
    };
    let mut kev = Kevent64 {
        ident: 3,
        filter: EVFILT_TIMER,
        flags: EV_ADD,
        fflags: NOTE_USECONDS,
        data: usec,
        ..Default::default()
    };
    let change = kev;
    let ret = env.kevent(Some(&change), None, None);
    t.require(ret == 0 && kev.flags & EV_ERROR == 0, || {
        format!("setup failure: kevent64 returned {ret}")
    })?;
    let before = env.now_us();
    let mut pops: u64 = 0;
    for _ in 0..expected_pops {
        let ret = env.kevent(None, Some(&mut kev), Some(&failure_timeout));
        t.require(ret == 1, || format!("kevent64 returned {ret}"))?;
        pops += kev.data as u64;
        if env.now_us() - before > test_duration_us {
            break;
        }
    }
    t.check(
        pops <= expected_pops + expected_pops / 20 && pops >= expected_pops - expected_pops / 20,
        || format!("usec:{usec} (saw {pops} of {expected_pops} expected pops)"),
    );
    let stop = Kevent64 {
        ident: 3,
        filter: EVFILT_TIMER,
        flags: EV_DELETE,
        ..Default::default()
    };
    let ret = env.kevent(Some(&stop), None, None);
    if ret != 0 {
        t.note(format!("failed to stop repeating timer: {ret}"));
    }
    Ok(())
}

/// `test_repeating_kevent(usec)`: the timer repeats while nothing polls.
fn repeating(t: &mut T, usec: i64) -> Result<(), Stop> {
    let env = Env::new(t, true)?;
    let failure_timeout = Timespec {
        tv_sec: 10,
        tv_nsec: 0,
    };
    let test_duration_us = USEC_PER_SEC;
    let expected_pops = test_duration_us / usec as u64;
    let mut kev = Kevent64 {
        ident: 4,
        filter: EVFILT_TIMER,
        flags: EV_ADD,
        fflags: NOTE_USECONDS,
        data: usec,
        ..Default::default()
    };
    let change = kev;
    let ret = env.kevent(Some(&change), None, None);
    t.require(ret == 0, || {
        format!("setup failure: kevent64 returned {ret}")
    })?;
    // SAFETY: plain call.
    unsafe { (env.usleep)(test_duration_us as c_uint) };
    let ret = env.kevent(None, Some(&mut kev), Some(&failure_timeout));
    t.require(ret == 1 && kev.flags & EV_ERROR == 0, || {
        format!("setup failure: kevent64 returned {ret}")
    })?;
    let pops = kev.data as u64;
    t.check(
        pops <= expected_pops + expected_pops / 20 && pops >= expected_pops - expected_pops / 20,
        || format!("usec:{usec} (saw {pops} of {expected_pops} expected pops)"),
    );
    let stop = Kevent64 {
        ident: 4,
        filter: EVFILT_TIMER,
        flags: EV_DELETE,
        ..Default::default()
    };
    if env.kevent(Some(&stop), None, None) != 0 {
        t.note("failed to stop repeating timer");
    }
    Ok(())
}

/// `test_updated_kevent(first, second)`: a timer re-armed by a change that is
/// not an `EV_ADD` fires on the second setting, not the first.
fn updated(t: &mut T, first: i64, second: i64) -> Result<(), Stop> {
    let env = Env::new(t, true)?;
    let add = Kevent64 {
        ident: 4,
        filter: EVFILT_TIMER,
        flags: EV_ADD | EV_ONESHOT,
        fflags: 0,
        data: first,
        ..Default::default()
    };
    let ret = env.kevent(Some(&add), None, None);
    t.require(ret == 0, || format!("initial kevent returned {ret}"))?;
    let mut kev = Kevent64 {
        ident: 4,
        filter: EVFILT_TIMER,
        flags: EV_ONESHOT,
        fflags: 0,
        data: second,
        ..Default::default()
    };
    let expected = match second < 0 {
        true => 0,
        false => second as u64 * 1000,
    };
    let ok = env.simple(t, &mut kev, expected);
    t.check(ok, || format!("test_updated_kevent() {first}, {second}"));
    Ok(())
}

macro_rules! timer_case {
    ($name:literal, $mayfail:literal, $body:expr) => {
        Case {
            name: $name,
            kind: Kind::Upstream,
            source: if $mayfail {
                "kqueue_timer_tests.c, kqueue_timer_tests; T_MAYFAIL upstream"
            } else {
                "kqueue_timer_tests.c, kqueue_timer_tests"
            },
            timeout_secs: 30,
            run: $body,
        }
    };
}

/// `kevent_continuous_time_periodic_tick`: a four-second continuous-time
/// timer pops once, and the two clocks agree with no sleep between.
fn continuous_periodic_tick(t: &mut T) -> Result<(), Stop> {
    let env = Env::new(t, false)?;
    let mut kev = Kevent64 {
        ident: 1,
        filter: EVFILT_TIMER,
        flags: EV_ADD | EV_RECEIPT,
        fflags: NOTE_SECONDS | NOTE_MACH_CONTINUOUS_TIME,
        data: 4,
        ..Default::default()
    };
    let change = kev;
    let ret = env.kevent(Some(&change), Some(&mut kev), None);
    t.require(ret == 1, || format!("kevent64 returned {ret}"))?;
    t.require(kev.data == 0, || format!("an error returned: {}", kev.data))?;
    // SAFETY: no arguments.
    let (abs_then, cnt_then) = unsafe { ((env.absolute)(), (env.continuous)()) };
    let ret = env.kevent(None, Some(&mut kev), None);
    t.require(ret == 1, || {
        format!("kevent() should have returned one event, got {ret}")
    })?;
    t.require(kev.flags & EV_ERROR == 0, || {
        format!("event should not have EV_ERROR set: {}", kev.data)
    })?;
    // SAFETY: no arguments.
    let (abs_now, cnt_now) = unsafe { ((env.absolute)(), (env.continuous)()) };
    let ct_ms = env.abs_to_nanos(cnt_now - cnt_then) / NSEC_PER_MSEC;
    let ab_ms = env.abs_to_nanos(abs_now - abs_then) / NSEC_PER_MSEC;
    t.require((ct_ms as i64 - ab_ms as i64).abs() < 10, || {
        format!("should have < 10ms difference between MCT and MAT: {ct_ms} vs {ab_ms}")
    })?;
    t.require((ct_ms as i64 - 4000).abs() < 100, || {
        format!("mach_continuous_time should progress ~4 seconds (+/- 100ms): {ct_ms}")
    })?;
    // SAFETY: plain call.
    unsafe { (env.sleep)(1) };
    let mut kev = Kevent64 {
        ident: 1,
        filter: EVFILT_TIMER,
        flags: EV_DELETE | EV_RECEIPT,
        ..Default::default()
    };
    let change = kev;
    let ret = env.kevent(Some(&change), Some(&mut kev), None);
    t.require(ret == 1, || format!("delete returned {ret}"))?;
    t.require(kev.data == 0, || format!("an error returned: {}", kev.data))?;
    // SAFETY: the queue this case opened.
    t.require(unsafe { (env.close)(env.kq) } == 0, || "close(kq)".into())?;
    Ok(())
}

/// `kevent_continuous_time_absolute`: an absolute continuous-time timer three
/// seconds out pops once, then never again.
fn continuous_absolute(t: &mut T) -> Result<(), Stop> {
    let env = Env::new(t, false)?;
    let nowus = env.now_us() as i64;
    let fire_at = 3 * USEC_PER_SEC as i64 + nowus;
    // SAFETY: no arguments.
    let cnt_now = unsafe { (env.continuous)() };
    let cnt_then = cnt_now + env.nanos_to_abs(3000 * NSEC_PER_MSEC);
    let mut kev = Kevent64 {
        ident: 2,
        filter: EVFILT_TIMER,
        flags: EV_ADD | EV_RECEIPT,
        fflags: NOTE_MACH_CONTINUOUS_TIME | NOTE_ABSOLUTE | NOTE_USECONDS,
        data: fire_at,
        ..Default::default()
    };
    let change = kev;
    let ret = env.kevent(Some(&change), Some(&mut kev), None);
    t.require(ret == 1, || format!("kevent64 returned {ret}"))?;
    t.require(kev.data == 0, || format!("an error returned: {}", kev.data))?;
    let timeout = Timespec {
        tv_sec: 10,
        tv_nsec: 0,
    };
    let mut event = Kevent64::default();
    let ret = env.kevent(None, Some(&mut event), Some(&timeout));
    t.require(ret == 1, || {
        format!("kevent() should have returned one event, got {ret}")
    })?;
    t.require(event.flags & EV_ERROR == 0, || {
        format!("event should not have EV_ERROR set: {}", event.data)
    })?;
    // SAFETY: no arguments.
    let missed_by = (env.abs_to_nanos(unsafe { (env.continuous)() }) as i64
        - env.abs_to_nanos(cnt_then) as i64)
        / 1_000_000;
    t.require(missed_by.abs() < 500, || {
        format!("timer should pop 3 sec in the future, missed by {missed_by}ms")
    })?;
    t.require(event.data == 1, || format!("event.data {}", event.data))?;
    t.require(event.ident == 2, || format!("event.ident {}", event.ident))?;
    let ret = env.kevent(None, Some(&mut event), Some(&timeout));
    t.require(ret == 0, || {
        format!("a periodic tick out of an absolute timer: {ret}")
    })?;
    t.require(event.flags & EV_ERROR == 0, || "EV_ERROR".into())?;
    // SAFETY: the queue this case opened.
    t.require(unsafe { (env.close)(env.kq) } == 0, || "close(kq)".into())?;
    Ok(())
}

/// `kevent_continuous_time_pops`: a 100ms continuous-time timer counts the
/// periods that end while nothing polls (upstream's simulated sleep).
fn continuous_pops(t: &mut T) -> Result<(), Stop> {
    let env = Env::new(t, false)?;
    // SAFETY: plain call.
    unsafe { (env.sleep)(2) };
    let mut kev = Kevent64 {
        ident: 3,
        filter: EVFILT_TIMER,
        flags: EV_ADD | EV_RECEIPT,
        fflags: NOTE_MACH_CONTINUOUS_TIME,
        data: 100,
        ..Default::default()
    };
    let change = kev;
    let ret = env.kevent(Some(&change), Some(&mut kev), None);
    t.require(ret == 1, || format!("kevent64 returned {ret}"))?;
    t.require(kev.data == 0, || format!("an error returned: {}", kev.data))?;
    let mut event = Kevent64::default();
    let ret = env.kevent(None, Some(&mut event), None);
    t.require(ret == 1, || {
        format!("kevent() should have returned one event, got {ret}")
    })?;
    t.require(event.flags & EV_ERROR == 0, || "EV_ERROR".into())?;
    t.require(event.ident == 3, || format!("event.ident {}", event.ident))?;
    // SAFETY: no arguments.
    let cnt_then = unsafe { (env.continuous)() };
    // SAFETY: plain call.
    unsafe { (env.sleep)(2) };
    // SAFETY: no arguments.
    let cnt_now = unsafe { (env.continuous)() };
    let ms_elapsed = env.abs_to_nanos(cnt_now - cnt_then) / NSEC_PER_MSEC;
    let ret = env.kevent(None, Some(&mut event), None);
    t.require(ret == 1, || {
        format!("kevent() should have returned one event, got {ret}")
    })?;
    t.require(event.ident == 3, || format!("event.ident {}", event.ident))?;
    let expected_pops = ms_elapsed / 100;
    let got_pops = event.data as u64;
    t.require(got_pops >= expected_pops - 1, || {
        format!("tracking pops while asleep: {got_pops} of {expected_pops}")
    })?;
    // SAFETY: the queue this case opened.
    t.require(unsafe { (env.close)(env.kq) } == 0, || "close(kq)".into())?;
    Ok(())
}

/// Ten milliseconds in Mach time units, as upstream computes it with
/// `nanos_to_abs` - here the timebase is read rather than assumed.
fn ten_ms_abs() -> i64 {
    imports! { timebase_info: TimebaseFn = "_mach_timebase_info"; }
    let mut tb = Timebase::default();
    if let Ok(f) = timebase_info.get() {
        // SAFETY: a writable `mach_timebase_info_data_t`.
        unsafe { f(&mut tb) };
    }
    if tb.numer == 0 {
        return (10 * NSEC_PER_MSEC) as i64;
    }
    (10 * NSEC_PER_MSEC * tb.denom as u64 / tb.numer as u64) as i64
}

const CASES: &[Case] = &[
    timer_case!("test_absolute_kevent(100, 0)", false, |t| absolute(
        t, 100, 0
    )),
    timer_case!("test_absolute_kevent(200, 0)", false, |t| absolute(
        t, 200, 0
    )),
    timer_case!("test_absolute_kevent(300, 0)", false, |t| absolute(
        t, 300, 0
    )),
    timer_case!("test_absolute_kevent(1000, 0)", false, |t| absolute(
        t, 1000, 0
    )),
    timer_case!("test_absolute_kevent(500, NOTE_USECONDS)", true, |t| {
        absolute(t, 500, NOTE_USECONDS)
    }),
    timer_case!("test_absolute_kevent(100, NOTE_USECONDS)", true, |t| {
        absolute(t, 100, NOTE_USECONDS)
    }),
    timer_case!("test_absolute_kevent(2, NOTE_SECONDS)", true, |t| {
        absolute(t, 2, NOTE_SECONDS)
    }),
    timer_case!("test_absolute_kevent(-1000, 0)", true, |t| absolute(
        t, -1000, 0
    )),
    timer_case!(
        "test_absolute_kevent(10ms in abs, NOTE_MACHTIME)",
        true,
        |t| { absolute(t, ten_ms_abs(), NOTE_MACHTIME) }
    ),
    timer_case!("test_oneshot_kevent(1, NOTE_SECONDS)", false, |t| {
        oneshot(t, 1, NOTE_SECONDS)
    }),
    timer_case!("test_oneshot_kevent(10, 0)", true, |t| oneshot(t, 10, 0)),
    timer_case!("test_oneshot_kevent(200, NOTE_USECONDS)", true, |t| {
        oneshot(t, 200, NOTE_USECONDS)
    }),
    timer_case!("test_oneshot_kevent(300000, NOTE_NSECONDS)", true, |t| {
        oneshot(t, 300_000, NOTE_NSECONDS)
    }),
    timer_case!("test_oneshot_kevent(-1, NOTE_SECONDS)", true, |t| {
        oneshot(t, -1, NOTE_SECONDS)
    }),
    timer_case!(
        "test_oneshot_kevent(10ms in abs, NOTE_MACHTIME)",
        true,
        |t| { oneshot(t, ten_ms_abs(), NOTE_MACHTIME) }
    ),
    timer_case!("test_interval_kevent(250 * 1000)", false, |t| interval(
        t, 250_000
    )),
    timer_case!("test_interval_kevent(5 * 1000)", true, |t| interval(
        t, 5_000
    )),
    timer_case!("test_interval_kevent(200)", true, |t| interval(t, 200)),
    timer_case!("test_interval_kevent(50)", true, |t| interval(t, 50)),
    timer_case!("test_interval_kevent(-1000)", false, |t| interval(t, -1000)),
    timer_case!("test_repeating_kevent(10000)", false, |t| repeating(
        t, 10_000
    )),
    timer_case!("test_updated_kevent(1000, 2000)", false, |t| updated(
        t, 1000, 2000
    )),
    timer_case!("test_updated_kevent(2000, 1000)", false, |t| updated(
        t, 2000, 1000
    )),
    timer_case!("test_updated_kevent(1000, -1)", true, |t| updated(
        t, 1000, -1
    )),
    Case {
        name: "kevent_continuous_time_periodic_tick",
        kind: Kind::Upstream,
        source: "kevent_continuous_time.c, run_sleep_tests = 0 as upstream",
        timeout_secs: 30,
        run: continuous_periodic_tick,
    },
    Case {
        name: "kevent_continuous_time_absolute",
        kind: Kind::Upstream,
        source: "kevent_continuous_time.c, run_sleep_tests = 0 as upstream",
        timeout_secs: 30,
        run: continuous_absolute,
    },
    Case {
        name: "kevent_continuous_time_pops",
        kind: Kind::Upstream,
        source: "kevent_continuous_time.c, run_sleep_tests = 0 as upstream",
        timeout_secs: 30,
        run: continuous_pops,
    },
];

#[test]
fn xnu_kevent_conformance() {
    conformance::run("xnu_kevent_conformance", "xnu", UPSTREAM, CASES);
}
