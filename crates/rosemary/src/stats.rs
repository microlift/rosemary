use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// Whether the exit histogram is being counted, right now.
///
/// What a caller must know: **counters do not rewind**.
///
/// A switch rather than a `OnceLock`, because this is the counter documented as
/// perturbing the program it measures: one relaxed `fetch_add` per region entry, at
/// fifteen million entries a second, measured at 44% of a steady-state window with it
/// off and 61% with it on. The read stays one relaxed load and a compare.
pub fn enabled() -> bool {
    match COUNTING.load(Relaxed) {
        ON => true,
        OFF => false,
        _ => from_environment(),
    }
}

const UNKNOWN: u8 = 0;
const OFF: u8 = 1;
const ON: u8 = 2;

static COUNTING: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(UNKNOWN);

/// The first read, which is what `ROSEMARY_STATS` decides.
#[cold]
fn from_environment() -> bool {
    let on = std::env::var_os("ROSEMARY_STATS").is_some();
    COUNTING.store(if on { ON } else { OFF }, Relaxed);
    on
}

/// Turn the exit histogram on or off. Returns the state it is now in.
pub fn set_enabled(on: bool) -> bool {
    COUNTING.store(if on { ON } else { OFF }, Relaxed);
    if on {
        // A run that started without `ROSEMARY_STATS` never started the clock
        // the report is a share of, and never installed the exit report.
        start_inner();
    }
    on
}

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        $(static $name: AtomicU64 = AtomicU64::new(0);)*
    };
}

counters! {
    REGIONS, INSTRUCTIONS, BYTES, CODE_BYTES, CACHED_REGIONS, CACHED_CODE_BYTES,
    SELECT_NS, LIFT_NS, COMPILE_NS,
    REFUSED, TAIL_EXITS, STALE_LIFTS, RETIRED,
    ENTRIES, WINDOW_MISSES, TAIL_IMPORTS, LINKED,
    DISPATCHES, DISPATCH_HITS, DISPATCH_EVICTIONS,
    PUBLISHES, PUBLISHED_ROWS, RELINKS,
    FRAMES,
}

/// One frame presented by the guest.
///
/// **Not gated on [`enabled`]**, unlike everything else here, and not gated on the
/// overlay either: the frame rate is the number this whole runtime is judged on,
/// and it must not cost a keystroke - or the overlay's own frame cost - to see. One
/// relaxed add at thirty to a hundred hertz is not a measurable perturbation.
pub fn record_frame() {
    FRAMES.fetch_add(1, Relaxed);
}

/// Frames presented since the process started. Never rewinds.
pub fn frames() -> u64 {
    FRAMES.load(Relaxed)
}

pub fn record_relink() {
    if enabled() {
        RELINKS.fetch_add(1, Relaxed);
    }
}

pub fn record_published(rows: usize) {
    if !enabled() {
        return;
    }
    PUBLISHES.fetch_add(1, Relaxed);
    PUBLISHED_ROWS.fetch_add(rows as u64, Relaxed);
}

/// One indirect call dispatched by the host, and whether this thread had already
/// been told where that address goes.
///
/// The figure [`crate::dispatch`] is judged on: a miss is four lookups and a hit is
/// a compare, so the rate is most of what the mechanism is worth.
pub fn record_dispatch(hit: bool) {
    if !enabled() {
        return;
    }
    DISPATCHES.fetch_add(1, Relaxed);
    if hit {
        DISPATCH_HITS.fetch_add(1, Relaxed);
    }
}

/// A verdict that displaced *another* address from its slot.
///
/// The one number that separates a table too small from a table that hashes badly:
/// a working set larger than the table evicts on nearly every miss, and no amount of
/// associativity fixes that. Measured before this table was left direct-mapped.
pub fn record_dispatch_eviction() {
    if enabled() {
        DISPATCH_EVICTIONS.fetch_add(1, Relaxed);
    }
}

/// How a region entry ended.
#[derive(Clone, Copy)]
pub enum Exit {
    /// The end of a call this runtime made: the `int3` of the return stub the entry
    /// pushed, reached by the callee's final `ret`. See [`crate::retstub`].
    Return,
    /// A `ret` that popped an address outside the region - the ordinary way a
    /// leaf function ends, and one region entry each time.
    ReturnOut,
    /// An unconditional `jmp` to a static address outside the region.
    DirectJump,
    /// A conditional branch whose taken edge left the region.
    CondJump,
    /// Control ran off the bottom of the bytes the lifter was given.
    Fallthrough,
    /// A direct `call` to guest code that was not lifted into this region.
    DirectCall,
    /// The translation declined to serve the PC - a region cut mid-instruction, or an
    /// entry point its dispatch was never offered. See [`record_tail_exit`].
    RegionTail,
    /// A tail `jmp` through a register or a memory operand.
    IndirectJump,
    /// Any other transfer out of the region.
    OtherJump,
    /// An intercepted `call` to a registered import.
    Import,
    /// An indirect `call` - a virtual call, or a call through a pointer.
    Indirect,
    /// `cpuid`, `xgetbv`, and the rest of the resumable one-instruction exits.
    Instruction,
    /// Anything else, which is an error path.
    Other,
}

impl Exit {
    /// Every arm, in report order.
    const ALL: [Exit; 13] = [
        Exit::Return,
        Exit::ReturnOut,
        Exit::DirectJump,
        Exit::CondJump,
        Exit::Fallthrough,
        Exit::DirectCall,
        Exit::RegionTail,
        Exit::IndirectJump,
        Exit::OtherJump,
        Exit::Import,
        Exit::Indirect,
        Exit::Instruction,
        Exit::Other,
    ];

    /// Whether a region ending this way carries a link probe.
    ///
    /// The exits that do not are the ones the host owes work before anything can
    /// continue at a guest address: an import call, an unregistered indirect call,
    /// a `cpuid`, the `int3` of a return stub, an error. Everything
    /// else is answered by *entering whatever covers `rip`*, which is what a link
    /// does. Kept in step with the `emit_exit` call sites in
    /// `microlift::core` and `microlift::arch::x86_64::semantics`.
    pub fn linkable(self) -> bool {
        matches!(
            self,
            Exit::ReturnOut
                | Exit::DirectJump
                | Exit::CondJump
                | Exit::Fallthrough
                | Exit::DirectCall
                | Exit::RegionTail
                | Exit::IndirectJump
                | Exit::OtherJump
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Exit::Return => "terminating block",
            Exit::ReturnOut => "ret out of region",
            Exit::DirectJump => "direct jmp",
            Exit::CondJump => "conditional branch",
            Exit::Fallthrough => "fallthrough",
            Exit::DirectCall => "direct call",
            Exit::RegionTail => "region tail",
            Exit::IndirectJump => "indirect jmp",
            Exit::OtherJump => "other jump",
            Exit::Import => "import call",
            Exit::Indirect => "indirect call",
            Exit::Instruction => "instruction",
            Exit::Other => "other",
        }
    }
}

/// One counter per [`Exit`].
static EXITS: [AtomicU64; Exit::ALL.len()] = [const { AtomicU64::new(0) }; Exit::ALL.len()];

/// The same histogram, restricted to exits that carried a probe and came back
/// anyway. See [`record_probe_miss`].
static PROBE_MISSES: [AtomicU64; Exit::ALL.len()] = [const { AtomicU64::new(0) }; Exit::ALL.len()];

/// One region entry: the event whose *rate* is the whole question.
pub fn record_entry(missed_window: bool) {
    if !enabled() {
        return;
    }
    ENTRIES.fetch_add(1, Relaxed);
    if missed_window {
        WINDOW_MISSES.fetch_add(1, Relaxed);
    }
}

/// A region entry the run loop never saw.
pub fn record_linked_entry() {
    if !enabled() {
        return;
    }
    ENTRIES.fetch_add(1, Relaxed);
    LINKED.fetch_add(1, Relaxed);
}

/// A region entry that turned out to be a *tail call to an import*: the run loop's
/// first question, asked before every entry and answered "yes" only here.
pub fn record_tail_import() {
    if enabled() {
        TAIL_IMPORTS.fetch_add(1, Relaxed);
    }
}

/// How one region entry ended.
pub fn record_exit(exit: Exit) {
    if enabled() {
        EXITS[exit as usize].fetch_add(1, Relaxed);
    }
}

/// An exit that could have linked and did not - because its probe declined, or
/// because `ROSEMARY_NO_BLOCK_LINK` meant it carried none.
///
/// Free, and it is the figure block linking is actually judged on. A linked exit
/// never reaches the run loop, so counting the ones that do needs no counter on
/// the path the mechanism exists to shorten - which is why this can be per kind
/// where [`record_linked_entry`] cannot be at all under a pinned ABI.
pub fn record_probe_miss(exit: Exit) {
    if exit.linkable() && enabled() {
        PROBE_MISSES[exit as usize].fetch_add(1, Relaxed);
    }
}

/// One completed region selection: how long it took to decide the extent.
pub fn record_select(took: Duration) {
    SELECT_NS.fetch_add(took.as_nanos() as u64, Relaxed);
}

/// How many regions between progress reports.
const REPORT_EVERY: u64 = 2048;

/// One completed translation.
pub fn record_lift(instructions: usize, bytes: usize, lift: Duration, compile: Duration) {
    let n = REGIONS.fetch_add(1, Relaxed) + 1;
    INSTRUCTIONS.fetch_add(instructions as u64, Relaxed);
    BYTES.fetch_add(bytes as u64, Relaxed);
    LIFT_NS.fetch_add(lift.as_nanos() as u64, Relaxed);
    COMPILE_NS.fetch_add(compile.as_nanos() as u64, Relaxed);
    if n.is_multiple_of(REPORT_EVERY) {
        report();
    }
}

/// Host code the JIT emitted for one region, its GOT entries included.
pub fn record_code(bytes: usize) {
    if enabled() {
        CODE_BYTES.fetch_add(bytes as u64, Relaxed);
    }
}

/// A region published from the code cache rather than compiled.
pub fn record_cached(code_bytes: u32) {
    if enabled() {
        CACHED_REGIONS.fetch_add(1, Relaxed);
        CACHED_CODE_BYTES.fetch_add(code_bytes.into(), Relaxed);
    }
}

/// A region that could not be translated at all.
pub fn record_refused() {
    REFUSED.fetch_add(1, Relaxed);
}

/// A lift of generated code whose bytes changed before it could be published.
pub fn record_stale_lift() {
    STALE_LIFTS.fetch_add(1, Relaxed);
}

/// Addresses unpublished because a write changed the code behind them.
pub fn record_retired(n: usize) {
    RETIRED.fetch_add(n as u64, Relaxed);
}

/// An exit taken because the translation could not serve the PC it was entered at -
/// either the instruction there runs past the bytes it was given, or its dispatch was
/// never offered that address.
pub fn record_tail_exit() {
    TAIL_EXITS.fetch_add(1, Relaxed);
}

/// The pages a lifted region touched, at both page sizes that matter.
static PAGES: std::sync::Mutex<
    Option<(
        std::collections::BTreeSet<u64>,
        std::collections::BTreeSet<u64>,
    )>,
> = std::sync::Mutex::new(None);

/// Record which pages a region of `[start, end)` fell in.
pub fn record_extent(start: u64, end: u64) {
    if !enabled() {
        return;
    }
    let mut guard = PAGES.lock().unwrap_or_else(|e| e.into_inner());
    let (small, large) = guard.get_or_insert_with(Default::default);
    for page in (start >> 12)..=(end.saturating_sub(1) >> 12) {
        small.insert(page);
    }
    for page in (start >> 14)..=(end.saturating_sub(1) >> 14) {
        large.insert(page);
    }
}

/// When the process started translating, so the report can say what share of
/// the run went into it.
fn origin() -> Instant {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *T0.get_or_init(Instant::now)
}

/// Start the clock the report is a share of.
pub fn start() {
    if enabled() {
        start_inner();
    }
}

/// The half of [`start`] that is not the environment check, so that [`set_enabled`] can
/// reach it when a run that was never asked to count is asked to, mid-flight.
fn start_inner() {
    origin();
    // A run that ends in the guest's own `exit` never returns to the driver, and
    // a report printed only on the way out would therefore be printed for almost
    // no run at all.
    install_exit_report();
}

/// Print the report when the process ends, however it ends.
fn install_exit_report() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Relaxed) {
        return;
    }
    // `atexit` rather than a `Drop` guard: the guest calls `exit`, which runs
    // the host's `atexit` handlers and never unwinds this crate's frames.
    extern "C" fn report_at_exit() {
        report();
    }
    // SAFETY: `atexit` takes an `extern "C" fn()`, which this is, and the
    // handler touches only process-global atomics and `stderr`.
    unsafe { libc::atexit(report_at_exit) };
}

/// Print what translation cost, if it was asked for.
pub fn report() {
    if !enabled() || REGIONS.load(Relaxed) == 0 {
        return;
    }
    let wall = origin().elapsed();
    let ns = |c: &AtomicU64| Duration::from_nanos(c.load(Relaxed));
    let (select, lift, compile) = (ns(&SELECT_NS), ns(&LIFT_NS), ns(&COMPILE_NS));
    let total = select + lift + compile;
    let regions = REGIONS.load(Relaxed);
    let instructions = INSTRUCTIONS.load(Relaxed);
    let share = |d: Duration| 100.0 * d.as_secs_f64() / wall.as_secs_f64().max(f64::EPSILON);
    eprintln!(
        "rosemary: stats  {regions} region(s), {instructions} instruction(s), \
         {} byte(s), {:.1} instr/region",
        BYTES.load(Relaxed),
        instructions as f64 / regions as f64,
    );
    let mib = |c: &AtomicU64| c.load(Relaxed) as f64 / (1024.0 * 1024.0);
    eprintln!(
        "rosemary: stats  code {:.1} MiB compiled, {} region(s) and {:.1} MiB from the code cache",
        mib(&CODE_BYTES),
        CACHED_REGIONS.load(Relaxed),
        mib(&CACHED_CODE_BYTES),
    );
    // What the *shared* cache saw, which is the other half of the linking story: the
    // counters above say how often the run loop was entered, and these say how often
    // it had to look an address up to do it. `miss_rate` is over the lookups that
    // reached the cache at all - a hit in this thread's window or in the lock-free
    // index never becomes one, which is the point.
    if let Some(vm) = crate::thunk::current() {
        let cache = vm.cache_stats();
        let (installed, pending) = vm.cell_counts();
        eprintln!(
            "rosemary: stats  cache {} lookup(s), {} hit(s){}  \
             cells {installed} live, {pending} waiting ({} filled, {} retracted)  \
             links {} followed",
            cache.lookups,
            cache.hits,
            cache
                .miss_rate()
                .map(|rate| format!(" ({:.1}% missed)", 100.0 * rate))
                .unwrap_or_default(),
            cache.cells_installed,
            cache.cells_retracted,
            cache.links_followed,
        );
    }
    eprintln!(
        "rosemary: stats  select {select:.2?} ({:.1}%)  lift {lift:.2?} ({:.1}%)  \
         compile {compile:.2?} ({:.1}%)  total {total:.2?} ({:.1}%) of {wall:.2?}",
        share(select),
        share(lift),
        share(compile),
        share(total),
    );
    let dispatches = DISPATCHES.load(Relaxed);
    if dispatches > 0 {
        let hits = DISPATCH_HITS.load(Relaxed);
        eprintln!(
            "rosemary: stats  {dispatches} indirect call(s) dispatched by the host, \
             {hits} answered from this thread's table ({:.2}%), \
             {} fill(s) evicted another address ({:.2}% of misses)",
            100.0 * hits as f64 / dispatches as f64,
            DISPATCH_EVICTIONS.load(Relaxed),
            100.0 * DISPATCH_EVICTIONS.load(Relaxed) as f64 / (dispatches - hits).max(1) as f64,
        );
    }
    let relinks = RELINKS.load(Relaxed);
    if relinks > 0 {
        eprintln!(
            "rosemary: stats  {relinks} link row(s) restored after eviction ({:.2}% of entries)",
            100.0 * relinks as f64 / ENTRIES.load(Relaxed).max(1) as f64,
        );
    }
    let publishes = PUBLISHES.load(Relaxed);
    if publishes > 0 {
        let rows = PUBLISHED_ROWS.load(Relaxed);
        eprintln!(
            "rosemary: stats  {publishes} publish(es) of {rows} link row(s), \
             {:.1} per publish into a {}-row {}-way table ({:?})",
            rows as f64 / publishes as f64,
            crate::thread::link_config().entries,
            crate::thread::link_config().ways,
            crate::jit::link_publish(),
        );
    }
    eprintln!(
        "rosemary: stats  {} refused, {} tail exit(s), {} address(es) retired by writes to code, \
         {} stale lift(s)",
        REFUSED.load(Relaxed),
        TAIL_EXITS.load(Relaxed),
        RETIRED.load(Relaxed),
        STALE_LIFTS.load(Relaxed),
    );
    // The exit mix, which is the other half of "why is this slow" and the half
    // translation time cannot answer. Printed as a rate as well as a count.
    let entries = ENTRIES.load(Relaxed);
    if entries > 0 {
        let per_second = entries as f64 / wall.as_secs_f64().max(f64::EPSILON);
        eprintln!(
            "rosemary: stats  {entries} region entr(ies), {:.2}M/s, {} window miss(es) ({:.2}%), \
             {} tail call(s) to an import ({:.2}%)",
            per_second / 1e6,
            WINDOW_MISSES.load(Relaxed),
            100.0 * WINDOW_MISSES.load(Relaxed) as f64 / entries as f64,
            TAIL_IMPORTS.load(Relaxed),
            100.0 * TAIL_IMPORTS.load(Relaxed) as f64 / entries as f64,
        );
        // How many of those entries the run loop never saw. This is the figure
        // block linking is judged on.
        match crate::jit::pinned_link_abi() {
            Some(abi) => eprintln!(
                "rosemary: stats  entries above are chains, not regions: links are branches \
                 under a pinned ABI of {} register(s) and are not counted",
                abi.len(),
            ),
            None => {
                let linked = LINKED.load(Relaxed);
                eprintln!(
                    "rosemary: stats  {linked} linked ({:.2}%), {} through the run loop",
                    100.0 * linked as f64 / entries as f64,
                    entries - linked,
                );
            }
        }
        let mut mix: Vec<(u64, &str)> = Exit::ALL
            .iter()
            .map(|&e| (EXITS[e as usize].load(Relaxed), e.label()))
            .filter(|&(n, _)| n > 0)
            .collect();
        mix.sort_by_key(|&(n, _)| std::cmp::Reverse(n));
        eprintln!(
            "rosemary: stats  exits by kind  {}",
            mix.iter()
                .map(|(n, l)| format!("{l} {:.1}%", 100.0 * *n as f64 / entries as f64))
                .collect::<Vec<_>>()
                .join("  "),
        );
        // Per kind, because a whole-program share hides the failure that matters: a
        // change that stops `ret` linking while leaving direct calls alone still
        // reads well in aggregate. The *denominator* is what a pinned ABI cannot
        // supply - `EXITS` then counts only what came back, so a share would read
        // 100% and mean nothing. Counts there, shares where they are real.
        let mut declined: Vec<(u64, u64, &str)> = Exit::ALL
            .iter()
            .filter(|e| e.linkable())
            .map(|&e| {
                (
                    PROBE_MISSES[e as usize].load(Relaxed),
                    EXITS[e as usize].load(Relaxed),
                    e.label(),
                )
            })
            .filter(|&(missed, total, _)| missed > 0 || total > 0)
            .collect();
        declined.sort_by_key(|&(_, total, _)| std::cmp::Reverse(total));
        if !declined.is_empty() {
            let shares = crate::jit::pinned_link_abi().is_none();
            eprintln!(
                "rosemary: stats  probes declined  {}",
                declined
                    .iter()
                    .map(|(missed, total, l)| match shares {
                        true => format!(
                            "{l} {:.2}% of {total}",
                            100.0 * *missed as f64 / (*total).max(1) as f64
                        ),
                        false => format!("{l} {missed}"),
                    })
                    .collect::<Vec<_>>()
                    .join("  "),
            );
        }
    }
    // Whether the import stubs did anything, which is three questions and not
    // one. See [`crate::trampoline::counts`].
    if let Some((made, substituted, reached)) = crate::trampoline::counts() {
        eprintln!(
            "rosemary: stats  {made} trampoline(s), {substituted} slot(s) substituted, \
             {reached} reached"
        );
    }
    if let Some((small, large)) = PAGES.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        eprintln!(
            "rosemary: stats  footprint {} page(s) of 4 KiB, {} of 16 KiB \
             ({} / {} byte(s) if lifted whole)",
            small.len(),
            large.len(),
            small.len() * 4096,
            large.len() * 16384,
        );
    }
}

/// Every counter above, read at one moment, for a consumer that is not stderr.
#[derive(Clone, Default)]
pub struct Snapshot {
    /// Whether `ROSEMARY_STATS` was set.
    pub counted: bool,
    /// Time since the driver started translating; the denominator for a rate.
    pub wall: Duration,
    /// Translations, and what came out of them.
    pub regions: u64,
    pub instructions: u64,
    pub bytes: u64,
    pub select: Duration,
    pub lift: Duration,
    pub compile: Duration,
    pub refused: u64,
    pub tail_exits: u64,
    /// Region entries and their shape. Zero unless [`enabled`].
    pub entries: u64,
    pub window_misses: u64,
    pub tail_imports: u64,
    pub linked: u64,
    /// The width of the pinned block-link ABI, when there is one.
    pub pinned_link_registers: Option<usize>,
    /// Exit kinds with a non-zero count, most frequent first.
    pub exits: Vec<(&'static str, u64)>,
    /// `(made, substituted, reached)` from [`crate::trampoline::counts`].
    pub trampolines: Option<(u64, u64, u64)>,
    /// Distinct pages of lifted code, at 4 KiB and at 16 KiB.
    pub pages: Option<(usize, usize)>,
}

/// Read every counter, once.
pub fn snapshot() -> Snapshot {
    let ns = |c: &AtomicU64| Duration::from_nanos(c.load(Relaxed));
    let mut exits: Vec<(&'static str, u64)> = Exit::ALL
        .iter()
        .map(|&e| (e.label(), EXITS[e as usize].load(Relaxed)))
        .filter(|&(_, n)| n > 0)
        .collect();
    exits.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    Snapshot {
        counted: enabled(),
        wall: origin().elapsed(),
        regions: REGIONS.load(Relaxed),
        instructions: INSTRUCTIONS.load(Relaxed),
        bytes: BYTES.load(Relaxed),
        select: ns(&SELECT_NS),
        lift: ns(&LIFT_NS),
        compile: ns(&COMPILE_NS),
        refused: REFUSED.load(Relaxed),
        tail_exits: TAIL_EXITS.load(Relaxed),
        entries: ENTRIES.load(Relaxed),
        window_misses: WINDOW_MISSES.load(Relaxed),
        tail_imports: TAIL_IMPORTS.load(Relaxed),
        linked: LINKED.load(Relaxed),
        pinned_link_registers: crate::jit::pinned_link_abi().map(|abi| abi.len()),
        exits,
        trampolines: crate::trampoline::counts(),
        // Cloned rather than borrowed.
        pages: PAGES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(small, large)| (small.len(), large.len())),
    }
}
