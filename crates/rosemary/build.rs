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
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("freebsd") {
        return;
    }
    println!("cargo::rerun-if-env-changed=LOCALBASE");
    let base = std::env::var("LOCALBASE").unwrap_or_else(|_| "/usr/local".into());
    println!("cargo::rustc-link-search=native={base}/lib");
}
