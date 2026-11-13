//! Starting `rosemary aot` in the background when a run finds an image uncached, or
//! its cache built from a profile that runs have since outgrown.
//!
//! The job must not stay this process's child: a guest that waits for any child
//! would reap it. So the child forks the real build and exits at once, and this
//! process waits for that before the guest runs, leaving it no child of ours.

use std::process::{Command, Stdio};

use machish::memory::Regions;

use super::Layout;
use crate::program::Program;

/// `0` or `off` stops a run from starting a background build.
pub const BACKGROUND_ENV: &str = "ROSEMARY_AOT_BACKGROUND";

/// How long a background build may run before it writes what it has.
const BACKGROUND_DEADLINE_SECONDS: &str = "600";

/// The lowest scheduling priority, which `nice` spells 19.
const BACKGROUND_NICE: libc::c_int = 19;

/// `setpriority`'s target for Darwin's process-wide background policy.
#[cfg(target_os = "macos")]
const PRIO_DARWIN_PROCESS: libc::c_int = 4;
/// The background band: efficiency cores only, with throttled I/O.
#[cfg(target_os = "macos")]
const PRIO_DARWIN_BG: libc::c_int = 0x1000;

/// Whether this run may start a background build.
fn enabled() -> bool {
    !matches!(
        std::env::var(BACKGROUND_ENV).as_deref(),
        Ok("0" | "off" | "")
    )
}

/// Whether image `id` has no cache, or one built from a profile runs have outgrown.
fn stale(layout: &Layout, id: &super::ImageId) -> bool {
    match super::embedded(layout, id) {
        None => true,
        Some(built) => super::profile::grown(
            built.profiled,
            super::profile::distinct(&layout.profile(id)),
        ),
    }
}

/// Starts a background build of `executable` if an image of `program` has no cache
/// file or a stale one. Call before any guest code runs; see the module note.
pub fn if_stale<M: Regions>(program: &Program<M>, executable: &str) {
    if !enabled() {
        return;
    }
    let Some(layout) = Layout::current() else {
        return;
    };
    let missing = program
        .report
        .images
        .iter()
        .filter(|report| report.segments.iter().any(|segment| segment.executable))
        .filter_map(|report| super::profile::image_id(&report.path))
        .any(|id| stale(layout, &id));
    // Only the image each record names: what it brings in is known after linking.
    let missing = missing
        || super::profile::image_id(executable)
            .and_then(|id| super::profile::read_dlopens(&layout.dlopens(&id)).ok())
            .unwrap_or_default()
            .iter()
            .filter_map(|path| super::profile::image_id(path))
            .any(|id| !layout.image(&id).exists());
    if !missing {
        return;
    }
    let Ok(rosemary) = std::env::current_exe() else {
        return;
    };
    let started = Command::new(rosemary)
        .args(["aot", "--detach", "--refresh", "--deadline"])
        .arg(BACKGROUND_DEADLINE_SECONDS)
        .arg(executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if let Err(e) = started {
        eprintln!("rosemary: could not start a background code cache build: {e}");
    }
}

/// In the `rosemary aot --detach` process: leave the parent's process group and
/// let the parent's child exit, continuing in a grandchild at the lowest priority.
///
/// Returns in the grandchild only.
pub fn detach() -> std::io::Result<()> {
    // SAFETY: called before this process starts a thread.
    match unsafe { libc::fork() } {
        -1 => return Err(std::io::Error::last_os_error()),
        // SAFETY: the parent has nothing to flush or unwind.
        0 => {}
        _ => unsafe { libc::_exit(0) },
    }
    // SAFETY: plain system calls on this process.
    unsafe {
        libc::setsid();
        libc::setpriority(libc::PRIO_PROCESS, 0, BACKGROUND_NICE);
        // Where the system has a background band, the build runs in it, as a
        // translation daemon would, rather than competing with the run it serves.
        #[cfg(target_os = "macos")]
        libc::setpriority(PRIO_DARWIN_PROCESS, 0, PRIO_DARWIN_BG);
    }
    Ok(())
}
