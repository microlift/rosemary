//! Network interfaces, what kind of link each one is, and the route out.

use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use crate::sys;

/// What carries an interface's traffic, as far as the host can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Link {
    Wifi,
    Wired,
    Cellular,
    Loopback,
    /// A tunnel, a point-to-point link, or anything the host does not name.
    Other,
}

/// One host interface and the addresses it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    /// Zero where the host has no index for it.
    pub index: u32,
    pub link: Link,
    /// Up and running, not just configured up.
    pub up: bool,
    pub v4: Vec<Ipv4Addr>,
    pub v6: Vec<Ipv6Addr>,
}

impl Interface {
    /// Whether traffic to elsewhere could leave by this interface: it is up,
    /// is not loopback, and has an address beyond its own link.
    pub fn routable(&self) -> bool {
        self.up
            && self.link != Link::Loopback
            && (self.v4.iter().any(|a| routable_v4(*a)) || self.v6.iter().any(|a| routable_v6(*a)))
    }
}

/// Link-local addresses are self-assigned when nothing better is on offer,
/// so they say nothing about reaching anywhere else.
fn routable_v4(address: Ipv4Addr) -> bool {
    !(address.is_link_local() || address.is_loopback() || address.is_unspecified())
}

fn routable_v6(address: Ipv6Addr) -> bool {
    !(address.is_unicast_link_local() || address.is_loopback() || address.is_unspecified())
}

/// Every interface the host lists, each once, in the host's order.
pub fn interfaces() -> Vec<Interface> {
    let mut out: Vec<Interface> = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: a list pointer for `getifaddrs` to fill in, freed below.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return out;
    }
    let mut at = list;
    while !at.is_null() {
        // SAFETY: a node of the list `getifaddrs` returned.
        let entry = unsafe { &*at };
        at = entry.ifa_next;
        if entry.ifa_name.is_null() {
            continue;
        }
        // SAFETY: a NUL-terminated name, live until the list is freed.
        let name = unsafe { CStr::from_ptr(entry.ifa_name) };
        let flags = entry.ifa_flags as libc::c_int;
        let position = match out
            .iter()
            .position(|i| i.name.as_bytes() == name.to_bytes())
        {
            Some(position) => position,
            None => {
                out.push(Interface {
                    name: name.to_string_lossy().into_owned(),
                    // SAFETY: a NUL-terminated name.
                    index: unsafe { libc::if_nametoindex(name.as_ptr()) },
                    // Classified once per interface, not once per address.
                    link: link(name, flags),
                    up: flags & libc::IFF_UP != 0 && flags & libc::IFF_RUNNING != 0,
                    v4: Vec::new(),
                    v6: Vec::new(),
                });
                out.len() - 1
            }
        };
        // SAFETY: null or a socket address of the family it states.
        match unsafe { address(entry.ifa_addr) } {
            Some(IpAddr::V4(a)) => out[position].v4.push(a),
            Some(IpAddr::V6(a)) => out[position].v6.push(a),
            None => {}
        }
    }
    // SAFETY: the list `getifaddrs` returned, freed once.
    unsafe { libc::freeifaddrs(list) };
    out
}

fn link(name: &CStr, flags: libc::c_int) -> Link {
    if flags & libc::IFF_LOOPBACK != 0 {
        return Link::Loopback;
    }
    if let Some(link) = sys::link_medium(name) {
        return link;
    }
    Link::Other
}

/// # Safety
///
/// `addr` is null or a live `struct sockaddr` of the family it states.
unsafe fn address(addr: *const libc::sockaddr) -> Option<IpAddr> {
    if addr.is_null() {
        return None;
    }
    // SAFETY: the caller's obligation, for each family's layout.
    unsafe {
        match (*addr).sa_family as libc::c_int {
            libc::AF_INET => {
                let v4 = &*addr.cast::<libc::sockaddr_in>();
                Some(IpAddr::V4(Ipv4Addr::from(v4.sin_addr.s_addr.to_ne_bytes())))
            }
            libc::AF_INET6 => {
                let v6 = &*addr.cast::<libc::sockaddr_in6>();
                Some(IpAddr::V6(Ipv6Addr::from(v6.sin6_addr.s6_addr)))
            }
            _ => None,
        }
    }
}

/// The local address a packet to the public internet would leave from, for
/// IPv4. Asked of the routing table without sending anything: `connect` on a
/// datagram socket only picks the route. `None` when there is no route.
pub fn route_v4() -> Option<Ipv4Addr> {
    match route_from(
        (Ipv4Addr::UNSPECIFIED, 0).into(),
        (Ipv4Addr::new(1, 1, 1, 1), 53).into(),
    )? {
        IpAddr::V4(a) => Some(a),
        IpAddr::V6(_) => None,
    }
}

/// As [`route_v4`], for IPv6.
pub fn route_v6() -> Option<Ipv6Addr> {
    let to = Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111);
    match route_from((Ipv6Addr::UNSPECIFIED, 0).into(), (to, 53).into())? {
        IpAddr::V6(a) => Some(a),
        IpAddr::V4(_) => None,
    }
}

fn route_from(bind: SocketAddr, to: SocketAddr) -> Option<IpAddr> {
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(to).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_is_listed_and_never_routable() {
        let all = interfaces();
        let names: Vec<_> = all.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names.len(), {
            let mut unique = names.clone();
            unique.sort();
            unique.dedup();
            unique.len()
        });
        let loopback = all
            .iter()
            .find(|i| i.v4.contains(&Ipv4Addr::LOCALHOST))
            .expect("the host has a loopback interface");
        assert_eq!(loopback.link, Link::Loopback);
        assert!(!loopback.routable());
    }

    #[test]
    fn a_link_local_address_alone_is_not_routable() {
        let mut interface = Interface {
            name: "en9".into(),
            index: 9,
            link: Link::Wired,
            up: true,
            v4: vec![Ipv4Addr::new(169, 254, 3, 4)],
            v6: vec!["fe80::1".parse().unwrap()],
        };
        assert!(!interface.routable());
        interface.v4.push(Ipv4Addr::new(192, 168, 1, 2));
        assert!(interface.routable());
        interface.up = false;
        assert!(!interface.routable());
    }

    #[test]
    fn the_route_out_leaves_from_a_listed_address() {
        let all = interfaces();
        if let Some(local) = route_v4() {
            assert!(all.iter().any(|i| i.v4.contains(&local)));
        }
        if let Some(local) = route_v6() {
            assert!(all.iter().any(|i| i.v6.contains(&local)));
        }
    }
}
