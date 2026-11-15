mod common;

use common::{build_native, build_native_with, run_guest_with, run_native};

#[test]
fn native_variadic_file_and_semaphore_calls_preserve_their_arguments() {
    let Some(path) = build_native("native_variadic_open", VARIADIC_OPEN_C) else {
        skip!("cannot build native fixtures")
    };
    for backend in ["builtin", "native"] {
        let output = common::driver_command()
            .args(["run", "--native", &path])
            .env("ROSEMARY_LIBC", backend)
            .output()
            .expect("run native file fixture");
        assert!(
            output.status.success(),
            "{backend}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

const VARIADIC_OPEN_C: &str = r#"
#include <assert.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <semaphore.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

static void check_mode(int fd, mode_t mode) {
    struct stat st;
    assert(fd >= 0);
    assert(fstat(fd, &st) == 0);
    assert((st.st_mode & 0777) == mode);
    assert(close(fd) == 0);
}

int main(void) {
    char dir[] = "/tmp/rosemary-native-open-XXXXXX";
    assert(mkdtemp(dir));
    assert(chdir(dir) == 0);
    umask(0);
    check_mode(open("direct", O_CREAT | O_EXCL | O_RDWR, 0640), 0640);
    int directory = open(".", O_RDONLY);
    assert(directory >= 0);
    check_mode(openat(directory, "relative", O_CREAT | O_EXCL | O_RDWR, 0604), 0604);
    assert(close(open("direct", O_RDONLY)) == 0);
    assert(close(openat(directory, "relative", O_RDONLY)) == 0);
    void *handles[] = {RTLD_DEFAULT, dlopen("/usr/lib/libSystem.B.dylib", RTLD_NOW)};
    assert(handles[1]);
    for (int i = 0; i < 2; ++i) {
        int (*opener)(const char *, int, ...) = dlsym(handles[i], "open");
        int (*relative)(int, const char *, int, ...) = dlsym(handles[i], "openat");
        sem_t *(*semaphore)(const char *, int, ...) = dlsym(handles[i], "sem_open");
        assert(opener && relative && semaphore);
        check_mode(opener("dynamic", O_CREAT | O_EXCL | O_RDWR, 0610), 0610);
        check_mode(relative(directory, "dynamic-relative", O_CREAT | O_EXCL | O_RDWR, 0620), 0620);
        char name[32];
        snprintf(name, sizeof name, "/rosemary-open-%d", getpid());
        sem_unlink(name);
        sem_t *sem = semaphore(name, O_CREAT | O_EXCL, 0600, 2);
        assert(sem != SEM_FAILED);
        assert(sem_trywait(sem) == 0);
        assert(sem_trywait(sem) == 0);
        assert(sem_trywait(sem) == -1 && errno == EAGAIN);
        sem_t *again = sem_open(name, 0);
        assert(again != SEM_FAILED);
        assert(sem_close(again) == 0);
        assert(sem_close(sem) == 0);
        assert(sem_unlink(name) == 0);
        assert(unlink("dynamic") == 0);
        assert(unlink("dynamic-relative") == 0);
    }
    assert(close(directory) == 0);
    assert(unlink("direct") == 0);
    assert(unlink("relative") == 0);
    assert(chdir("/tmp") == 0);
    assert(rmdir(dir) == 0);
    return 0;
}
"#;

#[cfg(target_os = "macos")]
#[test]
fn a_fatal_native_signal_exits_with_an_unresponsive_mach_crash_port() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let Some(path) = build_native(
        "native_crash_port",
        r#"
#include <assert.h>
#include <mach/mach.h>
#include <stdlib.h>
int main(void) {
    mach_port_t port;
    assert(mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &port) == KERN_SUCCESS);
    assert(mach_port_insert_right(mach_task_self(), port, port, MACH_MSG_TYPE_MAKE_SEND) == KERN_SUCCESS);
    assert(task_set_exception_ports(mach_task_self(), EXC_MASK_CRASH | EXC_MASK_CORPSE_NOTIFY,
        port, EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES, THREAD_STATE_NONE) == KERN_SUCCESS);
    abort();
}
"#,
    ) else {
        skip!("cannot build native Mach crash fixture")
    };
    let mut child = common::driver_command()
        .args(["run", "--native", &path])
        .env("ROSEMARY_LIBC", "builtin")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start fatal signal fixture");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child
            .try_wait()
            .expect("check fatal signal fixture")
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("fatal signal hung waiting for the Mach crash server");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child
        .wait_with_output()
        .expect("collect fatal signal fixture");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.signal(), Some(libc::SIGABRT), "{stderr}");
    assert!(stderr.contains("rosemary: fault: SIGABRT"), "{stderr}");
}

#[test]
fn a_bundle_path_launches_directly_and_forwards_arguments() {
    let Some(path) = build_native("native_direct_bundle", common::HELLO_C) else {
        skip!("cannot build native bundle fixture")
    };
    let bundle = std::path::PathBuf::from(format!("{path}.app"));
    let contents = bundle.join("Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    std::fs::copy(&path, contents.join("MacOS/Guest")).unwrap();
    std::fs::write(contents.join("Info.plist"),
        "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleExecutable</key><string>Guest</string></dict></plist>")
        .unwrap();
    let output = common::driver_command()
        .arg(bundle)
        .arg("bundle")
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Hello, bundle (6)! 2 1.50"));
}

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
    let object = machish::image::MachObject::parse_any(&bytes, rosemary::exec::native::HOST_SPEC)
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

/// The native backend links the base's libc++ of this host's architecture
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
        .env("ROSEMARY_STATS", "1")
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
    let (status, _, err) = run_guest_with(&["--emulated=x86-64-strict"], &path, &[]);

    if HOST_RUNS_X86_64 {
        // The "native" fixture is an x86-64 one here, so it is also a fixture
        // the lifter accepts, and forcing emulation recompiles it.
        assert_eq!(status, Some(0), "the lifter accepts it: {err}");
        return;
    }
    assert_eq!(status, Some(1));
    assert!(err.contains("emulated backend"), "{err}");

    // Without `strict`, a binary with no x86-64 slice runs natively, as
    // Rosetta 2 runs an arm64-only child of a translated process.
    let (status, _, err) = run_guest_with(&["--emulated"], &path, &[]);
    assert_eq!(status, Some(0), "{err}");
}

/// `/bin/ls` is x86-64 plus arm64e. The arm64e slice runs natively: its
/// authenticated pointers are stored plain, which an arm64 process - one the
/// kernel gives no PAC keys - treats exactly like signed ones.
#[test]
fn an_arm64e_slice_runs_on_the_native_backend() {
    if !common::host_macho("/bin/ls") {
        skip!("no host Mach-O at /bin/ls")
    }
    let (status, out, err) = run_guest_with(&["--native"], "/bin/ls", &["/bin"]);
    assert_eq!(status, Some(0), "stderr: {err}");
    assert!(out.lines().any(|l| l == "bash"), "{out}");
}
