mod common;

const SOURCE: &str = r#"
#include <spawn.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <crt_externs.h>

static int child(int argc, char **argv) {
    char cwd[1024];
    sigset_t mask;
    int many = !strcmp(argv[1], "child-many");
    if (argc != (many ? 12 : 4) || strcmp(argv[2], "hello child")) return 21;
    if (many && (strcmp(argv[4], "first extra") || strcmp(argv[11], "last stack argument"))) return 25;
    if (!strcmp(argv[1], "child-explicit") &&
        (getenv("SPAWN_PARENT_ONLY") || !getenv("EXECLE_ENV") || strcmp(getenv("EXECLE_ENV"), "explicit"))) return 26;
    if (!getenv("SPAWN_TOKEN") || strcmp(getenv("SPAWN_TOKEN"), "present")) return 22;
    if (!getcwd(cwd, sizeof cwd) || strcmp(cwd, argv[3])) return 23;
    if (sigprocmask(SIG_BLOCK, NULL, &mask) || !sigismember(&mask, SIGUSR1)) return 24;
    puts("child arguments, environment and cwd preserved");
    return 17;
}

int main(int argc, char **argv) {
    if (argc > 1 && !strncmp(argv[1], "child", 5)) return child(argc, argv);
    if (argc != 3) return 10;
    const char *mode = argv[1];
    const char *directory = argv[2];
    char *args[] = { "custom argv zero", "child", "hello child", (char *)directory, NULL };
    if (setenv("SPAWN_TOKEN", "present", 1) || setenv("ROSEMARY_STATS", "1", 1)) return 11;
    if (setenv("ROSEMARY_TRACE_PREFIX", "1", 1)) return 16;
    if (setenv("PATH", directory, 1) || setenv("SPAWN_PARENT_ONLY", "unwanted", 1)) return 18;
    const char *name = strrchr(argv[0], '/');
    name = name ? name + 1 : argv[0];
    char **env = *_NSGetEnviron();
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    if (sigprocmask(SIG_BLOCK, &mask, NULL)) return 12;

    pid_t pid;
    if (!strncmp(mode, "exec", 4) || !strcmp(mode, "vfork") || !strcmp(mode, "setexec")) {
        pid = !strcmp(mode, "vfork") ? vfork() : fork();
        if (pid == 0) {
            if (chdir(directory)) _exit(13);
            if (!strcmp(mode, "execl-many")) {
                execl(argv[0], args[0], "child-many", args[2], args[3],
                    "first extra", "two", "three", "four", "five", "six", "seven", "last stack argument", (char *)NULL);
            }
            else if (!strcmp(mode, "execl")) execl(argv[0], args[0], args[1], args[2], args[3], (char *)NULL);
            else if (!strcmp(mode, "execlp")) execlp(name, args[0], args[1], args[2], args[3], (char *)NULL);
            else if (!strcmp(mode, "execle")) {
                char *custom_env[] = {
                    "SPAWN_TOKEN=present", "ROSEMARY_STATS=1", "ROSEMARY_TRACE_PREFIX=1",
                    "ROSEMARY_PREFIX=off", "EXECLE_ENV=explicit", NULL
                };
                execle(argv[0], args[0], "child-explicit", args[2], args[3], (char *)NULL, custom_env);
            }
            else if (!strcmp(mode, "execv")) execv(argv[0], args);
            else if (!strcmp(mode, "execvp")) execvp(name, args);
            else if (!strcmp(mode, "setexec")) {
                posix_spawnattr_t attr;
                if (posix_spawnattr_init(&attr) || posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETEXEC)) _exit(19);
                posix_spawn(&pid, argv[0], NULL, &attr, args, env);
            }
            else execve(argv[0], args, env);
            _exit(14);
        }
        if (pid < 0) return 15;
    } else {
        posix_spawn_file_actions_t actions;
        posix_spawnattr_t attr;
        short flags;
        sigset_t read_mask;
        if (posix_spawn_file_actions_init(&actions) || posix_spawnattr_init(&attr)) return 30;
        int dirfd = -1;
        if (!strcmp(mode, "fchdir")) {
            dirfd = open(directory, O_RDONLY);
            if (dirfd < 0 || posix_spawn_file_actions_addfchdir_np(&actions, dirfd)) return 31;
            if (posix_spawn_file_actions_addclose(&actions, dirfd)) return 37;
        } else if (posix_spawn_file_actions_addchdir_np(&actions, directory)) return 31;
        if (posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETSIGMASK)) return 32;
        if (posix_spawnattr_setsigmask(&attr, &mask)) return 33;
        if (posix_spawnattr_getflags(&attr, &flags) || flags != POSIX_SPAWN_SETSIGMASK) return 34;
        if (posix_spawnattr_getsigmask(&attr, &read_mask) || !sigismember(&read_mask, SIGUSR1)) return 35;
        int result = !strcmp(mode, "spawnp")
            ? posix_spawnp(&pid, name, &actions, &attr, args, env)
            : posix_spawn(&pid, argv[0], &actions, &attr, args, env);
        if (dirfd >= 0) close(dirfd);
        posix_spawnattr_destroy(&attr);
        posix_spawn_file_actions_destroy(&actions);
        if (result) { printf("spawn error=%d\n", result); return 36; }
    }
    int status;
    if (waitpid(pid, &status, 0) != pid || !WIFEXITED(status) || WEXITSTATUS(status) != 17) return 40;
    puts("parent received child exit status");
    return 0;
}
"#;

fn spawn_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            // A universal child can choose native on Apple Silicon if --emulated is lost.
            let extra = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                &["-arch", "arm64"][..]
            } else {
                &[][..]
            };
            common::build_x86_64("spawn-child", SOURCE, extra)
        })
        .clone()
}

#[cfg(target_os = "macos")]
#[test]
fn child_launches_preserve_the_selected_libcs_environment_and_path() {
    let Some(emulated) = spawn_fixture() else {
        skip!("cannot build x86_64 spawn fixtures");
    };
    let Some(native) = common::build_native("spawn-native-env", SOURCE) else {
        skip!("cannot build native spawn fixtures");
    };
    for (path, flags) in [
        (&emulated, &["--emulated"][..]),
        (&native, &["--native"][..]),
    ] {
        let directory = std::path::Path::new(path)
            .parent()
            .unwrap()
            .to_str()
            .unwrap();
        for libc in ["native", "builtin"] {
            for mode in ["execv", "execvp", "execlp", "spawnp"] {
                let output = common::driver_command()
                    .env("ROSEMARY_PREFIX", "off")
                    .env("ROSEMARY_LIBC", libc)
                    .env_remove("ROSEMARY_STATS")
                    .env_remove("ROSEMARY_TRACE_PREFIX")
                    // The guest sets both for its child, which must be allowed.
                    .env(
                        "ROSEMARY_FORWARD_ENV_VARS",
                        "ROSEMARY_STATS,ROSEMARY_TRACE_PREFIX",
                    )
                    .arg("run")
                    .args(flags)
                    .args([path, mode, directory])
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{libc} {mode} {flags:?}: {output:?}"
                );
                assert!(
                    String::from_utf8_lossy(&output.stdout)
                        .contains("child arguments, environment and cwd preserved")
                );
            }
        }
    }
}

#[test]
fn child_launches_use_rosemary_and_preserve_explicit_emulation() {
    let Some(path) = spawn_fixture() else {
        skip!("cannot build Mach-O spawn fixtures")
    };
    let directory = std::path::Path::new(&path)
        .parent()
        .unwrap()
        .to_str()
        .unwrap();
    for flags in [&[][..], &["--emulated"][..]] {
        for mode in [
            "spawn",
            "spawnp",
            "fchdir",
            "execve",
            "execv",
            "execvp",
            "vfork",
            "setexec",
            "execl",
            "execlp",
            "execle",
            "execl-many",
        ] {
            let output = common::driver_command()
                .env("ROSEMARY_PREFIX", "off")
                .env_remove("ROSEMARY_STATS")
                .env_remove("ROSEMARY_TRACE_PREFIX")
                // The guest sets both for its child, which must be allowed.
                .env(
                    "ROSEMARY_FORWARD_ENV_VARS",
                    "ROSEMARY_STATS,ROSEMARY_TRACE_PREFIX",
                )
                .arg("run")
                .args(flags)
                .args([&path, mode, directory])
                .output()
                .expect("run spawn fixture");
            let out = String::from_utf8_lossy(&output.stdout);
            let err = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{mode} {flags:?}: {out}\n{err}"
            );
            assert!(
                out.contains("child arguments, environment and cwd preserved"),
                "{out}"
            );
            assert!(out.contains("parent received child exit status"), "{out}");
            // Only the child enables prefix tracing and stats at startup.
            assert!(
                err.contains("prefix:"),
                "{mode}: child bypassed Rosemary: {err}"
            );
            if !flags.is_empty() {
                assert!(
                    err.contains("region entr(ies)"),
                    "{mode}: --emulated lost: {err}"
                );
            } else if cfg!(target_os = "macos") || cfg!(target_arch = "x86_64") {
                assert!(
                    !err.contains("region entr(ies)"),
                    "{mode}: automatic native selection lost: {err}"
                );
            }
        }
    }
}

const TASK_SOURCE: &str = r#"
#import <Foundation/Foundation.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc != 3) return 10;
    setenv("SPAWN_TOKEN", "present", 1);
    setenv("ROSEMARY_STATS", "1", 1);
    setenv("ROSEMARY_TRACE_PREFIX", "1", 1);
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    if (sigprocmask(SIG_BLOCK, &mask, NULL)) return 11;
    @autoreleasepool {
        NSTask *task = [[NSTask alloc] init];
        [task setLaunchPath:[NSString stringWithUTF8String:argv[1]]];
        NSString *directory = [NSString stringWithUTF8String:argv[2]];
        [task setCurrentDirectoryPath:directory];
        [task setArguments:@[@"child", @"hello child", directory]];
        NSPipe *output = [NSPipe pipe];
        [task setStandardOutput:output];
        [task launch];
        [task waitUntilExit];
        NSData *data = [[output fileHandleForReading] readDataToEndOfFile];
        fwrite([data bytes], 1, [data length], stdout);
        return [task terminationStatus] == 17 ? 0 : 12;
    }
}
"#;

#[test]
fn framework_tasks_preserve_emulation_cwd_and_redirected_stdio() {
    let Some(child) = spawn_fixture() else {
        skip!("cannot build Mach-O spawn fixtures")
    };
    let Some(parent) = common::build_x86_64(
        "spawn-task",
        TASK_SOURCE,
        &["-x", "objective-c", "-framework", "Foundation"],
    ) else {
        skip!("cannot build Objective-C task fixtures")
    };
    let directory = std::path::Path::new(&child)
        .parent()
        .unwrap()
        .to_str()
        .unwrap();
    for libc in ["native", "builtin"] {
        let output = common::driver_command()
            .env("ROSEMARY_PREFIX", "off")
            .env("ROSEMARY_OBJC", "builtin")
            .env("ROSEMARY_LIBC", libc)
            .env("ROSEMARY_BUNDLED", "1")
            .env_remove("ROSEMARY_STATS")
            .env_remove("ROSEMARY_TRACE_PREFIX")
            // The guest sets both for its child, which must be allowed.
            .env(
                "ROSEMARY_FORWARD_ENV_VARS",
                "ROSEMARY_STATS,ROSEMARY_TRACE_PREFIX",
            )
            .args(["run", "--emulated", &parent, &child, directory])
            .output()
            .expect("run task fixture");
        let out = String::from_utf8_lossy(&output.stdout);
        let err = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{libc}: {out}\n{err}");
        assert!(
            out.contains("child arguments, environment and cwd preserved"),
            "{out}"
        );
        assert!(
            err.contains("region entr(ies)"),
            "NSTask lost --emulated: {err}"
        );
    }
}

#[cfg(target_arch = "aarch64")]
const ARCH_PARENT_SOURCE: &str = r#"
#include <spawn.h>
#include <stdio.h>
#include <sys/wait.h>
#include <crt_externs.h>

int main(int argc, char **argv) {
    if (argc != 2) return 10;
    char *args[] = { argv[1], NULL };
    pid_t pid;
    int status;
    if (posix_spawn(&pid, argv[1], NULL, NULL, args, *_NSGetEnviron())) return 11;
    if (waitpid(pid, &status, 0) != pid) return 12;
    printf("child status %d\n", WIFEXITED(status) ? WEXITSTATUS(status) : -1);
    return 0;
}
"#;

#[cfg(target_arch = "aarch64")]
const ARCH_CHILD_SOURCE: &str = r#"
#include <stdio.h>
#include <sys/sysctl.h>

int main(void) {
    int translated = -1;
    size_t size = sizeof translated;
    if (sysctlbyname("sysctl.proc_translated", &translated, &size, NULL, 0)) return 20;
    printf("child translated %d\n", translated);
    return 17;
}
"#;

/// Only a host whose native slices are not x86-64 has a child without one.
#[cfg(target_arch = "aarch64")]
#[test]
fn emulated_children_without_an_x86_64_slice_run_natively_unless_strict() {
    let Some(parent) = common::build_x86_64("spawn-arch-parent", ARCH_PARENT_SOURCE, &[]) else {
        skip!("cannot build x86_64 spawn fixtures")
    };
    let Some(child) = common::build_native("spawn-arch-child", ARCH_CHILD_SOURCE) else {
        skip!("cannot build native spawn fixtures")
    };
    let run = |mode: &str| {
        let output = common::driver_command()
            .env("ROSEMARY_PREFIX", "off")
            .args(["run", mode, &parent, &child])
            .output()
            .expect("run arch fixture");
        assert_eq!(output.status.code(), Some(0), "{mode}: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    for mode in ["--emulated", "--emulated=x86-64"] {
        let out = run(mode);
        assert!(out.contains("child translated 0"), "{mode}: {out}");
        assert!(out.contains("child status 17"), "{mode}: {out}");
    }
    let out = run("--emulated=x86-64-strict");
    assert!(!out.contains("child translated"), "{out}");
    assert!(out.contains("child status 1\n"), "{out}");
}
