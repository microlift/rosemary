//! The host objects behind the filters on FreeBSD: a private host kqueue per
//! registration, holding the host's own filter of the same name. A kqueue
//! polls readable while it has an event, which is all the backend needs.

use std::ffi::c_int;
use std::os::fd::{AsRawFd, OwnedFd};

use super::*;
use crate::kqueue::host_kevent;

/// The `EVFILT_VNODE` notes FreeBSD shares with Darwin, bit for bit; the
/// host is asked for all of them, so a registration can be told every part of
/// an event it asked for any part of, as Darwin does.
const VNODE_NOTES: u32 =
    NOTE_DELETE | NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB | NOTE_LINK | NOTE_RENAME | NOTE_REVOKE;

/// What an `EVFILT_VNODE` registration keeps beside its queue.
pub(super) struct VnodeWatch {
    /// A duplicate of the guest's descriptor, so a guest closing its own
    /// does not take the host registration with it.
    file: OwnedFd,
    links: u64,
}

impl VnodeWatch {
    /// The notes `queue` reported since the last call, and whether the file
    /// was revoked.
    pub(super) fn collect(&mut self, queue: c_int) -> (u32, bool) {
        let mut hints = 0;
        let mut revoked = false;
        while let Some(event) = next_event(queue) {
            hints |= event.fflags & VNODE_NOTES;
            revoked |= event.flags & libc::EV_EOF != 0;
        }
        // FreeBSD reports an unlink as `NOTE_DELETE` alone; Darwin adds
        // `NOTE_LINK`, as the link count changed too.
        if hints != 0
            && let Ok(now) = status(self.file.as_raw_fd())
            && now.st_nlink as u64 != self.links
        {
            hints |= NOTE_LINK;
            self.links = now.st_nlink as u64;
        }
        (hints, revoked)
    }
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

/// A host `kevent` change for `ident`'s `filter` with `fflags` and `data`.
fn host_change(ident: usize, filter: i16, flags: u16, fflags: u32, data: i64) -> libc::kevent {
    let mut change = host_kevent(ident, filter, flags, 0);
    change.fflags = fflags;
    change.data = data;
    change
}

/// The next event `queue` has, without waiting.
fn next_event(queue: c_int) -> Option<libc::kevent> {
    let mut event = host_kevent(0, 0, 0, 0);
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
pub(super) fn timer(schedule: Schedule) -> Result<OwnedFd, c_int> {
    let first = schedule.first.unwrap_or(1);
    // FIXME (ACCURACY): the host counts a relative timer in its uptime, so a
    // `NOTE_MACH_CONTINUOUS_TIME` timer does not count a suspend.
    let (flags, nanos) = match schedule.period {
        0 => (libc::EV_ADD | libc::EV_ONESHOT, first),
        period => (libc::EV_ADD, period),
    };
    let nanos = i64::try_from(nanos).map_err(|_| libc::ERANGE)?;
    queue_with(host_change(
        0,
        libc::EVFILT_TIMER,
        flags,
        libc::NOTE_NSECONDS,
        nanos,
    ))
}

/// How many times the timer in `queue` expired since the last call.
pub(super) fn expirations(queue: c_int) -> u64 {
    let mut expirations: u64 = 0;
    while let Some(event) = next_event(queue) {
        expirations = expirations.saturating_add(event.data as u64);
    }
    expirations
}

/// `EVFILT_VNODE`: the host's, on a duplicate of the guest's descriptor.
pub(super) fn vnode(guest: c_int, _: libc::mode_t) -> Result<(OwnedFd, VnodeWatch), c_int> {
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
    Ok((queue, VnodeWatch { file, links }))
}

/// `EVFILT_PROC`: the host's `NOTE_EXIT` on host process `pid`.
pub(super) fn process(pid: libc::pid_t) -> Result<OwnedFd, c_int> {
    let queue = queue_with(host_change(
        pid as usize,
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
    Ok(queue)
}

/// Darwin's wait status of the process `queue` watches, once it has exited.
pub(super) fn exit_status(queue: c_int) -> Option<u32> {
    let mut status = None;
    while let Some(event) = next_event(queue) {
        if event.fflags & NOTE_EXIT != 0 {
            status = Some(crate::wait::status_to_darwin(event.data as c_int) as u32);
        }
    }
    status
}

/// Whether host process `pid` is this process's child.
pub(super) fn is_child(_: c_int, pid: libc::pid_t) -> bool {
    // SAFETY: zero is a valid `siginfo_t`, which `waitid` may fill in.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let options = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    // SAFETY: a pid and a `siginfo_t` this function owns; nothing is reaped.
    unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &raw mut info, options) == 0 }
}
