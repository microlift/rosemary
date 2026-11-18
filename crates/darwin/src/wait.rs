#![cfg(not(target_os = "macos"))]

use std::ffi::{c_int, c_void};

const DARWIN_WNOHANG: c_int = 0x01;
const DARWIN_WUNTRACED: c_int = 0x02;
const DARWIN_WCONTINUED: c_int = 0x10;

/// Darwin's status for a continued child: `_WSTOPPED` with `SIGCONT`, 19.
const DARWIN_CONTINUED: c_int = 0x137f;

/// Darwin's options as this host's; `None` for a bit with no meaning here.
fn options_to_host(options: c_int) -> Option<c_int> {
    let known = DARWIN_WNOHANG | DARWIN_WUNTRACED | DARWIN_WCONTINUED;
    if options & !known != 0 {
        return None;
    }
    let mut host = 0;
    if options & DARWIN_WNOHANG != 0 {
        host |= libc::WNOHANG;
    }
    if options & DARWIN_WUNTRACED != 0 {
        host |= libc::WUNTRACED;
    }
    if options & DARWIN_WCONTINUED != 0 {
        host |= libc::WCONTINUED;
    }
    Some(host)
}

/// This host's status word as Darwin's.
fn status_to_darwin(status: c_int) -> c_int {
    let signal = |host: c_int| crate::signal::signal_from_host(host).unwrap_or(host);
    if libc::WIFCONTINUED(status) {
        DARWIN_CONTINUED
    } else if libc::WIFSTOPPED(status) {
        0x7f | (signal(libc::WSTOPSIG(status)) << 8)
    } else if libc::WIFSIGNALED(status) {
        signal(libc::WTERMSIG(status)) | (status & 0x80)
    } else {
        // Exited: the code in the second byte, the same on both.
        status
    }
}

/// The shared body: ask the host, and translate the answer.
///
/// # Safety
///
/// `status` is null or writable, and `rusage` is null or a writable Darwin
/// `struct rusage`.
unsafe fn reap(
    pid: libc::pid_t,
    status: *mut c_int,
    options: c_int,
    rusage: *mut c_void,
) -> libc::pid_t {
    let Some(host_options) = options_to_host(options) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    let mut host_status = 0;
    // SAFETY: a local for the status, and the caller's `rusage`, whose layout
    // the module note explains.
    let reaped = unsafe { libc::wait4(pid, &mut host_status, host_options, rusage.cast()) };
    if reaped > 0 && !status.is_null() {
        unsafe { *status = status_to_darwin(host_status) };
    }
    reaped
}

const DARWIN_WEXITED: c_int = 0x04;
const DARWIN_WSTOPPED: c_int = 0x08;
const DARWIN_WNOWAIT: c_int = 0x20;

/// `waitid`'s options as this host's: the bits are numbered differently.
fn id_options_to_host(options: c_int) -> Option<c_int> {
    let known =
        DARWIN_WNOHANG | DARWIN_WEXITED | DARWIN_WSTOPPED | DARWIN_WCONTINUED | DARWIN_WNOWAIT;
    if options & !known != 0 {
        return None;
    }
    let bits = [
        (DARWIN_WNOHANG, libc::WNOHANG),
        (DARWIN_WEXITED, libc::WEXITED),
        (DARWIN_WSTOPPED, libc::WSTOPPED),
        (DARWIN_WCONTINUED, libc::WCONTINUED),
        (DARWIN_WNOWAIT, libc::WNOWAIT),
    ];
    Some(
        bits.iter()
            .filter(|(darwin, _)| options & darwin != 0)
            .fold(0, |host, (_, bit)| host | bit),
    )
}

type DarwinId = u32;

/// `int waitid(idtype_t idtype, id_t id, siginfo_t *info, int options)`
///
/// `idtype_t` (`P_ALL`, `P_PID`, `P_PGID`) is numbered alike on both; the
/// options and the `siginfo_t` are not.
///
/// # Safety
///
/// `info` is null or a writable Darwin `siginfo_t`.
pub unsafe extern "C" fn waitid(
    idtype: c_int,
    id: DarwinId,
    info: *mut crate::signal::Siginfo,
    options: c_int,
) -> c_int {
    let Some(host_options) = id_options_to_host(options) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    let idtype = match idtype {
        0 => libc::P_ALL,
        1 => libc::P_PID,
        2 => libc::P_PGID,
        _ => {
            crate::errno::set(libc::EINVAL);
            return -1;
        }
    };
    // SAFETY: zeroed is a valid `siginfo_t`, and is what a `WNOHANG` with
    // nothing to report must leave behind.
    let mut host: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: a local `siginfo_t`.
    if unsafe { libc::waitid(idtype, id as libc::id_t, &mut host, host_options) } != 0 {
        return -1;
    }
    if !info.is_null() {
        // SAFETY: a host `siginfo_t` the kernel filled in.
        let host_info = unsafe { starnix::signal::info(&host) };
        let darwin = match host_info.signo {
            0 => 0,
            signo => crate::signal::signal_from_host(signo).unwrap_or(signo),
        };
        let converted = crate::signal::Siginfo::new(darwin, &host_info);
        // SAFETY: the caller's structure.
        unsafe { info.write_unaligned(converted) };
    }
    0
}

/// `pid_t wait(int *status)`
///
/// # Safety
///
/// As [`reap`].
pub unsafe extern "C" fn wait(status: *mut c_int) -> libc::pid_t {
    unsafe { reap(-1, status, 0, std::ptr::null_mut()) }
}

/// `pid_t waitpid(pid_t pid, int *status, int options)`
///
/// # Safety
///
/// As [`reap`].
pub unsafe extern "C" fn waitpid(
    pid: libc::pid_t,
    status: *mut c_int,
    options: c_int,
) -> libc::pid_t {
    unsafe { reap(pid, status, options, std::ptr::null_mut()) }
}

/// `pid_t wait3(int *status, int options, struct rusage *rusage)`
///
/// # Safety
///
/// As [`reap`].
pub unsafe extern "C" fn wait3(
    status: *mut c_int,
    options: c_int,
    rusage: *mut c_void,
) -> libc::pid_t {
    unsafe { reap(-1, status, options, rusage) }
}

/// `pid_t wait4(pid_t pid, int *status, int options, struct rusage *rusage)`
///
/// # Safety
///
/// As [`reap`].
pub unsafe extern "C" fn wait4(
    pid: libc::pid_t,
    status: *mut c_int,
    options: c_int,
    rusage: *mut c_void,
) -> libc::pid_t {
    unsafe { reap(pid, status, options, rusage) }
}

exports::exports! {
    functions {
        "wait" | "wait$NOCANCEL" => wait,
        "waitpid" | "waitpid$NOCANCEL" => waitpid,
        "wait3" => wait3,
        "wait4" | "wait4$NOCANCEL" => wait4,
        "waitid" | "waitid$NOCANCEL" => waitid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_control_options_are_accepted() {
        assert_eq!(
            options_to_host(DARWIN_WUNTRACED | DARWIN_WCONTINUED),
            Some(libc::WUNTRACED | libc::WCONTINUED)
        );
        assert_eq!(options_to_host(0x1000), None);
    }

    #[test]
    fn waitid_options_are_renumbered() {
        assert_eq!(
            id_options_to_host(DARWIN_WEXITED | DARWIN_WNOHANG),
            Some(libc::WEXITED | libc::WNOHANG)
        );
        assert_eq!(id_options_to_host(DARWIN_WSTOPPED), Some(libc::WSTOPPED));
        assert_eq!(id_options_to_host(DARWIN_WNOWAIT), Some(libc::WNOWAIT));
        assert_eq!(id_options_to_host(0x02), None);
    }

    #[test]
    fn waitid_reports_a_child_in_darwins_layout() {
        // SAFETY: the child only exits.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: as above.
            unsafe { libc::_exit(7) };
        }
        let mut info = std::mem::MaybeUninit::<crate::signal::Siginfo>::zeroed();
        // SAFETY: a writable structure.
        let rc = unsafe { waitid(1, pid as u32, info.as_mut_ptr(), DARWIN_WEXITED) };
        assert_eq!(rc, 0);
        // SAFETY: zeroed, then written by `waitid`.
        let info = unsafe { info.assume_init() };
        assert_eq!(info.signo, crate::signal::signum::SIGCHLD);
        assert_eq!((info.pid, info.code, info.status), (pid, 1, 7));
    }

    #[test]
    fn statuses_carry_darwins_signal_numbers() {
        // Exited with 3.
        assert_eq!(status_to_darwin(3 << 8), 3 << 8);
        // Stopped by the host's SIGTSTP, which is Darwin's 18.
        assert_eq!(
            status_to_darwin(0x7f | (libc::SIGTSTP << 8)),
            0x7f | (18 << 8)
        );
        // Killed by the host's SIGUSR1, Darwin's 30, with a core.
        assert_eq!(status_to_darwin(libc::SIGUSR1 | 0x80), 30 | 0x80);
        // Continued, which each host spells its own way.
        assert_eq!(status_to_darwin(HOST_CONTINUED), DARWIN_CONTINUED);
    }

    #[cfg(target_os = "linux")]
    const HOST_CONTINUED: c_int = 0xffff;
    #[cfg(target_os = "freebsd")]
    const HOST_CONTINUED: c_int = 0x13;

    #[test]
    fn a_child_is_reaped_with_its_exit_status() {
        // SAFETY: the child only exits.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: leaves the child without running the test harness's
            // exit handlers.
            unsafe { libc::_exit(7) };
        }
        let mut status = 0;
        // SAFETY: a local for the status.
        let reaped = unsafe { waitpid(pid, &mut status, DARWIN_WUNTRACED | DARWIN_WCONTINUED) };
        assert_eq!(reaped, pid);
        assert_eq!(status, 7 << 8);
    }
}
