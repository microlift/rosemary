use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use static_init::dynamic;

use crate::mach::space::{self, Name};
use crate::mach::wire::Receiver;

/// `struct kevent`, which the guest both fills in and reads back.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Kevent {
    pub ident: usize,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
    pub data: isize,
    pub udata: usize,
}

const _: () = {
    assert!(size_of::<Kevent>() == 32);
    assert!(std::mem::offset_of!(Kevent, filter) == 8);
    assert!(std::mem::offset_of!(Kevent, data) == 16);
    assert!(std::mem::offset_of!(Kevent, udata) == 24);
};

const EVFILT_READ: i16 = -1;
const EVFILT_WRITE: i16 = -2;
const EVFILT_PROC: i16 = -5;
const EVFILT_SIGNAL: i16 = -6;
const EVFILT_TIMER: i16 = -7;
const EVFILT_MACHPORT: i16 = -8;
const EVFILT_USER: i16 = -10;

const EV_ADD: u16 = 0x0001;
const EV_DELETE: u16 = 0x0002;
const EV_ENABLE: u16 = 0x0004;
const EV_DISABLE: u16 = 0x0008;
const EV_ONESHOT: u16 = 0x0010;
const EV_CLEAR: u16 = 0x0020;
const EV_RECEIPT: u16 = 0x0040;
const EV_DISPATCH: u16 = 0x0080;
const EV_ERROR: u16 = 0x4000;
const EV_EOF: u16 = 0x8000;

/// `NOTE_TRIGGER`, the one `fflags` bit `EVFILT_USER` acts on.
const NOTE_TRIGGER: u32 = 0x0100_0000;

/// `EVFILT_PROC`'s notes. Only the exit is observable from outside a process
/// on this host - a pidfd says when it ends - so a registration for its forks,
/// execs or signals is accepted, as XNU accepts it, and those never fire.
const NOTE_EXIT: u32 = 0x8000_0000;
const NOTE_EXITSTATUS: u32 = 0x0400_0000;

/// `EVFILT_TIMER`'s units, epoch and clock, from XNU's `sys/event.h`. With
/// none of the four units the period is in milliseconds. Mach time units are
/// nanoseconds here, as `mach_timebase_info` says.
const NOTE_SECONDS: u32 = 0x0000_0001;
const NOTE_USECONDS: u32 = 0x0000_0002;
const NOTE_NSECONDS: u32 = 0x0000_0004;
const NOTE_ABSOLUTE: u32 = 0x0000_0008;
const NOTE_LEEWAY: u32 = 0x0000_0010;
const NOTE_MACH_CONTINUOUS_TIME: u32 = 0x0000_0080;
const NOTE_MACHTIME: u32 = 0x0000_0100;

/// `MACH_RCV_MSG` in an `EVFILT_MACHPORT` registration's `fflags`: receive the
/// message into the event. Only `kevent64`/`kevent_qos` have room for it.
const MACH_RCV_MSG: u32 = 0x0000_0002;

/// What one `EV_ADD` recorded.
#[derive(Clone, Copy)]
struct Registration {
    ident: usize,
    filter: i16,
    flags: u16,
    fflags: u32,
    udata: usize,
    enabled: bool,
    /// `EVFILT_SIGNAL`: how many deliveries of the signal were already
    /// reported, or happened before the registration.
    seen: u64,
    /// `kevent64_s::ext`, handed back with every event as XNU's
    /// `knote_fill_kevent` does. For an `EVFILT_MACHPORT` registration with
    /// `MACH_RCV_MSG`, the buffer a message is received into and its size.
    ext: [u64; 2],
    /// `EVFILT_TIMER`: when it next fires, in nanoseconds on its clock (see
    /// [`timer_now`]) - zero for at once, as XNU's `kn_ext[0]` - or `None` for
    /// a one-shot that has fired and waits to be armed again.
    deadline: Option<u64>,
    /// `EVFILT_TIMER`: the period of a repeating timer, zero for one that
    /// fires once.
    interval: u64,
}

impl Registration {
    /// The event to hand back when this registration fires.
    ///
    /// `NOTE_TRIGGER` is cleared: it is the caller's *request*, and a caller
    /// that read it back in a delivered event would see its own trigger as a
    /// property of the event.
    ///
    /// The registration's own `EV_ONESHOT`, `EV_CLEAR` and `EV_DISPATCH` are
    /// echoed, as XNU's `knote_fill_kevent` copies the knote's flags into the
    /// event: a caller that muxes several registrations - libdispatch does -
    /// reads them back to know whether the knote is now disabled or gone.
    fn fired(&self) -> Kevent {
        Kevent {
            ident: self.ident,
            filter: self.filter,
            flags: self.flags & (EV_ONESHOT | EV_CLEAR | EV_DISPATCH),
            fflags: self.fflags & !NOTE_TRIGGER,
            data: 0,
            udata: self.udata,
        }
    }
}

struct Queue<B> {
    registrations: HashMap<(usize, i16), Registration>,
    /// The Mach receive sockets the backend watches for `EV_CLEAR`
    /// `EVFILT_MACHPORT` registrations, by descriptor. Holding the receiver
    /// keeps the descriptor open, so its number cannot be reused while the
    /// host still watches it.
    port_watch: HashMap<c_int, Arc<Receiver>>,
    /// Triggered `EVFILT_USER` idents, in the order they were triggered, and
    /// the set form of the same thing - a second trigger before the first is
    /// delivered is one event, not two.
    triggered: VecDeque<usize>,
    triggered_set: HashSet<usize>,
    /// A pidfd for each `EVFILT_PROC` registration, by pid: readable once the
    /// process has exited.
    processes: HashMap<usize, std::os::fd::OwnedFd>,
    backend: Arc<B>,
}

impl<B> Drop for Queue<B> {
    fn drop(&mut self) {
        for registration in self.registrations.values() {
            if registration.filter == EVFILT_SIGNAL {
                let _ = crate::signal::observe(registration.ident as i32, false);
            }
        }
    }
}

impl<B> Queue<B> {
    fn new(backend: B) -> Self {
        Self {
            registrations: HashMap::new(),
            port_watch: HashMap::new(),
            triggered: VecDeque::new(),
            triggered_set: HashSet::new(),
            processes: HashMap::new(),
            backend: Arc::new(backend),
        }
    }
}

/// What a descriptor is watched for, as `POLLIN`/`POLLOUT` bits: `level` for
/// plain registrations, `edge` for `EV_CLEAR` ones.
///
/// Edges come from the host's own edge triggering. Rebuilding them from level
/// readiness would lose the data that arrives after a caller drains a
/// descriptor and before its next `kevent`.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Interest {
    level: i16,
    edge: i16,
}

/// Where a ready descriptor was reported from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The private wakeup channel.
    Wake,
    Level,
    Edge,
    /// A Mach receive socket of an `EV_CLEAR` `EVFILT_MACHPORT`, which is never
    /// one of the guest's descriptors.
    Port,
}

/// A ready descriptor reported by one concrete host readiness mechanism.
#[derive(Clone, Copy)]
struct Ready {
    fd: c_int,
    revents: i16,
    source: Source,
}

/// The host-specific part of a kqueue. `Queue<B>` is monomorphized with the
/// selected backend; this is deliberately not a trait object or a run-time
/// host capability choice.
trait ReadinessBackend: Sized {
    fn new() -> Result<(Self, c_int), c_int>;
    fn wake(&self);
    /// The descriptor `wake` writes, for a signal handler to write.
    fn wake_descriptor(&self) -> c_int;
    fn drain_wake(&self);
    /// Whether the host can report edges, which `EV_CLEAR` needs.
    const EDGE: bool;
    fn update(&self, fd: c_int, before: Interest, after: Interest) -> Result<(), c_int>;
    /// Watch, or stop watching, a Mach receive socket for edges.
    fn watch_port(&self, fd: c_int, watch: bool) -> Result<(), c_int>;
    /// `ports` are Mach receive sockets that end the wait when readable but
    /// are never reported: `kevent` asks the ports themselves afterwards.
    fn wait(
        &self,
        queue: c_int,
        registrations: &HashMap<(usize, i16), Registration>,
        ports: &[c_int],
        capacity: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<Ready>, c_int>;
}

/// The portable reconstruction of a readiness set. It remains the complete
/// implementation on hosts with neither epoll nor a Darwin kqueue.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct PollBackend {
    wake: c_int,
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl ReadinessBackend for PollBackend {
    fn new() -> Result<(Self, c_int), c_int> {
        let wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if wake < 0 {
            Err(crate::errno::get())
        } else {
            Ok((Self { wake }, wake))
        }
    }

    fn wake(&self) {
        wake_fd(self.wake)
    }
    fn wake_descriptor(&self) -> c_int {
        self.wake
    }
    fn drain_wake(&self) {
        drain_fd(self.wake)
    }
    // `poll` has no edges, and an `EV_CLEAR` answered with level readiness
    // spins a caller that only reads on a change.
    const EDGE: bool = false;
    fn update(&self, _: c_int, _: Interest, after: Interest) -> Result<(), c_int> {
        match after.edge {
            0 => Ok(()),
            _ => Err(libc::EINVAL),
        }
    }
    fn watch_port(&self, _: c_int, _: bool) -> Result<(), c_int> {
        Err(libc::EINVAL)
    }

    fn wait(
        &self,
        queue: c_int,
        registrations: &HashMap<(usize, i16), Registration>,
        ports: &[c_int],
        _: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<Ready>, c_int> {
        let mut watched = poll_set(queue, registrations);
        let registered = watched.len();
        watched.extend(ports.iter().map(|fd| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        }));
        let ready = poll_for(&mut watched, timeout);
        if ready < 0 {
            return Err(crate::errno::get());
        }
        Ok(watched
            .into_iter()
            .take(registered)
            .filter(|entry| entry.revents != 0)
            .map(|entry| Ready {
                fd: entry.fd,
                revents: entry.revents,
                source: match entry.fd == queue {
                    true => Source::Wake,
                    false => Source::Level,
                },
            })
            .collect())
    }
}

#[cfg(target_os = "linux")]
struct EpollBackend {
    epoll: c_int,
    /// A second epoll holding the `EPOLLET` entries, itself watched by
    /// `epoll`: one epoll entry per descriptor cannot be level for one filter
    /// and edge for the other.
    edge: c_int,
    wake: c_int,
    poll_only: Mutex<HashSet<c_int>>,
}

#[cfg(target_os = "linux")]
impl Drop for EpollBackend {
    fn drop(&mut self) {
        // This is the private wakeup descriptor, never the guest-visible
        // kqueue descriptor (which the guest owns and may already have closed).
        forget_waker(self.wake);
        unsafe {
            libc::close(self.wake);
            libc::close(self.edge);
        }
    }
}

#[cfg(target_os = "linux")]
impl EpollBackend {
    const WAKE_TOKEN: u64 = u64::MAX;
    /// The token `edge` itself is registered in `epoll` under.
    const EDGE_TOKEN: u64 = u64::MAX - 1;
    /// Set in an `edge` entry's token for a Mach receive socket.
    const PORT_BIT: u64 = 1 << 32;

    fn ctl(epoll: c_int, op: c_int, fd: c_int, events: u32, token: u64) -> Result<(), c_int> {
        let mut event = libc::epoll_event { events, u64: token };
        if unsafe { libc::epoll_ctl(epoll, op, fd, &raw mut event) } < 0 {
            Err(crate::errno::get())
        } else {
            Ok(())
        }
    }

    fn events(interest: i16) -> u32 {
        ((interest & libc::POLLIN != 0) as u32 * libc::EPOLLIN as u32)
            | ((interest & libc::POLLOUT != 0) as u32 * libc::EPOLLOUT as u32)
    }

    /// Move `fd`'s entry in `epoll` from `before` to `after`.
    fn diff(epoll: c_int, fd: c_int, before: i16, after: i16, extra: u32) -> Result<(), c_int> {
        let events = Self::events(after) | extra;
        let result = match (before != 0, after != 0) {
            (false, true) => Self::ctl(epoll, libc::EPOLL_CTL_ADD, fd, events, fd as u64),
            (true, true) => Self::ctl(epoll, libc::EPOLL_CTL_MOD, fd, events, fd as u64),
            (true, false) => Self::ctl(epoll, libc::EPOLL_CTL_DEL, fd, 0, 0),
            (false, false) => Ok(()),
        };
        match result {
            // Closing a watched FD removes it from epoll. Keep Darwin's
            // deletion semantics even if the host has already done that work.
            Err(errno) if after == 0 && matches!(errno, libc::ENOENT | libc::EBADF) => Ok(()),
            other => other,
        }
    }
}

#[cfg(target_os = "linux")]
impl ReadinessBackend for EpollBackend {
    fn new() -> Result<(Self, c_int), c_int> {
        let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epoll < 0 {
            return Err(crate::errno::get());
        }
        let edge = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if edge < 0 {
            let errno = crate::errno::get();
            unsafe { libc::close(epoll) };
            return Err(errno);
        }
        let wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if wake < 0 {
            let errno = crate::errno::get();
            unsafe {
                libc::close(edge);
                libc::close(epoll);
            }
            return Err(errno);
        }
        // From here `Drop` closes `wake` and `edge`; only the guest-visible
        // epoll needs closing by hand on failure.
        let backend = Self {
            epoll,
            edge,
            wake,
            poll_only: Mutex::new(HashSet::new()),
        };
        let registered = Self::ctl(
            epoll,
            libc::EPOLL_CTL_ADD,
            wake,
            libc::EPOLLIN as u32,
            Self::WAKE_TOKEN,
        )
        .and_then(|()| {
            Self::ctl(
                epoll,
                libc::EPOLL_CTL_ADD,
                edge,
                libc::EPOLLIN as u32,
                Self::EDGE_TOKEN,
            )
        });
        if let Err(errno) = registered {
            drop(backend);
            unsafe { libc::close(epoll) };
            return Err(errno);
        }
        Ok((backend, epoll))
    }

    fn wake(&self) {
        wake_fd(self.wake)
    }
    fn wake_descriptor(&self) -> c_int {
        self.wake
    }
    fn drain_wake(&self) {
        drain_fd(self.wake)
    }

    const EDGE: bool = true;

    fn update(&self, fd: c_int, before: Interest, after: Interest) -> Result<(), c_int> {
        if before.edge != after.edge {
            match Self::diff(self.edge, fd, before.edge, after.edge, libc::EPOLLET as u32) {
                // A regular file, which epoll refuses. It is always ready, so
                // it has no edges to report; refused rather than reported
                // once and then never.
                Err(libc::EPERM) => return Err(libc::EINVAL),
                other => other?,
            }
        }
        if before.level == after.level {
            return Ok(());
        }
        let mut poll_only = self.poll_only.lock().unwrap_or_else(|e| e.into_inner());
        if poll_only.contains(&fd) {
            if after.level == 0 {
                poll_only.remove(&fd);
            }
            return Ok(());
        }
        match Self::diff(self.epoll, fd, before.level, after.level, 0) {
            Err(errno) if before.level == 0 && errno == libc::EPERM => {
                poll_only.insert(fd);
                Ok(())
            }
            other => other,
        }
    }

    fn watch_port(&self, fd: c_int, watch: bool) -> Result<(), c_int> {
        let token = fd as u64 | Self::PORT_BIT;
        let events = (libc::EPOLLIN | libc::EPOLLET) as u32;
        match watch {
            true => Self::ctl(self.edge, libc::EPOLL_CTL_ADD, fd, events, token),
            false => match Self::ctl(self.edge, libc::EPOLL_CTL_DEL, fd, 0, 0) {
                Err(libc::ENOENT | libc::EBADF) => Ok(()),
                other => other,
            },
        }
    }

    fn wait(
        &self,
        queue: c_int,
        registrations: &HashMap<(usize, i16), Registration>,
        ports: &[c_int],
        capacity: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<Ready>, c_int> {
        let mut result = Vec::new();
        let poll_only = self
            .poll_only
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if poll_only.is_empty() && ports.is_empty() {
            return self.wait_epoll(queue, capacity, timeout);
        }
        let mut watched = Vec::with_capacity(poll_only.len() + ports.len() + 1);
        watched.push(libc::pollfd {
            fd: queue,
            events: libc::POLLIN,
            revents: 0,
        });
        for fd in &poll_only {
            let interest = interest_for(registrations, *fd).level;
            if interest != 0 {
                watched.push(libc::pollfd {
                    fd: *fd,
                    events: interest,
                    revents: 0,
                });
            }
        }
        let registered = watched.len();
        watched.extend(ports.iter().map(|fd| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        }));
        if poll_for(&mut watched, timeout) < 0 {
            return Err(crate::errno::get());
        }
        for entry in watched[..registered].iter().skip(1) {
            if entry.revents != 0 {
                result.push(Ready {
                    fd: entry.fd,
                    revents: entry.revents,
                    source: Source::Level,
                });
            }
        }
        if watched[0].revents != 0 {
            result.extend(self.wait_epoll(
                queue,
                capacity.saturating_sub(result.len()).max(1),
                Some(Duration::ZERO),
            )?);
        }
        Ok(result)
    }
}

#[cfg(target_os = "linux")]
impl EpollBackend {
    fn wait_epoll(
        &self,
        queue: c_int,
        capacity: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<Ready>, c_int> {
        // One slot more than the caller wants, so the wakeup or the edge
        // epoll cannot crowd out a ready descriptor.
        let mut result = Vec::new();
        for event in Self::collect(queue, capacity + 1, timeout)? {
            match event.u64 {
                Self::WAKE_TOKEN => result.push(Ready {
                    fd: self.wake,
                    revents: libc::POLLIN,
                    source: Source::Wake,
                }),
                Self::EDGE_TOKEN => {
                    for edge in Self::collect(self.edge, capacity.max(1), Some(Duration::ZERO))? {
                        result.push(Ready {
                            fd: edge.u64 as u32 as c_int,
                            revents: epoll_revents(edge.events),
                            source: match edge.u64 & Self::PORT_BIT != 0 {
                                true => Source::Port,
                                false => Source::Edge,
                            },
                        });
                    }
                }
                token => result.push(Ready {
                    fd: token as c_int,
                    revents: epoll_revents(event.events),
                    source: Source::Level,
                }),
            }
        }
        Ok(result)
    }

    fn collect(
        epoll: c_int,
        capacity: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<libc::epoll_event>, c_int> {
        let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; capacity];
        let count = epoll_wait_for(epoll, &mut events, timeout);
        if count < 0 {
            return Err(crate::errno::get());
        }
        events.truncate(count as usize);
        Ok(events)
    }
}

#[cfg(target_os = "linux")]
fn epoll_revents(events: u32) -> i16 {
    ((events & libc::EPOLLIN as u32 != 0) as i16 * libc::POLLIN)
        | ((events & libc::EPOLLOUT as u32 != 0) as i16 * libc::POLLOUT)
        | ((events & libc::EPOLLERR as u32 != 0) as i16 * libc::POLLERR)
        | ((events & libc::EPOLLHUP as u32 != 0) as i16 * libc::POLLHUP)
}

/// The host's own kqueue. Guest signals are emulated, so `EVFILT_SIGNAL` and
/// `EVFILT_USER` stay in `Queue`; the host queue only answers readiness, and
/// is what the guest holds, so polling it reports watched descriptors.
#[cfg(target_os = "macos")]
struct KqueueBackend {
    kqueue: c_int,
    /// A pipe rather than `EVFILT_USER`, so a signal handler can wake the
    /// queue with a plain `write` through `WAKER_FDS`.
    wake_read: c_int,
    wake_write: c_int,
}

#[cfg(target_os = "macos")]
impl Drop for KqueueBackend {
    fn drop(&mut self) {
        // The host queue is the guest-visible descriptor and the guest's to
        // close; only the private pipe is ours.
        forget_waker(self.wake_write);
        unsafe {
            libc::close(self.wake_read);
            libc::close(self.wake_write);
        }
    }
}

#[cfg(target_os = "macos")]
impl KqueueBackend {
    /// The `udata` each host registration carries, so a reported event says
    /// which kind of registration it came from.
    const WAKE_TOKEN: usize = usize::MAX;
    const LEVEL_TOKEN: usize = 0;
    const EDGE_TOKEN: usize = 1;
    const PORT_TOKEN: usize = 2;

    fn change(&self, fd: c_int, filter: i16, flags: u16, token: usize) -> Result<(), c_int> {
        let change = libc::kevent {
            ident: fd as usize,
            filter,
            flags,
            fflags: 0,
            data: 0,
            udata: token as *mut c_void,
        };
        let result = unsafe {
            libc::kevent(
                self.kqueue,
                &raw const change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if result < 0 {
            Err(crate::errno::get())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "macos")]
fn cloexec_nonblocking(fd: c_int) -> Result<(), c_int> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0
            || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
            || libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0
        {
            return Err(crate::errno::get());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
impl ReadinessBackend for KqueueBackend {
    fn new() -> Result<(Self, c_int), c_int> {
        let kqueue = unsafe { libc::kqueue() };
        if kqueue < 0 {
            return Err(crate::errno::get());
        }
        let mut ends = [0 as c_int; 2];
        if unsafe { libc::pipe(ends.as_mut_ptr()) } < 0 {
            let errno = crate::errno::get();
            unsafe { libc::close(kqueue) };
            return Err(errno);
        }
        // From here `Drop` closes the pipe; only the queue needs closing by
        // hand on failure.
        let backend = Self {
            kqueue,
            wake_read: ends[0],
            wake_write: ends[1],
        };
        let wake = libc::kevent {
            ident: backend.wake_read as usize,
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD,
            fflags: 0,
            data: 0,
            udata: Self::WAKE_TOKEN as *mut c_void,
        };
        // `O_NONBLOCK` on a kqueue is `ENOTTY`, and it is never read directly.
        let cloexec = match unsafe { libc::fcntl(kqueue, libc::F_SETFD, libc::FD_CLOEXEC) } {
            0 => Ok(()),
            _ => Err(crate::errno::get()),
        };
        let registered = cloexec
            .and_then(|()| cloexec_nonblocking(backend.wake_read))
            .and_then(|()| cloexec_nonblocking(backend.wake_write))
            .and_then(|()| {
                let result = unsafe {
                    libc::kevent(
                        kqueue,
                        &raw const wake,
                        1,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null(),
                    )
                };
                if result < 0 {
                    Err(crate::errno::get())
                } else {
                    Ok(())
                }
            });
        if let Err(errno) = registered {
            drop(backend);
            unsafe { libc::close(kqueue) };
            return Err(errno);
        }
        Ok((backend, kqueue))
    }

    fn wake(&self) {
        wake_fd(self.wake_write)
    }
    fn wake_descriptor(&self) -> c_int {
        self.wake_write
    }
    fn drain_wake(&self) {
        drain_fd(self.wake_read)
    }

    const EDGE: bool = true;

    fn update(&self, fd: c_int, before: Interest, after: Interest) -> Result<(), c_int> {
        /// `None` for unwatched, else whether the filter is edge-triggered.
        fn mode(interest: Interest, bit: i16) -> Option<bool> {
            match (interest.edge & bit != 0, interest.level & bit != 0) {
                (true, _) => Some(true),
                (false, true) => Some(false),
                (false, false) => None,
            }
        }
        for (bit, filter) in [
            (libc::POLLIN, libc::EVFILT_READ),
            (libc::POLLOUT, libc::EVFILT_WRITE),
        ] {
            let (was, now) = (mode(before, bit), mode(after, bit));
            if was == now {
                continue;
            }
            // Deleted and added again rather than modified, so the host
            // re-reads `EV_CLEAR` and the token rather than keeping the old.
            if was.is_some() {
                // Closing a watched descriptor removes its filters. Keep
                // Darwin's deletion semantics even if the host already has.
                match self.change(fd, filter, libc::EV_DELETE, 0) {
                    Err(errno) if !matches!(errno, libc::ENOENT | libc::EBADF) => {
                        return Err(errno);
                    }
                    _ => {}
                }
            }
            if let Some(edge) = now {
                let (flags, token) = match edge {
                    true => (libc::EV_ADD | libc::EV_CLEAR, Self::EDGE_TOKEN),
                    false => (libc::EV_ADD, Self::LEVEL_TOKEN),
                };
                self.change(fd, filter, flags, token)?;
            }
        }
        Ok(())
    }

    fn watch_port(&self, fd: c_int, watch: bool) -> Result<(), c_int> {
        match watch {
            true => self.change(
                fd,
                libc::EVFILT_READ,
                libc::EV_ADD | libc::EV_CLEAR,
                Self::PORT_TOKEN,
            ),
            false => match self.change(fd, libc::EVFILT_READ, libc::EV_DELETE, 0) {
                Err(libc::ENOENT | libc::EBADF) => Ok(()),
                other => other,
            },
        }
    }

    fn wait(
        &self,
        queue: c_int,
        _: &HashMap<(usize, i16), Registration>,
        ports: &[c_int],
        capacity: usize,
        mut timeout: Option<Duration>,
    ) -> Result<Vec<Ready>, c_int> {
        // The ports' sockets are not in the host queue (a port set's members
        // can change under a registration), so wait on both with `poll` - a
        // kqueue descriptor polls readable while it has events - and then
        // collect from the host queue without waiting.
        if !ports.is_empty() && timeout != Some(Duration::ZERO) {
            let mut watched: Vec<libc::pollfd> = std::iter::once(queue)
                .chain(ports.iter().copied())
                .map(|fd| libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            if poll_for(&mut watched, timeout) < 0 {
                return Err(crate::errno::get());
            }
            timeout = Some(Duration::ZERO);
        }
        let wait = host_timespec(timeout);
        let timeout = wait
            .as_ref()
            .map_or(std::ptr::null(), |wait| wait as *const _);
        // One slot more than the caller wants, so the wakeup cannot crowd
        // out a ready descriptor.
        let mut events = vec![
            libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            capacity + 1
        ];
        let count = unsafe {
            libc::kevent(
                queue,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as c_int,
                timeout,
            )
        };
        if count < 0 {
            return Err(crate::errno::get());
        }
        // The host reports each filter on its own; `kevent` matches every
        // registration on a descriptor against one entry, so fold them.
        let mut result: Vec<Ready> = Vec::new();
        for event in events.iter().take(count as usize) {
            let source = match event.udata as usize {
                Self::WAKE_TOKEN => Source::Wake,
                Self::EDGE_TOKEN => Source::Edge,
                Self::PORT_TOKEN => Source::Port,
                _ => Source::Level,
            };
            let fd = event.ident as c_int;
            let mut revents = match event.filter {
                libc::EVFILT_READ => libc::POLLIN,
                libc::EVFILT_WRITE => libc::POLLOUT,
                _ => 0,
            };
            if event.flags & libc::EV_EOF != 0 {
                revents |= libc::POLLHUP;
            }
            if event.flags & libc::EV_ERROR != 0 {
                revents = match event.data as c_int {
                    libc::EBADF => libc::POLLNVAL,
                    _ => libc::POLLERR,
                };
            }
            match result
                .iter_mut()
                .find(|r| source != Source::Wake && r.source == source && r.fd == fd)
            {
                Some(existing) => existing.revents |= revents,
                None => result.push(Ready {
                    fd,
                    revents,
                    source,
                }),
            }
        }
        Ok(result)
    }
}

#[cfg(target_os = "linux")]
type SelectedBackend = EpollBackend;
#[cfg(target_os = "macos")]
type SelectedBackend = KqueueBackend;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
type SelectedBackend = PollBackend;

fn queues() -> &'static Mutex<HashMap<c_int, Arc<Mutex<Queue<SelectedBackend>>>>> {
    #[dynamic]
    static QUEUES: Mutex<HashMap<c_int, Arc<Mutex<Queue<SelectedBackend>>>>> = Default::default();
    &QUEUES
}

fn queue_of(fd: c_int) -> Option<Arc<Mutex<Queue<SelectedBackend>>>> {
    queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&fd)
        .cloned()
}

/// `int kqueue(void)`
pub extern "C" fn kqueue() -> c_int {
    let Ok((backend, fd)) = SelectedBackend::new() else {
        return -1;
    };
    // Whatever this number held before is gone with the descriptor it named;
    // see the module note on `close`.
    queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(fd, Arc::new(Mutex::new(Queue::new(backend))));
    fd
}

/// Wake a `kevent` blocked on `queue`.
fn wake_fd(queue: c_int) {
    let one: u64 = 1;
    // SAFETY: an eventfd or non-blocking pipe this module created, and eight
    // bytes to write. A full pipe is already a pending wakeup.
    unsafe { libc::write(queue, (&raw const one).cast::<c_void>(), size_of::<u64>()) };
}

/// The wakeup descriptors of queues that watch signals. A signal handler may
/// take no lock, so this is a fixed table it can scan.
const WAKERS: usize = 64;
static WAKER_FDS: [AtomicI32; WAKERS] = [const { AtomicI32::new(-1) }; WAKERS];

fn register_waker(fd: c_int) -> Result<(), c_int> {
    if WAKER_FDS
        .iter()
        .any(|slot| slot.load(Ordering::Acquire) == fd)
    {
        return Ok(());
    }
    for slot in &WAKER_FDS {
        if slot
            .compare_exchange(-1, fd, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Ok(());
        }
    }
    Err(libc::ENOMEM)
}

/// Before the descriptor closes, so its number is not written once it names
/// something else.
fn forget_waker(fd: c_int) {
    for slot in &WAKER_FDS {
        let _ = slot.compare_exchange(fd, -1, Ordering::AcqRel, Ordering::Acquire);
    }
}

/// A watched signal arrived: wake every queue that watches signals, and let
/// each decide whether it is theirs. Async-signal-safe.
pub(crate) fn signalled() {
    for slot in &WAKER_FDS {
        let fd = slot.load(Ordering::Acquire);
        if fd >= 0 {
            wake_fd(fd);
        }
    }
}

/// Drain the wakeup channel, which is level-triggered and would otherwise
/// return from every later `poll` at once.
fn drain_fd(queue: c_int) {
    let mut value: u64 = 0;
    // SAFETY: as [`wake`]; the descriptor is non-blocking, so this ends with
    // `EAGAIN` rather than waiting.
    while unsafe { libc::read(queue, (&raw mut value).cast::<c_void>(), size_of::<u64>()) } > 0 {}
}

/// Apply one change, answering with the `errno` it failed with.
///
/// The number is **Darwin's**: it reaches the guest as an `EV_ERROR` event's
/// `data`, which is a value rather than the `errno` slot, so nothing later
/// translates it.
fn apply<B: ReadinessBackend>(
    state: &mut Queue<B>,
    change: &Kevent,
    ext: [u64; 2],
    wide: bool,
) -> Result<(), c_int> {
    if !matches!(
        change.filter,
        EVFILT_READ
            | EVFILT_WRITE
            | EVFILT_USER
            | EVFILT_SIGNAL
            | EVFILT_TIMER
            | EVFILT_MACHPORT
            | EVFILT_PROC
    ) {
        return Err(crate::errno::from_host(libc::EINVAL));
    }
    let key = (change.ident, change.filter);

    let mut registrations = state.registrations.clone();
    let mut triggered = state.triggered.clone();
    let mut triggered_set = state.triggered_set.clone();

    if change.flags & EV_DELETE != 0 {
        if registrations.remove(&key).is_none() {
            return Err(crate::errno::from_host(libc::ENOENT));
        }
        triggered.retain(|ident| *ident != change.ident);
        triggered_set.remove(&change.ident);
        sync_interest(state, &registrations, change.filter, change.ident)?;
        state.registrations = registrations;
        state.triggered = triggered;
        state.triggered_set = triggered_set;
        if change.filter == EVFILT_SIGNAL {
            let _ = crate::signal::observe(change.ident as i32, false);
        }
        if change.filter == EVFILT_PROC {
            state.processes.remove(&change.ident);
        }
        return Ok(());
    }

    if change.flags & EV_ADD != 0
        && change.flags & EV_CLEAR != 0
        && matches!(change.filter, EVFILT_READ | EVFILT_WRITE | EVFILT_MACHPORT)
        && !B::EDGE
    {
        return Err(crate::errno::from_host(libc::EINVAL));
    }

    if change.flags & EV_ADD != 0 {
        // A readiness filter names a descriptor, and one that is not open is
        // `EBADF` at registration rather than at the first `poll` - which is
        // where BSD reports it, and the only place a caller looks.
        let mut seen = 0;
        let mut flags = change.flags;
        let (mut deadline, mut interval) = (None, 0);
        if change.filter == EVFILT_TIMER {
            // XNU's `filt_timertouch` refuses to turn a timer between relative
            // and absolute; anything else re-arms it from now, and an expiry
            // it had not yet delivered goes with the old setting.
            if registrations
                .get(&key)
                .is_some_and(|existing| (existing.fflags ^ change.fflags) & NOTE_ABSOLUTE != 0)
            {
                return Err(crate::errno::from_host(libc::EINVAL));
            }
            let (at, period) = timer_params(change.fflags, change.data, ext[1])?;
            (deadline, interval) = (Some(at), period);
            // `filt_timerattach`: a timer is always `EV_CLEAR`, and an absolute
            // one always `EV_ONESHOT`.
            flags |= EV_CLEAR;
            if change.fflags & NOTE_ABSOLUTE != 0 {
                flags |= EV_ONESHOT;
            }
        } else if change.filter == EVFILT_SIGNAL {
            let darwin =
                i32::try_from(change.ident).map_err(|_| crate::errno::from_host(libc::EINVAL))?;
            seen = match registrations.get(&key) {
                Some(existing) => existing.seen,
                None => {
                    register_waker(state.backend.wake_descriptor())
                        .map_err(crate::errno::from_host)?;
                    crate::signal::observe(darwin, true).map_err(crate::errno::from_host)?;
                    crate::signal::delivered(darwin)
                }
            };
        } else if change.filter == EVFILT_MACHPORT {
            // Receiving into the event needs the `ext` words only `kevent64`
            // has; without them there is nowhere to put the message.
            if change.fflags & MACH_RCV_MSG != 0 && !wide {
                return Err(crate::errno::from_host(libc::EINVAL));
            }
            let name =
                Name::try_from(change.ident).map_err(|_| crate::errno::from_host(libc::EINVAL))?;
            if space::receiver(name).is_none() && space::members(name).is_none() {
                return Err(crate::errno::from_host(libc::EINVAL));
            }
        } else if change.filter == EVFILT_PROC {
            // XNU's `filt_procattach`: a pid with no process is `ESRCH`.
            if !state.processes.contains_key(&change.ident) {
                let pidfd = open_pidfd(change.ident).map_err(crate::errno::from_host)?;
                state.processes.insert(change.ident, pidfd);
            }
        } else if change.filter != EVFILT_USER && change.filter != EVFILT_TIMER {
            let fd =
                c_int::try_from(change.ident).map_err(|_| crate::errno::from_host(libc::EBADF))?;
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
                return Err(crate::errno::from_host(crate::errno::get()));
            }
        }
        registrations.insert(
            key,
            Registration {
                ident: change.ident,
                filter: change.filter,
                flags,
                fflags: change.fflags,
                udata: change.udata,
                enabled: change.flags & EV_DISABLE == 0,
                seen,
                ext,
                deadline,
                interval,
            },
        );
    } else {
        let Some(registration) = registrations.get_mut(&key) else {
            return Err(crate::errno::from_host(libc::ENOENT));
        };
        // XNU hands every change to a registration that exists to the filter's
        // `f_touch`, `EV_ADD` or not, and `filt_timertouch` re-arms the timer
        // from the change's `data` and `fflags`.
        if change.filter == EVFILT_TIMER {
            if (registration.fflags ^ change.fflags) & NOTE_ABSOLUTE != 0 {
                return Err(crate::errno::from_host(libc::EINVAL));
            }
            let (at, period) = timer_params(change.fflags, change.data, ext[1])?;
            registration.deadline = Some(at);
            registration.interval = period;
            registration.fflags = change.fflags;
            registration.udata = change.udata;
            registration.ext = ext;
        }
        if change.flags & EV_ENABLE != 0 {
            registration.enabled = true;
        }
        if change.flags & EV_DISABLE != 0 {
            registration.enabled = false;
        }
    }

    if change.filter == EVFILT_USER && change.fflags & NOTE_TRIGGER != 0 {
        let enabled = registrations
            .get(&key)
            .is_some_and(|registration| registration.enabled);
        if enabled && triggered_set.insert(change.ident) {
            triggered.push_back(change.ident);
        }
    }
    sync_interest(state, &registrations, change.filter, change.ident)?;
    if change.filter == EVFILT_MACHPORT {
        sync_ports(&mut state.port_watch, &*state.backend, &registrations)?;
    }
    // A timer armed, re-armed or enabled moves the deadline a waiter on
    // another thread computed its wait from.
    let woke = (change.filter == EVFILT_USER
        && change.fflags & NOTE_TRIGGER != 0
        && triggered != state.triggered)
        || change.filter == EVFILT_TIMER;
    state.registrations = registrations;
    state.triggered = triggered;
    state.triggered_set = triggered_set;
    if woke {
        state.backend.wake();
    }
    Ok(())
}

/// The clock a timer registered with `fflags` runs on, in nanoseconds:
/// `mach_continuous_time`'s under `NOTE_MACH_CONTINUOUS_TIME`, which keeps
/// counting while the machine sleeps, and `mach_absolute_time`'s otherwise.
fn timer_now(fflags: u32) -> u64 {
    match fflags & NOTE_MACH_CONTINUOUS_TIME != 0 {
        true => crate::mach::continuous(),
        false => crate::mach::absolute(),
    }
}

/// XNU's `filt_timervalidate`: the deadline on [`timer_now`]'s clock and the
/// period a timer registration asks for, or Darwin's `EINVAL` for units that
/// contradict one another and `ERANGE` for an amount that overflows them.
///
/// - Relative, the default: `data` units from now, repeating every `data`.
///   A negative `data` fires at once and does not repeat - "certain clients
///   pass negative values on accident, and expect an event back".
/// - `NOTE_ABSOLUTE`: a moment on the calendar (`gettimeofday`'s epoch), or
///   under `NOTE_MACHTIME` on the timer's own clock, fired once; one already
///   past fires at once.
///
/// The leeway is checked as XNU checks it and then not used: a timer here is
/// never coalesced, so it fires no later than asked.
fn timer_params(fflags: u32, data: isize, leeway: u64) -> Result<(u64, u64), c_int> {
    let multiplier: u64 =
        match fflags & (NOTE_SECONDS | NOTE_USECONDS | NOTE_NSECONDS | NOTE_MACHTIME) {
            NOTE_SECONDS => 1_000_000_000,
            NOTE_USECONDS => 1_000,
            NOTE_NSECONDS | NOTE_MACHTIME => 1,
            0 => 1_000_000,
            _ => return Err(crate::errno::from_host(libc::EINVAL)),
        };
    let machtime = fflags & NOTE_MACHTIME != 0;
    let range = || crate::errno::from_host(libc::ERANGE);
    if fflags & NOTE_LEEWAY != 0 && !machtime {
        leeway.checked_mul(multiplier).ok_or_else(range)?;
    }
    let now = timer_now(fflags);
    if fflags & NOTE_ABSOLUTE != 0 {
        let deadline = match machtime {
            true => data as u64,
            false => {
                let at = (data as u64).checked_mul(multiplier).ok_or_else(range)?;
                let calendar = calendar_now();
                match calendar < at {
                    true => now.saturating_add(at - calendar),
                    false => 0,
                }
            }
        };
        return Ok((deadline, 0));
    }
    if data < 0 {
        return Ok((0, 0));
    }
    let interval = (data as u64).checked_mul(multiplier).ok_or_else(range)?;
    Ok((now.saturating_add(interval), interval))
}

/// Nanoseconds since 1970, which is what an absolute timer's deadline counts.
fn calendar_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos().min(u64::MAX as u128) as u64)
}

/// Whether an enabled timer is due, as XNU's `filt_timer_is_ready`.
fn timer_due(registration: &Registration) -> bool {
    registration.filter == EVFILT_TIMER
        && registration.enabled
        && registration
            .deadline
            .is_some_and(|deadline| deadline <= timer_now(registration.fflags))
}

/// How long until the first enabled timer is due, or `None` for a queue with
/// none armed.
fn next_timer(registrations: &HashMap<(usize, i16), Registration>) -> Option<Duration> {
    registrations
        .values()
        .filter(|r| r.filter == EVFILT_TIMER && r.enabled)
        .filter_map(|r| {
            let deadline = r.deadline?;
            Some(Duration::from_nanos(
                deadline.saturating_sub(timer_now(r.fflags)),
            ))
        })
        .min()
}

fn interest_for(registrations: &HashMap<(usize, i16), Registration>, fd: c_int) -> Interest {
    registrations
        .values()
        .filter(|registration| {
            registration.enabled
                && registration.ident == fd as usize
                && matches!(registration.filter, EVFILT_READ | EVFILT_WRITE)
        })
        .fold(Interest::default(), |mut interest, registration| {
            let bit = match registration.filter {
                EVFILT_READ => libc::POLLIN,
                EVFILT_WRITE => libc::POLLOUT,
                _ => 0,
            };
            match registration.flags & EV_CLEAR != 0 {
                true => interest.edge |= bit,
                false => interest.level |= bit,
            }
            interest
        })
}

/// Point the backend's port watches at the receive rights the enabled
/// `EV_CLEAR` `EVFILT_MACHPORT` registrations name now.
///
/// Run after every change to one of them, so a re-enabled registration is
/// watched afresh - which reports a message already waiting, as Darwin's
/// `EV_ENABLE` does - and before every wait, since a port set's members
/// change without telling the queue.
fn sync_ports<B: ReadinessBackend>(
    watch: &mut HashMap<c_int, Arc<Receiver>>,
    backend: &B,
    registrations: &HashMap<(usize, i16), Registration>,
) -> Result<(), c_int> {
    let wanted: HashMap<c_int, Arc<Receiver>> = registrations
        .values()
        .filter(|r| r.filter == EVFILT_MACHPORT && r.enabled && r.flags & EV_CLEAR != 0)
        .flat_map(|r| receivers(r.ident as Name))
        .map(|(_, port)| (port.fd(), port))
        .collect();
    // Removed while `watch` still holds the receiver, so the descriptor named
    // is still the socket that was watched.
    watch.retain(|fd, port| {
        let keep = wanted
            .get(fd)
            .is_some_and(|wanted| Arc::ptr_eq(wanted, port));
        if !keep {
            let _ = backend.watch_port(*fd, false);
        }
        keep
    });
    for (fd, port) in wanted {
        if watch.contains_key(&fd) {
            continue;
        }
        backend
            .watch_port(fd, true)
            .map_err(crate::errno::from_host)?;
        watch.insert(fd, port);
    }
    Ok(())
}

fn sync_interest<B: ReadinessBackend>(
    state: &mut Queue<B>,
    registrations: &HashMap<(usize, i16), Registration>,
    filter: i16,
    ident: usize,
) -> Result<(), c_int> {
    if matches!(
        filter,
        EVFILT_USER | EVFILT_SIGNAL | EVFILT_TIMER | EVFILT_MACHPORT | EVFILT_PROC
    ) {
        return Ok(());
    }
    let fd = c_int::try_from(ident).map_err(|_| crate::errno::from_host(libc::EBADF))?;
    state
        .backend
        .update(
            fd,
            interest_for(&state.registrations, fd),
            interest_for(registrations, fd),
        )
        .map_err(crate::errno::from_host)
}

/// The caller's `timespec` as a wait, or `None` for "wait forever". XNU's
/// `kevent` keeps the nanoseconds, and so does every wait below but `poll`'s.
fn timeout_of(timeout: *const libc::timespec) -> Result<Option<Duration>, c_int> {
    // SAFETY: null or the caller's `struct timespec`, whose layout is the same
    // two 64-bit words on both platforms.
    let Some(timeout) = (unsafe { timeout.as_ref() }) else {
        return Ok(None);
    };
    if timeout.tv_sec < 0 || !(0..1_000_000_000).contains(&timeout.tv_nsec) {
        return Err(crate::errno::from_host(libc::EINVAL));
    }
    Ok(Some(Duration::new(
        timeout.tv_sec as u64,
        timeout.tv_nsec as u32,
    )))
}

/// A host `timespec` for `timeout`, or `None` for "wait forever".
fn host_timespec(timeout: Option<Duration>) -> Option<libc::timespec> {
    timeout.map(|timeout| libc::timespec {
        tv_sec: timeout.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
        tv_nsec: timeout.subsec_nanos() as libc::c_long,
    })
}

/// `poll`'s timeout in milliseconds, or -1 for "wait forever".
///
/// Rounded **up**: a caller asking for half a millisecond asked to wait, and a
/// zero here would make its wait a poll.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn poll_ms(timeout: Option<Duration>) -> c_int {
    match timeout {
        None => -1,
        Some(timeout) => timeout
            .as_nanos()
            .div_ceil(1_000_000)
            .min(c_int::MAX as u128) as c_int,
    }
}

/// `poll` for at most `timeout`, to the nanosecond where the host has `ppoll`.
fn poll_for(watched: &mut [libc::pollfd], timeout: Option<Duration>) -> c_int {
    #[cfg(not(target_os = "macos"))]
    {
        let wait = host_timespec(timeout);
        // SAFETY: `watched` is the caller's array, and the timeout a local or
        // null; a null mask keeps the thread's own.
        unsafe {
            libc::ppoll(
                watched.as_mut_ptr(),
                watched.len() as libc::nfds_t,
                wait.as_ref()
                    .map_or(std::ptr::null(), |wait| wait as *const _),
                std::ptr::null(),
            )
        }
    }
    #[cfg(target_os = "macos")]
    // SAFETY: `watched` is the caller's array.
    unsafe {
        libc::poll(
            watched.as_mut_ptr(),
            watched.len() as libc::nfds_t,
            poll_ms(timeout),
        )
    }
}

/// `epoll_wait` for at most `timeout`, to the nanosecond through
/// `epoll_pwait2` (Linux 5.11); an older kernel waits whole milliseconds,
/// rounded up.
#[cfg(target_os = "linux")]
fn epoll_wait_for(
    epoll: c_int,
    events: &mut [libc::epoll_event],
    timeout: Option<Duration>,
) -> c_int {
    use std::sync::atomic::{AtomicBool, Ordering};
    static NO_PWAIT2: AtomicBool = AtomicBool::new(false);
    let sub_ms = timeout.is_some_and(|timeout| timeout.subsec_nanos() % 1_000_000 != 0);
    if sub_ms && !NO_PWAIT2.load(Ordering::Relaxed) {
        let wait = host_timespec(timeout);
        // SAFETY: `events` is the caller's array and the timeout a local; a
        // null mask keeps the thread's own.
        let count = unsafe {
            libc::syscall(
                libc::SYS_epoll_pwait2,
                epoll,
                events.as_mut_ptr(),
                events.len() as c_int,
                wait.as_ref()
                    .map_or(std::ptr::null(), |wait| wait as *const _),
                std::ptr::null::<libc::sigset_t>(),
                0usize,
            )
        } as c_int;
        if count >= 0 || crate::errno::get() != libc::ENOSYS {
            return count;
        }
        NO_PWAIT2.store(true, Ordering::Relaxed);
    }
    // SAFETY: `events` is the caller's array.
    unsafe {
        libc::epoll_wait(
            epoll,
            events.as_mut_ptr(),
            events.len() as c_int,
            poll_ms(timeout),
        )
    }
}

/// The descriptors to wait on: the wakeup channel, and every enabled readiness
/// registration folded together by descriptor.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn poll_set(
    queue: c_int,
    registrations: &HashMap<(usize, i16), Registration>,
) -> Vec<libc::pollfd> {
    let mut interest: HashMap<c_int, i16> = HashMap::new();
    interest.insert(queue, libc::POLLIN);
    for registration in registrations.values() {
        if !registration.enabled
            || matches!(
                registration.filter,
                EVFILT_USER | EVFILT_SIGNAL | EVFILT_TIMER | EVFILT_MACHPORT | EVFILT_PROC
            )
        {
            continue;
        }
        let Ok(fd) = c_int::try_from(registration.ident) else {
            continue;
        };
        *interest.entry(fd).or_default() |= match registration.filter {
            EVFILT_READ => libc::POLLIN,
            _ => libc::POLLOUT,
        };
    }
    interest
        .into_iter()
        .map(|(fd, events)| libc::pollfd {
            fd,
            events,
            revents: 0,
        })
        .collect()
}

/// How many bytes can be read from `fd`, which is what `EVFILT_READ` reports as
/// its `data` and what a caller sizes its next read from.
fn readable_bytes(fd: c_int) -> isize {
    let mut bytes: c_int = 0;
    // SAFETY: `FIONREAD` writes one `int` through the pointer it is given.
    match unsafe { libc::ioctl(fd, libc::FIONREAD, &raw mut bytes) } {
        0 => bytes as isize,
        _ => 0,
    }
}

/// `int kevent(int kq, const struct kevent *changelist, int nchanges,
/// struct kevent *eventlist, int nevents, const struct timespec *timeout)`
///
/// # Safety
///
/// `changes` holds `nchanges` events and `events` has room for `nevents`.
pub unsafe extern "C" fn kevent(
    queue: c_int,
    changes: *const Kevent,
    nchanges: c_int,
    events: *mut Kevent,
    nevents: c_int,
    timeout: *const libc::timespec,
) -> c_int {
    if nchanges < 0
        || nevents < 0
        || (nchanges != 0 && changes.is_null())
        || (nevents != 0 && events.is_null())
    {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let changes: Vec<(Kevent, [u64; 2])> = match nchanges {
        0 => Vec::new(),
        // SAFETY: the caller states there are `nchanges` of them.
        n => unsafe { std::slice::from_raw_parts(changes, n as usize) }
            .iter()
            .map(|change| (*change, [0; 2]))
            .collect(),
    };
    // SAFETY: the caller's timeout, null or a `timespec`.
    let wait = match unsafe { Wait::from(timeout) } {
        Ok(wait) => wait,
        Err(errno) => {
            crate::errno::set(crate::errno::to_host(errno));
            return -1;
        }
    };
    match collect(queue, &changes, nevents as usize, wait, false) {
        Ok(out) => {
            let narrow: Vec<Kevent> = out.into_iter().map(|(event, _)| event).collect();
            // SAFETY: the caller states there is room for `nevents`, and
            // `collect` never returns more.
            unsafe { std::ptr::copy_nonoverlapping(narrow.as_ptr(), events, narrow.len()) };
            narrow.len() as c_int
        }
        Err(errno) => {
            crate::errno::set(errno);
            -1
        }
    }
}

/// How long a `kevent` call may wait: a `timespec`, or none at all.
#[derive(Clone, Copy)]
enum Wait {
    Forever,
    For(Duration),
}

impl Wait {
    /// # Safety
    ///
    /// `timeout` is null or a readable `timespec`.
    unsafe fn from(timeout: *const libc::timespec) -> Result<Wait, c_int> {
        Ok(match timeout_of(timeout)? {
            None => Wait::Forever,
            Some(timeout) => Wait::For(timeout),
        })
    }
}

/// `kevent` and `kevent64`'s shared body: apply `changes`, then wait for and
/// gather up to `capacity` events, each with its `ext` words. `wide` is
/// `kevent64`, whose events have room to receive a Mach message into.
///
/// The error is a host `errno`.
fn collect(
    queue: c_int,
    changes: &[(Kevent, [u64; 2])],
    capacity: usize,
    wait: Wait,
    wide: bool,
) -> Result<Vec<(Kevent, [u64; 2])>, c_int> {
    let Some(state) = queue_of(queue) else {
        return Err(libc::EBADF);
    };
    let mut out: Vec<(Kevent, [u64; 2])> = Vec::with_capacity(capacity);
    {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        for (change, ext) in changes {
            let failure = apply(&mut state, change, *ext, wide).err();
            // `EV_RECEIPT` asks for the *outcome of every change* rather than
            // for readiness, and it asks for it whether or not the change
            // failed - which is how a caller applies a batch and finds out
            // which one of them the kernel rejected.
            if change.flags & EV_RECEIPT != 0 {
                if out.len() < capacity {
                    out.push((
                        Kevent {
                            flags: EV_ERROR,
                            data: failure.unwrap_or(0) as isize,
                            ..*change
                        },
                        *ext,
                    ));
                }
                continue;
            }
            if let Some(errno) = failure {
                return Err(crate::errno::to_host(errno));
            }
        }
    }
    if capacity == 0 || !out.is_empty() {
        return Ok(out);
    }

    // When the caller's timeout runs out, or `None` to wait as long as it takes.
    // A pass that ends with nothing to deliver - a wakeup for a signal another
    // queue watches, a timer re-armed elsewhere, a message another thread took
    // first - waits again for what is left, as XNU's `kevent` does: it returns
    // zero events only when the time is up.
    let until = match wait {
        Wait::Forever => None,
        Wait::For(timeout) => Some(Instant::now() + timeout),
    };
    loop {
        // Held for the whole wait so no socket closes, and its number is reused,
        // while `poll` watches it.
        let ports = {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            let state = &mut *state;
            if let Err(errno) =
                sync_ports(&mut state.port_watch, &*state.backend, &state.registrations)
            {
                return Err(crate::errno::to_host(errno));
            }
            watched_ports(&state.registrations)
        };
        // The pidfds of the enabled process registrations, duplicated so a
        // registration deleted on another thread mid-wait does not close the
        // descriptor `poll` is watching.
        let processes: Vec<std::os::fd::OwnedFd> = {
            let state = state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .registrations
                .values()
                .filter(|r| r.filter == EVFILT_PROC && r.enabled)
                .filter_map(|r| state.processes.get(&r.ident))
                .filter_map(|pidfd| pidfd.try_clone().ok())
                .collect()
        };
        let (triggered, timer) = {
            let state = state.lock().unwrap_or_else(|e| e.into_inner());
            let triggered = !state.triggered.is_empty()
                || signals_pending(&state.registrations)
                || ports.iter().any(|port| port.ready())
                || processes.iter().any(|pidfd| exited(pidfd))
                || state.registrations.values().any(timer_due);
            (triggered, next_timer(&state.registrations))
        };
        // Something is already waiting, so this is a collection rather than a wait;
        // otherwise the caller's time left, cut short by the first timer due.
        let left = until.map(|until| until.saturating_duration_since(Instant::now()));
        let wait = match (triggered, timer, left) {
            (true, _, _) => Some(Duration::ZERO),
            (false, Some(timer), Some(left)) => Some(timer.min(left)),
            (false, timer, left) => timer.or(left),
        };
        let ready = {
            let state = state.lock().unwrap_or_else(|e| e.into_inner());
            let backend = Arc::clone(&state.backend);
            let registrations = state.registrations.clone();
            drop(state);
            let port_fds: Vec<c_int> = ports
                .iter()
                .map(|port| port.fd())
                .chain(processes.iter().map(std::os::fd::AsRawFd::as_raw_fd))
                .collect();
            backend.wait(queue, &registrations, &port_fds, capacity, wait)?
        };

        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        if ready.iter().any(|entry| entry.source == Source::Wake) {
            state.backend.drain_wake();
        }

        // The user events first: a program triggers one to be told to stop, and a
        // busy descriptor must not keep that behind it.
        while out.len() < capacity {
            let Some(ident) = state.triggered.pop_front() else {
                break;
            };
            state.triggered_set.remove(&ident);
            if let Some(registration) = state.registrations.get(&(ident, EVFILT_USER))
                && registration.enabled
            {
                out.push((registration.fired(), registration.ext));
            }
        }

        let mut signals: Vec<(usize, i16)> = state
            .registrations
            .keys()
            .filter(|key| key.1 == EVFILT_SIGNAL)
            .copied()
            .collect();
        signals.sort_unstable();
        for key in signals {
            if out.len() == capacity {
                break;
            }
            let Some(registration) = state.registrations.get_mut(&key) else {
                continue;
            };
            let count = crate::signal::delivered(registration.ident as i32);
            if !registration.enabled || count <= registration.seen {
                continue;
            }
            let mut event = registration.fired();
            event.data = (count - registration.seen) as isize;
            registration.seen = count;
            out.push((event, registration.ext));
            let (ident, flags) = (registration.ident, registration.flags);
            if flags & EV_ONESHOT != 0 {
                state.registrations.remove(&key);
                let _ = crate::signal::observe(ident as i32, false);
            } else if flags & EV_DISPATCH != 0 {
                registration.enabled = false;
            }
        }

        // XNU's `filt_timerprocess`: a timer that fired once reports one
        // expiry, a repeating one how many periods have ended since it was last
        // reported, and is re-armed for the end of the period under way.
        let mut timers: Vec<(usize, i16)> = state
            .registrations
            .iter()
            .filter(|(_, r)| timer_due(r))
            .map(|(key, _)| *key)
            .collect();
        timers.sort_unstable();
        for key in timers {
            if out.len() == capacity {
                break;
            }
            let Some(registration) = state.registrations.get_mut(&key) else {
                continue;
            };
            let mut event = registration.fired();
            event.fflags = 0;
            event.data = 1;
            let mut ext = registration.ext;
            ext[0] = 0;
            let deadline = registration.deadline.unwrap_or(0);
            if registration.interval != 0 {
                let now = timer_now(registration.fflags);
                let armed = deadline.saturating_sub(registration.interval);
                let periods = (now.saturating_sub(armed) / registration.interval).max(1);
                event.data = periods.min(isize::MAX as u64) as isize;
                registration.deadline =
                    Some(deadline.saturating_add(periods.saturating_mul(registration.interval)));
            } else {
                registration.deadline = None;
            }
            let flags = registration.flags;
            if flags & EV_DISPATCH != 0 {
                registration.enabled = false;
            }
            out.push((event, ext));
            if flags & EV_ONESHOT != 0 {
                state.registrations.remove(&key);
            }
        }

        // XNU's `filt_procprocess` for an exit: the notes that were asked for and
        // happened, the wait status under `NOTE_EXITSTATUS`, and `EV_EOF |
        // EV_ONESHOT`, so the registration goes with its event.
        let mut exits: Vec<(usize, i16)> = state
            .registrations
            .keys()
            .filter(|key| key.1 == EVFILT_PROC)
            .copied()
            .collect();
        exits.sort_unstable();
        for key in exits {
            if out.len() == capacity {
                break;
            }
            let Some(registration) = state.registrations.get(&key).copied() else {
                continue;
            };
            if !registration.enabled || registration.fflags & NOTE_EXIT == 0 {
                continue;
            }
            let Some(pidfd) = state.processes.get(&key.0) else {
                continue;
            };
            if !exited(pidfd) {
                continue;
            }
            let mut event = registration.fired();
            event.fflags = registration.fflags & (NOTE_EXIT | NOTE_EXITSTATUS);
            event.flags |= EV_EOF | EV_ONESHOT;
            if registration.fflags & NOTE_EXITSTATUS != 0 {
                event.data = exit_status(pidfd) as isize;
            }
            out.push((event, registration.ext));
            state.registrations.remove(&key);
            state.processes.remove(&key.0);
        }

        let mut machports: Vec<(usize, i16)> = state
            .registrations
            .keys()
            .filter(|key| key.1 == EVFILT_MACHPORT)
            .copied()
            .collect();
        machports.sort_unstable();
        for key in machports {
            if out.len() == capacity {
                break;
            }
            let Some(registration) = state.registrations.get_mut(&key) else {
                continue;
            };
            // An `EV_CLEAR` registration fires on an arrival, from the backend.
            if !registration.enabled || registration.flags & EV_CLEAR != 0 {
                continue;
            }
            // Darwin names the member the message waits on, not the set.
            let Some(member) = receivers(registration.ident as Name)
                .into_iter()
                .find(|(_, port)| port.ready())
                .map(|(name, _)| name)
            else {
                continue;
            };
            let mut event = registration.fired();
            event.data = member as isize;
            out.push((event, registration.ext));
            if registration.flags & EV_DISPATCH != 0 {
                registration.enabled = false;
            }
            if registration.flags & EV_ONESHOT != 0 {
                state.registrations.remove(&key);
            }
        }

        let mut ports_changed = false;
        for entry in &ready {
            if out.len() == capacity || entry.source != Source::Port {
                continue;
            }
            let Some(port) = state.port_watch.get(&entry.fd).cloned() else {
                continue;
            };
            let mut firing: Vec<((usize, i16), Registration)> = state
                .registrations
                .iter()
                .filter(|(_, r)| {
                    r.filter == EVFILT_MACHPORT && r.enabled && r.flags & EV_CLEAR != 0
                })
                .map(|(key, r)| (*key, *r))
                .collect();
            firing.sort_unstable_by_key(|(key, _)| *key);
            for (key, registration) in firing {
                if out.len() == capacity {
                    break;
                }
                let Some(member) = receivers(registration.ident as Name)
                    .into_iter()
                    .find(|(_, member)| Arc::ptr_eq(member, &port))
                    .map(|(name, _)| name)
                else {
                    continue;
                };
                let mut event = registration.fired();
                event.data = member as isize;
                out.push((event, registration.ext));
                if registration.flags & EV_ONESHOT != 0 {
                    state.registrations.remove(&key);
                    ports_changed = true;
                } else if registration.flags & EV_DISPATCH != 0 {
                    if let Some(registration) = state.registrations.get_mut(&key) {
                        registration.enabled = false;
                    }
                    ports_changed = true;
                }
            }
        }
        if ports_changed {
            let state = &mut *state;
            let _ = sync_ports(&mut state.port_watch, &*state.backend, &state.registrations);
        }

        for entry in ready {
            if out.len() == capacity || entry.revents == 0 {
                continue;
            }
            // A level entry answers the plain registrations on the descriptor and
            // an edge entry the `EV_CLEAR` ones, so a hangup the level side keeps
            // reporting does not re-fire an edge registration.
            let edge = match entry.source {
                Source::Level => false,
                Source::Edge => true,
                Source::Wake | Source::Port => continue,
            };
            let firing: Vec<((usize, i16), Registration)> = state
                .registrations
                .iter()
                .filter(|(_, r)| {
                    r.enabled && r.ident == entry.fd as usize && (r.flags & EV_CLEAR != 0) == edge
                })
                .map(|(key, r)| (*key, *r))
                .collect();
            for (key, registration) in firing {
                if out.len() == capacity {
                    break;
                }
                let wanted = match registration.filter {
                    EVFILT_READ => libc::POLLIN,
                    EVFILT_WRITE => libc::POLLOUT,
                    _ => continue,
                };
                // The three conditions `poll` reports whether or not they were
                // asked for. A hangup or an error is readiness as far as kqueue is
                // concerned - the read that follows is what reports it.
                if entry.revents & (wanted | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0 {
                    continue;
                }
                let mut event = registration.fired();
                if entry.revents & libc::POLLNVAL != 0 {
                    event.flags |= EV_ERROR;
                    event.data = crate::errno::from_host(libc::EBADF) as isize;
                } else {
                    if entry.revents & libc::POLLHUP != 0 {
                        event.flags |= EV_EOF;
                    }
                    if registration.filter == EVFILT_READ {
                        event.data = readable_bytes(entry.fd);
                    }
                }
                out.push((event, registration.ext));
                // `EV_ONESHOT` deletes the registration once it has fired and
                // `EV_DISPATCH` disables it until it is enabled again. Both are how
                // a caller avoids being handed the same readiness on every turn.
                if registration.flags & EV_ONESHOT != 0 {
                    let before = interest_for(&state.registrations, entry.fd);
                    state.registrations.remove(&key);
                    let after = interest_for(&state.registrations, entry.fd);
                    let _ = state.backend.update(entry.fd, before, after);
                } else if registration.flags & EV_DISPATCH != 0 {
                    let before = interest_for(&state.registrations, entry.fd);
                    if let Some(registration) = state.registrations.get_mut(&key) {
                        registration.enabled = false;
                    }
                    let after = interest_for(&state.registrations, entry.fd);
                    let _ = state.backend.update(entry.fd, before, after);
                }
            }
        }
        drop(state);
        if wide {
            out.retain_mut(|(event, ext)| receive_into(event, ext));
        }
        if !out.is_empty() || until.is_some_and(|until| Instant::now() >= until) {
            return Ok(out);
        }
    }
}

/// `MACH_RCV_LARGE` and `MACH_RCV_TIMEOUT`, and the two receive results an
/// event reports in `fflags`.
const MACH_RCV_LARGE: u32 = 0x0000_0004;
const MACH_RCV_TIMEOUT: i32 = 0x0000_0100;
const MACH_RCV_TIMED_OUT: i32 = 0x1000_4003;
const MACH_RCV_TOO_LARGE: i32 = 0x1000_4004;
/// `mach_msg_trailer_t`, which a received message is followed by.
const TRAILER: u64 = 8;

/// An `EVFILT_MACHPORT` event registered with `MACH_RCV_MSG`, received into
/// its `ext[0]` buffer of `ext[1]` bytes as `filt_machportprocess` does: the
/// receive result in `fflags`, the size received with its trailer in `ext[1]`,
/// and `data` the port's name only for a message too large for the buffer.
/// A message another thread took first is no event at all.
///
/// Every other event passes through untouched.
fn receive_into(event: &mut Kevent, ext: &mut [u64; 2]) -> bool {
    if event.filter != EVFILT_MACHPORT || event.fflags & MACH_RCV_MSG == 0 {
        return true;
    }
    let (buffer, size) = (ext[0], ext[1] as u32);
    let name = event.data as Name;
    let large = (event.fflags & MACH_RCV_LARGE) as i32;
    let result = match buffer == 0 || size == 0 {
        // No buffer: XNU would carve one from `kevent_qos`'s data area, which
        // `kevent64` has none of, so the message is too large for it and
        // stays queued.
        true => MACH_RCV_TOO_LARGE,
        // SAFETY: the buffer and size the caller registered.
        false => unsafe {
            crate::mach::msg::mach_msg(
                buffer as *mut c_void,
                0x2 | MACH_RCV_TIMEOUT | large,
                0,
                size,
                name,
                0,
                0,
            )
        },
    };
    if result == MACH_RCV_TIMED_OUT {
        return false;
    }
    event.fflags = result as u32;
    event.data = match result {
        MACH_RCV_TOO_LARGE => name as isize,
        _ => 0,
    };
    ext[1] = match result {
        0 => {
            // SAFETY: the header the receive just wrote.
            let header_size = unsafe { ((buffer + 4) as *const u32).read_unaligned() };
            u64::from(header_size) + TRAILER
        }
        _ => 0,
    };
    true
}

/// `struct kevent64_s`: `kevent`'s fields at full width, and two more.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Kevent64 {
    pub ident: u64,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
    pub data: i64,
    pub udata: u64,
    pub ext: [u64; 2],
}

const _: () = {
    assert!(size_of::<Kevent64>() == 48);
    assert!(std::mem::offset_of!(Kevent64, ext) == 32);
};

/// `KEVENT_FLAG_IMMEDIATE` and `KEVENT_FLAG_ERROR_EVENTS`, from `<sys/event.h>`.
const KEVENT_FLAG_IMMEDIATE: u32 = 0x1;
const KEVENT_FLAG_ERROR_EVENTS: u32 = 0x2;

/// `int kevent64(int kq, const struct kevent64_s *changelist, int nchanges,
/// struct kevent64_s *eventlist, int nevents, unsigned int flags,
/// const struct timespec *timeout)`
///
/// `kevent` with room for `ext`. `KEVENT_FLAG_IMMEDIATE` is a zero timeout;
/// `KEVENT_FLAG_ERROR_EVENTS` reports only the changes that failed, as
/// `EV_ERROR` events, and waits for nothing.
///
/// # Safety
///
/// `changes` holds `nchanges` events and `events` has room for `nevents`.
pub unsafe extern "C" fn kevent64(
    queue: c_int,
    changes: *const Kevent64,
    nchanges: c_int,
    events: *mut Kevent64,
    nevents: c_int,
    flags: u32,
    timeout: *const libc::timespec,
) -> c_int {
    if nchanges < 0
        || nevents < 0
        || (nchanges != 0 && changes.is_null())
        || (nevents != 0 && events.is_null())
    {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let changes: Vec<(Kevent, [u64; 2])> = match nchanges {
        0 => Vec::new(),
        // SAFETY: the caller states there are `nchanges` of them.
        n => unsafe { std::slice::from_raw_parts(changes, n as usize) }
            .iter()
            .map(|change| {
                let mut narrow = Kevent {
                    ident: change.ident as usize,
                    filter: change.filter,
                    flags: change.flags,
                    fflags: change.fflags,
                    data: change.data as isize,
                    udata: change.udata as usize,
                };
                if flags & KEVENT_FLAG_ERROR_EVENTS != 0 {
                    narrow.flags |= EV_RECEIPT;
                }
                (narrow, change.ext)
            })
            .collect(),
    };
    let wait = match flags & (KEVENT_FLAG_IMMEDIATE | KEVENT_FLAG_ERROR_EVENTS) != 0 {
        true => Wait::For(Duration::ZERO),
        // SAFETY: the caller's timeout, null or a `timespec`.
        false => match unsafe { Wait::from(timeout) } {
            Ok(wait) => wait,
            Err(errno) => {
                crate::errno::set(crate::errno::to_host(errno));
                return -1;
            }
        },
    };
    let out = match collect(queue, &changes, nevents as usize, wait, true) {
        Ok(out) => out,
        Err(errno) => {
            crate::errno::set(errno);
            return -1;
        }
    };
    let wide: Vec<Kevent64> = out
        .into_iter()
        // With `KEVENT_FLAG_ERROR_EVENTS`, a receipt for a change that worked
        // is not an error to report.
        .filter(|(event, _)| {
            flags & KEVENT_FLAG_ERROR_EVENTS == 0 || event.flags & EV_ERROR == 0 || event.data != 0
        })
        .map(|(event, ext)| Kevent64 {
            ident: event.ident as u64,
            filter: event.filter,
            flags: match flags & KEVENT_FLAG_ERROR_EVENTS {
                0 => event.flags,
                _ => event.flags & !EV_RECEIPT,
            },
            fflags: event.fflags,
            data: event.data as i64,
            udata: event.udata as u64,
            ext,
        })
        .collect();
    // SAFETY: the caller states there is room for `nevents`, and `collect`
    // never returns more.
    unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), events, wide.len()) };
    wide.len() as c_int
}

/// The receive rights an `EVFILT_MACHPORT` registration on `name` watches:
/// the port itself, or every member of a port set, by name.
///
/// Resolved on every call rather than at registration, because Mach lets a
/// set gain and lose members while a kqueue watches it.
fn receivers(name: Name) -> Vec<(Name, Arc<Receiver>)> {
    if let Some(port) = space::receiver(name) {
        return vec![(name, port)];
    }
    space::members(name)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|member| space::receiver(member).map(|port| (member, port)))
        .collect()
}

/// Every receive right an enabled level-triggered `EVFILT_MACHPORT`
/// registration watches.
fn watched_ports(registrations: &HashMap<(usize, i16), Registration>) -> Vec<Arc<Receiver>> {
    registrations
        .values()
        .filter(|r| r.filter == EVFILT_MACHPORT && r.enabled && r.flags & EV_CLEAR == 0)
        .flat_map(|r| receivers(r.ident as Name))
        .map(|(_, port)| port)
        .collect()
}

/// A pidfd for `pid`, as the host errno it could not be opened with. Only
/// Linux has them; elsewhere `EVFILT_PROC` stays the `EINVAL` it was.
#[cfg(not(target_os = "linux"))]
fn open_pidfd(_pid: usize) -> Result<std::os::fd::OwnedFd, c_int> {
    Err(libc::EINVAL)
}

#[cfg(not(target_os = "linux"))]
fn exit_status(_pidfd: &std::os::fd::OwnedFd) -> i32 {
    0
}

#[cfg(target_os = "linux")]
fn open_pidfd(pid: usize) -> Result<std::os::fd::OwnedFd, c_int> {
    let pid = libc::pid_t::try_from(pid).map_err(|_| libc::ESRCH)?;
    if pid <= 0 {
        return Err(libc::ESRCH);
    }
    // SAFETY: `pidfd_open` takes a pid and flags, and returns a descriptor.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    match fd {
        -1 => Err(crate::errno::get()),
        // SAFETY: a descriptor the call above opened, owned from here.
        fd => Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd as c_int) }),
    }
}

/// Whether the process a pidfd names has exited: the descriptor is readable.
fn exited(pidfd: &std::os::fd::OwnedFd) -> bool {
    let mut entry = libc::pollfd {
        fd: std::os::fd::AsRawFd::as_raw_fd(pidfd),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one entry, and no wait.
    let ready = unsafe { libc::poll(&mut entry, 1, 0) };
    ready == 1 && entry.revents & libc::POLLIN != 0
}

/// An exited process's status in `wait`'s encoding, with Darwin's signal
/// numbers, read without reaping it: the `waitpid` the program makes next is
/// still the one that collects it. Zero for a process this one cannot wait
/// on, which is any but its children.
#[cfg(target_os = "linux")]
fn exit_status(pidfd: &std::os::fd::OwnedFd) -> i32 {
    // SAFETY: zeroed is a valid `siginfo_t`, which `waitid` fills.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    const P_PIDFD: libc::idtype_t = 3;
    // SAFETY: a pidfd this module opened, and the structure above.
    let rc = unsafe {
        libc::waitid(
            P_PIDFD,
            std::os::fd::AsRawFd::as_raw_fd(pidfd) as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
        )
    };
    if rc != 0 {
        return 0;
    }
    // SAFETY: filled by a successful `waitid` for an exit.
    let status = unsafe { info.si_status() };
    let signal = || crate::signal::signal_from_host(status).unwrap_or(status) & 0x7f;
    match info.si_code {
        libc::CLD_EXITED => (status & 0xff) << 8,
        libc::CLD_KILLED => signal(),
        libc::CLD_DUMPED => signal() | 0x80,
        _ => 0,
    }
}

/// Whether an enabled signal registration has deliveries it has not reported.
fn signals_pending(registrations: &HashMap<(usize, i16), Registration>) -> bool {
    registrations.values().any(|r| {
        r.filter == EVFILT_SIGNAL && r.enabled && crate::signal::delivered(r.ident as i32) > r.seen
    })
}

/// `kevent_qos`, whose `kevent_qos_s` and data area nothing here answers.
pub const REFUSED: &[&str] = &["kevent_qos"];

exports::exports! {
    functions {
        "kqueue" => kqueue,
        "kevent" => kevent,
        "kevent64" => kevent64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero timeout, which makes `kevent` a collection rather than a wait.
    static NO_WAIT: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    /// A pipe, as the two descriptors a readiness test needs.
    fn pipe() -> (c_int, c_int) {
        let mut ends = [0 as c_int; 2];
        // SAFETY: a live array of two `int`.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        (ends[0], ends[1])
    }

    /// The case the client needs: a descriptor is registered, nothing is ready,
    /// a byte arrives, and the *same* registration reports how much there is.
    #[test]
    fn a_readable_descriptor_is_reported_with_the_bytes_waiting() {
        let queue = kqueue();
        assert!(queue >= 0);
        let (read_end, write_end) = pipe();
        let add = Kevent {
            ident: read_end as usize,
            filter: EVFILT_READ,
            flags: EV_ADD,
            udata: 0xbeef,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 4];
        // SAFETY: one change and room for four events.
        unsafe {
            assert_eq!(
                kevent(
                    queue,
                    &raw const add,
                    1,
                    got.as_mut_ptr(),
                    4,
                    &raw const NO_WAIT
                ),
                0,
                "nothing is ready yet"
            );
            assert_eq!(libc::write(write_end, c"hello".as_ptr().cast(), 5), 5);
            assert_eq!(
                kevent(
                    queue,
                    std::ptr::null(),
                    0,
                    got.as_mut_ptr(),
                    4,
                    &raw const NO_WAIT
                ),
                1
            );
            libc::close(read_end);
            libc::close(write_end);
            libc::close(queue);
        }
        assert_eq!(got[0].ident, read_end as usize);
        assert_eq!(got[0].filter, EVFILT_READ);
        assert_eq!(got[0].udata, 0xbeef);
        assert_eq!(got[0].data, 5, "five bytes are waiting");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn queue_descriptor_is_readable_when_a_watched_descriptor_is_ready() {
        let queue = kqueue();
        let (read_end, write_end) = pipe();
        let add = Kevent {
            ident: read_end as usize,
            filter: EVFILT_READ,
            flags: EV_ADD,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: the descriptors and event buffers belong to this test.
        unsafe {
            assert_eq!(
                kevent(
                    queue,
                    &raw const add,
                    1,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT
                ),
                0
            );
            assert_eq!(libc::write(write_end, c"x".as_ptr().cast(), 1), 1);
            let mut watched = libc::pollfd {
                fd: queue,
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(libc::poll(&raw mut watched, 1, 0), 1);
            assert_ne!(watched.revents & libc::POLLIN, 0);
            assert_eq!(
                kevent(
                    queue,
                    std::ptr::null(),
                    0,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT
                ),
                1
            );
            libc::close(read_end);
            libc::close(write_end);
            libc::close(queue);
        }
    }

    /// epoll refuses regular files, so Linux answers them with `poll`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_regular_file_is_reported_readable() {
        use std::os::fd::AsRawFd;

        let queue = kqueue();
        let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/src/kqueue.rs"))
            .expect("this Rust source file");
        let add = Kevent {
            ident: file.as_raw_fd() as usize,
            filter: EVFILT_READ,
            flags: EV_ADD,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: one valid change and one event slot.
        let n = unsafe {
            kevent(
                queue,
                &raw const add,
                1,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 1);
        assert_eq!(got[0].ident, file.as_raw_fd() as usize);
        // SAFETY: this test's queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A closed writer is readiness with `EV_EOF`, reported once even when the
    /// reader also watches for writability.
    #[test]
    fn a_hangup_is_reported_once_with_eof() {
        let queue = kqueue();
        let (read_end, write_end) = pipe();
        let changes = [EVFILT_READ, EVFILT_WRITE].map(|filter| Kevent {
            ident: read_end as usize,
            filter,
            flags: EV_ADD,
            ..Kevent::default()
        });
        let mut got = [Kevent::default(); 4];
        // SAFETY: two changes and room for four events.
        unsafe {
            libc::close(write_end);
            let n = kevent(
                queue,
                changes.as_ptr(),
                2,
                got.as_mut_ptr(),
                4,
                &raw const NO_WAIT,
            );
            let reads = got[..n.max(0) as usize]
                .iter()
                .filter(|event| event.filter == EVFILT_READ)
                .collect::<Vec<_>>();
            assert_eq!(reads.len(), 1);
            assert_eq!(reads[0].flags & EV_EOF, EV_EOF);
            libc::close(read_end);
            libc::close(queue);
        }
    }

    /// `EVFILT_USER` is what a program wakes its own I/O thread with, and the
    /// trigger has to survive being set from another thread while `kevent` is
    /// blocked with no timeout.
    #[test]
    fn a_user_event_wakes_a_wait_that_had_no_timeout() {
        let queue = kqueue();
        let add = Kevent {
            ident: 42,
            filter: EVFILT_USER,
            flags: EV_ADD | EV_ENABLE,
            udata: 7,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: one change and room for one event.
        unsafe {
            assert_eq!(
                kevent(
                    queue,
                    &raw const add,
                    1,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT
                ),
                0
            );
        }
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let fire = Kevent {
                ident: 42,
                filter: EVFILT_USER,
                fflags: NOTE_TRIGGER,
                ..Kevent::default()
            };
            // SAFETY: one change and no room for events, which applies it and
            // returns.
            unsafe {
                kevent(
                    queue,
                    &raw const fire,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            }
        });
        // SAFETY: no changes, room for one event, and a null timeout - which is
        // the wait the trigger has to break.
        let n = unsafe {
            kevent(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                std::ptr::null(),
            )
        };
        assert_eq!(trigger.join().expect("the trigger thread"), 0);
        assert_eq!(n, 1);
        assert_eq!(got[0].ident, 42);
        assert_eq!(got[0].udata, 7);
        assert_eq!(
            got[0].fflags & NOTE_TRIGGER,
            0,
            "the caller's request is not handed back as a property of the event"
        );
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// Applies `change` and collects what is ready, without waiting.
    fn collect(queue: c_int, change: Option<&Kevent>) -> (c_int, Kevent) {
        let mut got = [Kevent::default(); 1];
        // SAFETY: at most one change, room for one event, and no wait.
        let n = unsafe {
            kevent(
                queue,
                change.map_or(std::ptr::null(), |c| c as *const Kevent),
                change.is_some() as c_int,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        (n, got[0])
    }

    /// A signal whose default action is to do nothing is still reported, with
    /// the number of deliveries since the last report.
    #[test]
    fn a_signal_event_counts_deliveries_of_an_ignored_by_default_signal() {
        use crate::signal::signum::SIGWINCH;
        let queue = kqueue();
        let add = Kevent {
            ident: SIGWINCH as usize,
            filter: EVFILT_SIGNAL,
            flags: EV_ADD | EV_ENABLE,
            udata: 9,
            ..Kevent::default()
        };
        assert_eq!(collect(queue, Some(&add)).0, 0, "nothing delivered yet");
        let host = crate::signal::signal_to_host(SIGWINCH).expect("a host signal");
        // SAFETY: a signal this process takes by default as a no-op.
        unsafe {
            libc::raise(host);
            libc::raise(host);
        }
        let (n, event) = collect(queue, None);
        assert_eq!(n, 1);
        assert_eq!(event.ident, SIGWINCH as usize);
        assert_eq!(event.filter, EVFILT_SIGNAL);
        assert_eq!(event.data, 2);
        assert_eq!(event.udata, 9);
        assert_eq!(collect(queue, None).0, 0, "reported once");
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// `SIG_IGN` does not hide a signal from the queue, and removing the last
    /// registration puts the disposition back, so the signal stays ignored.
    #[test]
    fn a_signal_event_sees_an_ignored_signal_and_restores_the_disposition() {
        use crate::signal::signum::SIGUSR2;
        let host = crate::signal::signal_to_host(SIGUSR2).expect("a host signal");
        // SAFETY: SIG_IGN is a value, not a function.
        unsafe { crate::signal::signal(SIGUSR2, 1) };
        let queue = kqueue();
        let mut change = Kevent {
            ident: SIGUSR2 as usize,
            filter: EVFILT_SIGNAL,
            flags: EV_ADD,
            ..Kevent::default()
        };
        assert_eq!(collect(queue, Some(&change)).0, 0);
        // SAFETY: ignored, so this returns.
        unsafe { libc::raise(host) };
        let (n, event) = collect(queue, None);
        assert_eq!((n, event.data), (1, 1));

        change.flags = EV_DELETE;
        assert_eq!(collect(queue, Some(&change)).0, 0);
        // SAFETY: the disposition is SIG_IGN again, or this kills the test.
        unsafe { libc::raise(host) };
        assert_eq!(collect(queue, None).0, 0);
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// `SIGCHLD`, which is what `opend` watches: a child exiting is reported.
    #[test]
    fn a_signal_event_reports_a_child_exiting() {
        use crate::signal::signum::SIGCHLD;
        let queue = kqueue();
        let add = Kevent {
            ident: SIGCHLD as usize,
            filter: EVFILT_SIGNAL,
            flags: EV_ADD,
            ..Kevent::default()
        };
        assert_eq!(collect(queue, Some(&add)).0, 0);
        let mut child = std::process::Command::new("true").spawn().expect("true");
        child.wait().expect("the child");
        let mut got = [Kevent::default(); 1];
        let timeout = libc::timespec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        // SAFETY: no changes, room for one event.
        let n = unsafe {
            kevent(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                &raw const timeout,
            )
        };
        assert_eq!(n, 1);
        assert!(got[0].data >= 1);
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// A filter this module cannot answer is refused, and `EV_RECEIPT` is how
    /// the caller finds out which change it was.
    #[test]
    fn an_unsupported_filter_is_refused_by_number() {
        let queue = kqueue();
        const EVFILT_VNODE: i16 = -4;
        let add = Kevent {
            ident: 1,
            filter: EVFILT_VNODE,
            flags: EV_ADD | EV_RECEIPT,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: one change and room for one event.
        let n = unsafe {
            kevent(
                queue,
                &raw const add,
                1,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 1);
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(got[0].data as i32, crate::errno::from_host(libc::EINVAL));
        // And without `EV_RECEIPT` the whole call fails.
        let add = Kevent {
            flags: EV_ADD,
            ..add
        };
        assert_eq!(
            unsafe {
                kevent(
                    queue,
                    &raw const add,
                    1,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT,
                )
            },
            -1
        );
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// A descriptor that is not a queue is `EBADF` rather than a queue with
    /// nothing in it.
    #[test]
    fn a_descriptor_that_is_not_a_queue_is_refused() {
        let mut got = [Kevent::default(); 1];
        // SAFETY: standard input is open and is not a kqueue.
        let n = unsafe {
            kevent(
                0,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EBADF);
    }

    /// A receive right in a fresh port set, which is how Crashpad's handshake
    /// server watches its port.
    fn port_in_set() -> (Name, Name) {
        let (mut set, mut port) = (space::MACH_PORT_NULL, space::MACH_PORT_NULL);
        // SAFETY: live locals for the out-parameters.
        unsafe {
            space::mach_port_allocate(space::TASK_SELF, space::RIGHT_PORT_SET, &mut set);
            space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut port);
        }
        assert_eq!(
            space::mach_port_insert_member(space::TASK_SELF, port, set),
            0
        );
        (set, port)
    }

    /// Send an empty message with `id` to the receive right `port`.
    fn post(port: Name, id: i32) {
        let mut header = [0u32; 6];
        header[1] = size_of_val(&header) as u32;
        header[2] = port;
        header[5] = id as u32;
        // SAFETY: a whole header-only message of the size it states.
        let rc = unsafe {
            crate::mach::msg::mach_msg(
                header.as_mut_ptr().cast(),
                1,
                size_of_val(&header) as u32,
                0,
                space::MACH_PORT_NULL,
                0,
                space::MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, 0);
    }

    /// Watch `name` with `EV_CLEAR`, as Crashpad does.
    fn watch_machport(queue: c_int, name: Name) -> c_int {
        watch(queue, name as usize, EVFILT_MACHPORT, EV_ADD | EV_CLEAR)
    }

    /// Apply one change carrying `udata` 9, answering what `kevent` did.
    fn watch(queue: c_int, ident: usize, filter: i16, flags: u16) -> c_int {
        let add = Kevent {
            ident,
            filter,
            flags,
            udata: 9,
            ..Kevent::default()
        };
        // SAFETY: one change and no room for events.
        unsafe {
            kevent(
                queue,
                &raw const add,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        }
    }

    /// Crashpad's `ChildPortHandshake` registers its port set this way and
    /// aborts on any failure.
    #[test]
    fn a_message_on_a_port_set_member_is_reported_with_the_member_name() {
        let queue = kqueue();
        let (set, port) = port_in_set();
        assert_eq!(watch_machport(queue, set), 0);
        let mut got = [Kevent::default(); 1];
        // SAFETY: room for one event.
        unsafe {
            assert_eq!(
                kevent(
                    queue,
                    std::ptr::null(),
                    0,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT
                ),
                0,
                "nothing is queued yet"
            );
            post(port, 7);
            assert_eq!(
                kevent(
                    queue,
                    std::ptr::null(),
                    0,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT
                ),
                1
            );
        }
        assert_eq!(got[0].ident, set as usize);
        assert_eq!(got[0].filter, EVFILT_MACHPORT);
        assert_eq!(got[0].udata, 9);
        assert_eq!(got[0].data, port as isize, "the member, not the set");
        space::destroy(set);
        space::destroy(port);
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    #[test]
    fn a_message_wakes_a_wait_on_a_port_set_that_had_no_timeout() {
        let queue = kqueue();
        let (set, port) = port_in_set();
        assert_eq!(watch_machport(queue, set), 0);
        let sender = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            post(port, 1);
        });
        let mut got = [Kevent::default(); 1];
        // SAFETY: room for one event and a null timeout.
        let n = unsafe {
            kevent(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                std::ptr::null(),
            )
        };
        sender.join().expect("the sending thread");
        assert_eq!(n, 1);
        assert_eq!(got[0].filter, EVFILT_MACHPORT);
        assert_eq!(got[0].data, port as isize);
        space::destroy(set);
        space::destroy(port);
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    #[test]
    fn a_machport_filter_on_a_name_without_a_receive_right_is_refused() {
        let queue = kqueue();
        assert_eq!(watch_machport(queue, 0xdead_0003), -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// Crashpad's handshake watches its pipe this way and blocks until
    /// something changes; a level answer would make that wait a spin.
    #[test]
    fn a_writable_pipe_under_ev_clear_is_reported_once() {
        let queue = kqueue();
        let (read_end, write_end) = pipe();
        assert_eq!(
            watch(queue, write_end as usize, EVFILT_WRITE, EV_ADD | EV_CLEAR),
            0
        );
        assert_eq!(collect(queue, None).0, 1, "writable when registered");
        assert_eq!(collect(queue, None).0, 0, "and not again until it changes");
        // SAFETY: this test's own descriptors.
        unsafe {
            libc::close(read_end);
            assert_eq!(
                collect(queue, None).0,
                1,
                "the reader going away is a change"
            );
            libc::close(write_end);
            libc::close(queue);
        }
    }

    #[test]
    fn a_readable_descriptor_under_ev_clear_fires_again_only_when_more_arrives() {
        let queue = kqueue();
        let (read_end, write_end) = pipe();
        assert_eq!(
            watch(queue, read_end as usize, EVFILT_READ, EV_ADD | EV_CLEAR),
            0
        );
        assert_eq!(collect(queue, None).0, 0);
        // SAFETY: this test's own descriptors.
        unsafe {
            assert_eq!(libc::write(write_end, c"x".as_ptr().cast(), 1), 1);
            let (n, event) = collect(queue, None);
            assert_eq!((n, event.data), (1, 1));
            assert_eq!(
                collect(queue, None).0,
                0,
                "the byte is still there, unchanged"
            );
            assert_eq!(libc::write(write_end, c"y".as_ptr().cast(), 1), 1);
            let (n, event) = collect(queue, None);
            assert_eq!((n, event.data), (1, 2));
            libc::close(read_end);
            libc::close(write_end);
            libc::close(queue);
        }
    }

    /// Without `EV_CLEAR` the same descriptor keeps reporting.
    #[test]
    fn a_readable_descriptor_without_ev_clear_keeps_firing() {
        let queue = kqueue();
        let (read_end, write_end) = pipe();
        assert_eq!(watch(queue, read_end as usize, EVFILT_READ, EV_ADD), 0);
        // SAFETY: this test's own descriptors.
        unsafe {
            assert_eq!(libc::write(write_end, c"x".as_ptr().cast(), 1), 1);
            assert_eq!(collect(queue, None).0, 1);
            assert_eq!(collect(queue, None).0, 1);
            libc::close(read_end);
            libc::close(write_end);
            libc::close(queue);
        }
    }

    /// libdispatch's pairing: fire once, stay quiet while disabled, and on
    /// `EV_ENABLE` report what is still waiting.
    #[test]
    fn re_enabling_an_ev_clear_dispatch_registration_reports_what_is_waiting() {
        let queue = kqueue();
        let (read_end, write_end) = pipe();
        assert_eq!(
            watch(
                queue,
                read_end as usize,
                EVFILT_READ,
                EV_ADD | EV_CLEAR | EV_DISPATCH
            ),
            0
        );
        // SAFETY: this test's own descriptors.
        unsafe {
            assert_eq!(libc::write(write_end, c"x".as_ptr().cast(), 1), 1);
            assert_eq!(collect(queue, None).0, 1);
            assert_eq!(libc::write(write_end, c"y".as_ptr().cast(), 1), 1);
            assert_eq!(collect(queue, None).0, 0, "disabled by the delivery");
            assert_eq!(watch(queue, read_end as usize, EVFILT_READ, EV_ENABLE), 0);
            let (n, event) = collect(queue, None);
            assert_eq!((n, event.data), (1, 2));
            libc::close(read_end);
            libc::close(write_end);
            libc::close(queue);
        }
    }

    #[test]
    fn a_port_under_ev_clear_fires_once_per_arrival() {
        let queue = kqueue();
        let (set, port) = port_in_set();
        assert_eq!(watch_machport(queue, set), 0);
        post(port, 1);
        let (n, event) = collect(queue, None);
        assert_eq!((n, event.data), (1, port as isize));
        assert_eq!(
            collect(queue, None).0,
            0,
            "the message is still queued, unchanged"
        );
        post(port, 2);
        assert_eq!(collect(queue, None).0, 1);
        space::destroy(set);
        space::destroy(port);
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    #[test]
    fn a_port_without_ev_clear_fires_while_a_message_waits() {
        let queue = kqueue();
        let (set, port) = port_in_set();
        assert_eq!(watch(queue, set as usize, EVFILT_MACHPORT, EV_ADD), 0);
        post(port, 1);
        assert_eq!(collect(queue, None).0, 1);
        assert_eq!(collect(queue, None).0, 1);
        space::destroy(set);
        space::destroy(port);
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    fn change64(queue: c_int, change: &Kevent64) -> c_int {
        // SAFETY: one change and no room for events.
        unsafe {
            kevent64(
                queue,
                change,
                1,
                std::ptr::null_mut(),
                0,
                0,
                std::ptr::null(),
            )
        }
    }

    /// Chromium's message pump: a port registered to have its message received
    /// straight into the event's buffer, as `filt_machportprocess` does.
    #[test]
    fn kevent64_receives_a_message_into_the_events_buffer() {
        let queue = kqueue();
        let mut port = space::MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut port) };
        let mut buffer = [0u32; 16];
        let add = Kevent64 {
            ident: port as u64,
            filter: EVFILT_MACHPORT,
            flags: EV_ADD,
            fflags: MACH_RCV_MSG,
            udata: 7,
            ext: [buffer.as_mut_ptr() as u64, size_of_val(&buffer) as u64],
            ..Kevent64::default()
        };
        assert_eq!(change64(queue, &add), 0);

        let mut got = [Kevent64::default(); 2];
        // SAFETY: room for two events.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                2,
                KEVENT_FLAG_IMMEDIATE,
                std::ptr::null(),
            )
        };
        assert_eq!(n, 0, "nothing waiting, and no wait");

        post(port, 0x55);
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                2,
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(n, 1);
        let event = got[0];
        assert_eq!((event.ident, event.udata), (port as u64, 7));
        assert_eq!(event.fflags, 0, "MACH_MSG_SUCCESS");
        assert_eq!(event.data, 0);
        assert_eq!(event.ext[0], buffer.as_ptr() as u64);
        assert_eq!(event.ext[1], 24 + 8, "a bare header and its trailer");
        assert_eq!(buffer[5], 0x55, "the message is in the buffer");
        assert!(
            !space::receiver(port).unwrap().ready(),
            "and no longer queued"
        );

        // A buffer too small keeps the message and names the port.
        let small = Kevent64 {
            ext: [buffer.as_mut_ptr() as u64, 8],
            fflags: MACH_RCV_MSG | MACH_RCV_LARGE,
            ..add
        };
        assert_eq!(change64(queue, &small), 0);
        post(port, 0x66);
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                2,
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(n, 1);
        assert_eq!(got[0].fflags, MACH_RCV_TOO_LARGE as u32);
        assert_eq!(got[0].data, port as i64);
        assert!(space::receiver(port).unwrap().ready(), "still queued");

        space::destroy(port);
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// `KEVENT_FLAG_ERROR_EVENTS` answers only the changes that failed.
    /// XNU's `EVFILT_PROC`: a pid with no process is refused with `ESRCH`, and
    /// a child's exit arrives once, with its status under `NOTE_EXITSTATUS`,
    /// as `EV_EOF | EV_ONESHOT` - and leaves the child for `waitpid` to reap.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_registration_reports_the_exit_and_its_status() {
        let kq = kqueue();
        assert!(kq >= 0);
        let watch = |pid: usize| Kevent {
            ident: pid,
            filter: EVFILT_PROC,
            flags: EV_ADD,
            fflags: NOTE_EXIT | NOTE_EXITSTATUS,
            data: 0,
            udata: 7,
        };
        // SAFETY: one change and no events.
        let rc = unsafe {
            kevent(
                kq,
                &watch(0x3fff_fff0),
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        assert_eq!((rc, crate::errno::get()), (-1, libc::ESRCH));

        // SAFETY: the child only exits.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            unsafe { libc::_exit(3) };
        }
        // SAFETY: as above.
        let rc = unsafe {
            kevent(
                kq,
                &watch(pid as usize),
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(rc, 0);
        let mut event = Kevent::default();
        let forever = std::ptr::null();
        // SAFETY: room for one event.
        let n = unsafe { kevent(kq, std::ptr::null(), 0, &mut event, 1, forever) };
        assert_eq!(n, 1);
        assert_eq!(
            (event.ident, event.filter, event.udata),
            (pid as usize, EVFILT_PROC, 7)
        );
        assert_eq!(event.fflags, NOTE_EXIT | NOTE_EXITSTATUS);
        assert_eq!(event.flags & (EV_EOF | EV_ONESHOT), EV_EOF | EV_ONESHOT);
        assert_eq!(event.data, 3 << 8, "the wait status of exit(3)");

        // Gone with its event: nothing more arrives.
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: as above.
        assert_eq!(
            unsafe { kevent(kq, std::ptr::null(), 0, &mut event, 1, &zero) },
            0
        );
        // And the child is still there to reap.
        let mut status = 0;
        // SAFETY: the child above.
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert_eq!(libc::WEXITSTATUS(status), 3);
        unsafe { libc::close(kq) };
    }

    #[test]
    fn kevent64_error_events_reports_only_failures() {
        let queue = kqueue();
        let changes = [
            Kevent64 {
                ident: 1,
                filter: EVFILT_USER,
                flags: EV_ADD,
                ..Kevent64::default()
            },
            Kevent64 {
                ident: 2,
                filter: EVFILT_USER,
                flags: EV_DELETE,
                ..Kevent64::default()
            },
        ];
        let mut got = [Kevent64::default(); 4];
        // SAFETY: two changes and room for four events.
        let n = unsafe {
            kevent64(
                queue,
                changes.as_ptr(),
                2,
                got.as_mut_ptr(),
                4,
                KEVENT_FLAG_ERROR_EVENTS,
                std::ptr::null(),
            )
        };
        assert_eq!(n, 1);
        assert_eq!(got[0].ident, 2);
        assert_ne!(got[0].flags & EV_ERROR, 0);
        assert_eq!(got[0].data, crate::errno::from_host(libc::ENOENT) as i64);
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }
}
