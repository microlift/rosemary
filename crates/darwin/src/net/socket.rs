use std::ffi::c_void;

use super::addr::{self, HostAddr};
use super::host;

/// Darwin's `MSG_*`, from `<sys/socket.h>`.
mod msg {
    pub const OOB: i32 = 0x1;
    pub const PEEK: i32 = 0x2;
    pub const DONTROUTE: i32 = 0x4;
    pub const EOR: i32 = 0x8;
    pub const TRUNC: i32 = 0x10;
    pub const CTRUNC: i32 = 0x20;
    pub const WAITALL: i32 = 0x40;
    pub const DONTWAIT: i32 = 0x80;
    /// Half-close after the send; see [`super::half_close_if_asked`].
    pub const EOF: i32 = 0x100;
    pub const NOSIGNAL: i32 = 0x80000;
}

/// Each flag that means the same thing on both, as `(darwin, host)`.
///
/// `MSG_EOF` is not here and must not be: `0x100` here is `MSG_WAITALL`. It is
/// the one Darwin flag this host answers with a *second call* rather than with
/// a bit - see [`half_close_if_asked`] - so it is stripped on the way through
/// and acted on afterwards.
const FLAGS: &[(i32, i32)] = &[
    (msg::OOB, libc::MSG_OOB),
    (msg::PEEK, libc::MSG_PEEK),
    (msg::DONTROUTE, libc::MSG_DONTROUTE),
    (msg::EOR, libc::MSG_EOR),
    (msg::TRUNC, libc::MSG_TRUNC),
    (msg::CTRUNC, libc::MSG_CTRUNC),
    (msg::WAITALL, libc::MSG_WAITALL),
    (msg::DONTWAIT, libc::MSG_DONTWAIT),
    (msg::NOSIGNAL, super::host::MSG_NOSIGNAL),
];

fn flags_to_host(darwin: i32) -> i32 {
    let mut out = 0;
    for (theirs, ours) in FLAGS {
        if darwin & theirs != 0 {
            out |= ours;
        }
    }
    out
}

fn flags_from_host(host: i32) -> i32 {
    let mut out = 0;
    for (theirs, ours) in FLAGS {
        if host & ours != 0 {
            out |= theirs;
        }
    }
    out
}

/// Darwin's `SOCK_CLOEXEC`, which this host spells with a different bit.
const SOCK_CLOEXEC: i32 = 0x1000_0000;
/// Darwin's `SOCK_NONBLOCK`, likewise.
const SOCK_NONBLOCK: i32 = 0x2000_0000;

/// The guest's `type` argument as this host's.
///
/// The `SOCK_*` type numbers and the `IPPROTO_*` protocol numbers agree. The two
/// flag bits Darwin ORs into `type` do not, and they are the top of the word
/// where this host's are `0x80000`/`0x800` - so a guest asking for a
/// non-blocking socket in one call gets `EINVAL` from a host that has never seen
/// that bit. Anything else above the type is left alone, so a value neither
/// platform defines is refused by the kernel rather than quietly dropped here.
fn kind_to_host(darwin: i32) -> i32 {
    let mut host = darwin & !(SOCK_CLOEXEC | SOCK_NONBLOCK);
    if darwin & SOCK_CLOEXEC != 0 {
        host |= host::SOCK_CLOEXEC;
    }
    if darwin & SOCK_NONBLOCK != 0 {
        host |= host::SOCK_NONBLOCK;
    }
    host
}

/// `int socket(int domain, int type, int protocol)`
pub unsafe extern "C" fn socket(domain: i32, kind: i32, protocol: i32) -> i32 {
    let Some(domain) = addr::family_to_host(domain) else {
        crate::errno::set(libc::EAFNOSUPPORT);
        return -1;
    };
    // SAFETY: three integers; the call touches no caller memory.
    let fd = unsafe { libc::socket(domain, kind_to_host(kind), protocol) };
    super::sockopt::forget(fd);
    fd
}

/// `int socketpair(int domain, int type, int protocol, int sv[2])`
///
/// # Safety
///
/// `sv` is two writable `int`s.
pub unsafe extern "C" fn socketpair(domain: i32, kind: i32, protocol: i32, sv: *mut i32) -> i32 {
    let Some(domain) = addr::family_to_host(domain) else {
        crate::errno::set(libc::EAFNOSUPPORT);
        return -1;
    };
    // SAFETY: the caller's two-element array.
    let rc = unsafe { libc::socketpair(domain, kind_to_host(kind), protocol, sv) };
    if rc == 0 && !sv.is_null() {
        // SAFETY: two `int`s the call just filled in.
        unsafe {
            super::sockopt::forget(sv.read());
            super::sockopt::forget(sv.add(1).read());
        }
    }
    rc
}

/// The shape `bind` and `connect` share: an address in, nothing out.
///
/// # Safety
///
/// `address` is `len` readable bytes.
unsafe fn with_address(
    fd: i32,
    address: *const c_void,
    len: u32,
    call: unsafe extern "C" fn(i32, *const libc::sockaddr, libc::socklen_t) -> i32,
) -> i32 {
    let host = match unsafe { addr::to_host(address, len) } {
        Ok(host) => host,
        Err(errno) => {
            crate::errno::set(errno);
            return -1;
        }
    };
    // SAFETY: an address this function owns, with the length it was built with.
    unsafe { call(fd, host.as_ptr(), host.len()) }
}

/// `int bind(int, const struct sockaddr *, socklen_t)`
///
/// # Safety
///
/// `address` is `len` readable bytes.
unsafe extern "C" fn bind(fd: i32, address: *const c_void, len: u32) -> i32 {
    let host = match unsafe { addr::to_host(address, len) } {
        Ok(host) => host,
        Err(errno) => {
            crate::errno::set(errno);
            return -1;
        }
    };
    // SAFETY: an address this function owns, with the length it was built with.
    let rc = unsafe { libc::bind(fd, host.as_ptr(), host.len()) };
    if rc == 0
        && let Some(file) = host.unix_file()
    {
        crate::sockpath::bound(file);
    }
    rc
}

/// `int connect(int, const struct sockaddr *, socklen_t)`
///
/// # Safety
///
/// As [`bind`].
pub unsafe extern "C" fn connect(fd: i32, address: *const c_void, len: u32) -> i32 {
    // SAFETY: the caller's guarantee, passed along.
    unsafe { with_address(fd, address, len, libc::connect) }
}

/// The shape `accept`, `getsockname` and `getpeername` share: an address out.
///
/// # Safety
///
/// `address` is `*len` writable bytes and `len` a writable `socklen_t`, or both
/// are null.
unsafe fn returning_address(
    fd: i32,
    address: *mut c_void,
    len: *mut u32,
    call: unsafe extern "C" fn(i32, *mut libc::sockaddr, *mut libc::socklen_t) -> i32,
) -> i32 {
    if address.is_null() || len.is_null() {
        // A caller that does not want the address still wants the call.
        // SAFETY: null for both is what the host reads as "no address wanted".
        return unsafe { call(fd, core::ptr::null_mut(), core::ptr::null_mut()) };
    }
    let mut host = HostAddr::empty();
    // SAFETY: a buffer this function owns, with its own length beside it.
    let rc = unsafe { call(fd, host.as_mut_ptr(), host.len_mut()) };
    if rc < 0 {
        return rc;
    }
    // SAFETY: the caller's buffer and length slot.
    unsafe { addr::from_host(&host, address, len) };
    rc
}

/// `int accept(int, struct sockaddr *, socklen_t *)`
///
/// # Safety
///
/// As [`returning_address`].
unsafe extern "C" fn accept(fd: i32, address: *mut c_void, len: *mut u32) -> i32 {
    // SAFETY: the caller's guarantee, passed along.
    let accepted = unsafe { returning_address(fd, address, len, libc::accept) };
    super::sockopt::forget(accepted);
    accepted
}

/// `int getsockname(int, struct sockaddr *, socklen_t *)`
///
/// # Safety
///
/// As [`returning_address`].
unsafe extern "C" fn getsockname(fd: i32, address: *mut c_void, len: *mut u32) -> i32 {
    // SAFETY: the caller's guarantee, passed along.
    unsafe { returning_address(fd, address, len, libc::getsockname) }
}

/// `int getpeername(int, struct sockaddr *, socklen_t *)`
///
/// # Safety
///
/// As [`returning_address`].
unsafe extern "C" fn getpeername(fd: i32, address: *mut c_void, len: *mut u32) -> i32 {
    // SAFETY: the caller's guarantee, passed along.
    unsafe { returning_address(fd, address, len, libc::getpeername) }
}

/// Who is at the other end of a connected Unix socket, as a guest sees them.
pub(super) struct PeerIds {
    pub pid: Option<i32>,
    pub euid: u32,
    pub egid: u32,
    pub groups: Vec<u32>,
}

/// launchd runs as virtual root but only ever as the host user, and its
/// control socket answers peers whose uid equals its own. The user who owns
/// the prefix is that account's owner, so launchd sees them as itself; a Mac
/// would let that user's `launchctl` talk to their own launchd the same way.
///
/// FIXME (ACCURACY): a Mac reports the peer's real uid, 501 for a user's
/// `launchctl`, and MiniDarwin's launchd would refuse it. This answers root
/// instead so `launchctl` works unprivileged; remove it once launchd accepts
/// the prefix owner itself.
///
/// `host_uid` is the peer's real uid, and only a peer still running as it, not
/// one that became another virtual user, is answered this way.
fn as_launchd_owner(ids: PeerIds, host_uid: u32) -> PeerIds {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    // SAFETY: `getpid` and `getuid` take nothing and cannot fail.
    let (me, owner) = unsafe { (libc::getpid(), libc::getuid()) };
    if server::client::launchd() != Some(me) || host_uid != owner || ids.euid != owner {
        return ids;
    }
    let Some(launchd) = server::client::process(me) else {
        return ids;
    };
    PeerIds {
        euid: launchd.euid,
        egid: launchd.egid,
        groups: launchd.groups,
        ..ids
    }
}

/// The peer of `fd`, or `None` with `errno` set.
///
/// The peer is usually another guest, whose ids are the server's to say - a
/// guest root is the host's user - so a daemon that insists on a root peer is
/// answered the way a Mac would. A peer the server does not know is the host
/// user at the other end.
pub(super) fn peer_ids(fd: i32) -> Option<PeerIds> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    /// Names the guest's descriptor without owning it, or asserting it is valid.
    struct Raw(i32);
    impl std::os::fd::AsRawFd for Raw {
        fn as_raw_fd(&self) -> i32 {
            self.0
        }
    }
    // SAFETY: a query of a descriptor number.
    if fd < 0 || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        crate::errno::set(libc::EBADF);
        return None;
    }
    let Some(peer) = starnix::socket::peer(&Raw(fd)) else {
        // Why `fd` has no peer: not a socket, or not connected.
        let mut value = 0;
        let mut len = size_of::<i32>() as libc::socklen_t;
        // SAFETY: a local to receive the type.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut value).cast(),
                &raw mut len,
            )
        };
        crate::errno::set(if rc == 0 {
            libc::ENOTCONN
        } else {
            crate::errno::get()
        });
        return None;
    };
    Some(match peer.pid.and_then(server::client::process) {
        Some(process) => {
            let ids = PeerIds {
                pid: peer.pid,
                euid: process.euid,
                egid: process.egid,
                groups: process.groups,
            };
            as_launchd_owner(ids, peer.uid)
        }
        None => {
            // SAFETY: `getgid` takes nothing and cannot fail.
            let gid = unsafe { libc::getgid() };
            PeerIds {
                pid: peer.pid,
                euid: peer.uid,
                egid: gid,
                groups: vec![gid],
            }
        }
    })
}

/// `int getpeereid(int fd, uid_t *euid, gid_t *egid)`
///
/// # Safety
///
/// `euid` and `egid` are writable.
unsafe extern "C" fn getpeereid(fd: i32, euid: *mut u32, egid: *mut u32) -> i32 {
    let Some(peer) = peer_ids(fd) else {
        return -1;
    };
    // SAFETY: the caller's.
    unsafe {
        if !euid.is_null() {
            euid.write_unaligned(peer.euid);
        }
        if !egid.is_null() {
            egid.write_unaligned(peer.egid);
        }
    }
    0
}

/// `ssize_t send(int, const void *, size_t, int)`
///
/// # Safety
///
/// `buf` is `len` readable bytes.
unsafe extern "C" fn send(fd: i32, buf: *const c_void, len: usize, flags: i32) -> isize {
    // SAFETY: the caller's buffer, with the flags translated.
    let sent = unsafe { libc::send(fd, buf, len, sending_flags(fd, flags)) };
    half_close_if_asked(fd, flags, sent)
}

/// `flags` for a send on `fd`, with `MSG_NOSIGNAL` added when the guest asked
/// for `SO_NOSIGPIPE` on it. See [`super::sockopt::suppresses_sigpipe`].
fn sending_flags(fd: i32, darwin: i32) -> i32 {
    let mut host = flags_to_host(darwin);
    if super::sockopt::suppresses_sigpipe(fd) {
        host |= super::host::MSG_NOSIGNAL;
    }
    host
}

/// `ssize_t recv(int, void *, size_t, int)`
///
/// # Safety
///
/// `buf` is `len` writable bytes.
unsafe extern "C" fn recv(fd: i32, buf: *mut c_void, len: usize, flags: i32) -> isize {
    // SAFETY: the caller's buffer, with the flags translated.
    unsafe { libc::recv(fd, buf, len, flags_to_host(flags)) }
}

/// `ssize_t sendto(int, const void *, size_t, int, const struct sockaddr *, socklen_t)`
///
/// # Safety
///
/// `buf` is `len` readable bytes and `address` is `addrlen` readable bytes, or
/// `address` is null.
unsafe extern "C" fn sendto(
    fd: i32,
    buf: *const c_void,
    len: usize,
    flags: i32,
    address: *const c_void,
    addrlen: u32,
) -> isize {
    let host_flags = sending_flags(fd, flags);
    if address.is_null() {
        // SAFETY: a connected socket, which is what a null address means.
        let sent = unsafe { libc::sendto(fd, buf, len, host_flags, core::ptr::null(), 0) };
        return half_close_if_asked(fd, flags, sent);
    }
    let host = match unsafe { addr::to_host(address, addrlen) } {
        Ok(host) => host,
        Err(errno) => {
            crate::errno::set(errno);
            return -1;
        }
    };
    // SAFETY: the caller's buffer and an address this function owns.
    let sent = unsafe { libc::sendto(fd, buf, len, host_flags, host.as_ptr(), host.len()) };
    half_close_if_asked(fd, flags, sent)
}

/// `ssize_t recvfrom(int, void *, size_t, int, struct sockaddr *, socklen_t *)`
///
/// # Safety
///
/// `buf` is `len` writable bytes; `address` is `*len` writable bytes with `len`
/// a writable `socklen_t`, or both are null.
unsafe extern "C" fn recvfrom(
    fd: i32,
    buf: *mut c_void,
    len: usize,
    flags: i32,
    address: *mut c_void,
    addrlen: *mut u32,
) -> isize {
    let flags = flags_to_host(flags);
    if address.is_null() || addrlen.is_null() {
        // SAFETY: the caller's buffer; null for both is "the address is not
        // wanted", which is what the host reads it as too.
        return unsafe {
            libc::recvfrom(
                fd,
                buf,
                len,
                flags,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            )
        };
    }
    let mut host = HostAddr::empty();
    // SAFETY: the caller's buffer, and an address buffer this function owns.
    let rc = unsafe { libc::recvfrom(fd, buf, len, flags, host.as_mut_ptr(), host.len_mut()) };
    if rc >= 0 {
        // SAFETY: the caller's address buffer and length slot.
        unsafe { addr::from_host(&host, address, addrlen) };
    }
    rc
}

/// `struct msghdr` as *Darwin* lays it out: forty-eight bytes, with `msg_iovlen`
/// and `msg_controllen` as 32-bit values where this host has 64-bit ones.
///
/// Not the host's, which is fifty-six bytes. Passing one along would read the
/// iovec count out of the count *and* the padding after it, which is a length of
/// several billion.
#[repr(C)]
#[derive(Clone, Copy)]
struct MsgHdr {
    name: *mut c_void,
    namelen: u32,
    iov: *mut libc::iovec,
    iovlen: i32,
    control: *mut c_void,
    controllen: u32,
    flags: i32,
}

/// Say once that a control message was refused, and which one.
fn say_control_refused(refused: super::control::Untranslatable) {
    use std::sync::Mutex;
    static SAID: Mutex<Vec<super::control::Untranslatable>> = Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.contains(&refused) {
        return;
    }
    said.push(refused);
    let (level, kind) = (refused.level, refused.kind);
    eprintln!(
        "rosemary: sendmsg with ancillary data of level {level:#x} type {kind:#x}: refused - \
         the control message type or payload shape is unsupported; types can have \
         different numbers on the two platforms, so a payload it does not know the shape of \
         cannot be handed across unread"
    );
}

/// The two halves `sendmsg` and `recvmsg` share: a guest `msghdr` as the host's.
///
/// `Err(EOPNOTSUPP)` when the message carries ancillary data this host cannot
/// be given - see [`super::control`], which owns that decision and the
/// conversion behind it - and the errno for a name that does not convert.
/// `control` is the host buffer the returned
/// `msghdr` points into, so the caller has to keep it alive across the call.
///
/// # Safety
///
/// `guest` is a readable Darwin `struct msghdr`.
unsafe fn msghdr_to_host(
    guest: *const MsgHdr,
    name: &mut Option<HostAddr>,
    control: &mut Vec<u8>,
    sending: bool,
) -> Result<libc::msghdr, i32> {
    let g = unsafe { guest.read() };
    if sending && !g.control.is_null() && g.controllen != 0 {
        // SAFETY: the guest's control buffer, with the length it gave for it.
        match unsafe { super::control::to_host(g.control, g.controllen) } {
            Ok(host) => *control = host,
            Err(refused) => {
                say_control_refused(refused);
                return Err(libc::EOPNOTSUPP);
            }
        }
    }
    // SAFETY: as above; a null name is a message with no address.
    *name = match !sending || g.name.is_null() {
        true => None,
        false => Some(unsafe { addr::to_host(g.name, g.namelen) }?),
    };
    // SAFETY: zeroed is a valid `struct msghdr` - no name, no iovec, no
    // control - and every field that matters is written below.
    let mut host: libc::msghdr = unsafe { core::mem::zeroed() };
    host.msg_name = match name {
        Some(a) => a.as_mut_ptr().cast(),
        None => core::ptr::null_mut(),
    };
    host.msg_namelen = name.as_ref().map_or(0, |a| a.len());
    host.msg_iov = g.iov;
    host.msg_iovlen = g.iovlen.max(0) as _;
    // msg_flags is output-only; callers need not initialize it.
    if !control.is_empty() {
        host.msg_control = control.as_mut_ptr().cast();
        host.msg_controllen = control.len() as _;
    }
    Ok(host)
}

/// `ssize_t sendmsg(int, const struct msghdr *, int)`
///
/// # Safety
///
/// `message` is a readable Darwin `struct msghdr` whose `msg_iov` is
/// `msg_iovlen` readable `struct iovec`.
unsafe extern "C" fn sendmsg(fd: i32, message: *const c_void, flags: i32) -> isize {
    if message.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    if unsafe { (*(message as *const MsgHdr)).iovlen } < 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let mut name = None;
    let mut control = Vec::new();
    let host = match unsafe { msghdr_to_host(message.cast(), &mut name, &mut control, true) } {
        Ok(host) => host,
        Err(errno) => {
            crate::errno::set(errno);
            return -1;
        }
    };
    // SAFETY: a `msghdr` this function built, pointing at the caller's own
    // iovecs - which `struct iovec` has the same layout for on both platforms -
    // and at a control buffer that outlives the call.
    let sent = unsafe { libc::sendmsg(fd, &host, sending_flags(fd, flags)) };
    half_close_if_asked(fd, flags, sent)
}

/// `ssize_t recvmsg(int, struct msghdr *, int)`
///
/// # Safety
///
/// As [`sendmsg`], with the iovecs writable.
unsafe extern "C" fn recvmsg(fd: i32, message: *mut c_void, flags: i32) -> isize {
    if message.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let guest: *mut MsgHdr = message.cast();
    if unsafe { (*guest).iovlen } < 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let mut name = None;
    let mut sent_control = Vec::new();
    let mut host = match unsafe { msghdr_to_host(guest, &mut name, &mut sent_control, false) } {
        Ok(host) => host,
        Err(errno) => {
            crate::errno::set(errno);
            return -1;
        }
    };
    // The name is written by the kernel, so it needs room rather than a copy of
    // what the guest had there.
    let mut received = HostAddr::empty();
    // SAFETY: the caller's own `msghdr`, read for its name and control buffers.
    let (guest_name, guest_namelen, guest_control, guest_controllen) = unsafe {
        (
            (*guest).name,
            (*guest).namelen,
            (*guest).control,
            (*guest).controllen,
        )
    };
    if !guest_name.is_null() {
        host.msg_name = received.as_mut_ptr().cast();
        host.msg_namelen = received.len();
    }
    // As with the name: the kernel fills the control buffer in, and what it
    // writes there is wider than what the guest reserved room for.
    let mut control = vec![0u8; super::control::host_capacity(guest_controllen)];
    if guest_control.is_null() || guest_controllen == 0 {
        host.msg_control = core::ptr::null_mut();
        host.msg_controllen = 0;
    } else {
        host.msg_control = control.as_mut_ptr().cast();
        host.msg_controllen = control.len() as _;
    }

    // SAFETY: a `msghdr` this function built over the caller's iovecs.
    let rc = unsafe { libc::recvmsg(fd, &mut host, flags_to_host(flags)) };
    if rc < 0 {
        return rc;
    }
    if !guest_name.is_null() {
        let mut len = guest_namelen;
        // The kernel reports the address's length in `msg_namelen` rather than
        // beside the bytes, so it is moved into the buffer's own length before
        // the conversion reads it. The bytes are already there - `msg_name` is
        // this buffer - so nothing is copied.
        // SAFETY: the length slot of a buffer this function owns.
        unsafe { *received.len_mut() = host.msg_namelen.min(addr::STORAGE as u32) };
        // SAFETY: the caller's name buffer, with the length they reserved.
        unsafe { addr::from_host(&received, guest_name, &mut len) };
        // SAFETY: the caller's own `msghdr`.
        unsafe { (*guest).namelen = len };
    }
    let mut received_flags = flags_from_host(host.msg_flags);
    if !guest_control.is_null() && guest_controllen != 0 {
        // SAFETY: a `msghdr` the kernel has just filled in, and the caller's
        // control buffer with the length they reserved.
        let (written, dropped) =
            unsafe { super::control::from_host(&host, guest_control, guest_controllen) };
        // SAFETY: the caller's own `msghdr`.
        unsafe { (*guest).controllen = written };
        if dropped {
            received_flags |= msg::CTRUNC;
        }
    }
    // SAFETY: the caller's own `msghdr`.
    unsafe { (*guest).flags = received_flags };
    rc
}

/// Darwin's `MSG_EOF`: send, then half-close.
///
/// The bit asks for the send and a `shutdown(SHUT_WR)` after it, which is how
/// Darwin's `send` ends a request without a second call. There is no host flag
/// for it and there was no host meaning either - `0x100` here is `MSG_WAITALL`,
/// so passing it through made a send wait - so it is stripped by
/// [`flags_to_host`] and answered here instead.
///
/// A failed send is not half-closed: the caller will retry it.
fn half_close_if_asked(fd: i32, darwin_flags: i32, sent: isize) -> isize {
    if sent >= 0 && darwin_flags & msg::EOF != 0 {
        // SAFETY: a descriptor and a constant; the call touches no memory.
        unsafe { libc::shutdown(fd, libc::SHUT_WR) };
    }
    sent
}

/// `ssize_t write(int, const void *, size_t)`
///
/// Here, and not passed through, for one reason: `SO_NOSIGPIPE`. Darwin's is a
/// property of the socket and this host's `MSG_NOSIGNAL` is a property of the
/// call, so a guest that set the option and then wrote to a closed socket with
/// `write` - which takes no flags - still took a `SIGPIPE` and died. A write to
/// a descriptor the guest set the option on is sent instead, which is the same
/// call for a socket and is where the flag can be given.
///
/// A process with no suppressed socket pays one relaxed load per write. A
/// descriptor recorded as suppressed is also checked with `fstat`, because
/// passed-through descriptor operations can reuse its number for a file; see
/// [`super::sockopt::suppresses_sigpipe`].
///
/// # Safety
///
/// `buf` is `len` readable bytes.
pub(crate) unsafe extern "C" fn write(fd: i32, buf: *const c_void, len: usize) -> isize {
    if super::sockopt::suppresses_sigpipe(fd) {
        let previous_errno = crate::errno::get();
        // SAFETY: the caller's buffer. A `send` with no flags is a `write` on
        // a socket.
        let sent = unsafe { libc::send(fd, buf, len, super::host::MSG_NOSIGNAL) };
        if sent >= 0 || crate::errno::get() != libc::ENOTSOCK {
            return sent;
        }
        // The descriptor may have been replaced after the file-type check. The
        // failed send touched no data; discard the stale socket state and make
        // the write the caller actually requested. Preserve errno if that
        // write succeeds.
        super::sockopt::forget(fd);
        crate::errno::set(previous_errno);
    }
    // SAFETY: the caller's buffer.
    unsafe { libc::write(fd, buf, len) }
}

/// `ssize_t writev(int, const struct iovec *, int)`
///
/// As [`write`]: the gathered form needs the same flag and has the same
/// nowhere to put it, so it becomes a `sendmsg` when the option is set.
///
/// # Safety
///
/// `iov` is `iovcnt` readable `struct iovec`.
unsafe extern "C" fn writev(fd: i32, iov: *const libc::iovec, iovcnt: i32) -> isize {
    if iovcnt < 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if super::sockopt::suppresses_sigpipe(fd) {
        let previous_errno = crate::errno::get();
        // SAFETY: zeroed is a valid `msghdr` - no name, no control - and the
        // two fields that matter are the caller's own iovecs.
        let mut host: libc::msghdr = unsafe { core::mem::zeroed() };
        host.msg_iov = iov.cast_mut();
        host.msg_iovlen = iovcnt.max(0) as _;
        // SAFETY: a `msghdr` this function built over the caller's iovecs.
        let sent = unsafe { libc::sendmsg(fd, &host, super::host::MSG_NOSIGNAL) };
        if sent >= 0 || crate::errno::get() != libc::ENOTSOCK {
            return sent;
        }
        // As `write`: a non-socket result identifies stale per-descriptor
        // state, and sendmsg cannot have consumed any of the iovecs.
        super::sockopt::forget(fd);
        crate::errno::set(previous_errno);
    }
    // SAFETY: the caller's iovecs, whose layout is the same on both platforms.
    unsafe { libc::writev(fd, iov, iovcnt) }
}

/// Darwin's optional header and trailer around a `sendfile` transfer.
///
/// `struct iovec` itself has the same layout on both platforms; only the
/// structure collecting the two arrays is Darwin-specific.
#[repr(C)]
#[derive(Clone, Copy)]
struct SendfileHdtr {
    headers: *const libc::iovec,
    header_count: i32,
    trailers: *const libc::iovec,
    trailer_count: i32,
}

/// Send an iovec array in order, adding its bytes to Darwin's value-result
/// count. An error after a partial write remains an error; Darwin reports the
/// partial byte count through `len` alongside it.
unsafe fn sendfile_iovecs(
    socket: i32,
    iov: *const libc::iovec,
    count: i32,
    limit: Option<u64>,
    sent: &mut i64,
) -> Result<(), ()> {
    if count < 0 || (count != 0 && iov.is_null()) {
        crate::errno::set(libc::EINVAL);
        return Err(());
    }

    let mut left = limit.unwrap_or(u64::MAX);
    for index in 0..count as usize {
        // SAFETY: the caller promises an array containing `count` readable
        // iovecs.
        let part = unsafe { iov.add(index).read_unaligned() };
        let mut at = part.iov_base.cast::<u8>();
        let mut remaining = part.iov_len.min(left as usize);
        while remaining != 0 {
            let flags = if super::sockopt::suppresses_sigpipe(socket) {
                super::host::MSG_NOSIGNAL
            } else {
                0
            };
            // SAFETY: `at` names `remaining` readable bytes from this iovec.
            let wrote = unsafe { libc::send(socket, at.cast(), remaining, flags) };
            if wrote < 0 {
                return Err(());
            }
            if wrote == 0 {
                crate::errno::set(libc::EAGAIN);
                return Err(());
            }
            let wrote = wrote as usize;
            *sent = sent.saturating_add(wrote as i64);
            left -= wrote as u64;
            remaining -= wrote;
            // SAFETY: `wrote` did not exceed `remaining` before subtraction.
            at = unsafe { at.add(wrote) };
        }
        if left == 0 {
            break;
        }
    }
    Ok(())
}

/// `int sendfile(int fd, int socket, off_t offset, off_t *len,
/// struct sf_hdtr *hdtr, int flags)`
///
/// Linux's function has four arguments in a different order and returns the
/// byte count directly; FreeBSD's has seven and reports the count through an
/// out-parameter of its own. Darwin returns zero or minus one and makes `len` a
/// value-result parameter. Its count includes headers and trailers, and a
/// zero input count means through end-of-file.
///
/// # Safety
///
/// `len` is a readable and writable Darwin `off_t`; `hdtr`, when non-null,
/// describes readable arrays of readable iovecs.
unsafe extern "C" fn sendfile(
    file: i32,
    socket: i32,
    offset: i64,
    len: *mut i64,
    hdtr: *const SendfileHdtr,
    flags: i32,
) -> i32 {
    if len.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: the caller's value-result slot.
    let requested = unsafe { len.read_unaligned() };
    // From here on every failure reports that no bytes have yet been sent.
    unsafe { len.write_unaligned(0) };
    if requested < 0 || offset < 0 || flags != 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }

    // Darwin requires the output descriptor to be a socket. Modern Linux also
    // accepts a regular file, so letting the host call decide would turn a
    // Darwin ENOTSOCK failure into a successful copy.
    let mut socket_type = 0i32;
    let mut socket_type_len = core::mem::size_of_val(&socket_type) as libc::socklen_t;
    // SAFETY: two live output slots and a descriptor supplied by the caller.
    if unsafe {
        libc::getsockopt(
            socket,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut socket_type).cast(),
            &raw mut socket_type_len,
        )
    } < 0
    {
        return -1;
    }

    // SAFETY: the caller's optional structure.
    let hdtr = match hdtr.is_null() {
        true => None,
        false => Some(unsafe { hdtr.read_unaligned() }),
    };
    let mut sent = 0i64;
    let finite = (requested != 0).then_some(requested as u64);
    if let Some(parts) = hdtr {
        // Headers count against a nonzero input limit.
        if unsafe { sendfile_iovecs(socket, parts.headers, parts.header_count, finite, &mut sent) }
            .is_err()
        {
            unsafe { len.write_unaligned(sent) };
            return -1;
        }
    }

    let header_bytes = sent as u64;
    let mut file_left = finite.map(|total| total.saturating_sub(header_bytes));
    let mut host_offset = offset as libc::off_t;
    loop {
        let count = match file_left {
            Some(0) => break,
            Some(left) => left.min(0x7fff_f000) as usize,
            None => 0x7fff_f000,
        };
        // SAFETY: a local offset slot; the kernel validates both descriptors.
        let wrote = unsafe { host::sendfile(file, socket, &mut host_offset, count) };
        if wrote < 0 {
            unsafe { len.write_unaligned(sent) };
            return -1;
        }
        if wrote == 0 {
            break;
        }
        sent = sent.saturating_add(wrote as i64);
        if let Some(left) = &mut file_left {
            *left -= wrote as u64;
        }
    }

    if let Some(parts) = hdtr {
        // Trailers do not count against Darwin's input limit and are sent after
        // the selected file region even when the headers exhausted that limit.
        if unsafe { sendfile_iovecs(socket, parts.trailers, parts.trailer_count, None, &mut sent) }
            .is_err()
        {
            unsafe { len.write_unaligned(sent) };
            return -1;
        }
    }

    unsafe { len.write_unaligned(sent) };
    0
}

/// Darwin's `sa_endpoints_t`, the argument that makes `connectx` more than a
/// `connect`: a source and a destination in one call.
#[repr(C)]
#[derive(Clone, Copy)]
struct Endpoints {
    srcif: u32,
    srcaddr: *const c_void,
    srcaddrlen: u32,
    dstaddr: *const c_void,
    dstaddrlen: u32,
}

/// The `sae_connid_t` Darwin's TCP and UDP number their one connection with.
const SAE_CONNID_ONLY: u32 = 1;

/// `SAE_ASSOCID_ANY` and `SAE_ASSOCID_ALL`, the only associations a socket
/// that is not multipath has.
const SAE_ASSOCID_ANY: u32 = 0;
const SAE_ASSOCID_ALL: u32 = u32::MAX;

/// Implements `connectx` with `bind`, `connect`, and `writev`. Refuses a source
/// interface because binding to a different interface would be incorrect.
///
/// # Safety
///
/// `endpoints` is a readable `sa_endpoints_t` whose addresses are readable at
/// their stated lengths; `iov` is `iovcnt` readable `struct iovec`; `len` and
/// `connid` are writable or null.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn connectx(
    fd: i32,
    endpoints: *const Endpoints,
    _associd: u32,
    _flags: u32,
    iov: *const libc::iovec,
    iovcnt: u32,
    len: *mut usize,
    connid: *mut u32,
) -> i32 {
    if endpoints.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let ends = unsafe { endpoints.read() };
    if ends.srcif != 0 {
        crate::errno::set(libc::ENOPROTOOPT);
        return -1;
    }
    if !ends.srcaddr.is_null() {
        // SAFETY: the caller's source address, at the length they gave.
        let rc = unsafe { with_address(fd, ends.srcaddr, ends.srcaddrlen, libc::bind) };
        if rc < 0 {
            return rc;
        }
    }
    // SAFETY: the caller's destination address, at the length they gave.
    let rc = unsafe { with_address(fd, ends.dstaddr, ends.dstaddrlen, libc::connect) };
    if rc < 0 {
        return rc;
    }
    if !connid.is_null() {
        // SAFETY: the caller's slot.
        unsafe { connid.write(SAE_CONNID_ONLY) };
    }
    if iov.is_null() || iovcnt == 0 {
        return 0;
    }
    // SAFETY: the caller's iovecs, whose layout is the same on both platforms.
    let written = unsafe { libc::writev(fd, iov, iovcnt as i32) };
    if written < 0 {
        // The connection stands; only the data did not go. Darwin reports this
        // the same way - the descriptor is connected and `errno` says why the
        // payload did not follow.
        return -1;
    }
    if !len.is_null() {
        // SAFETY: the caller's slot.
        unsafe { len.write(written as usize) };
    }
    0
}

/// `int disconnectx(int s, sae_associd_t aid, sae_connid_t cid)`
///
/// Darwin's TCP and UDP answer it, ignoring `cid` because each has only one
/// connection; every other protocol refuses it.
///
/// FIXME (ACCURACY): a TCP disconnect is a `shutdown`, which sends a FIN where
/// Darwin resets a connection with unread data or a zero linger, and leaves
/// unread data to be received where Darwin discards it. A UDP disconnect keeps
/// an address the socket was explicitly bound to where Darwin clears it.
pub unsafe extern "C" fn disconnectx(fd: i32, aid: u32, _cid: u32) -> i32 {
    let Some((family, kind)) = family_and_type(fd) else {
        return -1;
    };
    if !matches!(family, libc::AF_INET | libc::AF_INET6)
        || !matches!(kind, libc::SOCK_STREAM | libc::SOCK_DGRAM)
    {
        crate::errno::set_guest(crate::errno::DARWIN_EOPNOTSUPP);
        return -1;
    }
    if aid != SAE_ASSOCID_ANY && aid != SAE_ASSOCID_ALL {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if kind == libc::SOCK_STREAM {
        // SAFETY: a call on a descriptor number.
        if unsafe { libc::shutdown(fd, libc::SHUT_RDWR) } < 0 {
            // Darwin closes a connection that never was without complaint.
            return match crate::errno::get() {
                libc::ENOTCONN => 0,
                _ => -1,
            };
        }
        return 0;
    }
    // SAFETY: a zeroed `sockaddr_storage` is a valid empty address.
    let mut peer: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = size_of_val(&peer) as libc::socklen_t;
    // SAFETY: a local to receive the address.
    if unsafe { libc::getpeername(fd, (&raw mut peer).cast(), &raw mut len) } < 0 {
        return -1;
    }
    // SAFETY: as above.
    let mut unspecified: libc::sockaddr = unsafe { std::mem::zeroed() };
    unspecified.sa_family = libc::AF_UNSPEC as libc::sa_family_t;
    // SAFETY: an address this function owns, at its own length.
    unsafe {
        libc::connect(
            fd,
            &raw const unspecified,
            size_of_val(&unspecified) as libc::socklen_t,
        )
    }
}

/// The host address family and `SOCK_*` type of socket `fd`, or `None` with
/// `errno` set.
fn family_and_type(fd: i32) -> Option<(i32, i32)> {
    // SAFETY: a zeroed `sockaddr_storage` is a valid empty address.
    let mut address: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut len = size_of_val(&address) as libc::socklen_t;
    // SAFETY: a local to receive the address.
    if unsafe { libc::getsockname(fd, (&raw mut address).cast(), &raw mut len) } < 0 {
        return None;
    }
    let mut kind = 0i32;
    let mut len = size_of::<i32>() as libc::socklen_t;
    // SAFETY: a local to receive the type.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut len,
        )
    };
    (rc == 0).then_some((i32::from(address.ss_family), kind))
}

// Not in the `libc` crate on any platform, so declared here. Both take an
// address family, which is the only reason they are wrapped at all.
unsafe extern "C" {
    #[link_name = "inet_pton"]
    fn host_inet_pton(af: i32, src: *const libc::c_char, dst: *mut c_void) -> i32;
    #[link_name = "inet_ntop"]
    fn host_inet_ntop(
        af: i32,
        src: *const c_void,
        dst: *mut libc::c_char,
        size: u32,
    ) -> *const libc::c_char;
}

/// `int inet_pton(int af, const char *src, void *dst)`
///
/// # Safety
///
/// `src` is a NUL-terminated string and `dst` is large enough for the family's
/// address - four bytes or sixteen.
unsafe extern "C" fn inet_pton(af: i32, src: *const libc::c_char, dst: *mut c_void) -> i32 {
    let Some(af) = addr::family_to_host(af) else {
        crate::errno::set(libc::EAFNOSUPPORT);
        return -1;
    };
    // SAFETY: the caller's string and buffer.
    unsafe { host_inet_pton(af, src, dst) }
}

/// `const char *inet_ntop(int af, const void *src, char *dst, socklen_t size)`
///
/// # Safety
///
/// `src` is the family's address and `dst` is `size` writable bytes.
unsafe extern "C" fn inet_ntop(
    af: i32,
    src: *const c_void,
    dst: *mut libc::c_char,
    size: u32,
) -> *const libc::c_char {
    let Some(af) = addr::family_to_host(af) else {
        crate::errno::set(libc::EAFNOSUPPORT);
        return core::ptr::null();
    };
    // SAFETY: the caller's address and buffer.
    unsafe { host_inet_ntop(af, src, dst, size) }
}

exports::exports! {
    functions {
        "write" => write,
        "writev" => writev,
        "sendfile" => sendfile,
        "connectx" => connectx,
        "disconnectx" => disconnectx,
        "socket" => socket,
        "socketpair" => socketpair,
        "bind" => bind,
        "connect" => connect,
        "accept" => accept,
        "getsockname" => getsockname,
        "getpeername" => getpeername,
        "getpeereid" => getpeereid,
        "send" => send,
        "recv" => recv,
        "sendto" => sendto,
        "recvfrom" => recvfrom,
        "sendmsg" => sendmsg,
        "recvmsg" => recvmsg,
        "inet_pton" => inet_pton,
        "inet_ntop" => inet_ntop,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn getpeereid_names_this_user_on_a_socket_pair() {
        let (one, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let (mut uid, mut gid) = (u32::MAX, u32::MAX);
        use std::os::fd::AsRawFd;
        // SAFETY: two writable ids.
        let rc = unsafe { super::getpeereid(one.as_raw_fd(), &mut uid, &mut gid) };
        assert_eq!(rc, 0);
        // SAFETY: `getuid` has no preconditions.
        assert_eq!(uid, unsafe { libc::getuid() });
    }

    #[test]
    fn getpeereid_refuses_a_descriptor_that_is_not_a_socket() {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: two writable ids; fd 0 is not a connected socket.
        let rc = unsafe { super::getpeereid(-1, &mut uid, &mut gid) };
        assert_eq!(rc, -1);
    }

    #[test]
    fn receiving_does_not_parse_output_buffers() {
        let mut bytes = [0xffu8; 32];
        let guest = super::MsgHdr {
            name: bytes.as_mut_ptr().cast(),
            namelen: bytes.len() as u32,
            iov: core::ptr::null_mut(),
            iovlen: 0,
            control: bytes.as_mut_ptr().cast(),
            controllen: bytes.len() as u32,
            flags: 0,
        };
        let mut name = None;
        let mut control = Vec::new();
        // SAFETY: guest is initialized and its output buffers are writable.
        let host = unsafe { super::msghdr_to_host(&guest, &mut name, &mut control, false) };
        assert!(host.is_ok());
        assert!(name.is_none());
        assert!(control.is_empty());
    }

    use super::*;

    /// The two that collide, named: `MSG_WAITALL` passed through asks this host
    /// not to wait at all, and `MSG_EOR` asks about truncated control data.
    #[test]
    fn the_message_flags_are_translated_rather_than_passed_through() {
        assert_eq!(msg::WAITALL, libc::MSG_DONTWAIT);
        assert_eq!(flags_to_host(msg::WAITALL), libc::MSG_WAITALL);
        assert_eq!(msg::EOR, libc::MSG_CTRUNC);
        assert_eq!(flags_to_host(msg::EOR), libc::MSG_EOR);
    }

    #[test]
    fn every_shared_message_flag_round_trips() {
        for (darwin, _) in FLAGS {
            assert_eq!(flags_from_host(flags_to_host(*darwin)), *darwin);
        }
    }

    /// Darwin's `MSG_EOF` must not reach this host as a flag: `0x100` here is
    /// `MSG_WAITALL`, so passing it through would make a send wait. It is
    /// answered with a `shutdown` after the send instead, which the round trip
    /// below checks end to end.
    #[test]
    fn msg_eof_is_not_handed_to_this_host_as_a_flag() {
        assert_eq!(msg::EOF, libc::MSG_WAITALL, "the collision being avoided");
        assert_eq!(flags_to_host(msg::EOF), 0);
    }

    /// A pair of connected sockets, which is what the send-side tests need.
    fn pair() -> (i32, i32) {
        let mut ends = [0i32; 2];
        // SAFETY: a live array of two `int`.
        let rc =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, ends.as_mut_ptr()) };
        assert_eq!(rc, 0);
        (ends[0], ends[1])
    }

    fn memory_file(bytes: &[u8]) -> i32 {
        use std::io::Write;
        use std::os::fd::IntoRawFd;
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "darwin-sendfile-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut file = std::fs::File::create_new(&path).unwrap();
        // Unlinked at once, as an anonymous file is: nothing is left behind.
        std::fs::remove_file(&path).unwrap();
        file.write_all(bytes).unwrap();
        file.into_raw_fd()
    }

    /// A regular file whose number was most recently used by a socket carrying
    /// `SO_NOSIGPIPE`. This is the replacement race the write path must still
    /// defend against even though descriptor operations are shimmed.
    fn file_with_stale_nosigpipe() -> i32 {
        let (socket, peer) = pair();
        let on = 1i32;
        // SAFETY: one readable `int`, and the length says four.
        assert_eq!(
            unsafe {
                crate::net::sockopt::setsockopt(socket, 0xffff, 0x1022, (&raw const on).cast(), 4)
            },
            0
        );

        let file = memory_file(b"");
        assert_ne!(file, socket);
        // SAFETY: both descriptors are live. This closes `socket` and makes
        // its number refer to the regular file without passing through a shim.
        assert_eq!(unsafe { libc::dup2(file, socket) }, socket);
        // SAFETY: the duplicated source and the unused peer are no longer needed.
        unsafe {
            libc::close(file);
            libc::close(peer);
        }
        socket
    }

    #[test]
    fn sendfile_uses_darwins_argument_order_and_value_result_count() {
        let file = memory_file(b"abcdefghij");
        let (ours, theirs) = pair();
        let mut header = libc::iovec {
            iov_base: c"H".as_ptr().cast_mut().cast(),
            iov_len: 1,
        };
        let mut trailer = libc::iovec {
            iov_base: c"T".as_ptr().cast_mut().cast(),
            iov_len: 1,
        };
        let hdtr = SendfileHdtr {
            headers: &raw mut header,
            header_count: 1,
            trailers: &raw mut trailer,
            trailer_count: 1,
        };
        // Darwin counts the header against this limit and always sends the
        // trailer, so this asks for one header byte, seven file bytes and one
        // trailer byte.
        let mut len = 8i64;
        // SAFETY: live descriptors and initialized value-result/header slots.
        assert_eq!(
            unsafe { sendfile(file, ours, 0, &raw mut len, &raw const hdtr, 0) },
            0
        );
        assert_eq!(len, 9, "the returned count includes the trailer");

        let mut got = [0u8; 9];
        // SAFETY: a connected descriptor and nine writable bytes.
        assert_eq!(
            unsafe {
                libc::recv(
                    theirs,
                    got.as_mut_ptr().cast(),
                    got.len(),
                    libc::MSG_WAITALL,
                )
            },
            got.len() as isize
        );
        assert_eq!(&got, b"HabcdefgT");

        // SAFETY: descriptors this test opened and no longer uses.
        unsafe {
            libc::close(file);
            libc::close(ours);
            libc::close(theirs);
        }
    }

    /// Linux accepts a regular output file, while Darwin requires a socket.
    /// Besides preserving that failure, the guest must receive Darwin's 38
    /// rather than Linux's 88 for `ENOTSOCK`.
    #[test]
    fn sendfile_translates_the_hosts_enotsock_for_the_guest() {
        let input = memory_file(b"x");
        let output = memory_file(b"");
        crate::errno::set(0);
        assert_eq!(crate::errno::guest_errno(), 0);

        let mut len = 1i64;
        // SAFETY: live descriptors and a writable value-result slot.
        assert_eq!(
            unsafe { sendfile(input, output, 0, &raw mut len, core::ptr::null(), 0,) },
            -1
        );
        assert_eq!(len, 0);
        assert_eq!(crate::errno::get(), libc::ENOTSOCK);
        assert_eq!(crate::errno::guest_errno(), 38, "Darwin's ENOTSOCK");
        crate::errno::set(0);

        // SAFETY: descriptors this test opened and no longer uses.
        unsafe {
            libc::close(input);
            libc::close(output);
        }
    }

    /// `MSG_EOF` asks for a half-close after the send, which is the peer seeing
    /// the bytes and then seeing end-of-file without anything being closed.
    #[test]
    fn msg_eof_half_closes_the_socket_after_the_send() {
        let (ours, theirs) = pair();
        // SAFETY: five readable bytes, and the length says five.
        let sent = unsafe { send(ours, c"hello".as_ptr().cast(), 5, msg::EOF) };
        assert_eq!(sent, 5);

        let mut buf = [0u8; 8];
        // SAFETY: eight writable bytes of this test's own.
        let read = unsafe { libc::read(theirs, buf.as_mut_ptr().cast(), 8) };
        assert_eq!(read, 5, "the bytes went before the half-close");
        // SAFETY: as above; the write half is shut, so this is end of file.
        assert_eq!(unsafe { libc::read(theirs, buf.as_mut_ptr().cast(), 8) }, 0);

        // SAFETY: two descriptors this test opened and no longer uses.
        unsafe {
            libc::close(ours);
            libc::close(theirs);
        }
    }

    /// The refusal this module used to make: a descriptor passed over a socket
    /// now arrives as a descriptor, in Darwin's twelve-byte control message on
    /// both sides.
    #[test]
    fn a_descriptor_crosses_a_socket_in_darwins_control_message() {
        let (ours, theirs) = pair();
        let mut ends = [0i32; 2];
        // SAFETY: a live array of two `int`.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);

        // Darwin's `CMSG_SPACE(sizeof(int))`: a twelve-byte header and four
        // bytes of payload, which needs no padding.
        let mut control = [0u8; 16];
        control[0..4].copy_from_slice(&16u32.to_ne_bytes());
        control[4..8].copy_from_slice(&0xffffu32.to_ne_bytes());
        control[8..12].copy_from_slice(&1i32.to_ne_bytes());
        control[12..16].copy_from_slice(&ends[0].to_ne_bytes());

        let mut byte = *b"x";
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let out = MsgHdr {
            name: core::ptr::null_mut(),
            namelen: 0,
            iov: &mut iov,
            iovlen: 1,
            control: control.as_mut_ptr().cast(),
            controllen: 16,
            flags: 0,
        };
        // SAFETY: a Darwin `msghdr` this test built, with one live iovec and a
        // control buffer of the length it states.
        assert_eq!(unsafe { sendmsg(ours, (&raw const out).cast(), 0) }, 1);

        let mut got = [0u8; 1];
        let mut got_iov = libc::iovec {
            iov_base: got.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut back = [0u8; 32];
        let mut incoming = MsgHdr {
            name: core::ptr::null_mut(),
            namelen: 0,
            iov: &mut got_iov,
            iovlen: 1,
            control: back.as_mut_ptr().cast(),
            controllen: 32,
            flags: 0,
        };
        // SAFETY: as above, with the buffers writable.
        assert_eq!(unsafe { recvmsg(theirs, (&raw mut incoming).cast(), 0) }, 1);
        assert_eq!(got, *b"x");
        assert_eq!(incoming.controllen, 16, "one twelve-byte header and an int");
        assert_eq!(u32::from_ne_bytes(back[4..8].try_into().unwrap()), 0xffff);
        assert_eq!(i32::from_ne_bytes(back[8..12].try_into().unwrap()), 1);

        // The descriptor that arrived is a descriptor, and it is the read end
        // of the pipe rather than the number that was sent.
        let received = i32::from_ne_bytes(back[12..16].try_into().unwrap());
        // SAFETY: the write end of a pipe this test owns.
        assert_eq!(
            unsafe { libc::write(ends[1], c"pipe".as_ptr().cast(), 4) },
            4
        );
        let mut through = [0u8; 4];
        // SAFETY: a descriptor the kernel just installed, and four writable bytes.
        assert_eq!(
            unsafe { libc::read(received, through.as_mut_ptr().cast(), 4) },
            4
        );
        assert_eq!(&through, b"pipe");

        // SAFETY: descriptors this test opened and no longer uses.
        unsafe {
            for fd in [ours, theirs, ends[0], ends[1], received] {
                libc::close(fd);
            }
        }
    }

    /// The part of `SO_NOSIGPIPE` that a per-call flag did not reach: `write`
    /// takes no flags, so a guest that set the option and wrote to a socket
    /// whose peer had gone still died of a signal.
    #[test]
    fn a_write_to_a_closed_peer_reports_epipe_when_the_guest_asked_it_to() {
        let (ours, theirs) = pair();
        // SAFETY: the other end, which this test is finished with.
        unsafe { libc::close(theirs) };

        let on = 1i32;
        // SAFETY: one readable `int`, and the length says four. `0xffff` is
        // Darwin's `SOL_SOCKET` and `0x1022` its `SO_NOSIGPIPE`.
        let rc = unsafe {
            crate::net::sockopt::setsockopt(ours, 0xffff, 0x1022, (&raw const on).cast(), 4)
        };
        assert_eq!(rc, 0);

        // SAFETY: five readable bytes, and the length says five.
        let wrote = unsafe { write(ours, c"hello".as_ptr().cast(), 5) };
        assert_eq!((wrote, crate::errno::get()), (-1, libc::EPIPE));
        crate::errno::set(0);

        crate::net::sockopt::forget(ours);
        // SAFETY: a descriptor this test opened and no longer uses.
        unsafe { libc::close(ours) };
    }

    #[test]
    fn write_discards_stale_nosigpipe_state_when_a_file_reuses_the_descriptor() {
        let file = file_with_stale_nosigpipe();
        crate::errno::set(libc::EDOM);

        // SAFETY: three readable bytes and a live writable file descriptor.
        assert_eq!(unsafe { write(file, c"abc".as_ptr().cast(), 3) }, 3);
        assert_eq!(crate::errno::get(), libc::EDOM);
        assert!(!crate::net::sockopt::suppresses_sigpipe(file));

        // SAFETY: a live regular file, rewound before reading into three bytes.
        assert_eq!(unsafe { libc::lseek(file, 0, libc::SEEK_SET) }, 0);
        let mut bytes = [0u8; 3];
        assert_eq!(
            unsafe { libc::read(file, bytes.as_mut_ptr().cast(), bytes.len()) },
            3
        );
        assert_eq!(&bytes, b"abc");
        // SAFETY: a descriptor this test owns and no longer uses.
        unsafe { libc::close(file) };
    }

    #[test]
    fn writev_discards_stale_nosigpipe_state_when_a_file_reuses_the_descriptor() {
        let file = file_with_stale_nosigpipe();
        let parts = [
            libc::iovec {
                iov_base: c"ab".as_ptr().cast_mut().cast(),
                iov_len: 2,
            },
            libc::iovec {
                iov_base: c"c".as_ptr().cast_mut().cast(),
                iov_len: 1,
            },
        ];
        crate::errno::set(libc::EDOM);

        // SAFETY: two valid iovecs and a live writable file descriptor.
        assert_eq!(unsafe { writev(file, parts.as_ptr(), 2) }, 3);
        assert_eq!(crate::errno::get(), libc::EDOM);
        assert!(!crate::net::sockopt::suppresses_sigpipe(file));

        // SAFETY: a live regular file, rewound before reading into three bytes.
        assert_eq!(unsafe { libc::lseek(file, 0, libc::SEEK_SET) }, 0);
        let mut bytes = [0u8; 3];
        assert_eq!(
            unsafe { libc::read(file, bytes.as_mut_ptr().cast(), bytes.len()) },
            3
        );
        assert_eq!(&bytes, b"abc");
        // SAFETY: a descriptor this test owns and no longer uses.
        unsafe { libc::close(file) };
    }

    /// The name a Unix socket was bound by comes back from `getsockname` in
    /// Darwin's shape and the guest's spelling, with Darwin's length.
    #[test]
    fn getsockname_reports_a_unix_socket_by_the_path_it_was_bound_to() {
        let path = std::env::temp_dir().join(format!("gsn-{}.sock", std::process::id()));
        let path = path.as_os_str().as_encoded_bytes();
        let mut guest = [0u8; 106];
        guest[0] = guest.len() as u8;
        guest[1] = addr::family::UNIX as u8;
        guest[2..2 + path.len()].copy_from_slice(path);

        // SAFETY: three integers.
        let fd = unsafe { socket(addr::family::UNIX, libc::SOCK_STREAM, 0) };
        assert!(fd >= 0);
        // SAFETY: a Darwin `sockaddr_un` this test owns, with its length.
        assert_eq!(
            unsafe { bind(fd, guest.as_ptr().cast(), guest.len() as u32) },
            0
        );
        let mut back = [0xffu8; 106];
        let mut len = back.len() as u32;
        // SAFETY: a writable buffer and its length.
        assert_eq!(
            unsafe { getsockname(fd, back.as_mut_ptr().cast(), &mut len) },
            0
        );
        assert_eq!(back[1], addr::family::UNIX as u8);
        assert_eq!(&back[2..2 + path.len()], path);
        assert_eq!(back[2 + path.len()], 0);
        assert_eq!(len as usize, 2 + path.len() + 1);
        assert_eq!(back[0] as u32, len);

        // SAFETY: a descriptor this test owns and no longer uses.
        unsafe { libc::close(fd) };
        let _ = std::fs::remove_file(std::str::from_utf8(path).unwrap());
    }

    #[test]
    fn a_socket_of_a_family_this_host_lacks_is_refused_rather_than_opened() {
        // Darwin's `AF_LINK`.
        // SAFETY: no caller memory is touched on the refusing path.
        assert_eq!(unsafe { socket(18, libc::SOCK_DGRAM, 0) }, -1);
        assert_eq!(crate::errno::get(), libc::EAFNOSUPPORT);
        crate::errno::set(0);
    }

    /// A round trip through the real stack, in Darwin's spelling throughout: an
    /// IPv6 socket - the family whose number differs - bound to the loopback,
    /// asked for its own name back.
    #[test]
    fn an_ipv6_socket_binds_and_reports_its_address_in_darwins_shape() {
        // SAFETY: Darwin's `AF_INET6`, which this entry point translates.
        let fd = unsafe { socket(addr::family::INET6, libc::SOCK_DGRAM, 0) };
        assert!(fd >= 0, "an IPv6 datagram socket");

        // `sin6_len`, `sin6_family`, port 0, flow 0, `::1`, scope 0.
        let mut want = [0u8; 28];
        want[0] = 28;
        want[1] = addr::family::INET6 as u8;
        want[8 + 15] = 1;
        // SAFETY: twenty-eight readable bytes, and the length matches.
        assert_eq!(unsafe { bind(fd, want.as_ptr().cast(), 28) }, 0);

        let mut got = [0u8; 28];
        let mut len = 28u32;
        // SAFETY: twenty-eight writable bytes and a live length.
        assert_eq!(
            unsafe { getsockname(fd, got.as_mut_ptr().cast(), &mut len) },
            0
        );
        assert_eq!(len, 28);
        assert_eq!(got[1], addr::family::INET6 as u8, "Darwin's family number");
        assert_eq!(&got[8..24], &want[8..24], "the address itself");

        // SAFETY: a descriptor this test opened and no longer uses.
        unsafe { libc::close(fd) };
    }

    /// The sender's address a `recvmsg` reports, which this module used to
    /// overwrite with zeroes between the call and the conversion.
    #[test]
    fn recvmsg_reports_the_senders_address_in_darwins_shape() {
        /// A Darwin `sockaddr_in` for `127.0.0.1:port`.
        fn loopback(port: u16) -> [u8; 16] {
            let mut a = [0u8; 16];
            a[0] = 16;
            a[1] = addr::family::INET as u8;
            a[2..4].copy_from_slice(&port.to_be_bytes());
            a[4..8].copy_from_slice(&[127, 0, 0, 1]);
            a
        }

        // SAFETY: Darwin's `AF_INET`; no caller memory is touched.
        let (rx, tx) = unsafe {
            (
                socket(addr::family::INET, libc::SOCK_DGRAM, 0),
                socket(addr::family::INET, libc::SOCK_DGRAM, 0),
            )
        };
        assert!(rx >= 0 && tx >= 0, "two datagram sockets");

        let any = loopback(0);
        // SAFETY: sixteen readable bytes, and the length matches.
        unsafe {
            assert_eq!(bind(rx, any.as_ptr().cast(), 16), 0);
            assert_eq!(bind(tx, any.as_ptr().cast(), 16), 0);
        }

        let mut bound = [0u8; 16];
        let mut len = 16u32;
        // SAFETY: sixteen writable bytes and a live length.
        assert_eq!(
            unsafe { getsockname(rx, bound.as_mut_ptr().cast(), &mut len) },
            0
        );
        let mut sender = [0u8; 16];
        let mut sender_len = 16u32;
        assert_eq!(
            unsafe { getsockname(tx, sender.as_mut_ptr().cast(), &mut sender_len) },
            0
        );

        // SAFETY: one readable byte and a sixteen-byte address of that length.
        assert_eq!(
            unsafe { sendto(tx, c"x".as_ptr().cast(), 1, 0, bound.as_ptr().cast(), 16) },
            1
        );

        let mut got = [0u8; 1];
        let mut iov = libc::iovec {
            iov_base: got.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut name = [0u8; 128];
        let mut incoming = MsgHdr {
            name: name.as_mut_ptr().cast(),
            namelen: 128,
            iov: &mut iov,
            iovlen: 1,
            control: core::ptr::null_mut(),
            controllen: 0,
            flags: 0,
        };
        // SAFETY: a Darwin `msghdr` this test built, with one writable iovec
        // and a name buffer of the length it states.
        assert_eq!(unsafe { recvmsg(rx, (&raw mut incoming).cast(), 0) }, 1);
        assert_eq!(got, *b"x");
        assert_eq!(incoming.namelen, 16, "a `sockaddr_in`");
        assert_eq!(name[1], addr::family::INET as u8, "Darwin's family number");
        assert_eq!(&name[2..8], &sender[2..8], "the port and address that sent");

        // SAFETY: descriptors this test opened and no longer uses.
        unsafe {
            libc::close(rx);
            libc::close(tx);
        }
    }

    /// The layout claim this module rests on: a guest's `struct msghdr` is
    /// forty-eight bytes and the host's is not, so it is a value to convert.
    #[test]
    fn the_guests_msghdr_is_not_the_hosts() {
        assert_eq!(core::mem::size_of::<MsgHdr>(), 48);
        assert!(
            core::mem::size_of::<libc::msghdr>() > core::mem::size_of::<MsgHdr>(),
            "this host's struct msghdr is Darwin-shaped after all"
        );
    }
    /// A TCP disconnect ends the connection the way a close would, so the
    /// peer reads the end of the stream.
    #[test]
    fn disconnectx_ends_a_tcp_connection() {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ours = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut theirs, _) = listener.accept().unwrap();
        // SAFETY: a live socket.
        assert_eq!(
            unsafe { disconnectx(ours.as_raw_fd(), SAE_ASSOCID_ANY, 0) },
            0
        );
        let mut byte = [0u8];
        assert_eq!(theirs.read(&mut byte).unwrap(), 0);
    }

    /// A UDP disconnect dissolves the association, and a second has nothing
    /// left to dissolve.
    #[test]
    fn disconnectx_dissolves_a_udp_association() {
        use std::os::fd::AsRawFd;
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.connect("127.0.0.1:9").unwrap();
        let fd = socket.as_raw_fd();
        // SAFETY: a live socket.
        assert_eq!(
            unsafe { disconnectx(fd, SAE_ASSOCID_ALL, SAE_CONNID_ONLY) },
            0
        );
        assert!(socket.peer_addr().is_err());
        // SAFETY: as above.
        assert_eq!(unsafe { disconnectx(fd, SAE_ASSOCID_ANY, 0) }, -1);
        assert_eq!(crate::errno::get(), libc::ENOTCONN);
    }

    #[test]
    fn disconnectx_refuses_an_association_a_socket_cannot_have() {
        use std::os::fd::AsRawFd;
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        // SAFETY: a live socket.
        assert_eq!(unsafe { disconnectx(socket.as_raw_fd(), 7, 0) }, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
    }

    /// Darwin's local sockets have no disconnect, and the guest is told so in
    /// Darwin's own number, which this host would otherwise call `ENOTSUP`.
    #[test]
    fn disconnectx_is_not_supported_on_a_local_socket() {
        let (one, other) = pair();
        // SAFETY: a live socket.
        assert_eq!(unsafe { disconnectx(one, SAE_ASSOCID_ANY, 0) }, -1);
        assert_eq!(crate::errno::guest_errno(), crate::errno::DARWIN_EOPNOTSUPP);
        crate::errno::set(0);
        // SAFETY: descriptors this test opened.
        unsafe {
            libc::close(one);
            libc::close(other);
        }
    }
}
