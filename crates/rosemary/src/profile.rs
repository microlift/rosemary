use std::collections::HashMap;
use std::ffi::CStr;
use std::io::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

use static_init::dynamic;

use crate::fault::{Attribution, Unnamed};

/// Samples per second per guest thread, when `ROSEMARY_PROFILE` names no rate.
const DEFAULT_HZ: u32 = 1000;

/// Seconds between running reports, when `ROSEMARY_PROFILE_EVERY` names none.
const DEFAULT_EVERY: u64 = 10;

/// How many lines of each ranked list stderr gets. The file, when one is asked
/// for, gets all of them.
const TOP: usize = 15;

/// The rate `ROSEMARY_PROFILE` asked for, or `None` if it asked for nothing.
fn configured_hz() -> Option<u32> {
    #[dynamic]
    static HZ: Option<u32> = {
        let raw = std::env::var("ROSEMARY_PROFILE").ok()?;
        match raw.trim() {
            "" | "0" | "no" | "off" => None,
            // `1` is "on", not "one sample a second".
            "1" | "yes" | "on" => Some(DEFAULT_HZ),
            n => n.parse().ok().filter(|&hz| hz > 0),
        }
    };
    *HZ
}

/// Whether sampling was asked for.
pub fn enabled() -> bool {
    configured_hz().is_some()
}

/// An environment variable parsed as a number, with a default.
fn number(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// `KERN_SUCCESS`.
const KERN_SUCCESS: i32 = 0;

/// `ARM_THREAD_STATE64`, from `<mach/arm/thread_status.h>`.
#[cfg(target_arch = "aarch64")]
const HOST_THREAD_STATE: i32 = 6;

/// `arm_thread_state64_t` in 32-bit words: `__x[29]`, `__fp`, `__lr`, `__sp`,
/// `__pc` - thirty-three doublewords - then `__cpsr` and its padding.
#[cfg(target_arch = "aarch64")]
const HOST_THREAD_STATE_COUNT: u32 = 68;

/// Index of `__pc` within that state, in doublewords. Twenty-nine `__x` registers, then
/// `__fp`, `__lr` and `__sp`.
#[cfg(target_arch = "aarch64")]
const PC_WORD: usize = 32;

/// `x86_THREAD_STATE64`, from `<mach/i386/thread_status.h>`.
#[cfg(target_arch = "x86_64")]
const HOST_THREAD_STATE: i32 = 4;

/// `x86_thread_state64_t` in 32-bit words: twenty-one doublewords, `__rip` among
/// them, with `__rflags` and the segment registers stored just as wide.
#[cfg(target_arch = "x86_64")]
const HOST_THREAD_STATE_COUNT: u32 = 42;

/// Index of `__rip` within that state, in doublewords - after the sixteen
/// general-purpose registers `_STRUCT_X86_THREAD_STATE64` lists first.
#[cfg(target_arch = "x86_64")]
const PC_WORD: usize = 16;

/// `THREAD_BASIC_INFO`, and its size in 32-bit words.
const THREAD_BASIC_INFO: u32 = 3;
const THREAD_BASIC_INFO_COUNT: u32 = 10;

/// Index of `run_state` in `thread_basic_info_data_t`: two `time_value_t` (two
/// `int` each), `cpu_usage`, `policy`, then `run_state`.
const RUN_STATE_WORD: usize = 6;

/// `TH_STATE_RUNNING`. Everything else - waiting, stopped, halted - is a thread
/// that is not consuming the CPU this is trying to account for.
const TH_STATE_RUNNING: i32 = 1;

/// `mach_timebase_info_data_t`: the rational number converting the absolute
/// clock's ticks to nanoseconds.
#[repr(C)]
#[derive(Clone, Copy)]
struct Timebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut Timebase) -> i32;
    fn mach_wait_until(deadline: u64) -> i32;
}

/// Nanoseconds as ticks of the absolute clock.
fn ticks(nanos: u64) -> u64 {
    #[dynamic]
    static BASE: (u32, u32) = {
        let mut base = Timebase { numer: 1, denom: 1 };
        // SAFETY: `base` is a live local of the type the call fills in.
        match unsafe { mach_timebase_info(&mut base) } == KERN_SUCCESS {
            true => (base.numer.max(1), base.denom.max(1)),
            false => (1, 1),
        }
    };
    let (numer, denom) = *BASE;
    // ticks = nanos * denom / numer.
    (u128::from(nanos) * u128::from(denom) / u128::from(numer)) as u64
}

// SAFETY (for every call below): these are the Mach thread routines exactly as
// `<mach/thread_act.h>` declares them. Each is given a port this process owns,
// a flavour whose buffer is sized by the matching `_COUNT` constant above, and
// an in-out count initialised to that size.
unsafe extern "C" {
    fn thread_suspend(target: u32) -> i32;
    fn thread_resume(target: u32) -> i32;
    fn thread_get_state(target: u32, flavor: i32, state: *mut u32, count: *mut u32) -> i32;
    fn thread_info(target: u32, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
}

/// One thread's `pc`, and whether it was on a CPU when it was read.
///
/// `None` when the thread could not be stopped - it has exited between the registry
/// naming its port and this call, which is the ordinary race and not an error.
fn sample_thread(port: u32) -> Option<(u64, bool)> {
    let mut info = [0i32; THREAD_BASIC_INFO_COUNT as usize];
    let mut info_count = THREAD_BASIC_INFO_COUNT;
    // SAFETY: `port` names a thread in this task and the buffer is exactly
    // `info_count` `int`s, which is what `THREAD_BASIC_INFO` writes.
    let got_info =
        unsafe { thread_info(port, THREAD_BASIC_INFO, info.as_mut_ptr(), &mut info_count) };
    // A thread whose run state could not be read is counted as running.
    let running = got_info != KERN_SUCCESS || info[RUN_STATE_WORD] == TH_STATE_RUNNING;

    // SAFETY: as above. A failed suspend leaves nothing to undo, which is why
    // the early return is before the resume.
    if unsafe { thread_suspend(port) } != KERN_SUCCESS {
        return None;
    }
    let mut state = [0u32; HOST_THREAD_STATE_COUNT as usize];
    let mut count = HOST_THREAD_STATE_COUNT;
    // SAFETY: the buffer is exactly `count` words of the flavour asked for.
    let got_state =
        unsafe { thread_get_state(port, HOST_THREAD_STATE, state.as_mut_ptr(), &mut count) };
    // SAFETY: balances the suspend above, which succeeded.
    unsafe { thread_resume(port) };

    if got_state != KERN_SUCCESS {
        return None;
    }
    let pc = u64::from(state[PC_WORD * 2]) | u64::from(state[PC_WORD * 2 + 1]) << 32;
    Some((pc, running))
}

/// One host symbol the guest's threads were found inside.
struct HostSite {
    count: u64,
    /// Leaf name of the image, which is what a reader recognises.
    image: String,
    /// The symbol, or `+offset` into the image when the symbol table has none.
    symbol: String,
    /// Whether the image is this runtime rather than one of the host's
    /// libraries. The distinction the roadmap's first question turns on.
    runtime: bool,
}

/// Everything the sampler has seen. One lock, taken by the sampler thread after
/// each resume and by whoever prints a report.
#[derive(Default)]
struct Counts {
    /// Samples taken on a thread that was on a CPU. The denominator every share
    /// below is a share of.
    running: u64,
    /// Samples taken on a thread that was parked. Counted and reported, never mixed in.
    blocked: u64,
    /// Guest instruction address -> samples. The histogram this module exists
    /// for.
    guest: HashMap<u64, u64>,
    /// First guest address of a region -> samples anywhere in its translation, including
    /// the parts no guest instruction was attributed to.
    region: HashMap<u64, u64>,
    /// Samples inside a translation that no guest instruction covered, by the
    /// reason they went unnamed. See [`Unnamed`].
    unnamed: [u64; 4],
    /// Samples in executable memory belonging to neither a loaded image nor a
    /// translation.
    unknown: u64,
    /// Host symbol start address -> what was found there.
    host: HashMap<u64, HostSite>,
    /// Samples in this runtime's own code, and in the host's libraries.
    runtime: u64,
    library: u64,
    /// Guest threads found in the registry on the most recent sweep.
    threads: usize,
}

/// Frames presented as of the previous report, and when that report was, so a rate
/// can be over the interval between two of them rather than over all time.
static LAST_FRAMES: Mutex<Option<(u64, Instant)>> = Mutex::new(None);

/// When sampling first started, which is what the *first* frame rate is measured
/// from - there is no previous report to difference against.
fn wall_origin() -> Instant {
    STARTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .unwrap_or_else(Instant::now)
}

static COUNTS: Mutex<Option<Counts>> = Mutex::new(None);

/// When sampling started, so a share can be a share of something.
static STARTED: Mutex<Option<Instant>> = Mutex::new(None);

/// Sweeps completed, which is what the sample rate is checked against.
static SWEEPS: AtomicU64 = AtomicU64::new(0);

/// Nanoseconds spent inside a sweep rather than waiting for the next one.
static SWEEP_NS: AtomicU64 = AtomicU64::new(0);

/// This binary's own load address, so a host sample can be told from a library one.
fn self_base() -> u64 {
    static BASE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| {
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        // SAFETY: `self_base` is a live function in this image and `info` is a
        // live local of the right type.
        let ok = unsafe { libc::dladdr(self_base as *const libc::c_void, &mut info) };
        match ok != 0 {
            true => info.dli_fbase as u64,
            false => 0,
        }
    })
}

/// A `dli_*` string, or `""`. Null is what `dladdr` reports for a symbol it
/// could not name, which is common inside a stripped library.
fn name(raw: *const libc::c_char) -> &'static str {
    if raw.is_null() {
        return "";
    }
    // SAFETY: `dladdr` fills these with pointers into the image's own string
    // tables, which live as long as the image, which is the process.
    unsafe { CStr::from_ptr(raw) }.to_str().unwrap_or("")
}

/// The slot in [`Counts::unnamed`] a reason is counted in. One function so the
/// order the array is written in and the order it is reported in cannot drift.
fn unnamed_index(why: Unnamed) -> usize {
    match why {
        Unnamed::Prologue => 0,
        Unnamed::Budget => 1,
        Unnamed::Gap => 2,
        Unnamed::Beyond => 3,
    }
}

/// Fold one `pc` into the counts.
fn attribute(counts: &mut Counts, pc: u64) {
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a live local; `dladdr` writes only into it and returns
    // zero when the address is in no loaded image.
    if unsafe { libc::dladdr(pc as *const libc::c_void, &mut info) } != 0 {
        let base = info.dli_fbase as u64;
        let runtime = base != 0 && base == self_base();
        match runtime {
            true => counts.runtime += 1,
            false => counts.library += 1,
        }
        // Keyed by the symbol's own address so two samples in one function are
        // one row, and by the image's base when the symbol table could not name
        // it - which buckets a whole stripped library into one row rather than
        // one row per sampled address.
        let key = match info.dli_saddr.is_null() {
            false => info.dli_saddr as u64,
            true => base,
        };
        let site = counts.host.entry(key).or_insert_with(|| {
            let image = name(info.dli_fname);
            let symbol = match name(info.dli_sname) {
                "" => format!("+{:#x}", pc.wrapping_sub(base)),
                s => s.to_string(),
            };
            HostSite {
                count: 0,
                image: image.rsplit('/').next().unwrap_or(image).to_string(),
                symbol,
                runtime,
            }
        });
        site.count += 1;
        return;
    }

    // No image: an anonymous mapping, which in this process means JIT code.
    match crate::fault::attribute(pc) {
        Attribution::Instruction { guest_va, region } => {
            *counts.guest.entry(guest_va).or_default() += 1;
            *counts.region.entry(region).or_default() += 1;
        }
        Attribution::Translated { region, why } => {
            counts.unnamed[unnamed_index(why)] += 1;
            *counts.region.entry(region).or_default() += 1;
        }
        // Executable memory belonging to neither an image nor a translation.
        Attribution::Elsewhere => counts.unknown += 1,
    }
}

/// Start sampling, if `ROSEMARY_PROFILE` asked for it. Idempotent.
///
/// Called from [`crate::startup::run`] beside [`crate::fault::install`] and for the
/// same reason.
pub fn start() {
    MAIN.store(crate::thread::key(), Relaxed);
    if configured_hz().is_some() {
        set_sampling(true);
    }
}

/// The guest's main thread, which [`start`] runs on.
static MAIN: AtomicU32 = AtomicU32::new(0);

/// Whether `ROSEMARY_PROFILE_THREAD=main` asked for the main thread alone: the
/// one a game's frame rate usually waits on, which the other threads' samples
/// would otherwise bury.
fn main_only() -> bool {
    #[dynamic]
    static MAIN_ONLY: bool = std::env::var("ROSEMARY_PROFILE_THREAD").is_ok_and(|v| v == "main");
    *MAIN_ONLY
}

/// Whether the sampler is taking samples right now.
static SAMPLING: AtomicBool = AtomicBool::new(false);

/// Whether sampling is on at this instant.
pub fn sampling() -> bool {
    SAMPLING.load(Relaxed)
}

/// Start or stop sampling. Returns the state it is now in.
pub fn set_sampling(on: bool) -> bool {
    if !on {
        SAMPLING.store(false, Relaxed);
        return false;
    }
    let mut guard = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(Counts::default());
    *STARTED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    SWEEPS.store(0, Relaxed);
    SWEEP_NS.store(0, Relaxed);
    drop(guard);
    ensure_thread();
    SAMPLING.store(true, Relaxed);
    true
}

/// Throw away everything sampled so far and restart the clock.
///
/// What `ROSEMARY_PROFILE_WINDOW` is for: the histogram is otherwise cumulative,
/// so a program whose first two minutes are translation reports those two minutes
/// forever and the steady state never rises out of them. Reset after each report
/// and every report describes only the interval it covers.
pub fn reset() {
    let mut guard = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_some() {
        *guard = Some(Counts::default());
    }
    *STARTED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    SWEEPS.store(0, Relaxed);
    SWEEP_NS.store(0, Relaxed);
}

/// Whether each periodic report covers only its own interval.
fn windowed() -> bool {
    #[dynamic]
    static ON: bool = matches!(
        std::env::var("ROSEMARY_PROFILE_WINDOW")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1" | "yes" | "on" | "true")
    );
    *ON
}

/// Start the sampler thread, once, whether or not the environment asked.
///
/// Separate from [`start`] because a run that never set `ROSEMARY_PROFILE` can
/// still be asked for a profile from [`crate::gui`], and the rate it gets is the
/// default one - there is no keystroke that could name a number.
fn ensure_thread() {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Relaxed) {
        return;
    }
    let hz = configured_hz().unwrap_or(DEFAULT_HZ);
    install_exit_report();

    let period = Duration::from_nanos(1_000_000_000 / u64::from(hz));
    let every = match number("ROSEMARY_PROFILE_EVERY", DEFAULT_EVERY) {
        0 => None,
        s => Some(Duration::from_secs(s)),
    };
    eprintln!(
        "rosemary: profile  sampling guest threads at {hz} Hz{}",
        match every {
            Some(d) => format!(", reporting every {}s", d.as_secs()),
            None => String::new(),
        }
    );

    std::thread::Builder::new()
        .name("rosemary-profile".into())
        .spawn(move || sweep_forever(period, every))
        .map(|_| ())
        // Not fatal. A runtime that refused to run a program because it could
        // not profile it would be trading the thing for the measurement of it.
        .unwrap_or_else(|e| eprintln!("rosemary: profile  could not start the sampler: {e}"));
}

/// The sampler thread.
///
/// Never stops: there is nothing to stop it *for*.
fn sweep_forever(period: Duration, every: Option<Duration>) {
    let period = ticks(period.as_nanos() as u64).max(1);
    // Jitter, up to a tenth of the period.
    let spread = period / 10 + 1;
    let mut jitter = Xorshift::seeded();
    let mut next = unsafe { mach_absolute_time() };
    let mut last_report = Instant::now();
    let mine = crate::thread::key();
    loop {
        next += period;
        let now = unsafe { mach_absolute_time() };
        // A sweep that took longer than its own period has nothing to make up
        // by spinning - the samples it missed are gone - so the deadline is
        // moved to now rather than chased.
        if next < now {
            next = now;
        }
        // SAFETY: a deadline on the same absolute clock the value came from.
        unsafe { mach_wait_until(next + jitter.next() % spread) };

        // Paused.
        if !SAMPLING.load(Relaxed) {
            continue;
        }

        let began = Instant::now();
        let mut guard = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(counts) = guard.as_mut() else {
            continue;
        };
        let mut found = 0;
        // The registry is scanned with the counts held, which is safe in the
        // one direction that matters.
        crate::thread::each_key(|port| {
            if port == mine || (main_only() && port != MAIN.load(Relaxed)) {
                return;
            }
            found += 1;
            let Some((pc, running)) = sample_thread(port) else {
                return;
            };
            match running {
                true => {
                    counts.running += 1;
                    attribute(counts, pc);
                }
                false => counts.blocked += 1,
            }
        });
        counts.threads = found;
        drop(guard);
        SWEEPS.fetch_add(1, Relaxed);
        SWEEP_NS.fetch_add(began.elapsed().as_nanos() as u64, Relaxed);

        if every.is_some_and(|d| last_report.elapsed() >= d) {
            report();
            // After the report, not before: the report is of the window that
            // just closed.
            if windowed() {
                reset();
            }
            last_report = Instant::now();
        }
    }
}

/// Sixty-four bits of jitter, without a dependency.
struct Xorshift(u64);

impl Xorshift {
    fn seeded() -> Self {
        // The clock, which is the only entropy this needs and the only source
        // available without pulling in a crate.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x2545_f491_4f6c_dd1d, |d| d.as_nanos() as u64);
        Xorshift(nanos | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Print the report when the process ends, however it ends.
fn install_exit_report() {
    // `atexit` rather than a `Drop` guard, for the reason [`crate::stats`]
    // gives.
    extern "C" fn report_at_exit() {
        report();
    }
    // SAFETY: `atexit` takes an `extern "C" fn()`, which this is.
    unsafe { libc::atexit(report_at_exit) };
}

/// One row of a ranked list, already resolved.
struct Row {
    count: u64,
    /// The address, as the guest's own program sees it.
    va: u64,
    /// `image+offset`, or empty for code that belongs to no image - which is
    /// code the guest generated, and is the honest answer for it.
    located: String,
}

/// Rank a histogram, resolving each key through the program's image map.
fn rank(histogram: &HashMap<u64, u64>) -> Vec<Row> {
    let vm = crate::thunk::current();
    let mut rows: Vec<Row> = histogram
        .iter()
        .map(|(&va, &count)| Row {
            count,
            va,
            located: vm.and_then(|vm| vm.program.locate(va)).unwrap_or_default(),
        })
        .collect();
    // By count, then by address, so two runs of the same program print the
    // same order for a tie rather than whatever the hash map felt like.
    rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.va.cmp(&b.va)));
    rows
}

/// Print what the guest's threads were doing, if sampling was asked for.
pub fn report() {
    let guard = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(counts) = guard.as_ref() else { return };
    let taken = counts.running + counts.blocked;
    if taken == 0 {
        return;
    }
    let wall = STARTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .map(|t| t.elapsed())
        .unwrap_or_default();
    let share = |n: u64| 100.0 * n as f64 / counts.running.max(1) as f64;

    let translated = counts.guest.values().sum::<u64>() + counts.unnamed.iter().sum::<u64>();
    // The *achieved* rate, not the configured one.
    let sweeps = SWEEPS.load(Relaxed);
    let achieved = sweeps as f64 / wall.as_secs_f64().max(f64::EPSILON);
    let in_sweep = Duration::from_nanos(SWEEP_NS.load(Relaxed));
    eprintln!(
        "rosemary: profile  {taken} sample(s) over {wall:.1?} on {} guest thread(s) \
         ({} running, {} blocked)",
        counts.threads, counts.running, counts.blocked,
    );
    eprintln!(
        "rosemary: profile  {achieved:.0} sweep(s)/s, {:.1?} per sweep in the sampler \
         ({:.2}% of one core)",
        in_sweep
            .checked_div(sweeps.max(1) as u32)
            .unwrap_or_default(),
        100.0 * in_sweep.as_secs_f64() / wall.as_secs_f64().max(f64::EPSILON),
    );
    // The frame rate, before anything else: it is the only number here that is
    // the user's own experience rather than a share of it. Over the interval since
    // the previous report, so under `ROSEMARY_PROFILE_WINDOW` it tracks the window
    // and not the two minutes of loading that preceded it.
    {
        let now = Instant::now();
        let frames = crate::stats::frames();
        let mut last = LAST_FRAMES.lock().unwrap_or_else(|e| e.into_inner());
        let (since, at) = last.unwrap_or((0, wall_origin()));
        let elapsed = now.duration_since(at).as_secs_f64();
        *last = Some((frames, now));
        drop(last);
        match frames.saturating_sub(since) {
            0 => eprintln!(
                "rosemary: profile  0 frame(s) presented - the guest is not drawing, \
                 or presents by a path this runtime does not hook"
            ),
            drawn => eprintln!(
                "rosemary: profile  {:.1} fps ({drawn} frame(s) in {:.1}s), {:.1?} per frame",
                drawn as f64 / elapsed.max(f64::EPSILON),
                elapsed,
                Duration::from_secs_f64(elapsed / drawn as f64),
            ),
        }
    }
    eprintln!(
        "rosemary: profile  translated {:.1}%  runtime {:.1}%  host libraries {:.1}%  \
         unaccounted {:.1}%  (of {} running sample(s))",
        share(translated),
        share(counts.runtime),
        share(counts.library),
        share(counts.unknown),
        counts.running,
    );

    let instructions = rank(&counts.guest);
    let regions = rank(&counts.region);
    let mut hosts: Vec<&HostSite> = counts.host.values().collect();
    hosts.sort_by(|a, b| b.count.cmp(&a.count).then(a.symbol.cmp(&b.symbol)));

    // Regions before instructions, and the regions are the ranking to believe:
    // Cranelift attributes no source location to a prologue, an epilogue, the entry
    // dispatch, or to anything its optimiser produced without one, measured between
    // 2% and 47% of running samples on one fixture. Every sample still carries a
    // region, so the rollup is complete and the instruction ranking is a lead.
    say(
        "guest regions, by share of running time",
        &regions[..],
        share,
    );
    say("guest instructions", &instructions[..], share);

    eprintln!("rosemary: profile  host symbols");
    for site in hosts.iter().take(TOP) {
        eprintln!(
            "rosemary: profile    {:>6.2}%  {} {} {}",
            share(site.count),
            match site.runtime {
                true => "[runtime]",
                false => "[library]",
            },
            site.image,
            site.symbol,
        );
    }
    for (n, what) in [
        (
            counts.unnamed[unnamed_index(Unnamed::Prologue)],
            "[translated] the entry stub and `rip` dispatch - runtime cost per host \
             entry, not guest work",
        ),
        (
            counts.unnamed[unnamed_index(Unnamed::Budget)],
            "[translated] in a region whose source map the budget discarded - raise \
             ROSEMARY_MAP_BUDGET (MiB) to name these",
        ),
        (
            counts.unnamed[unnamed_index(Unnamed::Gap)],
            "[translated] inside a mapped region, in a range no guest instruction \
             claimed",
        ),
        (
            counts.unnamed[unnamed_index(Unnamed::Beyond)],
            "[translated] past every range a region attributed, so the region \
             is an inference (probably its epilogue)",
        ),
    ] {
        if n > 0 {
            eprintln!("rosemary: profile    {:>6.2}%  {what}", share(n));
        }
    }

    if let Ok(path) = std::env::var("ROSEMARY_PROFILE_OUT") {
        if let Err(e) = write_histogram(&path, &instructions, &regions, share) {
            eprintln!("rosemary: profile  could not write {path}: {e}");
        }
    }
    // Dropped before the map is written: `write_map` takes the *fault* module's
    // lock, and holding two is how an ordering bug is written.
    drop(guard);

    if let Ok(path) = std::env::var("ROSEMARY_PROFILE_MAP") {
        if let Err(e) = write_map(&path) {
            eprintln!("rosemary: profile  could not write {path}: {e}");
        }
    }

    // And [`crate::stats`], which has the same problem this module solves and
    // one fewer way to solve it.
    crate::stats::report();
}

/// One row of a ranked list, resolved and with its share already worked out.
#[derive(Clone)]
pub struct Entry {
    pub count: u64,
    pub share: f64,
    /// The address, as the guest's own program sees it.
    pub va: u64,
    /// `image+offset`, or empty for code that belongs to no image - which is
    /// code the guest generated, and is the honest answer for it.
    pub located: String,
}

/// One host symbol the guest's threads were found inside.
#[derive(Clone)]
pub struct HostEntry {
    pub count: u64,
    pub share: f64,
    pub image: String,
    pub symbol: String,
    /// This runtime, rather than one of the host's libraries.
    pub runtime: bool,
}

/// What the sampler has seen, ranked, for a consumer that is not stderr.
///
/// [`report`] renders the same counts as prose.
#[derive(Clone)]
pub struct Snapshot {
    /// Wall time since sampling started; the denominator for a rate.
    pub wall: Duration,
    /// Samples on a thread that was on a CPU. Every share below is of this.
    pub running: u64,
    /// Samples on a thread that was parked, kept out of the denominator.
    pub blocked: u64,
    /// Guest threads found in the registry on the most recent sweep.
    pub threads: usize,
    /// Sweeps completed, and the rate that works out to. The *achieved* rate,
    /// which is the only one worth showing - see the module note.
    pub sweeps: u64,
    pub sweeps_per_second: f64,
    /// Share of one core the sampler itself is costing.
    pub sampler_share: f64,
    /// Shares of running time, in the four buckets that decide what to work on.
    pub translated: f64,
    pub runtime: f64,
    pub library: f64,
    pub unaccounted: f64,
    /// The two rankings, longest first. **Believe the first one**.
    pub regions: Vec<Entry>,
    pub instructions: Vec<Entry>,
    pub host: Vec<HostEntry>,
}

/// Rank everything the sampler has, keeping the top `top` of each list.
///
/// `None` when sampling was never asked for, which a caller must show as
/// "`ROSEMARY_PROFILE` is not set" rather than as an empty profile.
pub fn snapshot(top: usize) -> Option<Snapshot> {
    let guard = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    let counts = guard.as_ref()?;
    let wall = STARTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .map(|t| t.elapsed())
        .unwrap_or_default();
    let share = |n: u64| 100.0 * n as f64 / counts.running.max(1) as f64;
    let translated = counts.guest.values().sum::<u64>() + counts.unnamed.iter().sum::<u64>();
    let sweeps = SWEEPS.load(Relaxed);
    let in_sweep = Duration::from_nanos(SWEEP_NS.load(Relaxed));

    let entries = |rows: Vec<Row>| -> Vec<Entry> {
        rows.into_iter()
            .take(top)
            .map(|row| Entry {
                count: row.count,
                share: share(row.count),
                va: row.va,
                located: row.located,
            })
            .collect()
    };
    let mut hosts: Vec<&HostSite> = counts.host.values().collect();
    hosts.sort_by(|a, b| b.count.cmp(&a.count).then(a.symbol.cmp(&b.symbol)));

    Some(Snapshot {
        wall,
        running: counts.running,
        blocked: counts.blocked,
        threads: counts.threads,
        sweeps,
        sweeps_per_second: sweeps as f64 / wall.as_secs_f64().max(f64::EPSILON),
        sampler_share: 100.0 * in_sweep.as_secs_f64() / wall.as_secs_f64().max(f64::EPSILON),
        translated: share(translated),
        runtime: share(counts.runtime),
        library: share(counts.library),
        unaccounted: share(counts.unknown),
        regions: entries(rank(&counts.region)),
        instructions: entries(rank(&counts.guest)),
        host: hosts
            .into_iter()
            .take(top)
            .map(|site| HostEntry {
                count: site.count,
                share: share(site.count),
                image: site.image.clone(),
                symbol: site.symbol.clone(),
                runtime: site.runtime,
            })
            .collect(),
    })
}

/// One ranked list on stderr, top [`TOP`] only.
fn say(what: &str, rows: &[Row], share: impl Fn(u64) -> f64) {
    if rows.is_empty() {
        return;
    }
    eprintln!("rosemary: profile  {what}");
    for row in rows.iter().take(TOP) {
        eprintln!(
            "rosemary: profile    {:>6.2}%  {:#018x}  {}",
            share(row.count),
            row.va,
            match row.located.is_empty() {
                // Not a gap.
                true => "(generated code)",
                false => &row.located,
            },
        );
    }
}

/// The whole histogram, for the run that wants more than the top of it.
fn write_histogram(
    path: &str,
    instructions: &[Row],
    regions: &[Row],
    share: impl Fn(u64) -> f64,
) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(out, "# rosemary guest profile, pid {}", std::process::id())?;
    writeln!(out, "# kind\tsamples\tshare\tguest_va\timage+offset")?;
    for (kind, rows) in [("region", regions), ("instruction", instructions)] {
        for row in rows {
            writeln!(
                out,
                "{kind}\t{}\t{:.4}\t{:#x}\t{}",
                row.count,
                share(row.count),
                row.va,
                match row.located.is_empty() {
                    true => "-",
                    false => &row.located,
                },
            )?;
        }
    }
    out.flush()
}

/// Emit the host-address-to-guest-address map an external profiler needs.
///
/// ```text
/// R 0x10a4c0000 0x10a4c1a8 0x100002340 0x1000023f0 Mac_Runner+0x2340
/// I 0x10a4c0000 0x10a4c018 0x100002340 Mac_Runner+0x2340
/// ```
pub fn write_map(path: &str) -> std::io::Result<()> {
    let instructions = number("ROSEMARY_PROFILE_MAP_INSTRUCTIONS", 0) != 0;
    let vm = crate::thunk::current();
    let locate = |va: u64| {
        vm.and_then(|vm| vm.program.locate(va))
            .unwrap_or_else(|| "-".to_string())
    };

    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(out, "# rosemary jit map, pid {}", std::process::id())?;
    writeln!(
        out,
        "# R host_start host_end guest_start guest_end image+offset"
    )?;
    writeln!(out, "# I host_start host_end guest_va image+offset")?;

    let mut error = None;
    crate::fault::for_each_placed(|region, ranges| {
        if error.is_some() {
            return;
        }
        let mut write = || -> std::io::Result<()> {
            writeln!(
                out,
                "R {:#x} {:#x} {:#x} {:#x} {}",
                region.code,
                region.code_end,
                region.guest.0,
                region.guest.1,
                locate(region.guest.0),
            )?;
            if instructions {
                for r in ranges {
                    writeln!(
                        out,
                        "I {:#x} {:#x} {:#x} {}",
                        region.code + u64::from(r.start),
                        region.code + u64::from(r.end),
                        r.guest_va,
                        locate(r.guest_va),
                    )?;
                }
            }
            Ok(())
        };
        error = write().err();
    });
    match error {
        Some(e) => Err(e),
        None => out.flush(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_switch_is_a_switch_and_a_number_is_a_rate() {
        // `ROSEMARY_PROFILE=1` must mean "on", not "one sample a second".
        let parse = |raw: &str| -> Option<u32> {
            match raw.trim() {
                "" | "0" | "no" | "off" => None,
                "1" | "yes" | "on" => Some(DEFAULT_HZ),
                n => n.parse().ok().filter(|&hz| hz > 0),
            }
        };
        assert_eq!(parse("1"), Some(DEFAULT_HZ));
        assert_eq!(parse("on"), Some(DEFAULT_HZ));
        assert_eq!(parse("4000"), Some(4000));
        assert_eq!(parse("0"), None);
        assert_eq!(parse("off"), None);
        assert_eq!(parse("nonsense"), None);
    }

    #[test]
    fn the_pc_word_index_agrees_with_the_offset_the_fault_handler_measured() {
        // `crate::fault::host_pc` reads the same register out of the same
        // structure, at an offset measured with `offsetof` against this
        // system's headers - so the two must not drift apart.
        #[cfg(target_arch = "aarch64")]
        {
            assert_eq!(PC_WORD * 8, 256);
            // And the state is thirty-three doublewords plus `__cpsr` and its pad.
            assert_eq!(HOST_THREAD_STATE_COUNT as usize, 33 * 2 + 2);
        }
        #[cfg(target_arch = "x86_64")]
        {
            assert_eq!(PC_WORD * 8, 128);
            // Twenty-one doublewords, every field of them eight bytes wide.
            assert_eq!(HOST_THREAD_STATE_COUNT as usize, 21 * 2);
        }
    }

    #[test]
    fn jitter_stays_inside_a_tenth_of_the_period_and_moves() {
        let period = Duration::from_millis(1);
        let bound = period.as_nanos() as u64 / 10 + 1;
        let mut x = Xorshift::seeded();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let j = x.next() % bound;
            assert!(j < bound);
            seen.insert(j);
        }
        // A generator that returned a constant would still satisfy the bound
        // and would defeat the entire point of jittering.
        assert!(seen.len() > 1, "the jitter never moved");
    }

    #[test]
    fn ranking_is_by_count_and_ties_break_on_the_address() {
        let mut h = HashMap::new();
        h.insert(0x2000, 5);
        h.insert(0x1000, 5);
        h.insert(0x3000, 9);
        let rows = rank(&h);
        assert_eq!(
            rows.iter().map(|r| r.va).collect::<Vec<_>>(),
            [0x3000, 0x1000, 0x2000],
            "a tie must not depend on hash order, or two runs disagree"
        );
    }
}
