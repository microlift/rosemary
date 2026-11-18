//! Waking the desktop's host from SDL's wait when a guest needs it.
//!
//! SDL sleeps on the window system's connection, which the guests' sockets
//! are not part of. While it does, a thread polls those sockets and pushes an
//! event into SDL's queue when one is ready, so the host sleeps until either
//! has something rather than waking to look.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::video;

enum Phase {
    Idle,
    /// Descriptors to poll, not yet taken up.
    Asked(Vec<libc::pollfd>),
    Polling,
    Quit,
}

struct Shared {
    phase: Mutex<Phase>,
    changed: Condvar,
    /// Readable when the poll is to be cut short.
    interrupted: UnixStream,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(super) struct Waker {
    shared: Arc<Shared>,
    interrupt: UnixStream,
    thread: Option<JoinHandle<()>>,
}

impl Waker {
    pub(super) fn new() -> io::Result<Waker> {
        let (interrupt, interrupted) = UnixStream::pair()?;
        interrupt.set_nonblocking(true)?;
        interrupted.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            phase: Mutex::new(Phase::Idle),
            changed: Condvar::new(),
            interrupted,
        });
        let thread = std::thread::Builder::new()
            .name("desktop waker".into())
            .spawn({
                let shared = shared.clone();
                move || run(&shared)
            })?;
        Ok(Waker {
            shared,
            interrupt,
            thread: Some(thread),
        })
    }

    /// Wake SDL's wait once any of `fds` is ready.
    pub(super) fn watch(&self, fds: Vec<libc::pollfd>) {
        *self.shared.lock() = Phase::Asked(fds);
        self.shared.changed.notify_all();
    }

    /// Stop watching, and return once the descriptors are no longer polled,
    /// so that they can be closed.
    pub(super) fn settle(&self) {
        let mut phase = self.shared.lock();
        match *phase {
            Phase::Asked(_) => *phase = Phase::Idle,
            Phase::Polling => {
                // Written under the lock, which the thread takes before it
                // empties the socket: nothing stays to cut the next poll short.
                let _ = (&self.interrupt).write(&[0]);
                while matches!(*phase, Phase::Polling) {
                    phase = self
                        .shared
                        .changed
                        .wait(phase)
                        .unwrap_or_else(|e| e.into_inner());
                }
            }
            Phase::Idle | Phase::Quit => {}
        }
    }
}

impl Drop for Waker {
    fn drop(&mut self) {
        self.settle();
        *self.shared.lock() = Phase::Quit;
        self.shared.changed.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(shared: &Shared) {
    let mut phase = shared.lock();
    loop {
        let mut fds = match std::mem::replace(&mut *phase, Phase::Polling) {
            Phase::Asked(fds) => fds,
            Phase::Quit => return,
            Phase::Idle | Phase::Polling => {
                *phase = Phase::Idle;
                phase = shared
                    .changed
                    .wait(phase)
                    .unwrap_or_else(|e| e.into_inner());
                continue;
            }
        };
        drop(phase);
        let watched = fds.len();
        fds.push(libc::pollfd {
            fd: shared.interrupted.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        let polled = loop {
            // SAFETY: `fds` holds that many descriptors, which the host keeps
            // open until `settle` has seen this poll end.
            let polled = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
            if polled >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break polled;
            }
        };
        // A poll that failed wakes the host too, which then finds out why
        // rather than sleeping on.
        if polled < 0 || fds[..watched].iter().any(|fd| fd.revents != 0) {
            video::wake();
        }
        phase = shared.lock();
        let mut drained = [0u8; 64];
        while matches!((&shared.interrupted).read(&mut drained), Ok(read) if read > 0) {}
        if matches!(*phase, Phase::Polling) {
            *phase = Phase::Idle;
        }
        shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readable(stream: &UnixStream) -> libc::pollfd {
        libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }
    }

    #[test]
    fn settling_ends_a_poll_that_nothing_answered() {
        let (_quiet, watched) = UnixStream::pair().unwrap();
        let waker = Waker::new().unwrap();
        for _ in 0..100 {
            waker.watch(vec![readable(&watched)]);
            waker.settle();
            assert!(matches!(*waker.shared.lock(), Phase::Idle));
        }
    }

    #[test]
    fn a_ready_descriptor_ends_the_poll_by_itself() {
        let (mut talker, watched) = UnixStream::pair().unwrap();
        talker.write_all(&[1]).unwrap();
        let waker = Waker::new().unwrap();
        waker.watch(vec![readable(&watched)]);
        let mut phase = waker.shared.lock();
        while !matches!(*phase, Phase::Idle) {
            phase = waker.shared.changed.wait(phase).unwrap();
        }
    }
}
