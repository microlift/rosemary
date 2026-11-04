//! `rosemary aot`: compile a loaded program's images into the cache without
//! running it.
//!
//! Regions are chosen by the same [`crate::region::region_for`] the JIT uses, so a
//! cached region is one the JIT would have made, and lifted under the program's own
//! lifter config. Chunks of them compile on separate threads into separate
//! Microlift images; the file lists the chunks in priority order, and a build
//! stopped by its deadline writes every chunk that finished.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use machish::memory::MmapSpace;
use machish::report::ImageReport;
use microlift::arch::x86_64::X86_64;
use microlift::core::{LifterConfig, lift_to_code};

use super::entries::{self, Entries};
use super::file::CacheFile;
use super::{ImageId, Layout};
use crate::vm::Vm;

/// Regions per chunk: enough that each image's helpers are shared by many, few
/// enough that the first chunks are written early in a time-boxed build.
const CHUNK_REGIONS: usize = 64;

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
    let mut built = HashSet::new();
    let mut out = Vec::new();
    for report in &vm.program.report.images {
        built.insert(report.path.clone());
        let outcome = build_image(vm, layout, report, &vm.program.memory, options);
        out.push((report.path.clone(), outcome));
    }
    for path in dlopened(layout) {
        // In the order runs opened them, so each binds as it did there.
        let image = match crate::dyld::link_without_running(&path) {
            Ok(image) => image,
            Err(e) => {
                out.push((path, Outcome::Failed(format!("dlopen: {e}"))));
                continue;
            }
        };
        for report in &image.report.images {
            if !built.insert(report.path.clone()) {
                continue;
            }
            let outcome = image
                .with_memory(|memory| build_image(vm, layout, report, memory, options))
                .unwrap_or(Outcome::Skipped("unloaded"));
            out.push((report.path.clone(), outcome));
        }
    }
    out
}

/// The images runs of this executable `dlopen`ed, in first-opened order.
fn dlopened(layout: &Layout) -> Vec<String> {
    crate::dyld::executable_host_path()
        .and_then(|executable| super::profile::image_id(&executable))
        .and_then(|id| super::profile::read_dlopens(&layout.dlopens(&id)).ok())
        .unwrap_or_default()
}

fn build_image(
    vm: &Vm,
    layout: &Layout,
    report: &ImageReport,
    memory: &MmapSpace,
    options: &Options,
) -> Outcome {
    if !report.segments.iter().any(|segment| segment.executable) {
        return Outcome::Skipped("no code");
    }
    let Ok(bytes) = std::fs::read(&report.path) else {
        return Outcome::Skipped("its file cannot be read");
    };
    let id = ImageId::of(&bytes);
    let path = layout.image(&id);
    if options.only_missing && path.exists() {
        return Outcome::Skipped("already cached");
    }
    if !super::matches_file(memory, report, &bytes) {
        return Outcome::Skipped("the code loaded is not the code in its file");
    }
    let _lock = match super::store::try_lock(&path) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Outcome::Skipped("another process is building it"),
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let image_layout = match entries::Layout::parse(&bytes, report.slide) {
        Ok(layout) => layout,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let profiled = super::profile::read(&layout.profile(&id)).unwrap_or_default();
    let entries = entries::of(report, memory, &image_layout, &profiled);
    let plan = plan(vm, &entries);

    let compiled = compile(vm, report, &plan, options);
    let complete = compiled.finished == plan.len();
    let file = CacheFile {
        lift_slide: report.slide,
        chunks: compiled.chunks,
    };
    match super::store(layout, &id, &file) {
        Ok(()) => Outcome::Written {
            regions: compiled.regions,
            refused: compiled.refused,
            complete,
        },
        Err(e) => Outcome::Failed(e.to_string()),
    }
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

struct Compiled {
    chunks: Vec<microlift::image::Image>,
    regions: usize,
    /// Regions that would not lift or compile, or that consulted an import only
    /// this run's registry answered.
    refused: usize,
    /// Planned regions reached before the deadline, compiled or refused.
    finished: usize,
}

fn compile(vm: &Vm, report: &ImageReport, plan: &[Planned], options: &Options) -> Compiled {
    let chunks: Vec<&[Planned]> = plan.chunks(CHUNK_REGIONS).collect();
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<(usize, Chunk)>> = Mutex::new(Vec::new());
    let fingerprint = vm.lifter_config().codegen_fingerprint();
    let Ok(resolved) = crate::jit::resolved_isa_settings() else {
        return Compiled {
            chunks: Vec::new(),
            regions: 0,
            refused: plan.len(),
            finished: plan.len(),
        };
    };
    let isa_settings: Vec<(&str, &str)> = resolved
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let threads = options.threads.clamp(1, chunks.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                // Each thread lifts under its own copy, which shares the program's
                // segments and imports through `Arc`s.
                let config = vm.lifter_config().clone();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(chunk) = chunks.get(index) else {
                        break;
                    };
                    if past(options.deadline) {
                        break;
                    }
                    let built = compile_chunk(
                        &config,
                        report,
                        fingerprint,
                        &isa_settings,
                        chunk,
                        options.deadline,
                    );
                    done.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((index, built));
                }
            });
        }
    });
    let mut done = done.into_inner().unwrap_or_else(|e| e.into_inner());
    done.sort_by_key(|(index, _)| *index);
    let mut out = Compiled {
        chunks: Vec::new(),
        regions: 0,
        refused: 0,
        finished: 0,
    };
    for (_, chunk) in done {
        out.regions += chunk.regions;
        out.refused += chunk.refused;
        out.finished += chunk.regions + chunk.refused;
        if let Some(image) = chunk.image.filter(|image| !image.regions.is_empty()) {
            out.chunks.push(image);
        }
    }
    out
}

struct Chunk {
    image: Option<microlift::image::Image>,
    regions: usize,
    refused: usize,
}

fn compile_chunk(
    config: &LifterConfig,
    report: &ImageReport,
    fingerprint: u64,
    isa_settings: &[(&str, &str)],
    plan: &[Planned],
    deadline: Option<Instant>,
) -> Chunk {
    let mut chunk = Chunk {
        image: None,
        regions: 0,
        refused: 0,
    };
    let Ok(mut builder) = microlift::backend::ArtifactBackend::image(
        None,
        Some(crate::jit::flags()),
        isa_settings,
        fingerprint,
    ) else {
        chunk.refused = plan.len();
        return chunk;
    };
    for planned in plan {
        if past(deadline) {
            break;
        }
        let added =
            lift(config, report, planned).is_some_and(|lifted| builder.add_region(lifted).is_ok());
        match added {
            true => chunk.regions += 1,
            false => chunk.refused += 1,
        }
    }
    chunk.image = Some(builder.finish());
    chunk
}

/// `planned`, a region of `report`, lifted as [`crate::cache`] lifts it, or `None`
/// if it may not be cached.
fn lift(
    config: &LifterConfig,
    report: &ImageReport,
    planned: &Planned,
) -> Option<microlift::core::LiftedCode> {
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
