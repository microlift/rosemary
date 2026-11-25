use std::ffi::{CStr, c_char, c_void};

use super::{addr, host};

/// `struct ifaddrs`, identical in shape on both platforms.
#[repr(C)]
struct IfAddrs {
    next: *mut IfAddrs,
    name: *mut c_char,
    flags: u32,
    addr: *mut c_void,
    netmask: *mut c_void,
    dstaddr: *mut c_void,
    data: *mut c_void,
}

fn aligned(len: usize) -> usize {
    len.next_multiple_of(core::mem::align_of::<libc::sockaddr_storage>())
}

fn flags_from_host(flags: u32) -> u32 {
    // The first ten bits agree; Linux's remaining bits have different meanings.
    (flags & 0x3ff)
        | if flags & libc::IFF_MULTICAST as u32 != 0 {
            0x8000
        } else {
            0
        }
}

/// What a host link record says about an interface, in the terms Darwin's
/// `sockaddr_dl` wants.
///
/// Hosts describe a link in different structures - Linux's `sockaddr_ll`,
/// the BSDs' `sockaddr_dl` - so reading one is the only per-host part, in
/// [`host::link_from_host`]; [`link_address`] below is shared.
#[derive(Default)]
pub struct Link {
    pub index: u32,
    /// An `IFT_*` media type, or `None` where the host names nothing this can
    /// be turned into.
    pub kind: Option<u8>,
    pub hardware: [u8; 8],
    pub hardware_len: usize,
}

/// Darwin's `AF_LINK`.
const AF_LINK: u8 = 18;

/// Darwin's `IFT_*` media types, from `<net/if_types.h>`.
pub mod ift {
    pub const OTHER: u8 = 1;
    pub const ETHER: u8 = 6;
    pub const PPP: u8 = 23;
    pub const LOOP: u8 = 24;
    pub const SLIP: u8 = 28;
}

/// Darwin's `struct sockaddr_dl`: the header, then the interface name and the
/// hardware address end to end in `data`.
///
/// Darwin's own `sdl_data` is twelve bytes and the record is as long as `len`
/// says, so `data` here is the rest of a `sockaddr_storage`.
#[repr(C, align(8))]
struct DarwinSockaddrDl {
    len: u8,
    family: u8,
    index: u16,
    kind: u8,
    name_len: u8,
    hardware_len: u8,
    selector_len: u8,
    data: [u8; addr::STORAGE - 8],
}
const _: () = assert!(core::mem::size_of::<DarwinSockaddrDl>() == addr::STORAGE);
const _: () = assert!(core::mem::offset_of!(DarwinSockaddrDl, data) == 8);

/// Darwin's smallest `sockaddr_dl`, with its twelve-byte `sdl_data`.
const MIN_DL_LEN: usize = 20;

fn link_address(
    name: &[u8],
    index: u32,
    flags: u32,
    link: Option<&Link>,
) -> (addr::DarwinSockaddr, usize) {
    let name = &name[..name.len().saturating_sub(1).min(libc::IF_NAMESIZE - 1)];
    let hardware = link.map_or(&[][..], |l| &l.hardware[..l.hardware_len]);
    let len = (core::mem::offset_of!(DarwinSockaddrDl, data) + name.len() + hardware.len())
        .max(MIN_DL_LEN);
    let mut dl = DarwinSockaddrDl {
        len: len as u8,
        family: AF_LINK,
        index: index as u16,
        kind: if flags & libc::IFF_LOOPBACK as u32 != 0 {
            ift::LOOP
        } else {
            link.and_then(|l| l.kind).unwrap_or(ift::OTHER)
        },
        name_len: name.len() as u8,
        hardware_len: hardware.len() as u8,
        selector_len: 0,
        data: [0; addr::STORAGE - 8],
    };
    dl.data[..name.len()].copy_from_slice(name);
    dl.data[name.len()..name.len() + hardware.len()].copy_from_slice(hardware);
    // SAFETY: both are `repr(C)` of plain integers, `STORAGE` bytes (asserted
    // here and in `addr`) with the same alignment, and begin with `len` and
    // `family`.
    let generic = unsafe { core::mem::transmute::<DarwinSockaddrDl, addr::DarwinSockaddr>(dl) };
    (generic, len)
}

/// One entry, as a single allocation: the node, its name, then up to three
/// addresses, so [`freeifaddrs`] is one `free` per entry.
///
/// # Safety
///
/// `host` is a live host `struct ifaddrs`.
unsafe fn node_from_host(host: &libc::ifaddrs) -> Option<*mut IfAddrs> {
    // SAFETY: the host's own name, which is NUL-terminated.
    let name: &[u8] = match host.ifa_name.is_null() {
        true => b"\0",
        false => unsafe { CStr::from_ptr(host.ifa_name) }.to_bytes_with_nul(),
    };
    let mut converted = [None, None, None];
    // Linux link records can have no sockaddr at all (no IFLA_ADDRESS).
    // SAFETY: the host's own address, which it says is live.
    let is_link = host.ifa_addr.is_null() || unsafe { host::is_link(host.ifa_addr.cast()) };
    if is_link {
        let link = match host.ifa_addr.is_null() {
            true => None,
            // SAFETY: `is_link` just said this is one.
            false => Some(unsafe { host::link_from_host(host.ifa_addr.cast()) }),
        };
        let index = link.as_ref().map_or_else(
            || {
                if host.ifa_name.is_null() {
                    0
                } else {
                    // SAFETY: the host's NUL-terminated name.
                    unsafe { libc::if_nametoindex(host.ifa_name) }
                }
            },
            |l| l.index,
        );
        converted[0] = Some(link_address(name, index, host.ifa_flags, link.as_ref()));
        // Darwin link records have neither an IP mask nor a broadcast address.
    } else {
        for (slot, from) in
            converted
                .iter_mut()
                .zip([host.ifa_addr, host.ifa_netmask, host::peer(host)])
        {
            if from.is_null() {
                continue;
            }
            // SAFETY: the host's live address; only copy known layouts.
            let len = match unsafe { (*from).sa_family } as i32 {
                libc::AF_INET => core::mem::size_of::<libc::sockaddr_in>(),
                libc::AF_INET6 => core::mem::size_of::<libc::sockaddr_in6>(),
                _ => 2,
            } as u32;
            *slot = unsafe { addr::darwin_from_host(from.cast(), len) };
        }
        if host.ifa_flags & (libc::IFF_BROADCAST | libc::IFF_POINTOPOINT) as u32 == 0 {
            converted[2] = None;
        }
    }

    let addr_bytes: usize = converted
        .iter()
        .flatten()
        .map(|(_, len)| aligned(*len))
        .sum();
    let total = core::mem::size_of::<IfAddrs>() + aligned(name.len()) + addr_bytes;
    // SAFETY: a non-zero size; null is checked.
    let block = unsafe { libc::malloc(total) }.cast::<u8>();
    if block.is_null() {
        return None;
    }
    let node = block.cast::<IfAddrs>();
    // SAFETY: `block` is `total` bytes, and the name and the three addresses are
    // written end to end inside it, each within the size computed above.
    unsafe {
        let name_at = block.add(core::mem::size_of::<IfAddrs>());
        core::ptr::copy_nonoverlapping(name.as_ptr(), name_at, name.len());
        let mut at = name_at.add(aligned(name.len()));
        let mut written = [core::ptr::null_mut::<c_void>(); 3];
        for (slot, address) in written.iter_mut().zip(converted.iter()) {
            let Some((bytes, len)) = address else {
                continue;
            };
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), at, *len);
            *slot = at.cast();
            at = at.add(aligned(*len));
        }
        node.write(IfAddrs {
            next: core::ptr::null_mut(),
            name: name_at.cast(),
            flags: flags_from_host(host.ifa_flags),
            addr: written[0],
            netmask: written[1],
            dstaddr: written[2],
            // Darwin's `ifa_data` is a `struct if_data` this host has no
            // counterpart for; null rather than a pointer to the wrong shape.
            data: core::ptr::null_mut(),
        });
    }
    Some(node)
}

/// `int getifaddrs(struct ifaddrs **)`
///
/// The Darwin `sockaddr_dl` of the interface numbered `index`: the link record
/// `getifaddrs` lists for it, which is what Darwin's `IP_RECVIF` hands a
/// receiver. `None` for an index no interface has.
pub fn link_of_index(index: u32) -> Option<Vec<u8>> {
    let mut list: *mut libc::ifaddrs = core::ptr::null_mut();
    // SAFETY: a live slot this function owns.
    if index == 0 || unsafe { libc::getifaddrs(&mut list) } != 0 {
        return None;
    }
    let mut found = None;
    let mut at = list;
    while !at.is_null() && found.is_none() {
        // SAFETY: a node of the list the host just returned.
        let host = unsafe { &*at };
        // SAFETY: the host's own address, which it says is live.
        if !host.ifa_addr.is_null() && unsafe { host::is_link(host.ifa_addr.cast()) } {
            // SAFETY: `is_link` just said this is one.
            let link = unsafe { host::link_from_host(host.ifa_addr.cast()) };
            if link.index == index {
                // SAFETY: the host's NUL-terminated name.
                let name = unsafe { CStr::from_ptr(host.ifa_name) }.to_bytes_with_nul();
                let (address, len) = link_address(name, index, host.ifa_flags, Some(&link));
                let mut bytes = vec![address.len, address.family];
                bytes.extend_from_slice(&address.data);
                bytes.truncate(len);
                found = Some(bytes);
            }
        }
        // SAFETY: as above.
        at = unsafe { (*at).ifa_next };
    }
    // SAFETY: the host's list, read to the end or to the match.
    unsafe { libc::freeifaddrs(list) };
    found
}

/// # Safety
///
/// `out` is a writable pointer slot.
unsafe extern "C" fn getifaddrs(out: *mut *mut IfAddrs) -> i32 {
    if out.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let mut list: *mut libc::ifaddrs = core::ptr::null_mut();
    // SAFETY: a live slot this function owns.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return -1;
    }

    let mut head: *mut IfAddrs = core::ptr::null_mut();
    let mut tail: *mut IfAddrs = core::ptr::null_mut();
    let mut at = list;
    while !at.is_null() {
        // SAFETY: a node of the list the host just returned.
        let Some(node) = (unsafe { node_from_host(&*at) }) else {
            // SAFETY: the host's list, and the partial copy built here.
            unsafe {
                libc::freeifaddrs(list);
                free_list(head);
                *out = core::ptr::null_mut();
            }
            crate::errno::set(libc::ENOMEM);
            return -1;
        };
        match tail.is_null() {
            true => head = node,
            // SAFETY: a node this loop allocated and still owns.
            false => unsafe { (*tail).next = node },
        }
        tail = node;
        at = unsafe { (*at).ifa_next };
    }
    // SAFETY: the host's list, now copied.
    unsafe { libc::freeifaddrs(list) };
    // SAFETY: a writable slot, per the caller's guarantee.
    unsafe { *out = head };
    0
}

/// # Safety
///
/// `head` is null or a list this module allocated.
unsafe fn free_list(head: *mut IfAddrs) {
    let mut at = head;
    while !at.is_null() {
        // SAFETY: a node allocated as one block, whose `next` is read before the
        // block goes away.
        unsafe {
            let next = (*at).next;
            libc::free(at.cast());
            at = next;
        }
    }
}

/// `void freeifaddrs(struct ifaddrs *)`
///
/// # Safety
///
/// `head` is null or a list [`getifaddrs`] returned.
unsafe extern "C" fn freeifaddrs(head: *mut IfAddrs) {
    unsafe { free_list(head) };
}

exports::exports! {
    functions {
        "getifaddrs" => getifaddrs,
        "freeifaddrs" => freeifaddrs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_address_has_darwins_layout() {
        let (dl, len) = link_address(b"en0\0", 3, 0, None);
        assert_eq!(len, MIN_DL_LEN);
        assert_eq!(dl[0] as usize, len);
        assert_eq!(dl[1], AF_LINK);
        assert_eq!(&dl[2..4], &3u16.to_ne_bytes());
        assert_eq!(&dl[4..8], &[ift::OTHER, 3, 0, 0]);
        assert_eq!(&dl[8..11], b"en0");
    }

    #[test]
    fn addressless_interface_has_a_link_address() {
        // SAFETY: null pointers and zero integers are valid for this host struct.
        let mut host: libc::ifaddrs = unsafe { core::mem::zeroed() };
        host.ifa_name = c"lo".as_ptr().cast_mut();
        host.ifa_flags = libc::IFF_LOOPBACK as u32;
        unsafe {
            let node = node_from_host(&host).unwrap();
            let bytes = core::slice::from_raw_parts((*node).addr.cast::<u8>(), 20);
            assert_eq!(bytes[1], AF_LINK);
            assert_eq!(bytes[4], ift::LOOP);
            assert_eq!(bytes[5], 2);
            assert_eq!(bytes[6], 0);
            assert_eq!(&bytes[8..10], b"lo");
            assert_eq!(
                u16::from_ne_bytes([bytes[2], bytes[3]]) as u32,
                libc::if_nametoindex(c"lo".as_ptr())
            );
            assert!((*node).netmask.is_null());
            assert!((*node).dstaddr.is_null());
            free_list(node);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn packet_address_preserves_name_index_and_mac() {
        let mut packet: libc::sockaddr_ll = unsafe { core::mem::zeroed() };
        packet.sll_family = libc::AF_PACKET as _;
        packet.sll_ifindex = 42;
        packet.sll_hatype = libc::ARPHRD_ETHER;
        packet.sll_halen = 6;
        packet.sll_addr[..6].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        let mut host: libc::ifaddrs = unsafe { core::mem::zeroed() };
        host.ifa_name = c"longdevice12345".as_ptr().cast_mut();
        host.ifa_addr = (&mut packet as *mut libc::sockaddr_ll).cast();
        let addr = host.ifa_addr;
        host::set_peer(&mut host, addr);
        unsafe {
            let node = node_from_host(&host).unwrap();
            let bytes = core::slice::from_raw_parts((*node).addr.cast::<u8>(), 29);
            assert_eq!(&bytes[..2], &[29, 18]);
            assert_eq!(u16::from_ne_bytes([bytes[2], bytes[3]]), 42);
            assert_eq!(&bytes[4..8], &[6, 15, 6, 0]);
            assert_eq!(&bytes[8..23], b"longdevice12345");
            assert_eq!(&bytes[23..29], &[2, 3, 4, 5, 6, 7]);
            assert_eq!((*node).addr as usize % 8, 0);
            assert!((*node).dstaddr.is_null());
            free_list(node);
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_private_flags_do_not_become_darwin_link_flags() {
        let flags = libc::IFF_UP
            | libc::IFF_MULTICAST
            | libc::IFF_MASTER
            | libc::IFF_SLAVE
            | libc::IFF_PORTSEL
            | libc::IFF_AUTOMEDIA
            | libc::IFF_DYNAMIC
            | libc::IFF_LOWER_UP;
        assert_eq!(flags_from_host(flags as u32), 0x8001);
    }

    #[test]
    fn ip_addresses_are_aligned_and_keep_masks_and_peers() {
        let mut ip: libc::sockaddr_in6 = unsafe { core::mem::zeroed() };
        ip.sin6_family = libc::AF_INET6 as _;
        ip.sin6_scope_id = 7;
        let mut host: libc::ifaddrs = unsafe { core::mem::zeroed() };
        host.ifa_name = c"tun0".as_ptr().cast_mut();
        host.ifa_addr = (&mut ip as *mut libc::sockaddr_in6).cast();
        host.ifa_netmask = host.ifa_addr;
        let addr = host.ifa_addr;
        host::set_peer(&mut host, addr);
        host.ifa_flags = libc::IFF_POINTOPOINT as u32;
        unsafe {
            let node = node_from_host(&host).unwrap();
            for ptr in [(*node).addr, (*node).netmask, (*node).dstaddr] {
                assert!(!ptr.is_null());
                assert_eq!(ptr as usize % 8, 0);
                let bytes = core::slice::from_raw_parts(ptr.cast::<u8>(), 28);
                assert_eq!(&bytes[..2], &[28, addr::family::INET6 as u8]);
                assert_eq!(&bytes[24..28], &7u32.to_ne_bytes());
            }
            free_list(node);
        }
    }

    /// Every host has a loopback, and its address must come back in Darwin's
    /// shape - which is the whole reason the list is copied.
    #[test]
    fn the_loopback_is_reported_with_a_darwin_family() {
        let mut list: *mut IfAddrs = core::ptr::null_mut();
        // SAFETY: a live slot this test owns.
        assert_eq!(unsafe { getifaddrs(&mut list) }, 0);
        assert!(!list.is_null());

        let mut seen_inet = false;
        let mut at = list;
        while !at.is_null() {
            // SAFETY: a node of the list just returned.
            unsafe {
                assert!(!(*at).addr.is_null());
                if !(*at).addr.is_null() {
                    let bytes = (*at).addr.cast::<u8>();
                    let family = i32::from(bytes.add(1).read());
                    if family == addr::family::INET {
                        seen_inet = true;
                        assert_eq!(bytes.read(), 16, "Darwin's sin_len");
                    }
                }
                at = (*at).next;
            }
        }
        assert!(seen_inet, "no interface reported an IPv4 address");
        // SAFETY: the list this test was given.
        unsafe { freeifaddrs(list) };
    }
}
