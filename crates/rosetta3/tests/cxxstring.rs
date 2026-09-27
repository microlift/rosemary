mod common;

use common::{build_cpp, run_guest, run_native};

const STRING_CPP: &str = r#"
#include <cstdio>
#include <cstring>
#include <string>

// Prints the object's own representation, leaving out the three things that
// legitimately differ and keeping everything that must not.
//
// Out: the heap address; the bytes past the terminator, which libc++ never
// initializes; and `capacity()`, which is implementation-defined
// ([string.capacity]) and really does differ - the vendored Intel libc++ rounds
// its allocations to sixteen where the host's system copy rounds to eight, so a
// 23-character string is `capacity() == 31` on one and `25` on the other. That
// is a fact about two builds of libc++, not about this runtime, and asserting
// on it would be asserting that they are the same build.
//
// In: the size, the long/short flag, the characters, and - for a short string -
// the object's own bytes. Plus `consistent`, which is what the capacity column
// was really guarding: that the capacity word round-trips through the bit the
// long/short flag lives in. A capacity of exactly `__min_cap` would be stored
// as an odd `cap+1` and read back one short, and that is a bug in *any*
// implementation rather than a difference between two.
static void dump(const char *tag, const std::string &s) {
    const unsigned char *p = (const unsigned char *)&s;
    int is_long = p[0] & 1;
    unsigned long long cap, size;
    memcpy(&cap, p, 8);
    memcpy(&size, p + 8, 8);
    int consistent = is_long
        ? (s.capacity() == (cap & ~1ULL) - 1 && s.size() == size && s.capacity() >= s.size())
        : (s.capacity() == 22 && s.size() == (p[0] >> 1));
    printf("%-12s %-3zu %d %d [%s]", tag, s.size(), is_long, consistent, s.c_str());
    if (!is_long) {
        for (size_t i = 0; i <= s.size() + 1; i++) printf(" %02x", p[i]);
    }
    printf("\n");
}

int main() {
    // __init(const char *, size_t), short and long.
    std::string a("google/protobuf");
    dump("init-short", a);
    std::string b("0123456789012345678901234567890123456789");
    dump("init-long", b);

    // The short/long boundary, from both sides.
    std::string edge(22, 'e');
    dump("edge-22", edge);
    std::string over(23, 'o');
    dump("edge-23", over);

    // push_back across the boundary, which is what calls __grow_by.
    std::string grow;
    for (int i = 0; i < 60; i++) {
        grow.push_back('a' + (i % 26));
        if (i == 21 || i == 22 || i == 24 || i == 30 || i == 46 || i == 47) dump("grow", grow);
    }

    // append, in each of its four spellings.
    std::string ap("head");
    ap.append("-tail");
    dump("append-cstr", ap);
    ap.append("-counted-and-more", 8);
    dump("append-n", ap);
    ap.append(b, 3, 5);
    dump("append-str", ap);
    ap.append(9, 'z');
    dump("append-fill", ap);

    // assign, both spellings, growing and then shrinking in place.
    std::string as("x");
    as.assign("a rather long replacement that will not fit inside the object");
    dump("assign-cstr", as);
    // Shrinking must not reallocate: the pointer the guest already holds has to
    // stay valid, so this records whether it did.
    const char *held = as.c_str();
    as.assign("short", 5);
    dump("assign-n", as);
    printf("stable=%d\n", held == as.c_str());

    // Editing.
    std::string ed("0123456789");
    ed.erase(2, 3);
    dump("erase", ed);
    ed.insert(3, "INS");
    dump("insert", ed);
    ed.replace(1, 4, "REPLACEMENT");
    dump("replace", ed);
    ed.resize(4, '.');
    dump("resize-down", ed);
    ed.resize(30, '.');
    dump("resize-up", ed);
    ed.reserve(100);
    dump("reserve", ed);

    // Copy construction, assignment, and the substring constructor.
    std::string c1(b);
    dump("copy-long", c1);
    std::string c2(a);
    dump("copy-short", c2);
    std::string sub(b, 4, 30);
    dump("substr", sub);
    std::string sub2(a, 2, 4);
    dump("substr-short", sub2);
    c2 = b;
    dump("assign-str", c2);

    // Inspection.
    printf("at=%c%c find=%zu %zu rfind=%zu %zu\n", a.at(0), a.at(6),
           a.find('/'), a.find('Q'), a.rfind('o'), a.rfind('Q'));
    printf("cmp=%d %d %d %d\n", a.compare(0, 6, "google", 6),
           a.compare(0, 6, "googlf", 6), a.compare(0, 6, "goo", 3),
           a.compare(0, 2, "google", 6));

    // The two that return a string by value.
    std::string plus = "prefix-" + a;
    dump("operator+", plus);
    dump("to_string", std::to_string(1234567890u));
    dump("to_string-i", std::to_string(-42));

    // wstring: the same code with four-byte characters, so the min_cap of 5 and
    // the data offset of 4 are exercised rather than assumed.
    std::wstring w(4, L'w');
    printf("w-short %zu %d\n", w.size(), w.capacity() >= w.size());
    std::wstring w2(9, L'w');
    printf("w-long %zu %d\n", w2.size(), w2.capacity() >= w2.size());
    w2.push_back(L'!');
    w2.append(L"abcdef", 6);
    w2.resize(40, L'.');
    printf("w-grown %zu %d %d\n", w2.size(), w2.capacity() >= w2.size(), (int)w2[39]);
    std::wstring w3(w2);
    printf("w-copy %zu %d\n", w3.size(), (int)w3[0]);
    return 0;
}
"#;

fn fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_cpp("cxxstring", STRING_CPP))
        .clone()
}

/// Why the C++ fixtures in this file do not run, in one place.
///
/// A freestanding `std::string` or `std::mutex` would be *this repository's*
/// reimplementation wearing the standard's name, and a fixture built against one
/// would be testing it - so the honest answer on a host with no macOS SDK is that
/// there is no C++ standard library here, and these say so rather than saying
/// "no toolchain", which stopped being true. See `test/logs/test-hygiene.md`.
const CXX_STANDARD_LIBRARY_ABSENT: &str = "the fixture needs the C++ standard library headers (`<string>`, `<mutex>`), which no host without a macOS SDK has; see test/logs/test-hygiene.md";

#[test]
fn the_same_binary_agrees_with_itself() {
    let Some(path) = fixture() else {
        skip!("{CXX_STANDARD_LIBRARY_ABSENT}");
    };
    let Some((native_status, native)) = run_native(&path) else {
        skip!("cannot run an x86_64 binary natively");
    };
    let (status, stdout, stderr) = run_guest(&path, &[]);
    assert_eq!(status, native_status, "exit status differs\n{stderr}");
    assert_eq!(
        stdout, native,
        "output differs.\n--- rosetta3 ---\n{stdout}\n--- native ---\n{native}\n\
         --- stderr ---\n{stderr}"
    );
}

#[test]
fn the_slow_path_agrees_too() {
    let Some(path) = fixture() else {
        skip!("{CXX_STANDARD_LIBRARY_ABSENT}");
    };
    let (status, fast, fast_err) = run_guest(&path, &[]);
    let (slow_status, slow, slow_err) =
        common::run_guest_with(&["--slow", "--emulated"], &path, &[]);
    assert_eq!(
        status, slow_status,
        "exit status differs\n{fast_err}{slow_err}"
    );
    assert_eq!(fast, slow, "--slow differs\n{fast_err}{slow_err}");
}

#[test]
fn every_string_call_lands_in_the_vendored_libcxx() {
    let Some(path) = fixture() else {
        skip!("{CXX_STANDARD_LIBRARY_ABSENT}");
    };
    let program = rosetta3::Program::load(&path, &[]).expect("load");

    // The image is present and is the one this crate supplies, under the name
    // the guest asked for.
    let libcxx = program
        .report
        .images
        .iter()
        .find(|i| rosetta3::cxxlib::is_libcxx(&i.name))
        .expect("the fixture links libc++ and no libc++ image was loaded");
    let extent: Vec<std::ops::Range<u64>> = libcxx
        .segments
        .iter()
        .map(|s| s.vmaddr..s.vmaddr.wrapping_add(s.vmsize))
        .collect();
    assert!(!extent.is_empty(), "the libc++ image mapped no segments");

    let mut seen = 0;
    for b in program.report.bindings() {
        if !b.symbol.contains("basic_string") {
            continue;
        }
        seen += 1;
        assert!(
            extent.iter().any(|r| r.contains(&b.resolved)),
            "{} resolved to {:#x}, which is outside the vendored libc++ - it went \
             to the host, which is the bug this exists to prevent",
            b.symbol,
            b.resolved
        );
    }
    assert!(seen > 0, "the fixture imported no string entry points");
}

const MUTEX_CPP: &str = r#"
#include <cstdio>
#include <mutex>

static std::mutex m;

int main() {
    m.lock();
    m.unlock();
    std::lock_guard<std::mutex> guard(m);
    printf("locked and unlocked\n");
    return 0;
}
"#;

fn mutex_fixture() -> Option<String> {
    static BUILT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| build_cpp("cxxmutex", MUTEX_CPP))
        .clone()
}

#[test]
fn no_inlined_import_targets_guest_memory() {
    let Some(path) = mutex_fixture() else {
        skip!("{CXX_STANDARD_LIBRARY_ABSENT}");
    };
    // `load_executable`, not `load`: the question is about real mappings, and
    // the inspectable form places images nominally.
    let program = rosetta3::Program::load_executable(&path, &[]).expect("load");
    let config = program.lifter_config();

    let mut promoted_into_libcxx = 0;
    for (&site, kind) in config.imports.iter() {
        let microlift::core::bridge::ImportKind::Fast(import) = kind else {
            continue;
        };
        let target = import.symbol.value as usize as u64;
        assert!(
            !program.is_guest_address(target),
            "the call site at {site:#x} would inline a native call to {} at {target:#x}, \
             which is guest memory and is not executable",
            import.symbol.name
        );
        promoted_into_libcxx += 1;
    }
    assert!(
        promoted_into_libcxx > 0,
        "nothing was promoted at all; the test proves nothing"
    );

    // And the other half: the describable libc++ entry points really are
    // present and really do resolve into the guest, so the assertion above had
    // something to catch.
    let guest_resolved = program
        .report
        .bindings()
        .filter(|b| b.symbol.contains("5mutex") && program.is_guest_address(b.resolved))
        .count();
    assert!(
        guest_resolved > 0,
        "the fixture imported no guest-resolved std::mutex entry points, so this \
         test cannot see the bug it exists for"
    );
}

/// The fixture above must actually run, not merely link.
#[test]
fn the_mutex_fixture_agrees_with_itself() {
    let Some(path) = mutex_fixture() else {
        skip!("{CXX_STANDARD_LIBRARY_ABSENT}");
    };
    let Some((native_status, native)) = run_native(&path) else {
        skip!("cannot run an x86_64 binary natively");
    };
    let (status, stdout, stderr) = run_guest(&path, &[]);
    assert_eq!(status, native_status, "exit status differs\n{stderr}");
    assert_eq!(stdout, native, "output differs\n{stderr}");
}
