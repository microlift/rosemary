mod common;

use common::{build_cpp, run_guest, run_native};

const THROW_CPP: &str = r#"
#include <cstdio>
#include <stdexcept>
#include <string>

struct Noisy {
    const char *tag;
    ~Noisy() { printf("~%s\n", tag); }
};

static void deeper() {
    Noisy n{"deeper"};
    throw std::runtime_error("thrown from deeper");
}

static void middle() {
    Noisy n{"middle"};
    deeper();
}

int main() {
    try {
        middle();
    } catch (const std::runtime_error &e) {
        printf("caught %s\n", e.what());
    }
    try {
        throw 42;
    } catch (int i) {
        printf("caught int %d\n", i);
    }
    printf("done\n");
    return 0;
}
"#;

fn fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT.get_or_init(|| build_cpp("throw", THROW_CPP)).clone()
}

/// The unwinder is mapped, and every `_Unwind_*` binding lands inside it.
/// Why the C++ fixtures in this file do not run, in one place.
///
/// A freestanding `std::string` or `std::mutex` would be *this repository's*
/// reimplementation wearing the standard's name, and a fixture built against one
/// would be testing it - so the honest answer on a host with no macOS SDK is that
/// there is no C++ standard library here, and these say so rather than saying
/// "no toolchain", which stopped being true. See `test/logs/test-hygiene.md`.
const CXX_STANDARD_LIBRARY_ABSENT: &str = "the fixture needs the C++ standard library headers (`<string>`, `<mutex>`), which no host without a macOS SDK has; see test/logs/test-hygiene.md";

#[test]
fn every_unwind_binding_lands_in_the_vendored_unwinder() {
    let Some(path) = fixture() else {
        skip!("{CXX_STANDARD_LIBRARY_ABSENT}");
    };
    let program = rosetta3::Program::load(&path, &[]).expect("load");

    let unwinder = program
        .report
        .images
        .iter()
        .find(|i| rosetta3::cxxlib::is_libunwind(&i.name))
        .expect(
            "the fixture throws and no libunwind image was loaded - nothing in the \
             graph names it, so it is there only if cxxlib puts it there",
        );
    let extent: Vec<std::ops::Range<u64>> = unwinder
        .segments
        .iter()
        .map(|s| s.vmaddr..s.vmaddr.wrapping_add(s.vmsize))
        .collect();
    assert!(!extent.is_empty(), "the libunwind image mapped no segments");

    let mut seen = 0;
    for b in program.report.bindings() {
        if !b.symbol.starts_with("__Unwind_") {
            continue;
        }
        seen += 1;
        assert!(
            extent.iter().any(|r| r.contains(&b.resolved)),
            "{} resolved to {:#x}, outside the vendored libunwind - it went to the \
             host, which would walk the host's stack",
            b.symbol,
            b.resolved
        );
    }
    assert!(
        seen > 0,
        "no `_Unwind_*` binding at all, so this test cannot see the bug it exists for"
    );
    assert!(
        program
            .report
            .bindings()
            .any(|b| b.symbol == "__Unwind_RaiseException"),
        "the throw path's entry point is not among the bindings"
    );
}

/// And the throw actually runs, with the destructors in the right order.
#[test]
fn the_throw_fixture_agrees_with_itself() {
    let Some(path) = fixture() else {
        skip!("{CXX_STANDARD_LIBRARY_ABSENT}");
    };
    let Some((native_status, native)) = run_native(&path) else {
        skip!("cannot run an x86_64 binary natively");
    };
    let (status, stdout, stderr) = run_guest(&path, &[]);
    assert_eq!(status, native_status, "exit status differs\n{stderr}");
    assert_eq!(stdout, native, "output differs\n{stderr}");
}
