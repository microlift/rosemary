#![cfg(not(target_os = "macos"))]

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
    /// BSD's password-change time. Not a thing this host records.
    change: i64,
    /// BSD's login class. Empty rather than null: a guest is entitled to print it.
    class: *mut c_char,
    gecos: *mut c_char,
    dir: *mut c_char,
    shell: *mut c_char,
    /// BSD's account-expiry time. As `change`.
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
    [
        text(host.pw_name),
        text(host.pw_passwd),
        // Darwin has a login class and this host has none. Empty is the honest
        // answer and a null is not: `pw_class` is a `char *` a guest may print.
        CString::default(),
        text(host.pw_gecos),
        home_of(host),
        text(host.pw_shell),
    ]
}

/// A [`Passwd`] over `host`'s ids and six strings at `at(i)`.
fn entry(host: &libc::passwd, at: impl Fn(usize) -> *mut c_char) -> Passwd {
    Passwd {
        name: at(0),
        passwd: at(1),
        uid: host.pw_uid,
        gid: host.pw_gid,
        change: 0,
        class: at(2),
        gecos: at(3),
        dir: at(4),
        shell: at(5),
        expire: 0,
    }
}

/// A host `struct passwd` as Darwin's, in this thread's storage.
///
/// # Safety
///
/// `host` is a live `struct passwd` this process's libc returned.
unsafe fn publish(host: *const libc::passwd) -> *mut Passwd {
    let host = unsafe { &*host };
    let strings = unsafe { strings_of(host) };
    let guest = entry(host, |i| strings[i].as_ptr().cast_mut());
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
    // SAFETY: takes a scalar and returns libc's own storage or null.
    let host = unsafe { libc::getpwuid(uid) };
    if host.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: non-null, and libc's own entry.
    unsafe { publish(host) }
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
    // SAFETY: the caller's string.
    let host = unsafe { libc::getpwnam(name) };
    if host.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: non-null, and libc's own entry.
    unsafe { publish(host) }
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
    // SAFETY: a scalar, or the caller's NUL-terminated name; both return libc's
    // own storage or null.
    let host = unsafe {
        match user {
            Some(name) => libc::getpwnam(name.as_ptr()),
            None => libc::getpwuid(libc::getuid()),
        }
    };
    if host.is_null() {
        // A container often has no passwd entry for the uid it runs as, and
        // `$HOME` is still the answer for the calling user.
        return match user {
            Some(_) => None,
            // A container often has no passwd entry for the uid it runs as,
            // and the guest's own home is still the answer for the caller.
            None => CString::new(prefix::ns::home()).ok(),
        };
    }
    // SAFETY: non-null, and libc's own entry.
    let home = home_of(unsafe { &*host });
    (!home.as_bytes().is_empty()).then_some(home)
}

/// The host's reentrant lookup, into a buffer of its own that grows until
/// the entry fits.
///
/// `Ok(None)` is "no such user", which the reentrant calls report as success
/// with a null result.
fn lookup_r(
    ask: impl Fn(*mut libc::passwd, *mut c_char, usize, *mut *mut libc::passwd) -> c_int,
) -> Result<Option<[CString; 6]>, c_int> {
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
            0 => return Ok(Some(unsafe { strings_of(&host) })),
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
    answer: Result<Option<[CString; 6]>, c_int>,
    host_ids: impl Fn() -> Option<(u32, u32)>,
    pwd: *mut Passwd,
    buf: *mut c_char,
    size: usize,
    result: *mut *mut Passwd,
) -> c_int {
    unsafe { *result = std::ptr::null_mut() };
    let strings = match answer {
        Ok(Some(strings)) => strings,
        Ok(None) => return 0,
        Err(error) => return error,
    };
    let Some((uid, gid)) = host_ids() else {
        return 0;
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

/// The uid and gid of the entry `ask` finds, for [`fill_r`].
fn ids_of(ask: impl Fn() -> *mut libc::passwd) -> Option<(u32, u32)> {
    let host = ask();
    // SAFETY: libc's own entry, read immediately.
    (!host.is_null()).then(|| unsafe { ((*host).pw_uid, (*host).pw_gid) })
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
    // SAFETY: the host's reentrant lookup into buffers `lookup_r` owns.
    let answer = lookup_r(|h, b, n, r| unsafe { libc::getpwuid_r(uid, h, b, n, r) });
    unsafe {
        fill_r(
            answer,
            || ids_of(|| libc::getpwuid(uid)),
            pwd,
            buf,
            size,
            result,
        )
    }
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
    // SAFETY: the host's reentrant lookup into buffers `lookup_r` owns.
    let answer = lookup_r(|h, b, n, r| unsafe { libc::getpwnam_r(name, h, b, n, r) });
    unsafe {
        fill_r(
            answer,
            || ids_of(|| libc::getpwnam(name)),
            pwd,
            buf,
            size,
            result,
        )
    }
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
        // SAFETY: a scalar, answered with libc's own entry or null.
        let host = unsafe { libc::getpwuid(uid) };
        // SAFETY: libc's own NUL-terminated name.
        (!host.is_null()).then(|| unsafe { CString::from(CStr::from_ptr((*host).pw_name)) })
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
        // SAFETY: a scalar, answered with libc's own entry or null.
        let host = unsafe { libc::getgrgid(gid) };
        // SAFETY: libc's own NUL-terminated name.
        (!host.is_null()).then(|| unsafe { CString::from(CStr::from_ptr((*host).gr_name)) })
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
    let mut host: Vec<u32> = Vec::new();
    let mut count: c_int = 32;
    loop {
        host.resize(count as usize, 0);
        let offered = count;
        // SAFETY: `host` holds `count` slots, and glibc writes no more.
        if unsafe { libc::getgrouplist(name, basegid, host.as_mut_ptr(), &mut count) } >= 0 {
            host.truncate(count as usize);
            break;
        }
        // glibc reports the size it needed; grow regardless, in case the
        // group database changed between the two calls.
        count = count.max(offered.saturating_mul(2));
    }
    let mut merged = vec![basegid];
    for gid in host {
        if !merged.contains(&gid) {
            merged.push(gid);
        }
    }
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

exports::exports! {
    functions {
        "getgrouplist_2" => getgrouplist_2,
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
    fn the_layout_is_darwins_and_not_this_hosts() {
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
            let me = libc::getpwuid(libc::getuid());
            assert!(!me.is_null());
            let base = (*me).pw_gid;
            let mut groups: *mut u32 = std::ptr::null_mut();
            let count = getgrouplist_2((*me).pw_name, base, &mut groups);
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
