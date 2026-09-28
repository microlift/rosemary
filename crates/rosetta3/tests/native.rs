mod common;

use common::{build_native, build_native_with, run_guest_with, run_native};

/// Run `path` through the driver, letting it choose a backend.
fn run(path: &str, args: &[&str]) -> (Option<i32>, String, String) {
    run_guest_with(&[], path, args)
}

#[test]
fn a_native_hello_world_matches_the_kernels_own_run_of_it() {
    let Some(path) = build_native("native_hello", common::HELLO_C) else {
        skip!("cannot build native fixtures")
    };
    let Some((native_status, native_out)) = run_native(&path) else {
        skip!("cannot run the native fixture")
    };
    let (status, out, err) = run(&path, &[]);

    assert_eq!(status, native_status, "stderr: {err}");
    assert_eq!(out, native_out, "stderr: {err}");
}

#[test]
fn a_native_exit_status_reaches_the_shell() {
    let Some(path) = build_native("native_hello_exit", common::HELLO_C) else {
        skip!("cannot build native fixtures")
    };
    // `main` returns `abs(argc) - 1`, so one argument makes it 1 - and a
    // runtime that dropped the status would report 0 for both.
    let (status, _, err) = run(&path, &["one"]);
    assert_eq!(status, Some(1), "stderr: {err}");
}

#[test]
fn a_native_unix_thread_entry_matches_the_kernels_own_run_of_it() {
    let Some(path) =
        build_native_with("native_unix_thread", common::HELLO_C, &["-Wl,-no_new_main"])
    else {
        skip!("cannot build native LC_UNIXTHREAD fixtures")
    };
    let bytes = std::fs::read(&path).expect("read LC_UNIXTHREAD fixture");
    let object = machish::image::MachObject::parse_any(&bytes, rosetta3::exec::native::HOST_SPEC)
        .expect("parse LC_UNIXTHREAD fixture");
    if !matches!(
        object.entry(),
        Some(machish::image::EntryPoint::UnixThread { .. })
    ) {
        skip!("the linker did not produce LC_UNIXTHREAD")
    }
    let Some((native_status, native_out)) = run_native(&path) else {
        skip!("cannot run the native LC_UNIXTHREAD fixture")
    };
    let (status, out, err) = run_guest_with(&["--native"], &path, &[]);

    assert_eq!(status, native_status, "stderr: {err}");
    assert_eq!(out, native_out, "stderr: {err}");
}

#[test]
fn native_constructors_run_in_order_before_main() {
    let Some(path) = build_native("native_ctors", common::CTOR_C) else {
        skip!("cannot build native fixtures")
    };
    let Some((_, native_out)) = run_native(&path) else {
        skip!("cannot run the native fixture")
    };
    let (status, out, err) = run(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, native_out, "stderr: {err}");
}

/// The native backend links the vendored libc++ of this host's architecture
/// rather than the system's, so a C++ fixture printing what the host's own build
/// prints is what says the substitution is ABI-faithful.
#[test]
fn a_native_cxx_program_matches_the_hosts_own_libcxx() {
    let Some(path) = build_native_with("native_cxx", CXX, &["-x", "c++", "-lc++"]) else {
        skip!("cannot build native C++ fixtures")
    };
    let Some((_, native_out)) = run_native(&path) else {
        skip!("cannot run the native fixture")
    };
    let (status, out, err) = run(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, native_out, "stderr: {err}");
}

const CXX: &str = r#"
#include <iostream>
#include <string>
#include <vector>

int main() {
    std::vector<std::string> v{"a", "bb", "ccc"};
    for (const auto &s : v) std::cout << s << " " << s.size() << "\n";
    return 0;
}
"#;

#[test]
fn a_native_cxx_program_unwinds_through_its_own_frames() {
    let Some(path) = build_native_with("native_throw", THROW_CXX, &["-x", "c++", "-lc++"]) else {
        skip!("cannot build native C++ fixtures")
    };
    let Some((_, native_out)) = run_native(&path) else {
        skip!("cannot run the native fixture")
    };
    let (status, out, err) = run(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, native_out, "stderr: {err}");
    assert!(out.contains("caught deep"), "no handler ran: {out}");
}

const THROW_CXX: &str = r#"
#include <cstdio>
#include <stdexcept>
#include <string>

static int destroyed = 0;
struct Trace { ~Trace() { destroyed++; } };

static void level(int n) {
    Trace t;
    std::string hold(64, 'z');
    if (n == 0) throw std::logic_error("deep");
    level(n - 1);
}

int main() {
    try { level(20); }
    catch (const std::logic_error &e) {
        printf("caught %s destroyed=%d\n", e.what(), destroyed);
    }
    try {
        try { throw std::out_of_range("inner"); }
        catch (...) { throw; }
    } catch (const std::exception &e) { printf("rethrown %s\n", e.what()); }
    return 0;
}
"#;

const HOST_RUNS_X86_64: bool = cfg!(target_arch = "x86_64");

#[test]
fn an_x86_64_binary_takes_the_backend_the_host_makes_available() {
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    // Spawned here rather than through a `common` helper because those state an
    // engine, and what this test is about is the one the driver picks when it is
    // not told. A region entry is the thing only the emulated backend can
    // report, because only the emulated backend translates a region to enter -
    // the image map is not that thing, since both backends print one.
    let out = common::driver_command()
        .args(["run", &path])
        .env("ROSETTA3_STATS", "1")
        .output()
        .expect("running the driver");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(0), "stderr: {err}");
    let translated = err.contains("region entr(");
    match HOST_RUNS_X86_64 {
        // Native is both faster and available, so selection must prefer it -
        // translating an x86-64 binary on an x86-64 CPU is pure loss.
        true => assert!(
            !translated,
            "the native backend should have been chosen: {err}"
        ),
        false => assert!(translated, "the emulated backend translates: {err}"),
    }
}

#[test]
fn forcing_native_on_an_x86_64_binary_names_the_architecture() {
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, _, err) = run_guest_with(&["--native"], &path, &[]);

    if HOST_RUNS_X86_64 {
        assert_eq!(status, Some(0), "the host can run it directly: {err}");
        return;
    }
    assert_eq!(status, Some(1));
    assert!(err.contains("native backend"), "{err}");
    assert!(err.contains("x86-64"), "{err}");
}

#[test]
fn forcing_emulated_on_a_native_binary_names_the_architecture() {
    let Some(path) = build_native("native_hello_emulated", common::HELLO_C) else {
        skip!("cannot build native fixtures")
    };
    let (status, _, err) = run_guest_with(&["--emulated"], &path, &[]);

    if HOST_RUNS_X86_64 {
        // The "native" fixture is an x86-64 one here, so it is also a fixture
        // the lifter accepts, and forcing emulation recompiles it.
        assert_eq!(status, Some(0), "the lifter accepts it: {err}");
        return;
    }
    assert_eq!(status, Some(1));
    assert!(err.contains("emulated backend"), "{err}");
}

/// `/bin/ls` is x86-64 plus arm64e, and machish refuses arm64e's authenticated
/// chained pointers. Selection has to know that before the loader finds out, or
/// a binary that used to run under emulation stops running at all.
#[test]
fn an_arm64e_slice_does_not_claim_the_native_backend() {
    if !common::host_macho("/bin/ls") {
        skip!("no host Mach-O at /bin/ls")
    }
    if HOST_RUNS_X86_64 {
        // There is no arm64e question to answer: the x86-64 slice is the one
        // the native backend takes, and it takes it directly.
        let (status, _, err) = run_guest_with(&["--native"], "/bin/ls", &["/bin"]);
        assert_eq!(status, Some(0), "stderr: {err}");
        return;
    }
    let (status, _, err) = run_guest_with(&["--native"], "/bin/ls", &[]);
    assert_eq!(status, Some(1));
    assert!(err.contains("arm64e"), "{err}");

    // And with nothing forced it runs, which is the part that would regress.
    let out = common::driver_command()
        .args(["run", "/bin/ls", "/bin"])
        .output()
        .expect("running the driver");
    assert_eq!(out.status.code(), Some(0));
}
