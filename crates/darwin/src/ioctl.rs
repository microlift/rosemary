/// Darwin's own request numbers, which the BSDs share: direction, size, group
/// and number are encoded the same way and the tty group is the same group.
#[cfg(not(target_os = "linux"))]
pub fn request(request: u64) -> Option<libc::c_ulong> {
    Some(request as libc::c_ulong)
}

#[cfg(target_os = "macos")]
pub unsafe extern "C" fn ioctl(fd: libc::c_int, request: u64, arg: usize) -> libc::c_int {
    unsafe { libc::ioctl(fd, request as libc::c_ulong, arg) }
}

#[cfg(target_os = "linux")]
pub fn request(request: u64) -> Option<libc::Ioctl> {
    // BSD encodes direction, size, group, and number while Linux's tty ioctls
    // use an unrelated numbering.  These payloads have identical layouts; a
    // request not listed here must not accidentally become a Linux ioctl.
    Some(match request {
        0x4008_7468 => libc::TIOCGWINSZ,
        0x8008_7467 => libc::TIOCSWINSZ,
        0x4004_667f => libc::FIONREAD,
        0x8004_667e => libc::FIONBIO,
        0x2000_6601 => libc::FIOCLEX,
        0x2000_6602 => libc::FIONCLEX,
        // The job-control and controlling-terminal requests, whose payloads are
        // an `int` process group, an `int` count or one `char` on both. A shell
        // and libedit ask `TIOCGPGRP` before they touch the terminal, and take a
        // refusal as "not a terminal".
        0x4004_7477 => libc::TIOCGPGRP,
        0x8004_7476 => libc::TIOCSPGRP,
        0x4004_7473 => libc::TIOCOUTQ,
        0x8001_7472 => libc::TIOCSTI,
        0x2000_7461 => libc::TIOCSCTTY,
        0x2000_7471 => libc::TIOCNOTTY,
        0x2000_740d => libc::TIOCEXCL,
        0x2000_740e => libc::TIOCNXCL,
        _ => return None,
    })
}

/// Fixed-width entry for Darwin's variadic `ioctl`.
///
/// A request with `IOC_VOID` is a two-argument call at the C level. The third
/// machine argument is intentionally ignored in that case, so this fixed ABI
/// entry also serves native guests without reading an argument they did not
/// supply.
#[cfg(not(target_os = "macos"))]
pub unsafe extern "C" fn ioctl(fd: libc::c_int, guest_request: u64, arg: usize) -> libc::c_int {
    let Some(request) = request(guest_request) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    let value = if guest_request & 0xe000_0000 == 0x2000_0000 {
        0
    } else {
        arg
    };
    unsafe { libc::ioctl(fd, request, value) }
}

#[cfg(not(target_os = "macos"))]
crate::stack_args! {
    darwin_ioctl = ioctl, 2, 1;
}

#[cfg(not(target_os = "macos"))]
exports::exports! {
    functions {
        "ioctl" => darwin_ioctl,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// libedit's "do I own this terminal" question has to reach the kernel.
    #[test]
    fn job_control_requests_are_translated() {
        assert_eq!(request(0x4004_7477), Some(libc::TIOCGPGRP));
        assert_eq!(request(0x8004_7476), Some(libc::TIOCSPGRP));
        assert_eq!(request(0x4008_7468), Some(libc::TIOCGWINSZ));
        assert_eq!(request(0xdead_beef), None);
    }
}
