//! The BSM audit session a guest asks about.
//!
//! Darwin puts every process in an audit session, and Security's
//! `SessionGetInfo` reads its flags to decide whether the caller may put up UI.
//! Linux's audit subsystem has the same two ids - the login uid and the session
//! id - so those are answered where the host has them, and the flags are
//! derived from what this process can actually reach.

use std::ffi::c_int;

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

/// Whether this process has a controlling terminal.
fn has_tty() -> bool {
    starnix::process::process(crate::process::host_self()).is_some_and(|me| me.has_tty())
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
    let audit = starnix::process::audit();
    AuditinfoAddr {
        // Without host audit (containers, audit off) the guest is still a
        // user's app, and Darwin would have given it that user's login.
        // SAFETY: `getuid` takes nothing and cannot fail.
        auid: audit.login_uid.unwrap_or_else(|| unsafe { libc::getuid() }),
        kind: AU_IPV4,
        // Callers compare the id to tell sessions apart, and 0 is Darwin's
        // "no session", so the POSIX session stands in when audit has none.
        // SAFETY: `getsid(0)` asks about this process only.
        asid: audit
            .session
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

/// `int getauid(au_id_t *)`: the login user the session was audited as.
///
/// # Safety
///
/// `out` is writable.
pub unsafe extern "C" fn getauid(out: *mut u32) -> c_int {
    if out.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: the caller's obligation.
    unsafe { out.write_unaligned(session().auid) };
    0
}

/// `int setaudit_addr(const struct auditinfo_addr *, u_int length)`
///
/// What `su` and `login` call to put the new user's session on record. Root's
/// to do on a Mac, and only root's here - by the guest's ids, see
/// `crate::credentials`.
///
/// FIXME (ACCURACY): nothing records the new session, so a later
/// `getaudit_addr` still describes the one the host gave this process. No host
/// audit trail would hold it, and the guest's only reader is itself.
///
/// # Safety
///
/// `info` is readable for `length` bytes.
pub unsafe extern "C" fn setaudit_addr(info: *const AuditinfoAddr, length: u32) -> c_int {
    if (length as usize) < size_of::<AuditinfoAddr>() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if info.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    if !crate::credentials::current().is_root() {
        crate::errno::set(libc::EPERM);
        return -1;
    }
    0
}

/// `int audit_submit(short au_event, au_id_t auid, char status, int reterr,
/// const char *fmt, ...)`: a BSM record of an event like a `su`.
///
/// Accepted and dropped: this host keeps no BSM trail for a guest to write to,
/// and the callers treat the record as a side effect, not a step that must
/// succeed.
pub extern "C" fn audit_submit(_event: i16, _auid: u32, _status: i8, _error: c_int) -> c_int {
    0
}

exports::exports! {
    functions {
        "getaudit_addr" => getaudit_addr,
        "getauid" => getauid,
        "setaudit_addr" => setaudit_addr,
        "audit_submit" => audit_submit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Darwin's `AU_DEFAUDITID`: no login uid.
    const AU_DEFAUDITID: u32 = u32::MAX;

    #[test]
    fn getaudit_addr_describes_an_authenticated_session() {
        let mut info = AuditinfoAddr::default();
        // SAFETY: a structure of this test's own, of exactly the size given.
        let rc = unsafe { getaudit_addr(&mut info, size_of::<AuditinfoAddr>() as u32) };
        assert_eq!(rc, 0);
        assert_ne!(info.auid, AU_DEFAUDITID);
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
