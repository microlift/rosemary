#![cfg(not(target_os = "macos"))]

use std::ffi::c_void;

/// Darwin's `POLL*`, from `<sys/poll.h>`.
mod bit {
    pub const IN: i16 = 0x0001;
    pub const PRI: i16 = 0x0002;
    pub const OUT: i16 = 0x0004;
    pub const ERR: i16 = 0x0008;
    pub const HUP: i16 = 0x0010;
    pub const NVAL: i16 = 0x0020;
    pub const RDNORM: i16 = 0x0040;
    pub const RDBAND: i16 = 0x0080;
    pub const WRBAND: i16 = 0x0100;
}

/// Each bit that means the same thing on both, as `(darwin, host)`.
///
/// `POLLWRNORM` appears only in the second direction: Darwin spells it
/// `POLLOUT`, so an `events` word never carries Linux's `0x100`, but a `revents`
/// the kernel filled in can.
const SHARED: &[(i16, i16)] = &[
    (bit::IN, libc::POLLIN),
    (bit::PRI, libc::POLLPRI),
    (bit::OUT, libc::POLLOUT),
    (bit::ERR, libc::POLLERR),
    (bit::HUP, libc::POLLHUP),
    (bit::NVAL, libc::POLLNVAL),
    (bit::RDNORM, libc::POLLRDNORM),
    (bit::RDBAND, libc::POLLRDBAND),
    (bit::WRBAND, libc::POLLWRBAND),
];

/// The four Darwin-only vnode bits, as `(bit, name)`.
///
/// `POLLEXTEND` (a file grew), `POLLATTRIB` (its attributes changed),
/// `POLLNLINK` (it was linked or unlinked) and `POLLWRITE` (it was written).
/// This host has no way to ask any of them of a descriptor - the nearest thing
/// is `inotify`, which watches a *path* and is a different interface with a
/// different lifetime - so each is dropped rather than aimed at the real Linux
/// bit that shares its number.
const VNODE: &[(i16, &str)] = &[
    (0x0200, "POLLEXTEND"),
    (0x0400, "POLLATTRIB"),
    (0x0800, "POLLNLINK"),
    (0x1000, "POLLWRITE"),
];

/// Name a dropped vnode bit, once for each.
///
/// Silence here is what the module note warns about one paragraph up: the guest
/// is told nothing happened, which is honest and indistinguishable from a file
/// that nothing happened to. A guest waiting on a file it will never be told
/// about should say so once rather than only look slow.
fn say_vnode_dropped(darwin: i16) {
    use std::sync::Mutex;
    static SAID: Mutex<i16> = Mutex::new(0);
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    for (bit, name) in VNODE {
        if darwin & bit == 0 || *said & bit != 0 {
            continue;
        }
        *said |= bit;
        eprintln!(
            "rosemary: poll for Darwin's {name} ({bit:#06x}): dropped - this host has no vnode \
             event a descriptor can be polled for, and {bit:#06x} here is a real bit meaning \
             something else. The poll will report that nothing happened."
        );
    }
}

/// A guest's `events` as this host's.
fn events_to_host(darwin: i16) -> i16 {
    if darwin & VNODE.iter().fold(0, |all, (bit, _)| all | bit) != 0 {
        say_vnode_dropped(darwin);
    }
    let mut out = 0;
    for (theirs, ours) in SHARED {
        if darwin & theirs != 0 {
            out |= ours;
        }
    }
    out
}

/// This host's `revents` as a guest's.
fn revents_from_host(host: i16) -> i16 {
    let mut out = 0;
    for (theirs, ours) in SHARED {
        if host & ours != 0 {
            out |= theirs;
        }
    }
    // Linux reports writability on a socket as `POLLWRNORM` beside `POLLOUT`;
    // Darwin has no separate bit for it.
    if host & libc::POLLWRNORM != 0 {
        out |= bit::OUT;
    }
    out
}

/// `int poll(struct pollfd *fds, nfds_t nfds, int timeout)`
///
/// The array is copied rather than rewritten in place: `events` belongs to the
/// caller, and a guest that reuses its array across calls would find Linux's
/// bits in it.
///
/// # Safety
///
/// `fds` is `nfds` writable `struct pollfd`, or `nfds` is zero.
unsafe extern "C" fn poll(fds: *mut c_void, nfds: u32, timeout: i32) -> i32 {
    let count = nfds as usize;
    if fds.is_null() && count != 0 {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    if count == 0 {
        // A poll of nothing is a sleep, which the host does as well as this can.
        // SAFETY: null with a zero count is what the caller passed.
        return unsafe { libc::poll(core::ptr::null_mut(), 0, timeout) };
    }
    let guest = fds.cast::<libc::pollfd>();
    // SAFETY: the caller's own array, whose element layout is the host's.
    let mut host: Vec<libc::pollfd> = (0..count)
        .map(|i| unsafe {
            let entry = guest.add(i).read();
            libc::pollfd {
                fd: entry.fd,
                events: events_to_host(entry.events),
                revents: 0,
            }
        })
        .collect();

    // SAFETY: `host` is `count` live `struct pollfd`, which is what `nfds` says.
    let rc = unsafe { libc::poll(host.as_mut_ptr(), nfds as libc::nfds_t, timeout) };

    for (i, entry) in host.iter().enumerate() {
        // SAFETY: as above, writing back only the field the kernel filled in.
        unsafe { (*guest.add(i)).revents = revents_from_host(entry.revents) };
    }
    rc
}

exports::exports! {
    functions {
        "poll" => poll,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The collision this module exists for: Darwin's `POLLWRBAND` is Linux's
    /// `POLLWRNORM`, so the mapping must not be the identity.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_write_bits_are_translated_rather_than_passed_through() {
        assert_eq!(bit::WRBAND, libc::POLLWRNORM);
        assert_eq!(events_to_host(bit::WRBAND), libc::POLLWRBAND);
    }

    /// The Darwin-only vnode bits collide with real Linux ones and are dropped.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_bit_this_host_has_not_got_is_dropped_rather_than_aimed_at_a_neighbour() {
        const EXTEND: i16 = 0x0200;
        assert_eq!(EXTEND, libc::POLLWRBAND, "the collision being avoided");
        assert_eq!(events_to_host(EXTEND), 0);
    }

    #[test]
    fn every_shared_bit_round_trips() {
        for (darwin, _) in SHARED {
            assert_eq!(revents_from_host(events_to_host(*darwin)), *darwin);
        }
    }

    /// A pipe with a byte in it is readable, through the entry point a guest is
    /// handed and in the bits a guest reads.
    #[test]
    fn a_readable_pipe_is_reported_in_darwins_bits() {
        let mut ends = [0; 2];
        // SAFETY: a two-element array, which is what `pipe` fills in.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        // SAFETY: the write end of a pipe this test owns.
        assert_eq!(unsafe { libc::write(ends[1], c"x".as_ptr().cast(), 1) }, 1);

        let mut fds = [libc::pollfd {
            fd: ends[0],
            events: bit::IN,
            revents: 0,
        }];
        // SAFETY: one live `struct pollfd`, and the count matches.
        let rc = unsafe { poll(fds.as_mut_ptr().cast(), 1, 0) };
        assert_eq!(rc, 1);
        assert_eq!(fds[0].revents & bit::IN, bit::IN);

        // SAFETY: two descriptors this test opened and no longer uses.
        unsafe {
            libc::close(ends[0]);
            libc::close(ends[1]);
        }
    }
}
