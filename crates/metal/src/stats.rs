use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use static_init::dynamic;

macro_rules! counters {
    ($($name:ident = $label:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum Counter { $($name),* }
        const COUNTERS: usize = [$(Counter::$name),*].len();
        const NAMES: [&str; COUNTERS] = [$($label),*];
    };
}

counters!(
    CommandBuffers = "command_buffers",
    Draws = "draws",
    Dispatches = "dispatches",
    Blits = "blits",
    Encoders = "encoders",
    DescriptorSets = "descriptor_sets",
    PushDescriptorWrites = "push_descriptor_writes",
    BuffersCreated = "buffers_created",
    BufferBytes = "buffer_bytes",
    SetBytesCalls = "set_bytes_calls",
    SetBytesBytes = "set_bytes_bytes",
    TexturesCreated = "textures_created",
    PipelinesCreated = "pipelines_created",
    SubmitMicros = "submit_micros",
    SubmissionQueueDepth = "submission_queue_depth_peak",
    FenceWaitMicros = "fence_wait_micros",
    FenceWakeToRetiredMicros = "fence_wake_to_retired_micros",
    DrawableVended = "drawable_vended",
    DrawablePressure = "drawable_pressure",
    DrawableReclaimed = "drawable_reclaimed",
    InFlightContexts = "in_flight_contexts_peak",
    ContextPressure = "context_pressure",
    InFlightTransientPeakBytes = "in_flight_transient_peak_bytes",
    PresentMicros = "present_micros",
    PresentAcquireMicros = "present_acquire_micros",
    PresentAcquireCalls = "present_acquire_calls",
    PresentAcquireTimeouts = "present_acquire_timeouts",
    PresentAcquireWaitMicros = "present_acquire_wait_micros",
    PresentAcquireWaitCalls = "present_acquire_wait_calls",
    PresentRecordMicros = "present_record_micros",
    PresentSubmitMicros = "present_submit_micros",
    QueuePresentMicros = "queue_present_micros",
    QueuePresentCalls = "queue_present_calls",
    QueuePresentLockMicros = "queue_present_lock_micros",
    QueuePresentDriverMicros = "queue_present_driver_micros",
    ReadbackMicros = "readback_micros",
    BlitMicros = "blit_micros",
    Frames = "frames",
    BufferMicros = "buffer_micros",
    BufferPeakBytes = "buffer_peak_bytes",
    BufferZeroMicros = "buffer_zero_micros",
    BufferBlockMicros = "buffer_block_micros",
    BindMicros = "bind_micros",
    PipelineMicros = "pipeline_micros",
    PipelinePeakMicros = "pipeline_micros_peak",
    FrameGapPeakMicros = "frame_gap_micros_peak",
    QueueMicros = "queue_micros",
    GpuMicros = "gpu_micros",
    ImmediateCalls = "immediate_calls",
    ImmediateMicros = "immediate_micros",
    DeferredTransfers = "deferred_transfers",
);

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);
static VALUES: [AtomicU64; COUNTERS] = [ZERO; COUNTERS];

#[inline(always)]
pub fn enabled() -> bool {
    #[dynamic]
    static ON: bool = std::env::var_os("ROSEMARY_METAL_STATS").is_some_and(|value| value != "0");

    *ON
}

pub fn add(counter: Counter, amount: u64) {
    if enabled() {
        VALUES[counter as usize].fetch_add(amount, Ordering::Relaxed);
    }
}

pub fn count(counter: Counter) {
    add(counter, 1);
}

/// Keep the largest value a counter has been offered this interval.
pub fn peak(counter: Counter, amount: u64) {
    if enabled() {
        VALUES[counter as usize].fetch_max(amount, Ordering::Relaxed);
    }
}

/// Time `body`, adding its duration in microseconds to `counter`.
pub fn timed<T>(counter: Counter, body: impl FnOnce() -> T) -> T {
    if !enabled() {
        return body();
    }
    let start = Instant::now();
    let value = body();
    add(counter, start.elapsed().as_micros() as u64);
    value
}

/// Time the driver building one pipeline: the total, the count, and the
/// slowest single one of the interval, which is what a hitch is.
pub fn pipeline<T>(body: impl FnOnce() -> T) -> T {
    if !enabled() {
        return body();
    }
    let start = Instant::now();
    let value = body();
    let micros = start.elapsed().as_micros() as u64;
    add(Counter::PipelinesCreated, 1);
    add(Counter::PipelineMicros, micros);
    peak(Counter::PipelinePeakMicros, micros);
    value
}

/// Count a presented frame, and keep the longest wait between two of them:
/// a rate hides a single late frame, and that frame is the stutter.
///
/// The gap includes idle time (a hidden window, a loading pause), so one such
/// interval's peak is a stall of the guest, not necessarily of the renderer.
pub fn frame() {
    if !enabled() {
        return;
    }
    static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
    let now = Instant::now();
    let last = LAST.lock().expect("frame time").replace(now);
    if let Some(last) = last {
        peak(Counter::FrameGapPeakMicros, (now - last).as_micros() as u64);
    }
    add(Counter::Frames, 1);
}

pub fn timer() -> Option<Instant> {
    enabled().then(Instant::now)
}

pub fn finish_timer(counter: Counter, start: Option<Instant>) {
    if let Some(start) = start {
        add(counter, start.elapsed().as_micros() as u64);
    }
}

/// Report and reset the counters, once per interval.
pub fn tick() {
    if !enabled() {
        return;
    }
    static SINCE: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
    let mut since = SINCE.lock().expect("statistics interval");
    let start = *since.get_or_insert_with(Instant::now);
    let elapsed = start.elapsed();
    if elapsed < Duration::from_secs(1) {
        return;
    }
    *since = Some(Instant::now());
    let seconds = elapsed.as_secs_f64();
    let line = NAMES
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            let value = VALUES[index].swap(0, Ordering::Relaxed);
            if value == 0 {
                return None;
            }
            Some(match name.contains("_peak") {
                true => format!("{name}={value}"),
                false => format!("{name}={:.0}/s", value as f64 / seconds),
            })
        })
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!("rosemary: Metal: stats {line}");
    report_memory();
    report_shader_cache();
}

/// Live buffer memory, as totals: a leak shows as one of these climbing.
fn report_memory() {
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    // A guest that enumerates devices may open several; name the ones in use.
    let heaps = adapters
        .iter()
        .enumerate()
        .filter_map(|(index, adapter)| {
            Some(format!(
                "gpu{index} {}",
                adapter.opened_gpu()?.heap_report()?
            ))
        })
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "rosemary: Metal: memory shared_buffers={}MiB {heaps}",
        crate::SHARED_BUFFER_BYTES.load(Ordering::Relaxed) >> 20,
    );
}

/// The on-disk shader cache, as totals rather than as a rate.
///
/// Libraries are loaded in a burst at startup and almost never afterwards, so
/// a per-second figure for them is zero for the whole of a run. What is worth
/// seeing is how much of the pack a launch translated and how much it read
/// back: a run whose misses do not fall to zero on the second launch has a
/// cache it cannot write.
fn report_shader_cache() {
    let cache = metal_translate::cache::stats();
    if cache == metal_translate::cache::Stats::default() {
        return;
    }
    eprintln!(
        "rosemary: Metal: shader cache total hits={} misses={} writes={} errors={}",
        cache.hits, cache.misses, cache.writes, cache.errors
    );
}
