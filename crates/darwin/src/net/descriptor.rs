/// `int close(int fd)`
pub unsafe extern "C" fn close(fd: i32) -> i32 {
    // Before the host close frees the number: another thread's `kqueue` can be
    // handed it at once, and must not lose its new queue here.
    crate::kqueue::forget(fd);
    // SAFETY: an integer descriptor; the call touches no caller memory.
    let rc = unsafe { libc::close(fd) };
    if rc == 0 || crate::errno::get() == libc::EBADF {
        super::sockopt::forget(fd);
        crate::attrlist::forget(fd);
    }
    rc
}

/// `int dup(int fd)`
pub(crate) unsafe extern "C" fn dup(fd: i32) -> i32 {
    // SAFETY: an integer descriptor; the call touches no caller memory.
    let copied = unsafe { libc::dup(fd) };
    if copied >= 0 {
        super::sockopt::inherit_sigpipe_suppression(fd, copied);
        crate::kqueue::duplicate(fd, copied);
    }
    copied
}

/// `int dup2(int from, int to)`
pub(crate) unsafe extern "C" fn dup2(from: i32, to: i32) -> i32 {
    // SAFETY: two integer descriptors; the call touches no caller memory.
    let copied = unsafe { libc::dup2(from, to) };
    if copied >= 0 && from != to {
        // `to` now names another file, so a listing of the old one is stale.
        crate::attrlist::forget(to);
    }
    if copied >= 0 {
        super::sockopt::inherit_sigpipe_suppression(from, copied);
        crate::kqueue::duplicate(from, copied);
    }
    copied
}

exports::exports! {
    functions {
        "close" => close,
        "dup" => dup,
        "dup2" => dup2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (i32, i32) {
        let mut ends = [0i32; 2];
        // SAFETY: a writable two-element descriptor array.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, ends.as_mut_ptr()) },
            0
        );
        (ends[0], ends[1])
    }

    fn suppress(fd: i32) {
        let on = 1i32;
        // SAFETY: one readable `int`, and the length says four.
        assert_eq!(
            unsafe {
                super::super::sockopt::setsockopt(fd, 0xffff, 0x1022, (&raw const on).cast(), 4)
            },
            0
        );
        assert!(super::super::sockopt::suppresses_sigpipe(fd));
    }

    #[test]
    fn close_discards_suppression_state() {
        let (fd, peer) = pair();
        suppress(fd);

        // SAFETY: descriptors owned by this test.
        assert_eq!(unsafe { close(fd) }, 0);
        assert!(!super::super::sockopt::suppresses_sigpipe(fd));
        unsafe { libc::close(peer) };
    }

    #[test]
    fn dup_inherits_suppression_state() {
        let (fd, peer) = pair();
        suppress(fd);

        // SAFETY: a live descriptor owned by this test.
        let copied = unsafe { dup(fd) };
        assert!(copied >= 0);
        assert!(super::super::sockopt::suppresses_sigpipe(copied));

        // SAFETY: descriptors owned by this test.
        unsafe {
            close(fd);
            close(copied);
            libc::close(peer);
        }
    }

    #[test]
    fn dup2_replaces_the_targets_suppression_state() {
        let (source, source_peer) = pair();
        let (target, target_peer) = pair();
        suppress(target);

        // SAFETY: live descriptors owned by this test.
        assert_eq!(unsafe { dup2(source, target) }, target);
        assert!(!super::super::sockopt::suppresses_sigpipe(target));

        // SAFETY: descriptors owned by this test.
        unsafe {
            close(source);
            close(target);
            libc::close(source_peer);
            libc::close(target_peer);
        }
    }

    #[test]
    fn dup2_inherits_the_sources_suppression_state() {
        let (source, source_peer) = pair();
        let (target, target_peer) = pair();
        suppress(source);

        // SAFETY: live descriptors owned by this test.
        assert_eq!(unsafe { dup2(source, target) }, target);
        assert!(super::super::sockopt::suppresses_sigpipe(target));

        // SAFETY: descriptors owned by this test.
        unsafe {
            close(source);
            close(target);
            libc::close(source_peer);
            libc::close(target_peer);
        }
    }
}
