use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::format::manifest::{DIR_MODE, Entry, Kind, Manifest};

/// The directory holding the prefix's own bookkeeping.
pub const CONTROL: &str = ".rosemary";

/// The directory that is the guest's `/`.
pub const DRIVE: &str = "root";

/// Prefix-owned paths expected alongside the downloaded MiniDarwin base.
/// The user's files inside these directories are not part of the base.
const DIRECTORIES: &[&str] = &[
    "/Applications",
    "/Library",
    "/System",
    "/System/Library",
    "/usr",
    "/usr/lib",
    "/private",
    "/private/tmp",
    "/private/var",
    // Darwin's `P_tmpdir`, which `mkstemp` templates are commonly built from.
    "/private/var/tmp",
    // Root's home, which `su -` and `sudo -i` change into.
    "/private/var/root",
    "/Volumes",
];
const SYMLINKS: &[(&str, &str)] = &[("/tmp", "private/tmp"), ("/var", "private/var")];
const USER_DIRECTORIES: &[&str] = &[
    "Library",
    "Library/Application Support",
    "Library/Caches",
    "Library/Logs",
    "Library/Preferences",
    "Library/Saved Application State",
];

/// Add prefix-owned layout paths to a base manifest for prefix checks and
/// repairs. The downloaded manifest itself remains unchanged and hashable.
pub fn expected_manifest(base: &Manifest) -> Manifest {
    let mut entries = base.entries.clone();
    for path in DIRECTORIES {
        if !base.contains(path) {
            entries.push(directory_entry(path.to_string()));
        }
    }
    if !base.contains("/Users") {
        entries.push(directory_entry("/Users".into()));
    }
    let home = format!("/Users/{}", user_name());
    if !base.contains(&home) {
        entries.push(directory_entry(home.clone()));
    }
    for relative in USER_DIRECTORIES {
        let path = format!("{home}/{relative}");
        if !base.contains(&path) {
            entries.push(directory_entry(path));
        }
    }
    for (path, target) in SYMLINKS {
        if !base.contains(path) {
            entries.push(Entry {
                path: (*path).into(),
                kind: Kind::Symlink {
                    target: (*target).into(),
                },
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Manifest::new(base.identity.clone(), base.build.clone(), entries)
}

fn directory_entry(path: String) -> Entry {
    Entry {
        path,
        kind: Kind::Directory {
            mode: DIR_MODE,
            digest: String::new(),
        },
    }
}

/// What `ROSEMARY_PREFIX` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setting {
    /// No prefix: a guest path is a host path. The default on macOS, which has
    /// a Darwin root already, and the escape hatch everywhere else.
    Off,
    /// The prefix directory - not the guest's `/`, which is [`Prefix::drive`].
    At(PathBuf),
}

/// Which prefix this process uses, from `ROSEMARY_PREFIX`.
///
/// The variable is an argument rather than a read, because the answer is
/// resolved once for the process and a test that set it could not otherwise
/// observe it. The same shape as `metal_translate`'s cache root.
///
/// Unset means *off*, deliberately. Whether there should be a prefix when
/// nobody said is a policy question, and the answer differs between a guest the
/// `rosemary` command is running - where Linux gets one by default - and this
/// crate used as a library, where quietly adopting a directory that happens to
/// exist would make behaviour depend on what a previous run left behind. The
/// command decides, and says so by setting the variable.
pub fn setting_from(prefix: Option<OsString>) -> Setting {
    match prefix {
        None => Setting::Off,
        Some(value) if matches!(value.to_str(), Some("" | "off" | "0")) => Setting::Off,
        // A path this cannot read is still a path; refusing it would silently
        // run the guest somewhere else.
        Some(value) => Setting::At(PathBuf::from(value)),
    }
}

/// Whether a prefix has been created at `directory`.
pub fn created(directory: &Path) -> bool {
    state_path(directory).is_file()
}

/// The file whose presence says a prefix was created, and what it is.
pub fn state_path(directory: &Path) -> PathBuf {
    directory.join(CONTROL).join("state.yaml")
}

/// Make a prefix at `directory`, or report that one is already there.
///
/// Only the shape: the directories a Mac program expects to exist, and the
/// state file that says this is a prefix. Filling `/usr/lib` and
/// `/System/Library` from a verified MiniDarwin base is provisioning, which is
/// a separate step - and one a guest does not need to have taken, because the
/// system libraries it links against are this runtime's own.
///
/// `Ok(false)` means there was already a prefix here and nothing was written.
pub fn create(directory: &Path) -> std::io::Result<bool> {
    if created(directory) {
        return Ok(false);
    }
    // Two processes started together on a fresh prefix would otherwise both
    // make its symlinks, and the second fails with EEXIST.
    let _lock = crate::repair::lock(&Prefix::at(directory))?;
    if created(directory) {
        return Ok(false);
    }
    let drive = directory.join(DRIVE);
    let user = user_name();
    fill_in(directory)?;
    for (path, target) in SYMLINKS {
        // `original` should not be absolute because that would make the prefix unmovable from folder to folder
        std::os::unix::fs::symlink(target, drive.join(path.trim_start_matches('/')))?;
    }
    // The `~/Library` subtree every Mac ships. `foundation::paths` creates
    // these too, for a guest that runs before anything made a prefix; listing
    // them here is what makes a fresh prefix complete on its own.
    for relative in USER_DIRECTORIES {
        std::fs::create_dir_all(drive.join("Users").join(&user).join(relative))?;
    }
    std::fs::create_dir_all(directory.join(CONTROL))?;
    // Written last: it is what [`created`] looks for, so a prefix that is
    // half-made is not one this will later mistake for finished.
    std::fs::write(state_path(directory), state_file())?;
    Ok(true)
}

/// Make any prefix-owned directory a prefix created by an older runtime lacks.
///
/// Only directories: they are cheap to check on every start, and a guest that
/// finds one missing fails in ways nothing traces back to the layout.
pub fn fill_in(directory: &Path) -> std::io::Result<()> {
    let drive = directory.join(DRIVE);
    for path in DIRECTORIES {
        std::fs::create_dir_all(drive.join(path.trim_start_matches('/')))?;
    }
    Ok(())
}

/// The contents of a new `state.yaml`.
///
/// Hand-written, and deliberately small: the fields that say which base a
/// prefix was made from belong to provisioning, and inventing empty ones now
/// would make a file that claims more than it knows.
fn state_file() -> String {
    // Written as separate lines rather than one continued literal: a `\` at the
    // end of a line in a Rust string eats the indentation on the next one, and
    // indentation is what makes this YAML.
    [
        "# Written by `rosemary prefix create`. See docs/prefixes.md.",
        "version: 1",
        "created_with:",
        &format!("  runtime_version: {}", env!("CARGO_PKG_VERSION")),
        "",
    ]
    .join("\n")
}

/// `$XDG_DATA_HOME/rosemary/prefixes/default`, or `$HOME/.local/share/...`.
pub fn default_directory(
    xdg_data_home: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(data) = xdg_data_home.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(data).join("rosemary/prefixes/default"));
    }
    let home = home.filter(|value| !value.is_empty())?;
    Some(PathBuf::from(home).join(".local/share/rosemary/prefixes/default"))
}

/// A prefix directory and the two paths inside it everything else is relative
/// to.
#[derive(Debug, Clone)]
pub struct Prefix {
    directory: PathBuf,
}

impl Prefix {
    pub fn at(directory: impl Into<PathBuf>) -> Prefix {
        Prefix {
            directory: directory.into(),
        }
    }

    /// The prefix directory itself.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Where the guest's `/dev/console` goes on a host with no console it may
    /// write, and where its launchd's own output goes.
    pub fn console(&self) -> PathBuf {
        self.directory.join("console.log")
    }

    /// The guest's `/`.
    pub fn drive(&self) -> PathBuf {
        self.directory.join(DRIVE)
    }

    /// Where `state.yaml`, `spec.yaml` and the manifest live.
    pub fn control(&self) -> PathBuf {
        self.directory.join(CONTROL)
    }
}

/// This user's login name, which is what a guest's `/Users/<name>` is called.
///
/// `getpwuid` first, because that is what Darwin's own `NSUserName` answers
/// from and a guest comparing the two would otherwise disagree with itself.
pub fn user_name() -> String {
    // SAFETY: `getpwuid` returns a pointer into a static buffer or null, and
    // `pw_name` is a NUL-terminated string owned by that buffer.
    unsafe {
        let passwd = libc::getpwuid(libc::geteuid());
        if !passwd.is_null()
            && !(*passwd).pw_name.is_null()
            && let Ok(name) = std::ffi::CStr::from_ptr((*passwd).pw_name).to_str()
            && !name.is_empty()
        {
            return name.to_string();
        }
    }
    std::env::var("USER")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "user".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{Difference, Scope, compare};

    #[test]
    fn the_variable_can_turn_the_prefix_off() {
        for spelling in ["off", "0", ""] {
            assert_eq!(setting_from(Some(OsString::from(spelling))), Setting::Off);
        }
    }

    /// A named prefix is used whether or not it has been created: the caller
    /// asked for that one, and silently running without it would be worse than
    /// the error it is about to get.
    #[test]
    fn the_variable_names_a_directory() {
        assert_eq!(
            setting_from(Some(OsString::from("/prefixes/game"))),
            Setting::At(PathBuf::from("/prefixes/game"))
        );
    }

    /// Unset is off. What to do when nobody said is the command's policy, not
    /// this crate's - see [`setting_from`].
    #[test]
    fn an_unset_variable_leaves_the_namespace_alone() {
        assert_eq!(setting_from(None), Setting::Off);
    }

    #[test]
    fn a_fresh_prefix_has_the_shape_a_mac_program_expects() {
        let at =
            std::env::temp_dir().join(format!("rosemary-prefix-create-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);

        assert!(create(&at).expect("a prefix is made"));
        assert!(created(&at), "the state file says so");
        let drive = at.join(DRIVE);
        for relative in [
            "Applications",
            "System/Library",
            "usr/lib",
            "private/tmp",
            "private/var/tmp",
        ] {
            assert!(drive.join(relative).is_dir(), "{relative} is missing");
        }
        assert!(
            drive
                .join("Users")
                .join(user_name())
                .join("Library/Application Support")
                .is_dir()
        );
        // Making one twice is not an error and does not write again.
        assert!(!create(&at).expect("the second call is a no-op"));

        let _ = std::fs::remove_dir_all(&at);
    }

    /// A prefix made before `/private/var/tmp` was part of the layout gains it
    /// on the next start, through the `/var` symlink a guest names it by.
    #[test]
    fn an_older_prefix_gains_missing_layout_directories() {
        let at = crate::verify::tests::scratch("layout-fill-in");
        create(&at).unwrap();
        let var_tmp = at.join(DRIVE).join("private/var/tmp");
        std::fs::remove_dir(&var_tmp).unwrap();

        fill_in(&at).unwrap();
        assert!(var_tmp.is_dir());
        assert!(at.join(DRIVE).join("var/tmp").is_dir());
    }

    #[test]
    fn layout_damage_is_verified_and_repaired_with_the_base() {
        let at = crate::verify::tests::scratch("layout-repair");
        let prefix = Prefix::at(at.join("prefix"));
        let base = at.join("base");
        std::fs::create_dir(&base).unwrap();
        let manifest = crate::verify::tests::materialise_sample(&base);
        create(prefix.directory()).unwrap();
        let expected = expected_manifest(&manifest);
        let root = prefix.drive();
        let initial = compare(&root, &expected, true, &Scope::All).unwrap();
        crate::repair::apply(&prefix, &root, &expected, &base, &initial, false).unwrap();

        std::fs::remove_dir(root.join("Applications")).unwrap();
        std::fs::remove_file(root.join("tmp")).unwrap();
        std::os::unix::fs::symlink("elsewhere", root.join("tmp")).unwrap();
        std::fs::write(root.join("Library/my-file"), b"user data").unwrap();
        let damage = compare(&root, &expected, true, &Scope::All).unwrap();
        assert!(
            damage
                .iter()
                .any(|p| { p.path == "/Applications" && p.kind == Difference::Missing })
        );
        assert!(
            damage
                .iter()
                .any(|p| { p.path == "/tmp" && p.kind == Difference::Modified })
        );
        let reset_damage = compare(&root, &expected, false, &Scope::All).unwrap();
        assert!(
            crate::repair::strays(&reset_damage, &manifest)
                .iter()
                .all(|p| p.path != "/Library/my-file")
        );
        crate::repair::apply(&prefix, &root, &expected, &base, &damage, false).unwrap();
        assert_eq!(compare(&root, &expected, true, &Scope::All).unwrap(), []);
        assert_eq!(
            std::fs::read(root.join("Library/my-file")).unwrap(),
            b"user data"
        );
        assert_eq!(
            std::fs::read_link(root.join("tmp")).unwrap(),
            Path::new("private/tmp")
        );
        let _ = std::fs::remove_dir_all(&at);
    }

    #[test]
    fn the_default_directory_prefers_xdg_over_home() {
        assert_eq!(
            default_directory(
                Some(OsString::from("/data")),
                Some(OsString::from("/home/user"))
            ),
            Some(PathBuf::from("/data/rosemary/prefixes/default"))
        );
        assert_eq!(
            default_directory(Some(OsString::new()), Some(OsString::from("/home/user"))),
            Some(PathBuf::from(
                "/home/user/.local/share/rosemary/prefixes/default"
            ))
        );
        assert_eq!(default_directory(None, None), None);
    }

    #[test]
    fn a_prefix_knows_its_drive_and_control_directories() {
        let prefix = Prefix::at("/prefixes/game");
        assert_eq!(prefix.drive(), PathBuf::from("/prefixes/game/root"));
        assert_eq!(prefix.control(), PathBuf::from("/prefixes/game/.rosemary"));
    }
}
