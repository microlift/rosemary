#![cfg(target_os = "linux")]
use darwin::kqueue::{Kevent, kevent, kqueue};
#[test]
fn persistent_user_knote_cannot_starve_edge_read_with_capacity_one() {
    let q = kqueue();
    assert!(q >= 0);
    let mut pipe = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) },
        0
    );
    let user = Kevent {
        ident: 9999,
        filter: -10,
        flags: 1,
        ..Kevent::default()
    };
    let io = Kevent {
        ident: pipe[0] as usize,
        filter: -1,
        flags: 1 | 0x20,
        ..Kevent::default()
    };
    assert_eq!(
        unsafe {
            kevent(
                q,
                [user, io].as_ptr(),
                2,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        },
        0
    );
    let trigger = Kevent {
        ident: 9999,
        filter: -10,
        fflags: 0x0100_0000,
        ..Kevent::default()
    };
    assert_eq!(
        unsafe { kevent(q, &trigger, 1, std::ptr::null_mut(), 0, std::ptr::null()) },
        0
    );
    let buf = b"a";
    assert_eq!(unsafe { libc::write(pipe[1], buf.as_ptr().cast(), 1) }, 1);
    let zero = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut filters = Vec::new();
    for _ in 0..10 {
        let mut out = Kevent::default();
        let n = unsafe { kevent(q, std::ptr::null(), 0, &mut out, 1, &zero) };
        assert_eq!(n, 1);
        filters.push(out.filter);
    }
    assert!(filters.contains(&-10));
    assert_eq!(
        filters.iter().filter(|&&filter| filter == -1).count(),
        1,
        "the original unread edge must appear once, not disappear or repeat: {filters:?}"
    );
    let mut consumed = [0u8; 1];
    assert_eq!(
        unsafe { libc::read(pipe[0], consumed.as_mut_ptr().cast(), 1) },
        1
    );
    assert_eq!(unsafe { libc::write(pipe[1], buf.as_ptr().cast(), 1) }, 1);
    let mut subsequent_edges = 0;
    for _ in 0..10 {
        let mut out = Kevent::default();
        assert_eq!(
            unsafe { kevent(q, std::ptr::null(), 0, &mut out, 1, &zero) },
            1
        );
        if out.filter == -1 {
            subsequent_edges += 1;
        }
    }
    assert_eq!(
        subsequent_edges, 1,
        "new data must produce exactly one further edge"
    );
    for fd in [pipe[0], pipe[1], q] {
        assert_eq!(unsafe { darwin::net::descriptor::close(fd) }, 0)
    }
}

#[test]
fn two_persistent_user_knotes_do_not_consume_and_lose_a_third_read_edge() {
    let q = kqueue();
    assert!(q >= 0);
    let mut p = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) },
        0
    );
    let changes = [
        Kevent {
            ident: 1010,
            filter: -10,
            flags: 1,
            ..Kevent::default()
        },
        Kevent {
            ident: 1011,
            filter: -10,
            flags: 1,
            ..Kevent::default()
        },
        Kevent {
            ident: p[0] as usize,
            filter: -1,
            flags: 1 | 0x20,
            ..Kevent::default()
        },
    ];
    assert_eq!(
        unsafe {
            kevent(
                q,
                changes.as_ptr(),
                3,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        },
        0
    );
    for id in [1010, 1011] {
        let trigger = Kevent {
            ident: id,
            filter: -10,
            fflags: 0x0100_0000,
            ..Kevent::default()
        };
        assert_eq!(
            unsafe { kevent(q, &trigger, 1, std::ptr::null_mut(), 0, std::ptr::null()) },
            0
        );
    }
    assert_eq!(unsafe { libc::write(p[1], b"x".as_ptr().cast(), 1) }, 1);
    let z = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut reads = 0;
    for _ in 0..10 {
        let mut events = [Kevent::default(); 2];
        let n = unsafe { kevent(q, std::ptr::null(), 0, events.as_mut_ptr(), 2, &z) };
        assert!(n > 0);
        reads += events[..n as usize]
            .iter()
            .filter(|e| e.filter == -1)
            .count();
    }
    assert_eq!(
        reads, 1,
        "the READ edge must survive saturated two-user batches"
    );
    for fd in [p[0], p[1], q] {
        assert_eq!(unsafe { darwin::net::descriptor::close(fd) }, 0);
    }
}

#[test]
fn persistent_user_and_level_read_must_not_starve_pending_signal() {
    let q = kqueue();
    assert!(q >= 0);
    let mut p = [-1; 2];
    assert_eq!(
        unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) },
        0
    );
    let signum = darwin::signal::signum::SIGWINCH;
    let changes = [
        Kevent {
            ident: 8001,
            filter: -10,
            flags: 1,
            ..Kevent::default()
        },
        Kevent {
            ident: p[0] as usize,
            filter: -1,
            flags: 1,
            ..Kevent::default()
        },
        Kevent {
            ident: signum as usize,
            filter: -6,
            flags: 1,
            ..Kevent::default()
        },
    ];
    assert_eq!(
        unsafe {
            kevent(
                q,
                changes.as_ptr(),
                changes.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        },
        0
    );
    let trigger = Kevent {
        ident: 8001,
        filter: -10,
        fflags: 0x0100_0000,
        ..Kevent::default()
    };
    assert_eq!(
        unsafe { kevent(q, &trigger, 1, std::ptr::null_mut(), 0, std::ptr::null()) },
        0
    );
    assert_eq!(unsafe { libc::write(p[1], b"s".as_ptr().cast(), 1) }, 1);
    let host = darwin::signal::signal_to_host(signum).expect("SIGWINCH mapping");
    assert_eq!(unsafe { libc::raise(host) }, 0);
    let zero = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut observed = Vec::new();
    for _ in 0..12 {
        let mut out = Kevent::default();
        let n = unsafe { kevent(q, std::ptr::null(), 0, &mut out, 1, &zero) };
        assert_eq!(n, 1);
        observed.push(out.filter);
    }
    assert!(
        observed.contains(&-10),
        "USER never delivered: {observed:?}"
    );
    assert!(observed.contains(&-1), "READ never delivered: {observed:?}");
    assert!(
        observed.contains(&-6),
        "SIGNAL starved behind always-ready READ: {observed:?}"
    );
    for fd in [p[0], p[1], q] {
        assert_eq!(unsafe { darwin::net::descriptor::close(fd) }, 0);
    }
}
