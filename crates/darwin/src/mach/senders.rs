use std::ffi::c_int;
use std::sync::Mutex;

use super::wire::Address;

struct Request {
    notify: Address,
    sync: u32,
}

struct Entry {
    address: Address,
    listener: c_int,
    claims: Vec<c_int>,
    made: u32,
    complete: bool,
    request: Option<Request>,
}

struct Registry {
    entries: Vec<Entry>,
    owner: u32,
    wake: Option<(c_int, c_int)>,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    entries: Vec::new(),
    owner: 0,
    wake: None,
});

fn lock() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

fn prepare(registry: &mut Registry) -> bool {
    let me = std::process::id();
    if registry.owner == me && registry.wake.is_some() {
        return true;
    }
    // A fork child: close what the parent's thread was tracking. The listeners
    // stay open - their `Receiver`s own them - but are no longer accepted from.
    for entry in registry.entries.drain(..) {
        for claim in entry.claims {
            // SAFETY: a descriptor this registry accepted and still owns.
            unsafe { libc::close(claim) };
        }
    }
    let mut pipe = [0 as c_int; 2];
    // SAFETY: a two-element array, as `pipe` requires.
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } < 0 {
        return false;
    }
    for fd in pipe {
        // SAFETY: descriptors just opened.
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let read = pipe[0];
    let started = std::thread::Builder::new()
        .name("mach-senders".into())
        .spawn(move || run(read, me))
        .is_ok();
    if !started {
        for fd in pipe {
            // SAFETY: the descriptors opened above, never shared.
            unsafe { libc::close(fd) };
        }
        return false;
    }
    registry.owner = me;
    registry.wake = Some((pipe[0], pipe[1]));
    true
}

fn wake(registry: &Registry) {
    if let Some((_, write)) = registry.wake {
        unsafe { libc::write(write, [0u8].as_ptr().cast(), 1) };
    }
}

/// What a port was owed when it stopped being tracked here.
pub struct Taken {
    /// Claims accepted and still open, now owned by the caller.
    pub claims: Vec<c_int>,
    pub complete: bool,
}

/// Start accepting claims on `listener`, which is bound for `address`, with
/// `claims` already accepted by whoever held the port before.
pub fn track(address: Address, listener: c_int, complete: bool, claims: Vec<c_int>) {
    let mut registry = lock();
    if !prepare(&mut registry) {
        for claim in claims {
            // SAFETY: descriptors handed over to this call.
            unsafe { libc::close(claim) };
        }
        return;
    }
    registry.entries.push(Entry {
        address,
        listener,
        made: claims.len() as u32,
        claims,
        complete,
        request: None,
    });
    wake(&registry);
}

pub fn untrack(address: Address) -> Option<Taken> {
    let mut registry = lock();
    let at = registry.entries.iter().position(|e| e.address == address)?;
    let entry = registry.entries.swap_remove(at);
    wake(&registry);
    Some(Taken {
        claims: entry.claims,
        complete: entry.complete,
    })
}

pub fn request(address: Address, notify: Option<Address>, sync: u32) -> bool {
    let mut registry = lock();
    let me = std::process::id();
    if registry.owner != me {
        return false;
    }
    let Some(entry) = registry.entries.iter_mut().find(|e| e.address == address) else {
        return false;
    };
    if notify.is_some() && !entry.complete {
        return false;
    }
    entry.request = notify.map(|notify| Request { notify, sync });
    wake(&registry);
    true
}

fn run(wake_read: c_int, owner: u32) {
    enum What {
        Wake,
        Listener(Address, c_int),
        Claim(Address, c_int),
    }
    loop {
        let mut what = vec![What::Wake];
        let mut polled = vec![libc::pollfd {
            fd: wake_read,
            events: libc::POLLIN,
            revents: 0,
        }];
        {
            let registry = lock();
            if registry.owner != owner {
                return;
            }
            for entry in &registry.entries {
                what.push(What::Listener(entry.address, entry.listener));
                polled.push(libc::pollfd {
                    fd: entry.listener,
                    events: libc::POLLIN,
                    revents: 0,
                });
                for &claim in &entry.claims {
                    what.push(What::Claim(entry.address, claim));
                    polled.push(libc::pollfd {
                        fd: claim,
                        events: libc::POLLIN,
                        revents: 0,
                    });
                }
            }
        }
        // SAFETY: `polled` is exactly the live `pollfd`s the count names.
        let rc = unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, -1) };
        if rc < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return;
        }

        let mut due = Vec::new();
        {
            let mut registry = lock();
            if registry.owner != owner {
                return;
            }
            for (item, poll) in what.iter().zip(&polled) {
                if poll.revents == 0 {
                    continue;
                }
                match item {
                    What::Wake => {
                        let mut drain = [0u8; 64];
                        // SAFETY: a live buffer of the length given.
                        while unsafe { libc::read(wake_read, drain.as_mut_ptr().cast(), 64) } > 0 {}
                    }
                    What::Listener(address, fd) => {
                        // Looked up again: the entry may have gone, and its
                        // descriptor number been reused, while this poll waited.
                        let Some(entry) = registry
                            .entries
                            .iter_mut()
                            .find(|e| e.address == *address && e.listener == *fd)
                        else {
                            continue;
                        };
                        loop {
                            // SAFETY: the registry's own listener; no peer
                            // address is wanted.
                            let claim = unsafe { accept(*fd) };
                            let Some(claim) = claim else { break };
                            entry.claims.push(claim);
                            entry.made = entry.made.saturating_add(1);
                        }
                    }
                    What::Claim(address, fd) => {
                        let Some(entry) = registry
                            .entries
                            .iter_mut()
                            .find(|e| e.address == *address && e.claims.contains(fd))
                        else {
                            continue;
                        };
                        if closed(*fd) {
                            entry.claims.retain(|c| c != fd);
                            // SAFETY: a claim this registry accepted.
                            unsafe { libc::close(*fd) };
                        }
                    }
                }
            }
            for entry in &mut registry.entries {
                let ready = matches!(
                    &entry.request,
                    Some(request) if entry.complete
                        && entry.claims.is_empty()
                        && entry.made > request.sync
                );
                if ready && let Some(request) = entry.request.take() {
                    due.push((request.notify, entry.made));
                }
            }
        }
        for (notify, made) in due {
            super::msg::deliver_no_senders(notify, made);
        }
    }
}

/// Accept one pending claim without waiting.
///
/// # Safety
///
/// `listener` is a listening socket.
unsafe fn accept(listener: c_int) -> Option<c_int> {
    loop {
        // SAFETY: the caller's listener.
        let fd = unsafe { accept_any(listener) }?;
        // A claim from another user would hold off no-senders forever.
        #[cfg(target_os = "linux")]
        if !peer_is_this_user(fd) {
            // SAFETY: the descriptor just accepted, never handed on.
            unsafe { libc::close(fd) };
            continue;
        }
        return Some(fd);
    }
}

/// Accept one pending connection from anyone without waiting.
///
/// # Safety
///
/// `listener` is a listening socket.
unsafe fn accept_any(listener: c_int) -> Option<c_int> {
    #[cfg(not(target_os = "macos"))]
    // SAFETY: the caller's listener; null address pointers are allowed.
    let fd = unsafe {
        libc::accept4(
            listener,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: the caller's listener; null address pointers are allowed.
    let fd = unsafe { libc::accept(listener, core::ptr::null_mut(), core::ptr::null_mut()) };
    if fd < 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    // SAFETY: the descriptor just accepted.
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    Some(fd)
}

/// Whether the process that connected `fd` runs as this user.
#[cfg(target_os = "linux")]
fn peer_is_this_user(fd: c_int) -> bool {
    // SAFETY: `ucred` is plain integers.
    let mut credentials: libc::ucred = unsafe { core::mem::zeroed() };
    let mut len = core::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: a `ucred` out-parameter of the length given.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut len,
        )
    };
    rc == 0 && super::wire::same_user(credentials.uid)
}

/// Whether the holder end of a claim has gone. Nothing is ever written to a
/// claim, so anything but "would block" means it closed.
fn closed(fd: c_int) -> bool {
    let mut byte = 0u8;
    loop {
        // SAFETY: one byte into a live local, on a non-blocking descriptor.
        let rc = unsafe { libc::recv(fd, (&raw mut byte).cast(), 1, libc::MSG_DONTWAIT) };
        if rc > 0 {
            continue;
        }
        if rc == 0 {
            return true;
        }
        let error = std::io::Error::last_os_error().raw_os_error();
        return !matches!(error, Some(libc::EAGAIN) | Some(libc::EINTR));
    }
}
