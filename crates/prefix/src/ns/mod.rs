mod case;
mod guard;
mod listing;
pub mod mount;

pub use guard::{may_write, may_write_at};
pub use listing::{Listing, Shown, listing, listing_of};
use static_init::dynamic;

use std::borrow::Cow;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::layout::{Prefix, Setting};
use mount::Table;

/// The prefix this process runs against, or `None` in identity mode.
pub fn current() -> Option<&'static Prefix> {
    resolved().prefix.as_ref()
}

/// The mount table, empty in identity mode.
pub fn table() -> &'static Table {
    &resolved().table
}

/// The guest's home directory, as the guest spells it.
///
/// `/Users/<name>` under a prefix, whether or not the directory behind it is
/// the host's own. In identity mode it is this host's `$HOME`, because there is
/// no other answer to give.
///
/// This is what `NSHomeDirectory`, `~` expansion and `NSSearchPath...` answer
/// from. Anything that means to *open* a file under it has to put it back
/// through [`host_path`] first.
pub fn home() -> &'static str {
    &resolved().home
}

struct Resolved {
    prefix: Option<Prefix>,
    table: Table,
    home: String,
}

fn resolved() -> &'static Resolved {
    static RESOLVED: LazyLock<Resolved> = LazyLock::new(|| {
        let setting = crate::layout::setting_from(std::env::var_os("ROSEMARY_PREFIX"));
        let Setting::At(directory) = setting else {
            return Resolved {
                prefix: None,
                table: Table::identity(),
                home: host_home_directory(),
            };
        };
        let prefix = Prefix::at(directory);
        let user = crate::layout::user_name();
        // The host's console is root's on Linux; a macOS host's is the guest's.
        let console = cfg!(not(target_os = "macos")).then(|| prefix.console());
        if let Some(console) = &console {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(console);
        }
        let system_version = system_version_file(&prefix);
        let files: Vec<(&str, &Path)> = [
            console.as_deref().map(|host| ("/dev/console", host)),
            system_version
                .as_deref()
                .map(|host| (crate::release::SYSTEM_VERSION_PLIST, host)),
        ]
        .into_iter()
        .flatten()
        .collect();
        let table = Table::rooted_with_files(&prefix.drive(), &user, host_home(), &files);
        Resolved {
            home: format!("/Users/{user}"),
            prefix: Some(prefix),
            table,
        }
    });
    &RESOLVED
}

/// The host file behind the guest's `SystemVersion.plist`, written from
/// [`crate::release`] so it cannot disagree with `kern.osproductversion`.
///
/// Named for its contents: processes told different releases share a prefix
/// without rewriting each other's answer. Read-only, as the real one is to
/// anyone but root.
fn system_version_file(prefix: &Prefix) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let contents = crate::release::system_version_plist();
    let digest = crate::format::sha256_hex(contents.as_bytes());
    let directory = prefix.control().join("system-version");
    let path = directory.join(format!("{digest}.plist"));
    if path.is_file() {
        return Some(path);
    }
    std::fs::create_dir_all(&directory).ok()?;
    let staged = directory.join(format!("{digest}.{}.new", std::process::id()));
    std::fs::write(&staged, contents).ok()?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o444)).ok()?;
    std::fs::rename(&staged, &path).ok()?;
    Some(path)
}

/// This host's own home directory, which is the guest's too in identity mode.
///
/// Public because a guest is told [`home`] instead, and something has to be
/// able to say which value that stands in for - see `darwin`'s `getenv`, which
/// substitutes only when the variable still holds this.
///
/// `HOME` first, as every Unix does, and the password database only if it is
/// unset - the order a guest's own `getenv("HOME")` would see, so the two agree
/// about where a save file goes.
pub fn host_home_directory() -> String {
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        return home.to_string_lossy().into_owned();
    }
    // SAFETY: `getpwuid` returns a pointer into the library's own storage,
    // valid until the next call to it - which this makes before returning.
    unsafe {
        let passwd = libc::getpwuid(libc::getuid());
        if passwd.is_null() || (*passwd).pw_dir.is_null() {
            return "/".to_string();
        }
        std::ffi::CStr::from_ptr((*passwd).pw_dir)
            .to_string_lossy()
            .into_owned()
    }
}

/// The host directory guest `/Users/<name>` is, when the user asked for their
/// own rather than the prefix's.
///
/// Off by default: a prefix that carries its own home is one directory to copy,
/// delete or archive, and a guest cannot scribble on the real one.
fn host_home() -> Option<&'static Path> {
    static HOME: LazyLock<Option<PathBuf>> = LazyLock::new(|| {
        if std::env::var("ROSEMARY_PREFIX_HOME").as_deref() != Ok("host") {
            return None;
        }
        std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
    });
    HOME.as_deref()
}

/// Whether every translation is announced on stderr.
///
/// `ROSEMARY_TRACE_PREFIX=1`. The namespace sits underneath every file call a
/// guest makes, so when a program behaves differently inside a prefix this is
/// the first thing to look at: it shows the exact pair of spellings, which is
/// what a wrong answer here looks like.
fn tracing() -> bool {
    #[dynamic]
    static ON: bool =
        std::env::var_os("ROSEMARY_TRACE_PREFIX").is_some_and(|v| v != "0" && !v.is_empty());
    *ON
}

/// The host path `guest` names.
///
/// String work only - no syscall decides this - so it is cheap enough to run on
/// every path a guest passes. Identity mode, a relative path and a path no
/// mount claims all give the caller's own string back untouched.
pub fn to_host(guest: &CStr) -> Cow<'_, CStr> {
    let table = table();
    if table.is_identity() {
        if tracing() {
            eprintln!("prefix: to_host {} -> (identity)", guest.to_string_lossy());
        }
        return Cow::Borrowed(guest);
    }
    let Ok(text) = guest.to_str() else {
        // A path that is not UTF-8 is not one this table can speak about. It
        // is still a perfectly legal path, so it goes through as it came.
        return Cow::Borrowed(guest);
    };
    match table.to_host(text).and_then(cstring) {
        Some(host) => {
            if tracing() {
                eprintln!("prefix: to_host {text} -> {}", host.to_string_lossy());
            }
            Cow::Owned(host)
        }
        None => {
            if tracing() {
                eprintln!("prefix: to_host {text} -> (unclaimed)");
            }
            Cow::Borrowed(guest)
        }
    }
}

/// [`to_host`] for a path interpreted against an open directory descriptor.
///
/// A relative path is left alone: the descriptor it is resolved against is
/// already a host directory, so mapping it would apply the prefix twice.
pub fn to_host_at(dirfd: libc::c_int, guest: &CStr) -> Cow<'_, CStr> {
    match guest.to_bytes().first() {
        Some(b'/') => to_host(guest),
        _ if dirfd == libc::AT_FDCWD => to_host(guest),
        _ => Cow::Borrowed(guest),
    }
}

/// The host path `guest` names, as a [`PathBuf`], for the Rust side of the
/// tree - the framework crates, which deal in `Path` rather than `CStr`.
pub fn host_path(guest: &str) -> PathBuf {
    match table().to_host(guest) {
        Some(host) => host,
        None => PathBuf::from(guest),
    }
}

/// [`host_path`], spelled the way the filesystem spells it when only a
/// case-insensitive reading of it exists.
///
/// For callers that open with `std::fs` rather than through the libc shims,
/// which fold on their own `ENOENT`: a Mac volume is case-insensitive, and a
/// program that hard-codes `Contents/PlugIns` finds a bundle's `Plugins` there.
/// A path that does not exist in any case comes back as `host_path` spells it,
/// so creating one is unaffected.
pub fn existing_host_path(guest: &str) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let host = host_path(guest);
    let folded = CString::new(host.as_os_str().as_bytes())
        .ok()
        .and_then(|exact| fold(&exact));
    match folded {
        Some(found) => PathBuf::from(std::ffi::OsStr::from_bytes(found.as_bytes())),
        None => host,
    }
}

/// The guest spelling of a host path.
///
/// `None` means no mount reaches it, and the caller must report the failure it
/// already has rather than hand the host path over: a Darwin program that is
/// given `/home/user/...` will store it, compare it, and be wrong about it
/// later in a way nothing traces back to here.
///
/// In identity mode every host path is its own guest path.
pub fn to_guest(host: &Path) -> Option<PathBuf> {
    let table = table();
    if table.is_identity() {
        if tracing() {
            eprintln!("prefix: to_guest {} -> (identity)", host.display());
        }
        return Some(host.to_path_buf());
    }
    let spelled = table.to_guest(host);
    if tracing() {
        match &spelled {
            Some(guest) => eprintln!("prefix: to_guest {} -> {}", host.display(), guest.display()),
            None => eprintln!("prefix: to_guest {} -> (no guest spelling)", host.display()),
        }
    }
    spelled
}

/// The host path spelled the way the filesystem spells it, or `None` if no
/// case-insensitive reading of it exists either.
///
/// A fallback and nothing more: the caller has already tried the exact name and
/// been told `ENOENT`. An exact match always wins; see the `case` module.
pub fn fold(host: &CStr) -> Option<CString> {
    case::insensitive(host)
}

/// [`fold`] for a path interpreted against an open directory descriptor.
pub fn fold_at(dirfd: libc::c_int, host: &CStr) -> Option<CString> {
    case::insensitive_at(dirfd, host)
}

fn cstring(path: PathBuf) -> Option<CString> {
    use std::os::unix::ffi::OsStringExt;
    CString::new(path.into_os_string().into_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests here cannot set `ROSEMARY_PREFIX` - the table is resolved once
    /// per process - so they assert the shape the process actually has, which
    /// on a machine with no prefix created is identity.
    #[test]
    fn identity_mode_hands_every_path_straight_back() {
        if !table().is_identity() {
            return;
        }
        assert!(matches!(to_host(c"/usr/lib"), Cow::Borrowed(_)));
        assert_eq!(
            to_guest(Path::new("/home/user")),
            Some(PathBuf::from("/home/user"))
        );
        assert_eq!(host_path("/usr/lib"), PathBuf::from("/usr/lib"));
    }

    /// A relative path is resolved against a working directory that is already
    /// a host directory.
    #[test]
    fn a_relative_path_is_never_mapped() {
        assert!(matches!(
            to_host_at(3, c"Contents/Info.plist"),
            Cow::Borrowed(_)
        ));
    }
}
