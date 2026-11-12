#[cfg(target_os = "freebsd")]
use self::freebsd as imp;
#[cfg(target_os = "linux")]
use self::linux as imp;
#[cfg(target_os = "macos")]
use self::macos as imp;

pub use imp::*;

#[cfg(target_os = "linux")]
mod linux {
    // Linux numbers these two differently from every BSD.
    pub const IP_OPTIONS: i32 = 4;
    pub const IP_RECVOPTS: i32 = 6;
    pub const IP_PKTINFO: Option<i32> = Some(libc::IP_PKTINFO);
    pub const IP_RECVTOS: i32 = libc::IP_RECVTOS;
    /// A received TOS arrives under the send-side name.
    pub const IP_TOS_MESSAGE: i32 = libc::IP_TOS;
    pub const IPV6_JOIN_GROUP: i32 = libc::IPV6_ADD_MEMBERSHIP;
    pub const IPV6_LEAVE_GROUP: i32 = libc::IPV6_DROP_MEMBERSHIP;
    /// `TCP_CORK` holds a partial segment until cleared, where Darwin's
    /// `TCP_NOPUSH` is advisory. Mapped anyway: the failure modes are not
    /// comparable, as a refused option cost the write itself.
    pub const TCP_NOPUSH: i32 = libc::TCP_CORK;
    pub const TCP_KEEPIDLE: i32 = libc::TCP_KEEPIDLE;
    pub const TCP_NOTSENT_LOWAT: Option<i32> = Some(libc::TCP_NOTSENT_LOWAT);
    /// No `getsockopt` for bytes waiting to be sent; an ioctl answers it.
    pub const QUEUED_WRITE_IOCTL: Option<libc::c_ulong> = Some(libc::TIOCOUTQ as libc::c_ulong);
    pub const EAI_NODATA: Option<i32> = Some(libc::EAI_NODATA);
    /// `send` takes the flag; there is no per-socket option for it.
    pub const MSG_NOSIGNAL: i32 = libc::MSG_NOSIGNAL;
    pub const NOSIGPIPE_SOCKOPT: Option<i32> = None;
    pub const SOCK_CLOEXEC: i32 = libc::SOCK_CLOEXEC;
    pub const SOCK_NONBLOCK: i32 = libc::SOCK_NONBLOCK;
}

#[cfg(target_os = "freebsd")]
mod freebsd {
    // Declared by FreeBSD's headers, and not by the `libc` crate.
    pub const IP_OPTIONS: i32 = 1;
    pub const IP_RECVOPTS: i32 = 5;
    /// FreeBSD splits the answer into `IP_RECVDSTADDR` and `IP_RECVIF`, neither
    /// of which fills Darwin's `struct in_pktinfo`, so the option is refused
    /// rather than aimed at something that answers in a different shape.
    pub const IP_PKTINFO: Option<i32> = None;
    pub const IP_RECVTOS: i32 = libc::IP_RECVTOS;
    pub const IP_TOS_MESSAGE: i32 = libc::IP_RECVTOS;
    pub const IPV6_JOIN_GROUP: i32 = libc::IPV6_JOIN_GROUP;
    pub const IPV6_LEAVE_GROUP: i32 = libc::IPV6_LEAVE_GROUP;
    /// Darwin's own `TCP_NOPUSH`, with Darwin's own advisory meaning.
    pub const TCP_NOPUSH: i32 = libc::TCP_NOPUSH;
    pub const TCP_KEEPIDLE: i32 = libc::TCP_KEEPIDLE;
    /// No FreeBSD counterpart; refused rather than mapped to a nearby option.
    pub const TCP_NOTSENT_LOWAT: Option<i32> = None;
    /// `FIONWRITE`, `_IOR('f', 119, int)`.
    pub const QUEUED_WRITE_IOCTL: Option<libc::c_ulong> = Some(0x4004_6677);
    pub const EAI_NODATA: Option<i32> = None;
    pub const MSG_NOSIGNAL: i32 = libc::MSG_NOSIGNAL;
    pub const NOSIGPIPE_SOCKOPT: Option<i32> = None;
    pub const SOCK_CLOEXEC: i32 = libc::SOCK_CLOEXEC;
    pub const SOCK_NONBLOCK: i32 = libc::SOCK_NONBLOCK;
}

#[cfg(target_os = "macos")]
mod macos {
    // Darwin's own values, spelled out: the host and the guest agree by
    // definition, and the translation runs through them regardless.
    pub const IP_OPTIONS: i32 = 1;
    pub const IP_RECVOPTS: i32 = 5;
    pub const IP_PKTINFO: Option<i32> = Some(26);
    pub const IP_RECVTOS: i32 = 27;
    pub const IP_TOS_MESSAGE: i32 = IP_RECVTOS;
    pub const IPV6_JOIN_GROUP: i32 = 12;
    pub const IPV6_LEAVE_GROUP: i32 = 13;
    pub const TCP_NOPUSH: i32 = 0x04;
    /// Darwin's `TCP_KEEPALIVE` is the idle time, which others call `KEEPIDLE`.
    pub const TCP_KEEPIDLE: i32 = 0x10;
    pub const TCP_NOTSENT_LOWAT: Option<i32> = Some(0x201);
    /// `SO_NWRITE` is a socket option here, not an ioctl.
    pub const QUEUED_WRITE_IOCTL: Option<libc::c_ulong> = None;
    pub const EAI_NODATA: Option<i32> = Some(7);
    /// No `MSG_NOSIGNAL`: the option is set on the socket instead.
    pub const MSG_NOSIGNAL: i32 = 0;
    pub const NOSIGPIPE_SOCKOPT: Option<i32> = Some(0x1022);
    // Darwin's own, which the `libc` crate does not declare for it.
    pub const SOCK_CLOEXEC: i32 = 0x1000_0000;
    pub const SOCK_NONBLOCK: i32 = 0x2000_0000;
}

#[cfg(target_os = "linux")]
mod link {
    use super::super::ifaddrs::{Link, ift};

    /// The family of a host's link record.
    pub const LINK_FAMILY: i32 = libc::AF_PACKET;

    /// The host's peer or broadcast address, which Linux keeps in a union named
    /// `ifa_ifu`.
    pub fn peer(host: &libc::ifaddrs) -> *mut libc::sockaddr {
        host.ifa_ifu.cast()
    }

    /// Set [`peer`], for tests.
    #[cfg(test)]
    pub fn set_peer(host: &mut libc::ifaddrs, to: *mut libc::sockaddr) {
        host.ifa_ifu = to.cast();
    }

    /// A host link record, read out of a `sockaddr_ll`.
    ///
    /// # Safety
    ///
    /// `addr` is a live link `struct sockaddr`, as [`is_link`] reports.
    pub unsafe fn link_from_host(addr: *const libc::sockaddr) -> Link {
        // SAFETY: AF_PACKET addresses have `sockaddr_ll` layout.
        let packet = unsafe { &*addr.cast::<libc::sockaddr_ll>() };
        let len = usize::from(packet.sll_halen).min(packet.sll_addr.len());
        let mut link = Link {
            index: packet.sll_ifindex as u32,
            // ARPHRD values are not IFT values, so only the three with a Darwin
            // counterpart are named and anything else stays unknown.
            kind: match packet.sll_hatype {
                libc::ARPHRD_ETHER => Some(ift::ETHER),
                libc::ARPHRD_PPP => Some(ift::PPP),
                libc::ARPHRD_SLIP => Some(ift::SLIP),
                _ => None,
            },
            hardware_len: len.min(8),
            ..Link::default()
        };
        link.hardware[..link.hardware_len].copy_from_slice(&packet.sll_addr[..link.hardware_len]);
        link
    }
}

#[cfg(not(target_os = "linux"))]
mod link {
    use super::super::ifaddrs::Link;

    pub const LINK_FAMILY: i32 = libc::AF_LINK;

    pub fn peer(host: &libc::ifaddrs) -> *mut libc::sockaddr {
        host.ifa_dstaddr
    }

    #[cfg(test)]
    pub fn set_peer(host: &mut libc::ifaddrs, to: *mut libc::sockaddr) {
        host.ifa_dstaddr = to;
    }

    /// A host link record: Darwin's own `sockaddr_dl`, down to the `IFT_*`
    /// numbering, so nothing is translated - only unpacked.
    ///
    /// # Safety
    ///
    /// `addr` is a live link `struct sockaddr`, as [`is_link`] reports.
    pub unsafe fn link_from_host(addr: *const libc::sockaddr) -> Link {
        // SAFETY: AF_LINK addresses have `sockaddr_dl` layout.
        let dl = unsafe { &*addr.cast::<libc::sockaddr_dl>() };
        // `sdl_data` holds the interface name first and the hardware address
        // after it - the name is rebuilt from `ifa_name`, so only the second is
        // read. The record runs past the declared twelve bytes of `sdl_data`
        // as far as `sdl_len` says, so that is the bound, not the array.
        let data = core::mem::offset_of!(libc::sockaddr_dl, sdl_data);
        let room = usize::from(dl.sdl_len).saturating_sub(data);
        let at = usize::from(dl.sdl_nlen);
        let len = usize::from(dl.sdl_alen).min(room.saturating_sub(at)).min(8);
        let mut link = Link {
            index: u32::from(dl.sdl_index),
            kind: (dl.sdl_type != 0).then_some(dl.sdl_type),
            hardware_len: len,
            ..Link::default()
        };
        // SAFETY: `len` bytes at `at` are inside the `sdl_len` the host gave.
        let hardware =
            unsafe { core::slice::from_raw_parts(addr.cast::<u8>().add(data + at), len) };
        link.hardware[..len].copy_from_slice(hardware);
        link
    }
}

pub use link::*;

/// Whether `addr` is this host's link record rather than an IP address.
///
/// # Safety
///
/// `addr` is null or a live `struct sockaddr`.
pub unsafe fn is_link(addr: *const libc::sockaddr) -> bool {
    // SAFETY: the caller's live address.
    !addr.is_null() && unsafe { (*addr).sa_family as i32 == LINK_FAMILY }
}

/// `sendfile`, in the one shape every host can be asked: send up to `count`
/// bytes of `file` from `*offset` to `socket`, advance `*offset` by what went,
/// and answer that count, or -1 with `errno` set when nothing did.
///
/// The hosts differ in argument order, in whether the offset is a pointer, and
/// in how the count comes back. A send that stops short still moved the bytes
/// it reports, so those are kept and the failure is left for the next call to
/// run into.
///
/// # Safety
///
/// `file` and `socket` are descriptors; the kernel validates them.
#[cfg(target_os = "linux")]
pub unsafe fn sendfile(file: i32, socket: i32, offset: &mut libc::off_t, count: usize) -> isize {
    // SAFETY: a live offset slot.
    unsafe { libc::sendfile(socket, file, offset, count) }
}

#[cfg(target_os = "freebsd")]
pub unsafe fn sendfile(file: i32, socket: i32, offset: &mut libc::off_t, count: usize) -> isize {
    let mut moved: libc::off_t = 0;
    // SAFETY: a local count slot.
    let rc = unsafe {
        libc::sendfile(
            file,
            socket,
            *offset,
            count,
            core::ptr::null_mut(),
            &raw mut moved,
            0,
        )
    };
    advance(rc < 0, moved, offset)
}

#[cfg(target_os = "macos")]
pub unsafe fn sendfile(file: i32, socket: i32, offset: &mut libc::off_t, count: usize) -> isize {
    // In: the most to send. Out: what was sent.
    let mut moved = count as libc::off_t;
    // SAFETY: a local count slot.
    let rc = unsafe {
        libc::sendfile(
            file,
            socket,
            *offset,
            &raw mut moved,
            core::ptr::null_mut(),
            0,
        )
    };
    advance(rc < 0, moved, offset)
}

/// The count a BSD `sendfile` reports through its out-parameter, as the
/// return value Linux's gives, with `offset` moved past it.
#[cfg(not(target_os = "linux"))]
fn advance(failed: bool, moved: libc::off_t, offset: &mut libc::off_t) -> isize {
    if failed && moved == 0 {
        return -1;
    }
    *offset += moved;
    moved as isize
}
