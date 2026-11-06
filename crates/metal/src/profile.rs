//! `ROSEMARY_METAL_PROFILE`: device time per pipeline.
//!
//! `gpu_micros` says a frame waits on the device; it cannot say on which
//! shader. Every draw and dispatch is bracketed by two timestamps in a query
//! pool the command buffer and its parallel sub-encoders share, and the
//! retired results are totalled by the pipeline's label. Labels name the
//! source library by the hash `ROSEMARY_MSL_OUT` files it under, so a slow
//! entry is found on disk.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ash::vk;
use static_init::dynamic;

use crate::gpu::Gpu;

/// Queries per command buffer: a draw takes two. Draws past it go untimed and
/// are counted, so a total which leaves them out says so.
const QUERIES: u32 = 8192;
const REPORT_INTERVAL: Duration = Duration::from_secs(5);
const REPORT_ROWS: usize = 20;

pub fn enabled() -> bool {
    #[dynamic]
    static ON: bool = std::env::var_os("ROSEMARY_METAL_PROFILE").is_some_and(|value| value != "0");
    *ON
}

/// Labels of translated entry points, by the artifact's address.
static ENTRY_LABELS: Mutex<Option<HashMap<usize, Arc<str>>>> = Mutex::new(None);

pub fn label_entry(artifact: &metal_translate::EntryArtifact, label: String) {
    if !enabled() {
        return;
    }
    let mut labels = ENTRY_LABELS.lock().expect("profile labels");
    labels
        .get_or_insert_with(HashMap::new)
        .insert(artifact as *const _ as usize, label.into());
}

/// Label a specialization of `base` after it, numbered so that variants
/// made with different constants total apart.
pub fn label_specialized(
    specialized: &metal_translate::EntryArtifact,
    base: &metal_translate::EntryArtifact,
) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let label = format!("{}~{serial}", entry_label(base));
    label_entry(specialized, label);
}

pub fn entry_label(artifact: &metal_translate::EntryArtifact) -> Arc<str> {
    let labels = ENTRY_LABELS.lock().expect("profile labels");
    labels
        .as_ref()
        .and_then(|labels| labels.get(&(artifact as *const _ as usize)).cloned())
        .unwrap_or_else(|| artifact.name.as_str().into())
}

/// A label for a pipeline, or `None` while profiling is off.
pub fn pipeline_label(stages: &[&metal_translate::EntryArtifact]) -> Option<Arc<str>> {
    enabled().then(|| {
        stages
            .iter()
            .map(|artifact| entry_label(artifact).to_string())
            .collect::<Vec<_>>()
            .join(" + ")
            .into()
    })
}

/// A label for work no pipeline names, made once.
pub fn named(name: &'static str) -> Arc<str> {
    static NAMED: Mutex<Option<HashMap<&'static str, Arc<str>>>> = Mutex::new(None);
    let mut named = NAMED.lock().expect("profile labels");
    Arc::clone(
        named
            .get_or_insert_with(HashMap::new)
            .entry(name)
            .or_insert_with(|| name.into()),
    )
}

pub(crate) struct Profile {
    pool: vk::QueryPool,
    next: AtomicU32,
    spans: Mutex<Vec<(u32, Arc<str>)>>,
}

static POOLS: Mutex<Vec<vk::QueryPool>> = Mutex::new(Vec::new());

impl Profile {
    /// A profile for a primary command buffer, reset at the start of its
    /// recording, which is outside any pass and ahead of every secondary.
    pub(crate) fn begin(gpu: &Gpu, command: vk::CommandBuffer) -> Option<Arc<Profile>> {
        if !enabled() || gpu.timestamp_period() == 0.0 {
            return None;
        }
        let pool = POOLS.lock().expect("profile pools").pop().or_else(|| {
            let info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::TIMESTAMP)
                .query_count(QUERIES);
            // SAFETY: a well-formed create info on this live device.
            unsafe { gpu.device.create_query_pool(&info, None) }.ok()
        })?;
        // SAFETY: recording into a command buffer which has just begun.
        unsafe { gpu.device.cmd_reset_query_pool(command, pool, 0, QUERIES) };
        Some(Arc::new(Profile {
            pool,
            next: AtomicU32::new(0),
            spans: Mutex::new(Vec::new()),
        }))
    }

    /// Time `body`, recorded into `command`, under `label`.
    pub(crate) fn span(
        &self,
        device: &ash::Device,
        command: vk::CommandBuffer,
        label: &Arc<str>,
        body: impl FnOnce(),
    ) {
        let slot = self.next.fetch_add(2, Ordering::Relaxed);
        if slot + 2 > QUERIES {
            body();
            *UNTIMED.lock().expect("profile totals") += 1;
            return;
        }
        // SAFETY: recording into `command`; the slots were reset when the
        // primary command buffer began and are written once.
        unsafe {
            device.cmd_write_timestamp(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                self.pool,
                slot,
            )
        };
        body();
        unsafe {
            device.cmd_write_timestamp(
                command,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                self.pool,
                slot + 1,
            )
        };
        self.spans
            .lock()
            .expect("profile spans")
            .push((slot, Arc::clone(label)));
    }

    /// Total the retired spans and give the pool back.
    pub(crate) fn finish(&self, gpu: &Gpu, completed: bool) {
        let spans = std::mem::take(&mut *self.spans.lock().expect("profile spans"));
        let used = self.next.load(Ordering::Relaxed).min(QUERIES);
        if completed && used > 0 {
            let mut ticks = vec![0u64; used as usize];
            // SAFETY: the submission completed, so every written query has
            // a result; the slots read were all written.
            let read = unsafe {
                gpu.device.get_query_pool_results(
                    self.pool,
                    0,
                    &mut ticks,
                    vk::QueryResultFlags::TYPE_64,
                )
            };
            if let Err(error) = read {
                static ONCE: std::sync::Once = std::sync::Once::new();
                ONCE.call_once(|| eprintln!("rosemary: Metal: profile query read failed: {error:?} ({used} queries, {} spans)", spans.len()));
            }
            if read.is_ok() {
                let mask = match gpu.timestamp_valid_bits() {
                    64.. => u64::MAX,
                    bits => (1u64 << bits) - 1,
                };
                let mut totals = TOTALS.lock().expect("profile totals");
                let totals = totals.get_or_insert_with(HashMap::new);
                for (slot, label) in spans {
                    let start = ticks[slot as usize];
                    let end = ticks[slot as usize + 1];
                    let nanos = (end.wrapping_sub(start) & mask) as f64 * gpu.timestamp_period();
                    let entry = totals.entry(label).or_default();
                    entry.0 += nanos;
                    entry.1 += 1;
                }
            }
        }
        POOLS.lock().expect("profile pools").push(self.pool);
        report();
    }
}

/// Nanoseconds and spans per label since the last report.
type Totals = HashMap<Arc<str>, (f64, u64)>;
static TOTALS: Mutex<Option<Totals>> = Mutex::new(None);
static UNTIMED: Mutex<u64> = Mutex::new(0);
static SINCE: Mutex<Option<Instant>> = Mutex::new(None);

fn report() {
    let mut since = SINCE.lock().expect("profile interval");
    let start = *since.get_or_insert_with(Instant::now);
    let elapsed = start.elapsed();
    if elapsed < REPORT_INTERVAL {
        return;
    }
    *since = Some(Instant::now());
    drop(since);
    let Some(totals) = TOTALS.lock().expect("profile totals").take() else {
        return;
    };
    let untimed = std::mem::take(&mut *UNTIMED.lock().expect("profile totals"));
    let seconds = elapsed.as_secs_f64();
    let mut rows = totals.into_iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
    let sum: f64 = rows.iter().map(|row| row.1.0).sum();
    eprintln!(
        "rosemary: Metal: profile over {seconds:.1}s: {:.0} us/s device time in {} pipelines, {untimed} untimed",
        sum / 1e3 / seconds,
        rows.len(),
    );
    for (label, (nanos, count)) in rows.into_iter().take(REPORT_ROWS) {
        eprintln!(
            "rosemary: Metal: profile {:>8.0} us/s {:>5.1}% {:>7.0}/s {:>8.1} us each  {label}",
            nanos / 1e3 / seconds,
            nanos * 100.0 / sum.max(1.0),
            count as f64 / seconds,
            nanos / 1e3 / count as f64,
        );
    }
}
