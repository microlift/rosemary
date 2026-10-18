//! `SIOCGIFMEDIA`, which both BSDs answer the same way: Wi-Fi interfaces
//! report Ethernet framing in their link address, but 802.11 media here.

use std::ffi::{CStr, c_char, c_int};

use crate::net::Link;

/// `struct ifmediareq` from `<net/if.h>` (macOS) and `<net/if_media.h>`
/// (FreeBSD), which agree.
#[repr(C)]
struct IfMediaReq {
    name: [c_char; libc::IFNAMSIZ],
    current: c_int,
    mask: c_int,
    status: c_int,
    active: c_int,
    count: c_int,
    list: *mut c_int,
}

/// `_IOWR('i', 56, struct ifmediareq)`.
const SIOCGIFMEDIA: libc::c_ulong = 0xc000_0000
    | ((size_of::<IfMediaReq>() as libc::c_ulong & 0x1fff) << 16)
    | (b'i' as libc::c_ulong) << 8
    | 56;

const IFM_NMASK: c_int = 0xe0;
const IFM_ETHER: c_int = 0x20;
const IFM_IEEE80211: c_int = 0x80;

pub fn link_medium(name: &CStr) -> Option<Link> {
    let bytes = name.to_bytes();
    if bytes.starts_with(b"pdp_ip") {
        return Some(Link::Cellular);
    }
    if bytes.len() >= libc::IFNAMSIZ {
        return None;
    }
    // SAFETY: plain data, valid when zeroed.
    let mut request: IfMediaReq = unsafe { std::mem::zeroed() };
    for (to, from) in request.name.iter_mut().zip(bytes) {
        *to = *from as c_char;
    }
    // SAFETY: a socket this function owns and closes.
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    if socket < 0 {
        return None;
    }
    // SAFETY: an `ifmediareq` with a zero-length list, which asks for the
    // current media only.
    let rc = unsafe { libc::ioctl(socket, SIOCGIFMEDIA, &raw mut request) };
    // SAFETY: closed once.
    unsafe { libc::close(socket) };
    if rc != 0 {
        return None;
    }
    let media = if request.active != 0 {
        request.active
    } else {
        request.current
    };
    match media & IFM_NMASK {
        IFM_IEEE80211 => Some(Link::Wifi),
        IFM_ETHER => Some(Link::Wired),
        _ => None,
    }
}
