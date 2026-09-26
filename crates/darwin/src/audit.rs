#![cfg(not(target_os = "macos"))]

//! The BSM audit session a guest asks about.
//!
//! Darwin puts every process in an audit session, and Security's
//! `SessionGetInfo` reads its flags to decide whether the caller may put up UI.
//! Linux's audit subsystem has the same two ids - the login uid and the session
//! id - so those are answered where the host has them, and the flags are
//! derived from what this process can actually reach.

use std::ffi::c_int;

/// Unset, in both Linux's `/proc/self/loginuid` and Darwin's `AU_DEFAUDITID`.
const UNSET: u32 = u32::MAX;

const AU_IPV4: u32 = 4;

const AU_SESSION_FLAG_HAS_GRAPHIC_ACCESS: u64 = 0x0010;
const AU_SESSION_FLAG_HAS_TTY: u64 = 0x0020;
const AU_SESSION_FLAG_IS_REMOTE: u64 = 0x1000;
const AU_SESSION_FLAG_HAS_CONSOLE_ACCESS: u64 = 0x2000;
const AU_SESSION_FLAG_HAS_AUTHENTICATED: u64 = 0x4000;

/// Darwin's `struct auditinfo_addr`, from `<bsm/audit.h>`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AuditinfoAddr {
    auid: u32,
    /// `au_mask_t`: success and failure. No event is preselected here, since
    /// nothing on this host would record it.
    mask: [u32; 2],
    /// `au_tid_addr_t`: port, address type, and up to an IPv6 address.
    port: i32,
    kind: u32,
    addr: [u32; 4],
    asid: i32,
    flags: u64,
}

const _: () = {
    assert!(size_of::<AuditinfoAddr>() == 48);
    assert!(std::mem::offset_of!(AuditinfoAddr, asid) == 36);
    assert!(std::mem::offset_of!(AuditinfoAddr, flags) == 40);
};

fn proc_id(path: &str) -> Option<u32> {
    let id = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    (id != UNSET).then_some(id)
}

/// Whether this process has a controlling terminal: `tty_nr` in
/// `/proc/self/stat`, after the parenthesised name that may hold spaces.
fn has_tty() -> bool {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return false;
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    rest.split_whitespace()
        .nth(4)
        .and_then(|tty| tty.parse::<i64>().ok())
        .is_some_and(|tty| tty != 0)
}

fn session() -> AuditinfoAddr {
    let env = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    let remote = env("SSH_CONNECTION") || env("SSH_CLIENT");
    let graphic = env("WAYLAND_DISPLAY") || env("DISPLAY");
    let mut flags = AU_SESSION_FLAG_HAS_AUTHENTICATED;
    if graphic {
        flags |= AU_SESSION_FLAG_HAS_GRAPHIC_ACCESS;
        if !remote {
            flags |= AU_SESSION_FLAG_HAS_CONSOLE_ACCESS;
        }
    }
    if remote {
        flags |= AU_SESSION_FLAG_IS_REMOTE;
    }
    if has_tty() {
        flags |= AU_SESSION_FLAG_HAS_TTY;
    }
    AuditinfoAddr {
        // Without host audit (containers, audit off) the guest is still a
        // user's app, and Darwin would have given it that user's login.
        // SAFETY: `getuid` takes nothing and cannot fail.
        auid: proc_id("/proc/self/loginuid").unwrap_or_else(|| unsafe { libc::getuid() }),
        kind: AU_IPV4,
        // Callers compare the id to tell sessions apart, and 0 is Darwin's
        // "no session", so the POSIX session stands in when audit has none.
        // SAFETY: `getsid(0)` asks about this process only.
        asid: proc_id("/proc/self/sessionid")
            .map(|id| id as i32)
            .unwrap_or_else(|| unsafe { libc::getsid(0) }),
        flags,
        ..Default::default()
    }
}

/// `int getaudit_addr(struct auditinfo_addr *, u_int length)`
///
/// # Safety
///
/// `out` is writable for `length` bytes.
pub unsafe extern "C" fn getaudit_addr(out: *mut AuditinfoAddr, length: u32) -> c_int {
    // XNU refuses a short buffer outright rather than truncating.
    if (length as usize) < size_of::<AuditinfoAddr>() {
        crate::errno::set(libc::EOVERFLOW);
        return -1;
    }
    if out.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: the caller's obligation; the guest's buffer need not be aligned.
    unsafe { out.write_unaligned(session()) };
    0
}

exports::exports! {
    functions {
        "getaudit_addr" => getaudit_addr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getaudit_addr_describes_an_authenticated_session() {
        let mut info = AuditinfoAddr::default();
        // SAFETY: a structure of this test's own, of exactly the size given.
        let rc = unsafe { getaudit_addr(&mut info, size_of::<AuditinfoAddr>() as u32) };
        assert_eq!(rc, 0);
        assert_ne!(info.auid, UNSET);
        assert_ne!(info.asid, 0);
        assert_eq!(info.kind, AU_IPV4);
        assert_ne!(info.flags & AU_SESSION_FLAG_HAS_AUTHENTICATED, 0);
    }

    #[test]
    fn getaudit_addr_refuses_a_short_buffer() {
        let mut info = AuditinfoAddr::default();
        // SAFETY: as above, but with a length one byte short.
        let rc = unsafe { getaudit_addr(&mut info, size_of::<AuditinfoAddr>() as u32 - 1) };
        assert_eq!(rc, -1);
        assert_eq!(crate::errno::get(), libc::EOVERFLOW);
        assert_eq!(info.auid, 0, "nothing was written");
    }
}
