//! `rosemary desktop`, and pointing a guest at its prefix's virtual desktop.
//!
//! The desktop's host is this same binary run as `rosemary desktop`. The
//! first guest to make a window with `ROSEMARY_DESKTOP` set starts one if
//! none is listening (see `sdl3::desktop`), so nobody has to start it by
//! hand; the subcommand exists for that launch, and for a person who wants to
//! see why one does not start.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sdl3::desktop::{Extent, host};

/// Start a detached desktop of `extent` listening in `directory`.
///
/// The child daemonizes itself and its first half exits at once, so waiting
/// for it here costs nothing and leaves no zombie behind in a guest that
/// never reaps children it did not make.
fn launch(directory: &Path, extent: Extent) -> std::io::Result<()> {
    let exe = darwin::exe::current()?;
    let status = std::process::Command::new(exe)
        .arg("desktop")
        .arg("--directory")
        .arg(directory)
        .arg("--extent")
        .arg(extent.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    match status.success() {
        true => Ok(()),
        false => Err(std::io::Error::other(format!("desktop launch {status}"))),
    }
}

/// Point this process at its prefix's desktop, which it joins only if it is
/// asked for one and makes a window. Idempotent.
///
/// The desktop listens where the prefix's process server does: guests of one
/// prefix are one machine's programs and share its screen, and those of
/// another prefix do not.
pub fn configure() {
    sdl3::desktop::configure(sdl3::desktop::Config {
        directory: crate::server_cli::directory(),
        launch: Some(launch),
    });
}

const USAGE: &str = "usage: rosemary desktop [--directory DIR] [--extent EXTENT] [--foreground]";

/// `rosemary desktop [--directory DIR] [--extent EXTENT] [--foreground]`.
pub fn command(mut args: impl Iterator<Item = OsString>) -> ExitCode {
    let mut directory = None;
    let mut extent = None;
    let mut foreground = false;
    while let Some(arg) = args.next() {
        match arg.to_str().unwrap_or_default() {
            "--directory" => directory = args.next().map(PathBuf::from),
            "--extent" => extent = args.next(),
            "--foreground" => foreground = true,
            other => {
                eprintln!("rosemary desktop: unknown argument {other:?}");
                eprintln!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    let extent = extent.or_else(|| std::env::var_os(sdl3::desktop::ENV));
    let Some(extent) = extent
        .as_deref()
        .and_then(|extent| extent.to_str())
        .and_then(|extent| extent.parse::<Extent>().ok())
    else {
        eprintln!(
            "rosemary desktop: no extent; give `--extent` or set {} to `fullscreen` or \
             `<width>x<height>`",
            sdl3::desktop::ENV
        );
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let directory = directory.unwrap_or_else(|| {
        crate::prefix_cli::settle_prefix();
        crate::server_cli::directory()
    });
    if !foreground {
        crate::server_cli::detach();
    }
    if let Err(e) = starnix::process::set_name(c"Rosemary Desktop") {
        eprintln!("could not set the desktop's process name: {e}");
    }
    let options = host::Options {
        directory,
        extent,
        linger: host::DEFAULT_LINGER,
    };
    match host::run(&options) {
        Ok(host::Outcome::Finished) => ExitCode::SUCCESS,
        // Somebody else won the race to start one, which is what was wanted.
        Ok(host::Outcome::AlreadyRunning) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rosemary desktop: {e}");
            ExitCode::FAILURE
        }
    }
}
