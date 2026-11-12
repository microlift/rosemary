use std::path::PathBuf;
use std::process::Command;

#[test]
fn every_corefoundation_declaration_is_exported() {
    if let Err(error) = sdk::corefoundation().files() {
        panic!("{error}");
    }
}

/// Needs a clang that targets Darwin. Without one this test checks nothing,
/// and says so.
#[test]
fn corefoundation_headers_compile_for_both_architectures() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipped: no clang on PATH");
        return;
    }
    let root = std::env::temp_dir().join(format!("rosemary-sdk-{}", std::process::id()));
    for (path, text) in sdk::corefoundation().files().unwrap() {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let source: PathBuf = root.join("use.c");
    std::fs::write(
        &source,
        "#include <CoreFoundation/CoreFoundation.h>\n\
         CFIndex length(void) { return CFStringGetLength(CFSTR(\"literal\")); }\n",
    )
    .unwrap();

    for arch in ["x86_64", "arm64"] {
        let output = Command::new("clang")
            .args(["-target", &format!("{arch}-apple-macos26.0"), "-isysroot"])
            .arg(&root)
            .args(["-fsyntax-only", "-Wall", "-Wextra", "-Werror", "-std=c99"])
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arch}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
