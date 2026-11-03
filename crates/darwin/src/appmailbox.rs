//! The transport under LaunchServices' handoff of a launch to an application
//! that already runs: on a Mac the Apple Events of an `open` go to the running
//! instance's port; here they go to a datagram socket named for its pid in the
//! prefix's `applications` directory, next to the process server's socket.
//!
//! The socket is bound before the process claims its application, so anybody
//! the server names as the running instance can already be written to, and
//! what arrives before the program is ready to hear it waits here: a Mac
//! queues an application's events until it has finished launching, too.
//!
//! What a message *is* belongs to AppKit; this moves bytes.

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// The largest message carried.
pub const MAX_MESSAGE: usize = 64 * 1024;

fn directory() -> Option<PathBuf> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    Some(server::client::directory()?.join("applications"))
}

fn path_for(pid: i32) -> Option<PathBuf> {
    Some(directory()?.join(pid.to_string()))
}

struct Mailbox {
    /// The pid the socket was bound for; a fork child must not read its
    /// parent's mail.
    pid: i32,
    deliver: Option<fn(Vec<u8>)>,
    pending: Vec<Vec<u8>>,
}

static MAILBOX: Mutex<Option<Mailbox>> = Mutex::new(None);

fn mailbox() -> std::sync::MutexGuard<'static, Option<Mailbox>> {
    MAILBOX.lock().unwrap_or_else(|e| e.into_inner())
}

/// Start receiving this process's mail. Idempotent per process; false when
/// there is nowhere to listen.
pub fn open() -> bool {
    let pid = std::process::id() as i32;
    let mut mailbox = mailbox();
    if mailbox.as_ref().is_some_and(|m| m.pid == pid) {
        return true;
    }
    let (Some(directory), Some(path)) = (directory(), path_for(pid)) else {
        return false;
    };
    if server::location::prepare(&directory).is_err() {
        return false;
    }
    sweep(&directory);
    // A stale socket of a process that had this pid before.
    let _ = std::fs::remove_file(&path);
    let Ok(socket) = UnixDatagram::bind(&path) else {
        return false;
    };
    let spawned = std::thread::Builder::new()
        .name("appmailbox".into())
        .spawn(move || {
            let mut buffer = vec![0u8; MAX_MESSAGE];
            loop {
                match socket.recv(&mut buffer) {
                    Ok(n) => arrived(buffer[..n].to_vec()),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return,
                }
            }
        });
    if spawned.is_err() {
        let _ = std::fs::remove_file(&path);
        return false;
    }
    *mailbox = Some(Mailbox {
        pid,
        deliver: None,
        pending: Vec::new(),
    });
    true
}

/// Remove the sockets of applications that have exited, which nothing else
/// does: a process does not get to tidy up after a `SIGKILL`.
fn sweep(directory: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        // SAFETY: a signal-0 probe of a pid.
        let gone = unsafe { libc::kill(pid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if gone {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Stop being reachable, for a process that turned out not to be the
/// application after all. Its reader is left blocked on a socket nobody can
/// name, which costs a thread in a process that is about to exit.
pub fn close() {
    let pid = std::process::id() as i32;
    if mailbox().take().is_some_and(|m| m.pid == pid)
        && let Some(path) = path_for(pid)
    {
        let _ = std::fs::remove_file(path);
    }
}

fn arrived(message: Vec<u8>) {
    let deliver = {
        let mut mailbox = mailbox();
        let Some(mailbox) = mailbox.as_mut() else {
            return;
        };
        match mailbox.deliver {
            Some(deliver) => deliver,
            None => {
                mailbox.pending.push(message);
                return;
            }
        }
    };
    deliver(message);
}

/// Hand every message, those already waiting first, to `deliver`, on the
/// mailbox's own thread for those still to come.
pub fn consume(deliver: fn(Vec<u8>)) {
    let pending = {
        let mut mailbox = mailbox();
        let Some(mailbox) = mailbox.as_mut() else {
            return;
        };
        mailbox.deliver = Some(deliver);
        std::mem::take(&mut mailbox.pending)
    };
    for message in pending {
        deliver(message);
    }
}

/// Send `message` to the application running as `pid`.
pub fn send(pid: i32, message: &[u8]) -> std::io::Result<()> {
    if message.len() > MAX_MESSAGE {
        return Err(std::io::Error::from_raw_os_error(libc::EMSGSIZE));
    }
    let path = path_for(pid).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    let socket = UnixDatagram::unbound()?;
    // A receiver that has stopped reading is not worth hanging a launch over.
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    socket.send_to(message, path).map(|_| ())
}
