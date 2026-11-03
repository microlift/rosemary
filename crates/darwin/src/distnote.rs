//! The transport under distributed notifications: what `distnoted` carries
//! between the processes of one login session on a Mac, carried here between
//! the guests of one prefix.
//!
//! Every process that observes binds a datagram socket named for its pid in
//! the prefix's `distnote` directory, next to the process server's socket; a
//! post is one datagram to every socket in there, the poster's own included,
//! because a Mac delivers a process's own distributed posts back to it too.
//! A socket nobody answers on belongs to a process that is gone, and is
//! removed by the first post that finds it so.
//!
//! What a message *is* belongs to Foundation; this moves bytes.
//!
//! FIXME (ACCURACY): make an actual `distnoted` rather than implement it here weirdly

use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::Mutex;

/// The prefix's rendezvous directory, or `None` outside any server's reach -
/// a test binary, or a host with no server configured - where distributed
/// notifications stay inside the process.
fn directory() -> Option<PathBuf> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    Some(server::client::directory()?.join("distnote"))
}

/// The pid the socket was bound for: a fork child inherits the descriptor but
/// not the name, and must bind its own.
static LISTENING: Mutex<Option<i32>> = Mutex::new(None);

/// Receive every message posted in this prefix from now on, calling `deliver`
/// on a thread of this module's with each. Idempotent per process; false when
/// there is nowhere to listen.
pub fn listen(deliver: fn(Vec<u8>)) -> bool {
    let pid = std::process::id() as i32;
    let mut listening = LISTENING.lock().unwrap_or_else(|e| e.into_inner());
    if *listening == Some(pid) {
        return true;
    }
    let Some(directory) = directory() else {
        return false;
    };
    if server::location::prepare(&directory).is_err() {
        return false;
    }
    let path = directory.join(pid.to_string());
    // A stale socket of a process that had this pid before.
    let _ = std::fs::remove_file(&path);
    let Ok(socket) = UnixDatagram::bind(&path) else {
        return false;
    };
    let spawned = std::thread::Builder::new()
        .name("distnote".into())
        .spawn(move || {
            let mut buffer = vec![0u8; MAX_MESSAGE];
            loop {
                match socket.recv(&mut buffer) {
                    Ok(n) => deliver(buffer[..n].to_vec()),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return,
                }
            }
        });
    if spawned.is_err() {
        let _ = std::fs::remove_file(&path);
        return false;
    }
    *listening = Some(pid);
    true
}

/// The largest message carried; a datagram socket's default send buffer on
/// Linux is about twice this.
pub const MAX_MESSAGE: usize = 64 * 1024;

/// Send `message` to every listener in the prefix. False when there is no
/// prefix to send in, which leaves delivery to the caller.
pub fn post(message: &[u8]) -> bool {
    if message.len() > MAX_MESSAGE {
        return false;
    }
    let Some(directory) = directory() else {
        return false;
    };
    let Ok(socket) = UnixDatagram::unbound() else {
        return false;
    };
    // A listener that has stopped reading loses this message rather than
    // stalling the poster.
    let _ = socket.set_nonblocking(true);
    let Ok(entries) = std::fs::read_dir(&directory) else {
        // Nobody has listened yet, so there is nobody to tell.
        return true;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Err(e) = socket.send_to(message, &path)
            && e.kind() == std::io::ErrorKind::ConnectionRefused
        {
            let _ = std::fs::remove_file(&path);
        }
    }
    true
}
