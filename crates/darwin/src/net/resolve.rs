use std::ffi::{CStr, c_char, c_void};

use super::addr;

/// Darwin's `struct addrinfo`.
///
/// `#[repr(C)]` because the guest reads these fields at Darwin's offsets, and
/// because `canonname` and `addr` are adjacent pointers that Rust would
/// otherwise be free to order the host's way - which is the exact bug this
/// module exists to avoid.
#[repr(C)]
struct AddrInfo {
    flags: i32,
    family: i32,
    socktype: i32,
    protocol: i32,
    addrlen: u32,
    canonname: *mut c_char,
    addr: *mut c_void,
    next: *mut AddrInfo,
}

/// Darwin's `AI_*`, paired with the host's.
///
/// `AI_V4MAPPED_CFG` (`0x200`) maps to the same host flag as `AI_V4MAPPED`:
/// Darwin's distinction is "only if the system is configured for it", which has
/// no host switch, and asking for the mapping is closer than dropping it.
const HINT_FLAGS: &[(i32, i32)] = &[
    (0x0001, libc::AI_PASSIVE),
    (0x0002, libc::AI_CANONNAME),
    (0x0004, libc::AI_NUMERICHOST),
    (0x0100, libc::AI_ALL),
    (0x0200, libc::AI_V4MAPPED),
    (0x0400, libc::AI_ADDRCONFIG),
    (0x0800, libc::AI_V4MAPPED),
    (0x1000, libc::AI_NUMERICSERV),
];

fn hints_to_host(darwin: i32) -> i32 {
    let mut out = 0;
    for (theirs, ours) in HINT_FLAGS {
        if darwin & theirs != 0 {
            out |= ours;
        }
    }
    out
}

/// Darwin's `EAI_*`, paired with the host's.
const ERRORS: &[(i32, i32)] = &[
    // This host has no `EAI_ADDRFAMILY` of its own; Darwin's 1 has no counterpart
    // and falls through to the default below.
    (2, libc::EAI_AGAIN),
    (3, libc::EAI_BADFLAGS),
    (4, libc::EAI_FAIL),
    (5, libc::EAI_FAMILY),
    (6, libc::EAI_MEMORY),
    (8, libc::EAI_NONAME),
    (9, libc::EAI_SERVICE),
    (10, libc::EAI_SOCKTYPE),
    (11, libc::EAI_SYSTEM),
    (14, libc::EAI_OVERFLOW),
];

/// [`ERRORS`] and the codes only some hosts have: `EAI_NODATA` was dropped
/// from RFC 3493, so Darwin's 7 has no counterpart on FreeBSD and falls through
/// to the default.
fn errors() -> impl Iterator<Item = (i32, i32)> {
    ERRORS
        .iter()
        .copied()
        .chain(super::host::EAI_NODATA.map(|host| (7, host)))
}

/// This host's resolver error as a guest's.
///
/// `EAI_FAIL` for a code with no Darwin spelling: a permanent failure is the
/// honest summary of "this host reported something Darwin has no word for", and
/// it is not one a caller retries forever.
fn error_from_host(host: i32) -> i32 {
    match errors().find(|(_, ours)| *ours == host) {
        Some((theirs, _)) => theirs,
        None => 4,
    }
}

/// A guest's resolver error as this host's, for [`gai_strerror`].
fn error_to_host(darwin: i32) -> i32 {
    match errors().find(|(theirs, _)| *theirs == darwin) {
        Some((_, ours)) => ours,
        None => libc::EAI_FAIL,
    }
}

/// One node of the list this module returns, as a single allocation.
///
/// The `addrinfo`, then the socket address, then the canonical name, in one
/// `malloc` so that [`freeaddrinfo`] is one `free` per node rather than three -
/// and so that a guest cannot free a piece of one and leave the rest.
///
/// # Safety
///
/// `host` is a live host `struct addrinfo`.
unsafe fn node_from_host(host: &libc::addrinfo) -> Option<*mut AddrInfo> {
    // SAFETY: the host's own address, with the length it reported for it.
    let address = match host.ai_addr.is_null() {
        true => None,
        false => unsafe { addr::darwin_from_host(host.ai_addr.cast(), host.ai_addrlen) },
    };
    // SAFETY: the host's own canonical name, which is NUL-terminated.
    let name: Option<&CStr> = match host.ai_canonname.is_null() {
        true => None,
        false => Some(unsafe { CStr::from_ptr(host.ai_canonname) }),
    };

    let addr_len = address.as_ref().map_or(0, |(_, len)| *len);
    let name_len = name.map_or(0, |n| n.to_bytes_with_nul().len());
    let total = core::mem::size_of::<AddrInfo>() + addr_len + name_len;

    // SAFETY: a non-zero size; null is checked.
    let block = unsafe { libc::malloc(total) }.cast::<u8>();
    if block.is_null() {
        return None;
    }
    let node = block.cast::<AddrInfo>();
    // SAFETY: `block` is `total` bytes and the three pieces are laid out end to
    // end inside it, each within the size computed above.
    unsafe {
        let addr_at = block.add(core::mem::size_of::<AddrInfo>());
        let name_at = addr_at.add(addr_len);
        if let Some((bytes, len)) = &address {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), addr_at, *len);
        }
        if let Some(name) = name {
            core::ptr::copy_nonoverlapping(
                name.to_bytes_with_nul().as_ptr(),
                name_at,
                name.to_bytes_with_nul().len(),
            );
        }
        node.write(AddrInfo {
            // The flags a caller reads back are the hints it asked for, which is
            // what the host returns too.
            flags: 0,
            family: addr::family_from_host(host.ai_family).unwrap_or(addr::family::UNSPEC),
            socktype: host.ai_socktype,
            protocol: host.ai_protocol,
            addrlen: addr_len as u32,
            canonname: match name_len {
                0 => core::ptr::null_mut(),
                _ => name_at.cast(),
            },
            addr: match addr_len {
                0 => core::ptr::null_mut(),
                _ => addr_at.cast(),
            },
            next: core::ptr::null_mut(),
        });
    }
    Some(node)
}

/// `int getaddrinfo(const char *, const char *, const struct addrinfo *,
/// struct addrinfo **)`
///
/// # Safety
///
/// `node` and `service` are NUL-terminated or null; `hints` is a readable Darwin
/// `struct addrinfo` or null; `out` is a writable pointer slot.
unsafe extern "C" fn getaddrinfo(
    node: *const c_char,
    service: *const c_char,
    hints: *const c_void,
    out: *mut *mut AddrInfo,
) -> i32 {
    if out.is_null() {
        // Darwin's `EAI_SYSTEM`, with `errno` saying which system error.
        crate::errno::set(libc::EFAULT);
        return 11;
    }
    // SAFETY: NUL-terminated or null.
    if unsafe { blocked(node) } {
        // SAFETY: a writable slot, per the caller's guarantee.
        unsafe { *out = core::ptr::null_mut() };
        // Darwin's `EAI_NONAME`.
        return 8;
    }
    // SAFETY: zeroed is a valid `struct addrinfo` - no hints at all.
    let mut host_hints: libc::addrinfo = unsafe { core::mem::zeroed() };
    let mut hints_ptr = core::ptr::null();
    if !hints.is_null() {
        // SAFETY: the caller's guarantee. Only the first five fields are read,
        // and those are at the same offsets on both platforms.
        let guest = unsafe { hints.cast::<AddrInfo>().read() };
        let Some(family) = addr::family_to_host(guest.family) else {
            // Darwin's `EAI_FAMILY`.
            return 5;
        };
        host_hints.ai_flags = hints_to_host(guest.flags);
        host_hints.ai_family = family;
        host_hints.ai_socktype = guest.socktype;
        host_hints.ai_protocol = guest.protocol;
        hints_ptr = &raw const host_hints;
    }

    let mut list: *mut libc::addrinfo = core::ptr::null_mut();
    // SAFETY: the caller's two strings and hints this function owns.
    let rc = unsafe { libc::getaddrinfo(node, service, hints_ptr, &mut list) };
    if rc != 0 {
        // SAFETY: a writable slot, per the caller's guarantee.
        unsafe { *out = core::ptr::null_mut() };
        return error_from_host(rc);
    }

    let mut head: *mut AddrInfo = core::ptr::null_mut();
    let mut tail: *mut AddrInfo = core::ptr::null_mut();
    let mut at = list;
    while !at.is_null() {
        // SAFETY: a node of the list the host just returned.
        let Some(node) = (unsafe { node_from_host(&*at) }) else {
            // SAFETY: the host's own list, and the partial list built here.
            unsafe {
                libc::freeaddrinfo(list);
                free_list(head);
                *out = core::ptr::null_mut();
            }
            // Darwin's `EAI_MEMORY`.
            return 6;
        };
        match tail.is_null() {
            true => head = node,
            // SAFETY: `tail` is a node this loop allocated and still owns.
            false => unsafe { (*tail).next = node },
        }
        tail = node;
        at = unsafe { (*at).ai_next };
    }
    // SAFETY: the host's list, now copied.
    unsafe { libc::freeaddrinfo(list) };
    // SAFETY: a writable slot, per the caller's guarantee.
    unsafe { *out = head };
    0
}

/// Whether `ROSEMARY_BLOCK_HOST` names this host, as a comma-separated list.
///
/// A diagnostic, not a policy: a client that reads its configuration from a
/// server cannot be told to use the local file instead, and answering
/// `EAI_NONAME` for the server is the only way to find out what it would do
/// without it. `getaddrinfo` is the choke point because every connection this
/// program makes goes through it, name by name.
///
/// # Safety
///
/// `node` is NUL-terminated or null.
unsafe fn blocked(node: *const c_char) -> bool {
    #[static_init::dynamic]
    static NAMES: Vec<String> = std::env::var("ROSEMARY_BLOCK_HOST")
        .unwrap_or_default()
        .split(',')
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect();

    if NAMES.is_empty() || node.is_null() {
        return false;
    }
    let Ok(name) = (unsafe { CStr::from_ptr(node) }).to_str() else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    let refused = NAMES.iter().any(|blocked| name == *blocked);
    if refused {
        eprintln!("rosemary: {name}: refused (ROSEMARY_BLOCK_HOST names it)");
    }
    refused
}

/// Free a list [`getaddrinfo`] built.
///
/// # Safety
///
/// `head` is null or a list this module allocated.
unsafe fn free_list(head: *mut AddrInfo) {
    let mut at = head;
    while !at.is_null() {
        // SAFETY: a node this module allocated as one block, whose `next` is
        // read before the block goes away.
        unsafe {
            let next = (*at).next;
            libc::free(at.cast());
            at = next;
        }
    }
}

/// `void freeaddrinfo(struct addrinfo *)`
///
/// # Safety
///
/// `head` is null or a list [`getaddrinfo`] returned. Handing this the host's
/// own list would free a block this module did not allocate.
unsafe extern "C" fn freeaddrinfo(head: *mut AddrInfo) {
    unsafe { free_list(head) };
}

/// `const char *gai_strerror(int)`
///
/// The guest's error code, which is not the number the host's own would explain.
unsafe extern "C" fn gai_strerror(code: i32) -> *const c_char {
    // SAFETY: a static string owned by the host's resolver, valid for the life
    // of the process.
    unsafe { libc::gai_strerror(error_to_host(code)) }
}

/// Darwin's `NI_*`, paired with the host's. Every one of the five is a different
/// bit on the two platforms.
const NAME_FLAGS: &[(i32, i32)] = &[
    (0x01, libc::NI_NOFQDN),
    (0x02, libc::NI_NUMERICHOST),
    (0x04, libc::NI_NAMEREQD),
    (0x08, libc::NI_NUMERICSERV),
    (0x10, libc::NI_DGRAM),
];

fn name_flags_to_host(darwin: i32) -> i32 {
    let mut out = 0;
    for (theirs, ours) in NAME_FLAGS {
        if darwin & theirs != 0 {
            out |= ours;
        }
    }
    out
}

/// `int getnameinfo(const struct sockaddr *, socklen_t, char *, socklen_t,
/// char *, socklen_t, int)`
///
/// # Safety
///
/// `address` is `addrlen` readable bytes; `host` is `hostlen` writable bytes and
/// `service` is `servicelen` writable bytes, either of which may be null with a
/// zero length.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn getnameinfo(
    address: *const c_void,
    addrlen: u32,
    host: *mut c_char,
    hostlen: u32,
    service: *mut c_char,
    servicelen: u32,
    flags: i32,
) -> i32 {
    let Ok(sockaddr) = (unsafe { addr::to_host(address, addrlen) }) else {
        // Darwin's `EAI_FAMILY`.
        return 5;
    };
    // SAFETY: an address this function owns, and the caller's two buffers.
    let rc = unsafe {
        libc::getnameinfo(
            sockaddr.as_ptr(),
            sockaddr.len(),
            host,
            hostlen as _,
            service,
            servicelen as _,
            name_flags_to_host(flags),
        )
    };
    match rc {
        0 => 0,
        _ => error_from_host(rc),
    }
}

// The `hostent` family, none of which the `libc` crate declares. All three are
// obsolescent - `getaddrinfo` replaced them - and all three are still exported
// by both platforms and still imported by programs older than the replacement.
unsafe extern "C" {
    #[link_name = "gethostbyname"]
    fn host_gethostbyname(name: *const c_char) -> *mut libc::hostent;
    #[link_name = "gethostbyname2"]
    fn host_gethostbyname2(name: *const c_char, af: i32) -> *mut libc::hostent;
    #[link_name = "gethostbyaddr"]
    fn host_gethostbyaddr(addr: *const c_void, len: u32, af: i32) -> *mut libc::hostent;
}

/// The lookup that predates `getaddrinfo`, and the one field of its answer that
/// is a platform constant.
///
/// `struct hostent` is the same five fields at the same offsets on both
/// platforms, so nothing here is rebuilt - except `h_addrtype`, which is an
/// `AF_*`: an IPv6 answer says 10 here and has to say 30 to the guest, and 10
/// on Darwin is `AF_ROUTE`. A caller that switches on it - which is all of them,
/// because the field is what says how many bytes `h_addr_list` entries hold -
/// reads an address family it has a name for and did not ask about.
///
/// The answer is copied into a slot of this thread's rather than rewritten in
/// place, because the structure belongs to the resolver: writing Darwin's family
/// number into it would leave the host's own `gethostbyaddr` reading `AF_ROUTE`
/// out of its cache. The pointers inside still point at the resolver's storage,
/// which is the same lifetime a caller of this function already has - valid
/// until the next lookup on this thread.
///
/// # Safety
///
/// `answer` is a `hostent` a resolver returned, or null.
unsafe fn in_darwins_shape(answer: *mut libc::hostent) -> *mut c_void {
    if answer.is_null() {
        // FIXME (ACCURACY): the host's own reason is per-thread and spelled
        // differently on each host, so every failure reads as not found.
        H_ERRNO.store(HOST_NOT_FOUND, core::sync::atomic::Ordering::Relaxed);
        return core::ptr::null_mut();
    }
    let mut copy = unsafe { answer.read() };
    let Some(family) = addr::family_from_host(copy.h_addrtype) else {
        // A family the guest cannot name is no answer at all: it would read the
        // address length out of a field it has no case for.
        return core::ptr::null_mut();
    };
    copy.h_addrtype = family;

    thread_local! {
        static HELD: core::cell::UnsafeCell<libc::hostent> =
            // SAFETY: zeroed is the empty answer, and it is overwritten below
            // before any pointer to it is handed out.
            core::cell::UnsafeCell::new(unsafe { core::mem::zeroed() });
    }
    HELD.with(|slot| {
        // SAFETY: a slot of this thread's, which nothing else holds a reference
        // to - the pointer handed out is read, not aliased, before the next
        // lookup on this thread replaces it.
        unsafe { slot.get().write(copy) };
        slot.get().cast()
    })
}

/// `<netdb.h>`'s `HOST_NOT_FOUND`, the same number on every platform.
const HOST_NOT_FOUND: i32 = 1;

/// `int h_errno`, which Darwin's libSystem exports as a plain variable rather
/// than a per-thread accessor, so a guest reads it by address.
static H_ERRNO: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);

/// `struct hostent *gethostbyname(const char *)`
///
/// # Safety
///
/// `name` is a NUL-terminated string.
unsafe extern "C" fn gethostbyname(name: *const c_char) -> *mut c_void {
    // SAFETY: the caller's string.
    unsafe { in_darwins_shape(host_gethostbyname(name)) }
}

/// `struct hostent *gethostbyname2(const char *, int af)`
///
/// # Safety
///
/// As [`gethostbyname`].
unsafe extern "C" fn gethostbyname2(name: *const c_char, af: i32) -> *mut c_void {
    let Some(af) = addr::family_to_host(af) else {
        crate::errno::set(libc::EAFNOSUPPORT);
        return core::ptr::null_mut();
    };
    // SAFETY: the caller's string.
    unsafe { in_darwins_shape(host_gethostbyname2(name, af)) }
}

/// `struct hostent *gethostbyaddr(const void *, socklen_t, int)`
///
/// # Safety
///
/// `addr` is `len` readable bytes - an address of the family `af` names, which
/// is the same bytes on both platforms.
unsafe extern "C" fn gethostbyaddr(addr: *const c_void, len: u32, af: i32) -> *mut c_void {
    let Some(af) = addr::family_to_host(af) else {
        crate::errno::set(libc::EAFNOSUPPORT);
        return core::ptr::null_mut();
    };
    // SAFETY: the caller's address, at the length they gave for it.
    unsafe { in_darwins_shape(host_gethostbyaddr(addr, len, af)) }
}

exports::exports! {
    functions {
        "getaddrinfo" => getaddrinfo,
        "freeaddrinfo" => freeaddrinfo,
        "gai_strerror" => gai_strerror,
        "getnameinfo" => getnameinfo,
        "gethostbyname" => gethostbyname,
        "gethostbyname2" => gethostbyname2,
        "gethostbyaddr" => gethostbyaddr,
    }
    constants {
        "h_errno" => H_ERRNO.as_ptr() as usize as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout claim: forty-eight bytes on both, with the two pointers in the
    /// other order - which is why the list is rebuilt rather than handed over.
    #[test]
    fn the_two_structures_are_the_same_size_and_not_the_same_shape() {
        assert_eq!(core::mem::size_of::<AddrInfo>(), 48);
        assert_eq!(
            core::mem::size_of::<libc::addrinfo>(),
            core::mem::size_of::<AddrInfo>(),
            "the same size is what makes the swap silent"
        );
        let guest = AddrInfo {
            flags: 0,
            family: 0,
            socktype: 0,
            protocol: 0,
            addrlen: 0,
            canonname: core::ptr::null_mut(),
            addr: core::ptr::null_mut(),
            next: core::ptr::null_mut(),
        };
        let base = &raw const guest as usize;
        assert_eq!((&raw const guest.canonname as usize) - base, 24);
        assert_eq!((&raw const guest.addr as usize) - base, 32);
    }

    /// The collisions, named: `AI_V4MAPPED` and `AI_ALL` are both real flags on
    /// this host at the numbers Darwin uses for something else.
    #[test]
    fn the_hint_flags_are_translated_rather_than_passed_through() {
        assert_eq!(hints_to_host(0x0800), libc::AI_V4MAPPED);
        assert_eq!(hints_to_host(0x0100), libc::AI_ALL);
        assert_ne!(0x0800, libc::AI_V4MAPPED);
    }

    /// Darwin's error codes are positive and this host's negative, so an
    /// untranslated one is not merely wrong but the wrong sign.
    #[test]
    fn the_error_codes_are_translated_in_both_directions() {
        assert_eq!(error_from_host(libc::EAI_NONAME), 8);
        assert_eq!(error_to_host(8), libc::EAI_NONAME);
        assert!(libc::EAI_NONAME < 0);
    }

    /// Four of the five `NI_*` bits differ, and each of the four is a real flag
    /// on the other platform - so passing them through asks a different
    /// question rather than an invalid one. Only `NI_DGRAM` agrees.
    #[test]
    fn the_name_flags_that_differ_are_translated() {
        assert_eq!(name_flags_to_host(0x01), libc::NI_NOFQDN);
        assert_eq!(name_flags_to_host(0x02), libc::NI_NUMERICHOST);
        assert_eq!(name_flags_to_host(0x04), libc::NI_NAMEREQD);
        assert_eq!(name_flags_to_host(0x08), libc::NI_NUMERICSERV);
        assert_eq!(0x01, libc::NI_NUMERICHOST, "the collision being avoided");
        assert_eq!(
            name_flags_to_host(0x10),
            libc::NI_DGRAM,
            "the one that agrees"
        );
    }

    /// The loopback, resolved numerically so the test needs no network: the list
    /// comes back in Darwin's shape, with a Darwin family, and frees cleanly.
    #[test]
    fn a_numeric_lookup_returns_a_darwin_shaped_list() {
        let hints = AddrInfo {
            // Darwin's `AI_NUMERICHOST | AI_NUMERICSERV`.
            flags: 0x0004 | 0x1000,
            family: addr::family::INET,
            socktype: libc::SOCK_STREAM,
            protocol: 0,
            addrlen: 0,
            canonname: core::ptr::null_mut(),
            addr: core::ptr::null_mut(),
            next: core::ptr::null_mut(),
        };
        let mut list: *mut AddrInfo = core::ptr::null_mut();
        // SAFETY: two static strings, hints this test owns, and a live slot.
        let rc = unsafe {
            getaddrinfo(
                c"127.0.0.1".as_ptr(),
                c"443".as_ptr(),
                (&raw const hints).cast(),
                &mut list,
            )
        };
        assert_eq!(rc, 0, "a numeric lookup needs no resolver");
        assert!(!list.is_null());

        // SAFETY: the list this call just returned.
        unsafe {
            assert_eq!((*list).family, addr::family::INET);
            assert_eq!((*list).addrlen, 16);
            let bytes = (*list).addr.cast::<u8>();
            assert_eq!(bytes.read(), 16, "Darwin's sin_len");
            assert_eq!(bytes.add(1).read(), addr::family::INET as u8);
            assert_eq!(
                core::slice::from_raw_parts(bytes.add(2), 2),
                443u16.to_be_bytes()
            );
            assert_eq!(core::slice::from_raw_parts(bytes.add(4), 4), [127, 0, 0, 1]);
            freeaddrinfo(list);
        }
    }

    /// A name that does not resolve reports Darwin's code, not this host's.
    #[test]
    fn an_unknown_name_reports_darwins_error_code() {
        let hints = AddrInfo {
            // `AI_NUMERICHOST`, so nothing is asked of the network.
            flags: 0x0004,
            family: addr::family::UNSPEC,
            socktype: 0,
            protocol: 0,
            addrlen: 0,
            canonname: core::ptr::null_mut(),
            addr: core::ptr::null_mut(),
            next: core::ptr::null_mut(),
        };
        let mut list: *mut AddrInfo = core::ptr::null_mut();
        // SAFETY: a static string, hints this test owns, and a live slot.
        let rc = unsafe {
            getaddrinfo(
                c"not-an-address".as_ptr(),
                core::ptr::null(),
                (&raw const hints).cast(),
                &mut list,
            )
        };
        assert!(rc > 0, "Darwin's codes are positive; this host's are not");
        assert!(list.is_null());
    }
}
