//! Running one test alone in a fresh process.
//!
//! SDL makes the first thread that uses video its owner, and keeps window and
//! joystick state process-wide, so a test that drives it cannot share its
//! process with libtest's other threads.

const CHILD: &str = "ROSEMARY_ISOLATED_TEST";

/// Whether the calling test should run its body in this process.
///
/// Outside the child, this runs the calling test again in a child process,
/// asserts that it passed there, and answers false. The test is named by its
/// libtest thread, so that renaming it or its module cannot leave the child
/// running nothing.
pub fn run_here() -> bool {
    run_here_with_video_driver(None)
}

/// Isolate a clipboard-only test from both libtest threads and the host's real
/// desktop clipboard. SDL's dummy video backend keeps clipboard contents local
/// to the subprocess; parallel text tests cannot overwrite each other's data.
pub fn run_here_with_dummy_video() -> bool {
    run_here_with_video_driver(Some("dummy"))
}

fn run_here_with_video_driver(video_driver: Option<&str>) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let current = std::thread::current();
    let name = current.name().expect("a test on its libtest thread");
    let mut command = std::process::Command::new(std::env::current_exe().expect("the test binary"));
    command
        .args(["--exact", name, "--test-threads=1"])
        .env(CHILD, "1");
    if let Some(driver) = video_driver {
        command.env("SDL_VIDEODRIVER", driver);
    }
    let output = command.output().expect("start the test in its own process");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed; 0 failed"),
        "{name} failed in its own process: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    false
}
