//! The filters that watch something other than a descriptor's readiness: a
//! timer, a file, a process. Each registration owns a host descriptor that
//! turns readable when its event happens; the queue's backend watches that
//! descriptor to end a wait, and [`Source::collect`] reads what it has to say
//! into the pending event. What the descriptor is depends on the host: see
//! `linux` and `freebsd`. This module holds what Darwin means by each filter.

use std::ffi::c_int;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use super::{EV_CLEAR, EV_EOF, EV_ONESHOT, EVFILT_PROC, EVFILT_TIMER, EVFILT_VNODE, Kevent};

#[cfg(target_os = "freebsd")]
mod freebsd;
#[cfg(target_os = "freebsd")]
use freebsd as host;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as host;

/// The filters this module answers.
pub(super) const FILTERS: [i16; 3] = [EVFILT_TIMER, EVFILT_VNODE, EVFILT_PROC];

// `EVFILT_TIMER` `fflags`, from `<sys/event.h>`.
const NOTE_SECONDS: u32 = 0x0000_0001;
const NOTE_USECONDS: u32 = 0x0000_0002;
const NOTE_NSECONDS: u32 = 0x0000_0004;
const NOTE_ABSOLUTE: u32 = 0x0000_0008;
const NOTE_MACH_CONTINUOUS_TIME: u32 = 0x0000_0080;
const NOTE_MACHTIME: u32 = 0x0000_0100;
const TIMER_UNITS: u32 = NOTE_SECONDS | NOTE_USECONDS | NOTE_NSECONDS | NOTE_MACHTIME;

// `EVFILT_VNODE` `fflags`.
const NOTE_DELETE: u32 = 0x0000_0001;
const NOTE_WRITE: u32 = 0x0000_0002;
const NOTE_EXTEND: u32 = 0x0000_0004;
const NOTE_ATTRIB: u32 = 0x0000_0008;
const NOTE_LINK: u32 = 0x0000_0010;
const NOTE_RENAME: u32 = 0x0000_0020;
const NOTE_REVOKE: u32 = 0x0000_0040;
const VNODE_REFUSED: &[(u32, &str)] = &[
    (0x0000_0080, "NOTE_NONE"),
    (0x0000_0100, "NOTE_FUNLOCK"),
    (0x0000_0200, "NOTE_LEASE_DOWNGRADE"),
    (0x0000_0400, "NOTE_LEASE_RELEASE"),
];

// `EVFILT_PROC` `fflags`.
const NOTE_EXIT: u32 = 0x8000_0000;
const NOTE_EXITSTATUS: u32 = 0x0400_0000;
/// The bits of an exit event's `data` that carry the wait status.
const NOTE_PDATAMASK: u32 = 0x000f_ffff;
/// Retired in 10.5; Darwin answers them `ENOTSUP`.
const NOTE_TRACK: u32 = 0x0000_0001;
const NOTE_TRACKERR: u32 = 0x0000_0002;
const NOTE_CHILD: u32 = 0x0000_0004;
const PROC_REFUSED: &[(u32, &str)] = &[
    (0x4000_0000, "NOTE_FORK"),
    (0x2000_0000, "NOTE_EXEC"),
    (0x1000_0000, "NOTE_REAP"),
    (0x0800_0000, "NOTE_SIGNAL"),
    (0x0200_0000, "NOTE_EXIT_DETAIL"),
];

/// Where Darwin's `W_EXITCODE` places an exit code in a wait status.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
const EXIT_CODE_SHIFT: u32 = 8;

const NANOS_PER_SEC: u64 = 1_000_000_000;
const NANOS_PER_MILLI: u64 = 1_000_000;
const NANOS_PER_MICRO: u64 = 1_000;

/// Say once per note that a registration asked for one this host cannot
/// observe, so the `EINVAL` it gets back has a name.
fn say_refused(filter: &str, note: &str) {
    use std::sync::Mutex;
    static SAID: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    let key = (filter.to_owned(), note.to_owned());
    if said.contains(&key) {
        return;
    }
    said.push(key);
    eprintln!("rosemary: kevent {filter}: {note} is not supported on this host - refused");
}

/// Refuse `fflags` naming any of `refused`, by name.
fn refuse(filter: &str, fflags: u32, refused: &[(u32, &str)]) -> Result<(), c_int> {
    let mut refusal = Ok(());
    for (_, name) in refused.iter().filter(|(bit, _)| fflags & bit != 0) {
        say_refused(filter, name);
        refusal = Err(libc::EINVAL);
    }
    refusal
}

fn owned(fd: c_int) -> Result<OwnedFd, c_int> {
    match fd {
        fd if fd < 0 => Err(crate::errno::get()),
        // SAFETY: a descriptor the caller just created and nothing else owns.
        fd => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
    }
}

fn status(fd: c_int) -> Result<libc::stat, c_int> {
    // SAFETY: zero is a valid `stat`, which `fstat` overwrites.
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a `stat` this function owns.
    match unsafe { libc::fstat(fd, &raw mut status) } {
        0 => Ok(status),
        _ => Err(crate::errno::get()),
    }
}

fn timespec(nanos: u64) -> libc::timespec {
    libc::timespec {
        tv_sec: (nanos / NANOS_PER_SEC) as libc::time_t,
        tv_nsec: (nanos % NANOS_PER_SEC) as libc::c_long,
    }
}

fn now(clock: libc::clockid_t) -> u64 {
    // SAFETY: zero is a valid `timespec`, which `clock_gettime` overwrites.
    let mut now: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: a `timespec` this function owns.
    unsafe { libc::clock_gettime(clock, &raw mut now) };
    now.tv_sec as u64 * NANOS_PER_SEC + now.tv_nsec as u64
}

/// When a timer registration first fires, and how often after that, as
/// Darwin's `filt_timervalidate` reads it.
struct Schedule {
    /// The first expiry from now; `None` fires at once.
    first: Option<u64>,
    /// Zero for a timer that fires once.
    period: u64,
    absolute: bool,
    /// What `mach_absolute_time` or `mach_continuous_time` count here. A
    /// FreeBSD host times a registration on its own uptime clock instead.
    #[cfg_attr(target_os = "freebsd", allow(dead_code))]
    clock: libc::clockid_t,
}

fn schedule(change: &Kevent) -> Result<Schedule, c_int> {
    let fflags = change.fflags;
    let unit = match fflags & TIMER_UNITS {
        NOTE_SECONDS => NANOS_PER_SEC,
        NOTE_USECONDS => NANOS_PER_MICRO,
        NOTE_NSECONDS => 1,
        // `mach_absolute_time` counts nanoseconds of `CLOCK_MONOTONIC` here.
        NOTE_MACHTIME => 1,
        0 => NANOS_PER_MILLI,
        _ => return Err(libc::EINVAL),
    };
    // `mach_continuous_time` and `mach_absolute_time`, as `mach` answers them.
    let clock = match fflags & NOTE_MACH_CONTINUOUS_TIME {
        0 => libc::CLOCK_MONOTONIC,
        _ => crate::mach::CONTINUOUS_CLOCK,
    };
    let absolute = fflags & NOTE_ABSOLUTE != 0;
    let scaled = || (change.data as u64).checked_mul(unit).ok_or(libc::ERANGE);
    // The first expiry from now and the period; `None` fires at once.
    let (first, period) = if absolute && fflags & NOTE_MACHTIME != 0 {
        (
            scaled()?.checked_sub(now(clock)).filter(|left| *left > 0),
            0,
        )
    } else if absolute {
        // A calendar deadline is converted to an interval once, as Darwin
        // does, so a later change to the wall clock does not move it.
        (
            scaled()?
                .checked_sub(now(libc::CLOCK_REALTIME))
                .filter(|left| *left > 0),
            0,
        )
    } else if change.data < 0 {
        // Darwin fires a negative interval once, at once.
        (None, 0)
    } else {
        let interval = scaled()?;
        (Some(interval).filter(|ns| *ns > 0), interval)
    };
    Ok(Schedule {
        first,
        period,
        absolute,
        clock,
    })
}

/// The descriptor an `EVFILT_VNODE` registration names, and its file type.
fn vnode_target(change: &Kevent) -> Result<(c_int, libc::mode_t), c_int> {
    let guest = c_int::try_from(change.ident).map_err(|_| libc::EBADF)?;
    let kind = status(guest)?.st_mode & libc::S_IFMT;
    // Darwin attaches the filter to vnodes only; a socket, and the anonymous
    // descriptors behind kqueues and the like, are not.
    if !matches!(
        kind,
        libc::S_IFREG
            | libc::S_IFDIR
            | libc::S_IFLNK
            | libc::S_IFIFO
            | libc::S_IFCHR
            | libc::S_IFBLK
    ) {
        return Err(libc::EINVAL);
    }
    Ok((guest, kind))
}

fn check_process_notes(fflags: u32) -> Result<(), c_int> {
    if fflags & (NOTE_TRACK | NOTE_TRACKERR | NOTE_CHILD) != 0 {
        return Err(libc::ENOTSUP);
    }
    refuse("EVFILT_PROC", fflags, PROC_REFUSED)
}

fn exit_status_refused() -> c_int {
    say_refused(
        "EVFILT_PROC",
        "NOTE_EXITSTATUS for a process that is not a child",
    );
    libc::ENOTSUP
}

pub(super) struct Source {
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
        requested: u32,
        watch: host::VnodeWatch,
    },
    Proc {
        requested: u32,
        /// The host pid, for hosts that cannot ask the watch itself.
        pid: libc::pid_t,
    },
}

impl Source {
    /// The host object behind a new registration. Errors are host `errno`s.
    pub(super) fn open(change: &Kevent) -> Result<Self, c_int> {
        let (fd, kind) = match change.filter {
            EVFILT_TIMER => {
                let schedule = schedule(change)?;
                let absolute = schedule.absolute;
                (host::timer(schedule)?, Kind::Timer { absolute })
            }
            EVFILT_VNODE => {
                refuse("EVFILT_VNODE", change.fflags, VNODE_REFUSED)?;
                let (guest, kind) = vnode_target(change)?;
                let (fd, watch) = host::vnode(guest, kind)?;
                let requested = change.fflags;
                (fd, Kind::Vnode { requested, watch })
            }
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
    pub(super) fn touch(&mut self, change: &Kevent) -> Result<Option<Self>, c_int> {
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
            Kind::Proc { requested, pid } => {
                check_process_notes(change.fflags)?;
                if change.fflags & NOTE_EXITSTATUS != 0
                    && !host::is_child(self.fd.as_raw_fd(), *pid)
                {
                    return Err(exit_status_refused());
                }
                *requested = change.fflags;
                Ok(None)
            }
        }
    }

    pub(super) fn fd(&self) -> c_int {
        self.fd.as_raw_fd()
    }

    /// The `flags` Darwin forces on a registration of this kind.
    pub(super) fn forced_flags(&self) -> u16 {
        match self.kind {
            Kind::Timer { absolute: true } => EV_CLEAR | EV_ONESHOT,
            Kind::Timer { absolute: false } | Kind::Proc { .. } => EV_CLEAR,
            Kind::Vnode { .. } => 0,
        }
    }

    /// Read what the host reported since the last call into the pending event.
    pub(super) fn collect(&mut self) {
        let fd = self.fd.as_raw_fd();
        match &mut self.kind {
            Kind::Timer { .. } => {
                self.data = self.data.saturating_add(host::expirations(fd));
            }
            Kind::Vnode { requested, watch } => {
                let (hints, revoked) = watch.collect(fd);
                // Darwin records every bit of an event the registration asked
                // for any part of, and a revoke whatever was asked.
                if *requested & hints != 0 {
                    self.fflags |= hints;
                }
                if revoked {
                    self.fflags |= *requested & NOTE_REVOKE;
                    self.eof = true;
                }
            }
            Kind::Proc { requested, .. } => {
                if self.eof {
                    return;
                }
                let Some(status) = host::exit_status(fd) else {
                    return;
                };
                self.eof = true;
                self.fflags = *requested & (NOTE_EXIT | NOTE_EXITSTATUS);
                if *requested & NOTE_EXITSTATUS != 0 {
                    self.data = status as u64;
                }
            }
        }
    }

    /// Whether there is an event to report.
    pub(super) fn pending(&self) -> bool {
        match self.kind {
            Kind::Timer { .. } => self.data != 0,
            Kind::Vnode { .. } => self.fflags != 0 || self.eof,
            Kind::Proc { .. } => self.fflags != 0,
        }
    }

    /// The event `fired` (the registration's own) becomes, if one is pending,
    /// and what is left pending afterwards. Darwin clears what it reported
    /// only for an `EV_CLEAR` registration.
    pub(super) fn take(&mut self, mut fired: Kevent, flags: u16) -> Option<Kevent> {
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

/// `EVFILT_PROC`: the host's watch on the process `change.ident` names.
fn process(change: &Kevent) -> Result<(OwnedFd, Kind), c_int> {
    check_process_notes(change.fflags)?;
    let guest = i32::try_from(change.ident).map_err(|_| libc::ESRCH)?;
    if guest <= 0 {
        return Err(libc::ESRCH);
    }
    let pid = crate::process::host_pid(guest);
    let fd = host::process(pid)?;
    // FIXME (ACCURACY): Darwin also gives the status to a process allowed to
    // signal the target; only a parent can read it here.
    if change.fflags & NOTE_EXITSTATUS != 0 && !host::is_child(fd.as_raw_fd(), pid) {
        return Err(exit_status_refused());
    }
    let requested = change.fflags;
    Ok((fd, Kind::Proc { requested, pid }))
}

#[cfg(test)]
mod tests {
    use super::super::{
        EV_ADD, EV_CLEAR, EV_DELETE, EV_EOF, EV_ERROR, EV_ONESHOT, EV_RECEIPT, kevent, kqueue,
    };
    use super::*;
    use std::ffi::CString;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    const NO_WAIT: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    /// Apply `change` with `EV_RECEIPT`: 0, or the Darwin errno it failed with.
    fn register(queue: c_int, change: Kevent) -> c_int {
        let change = Kevent {
            flags: change.flags | EV_RECEIPT,
            ..change
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: one change and room for one event.
        let n = unsafe { kevent(queue, &raw const change, 1, got.as_mut_ptr(), 1, &NO_WAIT) };
        assert_eq!(n, 1);
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        got[0].data as c_int
    }

    fn wait(queue: c_int, within: Duration) -> Option<Kevent> {
        let timeout = timespec(within.as_nanos() as u64);
        let mut got = [Kevent::default(); 1];
        // SAFETY: no changes and room for one event.
        let n = unsafe { kevent(queue, std::ptr::null(), 0, got.as_mut_ptr(), 1, &timeout) };
        assert!(n >= 0);
        (n == 1).then_some(got[0])
    }

    fn close(fd: c_int) {
        // SAFETY: a descriptor the test owns.
        unsafe { crate::net::descriptor::close(fd) };
    }

    fn timer(fflags: u32, data: isize, flags: u16) -> Kevent {
        Kevent {
            ident: 1,
            filter: EVFILT_TIMER,
            flags: EV_ADD | flags,
            fflags,
            data,
            udata: 0,
        }
    }

    /// The timer fires once `after` has passed, and not before.
    fn fires_after(fflags: u32, data: isize, after: Duration) {
        let queue = kqueue();
        let start = Instant::now();
        assert_eq!(register(queue, timer(fflags, data, 0)), 0);
        let event = wait(queue, after + Duration::from_secs(2)).expect("the timer fired");
        assert!(start.elapsed() >= after);
        assert_eq!(event.filter, EVFILT_TIMER);
        assert_eq!(event.fflags, 0);
        assert_eq!(event.data, 1);
        close(queue);
    }

    #[test]
    fn a_timer_counts_milliseconds_by_default() {
        fires_after(0, 30, Duration::from_millis(30));
    }

    #[test]
    fn a_timer_counts_seconds_with_note_seconds() {
        fires_after(NOTE_SECONDS, 1, Duration::from_secs(1));
    }

    #[test]
    fn a_timer_counts_microseconds_with_note_useconds() {
        fires_after(NOTE_USECONDS, 30_000, Duration::from_millis(30));
    }

    #[test]
    fn a_timer_counts_nanoseconds_with_note_nseconds() {
        fires_after(NOTE_NSECONDS, 30_000_000, Duration::from_millis(30));
    }

    #[test]
    fn a_repeating_timer_reports_every_interval_that_passed() {
        let queue = kqueue();
        assert_eq!(register(queue, timer(0, 10, 0)), 0);
        std::thread::sleep(Duration::from_millis(55));
        let event = wait(queue, Duration::ZERO).expect("the timer fired");
        assert!(event.data >= 4, "{} intervals", event.data);
        // It keeps going after it is reported.
        assert!(wait(queue, Duration::from_secs(1)).is_some());
        close(queue);
    }

    #[test]
    fn an_absolute_timer_fires_once_at_its_calendar_deadline() {
        let queue = kqueue();
        let deadline = now(libc::CLOCK_REALTIME) / NANOS_PER_MICRO + 30_000;
        let change = timer(NOTE_ABSOLUTE | NOTE_USECONDS, deadline as isize, 0);
        assert_eq!(register(queue, change), 0);
        assert!(wait(queue, Duration::ZERO).is_none());
        let event = wait(queue, Duration::from_secs(2)).expect("the timer fired");
        assert!(now(libc::CLOCK_REALTIME) / NANOS_PER_MICRO >= deadline);
        assert_eq!(event.data, 1);
        // `NOTE_ABSOLUTE` is one-shot: the registration is gone.
        let delete = Kevent {
            flags: EV_DELETE,
            ..change
        };
        assert_eq!(
            register(queue, delete),
            crate::errno::from_host(libc::ENOENT)
        );
        close(queue);
    }

    #[test]
    fn a_negative_interval_fires_once_at_once() {
        let queue = kqueue();
        assert_eq!(register(queue, timer(0, -1, 0)), 0);
        assert!(wait(queue, Duration::ZERO).is_some());
        assert!(wait(queue, Duration::from_millis(50)).is_none());
        close(queue);
    }

    #[test]
    fn a_oneshot_timer_is_deleted_once_it_fires() {
        let queue = kqueue();
        let change = timer(0, 5, EV_ONESHOT);
        assert_eq!(register(queue, change), 0);
        assert!(wait(queue, Duration::from_secs(1)).is_some());
        assert!(wait(queue, Duration::from_millis(50)).is_none());
        close(queue);
    }

    /// A fresh directory of this test's own.
    fn scratch() -> std::path::PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rosemary-kqueue-vnode-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    fn open(path: &std::path::Path) -> c_int {
        let path = CString::new(path.as_os_str().as_encoded_bytes()).expect("a path");
        // SAFETY: a NUL-terminated path.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        assert!(fd >= 0);
        fd
    }

    fn all_vnode_notes() -> u32 {
        NOTE_DELETE | NOTE_WRITE | NOTE_EXTEND | NOTE_ATTRIB | NOTE_LINK | NOTE_RENAME
    }

    /// Watch `path` for `fflags`, do `action`, and answer what was reported.
    fn vnode_event(path: &std::path::Path, fflags: u32, action: impl FnOnce()) -> Option<Kevent> {
        let queue = kqueue();
        let fd = open(path);
        let change = Kevent {
            ident: fd as usize,
            filter: EVFILT_VNODE,
            flags: EV_ADD | EV_CLEAR,
            fflags,
            ..Kevent::default()
        };
        assert_eq!(register(queue, change), 0);
        assert!(wait(queue, Duration::ZERO).is_none());
        action();
        let event = wait(queue, Duration::from_secs(1));
        close(fd);
        close(queue);
        event
    }

    /// A file of `contents` in a fresh directory.
    fn file(contents: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = scratch();
        let path = dir.join("file");
        std::fs::write(&path, contents).expect("the file");
        (dir, path)
    }

    #[test]
    fn overwriting_a_file_is_note_write() {
        let (dir, path) = file(b"abc");
        let event = vnode_event(&path, all_vnode_notes(), || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, b"x"))
                .expect("a write");
        })
        .expect("an event");
        assert_eq!(event.fflags, NOTE_WRITE);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn growing_a_file_is_note_extend() {
        let (dir, path) = file(b"abc");
        let event = vnode_event(&path, NOTE_EXTEND, || {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, b"more"))
                .expect("an append");
        })
        .expect("an event");
        assert_eq!(event.fflags, NOTE_WRITE | NOTE_EXTEND);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn changing_permissions_is_note_attrib() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = file(b"");
        let event = vnode_event(&path, all_vnode_notes(), || {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("a chmod");
        })
        .expect("an event");
        assert_eq!(event.fflags, NOTE_ATTRIB);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn renaming_a_file_is_note_rename() {
        let (dir, path) = file(b"");
        let event = vnode_event(&path, NOTE_RENAME, || {
            std::fs::rename(&path, dir.join("renamed")).expect("a rename");
        })
        .expect("an event");
        assert_eq!(event.fflags, NOTE_RENAME);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unlinking_a_file_is_note_delete() {
        let (dir, path) = file(b"");
        let event = vnode_event(&path, NOTE_DELETE, || {
            std::fs::remove_file(&path).expect("an unlink");
        })
        .expect("an event");
        assert_eq!(event.fflags, NOTE_DELETE | NOTE_LINK);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_new_hard_link_is_note_link() {
        let (dir, path) = file(b"");
        let event = vnode_event(&path, all_vnode_notes(), || {
            std::fs::hard_link(&path, dir.join("link")).expect("a link");
        })
        .expect("an event");
        assert_eq!(event.fflags, NOTE_LINK);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_new_entry_is_note_write_on_its_directory() {
        let dir = scratch();
        let event = vnode_event(&dir, all_vnode_notes(), || {
            std::fs::write(dir.join("entry"), b"").expect("a new file");
        })
        .expect("an event");
        assert_eq!(event.fflags, NOTE_WRITE);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn removing_a_directory_is_note_delete() {
        let parent = scratch();
        let dir = parent.join("doomed");
        std::fs::create_dir(&dir).expect("a directory");
        let event = vnode_event(&dir, NOTE_DELETE, || {
            std::fs::remove_dir(&dir).expect("an rmdir");
        })
        .expect("an event");
        assert_eq!(event.fflags & NOTE_DELETE, NOTE_DELETE);
        std::fs::remove_dir_all(parent).ok();
    }

    #[test]
    fn a_note_not_asked_for_does_not_fire() {
        let (dir, path) = file(b"abc");
        let event = vnode_event(&path, NOTE_RENAME, || {
            std::fs::write(&path, b"xyz").expect("a write");
        });
        assert!(event.is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_unsupported_vnode_note_is_refused() {
        let (dir, path) = file(b"");
        let fd = open(&path);
        let queue = kqueue();
        let change = Kevent {
            ident: fd as usize,
            filter: EVFILT_VNODE,
            flags: EV_ADD,
            fflags: NOTE_WRITE | VNODE_REFUSED[1].0,
            ..Kevent::default()
        };
        assert_eq!(
            register(queue, change),
            crate::errno::from_host(libc::EINVAL)
        );
        close(fd);
        close(queue);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A child that exits with `code` once its standard input closes.
    fn child(code: u8) -> std::process::Child {
        let _start = super::super::tests::child_start();
        std::process::Command::new("sh")
            .args(["-c", &format!("read _; exit {code}")])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("a shell")
    }

    fn exit_event(fflags: u32, code: u8) -> Kevent {
        let mut child = child(code);
        let queue = kqueue();
        let change = Kevent {
            ident: child.id() as usize,
            filter: EVFILT_PROC,
            flags: EV_ADD,
            fflags,
            ..Kevent::default()
        };
        assert_eq!(register(queue, change), 0);
        assert!(wait(queue, Duration::ZERO).is_none());
        drop(child.stdin.take());
        let event = wait(queue, Duration::from_secs(5)).expect("an exit");
        // The guest still reaps the child itself.
        let status = child.wait().expect("the child");
        assert_eq!(status.code(), Some(code as i32));
        // An exit ends the registration.
        let delete = Kevent {
            flags: EV_DELETE,
            ..change
        };
        assert_eq!(
            register(queue, delete),
            crate::errno::from_host(libc::ENOENT)
        );
        close(queue);
        event
    }

    #[test]
    fn a_child_exit_reports_its_status_with_note_exitstatus() {
        let event = exit_event(NOTE_EXIT | NOTE_EXITSTATUS, 3);
        assert_eq!(event.fflags, NOTE_EXIT | NOTE_EXITSTATUS);
        assert_eq!(event.data, 3 << EXIT_CODE_SHIFT);
        assert_eq!(event.flags & EV_EOF, EV_EOF);
    }

    #[test]
    fn note_exit_alone_reports_no_status() {
        let event = exit_event(NOTE_EXIT, 3);
        assert_eq!(event.fflags, NOTE_EXIT);
        assert_eq!(event.data, 0);
    }

    #[test]
    fn a_process_that_does_not_exist_is_esrch() {
        let queue = kqueue();
        // Above the kernel's `pid_max` ceiling.
        const NO_SUCH_PID: usize = 1 << 23;
        let change = Kevent {
            ident: NO_SUCH_PID,
            filter: EVFILT_PROC,
            flags: EV_ADD,
            fflags: NOTE_EXIT,
            ..Kevent::default()
        };
        assert_eq!(
            register(queue, change),
            crate::errno::from_host(libc::ESRCH)
        );
        close(queue);
    }

    #[test]
    fn an_unsupported_process_note_is_refused() {
        let mut child = child(0);
        let queue = kqueue();
        let change = Kevent {
            ident: child.id() as usize,
            filter: EVFILT_PROC,
            flags: EV_ADD,
            fflags: NOTE_EXIT | PROC_REFUSED[0].0,
            ..Kevent::default()
        };
        assert_eq!(
            register(queue, change),
            crate::errno::from_host(libc::EINVAL)
        );
        drop(child.stdin.take());
        child.wait().expect("the child");
        close(queue);
    }
}
