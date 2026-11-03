use std::path::{Component, Path, PathBuf};

/// One guest directory and the host directory it is.
#[derive(Debug, Clone)]
pub struct Mount {
    /// An absolute, normalised guest path, without a trailing separator except
    /// for the root mount, which is exactly `/`.
    guest: String,
    host: PathBuf,
    /// Whether a listing of the guest parent shows this mount.
    listed: bool,
}

/// The mounts, ordered so that the first match is the longest one.
///
/// Two orders are kept rather than one sorted list searched twice, because the
/// longest *guest* path and the longest *host* path are not the same mount:
/// `/Volumes/Host` is a long guest path onto the shortest host path there is.
#[derive(Debug, Clone, Default)]
pub struct Table {
    by_guest: Vec<Mount>,
    by_host: Vec<Mount>,
}

impl Table {
    /// The empty table: a guest path is a host path.
    pub fn identity() -> Table {
        Table::default()
    }

    /// The table for a prefix whose guest `/` is `root`.
    ///
    /// `host_home` mounts the guest's `/Users/<user>` onto a host directory
    /// instead of the prefix's own; `None` keeps the prefix self-contained.
    /// Passed in rather than read from `$HOME` here, because a process-wide
    /// table is built once and a test that set the variable could not observe
    /// it.
    pub fn rooted(root: &Path, user: &str, host_home: Option<&Path>) -> Table {
        Table::rooted_with_files(root, user, host_home, &[])
    }

    /// As [`Table::rooted`], with each guest path in `files` a single host
    /// file the prefix provides rather than one in its root.
    pub fn rooted_with_files(
        root: &Path,
        user: &str,
        host_home: Option<&Path>,
        files: &[(&str, &Path)],
    ) -> Table {
        let mut mounts = vec![
            Mount {
                guest: "/".to_string(),
                host: root.to_path_buf(),
                listed: true,
            },
            Mount {
                guest: "/dev".to_string(),
                host: PathBuf::from("/dev"),
                listed: true,
            },
            Mount {
                guest: "/private/etc/resolv.conf".to_string(),
                host: PathBuf::from("/etc/resolv.conf"),
                listed: true,
            },
            // Unlisted, so a program that walks `/Volumes` - a backup tool, a
            // "clean up old files" pass, a recursive delete gone wrong - never
            // wanders onto the host's whole filesystem.
            Mount {
                guest: "/Volumes/Host".to_string(),
                host: PathBuf::from("/"),
                listed: false,
            },
        ];
        mounts.extend(files.iter().map(|(guest, host)| Mount {
            guest: guest.to_string(),
            host: host.to_path_buf(),
            listed: true,
        }));
        if let Some(host_home) = host_home {
            mounts.push(Mount {
                guest: format!("/Users/{user}"),
                host: host_home.to_path_buf(),
                listed: true,
            });
        }
        let mut table = Table::new(mounts);
        table.alias_through_root_links(root);
        table
    }

    /// Give each mount a guest spelling through every top-level symlink of the
    /// root that leads to it.
    ///
    /// A Mac's `/etc`, `/tmp` and `/var` are links into `/private`. Mapped
    /// lexically, `/etc/resolv.conf` is a path in the prefix that the host
    /// kernel resolves inside the prefix, never reaching a mount at
    /// `/private/etc/resolv.conf`.
    fn alias_through_root_links(&mut self, root: &Path) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        let mut aliases = Vec::new();
        for entry in entries.flatten() {
            let (Ok(target), Some(name)) = (
                std::fs::read_link(entry.path()),
                entry.file_name().to_str().map(str::to_owned),
            ) else {
                continue;
            };
            let Some(target) = target.to_str().and_then(|t| normalise(&format!("/{t}"))) else {
                continue;
            };
            if target.contains("/..") {
                continue;
            }
            for mount in &self.by_guest {
                if mount.guest != "/" && under(&mount.guest, &target) {
                    aliases.push(Mount {
                        guest: format!("/{name}{}", &mount.guest[target.len()..]),
                        ..mount.clone()
                    });
                }
            }
        }
        self.by_guest.extend(aliases);
        self.by_guest
            .sort_by_key(|m| std::cmp::Reverse(m.guest.len()));
    }

    /// A table of the root and exactly the mounts given, for tests that need
    /// a mount target they made themselves.
    #[cfg(test)]
    pub(super) fn with<'a>(
        root: &Path,
        mounts: impl IntoIterator<Item = (&'a str, PathBuf, bool)>,
    ) -> Table {
        let root = Mount {
            guest: "/".to_string(),
            host: root.to_path_buf(),
            listed: true,
        };
        let rest = mounts.into_iter().map(|(guest, host, listed)| Mount {
            guest: guest.to_string(),
            host,
            listed,
        });
        Table::new(std::iter::once(root).chain(rest).collect())
    }

    fn new(mounts: Vec<Mount>) -> Table {
        let mut by_guest = mounts.clone();
        by_guest.sort_by_key(|m| std::cmp::Reverse(m.guest.len()));
        let mut by_host = mounts.clone();
        // getcwd and descriptor paths may resolve symlinked host directories.
        // Keep both spellings so /var and /private/var name the same guest path.
        for mount in mounts {
            if let Ok(host) = std::fs::canonicalize(&mount.host)
                && host != mount.host
            {
                by_host.push(Mount { host, ..mount });
            }
        }
        by_host.sort_by_key(|m| std::cmp::Reverse(m.host.as_os_str().len()));
        Table { by_guest, by_host }
    }

    pub fn is_identity(&self) -> bool {
        self.by_guest.is_empty()
    }

    /// The host path `guest` names, or `None` if no mount claims it.
    ///
    /// `None` for a relative path: it is resolved against the working
    /// directory, which is already a host directory inside the prefix, so
    /// rewriting it would apply the prefix twice.
    pub fn to_host(&self, guest: &str) -> Option<PathBuf> {
        let normalised = normalise(guest)?;
        let mount = self
            .by_guest
            .iter()
            .find(|m| under(&normalised, &m.guest))?;
        Some(join(
            &mount.host,
            &normalised[trimmed(&mount.guest).len()..],
        ))
    }

    /// Every mount point but the root, as the directory listing it sits in
    /// sees it.
    ///
    /// The host directory behind a mount point is not inside the directory the
    /// guest lists, so a listing has to be told to add it - or, for an
    /// unlisted mount, to leave out whatever the prefix has at that name.
    pub fn mount_points(&self) -> impl Iterator<Item = MountPoint<'_>> {
        self.by_guest.iter().filter_map(|m| {
            let (parent, name) = m.guest.rsplit_once('/')?;
            (!name.is_empty()).then_some(MountPoint {
                parent: match parent {
                    "" => "/",
                    parent => parent,
                },
                name,
                host: &m.host,
                listed: m.listed,
            })
        })
    }

    /// The guest path that names `host`, or `None` if no mount reaches it.
    ///
    /// `None` is a refusal, not a fallback: a host path no mount reaches is one
    /// the guest was never supposed to see, and handing it over unchanged would
    /// be the leak this table is here to prevent.
    pub fn to_guest(&self, host: &Path) -> Option<PathBuf> {
        let host = host.to_str()?;
        let normalised = normalise(host)?;
        let mount = self
            .by_host
            .iter()
            .find(|m| m.host.to_str().is_some_and(|root| under(&normalised, root)))?;
        let root = mount.host.to_str()?;
        let rest = &normalised[trimmed(root).len()..];
        Some(join(Path::new(&mount.guest), rest))
    }
}

/// One mount point, split the way a listing of its parent needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountPoint<'a> {
    /// The guest directory the mount point is an entry of.
    pub parent: &'a str,
    /// The entry's name in that directory.
    pub name: &'a str,
    /// What the entry is.
    pub host: &'a Path,
    /// Whether the entry is shown at all.
    pub listed: bool,
}

/// `path` lexically normalised: absolute, no `.`, no repeated or trailing
/// separators, `None` for anything else.
///
/// `..` is *not* resolved away. A guest's `..` means what the filesystem says
/// it means once symlinks are involved, and a table that collapsed it lexically
/// would answer about a different directory than the kernel would.
fn normalise(path: &str) -> Option<String> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return None;
    }
    let mut out = String::new();
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => continue,
            Component::Prefix(_) => return None,
            Component::ParentDir => {
                out.push('/');
                out.push_str("..");
            }
            Component::Normal(name) => {
                out.push('/');
                out.push_str(name.to_str()?);
            }
        }
    }
    Some(match out.is_empty() {
        true => "/".to_string(),
        false => out,
    })
}

/// Whether the normalised `path` is `root` or something inside it.
///
/// Whole components only, so `/dev` does not claim `/device`.
fn under(path: &str, root: &str) -> bool {
    let root = trimmed(root);
    if root.is_empty() {
        return path.starts_with('/');
    }
    path == root || (path.starts_with(root) && path.as_bytes().get(root.len()) == Some(&b'/'))
}

/// `root` without the trailing separator a root path has and nothing else does.
fn trimmed(root: &str) -> &str {
    match root {
        "/" => "",
        other => other.trim_end_matches('/'),
    }
}

/// `root` with `rest`, which is either empty or begins with a separator.
fn join(root: &Path, rest: &str) -> PathBuf {
    match rest.strip_prefix('/') {
        Some(relative) if !relative.is_empty() => root.join(relative),
        _ => root.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> Table {
        Table::rooted(Path::new("/prefixes/default/root"), "user", None)
    }

    #[test]
    fn an_identity_table_claims_nothing() {
        let identity = Table::identity();
        assert!(identity.is_identity());
        assert_eq!(identity.to_host("/usr/lib/libSystem.B.dylib"), None);
        assert_eq!(identity.to_guest(Path::new("/usr/lib")), None);
    }

    #[test]
    fn a_darwin_path_lands_under_the_prefix_root() {
        assert_eq!(
            table().to_host("/usr/lib/libSystem.B.dylib"),
            Some(PathBuf::from(
                "/prefixes/default/root/usr/lib/libSystem.B.dylib"
            ))
        );
        assert_eq!(
            table().to_host("/"),
            Some(PathBuf::from("/prefixes/default/root"))
        );
    }

    /// The longest guest match wins, not the first one listed.
    #[test]
    fn a_longer_mount_beats_the_root() {
        assert_eq!(
            table().to_host("/tmp/x"),
            Some(PathBuf::from("/prefixes/default/root/tmp/x"))
        );
        assert_eq!(
            table().to_host("/dev/urandom"),
            Some(PathBuf::from("/dev/urandom"))
        );
    }

    /// A mount claims whole components. This is the bug a `starts_with` would
    /// have: `/dev` would swallow `/deuserper`.
    #[test]
    fn a_mount_does_not_claim_a_longer_name() {
        assert_eq!(
            table().to_host("/deuserper/x"),
            Some(PathBuf::from("/prefixes/default/root/deuserper/x"))
        );
    }

    /// Every host path has a guest spelling, which is what makes `to_guest`
    /// total - and what lets a guest run from outside its own prefix.
    #[test]
    fn a_host_path_outside_the_prefix_is_named_under_volumes() {
        assert_eq!(
            table().to_guest(Path::new("/home/user/Foo.app/Contents/MacOS/Foo")),
            Some(PathBuf::from(
                "/Volumes/Host/home/user/Foo.app/Contents/MacOS/Foo"
            ))
        );
        assert_eq!(
            table().to_host("/Volumes/Host/home/user/Foo.app"),
            Some(PathBuf::from("/home/user/Foo.app"))
        );
    }

    /// The prefix root is a longer *host* path than `/`, so it answers first
    /// and a file in the prefix is never named through the escape hatch.
    #[test]
    fn a_resolved_host_mount_keeps_its_guest_spelling() {
        let at = std::env::temp_dir().join(format!("rosemary-mount-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("real")).unwrap();
        std::os::unix::fs::symlink("real", at.join("alias")).unwrap();
        let table = Table::with(&at.join("alias"), []);
        let physical = std::fs::canonicalize(at.join("real")).unwrap();
        assert_eq!(
            table.to_guest(&physical.join("file")),
            Some(PathBuf::from("/file"))
        );
        assert_eq!(
            table.to_guest(&at.join("alias/file")),
            Some(PathBuf::from("/file"))
        );
        std::fs::remove_dir_all(at).unwrap();
    }

    #[test]
    fn a_path_inside_the_prefix_is_named_by_its_darwin_path() {
        assert_eq!(
            table().to_guest(Path::new("/prefixes/default/root/usr/lib")),
            Some(PathBuf::from("/usr/lib"))
        );
        assert_eq!(
            table().to_guest(Path::new("/prefixes/default/root/private/tmp/x")),
            Some(PathBuf::from("/private/tmp/x"))
        );
    }

    #[test]
    fn every_mount_round_trips() {
        let table = table();
        for guest in [
            "/usr/lib/libSystem.B.dylib",
            "/tmp/x",
            "/dev/null",
            "/Volumes/Host/etc/hosts",
            "/Applications",
        ] {
            let host = table.to_host(guest).expect("a host path");
            assert_eq!(
                table.to_guest(&host).as_deref(),
                Some(Path::new(guest)),
                "{guest}"
            );
        }
    }

    /// What `-bundlePath` and the rest of `NSBundle` depend on: a guest joins
    /// onto the path it was given to find its own files, so the spelling it is
    /// given has to be one that maps back to the same place.
    #[test]
    fn a_bundles_paths_survive_being_joined_onto() {
        let table = table();
        let host = Path::new("/home/user/Games/Foo.app");
        let guest = table.to_guest(host).expect("a guest spelling");
        assert_eq!(
            guest,
            PathBuf::from("/Volumes/Host/home/user/Games/Foo.app")
        );
        // The guest joins onto what it was given, and lands back on the file.
        assert_eq!(
            table.to_host(guest.join("Contents/Resources/game.ios").to_str().unwrap()),
            Some(PathBuf::from(
                "/home/user/Games/Foo.app/Contents/Resources/game.ios"
            ))
        );
    }

    /// A relative path is already resolved against a host working directory, so
    /// applying the prefix to it would apply it twice.
    #[test]
    fn a_relative_path_is_left_alone() {
        assert_eq!(table().to_host("usr/lib"), None);
        assert_eq!(table().to_host(""), None);
    }

    /// Lexical `..` collapsing would answer about a different directory than
    /// the kernel does once a symlink is in the way, so it is carried through.
    #[test]
    fn a_parent_component_is_carried_not_collapsed() {
        assert_eq!(
            table().to_host("/usr/lib/../lib"),
            Some(PathBuf::from("/prefixes/default/root/usr/lib/../lib"))
        );
    }

    /// `/etc` is a link to `private/etc`, so the guest's `/etc/resolv.conf`
    /// must reach the mount at `/private/etc/resolv.conf`, as on a Mac.
    #[test]
    fn a_mount_is_reached_through_a_root_symlink() {
        let at = std::env::temp_dir().join(format!("rosemary-mount-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(at.join("private/etc")).unwrap();
        std::os::unix::fs::symlink("private/etc", at.join("etc")).unwrap();
        let table = Table::rooted(&at, "user", None);
        assert_eq!(
            table.to_host("/etc/resolv.conf"),
            Some(PathBuf::from("/etc/resolv.conf"))
        );
        assert_eq!(table.to_host("/etc/hosts"), Some(at.join("etc/hosts")));
        assert_eq!(
            point(&table, "/etc/resolv.conf").map(|p| p.host),
            Some(Path::new("/etc/resolv.conf"))
        );
        assert_eq!(
            table.to_guest(Path::new("/etc/resolv.conf")),
            Some(PathBuf::from("/private/etc/resolv.conf")),
            "the canonical spelling names the host file"
        );
        std::fs::remove_dir_all(at).unwrap();
    }

    fn point<'a>(table: &'a Table, guest: &str) -> Option<MountPoint<'a>> {
        table
            .mount_points()
            .find(|p| format!("{}/{}", p.parent.trim_end_matches('/'), p.name) == guest)
    }

    #[test]
    fn a_mount_point_is_an_entry_of_its_guest_parent() {
        let table = table();
        assert_eq!(
            point(&table, "/dev"),
            Some(MountPoint {
                parent: "/",
                name: "dev",
                host: Path::new("/dev"),
                listed: true,
            })
        );
        assert_eq!(
            point(&table, "/private/etc/resolv.conf").map(|p| (p.parent, p.name)),
            Some(("/private/etc", "resolv.conf"))
        );
        // The root is the directory being listed, never an entry of one.
        assert!(table.mount_points().all(|p| !p.name.is_empty()));
    }

    /// The escape hatch is reachable by name and nowhere else.
    #[test]
    fn the_host_volume_is_not_listed() {
        let table = table();
        let host = point(&table, "/Volumes/Host").expect("the host mount");
        assert_eq!(
            (host.parent, host.name, host.listed),
            ("/Volumes", "Host", false)
        );
        assert_eq!(
            table.to_host("/Volumes/Host/etc"),
            Some(PathBuf::from("/etc"))
        );
    }

    #[test]
    fn a_host_home_mount_replaces_the_prefixs_own() {
        let table = Table::rooted(
            Path::new("/prefixes/default/root"),
            "user",
            Some(Path::new("/home/user")),
        );
        assert_eq!(
            table.to_host("/Users/user/Documents"),
            Some(PathBuf::from("/home/user/Documents"))
        );
        assert_eq!(
            point(&table, "/Users/user").map(|p| (p.parent, p.name, p.listed)),
            Some(("/Users", "user", true))
        );
        // ...and another user's home is still the prefix's.
        assert_eq!(
            table.to_host("/Users/someone/Documents"),
            Some(PathBuf::from(
                "/prefixes/default/root/Users/someone/Documents"
            ))
        );
    }

    #[test]
    fn a_provided_file_shadows_the_prefixs_own() {
        let table = Table::rooted_with_files(
            Path::new("/prefixes/default/root"),
            "user",
            None,
            &[(
                "/System/Library/CoreServices/SystemVersion.plist",
                Path::new("/prefixes/default/.rosemary/system-version/x.plist"),
            )],
        );
        assert_eq!(
            table.to_host("/System/Library/CoreServices/SystemVersion.plist"),
            Some(PathBuf::from(
                "/prefixes/default/.rosemary/system-version/x.plist"
            ))
        );
        assert_eq!(
            table.to_host("/System/Library/CoreServices/Finder.app"),
            Some(PathBuf::from(
                "/prefixes/default/root/System/Library/CoreServices/Finder.app"
            ))
        );
    }
}
