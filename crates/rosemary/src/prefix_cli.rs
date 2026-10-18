use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use appkit::dialog::RichText;
use prefix::Prefix;
use prefix::cache::Cache;
use prefix::format::spec::Policy;
use prefix::provision::{self, Answer, Chosen, Error, Options, Provisioned, Request};
use prefix::state::{State, Trust};
use prefix::system::{self, SystemFiles};
use prefix::verify::{self, Difference, Problem, Scope};

const USAGE: &str = "\
usage: rosemary prefix create [PREFIX] [--arch ARCH] [--spec FILE] [--bundle FILE]
                              [--system-files repair|warn|immutable] [--no-base]
       rosemary prefix info [PREFIX]
       rosemary prefix verify [PREFIX]
       rosemary prefix repair [PREFIX] [--dry-run]
       rosemary prefix reset-system [PREFIX] [--dry-run]
       rosemary prefix update [PREFIX] [--allow-downgrade]
       rosemary prefix cache list|gc";

/// The prefix directory this command uses when nobody named one.
///
/// `$XDG_DATA_HOME/rosemary/prefixes/default`, falling back to
/// `$HOME/.local/share/...`.
pub fn default_prefix() -> Option<PathBuf> {
    prefix::layout::default_directory(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

/// What `ROSEMARY_PREFIX_SETUP` says to do when the base has to be downloaded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Setup {
    Ask,
    Download,
    Fail,
}

fn setup() -> Setup {
    match std::env::var("ROSEMARY_PREFIX_SETUP").as_deref() {
        Ok("download") => Setup::Download,
        Ok("fail") => Setup::Fail,
        _ => Setup::Ask,
    }
}

/// Ask whether to download, the way this process can: a dialog when `gui` and
/// a display allow one, then the terminal, else no.
fn asker(gui: bool) -> impl FnMut(&Request) -> Answer {
    move |request: &Request| match setup() {
        Setup::Download => Answer::Download,
        Setup::Fail => Answer::Decline,
        Setup::Ask => {
            if gui && let Some(answer) = ask_with_dialog(request) {
                return answer;
            }
            if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
                return ask_on_terminal(request);
            }
            eprintln!(
                "rosemary: couldn't show a download dialog for the Darwin base system. \
                Run `rosemary prefix create`, or set ROSEMARY_PREFIX_SETUP=download."
            );
            Answer::Decline
        }
    }
}

fn download_size(bytes: Option<u64>) -> String {
    bytes
        .map(|b| format!("{:.1} MB", b as f64 / 1e6))
        .unwrap_or_else(|| "an unknown amount".into())
}

/// `source` added to `text`: a link when it is a web address, which the
/// dialog can open.
fn with_source(text: RichText, source: Option<&str>) -> RichText {
    match source {
        Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
            text.link(url, url)
        }
        Some(other) => text.code(other),
        None => text.plain("(no source)"),
    }
}

const VERIFIED_AND_CACHED: &str = "It is checked against hashes built into Rosemary before \
    anything uses it, and kept in a cache so it never has to be downloaded again.";

fn question(request: &Request) -> String {
    let size = download_size(request.bytes);
    format!(
        "Rosemary needs a Darwin base system:\n  {}\n\n\
         This will download {size} from\n  {}\n\n{VERIFIED_AND_CACHED}",
        request.spec.describe(),
        request.sources.first().copied().unwrap_or("(no source)"),
    )
}

fn ask_with_dialog(request: &Request) -> Option<Answer> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return None;
    }
    let informative = RichText::new()
        .plain("Rosemary needs a Darwin base system:\n")
        .bold(request.spec.describe())
        .plain(format!(
            "\n\nThis will download {} from\n",
            download_size(request.bytes)
        ));
    let informative = with_source(informative, request.sources.first().copied())
        .plain(format!(
            "\n\n{VERIFIED_AND_CACHED}\n\nTo use a MiniDarwin bundle you already have \
             instead, choose Not Now and run\n"
        ))
        .code("rosemary prefix create --bundle FILE");
    let request = appkit::dialog::Request {
        title: "Rosemary".to_string(),
        message: "Download the Darwin base system?".to_string(),
        rich_informative: Some(informative),
        buttons: vec!["Download".to_string(), "Not Now".to_string()],
        cancel: Some(1),
        application_icon: false,
        ..Default::default()
    };
    Some(match appkit::dialog::run(&request)? {
        0 => Answer::Download,
        _ => Answer::Decline,
    })
}

fn ask_on_terminal(request: &Request) -> Answer {
    eprintln!(
        "rosemary: {}",
        question(request).replace('\n', "\n          ")
    );
    eprint!("Download it? [y]es, [n]o, or the path of a MiniDarwin bundle.zip: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return Answer::Decline;
    }
    match line.trim() {
        "y" | "Y" | "yes" | "Yes" => Answer::Download,
        "" | "n" | "N" | "no" | "No" => Answer::Decline,
        path => Answer::Bundle(PathBuf::from(path)),
    }
}

fn say(line: &str) {
    println!("  {line}");
}

/// Select or create the guest prefix and record it for child processes.
pub fn settle_prefix() {
    let named = std::env::var_os("ROSEMARY_PREFIX");
    if cfg!(target_os = "macos") && named.is_none() {
        return;
    }
    // An explicit `off` is the one way to run with no prefix at all.
    if matches!(
        prefix::layout::setting_from(named.clone()),
        prefix::layout::Setting::Off
    ) && named.is_some()
    {
        return;
    }
    let Some(directory) = named.clone().map(PathBuf::from).or_else(default_prefix) else {
        // No `$HOME` and no `$XDG_DATA_HOME` is a host this cannot place a
        // prefix on. Running without one is what happened before prefixes
        // existed, and is better than refusing to start.
        return;
    };
    match prefix::layout::create(&directory) {
        Ok(true) => {
            eprintln!("rosemary: created a prefix at {}", directory.display());
            // Never fatal: the libraries a guest links against are this
            // runtime's own, so a prefix without a base still runs it.
            if let Err(e) = provision_base(
                &directory,
                &Chosen::pinned(prefix::pinned::host_arch()),
                None,
                true,
            ) {
                eprintln!("rosemary: the prefix has no Darwin base: {e}");
            }
        }
        Ok(false) => {
            if let Err(e) = prefix::layout::fill_in(&directory) {
                eprintln!("rosemary: {}: {e}", directory.display());
            }
            let prefix = Prefix::at(&directory);
            if named.is_none() {
                offer_update(&prefix);
            }
            check_at_startup(&prefix);
        }
        Err(e) => {
            eprintln!(
                "rosemary: {}: {e}; running without a prefix",
                directory.display()
            );
            // SAFETY: before any thread has been started.
            unsafe { std::env::set_var("ROSEMARY_PREFIX", "off") };
            return;
        }
    }
    // SAFETY: before any thread has been started; the string is copied by the
    // call.
    unsafe { std::env::set_var("ROSEMARY_PREFIX", &directory) };
}

const UPDATE_TIP: &str = "Tip: You can update manually with `rosemary prefix update`. You are responsible for future MiniDarwin updates.";

fn offer_update(prefix: &Prefix) {
    let state_path = prefix::layout::state_path(prefix.directory());
    let Ok(mut state) = State::read(&state_path) else {
        return;
    };
    let Some(base) = &state.base else { return };
    let Ok(chosen) = Chosen::pinned(&base.arch) else {
        return;
    };
    if !update_needed(&state, chosen.spec.manifest.sha256.as_deref()) {
        return;
    }
    let from = base.tag.clone().unwrap_or_else(|| {
        format!(
            "release {}",
            base.sequence.map_or("?".into(), |n| n.to_string())
        )
    });
    let to = chosen.spec.describe();
    let choice = match setup() {
        Setup::Download => Some((true, false)),
        Setup::Fail => None,
        Setup::Ask => ask_update(&from, &chosen.spec),
    };
    let Some((update, skip)) = choice else { return };
    if skip {
        state.skip_update_prompts = true;
        if let Err(e) = state.write(&state_path) {
            eprintln!("rosemary: prefix: could not save update preference: {e}");
        }
        eprintln!("rosemary: {UPDATE_TIP}");
    }
    if !update {
        return;
    }
    let result = (|| -> Result<(), String> {
        let cache = Cache::from_env().ok_or("no $HOME, so there is nowhere to cache a base")?;
        let obtained = provision::obtain(&cache, &chosen, &mut |_| Answer::Download, &mut |line| {
            eprintln!("rosemary: {line}")
        })
        .map_err(|e| e.to_string())?;
        let installed = provision::install(
            prefix,
            &cache,
            &chosen,
            &obtained,
            &Options {
                policy: provision::load(prefix).map_err(|e| e.to_string())?.policy,
                allow_downgrade: false,
            },
        )
        .map_err(|e| e.to_string())?;
        eprintln!(
            "rosemary: prefix: updated MiniDarwin to {to} ({} paths changed)",
            installed.outcome.restored.len()
        );
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!(
            "rosemary: prefix: MiniDarwin update failed: {e}; run `rosemary prefix update` to retry"
        );
    }
}

fn update_needed(state: &State, pinned_manifest: Option<&str>) -> bool {
    let Some(base) = &state.base else {
        return false;
    };
    !state.skip_update_prompts
        && base.trust == Trust::Pinned
        && pinned_manifest.is_some_and(|sha| sha != base.manifest_sha256)
}

fn ask_update(from: &str, spec: &prefix::format::Spec) -> Option<(bool, bool)> {
    let size = download_size(spec.artifact.size);
    let source = spec.artifact.sources.first().map(String::as_str);
    let summary = format!(
        "This prefix has {from}. This build ships with {}. Updating may download {size} from\n{}",
        spec.describe(),
        source.unwrap_or("(no source)")
    );
    if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let informative = RichText::new()
            .plain(format!("This prefix has {from}. This build ships with "))
            .bold(spec.describe())
            .plain(format!(". Updating may download {size} from\n"));
        let request = appkit::dialog::Request {
            title: "Rosemary".into(),
            message: "Update the MiniDarwin base system?".into(),
            rich_informative: Some(with_source(informative, source)),
            buttons: vec!["Update".into(), "Not Now".into()],
            cancel: Some(1),
            checkbox: Some("Don't remind me about updates again".into()),
            checkbox_tip: UPDATE_TIP.into(),
            ..Default::default()
        };
        if let Some((button, checked)) = appkit::dialog::run_with_checkbox(&request) {
            return Some((button == 0, checked));
        }
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return None;
    }
    eprintln!("rosemary: {}", summary.replace('\n', "\n          "));
    let mut skip = false;
    loop {
        eprint!(
            "Update MiniDarwin? [y]es, [n]ot now, [t]oggle [{}] Don't remind me again: ",
            if skip { 'x' } else { ' ' }
        );
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().lock().read_line(&mut line).is_err() {
            return None;
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "t" => {
                skip = !skip;
                if skip {
                    eprintln!("rosemary: {UPDATE_TIP}");
                }
            }
            "y" | "yes" => return Some((true, skip)),
            "" | "n" | "no" => return Some((false, skip)),
            _ => eprintln!("rosemary: enter y, n, or t"),
        }
    }
}

/// The cheap check a start makes: nothing, unless a guest was seen writing a
/// system file, and then only those files.
fn check_at_startup(prefix: &Prefix) {
    let Ok(provisioned) = provision::load(prefix) else {
        return;
    };
    if provisioned.spec.policy == Policy::Insecure {
        eprintln!(
            "rosemary: *** this prefix's base was verified under an INSECURE spec: its \
             integrity was checked, not its authenticity ***"
        );
    }
    let dirty = system::dirty(prefix);
    if dirty.is_empty() {
        return;
    }
    let root = prefix.drive();
    let expected = prefix::layout::expected_manifest(&provisioned.manifest);
    let problems = match verify::compare(&root, &expected, true, &Scope::Paths(&dirty)) {
        Ok(problems) => problems,
        Err(e) => {
            eprintln!("rosemary: prefix: checking the system files failed: {e}");
            return;
        }
    };
    if problems.is_empty() {
        let _ = system::clear_dirty(prefix);
        return;
    }
    if provisioned.policy == SystemFiles::Warn {
        eprintln!(
            "rosemary: prefix: {} system file(s) differ from the base: {}. \
             `rosemary prefix repair` puts them back.",
            problems.len(),
            provision::describe(&problems)
        );
        return;
    }
    let Some(cache) = Cache::from_env() else {
        return;
    };
    // Offline only: a start never downloads without being asked to.
    match provision::repair(
        prefix,
        &cache,
        &provisioned,
        &problems,
        &mut |_| Answer::Decline,
        &mut |_| {},
    ) {
        Ok(outcome) => {
            eprintln!(
                "rosemary: prefix: restored {} system file(s) a guest had changed",
                outcome.restored.len()
            );
            for (path, to) in &outcome.displaced {
                eprintln!(
                    "rosemary: prefix: {path} was in the way; moved to {}",
                    to.display()
                );
            }
            let _ = system::clear_dirty(prefix);
        }
        Err(e) => eprintln!(
            "rosemary: prefix: {} system file(s) differ from the base, and could not be \
             restored offline ({e}); run `rosemary prefix repair`",
            problems.len()
        ),
    }
}

/// A warning when the guest's architecture is not the base's. Low stakes while
/// this runtime's shims answer for the system libraries, but a guest that
/// `dlopen`s one from `/usr/lib` would find the wrong slice.
pub fn check_guest_arch(guest_arch: &str) {
    let Some(prefix) = prefix::ns::current() else {
        return;
    };
    let Ok(state) = State::read(&prefix::layout::state_path(prefix.directory())) else {
        return;
    };
    if let Some(base) = state.base
        && base.arch != guest_arch
    {
        eprintln!(
            "rosemary: prefix: this guest is {guest_arch}, but the prefix's Darwin base is {}; \
             `rosemary prefix create --arch {guest_arch}` makes a matching one",
            base.arch
        );
    }
}

/// Make sure `engine`'s guests have a C++ runtime to link against: the prefix's
/// base, else the pinned MiniDarwin base for the guest's architecture, asking
/// before it is downloaded. There is no third source, so declining is an error.
pub fn ensure_cxx_runtime(engine: rosemary::exec::Engine) -> Result<(), String> {
    use rosemary::cxxlib::{self, Flavor};
    let flavor = match engine {
        rosemary::exec::Engine::Emulated => Flavor::Intel,
        rosemary::exec::Engine::Native => cxxlib::HOST,
    };
    if cxxlib::locate(flavor).is_ok() {
        return Ok(());
    }
    let arch = match flavor {
        Flavor::Intel => "x86_64",
        Flavor::Arm => "aarch64",
    };
    let chosen = Chosen::pinned(arch).map_err(|e| e.to_string())?;
    let cache = Cache::from_env().ok_or("no $HOME, so there is nowhere to cache a base")?;
    let mut ask = asker(true);
    match provision::obtain(&cache, &chosen, &mut ask, &mut |l| {
        eprintln!("rosemary: {l}")
    }) {
        Ok(_) => {}
        Err(Error::Declined) if setup() == Setup::Fail => {
            return Err(format!(
                "the {arch} MiniDarwin base is not cached, and ROSEMARY_PREFIX_SETUP=fail"
            ));
        }
        Err(Error::Declined) => {
            return Err(format!(
                "the {arch} MiniDarwin base provides this guest's libc++ and was not \
                 downloaded; run `rosemary prefix create --arch {arch}` or use --bundle"
            ));
        }
        Err(e) => return Err(e.to_string()),
    }
    cxxlib::locate(flavor).map(|_| ())
}

/// Obtain and install a base; the one path `create`, `update` and a first run
/// share.
fn provision_base(
    directory: &Path,
    chosen: &Result<Chosen, prefix::format::Invalid>,
    policy: Option<SystemFiles>,
    gui: bool,
) -> Result<provision::Installed, String> {
    let chosen = chosen.as_ref().map_err(|e| e.to_string())?;
    let cache = Cache::from_env().ok_or("no $HOME, so there is nowhere to cache a base")?;
    let prefix = Prefix::at(directory);
    let mut ask = asker(gui);
    let obtained = match provision::obtain(&cache, chosen, &mut ask, &mut |l| {
        eprintln!("rosemary: {l}")
    }) {
        Ok(obtained) => obtained,
        Err(Error::Declined) if setup() == Setup::Fail => {
            return Err("it is not cached, and ROSEMARY_PREFIX_SETUP=fail".into());
        }
        Err(Error::Declined) => {
            return Err("not downloaded; `rosemary prefix create` installs it later".into());
        }
        Err(e) => return Err(e.to_string()),
    };
    let previous = provision::load(&prefix).ok();
    let policy = policy
        .or(previous.as_ref().map(|p| p.policy))
        .unwrap_or_default();
    let installed = provision::install(
        &prefix,
        &cache,
        chosen,
        &obtained,
        &Options {
            policy,
            allow_downgrade: false,
        },
    )
    .map_err(|e| e.to_string())?;
    let changed = match installed.outcome.restored.len() {
        0 => "every file already in place".to_string(),
        n => format!("{n} paths, by {}", methods(&installed.outcome)),
    };
    eprintln!(
        "rosemary: installed {} into {} ({changed})",
        chosen.spec.describe(),
        directory.display(),
    );
    Ok(installed)
}

fn methods(outcome: &prefix::repair::Outcome) -> String {
    match outcome.methods.is_empty() {
        true => "nothing to copy".into(),
        false => outcome
            .methods
            .iter()
            .map(|m| m.name())
            .collect::<Vec<_>>()
            .join(" and "),
    }
}

/// `rosemary prefix ...`.
pub fn command(args: impl Iterator<Item = OsString>) -> ExitCode {
    let args: Vec<String> = args.map(|a| a.to_string_lossy().into_owned()).collect();
    let Some((action, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    if action == "cache" {
        return cache_command(rest);
    }
    let mut flags = Flags::default();
    let mut named = None;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        let mut value = |name: &str| {
            let v = rest.next().cloned();
            if v.is_none() {
                eprintln!("rosemary: prefix: {name} needs a value");
            }
            v
        };
        match arg.as_str() {
            "--arch" => flags.arch = value("--arch"),
            "--spec" => flags.spec = value("--spec").map(PathBuf::from),
            "--bundle" => flags.bundle = value("--bundle").map(PathBuf::from),
            "--system-files" => match value("--system-files").as_deref().map(SystemFiles::parse) {
                Some(Some(policy)) => flags.policy = Some(policy),
                _ => {
                    eprintln!("rosemary: prefix: --system-files is repair, warn or immutable");
                    return ExitCode::FAILURE;
                }
            },
            "--no-base" => flags.no_base = true,
            "--dry-run" => flags.dry_run = true,
            "--allow-downgrade" => flags.allow_downgrade = true,
            flag if flag.starts_with("--") => {
                eprintln!("rosemary: prefix: unknown option {flag}\n{USAGE}");
                return ExitCode::FAILURE;
            }
            path if named.is_none() => named = Some(PathBuf::from(path)),
            extra => {
                eprintln!("rosemary: prefix: unexpected {extra:?}\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(directory) = named.or_else(default_prefix) else {
        eprintln!("rosemary: no $HOME, so there is nowhere to put a prefix; name one");
        return ExitCode::FAILURE;
    };
    let prefix = Prefix::at(&directory);
    let result = match action.as_str() {
        "create" => create(&prefix, &flags),
        "info" => info(&prefix),
        "verify" => check(&prefix),
        "repair" => repair(&prefix, &flags, false),
        "reset-system" => repair(&prefix, &flags, true),
        "update" => update(&prefix, &flags),
        other => Err(format!("{other:?} is not a prefix command\n{USAGE}")),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rosemary: prefix: {e}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Default)]
struct Flags {
    arch: Option<String>,
    spec: Option<PathBuf>,
    bundle: Option<PathBuf>,
    policy: Option<SystemFiles>,
    no_base: bool,
    dry_run: bool,
    allow_downgrade: bool,
}

fn create(prefix: &Prefix, flags: &Flags) -> Result<ExitCode, String> {
    let directory = prefix.directory();
    match prefix::layout::create(directory).map_err(|e| format!("{}: {e}", directory.display()))? {
        true => println!("created {}", directory.display()),
        false => println!("{} is already a prefix", directory.display()),
    }
    if flags.no_base {
        return Ok(ExitCode::SUCCESS);
    }
    let existing = provision::load(prefix).ok();
    let asked_for_base = flags.arch.is_some() || flags.spec.is_some() || flags.bundle.is_some();
    if let Some(existing) = &existing
        && !asked_for_base
        && flags.policy.is_none_or(|p| p == existing.policy)
    {
        println!("base:    {}", existing.spec.describe());
        return Ok(ExitCode::SUCCESS);
    }
    let arch = flags
        .arch
        .clone()
        .or(existing.as_ref().map(|e| e.base.arch.clone()))
        .unwrap_or_else(|| prefix::pinned::host_arch().to_string());
    let mut chosen = match (&flags.spec, &existing) {
        (Some(spec), _) => Chosen::user(spec),
        // Changing only the policy keeps the base the prefix already has.
        (None, Some(existing)) if !asked_for_base => Ok(existing.chosen()),
        _ => Chosen::pinned(&arch),
    };
    if let Ok(chosen) = &mut chosen {
        chosen.bundle = flags.bundle.clone();
        if chosen.spec.arch().is_some_and(|a| a != arch) && flags.arch.is_some() {
            return Err(format!(
                "--arch {arch}, but the spec is for {}",
                chosen.spec.arch().unwrap_or("?")
            ));
        }
        if chosen.trust == Trust::User {
            println!("spec:    supplied by the user; integrity is checked, authenticity is not");
        }
        for key in system::ignored_keys(&chosen.spec) {
            println!("spec:    ignoring '{key}' (not part of the minidarwin format)");
        }
    }
    let installed = provision_base(directory, &chosen, flags.policy, false)?;
    for (path, to) in &installed.outcome.displaced {
        println!("moved aside: {path} -> {}", to.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn info(prefix: &Prefix) -> Result<ExitCode, String> {
    let directory = prefix.directory();
    if !prefix::layout::created(directory) {
        return Err(format!("{}: not a prefix", directory.display()));
    }
    println!("prefix:   {}", directory.display());
    println!("guest /:  {}", prefix.drive().display());
    let state = State::read(&prefix::layout::state_path(directory)).map_err(|e| e.to_string())?;
    if let Some(version) = &state.runtime_version {
        println!("made by:  rosemary {version}");
    }
    let provisioned = match provision::load(prefix) {
        Ok(p) => p,
        Err(e) if state.base.is_none() => {
            let _ = e;
            println!("base:     none; `rosemary prefix create` installs one");
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => return Err(e.to_string()),
    };
    describe_base(&provisioned);
    println!(
        "policy:   {} verification; system files: {}",
        provisioned.spec.policy.name(),
        provisioned.policy.name()
    );
    println!("files:    {}", provisioned.base.materialised.join(", "));
    if let Some(cache) = Cache::from_env() {
        let tree = cache.tree(&provisioned.base.tree).is_dir();
        let artifact = provisioned
            .base
            .artifact_sha256
            .as_ref()
            .is_some_and(|sha| cache.artifact(sha).is_file());
        println!(
            "cache:    tree {}, artifact {} ({})",
            if tree { "cached" } else { "not cached" },
            if artifact { "cached" } else { "not cached" },
            cache.root().display()
        );
    }
    let dirty = system::dirty(prefix);
    match dirty.len() {
        0 => println!("dirty:    no"),
        n => println!("dirty:    {n} system path(s) written since the last check"),
    }
    if let Some(Ok(pinned)) = prefix::pinned::spec(&provisioned.base.arch) {
        let current = pinned.manifest.sha256.as_deref() == Some(&provisioned.base.manifest_sha256);
        let sequence = pinned
            .release
            .sequence
            .map_or("?".into(), |s| s.to_string());
        match current {
            true => println!("pinned:   release {sequence}; up to date"),
            false => println!("pinned:   release {sequence}; `rosemary prefix update` installs it"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn describe_base(provisioned: &Provisioned) {
    let base = &provisioned.base;
    println!("base:     {}", provisioned.spec.describe());
    if let Some(sha) = &base.artifact_sha256 {
        println!("          artifact sha256 {sha}");
    }
    println!("          manifest sha256 {}", base.manifest_sha256);
    println!("          tree            {}", base.tree);
    match base.trust {
        Trust::Pinned => println!("trust:    pinned by this build of rosemary"),
        Trust::User => println!(
            "trust:    a spec the user supplied: integrity is checked, authenticity is not"
        ),
    }
    if provisioned.spec.policy == Policy::Insecure {
        println!("          *** verification policy is INSECURE ***");
    }
    for key in system::ignored_keys(&provisioned.spec) {
        println!("          ignoring '{key}' (not part of the minidarwin format)");
    }
}

fn report(problems: &[Problem], total: usize) {
    for p in problems {
        match p.detail.is_empty() {
            true => println!("  {:<10} {}", p.kind.name(), p.path),
            false => println!("  {:<10} {}  ({})", p.kind.name(), p.path, p.detail),
        }
    }
    let counts = verify::counts(problems);
    let kinds: Vec<String> = Difference::ALL[..4]
        .iter()
        .zip(counts)
        .map(|(kind, n)| format!("{n} {kind}"))
        .collect();
    println!("  {total} entries checked: {}", kinds.join(", "));
}

fn check(prefix: &Prefix) -> Result<ExitCode, String> {
    let provisioned = provision::load(prefix).map_err(|e| e.to_string())?;
    println!("prefix:   {}", prefix.directory().display());
    describe_base(&provisioned);
    println!("system integrity:");
    let expected = prefix::layout::expected_manifest(&provisioned.manifest);
    let problems = verify::compare(&prefix.drive(), &expected, true, &Scope::All)
        .map_err(|e| e.to_string())?;
    report(&problems, expected.entries.len());
    println!("  unexpected: not checked (a prefix holds the user's own files)");
    println!("user state: not checked");
    if problems.is_empty() {
        let _ = system::clear_dirty(prefix);
        println!("OK");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("FAILED; `rosemary prefix repair` puts the base back");
        Ok(ExitCode::FAILURE)
    }
}

/// `repair`, and with `reset` `reset-system`: the same, plus anything a user
/// put inside a directory the base owns is moved aside.
fn repair(prefix: &Prefix, flags: &Flags, reset: bool) -> Result<ExitCode, String> {
    let provisioned = match provision::load(prefix) {
        Ok(p) => p,
        // A prefix whose own spec or manifest was damaged: `reset-system` puts
        // those back too, from the spec that says what was installed.
        Err(e) if reset => return reset_from_scratch(prefix, flags, &e.to_string()),
        Err(e) => return Err(e.to_string()),
    };
    let root = prefix.drive();
    let expected = prefix::layout::expected_manifest(&provisioned.manifest);
    let found =
        verify::compare(&root, &expected, !reset, &Scope::All).map_err(|e| e.to_string())?;
    let mut problems: Vec<Problem> = prefix::repair::actionable(&found).cloned().collect();
    if reset {
        problems.extend(
            prefix::repair::strays(&found, &provisioned.manifest)
                .into_iter()
                .cloned(),
        );
    }
    if problems.is_empty() {
        println!(
            "nothing to repair: {} entries match the base",
            expected.entries.len()
        );
        let _ = system::clear_dirty(prefix);
        return Ok(ExitCode::SUCCESS);
    }
    for p in &problems {
        let verb = match p.kind {
            Difference::Unexpected => "move aside",
            Difference::WrongType => "replace",
            _ => "restore",
        };
        println!("  {verb:<10} {}  ({})", p.path, p.kind);
    }
    if flags.dry_run {
        println!("dry run: {} path(s) would change", problems.len());
        return Ok(ExitCode::SUCCESS);
    }
    let cache = Cache::from_env().ok_or("no $HOME, so there is no cache to repair from")?;
    let mut ask = asker(false);
    let outcome = provision::repair(prefix, &cache, &provisioned, &problems, &mut ask, &mut say)
        .map_err(|e| e.to_string())?;
    for (path, to) in &outcome.displaced {
        println!("moved aside: {path} -> {}", to.display());
    }
    let after = verify::compare(&root, &expected, true, &Scope::All).map_err(|e| e.to_string())?;
    if !after.is_empty() {
        return Err(format!(
            "still different after the repair: {}",
            provision::describe(&after)
        ));
    }
    let _ = system::clear_dirty(prefix);
    println!(
        "repaired: {} restored by {}, {} moved aside; {} entries match the base",
        outcome.restored.len(),
        methods(&outcome),
        outcome.displaced.len(),
        expected.entries.len()
    );
    Ok(ExitCode::SUCCESS)
}

fn reset_from_scratch(prefix: &Prefix, flags: &Flags, why: &str) -> Result<ExitCode, String> {
    let state =
        State::read(&prefix::layout::state_path(prefix.directory())).map_err(|e| e.to_string())?;
    let base = state
        .base
        .ok_or("no base has been installed; run `rosemary prefix create`")?;
    println!("the prefix's own description is damaged ({why}); reinstalling it");
    if flags.dry_run {
        return Ok(ExitCode::SUCCESS);
    }
    let chosen = match base.trust {
        Trust::Pinned => Chosen::pinned(&base.arch),
        Trust::User => Chosen::user(&prefix.control().join("spec.yaml")),
    };
    if let Ok(chosen) = &chosen
        && chosen
            .spec
            .manifest
            .sha256
            .as_deref()
            .is_some_and(|sha| sha != base.manifest_sha256)
    {
        return Err(
            "the spec that installed this base is no longer available; `rosemary prefix update` \
             installs the one this build pins"
                .into(),
        );
    }
    provision_base(prefix.directory(), &chosen, None, false)?;
    Ok(ExitCode::SUCCESS)
}

fn update(prefix: &Prefix, flags: &Flags) -> Result<ExitCode, String> {
    let provisioned = provision::load(prefix).map_err(|e| e.to_string())?;
    let arch = flags.arch.clone().unwrap_or(provisioned.base.arch.clone());
    let mut chosen = Chosen::pinned(&arch).map_err(|e| e.to_string())?;
    chosen.bundle = flags.bundle.clone();
    if chosen.spec.manifest.sha256.as_deref() == Some(&provisioned.base.manifest_sha256) {
        println!("up to date: {}", chosen.spec.describe());
        return Ok(ExitCode::SUCCESS);
    }
    println!("from:     {}", provisioned.spec.describe());
    println!("to:       {}", chosen.spec.describe());
    if flags.dry_run {
        return Ok(ExitCode::SUCCESS);
    }
    let cache = Cache::from_env().ok_or("no $HOME, so there is nowhere to cache a base")?;
    let mut ask = asker(false);
    let obtained =
        provision::obtain(&cache, &chosen, &mut ask, &mut say).map_err(|e| e.to_string())?;
    let installed = provision::install(
        prefix,
        &cache,
        &chosen,
        &obtained,
        &Options {
            policy: provisioned.policy,
            allow_downgrade: flags.allow_downgrade,
        },
    )
    .map_err(|e| e.to_string())?;
    println!(
        "updated:  {} path(s) changed, {} retired",
        installed.outcome.restored.len(),
        installed.retired.len()
    );
    Ok(ExitCode::SUCCESS)
}

fn cache_command(args: &[String]) -> ExitCode {
    let Some(cache) = Cache::from_env() else {
        eprintln!("rosemary: no $HOME, so there is no rootfs cache");
        return ExitCode::FAILURE;
    };
    // What a pinned spec names, and what every prefix that uses this cache
    // was made from: the objects `gc` keeps.
    let mut pinned: HashSet<String> = HashSet::new();
    for arch in prefix::pinned::architectures() {
        if let Some(Ok(spec)) = prefix::pinned::spec(arch) {
            pinned.extend(spec.artifact.sha256.clone());
            pinned.extend(spec.manifest.sha256.clone());
            pinned.extend(spec.tree.clone());
        }
    }
    let mut used: Vec<(String, PathBuf)> = Vec::new();
    for directory in cache.prefixes() {
        if let Ok(State {
            base: Some(base), ..
        }) = State::read(&prefix::layout::state_path(&directory))
        {
            for key in [
                Some(base.tree),
                Some(base.manifest_sha256),
                base.artifact_sha256,
            ]
            .into_iter()
            .flatten()
            {
                used.push((key, directory.clone()));
            }
        }
    }
    match args.first().map(String::as_str) {
        Some("list") => {
            println!("cache: {}", cache.root().display());
            for object in cache.list() {
                let users: Vec<String> = used
                    .iter()
                    .filter(|(key, _)| *key == object.key)
                    .map(|(_, p)| p.display().to_string())
                    .collect();
                let mut notes = Vec::new();
                if pinned.contains(&object.key) {
                    notes.push("pinned".to_string());
                }
                if !users.is_empty() {
                    notes.push(format!("used by {}", users.join(", ")));
                }
                println!(
                    "  {:<8} {}  {:>8.1} MB  {}",
                    object.kind,
                    object.key,
                    object.bytes as f64 / 1e6,
                    notes.join("; ")
                );
            }
            ExitCode::SUCCESS
        }
        Some("gc") => {
            let mut keep = pinned;
            keep.extend(used.into_iter().map(|(key, _)| key));
            let removed = cache.gc(&keep);
            for object in &removed {
                println!("removed {} {}", object.kind, object.key);
            }
            println!("{} object(s) removed", removed.len());
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prefix::state::Base;

    #[test]
    fn prompts_only_for_outdated_pinned_bases_with_updates_enabled() {
        let mut state = State {
            base: Some(Base {
                project: "minidarwin".into(),
                arch: "x86_64".into(),
                sequence: Some(1),
                tag: None,
                tree: "tree".into(),
                manifest_sha256: "old".into(),
                artifact_sha256: None,
                trust: Trust::Pinned,
                policy: Policy::Strict,
                materialised: vec![],
            }),
            ..State::default()
        };
        assert!(update_needed(&state, Some("new")));
        assert!(!update_needed(&state, Some("old")));
        assert!(!update_needed(&state, None));
        state.skip_update_prompts = true;
        assert!(!update_needed(&state, Some("new")));
        state.skip_update_prompts = false;
        state.base.as_mut().unwrap().trust = Trust::User;
        assert!(!update_needed(&state, Some("new")));
        state.base = None;
        assert!(!update_needed(&state, Some("new")));
    }
}
