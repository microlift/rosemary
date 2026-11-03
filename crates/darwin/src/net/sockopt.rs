use std::ffi::c_void;

use super::host;
use crate::errno;

/// Darwin's `SOL_SOCKET`.
const SOL_SOCKET: i32 = 0xffff;
/// The `IPPROTO_*` levels, which do agree: they are the IANA protocol numbers.
const IPPROTO_IP: i32 = 0;
const IPPROTO_TCP: i32 = 6;
const IPPROTO_IPV6: i32 = 41;

/// Darwin socket option names from `<sys/socket.h>`.
mod so {
    pub const DEBUG: i32 = 0x0001;
    pub const ACCEPTCONN: i32 = 0x0002;
    pub const REUSEADDR: i32 = 0x0004;
    pub const KEEPALIVE: i32 = 0x0008;
    pub const DONTROUTE: i32 = 0x0010;
    pub const BROADCAST: i32 = 0x0020;
    pub const LINGER: i32 = 0x0080;
    pub const OOBINLINE: i32 = 0x0100;
    pub const REUSEPORT: i32 = 0x0200;
    pub const TIMESTAMP: i32 = 0x0400;
    pub const SNDBUF: i32 = 0x1001;
    pub const RCVBUF: i32 = 0x1002;
    pub const SNDLOWAT: i32 = 0x1003;
    pub const RCVLOWAT: i32 = 0x1004;
    pub const SNDTIMEO: i32 = 0x1005;
    pub const RCVTIMEO: i32 = 0x1006;
    pub const ERROR: i32 = 0x1007;
    pub const TYPE: i32 = 0x1008;
    pub const NREAD: i32 = 0x1020;
    pub const NOSIGPIPE: i32 = 0x1022;
    pub const NWRITE: i32 = 0x1024;
    pub const LINGER_SEC: i32 = 0x1080;
}

/// Darwin IPv4 option names from `<netinet/in.h>`.
///
/// Allowed dead because the set is the point: a name missing from one of these
/// tables is a Darwin option that would be forwarded unrecognised, and a host
/// with no counterpart for one leaves it listed but unused.
#[allow(dead_code)]
mod ip {
    pub const OPTIONS: i32 = 1;
    pub const HDRINCL: i32 = 2;
    pub const TOS: i32 = 3;
    pub const TTL: i32 = 4;
    pub const RECVOPTS: i32 = 5;
    pub const MULTICAST_IF: i32 = 9;
    pub const MULTICAST_TTL: i32 = 10;
    pub const MULTICAST_LOOP: i32 = 11;
    pub const ADD_MEMBERSHIP: i32 = 12;
    pub const DROP_MEMBERSHIP: i32 = 13;
    pub const RECVTTL: i32 = 24;
    pub const PKTINFO: i32 = 26;
    pub const RECVTOS: i32 = 27;
    pub const DONTFRAG: i32 = 28;
}

/// Darwin IPv6 option names from `<netinet6/in6.h>`.
#[allow(dead_code)]
mod ipv6 {
    pub const UNICAST_HOPS: i32 = 4;
    pub const MULTICAST_IF: i32 = 9;
    pub const MULTICAST_HOPS: i32 = 10;
    pub const MULTICAST_LOOP: i32 = 11;
    pub const ADD_MEMBERSHIP: i32 = 12;
    pub const DROP_MEMBERSHIP: i32 = 13;
    pub const V6ONLY: i32 = 27;
    pub const RECVTCLASS: i32 = 35;
    pub const TCLASS: i32 = 36;
    pub const RECVPKTINFO: i32 = 61;
    pub const DONTFRAG: i32 = 62;
}

/// Darwin TCP option names from `<netinet/tcp.h>`.
#[allow(dead_code)]
mod tcp {
    pub const NODELAY: i32 = 0x01;
    pub const MAXSEG: i32 = 0x02;
    pub const NOPUSH: i32 = 0x04;
    pub const KEEPALIVE: i32 = 0x10;
    pub const KEEPINTVL: i32 = 0x101;
    pub const KEEPCNT: i32 = 0x102;
    pub const NOTSENT_LOWAT: i32 = 0x201;
}

/// Darwin's `SO_*` names, paired with the host's.
///
/// `SO_NOSIGPIPE` (`0x1022`) is absent: it has no host counterpart at all. See
/// [`nosigpipe`]. `SO_NREAD` and `SO_NWRITE` are absent for the opposite reason
/// - this host answers both, with an ioctl rather than an option; see
/// [`queued_bytes`].
const SOCKET_OPTIONS: &[(i32, i32)] = &[
    (so::DEBUG, libc::SO_DEBUG),
    (so::ACCEPTCONN, libc::SO_ACCEPTCONN),
    (so::REUSEADDR, libc::SO_REUSEADDR),
    (so::KEEPALIVE, libc::SO_KEEPALIVE),
    (so::DONTROUTE, libc::SO_DONTROUTE),
    (so::BROADCAST, libc::SO_BROADCAST),
    (so::LINGER, libc::SO_LINGER),
    (so::OOBINLINE, libc::SO_OOBINLINE),
    (so::REUSEPORT, libc::SO_REUSEPORT),
    (so::TIMESTAMP, libc::SO_TIMESTAMP),
    (so::SNDBUF, libc::SO_SNDBUF),
    (so::RCVBUF, libc::SO_RCVBUF),
    (so::SNDLOWAT, libc::SO_SNDLOWAT),
    (so::RCVLOWAT, libc::SO_RCVLOWAT),
    (so::SNDTIMEO, libc::SO_SNDTIMEO),
    (so::RCVTIMEO, libc::SO_RCVTIMEO),
    (so::ERROR, libc::SO_ERROR),
    (so::TYPE, libc::SO_TYPE),
    // `SO_LINGER_SEC` is `SO_LINGER` with `l_linger` in seconds, which is the
    // only unit this host's `SO_LINGER` has.
    (so::LINGER_SEC, libc::SO_LINGER),
];

/// Darwin's `IP_*`, paired with the host's.
///
/// The numbering agrees with this host's up to `IP_RECVOPTS` and then stops:
/// Darwin's 26 is `IP_PKTINFO` (spelled `IP_RECVPKTINFO` when it is asked for
/// on receive) where this host's is 8, and Darwin's 27 is `IP_RECVTOS`.
const IP_OPTIONS: &[(i32, i32)] = &[
    (ip::OPTIONS, host::IP_OPTIONS),
    (ip::HDRINCL, libc::IP_HDRINCL),
    (ip::TOS, libc::IP_TOS),
    (ip::TTL, libc::IP_TTL),
    (ip::RECVOPTS, host::IP_RECVOPTS),
    (ip::MULTICAST_IF, libc::IP_MULTICAST_IF),
    (ip::MULTICAST_TTL, libc::IP_MULTICAST_TTL),
    (ip::MULTICAST_LOOP, libc::IP_MULTICAST_LOOP),
    (ip::ADD_MEMBERSHIP, libc::IP_ADD_MEMBERSHIP),
    (ip::DROP_MEMBERSHIP, libc::IP_DROP_MEMBERSHIP),
    (ip::RECVTTL, libc::IP_RECVTTL),
    (ip::RECVTOS, host::IP_RECVTOS),
];

/// Entries only some hosts can answer; a `None` is refused, not approximated.
const IP_OPTIONAL: &[(i32, Option<i32>)] = &[(ip::PKTINFO, host::IP_PKTINFO)];

/// Darwin's `IPV6_*`, paired with the host's.
///
/// Darwin keeps the RFC 3493 numbers - `IPV6_UNICAST_HOPS` is 4, as it is on
/// every other BSD - and this host does not: 4 here is `IPV6_ADDRFORM`, and 16
/// here is the hop limit. Darwin's 36 is `IPV6_TCLASS`, which is what this table
/// once aimed at the hop limit.
const IPV6_OPTIONS: &[(i32, i32)] = &[
    (ipv6::UNICAST_HOPS, libc::IPV6_UNICAST_HOPS),
    (ipv6::MULTICAST_IF, libc::IPV6_MULTICAST_IF),
    (ipv6::MULTICAST_HOPS, libc::IPV6_MULTICAST_HOPS),
    (ipv6::MULTICAST_LOOP, libc::IPV6_MULTICAST_LOOP),
    // The BSDs spell these `IPV6_JOIN_GROUP`/`IPV6_LEAVE_GROUP`; same numbers,
    // same payload.
    (ipv6::ADD_MEMBERSHIP, host::IPV6_JOIN_GROUP),
    (ipv6::DROP_MEMBERSHIP, host::IPV6_LEAVE_GROUP),
    (ipv6::V6ONLY, libc::IPV6_V6ONLY),
    (ipv6::RECVTCLASS, libc::IPV6_RECVTCLASS),
    (ipv6::TCLASS, libc::IPV6_TCLASS),
    (ipv6::RECVPKTINFO, libc::IPV6_RECVPKTINFO),
];

/// Darwin's `TCP_*`, paired with the host's.
///
/// `TCP_NOPUSH` (4) is this host's `TCP_CORK`, and the two are not the same
/// promise: Darwin's is advisory - the stack sends when it suits it - and this
/// host's holds a partial segment until the option is cleared or 200ms pass.
/// Mapped anyway, because the failure modes are not comparable. A caller sets
/// `TCP_NOPUSH` around a header-then-body write and clears it after; corking is
/// what it meant, and the worst it costs is the delay it asked to avoid. The
/// refusal it replaces cost the write itself, three layers under a caller that
/// reported the network was broken.
const TCP_OPTIONS: &[(i32, i32)] = &[
    (tcp::NODELAY, libc::TCP_NODELAY),
    (tcp::MAXSEG, libc::TCP_MAXSEG),
    // Darwin's own meaning on the BSDs, so the mapping is exact there; see
    // `host::TCP_NOPUSH`.
    (tcp::NOPUSH, host::TCP_NOPUSH),
    (tcp::KEEPALIVE, host::TCP_KEEPIDLE),
    (tcp::KEEPINTVL, libc::TCP_KEEPINTVL),
    (tcp::KEEPCNT, libc::TCP_KEEPCNT),
];

/// As [`IP_OPTIONAL`].
const TCP_OPTIONAL: &[(i32, Option<i32>)] = &[(tcp::NOTSENT_LOWAT, host::TCP_NOTSENT_LOWAT)];

/// Darwin's `SO_NOSIGPIPE`, which this host has no option for.
///
/// Linux asks for the same thing per *call*, with `MSG_NOSIGNAL`. So the switch
/// is kept here and [`crate::net::socket`] reads it: a `send` on a socket the
/// guest set the option on is sent with `MSG_NOSIGNAL`, and the guest gets
/// `EPIPE` instead of a signal, which is what it asked for.
///
/// `write` and `writev` take no flags and so had nowhere to put it, which used
/// to be the gap: a guest that set the option and then wrote to a closed socket
/// still died of a signal. Both are now this crate's, and a write to a
/// descriptor in this set goes out as a `send`; see [`crate::net::socket`].
///
/// Refusing instead - which this module used to do, `ENOPROTOOPT` being what a
/// kernel without the option would say - turned out not to be survivable:
/// Crashpad `CHECK`s the return value and aborts the process, so the honest
/// failure was a crash before the program had started.
fn nosigpipe(level: i32, name: i32) -> bool {
    level == SOL_SOCKET && name == so::NOSIGPIPE
}

/// Darwin's `IP_DONTFRAG` / `IPV6_DONTFRAG`: set the Don't Fragment (DF) bit on
/// outgoing packets.
///
/// On Linux, there is no direct `IP_DONTFRAG` socket option; the DF bit is
/// controlled via Path MTU Discovery (`IP_MTU_DISCOVER` / `IPV6_MTU_DISCOVER`).
/// Setting `IP_DONTFRAG` to 1 maps to `IP_PMTUDISC_DO`, and 0 maps to
/// `IP_PMTUDISC_DONT`. On Darwin and BSDs, the option is native.
fn is_dontfrag(level: i32, name: i32) -> bool {
    (level == IPPROTO_IP && name == ip::DONTFRAG)
        || (level == IPPROTO_IPV6 && name == ipv6::DONTFRAG)
}

/// # Safety
///
/// `value` is `len` readable bytes, or is null with `len` zero.
unsafe fn set_dontfrag(fd: i32, level: i32, value: *const c_void, len: u32) -> i32 {
    if value.is_null() || (len as usize) < core::mem::size_of::<i32>() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: caller guarantees `value` points to at least `len` readable bytes.
    let on = unsafe { value.cast::<i32>().read_unaligned() } != 0;
    #[cfg(target_os = "linux")]
    {
        let (host_level, host_opt, host_val) = if level == IPPROTO_IP {
            (
                libc::IPPROTO_IP,
                libc::IP_MTU_DISCOVER,
                if on {
                    libc::IP_PMTUDISC_DO
                } else {
                    libc::IP_PMTUDISC_DONT
                },
            )
        } else {
            (
                libc::IPPROTO_IPV6,
                libc::IPV6_MTU_DISCOVER,
                if on {
                    libc::IPV6_PMTUDISC_DO
                } else {
                    libc::IPV6_PMTUDISC_DONT
                },
            )
        };
        // SAFETY: host_val is a stack-allocated c_int.
        unsafe {
            libc::setsockopt(
                fd,
                host_level,
                host_opt,
                (&raw const host_val).cast(),
                core::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (host_level, host_opt) = if level == IPPROTO_IP {
            (libc::IPPROTO_IP, ip::DONTFRAG)
        } else {
            (libc::IPPROTO_IPV6, ipv6::DONTFRAG)
        };
        let host_val: libc::c_int = on as libc::c_int;
        // SAFETY: host_val is a stack-allocated c_int.
        unsafe {
            libc::setsockopt(
                fd,
                host_level,
                host_opt,
                (&raw const host_val).cast(),
                core::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        }
    }
}

/// # Safety
///
/// `value` is `*len` writable bytes and `len` is a writable `socklen_t`.
unsafe fn get_dontfrag(fd: i32, level: i32, value: *mut c_void, len: *mut u32) -> i32 {
    if value.is_null() || len.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: caller guarantees `len` points to a writable socklen_t.
    if (unsafe { len.read() } as usize) < core::mem::size_of::<i32>() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    #[cfg(target_os = "linux")]
    {
        let (host_level, host_opt) = if level == IPPROTO_IP {
            (libc::IPPROTO_IP, libc::IP_MTU_DISCOVER)
        } else {
            (libc::IPPROTO_IPV6, libc::IPV6_MTU_DISCOVER)
        };
        let mut host_val: libc::c_int = 0;
        let mut host_len = core::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: host_val is stack-allocated with its own size in host_len.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                host_level,
                host_opt,
                (&raw mut host_val).cast(),
                &mut host_len,
            )
        };
        if rc == 0 {
            let on: i32 = match host_val {
                libc::IP_PMTUDISC_DO | libc::IP_PMTUDISC_PROBE => 1,
                _ => 0,
            };
            // SAFETY: `value` has room for an i32, verified above.
            unsafe {
                value.cast::<i32>().write_unaligned(on);
                len.write(core::mem::size_of::<i32>() as u32);
            }
        }
        rc
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (host_level, host_opt) = if level == IPPROTO_IP {
            (libc::IPPROTO_IP, ip::DONTFRAG)
        } else {
            (libc::IPPROTO_IPV6, ipv6::DONTFRAG)
        };
        let mut host_val: libc::c_int = 0;
        let mut host_len = core::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: host_val is stack-allocated with its own size in host_len.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                host_level,
                host_opt,
                (&raw mut host_val).cast(),
                &mut host_len,
            )
        };
        if rc == 0 {
            let on: i32 = if host_val != 0 { 1 } else { 0 };
            // SAFETY: `value` has room for an i32, verified above.
            unsafe {
                value.cast::<i32>().write_unaligned(on);
                len.write(core::mem::size_of::<i32>() as u32);
            }
        }
        rc
    }
}

/// Darwin's `SO_NREAD` and `SO_NWRITE`: how much is waiting to be read, and how
/// much this side has written that the peer has not acknowledged.
///
/// Both are read-only options on Darwin and neither is an option at all here -
/// this host asks the same two questions with `FIONREAD` and `TIOCOUTQ`. A
/// `setsockopt` on either is `ENOPROTOOPT`, which is what Darwin answers too.
/// # Safety
///
/// `value` is `*len` writable bytes and `len` is a writable `socklen_t`.
unsafe fn queued_bytes(fd: i32, name: i32, value: *mut c_void, len: *mut u32) -> i32 {
    if value.is_null() || len.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: the caller's length slot.
    if (unsafe { len.read() } as usize) < core::mem::size_of::<i32>() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let request = match (name, host::QUEUED_WRITE_IOCTL) {
        (so::NREAD, _) => libc::FIONREAD as libc::c_ulong,
        (_, Some(request)) => request,
        // The host has `SO_NWRITE` as an option of its own, in Darwin's number.
        // SAFETY: the caller's buffer and length slot, checked above.
        (_, None) => {
            return unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, so::NWRITE, value, len) };
        }
    };
    let mut bytes: i32 = 0;
    // SAFETY: both requests write one `int` through the pointer they are given.
    if unsafe { libc::ioctl(fd, request as _, &raw mut bytes) } < 0 {
        return -1;
    }
    // SAFETY: at least an `int` of the caller's, just checked.
    unsafe {
        value.cast::<i32>().write_unaligned(bytes);
        len.write(core::mem::size_of::<i32>() as u32);
    }
    0
}

/// The descriptors `SO_NOSIGPIPE` is set on.
///
/// `close` clears entries eagerly and descriptor duplication copies or replaces
/// them. `write` and `writev` still discard an entry when a file-type probe, or
/// a raced `send`, proves its descriptor is no longer a socket.
static SUPPRESSED: std::sync::RwLock<Option<std::collections::HashSet<i32>>> =
    std::sync::RwLock::new(None);

/// How many descriptors [`SUPPRESSED`] holds.
///
/// Kept beside the set because [`crate::net::socket::write`] asks on every
/// write a guest makes, and a process that never set the option - which is
/// most of them - should pay a relaxed load for that rather than a lock.
static SUPPRESSED_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn with_suppressed_mut<R>(f: impl FnOnce(&mut std::collections::HashSet<i32>) -> R) -> R {
    let mut guard = SUPPRESSED.write().unwrap_or_else(|e| e.into_inner());
    let held = guard.get_or_insert_with(Default::default);
    let answer = f(held);
    SUPPRESSED_COUNT.store(held.len(), std::sync::atomic::Ordering::Relaxed);
    answer
}

fn suppressed_contains(fd: i32) -> bool {
    let guard = SUPPRESSED.read().unwrap_or_else(|e| e.into_inner());
    guard.as_ref().is_some_and(|held| held.contains(&fd))
}

/// Whether any descriptor at all was given `SO_NOSIGPIPE`.
pub fn any_suppress_sigpipe() -> bool {
    SUPPRESSED_COUNT.load(std::sync::atomic::Ordering::Relaxed) != 0
}

/// Whether `fd` was given `SO_NOSIGPIPE`, still names that socket, and so wants
/// `MSG_NOSIGNAL`.
///
/// Membership alone is insufficient because descriptor replacement can race a
/// write. The file type reported by `fstat` distinguishes a live socket from a
/// reused non-socket. This check is only made for a number present in the set.
pub fn suppresses_sigpipe(fd: i32) -> bool {
    if !any_suppress_sigpipe() || !suppressed_contains(fd) {
        return false;
    }

    // This probe is invisible to the caller, including when it discovers a
    // stale entry. The operation that follows is responsible for errno.
    let _errno_guard = errno::Held::now();
    // SAFETY: zero is a valid initial representation for the host's `stat`,
    // and the call receives a writable object of exactly that type.
    let mut status: libc::stat = unsafe { core::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd, &raw mut status) };
    let is_socket = rc == 0 && status.st_mode & libc::S_IFMT == libc::S_IFSOCK;
    if !is_socket {
        forget(fd);
    }
    is_socket
}

/// Give a successfully duplicated descriptor the source's suppression state.
///
/// Darwin stores `SO_NOSIGPIPE` on the socket, so every descriptor referring
/// to it observes the same value. The host has no option to inherit for us.
pub fn inherit_sigpipe_suppression(from: i32, to: i32) {
    if from == to || to < 0 {
        return;
    }
    let suppressed = suppresses_sigpipe(from);
    if suppressed || any_suppress_sigpipe() {
        with_suppressed_mut(|held| match suppressed {
            true => held.insert(to),
            false => held.remove(&to),
        });
    }
}

/// Forget any option set on `fd`, because it no longer names that socket.
pub fn forget(fd: i32) {
    if fd >= 0 && any_suppress_sigpipe() {
        with_suppressed_mut(|held| held.remove(&fd));
    }
}

/// # Safety
///
/// `value` is `len` readable bytes, or is null with `len` zero.
unsafe fn set_nosigpipe(fd: i32, value: *const c_void, len: u32) -> i32 {
    if value.is_null() || (len as usize) < core::mem::size_of::<i32>() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: at least an `int` of the caller's, just checked.
    let wanted = unsafe { value.cast::<i32>().read_unaligned() } != 0;
    if let Some(option) = host::NOSIGPIPE_SOCKOPT {
        // A host with no `MSG_NOSIGNAL` is told on the socket itself.
        let on = wanted as i32;
        // SAFETY: an `int` this function owns, with its own size.
        let rc =
            unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, option, (&raw const on).cast(), 4) };
        if rc < 0 {
            return -1;
        }
    }
    with_suppressed_mut(|held| match wanted {
        true => held.insert(fd),
        false => held.remove(&fd),
    });
    0
}

/// # Safety
///
/// `value` is `*len` writable bytes and `len` is a writable `socklen_t`.
unsafe fn get_nosigpipe(fd: i32, value: *mut c_void, len: *mut u32) -> i32 {
    if value.is_null() || len.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: the caller's length slot.
    if (unsafe { len.read() } as usize) < core::mem::size_of::<i32>() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: at least an `int` of the caller's, just checked.
    unsafe {
        value
            .cast::<i32>()
            .write_unaligned(suppresses_sigpipe(fd) as i32);
        len.write(core::mem::size_of::<i32>() as u32);
    }
    0
}

/// `(level, name)` in the host's numbering, or `None` for a pair this module has
/// not been taught.
///
/// A `None` is named on stderr, once per pair. `ENOPROTOOPT` on its own is a
/// refusal the caller reports as a network failure three layers away - it looks
/// like the peer, not like a gap in this table - and the pair is the only thing
/// that says which entry to add.
fn option_to_host(level: i32, name: i32) -> Option<(i32, i32)> {
    let translated = || {
        let (host_level, table) = match level {
            SOL_SOCKET => (libc::SOL_SOCKET, SOCKET_OPTIONS),
            IPPROTO_IP => (IPPROTO_IP, IP_OPTIONS),
            IPPROTO_IPV6 => (IPPROTO_IPV6, IPV6_OPTIONS),
            IPPROTO_TCP => (IPPROTO_TCP, TCP_OPTIONS),
            _ => return None,
        };
        let optional = match level {
            IPPROTO_IP => IP_OPTIONAL,
            IPPROTO_TCP => TCP_OPTIONAL,
            _ => &[],
        };
        let host_name = table
            .iter()
            .find(|(theirs, _)| *theirs == name)
            .map(|entry| entry.1)
            .or_else(|| optional.iter().find(|(theirs, _)| *theirs == name)?.1)?;
        Some((host_level, host_name))
    };
    let found = translated();
    if found.is_none() {
        say_unknown_option(level, name);
    }
    found
}

/// Name a refused `(level, name)` pair, once for each distinct one.
fn say_unknown_option(level: i32, name: i32) {
    use std::sync::Mutex;
    static SAID: Mutex<Vec<(i32, i32)>> = Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.contains(&(level, name)) {
        return;
    }
    said.push((level, name));
    eprintln!(
        "rosemary: socket option level {level:#x} name {name:#x} is not in \
         darwin::net::sockopt's tables - refused with ENOPROTOOPT"
    );
}

/// Whether an option's *value* is a `struct timeval`, whose two platforms differ
/// in the width of `tv_usec`.
///
/// Darwin's `suseconds_t` is a 32-bit `int` followed by four bytes of padding;
/// this host's is a 64-bit `long`. The bytes are the same length and not the
/// same value: a guest asking for a 500 ms timeout is asking here for one of
/// 500000 seconds plus whatever the padding held.
fn is_timeval(level: i32, name: i32) -> bool {
    level == SOL_SOCKET && matches!(name, so::SNDTIMEO | so::RCVTIMEO)
}

/// A guest's `struct timeval` as this host's.
///
/// # Safety
///
/// `value` is at least sixteen readable bytes.
pub(crate) unsafe fn timeval_to_host(value: *const c_void) -> libc::timeval {
    // SAFETY: the caller's guarantee; `Timeval::read` performs an unaligned
    // read of the shared Darwin guest representation.
    unsafe { crate::clock::Timeval::read(value.cast()).host() }
}

/// `int setsockopt(int, int, int, const void *, socklen_t)`
///
/// # Safety
///
/// `value` is `len` readable bytes, or is null with `len` zero.
pub(crate) unsafe extern "C" fn setsockopt(
    fd: i32,
    level: i32,
    name: i32,
    value: *const c_void,
    len: u32,
) -> i32 {
    if nosigpipe(level, name) {
        // SAFETY: the caller's buffer, with the length they gave for it.
        return unsafe { set_nosigpipe(fd, value, len) };
    }
    if is_dontfrag(level, name) {
        // SAFETY: the caller's buffer, with the length they gave for it.
        return unsafe { set_dontfrag(fd, level, value, len) };
    }
    if level == SOL_SOCKET && matches!(name, so::NREAD | so::NWRITE) {
        crate::errno::set(libc::ENOPROTOOPT);
        return -1;
    }
    let Some((level, name)) = option_to_host(level, name) else {
        crate::errno::set(libc::ENOPROTOOPT);
        return -1;
    };
    if level == libc::SOL_SOCKET && (name == libc::SO_SNDTIMEO || name == libc::SO_RCVTIMEO) {
        if value.is_null() || (len as usize) < 16 {
            crate::errno::set(libc::EINVAL);
            return -1;
        }
        // SAFETY: sixteen readable bytes, just checked.
        let host = unsafe { timeval_to_host(value) };
        // SAFETY: a `timeval` this function owns, with its own size.
        return unsafe {
            libc::setsockopt(
                fd,
                level,
                name,
                (&raw const host).cast(),
                core::mem::size_of::<libc::timeval>() as u32,
            )
        };
    }
    // SAFETY: the caller's buffer, with the length they gave for it. Every
    // remaining option's value is an `int` or a `struct` that agrees on both.
    unsafe { libc::setsockopt(fd, level, name, value, len) }
}

/// `SOL_LOCAL`, and the options on it that name the peer of a Unix socket.
const SOL_LOCAL: i32 = 0;
const LOCAL_PEERCRED: i32 = 0x001;
const LOCAL_PEERPID: i32 = 0x002;
const LOCAL_PEEREPID: i32 = 0x003;

/// Darwin's `struct xucred`: version, uid, a count, and the group list, whose
/// first entry is the effective gid.
#[repr(C)]
struct Xucred {
    version: u32,
    uid: u32,
    ngroups: i16,
    /// The two bytes of padding Darwin's layout has here, spelled out so the
    /// struct has none Rust could leave uninitialised.
    _pad: u16,
    groups: [u32; 16],
}

impl Xucred {
    fn as_bytes(&self) -> &[u8] {
        // SAFETY: `repr(C)` of plain integers with no padding (see `_pad`).
        unsafe { core::slice::from_raw_parts((self as *const Xucred).cast(), size_of::<Xucred>()) }
    }
}

/// The `SOL_LOCAL` options, which this host has none of: Linux says the same
/// things through `SO_PEERCRED`, in a different structure.
///
/// # Safety
///
/// `value` is `*len` writable bytes and `len` is a writable `socklen_t`.
unsafe fn local_option(fd: i32, name: i32, value: *mut c_void, len: *mut u32) -> i32 {
    if value.is_null() || len.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let cred;
    let pid;
    let answer: &[u8] = match name {
        LOCAL_PEERCRED | LOCAL_PEERPID | LOCAL_PEEREPID => {
            let Some(peer) = super::socket::peer_ids(fd) else {
                return -1;
            };
            if name == LOCAL_PEERCRED {
                let mut value = Xucred {
                    version: 0,
                    uid: peer.euid,
                    ngroups: 0,
                    _pad: 0,
                    groups: [0; 16],
                };
                // The effective gid leads, as a Mac's `cr_groups[0]` does.
                let list = std::iter::once(peer.egid)
                    .chain(peer.groups.iter().copied().filter(|g| *g != peer.egid));
                for (slot, gid) in value.groups.iter_mut().zip(list) {
                    *slot = gid;
                    value.ngroups += 1;
                }
                cred = value;
                cred.as_bytes()
            } else {
                let Some(found) = peer.pid else {
                    crate::errno::set(libc::ENOTCONN);
                    return -1;
                };
                pid = found.to_ne_bytes();
                &pid
            }
        }
        _ => {
            say_unknown_option(SOL_LOCAL, name);
            crate::errno::set(libc::ENOPROTOOPT);
            return -1;
        }
    };
    // SAFETY: `len` is the caller's; a short buffer gets a short answer, as
    // `getsockopt` gives.
    unsafe {
        let room = *len as usize;
        core::ptr::copy_nonoverlapping(answer.as_ptr(), value.cast::<u8>(), answer.len().min(room));
        *len = answer.len() as u32;
    }
    0
}

/// Whether `fd` is a Unix-domain socket.
///
/// `SOL_LOCAL` and `IPPROTO_IP` are both level 0; Darwin tells them apart by
/// the socket's domain, so `IP_MULTICAST_IF` on an inet socket is not
/// `SOL_LOCAL`'s option 9.
fn is_local_socket(fd: i32) -> bool {
    // SAFETY: zero is a valid `sockaddr_storage`.
    let mut addr: libc::sockaddr_storage = unsafe { core::mem::zeroed() };
    let mut len = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: a `sockaddr_storage` this function owns, with its own size.
    let rc = unsafe { libc::getsockname(fd, (&raw mut addr).cast(), &mut len) };
    rc == 0 && i32::from(addr.ss_family) == libc::AF_UNIX
}

/// `int getsockopt(int, int, int, void *, socklen_t *)`
///
/// # Safety
///
/// `value` is `*len` writable bytes and `len` is a writable `socklen_t`.
unsafe extern "C" fn getsockopt(
    fd: i32,
    level: i32,
    name: i32,
    value: *mut c_void,
    len: *mut u32,
) -> i32 {
    if nosigpipe(level, name) {
        // SAFETY: the caller's buffer and length slot.
        return unsafe { get_nosigpipe(fd, value, len) };
    }
    if is_dontfrag(level, name) {
        // SAFETY: the caller's buffer and length slot.
        return unsafe { get_dontfrag(fd, level, value, len) };
    }
    if level == SOL_SOCKET && matches!(name, so::NREAD | so::NWRITE) {
        // SAFETY: the caller's buffer and length slot.
        return unsafe { queued_bytes(fd, name, value, len) };
    }
    if level == SOL_LOCAL && is_local_socket(fd) {
        // SAFETY: the caller's buffer and length slot.
        return unsafe { local_option(fd, name, value, len) };
    }
    let Some((host_level, host_name)) = option_to_host(level, name) else {
        crate::errno::set(libc::ENOPROTOOPT);
        return -1;
    };
    if is_timeval(level, name) {
        if value.is_null() || len.is_null() {
            crate::errno::set(libc::EFAULT);
            return -1;
        }
        let mut host = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut host_len = core::mem::size_of::<libc::timeval>() as u32;
        // SAFETY: a `timeval` this function owns, with its own length beside it.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                host_level,
                host_name,
                (&raw mut host).cast(),
                &mut host_len,
            )
        };
        if rc == 0 {
            // SAFETY: the caller's buffer, which must be Darwin's sixteen bytes.
            unsafe {
                if (*len as usize) < 16 {
                    crate::errno::set(libc::EINVAL);
                    return -1;
                }
                crate::clock::Timeval::from_host(host).write(value.cast());
                *len = 16;
            }
        }
        return rc;
    }
    // SAFETY: the caller's buffer and length slot.
    let rc = unsafe { libc::getsockopt(fd, host_level, host_name, value, len) };
    if rc == 0 && is_pending_error(level, name, len) {
        // SAFETY: `len` said the buffer holds at least an `int`, checked above.
        unsafe {
            let host = value.cast::<i32>().read_unaligned();
            value
                .cast::<i32>()
                .write_unaligned(crate::errno::from_host(host));
        }
    }
    rc
}

/// Whether this is `SO_ERROR`, whose *value* is an `errno` in the host's
/// numbering and has to be translated like any other.
///
/// The one option whose payload is not a flag, a size or a structure. Left
/// untranslated it is the `errno` collision in its most misleading form: a guest
/// asking a non-blocking `connect` how it went reads `EINPROGRESS` (115 here) as
/// Darwin's 115, which is nothing, or `ECONNREFUSED` (111) as Darwin's 111,
/// which is nothing either - so a failed connection reports an error the guest
/// has no name for rather than the one that happened.
fn is_pending_error(level: i32, name: i32, len: *const u32) -> bool {
    // SAFETY: `len` is the caller's length slot, already known non-null here.
    level == SOL_SOCKET
        && name == so::ERROR
        && unsafe { *len as usize >= core::mem::size_of::<i32>() }
}

exports::exports! {
    functions {
        "setsockopt" => setsockopt,
        "getsockopt" => getsockopt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The level itself collides: Darwin's `SOL_SOCKET` passed through names an
    /// IP protocol here, and this host's names one on Darwin.
    #[test]
    fn a_unix_socket_peer_is_named_by_its_credentials() {
        use std::os::fd::AsRawFd;
        let (one, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        // SAFETY: `getuid` has no preconditions.
        let uid = unsafe { libc::getuid() };
        let mut cred = [0u8; size_of::<Xucred>()];
        let mut len = cred.len() as u32;
        // SAFETY: a buffer and its length.
        let rc = unsafe {
            local_option(
                one.as_raw_fd(),
                LOCAL_PEERCRED,
                cred.as_mut_ptr().cast(),
                &mut len,
            )
        };
        assert_eq!((rc, len as usize), (0, size_of::<Xucred>()));
        assert_eq!(u32::from_ne_bytes(cred[4..8].try_into().unwrap()), uid);
        assert!(i16::from_ne_bytes(cred[8..10].try_into().unwrap()) >= 1);

        let mut pid = [0u8; 4];
        let mut len = 4;
        // SAFETY: as above.
        let rc = unsafe {
            local_option(
                one.as_raw_fd(),
                LOCAL_PEERPID,
                pid.as_mut_ptr().cast(),
                &mut len,
            )
        };
        assert_eq!(rc, 0);
        assert_eq!(i32::from_ne_bytes(pid), std::process::id() as i32);
    }

    #[test]
    fn the_level_is_translated_rather_than_passed_through() {
        assert_ne!(SOL_SOCKET, libc::SOL_SOCKET);
        assert_eq!(
            option_to_host(SOL_SOCKET, so::REUSEADDR),
            Some((libc::SOL_SOCKET, libc::SO_REUSEADDR))
        );
    }

    /// Darwin's `SO_REUSEADDR` is this host's `SO_ERROR` - a real option, so
    /// passing it through succeeds and reads or writes something else.
    #[test]
    fn a_boolean_option_is_not_the_option_the_guest_named() {
        assert_eq!(so::REUSEADDR, libc::SO_ERROR);
        assert_eq!(so::KEEPALIVE, libc::SO_RCVBUF, "Darwin's SO_KEEPALIVE");
    }

    #[test]
    fn an_option_this_module_has_not_been_taught_is_refused() {
        assert_eq!(option_to_host(SOL_SOCKET, 0x4242), None);
        assert_eq!(option_to_host(1234, 1), None);
    }

    /// The IPv6 numbering is the one place a table entry aimed at the wrong
    /// real option rather than at nothing: Darwin's 36 is the traffic class and
    /// this host's 36 is not, so a guest setting it once set the hop limit.
    #[test]
    fn the_ipv6_options_are_the_rfc_numbers_and_not_this_hosts() {
        assert_eq!(
            option_to_host(IPPROTO_IPV6, ipv6::TCLASS),
            Some((IPPROTO_IPV6, libc::IPV6_TCLASS))
        );
        assert_eq!(
            option_to_host(IPPROTO_IPV6, ipv6::UNICAST_HOPS),
            Some((IPPROTO_IPV6, libc::IPV6_UNICAST_HOPS))
        );
        assert_ne!(
            ipv6::UNICAST_HOPS,
            libc::IPV6_UNICAST_HOPS,
            "the collision being avoided"
        );
        // Darwin's `IP_PKTINFO` is 26; 27 there is `IP_RECVTOS`. A host with no
        // counterpart refuses the option.
        assert_eq!(
            option_to_host(IPPROTO_IP, ip::PKTINFO),
            host::IP_PKTINFO.map(|name| (IPPROTO_IP, name))
        );
    }

    /// `TCP_NOPUSH` is corking, which is the closest thing this host has, and
    /// is mapped rather than refused - a refused write is worse than a delayed
    /// one. See [`TCP_OPTIONS`].
    #[test]
    fn tcp_nopush_corks_rather_than_being_refused() {
        assert_eq!(
            option_to_host(IPPROTO_TCP, tcp::NOPUSH),
            Some((IPPROTO_TCP, host::TCP_NOPUSH))
        );
    }

    /// `SO_NREAD` is not a `getsockopt` name on this host, and the question it
    /// asks still has an answer: five bytes written into a socket pair are five
    /// bytes waiting at the other end.
    #[test]
    fn the_bytes_waiting_are_reported_through_an_option_this_host_has_not_got() {
        let mut ends = [0 as std::ffi::c_int; 2];
        // SAFETY: a live array of two `int`.
        let rc =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, ends.as_mut_ptr()) };
        assert_eq!(rc, 0);
        // SAFETY: one end of a socket pair this test owns.
        assert_eq!(
            unsafe { libc::write(ends[1], c"hello".as_ptr().cast(), 5) },
            5
        );

        let mut waiting: i32 = 0;
        let mut len = 4u32;
        // SAFETY: four writable bytes and a live length.
        let rc = unsafe {
            getsockopt(
                ends[0],
                SOL_SOCKET,
                so::NREAD,
                (&raw mut waiting).cast(),
                &mut len,
            )
        };
        assert_eq!((rc, waiting, len), (0, 5, 4));

        // And it is read-only, as it is on Darwin.
        let value = 1i32;
        // SAFETY: four readable bytes, and the length says four.
        let rc =
            unsafe { setsockopt(ends[0], SOL_SOCKET, so::NREAD, (&raw const value).cast(), 4) };
        assert_eq!((rc, crate::errno::get()), (-1, libc::ENOPROTOOPT));
        crate::errno::set(0);

        // SAFETY: two descriptors this test opened and no longer uses.
        unsafe {
            libc::close(ends[0]);
            libc::close(ends[1]);
        }
    }

    /// Through the real stack: `SO_REUSEADDR` set in Darwin's spelling comes
    /// back set, in Darwin's spelling.
    #[test]
    fn a_socket_option_set_in_darwins_numbering_reads_back_set() {
        // SAFETY: a TCP socket of this host's own making.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0);

        let on: i32 = 1;
        // SAFETY: four readable bytes, and the length says four.
        let rc = unsafe { setsockopt(fd, SOL_SOCKET, so::REUSEADDR, (&raw const on).cast(), 4) };
        assert_eq!(rc, 0, "SO_REUSEADDR in Darwin's numbering");

        let mut back: i32 = 0;
        let mut len = 4u32;
        // SAFETY: four writable bytes and a live length.
        let rc = unsafe {
            getsockopt(
                fd,
                SOL_SOCKET,
                so::REUSEADDR,
                (&raw mut back).cast(),
                &mut len,
            )
        };
        assert_eq!(rc, 0);
        assert_ne!(back, 0, "the option the guest set is the one that is on");

        // SAFETY: a descriptor this test opened and no longer uses.
        unsafe { libc::close(fd) };
    }

    /// Level 0 is `IPPROTO_IP` on an inet socket and `SOL_LOCAL` on a Unix
    /// one, so the same name reaches a different option on each.
    #[test]
    fn level_zero_is_chosen_by_the_sockets_domain() {
        use std::os::fd::AsRawFd;
        // SAFETY: a UDP socket of this host's own making.
        let inet = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        assert!(inet >= 0);
        let mut interface = [0xffu8; 4];
        let mut len = 4u32;
        // SAFETY: four writable bytes and a live length.
        let rc = unsafe {
            getsockopt(
                inet,
                IPPROTO_IP,
                ip::MULTICAST_IF,
                interface.as_mut_ptr().cast(),
                &mut len,
            )
        };
        assert_eq!((rc, len, interface), (0, 4, [0; 4]), "INADDR_ANY");
        // SAFETY: a descriptor this test opened and no longer uses.
        unsafe { libc::close(inet) };

        let (local, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut pid = 0i32;
        let mut len = 4u32;
        // SAFETY: four writable bytes and a live length.
        let rc = unsafe {
            getsockopt(
                local.as_raw_fd(),
                SOL_LOCAL,
                LOCAL_PEERPID,
                (&raw mut pid).cast(),
                &mut len,
            )
        };
        assert_eq!((rc, pid), (0, std::process::id() as i32));
    }

    /// The timeout's `tv_usec` is thirty-two bits wide on Darwin and sixty-four
    /// here, so the value has to be rebuilt rather than handed along.
    #[test]
    fn a_timeout_survives_the_narrower_microseconds_field() {
        // SAFETY: a UDP socket of this host's own making.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        assert!(fd >= 0);

        // Darwin's sixteen bytes: eight of seconds, four of microseconds, four
        // of padding left deliberately non-zero.
        let mut guest = [0u8; 16];
        guest[0..8].copy_from_slice(&2i64.to_le_bytes());
        guest[8..12].copy_from_slice(&500_000i32.to_le_bytes());
        guest[12..16].copy_from_slice(&(-1i32).to_le_bytes());

        // SAFETY: sixteen readable bytes, and the length says sixteen.
        let rc = unsafe { setsockopt(fd, SOL_SOCKET, so::RCVTIMEO, guest.as_ptr().cast(), 16) };
        assert_eq!(rc, 0, "SO_RCVTIMEO in Darwin's numbering and shape");

        let mut back = [0u8; 16];
        let mut len = 16u32;
        // SAFETY: sixteen writable bytes and a live length.
        let rc = unsafe {
            getsockopt(
                fd,
                SOL_SOCKET,
                so::RCVTIMEO,
                back.as_mut_ptr().cast(),
                &mut len,
            )
        };
        assert_eq!(rc, 0);
        assert_eq!(i64::from_le_bytes(back[0..8].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(back[8..12].try_into().unwrap()), 500_000);

        // SAFETY: a descriptor this test opened and no longer uses.
        unsafe { libc::close(fd) };
    }

    /// `SO_NOSIGPIPE` is honoured per send rather than refused, and a
    /// descriptor number reused by a new socket does not inherit it.
    #[test]
    fn nosigpipe_is_recorded_read_back_and_forgotten_when_the_number_is_reused() {
        // SAFETY: a plain socket call with constant arguments.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0);

        let on = 1i32;
        // SAFETY: one readable `int`, and the length says four.
        let rc = unsafe { setsockopt(fd, SOL_SOCKET, so::NOSIGPIPE, (&raw const on).cast(), 4) };
        assert_eq!(rc, 0, "refusing this aborts Crashpad before it starts");
        assert!(suppresses_sigpipe(fd));

        let mut back = 0i32;
        let mut len = 4u32;
        // SAFETY: one writable `int` and a live length.
        let rc = unsafe {
            getsockopt(
                fd,
                SOL_SOCKET,
                so::NOSIGPIPE,
                (&raw mut back).cast(),
                &mut len,
            )
        };
        assert_eq!((rc, back, len), (0, 1, 4));

        forget(fd);
        assert!(!suppresses_sigpipe(fd));
        // SAFETY: a descriptor this test opened and no longer uses.
        unsafe { libc::close(fd) };
    }

    /// `IP_DONTFRAG` sets the DF bit on IPv4 sockets (translated to PMTUD on Linux)
    /// and reads back in Darwin's 0/1 boolean representation.
    #[test]
    fn ip_dontfrag_sets_and_reads_back() {
        // SAFETY: a UDP socket of this host's own making.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        assert!(fd >= 0);

        let on = 1i32;
        // SAFETY: four readable bytes, and the length says four.
        let rc = unsafe { setsockopt(fd, IPPROTO_IP, ip::DONTFRAG, (&raw const on).cast(), 4) };
        assert_eq!(rc, 0, "IP_DONTFRAG must succeed");

        let mut back = 0i32;
        let mut len = 4u32;
        // SAFETY: four writable bytes and a live length.
        let rc = unsafe {
            getsockopt(
                fd,
                IPPROTO_IP,
                ip::DONTFRAG,
                (&raw mut back).cast(),
                &mut len,
            )
        };
        assert_eq!((rc, back, len), (0, 1, 4));

        let off = 0i32;
        // SAFETY: four readable bytes, and the length says four.
        let rc = unsafe { setsockopt(fd, IPPROTO_IP, ip::DONTFRAG, (&raw const off).cast(), 4) };
        assert_eq!(rc, 0);

        let rc = unsafe {
            getsockopt(
                fd,
                IPPROTO_IP,
                ip::DONTFRAG,
                (&raw mut back).cast(),
                &mut len,
            )
        };
        assert_eq!((rc, back, len), (0, 0, 4));

        // SAFETY: a descriptor this test opened and no longer uses.
        unsafe { libc::close(fd) };
    }

    /// `IPV6_DONTFRAG` sets the DF bit on IPv6 sockets.
    #[test]
    fn ipv6_dontfrag_sets_and_reads_back() {
        // SAFETY: an IPv6 UDP socket of this host's own making.
        let fd = unsafe { libc::socket(libc::AF_INET6, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            // Host environment might not have IPv6 enabled.
            return;
        }

        let on = 1i32;
        // SAFETY: four readable bytes, and the length says four.
        let rc = unsafe { setsockopt(fd, IPPROTO_IPV6, ipv6::DONTFRAG, (&raw const on).cast(), 4) };
        assert_eq!(rc, 0, "IPV6_DONTFRAG must succeed");

        let mut back = 0i32;
        let mut len = 4u32;
        // SAFETY: four writable bytes and a live length.
        let rc = unsafe {
            getsockopt(
                fd,
                IPPROTO_IPV6,
                ipv6::DONTFRAG,
                (&raw mut back).cast(),
                &mut len,
            )
        };
        assert_eq!((rc, back, len), (0, 1, 4));

        // SAFETY: a descriptor this test opened and no longer uses.
        unsafe { libc::close(fd) };
    }
}
