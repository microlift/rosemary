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
    starnix::process::process(starnix::process::me()).is_some_and(|me| me.has_tty())
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

// libbsm's readers of an `audit_token_t`, from OpenBSM's `bsm_wrappers.c`
// (`audit_token_to_au32`) and the SDK's `<bsm/libbsm.h>`: the token is taken by
// value, and each field is the word `set_security_token_task_internal` put
// there - see [`crate::mach::info::audit_token`].

use crate::mach::info::AuditToken;

/// `au_tid_t`: the terminal's port and machine.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct AuTid {
    port: i32,
    machine: u32,
}

/// `uid_t audit_token_to_auid(audit_token_t)`
pub extern "C" fn audit_token_to_auid(token: AuditToken) -> u32 {
    token.val[0]
}

/// `uid_t audit_token_to_euid(audit_token_t)`
pub extern "C" fn audit_token_to_euid(token: AuditToken) -> u32 {
    token.val[1]
}

/// `gid_t audit_token_to_egid(audit_token_t)`
pub extern "C" fn audit_token_to_egid(token: AuditToken) -> u32 {
    token.val[2]
}

/// `uid_t audit_token_to_ruid(audit_token_t)`
pub extern "C" fn audit_token_to_ruid(token: AuditToken) -> u32 {
    token.val[3]
}

/// `gid_t audit_token_to_rgid(audit_token_t)`
pub extern "C" fn audit_token_to_rgid(token: AuditToken) -> u32 {
    token.val[4]
}

/// `pid_t audit_token_to_pid(audit_token_t)`
pub extern "C" fn audit_token_to_pid(token: AuditToken) -> i32 {
    token.val[5] as i32
}

/// `au_asid_t audit_token_to_asid(audit_token_t)`
pub extern "C" fn audit_token_to_asid(token: AuditToken) -> i32 {
    token.val[6] as i32
}

/// `int audit_token_to_pidversion(audit_token_t)`
pub extern "C" fn audit_token_to_pidversion(token: AuditToken) -> c_int {
    token.val[7] as c_int
}

/// `void audit_token_to_au32(audit_token_t, uid_t *auid, uid_t *euid,
/// gid_t *egid, uid_t *ruid, gid_t *rgid, pid_t *pid, au_asid_t *asid,
/// au_tid_t *tid)`: every field, each to a pointer that is not null. The
/// terminal's machine is `kern.hostid`, which nothing here sets, so it is 0 as
/// on a Mac that never called `sethostid`.
///
/// # Safety
///
/// Each pointer is null or writable.
#[allow(clippy::too_many_arguments, reason = "the C prototype")]
pub unsafe extern "C" fn audit_token_to_au32(
    token: AuditToken,
    auid: *mut u32,
    euid: *mut u32,
    egid: *mut u32,
    ruid: *mut u32,
    rgid: *mut u32,
    pid: *mut i32,
    asid: *mut i32,
    tid: *mut AuTid,
) {
    let v = token.val;
    // SAFETY: the caller's pointers, each null or writable.
    unsafe {
        for (out, value) in [
            (auid, v[0]),
            (euid, v[1]),
            (egid, v[2]),
            (ruid, v[3]),
            (rgid, v[4]),
        ] {
            if let Some(out) = out.as_mut() {
                *out = value;
            }
        }
        if let Some(out) = pid.as_mut() {
            *out = v[5] as i32;
        }
        if let Some(out) = asid.as_mut() {
            *out = v[6] as i32;
        }
        if let Some(out) = tid.as_mut() {
            *out = AuTid {
                port: v[7] as i32,
                machine: 0,
            };
        }
    }
}

exports::exports! {
    functions {
        "getaudit_addr" => getaudit_addr,
        "getauid" => getauid,
        "setaudit_addr" => setaudit_addr,
        "audit_submit" => audit_submit,
        "audit_token_to_auid" => audit_token_to_auid,
        "audit_token_to_euid" => audit_token_to_euid,
        "audit_token_to_egid" => audit_token_to_egid,
        "audit_token_to_ruid" => audit_token_to_ruid,
        "audit_token_to_rgid" => audit_token_to_rgid,
        "audit_token_to_pid" => audit_token_to_pid,
        "audit_token_to_asid" => audit_token_to_asid,
        "audit_token_to_pidversion" => audit_token_to_pidversion,
        "audit_token_to_au32" => audit_token_to_au32,
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

    /// Each reader names its own word of the token, in the order
    /// `audit_token_to_au32` gives them, and this process's token reads back
    /// as this process.
    #[test]
    fn a_token_reads_back_field_by_field() {
        let token = AuditToken {
            val: [10, 11, 12, 13, 14, 15, 16, 17],
        };
        assert_eq!(
            (
                audit_token_to_auid(token),
                audit_token_to_euid(token),
                audit_token_to_egid(token),
                audit_token_to_ruid(token),
                audit_token_to_rgid(token),
                audit_token_to_pid(token),
                audit_token_to_asid(token),
                audit_token_to_pidversion(token),
            ),
            (10, 11, 12, 13, 14, 15, 16, 17)
        );
        let (mut auid, mut euid, mut egid, mut ruid, rgid) = (0, 0, 0, 0, 0);
        let (mut pid, mut asid, mut tid) = (0, 0, AuTid::default());
        // SAFETY: locals of this test's, and one pointer left null.
        unsafe {
            audit_token_to_au32(
                token,
                &mut auid,
                &mut euid,
                &mut egid,
                &mut ruid,
                std::ptr::null_mut(),
                &mut pid,
                &mut asid,
                &mut tid,
            );
        }
        assert_eq!((auid, euid, egid, ruid, rgid), (10, 11, 12, 13, 0));
        assert_eq!((pid, asid), (15, 16));
        assert_eq!(
            tid,
            AuTid {
                port: 17,
                machine: 0
            }
        );

        let me = std::process::id() as i32;
        let own = crate::mach::info::audit_token(me);
        assert_eq!(audit_token_to_pid(own), me);
        // SAFETY: no arguments.
        assert_eq!(audit_token_to_euid(own), unsafe { libc::geteuid() });
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
