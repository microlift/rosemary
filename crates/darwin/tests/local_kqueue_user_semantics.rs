#![cfg(any(target_os = "linux", target_os = "macos"))]
//! Portable public-kqueue USER semantics; shared code across Linux and macOS.
use darwin::kqueue::{Kevent, kevent, kqueue};
const USER: i16 = -10;
const ADD: u16 = 0x1;
const ENABLE: u16 = 0x4;
const DISABLE: u16 = 0x8;
const ONESHOT: u16 = 0x10;
const CLEAR: u16 = 0x20;
const DISPATCH: u16 = 0x80;
const TRIGGER: u32 = 0x0100_0000;
const COPY: u32 = 0xc000_0000;
const OR: u32 = 0x8000_0000;
const AND: u32 = 0x4000_0000;

fn change(q: i32, c: Kevent) {
    assert_eq!(
        unsafe { kevent(q, &c, 1, std::ptr::null_mut(), 0, std::ptr::null()) },
        0
    );
}
fn poll(q: i32) -> Option<Kevent> {
    let zero = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut event = Kevent::default();
    let n = unsafe { kevent(q, std::ptr::null(), 0, &mut event, 1, &zero) };
    assert!(matches!(n, 0 | 1), "kevent returned {n}");
    (n == 1).then_some(event)
}
fn add(q: i32, id: usize, flags: u16) {
    change(
        q,
        Kevent {
            ident: id,
            filter: USER,
            flags: ADD | flags,
            ..Kevent::default()
        },
    );
}
fn fire(q: i32, id: usize, fflags: u32, data: isize) {
    change(
        q,
        Kevent {
            ident: id,
            filter: USER,
            fflags: fflags | TRIGGER,
            data,
            ..Kevent::default()
        },
    );
}

#[test]
fn persistent_user_event_repeats_until_clear_or_deletion() {
    let q = kqueue();
    assert!(q >= 0);
    add(q, 600, 0);
    fire(q, 600, 0, 7);
    for _ in 0..3 {
        let event = poll(q).expect("persistent NOTE_TRIGGER must stay active");
        assert_eq!((event.filter, event.ident, event.data), (USER, 600, 7));
    }
    assert_eq!(unsafe { libc::close(q) }, 0);
}

#[test]
fn user_clear_dispatch_and_oneshot_are_independent_semantics() {
    for (flags, should_reactivate_on_enable) in [(CLEAR, false), (DISPATCH, true), (ONESHOT, false)]
    {
        let q = kqueue();
        assert!(q >= 0);
        add(q, 601, flags);
        fire(q, 601, 0, 5);
        assert_eq!(poll(q).expect("initial trigger").ident, 601);
        assert!(poll(q).is_none(), "once after CLEAR, DISPATCH, or ONESHOT");
        if should_reactivate_on_enable {
            change(
                q,
                Kevent {
                    ident: 601,
                    filter: USER,
                    flags: ENABLE,
                    ..Kevent::default()
                },
            );
            assert_eq!(
                poll(q)
                    .expect("DISPATCH latches event while disabled")
                    .ident,
                601
            );
        } else if flags == CLEAR {
            fire(q, 601, 0, 6);
            assert_eq!(poll(q).expect("new NOTE_TRIGGER after CLEAR").data, 6);
            assert!(poll(q).is_none());
        }
        assert_eq!(unsafe { libc::close(q) }, 0);
    }
}

#[test]
fn user_fflags_and_data_respond_to_updates_and_disabled_trigger_latches() {
    let q = kqueue();
    assert!(q >= 0);
    change(
        q,
        Kevent {
            ident: 602,
            filter: USER,
            flags: ADD | CLEAR,
            fflags: COPY | 0b0101,
            udata: 44,
            ..Kevent::default()
        },
    );
    for (controls, data, expected) in [
        (OR | 0b0010, 11, COPY | 0b0111),
        (AND | 0b0011, 12, 0b0011),
        (COPY | 0b1000, 13, 0b1000),
    ] {
        fire(q, 602, controls, data);
        let ev = poll(q).expect("user trigger");
        assert_eq!((ev.fflags, ev.data, ev.udata), (expected, data, 0));
        assert_eq!(ev.flags, ADD | CLEAR);
        assert!(poll(q).is_none(), "CLEAR removes active state");
    }
    change(
        q,
        Kevent {
            ident: 602,
            filter: USER,
            flags: DISABLE,
            ..Kevent::default()
        },
    );
    fire(q, 602, 0, 77);
    assert!(poll(q).is_none());
    change(
        q,
        Kevent {
            ident: 602,
            filter: USER,
            flags: ENABLE,
            ..Kevent::default()
        },
    );
    assert_eq!(poll(q).expect("disabled trigger is latched").fflags, 0b1000);
    assert_eq!(unsafe { libc::close(q) }, 0);
}

#[test]
fn persistent_user_batch_contains_each_active_knote_once() {
    let q = kqueue();
    assert!(q >= 0);
    for id in [701usize, 702] {
        add(q, id, 0);
        fire(q, id, 0, id as isize);
    }
    let zero = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut output = [Kevent::default(); 4];
    for _ in 0..3 {
        let count = unsafe { kevent(q, std::ptr::null(), 0, output.as_mut_ptr(), 4, &zero) };
        assert_eq!(
            count, 2,
            "persistent user knotes should not duplicate within one batch"
        );
        let mut ids = [output[0].ident, output[1].ident];
        ids.sort_unstable();
        assert_eq!(ids, [701, 702]);
    }
    assert_eq!(unsafe { libc::close(q) }, 0);
}

#[test]
fn oneshot_user_knote_is_removed_on_delivery() {
    let q = kqueue();
    assert!(q >= 0);
    add(q, 703, ONESHOT);
    fire(q, 703, 0, 5);
    assert_eq!(poll(q).expect("first delivery").data, 5);
    let again = Kevent {
        ident: 703,
        filter: USER,
        fflags: TRIGGER,
        ..Kevent::default()
    };
    assert_eq!(
        unsafe { kevent(q, &again, 1, std::ptr::null_mut(), 0, std::ptr::null()) },
        -1,
        "NOTE_TRIGGER after EV_ONESHOT must reject the deleted knote"
    );
    assert_eq!(unsafe { libc::close(q) }, 0);
}

#[test]
fn deleting_other_filter_with_same_ident_does_not_cancel_user_trigger() {
    let q = kqueue();
    assert!(q >= 0);
    let mut p = [-1; 2];
    assert_eq!(unsafe { libc::pipe(p.as_mut_ptr()) }, 0);
    for fd in p {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    let id = p[0] as usize;
    add(q, id, 0);
    fire(q, id, 0, 23);
    change(
        q,
        Kevent {
            ident: id,
            filter: -1,
            flags: ADD,
            ..Kevent::default()
        },
    );
    change(
        q,
        Kevent {
            ident: id,
            filter: -1,
            flags: 2,
            ..Kevent::default()
        },
    );
    let event = poll(q).expect("READ deletion must not clear a separate USER registration");
    assert_eq!((event.filter, event.ident, event.data), (USER, id, 23));
    assert_eq!(
        poll(q)
            .expect("persistent trigger must remain active")
            .ident,
        id
    );
    for fd in [p[0], p[1], q] {
        assert_eq!(unsafe { darwin::net::descriptor::close(fd) }, 0);
    }
}

#[test]
fn user_metadata_and_initial_control_bits_match_native_xnu() {
    let q = kqueue();
    assert!(q >= 0);
    change(
        q,
        Kevent {
            ident: 8011,
            filter: USER,
            flags: ADD | CLEAR,
            fflags: COPY | 0b0101,
            data: 17,
            udata: 111,
        },
    );
    change(
        q,
        Kevent {
            ident: 8011,
            filter: USER,
            fflags: OR | TRIGGER | 0b0010,
            data: 23,
            udata: 222,
            ..Kevent::default()
        },
    );
    let got = poll(q).expect("USER activation");
    assert_eq!(
        (got.flags, got.fflags, got.data, got.udata),
        (ADD | CLEAR, COPY | 0b0111, 23, 222)
    );
    assert!(poll(q).is_none());

    // A fresh EV_ADD stores the unmodified initial NOTE_FFAND and NOTE_TRIGGER
    // bits, unlike an update that applies the 24-bit control operation.
    change(
        q,
        Kevent {
            ident: 8012,
            filter: USER,
            flags: ADD,
            fflags: AND | TRIGGER | 0b0101,
            data: 9,
            udata: 77,
        },
    );
    let got = poll(q).expect("triggered on initial add");
    assert_eq!(
        (got.flags, got.fflags, got.data, got.udata),
        (ADD, AND | TRIGGER | 0b0101, 9, 77)
    );
    assert_eq!(unsafe { darwin::net::descriptor::close(q) }, 0);
}

#[test]
fn repeated_add_preserves_initial_delivery_mode_and_updates_metadata() {
    for (initial_flags, attempted_flags, persistent) in [
        (ADD, CLEAR, true),
        (ADD, DISPATCH, true),
        (ADD, ONESHOT, true),
        (CLEAR, 0, false),
        (ONESHOT, 0, false),
        (DISPATCH, CLEAR, false),
    ] {
        let q = kqueue();
        assert!(q >= 0);
        change(
            q,
            Kevent {
                ident: 8020,
                filter: USER,
                flags: ADD | initial_flags,
                data: 10,
                udata: 100,
                ..Kevent::default()
            },
        );
        fire(q, 8020, 0, 11);
        change(
            q,
            Kevent {
                ident: 8020,
                filter: USER,
                flags: ADD | attempted_flags,
                data: 12,
                udata: 102,
                ..Kevent::default()
            },
        );
        let first = poll(q).expect("initial trigger must survive re-registration");
        assert_eq!(first.flags, ADD | initial_flags);
        assert_eq!((first.data, first.udata), (12, 102));
        assert_eq!(
            poll(q).is_some(),
            persistent,
            "initial {initial_flags:#x}, attempted {attempted_flags:#x}"
        );
        assert_eq!(unsafe { libc::close(q) }, 0);
    }
}

#[test]
fn repeated_add_does_not_reenable_disabled_trigger_without_explicit_enable() {
    let q = kqueue();
    assert!(q >= 0);
    change(
        q,
        Kevent {
            ident: 8021,
            filter: USER,
            flags: ADD | DISABLE,
            ..Kevent::default()
        },
    );
    fire(q, 8021, 0, 11);
    change(
        q,
        Kevent {
            ident: 8021,
            filter: USER,
            flags: ADD,
            data: 12,
            udata: 102,
            ..Kevent::default()
        },
    );
    assert!(
        poll(q).is_none(),
        "EV_ADD must not reenable a disabled knote"
    );
    change(
        q,
        Kevent {
            ident: 8021,
            filter: USER,
            flags: ENABLE,
            data: 13,
            udata: 103,
            ..Kevent::default()
        },
    );
    for _ in 0..2 {
        let event = poll(q).expect("disabled trigger should latch until explicit enable");
        assert_eq!(
            (event.flags, event.data, event.udata),
            (ADD | DISABLE, 13, 103)
        );
    }
    assert_eq!(unsafe { libc::close(q) }, 0);
}

#[test]
fn dispatch_disable_survives_readd_and_requires_explicit_enable() {
    let q = kqueue();
    assert!(q >= 0);
    add(q, 8022, DISPATCH);
    fire(q, 8022, 0, 11);
    assert_eq!(poll(q).expect("initial dispatch").flags, ADD | DISPATCH);
    change(
        q,
        Kevent {
            ident: 8022,
            filter: USER,
            flags: ADD | CLEAR,
            data: 12,
            udata: 102,
            ..Kevent::default()
        },
    );
    assert!(
        poll(q).is_none(),
        "re-registration must not undo EV_DISPATCH"
    );
    change(
        q,
        Kevent {
            ident: 8022,
            filter: USER,
            flags: ENABLE,
            data: 13,
            udata: 103,
            ..Kevent::default()
        },
    );
    let event = poll(q).expect("explicit EV_ENABLE should release the latched event");
    assert_eq!(
        (event.flags, event.data, event.udata),
        (ADD | DISPATCH, 13, 103)
    );
    assert!(
        poll(q).is_none(),
        "original dispatch mode must remain in effect"
    );
    assert_eq!(unsafe { libc::close(q) }, 0);
}

#[test]
fn repeated_add_respects_explicit_disable_enable_without_changing_mode() {
    let q = kqueue();
    assert!(q >= 0);
    add(q, 8023, 0);
    fire(q, 8023, 0, 11);
    change(
        q,
        Kevent {
            ident: 8023,
            filter: USER,
            flags: ADD | DISABLE | CLEAR,
            ..Kevent::default()
        },
    );
    assert!(poll(q).is_none());
    change(
        q,
        Kevent {
            ident: 8023,
            filter: USER,
            flags: ADD | ENABLE | CLEAR,
            data: 12,
            udata: 102,
            ..Kevent::default()
        },
    );
    for _ in 0..2 {
        let event = poll(q).expect("EV_ADD | EV_ENABLE should enable, not change mode");
        assert_eq!((event.flags, event.data, event.udata), (ADD, 12, 102));
    }
    assert_eq!(unsafe { libc::close(q) }, 0);
}

#[test]
fn simultaneous_explicit_enable_and_disable_keep_knote_disabled() {
    let q = kqueue();
    assert!(q >= 0);
    add(q, 8024, 0);
    fire(q, 8024, 0, 11);
    change(
        q,
        Kevent {
            ident: 8024,
            filter: USER,
            flags: ADD | ENABLE | DISABLE,
            ..Kevent::default()
        },
    );
    assert!(poll(q).is_none(), "explicit EV_DISABLE wins over EV_ENABLE");
    change(
        q,
        Kevent {
            ident: 8024,
            filter: USER,
            flags: ENABLE,
            data: 13,
            udata: 103,
            ..Kevent::default()
        },
    );
    let event = poll(q).expect("pending trigger must survive both control bits");
    assert_eq!((event.flags, event.data, event.udata), (ADD, 13, 103));
    assert_eq!(unsafe { libc::close(q) }, 0);
}

fn wait_no_event_for(q: i32, milliseconds: u64) {
    let timeout = libc::timespec {
        tv_sec: (milliseconds / 1000) as libc::time_t,
        tv_nsec: ((milliseconds % 1000) * 1_000_000) as libc::c_long,
    };
    let mut event = Kevent::default();
    let start = std::time::Instant::now();
    let n = unsafe { kevent(q, std::ptr::null(), 0, &mut event, 1, &timeout) };
    let elapsed = start.elapsed();
    assert_eq!(n, 0, "unexpected event after USER state became inactive");
    assert!(
        elapsed >= std::time::Duration::from_millis(milliseconds.saturating_sub(30)),
        "USER wakeup shortened {milliseconds} ms wait to {elapsed:?}"
    );
}

#[test]
fn completed_user_delivery_does_not_leave_wake_for_next_timed_wait() {
    for (flags, after_delivery) in [
        (CLEAR, 0),
        (ONESHOT, 0),
        (DISPATCH, 0),
        (0, DISABLE),
        (0, 2),
    ] {
        let q = kqueue();
        assert!(q >= 0);
        add(q, 8077, flags);
        fire(q, 8077, 0, 99);
        assert_eq!(poll(q).expect("USER event must be delivered").ident, 8077);
        if after_delivery != 0 {
            change(
                q,
                Kevent {
                    ident: 8077,
                    filter: USER,
                    flags: after_delivery,
                    ..Kevent::default()
                },
            );
        }
        // Match the 400 ms timeout from the independent report for EV_CLEAR.
        wait_no_event_for(q, if flags == CLEAR { 400 } else { 130 });
        assert_eq!(unsafe { libc::close(q) }, 0);
    }
}

#[test]
fn cancellation_before_user_delivery_does_not_leave_wake_for_timed_wait() {
    for flags in [DISABLE, 2] {
        let q = kqueue();
        assert!(q >= 0);
        add(q, 8078, 0);
        fire(q, 8078, 0, 98);
        change(
            q,
            Kevent {
                ident: 8078,
                filter: USER,
                flags,
                ..Kevent::default()
            },
        );
        wait_no_event_for(q, 130);
        assert_eq!(unsafe { libc::close(q) }, 0);
    }
}

#[test]
fn future_user_trigger_still_wakes_real_blocking_wait_after_fast_delivery() {
    let q = kqueue();
    assert!(q >= 0);
    add(q, 8079, CLEAR);
    fire(q, 8079, 0, 97);
    assert_eq!(poll(q).expect("initial USER trigger").ident, 8079);
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(40));
        fire(q, 8079, 0, 98);
    });
    let timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 500_000_000,
    };
    let mut event = Kevent::default();
    let start = std::time::Instant::now();
    let n = unsafe { kevent(q, std::ptr::null(), 0, &mut event, 1, &timeout) };
    let elapsed = start.elapsed();
    trigger.join().expect("triggering thread");
    assert_eq!(n, 1, "trigger should wake the wait, not return empty");
    assert_eq!((event.filter, event.ident, event.data), (USER, 8079, 98));
    assert!(
        elapsed >= std::time::Duration::from_millis(20),
        "returned before new trigger: {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(450),
        "did not wake promptly: {elapsed:?}"
    );
    assert_eq!(unsafe { libc::close(q) }, 0);
}

#[test]
fn signal_notification_survives_user_first_wake_drain() {
    let q = kqueue();
    assert!(q >= 0);
    add(q, 8080, CLEAR);
    let signal = darwin::signal::signum::SIGWINCH;
    change(
        q,
        Kevent {
            ident: signal as usize,
            filter: -6,
            flags: ADD,
            ..Kevent::default()
        },
    );
    fire(q, 8080, 0, 10);
    let host = darwin::signal::signal_to_host(signal).expect("SIGWINCH must map to host");
    assert_eq!(unsafe { libc::raise(host) }, 0);
    let first = poll(q).expect("USER-first notification");
    assert_eq!(first.filter, USER);
    let second = poll(q).expect("pending signal cannot be consumed with USER wake");
    assert_eq!(
        (second.filter, second.ident, second.data),
        (-6, signal as usize, 1)
    );
    wait_no_event_for(q, 110);
    assert_eq!(unsafe { libc::close(q) }, 0);
}

#[test]
fn combined_clear_dispatch_requires_new_trigger_after_reenable() {
    let q = kqueue();
    assert!(q >= 0);
    add(q, 8090, CLEAR | DISPATCH);
    fire(q, 8090, 0, 51);
    let event = poll(q).expect("combined mode must deliver its first trigger");
    assert_eq!(
        (event.filter, event.flags, event.data),
        (USER, ADD | CLEAR | DISPATCH, 51)
    );
    assert!(
        poll(q).is_none(),
        "EV_DISPATCH must disable the registration"
    );
    change(
        q,
        Kevent {
            ident: 8090,
            filter: USER,
            flags: ENABLE,
            ..Kevent::default()
        },
    );
    assert!(
        poll(q).is_none(),
        "EV_CLEAR must discard the delivered trigger despite EV_DISPATCH"
    );
    fire(q, 8090, 0, 52);
    let event = poll(q).expect("new NOTE_TRIGGER must fire after explicit EV_ENABLE");
    assert_eq!(
        (event.filter, event.flags, event.data),
        (USER, ADD | CLEAR | DISPATCH, 52)
    );
    assert!(
        poll(q).is_none(),
        "combined mode disables and clears each delivery"
    );
    assert_eq!(unsafe { libc::close(q) }, 0);
}

fn queue_fd_readable(q: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd: q,
        events: libc::POLLIN,
        revents: 0,
    };
    let n = unsafe { libc::poll(&raw mut pfd, 1, 0) };
    assert!(n >= 0, "host poll failed");
    n == 1 && pfd.revents & libc::POLLIN != 0
}

#[test]
fn persistent_user_keeps_host_queue_fd_readable_until_deactivated() {
    for (end_flags, once_flags) in [(2, 0), (DISABLE, 0), (0, CLEAR), (0, ONESHOT)] {
        let q = kqueue();
        assert!(q >= 0);
        let original = if once_flags != 0 { once_flags } else { 0 };
        add(q, 8091, original);
        fire(q, 8091, 0, 42);
        assert!(queue_fd_readable(q), "new USER must wake host poll");
        assert_eq!(poll(q).expect("first USER event").ident, 8091);
        if original == 0 {
            assert!(
                queue_fd_readable(q),
                "persistent USER must keep kqueue FD readable"
            );
            assert_eq!(poll(q).expect("second USER event").ident, 8091);
            assert!(queue_fd_readable(q), "persistent USER must remain readable");
            change(
                q,
                Kevent {
                    ident: 8091,
                    filter: USER,
                    flags: end_flags,
                    ..Kevent::default()
                },
            );
        }
        assert!(
            !queue_fd_readable(q),
            "ended USER must retire host poll readiness (end_flags={end_flags:#x}, original={original:#x})"
        );
        wait_no_event_for(q, 100);
        assert_eq!(unsafe { libc::close(q) }, 0);
    }
}

/// Release-only timing probe, deliberately excluded from normal correctness
/// suites. Run with `cargo test --release -p darwin --test
/// local_kqueue_user_semantics release_user_wakeup_microbenchmark -- --ignored
/// --nocapture --test-threads=1`. These are end-to-end API costs, not the cost
/// of an individual system call or a performance acceptance threshold.
#[test]
#[ignore = "release-mode diagnostic: timed USER notification cycles"]
fn release_user_wakeup_microbenchmark() {
    use std::time::Instant;

    let iterations = std::env::var("ROSEMARY_USER_BENCH_ITERATIONS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| (100..=1_000_000).contains(&n))
        .unwrap_or(10_000);
    const REPEATS: usize = 5;
    const WARMUP: usize = 500;

    fn cycle(q: i32, i: usize) {
        fire(q, 8099, 0, i as isize);
        let event = poll(q).expect("USER trigger must produce one event");
        assert_eq!(
            (event.filter, event.ident, event.data),
            (USER, 8099, i as isize)
        );
        std::hint::black_box(event);
    }

    fn measure(flags: u16, iterations: usize) -> Vec<f64> {
        let q = kqueue();
        assert!(q >= 0);
        add(q, 8099, flags);
        for i in 0..WARMUP {
            cycle(q, i);
        }
        let mut samples = Vec::with_capacity(REPEATS);
        for trial in 0..REPEATS {
            let start = Instant::now();
            for i in 0..iterations {
                cycle(q, trial * iterations + i);
            }
            samples.push(start.elapsed().as_secs_f64() * 1_000_000_000.0 / iterations as f64);
        }
        // A cleared registration must not leave host queue readability behind.
        if flags & CLEAR != 0 {
            assert!(!queue_fd_readable(q));
        }
        assert_eq!(unsafe { libc::close(q) }, 0);
        samples
    }

    println!(
        "USER_WAKE_BENCH platform={} release={} iterations={} trials={} calls_per_cycle=2",
        std::env::consts::OS,
        !cfg!(debug_assertions),
        iterations,
        REPEATS
    );
    // Same two public `kevent` operations per iteration. Persistent USER
    // remains level-active; CLEAR alternates active/inactive and exercises
    // wake-token retirement, including macOS host knote reset.
    for (label, flags) in [("persistent", 0), ("ev_clear", CLEAR)] {
        let mut samples = measure(flags, iterations);
        samples.sort_by(f64::total_cmp);
        println!(
            "USER_WAKE_BENCH case={label} ns_per_cycle_median={:.1} ns_per_cycle_min={:.1} ns_per_cycle_max={:.1}",
            samples[REPEATS / 2],
            samples[0],
            samples[REPEATS - 1]
        );
    }
}
