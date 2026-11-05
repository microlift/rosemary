//! `rosemary aot`: compile a loaded program's images into the cache without
//! running it.
//!
//! Regions are chosen by the same [`crate::region::region_for`] the JIT uses, so a
//! cached region is one the JIT would have made, and lifted under the program's own
//! lifter config. Every image is planned first; then threads take regions one at a
//! time from all of them, in priority order, so a region that is slow to compile
//! holds up only the thread compiling it. Each thread gathers what it takes into a
//! chunk, a Microlift image, which it writes to the image's file once full; a build
//! stopped by its deadline keeps every chunk written.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use machish::memory::MmapSpace;
use machish::report::ImageReport;
use microlift::arch::x86_64::X86_64;
use microlift::backend::ImageBuilder;
use microlift::core::{LiftedCode, LifterConfig, lift_to_code};

use super::entries::{self, Entries};
use super::file;
use super::store::{AtomicFile, BuildLock};
use super::{ImageId, Layout};
use crate::vm::Vm;

/// Regions per chunk: enough that each image's helpers are shared by many, few
/// enough that a chunk is written, and its memory freed, soon after it starts.
const CHUNK_REGIONS: usize = 64;

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
}

/// What became of one image.
#[derive(Debug)]
pub enum Outcome {
    Written {
        regions: usize,
        refused: usize,
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

/// One image being compiled: its plan, and its file as far as it is written.
struct Job<'a> {
    report: &'a ImageReport,
    plan: Vec<Planned>,
    out: Mutex<std::io::Result<file::Writer<AtomicFile>>>,
    regions: AtomicUsize,
    /// Regions that would not lift or compile, or that consulted an import only
    /// this run's registry answered.
    refused: AtomicUsize,
    _lock: BuildLock,
}

impl Job<'_> {
    fn write(&self, chunk: ImageBuilder) {
        let chunk = chunk.finish();
        if chunk.regions.is_empty() {
            return;
        }
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(writer) = &mut *out
            && let Err(e) = writer.push(&chunk)
        {
            *out = Err(e);
        }
    }

    fn finish(self) -> Outcome {
        let regions = self.regions.into_inner();
        let refused = self.refused.into_inner();
        let written = self
            .out
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
            .and_then(file::Writer::finish)
            .and_then(AtomicFile::commit);
        match written {
            Ok(()) => Outcome::Written {
                regions,
                refused,
                complete: regions + refused == self.plan.len(),
            },
            Err(e) => Outcome::Failed(e.to_string()),
        }
    }
}

/// Plans `report`'s regions and opens its cache file, or says why it will not be built.
fn prepare<'a>(
    vm: &Vm,
    layout: &Layout,
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
    let profiled = super::profile::read(&layout.profile(&id)).unwrap_or_default();
    let entries = entries::of(report, memory, &image_layout, &profiled);
    let plan = plan(vm, &entries);
    layout.prune();
    let out = AtomicFile::create(&path)
        .and_then(|file| file::Writer::new(file, report.slide, &report.path));
    Ok(Job {
        report,
        plan,
        out: Mutex::new(out),
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
/// already declared as a resume point: the run-time cache finds it there.
fn plan(vm: &Vm, entries: &Entries) -> Vec<Planned> {
    let mut covered = std::collections::HashSet::new();
    let mut out = Vec::new();
    for &pc in &entries.order {
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
    }
    out
}

/// Compiles every planned region of `jobs` on `options.threads` threads, taking
/// regions one at a time in the order planned.
fn compile(vm: &Vm, jobs: &[&Job<'_>], options: &Options) {
    let Ok(resolved) = crate::jit::resolved_isa_settings() else {
        for job in jobs {
            job.refused.store(job.plan.len(), Ordering::Relaxed);
        }
        return;
    };
    let isa_settings: Vec<(&str, &str)> = resolved
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let queue = Queue {
        work: jobs
            .iter()
            .enumerate()
            .flat_map(|(job, planned)| (0..planned.plan.len()).map(move |region| (job, region)))
            .collect(),
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
                let mut worker = Worker {
                    jobs,
                    isa_settings: &isa_settings,
                    fingerprint,
                    // Each thread lifts under its own copy, which shares the
                    // program's segments and imports through `Arc`s.
                    config: vm.lifter_config().clone(),
                    chunks: jobs.iter().map(|_| None).collect(),
                };
                while let Some(next) = queue.take(&worker) {
                    let job = jobs[next.job];
                    match next.code.and_then(|code| worker.add(next.job, code)) {
                        Some(()) => job.regions.fetch_add(1, Ordering::Relaxed),
                        None => job.refused.fetch_add(1, Ordering::Relaxed),
                    };
                    queue.budget.give(next.cost);
                }
                worker.flush_all();
            });
        }
    });
}

/// The regions left to compile.
struct Queue {
    /// Every planned region, as `(job, region)`, in the order planned.
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
                return Some(Lifted { job, code, cost });
            }
            let Some(&(job, region)) = self.work.get(self.next.fetch_add(1, Ordering::Relaxed))
            else {
                let ((job, region), cost) = self.wait_for_deferred()?;
                let code = worker.lift(job, region);
                return Some(Lifted { job, code, cost });
            };
            let code = worker.lift(job, region);
            let cost = code.as_ref().map_or(0, cost);
            if self.budget.try_take(cost) {
                return Some(Lifted { job, code, cost });
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

/// One thread's compiler, and the chunk it is filling for each job.
struct Worker<'j, 'a> {
    jobs: &'j [&'j Job<'a>],
    isa_settings: &'j [(&'j str, &'j str)],
    fingerprint: u64,
    config: LifterConfig,
    chunks: Vec<Option<ImageBuilder>>,
}

impl Worker<'_, '_> {
    fn lift(&self, job: usize, region: usize) -> Option<LiftedCode> {
        let job = self.jobs[job];
        lift(&self.config, job.report, &job.plan[region])
    }

    /// Compiles `code` into the chunk for `job`, which is written out once full.
    fn add(&mut self, job: usize, code: LiftedCode) -> Option<()> {
        let chunk = match &mut self.chunks[job] {
            Some(chunk) => chunk,
            empty => empty.insert(
                microlift::backend::ArtifactBackend::image(
                    None,
                    Some(crate::jit::flags()),
                    self.isa_settings,
                    self.fingerprint,
                )
                .ok()?,
            ),
        };
        let added = chunk.add_region(code).ok();
        if chunk.region_count() >= CHUNK_REGIONS {
            self.flush(job);
        }
        added
    }

    fn flush(&mut self, job: usize) {
        if let Some(chunk) = self.chunks[job].take() {
            self.jobs[job].write(chunk);
        }
    }

    fn flush_all(&mut self) {
        (0..self.chunks.len()).for_each(|job| self.flush(job));
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

    #[test]
    fn an_offline_image_is_compiled_for_the_jit_isa() {
        let resolved = crate::jit::resolved_isa_settings().unwrap();
        let settings: Vec<(&str, &str)> = resolved.iter().map(|(n, v)| (*n, v.as_str())).collect();
        let builder = microlift::backend::ArtifactBackend::image(
            None,
            Some(crate::jit::flags()),
            &settings,
            0,
        )
        .unwrap();
        let jit = crate::jit::backend().unwrap();
        assert_eq!(
            builder.finish().isa_fingerprint,
            microlift::backend::isa_fingerprint(jit.isa())
        );
    }
}
