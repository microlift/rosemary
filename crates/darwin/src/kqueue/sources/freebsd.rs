//! The host objects behind the filters on FreeBSD: a private host kqueue per
//! registration, holding the host's own filter of the same name. A kqueue
//! polls readable while it has an event, which is all the backend needs.

use std::ffi::c_int;
use std::os::fd::{AsRawFd, OwnedFd};

use super::*;
use crate::kqueue::{EV_CLEAR, EV_EOF, EV_ONESHOT};

/// The `EVFILT_VNODE` notes FreeBSD shares with Darwin, bit for bit; the
/// host is asked for all of them, so a registration can be told every part of
/// an event it asked for any part of, as Darwin does.
const VNODE_NOTES: u32 =
    NOTE_DELETE | NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB | NOTE_LINK | NOTE_RENAME | NOTE_REVOKE;

pub(in crate::kqueue) struct Source {
    /// The private kqueue the backend watches.
    queue: OwnedFd,
    kind: Kind,
    /// The `NOTE_*` bits that happened and are not yet reported.
    fflags: u32,
    /// Timer expirations, or the exit status, not yet reported.
    data: u64,
    /// The watched object is gone: the event ends the registration.
    eof: bool,
}

enum Kind {
    Timer {
        absolute: bool,
    },
    Vnode {
        /// A duplicate of the guest's descriptor, so a guest closing its own
        /// does not take the host registration with it.
        file: OwnedFd,
        requested: u32,
        links: u64,
    },
    Proc {
        requested: u32,
    },
}

impl Source {
    /// The host object behind a new registration. Errors are host `errno`s.
    pub(in crate::kqueue) fn open(change: &Kevent) -> Result<Self, c_int> {
        let (queue, kind) = match change.filter {
            EVFILT_TIMER => timer(change)?,
            EVFILT_VNODE => vnode(change)?,
            EVFILT_PROC => process(change)?,
            _ => return Err(libc::EINVAL),
        };
        Ok(Self {
            queue,
            kind,
            fflags: 0,
            data: 0,
            eof: false,
        })
    }

    /// `EV_ADD` on an existing registration: the new `fflags` take over. A
    /// timer is re-armed from scratch, which Darwin does too, so the answer is
    /// a replacement source.
    pub(in crate::kqueue) fn touch(&mut self, change: &Kevent) -> Result<Option<Self>, c_int> {
        match &mut self.kind {
            Kind::Timer { absolute } => {
                if *absolute != (change.fflags & NOTE_ABSOLUTE != 0) {
                    return Err(libc::EINVAL);
                }
                Self::open(change).map(Some)
            }
            Kind::Vnode { requested, .. } => {
                refuse("EVFILT_VNODE", change.fflags, VNODE_REFUSED)?;
                *requested = change.fflags;
                Ok(None)
            }
            Kind::Proc { requested } => {
                check_process_notes(change.fflags)?;
                if change.fflags & NOTE_EXITSTATUS != 0 && !is_child(change.ident) {
                    return Err(exit_status_refused());
                }
                *requested = change.fflags;
                Ok(None)
            }
        }
    }

    pub(in crate::kqueue) fn fd(&self) -> c_int {
        self.queue.as_raw_fd()
    }

    /// The `flags` Darwin forces on a registration of this kind.
    pub(in crate::kqueue) fn forced_flags(&self) -> u16 {
        match self.kind {
            Kind::Timer { absolute: true } => EV_CLEAR | EV_ONESHOT,
            Kind::Timer { absolute: false } | Kind::Proc { .. } => EV_CLEAR,
            Kind::Vnode { .. } => 0,
        }
    }

    /// Read what the host reported since the last call into the pending event.
    pub(in crate::kqueue) fn collect(&mut self) {
        while let Some(event) = next_event(self.queue.as_raw_fd()) {
            match &mut self.kind {
                Kind::Timer { .. } => {
                    self.data = self.data.saturating_add(event.data as u64);
                }
                Kind::Vnode {
                    file,
                    requested,
                    links,
                } => {
                    let mut hints = event.fflags & VNODE_NOTES;
                    // FreeBSD reports an unlink as `NOTE_DELETE` alone; Darwin
                    // adds `NOTE_LINK`, as the link count changed too.
                    if let Ok(now) = status(file.as_raw_fd())
                        && now.st_nlink as u64 != *links
                    {
                        hints |= NOTE_LINK;
                        *links = now.st_nlink as u64;
                    }
                    if *requested & hints != 0 {
                        self.fflags |= hints;
                    }
                    if event.flags & EV_EOF != 0 {
                        self.fflags |= *requested & NOTE_REVOKE;
                        self.eof = true;
                    }
                }
                Kind::Proc { requested } => {
                    if event.fflags & NOTE_EXIT == 0 || self.eof {
                        continue;
                    }
                    self.eof = true;
                    self.fflags = *requested & (NOTE_EXIT | NOTE_EXITSTATUS);
                    if *requested & NOTE_EXITSTATUS != 0 {
                        self.data = darwin_wait_status(event.data as c_int) as u64;
                    }
                }
            }
        }
    }

    /// Whether there is an event to report.
    pub(in crate::kqueue) fn pending(&self) -> bool {
        match self.kind {
            Kind::Timer { .. } => self.data != 0,
            Kind::Vnode { .. } => self.fflags != 0 || self.eof,
            Kind::Proc { .. } => self.fflags != 0,
        }
    }

    /// The event `fired` (the registration's own) becomes, if one is pending,
    /// and what is left pending afterwards. Darwin clears what it reported
    /// only for an `EV_CLEAR` registration.
    pub(in crate::kqueue) fn take(&mut self, mut fired: Kevent, flags: u16) -> Option<Kevent> {
        if !self.pending() {
            return None;
        }
        match self.kind {
            Kind::Timer { .. } => {
                fired.fflags = 0;
                fired.data = std::mem::take(&mut self.data) as isize;
            }
            Kind::Vnode { .. } => {
                fired.fflags = self.fflags;
                if self.eof {
                    fired.flags |= EV_EOF | EV_ONESHOT;
                }
                if flags & EV_CLEAR != 0 {
                    self.fflags = 0;
                }
            }
            Kind::Proc { .. } => {
                fired.fflags = std::mem::take(&mut self.fflags);
                fired.data = (self.data as u32 & NOTE_PDATAMASK) as isize;
                fired.flags |= EV_EOF | EV_ONESHOT;
            }
        }
        Some(fired)
    }
}

/// A host `kevent` change for `ident`'s `filter`.
fn host_change(ident: usize, filter: i16, flags: u16, fflags: u32, data: i64) -> libc::kevent {
    // SAFETY: zero is a valid `kevent`; every field that matters is set below.
    let mut change: libc::kevent = unsafe { std::mem::zeroed() };
    change.ident = ident;
    change.filter = filter;
    change.flags = flags;
    change.fflags = fflags;
    change.data = data;
    change
}

/// A private kqueue holding `change`.
fn queue_with(change: libc::kevent) -> Result<OwnedFd, c_int> {
    // SAFETY: `kqueue` takes no arguments; the descriptor is owned below.
    let queue = owned(unsafe { libc::kqueue() })?;
    // SAFETY: the queue just made, which the guest never sees, so the flag
    // cannot race an `exec`.
    if unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(crate::errno::get());
    }
    // SAFETY: one change, and no events asked for.
    let result = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &raw const change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    match result {
        0 => Ok(queue),
        _ => Err(crate::errno::get()),
    }
}

/// The next event `queue` has, without waiting.
fn next_event(queue: c_int) -> Option<libc::kevent> {
    // SAFETY: zero is a valid `kevent`, which `kevent` overwrites.
    let mut event: libc::kevent = unsafe { std::mem::zeroed() };
    let no_wait = timespec(0);
    // SAFETY: no changes, room for one event, and no wait.
    let count = unsafe {
        libc::kevent(
            queue,
            std::ptr::null(),
            0,
            &raw mut event,
            1,
            &raw const no_wait,
        )
    };
    (count == 1).then_some(event)
}

/// `EVFILT_TIMER`: the host's timer, counting the same nanoseconds Darwin's
/// `filt_timervalidate` would.
fn timer(change: &Kevent) -> Result<(OwnedFd, Kind), c_int> {
    let Schedule {
        first,
        period,
        absolute,
        ..
    } = schedule(change)?;
    let first = first.unwrap_or(1);
    // FIXME (ACCURACY): the host counts a relative timer in its uptime, so a
    // `NOTE_MACH_CONTINUOUS_TIME` timer does not count a suspend.
    let (flags, nanos) = match period {
        0 => (libc::EV_ADD | libc::EV_ONESHOT, first),
        period => (libc::EV_ADD, period),
    };
    let nanos = i64::try_from(nanos).map_err(|_| libc::ERANGE)?;
    let host = host_change(0, libc::EVFILT_TIMER, flags, libc::NOTE_NSECONDS, nanos);
    Ok((queue_with(host)?, Kind::Timer { absolute }))
}

/// `EVFILT_VNODE`: the host's, on a duplicate of the descriptor
/// `change.ident` names.
fn vnode(change: &Kevent) -> Result<(OwnedFd, Kind), c_int> {
    refuse("EVFILT_VNODE", change.fflags, VNODE_REFUSED)?;
    let (guest, _) = vnode_target(change)?;
    // SAFETY: a descriptor the guest named; the duplicate is owned below.
    let file = owned(unsafe { libc::fcntl(guest, libc::F_DUPFD_CLOEXEC, 0) })?;
    let links = status(file.as_raw_fd())?.st_nlink as u64;
    let host = host_change(
        file.as_raw_fd() as usize,
        libc::EVFILT_VNODE,
        libc::EV_ADD | libc::EV_CLEAR,
        VNODE_NOTES,
        0,
    );
    let queue = queue_with(host).map_err(|errno| match errno {
        // A descriptor the host's filter does not take is not a vnode.
        libc::EOPNOTSUPP => libc::EINVAL,
        errno => errno,
    })?;
    Ok((
        queue,
        Kind::Vnode {
            file,
            requested: change.fflags,
            links,
        },
    ))
}

/// `EVFILT_PROC`: the host's `NOTE_EXIT` on the process `change.ident` names.
fn process(change: &Kevent) -> Result<(OwnedFd, Kind), c_int> {
    check_process_notes(change.fflags)?;
    let guest = i32::try_from(change.ident).map_err(|_| libc::ESRCH)?;
    if guest <= 0 {
        return Err(libc::ESRCH);
    }
    let host = crate::process::host_pid(guest);
    let queue = queue_with(host_change(
        host as usize,
        libc::EVFILT_PROC,
        libc::EV_ADD,
        libc::NOTE_EXIT,
        0,
    ))?;
    // The host reports a zombie's exit at once; Darwin finds no process that
    // has already exited.
    if next_event(queue.as_raw_fd()).is_some() {
        return Err(libc::ESRCH);
    }
    // FIXME (ACCURACY): Darwin also gives the status to a process allowed to
    // signal the target; only a parent can read it here.
    if change.fflags & NOTE_EXITSTATUS != 0 && !is_child(change.ident) {
        return Err(exit_status_refused());
    }
    Ok((
        queue,
        Kind::Proc {
            requested: change.fflags,
        },
    ))
}

/// Whether the process a guest pid names is this process's child.
fn is_child(guest: usize) -> bool {
    let Ok(guest) = i32::try_from(guest) else {
        return false;
    };
    let host = crate::process::host_pid(guest);
    // SAFETY: zero is a valid `siginfo_t`, which `waitid` may fill in.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let options = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    // SAFETY: a pid and a `siginfo_t` this function owns; nothing is reaped.
    unsafe { libc::waitid(libc::P_PID, host as libc::id_t, &raw mut info, options) == 0 }
}

/// Darwin's wait status for the host's: the same layout, with the signal
/// renumbered.
fn darwin_wait_status(status: c_int) -> u32 {
    let signal = |host| crate::signal::signal_from_host(host).unwrap_or(host) as u32;
    if libc::WIFEXITED(status) {
        (libc::WEXITSTATUS(status) as u32 & EXIT_CODE_MASK) << EXIT_CODE_SHIFT
    } else if libc::WIFSIGNALED(status) {
        let core = match libc::WCOREDUMP(status) {
            true => WCOREFLAG,
            false => 0,
        };
        signal(libc::WTERMSIG(status)) | core
    } else {
        0
    }
}
