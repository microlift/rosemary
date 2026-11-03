use std::ffi::c_void;

/// `sizeof(struct sockaddr_storage)`, 128 on both platforms.
pub const STORAGE: usize = 128;

/// Darwin's `AF_*`, from `<sys/socket.h>`.
pub mod family {
    pub const UNSPEC: i32 = 0;
    pub const UNIX: i32 = 1;
    pub const INET: i32 = 2;
    pub const INET6: i32 = 30;
}

/// A guest's address family as this host's, or `None` for one it has not got.
///
/// Refused rather than passed through: Darwin's `AF_INET6` is a family this host
/// also defines, for something else.
pub fn family_to_host(darwin: i32) -> Option<i32> {
    Some(match darwin {
        family::UNSPEC => libc::AF_UNSPEC,
        family::UNIX => libc::AF_UNIX,
        family::INET => libc::AF_INET,
        family::INET6 => libc::AF_INET6,
        _ => return None,
    })
}

/// This host's address family as a guest's.
pub fn family_from_host(host: i32) -> Option<i32> {
    Some(match host {
        libc::AF_UNSPEC => family::UNSPEC,
        libc::AF_UNIX => family::UNIX,
        libc::AF_INET => family::INET,
        libc::AF_INET6 => family::INET6,
        _ => return None,
    })
}

/// Bytes before the address proper in every `struct sockaddr`: Darwin's
/// `sa_len` and `sa_family`, or the host's one sixteen-bit family. The two are
/// the same size, which is what lets everything past them be copied as it is.
const HEADER: usize = 2;
const _: () = assert!(core::mem::offset_of!(libc::sockaddr, sa_data) == HEADER);
const _: () = assert!(core::mem::size_of::<libc::sockaddr_storage>() == STORAGE);

/// A guest's `struct sockaddr`: Darwin's layout, whatever the host's is.
///
/// Sized as `sockaddr_storage`, so any address Darwin defines fits.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct DarwinSockaddr {
    pub len: u8,
    pub family: u8,
    pub data: [u8; STORAGE - HEADER],
}

impl DarwinSockaddr {
    /// The `len` bytes at `addr` (at most `STORAGE`) as an address, zero
    /// padded.
    ///
    /// # Safety
    ///
    /// `addr` is `len` readable bytes.
    unsafe fn read(addr: *const c_void, len: usize) -> DarwinSockaddr {
        let mut out = DarwinSockaddr {
            len: 0,
            family: 0,
            data: [0; STORAGE - HEADER],
        };
        // SAFETY: the caller's readable bytes, into a buffer bounded by `STORAGE`.
        unsafe {
            core::ptr::copy_nonoverlapping(
                addr.cast::<u8>(),
                (&raw mut out).cast::<u8>(),
                len.min(STORAGE),
            )
        };
        out
    }
}

impl core::ops::Deref for DarwinSockaddr {
    type Target = [u8; STORAGE];

    fn deref(&self) -> &[u8; STORAGE] {
        // SAFETY: `repr(C)` with no padding: two bytes and `STORAGE - 2` more.
        unsafe { &*(self as *const DarwinSockaddr).cast::<[u8; STORAGE]>() }
    }
}

/// Darwin's `struct sockaddr_un`, the one family whose payload is rewritten.
#[repr(C)]
struct DarwinSockaddrUn {
    len: u8,
    family: u8,
    path: [u8; 104],
}
const _: () = assert!(core::mem::offset_of!(DarwinSockaddrUn, path) == HEADER);

/// A socket address in this host's shape, with the length the host wants.
///
/// Owned by value so a caller can hand `libc` a pointer to it without a second
/// allocation; `sockaddr_storage` is what the host reserves for the largest
/// address it defines, and the most any address either side defines needs.
#[repr(C)]
pub struct HostAddr {
    storage: libc::sockaddr_storage,
    len: libc::socklen_t,
    /// The `AF_UNIX` path this address was built from.
    unix: Option<crate::sockpath::HostPath>,
}

impl HostAddr {
    /// Room for an address the host is about to fill in.
    pub fn empty() -> HostAddr {
        HostAddr {
            // SAFETY: all-zero is a valid `sockaddr_storage`, with family
            // `AF_UNSPEC`.
            storage: unsafe { core::mem::zeroed() },
            len: STORAGE as libc::socklen_t,
            unix: None,
        }
    }

    pub fn as_ptr(&self) -> *const libc::sockaddr {
        (&raw const self.storage).cast()
    }

    pub fn as_mut_ptr(&mut self) -> *mut libc::sockaddr {
        (&raw mut self.storage).cast()
    }

    pub fn len(&self) -> libc::socklen_t {
        self.len
    }

    /// The length slot the host writes the answer's real length into.
    pub fn len_mut(&mut self) -> *mut libc::socklen_t {
        &mut self.len
    }

    /// The address's bytes, as they are in memory.
    pub fn as_bytes(&self) -> &[u8; STORAGE] {
        // SAFETY: `sockaddr_storage` is `STORAGE` bytes (asserted above) and
        // every bit pattern is a valid `u8`.
        unsafe { &*self.as_ptr().cast::<[u8; STORAGE]>() }
    }

    /// The host path of the file a guest's `AF_UNIX` address names,
    /// NUL-terminated, which `sun_path` may only reach indirectly.
    pub fn unix_file(&self) -> Option<&[u8]> {
        self.unix.as_ref().map(|path| path.file.as_slice())
    }

    /// The host's family and, where it keeps one, `sa_len`, for `len` bytes.
    fn set_header(&mut self, family: i32, len: usize) {
        self.storage.ss_family = family as _;
        // A BSD's `sa_len` is the byte before the family; Linux's family is
        // sixteen bits and has no such byte, so writing one would clobber it.
        if cfg!(not(target_os = "linux")) {
            // SAFETY: the first byte of the storage this value owns.
            unsafe { self.as_mut_ptr().cast::<u8>().write(len as u8) };
        }
    }
}

/// A guest's `sockaddr` as this host's, or the errno to fail with:
/// `EAFNOSUPPORT` for a family this host has not got, `EINVAL` for a length
/// that is not an address, or why a socket path could not be spelled.
///
/// # Safety
///
/// `addr` is `len` readable bytes, or is null with `len` zero.
pub unsafe fn to_host(addr: *const c_void, len: u32) -> Result<HostAddr, i32> {
    if addr.is_null() || (len as usize) < HEADER || len as usize > STORAGE {
        return Err(libc::EINVAL);
    }
    let len = len as usize;
    // SAFETY: the caller's `len` readable bytes, which `len` was just checked
    // against `STORAGE`.
    let guest = unsafe { DarwinSockaddr::read(addr, len) };
    let host_family = family_to_host(i32::from(guest.family)).ok_or(libc::EAFNOSUPPORT)?;
    let mut out = HostAddr::empty();
    out.len = len as libc::socklen_t;
    // Everything past the header is laid out alike on both sides, for the
    // families this module knows.
    out.as_bytes_mut()[HEADER..len].copy_from_slice(&guest.data[..len - HEADER]);
    if host_family == libc::AF_UNIX && len > HEADER {
        // SAFETY: `DarwinSockaddrUn` is `repr(C)` of plain bytes, and `guest`
        // is at least as large.
        let un = unsafe { &*(&raw const guest).cast::<DarwinSockaddrUn>() };
        let path = crate::sockpath::to_host(&un.path[..(len - HEADER).min(un.path.len())])?;
        // SAFETY: the storage this value owns is a `sockaddr_un`'s size or more.
        let host_un = unsafe { &mut *(&raw mut out.storage).cast::<libc::sockaddr_un>() };
        if path.bytes.len() > host_un.sun_path.len() {
            return Err(libc::ENAMETOOLONG);
        }
        host_un.sun_path.fill(0);
        for (slot, byte) in host_un.sun_path.iter_mut().zip(&path.bytes) {
            *slot = *byte as _;
        }
        out.len = (core::mem::offset_of!(libc::sockaddr_un, sun_path) + path.bytes.len())
            as libc::socklen_t;
        out.unix = Some(path);
    }
    out.set_header(host_family, out.len as usize);
    Ok(out)
}

impl HostAddr {
    fn as_bytes_mut(&mut self) -> &mut [u8; STORAGE] {
        // SAFETY: as `as_bytes`.
        unsafe { &mut *self.as_mut_ptr().cast::<[u8; STORAGE]>() }
    }
}

/// A host `sockaddr` in Darwin's shape, as bytes and the length that goes with
/// them.
///
/// `None` for a null pointer or a length that is not an address. A family this
/// host has that Darwin has not becomes `AF_UNSPEC` rather than being dropped:
/// the address is still the kernel's answer, and a guest that cannot name the
/// family can still see there was one.
///
/// # Safety
///
/// `addr` is `len` readable bytes.
pub unsafe fn darwin_from_host(addr: *const c_void, len: u32) -> Option<(DarwinSockaddr, usize)> {
    if addr.is_null() || (len as usize) < HEADER {
        return None;
    }
    let real = (len as usize).min(STORAGE);
    let mut host = HostAddr::empty();
    // SAFETY: the caller's readable bytes, bounded by `STORAGE`.
    unsafe { core::ptr::copy_nonoverlapping(addr.cast::<u8>(), host.as_mut_ptr().cast(), real) };
    let family = i32::from(host.storage.ss_family);
    // SAFETY: `host` holds `STORAGE` bytes, `real` of them the caller's.
    let mut out = unsafe { DarwinSockaddr::read(host.as_ptr().cast(), real) };
    out.len = real.min(u8::MAX as usize) as u8;
    out.family = family_from_host(family).unwrap_or(family::UNSPEC) as u8;
    Some((out, real))
}

/// Write `host` into the guest's buffer in Darwin's shape.
///
/// Follows the kernel's own truncation rule: at most `*len` bytes are written
/// and `*len` is then set to the address's *real* length, so a caller that
/// passed too small a buffer can tell.
///
/// # Safety
///
/// `addr` is `*len` writable bytes and `len` is a writable `socklen_t`, or both
/// are null.
pub unsafe fn from_host(host: &HostAddr, addr: *mut c_void, len: *mut u32) {
    if addr.is_null() || len.is_null() {
        return;
    }
    // SAFETY: the caller's own slot.
    let capacity = unsafe { *len } as usize;
    let mut real = host.len as usize;
    // SAFETY: `host` holds `STORAGE` bytes and `len` no more than that.
    let Some((mut darwin, mut copied)) =
        (unsafe { darwin_from_host(host.as_ptr().cast(), host.len) })
    else {
        return;
    };
    if i32::from(host.storage.ss_family) == libc::AF_UNIX
        && let Some(path) = host.as_bytes().get(HEADER..real)
        && let Some(guest) = crate::sockpath::to_guest(path)
    {
        // The host path is the prefix's, or an alias for it; the guest named
        // the socket by its own path and must get that back.
        darwin.data.fill(0);
        let kept = guest.len().min(darwin.data.len() - 1);
        darwin.data[..kept].copy_from_slice(&guest[..kept]);
        real = HEADER + guest.len() + 1;
        copied = HEADER + kept + 1;
        darwin.len = copied.min(u8::MAX as usize) as u8;
    }

    let written = capacity.min(copied);
    // SAFETY: `written` is no more than the caller's capacity and no more than
    // the bytes prepared above.
    unsafe { core::ptr::copy_nonoverlapping(darwin.as_ptr(), addr.cast::<u8>(), written) };
    // SAFETY: the caller's own slot, as above.
    unsafe { *len = real as u32 };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The collision this module exists for, stated as an assertion: Darwin's
    /// `AF_INET6` is a family this host also has, meaning something else.
    #[test]
    fn af_inet6_is_not_the_hosts_number() {
        assert_ne!(family::INET6, libc::AF_INET6);
        assert_eq!(family_to_host(family::INET6), Some(libc::AF_INET6));
    }

    #[test]
    fn a_family_this_host_has_not_got_is_refused() {
        // Darwin's `AF_LINK`, which is not this host's `AF_PACKET`.
        assert_eq!(family_to_host(18), None);
    }

    /// A guest's IPv4 address, converted and converted back, is the address it
    /// started as - port and octets included, which is what a raw pass-through
    /// gets right and the family byte does not.
    #[test]
    fn an_ipv4_address_round_trips_with_its_family_intact() {
        // `sin_len`, `sin_family`, port 443, 192.0.2.7, eight zero bytes.
        let mut guest = [0u8; 16];
        guest[0] = 16;
        guest[1] = family::INET as u8;
        guest[2..4].copy_from_slice(&443u16.to_be_bytes());
        guest[4..8].copy_from_slice(&[192, 0, 2, 7]);

        // SAFETY: sixteen readable bytes, and the length says sixteen.
        let host = unsafe { to_host(guest.as_ptr().cast(), 16) }.expect("a known family");
        assert_eq!(unsafe { (*host.as_ptr()).sa_family } as i32, libc::AF_INET);
        assert_eq!(&host.as_bytes()[2..8], &guest[2..8]);

        let mut back = [0u8; 16];
        let mut len = 16u32;
        // SAFETY: sixteen writable bytes and a live length.
        unsafe { from_host(&host, back.as_mut_ptr().cast(), &mut len) };
        assert_eq!(len, 16);
        assert_eq!(back, guest);
    }

    /// Everything past the family is the same bytes at the same offsets, which
    /// is why only the first two are rewritten.
    #[test]
    fn an_ipv6_address_keeps_its_flow_label_and_scope() {
        let mut guest = [0u8; 28];
        guest[0] = 28;
        guest[1] = family::INET6 as u8;
        for (i, b) in guest[2..28].iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(1);
        }
        // SAFETY: twenty-eight readable bytes, and the length matches.
        let host = unsafe { to_host(guest.as_ptr().cast(), 28) }.expect("a known family");
        assert_eq!(&host.as_bytes()[2..28], &guest[2..28]);

        let mut back = [0u8; 28];
        let mut len = 28u32;
        // SAFETY: twenty-eight writable bytes and a live length.
        unsafe { from_host(&host, back.as_mut_ptr().cast(), &mut len) };
        assert_eq!(back, guest);
    }

    /// Too small a buffer is truncated and the real length reported, which is
    /// how a caller of `getsockname` learns it asked for too little.
    #[test]
    fn a_short_buffer_is_truncated_and_the_real_length_reported() {
        let mut host = HostAddr::empty();
        unsafe {
            (*host.as_mut_ptr()).sa_family = libc::AF_INET as _;
        }
        host.len = 16;

        let mut back = [0xAAu8; 16];
        let mut len = 4u32;
        // SAFETY: sixteen writable bytes, of which the call may use four.
        unsafe { from_host(&host, back.as_mut_ptr().cast(), &mut len) };
        assert_eq!(len, 16, "the real length, not the truncated one");
        assert_eq!(back[1], family::INET as u8);
        assert_eq!(back[4..], [0xAA; 12], "wrote past the caller's length");
    }
}
