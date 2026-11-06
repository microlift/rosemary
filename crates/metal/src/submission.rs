use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use crate::encoder::Submission;
use crate::gpu::Gpu;
use crate::stats::{self, Counter};

/// Completion state belonging to one logical device.
///
/// Vulkan queues on different devices have no ordering relationship, so they
/// must not share a worker which can block on one device's fence.
pub(crate) struct Tracker {
    pending: Mutex<VecDeque<Submission>>,
    ready: Condvar,
    started: AtomicBool,
    next_serial: AtomicU64,
    last_enqueued: AtomicU64,
    depth: AtomicUsize,
    high_water: AtomicUsize,
    transient_bytes: AtomicU64,
}

impl Tracker {
    pub(crate) fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            started: AtomicBool::new(false),
            next_serial: AtomicU64::new(1),
            last_enqueued: AtomicU64::new(0),
            depth: AtomicUsize::new(0),
            high_water: AtomicUsize::new(0),
            transient_bytes: AtomicU64::new(0),
        }
    }

    /// Start this device's worker before entering the Vulkan queue critical
    /// section. Thread creation must never lengthen queue serialization.
    pub(crate) fn start(&'static self, gpu: &'static Gpu) {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        std::thread::Builder::new()
            .name("metal:retire".to_string())
            .spawn(move || worker(gpu))
            .expect("a thread for Metal submissions");
    }

    pub(crate) fn next_serial(&self) -> u64 {
        self.next_serial.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn push(&self, submission: Submission) {
        let bytes = submission.transient_bytes();
        let previous = self
            .last_enqueued
            .swap(submission.serial, Ordering::Relaxed);
        debug_assert!(
            submission.serial > previous,
            "submissions must enter in queue order"
        );
        let mut pending = self.pending.lock().expect("the device submission queue");
        pending.push_back(submission);
        let depth = self.depth.fetch_add(1, Ordering::Relaxed) + 1;
        self.high_water.fetch_max(depth, Ordering::Relaxed);
        stats::peak(Counter::SubmissionQueueDepth, depth as u64);
        let bytes = self.transient_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes;
        stats::peak(Counter::InFlightTransientPeakBytes, bytes);
        self.ready.notify_one();
    }

    fn pop(&self) -> Submission {
        let mut pending = self.pending.lock().expect("the device submission queue");
        loop {
            if let Some(submission) = pending.pop_front() {
                self.depth.fetch_sub(1, Ordering::Relaxed);
                self.transient_bytes
                    .fetch_sub(submission.transient_bytes(), Ordering::Relaxed);
                return submission;
            }
            pending = self
                .ready
                .wait(pending)
                .expect("the device submission queue");
        }
    }

    fn pop_signalled(&self, gpu: &Gpu) -> Option<Submission> {
        let mut pending = self.pending.lock().expect("the device submission queue");
        let signalled = pending.front().is_some_and(|submission| {
            // SAFETY: this device created the fence and the in-flight record
            // keeps it alive until it is removed here.
            unsafe { gpu.device.get_fence_status(submission.fence) }.unwrap_or(false)
        });
        signalled.then(|| {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            let submission = pending.pop_front().expect("a signalled front submission");
            self.transient_bytes
                .fetch_sub(submission.transient_bytes(), Ordering::Relaxed);
            submission
        })
    }
}

fn worker(gpu: &'static Gpu) {
    loop {
        gpu.submissions.pop().wait_and_finish();
        // Queue execution is ordered. Once the oldest fence wakes, avoid a
        // blocking driver call for every contiguous submission already done.
        while let Some(submission) = gpu.submissions.pop_signalled(gpu) {
            submission.finish(Ok(()));
        }
        // A copy queued before the work that just completed is done too.
        crate::swapchain::retire_copied_on(gpu);
    }
}
