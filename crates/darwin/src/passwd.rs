use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_int};

/// Darwin's `struct passwd`, from `<pwd.h>`.
///
/// `#[repr(C)]` because the guest reads it by offset - see the module note on
/// `#[repr(C)]` in `darwin::dispatch`, which is the same rule for the same
/// reason.
#[repr(C)]
pub struct Passwd {
    name: *mut c_char,
    passwd: *mut c_char,
    uid: u32,
    gid: u32,
    /// BSD's password-change time, or zero on hosts that do not record it.
    change: i64,
    /// BSD's login class. Empty rather than null: a guest is entitled to print it.
    class: *mut c_char,
    gecos: *mut c_char,
    dir: *mut c_char,
    shell: *mut c_char,
    /// BSD's account-expiry time, or zero when unavailable.
    expire: i64,
}

/// One answer, with the strings it points at.
///
/// Per thread and overwritten by the next call, which is the lifetime both
/// platforms' `getpwuid` documents. The strings are *copied* rather than
/// borrowed from the host's own static storage, so a guest that calls something
/// else in libc between the lookup and the read still finds them intact.
#[derive(Default)]
struct Answer {
    entry: Option<Box<Entry>>,
}

struct Entry {
    guest: Passwd,
    _strings: Vec<CString>,
}

thread_local! {
    static ANSWER: RefCell<Answer> = RefCell::new(Answer::default());
}

/// A host entry's strings, in [`Passwd`]'s order: name, password, class,
/// gecos, home, shell.
///
/// # Safety
///
/// `host` is a live `struct passwd` this process's libc returned.
unsafe fn strings_of(host: &libc::passwd) -> [CString; 6] {
    // SAFETY: each is a NUL-terminated string libc owns, or null.
    let text = |p: *const c_char| unsafe {
        match p.is_null() {
            true => CString::default(),
            false => CString::from(CStr::from_ptr(p)),
        }
    };
    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    let class = text(host.pw_class);
    #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
    let class = CString::default();
    [
        text(host.pw_name),
        text(host.pw_passwd),
        class,
        text(host.pw_gecos),
        home_of(host),
        CString::new(crate::accounts::guest_shell(text(host.pw_shell).as_bytes()))
            .unwrap_or_default(),
    ]
}

/// A [`Passwd`] over `host`'s ids and six strings at `at(i)`.
fn entry(host: &libc::passwd, at: impl Fn(usize) -> *mut c_char) -> Passwd {
    Passwd {
        name: at(0),
        passwd: at(1),
        uid: host.pw_uid,
        gid: host.pw_gid,
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        change: host.pw_change,
        #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
        change: 0,
        class: at(2),
        gecos: at(3),
        dir: at(4),
        shell: at(5),
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        expire: host.pw_expire,
        #[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
        expire: 0,
    }
}

/// A host lookup's answer as Darwin's, in this thread's storage.
///
/// The host's reentrant calls, because its `getpwuid` and `getpwnam` share
/// one static entry between threads on FreeBSD, where Darwin's are per thread.
fn published(answer: Result<Option<([CString; 6], u32, u32)>, c_int>) -> *mut Passwd {
    match answer {
        Ok(Some((strings, uid, gid))) => publish_strings(strings, uid, gid),
        Ok(None) => std::ptr::null_mut(),
        Err(error) => {
            crate::errno::set(error);
            std::ptr::null_mut()
        }
    }
}

/// A system account's strings, in [`Passwd`]'s order.
fn system_strings(user: &crate::accounts::User) -> [CString; 6] {
    let text = |s: &str| CString::new(s).unwrap_or_default();
    [
        text(&user.name),
        // Darwin's own placeholder: the password is not in this database.
        text("********"),
        CString::default(),
        text(&user.gecos),
        text(&user.home),
        text(&user.shell),
    ]
}

/// An entry made of `strings`, in this thread's storage.
fn publish_strings(strings: [CString; 6], uid: u32, gid: u32) -> *mut Passwd {
    // SAFETY: a zeroed host entry carries only the ids `entry` reads.
    let mut ids: libc::passwd = unsafe { std::mem::zeroed() };
    ids.pw_uid = uid;
    ids.pw_gid = gid;
    let guest = entry(&ids, |i| strings[i].as_ptr().cast_mut());
    let answer = Box::new(Entry {
        guest,
        _strings: strings.into(),
    });
    let mut answer = Some(answer);
    let published = ANSWER.try_with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.entry = answer.take();
        slot.entry
            .as_mut()
            .map_or(std::ptr::null_mut(), |e| &raw mut e.guest)
    });
    // A guest `atexit` handler runs after glibc has destroyed the main thread's
    // storage; the process is ending, so the entry is leaked instead.
    published.unwrap_or_else(|_| {
        let leaked = Box::leak(answer.take().expect("not consumed by try_with"));
        &raw mut leaked.guest
    })
}

/// The home directory to report for `host`.
///
/// The guest's own spelling for the calling user, and the reason is not
/// politeness: this runtime creates the `~/Library` subdirectories a Mac
/// program assumes exist (`foundation::paths`), and it creates them under the
/// guest's home. A guest told a *different* home here would build its `Library`
/// somewhere the runtime never prepared, and every path the two of them built
/// would disagree - which is exactly what `$HOME` used to do, putting the
/// program's data in `<prefix>/root/home/user/Library` beside the `/Users/user/Library`
/// nothing would ever look in again.
///
/// Another user's home is a host path, so it is given the guest spelling that
/// names it; one no mount reaches is reported as empty rather than as a path
/// the guest cannot use.
fn home_of(host: &libc::passwd) -> CString {
    if host.pw_uid == unsafe { libc::getuid() }
        && let Ok(home) = CString::new(prefix::ns::home())
    {
        return home;
    }
    // SAFETY: libc's own NUL-terminated string, or null.
    let theirs = unsafe {
        match host.pw_dir.is_null() {
            true => return CString::default(),
            false => CStr::from_ptr(host.pw_dir),
        }
    };
    let Some(spelled) = theirs
        .to_str()
        .ok()
        .and_then(|path| prefix::ns::to_guest(std::path::Path::new(path)))
    else {
        return CString::default();
    };
    CString::new(spelled.into_os_string().into_encoded_bytes()).unwrap_or_default()
}

/// `struct passwd *getpwuid(uid_t)`
///
/// # Safety
///
/// Called only through an import slot this crate defines.
pub unsafe extern "C" fn getpwuid(uid: u32) -> *mut Passwd {
    // A null return means "no such user" *or* an error, and the two are told
    // apart by `errno` - which the caller only trusts if it was cleared first.
    crate::errno::set(0);
    if let Some(user) = crate::accounts::system_user_by_uid(uid) {
        return publish_strings(system_strings(&user), user.uid, user.gid);
    }
    // SAFETY: the host's reentrant lookup into buffers `lookup_r` owns.
    published(lookup_r(|h, b, n, r| unsafe {
        libc::getpwuid_r(uid, h, b, n, r)
    }))
}

/// `struct passwd *getpwnam(const char *)`
///
/// # Safety
///
/// `name` is a NUL-terminated string.
pub unsafe extern "C" fn getpwnam(name: *const c_char) -> *mut Passwd {
    if name.is_null() {
        crate::errno::set(libc::EINVAL);
        return std::ptr::null_mut();
    }
    crate::errno::set(0);
    // SAFETY: the caller's NUL-terminated string.
    if let Some(user) =
        crate::accounts::system_user_by_name(unsafe { CStr::from_ptr(name) }.to_bytes())
    {
        return publish_strings(system_strings(&user), user.uid, user.gid);
    }
    // SAFETY: the caller's string, and buffers `lookup_r` owns.
    published(lookup_r(|h, b, n, r| unsafe {
        libc::getpwnam_r(name, h, b, n, r)
    }))
}

/// The directory `~` or `~name` stands for, or `None` for a user this host has
/// never heard of.
///
/// Here rather than in [`crate::glob`], which is what asks, because the `$HOME`
/// rule [`home_of`] applies is the one a guest must see from every direction:
/// a tilde that expanded to a different directory than `getpwuid` reports would
/// have the guest building two sets of paths.
pub fn home(user: Option<&CStr>) -> Option<CString> {
    crate::errno::set(0);
    if let Some(user) = user.and_then(|name| crate::accounts::system_user_by_name(name.to_bytes()))
    {
        return CString::new(user.home).ok();
    }
    // SAFETY: the caller's NUL-terminated name, and buffers `lookup_r` owns.
    let host = lookup_r(|h, b, n, r| unsafe {
        match user {
            Some(name) => libc::getpwnam_r(name.as_ptr(), h, b, n, r),
            None => libc::getpwuid_r(libc::getuid(), h, b, n, r),
        }
    });
    let Ok(Some(([_, _, _, _, home, _], _, _))) = host else {
        // A container often has no passwd entry for the uid it runs as, and
        // `$HOME` is still the answer for the calling user.
        return match user {
            Some(_) => None,
            // A container often has no passwd entry for the uid it runs as,
            // and the guest's own home is still the answer for the caller.
            None => CString::new(prefix::ns::home()).ok(),
        };
    };
    (!home.as_bytes().is_empty()).then_some(home)
}

/// The host's reentrant lookup, into a buffer of its own that grows until
/// the entry fits.
///
/// `Ok(None)` is "no such user", which the reentrant calls report as success
/// with a null result.
fn lookup_r(
    ask: impl Fn(*mut libc::passwd, *mut c_char, usize, *mut *mut libc::passwd) -> c_int,
) -> Result<Option<([CString; 6], u32, u32)>, c_int> {
    let mut size = 1024;
    loop {
        let mut buf = vec![0 as c_char; size];
        // SAFETY: a zeroed `struct passwd` is what the host fills.
        let mut host: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        match ask(&mut host, buf.as_mut_ptr(), buf.len(), &mut result) {
            libc::ERANGE if size < 1 << 20 => size *= 4,
            0 if result.is_null() => return Ok(None),
            // SAFETY: the host filled `host`, whose strings point into `buf`,
            // which is still alive.
            0 => {
                let strings = unsafe { strings_of(&host) };
                if crate::accounts::shadows_user(strings[0].as_bytes()) {
                    return Ok(None);
                }
                return Ok(Some((strings, host.pw_uid, host.pw_gid)));
            }
            error => return Err(error),
        }
    }
}

/// Pack an answer into the caller's `pwd` and `buf`, Darwin's way.
///
/// # Safety
///
/// `pwd` is a writable Darwin `struct passwd`, `buf` is writable for `size`
/// bytes, and `result` is writable.
unsafe fn fill_r(
    answer: Result<Option<([CString; 6], u32, u32)>, c_int>,
    pwd: *mut Passwd,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Passwd,
) -> c_int {
    if result.is_null() || pwd.is_null() || buf.is_null() {
        return libc::EINVAL;
    }
    unsafe { *result = std::ptr::null_mut() };
    let (strings, uid, gid) = match answer {
        Ok(Some(strings)) => strings,
        Ok(None) => return 0,
        Err(error) => return crate::errno::from_host(error),
    };
    let needed: usize = strings.iter().map(|s| s.as_bytes_with_nul().len()).sum();
    if needed > size {
        return libc::ERANGE;
    }
    let mut offsets = [0usize; 6];
    let mut at = 0;
    for (i, text) in strings.iter().enumerate() {
        let bytes = text.as_bytes_with_nul();
        // SAFETY: `needed <= size`, so every copy lands inside `buf`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr().cast(), buf.add(at), bytes.len()) };
        offsets[i] = at;
        at += bytes.len();
    }
    // SAFETY: a zeroed host entry carries only the ids `entry` reads.
    let mut ids: libc::passwd = unsafe { std::mem::zeroed() };
    ids.pw_uid = uid;
    ids.pw_gid = gid;
    // SAFETY: the caller's obligation, and offsets inside `buf`.
    unsafe {
        *pwd = entry(&ids, |i| buf.add(offsets[i]));
        *result = pwd;
    }
    0
}

/// `int getpwuid_r(uid_t, struct passwd *, char *, size_t, struct passwd **)`
///
/// The caller's structure is filled in Darwin's layout and its buffer holds
/// the strings, which is the whole reason this is not the host's own.
///
/// # Safety
///
/// As [`fill_r`].
pub unsafe extern "C" fn getpwuid_r(
    uid: u32,
    pwd: *mut Passwd,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Passwd,
) -> c_int {
    let answer = match crate::accounts::system_user_by_uid(uid) {
        Some(user) => Ok(Some((system_strings(&user), user.uid, user.gid))),
        // SAFETY: the host's reentrant lookup into buffers `lookup_r` owns.
        None => lookup_r(|h, b, n, r| unsafe { libc::getpwuid_r(uid, h, b, n, r) }),
    };
    unsafe { fill_r(answer, pwd, buf, size, result) }
}

/// `int getpwnam_r(const char *, struct passwd *, char *, size_t, struct passwd **)`
///
/// # Safety
///
/// `name` is NUL-terminated, and as [`fill_r`].
pub unsafe extern "C" fn getpwnam_r(
    name: *const c_char,
    pwd: *mut Passwd,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Passwd,
) -> c_int {
    if name.is_null() {
        return libc::EINVAL;
    }
    // SAFETY: the caller's NUL-terminated string.
    let answer =
        match crate::accounts::system_user_by_name(unsafe { CStr::from_ptr(name) }.to_bytes()) {
            Some(user) => Ok(Some((system_strings(&user), user.uid, user.gid))),
            // SAFETY: the host's reentrant lookup into buffers `lookup_r` owns.
            None => lookup_r(|h, b, n, r| unsafe { libc::getpwnam_r(name, h, b, n, r) }),
        };
    unsafe { fill_r(answer, pwd, buf, size, result) }
}

/// Names by id, kept for the life of the process: `user_from_uid` hands out a
/// pointer the caller never frees and may hold, as BSD's cache does.
static NAMES: std::sync::Mutex<Option<std::collections::HashMap<(bool, u32), &'static CStr>>> =
    std::sync::Mutex::new(None);

/// The cached name for a user (`group == false`) or group id.
fn cached_name(
    group: bool,
    id: u32,
    lookup: impl FnOnce() -> Option<CString>,
) -> Option<&'static CStr> {
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let names = names.get_or_insert_with(Default::default);
    if let Some(name) = names.get(&(group, id)) {
        return Some(name);
    }
    let name: &'static CStr = Box::leak(lookup()?.into_boxed_c_str());
    names.insert((group, id), name);
    Some(name)
}

/// What BSD's pwcache answers for an id nothing names: null when asked to, or
/// the number itself.
fn unnamed(id: u32, nothing: c_int) -> *const c_char {
    match nothing {
        0 => cached_number(id),
        _ => std::ptr::null(),
    }
}

fn cached_number(id: u32) -> *const c_char {
    static NUMBERS: std::sync::Mutex<Option<std::collections::HashMap<u32, &'static CStr>>> =
        std::sync::Mutex::new(None);
    let mut numbers = NUMBERS.lock().unwrap_or_else(|e| e.into_inner());
    let numbers = numbers.get_or_insert_with(Default::default);
    numbers
        .entry(id)
        .or_insert_with(|| {
            Box::leak(
                CString::new(id.to_string())
                    .unwrap_or_default()
                    .into_boxed_c_str(),
            )
        })
        .as_ptr()
}

/// `const char *user_from_uid(uid_t uid, int nouser)`
pub extern "C" fn user_from_uid(uid: u32, nouser: c_int) -> *const c_char {
    let name = cached_name(false, uid, || {
        if let Some(user) = crate::accounts::system_user_by_uid(uid) {
            return CString::new(user.name).ok();
        }
        // SAFETY: the host's reentrant lookup into buffers `lookup_r` owns.
        let host = lookup_r(|h, b, n, r| unsafe { libc::getpwuid_r(uid, h, b, n, r) });
        host.ok().flatten().map(|([name, ..], _, _)| name)
    });
    match name {
        Some(name) => name.as_ptr(),
        None => unnamed(uid, nouser),
    }
}

/// `const char *group_from_gid(gid_t gid, int nogroup)`
///
/// `struct group` agrees between the two platforms, but only the name crosses
/// here anyway.
pub extern "C" fn group_from_gid(gid: u32, nogroup: c_int) -> *const c_char {
    let name = cached_name(true, gid, || {
        if let Some(group) = crate::accounts::system_group_by_gid(gid) {
            return CString::new(group.name).ok();
        }
        // SAFETY: a scalar, answered with libc's own entry or null.
        let host = unsafe { libc::getgrgid(gid) };
        // SAFETY: libc's own NUL-terminated name.
        let name =
            (!host.is_null()).then(|| unsafe { CString::from(CStr::from_ptr((*host).gr_name)) })?;
        (!crate::accounts::shadows_group(name.as_bytes())).then_some(name)
    });
    match name {
        Some(name) => name.as_ptr(),
        None => unnamed(gid, nogroup),
    }
}

/// `int32_t getgrouplist_2(const char *name, gid_t basegid, gid_t **groups)`
///
/// Libinfo's private form of `getgrouplist`, which allocates the list instead
/// of filling the caller's. The caller frees it, so it comes from the guest's
/// allocator. `basegid` leads and no gid repeats, as Libinfo's merge leaves it.
///
/// # Safety
///
/// `name` is NUL-terminated and `groups` is writable.
pub unsafe extern "C" fn getgrouplist_2(
    name: *const c_char,
    basegid: u32,
    groups: *mut *mut u32,
) -> i32 {
    if name.is_null() || groups.is_null() {
        return -1;
    }
    // SAFETY: the caller's NUL-terminated name.
    let merged = crate::accounts::groups_of(unsafe { CStr::from_ptr(name) }, basegid);
    // SAFETY: a block of exactly `merged`'s bytes, filled immediately.
    let list = unsafe { crate::heap::malloc(size_of_val(merged.as_slice())) }.cast::<u32>();
    if list.is_null() {
        crate::errno::set(libc::ENOMEM);
        return -1;
    }
    // SAFETY: the block just allocated, and the caller's slot.
    unsafe {
        std::ptr::copy_nonoverlapping(merged.as_ptr(), list, merged.len());
        *groups = list;
    }
    merged.len() as i32
}

/// The user and primary group ids of the account `name`, by the same
/// accounts `getpwnam` answers from - without touching the storage its answer
/// lives in, which the guest may still be holding a pointer into. For this
/// runtime's own callers: PAM asks while `su` holds `getpwnam`'s answer.
pub(crate) fn user_ids(name: &CStr) -> Option<(u32, u32)> {
    if let Some(user) = crate::accounts::system_user_by_name(name.to_bytes()) {
        return Some((user.uid, user.gid));
    }
    // SAFETY: the host's reentrant lookup into buffers `lookup_r` owns.
    match lookup_r(|h, b, n, r| unsafe { libc::getpwnam_r(name.as_ptr(), h, b, n, r) }) {
        Ok(Some((_, uid, gid))) => Some((uid, gid)),
        _ => None,
    }
}

/// The id of the group `name`, as [`user_ids`] answers for users.
pub(crate) fn group_id(name: &CStr) -> Option<u32> {
    if let Some(group) = crate::accounts::system_group_by_name(name.to_bytes()) {
        return Some(group.gid);
    }
    if crate::accounts::shadows_group(name.to_bytes()) {
        return None;
    }
    let mut buffer = vec![0 as c_char; 4096];
    loop {
        // SAFETY: a zeroed `struct group` and a buffer, both this function's.
        let mut host: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        let rc = unsafe {
            libc::getgrnam_r(
                name.as_ptr(),
                &mut host,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut result,
            )
        };
        match rc {
            libc::ERANGE if buffer.len() < 1 << 20 => buffer.resize(buffer.len() * 4, 0),
            0 if !result.is_null() => return Some(host.gr_gid),
            _ => return None,
        }
    }
}

/// `struct group`, which Darwin and this host lay out alike.
#[repr(C)]
pub struct GroupEntry {
    name: *mut c_char,
    passwd: *mut c_char,
    gid: u32,
    members: *mut *mut c_char,
}

const _: () = {
    assert!(size_of::<GroupEntry>() == size_of::<libc::group>());
    assert!(std::mem::offset_of!(GroupEntry, members) == std::mem::offset_of!(libc::group, gr_mem));
};

struct SystemGroupAnswer {
    entry: GroupEntry,
    _strings: Vec<CString>,
    _members: Vec<*mut c_char>,
}

thread_local! {
    static GROUP: RefCell<Option<Box<SystemGroupAnswer>>> = const { RefCell::new(None) };
}

/// A system group in this thread's storage, overwritten by the next call.
fn publish_group(group: &crate::accounts::Group) -> *mut GroupEntry {
    let mut strings = vec![
        CString::new(group.name.as_str()).unwrap_or_default(),
        CString::new("*").unwrap_or_default(),
    ];
    strings.extend(
        group
            .members
            .iter()
            .map(|member| CString::new(member.as_str()).unwrap_or_default()),
    );
    let mut members: Vec<*mut c_char> =
        strings[2..].iter().map(|s| s.as_ptr().cast_mut()).collect();
    members.push(std::ptr::null_mut());
    let mut answer = Box::new(SystemGroupAnswer {
        entry: GroupEntry {
            name: strings[0].as_ptr().cast_mut(),
            passwd: strings[1].as_ptr().cast_mut(),
            gid: group.gid,
            members: std::ptr::null_mut(),
        },
        _strings: strings,
        _members: members,
    });
    answer.entry.members = answer._members.as_mut_ptr();
    let mut answer = Some(answer);
    let published = GROUP.try_with(|slot| {
        let mut slot = slot.borrow_mut();
        *slot = answer.take();
        slot.as_mut()
            .map_or(std::ptr::null_mut(), |a| &raw mut a.entry)
    });
    published.unwrap_or_else(|_| {
        let leaked = Box::leak(answer.take().expect("not consumed by try_with"));
        &raw mut leaked.entry
    })
}

/// A host group, unless a system one hides it.
fn host_group(host: *mut libc::group) -> *mut GroupEntry {
    // SAFETY: libc's own entry, or null.
    if host.is_null()
        || crate::accounts::shadows_group(unsafe { CStr::from_ptr((*host).gr_name) }.to_bytes())
    {
        return std::ptr::null_mut();
    }
    host.cast()
}

/// `struct group *getgrgid(gid_t)`
pub extern "C" fn getgrgid(gid: u32) -> *mut GroupEntry {
    crate::errno::set(0);
    if let Some(group) = crate::accounts::system_group_by_gid(gid) {
        return publish_group(&group);
    }
    // SAFETY: a scalar; the answer is libc's own entry or null.
    host_group(unsafe { libc::getgrgid(gid) })
}

/// `struct group *getgrnam(const char *)`
///
/// # Safety
///
/// `name` is a NUL-terminated string.
pub unsafe extern "C" fn getgrnam(name: *const c_char) -> *mut GroupEntry {
    if name.is_null() {
        crate::errno::set(libc::EINVAL);
        return std::ptr::null_mut();
    }
    crate::errno::set(0);
    // SAFETY: the caller's string.
    if let Some(group) =
        crate::accounts::system_group_by_name(unsafe { CStr::from_ptr(name) }.to_bytes())
    {
        return publish_group(&group);
    }
    // SAFETY: the caller's string; the answer is libc's own entry or null.
    host_group(unsafe { libc::getgrnam(name) })
}

/// Pack a system group into the caller's `grp` and `buf`.
///
/// # Safety
///
/// `grp` is a writable `struct group`, `buf` is writable for `size` bytes,
/// and `result` is writable.
unsafe fn fill_group_r(
    group: &crate::accounts::Group,
    grp: *mut GroupEntry,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut GroupEntry,
) -> c_int {
    let pointer = size_of::<*mut c_char>();
    // The member list first, where `buf` is as aligned as it will ever be.
    let skew = (buf as usize).next_multiple_of(pointer) - buf as usize;
    let list = skew + pointer * (group.members.len() + 1);
    let strings: Vec<&str> = [group.name.as_str(), "*"]
        .into_iter()
        .chain(group.members.iter().map(String::as_str))
        .collect();
    let needed = list + strings.iter().map(|s| s.len() + 1).sum::<usize>();
    if needed > size {
        return libc::ERANGE;
    }
    let mut at = list;
    let mut placed = Vec::with_capacity(strings.len());
    for text in &strings {
        // SAFETY: `needed <= size`, so every copy lands inside `buf`.
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr().cast(), buf.add(at), text.len());
            *buf.add(at + text.len()) = 0;
            placed.push(buf.add(at));
        }
        at += text.len() + 1;
    }
    // SAFETY: inside `buf`, aligned, and room for every member and a null.
    unsafe {
        let members = buf.add(skew).cast::<*mut c_char>();
        for (i, member) in placed[2..].iter().enumerate() {
            *members.add(i) = *member;
        }
        *members.add(group.members.len()) = std::ptr::null_mut();
        *grp = GroupEntry {
            name: placed[0],
            passwd: placed[1],
            gid: group.gid,
            members,
        };
        *result = grp;
    }
    0
}

/// A host reentrant answer, unless a system group hides it.
///
/// # Safety
///
/// `result` is writable and, after a successful lookup, null or `grp`.
unsafe fn host_group_r(rc: c_int, result: *mut *mut GroupEntry) -> c_int {
    // SAFETY: the caller's obligation.
    unsafe {
        if rc == 0
            && !(*result).is_null()
            && crate::accounts::shadows_group(CStr::from_ptr((**result).name).to_bytes())
        {
            *result = std::ptr::null_mut();
        }
    }
    crate::errno::from_host(rc)
}

/// `int getgrgid_r(gid_t, struct group *, char *, size_t, struct group **)`
///
/// # Safety
///
/// As [`fill_group_r`].
pub unsafe extern "C" fn getgrgid_r(
    gid: u32,
    grp: *mut GroupEntry,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut GroupEntry,
) -> c_int {
    if result.is_null() || grp.is_null() || buf.is_null() {
        return libc::EINVAL;
    }
    unsafe { *result = std::ptr::null_mut() };
    if let Some(group) = crate::accounts::system_group_by_gid(gid) {
        return unsafe { fill_group_r(&group, grp, buf, size, result) };
    }
    // SAFETY: the layouts agree, and the buffers are the caller's.
    let rc = unsafe { libc::getgrgid_r(gid, grp.cast(), buf, size, result.cast()) };
    unsafe { host_group_r(rc, result) }
}

/// `int getgrnam_r(const char *, struct group *, char *, size_t, struct group **)`
///
/// # Safety
///
/// `name` is NUL-terminated, and as [`fill_group_r`].
pub unsafe extern "C" fn getgrnam_r(
    name: *const c_char,
    grp: *mut GroupEntry,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut GroupEntry,
) -> c_int {
    if name.is_null() || result.is_null() || grp.is_null() || buf.is_null() {
        return libc::EINVAL;
    }
    unsafe { *result = std::ptr::null_mut() };
    // SAFETY: the caller's string.
    if let Some(group) =
        crate::accounts::system_group_by_name(unsafe { CStr::from_ptr(name) }.to_bytes())
    {
        return unsafe { fill_group_r(&group, grp, buf, size, result) };
    }
    // SAFETY: the layouts agree, and the buffers are the caller's.
    let rc = unsafe { libc::getgrnam_r(name, grp.cast(), buf, size, result.cast()) };
    unsafe { host_group_r(rc, result) }
}

/// `int getgrouplist(const char *name, int basegid, int *groups, int *ngroups)`
///
/// As many as fit, and -1 with the full count in `ngroups` if that was not all.
///
/// # Safety
///
/// `name` is NUL-terminated, `ngroups` is writable, and `groups` is writable
/// for `*ngroups` entries.
pub unsafe extern "C" fn getgrouplist(
    name: *const c_char,
    basegid: c_int,
    groups: *mut c_int,
    ngroups: *mut c_int,
) -> c_int {
    if name.is_null() || ngroups.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: the caller's string.
    let list = crate::accounts::groups_of(unsafe { CStr::from_ptr(name) }, basegid as u32);
    // SAFETY: the caller's obligations.
    unsafe {
        let room = (*ngroups).max(0) as usize;
        for (i, gid) in list.iter().take(room).enumerate() {
            *groups.add(i) = *gid as c_int;
        }
        *ngroups = list.len() as c_int;
        if list.len() > room { -1 } else { 0 }
    }
}

/// `int setgroupent(int stayopen)` - BSD's `setgrent` that may keep the
/// database open. Nothing here is a file to keep open.
pub extern "C" fn setgroupent(_stayopen: c_int) -> c_int {
    // SAFETY: no arguments.
    unsafe { libc::setgrent() };
    1
}

/// `int setpassent(int stayopen)`, as [`setgroupent`] for users.
pub extern "C" fn setpassent(_stayopen: c_int) -> c_int {
    // SAFETY: no arguments.
    unsafe { libc::setpwent() };
    1
}

exports::exports! {
    functions {
        "getgrouplist_2" => getgrouplist_2,
        "getgrouplist" => getgrouplist,
        "getgrgid" => getgrgid,
        "getgrnam" => getgrnam,
        "getgrgid_r" => getgrgid_r,
        "getgrnam_r" => getgrnam_r,
        "setgroupent" => setgroupent,
        "setpassent" => setpassent,
        "getpwuid" => getpwuid,
        "getpwnam" => getpwnam,
        "getpwuid_r" => getpwuid_r,
        "getpwnam_r" => getpwnam_r,
        "user_from_uid" => user_from_uid,
        "group_from_gid" => group_from_gid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offsets are the guest's, and `pw_dir` is the one that matters: at
    /// Darwin's 48 it is past the end of this host's whole structure.
    #[test]
    fn the_layout_has_darwins_offsets() {
        let base = std::ptr::null::<Passwd>();
        let offset = |field: *const c_char| field as usize;
        // SAFETY: offsets computed from a null base, never dereferenced.
        unsafe {
            assert_eq!(offset(&raw const (*base).name as *const c_char), 0);
            assert_eq!(offset(&raw const (*base).class as *const c_char), 32);
            assert_eq!(offset(&raw const (*base).gecos as *const c_char), 40);
            assert_eq!(offset(&raw const (*base).dir as *const c_char), 48);
            assert_eq!(offset(&raw const (*base).shell as *const c_char), 56);
        }
        assert_eq!(core::mem::size_of::<Passwd>(), 72);
        #[cfg(target_os = "linux")]
        assert!(
            core::mem::size_of::<libc::passwd>() <= 48,
            "this host's whole structure ends at or before Darwin's pw_dir offset, \
             which is why reading it there reads past the end"
        );
    }

    /// And a real lookup answers with strings a guest can read.
    #[test]
    fn the_calling_user_has_a_name_and_a_home() {
        // SAFETY: this process's own uid.
        let entry = unsafe { getpwuid(libc::getuid()) };
        assert!(!entry.is_null(), "the calling user has a password entry");
        // SAFETY: just published, and this thread owns it.
        unsafe {
            assert!(!CStr::from_ptr((*entry).name).to_bytes().is_empty());
            assert!((*entry).dir.as_ref().is_some());
            assert!(!(*entry).class.is_null(), "empty, never null");
        }
    }

    /// The reentrant form packs Darwin's layout into the caller's buffer, and
    /// says so when the buffer is too small.
    #[test]
    fn getpwuid_r_fills_the_callers_structure_from_its_buffer() {
        // SAFETY: a zeroed Darwin entry, a buffer and a result slot of this
        // test's own.
        unsafe {
            let uid = libc::getuid();
            let mut pwd: Passwd = std::mem::zeroed();
            let mut buf = vec![0 as c_char; 4096];
            let mut result: *mut Passwd = std::ptr::null_mut();
            let rc = getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result);
            assert_eq!(rc, 0);
            assert_eq!(result, &raw mut pwd);
            assert_eq!(pwd.uid, uid);
            let range = buf.as_ptr() as usize..buf.as_ptr() as usize + buf.len();
            assert!(range.contains(&(pwd.name as usize)));
            assert!(range.contains(&(pwd.dir as usize)));
            assert!(!CStr::from_ptr(pwd.name).to_bytes().is_empty());

            let mut tiny = [0 as c_char; 2];
            let rc = getpwuid_r(uid, &mut pwd, tiny.as_mut_ptr(), tiny.len(), &mut result);
            assert_eq!(rc, libc::ERANGE);
            assert!(result.is_null());
        }
    }

    #[test]
    fn getgrouplist_2_allocates_the_users_groups_base_first() {
        // SAFETY: this process's own user, and a list this test frees.
        unsafe {
            let me = getpwuid(libc::getuid());
            assert!(!me.is_null());
            let base = (*me).gid;
            let mut groups: *mut u32 = std::ptr::null_mut();
            let count = getgrouplist_2((*me).name, base, &mut groups);
            assert!(count >= 1);
            let list = std::slice::from_raw_parts(groups, count as usize);
            assert_eq!(list[0], base);
            let mut unique = list.to_vec();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(unique.len(), list.len(), "no gid repeats");
            crate::heap::free(groups.cast());

            assert_eq!(getgrouplist_2(std::ptr::null(), base, &mut groups), -1);
        }
    }

    #[test]
    fn names_by_id_fall_back_to_the_number_or_null() {
        // SAFETY: this process's own uid, and pointers the cache keeps alive.
        unsafe {
            let me = CStr::from_ptr(user_from_uid(libc::getuid(), 0));
            assert!(!me.to_bytes().is_empty());
            let nobody = u32::MAX - 7;
            assert_eq!(
                CStr::from_ptr(user_from_uid(nobody, 0)).to_str(),
                Ok("4294967288")
            );
            assert!(user_from_uid(nobody, 1).is_null());
            assert!(group_from_gid(nobody, 1).is_null());
        }
    }
}
