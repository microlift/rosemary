mod common;

use std::path::Path;

use common::{run_guest, run_guest_with};

#[test]
#[cfg(target_os = "macos")]
fn objc_debug_rejects_the_native_objc_runtime_before_loading_the_guest() {
    let output = common::driver_command()
        .args(["run", "--objc-debug", "/etc/hosts"])
        .env("ROSETTA3_OBJC", "native")
        .output()
        .expect("running the driver");
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("--objc-debug requires"), "{err}");
    assert!(err.contains("ROSETTA3_OBJC=builtin"), "{err}");
}

#[test]
#[cfg(not(target_os = "macos"))]
fn objc_debug_is_accepted_with_the_default_builtin_runtime() {
    let output = common::driver_command()
        .args(["run", "--objc-debug", "/etc/hosts"])
        .output()
        .expect("running the driver");
    assert_eq!(output.status.code(), Some(1));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(!err.contains("usage:"), "{err}");
    assert!(!err.contains("--objc-debug requires"), "{err}");
    // The last two are the claim: `--objc-debug` was *accepted* and the run
    // carried on far enough to try to load the file. Searched for rather than
    // required at the start of stderr, for the reason
    // `an_untranslatable_stop_names_itself_instead_of_crashing` gives - and on
    // this path it happens to be the first line, because the option is refused
    // before the loader publishes anything, which is itself worth not relying
    // on: publishing is not this test's business and may move.
    assert!(
        err.lines()
            .any(|line| line.starts_with("rosetta3: /etc/hosts:")),
        "{err}"
    );
}

#[test]
fn a_hello_world_prints_and_exits_zero() {
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(
        out, "Hello, world (5)! 1 1.50\ndone\ngoodbye\n",
        "stderr: {err}"
    );
}

#[test]
fn a_guest_exit_status_reaches_the_shell() {
    // `main` returns `abs(argc) - 1`, so one argument makes it 1. A runtime
    // that always exited 0 would pass every other test here.
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, _, err) = run_guest(&path, &["someone"]);
    assert_eq!(status, Some(1), "stderr: {err}");
}

#[test]
fn arguments_reach_the_guest() {
    let Some(path) = common::hello() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (_, out, err) = run_guest(&path, &["rosetta"]);
    assert!(
        out.starts_with("Hello, rosetta (7)! 2 1.50\n"),
        "{out:?} (stderr: {err})"
    );
}

#[test]
fn the_host_calls_a_guest_comparator_and_a_guest_exit_handler() {
    // The host->guest thunk, from both directions it is reached from: `qsort`
    // calls back during the call, `atexit`'s handler after `main` has returned
    // and from inside the host's own `exit`.
    let Some(path) = common::qsort_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, "12345789 calls=yes\nbye\n", "stderr: {err}");
}

#[test]
fn initializers_run_before_main_and_in_order() {
    let Some(path) = common::ctor_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, "order=abc\n", "stderr: {err}");
}

#[test]
fn a_destructor_runs_at_exit_in_one_list_with_the_guests_atexit_handler() {
    // `__cxa_atexit`, reached the way every real guest reaches it: the source
    // says `__attribute__((destructor))` and clang registers it from a
    // constructor. The order is the claim - the destructor is registered before
    // `main`, the `atexit` handler during it, and one reversed list prints them
    // in this order and no other.
    let Some(path) = common::dtor_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, "main\natexit\ndestructor\n", "stderr: {err}");
}

#[test]
fn a_callback_is_translated_after_the_main_threads_tls_is_gone() {
    // macOS destroys the main thread's TLS inside `exit`, so an exit handler
    // that hands the host a callback it has never seen makes the runtime lift
    // and compile from a thread where every Rust `thread_local!` is already
    // gone. Cranelift's `timing` feature is one - hence the feature list in
    // `Cargo.toml`. Failure is exit status 70 and no `late=` line.
    let Some(path) = common::late_callback_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stderr: {err}");
    assert_eq!(out, "main\nlate=12345\n", "stderr: {err}");
}

#[test]
fn fast_and_slow_imports_produce_the_same_program() {
    // Milestone 2 claimed promotion is an optimisation; this is the end-to-end
    // form of that claim. `--slow` sends every import through a VM exit and the
    // marshaller, `--fast` (the default) lets the lifter inline the ones it can.
    // Any observable difference means one of the two paths is wrong.
    let Some(path) = common::qsort_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };

    let (fast_status, fast_out, _) = run_guest(&path, &[]);
    let (slow_status, slow_out, slow_err) = run_guest_with(&["--slow", "--emulated"], &path, &[]);

    assert_eq!(fast_status, slow_status, "slow stderr: {slow_err}");
    assert_eq!(fast_out, slow_out);
}

#[test]
fn bin_ls_agrees_with_the_hosts_own_ls() {
    if !common::host_macho("/bin/ls") {
        skip!("no host Mach-O at /bin/ls");
    }
    // A directory with entries of both kinds, and content that is stable while
    // the test runs - `ls` of anything shared would race with the rest of the
    // system.
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ls-fixture");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("subdir")).expect("create fixture dir");
    for name in ["alpha", "beta", "gamma"] {
        std::fs::write(dir.join(name), name).expect("write fixture file");
    }
    let dir = dir.to_string_lossy().into_owned();

    // `-l` as well as the bare form: the long listing pulls in `strftime`,
    // `getpwuid`, `snprintf` and the terminal width, so it exercises far more
    // of the import surface than the names alone.
    for flags in [&[][..], &["-1"][..], &["-la"][..]] {
        let mut args: Vec<&str> = flags.to_vec();
        args.push(&dir);

        let host = std::process::Command::new("/bin/ls")
            .args(&args)
            .output()
            .expect("running the host's ls");
        let (status, out, err) = run_guest("/bin/ls", &args);

        assert_eq!(status, host.status.code(), "ls {args:?}: stderr: {err}");
        assert_eq!(
            out,
            String::from_utf8_lossy(&host.stdout),
            "ls {args:?} differs from the host's own output"
        );
    }
}

#[test]
fn an_untranslatable_stop_names_itself_instead_of_crashing() {
    // The other half of "it runs": when it does not, the reason is a sentence.
    // `/usr/bin/true` is not a Mach-O this loader accepts on every system, so
    // the case chosen is one that is always available and always wrong - a
    // path that is not an executable at all.
    //
    // The sentence is looked for *anywhere* on stderr, and it used to be
    // required to be the first line. That was an assumption about a different
    // component's scheduling, and it was wrong: `darwin::data_unavailable`
    // announces its 504 zeroed data refusals from the loader's first publish,
    // which happens while the executable is still being opened, so the banner is
    // genuinely on stderr first. The test's claim is about *this* stop, not
    // about being the first thing the process says - and the banner is a real
    // notice that has to be out before the guest runs, so it is not the thing
    // to move.
    let (status, out, err) = run_guest("/etc/hosts", &[]);
    assert_eq!(status, Some(1));
    assert!(out.is_empty());

    let refusal = err
        .lines()
        .find(|line| line.starts_with("rosetta3: /etc/hosts:"))
        .unwrap_or_else(|| panic!("the stop did not name the file it could not run:\n{err}"));
    // And it says *why*, rather than echoing the path back: a named refusal
    // with no reason in it is the shape of the failure this test exists to
    // catch, and `not a Mach-O file (magic 0x74532023)` is the reason.
    assert!(
        refusal.len() > "rosetta3: /etc/hosts: ".len(),
        "the refusal names the file and nothing else: {refusal:?}"
    );
}

#[test]
fn the_va_list_family_prints_what_the_host_libc_prints() {
    let Some(path) = common::valist_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let Some((native_status, expected)) = common::run_native(&path) else {
        skip!("cannot run x86_64 binaries natively")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(out, expected, "stderr: {err}");
    assert_eq!(status, native_status, "stderr: {err}");
    // Named as well as diffed: the ninth argument of each class is the one in
    // the overflow area, and it is what a walker with the wrong step size gets
    // wrong first.
    assert!(out.contains("ints 1 2 3 4 5 6 7 8 9\n"), "{out}");
    assert!(
        out.contains("fps 1.5 2.5 3.5 4.5 5.5 6.5 7.5 8.5 9.5\n"),
        "{out}"
    );
}

#[test]
fn the_guest_loads_an_intel_dylib_of_its_own_and_calls_into_it() {
    let Some(path) = common::dlopen_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest(&path, &[]);

    assert_eq!(status, Some(0), "stdout: {out:?} stderr: {err}");
    assert_eq!(
        out, "plugin loaded 42\nlen=6\nmissing=1 reported=1\n",
        "stderr: {err}"
    );
}

/// The native backend uses the same guest dyld implementation, but has no VM
/// to run a loaded image's constructor. The active [`rosetta3::Execution`]
/// must dispatch that call through [`rosetta3::Native`] instead.
#[test]
fn the_native_guest_loads_a_dylib_and_runs_its_constructor() {
    if !cfg!(target_arch = "x86_64") {
        skip!("the x86-64 fixture is not native on this host")
    }
    let Some(path) = common::dlopen_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let (status, out, err) = run_guest_with(&["--native"], &path, &[]);

    assert_eq!(status, Some(0), "stdout: {out:?} stderr: {err}");
    assert_eq!(
        out, "plugin loaded 42\nlen=6\nmissing=1 reported=1\n",
        "stderr: {err}"
    );
}

#[test]
fn a_dynamically_loaded_image_runs_the_same_bridged_and_exited() {
    // The falsifiable half of the lift-time import registry, and the reason it
    // is worth stating separately from the fixture's own expectations.
    //
    // Under `--slow` there is no registry and no bridge: the plugin's call sites
    // are unknown to the lifter, so each one leaves microlift as an
    // `UnknownJump` and is recognised by the run loop's own lookup before it
    // enters the region. Under the default the same sites are recognised *while
    // the plugin is lifted* and become inline calls to
    // [`rosetta3::bridge::import_call`].
    //
    // Two entirely different routes to the same host function, which is exactly
    // the shape a silent divergence would hide in - a stale register reloaded
    // around the bridge, a return address pushed twice, a `dlsym`ed pointer
    // dispatched as host code on one path and guest code on the other. The
    // program is small enough that any of those changes what it prints.
    let Some(path) = common::dlopen_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };

    let (fast_status, fast_out, fast_err) = run_guest(&path, &[]);
    let (slow_status, slow_out, slow_err) = run_guest_with(&["--slow", "--emulated"], &path, &[]);

    assert_eq!(fast_status, Some(0), "stderr: {fast_err}");
    assert_eq!(slow_status, Some(0), "stderr: {slow_err}");
    assert_eq!(fast_out, slow_out);
}

#[test]
fn the_mode_a_guest_opens_with_is_the_mode_the_file_gets() {
    let Some(path) = common::open_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };

    for extra in [&["--emulated"][..], &["--slow", "--emulated"][..]] {
        let dir = std::env::temp_dir().join(format!("rosetta3-open-mode{}", extra.len()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a directory the guest can write to");

        let (status, out, err) = run_guest_with(extra, &path, &[dir.to_str().unwrap()]);
        assert_eq!(status, Some(0), "stdout: {out:?} stderr: {err}");
        // `sem 110`: the semaphore was created holding exactly the two the
        // guest asked for, which is the pair of optional arguments a variadic
        // callee would have looked for on the stack.
        assert_eq!(
            out, "open ok\nreopen ok\nopenat ok\nsem 110\n",
            "stderr: {err}"
        );

        assert_eq!(mode_of(&dir.join("created")), 0o604, "open's mode");
        assert_eq!(mode_of(&dir.join("created-at")), 0o641, "openat's mode");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A file's permission bits, as the guest's `umask(0)` left them.
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o7777
}
