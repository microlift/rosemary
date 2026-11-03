use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GIT_HASH={}", hash.trim());

    let date = Command::new("date")
        .arg("+%Y-%m-%d")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=BUILD_DATE={}", date.trim());

    println!("cargo:rerun-if-changed=.git/HEAD");

    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo:rustc-env=ROSEMARY_BUILD_ID={}", build_id());

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("freebsd") {
        return;
    }
    println!("cargo::rerun-if-env-changed=LOCALBASE");
    let base = std::env::var("LOCALBASE").unwrap_or_else(|_| "/usr/local".into());
    println!("cargo::rustc-link-search=native={base}/lib");
}

/// What names the code this build emits, for the AOT cache (`src/aot`): this tree, and
/// the Microlift and Cranelift it was built against, each as a revision plus a hash
/// of any uncommitted sources.
fn build_id() -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lock = std::fs::read_to_string(workspace.join("Cargo.lock")).unwrap_or_default();
    let patches = std::fs::read_to_string(workspace.join(".cargo/config.toml")).unwrap_or_default();
    for watched in ["Cargo.lock", ".cargo/config.toml", "crates"] {
        println!(
            "cargo::rerun-if-changed={}",
            workspace.join(watched).display()
        );
    }

    let mut parts = vec![format!("rosemary {}", tree(&workspace))];
    for package in ["microlift", "cranelift-codegen"] {
        let revision = locked_revision(&lock, package)
            .or_else(|| {
                let path = workspace.join(patched_path(&patches, package)?);
                println!("cargo::rerun-if-changed={}", path.join("src").display());
                Some(tree(&path))
            })
            .unwrap_or_else(|| "unknown".into());
        parts.push(format!("{package} {revision}"));
    }
    format!("{:016x}", fnv(parts.join("\n").as_bytes()))
}

/// The commit `package` is locked to, from its `source = "git+...#<rev>"` line.
fn locked_revision(lock: &str, package: &str) -> Option<String> {
    let entry = lock.split("[[package]]").find(|entry| {
        entry
            .lines()
            .any(|line| line.trim() == format!("name = \"{package}\""))
    })?;
    let source = entry
        .lines()
        .find_map(|line| line.trim().strip_prefix("source = \"git+"))?;
    Some(source.rsplit_once('#')?.1.trim_end_matches('"').to_string())
}

/// Where `.cargo/config.toml` points `package`, relative to the workspace.
fn patched_path(config: &str, package: &str) -> Option<PathBuf> {
    let line = config
        .lines()
        .map(str::trim)
        .find(|line| !line.starts_with('#') && line.starts_with(&format!("{package} =")))?;
    let (_, rest) = line.split_once("path = \"")?;
    Some(PathBuf::from(rest.split_once('"')?.0))
}

/// `HEAD`, plus a hash of the tracked changes and untracked files when the tree is dirty.
fn tree(dir: &Path) -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| o.stdout)
    };
    let head = git(&["rev-parse", "HEAD"])
        .map(|out| String::from_utf8_lossy(&out).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let mut dirty = git(&["diff", "HEAD", "--binary"]).unwrap_or_default();
    let untracked = git(&["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    for name in untracked.split(|&b| b == 0).filter(|name| !name.is_empty()) {
        dirty.extend_from_slice(name);
        let path = dir.join(String::from_utf8_lossy(name).as_ref());
        dirty.extend(std::fs::read(path).unwrap_or_default());
    }
    if dirty.is_empty() {
        head
    } else {
        format!("{head}+{:016x}", fnv(&dirty))
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
