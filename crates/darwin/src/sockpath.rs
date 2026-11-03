//! `AF_UNIX` socket paths, which name files and so live in the prefix.
//!
//! A socket bound at `/private/var/run/x.sock` by one guest is the file
//! another guest connects to by the same name, so both must reach the same
//! host file - the prefix's, not the host's own `/private/var/run`.
//!
//! `net` converts every address, on every host, and calls [`to_host`] for this
//! part, [`bound`] once the file exists, and [`to_guest`] for every address
//! the kernel hands back.

use std::ffi::OsStr;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// A guest's `sun_path` as the host should be given it.
///
/// `bytes` is what goes in `sun_path`, which for a long path is a name in
/// [`alias_root`] rather than the file's own; `file` is the path that names
/// the file itself, which is what anything that records facts about the file
/// by path - see [`bound`] - has to be given.
pub struct HostPath {
    pub bytes: Vec<u8>,
    pub file: Vec<u8>,
}

/// The host spelling of a guest's `sun_path` bytes, NUL-terminated, or the
/// errno to fail with.
///
/// An abstract or unnamed address (empty, or not absolute) is not a file in
/// any prefix and is returned unchanged; a relative one is already resolved
/// against a host working directory.
pub fn to_host(path: &[u8]) -> Result<HostPath, i32> {
    let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
    let path = &path[..end];
    if path.first() != Some(&b'/') {
        let bytes = [path, &[0]].concat();
        return Ok(HostPath {
            file: bytes.clone(),
            bytes,
        });
    }
    let Ok(guest) = std::ffi::CString::new(path) else {
        return Err(libc::EINVAL);
    };
    let host = prefix::ns::to_host(&guest);
    let bytes = host.to_bytes_with_nul();
    if bytes.len() <= host_capacity() {
        return Ok(HostPath {
            bytes: bytes.to_vec(),
            file: bytes.to_vec(),
        });
    }
    // `sun_path` is a fixed array on the host too, and a prefix deep in the
    // host's tree (a Flatpak's `~/.var/app/...`) can push a short guest path
    // past it. A guest that fit Darwin's limit should not fail for that.
    through_alias(host.to_bytes(), &alias_root())
}

/// The guest spelling of a `sun_path` the kernel reported, without its NUL,
/// or `None` to pass it on as it is: unnamed, abstract, or relative.
///
/// A name in [`alias_root`] is read back through its link first. The kernel
/// keeps the name a socket was bound by, so this is what `getsockname` and
/// `getpeername` see - the peer's in another process, too, which is why the
/// alias is a link any process can read rather than a descriptor of one.
pub fn to_guest(host: &[u8]) -> Option<Vec<u8>> {
    to_guest_with(host, &alias_root())
}

fn to_guest_with(host: &[u8], aliases: &Path) -> Option<Vec<u8>> {
    let end = host.iter().position(|&b| b == 0).unwrap_or(host.len());
    let host = Path::new(OsStr::from_bytes(&host[..end]));
    if !host.is_absolute() {
        return None;
    }
    let file = match host.strip_prefix(aliases).ok().and_then(|rest| {
        let mut parts = rest.components();
        let link = parts.next()?;
        Some(
            std::fs::read_link(aliases.join(link))
                .ok()?
                .join(parts.as_path()),
        )
    }) {
        Some(real) => real,
        None => host.to_path_buf(),
    };
    // A socket no mount reaches is still the kernel's answer, and the guest
    // gets the host's spelling of it rather than nothing.
    let guest = prefix::ns::to_guest(&file).unwrap_or(file);
    Some(guest.into_os_string().into_vec())
}

/// Where the links [`through_alias`] makes live: short, private to the host
/// user, and the same for every process, which all need to agree on a name.
fn alias_root() -> PathBuf {
    server::location::runtime_directory().join("sockets")
}

/// `path` spelled as `<aliases>/<key>/name`, `<key>` a symlink to its
/// directory: `bind` and `connect` follow a link in a directory component, so
/// both reach the same file by a name short enough for `sun_path`.
///
/// The key is a hash of the directory, so every process makes the same link
/// for it and a name any of them reports can be read back by any other.
fn through_alias(path: &[u8], aliases: &Path) -> Result<HostPath, i32> {
    let split = path.iter().rposition(|&b| b == b'/').unwrap_or(0);
    let (dir, name) = (&path[..split.max(1)], &path[split + 1..]);
    let key = format!("{:016x}", fnv1a(dir));
    let mut bytes = aliases.join(&key).into_os_string().into_vec();
    bytes.push(b'/');
    bytes.extend_from_slice(name);
    bytes.push(0);
    if bytes.len() > host_capacity() {
        return Err(libc::ENAMETOOLONG);
    }
    let errno = |e: std::io::Error| e.raw_os_error().unwrap_or(libc::EIO);
    server::location::prepare(aliases).map_err(errno)?;
    link(Path::new(OsStr::from_bytes(dir)), &aliases.join(&key)).map_err(errno)?;
    Ok(HostPath {
        bytes,
        file: [path, &[0]].concat(),
    })
}

/// Make `link` point at `target`, unless it already does. A link left by a
/// hash collision or a moved prefix is replaced by a rename, so a process
/// following it meanwhile finds the old target or the new, never neither.
fn link(target: &Path, link: &Path) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if std::fs::read_link(link).is_ok_and(|now| now == target) {
        return Ok(());
    }
    let mut staged = link.as_os_str().to_owned();
    staged.push(format!(
        ".{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let staged = PathBuf::from(staged);
    std::os::unix::fs::symlink(target, &staged)?;
    std::fs::rename(&staged, link).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}

/// 64-bit FNV-1a: stable across builds and processes, which `std`'s hasher
/// does not promise.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Record the socket file a successful `bind` just made at host path `host`
/// as the guest's, the way every other file a guest creates is: a daemon
/// checks its old socket is its own before replacing it.
pub fn bound(host: &[u8]) {
    if !crate::credentials::creates_as_another() {
        return;
    }
    let Ok(path) = std::ffi::CStr::from_bytes_until_nul(host) else {
        return;
    };
    if !path.is_empty() {
        crate::credentials::created(libc::AT_FDCWD, path);
    }
}

fn host_capacity() -> usize {
    // SAFETY: only the array's size is taken.
    let un: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    un.sun_path.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unnamed_or_relative_socket_path_is_left_alone() {
        assert_eq!(to_host(b"").unwrap().bytes, b"\0");
        assert_eq!(to_host(b"rel.sock\0junk").unwrap().bytes, b"rel.sock\0");
        assert_eq!(to_guest(b""), None);
        assert_eq!(to_guest(b"\0abstract"), None);
        assert_eq!(to_guest(b"rel.sock\0"), None);
    }

    #[test]
    fn an_absolute_socket_path_comes_back_terminated() {
        let host = to_host(b"/tmp/x.sock").unwrap().bytes;
        assert_eq!(host.last(), Some(&0));
        assert!(host.ends_with(b"x.sock\0"));
    }

    fn unix_address(path: &[u8]) -> libc::sockaddr_un {
        // SAFETY: all-zero is a valid `sockaddr_un`.
        let mut un: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        un.sun_family = libc::AF_UNIX as _;
        for (slot, byte) in un.sun_path.iter_mut().zip(path) {
            *slot = *byte as _;
        }
        un
    }

    /// A prefix deep enough to push the path past `sun_path` still binds and
    /// connects, both ends reaching the one file, and the name the kernel
    /// reports for it reads back as that file.
    #[test]
    fn a_socket_path_longer_than_sun_path_binds_connects_and_reads_back() {
        let root = std::env::temp_dir().join(format!("sockpath-{}", std::process::id()));
        let aliases = root.join("a");
        let dir = root.join("d".repeat(60)).join("e".repeat(60));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.sock");
        let file = file.as_os_str().as_bytes();
        assert!(file.len() > host_capacity());

        let size = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
        let bound = through_alias(file, &aliases).unwrap();
        assert_eq!(bound.file, [file, &[0]].concat());
        let address = unix_address(&bound.bytes);
        // SAFETY: a fresh socket and an address this test owns.
        let server = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert_eq!(
            unsafe { libc::bind(server, (&raw const address).cast(), size) },
            0
        );
        assert_eq!(unsafe { libc::listen(server, 1) }, 0);
        assert!(dir.join("x.sock").exists());

        let reached = through_alias(file, &aliases).unwrap();
        assert_eq!(reached.bytes, bound.bytes);
        let address = unix_address(&reached.bytes);
        // SAFETY: as above.
        let client = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert_eq!(
            unsafe { libc::connect(client, (&raw const address).cast(), size) },
            0
        );

        // SAFETY: a zeroed address for the kernel to fill, and its length.
        let mut peer: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let mut len = size;
        assert_eq!(
            unsafe { libc::getpeername(client, (&raw mut peer).cast(), &mut len) },
            0
        );
        let reported: Vec<u8> = peer.sun_path.iter().map(|&c| c as u8).collect();
        assert_eq!(to_guest_with(&reported, &aliases).unwrap(), file);

        // SAFETY: descriptors this test owns and no longer uses.
        unsafe {
            libc::close(client);
            libc::close(server);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A link a different directory left at the same name - a moved prefix -
    /// is pointed at the right one rather than trusted.
    #[test]
    fn a_stale_alias_is_replaced() {
        let root = std::env::temp_dir().join(format!("sockpath-stale-{}", std::process::id()));
        let aliases = root.join("a");
        let dir = root.join("f".repeat(120));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("y.sock");
        let file = file.as_os_str().as_bytes();
        let first = through_alias(file, &aliases).unwrap();
        let end = first.bytes.iter().rposition(|&b| b == b'/').unwrap();
        let link = PathBuf::from(OsStr::from_bytes(&first.bytes[..end]));
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&root, &link).unwrap();

        through_alias(file, &aliases).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), dir);
        std::fs::remove_dir_all(root).unwrap();
    }
}
