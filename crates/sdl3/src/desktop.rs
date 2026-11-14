//! The virtual desktop: every window the guests of a prefix make, drawn into
//! one host window that stands for the screen. Between the windows the host
//! window is clear and takes no input, where the host can be told so, and
//! what the host has behind it is seen and used as ever.
//!
//! A Mac program places its windows itself, in one coordinate space shared
//! with every other program's, and reads the pointer in that space. A window
//! system that keeps placement to itself - Wayland - cannot honour that for
//! separate host windows, and a program that docks one window into another
//! depends on it. So here the desktop is the window system instead, as a
//! Mac's window server is: it owns each window's frame, level and stacking,
//! composites their pictures, and sends input to the one under the pointer.
//!
//! Like the window server it is a process of its own ([`host`]), which the
//! first guest to make a window starts and every guest of the prefix joins.
//! The rest of this module is a guest's side ([`client`]): [`crate::video`]
//! asks it first about every window. A window made while the desktop is on
//! is a real but never shown SDL window - it keeps its id, title and place in
//! the tables keyed by its address - whose geometry, visibility, focus and
//! picture are the host's.
//!
//! [`scene`] is the window list and [`compose`] the drawing, both the host's;
//! [`protocol`] is what the two sides say and [`shared`] the pixels they
//! both see.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

mod client;
mod compose;
pub mod host;
mod protocol;
mod scene;
mod shared;
#[cfg(test)]
mod tests;

pub use client::set_window_level;
pub(crate) use client::*;

/// `ROSEMARY_DESKTOP`: `fullscreen` for a desktop that takes the screen at
/// its resolution, or `<width>x<height>` for one in a window of that size.
pub const ENV: &str = "ROSEMARY_DESKTOP";

/// How large the desktop is, which is its first guest's to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extent {
    Fullscreen,
    Sized(i32, i32),
}

impl std::str::FromStr for Extent {
    type Err = ();

    fn from_str(value: &str) -> Result<Extent, ()> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("fullscreen") {
            return Ok(Extent::Fullscreen);
        }
        let (width, height) = value.split_once(['x', 'X']).ok_or(())?;
        let width = width.trim().parse::<i32>().map_err(|_| ())?;
        let height = height.trim().parse::<i32>().map_err(|_| ())?;
        match width > 0 && height > 0 {
            true => Ok(Extent::Sized(width, height)),
            false => Err(()),
        }
    }
}

impl std::fmt::Display for Extent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Extent::Fullscreen => f.write_str("fullscreen"),
            Extent::Sized(width, height) => write!(f, "{width}x{height}"),
        }
    }
}

fn extent() -> Option<Extent> {
    static EXTENT: OnceLock<Option<Extent>> = OnceLock::new();
    *EXTENT.get_or_init(|| {
        let value = std::env::var(ENV).ok()?;
        if value.trim().is_empty() {
            return None;
        }
        let extent = value.parse().ok();
        if extent.is_none() {
            eprintln!("rosemary: sdl3: {ENV}={value} is not recognised; running without a desktop");
        }
        extent
    })
}

/// Set in the process that is the desktop: its window is the screen, not a
/// window on one.
static HOSTING: AtomicBool = AtomicBool::new(false);

/// Whether windows are made on the virtual desktop.
pub fn active() -> bool {
    !HOSTING.load(Ordering::Acquire) && extent().is_some()
}

/// Starts a desktop of an extent listening in a directory. The runtime
/// supplies this - it is the binary that knows how to run itself as one.
pub type Launcher = fn(&Path, Extent) -> io::Result<()>;

pub struct Config {
    /// The directory the prefix's desktop listens in, private to this user.
    pub directory: PathBuf,
    pub launch: Option<Launcher>,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Point this process at its prefix's desktop. Until this is called a
/// process asked for a desktop has none to join and makes no windows.
pub fn configure(config: Config) {
    let _ = CONFIG.set(config);
}

pub fn socket_in(directory: &Path) -> PathBuf {
    directory.join("desktop-socket")
}

fn lock_in(directory: &Path) -> PathBuf {
    directory.join("desktop-lock")
}

/// What the desktop will not show, each named once.
fn refuse(what: &'static str) {
    static REFUSED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut refused = REFUSED.lock().unwrap_or_else(|e| e.into_inner());
    if !refused.contains(&what) {
        refused.push(what);
        eprintln!("rosemary: sdl3: {what} cannot be shown on the virtual desktop ({ENV}); refused");
    }
}
