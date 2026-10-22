//! The per-user directories under `/var/folders` that `confstr`'s
//! `_CS_DARWIN_USER_DIR`, `_CS_DARWIN_USER_TEMP_DIR` and
//! `_CS_DARWIN_USER_CACHE_DIR` name: Libc's `darwin/_dirhelper.c`
//! (apple-oss-distributions/Libc at Libc-825.40.1).
//!
//! A user's directory is named for its UUID and uid, encoded five bits to a
//! character, with the first two characters a bucket of their own:
//! `/var/folders/zz/zyxvpxvq6csfxvn_n0000000000000/T/`. The UUID is the one
//! Libinfo's `mbr_uid_to_uuid` gives an account with no directory record - the
//! compatibility UUID, `FFFFEEEE-DDDD-CCCC-BBBB-AAAA` and the uid - since no
//! account here has one.

use std::ffi::{CStr, CString};
use std::io;

/// `dirhelper_which_t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// `DIRHELPER_USER_LOCAL`, `0/`.
    User = 0,
    /// `DIRHELPER_USER_LOCAL_TEMP`, `T/`.
    Temp = 1,
    /// `DIRHELPER_USER_LOCAL_CACHE`, `C/`.
    Cache = 2,
}

impl Which {
    /// `subdirs[which]`: `DIRHELPER_TOP_STR`, `DIRHELPER_TEMP_STR` and
    /// `DIRHELPER_CACHE_STR`.
    fn subdir(self) -> &'static str {
        match self {
            Which::User => "0/",
            Which::Temp => "T/",
            Which::Cache => "C/",
        }
    }

    /// `modes[which]`.
    fn mode(self) -> libc::mode_t {
        match self {
            Which::User => 0o755,
            Which::Temp | Which::Cache => 0o700,
        }
    }
}

/// `VAR_FOLDERS_PATH`.
const VAR_FOLDERS_PATH: &str = "/var/folders/";

/// `BUCKETLEN`.
const BUCKETLEN: usize = 2;

/// `DIRHELPER_ENV_USER_DIR_SUFFIX`.
const USER_DIR_SUFFIX: &CStr = c"DIRHELPER_USER_DIR_SUFFIX";

/// `_user_compat_prefix` in Libinfo's `membership.c`: the first twelve bytes
/// of the UUID `mbr_uid_to_uuid` makes up for a uid with no record.
const USER_COMPAT_PREFIX: [u8; 12] = [
    0xff, 0xff, 0xee, 0xee, 0xdd, 0xdd, 0xcc, 0xcc, 0xbb, 0xbb, 0xaa, 0xaa,
];

/// "lower case letter (minus vowels), plus numbers and _, making 32
/// characters."
const ENCODE: &[u8; 32] = b"0123456789_bcdfghjklmnpqrstvwxyz";

/// `UUID_UID_SIZE`.
const UUID_UID_SIZE: usize = 16 + 4;

/// `ENCODEDSIZE`: 160 bits, five to a character.
const ENCODEDSIZE: usize = (8 * UUID_UID_SIZE).div_ceil(5);

/// `mbr_uid_to_uuid` for an account with no directory record.
pub fn compat_uuid(uid: u32) -> [u8; 16] {
    let mut uuid = [0; 16];
    uuid[..12].copy_from_slice(&USER_COMPAT_PREFIX);
    uuid[12..].copy_from_slice(&uid.to_be_bytes());
    uuid
}

/// `encode_uuid_uid`: the UUID and the big-endian uid, five bits to a
/// character, most significant first, the last padded with zeros.
fn encode_uuid_uid(uuid: &[u8; 16], uid: u32) -> String {
    let mut bytes = [0u8; UUID_UID_SIZE];
    bytes[..16].copy_from_slice(uuid);
    bytes[16..].copy_from_slice(&uid.to_be_bytes());
    (0..ENCODEDSIZE)
        .map(|i| {
            let bit = i * 5;
            let (byte, shift) = (bit / 8, bit % 8);
            let pair =
                u16::from(bytes[byte]) << 8 | u16::from(bytes.get(byte + 1).copied().unwrap_or(0));
            ENCODE[usize::from((pair >> (11 - shift)) & 0x1f)] as char
        })
        .collect()
}

/// `__user_local_dirname`: `/var/folders/<bucket>/<rest>/<subdir>`.
pub fn user_local_dirname(uid: u32, which: Which) -> String {
    let encoded = encode_uuid_uid(&compat_uuid(uid), uid);
    format!(
        "{VAR_FOLDERS_PATH}{}/{}/{}",
        &encoded[..BUCKETLEN],
        &encoded[BUCKETLEN..],
        which.subdir()
    )
}

/// `mkdir` on the guest's `path`, an existing directory being no error.
fn make_directory(path: &str, mode: libc::mode_t) -> io::Result<()> {
    let guest = CString::new(path).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let host = prefix::ns::to_host(&guest);
    // SAFETY: a NUL-terminated path.
    match unsafe { libc::mkdir(host.as_ptr(), mode) } {
        0 => Ok(()),
        _ => match io::Error::last_os_error() {
            error if error.raw_os_error() == Some(libc::EEXIST) => Ok(()),
            error => Err(error),
        },
    }
}

/// Whether the guest's `path` exists.
fn exists(path: &str) -> io::Result<bool> {
    let guest = CString::new(path).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let host = prefix::ns::to_host(&guest);
    let mut sb = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: a NUL-terminated path and a `stat` to fill.
    match unsafe { libc::stat(host.as_ptr(), sb.as_mut_ptr()) } {
        0 => Ok(true),
        _ => match io::Error::last_os_error() {
            error if error.raw_os_error() == Some(libc::ENOENT) => Ok(false),
            error => Err(error),
        },
    }
}

/// `__user_local_mkdir_p`: each directory below `/var/folders/` in `path`,
/// which ends in a separator - and `/var/folders` itself, which a Mac's
/// installation makes and a prefix need not have.
fn mkdir_p(path: &str) -> io::Result<()> {
    make_directory(VAR_FOLDERS_PATH.trim_end_matches('/'), 0o755)?;
    let start = VAR_FOLDERS_PATH.len();
    for (at, _) in path[start..].match_indices('/') {
        make_directory(&path[..start + at], 0o755)?;
    }
    Ok(())
}

/// The guest's value of the environment variable `name`.
fn guest_env(name: &CStr) -> Option<String> {
    // SAFETY: a NUL-terminated name.
    let value = unsafe { crate::env::getenv(name.as_ptr()) };
    // SAFETY: a value `getenv` returned is NUL-terminated.
    (!value.is_null()).then(|| {
        unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned()
    })
}

/// `_dirhelper(which, path, pathlen)`: the user's directory of this kind,
/// made if it is not there, ending in a separator.
///
/// The user's own directory under `/var/folders` is made by the `dirhelper`
/// daemon on a Mac, or by the process itself when it is root; here the
/// process has the rights to make it in its prefix, so it does as root would.
/// The process is never in App Sandbox, so the container suffix is only ever
/// `DIRHELPER_USER_DIR_SUFFIX`'s.
pub fn dirhelper(which: Which) -> io::Result<String> {
    let mut userdir = user_local_dirname(crate::credentials::geteuid(), Which::User);
    // "All dirhelper directories are now at the same level, so we need to
    // remove the DIRHELPER_TOP_STR suffix to get the parent directory."
    userdir.truncate(userdir.len() - Which::User.subdir().len());
    if !exists(&userdir)? {
        mkdir_p(&userdir)?;
    }

    let mut path = userdir + which.subdir();
    make_directory(&path, which.mode())?;

    if let Some(suffix) = guest_env(USER_DIR_SUFFIX) {
        path.push_str(&suffix);
        path.push('/');
        make_directory(&path, which.mode())?;
        if which == Which::Temp
            && guest_env(c"TMPDIR").is_none_or(|tmpdir| !tmpdir.starts_with(&path))
        {
            let value = CString::new(path.as_str())
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
            // SAFETY: two NUL-terminated strings.
            unsafe { crate::env::setenv(c"TMPDIR".as_ptr(), value.as_ptr(), 1) };
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoding against Libc's own, written out by hand: twenty bytes,
    /// thirty-two characters, the first two the bucket.
    #[test]
    fn a_users_directory_is_named_as_libc_names_it() {
        let uid = 501;
        let encoded = encode_uuid_uid(&compat_uuid(uid), uid);
        assert_eq!(encoded.len(), ENCODEDSIZE);
        assert_eq!(ENCODEDSIZE, 32);
        // 0xff 0xff: 11111 11111 11111 1....
        assert!(encoded.starts_with("zzz"));
        // The uid, twice over: as the UUID's last four bytes, then itself;
        // 0x000001f5 0x000001f5 ends ...0001 1111 0101 (and four zero bits).
        let bits: String = [
            0xffu8, 0xff, 0xee, 0xee, 0xdd, 0xdd, 0xcc, 0xcc, 0xbb, 0xbb, 0xaa, 0xaa, 0, 0, 1,
            0xf5, 0, 0, 1, 0xf5,
        ]
        .iter()
        .map(|b| format!("{b:08b}"))
        .collect();
        let want: String = bits
            .as_bytes()
            .chunks(5)
            .map(|chunk| {
                let text = std::str::from_utf8(chunk).unwrap();
                ENCODE[usize::from_str_radix(&format!("{text:0<5}"), 2).unwrap()] as char
            })
            .collect();
        assert_eq!(encoded, want);
        assert_eq!(
            user_local_dirname(uid, Which::Temp),
            format!("/var/folders/{}/{}/T/", &want[..2], &want[2..])
        );
    }

    /// The well-known name a Mac gives the directories of an account with no
    /// directory record, uid 0 - root's, the same on every Mac.
    #[test]
    fn roots_directory_is_the_one_every_mac_has() {
        assert_eq!(
            user_local_dirname(0, Which::User),
            "/var/folders/zz/zyxvpxvq6csfxvn_n0000000000000/0/"
        );
    }
}
