//! Who owns a file, as a guest sees it.
//!
//! Every file in a prefix belongs to the host user who made it, and a Mac's
//! system files belong to root. Programs that check - `sudo` refuses a
//! sudoers file root does not own, and itself unless it is set-user-ID root -
//! are told what a Mac would say:
//!
//! * a **set-id program** listed in `.rosemary/setid.yaml` is reported with the
//!   owner, group and mode given there, set-id bits included, and `exec` honours
//!   them (see `darwin::credentials`). Without the file the list is `su` and
//!   `sudo`, which is what a Mac has and what the base cannot carry: a
//!   MiniDarwin artifact has no set-id bits, and nothing here could honour a
//!   host one;
//! * a file whose owner a guest **recorded** - by `chown`ing it, or creating it
//!   while it was somebody other than the host's user - is that owner's. The
//!   record is an extended attribute on the file, [`RECORD`], which is what
//!   `fakeroot` keeps in its daemon's table and rootless containers keep in
//!   the same place: it moves with the file and outlives every process;
//! * a file the **base** put in place is root's, group wheel;
//! * anything else is reported as the host has it.
//!
//! Only the report changes. The host still decides every access by the user
//! who ran the guest, which is what makes this `fakeroot` rather than root.

use std::collections::HashMap;
use std::ffi::{CStr, c_int};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::format::Invalid;
use crate::format::yaml::{self, Value};
use crate::layout::Prefix;

/// The file listing the prefix's set-id programs, in [`Prefix::control`].
pub const SETID_FILE: &str = "setid.yaml";

/// The index of the base's paths, in [`Prefix::control`].
const OWNED_FILE: &str = "owned";
const OWNED_HEADER: &str = "rosemary-owned 1";

/// Darwin's root and its group, `wheel`.
pub const ROOT: u32 = 0;
pub const WHEEL: u32 = 0;

const S_ISUID: u32 = 0o4000;
const S_ISGID: u32 = 0o2000;
const PERMISSIONS: u32 = 0o7777;

/// One set-id program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetIdProgram {
    /// The guest path.
    pub path: String,
    pub owner: u32,
    pub group: u32,
    /// Permission bits, set-id bits included.
    pub mode: u32,
}

impl SetIdProgram {
    fn root(path: &str, mode: u32) -> SetIdProgram {
        SetIdProgram {
            path: path.into(),
            owner: ROOT,
            group: WHEEL,
            mode,
        }
    }
}

/// The list a prefix without a `setid.yaml` has: a Mac's own modes for the
/// two programs that change user.
pub fn default_programs() -> Vec<SetIdProgram> {
    vec![
        SetIdProgram::root("/usr/bin/su", 0o4755),
        SetIdProgram::root("/usr/bin/sudo", 0o4511),
    ]
}

/// Parse a `setid.yaml`:
///
/// ```yaml
/// programs:
///   - {path: "/usr/bin/su", owner: 0, group: 0, mode: "4755"}
/// ```
pub fn parse_programs(text: &str, what: &str) -> Result<Vec<SetIdProgram>, Invalid> {
    let Value::Map(map) = yaml::parse(text, what)? else {
        return Err(Invalid::new(format!("{what}: expected a mapping")));
    };
    if let Some(unknown) = map.keys().find(|key| *key != "programs") {
        return Err(Invalid::new(format!("{what}: unknown key '{unknown}'")));
    }
    let list = match map.get("programs") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::List(list)) => list,
        Some(_) => return Err(Invalid::new(format!("{what}: programs: expected a list"))),
    };
    list.iter()
        .enumerate()
        .map(|(index, item)| {
            let at = format!("{what}: programs[{index}]");
            let Value::Map(entry) = item else {
                return Err(Invalid::new(format!("{at}: expected a mapping")));
            };
            if let Some(unknown) = entry
                .keys()
                .find(|key| !matches!(*key, "path" | "owner" | "group" | "mode"))
            {
                return Err(Invalid::new(format!("{at}: unknown key '{unknown}'")));
            }
            let path = match entry.get("path") {
                Some(Value::Str(path)) if path.starts_with('/') => path.clone(),
                _ => {
                    return Err(Invalid::new(format!(
                        "{at}.path: expected an absolute path"
                    )));
                }
            };
            let id = |key: &str| match entry.get(key) {
                None => Ok(ROOT),
                Some(Value::Int(id)) => u32::try_from(*id)
                    .map_err(|_| Invalid::new(format!("{at}.{key}: {id} is not an id"))),
                Some(_) => Err(Invalid::new(format!("{at}.{key}: expected a number"))),
            };
            // Octal as a string: a YAML number would be read as decimal.
            let mode = match entry.get("mode") {
                Some(Value::Str(mode)) => u32::from_str_radix(mode, 8)
                    .ok()
                    .filter(|mode| mode & !PERMISSIONS == 0)
                    .ok_or_else(|| Invalid::new(format!("{at}.mode: '{mode}' is not a mode")))?,
                _ => return Err(Invalid::new(format!("{at}.mode: expected an octal string"))),
            };
            Ok(SetIdProgram {
                path,
                owner: id("owner")?,
                group: id("group")?,
                mode,
            })
        })
        .collect()
}

/// The set-id programs `prefix` lists, or the defaults when it lists none.
pub fn programs(prefix: &Prefix) -> Result<Vec<SetIdProgram>, Invalid> {
    let path = prefix.control().join(SETID_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_programs(&text, &path.display().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(default_programs()),
        Err(e) => Err(Invalid::new(format!("{}: {e}", path.display()))),
    }
}

/// What a guest is told about a file in place of what the host says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
    /// Permission bits to report instead of the file's own, set-id bits
    /// included; `None` keeps the file's.
    pub mode: Option<u32>,
}

impl Owner {
    /// The set-id an `exec` of this file applies, given the permission bits
    /// the host has for it.
    pub fn setid(&self, host_mode: u32) -> (Option<u32>, Option<u32>) {
        let mode = self.mode.unwrap_or(host_mode);
        (
            (mode & S_ISUID != 0).then_some(self.uid),
            (mode & S_ISGID != 0).then_some(self.gid),
        )
    }
}

struct View {
    /// The guest's `/`, as spelled and as resolved.
    drives: Vec<PathBuf>,
    /// Set-id programs by the file they are, so a hard link, a symlink
    /// followed or a descriptor finds them as well as the path does.
    programs: HashMap<(u64, u64), Owner>,
    owned: Option<Owned>,
}

fn view() -> Option<&'static View> {
    static VIEW: LazyLock<Option<View>> = LazyLock::new(|| {
        let _errno = starnix::errno::Held::now();
        let prefix = crate::ns::current()?;
        let drive = prefix.drive();
        let mut drives = vec![drive.clone()];
        if let Ok(resolved) = std::fs::canonicalize(&drive)
            && resolved != drive
        {
            drives.push(resolved);
        }
        let listed = programs(prefix).unwrap_or_else(|e| {
            eprintln!("rosemary: prefix: {e}; no program is set-id");
            Vec::new()
        });
        let programs = listed
            .iter()
            .filter_map(|program| {
                let host = crate::ns::host_path(&program.path);
                let meta = std::fs::metadata(host).ok()?;
                let owner = Owner {
                    uid: program.owner,
                    gid: program.group,
                    mode: Some(program.mode),
                };
                Some(((meta.dev(), meta.ino()), owner))
            })
            .collect();
        Some(View {
            drives,
            programs,
            owned: Owned::load(prefix),
        })
    });
    VIEW.as_ref()
}

/// Where a file that was stat'ed was found, for [`of`] to ask who owns it.
#[derive(Clone, Copy, Debug)]
pub enum Found<'a> {
    /// A host path, relative to the working directory if not absolute; and
    /// whether a symlink at its end was followed.
    Path(&'a CStr, bool),
    /// A host path relative to a directory descriptor, and the same.
    At(c_int, &'a CStr, bool),
    /// An open descriptor.
    Fd(c_int),
}

/// Who owns the file `(dev, ino)`, found at `found`, if the guest is to be
/// told something other than what the host says.
///
/// Free for a file off the prefix's drive. On it, a hash lookup, one
/// `getxattr`, and a binary search over the base's paths - with a `readlink`
/// first when all there is to go on is a descriptor.
pub fn of(found: Found<'_>, dev: u64, ino: u64) -> Option<Owner> {
    crate::ns::current()?;
    let view = view()?;
    if let Some(owner) = view.programs.get(&(dev, ino)) {
        return Some(*owner);
    }
    if !on_drive(dev) {
        return None;
    }
    let _errno = starnix::errno::Held::now();
    if records_exist()
        && let Some((uid, gid)) = record::read(found)
    {
        return Some(Owner {
            uid,
            gid,
            mode: None,
        });
    }
    let owned = view.owned.as_ref()?;
    let guest = view.guest_path(&host_path_of(found)?)?;
    owned.contains(&guest).then_some(Owner {
        uid: ROOT,
        gid: WHEEL,
        mode: None,
    })
}

/// Record `owner` as who owns the file at `found`, or forget the record with
/// `None`. Only for a file on the prefix's drive, which is the only place a
/// record is read; elsewhere this says `Unsupported`.
pub fn record(found: Found<'_>, dev: u64, owner: Option<(u32, u32)>) -> std::io::Result<()> {
    if !inside(found, dev) {
        return Err(std::io::ErrorKind::Unsupported.into());
    }
    let _errno = starnix::errno::Held::now();
    if owner.is_some()
        && let Some(prefix) = crate::ns::current()
    {
        let marker = prefix.control().join(RECORDS_FILE);
        if !marker.exists() {
            std::fs::write(&marker, b"")?;
        }
        RECORDS_SEEN.store(true, Ordering::Relaxed);
    }
    record::write(found, owner)
}

/// Whether the file at `found`, on device `dev`, is inside the prefix's drive:
/// the only files whose owner the prefix says anything about. The device
/// alone is not enough - the host's own files can share it, and are reached
/// through `/Volumes/Host`.
pub fn inside(found: Found<'_>, dev: u64) -> bool {
    if crate::ns::current().is_none() || !on_drive(dev) {
        return false;
    }
    let _errno = starnix::errno::Held::now();
    let (Some(view), Some(path)) = (view(), host_path_of(found)) else {
        return false;
    };
    view.guest_path(&path).is_some()
}

/// The marker [`record`] leaves in [`Prefix::control`] the first time any
/// owner is recorded. Until it exists no file can carry a record, and [`of`]
/// saves itself a `getxattr` per `stat` - which on a stat-heavy guest is a
/// third again on the filesystem's time.
const RECORDS_FILE: &str = "owners";

static RECORDS_SEEN: AtomicBool = AtomicBool::new(false);
/// When the marker was last found missing, in milliseconds since this
/// process's first look; a long-lived guest notices records another made
/// within a second.
static RECORDS_CHECKED: AtomicU64 = AtomicU64::new(u64::MAX);

fn records_exist() -> bool {
    static EPOCH: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);
    if RECORDS_SEEN.load(Ordering::Relaxed) {
        return true;
    }
    let now = EPOCH.elapsed().as_millis() as u64;
    let last = RECORDS_CHECKED.load(Ordering::Relaxed);
    if last != u64::MAX && now.saturating_sub(last) < 1000 {
        return false;
    }
    RECORDS_CHECKED.store(now, Ordering::Relaxed);
    let present =
        crate::ns::current().is_some_and(|prefix| prefix.control().join(RECORDS_FILE).exists());
    if present {
        RECORDS_SEEN.store(true, Ordering::Relaxed);
    }
    present
}

/// The extended attribute a recorded owner is kept in, as `uid:gid`.
pub const RECORD: &CStr = c"user.rosemary.owner";

/// The absolute host path `found` names, as far as it can be told.
fn host_path_of(found: Found<'_>) -> Option<PathBuf> {
    let (dirfd, path) = match found {
        Found::Fd(fd) => return descriptor_path(fd),
        Found::Path(path, _) => (libc::AT_FDCWD, path),
        Found::At(dirfd, path, _) => (dirfd, path),
    };
    let path = Path::new(std::ffi::OsStr::from_bytes(path.to_bytes()));
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    let directory = match dirfd {
        libc::AT_FDCWD => std::env::current_dir().ok()?,
        fd => descriptor_path(fd)?,
    };
    Some(directory.join(path))
}

fn descriptor_path(fd: c_int) -> Option<PathBuf> {
    starnix::fd::resolved(fd)
}

mod record {
    use super::{Found, RECORD};
    use starnix::xattr::{self, Target};

    fn parse(value: &[u8]) -> Option<(u32, u32)> {
        let text = std::str::from_utf8(value).ok()?;
        let (uid, gid) = text.split_once(':')?;
        Some((uid.parse().ok()?, gid.parse().ok()?))
    }

    fn target(found: Found<'_>) -> Target<'_> {
        match found {
            Found::Fd(fd) => Target::Fd(fd),
            Found::Path(path, follow) => Target::Path(path, follow),
            Found::At(dirfd, path, follow) => Target::At(dirfd, path, follow),
        }
    }

    pub fn read(found: Found<'_>) -> Option<(u32, u32)> {
        let mut value = [0u8; 32];
        let got = xattr::get(target(found), RECORD, &mut value).ok()?;
        parse(value.get(..got)?)
    }

    pub fn write(found: Found<'_>, owner: Option<(u32, u32)>) -> std::io::Result<()> {
        match owner {
            Some((uid, gid)) => {
                xattr::set(target(found), RECORD, format!("{uid}:{gid}").as_bytes())
            }
            // Forgetting a record that was never made is done already.
            None => match xattr::remove(target(found), RECORD) {
                Err(e) if xattr::is_missing(&e) => Ok(()),
                other => other,
            },
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        use std::ffi::CString;

        #[test]
        fn a_record_is_two_ids() {
            assert_eq!(parse(b"0:0"), Some((0, 0)));
            assert_eq!(parse(b"501:20"), Some((501, 20)));
            assert_eq!(parse(b"0"), None);
            assert_eq!(parse(b"a:b"), None);
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        #[test]
        fn a_record_round_trips_on_a_file() {
            let dir = std::env::temp_dir().join(format!("ownership-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("f");
            std::fs::write(&file, b"").unwrap();
            let path = CString::new(file.to_str().unwrap()).unwrap();
            match write(Found::Path(&path, true), Some((0, 80))) {
                // A filesystem without user attributes has nowhere to keep one.
                Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => {}
                Err(e) => panic!("{e}"),
                Ok(()) => {
                    assert_eq!(read(Found::Path(&path, true)), Some((0, 80)));
                    use std::os::fd::AsRawFd;
                    let handle = std::fs::File::open(&file).unwrap();
                    assert_eq!(read(Found::Fd(handle.as_raw_fd())), Some((0, 80)));
                    write(Found::Fd(handle.as_raw_fd()), Some((1234, 20))).unwrap();
                    let directory = std::fs::File::open(&dir).unwrap();
                    assert_eq!(
                        read(Found::At(directory.as_raw_fd(), c"f", true)),
                        Some((1234, 20))
                    );
                    write(Found::At(directory.as_raw_fd(), c"f", true), Some((0, 80))).unwrap();
                    assert_eq!(read(Found::Path(&path, true)), Some((0, 80)));
                    write(Found::Path(&path, true), None).unwrap();
                    assert_eq!(read(Found::Path(&path, true)), None);
                    write(Found::Path(&path, true), None).expect("forgetting twice is fine");
                }
            }
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}

/// Whether the device `dev` holds the prefix's drive, so a caller with no path
/// knows whether finding one is worth a syscall.
pub fn on_drive(dev: u64) -> bool {
    static DEV: LazyLock<Option<u64>> = LazyLock::new(|| {
        let _errno = starnix::errno::Held::now();
        Some(std::fs::metadata(crate::ns::current()?.drive()).ok()?.dev())
    });
    *DEV == Some(dev)
}

impl View {
    /// The drive-relative spelling of `host`, as the base's index has it.
    fn guest_path(&self, host: &Path) -> Option<String> {
        let host = normalise(host);
        self.drives.iter().find_map(|drive| {
            let rest = host.strip_prefix(drive).ok()?;
            Some(format!("/{}", rest.to_str()?))
        })
    }
}

/// The base's paths, sorted, one per line; a symlink's line carries its
/// target after a tab, so a path through `/etc` - which is `private/etc` - is
/// followed without asking the filesystem.
struct Owned {
    text: Vec<u8>,
    /// Each line's path, and its target if it is a symlink.
    lines: Vec<(Span, Option<Span>)>,
}

#[derive(Clone, Copy)]
struct Span(u32, u32);

/// How many base symlinks one lookup follows before giving up, as `ELOOP`.
const MAX_LINKS: usize = 16;

impl Owned {
    /// The index for `prefix`'s base, made from its manifest if it is missing
    /// or was made for another one. `None` for a prefix without a base.
    fn load(prefix: &Prefix) -> Option<Owned> {
        let state =
            crate::state::State::read(&crate::layout::state_path(prefix.directory())).ok()?;
        let sha = state.base?.manifest_sha256;
        let path = prefix.control().join(OWNED_FILE);
        let header = format!("{OWNED_HEADER} {sha}\n");
        let text = match std::fs::read(&path) {
            Ok(text) if text.starts_with(header.as_bytes()) => text,
            // Parsing the manifest costs more than a short-lived guest does,
            // so it is done once per base and kept.
            _ => {
                let provisioned = crate::provision::load(prefix).ok()?;
                let text = Owned::render(&header, &provisioned.manifest);
                let staging = path.with_extension(format!("{}.tmp", std::process::id()));
                if std::fs::write(&staging, &text).is_ok() {
                    let _ = std::fs::rename(&staging, &path);
                }
                text
            }
        };
        Some(Owned::from_text(text, header.len()))
    }

    fn render(header: &str, manifest: &crate::format::manifest::Manifest) -> Vec<u8> {
        use crate::format::manifest::Kind;
        let mut entries: Vec<(&str, Option<&str>)> = manifest
            .entries
            .iter()
            .map(|entry| match &entry.kind {
                Kind::Symlink { target } => (entry.path.as_str(), Some(target.as_str())),
                _ => (entry.path.as_str(), None),
            })
            .collect();
        entries.sort_unstable();
        let mut text = header.as_bytes().to_vec();
        for (path, target) in entries {
            text.extend_from_slice(path.as_bytes());
            if let Some(target) = target {
                text.push(b'\t');
                text.extend_from_slice(target.as_bytes());
            }
            text.push(b'\n');
        }
        text
    }

    fn from_text(text: Vec<u8>, skip: usize) -> Owned {
        let mut lines = Vec::new();
        let mut start = skip;
        for (at, byte) in text.iter().enumerate().skip(skip) {
            if *byte != b'\n' {
                continue;
            }
            let line = &text[start..at];
            let entry = match line.iter().position(|b| *b == b'\t') {
                Some(tab) => (
                    Span(start as u32, (start + tab) as u32),
                    Some(Span((start + tab + 1) as u32, at as u32)),
                ),
                None => (Span(start as u32, at as u32), None),
            };
            lines.push(entry);
            start = at + 1;
        }
        Owned { text, lines }
    }

    fn bytes(&self, span: Span) -> &[u8] {
        &self.text[span.0 as usize..span.1 as usize]
    }

    fn find(&self, path: &[u8]) -> Option<&(Span, Option<Span>)> {
        let at = self
            .lines
            .binary_search_by(|(line, _)| self.bytes(*line).cmp(path))
            .ok()?;
        self.lines.get(at)
    }

    /// Whether `path` is the base's, once every base symlink on the way to its
    /// last component is followed.
    fn contains(&self, path: &str) -> bool {
        let mut path = path.to_string();
        'restart: for _ in 0..MAX_LINKS {
            let mut end = 0;
            while let Some(next) = path[end + 1..].find('/').map(|at| at + end + 1) {
                let directory = &path[..next];
                match self.find(directory.as_bytes()) {
                    None => return false,
                    Some((_, Some(target))) => {
                        let target = String::from_utf8_lossy(self.bytes(*target)).into_owned();
                        let parent = &directory[..directory.rfind('/').unwrap_or(0)];
                        let resolved = match target.starts_with('/') {
                            true => target,
                            false => format!("{parent}/{target}"),
                        };
                        let rest = &path[next..];
                        path = normalise(Path::new(&format!("{resolved}{rest}")))
                            .to_string_lossy()
                            .into_owned();
                        continue 'restart;
                    }
                    Some((_, None)) => end = next,
                }
            }
            return self.find(path.as_bytes()).is_some();
        }
        false
    }
}

/// `.` and `..` resolved as text.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    // A trailing slash names the same directory.
    let bytes = out.as_os_str().as_bytes();
    if bytes.len() > 1 && bytes.ends_with(b"/") {
        out = PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..bytes.len() - 1]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setid_file_lists_programs_with_octal_modes() {
        let text = "programs:\n  - {path: \"/usr/bin/su\", owner: 0, group: 0, mode: \"4755\"}\n  - {path: \"/usr/libexec/x\", mode: \"2711\", group: 80}\n";
        let programs = parse_programs(text, "t").unwrap();
        assert_eq!(programs[0], SetIdProgram::root("/usr/bin/su", 0o4755));
        assert_eq!(programs[1].group, 80);
        assert_eq!(programs[1].mode, 0o2711);
    }

    #[test]
    fn an_empty_list_turns_the_defaults_off() {
        assert!(parse_programs("programs: []\n", "t").unwrap().is_empty());
    }

    #[test]
    fn a_bad_setid_file_is_refused_with_where() {
        for bad in [
            "programs:\n  - {path: \"usr/bin/su\", mode: \"4755\"}\n",
            "programs:\n  - {path: \"/usr/bin/su\", mode: 4755}\n",
            "programs:\n  - {path: \"/usr/bin/su\", mode: \"17777\"}\n",
            "programs:\n  - {path: \"/usr/bin/su\", mode: \"4755\", user: 0}\n",
            "setuid: []\n",
        ] {
            assert!(parse_programs(bad, "t").is_err(), "{bad}");
        }
    }

    #[test]
    fn the_defaults_are_a_macs_su_and_sudo() {
        let programs = default_programs();
        let su = programs.iter().find(|p| p.path == "/usr/bin/su").unwrap();
        let owner = Owner {
            uid: su.owner,
            gid: su.group,
            mode: Some(su.mode),
        };
        assert_eq!(owner.setid(0o755), (Some(ROOT), None));
        let recorded = Owner {
            uid: 80,
            gid: 20,
            mode: None,
        };
        assert_eq!(recorded.setid(0o6755), (Some(80), Some(20)));
    }

    fn index(lines: &[&str]) -> Owned {
        let mut text = b"header\n".to_vec();
        for line in lines {
            text.extend_from_slice(line.as_bytes());
            text.push(b'\n');
        }
        Owned::from_text(text, "header\n".len())
    }

    #[test]
    fn the_index_answers_exact_paths_only() {
        let owned = index(&["/bin", "/bin/sh", "/usr", "/usr/bin", "/usr/bin/su"]);
        assert!(owned.contains("/bin/sh"));
        assert!(owned.contains("/usr/bin/su"));
        assert!(!owned.contains("/bin/s"));
        assert!(!owned.contains("/Users/me"));
    }

    #[test]
    fn a_path_through_a_base_symlink_is_the_targets() {
        let owned = index(&[
            "/etc\tprivate/etc",
            "/private",
            "/private/etc",
            "/private/etc/sudoers",
            "/usr",
            "/usr/lib",
            "/usr/lib/up\t../../private",
        ]);
        assert!(owned.contains("/etc/sudoers"));
        assert!(owned.contains("/etc"), "the link itself is the base's");
        assert!(owned.contains("/usr/lib/up/etc/sudoers"));
        assert!(!owned.contains("/etc/mine"));
    }

    #[test]
    fn a_symlink_loop_in_the_base_ends() {
        let owned = index(&["/a\tb", "/b\ta"]);
        assert!(!owned.contains("/a/x"));
    }

    #[test]
    fn a_host_path_is_matched_under_either_spelling_of_the_drive() {
        let view = View {
            drives: vec![
                PathBuf::from("/home/u/p/root"),
                PathBuf::from("/var/home/u/p/root"),
            ],
            programs: HashMap::new(),
            owned: None,
        };
        for spelled in [
            "/home/u/p/root/private/etc/sudoers",
            "/var/home/u/p/root/private/etc/../etc/./sudoers",
        ] {
            assert_eq!(
                view.guest_path(Path::new(spelled)).as_deref(),
                Some("/private/etc/sudoers")
            );
        }
        assert_eq!(
            view.guest_path(Path::new("/home/u/p/root/")).as_deref(),
            Some("/")
        );
        assert_eq!(view.guest_path(Path::new("/elsewhere/etc")), None);
    }
}
