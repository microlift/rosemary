//! libmalloc conformance: the assertions of Apple's libmalloc test suite
//! (`tests/*.c` at libmalloc-812.100.31), plus behaviour its sources define
//! that no test pins down, run against this crate's zones through the entry
//! points a guest calls.
//!
//! Every check runs alone in a child process: some exercise what libmalloc
//! aborts on (double free, interior free) and may corrupt the heap, others
//! change the default zone.
//!
//! A failure not in [`KNOWN_DEVIATIONS`] fails the test. Environment:
//! `LIBMALLOC_STRICT=1` fails on those too, `LIBMALLOC_ONLY=a,b` selects checks,
//! `LIBMALLOC_JOBS=n` sets parallelism, `LIBMALLOC_REPORT=path` writes
//! JSON-lines results.
//!
//! ```text
//! cargo test -p darwin --test libmalloc_conformance -- --nocapture
//! ```
#![cfg(not(target_os = "macos"))]
#![allow(unsafe_op_in_unsafe_fn)]

use std::ffi::{CStr, c_char, c_uint, c_void};
use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use darwin::malloc::{self as m, KernReturn, MallocZone, Statistics, Task, VmRange};

const UPSTREAM: &str = "libmalloc-812.100.31";
const CHILD: &str = "LIBMALLOC_CONFORMANCE_CHECK";

/// Checks that fail by design: the differences from libmalloc that
/// `darwin::malloc`'s module documentation lists.
const KNOWN_DEVIATIONS: &[&str] = &[
    // `enumerator` reports KERN_FAILURE.
    "posix_memalign_enumerated",
    // `malloc_set_zone_name` always copies the name.
    "set_zone_name_literal",
    // A block handed to the wrong zone is freed or moved, not aborted on.
    "wrong_zone_free_aborts",
    "wrong_zone_realloc_aborts",
];

const KERN_SUCCESS: KernReturn = 0;
const MALLOC_PTR_IN_USE_RANGE_TYPE: c_uint = 1;
const OPTION_CLEAR: u64 = 1 << 0;
const OPTION_CANONICAL_TAG: u64 = 1 << 1;

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;
const GIB: usize = 1024 * MIB;

// The harness.

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An assertion of a test in upstream's `tests/`.
    Test,
    /// Behaviour upstream's `src/` defines, which no test there asserts.
    Source,
    /// What upstream does with a program's memory error.
    Diagnostic,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Test => "upstream tests",
            Kind::Source => "upstream behaviour",
            Kind::Diagnostic => "error detection",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The check returns, with every expectation met.
    Returns,
    /// libmalloc aborts the process (SIGABRT, or a trap) partway through.
    Aborts,
}

struct Check {
    name: &'static str,
    kind: Kind,
    /// Where upstream says so.
    source: &'static str,
    expect: Expect,
    timeout_secs: u64,
    run: fn(&mut T),
}

/// The expectations one check made: like darwintest's `T_EXPECT`, a failure is
/// recorded and the check goes on, so a report shows every kind of miss.
#[derive(Default)]
struct T {
    /// What failed, how often, and the first instance.
    failures: Vec<(&'static str, usize, String)>,
    notes: Vec<String>,
}

impl T {
    fn expect(&mut self, ok: bool, what: &'static str, detail: impl FnOnce() -> String) -> bool {
        if !ok {
            match self.failures.iter_mut().find(|f| f.0 == what) {
                Some(f) => f.1 += 1,
                None => self.failures.push((what, 1, detail())),
            }
        }
        ok
    }

    fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    fn summary(&self) -> String {
        let mut parts: Vec<String> = self
            .failures
            .iter()
            .map(|(what, n, first)| {
                if *n == 1 {
                    format!("{what}: {first}")
                } else {
                    format!("{what} (x{n}; first: {first})")
                }
            })
            .collect();
        parts.extend(self.notes.iter().cloned());
        parts.join("; ")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
}

struct Outcome {
    status: Status,
    detail: String,
    secs: f64,
}

#[test]
fn libmalloc_conformance() {
    if let Ok(name) = std::env::var(CHILD) {
        child(&name);
    }
    let only: Option<Vec<String>> = std::env::var("LIBMALLOC_ONLY")
        .ok()
        .map(|list| list.split(',').map(str::to_string).collect());
    let checks: Vec<&Check> = CHECKS
        .iter()
        .filter(|c| only.as_ref().is_none_or(|o| o.iter().any(|n| n == c.name)))
        .collect();
    let jobs: usize = std::env::var("LIBMALLOC_JOBS")
        .ok()
        .and_then(|j| j.parse().ok())
        .unwrap_or(4)
        .max(1);
    let exe = std::env::current_exe().unwrap();

    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Outcome>>> =
        Mutex::new((0..checks.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(check) = checks.get(i) else { break };
                    let outcome = run_in_child(&exe, check);
                    results.lock().unwrap()[i] = Some(outcome);
                }
            });
        }
    });
    let results: Vec<Outcome> = results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect();

    println!("\nlibmalloc conformance: darwin::malloc against {UPSTREAM}\n");
    for (check, outcome) in checks.iter().zip(&results) {
        println!(
            "  {}  {:<18}  {:<40}  {:<40} {:>6.1}s",
            match outcome.status {
                Status::Pass => "PASS",
                Status::Fail => "FAIL",
            },
            check.kind.label(),
            check.source,
            check.name,
            outcome.secs
        );
        if outcome.status == Status::Fail || !outcome.detail.is_empty() {
            for line in outcome.detail.split("; ") {
                println!("        {line}");
            }
        }
    }
    println!();
    let mut failed = 0;
    for kind in [Kind::Test, Kind::Source, Kind::Diagnostic] {
        let of_kind: Vec<_> = checks
            .iter()
            .zip(&results)
            .filter(|(c, _)| c.kind == kind)
            .collect();
        if of_kind.is_empty() {
            continue;
        }
        let passed = of_kind
            .iter()
            .filter(|(_, o)| o.status == Status::Pass)
            .count();
        failed += of_kind.len() - passed;
        println!("  {:<18}  {passed}/{} passed", kind.label(), of_kind.len());
    }
    let passed = results.iter().filter(|o| o.status == Status::Pass).count();
    println!("  {:<18}  {passed}/{} passed\n", "total", results.len());

    if let Ok(path) = std::env::var("LIBMALLOC_REPORT") {
        let mut file = std::fs::File::create(&path).unwrap();
        for (check, outcome) in checks.iter().zip(&results) {
            writeln!(
                file,
                "{{\"name\":{},\"kind\":{},\"source\":{},\"expect\":{},\"status\":{},\"secs\":{:.2},\"detail\":{}}}",
                json(check.name),
                json(check.kind.label()),
                json(check.source),
                json(match check.expect {
                    Expect::Returns => "returns",
                    Expect::Aborts => "aborts",
                }),
                json(match outcome.status {
                    Status::Pass => "pass",
                    Status::Fail => "fail",
                }),
                outcome.secs,
                json(&outcome.detail)
            )
            .unwrap();
        }
    }
    if std::env::var("LIBMALLOC_STRICT").is_ok() {
        assert_eq!(failed, 0, "{failed} checks deviate from libmalloc");
    }
    let unexpected: Vec<_> = checks
        .iter()
        .zip(&results)
        .filter(|(c, o)| o.status == Status::Fail && !KNOWN_DEVIATIONS.contains(&c.name))
        .map(|(c, _)| c.name)
        .collect();
    assert!(
        unexpected.is_empty(),
        "checks deviate from libmalloc: {unexpected:?}"
    );
}

fn json(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn run_in_child(exe: &std::path::Path, check: &Check) -> Outcome {
    let start = Instant::now();
    let mut child = Command::new(exe)
        .args([
            "--exact",
            "libmalloc_conformance",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, check.name)
        .env_remove("LIBMALLOC_ONLY")
        .env_remove("LIBMALLOC_REPORT")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let deadline = start + Duration::from_secs(check.timeout_secs);
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break (status, false);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            break (child.wait().unwrap(), true);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let secs = start.elapsed().as_secs_f64();
    let out = out.join().unwrap();
    let err = err.join().unwrap();
    let result = out
        .lines()
        .find_map(|l| l.split_once("@@RESULT ").map(|(_, r)| r));
    // `prog(pid,thread) malloc: message`, the first report.
    let reported = match err.lines().find_map(|l| l.split_once(") malloc: ")) {
        Some((_, text)) => format!("; reported: {text}"),
        None => String::new(),
    };

    if timed_out {
        return Outcome {
            status: Status::Fail,
            detail: format!("timed out after {}s{reported}", check.timeout_secs),
            secs,
        };
    }
    if let Some(signal) = status.signal() {
        let name = signal_name(signal);
        let aborted = matches!(signal, libc::SIGABRT | libc::SIGTRAP | libc::SIGILL);
        if check.expect == Expect::Aborts && aborted {
            return Outcome {
                status: Status::Pass,
                detail: format!("aborted with {name}{reported}")
                    .trim_start_matches("; ")
                    .to_string(),
                secs,
            };
        }
        let tail: String = err.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
        return Outcome {
            status: Status::Fail,
            detail: format!("killed by {name}; stderr: {tail}"),
            secs,
        };
    }
    match result {
        Some("pass") => Outcome {
            status: Status::Pass,
            detail: String::new(),
            secs,
        },
        Some(fail) => Outcome {
            status: Status::Fail,
            detail: format!("{}{reported}", fail.strip_prefix("fail ").unwrap_or(fail)),
            secs,
        },
        None => Outcome {
            status: Status::Fail,
            detail: format!(
                "no result; exit {:?}; stderr: {}",
                status.code(),
                err.trim()
            ),
            secs,
        },
    }
}

fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGABRT => "SIGABRT".into(),
        libc::SIGTRAP => "SIGTRAP".into(),
        libc::SIGILL => "SIGILL".into(),
        libc::SIGSEGV => "SIGSEGV".into(),
        libc::SIGBUS => "SIGBUS".into(),
        libc::SIGKILL => "SIGKILL".into(),
        n => format!("signal {n}"),
    }
}

fn child(name: &str) -> ! {
    let line = match CHECKS.iter().find(|c| c.name == name) {
        None => format!("@@RESULT fail no check named {name}"),
        Some(check) => {
            // As libSystem does before `main`.
            m::init();
            let mut t = T::default();
            (check.run)(&mut t);
            match check.expect {
                Expect::Returns if t.failures.is_empty() => "@@RESULT pass".to_string(),
                Expect::Returns => format!("@@RESULT fail {}", t.summary()),
                Expect::Aborts => {
                    let mut why = "returned where libmalloc aborts".to_string();
                    if !t.notes.is_empty() || !t.failures.is_empty() {
                        why = format!("{why}; {}", t.summary());
                    }
                    format!("@@RESULT fail {why}")
                }
            }
        }
    };
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", line.replace('\n', " "));
    let _ = out.flush();
    // SAFETY: ends the process without running anything a corrupt heap could
    // trip over.
    unsafe { libc::_exit(0) }
}

// Helpers.

/// xorshift64*, seeded per check so that a failure reproduces.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn null() -> *mut c_void {
    std::ptr::null_mut()
}

fn errno() -> i32 {
    darwin::errno::get()
}

fn set_errno(value: i32) {
    darwin::errno::set(value)
}

unsafe fn scribble(p: *mut c_void, n: usize) {
    std::ptr::write_bytes(p.cast::<u8>(), 0xbe, n);
}

unsafe fn all_bytes(p: *const c_void, n: usize, byte: u8) -> bool {
    n == 0
        || std::slice::from_raw_parts(p.cast::<u8>(), n)
            .iter()
            .all(|&b| b == byte)
}

fn at(p: *mut c_void, offset: usize) -> *mut c_void {
    p.cast::<u8>().wrapping_add(offset).cast()
}

/// The registered zones, through `malloc_get_all_zones` as a guest finds them.
unsafe fn zones() -> Vec<*mut MallocZone> {
    let mut addresses: *mut usize = std::ptr::null_mut();
    let mut count = 0;
    let kr = m::malloc_get_all_zones(0, None, &mut addresses, &mut count);
    assert_eq!(kr, KERN_SUCCESS);
    (0..count as usize)
        .map(|i| *addresses.add(i) as *mut MallocZone)
        .collect()
}

unsafe fn zone_name(zone: *mut MallocZone) -> Option<String> {
    let name = m::malloc_get_zone_name(zone);
    (!name.is_null()).then(|| CStr::from_ptr(name).to_string_lossy().into_owned())
}

unsafe fn stats(zone: *mut MallocZone) -> Statistics {
    let mut s = Statistics::default();
    m::malloc_zone_statistics(zone, &mut s);
    s
}

fn page_size() -> usize {
    // SAFETY: a plain query.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

unsafe fn mmap_anonymous(len: usize) -> *mut c_void {
    let p = libc::mmap(
        null(),
        len,
        libc::PROT_READ | libc::PROT_WRITE,
        libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
        -1,
        0,
    );
    assert_ne!(p, libc::MAP_FAILED);
    p
}

// tests/malloc_free_test.c

unsafe fn t_malloc(t: &mut T, s: usize) -> *mut c_void {
    let p = m::malloc(s);
    if !t.expect(!p.is_null(), "malloc returned NULL", || {
        format!("malloc({s})")
    }) {
        return p;
    }
    let size = m::malloc_size(p);
    t.expect(s <= size, "malloc_size below the request", || {
        format!("malloc_size(malloc({s})) = {size}")
    });
    scribble(p, s);
    p
}

unsafe fn malloc_free_range(t: &mut T, min: usize, max: usize, incr: usize) {
    let mut s = min;
    while s <= max {
        let p = t_malloc(t, s);
        m::free(p);
        s += incr;
    }
    let (mut s, mut u) = (min, max);
    while s <= max {
        let a = t_malloc(t, s);
        let b = t_malloc(t, u);
        m::free(a);
        m::free(b);
        s += incr;
        u = u.wrapping_sub(incr);
    }
}

/// Upstream's `test_malloc_free_random`, loop structure and all: each block
/// is freed on the iteration after the one that made it.
unsafe fn malloc_free_random(t: &mut T, min: usize, max: usize, incr: usize, n: usize) {
    const P: usize = 100;
    let r = (max - min) / incr;
    let mut rng = Rng::new(0x5eed ^ min as u64 ^ (n as u64) << 20);
    let mut ptrs = [null(); P];
    let (mut j, mut k) = (0, 0);
    for i in 0..n + P {
        let p = if i < n {
            t_malloc(t, min + rng.below(r) * incr)
        } else {
            null()
        };
        m::free(ptrs[j]);
        ptrs[k] = p;
        j = k;
        k = (k + 1) % P;
    }
}

fn malloc_free_tiny(t: &mut T) {
    unsafe {
        malloc_free_range(t, 0, 1008, 16);
        malloc_free_random(t, 0, 1008, 16, 10_000);
    }
}

fn malloc_free_small(t: &mut T) {
    unsafe {
        malloc_free_range(t, 1024, 256 * KIB, 512);
        malloc_free_random(t, 1024, 256 * KIB, 512, 100_000);
    }
}

fn malloc_free_large(t: &mut T) {
    unsafe {
        malloc_free_range(t, 8 * MIB, 16 * MIB, MIB);
        malloc_free_random(t, 8 * MIB, 16 * MIB, MIB, 1000);
    }
}

fn malloc_free_null(_: &mut T) {
    unsafe {
        m::free(null());
        m::malloc_zone_free(m::malloc_default_zone(), null());
    }
}

// tests/malloc_empty.c

fn empty_malloc_valid(t: &mut T) {
    unsafe {
        let made = [
            ("malloc(0)", m::malloc(0)),
            ("calloc(1, 0)", m::calloc(1, 0)),
            ("realloc(NULL, 0)", m::realloc(null(), 0)),
            ("aligned_alloc(8, 0)", m::aligned_alloc(8, 0)),
            ("reallocf(NULL, 0)", m::reallocf(null(), 0)),
            ("valloc(0)", m::valloc(0)),
        ];
        for (what, p) in made {
            t.expect(!p.is_null(), "a zero-size allocation returned NULL", || {
                what.into()
            });
            m::free(p);
        }
        let mut p = null();
        let r = m::posix_memalign(&mut p, 8, 0);
        t.expect(
            r == 0 && !p.is_null(),
            "posix_memalign(8, 0) failed",
            || format!("returned {r}"),
        );
        m::free(p);
    }
}

// tests/malloc_size_test.c

fn malloc_size_valid(t: &mut T) {
    unsafe {
        for (min, max, incr) in [(2, 256, 16), (512, 8192, 256), (8192, 65536, 1024)] {
            for sz in (min..=max).step_by(incr) {
                let p = m::malloc(sz);
                t.expect(!p.is_null(), "malloc returned NULL", || {
                    format!("malloc({sz})")
                });
                let size = m::malloc_size(p);
                t.expect(size >= sz, "malloc_size below the request", || {
                    format!("malloc_size(malloc({sz})) = {size}")
                });
                let good = m::malloc_good_size(sz);
                t.expect(good >= sz, "malloc_good_size below the request", || {
                    format!("malloc_good_size({sz}) = {good}")
                });
                m::free(p);
            }
        }
    }
}

fn malloc_size_invalid(t: &mut T) {
    unsafe {
        for (min, max, incr) in [(2, 256, 16), (512, 8192, 256), (8192, 32768, 1024)] {
            for sz in (min..=max).step_by(incr) {
                let p = m::malloc(sz);
                let one = m::malloc_size(at(p, 1));
                t.expect(one == 0, "malloc_size(p + 1) is not 0", || {
                    format!("{one} for p = malloc({sz})")
                });
                let half = m::malloc_size(at(p, sz / 2));
                t.expect(half == 0, "malloc_size(p + size / 2) is not 0", || {
                    format!("{half} for p = malloc({sz})")
                });
                m::free(p);
            }
        }
    }
}

/// From `malloc_size_invalid_xzone`: a freed block is not one any zone owns.
fn malloc_size_freed(t: &mut T) {
    unsafe {
        for _ in 0..128 {
            m::free(m::malloc(64));
            m::free(m::malloc(4097));
        }
        let check = |t: &mut T, p: *mut c_void, what: &'static str| {
            let size = m::malloc_size(p);
            t.expect(size == 0, "malloc_size of a freed block is not 0", || {
                format!("{what}: malloc_size = {size} after free")
            });
        };
        let p = m::malloc(64);
        m::free(p);
        check(t, p, "malloc(64)");
        let p = m::malloc(4097);
        let p2 = m::malloc(4097);
        m::free(p2);
        check(t, p2, "malloc(4097)");
        m::free(p);
        check(t, p, "malloc(4097)");
        let p = m::malloc(65536);
        m::free(p);
        check(t, p, "malloc(65536)");

        let p = m::malloc(MIB);
        m::free(p);
        let p2 = m::aligned_alloc(16 * KIB, 160 * KIB);
        if p == p2 {
            let size = m::malloc_size(at(p2, 192 * KIB));
            t.expect(
                size == 0,
                "malloc_size past a smaller reuse is not 0",
                || format!("{size}"),
            );
        }
        m::free(p2);
    }
}

fn malloc_size_large_allocation(t: &mut T) {
    unsafe {
        for gib in [4, 6] {
            let p = m::malloc(gib * GIB);
            if !t.expect(!p.is_null(), "a multi-GiB malloc failed", || {
                format!("malloc({gib} GiB)")
            }) {
                continue;
            }
            let size = m::malloc_size(p);
            t.expect(size >= gib * GIB, "malloc_size below the request", || {
                format!("malloc_size(malloc({gib} GiB)) = {size}")
            });
            m::free(p);
        }
    }
}

fn malloc_size_max_good_size(t: &mut T) {
    let request = usize::MAX - 5;
    let good = unsafe { m::malloc_good_size(request) };
    t.expect(
        good >= request,
        "malloc_good_size(SIZE_MAX - 5) below the request",
        || format!("{good:#x}"),
    );
}

// tests/calloc_test.c

fn calloc_overflow(t: &mut T) {
    let long_max = i64::MAX as usize;
    unsafe {
        for (count, size) in [
            (long_max, 1000),
            (1000, long_max),
            (long_max, 256),
            (256, long_max),
        ] {
            set_errno(0);
            let p = m::calloc(count, size);
            t.expect(p.is_null(), "calloc overflow returned a block", || {
                format!("calloc({count:#x}, {size})")
            });
            let e = errno();
            t.expect(
                e == libc::ENOMEM,
                "calloc overflow did not set ENOMEM",
                || format!("errno {e}"),
            );
            m::free(p);
        }
    }
}

unsafe fn t_calloc(t: &mut T, count: usize, s: usize) -> *mut c_void {
    let p = m::calloc(count, s);
    if !t.expect(!p.is_null(), "calloc returned NULL", || {
        format!("calloc({count}, {s})")
    }) {
        return p;
    }
    let size = m::malloc_size(p);
    t.expect(size >= count * s, "malloc_size below the request", || {
        format!("malloc_size(calloc({count}, {s})) = {size}")
    });
    t.expect(
        all_bytes(p, count * s, 0),
        "calloc memory is not zero",
        || format!("calloc({count}, {s})"),
    );
    p
}

/// Upstream's loops, but each block is dirtied before it is freed so that a
/// recycled one has to be cleared.
unsafe fn calloc_range(t: &mut T, count: usize, min: usize, max: usize, incr: usize) {
    let mut s = min;
    while s <= max {
        let p = t_calloc(t, count, s);
        if !p.is_null() {
            scribble(p, count * s);
        }
        m::free(p);
        s += incr;
    }
}

unsafe fn calloc_random(t: &mut T, count: usize, min: usize, max: usize, incr: usize, n: usize) {
    const P: usize = 100;
    let r = (max - min) / incr;
    let mut rng = Rng::new(0xca11 ^ (count as u64) << 32 ^ min as u64);
    let mut ptrs: [(*mut c_void, usize); P] = [(null(), 0); P];
    let (mut j, mut k) = (0, 0);
    for i in 0..n + P {
        let made = if i < n {
            let s = min + rng.below(r) * incr;
            (t_calloc(t, count, s), count * s)
        } else {
            (null(), 0)
        };
        let (old, len) = ptrs[j];
        if !old.is_null() {
            scribble(old, len);
        }
        m::free(old);
        ptrs[k] = made;
        j = k;
        k = (k + 1) % P;
    }
}

fn calloc_sizes(t: &mut T) {
    unsafe {
        for count in [1, 4, 16] {
            calloc_range(t, count, 0, 1008 / count, 16);
            calloc_random(t, count, 0, 1008 / count, 16, 100);
        }
        for count in [1, 16, 64] {
            calloc_range(t, count, 1024 / count, 256 * KIB / count, 512);
            calloc_random(t, count, 1024 / count, 256 * KIB / count, 512, 100);
        }
    }
}

// tests/aligned_alloc_test.c and tests/posix_memalign_test.c

unsafe fn t_aligned_alloc(t: &mut T, alignment: usize, size: usize) -> *mut c_void {
    let p = m::aligned_alloc(alignment, size);
    if !t.expect(!p.is_null(), "aligned_alloc returned NULL", || {
        format!("aligned_alloc({alignment}, {size})")
    }) {
        return p;
    }
    t.expect(
        (p as usize).is_multiple_of(alignment),
        "aligned_alloc misaligned",
        || format!("{p:p} for alignment {alignment}"),
    );
    let got = m::malloc_size(p);
    t.expect(got >= size, "malloc_size below the request", || {
        format!("malloc_size(aligned_alloc({alignment}, {size})) = {got}")
    });
    scribble(p, size);
    p
}

fn aligned_alloc_free(t: &mut T) {
    unsafe {
        let mut alignment = size_of::<usize>();
        while alignment < 64 * KIB {
            let mut size = alignment;
            while size <= 256 * alignment {
                let ptrs: Vec<*mut c_void> = (0..10)
                    .map(|_| t_aligned_alloc(t, alignment, size))
                    .collect();
                for p in ptrs {
                    m::free(p);
                }
                size += alignment;
            }
            alignment *= 2;
        }
    }
}

fn aligned_alloc_errors(t: &mut T) {
    unsafe {
        for (alignment, size) in [(8, 12), (32, 16), (24, 48), (23, 46), (9, 18)] {
            set_errno(0);
            let p = m::aligned_alloc(alignment, size);
            t.expect(
                p.is_null(),
                "an invalid aligned_alloc returned a block",
                || format!("aligned_alloc({alignment}, {size})"),
            );
            let e = errno();
            t.expect(
                e == libc::EINVAL,
                "an invalid aligned_alloc did not set EINVAL",
                || format!("aligned_alloc({alignment}, {size}): errno {e}"),
            );
        }
    }
}

unsafe fn t_posix_memalign(
    t: &mut T,
    alignment: usize,
    size: usize,
    scribble_it: bool,
) -> *mut c_void {
    let mut p = null();
    let r = m::posix_memalign(&mut p, alignment, size);
    if !t.expect(r == 0 && !p.is_null(), "posix_memalign failed", || {
        format!("posix_memalign({alignment:#x}, {size:#x}) = {r}")
    }) {
        return null();
    }
    t.expect(
        (p as usize).is_multiple_of(alignment),
        "posix_memalign misaligned",
        || format!("{p:p} for alignment {alignment}"),
    );
    let got = m::malloc_size(p);
    t.expect(got >= size, "malloc_size below the request", || {
        format!("malloc_size = {got} for posix_memalign({alignment}, {size})")
    });
    t.expect(
        m::malloc_claimed_address(p) != 0,
        "the block is not claimed",
        || format!("{p:p}"),
    );
    if scribble_it {
        scribble(p, size);
    }
    p
}

fn posix_memalign_free(t: &mut T) {
    unsafe {
        let mut alignment = size_of::<usize>();
        while alignment < 4096 {
            let mut size = alignment;
            while size <= 256 * alignment {
                let p = t_posix_memalign(t, alignment, size, true);
                m::free(p);
                size += 8;
            }
            alignment *= 2;
        }
    }
}

fn posix_memalign_errors(t: &mut T) {
    unsafe {
        for (alignment, size) in [(24, 48), (23, 46), (9, 18)] {
            let mut p = null();
            let r = m::posix_memalign(&mut p, alignment, size);
            t.expect(
                p.is_null(),
                "posix_memalign wrote memptr on failure",
                || format!("alignment {alignment}"),
            );
            t.expect(
                r == libc::EINVAL,
                "an invalid posix_memalign did not return EINVAL",
                || format!("alignment {alignment}: {r}"),
            );
        }
        let mut p = null();
        let r = m::posix_memalign(&mut p, 8, 0);
        t.expect(r == 0, "posix_memalign(8, 0) failed", || format!("{r}"));
        m::free(p);
    }
}

fn posix_memalign_large(t: &mut T) {
    unsafe {
        let mut alignment = size_of::<usize>();
        let mut largest = 0;
        while alignment <= 64 * GIB {
            let p = t_posix_memalign(t, alignment, alignment, false);
            if !p.is_null() {
                largest = alignment;
            }
            m::free(p);
            alignment *= 2;
        }
        t.note(format!(
            "largest alignment = size that succeeded: {largest:#x}"
        ));
    }
}

struct Seen {
    ptr: usize,
    size: usize,
    found: bool,
}

unsafe extern "C" fn record_ranges(
    _: Task,
    context: *mut c_void,
    kind: c_uint,
    ranges: *mut VmRange,
    count: c_uint,
) {
    if kind & MALLOC_PTR_IN_USE_RANGE_TYPE == 0 {
        return;
    }
    let seen = &mut *context.cast::<Seen>();
    for i in 0..count as usize {
        let range = *ranges.add(i);
        if range.address <= seen.ptr && range.address + range.size > seen.ptr {
            seen.found = range.size - (seen.ptr - range.address) >= seen.size;
        }
    }
}

/// `check_pointer_is_enumerated`, from posix_memalign_test.c: some zone's
/// enumerator reports the block as in use.
fn posix_memalign_enumerated(t: &mut T) {
    unsafe {
        let mut alignment = size_of::<usize>();
        while alignment < 4096 {
            let p = t_posix_memalign(t, alignment, alignment, false);
            let mut seen = Seen {
                ptr: p as usize,
                size: alignment,
                found: false,
            };
            let mut answers = Vec::new();
            for zone in zones() {
                let introspect = (*zone).introspect;
                let Some(enumerate) = introspect.as_ref().and_then(|i| i.enumerator) else {
                    continue;
                };
                let kr = enumerate(
                    0,
                    (&raw mut seen).cast(),
                    MALLOC_PTR_IN_USE_RANGE_TYPE,
                    zone as usize,
                    None,
                    Some(record_ranges),
                );
                answers.push(kr);
                if seen.found {
                    break;
                }
            }
            t.expect(seen.found, "no zone's enumerator reports the block", || {
                format!(
                    "posix_memalign({alignment}, {alignment}); enumerators returned {answers:?}"
                )
            });
            m::free(p);
            alignment *= 2;
        }
    }
}

// tests/realloc.c, tests/reallocarray.c, tests/malloc_realloc_large.c

fn realloc_failure(t: &mut T) {
    unsafe {
        let a = m::malloc(16);
        set_errno(0);
        let b = m::realloc(a, usize::MAX - (1 << 17));
        let e = errno();
        let size = m::malloc_size(a);
        t.expect(b.is_null(), "realloc to SIZE_MAX - 128K succeeded", || {
            format!("{b:p}")
        });
        t.expect(
            e == libc::ENOMEM,
            "realloc failure did not set ENOMEM",
            || format!("errno {e}"),
        );
        t.expect(size > 0, "realloc failure freed the original", || {
            format!("{size}")
        });
        m::free(a);
    }
}

fn reallocf_failure(t: &mut T) {
    unsafe {
        let a = m::malloc(65536);
        let before = stats(zones()[0]).blocks_in_use;
        set_errno(0);
        let b = m::reallocf(a, usize::MAX - (1 << 17));
        let e = errno();
        let size = m::malloc_size(a);
        let after = stats(zones()[0]).blocks_in_use;
        t.expect(
            after + 1 == before,
            "reallocf did not free the block",
            || format!("blocks_in_use {before} -> {after}"),
        );
        t.expect(b.is_null(), "reallocf to SIZE_MAX - 128K succeeded", || {
            format!("{b:p}")
        });
        t.expect(
            e == libc::ENOMEM,
            "reallocf failure did not set ENOMEM",
            || format!("errno {e}"),
        );
        t.expect(
            size == 0,
            "malloc_size of the block reallocf freed is not 0",
            || format!("{size}"),
        );
    }
}

fn reallocarray_both(t: &mut T) {
    type F = unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void;
    let fns: [(&str, F); 2] = [
        ("reallocarray", m::reallocarray),
        ("reallocarrayf", m::reallocarrayf),
    ];
    unsafe {
        for (name, f) in fns {
            let p = f(null(), 8, 8);
            t.expect(!p.is_null(), "(NULL, 8, 8) returned NULL", || name.into());
            let p = f(p, 8, 8);
            t.expect(!p.is_null(), "(ptr, 8, 8) returned NULL", || name.into());
            set_errno(0);
            let q = f(null(), usize::MAX >> 3, 1 << 5);
            t.expect(
                q.is_null() && errno() == libc::ENOMEM,
                "an overflowing count was not ENOMEM",
                || format!("{name}(NULL, ...)"),
            );
            set_errno(0);
            let q = f(p, usize::MAX >> 3, 1 << 5);
            t.expect(
                q.is_null() && errno() == libc::ENOMEM,
                "an overflowing count was not ENOMEM",
                || format!("{name}(ptr, ...)"),
            );
        }
    }
}

/// `do_realloc_test`, without its in-place assertions, which the file says
/// are xzone's choice and no promise.
fn realloc_large_huge(t: &mut T) {
    const LARGE_MAX: usize = 2 * MIB;
    const SLICE: usize = 16 * KIB;
    unsafe {
        let cases: [(&str, usize, usize, usize); 10] = [
            ("LARGE shrink", LARGE_MAX, LARGE_MAX / 4, 0),
            ("LARGE shrink one slice", LARGE_MAX, LARGE_MAX - SLICE, 0),
            ("LARGE grow", LARGE_MAX / 4, LARGE_MAX / 2, 0),
            ("HUGE shrink", LARGE_MAX * 8, LARGE_MAX * 2, 0),
            ("HUGE grow", LARGE_MAX * 2, LARGE_MAX * 4, 0),
            ("HUGE grow odd", 3 * MIB, 5 * MIB, 0),
            ("LARGE aligned", LARGE_MAX / 2, LARGE_MAX / 8, 2048),
            ("LARGE to HUGE", LARGE_MAX / 2, LARGE_MAX * 2, 0),
            ("HUGE to LARGE", LARGE_MAX * 2, LARGE_MAX / 2, 0),
            ("HUGE aligned", LARGE_MAX * 8, LARGE_MAX * 2, 0x10000),
        ];
        for (i, (what, size1, size2, align)) in cases.into_iter().enumerate() {
            let byte = b'A' + i as u8;
            let p1 = if align == 0 {
                m::malloc(size1)
            } else {
                m::aligned_alloc(align, size1)
            };
            std::ptr::write_bytes(p1.cast::<u8>(), byte, size1);
            let p2 = m::realloc(p1, size2);
            if !t.expect(!p2.is_null(), "realloc returned NULL", || what.into()) {
                continue;
            }
            t.expect(
                all_bytes(p2, size1.min(size2), byte),
                "contents changed in realloc",
                || what.into(),
            );
            let got = m::malloc_size(p2);
            t.expect(got >= size2, "malloc_size below the request", || {
                format!("{what}: {got} < {size2}")
            });
            m::free(p2);
        }
    }
}

// tests/zone_names.c

fn zone_names_default(t: &mut T) {
    unsafe {
        let names: Vec<Option<String>> = zones().into_iter().map(|z| zone_name(z)).collect();
        t.expect(
            names == [Some("DefaultMallocZone".to_string())],
            "the initial zones are not [DefaultMallocZone]",
            || format!("{names:?}"),
        );
    }
}

fn zone_singletons(t: &mut T) {
    unsafe {
        let default = zone_name(m::malloc_default_zone());
        t.expect(
            default.as_deref() == Some("DefaultMallocZone"),
            "default zone name",
            || format!("{default:?}"),
        );
        let purgeable = zone_name(m::malloc_default_purgeable_zone());
        t.expect(
            purgeable.as_deref() == Some("DefaultPurgeableMallocZone"),
            "purgeable zone name",
            || format!("{purgeable:?}"),
        );
    }
}

fn virtual_zone0(t: &mut T) {
    unsafe {
        let virtual_zone = m::malloc_default_zone();
        t.expect(
            virtual_zone != zones()[0],
            "malloc_default_zone() is malloc_zones[0]",
            String::new,
        );
        let p = m::malloc(5);
        let zone = m::malloc_zone_from_ptr(p);
        m::free(p);
        t.expect(
            zone == virtual_zone,
            "malloc_zone_from_ptr is not the virtual zone",
            || format!("{zone:p}"),
        );
    }
}

fn zone_creation_unnamed(t: &mut T) {
    unsafe {
        let zone = m::malloc_create_zone(0, 0);
        t.expect((*zone).zone_name.is_null(), "a new zone has a name", || {
            format!("{:?}", zone_name(zone))
        });
    }
}

static LITERAL_NAME: &CStr = c"string literal";

/// `check_set_zone_name`'s first assertion: a name in an image is kept by
/// reference (`_dyld_is_memory_immutable`).
fn set_zone_name_literal(t: &mut T) {
    unsafe {
        for zone in [m::malloc_default_zone(), m::malloc_create_zone(0, 0)] {
            m::malloc_set_zone_name(zone, LITERAL_NAME.as_ptr());
            let name = m::malloc_get_zone_name(zone);
            t.expect(
                name == LITERAL_NAME.as_ptr(),
                "a literal name was copied",
                || format!("{name:p} != {:p}", LITERAL_NAME.as_ptr()),
            );
        }
    }
}

/// The rest of `check_set_zone_name`.
fn set_zone_name_heap(t: &mut T) {
    unsafe {
        for zone in [m::malloc_default_zone(), m::malloc_create_zone(0, 0)] {
            let heap: *mut c_char = libc::strdup(c"heap string".as_ptr());
            m::malloc_set_zone_name(zone, heap);
            let name = m::malloc_get_zone_name(zone);
            t.expect(
                name != heap.cast_const(),
                "a heap name was not copied",
                String::new,
            );
            t.expect(
                CStr::from_ptr(name) == CStr::from_ptr(heap),
                "the copied name differs",
                || format!("{:?}", CStr::from_ptr(name)),
            );
            libc::free(heap.cast());
            let before = stats(zone).blocks_in_use;
            m::malloc_set_zone_name(zone, std::ptr::null());
            let after = stats(zone).blocks_in_use;
            t.expect(
                after + 1 == before,
                "the replaced name's copy was not freed",
                || format!("blocks_in_use {before} -> {after}"),
            );
            t.expect(
                m::malloc_get_zone_name(zone).is_null(),
                "the name is not cleared",
                String::new,
            );
            let size = m::malloc_size(name.cast());
            t.expect(
                size == 0,
                "malloc_size of the replaced name's copy is not 0",
                || format!("{size}"),
            );
        }
    }
}

// tests/introspect_tests.c

fn szone_statistics(t: &mut T) {
    unsafe {
        let zone = zones()[0];
        t.expect(
            (*zone).version >= 12,
            "zone 0 is older than version 12",
            || format!("{}", (*zone).version),
        );
        let introspect = &*(*zone).introspect;
        let task_statistics = introspect.task_statistics.unwrap();
        let statistics = introspect.statistics.unwrap();
        let task = |s: &mut Statistics| task_statistics(0, zone as usize, None, s);
        let check = |t: &mut T, s: &Statistics| {
            let mut direct = Statistics::default();
            statistics(zone, &mut direct);
            t.expect(
                (s.blocks_in_use, s.size_in_use, s.size_allocated)
                    == (
                        direct.blocks_in_use,
                        direct.size_in_use,
                        direct.size_allocated,
                    ),
                "task_statistics and statistics differ",
                || format!("{s:?} vs {direct:?}"),
            );
        };
        let mut old = Statistics::default();
        task(&mut old);
        check(t, &old);
        for i in 0..5000 {
            let mut p = m::malloc(i);
            let mut size = m::malloc_size(p);
            let mut now = Statistics::default();
            task(&mut now);
            check(t, &now);
            if now.blocks_in_use == old.blocks_in_use {
                p = m::malloc(i);
                size = m::malloc_size(p);
                task(&mut now);
                check(t, &now);
            }
            let _ = p;
            t.expect(
                now.size_in_use == old.size_in_use + size,
                "size_in_use did not grow by malloc_size",
                || {
                    format!(
                        "malloc({i}): {} -> {}, malloc_size {size}",
                        old.size_in_use, now.size_in_use
                    )
                },
            );
            t.expect(
                now.blocks_in_use == old.blocks_in_use + 1,
                "blocks_in_use did not grow by one",
                || {
                    format!(
                        "malloc({i}): {} -> {}",
                        old.blocks_in_use, now.blocks_in_use
                    )
                },
            );
            old = now;
        }
    }
}

unsafe extern "C" fn in_place_reader(
    _: Task,
    address: usize,
    _: usize,
    out: *mut *mut c_void,
) -> KernReturn {
    *out = address as *mut c_void;
    KERN_SUCCESS
}

fn get_all_zones(t: &mut T) {
    unsafe {
        m::malloc_default_purgeable_zone();
        m::malloc_create_zone(0, 0);
        let mut runs = Vec::new();
        for (task, reader) in [
            (0x103, None),
            (0, None),
            (0x103, Some(in_place_reader as m::MemoryReaderFn)),
        ] {
            let mut addresses: *mut usize = std::ptr::null_mut();
            let mut count = 0;
            let kr = m::malloc_get_all_zones(task, reader, &mut addresses, &mut count);
            t.expect(kr == KERN_SUCCESS, "malloc_get_all_zones failed", || {
                format!("{kr}")
            });
            runs.push(
                (0..count as usize)
                    .map(|i| *addresses.add(i))
                    .collect::<Vec<_>>(),
            );
        }
        t.expect(runs[0].len() == 3, "not every zone is listed", || {
            format!("{:?}", runs[0])
        });
        t.expect(
            runs.windows(2).all(|w| w[0] == w[1]),
            "the answers differ",
            || format!("{runs:x?}"),
        );
    }
}

// tests/malloc_create_zone_tests.c

fn create_many_zones(t: &mut T) {
    unsafe {
        for i in 0..4097 {
            let zone = m::malloc_create_zone(0, 0);
            if !t.expect(!zone.is_null(), "malloc_create_zone failed", || {
                format!("zone {i}")
            }) {
                return;
            }
            let p = m::malloc_zone_malloc(zone, 16);
            t.expect(!p.is_null(), "malloc_zone_malloc failed", || {
                format!("zone {i}")
            });
            let size = m::malloc_size(p);
            t.expect(size >= 16, "malloc_size below 16", || {
                format!("zone {i}: {size}")
            });
            m::free(p);
        }
        let n = zones().len();
        t.expect(n == 4098, "not every zone is registered", || format!("{n}"));
    }
}

fn new_zone_statistics(t: &mut T) {
    unsafe {
        let zone = m::malloc_create_zone(0, 0);
        let before = stats(zone);
        t.expect(
            before.size_in_use == 0 && before.blocks_in_use == 0,
            "a new zone is not empty",
            || format!("{before:?}"),
        );
        let p1 = m::malloc_zone_malloc(zone, 1024);
        let p2 = m::malloc_zone_malloc(zone, 8192);
        let during = stats(zone);
        m::free(p1);
        m::free(p2);
        t.expect(
            during.size_in_use == 1024 + 8192,
            "size_in_use is not 9216",
            || format!("{during:?}"),
        );
        t.expect(during.blocks_in_use == 2, "blocks_in_use is not 2", || {
            format!("{during:?}")
        });
        let after = stats(zone);
        t.expect(
            after.size_in_use == 0 && after.blocks_in_use == 0,
            "the zone is not empty after free",
            || format!("{after:?}"),
        );
    }
}

fn free_pointers_on_destroy(t: &mut T) {
    unsafe {
        let zone = m::malloc_create_zone(0, 0);
        let mut made: Vec<(&'static str, *mut c_void)> = Vec::new();
        for (what, size, count) in [
            ("TINY", 1024, 1024),
            ("SMALL", 32768, 384),
            ("LARGE", 65536, 256),
            ("HUGE", 32 * MIB, 2),
        ] {
            for _ in 0..count {
                let p = m::malloc_zone_malloc(zone, size);
                t.expect(!p.is_null(), "an allocation failed", || what.into());
                let got = m::malloc_size(p);
                t.expect(got >= size, "malloc_size below the request", || {
                    format!("{what}: {got}")
                });
                made.push((what, p));
            }
        }
        m::malloc_destroy_zone(zone);
        for (what, p) in made {
            let size = m::malloc_size(p);
            t.expect(
                size == 0,
                "a block of a destroyed zone is still sized",
                || format!("{what}: {size}"),
            );
        }
    }
}

fn zone_pressure(zone: usize, finished: &AtomicBool, free_all: bool, seed: u64) {
    let zone = zone as *mut MallocZone;
    let mut rng = Rng::new(seed);
    let mut ptrs = vec![null(); 1024];
    unsafe {
        while !finished.load(Ordering::Relaxed) {
            let data = rng.next() & 1 == 1;
            let index = rng.below(1024);
            let new_size = ((rng.below(256)) + 1) << rng.below(10);
            if !ptrs[index].is_null() && rng.below(8) == 0 {
                ptrs[index] = if data {
                    m::malloc_type_zone_realloc(zone, ptrs[index], new_size, 0)
                } else {
                    m::malloc_zone_realloc(zone, ptrs[index], new_size)
                };
            } else {
                m::malloc_zone_free(zone, ptrs[index]);
                ptrs[index] = if data {
                    m::malloc_type_zone_malloc(zone, new_size, 0)
                } else {
                    m::malloc_zone_malloc(zone, new_size)
                };
            }
        }
        if free_all {
            for p in ptrs {
                m::malloc_zone_free(zone, p);
            }
        }
    }
}

/// `create_zone_stress`, with 30 rounds of new zones rather than 100.
fn create_zone_stress(_: &mut T) {
    unsafe {
        let main_zone = zones()[0] as usize;
        let main_finished = Arc::new(AtomicBool::new(false));
        let mains: Vec<_> = (0..3)
            .map(|i| {
                let finished = main_finished.clone();
                std::thread::spawn(move || zone_pressure(main_zone, &finished, true, 100 + i))
            })
            .collect();
        std::thread::sleep(Duration::from_secs(1));
        for round in 0..30u64 {
            let finished = Arc::new(AtomicBool::new(false));
            let news: Vec<_> = (0..3u64)
                .map(|j| {
                    let finished = finished.clone();
                    let thread = std::thread::spawn(move || {
                        let zone = m::malloc_create_zone(0, 0);
                        zone_pressure(zone as usize, &finished, false, round << 8 | j);
                        m::malloc_destroy_zone(zone);
                    });
                    std::thread::sleep(Duration::from_micros(500));
                    thread
                })
                .collect();
            std::thread::sleep(Duration::from_millis(50));
            finished.store(true, Ordering::Relaxed);
            for thread in news {
                thread.join().unwrap();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        main_finished.store(true, Ordering::Relaxed);
        for thread in mains {
            thread.join().unwrap();
        }
    }
}

/// `malloc_fork_with_xzone`, with a zone of `malloc_create_zone`'s.
fn fork_with_zone(t: &mut T) {
    unsafe {
        let zone = m::malloc_create_zone(0, 0) as usize;
        let done = Arc::new(AtomicBool::new(false));
        let workers: Vec<_> = (0..8u64)
            .map(|i| {
                let done = done.clone();
                std::thread::spawn(move || {
                    let mut rng = Rng::new(i + 1);
                    while !done.load(Ordering::Relaxed) {
                        let p = m::malloc_zone_malloc(zone as *mut MallocZone, 1 << rng.below(16));
                        assert!(!p.is_null());
                        m::free(p);
                    }
                })
            })
            .collect();
        std::thread::sleep(Duration::from_micros(100));
        let p1 = m::malloc_zone_malloc(zone as *mut MallocZone, 64);
        let p2 = m::malloc_zone_malloc(zone as *mut MallocZone, 65536);
        let pid = libc::fork();
        if pid == 0 {
            let code = if m::malloc_size(p1) < 64 {
                1
            } else if m::malloc_size(p2) < 65536 {
                2
            } else {
                let before = stats(zone as *mut MallocZone).blocks_in_use;
                m::free(p1);
                m::free(p2);
                if stats(zone as *mut MallocZone).blocks_in_use + 2 != before {
                    5
                } else if m::malloc_size(p1) != 0 {
                    3
                } else if m::malloc_size(p2) != 0 {
                    4
                } else {
                    0
                }
            };
            libc::_exit(code);
        }
        let mut status = 0;
        let waited = wait_with_timeout(pid, Duration::from_secs(10), &mut status);
        if t.expect(waited, "the forked child hung", String::new) {
            let code = libc::WEXITSTATUS(status);
            t.expect(
                libc::WIFEXITED(status) && code == 0,
                "the forked child failed",
                || match code {
                    1 => "malloc_size(tiny) < 64 in the child".into(),
                    2 => "malloc_size(large) < 65536 in the child".into(),
                    3 => "malloc_size of the freed tiny block is not 0 in the child".into(),
                    4 => "malloc_size of the freed large block is not 0 in the child".into(),
                    5 => "the frees in the child did not reach the zone's figures".into(),
                    _ => format!("status {status:#x}"),
                },
            );
        }
        t.expect(
            m::malloc_size(p1) >= 64,
            "the parent's tiny block was lost",
            String::new,
        );
        t.expect(
            m::malloc_size(p2) >= 65536,
            "the parent's large block was lost",
            String::new,
        );
        m::free(p1);
        m::free(p2);
        done.store(true, Ordering::Relaxed);
        for worker in workers {
            worker.join().unwrap();
        }
    }
}

/// Wait for `pid`, killing it after `limit`. False if it had to be killed.
unsafe fn wait_with_timeout(pid: libc::pid_t, limit: Duration, status: &mut i32) -> bool {
    wait_with_timeout_then(pid, limit, status, |_| {})
}

/// As [`wait_with_timeout`], running `on_hang` before the kill.
unsafe fn wait_with_timeout_then(
    pid: libc::pid_t,
    limit: Duration,
    status: &mut i32,
    on_hang: impl FnOnce(libc::pid_t),
) -> bool {
    let start = Instant::now();
    loop {
        let r = libc::waitpid(pid, status, libc::WNOHANG);
        if r == pid {
            return true;
        }
        if start.elapsed() > limit {
            on_hang(pid);
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, status, 0);
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

// tests/malloc_zone_unregister_test.c

fn custom_default_zone(t: &mut T) {
    unsafe {
        let ptr = m::malloc(7);
        t.expect(
            !m::malloc_zone_from_ptr(ptr).is_null(),
            "no zone for a fresh block",
            String::new,
        );
        t.expect(
            m::malloc_claimed_address(ptr) != 0,
            "a fresh block is not claimed",
            String::new,
        );
        let initial = zones();
        for _ in 0..initial.len() {
            m::malloc_zone_unregister(zones()[0]);
        }
        t.expect(
            zones().is_empty(),
            "zones remain after unregistering all",
            || format!("{}", zones().len()),
        );
        t.expect(
            m::malloc_zone_from_ptr(ptr).is_null(),
            "a zone is found with none registered",
            String::new,
        );
        let custom = m::malloc_create_zone(0, 0);
        t.expect(
            m::malloc_zone_from_ptr(ptr).is_null(),
            "the custom zone claims the old block",
            String::new,
        );
        for zone in &initial {
            m::malloc_zone_register(*zone);
        }
        let n = zones().len();
        t.expect(
            n == initial.len() + 1,
            "re-registering left the wrong count",
            || format!("{n}"),
        );
        t.expect(
            !m::malloc_zone_from_ptr(ptr).is_null(),
            "the old block's zone is not found again",
            String::new,
        );
        t.expect(
            m::malloc_claimed_address(ptr) != 0,
            "the old block is not claimed again",
            String::new,
        );
        let ptr2 = m::malloc(7);
        t.expect(
            zones()[0] == custom,
            "the custom zone is not zone 0",
            String::new,
        );
        t.expect(
            m::malloc_zone_from_ptr(ptr2) == m::malloc_default_zone(),
            "a block of the custom zone 0 is not the virtual zone's",
            String::new,
        );
        t.expect(
            m::malloc_zone_claimed_address(custom, ptr2) != 0,
            "the custom zone does not claim its block",
            String::new,
        );
        m::free(ptr2);
        m::free(ptr);
    }
}

// tests/malloc_claimed_address_tests.c

const CLAIM_CASES: [(&str, usize, usize); 3] = [
    ("tiny", 16, 8),
    ("small", 2048, 1000),
    ("large", 140000, 1000),
];

fn claimed_address_default(t: &mut T) {
    unsafe {
        t.expect(
            m::malloc_claimed_address(null()) == 0,
            "NULL is claimed",
            String::new,
        );
        for (what, size, offset) in CLAIM_CASES {
            let p = m::malloc(size);
            t.expect(
                m::malloc_claimed_address(p) != 0,
                "a block is not claimed",
                || what.into(),
            );
            t.expect(
                m::malloc_claimed_address(at(p, offset)) != 0,
                "an interior address is not claimed",
                || what.into(),
            );
            m::free(p);
        }
        let mapped = mmap_anonymous(1024);
        t.expect(
            m::malloc_claimed_address(mapped) == 0,
            "mmap'd memory is claimed",
            String::new,
        );
        libc::munmap(mapped, 1024);
    }
}

fn zone_claimed_address(t: &mut T) {
    unsafe {
        let zone = m::malloc_create_zone(0, 0);
        t.expect(
            m::malloc_zone_claimed_address(zone, null()) == 0,
            "NULL is claimed",
            String::new,
        );
        for (what, size, offset) in CLAIM_CASES {
            let p = m::malloc_zone_malloc(zone, size);
            t.expect(
                m::malloc_zone_claimed_address(zone, p) != 0,
                "the zone's block is not claimed",
                || what.into(),
            );
            t.expect(
                m::malloc_zone_claimed_address(zone, at(p, offset)) != 0,
                "an interior address of the zone's block is not claimed",
                || what.into(),
            );
            m::free(p);
            let p = m::malloc(size);
            t.expect(
                m::malloc_zone_claimed_address(zone, p) == 0,
                "the default zone's block is claimed",
                || what.into(),
            );
            t.expect(
                m::malloc_zone_claimed_address(zone, at(p, 8)) == 0,
                "an interior address of the default zone's block is claimed",
                || what.into(),
            );
            m::free(p);
        }
        let mapped = mmap_anonymous(1024);
        t.expect(
            m::malloc_zone_claimed_address(zone, mapped) == 0,
            "mmap'd memory is claimed",
            String::new,
        );
        libc::munmap(mapped, 1024);
        m::malloc_destroy_zone(zone);
    }
}

fn claimed_address_other_zone(t: &mut T) {
    unsafe {
        let zone = m::malloc_create_zone(0, 0);
        for (what, size, offset) in CLAIM_CASES {
            let p = m::malloc_zone_malloc(zone, size);
            t.expect(
                m::malloc_claimed_address(p) != 0,
                "a created zone's block is not claimed",
                || what.into(),
            );
            t.expect(
                m::malloc_claimed_address(at(p, offset)) != 0,
                "an interior address is not claimed",
                || what.into(),
            );
            m::free(p);
        }
        m::malloc_destroy_zone(zone);
    }
}

// tests/malloc_with_options_test.c

fn malloc_options_random(t: &mut T) {
    unsafe {
        let mut rng = Rng::new(0x0071_0115);
        let mut pointers = [null(); 64];
        for iteration in 0..10_000 {
            let index = rng.below(64);
            let bits = rng.next();
            let (aligned, zeroed, canonical) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
            let mut options = 0;
            if zeroed {
                options |= OPTION_CLEAR;
            }
            if canonical {
                options |= OPTION_CANONICAL_TAG;
            }
            let bits = rng.next() as usize;
            let (align, size) = if aligned {
                let align = 8 << (bits & 0xf);
                (align, align * (((bits >> 4) & 0x7) + 1))
            } else {
                (8, (bits & 0x7f_ffff) & !7)
            };
            m::free(pointers[index]);
            let p = m::malloc_zone_malloc_with_options(null().cast(), align, size, options);
            pointers[index] = p;
            if !t.expect(!p.is_null(), "allocation failed", || {
                format!("iteration {iteration}, align {align}, size {size}")
            }) {
                continue;
            }
            if zeroed {
                t.expect(all_bytes(p, size, 0), "memory was not cleared", || {
                    format!("size {size}")
                });
            }
            if canonical {
                t.expect(
                    p as usize & 0x0f00_0000_0000_0000 == 0,
                    "tag is not canonical",
                    || format!("{p:p}"),
                );
            }
            if aligned {
                t.expect(
                    (p as usize).is_multiple_of(align),
                    "pointer is not aligned",
                    || format!("{p:p} % {align}"),
                );
            }
            scribble(p, size);
        }
        for p in pointers {
            m::free(p);
        }
    }
}

fn malloc_options_alignment(t: &mut T) {
    unsafe {
        for size in 0..=32 {
            let p = m::malloc_zone_malloc_with_options(null().cast(), 8, size, 0);
            t.expect(!p.is_null(), "default alignment returned NULL", || {
                format!("size {size}")
            });
            m::free(p);
        }
        for size in 0..=32 {
            let p = m::malloc_zone_malloc_with_options(null().cast(), 16, size, 0);
            if size % 16 != 0 {
                t.expect(
                    p.is_null(),
                    "a size the alignment does not divide was allocated",
                    || format!("size {size}"),
                );
            } else {
                t.expect(!p.is_null(), "an aligned size returned NULL", || {
                    format!("size {size}")
                });
            }
            m::free(p);
        }
    }
}

// tests/malloc_create_purgeable_zone.c

fn purgeable_aligned_alloc(t: &mut T) {
    unsafe {
        let zone = m::malloc_default_purgeable_zone();
        let p = m::malloc_zone_memalign(zone, 64 * KIB, 32 * KIB);
        t.expect(
            !p.is_null(),
            "aligned purgeable allocation failed",
            String::new,
        );
        let size = m::malloc_size(p);
        t.expect(size >= 32 * KIB, "malloc_size below the request", || {
            format!("{size}")
        });
        m::malloc_zone_free(zone, p);
    }
}

/// `purgeable_realloc`, for the contents: the VM purgeable states it also
/// checks have no counterpart on this host.
fn purgeable_realloc(t: &mut T) {
    unsafe {
        let zone = m::malloc_default_purgeable_zone();
        for steps in [
            &[64 * KIB, 128 * KIB][..],
            &[2 * KIB, 4 * KIB, 15 * KIB, 64 * KIB][..],
            &[64 * KIB, 4 * KIB][..],
            &[6 * MIB, 8 * MIB][..],
        ] {
            let mut p = m::malloc_zone_malloc(zone, steps[0]);
            if !t.expect(!p.is_null(), "purgeable allocation failed", String::new) {
                continue;
            }
            p.cast::<u32>().write(0xcafe_babe);
            for &size in &steps[1..] {
                p = m::malloc_zone_realloc(zone, p, size);
                if !t.expect(!p.is_null(), "purgeable realloc failed", || {
                    format!("to {size}")
                }) {
                    break;
                }
                t.expect(
                    p.cast::<u32>().read() == 0xcafe_babe,
                    "memory not preserved in realloc",
                    || format!("to {size}"),
                );
            }
            m::malloc_zone_free(zone, p);
        }
    }
}

// tests/memory_pressure.c

/// `tiny_mem_pressure`: pressure relief on another thread while blocks come
/// and go.
fn pressure_relief_concurrent(_: &mut T) {
    unsafe {
        let stop = Arc::new(AtomicBool::new(false));
        let relief = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    m::malloc_zone_pressure_relief(null().cast(), 0);
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        let mut list = Vec::with_capacity(50_000);
        for _ in 0..100 {
            for _ in 0..50_000 {
                let p = m::calloc(1, 128);
                assert!(!p.is_null());
                list.push(p);
            }
            for p in list.drain(..) {
                m::free(p);
            }
        }
        stop.store(true, Ordering::Relaxed);
        relief.join().unwrap();
    }
}

// Behaviour of upstream's sources that no test there asserts.

/// `TINY_QUANTUM`: tiny, nano and xzone blocks are all sized and aligned in
/// sixteens, and `szone_good_size` rounds the same way.
fn quantum_multiple(t: &mut T) {
    unsafe {
        for n in 0..=4096 {
            let p = m::malloc(n);
            t.expect(
                (p as usize).is_multiple_of(16),
                "a block is not 16-byte aligned",
                || format!("malloc({n}) = {p:p}"),
            );
            let size = m::malloc_size(p);
            t.expect(
                size.is_multiple_of(16),
                "malloc_size is not a multiple of 16",
                || format!("malloc({n}): {size}"),
            );
            let good = m::malloc_good_size(n);
            t.expect(
                good.is_multiple_of(16),
                "malloc_good_size is not a multiple of 16",
                || format!("{n}: {good}"),
            );
            m::free(p);
        }
    }
}

/// `szone_good_size` rounds as `szone_malloc` does, so up to the small limit
/// it predicts `malloc_size` exactly.
fn good_size_predicts_size(t: &mut T) {
    unsafe {
        for n in 0..=15 * KIB {
            let p = m::malloc(n);
            let size = m::malloc_size(p);
            let good = m::malloc_good_size(n);
            t.expect(
                size == good,
                "malloc_good_size differs from malloc_size",
                || format!("n = {n}: good {good}, size {size}"),
            );
            m::free(p);
        }
    }
}

/// Above the small limit, szone's medium (32K quantum) and large (page)
/// allocators and xzone's large and huge ones all return page-aligned blocks.
fn large_blocks_page_aligned(t: &mut T) {
    unsafe {
        let page = page_size();
        for n in [
            256 * KIB,
            256 * KIB + 1,
            MIB,
            3 * MIB + 1,
            16 * MIB,
            100 * MIB + 7,
        ] {
            let p = m::malloc(n);
            t.expect(
                (p as usize).is_multiple_of(page),
                "a large block is not page-aligned",
                || format!("malloc({n}) = {p:p}"),
            );
            m::free(p);
        }
    }
}

/// `malloc_size`, `malloc_zone_from_ptr` and `malloc_claimed_address` of
/// memory that is no zone's.
fn foreign_pointers(t: &mut T) {
    static STATIC: [u64; 4] = [0; 4];
    unsafe {
        let local = [0u64; 4];
        let mapped = mmap_anonymous(4096);
        for (what, p) in [
            ("NULL", null()),
            ("stack", local.as_ptr().cast_mut().cast()),
            ("static", STATIC.as_ptr().cast_mut().cast()),
            ("mmap", mapped),
        ] {
            let size = m::malloc_size(p);
            t.expect(size == 0, "malloc_size of foreign memory is not 0", || {
                format!("{what}: {size}")
            });
            t.expect(
                m::malloc_zone_from_ptr(p).is_null(),
                "foreign memory has a zone",
                || what.into(),
            );
            t.expect(
                m::malloc_claimed_address(p) == 0,
                "foreign memory is claimed",
                || what.into(),
            );
        }
        libc::munmap(mapped, 4096);
    }
}

/// An allocation that must fail.
type Failing = fn() -> *mut c_void;

/// `malloc_set_errno_fast(MZ_POSIX, ...)` on every failing C entry point.
fn errno_on_failure(t: &mut T) {
    unsafe {
        let cases: [(&str, Failing); 4] = [
            ("malloc(SIZE_MAX)", || m::malloc(usize::MAX)),
            ("valloc(SIZE_MAX)", || m::valloc(usize::MAX)),
            ("malloc(SIZE_MAX / 2)", || m::malloc(usize::MAX / 2)),
            ("aligned_alloc(4096, SIZE_MAX - 4095)", || {
                m::aligned_alloc(4096, usize::MAX - 4095)
            }),
        ];
        for (what, f) in cases {
            set_errno(0);
            let p = f();
            let e = errno();
            t.expect(p.is_null(), "an impossible request succeeded", || {
                what.into()
            });
            t.expect(e == libc::ENOMEM, "a failure did not set ENOMEM", || {
                format!("{what}: errno {e}")
            });
        }
        let mut p = null();
        let r = m::posix_memalign(&mut p, 4096, usize::MAX - 4095);
        t.expect(
            r == libc::ENOMEM && p.is_null(),
            "posix_memalign of an impossible size",
            || format!("{r}"),
        );
    }
}

/// `_realloc`: contents survive every move between size classes.
fn realloc_contents(t: &mut T) {
    unsafe {
        let sizes = [
            1usize,
            15,
            16,
            17,
            100,
            1008,
            1009,
            4096,
            15 * KIB,
            15 * KIB + 1,
            128 * KIB,
            MIB,
            9 * MIB,
        ];
        for &from in &sizes {
            for &to in &sizes {
                let p = m::malloc(from);
                for i in 0..from {
                    *p.cast::<u8>().add(i) = (i * 7 + from) as u8;
                }
                let q = m::realloc(p, to);
                if !t.expect(!q.is_null(), "realloc returned NULL", || {
                    format!("{from} -> {to}")
                }) {
                    continue;
                }
                let kept =
                    (0..from.min(to)).all(|i| *q.cast::<u8>().add(i) == (i * 7 + from) as u8);
                t.expect(kept, "contents changed", || format!("{from} -> {to}"));
                let size = m::malloc_size(q);
                t.expect(size >= to, "malloc_size below the request", || {
                    format!("{from} -> {to}: {size}")
                });
                m::free(q);
            }
        }
        let p = m::malloc(64);
        let r = m::realloc(p, 0);
        t.expect(
            !r.is_null() && m::malloc_size(r) > 0,
            "realloc(p, 0) is not a live minimal block",
            String::new,
        );
        m::free(r);
    }
}

/// `malloc_zone_register_while_locked` refuses a zone twice, and
/// `malloc_zone_unregister` moves the last zone into the hole.
fn registry_order(t: &mut T) {
    unsafe {
        let default = zones()[0];
        let a = m::malloc_create_zone(0, 0);
        let b = m::malloc_create_zone(0, 0);
        let c = m::malloc_create_zone(0, 0);
        t.expect(
            zones() == [default, a, b, c],
            "zones are not appended in order",
            String::new,
        );
        m::malloc_zone_register(b);
        t.expect(
            zones() == [default, a, b, c],
            "a zone was registered twice",
            String::new,
        );
        m::malloc_zone_unregister(a);
        t.expect(
            zones() == [default, c, b],
            "the last zone did not move into the hole",
            || format!("{:?}", zones()),
        );
        m::malloc_zone_register(a);
        m::malloc_zone_unregister(default);
        t.expect(
            zones()[0] == a,
            "the last zone did not become zone 0",
            String::new,
        );
        let p = m::malloc(10);
        t.expect(
            m::malloc_zone_claimed_address(a, p) != 0,
            "malloc did not follow zone 0",
            String::new,
        );
        m::free(p);
    }
}

/// `malloc_zone_batch_malloc` may return fewer than asked (szone only batches
/// tiny sizes), but what it returns is live and `malloc_zone_batch_free`
/// takes it back.
fn batch_malloc_free(t: &mut T) {
    unsafe {
        let zone = m::malloc_default_zone();
        let mut blocks = [null(); 64];
        let n = m::malloc_zone_batch_malloc(zone, 48, blocks.as_mut_ptr(), 64);
        t.expect(n >= 1, "a tiny batch returned nothing", String::new);
        for &p in &blocks[..n as usize] {
            t.expect(
                m::malloc_size(p) >= 48,
                "a batch block is too small",
                String::new,
            );
            scribble(p, 48);
        }
        let mut sorted: Vec<usize> = blocks[..n as usize].iter().map(|p| *p as usize).collect();
        sorted.sort();
        t.expect(
            sorted.windows(2).all(|w| w[1] - w[0] >= 48),
            "batch blocks overlap",
            String::new,
        );
        m::malloc_zone_batch_free(zone, blocks.as_mut_ptr(), n);
    }
}

/// `mstats` and `malloc_zone_statistics(NULL, ...)` sum every zone.
fn statistics_totals(t: &mut T) {
    unsafe {
        let zone = m::malloc_create_zone(0, 0);
        let kept: Vec<_> = (0..50)
            .map(|i| m::malloc_zone_malloc(zone, 100 * i + 1))
            .collect();
        let kept2: Vec<_> = (0..50).map(|i| m::malloc(37 * i + 3)).collect();
        let all = stats(null().cast());
        let sum = zones()
            .into_iter()
            .map(|z| stats(z))
            .fold((0u32, 0usize), |(b, s), z| {
                (b + z.blocks_in_use, s + z.size_in_use)
            });
        t.expect(
            (all.blocks_in_use, all.size_in_use) == sum,
            "the total is not the sum of the zones",
            || format!("{all:?} vs {sum:?}"),
        );
        let ms = m::mstats();
        t.expect(
            ms.bytes_used == all.size_in_use && ms.chunks_used == all.blocks_in_use as usize,
            "mstats disagrees with malloc_zone_statistics",
            || format!("{ms:?} vs {all:?}"),
        );
        t.expect(
            all.size_allocated >= all.size_in_use,
            "size_allocated below size_in_use",
            || format!("{all:?}"),
        );
        for p in kept.into_iter().chain(kept2) {
            m::free(p);
        }
    }
}

/// Typed allocation (macOS 14's `malloc_type_*`): what code built with the
/// macOS 14 SDK calls in place of `malloc` and friends.
fn typed_entry_points(t: &mut T) {
    unsafe {
        let id = 0x1234_5678_9abc_def0;
        let p = m::malloc_type_malloc(100, id);
        t.expect(
            !p.is_null() && m::malloc_size(p) >= 100,
            "malloc_type_malloc",
            String::new,
        );
        scribble(p, 100);
        let p = m::malloc_type_realloc(p, 5000, id);
        t.expect(
            !p.is_null() && all_bytes(p, 100, 0xbe),
            "malloc_type_realloc",
            String::new,
        );
        m::malloc_type_free(p, id);
        let p = m::malloc_type_calloc(10, 10, id);
        t.expect(
            !p.is_null() && all_bytes(p, 100, 0),
            "malloc_type_calloc",
            String::new,
        );
        m::malloc_type_free(p, id);
        let p = m::malloc_type_aligned_alloc(256, 512, id);
        t.expect(
            !p.is_null() && (p as usize).is_multiple_of(256),
            "malloc_type_aligned_alloc",
            String::new,
        );
        m::malloc_type_free(p, id);
        let mut p = null();
        t.expect(
            m::malloc_type_posix_memalign(&mut p, 1024, 10, id) == 0
                && (p as usize).is_multiple_of(1024),
            "malloc_type_posix_memalign",
            String::new,
        );
        m::malloc_type_free(p, id);
        let p = m::malloc_type_valloc(10, id);
        t.expect(
            !p.is_null() && (p as usize).is_multiple_of(page_size()),
            "malloc_type_valloc",
            String::new,
        );
        m::malloc_type_free(p, id);
        let zone = m::malloc_create_zone(0, 0);
        let p = m::malloc_type_zone_malloc(zone, 64, id);
        t.expect(
            m::malloc_zone_from_ptr(p) == zone,
            "malloc_type_zone_malloc used another zone",
            String::new,
        );
        let p = m::malloc_type_zone_realloc(zone, p, 128, id);
        t.expect(
            m::malloc_zone_from_ptr(p) == zone,
            "malloc_type_zone_realloc used another zone",
            String::new,
        );
        m::malloc_type_zone_free(zone, p, id);
        let p = m::malloc_type_zone_memalign(zone, 4096, 64, id);
        t.expect(
            (p as usize).is_multiple_of(4096) && m::malloc_zone_from_ptr(p) == zone,
            "malloc_type_zone_memalign",
            String::new,
        );
        m::malloc_type_zone_free(zone, p, id);
        let p = m::malloc_type_zone_malloc_with_options(zone, 64, 128, OPTION_CLEAR, id);
        t.expect(
            (p as usize).is_multiple_of(64) && all_bytes(p, 128, 0),
            "malloc_type_zone_malloc_with_options",
            String::new,
        );
        m::malloc_type_zone_free(zone, p, id);
        m::malloc_destroy_zone(zone);
    }
}

/// Blocks are freed on other threads than the ones that made them. Each is
/// tagged and checked before it goes, so overlapping or clobbered blocks show
/// up, and the default zone's figures must return to where they began.
fn threaded_stress(t: &mut T) {
    unsafe {
        let default = zones()[0];
        let before = stats(default);
        let handoff: Arc<Mutex<Vec<(usize, usize, u8)>>> = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(AtomicUsize::new(0));
        let first_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let threads: Vec<_> = (0..8u64)
            .map(|id| {
                let handoff = handoff.clone();
                let errors = errors.clone();
                let first_error = first_error.clone();
                std::thread::spawn(move || {
                    let mut rng = Rng::new(0xbeef + id);
                    let mut live: Vec<(usize, usize, u8)> = Vec::new();
                    let check = |p: usize, len: usize, tag: u8, what: &str| {
                        if !all_bytes(p as *const c_void, len, tag) {
                            errors.fetch_add(1, Ordering::Relaxed);
                            first_error.lock().unwrap().get_or_insert_with(|| {
                                format!("{what}: block {p:#x}+{len} lost its tag")
                            });
                        }
                    };
                    for _ in 0..100_000 {
                        match rng.below(10) {
                            0..=3 => {
                                let shift = rng.below(17);
                                let len = rng.below(1 << shift) + 1;
                                let p = match rng.below(4) {
                                    0 => m::malloc(len),
                                    1 => {
                                        let p = m::calloc(1, len);
                                        check(p as usize, len, 0, "calloc");
                                        p
                                    }
                                    2 => {
                                        let mut p = null();
                                        m::posix_memalign(&mut p, 16 << rng.below(8), len);
                                        p
                                    }
                                    _ => m::malloc_zone_malloc(m::malloc_default_zone(), len),
                                };
                                let tag = rng.next() as u8 | 1;
                                std::ptr::write_bytes(p.cast::<u8>(), tag, len);
                                live.push((p as usize, len, tag));
                            }
                            4..=5 if !live.is_empty() => {
                                let i = rng.below(live.len());
                                let (p, len, tag) = live[i];
                                check(p, len, tag, "before realloc");
                                let shift = rng.below(17);
                                let new_len = rng.below(1 << shift) + 1;
                                let q = m::realloc(p as *mut c_void, new_len) as usize;
                                check(q, len.min(new_len), tag, "after realloc");
                                std::ptr::write_bytes(q as *mut u8, tag, new_len);
                                live[i] = (q, new_len, tag);
                            }
                            6..=8 if !live.is_empty() => {
                                let (p, len, tag) = live.swap_remove(rng.below(live.len()));
                                check(p, len, tag, "before free");
                                if rng.below(3) == 0 {
                                    handoff.lock().unwrap().push((p, len, tag));
                                } else {
                                    m::free(p as *mut c_void);
                                }
                            }
                            _ => {
                                let taken = handoff.lock().unwrap().pop();
                                if let Some((p, len, tag)) = taken {
                                    check(p, len, tag, "before a cross-thread free");
                                    m::free(p as *mut c_void);
                                }
                            }
                        }
                    }
                    for (p, len, tag) in live {
                        check(p, len, tag, "at the end");
                        m::free(p as *mut c_void);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        for (p, _, _) in handoff.lock().unwrap().drain(..) {
            m::free(p as *mut c_void);
        }
        let n = errors.load(Ordering::Relaxed);
        t.expect(n == 0, "a block's contents changed under it", || {
            format!(
                "{n} times; {}",
                first_error.lock().unwrap().clone().unwrap_or_default()
            )
        });
        let after = stats(default);
        t.expect(
            (after.blocks_in_use, after.size_in_use) == (before.blocks_in_use, before.size_in_use),
            "the default zone's figures did not return",
            || format!("{before:?} -> {after:?}"),
        );
    }
}

/// The prepare handler holds every lock across `fork`, so a child can always
/// allocate, create a zone or start a thread. Other threads churn zones while
/// the main thread forks; each child marks shared memory before each step so
/// a hang says where.
fn fork_while_allocating(t: &mut T) {
    const STEPS: [&str; 5] = [
        "malloc_create_zone",
        "malloc_zone_malloc on a new zone",
        "starting a thread",
        "malloc on the new thread",
        "malloc(3 MiB)",
    ];
    let stacks = std::env::var("LIBMALLOC_STACKS").is_ok();
    let mut first_stack = None;
    const FORKS: usize = 2000;
    unsafe {
        let mark = libc::mmap(
            null(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
        .cast::<AtomicUsize>();
        assert_ne!(mark.cast::<c_void>(), libc::MAP_FAILED);
        let stop = Arc::new(AtomicBool::new(false));
        let mut busy = Vec::new();
        for i in 0..4u64 {
            let stop = stop.clone();
            busy.push(std::thread::spawn(move || {
                let mut rng = Rng::new(77 + i);
                while !stop.load(Ordering::Relaxed) {
                    let zone = m::malloc_create_zone(0, 0);
                    if rng.below(4) == 0 {
                        let a = m::malloc_zone_malloc(zone, 64);
                        let b = m::malloc_zone_malloc(zone, MIB + rng.below(MIB));
                        m::free(a);
                        m::free(b);
                    }
                    m::malloc_destroy_zone(zone);
                }
            }));
        }
        for _ in 0..2 {
            let stop = stop.clone();
            busy.push(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::spawn(|| m::free(m::malloc(32)))
                        .join()
                        .unwrap();
                }
            }));
        }
        std::thread::sleep(Duration::from_millis(100));
        let (mut forks, mut failed) = (0, 0);
        let mut hung: Vec<&str> = Vec::new();
        while forks < FORKS && hung.len() < 5 {
            forks += 1;
            (*mark).store(0, Ordering::SeqCst);
            let pid = libc::fork();
            if pid == 0 {
                if stacks {
                    libc::prctl(libc::PR_SET_PTRACER, libc::PR_SET_PTRACER_ANY);
                }
                (*mark).store(1, Ordering::SeqCst);
                let zone = m::malloc_create_zone(0, 0);
                (*mark).store(2, Ordering::SeqCst);
                let p = m::malloc_zone_malloc(zone, 64);
                m::free(p);
                m::malloc_destroy_zone(zone);
                (*mark).store(3, Ordering::SeqCst);
                let shared = mark as usize;
                std::thread::spawn(move || {
                    (*(shared as *const AtomicUsize)).store(4, Ordering::SeqCst);
                    m::free(m::malloc(32))
                })
                .join()
                .unwrap();
                (*mark).store(5, Ordering::SeqCst);
                m::free(m::malloc(3 * MIB));
                libc::_exit(0);
            }
            let mut status = 0;
            let on_hang = |pid: libc::pid_t| {
                if stacks && first_stack.is_none() {
                    let out = Command::new("eu-stack")
                        .args(["-p", &pid.to_string()])
                        .output();
                    first_stack = Some(match out {
                        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
                        Err(e) => format!("eu-stack: {e}"),
                    });
                }
            };
            if !wait_with_timeout_then(pid, Duration::from_secs(5), &mut status, on_hang) {
                let step = (*mark).load(Ordering::SeqCst);
                hung.push(
                    step.checked_sub(1)
                        .and_then(|s| STEPS.get(s).copied())
                        .unwrap_or("before its first step"),
                );
            } else if !(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0) {
                failed += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        for thread in busy {
            thread.join().unwrap();
        }
        t.expect(hung.is_empty(), "a forked child hung", || {
            format!(
                "{} of {forks} children, in: {}",
                hung.len(),
                hung.join(", ")
            )
        });
        t.expect(failed == 0, "a forked child failed", || {
            format!("{failed} of {forks} children")
        });
        if let Some(stack) = first_stack {
            eprintln!("the first child that hung:\n{stack}");
        }
        libc::munmap(mark.cast(), 4096);
    }
}

// What upstream does with a program's memory errors. On LP64 libmalloc always
// sets MALLOC_ABORT_ON_CORRUPTION (`set_flags_from_environment`), so each of
// these reports the error and aborts.

/// `find_zone_and_free` with no owner: MALLOC_REPORT_CRASH.
fn bad_free_aborts(_: &mut T) {
    let local = [0u64; 4];
    unsafe { m::free(local.as_ptr().cast_mut().cast()) };
}

/// `_realloc` with no owner: MALLOC_REPORT_CRASH.
fn bad_realloc_aborts(t: &mut T) {
    let mut local = [0u64; 8];
    let r = unsafe { m::realloc(local.as_mut_ptr().cast(), 128) };
    t.note(format!("realloc returned {r:p}"));
}

/// `free_tiny`: "Double free of object".
fn double_free_aborts(t: &mut T) {
    unsafe {
        let _keep: Vec<_> = (0..8).map(|_| m::malloc(64)).collect();
        let p = m::malloc(64);
        m::free(p);
        m::free(p);
        let later: Vec<usize> = (0..4096).map(|_| m::malloc(64) as usize).collect();
        let hits: Vec<usize> = (0..later.len())
            .filter(|&i| later[i] == p as usize)
            .collect();
        if hits.len() > 1 {
            t.note(format!(
                "then malloc(64) returned p, still live, as #{} and #{}",
                hits[0], hits[1]
            ));
        } else {
            t.note(format!(
                "p came back {} times in 4096 later blocks",
                hits.len()
            ));
        }
    }
}

/// `free_tiny` of an address that does not start a block: "pointer being
/// freed was not allocated".
fn interior_free_aborts(t: &mut T) {
    unsafe {
        let _keep: Vec<_> = (0..8).map(|_| m::malloc(64)).collect();
        let p = m::malloc(64);
        let inside = at(p, 16);
        m::free(inside);
        let later: Vec<_> = (0..64).map(|_| m::malloc(64)).collect();
        if let Some(i) = later.iter().position(|&q| q == inside) {
            t.note(format!(
                "then malloc(64) #{i} returned p + 16, inside the live block p"
            ));
        }
    }
}

/// `_szone_free`: "Non-aligned pointer being freed".
fn misaligned_free_aborts(t: &mut T) {
    unsafe {
        let _keep: Vec<_> = (0..8).map(|_| m::malloc(64)).collect();
        let p = m::malloc(64);
        m::free(at(p, 1));
        let later: Vec<_> = (0..64).map(|_| m::malloc(64)).collect();
        if let Some(q) = later.iter().find(|q| !(**q as usize).is_multiple_of(16)) {
            t.note(format!("then malloc(64) returned the misaligned {q:p}"));
        }
    }
}

/// `szone_free` of another zone's block finds it in no region of its own and
/// reaches `free_large`'s "pointer being freed was not allocated".
fn wrong_zone_free_aborts(t: &mut T) {
    unsafe {
        let zone = m::malloc_create_zone(0, 0);
        let p = m::malloc_zone_malloc(zone, 100);
        let before = stats(zone);
        m::malloc_zone_free(m::malloc_default_zone(), p);
        let after = stats(zone);
        if after.blocks_in_use == before.blocks_in_use {
            t.note(format!(
                "the block was freed, but its zone still counts it ({} blocks, {} bytes)",
                after.blocks_in_use, after.size_in_use
            ));
        } else {
            t.note("the block was freed, and its zone's figures follow".to_string());
        }
    }
}

/// `szone_realloc` of another zone's block: "pointer being reallocated was
/// not allocated", a corruption error.
fn wrong_zone_realloc_aborts(t: &mut T) {
    unsafe {
        let zone = m::malloc_create_zone(0, 0);
        let p = m::malloc(100);
        let q = m::malloc_zone_realloc(zone, p, 5000);
        t.note(format!(
            "returned a block of {}",
            if m::malloc_zone_from_ptr(q) == zone {
                "the target zone"
            } else {
                "another zone"
            }
        ));
    }
}

/// `_malloc_zone_malloc_with_options_outlined`: options it does not know are
/// a `__builtin_trap`.
fn unknown_options_trap(t: &mut T) {
    let p = unsafe { m::malloc_zone_malloc_with_options(null().cast(), 0, 16, 1 << 20) };
    t.note(format!("returned {p:p}"));
}

// The checks.

macro_rules! check {
    ($kind:ident, $source:expr, $name:ident) => {
        check!($kind, $source, $name, Returns, 120)
    };
    ($kind:ident, $source:expr, $name:ident, $expect:ident, $timeout:expr) => {
        Check {
            name: stringify!($name),
            kind: Kind::$kind,
            source: $source,
            expect: Expect::$expect,
            timeout_secs: $timeout,
            run: $name,
        }
    };
}

const CHECKS: &[Check] = &[
    check!(Test, "malloc_free_test.c", malloc_free_tiny),
    check!(Test, "malloc_free_test.c", malloc_free_small, Returns, 600),
    check!(Test, "malloc_free_test.c", malloc_free_large, Returns, 600),
    check!(Test, "malloc_free_test.c", malloc_free_null),
    check!(Test, "malloc_empty.c", empty_malloc_valid),
    check!(Test, "malloc_size_test.c", malloc_size_valid),
    check!(Test, "malloc_size_test.c", malloc_size_invalid),
    check!(Test, "malloc_size_test.c", malloc_size_freed),
    check!(Test, "malloc_size_test.c", malloc_size_large_allocation),
    check!(Test, "malloc_size_test.c", malloc_size_max_good_size),
    check!(Test, "calloc_test.c", calloc_overflow),
    check!(Test, "calloc_test.c", calloc_sizes, Returns, 600),
    check!(
        Test,
        "aligned_alloc_test.c",
        aligned_alloc_free,
        Returns,
        600
    ),
    check!(Test, "aligned_alloc_test.c", aligned_alloc_errors),
    check!(
        Test,
        "posix_memalign_test.c",
        posix_memalign_free,
        Returns,
        600
    ),
    check!(Test, "posix_memalign_test.c", posix_memalign_errors),
    check!(Test, "posix_memalign_test.c", posix_memalign_large),
    check!(Test, "posix_memalign_test.c", posix_memalign_enumerated),
    check!(Test, "realloc.c", realloc_failure),
    check!(Test, "realloc.c", reallocf_failure),
    check!(Test, "reallocarray.c", reallocarray_both),
    check!(Test, "malloc_realloc_large.c", realloc_large_huge),
    check!(Test, "zone_names.c", zone_names_default),
    check!(Test, "zone_names.c", zone_singletons),
    check!(Test, "zone_names.c", virtual_zone0),
    check!(Test, "zone_names.c", zone_creation_unnamed),
    check!(Test, "zone_names.c", set_zone_name_literal),
    check!(Test, "zone_names.c", set_zone_name_heap),
    check!(Test, "introspect_tests.c", szone_statistics),
    check!(Test, "introspect_tests.c", get_all_zones),
    check!(Test, "malloc_create_zone_tests.c", create_many_zones),
    check!(Test, "malloc_create_zone_tests.c", new_zone_statistics),
    check!(Test, "malloc_create_zone_tests.c", free_pointers_on_destroy),
    check!(Test, "malloc_create_zone_tests.c", create_zone_stress),
    check!(Test, "malloc_create_zone_tests.c", fork_with_zone),
    check!(Test, "malloc_zone_unregister_test.c", custom_default_zone),
    check!(
        Test,
        "malloc_claimed_address_tests.c",
        claimed_address_default
    ),
    check!(Test, "malloc_claimed_address_tests.c", zone_claimed_address),
    check!(
        Test,
        "malloc_claimed_address_tests.c",
        claimed_address_other_zone
    ),
    check!(Test, "malloc_with_options_test.c", malloc_options_random),
    check!(Test, "malloc_with_options_test.c", malloc_options_alignment),
    check!(
        Test,
        "malloc_create_purgeable_zone.c",
        purgeable_aligned_alloc
    ),
    check!(Test, "malloc_create_purgeable_zone.c", purgeable_realloc),
    check!(Test, "memory_pressure.c", pressure_relief_concurrent),
    check!(Source, "magazine_tiny.c TINY_QUANTUM", quantum_multiple),
    check!(
        Source,
        "magazine_malloc.c szone_good_size",
        good_size_predicts_size
    ),
    check!(
        Source,
        "magazine_large.c, xzone large",
        large_blocks_page_aligned
    ),
    check!(Source, "malloc.c _find_registered_zone", foreign_pointers),
    check!(Source, "malloc_common.h set_errno_fast", errno_on_failure),
    check!(Source, "malloc.c _realloc", realloc_contents),
    check!(Source, "malloc.c register/unregister", registry_order),
    check!(Source, "malloc.c malloc_zone_batch_*", batch_malloc_free),
    check!(Source, "malloc.c malloc_zone_statistics", statistics_totals),
    check!(Source, "malloc_type.c", typed_entry_points),
    check!(
        Source,
        "thread safety, cross-thread free",
        threaded_stress,
        Returns,
        300
    ),
    check!(
        Source,
        "malloc.c _malloc_fork_prepare",
        fork_while_allocating,
        Returns,
        600
    ),
    check!(
        Diagnostic,
        "malloc.c find_zone_and_free",
        bad_free_aborts,
        Aborts,
        60
    ),
    check!(
        Diagnostic,
        "malloc.c _realloc",
        bad_realloc_aborts,
        Aborts,
        60
    ),
    check!(
        Diagnostic,
        "magazine_tiny.c free_tiny",
        double_free_aborts,
        Aborts,
        60
    ),
    check!(
        Diagnostic,
        "magazine_tiny.c free_tiny",
        interior_free_aborts,
        Aborts,
        60
    ),
    check!(
        Diagnostic,
        "magazine_malloc.c _szone_free",
        misaligned_free_aborts,
        Aborts,
        60
    ),
    check!(
        Diagnostic,
        "magazine_large.c free_large",
        wrong_zone_free_aborts,
        Aborts,
        60
    ),
    check!(
        Diagnostic,
        "magazine_malloc.c szone_realloc",
        wrong_zone_realloc_aborts,
        Aborts,
        60
    ),
    check!(
        Diagnostic,
        "malloc.c malloc_with_options",
        unknown_options_trap,
        Aborts,
        60
    ),
];
