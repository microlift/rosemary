//! Bound Linux SCM_RIGHTS queues across cooperating Rosemary processes.
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

// Leave headroom below the common 1024-descriptor limit of session services.
const BUDGET: usize = 256;
pub(super) const HEADER: &[u8; 8] = b"RSCM\0\0\0\x01";

pub(super) fn reserve(count: usize) -> io::Result<OwnedFd> {
    let uid = unsafe { libc::getuid() };
    // Do not key this by prefix: Linux accounts in-flight descriptors by UID.
    let path = format!("/tmp/rosemary-scm-credits-{uid}");
    reserve_at(Path::new(&path), count, BUDGET)
}

fn reserve_at(path: &Path, count: usize, budget: usize) -> io::Result<OwnedFd> {
    if count == 0 || count > budget {
        return Err(io::Error::from_raw_os_error(libc::EMSGSIZE));
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::getuid() } || metadata.nlink() != 1 {
        return Err(io::Error::from_raw_os_error(libc::EPERM));
    }
    for start in 0..=budget - count {
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = start as _;
        lock.l_len = count as _;
        // OFD locks follow the descriptor through SCM_RIGHTS, including while
        // queued. Receiving or discarding the token releases its reservation.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_SETLK, &lock) } == 0 {
            return Ok(file.into());
        }
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EACCES)) {
            return Err(error);
        }
    }
    Err(io::Error::from_raw_os_error(libc::EAGAIN))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    #[test]
    fn queued_token_holds_credit_until_received_or_discarded() {
        let path = std::env::temp_dir().join(format!(
            "rosemary-credit-test-{}-{}",
            std::process::id(),
            super::super::wire::fresh_address()
        ));
        for receive in [true, false] {
            let token = reserve_at(&path, 2, 2).unwrap();
            let mut pair = [-1; 2];
            assert_eq!(
                unsafe {
                    libc::socketpair(
                        libc::AF_UNIX,
                        libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                        0,
                        pair.as_mut_ptr(),
                    )
                },
                0
            );
            let sender = unsafe { OwnedFd::from_raw_fd(pair[0]) };
            let receiver = unsafe { OwnedFd::from_raw_fd(pair[1]) };
            let mut byte = 0u8;
            let mut iov = libc::iovec {
                iov_base: (&raw mut byte).cast(),
                iov_len: 1,
            };
            let mut control = [0usize; 4];
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &raw mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = unsafe { libc::CMSG_SPACE(4) } as _;
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&msg);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(4) as _;
                libc::CMSG_DATA(cmsg).cast::<i32>().write(token.as_raw_fd());
                assert_eq!(libc::sendmsg(sender.as_raw_fd(), &msg, 0), 1);
            }
            drop(token);
            assert_eq!(
                reserve_at(&path, 1, 2).unwrap_err().raw_os_error(),
                Some(libc::EAGAIN)
            );
            if receive {
                unsafe {
                    assert_eq!(
                        libc::recvmsg(receiver.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC),
                        1
                    );
                    let fd = libc::CMSG_DATA(libc::CMSG_FIRSTHDR(&msg))
                        .cast::<i32>()
                        .read();
                    libc::close(fd);
                }
            }
            drop(receiver);
            assert!(reserve_at(&path, 2, 2).is_ok());
        }
        std::fs::remove_file(path).unwrap();
    }
}
