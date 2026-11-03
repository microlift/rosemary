use std::ffi::{CStr, c_char, c_int};
use std::path::Path;
use std::sync::{LazyLock, RwLock};

use server::credentials::{Change, Credentials, Denied, MAX_GROUPS, SetId};

static CURRENT: LazyLock<RwLock<Credentials>> = LazyLock::new(|| RwLock::new(of_host_user()));

/// The user who ran the guest, nothing changed: the host's ids, with the
/// groups the guest's account database gives them.
fn of_host_user() -> Credentials {
    // SAFETY: none of these takes an argument or can fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let groups = match crate::accounts::me().and_then(|name| std::ffi::CString::new(name).ok()) {
        Some(name) => crate::accounts::groups_of(&name, gid),
        None => vec![gid],
    };
    let mut groups = groups;
    groups.truncate(MAX_GROUPS);
    Credentials::of(uid, gid, groups)
}

/// This process's ids.
pub fn current() -> Credentials {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn replace(credentials: Credentials) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = credentials;
}

/// What `exec`ing the host file `image` asks for, by the prefix's view of
/// who owns it.
pub fn setid_of(image: &Path) -> SetId {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(image) else {
        return SetId::default();
    };
    let Ok(path) = std::ffi::CString::new(image.as_os_str().as_bytes()) else {
        return SetId::default();
    };
    let found = prefix::ownership::Found::Path(&path, true);
    let owner =
        prefix::ownership::of(found, meta.dev(), meta.ino()).unwrap_or(prefix::ownership::Owner {
            uid: meta.uid(),
            gid: meta.gid(),
            mode: None,
        });
    let (uid, gid) = owner.setid(meta.mode() & 0o7777);
    SetId { uid, gid }
}

/// Who a new image is: what the server says it inherits, or - without one -
/// the host's user, with `setid` applied either way.
///
/// `checked_in` is the server's answer to this image's checkin, if it gave
/// one.
pub fn start(checked_in: Option<Credentials>, setid: SetId) -> Credentials {
    let credentials = checked_in.unwrap_or_else(|| of_host_user().exec(setid));
    replace(credentials.clone());
    credentials
}

/// The credentials a fresh image offers the server, before it knows better.
pub fn offered() -> Credentials {
    // Nothing has changed them yet: this is the host's user, computed once.
    current()
}

/// Apply `change`, through the server when there is one.
fn apply(change: Change) -> c_int {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    let result = match server::client::change_credentials(change.clone()) {
        Some(result) => result,
        None => {
            let result = current().changed(&change);
            if let Ok(credentials) = &result {
                server::client::remember(credentials.clone());
            }
            result
        }
    };
    match result {
        Ok(credentials) => {
            replace(credentials);
            0
        }
        Err(denied) => {
            crate::errno::set(match denied {
                Denied::Permission => libc::EPERM,
                Denied::Invalid => libc::EINVAL,
            });
            -1
        }
    }
}

/// Whether a file this process creates belongs to somebody the host would not
/// name: it is acting as another user, or with another group.
pub fn creates_as_another() -> bool {
    let current = CURRENT.read().unwrap_or_else(|e| e.into_inner());
    // SAFETY: neither takes an argument or can fail.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    current.euid != uid || current.egid != gid
}

/// Record the file just created at `path`, relative to `dirfd`, as Darwin
/// would own it: by this process's effective user, and by the group of the
/// directory it was made in. Only worth calling when [`creates_as_another`].
///
/// A record that cannot be kept - a filesystem without user attributes, a
/// symlink on Linux - leaves the file to the host's owner.
pub fn created(dirfd: c_int, path: &CStr) {
    use prefix::ownership::Found;
    let bytes = path.to_bytes();
    let parent = match bytes.iter().rposition(|b| *b == b'/') {
        Some(0) => b"/".to_vec(),
        Some(at) => bytes[..at].to_vec(),
        None => b".".to_vec(),
    };
    let Ok(parent) = std::ffi::CString::new(parent) else {
        return;
    };
    let saved = crate::errno::get();
    let stat_at = |path: &CStr, flags: c_int| {
        // SAFETY: a zeroed `struct stat` for the host to fill.
        let mut host: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a NUL-terminated path, and the caller's descriptor.
        (unsafe { libc::fstatat(dirfd, path.as_ptr(), &mut host, flags) } == 0).then_some(host)
    };
    let euid = current().euid;
    let group = stat_at(&parent, 0)
        .map(|host| crate::stat::as_seen(&host, Found::At(dirfd, &parent, true)).gid);
    if let (Some(file), Some(gid)) = (stat_at(path, libc::AT_SYMLINK_NOFOLLOW), group) {
        let found = Found::At(dirfd, path, false);
        let _ = prefix::ownership::record(found, file.st_dev as u64, Some((euid, gid)));
    }
    crate::errno::set(saved);
}

/// `uid_t getuid(void)`
pub extern "C" fn getuid() -> u32 {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).ruid
}

/// `uid_t geteuid(void)`
pub extern "C" fn geteuid() -> u32 {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).euid
}

/// `gid_t getgid(void)`
pub extern "C" fn getgid() -> u32 {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).rgid
}

/// `gid_t getegid(void)`
pub extern "C" fn getegid() -> u32 {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).egid
}

/// `int setuid(uid_t)`
pub extern "C" fn setuid(uid: u32) -> c_int {
    apply(Change::SetUid(uid))
}

/// `int seteuid(uid_t)`
pub extern "C" fn seteuid(uid: u32) -> c_int {
    apply(Change::SetEuid(uid))
}

/// `int setreuid(uid_t, uid_t)`
pub extern "C" fn setreuid(ruid: u32, euid: u32) -> c_int {
    apply(Change::SetReuid(ruid, euid))
}

/// `int setgid(gid_t)`
pub extern "C" fn setgid(gid: u32) -> c_int {
    apply(Change::SetGid(gid))
}

/// `int setegid(gid_t)`
pub extern "C" fn setegid(gid: u32) -> c_int {
    apply(Change::SetEgid(gid))
}

/// `int setregid(gid_t, gid_t)`
pub extern "C" fn setregid(rgid: u32, egid: u32) -> c_int {
    apply(Change::SetRegid(rgid, egid))
}

/// `int getgroups(int gidsetsize, gid_t grouplist[])`
///
/// # Safety
///
/// `list` is writable for `size` entries when `size` is not zero.
pub unsafe extern "C" fn getgroups(size: c_int, list: *mut u32) -> c_int {
    let groups = current().groups;
    if size == 0 {
        return groups.len() as c_int;
    }
    if size < 0 || (size as usize) < groups.len() || list.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: the caller's array, which holds at least `groups.len()`.
    unsafe { std::ptr::copy_nonoverlapping(groups.as_ptr(), list, groups.len()) };
    groups.len() as c_int
}

/// `int setgroups(int ngroups, const gid_t *gidset)`
///
/// # Safety
///
/// `list` is readable for `count` entries.
pub unsafe extern "C" fn setgroups(count: c_int, list: *const u32) -> c_int {
    if count < 0 || (count > 0 && list.is_null()) {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let groups = match count {
        0 => Vec::new(),
        // SAFETY: the caller's array of `count`.
        _ => unsafe { std::slice::from_raw_parts(list, count as usize) }.to_vec(),
    };
    apply(Change::SetGroups(groups))
}

/// `int initgroups(const char *name, int basegid)`
///
/// # Safety
///
/// `name` is a NUL-terminated string.
pub unsafe extern "C" fn initgroups(name: *const c_char, basegid: c_int) -> c_int {
    if name.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: the caller's string.
    let mut groups = crate::accounts::groups_of(unsafe { CStr::from_ptr(name) }, basegid as u32);
    groups.truncate(MAX_GROUPS);
    apply(Change::SetGroups(groups))
}

/// `int issetugid(void)`: whether this image was started set-id, or has
/// changed its ids since.
pub extern "C" fn issetugid() -> c_int {
    c_int::from(CURRENT.read().unwrap_or_else(|e| e.into_inner()).tainted)
}

exports::exports! {
    functions {
        "getuid" => getuid,
        "geteuid" => geteuid,
        "getgid" => getgid,
        "getegid" => getegid,
        "setuid" => setuid,
        "seteuid" => seteuid,
        "setreuid" => setreuid,
        "setgid" => setgid,
        "setegid" => setegid,
        "setregid" => setregid,
        "getgroups" => getgroups,
        "setgroups" => setgroups,
        "initgroups" => initgroups,
        "issetugid" => issetugid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because the ids are the process's own and the test harness
    /// runs tests on threads that share them.
    #[test]
    fn a_set_user_id_start_can_become_root_and_give_it_back() {
        // SAFETY: no arguments.
        let host = unsafe { libc::getuid() };
        assert_eq!(getuid(), host, "the host's user to begin with");
        assert_eq!(issetugid(), 0);
        assert_eq!(setuid(0), -1, "an ordinary user may not");
        assert_eq!(crate::errno::get(), libc::EPERM);

        start(
            None,
            SetId {
                uid: Some(0),
                gid: None,
            },
        );
        assert_eq!((getuid(), geteuid()), (host, 0));
        assert_eq!(issetugid(), 1);

        assert_eq!(seteuid(host), 0, "a set-id program may drop privilege");
        assert_eq!(geteuid(), host);
        assert_eq!(seteuid(0), 0, "and take it back, from the saved id");
        assert_eq!(setuid(0), 0);
        assert_eq!((getuid(), geteuid()), (0, 0));

        let groups = [0u32, 80];
        // SAFETY: two entries, as said.
        assert_eq!(unsafe { setgroups(2, groups.as_ptr()) }, 0);
        let mut out = [0u32; 16];
        // SAFETY: room for sixteen.
        assert_eq!(unsafe { getgroups(16, out.as_mut_ptr()) }, 2);
        assert_eq!(&out[..2], &groups);
        // SAFETY: a size query writes nothing.
        assert_eq!(unsafe { getgroups(0, std::ptr::null_mut()) }, 2);

        assert_eq!(setuid(host), 0);
        assert_eq!((getuid(), geteuid()), (host, host));
        assert_eq!(setuid(0), -1, "given away for good");
        replace(of_host_user());
    }
}
