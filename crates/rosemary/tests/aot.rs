mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

/// A cache directory of this test's own, emptied.
fn cache(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Every cache file under `dir`.
fn cache_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rmc") {
                out.push(path);
            }
        }
    }
    out
}

fn run(cache: &Path, path: &str, env: &[(&str, &str)]) -> Output {
    let mut command = common::driver_command();
    command.env("ROSEMARY_AOT_CACHE", cache);
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .args(["run", "--emulated", path, "1000"])
        .output()
        .expect("running the driver")
}

/// How many regions a traced run said it took from the cache.
fn adopted(stderr: &str) -> usize {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("rosemary: aot: "))
        .filter_map(|line| {
            line.rsplit_once(": ")?
                .1
                .split(' ')
                .next()?
                .parse::<usize>()
                .ok()
        })
        .sum()
}

#[test]
fn a_program_compiled_ahead_of_time_runs_from_its_cache() {
    let Some(path) = common::build_x86_64("aot-link", common::LINK_C, &[]) else {
        skip!("cannot build x86_64 fixtures")
    };
    let dir = cache("aot-cache-run");
    let built = common::driver_command()
        .env("ROSEMARY_AOT_CACHE", &dir)
        .args(["aot", &path])
        .output()
        .expect("running the driver");
    let report = String::from_utf8_lossy(&built.stdout);
    assert!(
        built.status.success() && report.contains("region(s) cached"),
        "{report}{}",
        String::from_utf8_lossy(&built.stderr)
    );
    assert_eq!(
        cache_files(&dir).len(),
        1,
        "one file for the one guest image"
    );
    let cached_regions: usize = report
        .split(": ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next()?.parse().ok())
        .expect("a region count");

    let plain = run(Path::new("off"), &path, &[]);
    let cached = run(&dir, &path, &[("ROSEMARY_TRACE_VM", "1")]);
    let stderr = String::from_utf8_lossy(&cached.stderr);
    assert_eq!(cached.status.code(), plain.status.code(), "{stderr}");
    assert_eq!(cached.stdout, plain.stdout);
    // The fixture folds in no data, so every region runs wherever the image lands.
    assert_eq!(adopted(&stderr), cached_regions, "{stderr}");
}

#[test]
fn a_run_with_nothing_cached_builds_the_cache_in_the_background() {
    let Some(path) = common::build_x86_64("aot-background", common::LINK_C, &[]) else {
        skip!("cannot build x86_64 fixtures")
    };
    let dir = cache("aot-cache-background");
    let out = run(&dir, &path, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while cache_files(&dir).is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "no cache file appeared in {}",
            dir.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn an_image_a_run_dlopened_is_compiled_and_runs_from_its_cache() {
    let Some(path) = common::dlopen_fixture() else {
        skip!("cannot build x86_64 fixtures")
    };
    let plugin = Path::new(&path).with_file_name("libplugin.dylib");
    let plugin = std::fs::canonicalize(plugin).expect("the fixture's plugin");
    let plugin = plugin.to_str().expect("a UTF-8 fixture path");
    let dir = cache("aot-cache-dlopen");
    let quiet = [("ROSEMARY_AOT_BACKGROUND", "0")];

    let first = run(&dir, &path, &quiet);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    let built = common::driver_command()
        .env("ROSEMARY_AOT_CACHE", &dir)
        .args(["aot", &path])
        .output()
        .expect("running the driver");
    let report = String::from_utf8_lossy(&built.stdout);
    assert!(
        built.status.success()
            && report
                .lines()
                .any(|line| line.starts_with(&format!("{plugin}: ")) && line.contains("cached")),
        "{report}{}",
        String::from_utf8_lossy(&built.stderr)
    );

    let cached = run(&dir, &path, &[quiet[0], ("ROSEMARY_TRACE_VM", "1")]);
    let stderr = String::from_utf8_lossy(&cached.stderr);
    assert_eq!(cached.status.code(), first.status.code(), "{stderr}");
    assert_eq!(cached.stdout, first.stdout);
    let prefix = format!("rosemary: aot: {plugin}: ");
    assert!(
        stderr.lines().any(|line| line
            .strip_prefix(&prefix)
            .and_then(|rest| rest.split(' ').next()?.parse::<usize>().ok())
            .is_some_and(|regions| regions > 0)),
        "{stderr}"
    );
}
