use std::ffi::{CStr, CString};

/// One user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub gecos: String,
    /// The guest's spelling.
    pub home: String,
    pub shell: String,
}

/// One group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub gid: u32,
    pub members: Vec<String>,
}

pub const ROOT: u32 = 0;
pub const WHEEL: u32 = 0;
pub const ADMIN: u32 = 80;

/// The user who ran the guest, by the host's name for them. Looked up once:
/// it does not change, and the reentrant lookup leaves alone the host's static
/// entry, which a guest may be holding.
pub fn me() -> Option<String> {
    static ME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ME.get_or_init(|| {
        let mut buffer = vec![0 as libc::c_char; 4096];
        loop {
            let mut host: libc::passwd = unsafe { std::mem::zeroed() };
            let mut result: *mut libc::passwd = std::ptr::null_mut();
            let rc = unsafe {
                libc::getpwuid_r(
                    libc::getuid(),
                    &mut host,
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    &mut result,
                )
            };
            match rc {
                libc::ERANGE if buffer.len() < 1 << 20 => buffer.resize(buffer.len() * 4, 0),
                0 if !result.is_null() => {
                    return Some(
                        unsafe { CStr::from_ptr(host.pw_name) }
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
                _ => return None,
            }
        }
    })
    .clone()
}

/// The prefix's own users: its local node's records first, then the built-in
/// ones they do not replace.
fn system_users() -> Vec<User> {
    let mut users: Vec<User> = crate::dslocal::records(crate::dslocal::Kind::Users)
        .iter()
        .filter_map(local_user)
        .collect();
    for user in builtin_users() {
        if !users
            .iter()
            .any(|u| u.name == user.name || u.uid == user.uid)
        {
            users.push(user);
        }
    }
    users
}

/// A local record as a user; one without a name or ids is not one.
fn local_user(record: &crate::dslocal::Record) -> Option<User> {
    Some(User {
        name: record.first("name")?.to_owned(),
        uid: record.first("uid")?.parse().ok()?,
        gid: record.first("gid")?.parse().ok()?,
        gecos: record.first("realname").unwrap_or_default().to_owned(),
        home: record.first("home").unwrap_or_default().to_owned(),
        shell: record.first("shell").unwrap_or_default().to_owned(),
    })
}

fn local_group(record: &crate::dslocal::Record) -> Option<Group> {
    Some(Group {
        name: record.first("name")?.to_owned(),
        gid: record.first("gid")?.parse().ok()?,
        members: record.values("users").to_vec(),
    })
}

fn builtin_users() -> Vec<User> {
    vec![User {
        name: "root".into(),
        uid: ROOT,
        gid: WHEEL,
        gecos: "System Administrator".into(),
        home: "/var/root".into(),
        shell: "/bin/sh".into(),
    }]
}

/// The prefix's own groups, as [`system_users`] are its users.
fn system_groups() -> Vec<Group> {
    let mut groups: Vec<Group> = crate::dslocal::records(crate::dslocal::Kind::Groups)
        .iter()
        .filter_map(local_group)
        .collect();
    for group in builtin_groups() {
        if !groups
            .iter()
            .any(|g| g.name == group.name || g.gid == group.gid)
        {
            groups.push(group);
        }
    }
    groups
}

fn builtin_groups() -> Vec<Group> {
    let mut admins = vec!["root".to_string()];
    admins.extend(me());
    vec![
        Group {
            name: "wheel".into(),
            gid: WHEEL,
            members: vec!["root".into()],
        },
        Group {
            name: "admin".into(),
            gid: ADMIN,
            members: admins,
        },
    ]
}

/// A system user with this id, which answers before the host's.
pub fn system_user_by_uid(uid: u32) -> Option<User> {
    system_users().into_iter().find(|user| user.uid == uid)
}

/// A system user with this name, which answers before the host's.
pub fn system_user_by_name(name: &[u8]) -> Option<User> {
    system_users()
        .into_iter()
        .find(|user| user.name.as_bytes() == name)
}

pub fn system_group_by_gid(gid: u32) -> Option<Group> {
    system_groups().into_iter().find(|group| group.gid == gid)
}

pub fn system_group_by_name(name: &[u8]) -> Option<Group> {
    system_groups()
        .into_iter()
        .find(|group| group.name.as_bytes() == name)
}

/// Whether a host user of this name is hidden by a system one.
pub fn shadows_user(name: &[u8]) -> bool {
    system_user_by_name(name).is_some()
}

/// Whether a host group of this name is hidden by a system one.
pub fn shadows_group(name: &[u8]) -> bool {
    system_group_by_name(name).is_some()
}

/// The groups `name` belongs to, `base` first and none twice: the host's
/// answer for a host user, less any group a system one hides, and then every
/// system group that lists them.
pub fn groups_of(name: &CStr, base: u32) -> Vec<u32> {
    let mut groups = vec![base];
    if !shadows_user(name.to_bytes()) {
        for gid in host_groups_of(name, base) {
            let hidden = host_group_name(gid).is_some_and(|group| shadows_group(group.as_bytes()));
            if !hidden && system_group_by_gid(gid).is_none() && !groups.contains(&gid) {
                groups.push(gid);
            }
        }
    }
    let name = name.to_string_lossy();
    for group in system_groups() {
        if group.members.iter().any(|member| *member == name) && !groups.contains(&group.gid) {
            groups.push(group.gid);
        }
    }
    groups
}

fn host_groups_of(name: &CStr, base: u32) -> Vec<u32> {
    let mut host: Vec<libc::gid_t> = Vec::new();
    let mut count: libc::c_int = 32;
    loop {
        host.resize(count as usize, 0);
        let offered = count;
        // SAFETY: `host` holds `count` slots, and libc writes no more.
        if unsafe {
            libc::getgrouplist(
                name.as_ptr(),
                base as _,
                host.as_mut_ptr().cast(),
                &mut count,
            )
        } >= 0
        {
            host.truncate(count as usize);
            return host;
        }
        // libc reports the size it needed; grow regardless, in case the
        // group database changed between the two calls.
        count = count.max(offered.saturating_mul(2));
    }
}

/// The host's name for `gid`, by the reentrant lookup: a guest may be holding
/// the static entry `getgrgid` answers with, which is the host's own.
fn host_group_name(gid: u32) -> Option<CString> {
    let mut buffer = vec![0 as libc::c_char; 4096];
    loop {
        // SAFETY: a zeroed `struct group` and a buffer, both this function's.
        let mut host: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        let rc = unsafe {
            libc::getgrgid_r(
                gid,
                &mut host,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        };
        match rc {
            libc::ERANGE if buffer.len() < 1 << 20 => buffer.resize(buffer.len() * 4, 0),
            // SAFETY: the name points into `buffer`, still alive.
            0 if !result.is_null() => {
                return Some(unsafe { CStr::from_ptr(host.gr_name) }.to_owned());
            }
            _ => return None,
        }
    }
}

pub use crate::env::guest_shell;

/// Whether `user` has to give a password to prove who they are.
///
/// Never yet: no account here has one, and an in-prefix account database is
/// what would change that.
pub fn password_required(_user: &CStr) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_darwins_and_not_the_hosts() {
        let root = system_user_by_name(b"root").unwrap();
        assert_eq!((root.uid, root.gid), (ROOT, WHEEL));
        assert_eq!(root.home, "/var/root");
        assert!(shadows_user(b"root"));
    }

    #[test]
    fn the_user_who_ran_the_guest_is_an_administrator() {
        let me = CString::new(me().unwrap()).unwrap();
        // SAFETY: a scalar.
        let gid = unsafe { libc::getgid() };
        let groups = groups_of(&me, gid);
        assert_eq!(groups[0], gid);
        assert!(groups.contains(&ADMIN));
        if me != c"root" && gid != WHEEL {
            assert!(!groups.contains(&WHEEL), "only root is in wheel");
        }
    }

    #[test]
    fn roots_groups_are_wheel_and_admin() {
        let groups = groups_of(c"root", WHEEL);
        assert_eq!(groups, vec![WHEEL, ADMIN]);
    }
}
