//! The first process of a packaged Rosemary.
//!
//! A bundle carries its own glibc because the libraries in it were built
//! against a newer one than many hosts have. Using the bundled glibc on a host
//! that has a perfectly good one is worse, though: the host's graphics and
//! audio drivers, NSS modules and the like are loaded into this process, and
//! they were built for the host's glibc. So this picks: the host's when it is
//! new enough for everything in the bundle, the bundle's otherwise.
//!
//! Layout, relative to the root (the directory above this binary's own):
//!
//! ```text
//! bin/rosemary                   this program
//! libexec/rosemary               the real binary
//! lib/                           vendored libraries
//! lib-fallback/                  libraries the host should supply; used only if it does not
//! glibc/lib/ld-linux-*.so.*      vendored glibc, and its libc.so.6 and kin
//! share/rosemary/bundle.conf     see `conf`
//! ```
//!
//! Built static against musl, so it runs wherever the host glibc is too old
//! (or not glibc at all).

mod conf;
mod glibc;

use conf::Conf;
use std::env;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// `host`, `bundled` or `auto` (the default).
const CHOICE_VAR: &str = "ROSEMARY_GLIBC";
/// Set to anything to have the launcher say what it chose, and why.
const TRACE_VAR: &str = "ROSEMARY_LAUNCHER_TRACE";
/// What was chosen, for the real binary and for anyone debugging it.
const SELECTED_VAR: &str = "ROSEMARY_GLIBC_SELECTED";
/// The launcher's own path: the thing to run to get another Rosemary. Under
/// the bundled loader the kernel reports the loader as the executable.
const EXE_VAR: &str = "ROSEMARY_EXE";
const ROOT_VAR: &str = "ROSEMARY_BUNDLE_ROOT";
/// `LD_LIBRARY_PATH` as it was before the bundle added to it, for children
/// that should not inherit the bundle's libraries.
const HOST_LIBRARY_PATH_VAR: &str = "ROSEMARY_HOST_LD_LIBRARY_PATH";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Host,
    Bundled,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Host => "host",
            Mode::Bundled => "bundled",
        }
    }
}

struct Layout {
    root: PathBuf,
}

impl Layout {
    fn real(&self) -> PathBuf {
        self.root.join("libexec/rosemary")
    }
    fn libs(&self) -> PathBuf {
        self.root.join("lib")
    }
    fn fallback_libs(&self) -> PathBuf {
        self.root.join("lib-fallback")
    }
    fn glibc_libs(&self) -> PathBuf {
        self.root.join("glibc/lib")
    }
    fn loader(&self) -> Option<PathBuf> {
        let name = Path::new(glibc::HOST_LOADER).file_name()?;
        Some(self.glibc_libs().join(name))
    }
    fn conf(&self) -> PathBuf {
        self.root.join("share/rosemary/bundle.conf")
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(never) => match never {},
        Err(message) => {
            eprintln!("rosemary: {message}");
            ExitCode::from(127)
        }
    }
}

fn run() -> Result<std::convert::Infallible, String> {
    // Resolved by the kernel, so an `AppRun` symlink or a Steam tool directory
    // reached through a link still finds the real tree.
    let exe = std::fs::read_link("/proc/self/exe").map_err(|e| format!("/proc/self/exe: {e}"))?;
    let root = exe
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("{} is not inside a bundle", exe.display()))?;
    let layout = Layout {
        root: root.to_path_buf(),
    };
    let conf = Conf::load(&layout.conf());

    let mut args = env::args_os();
    let argv0 = args.next().unwrap_or_else(|| OsString::from("rosemary"));
    let args: Vec<OsString> = args.collect();

    let (mode, why) = choose(&layout, &conf);
    if env::var_os(TRACE_VAR).is_some() {
        eprintln!(
            "rosemary-launcher: {} glibc ({why}); root {}",
            mode.name(),
            layout.root.display()
        );
    }
    prepare_environment(&layout, &conf, &exe, mode);

    let real = layout.real();
    let mut command = match mode {
        Mode::Host => Command::new(&real),
        Mode::Bundled => {
            let loader = layout
                .loader()
                .ok_or("no dynamic loader for this architecture")?;
            let mut command = Command::new(loader);
            command.arg("--argv0").arg(&argv0).arg(&real);
            command
        }
    };
    if mode == Mode::Host {
        command.arg0(&argv0);
    }
    let error = command.args(&args).exec();
    Err(format!(
        "cannot run {} ({} glibc): {error}",
        real.display(),
        mode.name()
    ))
}

/// The mode, and a sentence saying why, for `ROSEMARY_LAUNCHER_TRACE`.
fn choose(layout: &Layout, conf: &Conf) -> (Mode, String) {
    let bundle_has_glibc = layout.loader().is_some_and(|loader| loader.exists());
    match env::var(CHOICE_VAR).as_deref() {
        Ok("host") => return (Mode::Host, format!("{CHOICE_VAR}=host")),
        Ok("bundled") if bundle_has_glibc => {
            return (Mode::Bundled, format!("{CHOICE_VAR}=bundled"));
        }
        _ => {}
    }
    // A plain FHS install has no glibc of its own to fall back on.
    if !bundle_has_glibc {
        return (Mode::Host, "the tree has no glibc of its own".into());
    }
    let required = conf.required_glibc;
    let host = Path::new(glibc::HOST_LOADER)
        .exists()
        .then(glibc::host_version)
        .flatten();
    let facts = format!(
        "host glibc {}, bundle needs {}",
        host.map_or("none".into(), |v| v.to_string()),
        required.map_or("unknown".into(), |v| v.to_string()),
    );
    match (required, host) {
        (Some(required), Some(host)) if host >= required => (Mode::Host, facts),
        // Too old, not glibc, or no way to tell what the bundle needs.
        _ => (Mode::Bundled, facts),
    }
}

fn prepare_environment(layout: &Layout, conf: &Conf, exe: &Path, mode: Mode) {
    // A relaunch from inside a Rosemary starts from the original value, not
    // from the bundle's additions to it.
    let original = env::var_os(HOST_LIBRARY_PATH_VAR).or_else(|| env::var_os("LD_LIBRARY_PATH"));
    let path = library_path(layout, original.as_deref(), mode);
    // SAFETY: single-threaded: nothing has been spawned, and `exec` follows.
    unsafe {
        env::set_var("LD_LIBRARY_PATH", path);
        if let Some(original) = &original {
            env::set_var(HOST_LIBRARY_PATH_VAR, original);
        }
        env::set_var(SELECTED_VAR, mode.name());
        env::set_var(EXE_VAR, exe);
        env::set_var(ROOT_VAR, &layout.root);
        let places = Placeholders {
            root: layout.root.as_os_str(),
            cache: cache_dir(),
        };
        for (name, value) in &conf.default_env {
            if env::var_os(name).is_none()
                && let Some(value) = expand(value, &places)
            {
                env::set_var(name, value);
            }
        }
        for (name, value) in &conf.env {
            match expand(value, &places) {
                Some(value) => env::set_var(name, value),
                None => env::remove_var(name),
            }
        }
        for (name, candidates) in &conf.default_path {
            if env::var_os(name).is_none()
                && let Some(found) = first_existing(candidates)
            {
                env::set_var(name, found);
            }
        }
        if mode == Mode::Bundled {
            // The bundled glibc looks for its converters in the store path it
            // was built at.
            let gconv = layout.glibc_libs().join("gconv");
            if env::var_os("GCONV_PATH").is_none() && gconv.is_dir() {
                env::set_var("GCONV_PATH", gconv);
            }
        }
    }
}

/// The bundle's libraries first, then whatever the caller had, then the
/// host's own directories, then the bundle's fallbacks.
///
/// The host's directories are listed explicitly where the loader would not
/// search them itself: the bundled glibc's loader reads no `ld.so.cache`, and
/// the fallbacks must come after the host's libraries, which `LD_LIBRARY_PATH`
/// would otherwise shadow.
fn library_path(layout: &Layout, original: Option<&OsStr>, mode: Mode) -> OsString {
    let fallback = layout.fallback_libs();
    let has_fallback = fallback.is_dir();
    let mut dirs: Vec<PathBuf> = vec![layout.libs()];
    if mode == Mode::Bundled {
        dirs.push(layout.glibc_libs());
    }
    if let Some(original) = original {
        dirs.extend(env::split_paths(original).filter(|dir| !dir.as_os_str().is_empty()));
    }
    if mode == Mode::Bundled || has_fallback {
        dirs.extend(host_library_dirs());
    }
    if has_fallback {
        dirs.push(fallback);
    }
    let mut seen = Vec::new();
    dirs.retain(|dir| {
        let fresh = !seen.contains(dir);
        seen.push(dir.clone());
        fresh
    });
    env::join_paths(dirs).unwrap_or_default()
}

fn first_existing(candidates: &[String]) -> Option<&str> {
    candidates
        .iter()
        .map(String::as_str)
        .find(|candidate| Path::new(candidate).exists())
}

/// What `@ROOT@` and `@CACHE@` in `bundle.conf` stand for.
struct Placeholders<'a> {
    root: &'a OsStr,
    cache: Option<PathBuf>,
}

/// `value` with its placeholders filled in, or `None` if it needs one that
/// has no value here.
fn expand(value: &str, places: &Placeholders) -> Option<OsString> {
    let tokens: [(&[u8], Option<&OsStr>); 2] = [
        (b"@ROOT@", Some(places.root)),
        (b"@CACHE@", places.cache.as_deref().map(Path::as_os_str)),
    ];
    let mut out = Vec::new();
    let mut rest = value.as_bytes();
    'scan: while !rest.is_empty() {
        for (token, replacement) in tokens {
            if let Some(after) = rest.strip_prefix(token) {
                out.extend_from_slice(replacement?.as_bytes());
                rest = after;
                continue 'scan;
            }
        }
        out.push(rest[0]);
        rest = &rest[1..];
    }
    Some(OsString::from_vec(out))
}

/// The XDG cache directory: `$XDG_CACHE_HOME`, else `~/.cache`.
fn cache_dir() -> Option<PathBuf> {
    let absolute = |dir: OsString| Some(PathBuf::from(dir)).filter(|dir| dir.is_absolute());
    env::var_os("XDG_CACHE_HOME")
        .and_then(absolute)
        .or_else(|| {
            env::var_os("HOME")
                .and_then(absolute)
                .map(|home| home.join(".cache"))
        })
}

/// NixOS and NixGL search dirs
const DRIVER_DIRS: &[&str] = &["/run/opengl-driver/lib"];

fn host_library_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    read_ld_conf(Path::new("/etc/ld.so.conf"), &mut dirs, 0);
    dirs.extend(glibc::LIBRARY_DIRS.iter().map(PathBuf::from));
    dirs.extend(DRIVER_DIRS.iter().map(PathBuf::from));
    dirs.retain(|dir| dir.is_dir());
    dirs
}

fn read_ld_conf(path: &Path, dirs: &mut Vec<PathBuf>, depth: usize) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(pattern) = line.strip_prefix("include") {
            if depth < 4 {
                include(pattern.trim(), dirs, depth);
            }
        } else if line.starts_with('/') {
            dirs.push(PathBuf::from(line));
        }
    }
}

fn include(pattern: &str, dirs: &mut Vec<PathBuf>, depth: usize) {
    let pattern = Path::new(pattern);
    let (Some(parent), Some(name)) = (
        pattern.parent(),
        pattern.file_name().and_then(OsStr::to_str),
    ) else {
        return;
    };
    let suffix = name.strip_prefix('*');
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(
            |path| match (suffix, path.file_name().and_then(OsStr::to_str)) {
                (Some(suffix), Some(found)) => found.ends_with(suffix),
                (None, Some(found)) => found == name,
                _ => false,
            },
        )
        .collect();
    files.sort();
    for file in files {
        read_ld_conf(&file, dirs, depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(root: &Path) -> Layout {
        Layout {
            root: root.to_path_buf(),
        }
    }

    #[test]
    fn bundled_path_puts_the_bundle_before_the_original_before_the_host() {
        let l = layout(Path::new("/b"));
        let path = library_path(&l, Some(OsStr::new("/steam/a:/steam/b")), Mode::Bundled);
        let dirs: Vec<_> = env::split_paths(&path).collect();
        assert_eq!(
            dirs[..4],
            [
                PathBuf::from("/b/lib"),
                PathBuf::from("/b/glibc/lib"),
                PathBuf::from("/steam/a"),
                PathBuf::from("/steam/b"),
            ]
        );
    }

    #[test]
    fn host_path_leaves_glibc_and_host_directories_alone() {
        let l = layout(Path::new("/b"));
        let path = library_path(&l, Some(OsStr::new("/x")), Mode::Host);
        assert_eq!(
            env::split_paths(&path).collect::<Vec<_>>(),
            [PathBuf::from("/b/lib"), PathBuf::from("/x")]
        );
    }

    #[test]
    fn fallbacks_come_after_everything_the_host_has() {
        let dir = std::env::temp_dir().join(format!("launcher-fallback-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("lib-fallback")).unwrap();
        let l = layout(&dir);
        let path = library_path(&l, Some(OsStr::new("/x")), Mode::Host);
        let dirs: Vec<_> = env::split_paths(&path).collect();
        assert_eq!(dirs.first(), Some(&dir.join("lib")));
        assert_eq!(dirs.get(1), Some(&PathBuf::from("/x")));
        assert_eq!(dirs.last(), Some(&dir.join("lib-fallback")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_repeated_directory_is_listed_once() {
        let l = layout(Path::new("/b"));
        let path = library_path(&l, Some(OsStr::new("/b/lib:/x:/x:")), Mode::Host);
        assert_eq!(
            env::split_paths(&path).collect::<Vec<_>>(),
            [PathBuf::from("/b/lib"), PathBuf::from("/x")]
        );
    }

    #[test]
    fn a_tree_without_glibc_is_always_host() {
        let dir = std::env::temp_dir().join(format!("launcher-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let conf = Conf {
            required_glibc: glibc::Version::parse("99.0"),
            ..Conf::default()
        };
        assert_eq!(choose(&layout(&dir), &conf).0, Mode::Host);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn picks_the_first_candidate_that_exists() {
        let candidates = [
            "/nonexistent/a".to_string(),
            "/".to_string(),
            "/proc".to_string(),
        ];
        assert_eq!(first_existing(&candidates), Some("/"));
        assert_eq!(first_existing(&candidates[..1]), None);
    }

    #[test]
    fn expands_the_root_everywhere_it_appears() {
        let places = Placeholders {
            root: OsStr::new("/r"),
            cache: None,
        };
        assert_eq!(
            expand("@ROOT@/a:@ROOT@/b", &places),
            Some(OsString::from("/r/a:/r/b"))
        );
        assert_eq!(expand("plain", &places), Some(OsString::from("plain")));
    }

    #[test]
    fn a_value_needing_an_unknown_cache_directory_is_not_set() {
        let mut places = Placeholders {
            root: OsStr::new("/r"),
            cache: None,
        };
        assert_eq!(expand("@CACHE@/x", &places), None);
        places.cache = Some(PathBuf::from("/c"));
        assert_eq!(
            expand("@CACHE@/x:@ROOT@", &places),
            Some(OsString::from("/c/x:/r"))
        );
    }
}
