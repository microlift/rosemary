//! `rosemary aot`: compile a loaded program's images into the cache without
//! running it.
//!
//! Regions are chosen by the same [`crate::region::region_for`] the JIT uses, so a
//! cached region is one the JIT would have made, and lifted under the program's own
//! lifter config. Every image is planned first; then threads take regions one at a
//! time from all of them, hot ones first, so a region that is slow to compile holds
//! up only the thread compiling it.
//!
//! Each image's hot regions, what earlier runs executed and what start-up needs, go
//! to part 0 of its cache, compiled at the run's level; the file is committed as soon
//! as the last of them is written, so a run can use it while the rest compiles. Cold
//! regions follow into further parts, compiled provisionally when the JIT tiers:
//! most of them never run, and a run upgrades those that do as it upgrades its own.
//! A build stopped by its deadline commits every part it started.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use machish::memory::MmapSpace;
use machish::report::ImageReport;
use microlift::arch::x86_64::X86_64;
use microlift::core::{LiftedCode, LifterConfig, lift_to_code};
use microlift::image::RegionObject;

use super::entries::{self, Entries};
use super::file::Embedded;
use super::store::{AtomicFile, BuildLock};
use super::{ImageId, Layout};
use crate::cache::Tier;
use crate::vm::Vm;

/// Code past which a part is committed and the next one started. Below what one
/// file may hold by more than any region is long.
const ROLLOVER: u64 = microlift::image::file::MAX_TEXT - (256 << 20);

/// Regions of at least this many CLIF instructions are large: they compile only
/// within [`LARGE_BUDGET`].
const LARGE_REGION: usize = 16 * 1024;

/// CLIF instructions of large regions compiled at once. Cranelift's peak memory
/// grows faster than a function's size: about 1 KiB per instruction is typical,
/// but a few regions of unrolled crypto code lift to over 130,000 instructions
/// and take 1 to 2 GiB each. This lets one of those compile beside smaller large
/// regions, never beside another.
const LARGE_BUDGET: usize = 256 * 1024;

pub struct Options {
    /// When to stop starting work. `None` compiles everything.
    pub deadline: Option<Instant>,
    pub threads: usize,
    /// Skip images that already have a cache file.
    pub only_missing: bool,
    /// Compile only hot regions: what earlier runs executed and what start-up needs.
    pub hot_only: bool,
    /// Build images with no cache file, and the hot part of those whose profile has
    /// grown since theirs was built; leave the rest alone.
    pub refresh: bool,
}

/// What became of one image.
#[derive(Debug)]
pub enum Outcome {
    Written {
        regions: usize,
        refused: usize,
        planned: usize,
        parts: u32,
        complete: bool,
    },
    /// Nothing to do, or nothing that may be done, with a reason.
    Skipped(&'static str),
    Failed(String),
}

/// Compiles every image of `vm`'s program into `layout`, then every image earlier
/// runs `dlopen`ed and what each brought in, reporting each by path.
///
/// `vm` must be published (`thunk::publish`): linking a `dlopen`ed image asks it
/// what is guest code, as a run does.
pub fn build(vm: &Vm, layout: &Layout, options: &Options) -> Vec<(String, Outcome)> {
    let mut seen = HashSet::new();
    let mut prepared = Vec::new();
    for report in &vm.program.report.images {
        seen.insert(report.path.clone());
        let job = prepare(vm, layout, report, &vm.program.memory, options);
        prepared.push((report.path.clone(), job));
    }
    for path in dlopened(layout) {
        // In the order runs opened them, so each binds as it did there.
        let image = match crate::dyld::link_without_running(&path) {
            Ok(image) => image,
            Err(e) => {
                prepared.push((path, Err(Outcome::Failed(format!("dlopen: {e}")))));
                continue;
            }
        };
        for report in &image.report.images {
            if !seen.insert(report.path.clone()) {
                continue;
            }
            let job = image
                .with_memory(|memory| prepare(vm, layout, report, memory, options))
                .unwrap_or(Err(Outcome::Skipped("unloaded")));
            prepared.push((report.path.clone(), job));
        }
    }
    let jobs: Vec<&Job> = prepared
        .iter()
        .filter_map(|(_, job)| job.as_ref().ok())
        .collect();
    compile(vm, &jobs, options);
    prepared
        .into_iter()
        .map(|(path, job)| (path, job.map_or_else(|skipped| skipped, Job::finish)))
        .collect()
}

/// The images runs of this executable `dlopen`ed, in first-opened order.
fn dlopened(layout: &Layout) -> Vec<String> {
    crate::dyld::executable_host_path()
        .and_then(|executable| super::profile::image_id(&executable))
        .and_then(|id| super::profile::read_dlopens(&layout.dlopens(&id)).ok())
        .unwrap_or_default()
}

/// One image being compiled: its plan, and its parts as far as they are written.
struct Job<'a> {
    report: &'a ImageReport,
    layout: &'a Layout,
    id: ImageId,
    plan: Vec<Planned>,
    /// How many of `plan` are hot.
    hot: usize,
    /// Only the hot part is being built: the cold parts already there stay.
    hot_only: bool,
    /// Addresses the profile held when the plan was made.
    profiled: u64,
    /// Part 0, then the cold parts.
    streams: [Mutex<Stream>; 2],
    /// The number the next part opened takes, past the one reserved for part 0:
    /// shared, so a stream that rolls over never takes another's number.
    next_part: AtomicU32,
    hot_left: AtomicUsize,
    regions: AtomicUsize,
    /// Regions that would not lift or compile, or that consulted an import only
    /// this run's registry answered.
    refused: AtomicUsize,
    _lock: BuildLock,
}

/// One sequence of parts being written.
struct Stream {
    tier: Tier,
    /// The number reserved for the first part this stream opens; later ones take
    /// [`Job::next_part`].
    first: Option<u32>,
    open: Option<OpenPart>,
    /// Parts committed.
    written: u32,
    error: Option<String>,
}

struct OpenPart {
    file: AtomicFile,
    writer: microlift::image::file::Writer,
    number: u32,
}

impl Stream {
    fn new(tier: Tier, first: Option<u32>) -> Self {
        Stream {
            tier,
            first,
            open: None,
            written: 0,
            error: None,
        }
    }
}

impl Job<'_> {
    fn stream(&self, hot: bool) -> std::sync::MutexGuard<'_, Stream> {
        self.streams[usize::from(!hot)]
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Appends `object` to its stream, committing a part once it is full.
    fn write(&self, hot: bool, object: &RegionObject, fingerprint: u64) -> bool {
        let mut stream = self.stream(hot);
        if stream.error.is_some() {
            return false;
        }
        if stream.open.is_none() {
            let number = stream
                .first
                .take()
                .unwrap_or_else(|| self.next_part.fetch_add(1, Ordering::Relaxed));
            match self.open_part(stream.tier, number, fingerprint) {
                Ok(part) => stream.open = Some(part),
                Err(e) => {
                    stream.error = Some(e);
                    return false;
                }
            }
        }
        let part = stream.open.as_mut().expect("opened above");
        if let Err(e) = part.writer.add(object) {
            // A region the file cannot hold is refused; the file stays good.
            if part.writer.text_len() < ROLLOVER {
                return false;
            }
            stream.error = Some(e.to_string());
            return false;
        }
        if part.writer.text_len() >= ROLLOVER {
            self.commit(&mut stream);
        }
        true
    }

    fn open_part(&self, tier: Tier, number: u32, fingerprint: u64) -> Result<OpenPart, String> {
        let path = self.layout.part(&self.id, number);
        let directory = path.parent().expect("cache files have a directory");
        let isa = crate::jit::region_isa(tier).map_err(|e| e.to_string())?;
        let file = AtomicFile::create(&path).map_err(|e| e.to_string())?;
        let writer = microlift::image::file::Writer::new(
            file.handle().map_err(|e| e.to_string())?,
            super::store::scratch(directory).map_err(|e| e.to_string())?,
            microlift::backend::isa_fingerprint(isa),
            fingerprint,
        )
        .map_err(|e| e.to_string())?;
        Ok(OpenPart {
            file,
            writer,
            number,
        })
    }

    /// Writes out and renames into place the part `stream` has open.
    fn commit(&self, stream: &mut Stream) {
        let Some(part) = stream.open.take() else {
            return;
        };
        let number = part.number;
        let embedded = Embedded {
            lift_slide: self.report.slide,
            profiled: self.profiled,
            tier: stream.tier,
            source: self.report.path.clone(),
        };
        let committed = part
            .writer
            .finish(&embedded.encode())
            .map_err(|e| e.to_string())
            .and_then(|written| {
                drop(written);
                part.file.commit().map_err(|e| e.to_string())
            });
        match committed {
            Ok(()) => {
                stream.written += 1;
                warm(&self.layout.part(&self.id, number));
            }
            Err(e) => stream.error = Some(e),
        }
    }

    /// The last hot region is written: commit part 0, so runs can use it now.
    fn hot_done(&self) {
        if self.hot_left.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.commit(&mut self.stream(true));
        }
    }

    fn finish(self) -> Outcome {
        let regions = self.regions.load(Ordering::Relaxed);
        let refused = self.refused.load(Ordering::Relaxed);
        let mut parts = 0;
        let mut error = None;
        for (index, stream) in self.streams.iter().enumerate() {
            let mut stream = stream.lock().unwrap_or_else(|e| e.into_inner());
            self.commit(&mut stream);
            parts += stream.written;
            if let Some(e) = stream.error.take() {
                error = Some(e);
            }
            // A complete build replaces every cold part an earlier one left.
            if index == 1
                && !self.hot_only
                && regions + refused == self.plan.len()
                && error.is_none()
            {
                for n in self.next_part.load(Ordering::Relaxed).. {
                    let stale = self.layout.part(&self.id, n);
                    if std::fs::remove_file(&stale).is_err() {
                        break;
                    }
                }
            }
        }
        match error {
            Some(e) => Outcome::Failed(e),
            None => Outcome::Written {
                regions,
                refused,
                planned: self.plan.len(),
                parts,
                complete: regions + refused == self.plan.len(),
            },
        }
    }
}

/// Maps a committed part's code once, here, so a run need not: the first time a
/// file's pages are made executable Darwin validates all of it, slowly, and keeps the
/// result for every process that maps the file after.
fn warm(path: &std::path::Path) {
    if let Ok(file) = std::fs::File::open(path)
        && let Ok(image) = microlift::runtime::image::MappedImage::open(&file)
    {
        let _ = image.map_code();
    }
}

/// Plans `report`'s regions and takes its build lock, or says why it will not be built.
fn prepare<'a>(
    vm: &Vm,
    layout: &'a Layout,
    report: &'a ImageReport,
    memory: &MmapSpace,
    options: &Options,
) -> Result<Job<'a>, Outcome> {
    if !report.segments.iter().any(|segment| segment.executable) {
        return Err(Outcome::Skipped("no code"));
    }
    let Ok(bytes) = std::fs::read(&report.path) else {
        return Err(Outcome::Skipped("its file cannot be read"));
    };
    let id = ImageId::of(&bytes);
    let path = layout.image(&id);
    if options.only_missing && path.exists() {
        return Err(Outcome::Skipped("already cached"));
    }
    let mut hot_only = options.hot_only;
    if options.refresh
        && let Some(built) = super::embedded(layout, &id)
    {
        if !super::profile::grown(
            built.profiled,
            super::profile::distinct(&layout.profile(&id)),
        ) {
            return Err(Outcome::Skipped("up to date"));
        }
        hot_only = true;
    }
    if !super::matches_file(memory, report, &bytes) {
        return Err(Outcome::Skipped(
            "the code loaded is not the code in its file",
        ));
    }
    let lock = match super::store::try_lock(&path) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Err(Outcome::Skipped("another process is building it")),
        Err(e) => return Err(Outcome::Failed(e.to_string())),
    };
    let image_layout =
        entries::Layout::parse(&bytes, report.slide).map_err(|e| Outcome::Failed(e.to_string()))?;
    let profiled = super::profile::compact(&layout.profile(&id)).unwrap_or_default();
    let mut entries = entries::of(report, memory, &image_layout, &profiled);
    if hot_only {
        entries.order.truncate(entries.hot);
    }
    let (plan, hot) = plan(vm, &entries);
    layout.prune();
    let cold_tier = match crate::jit::tiered() {
        true => Tier::Provisional,
        false => Tier::Final,
    };
    Ok(Job {
        report,
        layout,
        id,
        hot,
        hot_only,
        profiled: profiled.len() as u64,
        hot_left: AtomicUsize::new(hot),
        plan,
        streams: [
            Mutex::new(Stream::new(Tier::Final, Some(0))),
            Mutex::new(Stream::new(cold_tier, None)),
        ],
        next_part: AtomicU32::new(1),
        regions: AtomicUsize::new(0),
        refused: AtomicUsize::new(0),
        _lock: lock,
    })
}

/// One region to compile, as the JIT would select it.
struct Planned {
    region: crate::region::Region,
    bytes: Vec<u8>,
    /// Its resume points: the entry, and every declared address it holds.
    resume_points: Vec<u64>,
}

/// Selects a region for each entry in order, skipping an entry an earlier region
/// already declared as a resume point: the run-time cache finds it there. Returns
/// the plan and how many of its regions are hot.
fn plan(vm: &Vm, entries: &Entries) -> (Vec<Planned>, usize) {
    let mut covered = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut hot = 0;
    for (n, &pc) in entries.order.iter().enumerate() {
        if covered.contains(&pc) {
            continue;
        }
        let Some((region, bytes)) = vm.program.lift_region(pc) else {
            continue;
        };
        let range = &region.range;
        let lo = entries.declared.partition_point(|&a| a < range.start);
        let hi = entries.declared.partition_point(|&a| a < range.end);
        let mut resume_points: Vec<u64> = entries.declared[lo..hi]
            .iter()
            .copied()
            .filter(|&a| region.covers(a))
            .collect();
        resume_points.push(pc);
        resume_points.sort_unstable();
        resume_points.dedup();
        covered.extend(resume_points.iter().copied());
        out.push(Planned {
            region,
            bytes,
            resume_points,
        });
        if n < entries.hot {
            hot = out.len();
        }
    }
    (out, hot)
}

/// Compiles every planned region of `jobs` on `options.threads` threads: every
/// image's hot regions, then every image's cold ones, one at a time in the order
/// planned.
fn compile(vm: &Vm, jobs: &[&Job<'_>], options: &Options) {
    for job in jobs {
        // An image with nothing hot commits its first part now, empty, so a run
        // knows it is cached.
        if job.hot == 0 {
            job.hot_left.store(1, Ordering::Relaxed);
            job.hot_done();
        }
    }
    let hot = jobs
        .iter()
        .enumerate()
        .flat_map(|(job, planned)| (0..planned.hot).map(move |region| (job, region)));
    let cold = jobs.iter().enumerate().flat_map(|(job, planned)| {
        (planned.hot..planned.plan.len()).map(move |region| (job, region))
    });
    let queue = Queue {
        work: hot.chain(cold).collect(),
        next: AtomicUsize::new(0),
        deferred: Mutex::new(VecDeque::new()),
        budget: Budget::default(),
        deadline: options.deadline,
    };
    let fingerprint = vm.lifter_config().codegen_fingerprint();
    let threads = options.threads.clamp(1, queue.work.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let worker = Worker {
                    jobs,
                    // Each thread lifts under its own copy, which shares the
                    // program's segments and imports through `Arc`s.
                    config: vm.lifter_config().clone(),
                };
                while let Some(next) = queue.take(&worker) {
                    let job = jobs[next.job];
                    let hot = next.region < job.hot;
                    let written = next.code.is_some_and(|code| {
                        let tier = job.stream(hot).tier;
                        worker
                            .compile(tier, code)
                            .is_some_and(|object| job.write(hot, &object, fingerprint))
                    });
                    match written {
                        true => job.regions.fetch_add(1, Ordering::Relaxed),
                        false => job.refused.fetch_add(1, Ordering::Relaxed),
                    };
                    if hot {
                        job.hot_done();
                    }
                    queue.budget.give(next.cost);
                }
            });
        }
    });
}

/// The regions left to compile.
struct Queue {
    /// Every planned region, as `(job, region)`, in the order compiled.
    work: Vec<(usize, usize)>,
    next: AtomicUsize,
    /// Large regions the budget had no room for when they were lifted, with their
    /// cost. Their code is dropped and lifted again when there is room, so a
    /// waiting region holds no memory.
    deferred: Mutex<VecDeque<((usize, usize), usize)>>,
    budget: Budget,
    deadline: Option<Instant>,
}

/// A region to compile, holding `cost` of the budget.
struct Lifted {
    job: usize,
    region: usize,
    /// `None` when it would not lift or may not be cached.
    code: Option<LiftedCode>,
    cost: usize,
}

impl Queue {
    /// The next region to compile, its share of the budget taken.
    ///
    /// A large region the budget has no room for is deferred while this thread
    /// goes on to the next region; deferred regions go first once they fit, and
    /// once nothing else is left this thread waits for room.
    fn take(&self, worker: &Worker<'_, '_>) -> Option<Lifted> {
        loop {
            if past(self.deadline) {
                return None;
            }
            if let Some(((job, region), cost)) = self.deferred_that_fits() {
                let code = worker.lift(job, region);
                return Some(Lifted {
                    job,
                    region,
                    code,
                    cost,
                });
            }
            let Some(&(job, region)) = self.work.get(self.next.fetch_add(1, Ordering::Relaxed))
            else {
                let ((job, region), cost) = self.wait_for_deferred()?;
                let code = worker.lift(job, region);
                return Some(Lifted {
                    job,
                    region,
                    code,
                    cost,
                });
            };
            let code = worker.lift(job, region);
            let cost = code.as_ref().map_or(0, cost);
            if self.budget.try_take(cost) {
                return Some(Lifted {
                    job,
                    region,
                    code,
                    cost,
                });
            }
            self.deferred
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(((job, region), cost));
        }
    }

    /// The first deferred region that fits, its share of the budget taken.
    fn deferred_that_fits(&self) -> Option<((usize, usize), usize)> {
        let mut deferred = self.deferred.lock().unwrap_or_else(|e| e.into_inner());
        let at = deferred
            .iter()
            .position(|&(_, cost)| self.budget.try_take(cost))?;
        deferred.remove(at)
    }

    fn wait_for_deferred(&self) -> Option<((usize, usize), usize)> {
        let waiting = self
            .deferred
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()?;
        self.budget.take(waiting.1);
        Some(waiting)
    }
}

/// What a region costs against [`LARGE_BUDGET`].
fn cost(code: &LiftedCode) -> usize {
    let instructions = code.func.dfg.num_insts();
    match instructions >= LARGE_REGION {
        true => instructions,
        false => 0,
    }
}

/// The CLIF instructions of large regions being compiled. A region always fits
/// when nothing else holds any, so one larger than the whole budget still runs.
#[derive(Default)]
struct Budget {
    used: Mutex<usize>,
    freed: Condvar,
}

impl Budget {
    fn fits(used: usize, cost: usize) -> bool {
        cost == 0 || used == 0 || used + cost <= LARGE_BUDGET
    }

    fn try_take(&self, cost: usize) -> bool {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        let fits = Self::fits(*used, cost);
        if fits {
            *used += cost;
        }
        fits
    }

    fn take(&self, cost: usize) {
        let mut used = self.used.lock().unwrap_or_else(|e| e.into_inner());
        while !Self::fits(*used, cost) {
            used = self.freed.wait(used).unwrap_or_else(|e| e.into_inner());
        }
        *used += cost;
    }

    fn give(&self, cost: usize) {
        if cost == 0 {
            return;
        }
        *self.used.lock().unwrap_or_else(|e| e.into_inner()) -= cost;
        self.freed.notify_all();
    }
}

/// One thread's view of the jobs, with its own copy of the lifter config.
struct Worker<'j, 'a> {
    jobs: &'j [&'j Job<'a>],
    config: LifterConfig,
}

impl Worker<'_, '_> {
    fn lift(&self, job: usize, region: usize) -> Option<LiftedCode> {
        let job = self.jobs[job];
        lift(&self.config, job.report, &job.plan[region])
    }

    /// Compiles `code` with the JIT's own compiler for `tier`, so the code cached is
    /// the code the JIT would run.
    fn compile(&self, tier: Tier, code: LiftedCode) -> Option<RegionObject> {
        crate::jit::region_compiler(tier)
            .ok()?
            .compiler()
            .compile(code)
            .ok()
    }
}

/// `planned`, a region of `report`, lifted as [`crate::cache`] lifts it, or `None`
/// if it may not be cached.
fn lift(config: &LifterConfig, report: &ImageReport, planned: &Planned) -> Option<LiftedCode> {
    let mut region_config = config.clone();
    region_config.resume_points = planned.resume_points.clone();
    region_config.block_ranges = planned.region.blocks.clone();
    let lifted =
        lift_to_code::<X86_64>(&planned.bytes, planned.region.range.start, region_config).ok()?;
    let in_image = |va: u64| {
        report
            .segments
            .iter()
            .any(|segment| va.wrapping_sub(segment.vmaddr) < segment.vmsize)
    };
    super::resolve::cacheable(config, &lifted.import_decisions, in_image).then_some(lifted)
}

fn past(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_regions_never_wait_for_the_budget() {
        let budget = Budget::default();
        assert!(budget.try_take(LARGE_BUDGET));
        assert!(budget.try_take(0));
    }

    #[test]
    fn large_regions_share_the_budget() {
        let budget = Budget::default();
        assert!(budget.try_take(LARGE_BUDGET / 2));
        assert!(!budget.try_take(LARGE_BUDGET / 2 + 1));
        assert!(budget.try_take(LARGE_BUDGET / 2));
        budget.give(LARGE_BUDGET / 2);
        budget.give(LARGE_BUDGET / 2);
        assert_eq!(*budget.used.lock().unwrap(), 0);
    }

    #[test]
    fn a_region_larger_than_the_budget_runs_alone() {
        let budget = Budget::default();
        assert!(budget.try_take(2 * LARGE_BUDGET));
        assert!(!budget.try_take(LARGE_REGION));
        budget.give(2 * LARGE_BUDGET);
        assert!(budget.try_take(LARGE_REGION));
    }
}
