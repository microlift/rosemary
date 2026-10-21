//! What the `*_conformance` tests that port Apple's test suites share.
//!
//! Each upstream test case is ported case by case and calls what it tests
//! through the symbols a guest's imports bind to: [`darwin::provider`], which
//! is the loader's first provider, and then the host's, as the loader falls
//! back to it ([`darwin::symbol`]). A symbol neither answers is unbound, and
//! is unbound the way rosemary leaves it - the program still runs, and fails
//! at the first call through it - so a case runs up to that call and is
//! reported with what it had checked by then.
//!
//! Every case runs alone in a child process: some leave state behind (the
//! working directory, a resolver context), and a crash in one is that case's
//! result rather than the end of the run.
//!
//! Statuses:
//!
//! - `PASS`: every expectation met.
//! - `FAIL`: an expectation not met, a crash, or a timeout.
//! - `UNBOUND`: the case called something nothing a guest binds against
//!   provides.
//! - `SKIP`: upstream skips the case itself, or this host cannot give it what
//!   it needs (a network, root).
//!
//! A case is one of three kinds, scored apart: an upstream test as written; an
//! excerpt, the steps of an upstream test that exercise this crate, from a
//! test whose other steps test something else; and an adaptation, upstream's
//! assertions with an unbound call replaced by the bound one that does the
//! same work, so what *is* implemented underneath is still measured.
//!
//! `CONFORMANCE_STRICT=1` fails the test on any upstream case that does not
//! pass or skip, `CONFORMANCE_ONLY=a,b` runs only the cases named, and
//! `CONFORMANCE_REPORT=path` writes the results as JSON lines.

#![allow(dead_code)]

use std::cell::RefCell;
use std::ffi::c_int;
use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::panic::AssertUnwindSafe;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use machish::fixups::{LibraryRef, SymbolRef};
use machish::resolve::{SymbolProvider, SyntheticDylib};

const CHILD: &str = "CONFORMANCE_CASE";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Upstream,
    Excerpt,
    Adapted,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Upstream => "upstream",
            Kind::Excerpt => "excerpt",
            Kind::Adapted => "adapted",
        }
    }
}

pub struct Case {
    pub name: &'static str,
    pub kind: Kind,
    /// Where upstream has it, and what an excerpt or adaptation changed.
    pub source: &'static str,
    pub timeout_secs: u64,
    pub run: fn(&mut T) -> Result<(), Stop>,
}

/// Why a case stopped before its end.
pub enum Stop {
    /// `T_ASSERT`, `ATF_REQUIRE`, `atf_tc_fail`: the case cannot go on.
    Fail(String),
    /// Upstream's own skip, or what this host cannot provide.
    Skip(String),
    /// A call through an import nothing bound.
    Unbound(String),
}

/// The expectations one case made. Like darwintest's `T_EXPECT` and ATF's
/// `ATF_CHECK`, a failed [`T::check`] is recorded and the case goes on.
#[derive(Default)]
pub struct T {
    passed: usize,
    failures: Vec<String>,
    notes: Vec<String>,
}

impl T {
    /// `T_EXPECT_*`, `ATF_CHECK*`, testmore's `ok`.
    pub fn check(&mut self, ok: bool, what: impl FnOnce() -> String) -> bool {
        match ok {
            true => self.passed += 1,
            false => self.failures.push(what()),
        }
        ok
    }

    /// `T_ASSERT_*`, `ATF_REQUIRE*`: the case stops at the first that fails.
    pub fn require(&mut self, ok: bool, what: impl FnOnce() -> String) -> Result<(), Stop> {
        match ok {
            true => {
                self.passed += 1;
                Ok(())
            }
            false => Err(Stop::Fail(what())),
        }
    }

    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }
}

/// An import: what a guest's slot for `symbol` holds, or nothing.
#[derive(Clone, Copy)]
pub struct Import<F> {
    pub symbol: &'static str,
    value: Option<F>,
}

impl<F: Copy> Import<F> {
    /// The function or address, or the case stops as a call through an
    /// unbound slot would.
    pub fn get(&self) -> Result<F, Stop> {
        self.value
            .ok_or_else(|| Stop::Unbound(self.symbol.to_string()))
    }

    pub fn bound(&self) -> bool {
        self.value.is_some()
    }
}

thread_local! {
    static PROVIDER: RefCell<Option<SyntheticDylib>> = const { RefCell::new(None) };
}

/// The address a guest's import of `symbol` (Mach-O spelling) binds to.
pub fn address(symbol: &str) -> Option<u64> {
    let ours = PROVIDER.with_borrow_mut(|provider| {
        provider
            .get_or_insert_with(darwin::provider)
            .resolve(&SymbolRef {
                name: symbol,
                library: LibraryRef::Flat,
                weak_import: false,
            })
            .map(|resolved| resolved.addr)
    });
    ours.or_else(|| darwin::symbol(symbol))
        .filter(|&addr| addr != 0)
}

/// Bind `symbol`, a function or a datum, as a guest's import of it is bound.
/// `F` is the function pointer type, or a pointer to the datum.
pub fn import<F: Copy>(symbol: &'static str) -> Import<F> {
    bind(symbol, address(symbol))
}

/// `symbol` bound to `addr`, which a suite outside this crate looks up in the
/// tables its own framework publishes.
pub fn bind<F: Copy>(symbol: &'static str, addr: Option<u64>) -> Import<F> {
    assert_eq!(size_of::<F>(), size_of::<usize>(), "{symbol}: not a word");
    let value = addr.filter(|&addr| addr != 0).map(|addr| {
        // SAFETY: a word-sized function pointer or data pointer, which the
        // caller's type describes.
        unsafe { std::mem::transmute_copy::<usize, F>(&(addr as usize)) }
    });
    Import { symbol, value }
}

/// Bind each name as a guest's import, in one statement:
/// `imports! { query: unsafe extern "C" fn(..) -> c_int = "_res_9_query"; }`
#[allow(unused_macros)]
macro_rules! imports {
    ($($name:ident: $ty:ty = $symbol:literal;)*) => {
        $(let $name: $crate::conformance::Import<$ty> = $crate::conformance::import($symbol);)*
    };
}
#[allow(unused_imports)]
pub(crate) use imports;

/// The guest's `errno`, through Darwin's `___error`, in Darwin's numbering.
pub fn errno() -> c_int {
    let error: Import<unsafe extern "C" fn() -> *mut c_int> = import("___error");
    match error.get() {
        // SAFETY: `__error` answers this thread's slot.
        Ok(error) => unsafe { *error() },
        Err(_) => -1,
    }
}

pub fn set_errno(value: c_int) {
    let error: Import<unsafe extern "C" fn() -> *mut c_int> = import("___error");
    if let Ok(error) = error.get() {
        // SAFETY: as in [`errno`].
        unsafe { *error() = value };
    }
}

/// Darwin's `errno` values the ported tests name.
pub mod darwin_errno {
    pub const EPERM: i32 = 1;
    pub const ENOENT: i32 = 2;
    pub const EBADF: i32 = 9;
    pub const EFAULT: i32 = 14;
    pub const EINVAL: i32 = 22;
    pub const ENOTTY: i32 = 25;
    pub const ELOOP: i32 = 62;
    pub const ENOTCAPABLE: i32 = 107;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Unbound,
    Skip,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Fail => "FAIL",
            Status::Unbound => "UNBOUND",
            Status::Skip => "SKIP",
        }
    }

    fn parse(text: &str) -> Option<Status> {
        [Status::Pass, Status::Fail, Status::Unbound, Status::Skip]
            .into_iter()
            .find(|s| s.label() == text)
    }
}

struct Outcome {
    status: Status,
    detail: String,
    secs: f64,
}

/// Run `cases`, each in a child, and print what each did and a score.
///
/// `test` is the name of the `#[test]` calling this, which is how a child
/// finds its way back here.
pub fn run(test: &str, suite: &str, upstream: &str, cases: &[Case]) {
    if let Ok(name) = std::env::var(CHILD) {
        child(cases, &name);
    }
    let only: Option<Vec<String>> = std::env::var("CONFORMANCE_ONLY")
        .ok()
        .map(|list| list.split(',').map(str::to_string).collect());
    let cases: Vec<&Case> = cases
        .iter()
        .filter(|c| only.as_ref().is_none_or(|o| o.iter().any(|n| n == c.name)))
        .collect();
    let exe = std::env::current_exe().unwrap();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Outcome>>> = Mutex::new((0..cases.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(case) = cases.get(i) else { break };
                    let outcome = in_child(&exe, test, case);
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

    println!("\n{suite} conformance, against {upstream}\n");
    for (case, outcome) in cases.iter().zip(&results) {
        println!(
            "  {:<7}  {:<8}  {:<34} {:>6.1}s  {}",
            outcome.status.label(),
            case.kind.label(),
            case.name,
            outcome.secs,
            case.source
        );
        for line in outcome.detail.split("; ").filter(|l| !l.is_empty()) {
            println!("             {line}");
        }
    }
    println!();
    let mut deviations = 0;
    for kind in [Kind::Upstream, Kind::Excerpt, Kind::Adapted] {
        let of_kind: Vec<&Outcome> = cases
            .iter()
            .zip(&results)
            .filter(|(c, _)| c.kind == kind)
            .map(|(_, o)| o)
            .collect();
        if of_kind.is_empty() {
            continue;
        }
        let count = |status| of_kind.iter().filter(|o| o.status == status).count();
        let (pass, fail, unbound, skip) = (
            count(Status::Pass),
            count(Status::Fail),
            count(Status::Unbound),
            count(Status::Skip),
        );
        if kind == Kind::Upstream {
            deviations += fail + unbound;
        }
        println!(
            "  {:<8}  {pass}/{} pass  ({fail} fail, {unbound} unbound, {skip} skipped; {pass}/{} of those that ran to a verdict)",
            kind.label(),
            of_kind.len(),
            pass + fail,
        );
    }
    println!();

    if let Ok(path) = std::env::var("CONFORMANCE_REPORT") {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        for (case, outcome) in cases.iter().zip(&results) {
            writeln!(
                file,
                "{{\"suite\":{},\"name\":{},\"kind\":{},\"source\":{},\"status\":{},\"secs\":{:.2},\"detail\":{}}}",
                json(suite),
                json(case.name),
                json(case.kind.label()),
                json(case.source),
                json(outcome.status.label()),
                outcome.secs,
                json(&outcome.detail)
            )
            .unwrap();
        }
    }
    if std::env::var("CONFORMANCE_STRICT").is_ok() {
        assert_eq!(
            deviations, 0,
            "{deviations} upstream cases neither pass nor skip"
        );
    }
}

fn child(cases: &[Case], name: &str) -> ! {
    let case = cases
        .iter()
        .find(|c| c.name == name)
        .expect("a case of this suite");
    let mut t = T::default();
    let stopped = std::panic::catch_unwind(AssertUnwindSafe(|| (case.run)(&mut t)));
    let (status, mut detail) = match stopped {
        Ok(Ok(())) if t.failures.is_empty() => (Status::Pass, Vec::new()),
        Ok(Ok(())) => (Status::Fail, Vec::new()),
        Ok(Err(Stop::Fail(what))) => (Status::Fail, vec![format!("stopped: {what}")]),
        Ok(Err(Stop::Unbound(symbol))) => {
            (Status::Unbound, vec![format!("called unbound {symbol}")])
        }
        Ok(Err(Stop::Skip(why))) => (Status::Skip, vec![why]),
        Err(panic) => {
            let what = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            (Status::Fail, vec![format!("panicked: {what}")])
        }
    };
    let failed = t.failures.len();
    if failed > 0 {
        // The first few, and how many there were: a loop that fails every
        // iteration says so once.
        detail.push(format!(
            "{failed} expectation(s) failed, first: {}",
            t.failures[0]
        ));
        for more in t.failures.iter().skip(1).take(4) {
            detail.push(format!("also: {more}"));
        }
    }
    if status != Status::Skip {
        detail.push(format!("{} expectation(s) met", t.passed));
    }
    detail.extend(t.notes.iter().cloned());
    let line = detail.join("; ").replace('\n', " ");
    println!("@@RESULT {}\t{line}", status.label());
    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

fn in_child(exe: &std::path::Path, test: &str, case: &Case) -> Outcome {
    let start = Instant::now();
    let mut child = Command::new(exe)
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, case.name)
        .env_remove("CONFORMANCE_ONLY")
        .env_remove("CONFORMANCE_REPORT")
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
    let deadline = start + Duration::from_secs(case.timeout_secs);
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
    if timed_out {
        return Outcome {
            status: Status::Fail,
            detail: format!("timed out after {}s", case.timeout_secs),
            secs,
        };
    }
    let result = out
        .lines()
        .find_map(|l| l.split_once("@@RESULT ").map(|(_, r)| r.to_string()));
    match result.and_then(|r| {
        let (status, detail) = r.split_once('\t').unwrap_or((&r, ""));
        Some((Status::parse(status)?, detail.to_string()))
    }) {
        Some((status, detail)) => Outcome {
            status,
            detail,
            secs,
        },
        None => {
            let how = match status.signal() {
                Some(signal) => format!("crashed with signal {signal}"),
                None => format!("exited with {status} before reporting"),
            };
            let last = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
            Outcome {
                status: Status::Fail,
                detail: format!("{how}; last stderr line: {last}"),
                secs,
            }
        }
    }
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
