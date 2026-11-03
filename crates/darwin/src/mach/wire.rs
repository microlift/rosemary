use std::ffi::c_int;
use std::sync::Mutex;

/// A port's address: the socket name is derived from it and nothing
/// else, so this is the whole of a send right's contents.
pub type Address = u64;

/// A datagram as it crossed, with any rights that travelled with it still as
/// descriptors.
pub struct Datagram {
    pub bytes: Vec<u8>,
    pub fds: Vec<c_int>,
}

impl Drop for Datagram {
    /// Any right that was received and never claimed is destroyed, which is what
    /// Mach does with the rights in a message a receiver discards.
    fn drop(&mut self) {
        for fd in self.fds.drain(..) {
            // SAFETY: a descriptor this process received and has not handed on.
            unsafe { libc::close(fd) };
        }
    }
}

impl Datagram {
    /// Take the descriptor at `index`, leaving nothing behind for [`Drop`].
    pub fn take_fd(&mut self, index: usize) -> Option<c_int> {
        let slot = self.fds.get_mut(index)?;
        match *slot {
            -1 => None,
            fd => {
                *slot = -1;
                Some(fd)
            }
        }
    }
}

/// The largest message this transport carries.
///
/// A unix datagram's real limit is the socket buffer, which is tunable and not
/// worth guessing at; this is a ceiling of our own so that an absurd
/// `msgh_size` is refused by name rather than truncated by the kernel. Mach's
/// own inline limit is of the same order.
pub const MAX_MESSAGE: usize = 64 * 1024;

/// A fresh, unused port address.
pub fn fresh_address() -> Address {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_ok() {
        // Never zero: zero is `MACH_PORT_NULL`'s address and a null send right
        // must not be indistinguishable from a real one.
        let id = u64::from_ne_bytes(bytes);
        if id != 0 {
            return id;
        }
    }
    // The kernel declining to answer is not a reason to hand out a colliding
    // name, so fall back to something still unique within this host and boot:
    // the process, the thread, and a counter.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let count = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = crate::process::host_self() as u64;
    (pid << 32) | count
}

/// The abstract socket name for `address`, and its length.
///
/// The uid is in the name only so that two users' ports are visibly separate in
/// `ss -x`; the random address is what actually keeps them apart.
#[cfg(target_os = "linux")]
fn sockaddr(address: Address) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    named(address, "")
}

/// The listener a port's send rights hold a claim on, beside the port itself.
#[cfg(target_os = "linux")]
fn senders_sockaddr(address: Address) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    named(address, ".senders")
}

#[cfg(target_os = "linux")]
fn named(address: Address, suffix: &str) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let mut addr: libc::sockaddr_un = unsafe { core::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let uid = unsafe { libc::getuid() };
    let name = format!("rosemary.mach.{uid}.{address:016x}{suffix}");
    // The leading NUL is the abstract namespace; the name is what follows it and
    // is *not* NUL-terminated, so the length has to be exact.
    let bytes = name.as_bytes();
    for (slot, byte) in addr.sun_path[1..].iter_mut().zip(bytes) {
        *slot = *byte as _;
    }
    let len = core::mem::size_of::<libc::sa_family_t>() + 1 + bytes.len();
    Ok((addr, len as libc::socklen_t))
}

/// BSD hosts bind a filesystem socket in Rosemary's private runtime directory.
#[cfg(not(target_os = "linux"))]
fn socket_path(address: Address) -> std::path::PathBuf {
    server::location::runtime_directory().join(format!("mach/{address:016x}"))
}

#[cfg(not(target_os = "linux"))]
fn senders_path(address: Address) -> std::path::PathBuf {
    server::location::runtime_directory().join(format!("mach/{address:016x}.senders"))
}

#[cfg(not(target_os = "linux"))]
fn sockaddr(address: Address) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    at_path(socket_path(address))
}

#[cfg(not(target_os = "linux"))]
fn senders_sockaddr(address: Address) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    at_path(senders_path(address))
}

#[cfg(not(target_os = "linux"))]
fn at_path(path: std::path::PathBuf) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    use std::os::unix::ffi::OsStrExt;
    let mut addr: libc::sockaddr_un = unsafe { core::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= addr.sun_path.len() {
        return Err(std::io::Error::from_raw_os_error(libc::ENAMETOOLONG));
    }
    for (slot, byte) in addr.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as _;
    }
    let len = core::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    addr.sun_len = len as u8;
    Ok((addr, len as libc::socklen_t))
}

/// Whether anything still holds the receive right for `address`.
///
/// Asked by the name server, which has to tell a service that is running from
/// one whose process died without unregistering. A datagram `connect` is the
/// cheap way to find out: it fails with `ECONNREFUSED` when nothing is bound
/// and sends nothing when it succeeds, so a live holder never sees it.
pub fn is_bound(address: Address) -> bool {
    let Ok((addr, len)) = sockaddr(address) else {
        return false;
    };
    let Ok(fd) = socket() else { return false };
    // SAFETY: a socket this call just opened, and an address whose length is
    // computed from the name written into it.
    let rc = unsafe { libc::connect(fd, (&raw const addr).cast(), len) };
    // SAFETY: the descriptor opened above, which nothing else holds.
    unsafe { libc::close(fd) };
    rc == 0
}

/// A receive right taken apart to be sent to another process.
pub struct Moved {
    pub fd: c_int,
    pub senders: c_int,
    pub claims: Vec<c_int>,
    pub complete: bool,
}

/// The end of a port that messages arrive at: a receive right.
pub struct Receiver {
    fd: c_int,
    /// The listener send rights to this port hold a claim on, or `-1` when it
    /// could not be made - in which case no-senders is never reported.
    senders: c_int,
    address: Address,
    /// A message that was too large for the buffer a caller offered.
    ///
    /// Mach leaves such a message queued when the caller asked for
    /// `MACH_RCV_LARGE`, so that a second call with a bigger buffer gets it. A
    /// datagram socket has no way to put one back, so it is held here instead
    /// and handed to the next receive on this port.
    held: Mutex<Option<Datagram>>,
}

impl Drop for Receiver {
    fn drop(&mut self) {
        // SAFETY: this type's own descriptor, not shared with any other value:
        // a `Receiver` is only ever reached through an `Arc`.
        unsafe { libc::close(self.fd) };
        if self.senders >= 0 {
            // Before the close, so the registry never polls a reused number.
            for claim in super::senders::untrack(self.address)
                .map(|taken| taken.claims)
                .unwrap_or_default()
            {
                // SAFETY: a claim the registry accepted and handed back.
                unsafe { libc::close(claim) };
            }
            // SAFETY: as above, this type's own listener.
            unsafe { libc::close(self.senders) };
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = std::fs::remove_file(socket_path(self.address));
            let _ = std::fs::remove_file(senders_path(self.address));
        }
    }
}

impl Receiver {
    /// Bind a new port. The address it answers on is fresh and unused.
    pub fn bind() -> std::io::Result<Self> {
        #[cfg(not(target_os = "linux"))]
        server::location::prepare(&server::location::runtime_directory().join("mach"))?;
        // Retried because two processes can draw the same address, however
        // unlikely - and the failure mode of not retrying is one run receiving
        // another's messages, which no caller could diagnose.
        for _ in 0..8 {
            let address = fresh_address();
            let (addr, len) = sockaddr(address)?;
            let fd = socket()?;
            // SAFETY: a socket this call just opened, and an address whose
            // length is computed from the name written into it.
            let rc = unsafe { libc::bind(fd, (&raw const addr).cast(), len) };
            if rc == 0 {
                let senders = listen_for_senders(address);
                if senders >= 0 {
                    super::senders::track(address, senders, true, Vec::new());
                }
                return Ok(Receiver {
                    fd,
                    senders,
                    address,
                    held: Mutex::new(None),
                });
            }
            let taken = std::io::Error::last_os_error();
            // SAFETY: the descriptor opened above, which nothing else holds.
            unsafe { libc::close(fd) };
            if taken.raw_os_error() != Some(libc::EADDRINUSE) {
                return Err(taken);
            }
        }
        Err(std::io::Error::other("no free Mach port address"))
    }

    /// Adopt a receive right that arrived in a message: the descriptor is the
    /// same socket the sender had bound, so this end answers on the same
    /// address.
    ///
    /// `senders` is the listener that travelled with it, or `-1`, and `claims`
    /// the claims its old owner had accepted - the senders it already has.
    /// `complete` is false when some of those could not be carried.
    ///
    /// # Safety
    ///
    /// `fd` is an owned descriptor for the socket bound to `address`, and
    /// `senders` and `claims` are owned descriptors for its listener (or `-1`)
    /// and the claims on it.
    pub unsafe fn adopt(
        fd: c_int,
        senders: c_int,
        claims: Vec<c_int>,
        complete: bool,
        address: Address,
    ) -> Self {
        if senders >= 0 {
            super::senders::track(address, senders, complete, claims);
        } else {
            for claim in claims {
                // SAFETY: descriptors handed over to this call.
                unsafe { libc::close(claim) };
            }
        }
        Receiver {
            fd,
            senders,
            address,
            held: Mutex::new(None),
        }
    }

    pub fn address(&self) -> Address {
        self.address
    }

    pub fn fd(&self) -> c_int {
        self.fd
    }

    /// Give up the descriptors - the port, its listener (`-1` if it has none)
    /// and the claims accepted on it - so they can be sent to another process.
    ///
    /// The `Receiver` must be dropped without closing after this, which is why
    /// it consumes the value.
    pub fn into_parts(self) -> Moved {
        let taken = match self.senders >= 0 {
            true => super::senders::untrack(self.address),
            false => None,
        };
        let moved = Moved {
            fd: self.fd,
            senders: self.senders,
            complete: taken.as_ref().is_some_and(|t| t.complete),
            claims: taken.map(|t| t.claims).unwrap_or_default(),
        };
        core::mem::forget(self);
        moved
    }

    /// A message held back because the last caller's buffer was too small.
    pub fn take_held(&self) -> Option<Datagram> {
        self.held.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Keep `message` for the next receive on this port.
    pub fn hold(&self, message: Datagram) {
        *self.held.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
    }

    /// Whether a message is waiting, without taking it.
    pub fn ready(&self) -> bool {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
            || poll_one(self.fd, 0)
    }

    /// Take the next message, or `None` if none arrived within `timeout_ms`.
    ///
    /// `timeout_ms` of `-1` waits forever, which is what a `mach_msg` without
    /// `MACH_RCV_TIMEOUT` asks for.
    pub fn receive(&self, timeout_ms: i32) -> std::io::Result<Option<Datagram>> {
        if let Some(held) = self.take_held() {
            return Ok(Some(held));
        }
        if !poll_one(self.fd, timeout_ms) {
            return Ok(None);
        }
        receive_from(self.fd)
    }
}

/// Bind and listen on the sender-tracking socket for `address`, or `-1`.
fn listen_for_senders(address: Address) -> c_int {
    let Ok((addr, len)) = senders_sockaddr(address) else {
        return -1;
    };
    let Ok(fd) = stream_socket() else { return -1 };
    // SAFETY: a socket opened above, and an address whose length is computed
    // from the name written into it.
    let bound = unsafe { libc::bind(fd, (&raw const addr).cast(), len) } == 0
        && unsafe { libc::listen(fd, 1024) } == 0;
    if !bound {
        // SAFETY: the descriptor opened above, which nothing else holds.
        unsafe { libc::close(fd) };
        return -1;
    }
    fd
}

/// Take a claim on the port at `address`: it counts as a sender for as long as
/// the descriptor stays open, in this process or - once sent - another.
///
/// `None` when the port is gone or does not track senders, which is also when
/// there is nothing for the claim to keep alive.
pub fn claim(address: Address) -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    let (addr, len) = senders_sockaddr(address).ok()?;
    let fd = stream_socket().ok()?;
    // SAFETY: a socket opened above. Non-blocking, so that a full backlog is a
    // refusal here rather than a wait on a port that is not accepting.
    let rc = unsafe { libc::connect(fd, (&raw const addr).cast(), len) };
    if rc < 0 {
        // SAFETY: the descriptor opened above, which nothing else holds.
        unsafe { libc::close(fd) };
        return None;
    }
    // SAFETY: an open descriptor this call owns.
    Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// A non-blocking, close-on-exec stream socket.
fn stream_socket() -> std::io::Result<c_int> {
    #[cfg(not(target_os = "macos"))]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK;
    #[cfg(target_os = "macos")]
    let kind = libc::SOCK_STREAM;
    // SAFETY: a plain socket call with constant arguments.
    let fd = unsafe { libc::socket(libc::AF_UNIX, kind, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(target_os = "macos")]
    // SAFETY: the descriptor opened above.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    Ok(fd)
}

/// A socket suitable for a port: datagrams, close-on-exec.
///
/// `SOCK_CLOEXEC` because a port must not leak into a process that was never
/// given the right - a `posix_spawn` of an unrelated program would otherwise
/// inherit every port this process holds.
fn socket() -> std::io::Result<c_int> {
    // SAFETY: a plain socket call with constant arguments.
    #[cfg(not(target_os = "macos"))]
    let kind = libc::SOCK_DGRAM | libc::SOCK_CLOEXEC;
    #[cfg(target_os = "macos")]
    let kind = libc::SOCK_DGRAM;
    let fd = unsafe { libc::socket(libc::AF_UNIX, kind, 0) };
    #[cfg(target_os = "macos")]
    if fd >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error);
    }
    match fd < 0 {
        true => Err(std::io::Error::last_os_error()),
        false => Ok(fd),
    }
}

/// Wait for `fd` to have a datagram. `timeout_ms` of `-1` waits forever.
fn poll_one(fd: c_int, timeout_ms: i32) -> bool {
    let mut entry = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: one live `struct pollfd`, and the count matches.
        let rc = unsafe { libc::poll(&mut entry, 1, timeout_ms) };
        if rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            // A signal is not a timeout. Restarting with the original timeout
            // can wait longer than asked; waking a `mach_msg` early would be
            // read as `MACH_RCV_TIMED_OUT`, which is the worse answer.
            continue;
        }
        return rc > 0;
    }
}

/// Wait for any of `fds` and answer with the index of the first that is ready.
///
/// This is what a port *set* receives on: Mach's set has no queue of its own, it
/// just names the members whose queues to look at.
pub fn poll_any(fds: &[c_int], timeout_ms: i32) -> Option<usize> {
    let mut entries: Vec<libc::pollfd> = fds
        .iter()
        .map(|fd| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    loop {
        // SAFETY: `entries` is exactly the number of live `struct pollfd` the
        // count names.
        let rc = unsafe {
            libc::poll(
                entries.as_mut_ptr(),
                entries.len() as libc::nfds_t,
                timeout_ms,
            )
        };
        if rc < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return None;
        }
        if rc == 0 {
            return None;
        }
        return entries.iter().position(|e| e.revents & libc::POLLIN != 0);
    }
}

/// The largest number of rights one message can carry.
///
/// Mach has no such limit; `SCM_RIGHTS` does, and a fixed control buffer is what
/// keeps the receive path from allocating on a length the sender chose.
pub const MAX_RIGHTS: usize = 16;

/// The most descriptors one message can carry, just under the kernel's limit of
/// 253 for one `SCM_RIGHTS`. A receive right is the port, its listener and the
/// claims on it; a send right is one claim.
pub const MAX_FDS: usize = 252;

fn control_len() -> usize {
    // SAFETY: `CMSG_SPACE` is arithmetic on its argument.
    unsafe { libc::CMSG_SPACE((MAX_FDS * core::mem::size_of::<c_int>()) as u32) as usize }
}

/// Send `bytes` to `destination`, moving `fds` with it.
///
/// The descriptors are *not* closed here: a caller that moved a right closes its
/// own copy once the send has succeeded, and one that failed still holds the
/// right it was asked to move.
pub fn send(destination: Address, bytes: &[u8], fds: &[c_int]) -> std::io::Result<()> {
    /// One sending socket for the whole process.
    ///
    /// Unbound: a reply travels to a port named inside the message, never back
    /// to the source address of the datagram, so this end never needs a name.
    /// `sendmsg` is atomic per datagram, so the descriptor is shared freely.
    static SENDER: std::sync::OnceLock<Option<c_int>> = std::sync::OnceLock::new();

    let Some(sender) = *SENDER.get_or_init(|| socket().ok()) else {
        return Err(std::io::Error::other("no Mach sending socket"));
    };
    if fds.len() > MAX_FDS {
        return Err(std::io::Error::from_raw_os_error(libc::EMSGSIZE));
    }

    let (addr, addr_len) = sockaddr(destination)?;
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut _,
        iov_len: bytes.len(),
    };
    let mut control = vec![0u8; control_len()];
    // SAFETY: `msghdr` is a plain C structure; zeroing it is what the header's
    // own examples do before filling in the fields used below.
    let mut msg: libc::msghdr = unsafe { core::mem::zeroed() };
    msg.msg_name = (&raw const addr) as *mut _;
    msg.msg_namelen = addr_len;
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;

    if !fds.is_empty() {
        let payload = fds.len() * core::mem::size_of::<c_int>();
        // SAFETY: `control` is `control_len()` bytes, which is `CMSG_SPACE` for
        // the largest payload allowed, and `fds.len()` is within that.
        unsafe {
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(payload as u32) as _;
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(payload as u32) as _;
            core::ptr::copy_nonoverlapping(
                fds.as_ptr().cast::<u8>(),
                libc::CMSG_DATA(cmsg),
                payload,
            );
        }
    }

    loop {
        // SAFETY: `msg` describes `bytes` and the control block above, both
        // still live.
        let sent = unsafe { libc::sendmsg(sender, &msg, 0) };
        if sent >= 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(error);
    }
}

/// Take one datagram from `fd`, with any descriptors that came with it.
fn receive_from(fd: c_int) -> std::io::Result<Option<Datagram>> {
    let mut bytes = vec![0u8; MAX_MESSAGE];
    let mut control = vec![0u8; control_len()];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: as in [`send`].
    let mut msg: libc::msghdr = unsafe { core::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len() as _;

    let received = loop {
        // SAFETY: `msg` describes `bytes` and `control`, both live and of the
        // lengths recorded in it.
        #[cfg(not(target_os = "macos"))]
        let flags = libc::MSG_CMSG_CLOEXEC;
        #[cfg(target_os = "macos")]
        let flags = 0;
        let rc = unsafe { libc::recvmsg(fd, &mut msg, flags) };
        if rc >= 0 {
            break rc as usize;
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EAGAIN) => return Ok(None),
            _ => return Err(error),
        }
    };

    let mut fds = Vec::new();
    // SAFETY: `msg` was just filled in by `recvmsg`, so its control block is a
    // well-formed sequence of `cmsghdr`.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let payload = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                let count = payload / core::mem::size_of::<c_int>();
                let data = libc::CMSG_DATA(cmsg).cast::<c_int>();
                for i in 0..count {
                    fds.push(data.add(i).read_unaligned());
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }

    #[cfg(target_os = "macos")]
    for &fd in &fds {
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            let error = std::io::Error::last_os_error();
            for fd in fds {
                unsafe { libc::close(fd) };
            }
            return Err(error);
        }
    }
    bytes.truncate(received);
    Ok(Some(Datagram { bytes, fds }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_sent_to_a_bound_port_arrives_whole() {
        let port = Receiver::bind().expect("bind");
        send(port.address(), b"hello", &[]).expect("send");
        let message = port.receive(1000).expect("receive").expect("a message");
        assert_eq!(message.bytes, b"hello");
        assert!(message.fds.is_empty());
    }

    /// Nothing bound to an address is the same event as a dead port, and it has
    /// to be reported rather than silently dropped: it is how a client learns
    /// the other end is gone.
    #[test]
    fn a_send_to_an_address_nobody_holds_fails() {
        let error = send(fresh_address(), b"x", &[]).expect_err("no such port");
        assert!(matches!(
            error.raw_os_error(),
            Some(libc::ECONNREFUSED | libc::ENOENT)
        ));
    }

    #[test]
    fn a_receive_with_a_timeout_gives_up_rather_than_waiting() {
        let port = Receiver::bind().expect("bind");
        let started = std::time::Instant::now();
        assert!(port.receive(50).expect("receive").is_none());
        assert!(started.elapsed() >= std::time::Duration::from_millis(40));
    }

    /// The property the whole design rests on: a receive right is a descriptor,
    /// so passing the descriptor passes the right - the adopting end answers on
    /// the same address the sender was bound to.
    #[test]
    fn a_receive_right_survives_being_passed_as_a_descriptor() {
        let mailbox = Receiver::bind().expect("mailbox");
        let moved = Receiver::bind().expect("the right being moved");
        let address = moved.address();

        let moved = moved.into_parts();
        let (fd, senders) = (moved.fd, moved.senders);
        send(mailbox.address(), b"carrying a right", &[fd]).expect("send");
        // SAFETY: the sender's own copies, no longer used - the kernel holds the
        // in-flight reference until the receive below.
        unsafe {
            libc::close(fd);
            libc::close(senders);
        }

        let mut message = mailbox.receive(1000).expect("receive").expect("a message");
        let carried = message.take_fd(0).expect("a right came with it");
        // SAFETY: the descriptor just received, bound to `address`.
        let adopted = unsafe { Receiver::adopt(carried, -1, Vec::new(), true, address) };
        assert_eq!(adopted.address(), address);

        send(address, b"to the new holder", &[]).expect("send to the moved right");
        let arrived = adopted.receive(1000).expect("receive").expect("a message");
        assert_eq!(arrived.bytes, b"to the new holder");
    }

    #[test]
    fn a_message_too_large_for_the_caller_is_kept_for_the_next_receive() {
        let port = Receiver::bind().expect("bind");
        send(port.address(), b"first", &[]).expect("send");
        let held = port.receive(1000).expect("receive").expect("a message");
        port.hold(held);
        assert!(port.ready());
        let again = port.receive(0).expect("receive").expect("the held message");
        assert_eq!(again.bytes, b"first");
    }
}
