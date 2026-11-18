//! The host objects behind the filters on Linux: a timerfd, an inotify
//! instance, a pidfd.

use std::ffi::c_int;
use std::os::fd::{AsRawFd, OwnedFd};

use super::*;
use crate::kqueue::{EV_CLEAR, EV_EOF, EV_ONESHOT};

/// The path that names whatever `fd` is open on, deleted or not.
fn proc_path(fd: c_int) -> std::ffi::CString {
    std::ffi::CString::new(format!("/proc/self/fd/{fd}")).expect("no interior NUL")
}

pub(in crate::kqueue) struct Source {
    /// What the backend watches.
    fd: OwnedFd,
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
        /// An `O_PATH` reference of this source's own, so a guest closing its
        /// descriptor does not take the file from under the watch.
        file: OwnedFd,
        directory: bool,
        /// The inotify watch on a directory's parent, which is the only
        /// place a removed directory is heard of while it is still open.
        parent: Option<c_int>,
        requested: u32,
        size: i64,
        links: u64,
    },
    Proc {
        requested: u32,
    },
}

impl Source {
    /// The host object behind a new registration. Errors are host `errno`s.
    pub(in crate::kqueue) fn open(change: &Kevent) -> Result<Self, c_int> {
        let (fd, kind) = match change.filter {
            EVFILT_TIMER => timer(change)?,
            EVFILT_VNODE => vnode(change)?,
            EVFILT_PROC => process(change)?,
            _ => return Err(libc::EINVAL),
        };
        Ok(Self {
            fd,
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
                if change.fflags & NOTE_EXITSTATUS != 0 && !is_child(self.fd.as_raw_fd()) {
                    return Err(exit_status_refused());
                }
                *requested = change.fflags;
                Ok(None)
            }
        }
    }

    pub(in crate::kqueue) fn fd(&self) -> c_int {
        self.fd.as_raw_fd()
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
        let fd = self.fd.as_raw_fd();
        match &mut self.kind {
            Kind::Timer { .. } => {
                let mut expirations: u64 = 0;
                // SAFETY: eight bytes into a `u64`; the timerfd is non-blocking.
                let read =
                    unsafe { libc::read(fd, (&raw mut expirations).cast(), size_of::<u64>()) };
                if read == size_of::<u64>() as isize {
                    self.data = self.data.saturating_add(expirations);
                }
            }
            Kind::Vnode { .. } => self.collect_vnode(),
            Kind::Proc { requested } => {
                if self.eof || !exited(fd) {
                    return;
                }
                self.eof = true;
                self.fflags = *requested & (NOTE_EXIT | NOTE_EXITSTATUS);
                if *requested & NOTE_EXITSTATUS != 0 {
                    self.data = exit_status(fd) as u64;
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

    fn collect_vnode(&mut self) {
        let Kind::Vnode {
            file,
            directory,
            parent,
            requested,
            size,
            links,
        } = &mut self.kind
        else {
            return;
        };
        // Aligned for `inotify_event`, and room for many at once.
        let mut buffer = [0u64; 512];
        let mut hints = 0;
        let mut revoked = false;
        let mut recheck_links = false;
        let mut moved = false;
        loop {
            // SAFETY: the buffer, with its size; the inotify is non-blocking.
            let read = unsafe {
                libc::read(
                    self.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    size_of_val(&buffer),
                )
            };
            if read <= 0 {
                break;
            }
            // SAFETY: the kernel wrote `read` bytes of whole events.
            let bytes =
                unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), read as usize) };
            let mut offset = 0;
            while offset + size_of::<libc::inotify_event>() <= bytes.len() {
                // SAFETY: a whole `inotify_event` header at `offset`.
                let event = unsafe {
                    bytes
                        .as_ptr()
                        .add(offset)
                        .cast::<libc::inotify_event>()
                        .read_unaligned()
                };
                offset += size_of::<libc::inotify_event>() + event.len as usize;
                let from_parent = Some(event.wd) == *parent;
                let named = event.len != 0;
                let mask = event.mask;
                if from_parent {
                    // A directory left the parent: perhaps the watched one.
                    recheck_links |= mask & libc::IN_ISDIR != 0;
                    continue;
                }
                if mask & libc::IN_UNMOUNT != 0 {
                    revoked = true;
                } else if named {
                    // A directory's entries changed: Darwin's `NOTE_WRITE` on
                    // it, and `NOTE_LINK` too when the entry is a directory,
                    // whose `..` is one of its links.
                    if mask
                        & (libc::IN_CREATE
                            | libc::IN_DELETE
                            | libc::IN_MOVED_FROM
                            | libc::IN_MOVED_TO)
                        != 0
                    {
                        hints |= NOTE_WRITE;
                        if mask & libc::IN_ISDIR != 0 {
                            hints |= NOTE_LINK;
                        }
                    }
                } else if mask & libc::IN_MODIFY != 0 {
                    hints |= NOTE_WRITE;
                    if let Ok(now) = status(file.as_raw_fd()) {
                        if now.st_size > *size {
                            hints |= NOTE_EXTEND;
                        }
                        *size = now.st_size;
                    }
                } else if mask & libc::IN_ATTRIB != 0 {
                    // Linux tells a link count change apart from other
                    // attribute changes only by the count itself.
                    match status(file.as_raw_fd()) {
                        Ok(now) if now.st_nlink as u64 != *links => recheck_links = true,
                        _ => hints |= NOTE_ATTRIB,
                    }
                } else if mask & libc::IN_MOVE_SELF != 0 {
                    hints |= NOTE_RENAME;
                    moved = true;
                } else if mask & libc::IN_DELETE_SELF != 0 {
                    hints |= NOTE_DELETE;
                }
            }
        }
        if recheck_links && let Ok(now) = status(file.as_raw_fd()) {
            let count = now.st_nlink as u64;
            if count != *links {
                hints |= NOTE_LINK;
                if count == 0 {
                    hints |= NOTE_DELETE;
                }
                *links = count;
            }
        }
        if moved && *directory {
            *parent = watch_parent(self.fd.as_raw_fd(), file.as_raw_fd(), *parent);
        }
        // Darwin records every bit of an event the registration asked for any
        // part of, and a revoke whatever was asked.
        if *requested & hints != 0 {
            self.fflags |= hints;
        }
        if revoked {
            self.fflags |= *requested & NOTE_REVOKE;
            self.eof = true;
        }
    }
}

/// `EVFILT_TIMER`: a timerfd armed as Darwin's `filt_timervalidate` would.
fn timer(change: &Kevent) -> Result<(OwnedFd, Kind), c_int> {
    let Schedule {
        first,
        period,
        absolute,
        clock,
    } = schedule(change)?;
    // SAFETY: plain flags; the descriptor is owned below.
    let fd = owned(unsafe { libc::timerfd_create(clock, libc::TFD_NONBLOCK | libc::TFD_CLOEXEC) })?;
    let spec = libc::itimerspec {
        it_interval: timespec(period),
        // A zero `it_value` disarms a timerfd, so "at once" is a nanosecond.
        it_value: timespec(first.unwrap_or(1)),
    };
    // SAFETY: the timerfd just created and a spec this function owns.
    if unsafe { libc::timerfd_settime(fd.as_raw_fd(), 0, &raw const spec, std::ptr::null_mut()) }
        < 0
    {
        return Err(crate::errno::get());
    }
    Ok((fd, Kind::Timer { absolute }))
}

/// `EVFILT_VNODE`: an inotify instance watching the file `change.ident` is
/// open on.
fn vnode(change: &Kevent) -> Result<(OwnedFd, Kind), c_int> {
    refuse("EVFILT_VNODE", change.fflags, VNODE_REFUSED)?;
    let (guest, kind) = vnode_target(change)?;
    // SAFETY: a NUL-terminated path; the descriptor is owned below.
    let file =
        owned(unsafe { libc::open(proc_path(guest).as_ptr(), libc::O_PATH | libc::O_CLOEXEC) })?;
    // SAFETY: plain flags; the descriptor is owned below.
    let inotify = owned(unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) })?;
    let mask = libc::IN_MODIFY
        | libc::IN_ATTRIB
        | libc::IN_MOVE_SELF
        | libc::IN_DELETE_SELF
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO;
    // SAFETY: a NUL-terminated path naming this source's own reference.
    let watch = unsafe {
        libc::inotify_add_watch(
            inotify.as_raw_fd(),
            proc_path(file.as_raw_fd()).as_ptr(),
            mask,
        )
    };
    if watch < 0 {
        return Err(match crate::errno::get() {
            // Not a file inotify can watch (a pipe, say): not a vnode either.
            libc::ENOENT | libc::EACCES => libc::EINVAL,
            errno => errno,
        });
    }
    let now = status(file.as_raw_fd())?;
    let directory = kind == libc::S_IFDIR;
    let parent = match directory {
        true => watch_parent(inotify.as_raw_fd(), file.as_raw_fd(), None),
        false => None,
    };
    Ok((
        inotify,
        Kind::Vnode {
            file,
            directory,
            parent,
            requested: change.fflags,
            size: now.st_size,
            links: now.st_nlink as u64,
        },
    ))
}

/// Watch the current parent of directory `file` for entries leaving it, in
/// place of `previous`. Unlinking a file is an attribute change on the file
/// itself; removing a directory is heard of only on its parent.
fn watch_parent(inotify: c_int, file: c_int, previous: Option<c_int>) -> Option<c_int> {
    if let Some(previous) = previous {
        // SAFETY: a watch on this source's own inotify.
        unsafe { libc::inotify_rm_watch(inotify, previous) };
    }
    let dotdot = c"..";
    // SAFETY: an `O_PATH` directory reference and a NUL-terminated name.
    let parent =
        owned(unsafe { libc::openat(file, dotdot.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) })
            .ok()?;
    // SAFETY: a NUL-terminated path naming the parent just opened.
    let watch = unsafe {
        libc::inotify_add_watch(
            inotify,
            proc_path(parent.as_raw_fd()).as_ptr(),
            libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_ONLYDIR,
        )
    };
    (watch >= 0).then_some(watch)
}

/// `EVFILT_PROC`: a pidfd for the process `change.ident` names.
fn process(change: &Kevent) -> Result<(OwnedFd, Kind), c_int> {
    check_process_notes(change.fflags)?;
    let guest = i32::try_from(change.ident).map_err(|_| libc::ESRCH)?;
    if guest <= 0 {
        return Err(libc::ESRCH);
    }
    let host = crate::process::host_pid(guest);
    // SAFETY: `pidfd_open` takes a pid and flags; the descriptor is owned below.
    let fd = owned(unsafe { libc::syscall(libc::SYS_pidfd_open, host, 0) } as c_int)?;
    // Darwin finds no process that has already exited, zombie or not.
    if exited(fd.as_raw_fd()) {
        return Err(libc::ESRCH);
    }
    // FIXME (ACCURACY): Darwin also gives the status to a process allowed to
    // signal the target; only a parent can read it here.
    if change.fflags & NOTE_EXITSTATUS != 0 && !is_child(fd.as_raw_fd()) {
        return Err(exit_status_refused());
    }
    Ok((
        fd,
        Kind::Proc {
            requested: change.fflags,
        },
    ))
}

fn exited(pidfd: c_int) -> bool {
    let mut probe = libc::pollfd {
        fd: pidfd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one `pollfd` this function owns, and no wait.
    unsafe { libc::poll(&raw mut probe, 1, 0) > 0 && probe.revents & libc::POLLIN != 0 }
}

/// Ask about `pidfd`'s process as its parent, without reaping it.
fn peek_child(pidfd: c_int) -> Option<libc::siginfo_t> {
    // SAFETY: zero is a valid `siginfo_t`, and what `WNOHANG` leaves when the
    // child is still running.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let options = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    // SAFETY: a pidfd and a `siginfo_t` this function owns.
    match unsafe { libc::waitid(libc::P_PIDFD, pidfd as libc::id_t, &raw mut info, options) } {
        0 => Some(info),
        _ => None,
    }
}

fn is_child(pidfd: c_int) -> bool {
    peek_child(pidfd).is_some()
}

/// The Darwin wait status of `pidfd`'s exited child, left for the guest to
/// reap.
// FIXME (ACCURACY): Darwin records the status at exit; a child the guest
// reaped before the queue looked reports a status of 0.
fn exit_status(pidfd: c_int) -> u32 {
    let Some(info) = peek_child(pidfd) else {
        return 0;
    };
    // SAFETY: `waitid` filled in a child's exit.
    let (code, status) = unsafe { (info.si_code, info.si_status()) };
    let signal = || crate::signal::signal_from_host(status).unwrap_or(status) as u32;
    match code {
        libc::CLD_EXITED => (status as u32 & EXIT_CODE_MASK) << EXIT_CODE_SHIFT,
        libc::CLD_KILLED => signal(),
        libc::CLD_DUMPED => signal() | WCOREFLAG,
        _ => 0,
    }
}
