mod corners;
mod present;
mod scene;
use scene::{SceneBlender, SceneLayer, ScenePlacement, ScenePrepared, SceneRollbackLayer};
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::isolate_vulkan_graphical_test;

use present::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, TryLockError};
use std::time::{Duration, Instant};

use ash::vk;
use vk::Handle;

use crate::gpu::{self, Gpu, GpuImage, ImageDescription};

// Shared by the retained scene and CPU-store pools: their independent soft
// limits cannot guarantee a hard aggregate when both own images for a window.
// A retired image stays charged until its last GPU fence-held Arc is dropped.
const WINDOW_GPU_PAYLOAD_BUDGET: u64 = 768 * 1024 * 1024;
struct WindowGpuBudget {
    held: std::sync::atomic::AtomicU64,
}
static WINDOW_GPU_BUDGETS: std::sync::LazyLock<
    Mutex<HashMap<usize, std::sync::Weak<WindowGpuBudget>>>,
> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct GpuCharge {
    budget: Arc<WindowGpuBudget>,
    bytes: u64,
}

impl GpuCharge {
    pub(crate) fn reserve(window: usize, bytes: u64) -> Result<Self, String> {
        if bytes == 0 || bytes > WINDOW_GPU_PAYLOAD_BUDGET {
            return Err("invalid or over-budget GPU payload allocation".into());
        }
        let budget = {
            let mut entries = WINDOW_GPU_BUDGETS
                .lock()
                .expect("window GPU budget registry");
            entries.retain(|_, entry| entry.strong_count() != 0);
            entries
                .entry(window)
                .or_default()
                .upgrade()
                .unwrap_or_else(|| {
                    let fresh = Arc::new(WindowGpuBudget {
                        held: std::sync::atomic::AtomicU64::new(0),
                    });
                    entries.insert(window, Arc::downgrade(&fresh));
                    fresh
                })
        };
        // Atomic reservation, with no GPU queue/scene/backing lock held by
        // the ledger itself. Two independent worker pools cannot overcommit.
        budget
            .held
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used <= WINDOW_GPU_PAYLOAD_BUDGET - bytes).then_some(used + bytes)
            })
            .map_err(|_| "per-window compositor GPU payload budget exhausted".to_owned())?;
        Ok(Self { budget, bytes })
    }
}

fn window_gpu_capacity_available(window: usize, incoming: u64) -> bool {
    if incoming > WINDOW_GPU_PAYLOAD_BUDGET {
        return false;
    }
    let registry = WINDOW_GPU_BUDGETS
        .lock()
        .expect("window GPU budget registry");
    let held = registry
        .get(&window)
        .and_then(std::sync::Weak::upgrade)
        .map_or(0, |budget| budget.held.load(Ordering::Acquire));
    held <= WINDOW_GPU_PAYLOAD_BUDGET - incoming
}

impl Drop for GpuCharge {
    fn drop(&mut self) {
        self.budget.held.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[cfg(test)]
#[test]
fn window_gpu_ledger_caps_simultaneous_sources_and_reclaims_retired_allocations() {
    let window = 0xA7101726usize;
    let another = window + 1;
    let pixels = 6016u64 * 3384 * 4;
    let owned = Arc::new(GpuCharge::reserve(window, pixels * 4).unwrap());
    let inflight = Arc::clone(&owned);
    let second = GpuCharge::reserve(window, pixels * 3).unwrap();
    assert!(GpuCharge::reserve(window, pixels * 3).is_err());
    assert!(GpuCharge::reserve(another, WINDOW_GPU_PAYLOAD_BUDGET).is_ok());
    drop(owned);
    assert!(GpuCharge::reserve(window, WINDOW_GPU_PAYLOAD_BUDGET).is_err());
    drop(inflight);
    drop(second);
    assert!(GpuCharge::reserve(window, WINDOW_GPU_PAYLOAD_BUDGET).is_ok());
}

fn gpu_image_payload_bytes(width: u32, height: u32, format: vk::Format) -> u64 {
    let bytes = match format {
        vk::Format::B8G8R8A8_UNORM
        | vk::Format::R8G8B8A8_UNORM
        | vk::Format::B8G8R8A8_SRGB
        | vk::Format::R8G8B8A8_SRGB => 4,
        vk::Format::R16G16B16A16_SFLOAT => 8,
        _ => 16, // Unsupported/unusual formats are conservatively charged.
    };
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(bytes)
}

/// How many compositor updates can be queued before one has to finish.
///
/// Independent Core Animation layers are staged before a window presentation.
/// The accumulator submissions must not reuse their command buffers while
/// those small copies are still in the GPU queue.
const FRAMES_IN_FLIGHT: usize = 8;
// A proposed scene whose immutable sources plus one scratch per *actual*
// isolation depth cannot fit the retained-scene budget is not made valid by
// waiting for fences. Preserve the last complete scene until the producer
// publishes a different proposal rather than retrying that exact image.
const TERMINAL_GROUP_BUDGET: &str = "Core Animation isolation scene irreducible under GPU budget";

// Keep the historical Vulkan image handle at `.image` for callers while
// retaining the actual allocation and its ledger charge behind an Arc.
struct CompositeImage {
    image: vk::Image,
    lease: Arc<BackingImageLease>,
}

impl CompositeImage {
    fn create(
        gpu: &'static Gpu,
        window: usize,
        description: &ImageDescription,
    ) -> Result<Arc<Self>, String> {
        let lease = BackingImageLease::create_for_window(gpu, window, description)?;
        Ok(Arc::new(Self {
            image: lease.image.image,
            lease,
        }))
    }
}

impl std::ops::Deref for CompositeImage {
    type Target = GpuImage;
    fn deref(&self) -> &GpuImage {
        &self.lease.image
    }
}

// A cached CPU-only base is safe to sample across DMA-content revisions only
// while its original immutable Arc<Vec<u8>> and exact extent still match.
// GPU snapshots of the *blended* result remain separately revision-owned.
struct NativeCpuBaseCache {
    id: usize,
    source: std::sync::Weak<Vec<u8>>,
    cpu_revision: Option<u64>,
    image: Arc<BackingImageLease>,
}

// An unsupported *individual* immutable DMA producer does not need another
// futile GPU allocation on the same revision. This cache is not a window-wide
// capability flag and never covers an unrelated producer or transient error.
struct UnsupportedNativeDma {
    id: usize,
    revision: u64,
    producer: std::sync::Weak<appkit::backing::Dmabuf>,
}

/// The per-window state presentation goes through.
// A queued native upload owns its charge, buffer and every GPU image the
// command references until the submitted fence is confirmed complete.
struct NativeStageGpuResources {
    gpu: &'static Gpu,
    buffer: Option<gpu::GpuBuffer>,
    _charge: Option<GpuCharge>,
    _source: Arc<BackingImageLease>,
    _target: Option<Arc<BackingImageLease>>,
}

impl Drop for NativeStageGpuResources {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.gpu.destroy_buffer(buffer);
        }
    }
}

struct Swapchain {
    window: usize,
    gpu: &'static Gpu,
    surface: vk::SurfaceKHR,
    handle: vk::SwapchainKHR,
    format: vk::Format,
    /// The size the swapchain was actually created at, which is the surface's
    /// answer and not necessarily the one that was asked for.
    extent: vk::Extent2D,
    /// The window size that was last asked for, which is what a later frame
    /// is compared against to decide whether the window resized.
    ///
    /// Not [`Self::extent`]: a surface with a `currentExtent` of its own
    /// overrides the request, and comparing against the overridden value
    /// would see a difference on every frame and rebuild the swapchain on
    /// every frame.
    requested: vk::Extent2D,
    /// The event pump never waits for WSI recreation. A window-local
    /// worker consumes the newest requested extent under the presenting lock.
    pending_rebuild: Option<vk::Extent2D>,
    /// vkCreateSwapchainKHR retires oldSwapchain even if creation fails.
    retired: bool,
    images: Vec<vk::Image>,
    /// One presentation semaphore per swapchain image, rather than one per
    /// frame in flight.
    ///
    /// A presentation holds the semaphore it waits on until that image comes
    /// back from `vkAcquireNextImageKHR`, and how many images there are is
    /// the presentation engine's business, not this module's - a swapchain
    /// with four images and two frames in flight would signal a semaphore
    /// the engine was still waiting on. Indexing by the acquired image makes
    /// the acquisition itself the proof that the last presentation of this
    /// image is done with it.
    blitted: Vec<vk::Semaphore>,
    /// Fences attached to presentations through swapchain maintenance1. One
    /// per image lets acquisition identify the fence which may be recycled.
    /// Empty on devices which use the queue-idle fallback.
    presented: Vec<PresentFence>,
    /// Where layers accumulate when the window has more than one. `None`
    /// while a single layer covers the window, which needs no accumulator.
    composite: Option<Arc<CompositeImage>>,
    /// The latest full-window opaque drawable, retained until a later direct
    /// frame replaces it or a second layer needs it as a GPU snapshot.
    direct: Option<DirectLayer>,
    /// The layers which have presented to this window. Its length is what
    /// decides whether the composite is needed.
    layers: Vec<usize>,
    /// Latest GPU-owned contents for each layer in the flattened window tree.
    /// Scene images must survive both drawable reuse and in-flight GPU reads.
    scene_layers: Vec<SceneLayer>,
    // Bounded optional GPU copies of immutable native CPU bases. Never an
    // authoritative scene image; safe to drop on any budget pressure.
    native_cpu_bases: Vec<NativeCpuBaseCache>,
    unsupported_native_dma: Vec<UnsupportedNativeDma>,
    /// Old image leases pinned by the current native admission transaction.
    /// GPU bytes remain charged even after their live SceneLayer is replaced.
    transaction_pinned: Vec<Arc<BackingImageLease>>,
    /// Rollback-pinned images of native views absent from the proposed scene.
    /// Excluded only from projected 512 MiB capacity, never actual allocations.
    transaction_retiring_bytes: u64,
    scene_placements: Vec<ScenePlacement>,
    scene_blender: Option<SceneBlender>,
    /// Fence-slot-local isolated Core Animation group targets. Never shared
    /// across in-flight GPU command buffers.
    group_targets: Vec<Vec<Arc<BackingImageLease>>>,
    /// Zero for a full-group scene plan, nonzero for bounded isolation tiles.
    /// Output semantics do not depend on the chosen scratch representation.
    scene_tile_edge: u32,
    #[cfg(test)]
    last_scene_repaint_pixels: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    last_scene_dispatches: std::sync::atomic::AtomicU64,
    /// The previous layer's frame is needed when its contents must first be
    /// captured out of a single-layer accumulator when another layer joins.
    scene_frames: Vec<(usize, DirectFrame)>,
    /// Scene damage survives an early membership sync or a busy staging slot.
    /// It is cleared only once a GPU update contains the new visible scene.
    scene_repaint_needed: bool,
    /// An incomplete source batch has altered GPU pixels. Never publish its
    /// accumulator until the previously committed scene is recomposed.
    scene_restore_pending: bool,
    /// Host or GPU copies of the last accepted image generations, pending
    /// complete reconstruction before another frame can be published.
    pending_scene_rollback: Option<Vec<SceneRollbackLayer>>,
    #[cfg(test)]
    injected_restore_busy_cycles: u32,
    #[cfg(test)]
    restore_busy_cycles_seen: u32,
    #[cfg(test)]
    wsi_publishes: u64,
    /// For a window whose surface shows its alpha, the pass that makes its
    /// content opaque and rounds its corners. `None` presents opaque.
    corners: Option<corners::Corners>,
    pool: vk::CommandPool,
    frames: Vec<Frame>,
    frame: usize,
    present_mode: vk::PresentModeKHR,
    /// Metal layers accumulate in an offscreen image independently of Vulkan
    /// WSI pacing. The presentation worker publishes the latest window image.
    composite_dirty: bool,
    composite_shape: Option<corners::Shape>,
    composite_dirty_since: Option<Instant>,
    compositor_batch_delay: Duration,
    last_wsi_error: Option<vk::Result>,
    /// A maintenance1 present fence did not signal even after reacquiring
    /// its image. Queue idle is the fallback at retirement for this driver.
    stalled_present_fence: bool,
    #[cfg(test)]
    injected_unsignaled_present_fence: bool,
    #[cfg(test)]
    skipped_stalled_present_fence: bool,
    #[cfg(test)]
    injected_build_vk_error: Option<vk::Result>,
    #[cfg(test)]
    injected_stage_vk_error: Option<vk::Result>,
    #[cfg(test)]
    injected_post_scene_record_error: Option<vk::Result>,
    #[cfg(test)]
    injected_queue_present_error: Option<vk::Result>,
    #[cfg(test)]
    #[cfg(test)]
    injected_queue_submit_error: Option<vk::Result>,
    /// vkAcquireNextImageKHR succeeded, but its semaphore wait has not yet
    /// been submitted. Failure requires retiring this swapchain, not simply
    /// resetting a fence and reusing the signaled acquisition semaphore.
    acquired_unsubmitted: Option<(vk::Semaphore, vk::Fence)>,
    #[cfg(test)]
    injected_recovery_error: Option<vk::Result>,
    #[cfg(test)]
    injected_build_failure: bool,
    #[cfg(test)]
    injected_unavailable_extent: bool,
    #[cfg(test)]
    rebuild_attempts: usize,
    #[cfg(test)]
    last_create_old_swapchain: vk::SwapchainKHR,
}

/// A backing-store GPU upload can be reused only after every asynchronous
/// WSI read of it has completed. Store uploads own one Arc; pending direct
/// presentation and its GPU copy fence hold additional references.
pub(crate) struct BackingImageLease {
    gpu: &'static Gpu,
    pub image: GpuImage,
    _charge: Option<GpuCharge>,
}

impl BackingImageLease {
    pub(crate) fn new(gpu: &'static Gpu, image: GpuImage) -> Arc<Self> {
        Arc::new(Self {
            gpu,
            image,
            _charge: None,
        })
    }
}

impl BackingImageLease {
    /// Reserve the per-window aggregate before creating Vulkan memory.
    pub(crate) fn create_for_window(
        gpu: &'static Gpu,
        window: usize,
        description: &ImageDescription,
    ) -> Result<Arc<Self>, String> {
        let charge = GpuCharge::reserve(
            window,
            gpu_image_payload_bytes(description.width, description.height, description.format),
        )?;
        let image = gpu.create_image(description)?;
        Ok(Arc::new(Self {
            gpu,
            image,
            _charge: Some(charge),
        }))
    }
}

impl std::ops::Deref for BackingImageLease {
    type Target = GpuImage;
    fn deref(&self) -> &GpuImage {
        &self.image
    }
}

impl Drop for BackingImageLease {
    fn drop(&mut self) {
        self.gpu
            .destroy_image(std::mem::replace(&mut self.image, GpuImage::none()));
    }
}

/// Serialize presentations for one window while leaving swapchain state
/// available to fence retirement during the blocking image acquisition.
/// Dropping the permit releases the window lock before waking the Metal
/// presenter. That ordering lets deferred staging acquire the lock immediately.
struct PresentingGuard<'a> {
    held: Option<std::sync::MutexGuard<'a, ()>>,
    gpu: &'static Gpu,
}

impl Drop for PresentingGuard<'_> {
    fn drop(&mut self) {
        drop(self.held.take());
        self.gpu.presenter.notify_progress();
    }
}

struct WindowSwapchain {
    gpu: &'static Gpu,
    presenting: Mutex<()>,
    state: Mutex<Swapchain>,
    /// Latest AppKit hierarchy snapshot, handed to the WSI worker without
    /// holding its GPU or presentation locks on the AppKit event thread.
    pending_scene: Mutex<Option<Vec<ScenePlacement>>>,
    wake: Condvar,
    alive: AtomicBool,
    worker_started: AtomicBool,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    resize_worker_started: AtomicBool,
    resize_worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// 0 = healthy, 1 = surface lost, 2 = device/unknown terminal failure.
    present_failure: std::sync::atomic::AtomicU8,
    #[cfg(test)]
    injected_acquire_error: Mutex<Option<vk::Result>>,
}

struct PreparedPresent {
    slot: usize,
    command_buffer: vk::CommandBuffer,
    acquired: vk::Semaphore,
    done: vk::Fence,
    composite: Option<vk::Image>,
}

#[derive(Clone, Copy)]
struct AcquireRequest {
    handle: vk::SwapchainKHR,
    semaphore: vk::Semaphore,
    fence: vk::Fence,
}

/// Only the image identity and extent cross an acquire. The swapchain
/// retains ownership of the actual Vulkan allocation under `presenting`.
#[derive(Clone, Copy)]
struct PresentSource {
    image: vk::Image,
    format: vk::Format,
    width: u32,
    height: u32,
}

impl From<&GpuImage> for PresentSource {
    fn from(image: &GpuImage) -> Self {
        Self {
            image: image.image,
            format: image.format,
            width: image.width,
            height: image.height,
        }
    }
}

impl From<&Arc<CompositeImage>> for PresentSource {
    fn from(image: &Arc<CompositeImage>) -> Self {
        Self::from(&image.lease.image)
    }
}

/// The compositor keeps its image and DMA fence alive across a WSI acquire.
/// The Vulkan acquire itself must not hold the swapchain-state mutex.
struct PreparedComposite {
    frame: appkit::view::Frame,
    source: PresentSource,
    prepared: PreparedPresent,
    request: AcquireRequest,
    direct: bool,
}

/// A directly presented drawable stays available for a later layer to join.
/// Its WSI-copy fence protects release if another frame supersedes it.
// Store SDL's opaque window handle as an integer, as the other worker state
// does. Reconstruct the Frame only under the window's presenting mutex.
#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectFrame {
    window: usize,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    window_width: u32,
    window_height: u32,
}

impl DirectFrame {
    fn from_frame(frame: &appkit::view::Frame) -> Self {
        Self {
            window: frame.window as usize,
            x: frame.x,
            y: frame.y,
            width: frame.width,
            height: frame.height,
            window_width: frame.window_width,
            window_height: frame.window_height,
        }
    }

    fn frame(self) -> appkit::view::Frame {
        appkit::view::Frame {
            window: self.window as sdl3::Window,
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            window_width: self.window_width,
            window_height: self.window_height,
        }
    }
}

struct DirectLayer {
    layer: usize,
    frame: DirectFrame,
    source: PresentSource,
    owner: Option<usize>,
    backing: Option<Arc<BackingImageLease>>,
    submitted_slot: Option<usize>,
    queued_at: Instant,
}

impl Drop for DirectLayer {
    fn drop(&mut self) {
        finish_source(self.owner.take());
    }
}

enum CompositorAcquire {
    Ready(Option<(u32, bool)>),
    TimedOut,
    Terminal(vk::Result),
}

/// Wake an idle compositor worker without losing a wake issued immediately
/// before it parks. Condvar still covers the bounded batch-delay wait.
fn wake_compositor_worker(entry: &WindowSwapchain) {
    if let Some(worker) = entry
        .worker
        .lock()
        .expect("layer compositor worker")
        .as_ref()
    {
        worker.thread().unpark();
    }
    entry.wake.notify_all();
}

/// AppKit only transfers value snapshots. It never waits for an in-flight
/// Vulkan frame or calls into the renderer with an Objective-C object alive.
pub(crate) fn scene_changed(window: sdl3::Window, visible: Vec<appkit::view::MetalSceneLayer>) {
    let entry = SWAPCHAINS
        .lock()
        .expect("window swapchains")
        .as_ref()
        .and_then(|all| all.get(&(window as usize)).cloned());
    let Some(entry) = entry else {
        return;
    };
    if !entry.alive.load(Ordering::Acquire) {
        return;
    }
    let ordered = visible
        .into_iter()
        .map(|layer| ScenePlacement {
            opacity_groups: layer.opacity_groups,
            id: layer.layer,
            frame: DirectFrame::from_frame(&layer.frame),
            opacity: layer.opacity,
            opaque: layer.opaque,
            clip: layer.clip.map(|frame| DirectFrame::from_frame(&frame)),
            rounded: layer
                .rounded
                .into_iter()
                .map(|clip| scene::RoundedClip {
                    frame: DirectFrame::from_frame(&clip.frame),
                    radius: clip.radius,
                })
                .collect(),
            occluded: Some(
                layer
                    .occluded
                    .into_iter()
                    .map(|r| Region {
                        x: r.x,
                        y: r.y,
                        width: r.width.min(i32::MAX as u32) as i32,
                        height: r.height.min(i32::MAX as u32) as i32,
                    })
                    .collect(),
            ),
            native: native_scene_source(
                layer.native_pixels,
                layer.native_dmabuf,
                layer.native_revision,
                layer.frame.width,
                layer.frame.height,
                layer.native_cpu_capture,
                layer.native_dma_sample,
            ),
        })
        .collect();
    *entry.pending_scene.lock().expect("AppKit scene pending") = Some(ordered);
    wake_compositor_worker(&entry);
}

fn wake_resize_worker(entry: &WindowSwapchain) {
    if let Some(worker) = entry.resize_worker.lock().expect("resize worker").as_ref() {
        worker.thread().unpark();
    }
}

fn ensure_resize_worker(window: usize, entry: &Arc<WindowSwapchain>) {
    if entry.resize_worker_started.swap(true, Ordering::AcqRel) {
        return;
    }
    let owned = Arc::clone(entry);
    let handle = std::thread::Builder::new()
        .name("rosemary-wsi-resize".into())
        .spawn(move || resize_worker(window, owned))
        .expect("window-local WSI resize worker");
    *entry.resize_worker.lock().expect("resize worker") = Some(handle);
}

fn resize_worker(window: usize, entry: Arc<WindowSwapchain>) {
    loop {
        if !entry.alive.load(Ordering::Acquire)
            || entry.gpu.lost.load(Ordering::Acquire)
            || entry.present_failure.load(Ordering::Acquire) != 0
        {
            return;
        }
        if entry
            .state
            .lock()
            .expect("swapchain")
            .pending_rebuild
            .is_none()
        {
            std::thread::park();
            continue;
        }
        let presenting = PresentingGuard {
            held: Some(entry.presenting.lock().expect("window presentation")),
            gpu: entry.gpu,
        };
        #[cfg(test)]
        if TEST_BLOCK_RESIZE_WORKER.load(Ordering::Acquire) == window {
            TEST_RESIZE_WORKER_PAUSED.store(window, Ordering::Release);
            while TEST_BLOCK_RESIZE_WORKER.load(Ordering::Acquire) == window {
                std::thread::sleep(Duration::from_millis(1));
            }
            TEST_RESIZE_WORKER_PAUSED.store(0, Ordering::Release);
        }
        if !entry.alive.load(Ordering::Acquire) || entry.gpu.lost.load(Ordering::Acquire) {
            return;
        }
        let mut state = entry.state.lock().expect("swapchain");
        let Some(extent) = state.pending_rebuild else {
            continue;
        };
        // idle() and swapchain construction can block on GPU progress.
        // Neither operation is ever performed by the AppKit event thread.
        let rebuilt = if state.acquired_unsubmitted.is_some() {
            // A failed submit left an acquired WSI semaphore outstanding.
            // Consuming it and retiring the image can wait for GPU progress;
            // that recovery belongs on this worker, never in AppKit.
            state.recover_unsubmitted_acquire().and_then(|_| {
                if state.requested != extent {
                    state.rebuild(extent)
                } else {
                    Ok(())
                }
            })
        } else {
            state.rebuild(extent)
        };
        let typed_device_loss = state.last_wsi_error.take() == Some(vk::Result::ERROR_DEVICE_LOST);
        match rebuilt {
            Ok(()) => {
                state.pending_rebuild = None;
                let dirty = uses_layer_compositor(state.present_mode) && state.composite_dirty;
                drop(state);
                drop(presenting);
                if dirty {
                    ensure_layer_compositor_worker(window, &entry);
                    wake_compositor_worker(&entry);
                }
                appkit::backing::present_again(window as sdl3::Window);
            }
            Err(reason) if recoverable_composite_rebuild_failure(&reason) => {
                drop(state);
                drop(presenting);
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(reason) => {
                gpu::trace(format!("deferred WSI resize failed: {reason}"));
                let device_lost = typed_device_loss || reason.contains("ERROR_DEVICE_LOST");
                drop(state);
                drop(presenting);
                // No window lock may be held while notifying the device registry.
                if device_lost {
                    record_device_loss(entry.gpu);
                } else {
                    entry.present_failure.store(2, Ordering::Release);
                    wake_compositor_worker(&entry);
                }
                return;
            }
        }
    }
}

pub(crate) fn record_device_loss(gpu: &Gpu) {
    if gpu.lost.swap(true, Ordering::AcqRel) {
        return;
    }
    gpu::trace("Vulkan logical device lost; stopping all associated composited windows");
    let registry = SWAPCHAINS.lock().expect("window swapchains");
    if let Some(entries) = registry.as_ref() {
        for entry in entries
            .values()
            .filter(|entry| std::ptr::eq(entry.gpu, gpu))
        {
            entry.present_failure.store(2, Ordering::Release);
            wake_compositor_worker(entry);
            wake_resize_worker(entry);
        }
    }
}

fn handle_terminal_error(gpu: &'static Gpu, entry: &WindowSwapchain, error: vk::Result) {
    if error == vk::Result::ERROR_DEVICE_LOST {
        record_device_loss(gpu);
    } else {
        entry
            .present_failure
            .store(terminal_present_failure(error), Ordering::Release);
    }
}

// Zero extent and out-of-date surfaces need a later retry, not a terminal
// failure. A minimized window may remain in this state indefinitely.
fn recoverable_composite_rebuild_failure(reason: &str) -> bool {
    reason.contains("the window has no area to present into")
        || reason.contains("ERROR_OUT_OF_DATE_KHR")
        || reason.contains("SUBOPTIMAL_KHR")
        || reason.contains("ERROR_INITIALIZATION_FAILED")
}

// Converted SDL windows cannot use SDL_GetWindowSurface as fallback.
// Terminal allocation/device failures stop the window, while transient
// staging failures leave the Metal drawable queued for a later retry.
fn terminal_composite_staging_failure(reason: &str) -> bool {
    reason.contains("ERROR_DEVICE_LOST")
        || reason.contains("ERROR_OUT_OF_HOST_MEMORY")
        || reason.contains("ERROR_OUT_OF_DEVICE_MEMORY")
}

/// CPU backing-store damage is needed only once the *first* frame of a new
/// Metal layer has actually entered the Vulkan window accumulator.
fn composite_first_stage_needs_store(
    joined: bool,
    staged: &Result<Option<bool>, String>,
    layer: usize,
    window: usize,
) -> bool {
    joined && layer != window && matches!(staged, Ok(Some(true)))
}

fn terminal_present_failure(error: vk::Result) -> u8 {
    if error == vk::Result::ERROR_SURFACE_LOST_KHR {
        1
    } else {
        2
    }
}

/// One frame's worth of recording and synchronization.
struct Frame {
    command_buffer: vk::CommandBuffer,
    /// Signalled when the presentation engine is done with the acquired
    /// image, waited on before anything is written into it. Per frame rather
    /// than per image, because which image an acquisition returns is not
    /// known until it has returned.
    acquired: vk::Semaphore,
    /// Signals when WSI finishes signaling the acquisition semaphore.
    /// Needed to safely retire an unconsumed semaphore if recovery fails.
    acquire_done: vk::Fence,
    acquire_pending: bool,
    /// Signalled with the submission, so that this frame's command buffer and
    /// its acquisition semaphore are not reused while the device still has
    /// them.
    done: vk::Fence,
    submitted: bool,
    /// Retained drawable whose image this frame's command buffer reads.
    source: Option<usize>,
    /// Store upload source kept immutable until the associated GPU copy fence.
    backing: Option<Arc<BackingImageLease>>,
    /// Retained scene source images referenced by the GPU command buffer.
    scene_reads: Vec<Arc<BackingImageLease>>,
}

struct PresentFence {
    fence: vk::Fence,
    /// Whether this fence was attached to a presentation and has not yet been
    /// waited for. A fresh fence is unsignalled but has no operation to wait
    /// for, so the distinction is required during initial teardown.
    pending: bool,
}

static SWAPCHAINS: Mutex<Option<HashMap<usize, Arc<WindowSwapchain>>>> = Mutex::new(None);

/// A Vulkan initializer never owns the global map mutex while creating
/// swapchain resources. Each window can initialize independently.
static INITIALIZING: Mutex<Option<HashMap<usize, Arc<AtomicBool>>>> = Mutex::new(None);

/// An initializer removes only its own token. SDL may recycle a window
/// address while a cancelled initializer is still unwinding.
struct InitializerRegistration {
    window: usize,
    token: Arc<AtomicBool>,
}

impl Drop for InitializerRegistration {
    fn drop(&mut self) {
        let mut pending = INITIALIZING
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(all) = pending.as_mut()
            && all
                .get(&self.window)
                .is_some_and(|current| Arc::ptr_eq(current, &self.token))
        {
            all.remove(&self.window);
        }
    }
}

struct InitRetry {
    after: Instant,
    delay: Duration,
    unknown_failures: u32,
}
static INIT_RETRIES: Mutex<Option<HashMap<usize, InitRetry>>> = Mutex::new(None);

fn retry_ready(window: usize) -> bool {
    let retry = INIT_RETRIES.lock().expect("WSI initialization retry");
    retry
        .as_ref()
        .and_then(|retries| retries.get(&window))
        .is_none_or(|retry| Instant::now() >= retry.after)
}

// Minimized windows can be unavailable indefinitely. Cap backoff, not retries
// for identified transient failures. Unclassified SDL failures have a budget.
const MAX_UNCLASSIFIED_SURFACE_FAILURES: u32 = 8;

fn schedule_retry(window: usize, unclassified_failure: bool) -> bool {
    let mut retries = INIT_RETRIES.lock().expect("WSI initialization retry");
    let retry = retries.get_or_insert_with(HashMap::new);
    let unknown_failures = retry
        .get(&window)
        .map_or(0, |previous| previous.unknown_failures)
        .saturating_add(u32::from(unclassified_failure));
    if unclassified_failure && unknown_failures >= MAX_UNCLASSIFIED_SURFACE_FAILURES {
        return false;
    }
    let delay = retry
        .get(&window)
        .map_or(Duration::from_millis(100), |previous| {
            (previous.delay * 2).min(Duration::from_secs(2))
        });
    retry.insert(
        window,
        InitRetry {
            after: Instant::now() + delay,
            delay,
            unknown_failures,
        },
    );
    true
}

fn reset_retry(window: usize) {
    if let Some(retries) = INIT_RETRIES
        .lock()
        .expect("WSI initialization retry")
        .as_mut()
    {
        retries.remove(&window);
    }
}

fn permanent_sdl_surface_failure(failure: &sdl3::video::vulkan::SurfaceFailure) -> bool {
    let text = failure.reason.to_ascii_lowercase();
    !failure.transient
        && (text.contains("not supported")
            || text.contains("unsupported")
            || text.contains("not available")
            || text.contains("incompatible")
            || text.contains("failed to load")
            || text.contains("missing")
            || text.contains("abandoned"))
}

fn permanent_init_failure(reason: &str) -> bool {
    reason.contains("belongs to OpenGL")
        || reason.contains("cannot present to this window")
        || reason.contains("VK_KHR_surface is gone")
        || reason.contains("VK_KHR_swapchain is gone")
}

#[cfg(test)]
static TEST_SLOW_INITIALIZER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_SLOW_INITIALIZER_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static TEST_BLOCK_RESIZE_WORKER: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_RESIZE_WORKER_PAUSED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_WINDOW_RETIRED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_ACTIVE_RETIREMENTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn wait_for_test_retirements() {
    let deadline = Instant::now() + Duration::from_secs(8);
    while TEST_ACTIVE_RETIREMENTS.load(Ordering::Acquire) != 0 {
        assert!(
            Instant::now() < deadline,
            "Vulkan retirement failed to finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    sdl3::video::vulkan::service_surface_requests();
}
#[cfg(test)]
static TEST_COMPOSITOR_IDLE_PARKED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_FAIL_FIRST_CREATE_WINDOW: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_FAIL_DEVICE_CREATE_WINDOW: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_SURFACE_GUARD_RELEASES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn release_created_surface(instance: u64, surface: u64) {
    #[cfg(test)]
    if surface == u64::MAX {
        TEST_SURFACE_GUARD_RELEASES.fetch_add(1, Ordering::AcqRel);
        return;
    }
    sdl3::video::vulkan::destroy_surface(instance, surface);
}

/// Whether a swapchain could not be made for this window, so that a run does
/// not retry - and re-report - a surface the host will never give.
static REFUSED: Mutex<Option<HashMap<usize, ()>>> = Mutex::new(None);

struct SourceOwner(Option<usize>);

impl SourceOwner {
    fn take(&mut self) -> Option<usize> {
        self.0.take()
    }
}

impl Drop for SourceOwner {
    fn drop(&mut self) {
        if let Some(source) = self.0.take() {
            // A pre-WSI failure may still fall back to CPU readback, so its
            // original Metal drawable submission owns the drawable slot.
            objc::arc::release(source as objc::object::Id);
        }
    }
}

/// Drop everything held for an SDL window that is about to be destroyed.
///
/// `appkit` remakes a window when what it is changes - a hidden dialog shown
/// again as a menu - and the map here is keyed by the window's address, which
/// SDL reuses. An entry left behind would be presented to a surface that no
/// longer exists.
pub(crate) fn forget_window(window: sdl3::Window) {
    let key = window as usize;
    reset_retry(key);
    if let Some(initializing) = INITIALIZING
        .lock()
        .expect("initializers")
        .as_mut()
        .and_then(|all| all.remove(&key))
    {
        initializing.store(true, Ordering::Release);
    }
    let entry = SWAPCHAINS
        .lock()
        .expect("swapchains")
        .as_ref()
        .and_then(|all| all.get(&key))
        .cloned();
    if let Some(entry) = &entry {
        // A resize may be blocked in vkQueueWaitIdle with the window lock.
        // Cancellation cannot wait for that lock on the AppKit thread.
        entry.alive.store(false, Ordering::Release);
        entry.wake.notify_all();
        wake_compositor_worker(entry);
        wake_resize_worker(entry);
    }
    let removed = SWAPCHAINS
        .lock()
        .expect("swapchains")
        .as_mut()
        .and_then(|all| all.remove(&key));
    drop(entry);
    if let Some(entry) = removed {
        // SDL_DestroyWindow can run as soon as we return. A token prevents
        // SDL from disposing of the native VkSurface until this swapchain
        // and every window-local worker have actually retired.
        let retired = Arc::new(AtomicBool::new(false));
        sdl3::video::vulkan::hold_window_for_teardown(window, Arc::clone(&retired));
        #[cfg(test)]
        TEST_ACTIVE_RETIREMENTS.fetch_add(1, Ordering::AcqRel);
        std::thread::Builder::new()
            .name("rosemary-wsi-retire".into())
            .spawn(move || {
                let worker = entry.worker.lock().expect("layer compositor worker").take();
                if let Some(worker) = worker {
                    worker.thread().unpark();
                    let _ = worker.join();
                }
                let resize = entry.resize_worker.lock().expect("resize worker").take();
                if let Some(resize) = resize {
                    resize.thread().unpark();
                    let _ = resize.join();
                }
                // A concurrent presentation may retain another Arc. Wait
                // without holding the AppKit or SDL video thread.
                while Arc::strong_count(&entry) != 1 {
                    std::thread::sleep(Duration::from_millis(1));
                }
                drop(entry);
                #[cfg(test)]
                TEST_WINDOW_RETIRED.store(key, Ordering::Release);
                retired.store(true, Ordering::Release);
                #[cfg(test)]
                TEST_ACTIVE_RETIREMENTS.fetch_sub(1, Ordering::AcqRel);
            })
            .expect("asynchronous Vulkan window retirement");
    }
    if let Some(refused) = REFUSED.lock().expect("refused windows").as_mut() {
        refused.remove(&key);
    }
    sdl3::video::forget_cached_window_refresh_rate(window);
}

/// Blit `source` into `window`'s swapchain image at `frame` and present it.
///
/// `Ok(false)` is "this window cannot be presented to with a swapchain",
/// which is not an error: the caller falls back to reading the frame back and
/// blitting it through SDL's window surface, and every host without
/// `VK_KHR_surface` takes that path.
///
/// `holes` are rectangles of other layers, which keep what those layers last
/// presented there: the window's store is presented around its views' layers.
#[cfg(test)]
pub(crate) fn present(
    gpu: &'static Gpu,
    layer: usize,
    frame: &appkit::view::Frame,
    source: &GpuImage,
    holes: &[(usize, Region)],
) -> Result<bool, String> {
    let limit = Instant::now() + Duration::from_secs(3);
    loop {
        match present_impl(gpu, layer, frame, source, holes, None, None, true)? {
            Some(shown) => return Ok(shown),
            None if Instant::now() < limit => {
                // These tests do not run an AppKit event pump.
                sdl3::video::vulkan::service_surface_requests();
                std::thread::sleep(Duration::from_millis(2));
            }
            None => return Err("timed out initializing test swapchain".into()),
        }
    }
}

/// Fast preflight before uploading a backing store. The post-upload
/// `present_nonblocking` check handles races with newly acquired WSI locks.
pub(crate) fn window_rendering_disabled(window: sdl3::Window) -> bool {
    let Ok(guard) = SWAPCHAINS.try_lock() else {
        return false;
    };
    guard
        .as_ref()
        .and_then(|all| all.get(&(window as usize)))
        .is_some_and(|entry| {
            entry.gpu.lost.load(Ordering::Acquire)
                || entry.present_failure.load(Ordering::Acquire) != 0
        })
}

/// A permanent failure can still use CPU fallback only if SDL never
/// converted the window to Vulkan.
pub(crate) fn window_refused(window: sdl3::Window) -> Option<bool> {
    let refused = REFUSED.lock().expect("refused windows");
    refused
        .as_ref()
        .filter(|m| m.contains_key(&(window as usize)))
        .map(|_| sdl3::video::vulkan::owns_window(window))
}

/// Do not upload the backing store on every pump while initialization is
/// still running or backing off. The first frame still requests a surface.
pub(crate) fn window_initialization_pending(window: sdl3::Window) -> bool {
    let key = window as usize;
    let entry_exists = match SWAPCHAINS.try_lock() {
        Ok(registry) => registry.as_ref().is_some_and(|m| m.contains_key(&key)),
        Err(_) => return true,
    };
    if entry_exists {
        return false;
    }
    !retry_ready(key)
        || INITIALIZING
            .lock()
            .expect("initializers")
            .as_ref()
            .is_some_and(|m| m.contains_key(&key))
        || sdl3::video::vulkan::surface_request_pending(window)
}

/// A live swapchain can temporarily defer staging while its presentation
/// lock or a GPU-copy fence is occupied. Unlike missing WSI metadata, these
/// deferred drawables must be retried before their layer's ring fills.
pub(crate) fn window_has_active_swapchain(window: sdl3::Window) -> bool {
    let registry = match SWAPCHAINS.try_lock() {
        Ok(registry) => registry,
        // Do not turn transient contention with teardown into a long delay.
        Err(_) => return true,
    };
    registry
        .as_ref()
        .and_then(|all| all.get(&(window as usize)))
        .is_some_and(|entry| {
            entry.alive.load(Ordering::Acquire)
                && !entry.gpu.lost.load(Ordering::Acquire)
                && entry.present_failure.load(Ordering::Acquire) == 0
        })
}

pub(crate) fn window_presenting_busy(window: sdl3::Window) -> bool {
    let guard = match SWAPCHAINS.try_lock() {
        Ok(guard) => guard,
        Err(_) => return true,
    };
    let entry = guard.as_ref().and_then(|all| all.get(&(window as usize)));
    entry.is_some_and(|entry| entry.presenting.try_lock().is_err())
}

/// Exercise synthetic unowned sources in Vulkan regression tests. Production
/// backing stores use retained leases to preserve their GPU-copy lifetime.
#[cfg(test)]
pub(crate) fn present_nonblocking(
    gpu: &'static Gpu,
    layer: usize,
    frame: &appkit::view::Frame,
    source: &GpuImage,
    holes: &[(usize, Region)],
) -> Result<Option<bool>, String> {
    present_impl(gpu, layer, frame, source, holes, None, None, true)
}

/// Present a command buffer's drawable, consuming its retained `owner` on
/// every return path. A successful copy holds it until the frame fence wakes.
pub(crate) fn present_owned(
    gpu: &'static Gpu,
    layer: usize,
    frame: &appkit::view::Frame,
    source: &GpuImage,
    holes: &[(usize, Region)],
    owner: usize,
) -> Result<Option<bool>, String> {
    present_impl(
        gpu,
        layer,
        frame,
        source,
        holes,
        Some(owner),
        None,
        #[cfg(test)]
        false,
    )
}

/// Retain one immutable backing-store GPU image through its direct WSI copy.
/// The caller's pool never writes an image with outstanding Arc references.
pub(crate) fn present_backing_nonblocking(
    gpu: &'static Gpu,
    layer: usize,
    frame: &appkit::view::Frame,
    source: Arc<BackingImageLease>,
    holes: &[(usize, Region)],
) -> Result<Option<bool>, String> {
    let lease = Arc::clone(&source);
    present_impl(
        gpu,
        layer,
        frame,
        &source.image,
        holes,
        None,
        Some(lease),
        #[cfg(test)]
        false,
    )
}

#[cfg_attr(
    test,
    expect(
        clippy::too_many_arguments,
        reason = "Test-only synthetic-scene flag adds one argument"
    )
)]
fn present_impl(
    gpu: &'static Gpu,
    layer: usize,
    frame: &appkit::view::Frame,
    source: &GpuImage,
    holes: &[(usize, Region)],
    owner: Option<usize>,
    backing: Option<Arc<BackingImageLease>>,
    #[cfg(test)] synthetic_scene: bool,
) -> Result<Option<bool>, String> {
    let mut owner = SourceOwner(owner);
    if gpu.lost.load(Ordering::Acquire) {
        finish_source(owner.take());
        return Ok(Some(true));
    }
    if gpu.swapchain_api().is_none() || crate::vulkan::surface_api().is_none() {
        return Ok(Some(false));
    }
    let window = frame.window as usize;
    // Missing main-thread metadata is transient, not a permanently refused
    // surface. Defer the drawable or backing store until AppKit publishes it.
    if sdl3::video::presentation_window(frame.window).is_none() {
        return Ok(None);
    }
    if REFUSED
        .lock()
        .expect("refused windows")
        .as_ref()
        .is_some_and(|refused| refused.contains_key(&window))
    {
        // A failed SDL Vulkan conversion may only fall back while SDL has
        // not converted the window to Vulkan. Otherwise SDL_GetWindowSurface
        // is invalid even when there is no longer a live VkSurface.
        let converted = sdl3::video::vulkan::owns_window(frame.window);
        if converted {
            finish_source(owner.take());
        }
        return Ok(Some(converted));
    }
    let window_swapchain = {
        // This mutex covers only map operations. Never run Vulkan or SDL
        // initialization while holding it. In particular AppKit may not wait
        // for another window's VkSurface or swapchain creation.
        let guard = match SWAPCHAINS.try_lock() {
            Ok(guard) => guard,
            Err(_) => return Ok(None),
        };
        guard.as_ref().and_then(|all| all.get(&window)).cloned()
    };
    let Some(window_swapchain) = window_swapchain else {
        if !retry_ready(window) {
            return Ok(None);
        }
        let instance = crate::vulkan::instance_handle().ok_or("Vulkan instance unavailable")?;
        // The OpenGL flag was captured on SDL's owner thread. Creating a
        // Vulkan surface would irreversibly replace OpenGL on Wayland.
        if sdl3::video::presentation_window(frame.window)
            .is_some_and(|w| w.flags & sdl3::video::flags::OPENGL != 0)
        {
            REFUSED
                .lock()
                .expect("refused windows")
                .get_or_insert_with(HashMap::new)
                .insert(window, ());
            return Ok(Some(false));
        }
        let (surface, init_token) = match sdl3::video::vulkan::acquire_prepared_surface(
            frame.window,
            instance,
            crate::vulkan::library(),
        ) {
            sdl3::video::vulkan::PreparedSurface::Pending => return Ok(None),
            sdl3::video::vulkan::PreparedSurface::Retired => return Ok(None),
            sdl3::video::vulkan::PreparedSurface::Abandoned => {
                // A terminated initializer cannot be retried. SDL converted
                // windows cannot legally use its CPU surface fallback.
                let converted = sdl3::video::vulkan::owns_window(frame.window);
                REFUSED
                    .lock()
                    .expect("refused windows")
                    .get_or_insert_with(HashMap::new)
                    .insert(window, ());
                if converted {
                    finish_source(owner.take());
                }
                return Ok(Some(converted));
            }
            sdl3::video::vulkan::PreparedSurface::Failed(failure) => {
                let converted = sdl3::video::vulkan::owns_window(frame.window);
                if !converted
                    && !failure.transient
                    && (permanent_sdl_surface_failure(&failure) || !schedule_retry(window, true))
                {
                    gpu::trace(format!(
                        "SDL Vulkan window permanently refused; using CPU fallback: {}",
                        failure.reason
                    ));
                    sdl3::video::vulkan::refuse_failed_surface(frame.window, instance);
                    REFUSED
                        .lock()
                        .expect("refused windows")
                        .get_or_insert_with(HashMap::new)
                        .insert(window, ());
                    return Ok(Some(false));
                }
                if converted || failure.transient {
                    schedule_retry(window, false);
                }
                let _ = sdl3::video::vulkan::retry_surface(
                    frame.window,
                    instance,
                    crate::vulkan::library(),
                );
                return Ok(None);
            }
            sdl3::video::vulkan::PreparedSurface::Ready(surface, token) => (surface, token),
        };
        // The real VkSurface is now owned by this initializer. Do not call
        // SDL while building its Vulkan swapchain or holding the shared map.
        let cancelled = Arc::new(AtomicBool::new(false));
        let accepted = {
            let mut pending = INITIALIZING.lock().expect("initializers");
            let all = pending.get_or_insert_with(HashMap::new);
            if let std::collections::hash_map::Entry::Vacant(e) = all.entry(window) {
                e.insert(Arc::clone(&cancelled));
                true
            } else {
                false
            }
        };
        if !accepted {
            // A concurrent initializer already owns this window. A prepared
            // handle obtained during the race still needs to be retired.
            sdl3::video::vulkan::destroy_surface(instance, surface);
            return Ok(None);
        }
        let extent = vk::Extent2D {
            width: frame.window_width.max(1),
            height: frame.window_height.max(1),
        };
        let cancellation = Arc::clone(&cancelled);
        let spawned = std::thread::Builder::new()
            .name("metal:swapchain-init".into())
            .spawn(move || {
                let cancelled = cancellation;
                let registration = InitializerRegistration {
                    window,
                    token: Arc::clone(&cancelled),
                };
                let _init_token = init_token;
                #[cfg(test)]
                if TEST_SLOW_INITIALIZER.load(Ordering::Acquire) == window {
                    TEST_SLOW_INITIALIZER_STARTED.store(true, Ordering::Release);
                    std::thread::sleep(Duration::from_millis(600));
                }
                if cancelled.load(Ordering::Acquire) {
                    sdl3::video::vulkan::destroy_surface(instance, surface);
                    return;
                }
                if gpu.lost.load(Ordering::Acquire) {
                    // SDL already converted this window to Vulkan. Mark its
                    // taken surface terminal, so the backing-store preflight
                    // will no longer mistake it for an active initializer.
                    sdl3::video::vulkan::abandon_surface(window as sdl3::Window, instance);
                    sdl3::video::vulkan::destroy_surface(instance, surface);
                    return;
                }
                let result =
                    Swapchain::create(gpu, window as sdl3::Window, extent, surface, instance);
                let pending = INITIALIZING.lock().expect("initializers");
                // A destroyed window may have been recreated at the same
                // pointer. Only the exact initializer may publish its result.
                let current = pending
                    .as_ref()
                    .and_then(|all| all.get(&window))
                    .is_some_and(|existing| Arc::ptr_eq(existing, &cancelled));
                if !current || cancelled.load(Ordering::Acquire) {
                    return;
                }
                let mut registered = false;
                let device_lost_on_init = result
                    .as_ref()
                    .err()
                    .is_some_and(|reason| reason.contains("ERROR_DEVICE_LOST"));
                let mut registry = SWAPCHAINS.lock().expect("window swapchains");
                match result {
                    Ok(mut swapchain) => {
                        reset_retry(window);
                        gpu::milestone("window swapchain");
                        if swapchain.corners.is_none() {
                            appkit::decor::presented_opaque(window as sdl3::Window);
                        }
                        let rate = sdl3::video::cached_window_refresh_rate(window as sdl3::Window)
                            .unwrap_or(60.0)
                            .max(30.0);
                        swapchain.compositor_batch_delay =
                            compositor_batch_delay(swapchain.present_mode, rate);
                        registered = true;
                        registry.get_or_insert_with(HashMap::new).insert(
                            window,
                            Arc::new(WindowSwapchain {
                                gpu,
                                presenting: Mutex::new(()),
                                state: Mutex::new(swapchain),
                                pending_scene: Mutex::new(None),
                                wake: Condvar::new(),
                                alive: AtomicBool::new(true),
                                worker_started: AtomicBool::new(false),
                                worker: Mutex::new(None),
                                resize_worker_started: AtomicBool::new(false),
                                resize_worker: Mutex::new(None),
                                present_failure: std::sync::atomic::AtomicU8::new(0),
                                #[cfg(test)]
                                injected_acquire_error: Mutex::new(None),
                            }),
                        );
                    }
                    Err(reason) => {
                        gpu::trace(format!("window swapchain initialization failed: {reason}"));
                        if device_lost_on_init {
                            // Propagate after releasing the global registry:
                            // record_device_loss also needs that mutex.
                        } else if permanent_init_failure(&reason) {
                            REFUSED
                                .lock()
                                .expect("refused windows")
                                .get_or_insert_with(HashMap::new)
                                .insert(window, ());
                        } else {
                            schedule_retry(window, false);
                            // SDL is still responsible for its window's
                            // surface on the video thread, even after many
                            // temporary initialization failures.
                            let _ = sdl3::video::vulkan::retry_surface(
                                window as sdl3::Window,
                                instance,
                                crate::vulkan::library(),
                            );
                        }
                    }
                }
                drop(registry);
                // The earlier AppKit scene publication may have happened
                // before the swapchain existed. Queue an owner-thread replay:
                // it must still pass the compositor's native staging gate.
                if registered {
                    appkit::view::request_metal_scene_replay(window as sdl3::Window);
                }
                if device_lost_on_init {
                    // The failed Swapchain::create already released its
                    // VkSurface. Mark SDL's transferred request terminal
                    // before dropping our initializer generation and
                    // reporting the lost device. The pending mutex still
                    // protects this exact generation against window reuse.
                    sdl3::video::vulkan::abandon_surface(window as sdl3::Window, instance);
                }
                drop(pending);
                // Clear before notifying the device-loss path, not merely
                // when the initializer's closure eventually returns.
                drop(registration);
                if device_lost_on_init {
                    record_device_loss(gpu);
                }
            });
        if let Err(error) = spawned {
            cancelled.store(true, Ordering::Release);
            let mut pending = INITIALIZING.lock().expect("initializers");
            if let Some(all) = pending.as_mut()
                && all
                    .get(&window)
                    .is_some_and(|job| Arc::ptr_eq(job, &cancelled))
            {
                all.remove(&window);
            }
            drop(pending);
            // The closure never executed, so no guard took this VkSurface.
            sdl3::video::vulkan::destroy_surface(instance, surface);
            sdl3::video::vulkan::abandon_surface(frame.window, instance);
            return Err(format!("cannot start swapchain initializer: {error}"));
        }
        return Ok(None);
    };
    // Presentations must reserve frame slots in order, but waiting for an
    // image must not hold the state lock: nextDrawable uses that time to
    // retire completed drawables and release their ring slots.
    let presenting = match window_swapchain.presenting.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => return Ok(None),
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
    };
    let presenting = PresentingGuard {
        held: Some(presenting),
        gpu: window_swapchain.gpu,
    };
    // The SDL window can be torn down after we cloned the entry, before
    // acquiring its per-window lock. Do not touch its retired surface.
    if !window_swapchain.alive.load(Ordering::Acquire) {
        drop(presenting);
        // The caller already transferred this drawable to Submitted. A
        // plain ARC release cannot return its slot to the drawable ring.
        finish_source(owner.take());
        return Ok(Some(true));
    }
    match window_swapchain.present_failure.load(Ordering::Acquire) {
        1 => {
            // SDL_GetWindowSurface is incompatible with a window converted to
            // Vulkan. A lost surface stays unavailable until window teardown;
            // do not incorrectly switch that same window to SDL readback.
            drop(presenting);
            finish_source(owner.take());
            return Ok(Some(true));
        }
        2 => {
            drop(presenting);
            finish_source(owner.take());
            return Ok(Some(true));
        }
        _ => {}
    }
    let mut swapchain = window_swapchain.state.lock().expect("window swapchain");
    let extent = vk::Extent2D {
        width: frame.window_width.max(1),
        height: frame.window_height.max(1),
    };
    // Defer all potentially unbounded VkQueueWaitIdle/vkWaitForFences calls
    // to the window-local worker, including a previously retired swapchain.
    // No new layer is registered before recreation completes.
    if swapchain.pending_rebuild.is_some() || swapchain.retired || swapchain.requested != extent {
        swapchain.pending_rebuild = Some(extent);
        ensure_resize_worker(window, &window_swapchain);
        wake_resize_worker(&window_swapchain);
        return Ok(None);
    }
    // The view hierarchy, rather than drawable submission order, is the
    // authority on what was attached and which layer is in front. Synthetic
    // WSI tests have no NSWindow and retain their explicit registration order.
    // Synthetic WSI fixtures deliberately submit layer IDs that are not
    // associated with an NSView. Guest-owned and backing-store sources always
    // obey authoritative live AppKit membership.
    #[cfg(test)]
    let live_scene = (!synthetic_scene)
        .then(|| appkit::view::metal_scene(frame.window))
        .flatten();
    #[cfg(not(test))]
    let live_scene = appkit::view::metal_scene(frame.window);
    let live_scene = live_scene.map(|scene| {
        scene
            .into_iter()
            .map(|item| ScenePlacement {
                opacity_groups: item.opacity_groups,
                id: item.layer,
                frame: DirectFrame::from_frame(&item.frame),
                opacity: item.opacity,
                opaque: item.opaque,
                clip: item.clip.map(|frame| DirectFrame::from_frame(&frame)),
                rounded: item
                    .rounded
                    .into_iter()
                    .map(|clip| scene::RoundedClip {
                        frame: DirectFrame::from_frame(&clip.frame),
                        radius: clip.radius,
                    })
                    .collect(),
                occluded: Some(
                    item.occluded
                        .into_iter()
                        .map(|r| Region {
                            x: r.x,
                            y: r.y,
                            width: r.width.min(i32::MAX as u32) as i32,
                            height: r.height.min(i32::MAX as u32) as i32,
                        })
                        .collect(),
                ),
                native: native_scene_source(
                    item.native_pixels,
                    item.native_dmabuf,
                    item.native_revision,
                    item.frame.width,
                    item.frame.height,
                    item.native_cpu_capture,
                    item.native_dma_sample,
                ),
            })
            .collect::<Vec<_>>()
    });
    // Only the worker can commit changed native placement metadata. A guest
    // drawable may arrive while the new native revision is still pending DMA
    // import, and must never overwrite the last accepted rollback geometry.
    let drawable_scene = live_scene
        .as_deref()
        .map(|scene| swapchain.guest_present_scene(scene));
    if let Some(scene) = live_scene.as_deref()
        && layer != window
        && !scene.iter().any(|item| item.id == layer)
    {
        // A view removed while its GPU drawable was still queued cannot
        // resurrect itself by a late presentation.
        finish_source(owner.take());
        return Ok(Some(true));
    }
    let joined = !swapchain.layers.contains(&layer);
    if joined {
        swapchain.layers.push(layer);
    }
    if let Some(scene) = drawable_scene.as_deref() {
        swapchain.synchronize_scene(window, scene);
    }
    // Only a layer that has presented here has anything of its own to keep:
    // over any other hole the source is still what the window shows.
    let holes: Vec<Region> = holes
        .iter()
        .filter(|(owner, _)| {
            *owner != layer && (*owner == window || swapchain.layers.contains(owner))
        })
        .map(|(_, region)| *region)
        .collect();
    // In the retained scene the CPU backing store is the bottom layer,
    // including pixels behind translucent Metal. The window store's holes
    // describe Metal coverage, not true occlusion. Conversely, the Metal
    // drawable's holes describe opaque AppKit views above it and must still
    // be respected to keep native controls in front of the guest drawable.
    let regions = if live_scene.is_some() && layer != window {
        swapchain.live_scene_regions(layer, frame)
    } else {
        scene_regions(layer, window, live_scene.is_some(), frame, &holes)
    };
    // Core Animation combines independent layers into one window image.
    // Host WSI presentation mode only determines how that completed image
    // reaches the display; never enqueue one host present per Metal layer.
    if uses_layer_compositor(swapchain.present_mode) {
        // The main event pump refreshes this snapshot when the display or its
        // mode changes. Never call main-thread-only SDL APIs from Metal.
        let rate = sdl3::video::cached_window_refresh_rate(frame.window)
            .unwrap_or(60.0)
            .max(30.0);
        swapchain.compositor_batch_delay = compositor_batch_delay(swapchain.present_mode, rate);
        #[cfg(not(test))]
        ensure_layer_compositor_worker(window, &window_swapchain);
        swapchain.composite_shape = Some(corners::Shape::of(
            frame.window,
            swapchain.extent.width,
            swapchain.extent.height,
        ));
        // A single opaque Metal drawable can be copied straight into the WSI
        // image by the same window worker, without an offscreen full-frame copy.
        // The retained drawable also supplies the initial snapshot if a second
        // layer joins later. Other cases must stage into the GPU accumulator.
        let direct = fast_direct_scene(
            &swapchain,
            layer,
            frame,
            regions.as_deref(),
            owner.0.is_some() || backing.is_some(),
        );
        let converting = !direct && swapchain.direct.is_some();
        let converted = if converting {
            swapchain.stage_direct_snapshot(true)
        } else {
            Ok(None)
        };
        // Keep the submitted drawable's additional retained reference until
        // staging actually transfers it to a GPU fence. Error paths must not
        // release the ring slot before the dispatcher decides to retry.
        let converted_staged = matches!(converted, Ok(Some(true)));
        let mut backing = backing;
        let staged = match converted {
            Ok(_) if direct => {
                swapchain.queue_direct(
                    layer,
                    frame,
                    PresentSource::from(source),
                    &mut owner,
                    backing.take(),
                );
                Ok(Some(true))
            }
            // A busy GPU staging slot cannot yet preserve the previous
            // full-window drawable. Defer the incoming layer rather than
            // presenting an incomplete window image over that drawable.
            Ok(None) if converting => Ok(None),
            Ok(None) | Ok(Some(true)) => {
                let result = swapchain.stage_composite_layer(
                    layer,
                    frame,
                    source,
                    regions.as_deref(),
                    &mut owner,
                    true,
                );
                if matches!(result, Ok(Some(true))) {
                    // A backing-store source must also survive its staging
                    // copy when this window already has multiple layers.
                    let slot = swapchain.frame.wrapping_sub(1) % swapchain.frames.len();
                    debug_assert!(swapchain.frames[slot].backing.is_none());
                    swapchain.frames[slot].backing = backing.take();
                }
                result
            }
            Ok(Some(false)) => unreachable!(),
            Err(error) => Err(error),
        };
        let typed_device_loss =
            swapchain.last_wsi_error.take() == Some(vk::Result::ERROR_DEVICE_LOST);
        if matches!(staged, Ok(Some(true))) {
            swapchain.note_scene_frame(layer, frame);
            swapchain.scene_repaint_needed = false;
        }
        let refresh_store = composite_first_stage_needs_store(joined, &staged, layer, window);
        if !matches!(staged, Ok(Some(true))) && joined {
            // A saved background snapshot does not mean the new layer's own
            // frame was staged: keep the layer unknown until that succeeds.
            swapchain.layers.retain(|&known| known != layer);
        }
        if matches!(staged, Ok(Some(true))) || converted_staged {
            wake_compositor_worker(&window_swapchain);
        }
        drop(swapchain);
        drop(presenting);
        if refresh_store {
            // A deferred or failed first frame has not changed the Vulkan
            // accumulator; it must not dirty the AppKit CPU backing store.
            appkit::backing::present_again(frame.window);
        }
        return match staged {
            Ok(staged) => Ok(staged),
            Err(reason) if sdl3::video::vulkan::owns_window(frame.window) => {
                gpu::trace(format!("converted layer staging failed: {reason}"));
                if typed_device_loss || terminal_composite_staging_failure(&reason) {
                    if typed_device_loss || reason.contains("ERROR_DEVICE_LOST") {
                        record_device_loss(gpu);
                    } else {
                        window_swapchain.present_failure.store(2, Ordering::Release);
                        wake_compositor_worker(&window_swapchain);
                    }
                    finish_source(owner.take());
                    Ok(Some(true))
                } else {
                    // The presentation queue still owns its original ARC
                    // reference and can retry when the frame slot recovers.
                    Ok(None)
                }
            }
            Err(reason) => Err(reason),
        };
    }
    Err(format!(
        "unsupported Vulkan window presentation mode {:?}",
        swapchain.present_mode
    ))
}

/// The CPU backing-store source is below the Metal layer tree. Its
/// rectangles labelled as Metal holes must not erase pixels that a
/// translucent drawable needs to reveal. Metal's own masks still clip opaque
/// AppKit controls that are actually above the drawable.
fn scene_regions(
    layer: usize,
    window: usize,
    retained: bool,
    frame: &appkit::view::Frame,
    holes: &[Region],
) -> Option<Vec<Region>> {
    if retained && layer == window {
        return None;
    }
    if holes.is_empty() {
        return None;
    }
    Some(subtract(
        Region {
            x: frame.x,
            y: frame.y,
            width: frame.width as i32,
            height: frame.height as i32,
        },
        holes,
    ))
}

/// Some Gamescope WSI versions omit a present-fence signal when
/// vkQueuePresentKHR fails. Only trust the fence after a successful result
/// in that opt-in WSI path. Other drivers still enqueue ordinary OUT_OF_DATE
/// and SURFACE_LOST operations; device loss and OOM cannot be assumed queued.
/// Mirrors DXVK Presenter::presentImage's fence-result classification.
fn present_fence_will_signal(result: &Result<bool, vk::Result>, gamescope_wsi: bool) -> bool {
    if gamescope_wsi {
        return result.is_ok();
    }
    !matches!(
        result,
        Err(vk::Result::ERROR_DEVICE_LOST
            | vk::Result::ERROR_OUT_OF_HOST_MEMORY
            | vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)
    )
}

/// Core Animation composites Metal layers into a window independently of
/// when the host display scans out the latest completed composition.
/// All supported WSI modes share that compositing path.
fn uses_layer_compositor(mode: vk::PresentModeKHR) -> bool {
    matches!(
        mode,
        vk::PresentModeKHR::FIFO
            | vk::PresentModeKHR::FIFO_RELAXED
            | vk::PresentModeKHR::MAILBOX
            | vk::PresentModeKHR::IMMEDIATE
    )
}

/// Bound source-copy work independently of WSI's own image count. A guest
/// drawing multiple layers per frame should not wait for each WSI image.
fn frame_slots(_mode: vk::PresentModeKHR) -> usize {
    FRAMES_IN_FLIGHT
}

/// In immediate mode the compositor publishes promptly, while FIFO's longer
/// window batch gathers a full refresh's independent layer changes. Neither
/// timer changes the guest's command-buffer ordering or source completion.
fn compositor_batch_delay(mode: vk::PresentModeKHR, rate: f32) -> Duration {
    let rate = rate.max(30.0);
    if matches!(
        mode,
        vk::PresentModeKHR::IMMEDIATE | vk::PresentModeKHR::MAILBOX
    ) {
        Duration::from_secs_f32((0.35 / rate).min(0.002))
    } else {
        Duration::from_secs_f32((0.65 / rate).min(0.004))
    }
}

/// Only a full-window opaque drawable with a retained lifetime may bypass
/// offscreen composition. FIFO retains its previous batching behavior.
/// A window with the corners pass still goes through its composite at
/// present time; a direct layer only skips the accumulator stage.
fn fast_direct_layer(
    mode: vk::PresentModeKHR,
    layers: usize,
    frame: &appkit::view::Frame,
    regions: Option<&[Region]>,
    owned: bool,
) -> bool {
    matches!(
        mode,
        vk::PresentModeKHR::IMMEDIATE | vk::PresentModeKHR::MAILBOX
    ) && layers == 1
        && frame.covers_window()
        && regions.is_none()
        && owned
}

/// How many layers presenting `layer` directly would have to account for.
/// The window's CPU store lies below every Metal layer, so it cannot show
/// through a drawable that covers the window with no regions cut out of it.
fn composited_layers(swapchain: &Swapchain, layer: usize) -> usize {
    swapchain
        .layers
        .iter()
        .filter(|&&known| layer == swapchain.window || known != swapchain.window)
        .count()
}

/// Effective CA state is a hard eligibility requirement for bypassing the
/// scene shader; rectangular and rounded masks must both be considered.
fn fast_direct_scene(
    swapchain: &Swapchain,
    layer: usize,
    frame: &appkit::view::Frame,
    regions: Option<&[Region]>,
    owned: bool,
) -> bool {
    fast_direct_layer(
        swapchain.present_mode,
        composited_layers(swapchain, layer),
        frame,
        regions,
        owned,
    ) && !appkit::view::metal_scene_pending()
        && !swapchain
            .scene_placements
            .iter()
            .any(|p| p.id != layer && p.native.is_some())
        && swapchain.scene_opacity(layer) >= 1.0
        && !swapchain.scene_has_opacity_groups(layer)
        && swapchain.scene_opaque(layer)
        && swapchain.scene_rounded(layer).is_empty()
        && swapchain
            .scene_placements
            .iter()
            .find(|placement| placement.id == layer)
            .and_then(|placement| placement.clip)
            .is_none_or(|clip| scene::scene_clip_contains(&clip.frame(), frame))
}

/// Direct opaque presents have nothing to coalesce, so they do not wait for
/// the multi-layer Core Animation batching heuristic.
fn pending_batch_delay(swapchain: &Swapchain) -> Duration {
    if swapchain.direct.is_some() {
        Duration::ZERO
    } else {
        swapchain.compositor_batch_delay
    }
}

// A static window must not permanently reserve one of a CAMetalLayer's three
// drawable slots. Only idle windows snapshot once; active direct frames never
// pay the extra full-frame GPU transfer.
const DIRECT_IDLE_SNAPSHOT_DELAY: Duration = Duration::from_millis(100);

/// One presentation worker per window prevents an occluded window from
/// stalling other windows and decouples Metal drawables from WSI acquisitions.
fn ensure_layer_compositor_worker(window: usize, entry: &Arc<WindowSwapchain>) {
    if entry.worker_started.swap(true, Ordering::AcqRel) {
        return;
    }
    let worker_entry = Arc::clone(entry);
    let handle = std::thread::Builder::new()
        .name("rosemary-layer-present".into())
        .spawn(move || layer_compositor_worker(window, worker_entry))
        .expect("layer presenter thread");
    // Started under the window presentation mutex, so forget_window cannot
    // run between launching the worker and recording its join handle.
    *entry.worker.lock().expect("layer compositor worker") = Some(handle);
}

/// A retry must not overwrite a newer scene published during GPU recording.
fn requeue_scene(pending: &Mutex<Option<Vec<ScenePlacement>>>, consumed: Vec<ScenePlacement>) {
    pending
        .lock()
        .expect("AppKit scene pending")
        .get_or_insert(consumed);
}

/// A DMA-BUF-only native view needs a transparent source below its imported
/// image. Cap the pixel allocation so an invalid/oversized guest geometry can
/// never allocate unbounded process memory on the event thread.
fn native_scene_source(
    pixels: Option<Arc<Vec<u8>>>,
    dmabuf: Option<Arc<appkit::backing::Dmabuf>>,
    revision: u64,
    width: u32,
    height: u32,
    cpu_capture: Option<appkit::backing::NativeCpuCapture>,
    dma_sample: Option<(u32, u32, u32, u32)>,
) -> Option<scene::NativeCpuSceneSource> {
    let supported = if pixels.is_some() {
        appkit::backing::native_scene_extent_supported(width, height)
    } else if dmabuf.is_some() {
        appkit::backing::native_scene_dma_extent_supported(width, height)
    } else {
        false
    };
    if !supported {
        return None;
    }
    if let Some((full_width, full_height, crop_x, crop_y)) = dma_sample
        && (dmabuf.is_none()
            || !appkit::backing::native_scene_dma_extent_supported(full_width, full_height)
            || crop_x.checked_add(width).is_none_or(|x| x > full_width)
            || crop_y.checked_add(height).is_none_or(|y| y > full_height))
    {
        return None;
    }
    let bytes = u64::from(width) * u64::from(height) * 4;
    let pixels = match (pixels, dmabuf.as_ref()) {
        (Some(pixels), _) if pixels.len() as u64 == bytes => pixels,
        // An empty byte slice means an entirely transparent native CPU base.
        // The GPU clears the staging target; no full-resolution zero-filled
        // CPU image is needed for a DMA-BUF-only overlay.
        (None, Some(_)) => Arc::new(Vec::new()),
        _ => return None,
    };
    Some(scene::NativeCpuSceneSource {
        bytes: pixels,
        revision,
        cpu_capture,
        dmabuf,
        dma_sample: dma_sample.map(
            |(full_width, full_height, crop_x, crop_y)| scene::DmaSample {
                full_width,
                full_height,
                crop_x,
                crop_y,
            },
        ),
    })
}

/// Upload native AppKit pixels exclusively on the window's worker, then copy
/// to the same fence-retained scene snapshot used for Metal drawables. AppKit
/// publishes immutable bytes but never blocks on a Vulkan queue.
#[cfg(test)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
static TEST_BLOCK_LINEAR_DMA_FALLBACK: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static TEST_NATIVE_RETRYABLE_FAILURES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
static NATIVE_IMAGE_UPLOAD_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static NATIVE_HARD_BUDGET_REFUSALS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// When the Vulkan importer refuses a *linear* DRM image, map its actual
/// producer allocation under the kernel's DMA_BUF_SYNC read protocol. Only
/// known 32-bit, premultiplied formats with one plane and unscaled extents
/// qualify. Never replace an unreadable DMA-BUF with transparent pixels.
#[cfg(target_os = "linux")]
fn linear_dmabuf_cpu_fallback(layer: &ScenePlacement) -> Option<ScenePlacement> {
    use std::os::fd::AsRawFd;
    #[cfg(test)]
    if TEST_BLOCK_LINEAR_DMA_FALLBACK.load(Ordering::Acquire) {
        return None;
    }
    let native = layer.native.as_ref()?;
    let picture = native.dmabuf.as_ref()?;
    let frame = layer.frame;
    let sample = native.dma_sample.unwrap_or(scene::DmaSample {
        full_width: frame.width,
        full_height: frame.height,
        crop_x: 0,
        crop_y: 0,
    });
    if picture.modifier != 0
        || picture.planes.len() != 1
        || picture.width != sample.full_width
        || picture.height != sample.full_height
    {
        return None;
    }
    let format = picture.fourcc;
    let ar24 = format == u32::from_le_bytes(*b"AR24");
    let xr24 = format == u32::from_le_bytes(*b"XR24");
    let ab24 = format == u32::from_le_bytes(*b"AB24");
    let xb24 = format == u32::from_le_bytes(*b"XB24");
    if !(ar24 || xr24 || ab24 || xb24) {
        return None;
    }
    let stride = u64::from(picture.planes[0].stride);
    let source_bytes = u64::from(sample.full_width).checked_mul(4)?;
    if stride < source_bytes {
        return None;
    }
    let full_bytes = u64::from(frame.width)
        .checked_mul(4)?
        .checked_mul(u64::from(frame.height))?;
    if !native.bytes.is_empty() && native.bytes.len() as u64 != full_bytes {
        return None;
    }
    // Restrict the transfer to the actual intersection of this view's frame,
    // the window and an intermediate rectangular CA mask. No 6K CPU copy is
    // created when the window only exposes a few hundred pixels.
    let mut left = i64::from(frame.x).max(0);
    let mut top = i64::from(frame.y).max(0);
    let mut right =
        (i64::from(frame.x) + i64::from(frame.width)).min(i64::from(frame.window_width));
    let mut bottom =
        (i64::from(frame.y) + i64::from(frame.height)).min(i64::from(frame.window_height));
    if let Some(clip) = layer.clip {
        left = left.max(i64::from(clip.x));
        top = top.max(i64::from(clip.y));
        right = right.min(i64::from(clip.x) + i64::from(clip.width));
        bottom = bottom.min(i64::from(clip.y) + i64::from(clip.height));
    }
    if left >= right || top >= bottom {
        return None;
    }
    let width = u32::try_from(right - left).ok()?;
    let height = u32::try_from(bottom - top).ok()?;
    if !appkit::backing::native_scene_extent_supported(width, height) {
        return None;
    }
    let local_x = usize::try_from(left - i64::from(frame.x)).ok()?;
    let local_y = usize::try_from(top - i64::from(frame.y)).ok()?;
    let x = usize::try_from(sample.crop_x).ok()?.checked_add(local_x)?;
    let y = usize::try_from(sample.crop_y).ok()?.checked_add(local_y)?;
    let width_usize = width as usize;
    let height_usize = height as usize;
    let fd = picture.planes[0].fd.as_raw_fd();
    let offset = u64::from(picture.planes[0].offset);
    let end = offset
        .checked_add(stride.checked_mul(u64::try_from(y + height_usize - 1).ok()?)?)?
        .checked_add(u64::try_from(x.checked_add(width_usize)?.checked_mul(4)?).ok()?)?;
    let length = usize::try_from(end).ok()?;
    if length == 0 || length > isize::MAX as usize {
        return None;
    }
    // SAFETY: Fstat validates a descriptor only, not a guest memory pointer.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0
        || stat.st_size <= 0
        || u64::try_from(stat.st_size).ok()? < end
    {
        return None;
    }
    // Compute and bounds-check every row before acquiring a mapping or a
    // producer read fence. No fallible indexing path may return without
    // ending DMA_BUF_SYNC or unmapping the descriptor.
    let offset_usize = usize::try_from(offset).ok()?;
    let stride_usize = usize::try_from(stride).ok()?;
    let first_row = y
        .checked_mul(stride_usize)?
        .checked_add(offset_usize)?
        .checked_add(x.checked_mul(4)?)?;
    let row_size = width_usize.checked_mul(4)?;
    let output_size = row_size.checked_mul(height_usize)?;
    let last_end = first_row
        .checked_add((height_usize - 1).checked_mul(stride_usize)?)?
        .checked_add(row_size)?;
    if last_end > length {
        return None;
    }
    // Map only the used rows, not the entire producer allocation from byte
    // zero. A large offscreen NSView can expose a tiny far-offset viewport.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return None;
    }
    let page = usize::try_from(page).ok()?;
    let map_start = first_row - first_row % page;
    let map_size = last_end.checked_sub(map_start)?;
    if map_size == 0 || map_size > 256 * 1024 * 1024 {
        return None;
    }
    let map_offset = libc::off_t::try_from(map_start).ok()?;
    let base_count = if native.bytes.is_empty() {
        0
    } else {
        native.bytes.len()
    };
    if base_count != 0 {
        let last_source = (local_y + height_usize - 1)
            .checked_mul(usize::try_from(frame.width).ok()?)?
            .checked_add(local_x)?
            .checked_add(width_usize)?
            .checked_mul(4)?;
        if last_source > base_count {
            return None;
        }
    }
    // SAFETY: one read-only shared mapping of the same live OwnedFd. The
    // backing Dmabuf Arc pins the descriptor and fstat bounds every row.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            map_size,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            map_offset,
        )
    };
    if mapping == libc::MAP_FAILED {
        return None;
    }
    // Linux UAPI: DMA_BUF_IOCTL_SYNC = _IOW('b',0,struct dma_buf_sync).
    const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
    let mut sync_start: u64 = 1; // DMA_BUF_SYNC_READ | DMA_BUF_SYNC_START.
    let started = unsafe { libc::ioctl(fd, DMA_BUF_IOCTL_SYNC, &mut sync_start) } == 0;
    // The synthetic fixture uses a sealed memfd. In a real (non-test) build
    // only DMA_BUF_IOCTL_SYNC-supported producer buffers qualify.
    #[cfg(test)]
    let synthetic_memfd = !started && unsafe { libc::fcntl(fd, libc::F_GET_SEALS) } >= 0;
    #[cfg(not(test))]
    let synthetic_memfd = false;
    if !started && !synthetic_memfd {
        unsafe { libc::munmap(mapping, map_size) };
        return None;
    }
    let mut pixels = vec![0u8; output_size];
    // SAFETY: prevalidated `first_row..last_end` is inside the mapping.
    let bytes = unsafe { std::slice::from_raw_parts(mapping.cast::<u8>(), map_size) };
    for row in 0..height_usize {
        let pos = first_row - map_start + row * stride_usize;
        for col in 0..width_usize {
            let base = pos + col * 4;
            let src = &bytes[base..base + 4];
            let (b, g, r) = if ar24 || xr24 {
                (src[0], src[1], src[2])
            } else {
                (src[2], src[1], src[0])
            };
            let alpha = if xr24 || xb24 { 255u8 } else { src[3] };
            let dst = (row * width_usize + col) * 4;
            let result = &mut pixels[dst..dst + 4];
            result.copy_from_slice(&[b, g, r, alpha]);
            if !native.bytes.is_empty() && alpha < 255 {
                let original =
                    ((local_y + row) * usize::try_from(frame.width).ok()? + local_x + col) * 4;
                let beneath = &native.bytes[original..original + 4];
                let keep = u32::from(255 - alpha);
                for component in 0..4 {
                    result[component] = (u32::from(result[component])
                        + u32::from(beneath[component]) * keep / 255)
                        .min(255) as u8;
                }
            }
        }
    }
    let ended = if started {
        let mut sync_end: u64 = 5; // DMA_BUF_SYNC_READ | DMA_BUF_SYNC_END.
        (unsafe { libc::ioctl(fd, DMA_BUF_IOCTL_SYNC, &mut sync_end) }) == 0
    } else {
        true
    };
    unsafe { libc::munmap(mapping, map_size) };
    if !ended {
        return None;
    }
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    native.revision.hash(&mut hash);
    (left, top, width, height, 0x4c494e45u32).hash(&mut hash);
    let mut replacement = layer.clone();
    replacement.frame.x = left as i32;
    replacement.frame.y = top as i32;
    replacement.frame.width = width;
    replacement.frame.height = height;
    let replacement_source = replacement.native.as_mut().expect("present above");
    replacement_source.bytes = Arc::new(pixels);
    replacement_source.revision = hash.finish();
    replacement_source.dmabuf = None;
    replacement_source.dma_sample = None;
    Some(replacement)
}

#[cfg(not(target_os = "linux"))]
fn linear_dmabuf_cpu_fallback(_layer: &ScenePlacement) -> Option<ScenePlacement> {
    None
}

/// A native snapshot is reusable only if it contains the exact GPU texel
/// generation described by the proposed native publication. Frame movement
/// alone is composited from the existing image and does not retire its lease.
fn native_image_matches_placement(image: &scene::SceneLayer, proposed: &ScenePlacement) -> bool {
    let Some(native) = proposed.native.as_ref() else {
        return false;
    };
    image.id == proposed.id
        && image.ready
        && image.native_revision == Some(native.revision)
        && image.native_dma_sample == native.dma_sample
        && image.snapshot.image.width == proposed.frame.width
        && image.snapshot.image.height == proposed.frame.height
}

// The producer attests to each changed rectangle relative to an exact CPU
// capture identity. No arbitrary changed Arc is accepted as dirty evidence.
fn cached_native_cpu_base<'a>(
    state: &'a Swapchain,
    layer: &ScenePlacement,
) -> Option<(&'a NativeCpuBaseCache, Option<appkit::backing::Rect>)> {
    let native = layer.native.as_ref()?;
    if native.dmabuf.is_none() || native.bytes.is_empty() {
        return None;
    }
    let base = state.native_cpu_bases.iter().find(|base| {
        base.id == layer.id
            && base.image.image.width == layer.frame.width
            && base.image.image.height == layer.frame.height
    })?;
    if base.source.ptr_eq(&Arc::downgrade(&native.bytes)) {
        return Some((base, None));
    }
    let capture = native.cpu_capture?;
    let proof = capture.damage?;
    if base.cpu_revision != Some(proof.from_revision) || Arc::strong_count(&base.image) != 1 {
        return None;
    }
    let rect = proof.rect;
    if rect.x < 0
        || rect.y < 0
        || rect.width <= 0
        || rect.height <= 0
        || rect.x.checked_add(rect.width)? > i64::from(layer.frame.width)
        || rect.y.checked_add(rect.height)? > i64::from(layer.frame.height)
    {
        return None;
    }
    let rect_bytes = (rect.width as u64)
        .checked_mul(rect.height as u64)?
        .checked_mul(4)?;
    // Large regions use the original independent image path. Never assume a
    // partial transfer is cheaper or trade scene lifetime for an upload.
    (rect_bytes < native.bytes.len() as u64 / 2).then_some((base, Some(rect)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StagedNativeIdentity {
    revision: u64,
    dma_sample: Option<scene::DmaSample>,
}

impl StagedNativeIdentity {
    fn from_placement(layer: &ScenePlacement) -> Option<Self> {
        layer.native.as_ref().map(|native| Self {
            revision: native.revision,
            dma_sample: native.dma_sample,
        })
    }
}

// Existing direct staging users need only completion. The compositor worker
// additionally needs the identity of the *effective* bytes after CPU fallback.
#[cfg(test)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn stage_native_scene_source(
    state: &mut Swapchain,
    layer: &ScenePlacement,
) -> Result<bool, String> {
    stage_native_scene_source_effective(state, layer).map(|identity| identity.is_some())
}

fn stage_native_scene_source_effective(
    state: &mut Swapchain,
    layer: &ScenePlacement,
) -> Result<Option<StagedNativeIdentity>, String> {
    let Some(native) = layer.native.as_ref() else {
        return Ok(None);
    };
    state
        .unsupported_native_dma
        .retain(|item| item.producer.strong_count() > 0);
    if let Some(picture) = &native.dmabuf
        && state.unsupported_native_dma.iter().any(|item| {
            item.id == layer.id
                && item.revision == native.revision
                && item.producer.ptr_eq(&Arc::downgrade(picture))
        })
    {
        // Cache only the unsupported GPU representation. Placement and
        // opacity changes still require recompositing exact CPU pixels.
        if let Some(cpu) = linear_dmabuf_cpu_fallback(layer) {
            return stage_native_scene_source_effective(state, &cpu);
        }
        return Err("native-dmabuf-import-refused".into());
    }
    if let Some(picture) = &native.dmabuf
        && appkit::backing::dmabuf_producer_refused(
            layer.frame.window as sdl3::Window,
            layer.id,
            picture,
        )
    {
        if let Some(cpu) = linear_dmabuf_cpu_fallback(layer) {
            return stage_native_scene_source_effective(state, &cpu);
        }
        return Err("native-dmabuf-import-refused".into());
    }
    // A direct WSI source is not a retained scene image. Materialize it
    // *before* recording the newly appearing native layer, or the latter's
    // transparent regions will permanently erase the still-static Metal
    // drawable. A busy frame must defer the entire scene transaction.
    if state.direct.is_some() && state.stage_direct_snapshot(true)? != Some(true) {
        return Ok(None);
    }
    if state
        .scene_layers
        .iter()
        .any(|image| native_image_matches_placement(image, layer))
    {
        return Ok(StagedNativeIdentity::from_placement(layer));
    }
    let frame = layer.frame.frame();
    if frame.width == 0
        || frame.height == 0
        || u64::from(frame.width)
            .saturating_mul(u64::from(frame.height))
            .saturating_mul(4)
            != native.bytes.len() as u64
            && !(native.bytes.is_empty() && native.dmabuf.is_some())
    {
        return Err("invalid native AppKit scene pixel extent".into());
    }
    // Native staging can simultaneously hold a transparent/CPU base, a DMA
    // blend output, a CPU upload buffer and a newly retained scene image.
    // Reserve all of them before creating any Vulkan allocation. The 512 MiB
    // per-window cap is checked against existing scene images and in-flight
    // frame leases, so resize churn cannot silently bypass the budget.
    state.retire_copied();
    let image_bytes = u64::from(frame.width)
        .saturating_mul(u64::from(frame.height))
        .saturating_mul(4);
    // Distinct published native revisions may share the same immutable CPU
    // base, but pointer identity never substitutes for revision admission.
    // A producer may retire its old Arc after capturing new pixels. The
    // exact CPU capture revision still authenticates its retained GPU base.
    state
        .native_cpu_bases
        .retain(|item| item.source.strong_count() > 0 || item.cpu_revision.is_some());
    let mut patch = cached_native_cpu_base(state, layer).and_then(|(_, rect)| rect);
    let mut reuse_base = cached_native_cpu_base(state, layer).is_some();
    // One retained native source (the completed CPU base or DMA blend
    // result), one optional DMA output and one optional full CPU upload.
    // No second retained-scene blit allocation is necessary.
    let full_scratch_count =
        1 + u64::from(native.dmabuf.is_some()) + u64::from(!native.bytes.is_empty());
    let full_scratch_bytes = image_bytes.saturating_mul(full_scratch_count);
    let mut scratch_bytes = if reuse_base {
        image_bytes.saturating_add(patch.map_or(0, |rect| {
            u64::try_from(rect.width)
                .unwrap_or(0)
                .saturating_mul(u64::try_from(rect.height).unwrap_or(0))
                .saturating_mul(4)
        }))
    } else {
        full_scratch_bytes
    };
    if !scene::fits_scene_budget(state.budgeted_scene_bytes(), scratch_bytes) {
        // Optional bases are freed alongside other spares. Re-evaluate
        // admission against the full upload requirement before allocating:
        // otherwise a missing base could be used as uninitialized pixels.
        state.shed_scene_spares();
        reuse_base = false;
        patch = None;
        scratch_bytes = full_scratch_bytes;
        if !scene::fits_scene_budget(state.budgeted_scene_bytes(), scratch_bytes) {
            // Fence-held retired leases may clear shortly. A committed
            // snapshot and its accumulator cannot disappear without new
            // scene membership. Distinguish those cases so a permanently
            // impossible native revision never spins the GPU presentation
            // worker in a 10-ms retry loop.
            if scene::fits_scene_budget(state.projected_committed_scene_bytes(), scratch_bytes) {
                return Ok(None);
            }
            // Only a committed, non-evictable footprint is a hard failure.
            // A temporarily pinned generation must never poison importer
            // capability for the next attempt after GPU fence retirement.
            return Err("native AppKit GPU scene budget exhausted".into());
        }
    }
    // Unlike an impossible committed-scene footprint, transient backing
    // and fence-retained window allocations may retire without a scene edit.
    // Defer these without allocating or permanently rejecting this revision.
    if !window_gpu_capacity_available(frame.window as usize, scratch_bytes) {
        // A cached CPU base is optional. Shed it (and every other cache
        // entry) before declaring the physical window ledger exhausted.
        state.native_cpu_bases.clear();
        reuse_base = false;
        patch = None;
        scratch_bytes = full_scratch_bytes;
    }
    if !window_gpu_capacity_available(frame.window as usize, scratch_bytes) {
        // Fence-held allocations may disappear on retry. Rollback-pinned
        // *committed* images cannot: they need a different atomic transition
        // strategy, not a perpetual 10-ms poll. Do not disable DMA importing.
        if state.atomic_transition_gpu_floor()
            > WINDOW_GPU_PAYLOAD_BUDGET.saturating_sub(scratch_bytes)
        {
            return Err("native AppKit atomic GPU transition peak exhausted".into());
        }
        return Ok(None);
    }
    #[cfg(test)]
    NATIVE_IMAGE_UPLOAD_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let gpu = state.gpu;
    let reused_image = if reuse_base {
        let image = Arc::clone(
            &cached_native_cpu_base(state, layer)
                .expect("base lease was validated before staging")
                .0
                .image,
        );
        if patch.is_some() {
            // A failed GPU wait makes the patched pixels uncertain. Drop the
            // cache authority before submission; rollback owns an independent
            // last-admitted DMA-blended scene image.
            state.native_cpu_bases.retain(|base| base.id != layer.id);
        }
        Some(image)
    } else {
        None
    };
    let lease = if let Some(image) = reused_image {
        image
    } else {
        BackingImageLease::create_for_window(
            gpu,
            frame.window as usize,
            &ImageDescription {
                image_type: vk::ImageType::TYPE_2D,
                view_type: vk::ImageViewType::TYPE_2D,
                format: vk::Format::B8G8R8A8_UNORM,
                width: frame.width,
                height: frame.height,
                depth: 1,
                levels: 1,
                layers: 1,
                samples: 1,
                usage: gpu::USAGE_RENDER_TARGET | gpu::USAGE_SHADER_READ,
                cube: false,
                components: vk::ComponentMapping::default(),
            },
        )?
    };
    // DMA-backed native tiles initialize their CPU base in the same GPU
    // command buffer as the producer blend. CPU-only sources use the existing
    // upload path; no buffer may be released before its submit fence signals.
    if native.dmabuf.is_none() {
        if native.bytes.is_empty() {
            gpu.one_shot_owned(Arc::clone(&lease), |device, cb, _source| unsafe {
                device.cmd_clear_color_image(
                    cb,
                    lease.image.image,
                    vk::ImageLayout::GENERAL,
                    &vk::ClearColorValue {
                        float32: [0.0, 0.0, 0.0, 0.0],
                    },
                    &[vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    }],
                );
            })?;
        } else {
            let staging_charge =
                GpuCharge::reserve(frame.window as usize, native.bytes.len() as u64)?;
            let staging = gpu.create_staging_buffer(native.bytes.len() as u64)?;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    native.bytes.as_ptr(),
                    staging.contents,
                    native.bytes.len(),
                )
            };
            let owned = NativeStageGpuResources {
                gpu,
                buffer: Some(staging),
                _charge: Some(staging_charge),
                _source: Arc::clone(&lease),
                _target: None,
            };
            gpu.one_shot_owned(owned, |device, cb, owned| unsafe {
                device.cmd_copy_buffer_to_image(
                    cb,
                    owned.buffer.as_ref().unwrap().buffer,
                    lease.image.image,
                    vk::ImageLayout::GENERAL,
                    &[vk::BufferImageCopy::default()
                        .image_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            mip_level: 0,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .image_extent(vk::Extent3D {
                            width: frame.width,
                            height: frame.height,
                            depth: 1,
                        })],
                );
            })?;
        }
    }
    // The existing DMA-BUF importer already validates external memory and
    // performs acquire/release barriers. Render onto a transparent (or CPU
    // backed) native source *before* staging that source as an ordered scene
    // layer. A failed import cannot be silently treated as transparent.
    let blended = if let Some(picture) = native.dmabuf.as_ref() {
        let mut description = crate::overlay::target_description(frame.width, frame.height);
        description.usage |= gpu::USAGE_SHADER_READ;
        let output =
            BackingImageLease::create_for_window(gpu, frame.window as usize, &description)?;
        let overlay = [appkit::backing::GpuOverlay {
            picture: Arc::clone(picture),
            view: layer.id & !(1usize << (usize::BITS - 1)),
            rect: appkit::backing::Rect {
                x: -i64::from(native.dma_sample.map_or(0, |sample| sample.crop_x)),
                y: -i64::from(native.dma_sample.map_or(0, |sample| sample.crop_y)),
                width: i64::from(
                    native
                        .dma_sample
                        .map_or(frame.width, |sample| sample.full_width),
                ),
                height: i64::from(
                    native
                        .dma_sample
                        .map_or(frame.height, |sample| sample.full_height),
                ),
            },
        }];
        let mut staging_charge = None;
        let upload_bytes = if let Some(rect) = patch {
            (rect.width * rect.height * 4) as u64
        } else if reuse_base {
            0
        } else {
            native.bytes.len() as u64
        };
        let staging = if upload_bytes == 0 {
            None
        } else {
            staging_charge = Some(GpuCharge::reserve(frame.window as usize, upload_bytes)?);
            let staging = gpu.create_staging_buffer(upload_bytes)?;
            unsafe {
                if let Some(rect) = patch {
                    let dst_stride = rect.width as usize * 4;
                    let src_stride = frame.width as usize * 4;
                    for row in 0..rect.height as usize {
                        let src = (rect.y as usize + row) * src_stride + rect.x as usize * 4;
                        std::ptr::copy_nonoverlapping(
                            native.bytes.as_ptr().add(src),
                            staging.contents.add(row * dst_stride),
                            dst_stride,
                        );
                    }
                } else {
                    std::ptr::copy_nonoverlapping(
                        native.bytes.as_ptr(),
                        staging.contents,
                        native.bytes.len(),
                    );
                }
            }
            Some(staging)
        };
        let init = if let Some(rect) = patch {
            Some(crate::overlay::StoreInit::UploadRegion {
                buffer: staging.as_ref().unwrap().buffer,
                x: rect.x as u32,
                y: rect.y as u32,
                width: rect.width as u32,
                height: rect.height as u32,
            })
        } else if reuse_base {
            None
        } else {
            Some(
                staging
                    .as_ref()
                    .map_or(crate::overlay::StoreInit::Transparent, |buffer| {
                        crate::overlay::StoreInit::Upload(buffer.buffer)
                    }),
            )
        };
        // blend_with_store_init waits for this submission fence. The staging
        // allocation and its window-wide budget reservation stay live until
        // the GPU is done copying the CPU base and sampling imported DMA.
        let owned = NativeStageGpuResources {
            gpu,
            buffer: staging,
            _charge: staging_charge,
            _source: Arc::clone(&lease),
            _target: Some(Arc::clone(&output)),
        };
        let blended_result = crate::overlay::blend_with_store_init_owned(
            gpu,
            &lease.image,
            &output.image,
            &overlay,
            init,
            true,
            owned,
        );
        if let Some(failure) = blended_result?.into_iter().next() {
            if failure.kind == crate::overlay::ImportFailureKind::UnsupportedProducer {
                const MAX_UNSUPPORTED_SOURCES: usize = 64;
                state
                    .unsupported_native_dma
                    .retain(|item| item.id != layer.id);
                if state.unsupported_native_dma.len() == MAX_UNSUPPORTED_SOURCES {
                    state.unsupported_native_dma.remove(0);
                }
                state.unsupported_native_dma.push(UnsupportedNativeDma {
                    id: layer.id,
                    revision: native.revision,
                    producer: Arc::downgrade(native.dmabuf.as_ref().unwrap()),
                });
            }
            // Static device/format incompatibilities are isolated to this
            // producer. Transient Vulkan/descriptor failures may recover on
            // the next revision; neither may disable imports for the window.
            gpu::trace(format!(
                "native dma import {:?}, view {}: {}",
                failure.kind, failure.producer_view, failure.reason,
            ));
            // An individual rejected import may be transient (allocation,
            // descriptors or driver pressure). Never disable GPU importing
            // for every future DMA source on this window.
            // A producer-backed linear buffer can be mapped with DMA_BUF_SYNC.
            // If it cannot, request a CPU representation; a transparent
            // replacement is not an acceptable fallback for unknown pixels.
            if let Some(cpu) = linear_dmabuf_cpu_fallback(layer) {
                return stage_native_scene_source_effective(state, &cpu);
            }
            return Err(match failure.kind {
                crate::overlay::ImportFailureKind::UnsupportedProducer => {
                    "native-dmabuf-import-refused"
                }
                crate::overlay::ImportFailureKind::Retryable => "native-dmabuf-import-retryable",
            }
            .into());
        }
        Some(output)
    } else {
        None
    };
    // Cache only when a new immutable base was actually uploaded and its
    // retained charge fits the scene budget. Existing native revisions may
    // share this GPU base but never share their DMA-blended result.
    if native.dmabuf.is_some()
        && !native.bytes.is_empty()
        && reuse_base
        && patch.is_none()
        && let Some(cached) = state
            .native_cpu_bases
            .iter_mut()
            .find(|base| base.id == layer.id)
    {
        cached.cpu_revision = native.cpu_capture.map(|c| c.revision);
    }
    if native.dmabuf.is_some() && !native.bytes.is_empty() && (!reuse_base || patch.is_some()) {
        state.native_cpu_bases.retain(|old| old.id != layer.id);
        const MAX_CPU_BASE_CACHE: u64 = 128 * 1024 * 1024;
        let existing: u64 = state
            .native_cpu_bases
            .iter()
            .map(|item| {
                u64::from(item.image.image.width)
                    .saturating_mul(u64::from(item.image.image.height))
                    .saturating_mul(4)
            })
            .sum();
        let output_bytes = blended.as_ref().map_or(0, |image| {
            u64::from(image.image.width)
                .saturating_mul(u64::from(image.image.height))
                .saturating_mul(4)
        });
        if existing.saturating_add(image_bytes) <= MAX_CPU_BASE_CACHE
            && scene::fits_scene_budget(
                state.budgeted_scene_bytes(),
                image_bytes.saturating_add(output_bytes),
            )
        {
            state.native_cpu_bases.push(NativeCpuBaseCache {
                id: layer.id,
                source: Arc::downgrade(&native.bytes),
                cpu_revision: native.cpu_capture.map(|c| c.revision),
                image: Arc::clone(&lease),
            });
        }
    }
    let source = blended.unwrap_or(lease);
    // The CPU upload or completed DMA blend is already a GPU-owned native
    // snapshot. Retain it without allocating/copying a second per-tile image.
    // A previously flattened single Metal layer must be captured first or its
    // static background disappears when the first native foreground joins.
    if state.layers.len() == 1 && state.scene_layers.is_empty() && state.layers[0] != layer.id {
        state.capture_single_scene_from_composite()?;
    }
    state.accept_native_gpu_snapshot(layer, source);
    Ok(StagedNativeIdentity::from_placement(layer))
}

enum NativeEscrowProgress {
    Released,
    WaitingForReaders,
    NoCandidate,
}

// Shed a superseded, fully admitted native GPU generation only after its
// exact pixels have been copied to a bounded CPU rollback record. No scene
// publication may occur until this transaction has either committed or its
// old snapshots have been reconstructed.
fn escrow_one_retired_native(
    state: &mut Swapchain,
    rollback: &mut [SceneRollbackLayer],
    proposal: &[ScenePlacement],
) -> Result<NativeEscrowProgress, String> {
    const MAX_TRANSACTION_ESCROW: u64 = 384 * 1024 * 1024;
    let mut has_busy_reader = false;
    let held: u64 = rollback.iter().map(SceneRollbackLayer::escrow_bytes).sum();
    for prior in rollback {
        let Some(old) = prior.resident() else {
            continue;
        };
        if !old.ready
            || old.native_revision.is_none()
            || old.native_published_revision.is_none()
            || proposal
                .iter()
                .any(|p| native_image_matches_placement(old, p))
        {
            continue;
        }
        let snapshot = Arc::clone(&old.snapshot);
        let Ok(bytes) = scene::NativeGpuEscrow::extent(&snapshot.image) else {
            continue;
        };
        if bytes > MAX_TRANSACTION_ESCROW.saturating_sub(held) {
            continue;
        }
        if state
            .direct
            .as_ref()
            .and_then(|d| d.backing.as_ref())
            .is_some_and(|direct| Arc::ptr_eq(direct, &snapshot))
        {
            continue;
        }
        let Some(saved) = scene::NativeGpuEscrow::capture(state, &snapshot)? else {
            has_busy_reader = true;
            continue;
        };
        state
            .scene_layers
            .retain(|layer| !Arc::ptr_eq(&layer.snapshot, &snapshot));
        state
            .transaction_pinned
            .retain(|image| !Arc::ptr_eq(image, &snapshot));
        state.transaction_retiring_bytes = state.transaction_retiring_bytes.saturating_sub(bytes);
        drop(snapshot);
        prior.replace_with_escrow(saved);
        return Ok(NativeEscrowProgress::Released);
    }
    Ok(if has_busy_reader {
        NativeEscrowProgress::WaitingForReaders
    } else {
        NativeEscrowProgress::NoCandidate
    })
}

// Retry native staging only when the exact previous GPU generation can be
// retired to authoritative CPU escrow. The ordinary deferred path remains for
// unrelated fence pressure, and no revision is acknowledged here.
fn stage_native_transaction_source(
    state: &mut Swapchain,
    layer: &ScenePlacement,
    previous: &mut [SceneRollbackLayer],
    proposal: &[ScenePlacement],
) -> Result<Option<StagedNativeIdentity>, String> {
    loop {
        let result = stage_native_scene_source_effective(state, layer);
        let physical_pressure = matches!(result, Ok(None))
            && layer.native.as_ref().is_some_and(|native| {
                let pixels = u64::from(layer.frame.width)
                    .saturating_mul(u64::from(layer.frame.height))
                    .saturating_mul(4);
                let sources = if cached_native_cpu_base(state, layer).is_some() {
                    1
                } else {
                    1 + u64::from(native.dmabuf.is_some()) + u64::from(!native.bytes.is_empty())
                };
                !window_gpu_capacity_available(state.window, pixels.saturating_mul(sources))
            });
        if physical_pressure
            || matches!(result.as_ref(), Err(reason)
            if reason == "native AppKit atomic GPU transition peak exhausted")
        {
            match escrow_one_retired_native(state, previous, proposal) {
                Ok(NativeEscrowProgress::Released) => continue,
                Ok(NativeEscrowProgress::WaitingForReaders) => return Ok(None),
                Ok(NativeEscrowProgress::NoCandidate) => {}
                Err(error) => return Err(error),
            }
        }
        return result;
    }
}

// The rollback manifest is the authoritative old scene until every CPU
// record has been reconstructed. A failed restoration leaves it untouched;
// WSI cannot publish the partially staged accumulator in the meantime.
fn restore_native_rollback(state: &mut Swapchain) -> Result<(), String> {
    state.retire_copied();
    let Some(rollback) = state.pending_scene_rollback.as_ref() else {
        return Ok(());
    };
    let mut restored = Vec::with_capacity(rollback.len());
    for old in rollback {
        restored.push(old.materialize(state.gpu, state.window)?);
    }
    state.scene_layers = restored;
    state.pending_scene_rollback = None;
    Ok(())
}

/// A refused native DMA-BUF must not prevent unrelated Metal visibility or
/// opacity changes from publishing. Retain its previously accepted GPU image
/// (if any), using the *current* placement; otherwise omit only that native
/// source. No rejected image is treated as successfully imported.
fn preserve_refused_native_sources(
    state: &Swapchain,
    mut live: Vec<ScenePlacement>,
    refused: &[usize],
) -> Vec<ScenePlacement> {
    live.retain_mut(|placement| {
        if !refused.contains(&placement.id) {
            return true;
        }
        let retained = state
            .scene_layers
            .iter()
            .any(|source| source.id == placement.id && source.ready);
        let old = state
            .scene_placements
            .iter()
            .find(|previous| previous.id == placement.id && previous.native.is_some());
        if retained && let Some(old) = old {
            // An old GPU allocation has the old source extent and sampling
            // coordinates. Moving, resizing or remasking it with an unimported
            // payload does not describe any valid Core Animation frame.
            *placement = old.clone();
            true
        } else {
            false
        }
    });
    live
}

/// Native CPU layers can have a much larger logical image than their visible
/// intersection with a window. Preserve exact source texels and the absolute
/// CA mask coordinates while retaining only the required GPU pixels. This
/// prevents a pile of giant offscreen NSViews from exhausting the window's
/// hard GPU budget despite each drawing only a small foreground rectangle.
fn crop_native_to_window(mut placement: ScenePlacement) -> Option<ScenePlacement> {
    let native = placement.native.as_ref()?;
    if native.dmabuf.is_none() && native.bytes.is_empty() {
        return Some(placement);
    }
    let full = placement.frame;
    let stride = u64::from(full.width).checked_mul(4)?;
    if !native.bytes.is_empty()
        && stride.checked_mul(u64::from(full.height))? != native.bytes.len() as u64
    {
        return Some(placement); // Native validation produces the diagnostic.
    }
    let left = i64::from(full.x).max(0);
    let top = i64::from(full.y).max(0);
    let right = (i64::from(full.x) + i64::from(full.width)).min(i64::from(full.window_width));
    let bottom = (i64::from(full.y) + i64::from(full.height)).min(i64::from(full.window_height));
    let (left, top, right, bottom) = if let Some(mask) = placement.clip {
        (
            left.max(i64::from(mask.x)),
            top.max(i64::from(mask.y)),
            right.min(i64::from(mask.x) + i64::from(mask.width)),
            bottom.min(i64::from(mask.y) + i64::from(mask.height)),
        )
    } else {
        (left, top, right, bottom)
    };
    if left >= right || top >= bottom {
        return None;
    }
    if left == i64::from(full.x)
        && top == i64::from(full.y)
        && right - left == i64::from(full.width)
        && bottom - top == i64::from(full.height)
    {
        return Some(placement);
    }
    let width = usize::try_from(right - left).ok()?;
    let height = usize::try_from(bottom - top).ok()?;
    let source_width = usize::try_from(full.width).ok()?;
    let x = usize::try_from(left - i64::from(full.x)).ok()?;
    let y = usize::try_from(top - i64::from(full.y)).ok()?;
    let line_bytes = width.checked_mul(4)?;
    let output_bytes = line_bytes.checked_mul(height)?;
    // Admission remains bounded by the same per-source CPU capture limit.
    if !appkit::backing::native_scene_extent_supported(width as u32, height as u32) {
        return Some(placement);
    }
    let cropped = if native.bytes.is_empty() {
        Arc::new(Vec::new())
    } else {
        let mut cropped = Vec::with_capacity(output_bytes);
        for row in y..y + height {
            let start = row
                .checked_mul(source_width)?
                .checked_add(x)?
                .checked_mul(4)?;
            cropped.extend_from_slice(native.bytes.get(start..start + line_bytes)?);
        }
        Arc::new(cropped)
    };
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    native.revision.hash(&mut hash);
    (left, top, width, height, full.width, full.height).hash(&mut hash);
    let revision = hash.finish();
    let native = placement.native.as_mut().expect("validated above");
    native.bytes = cropped;
    native.revision = revision;
    // This is an additional worker-side crop, independent of AppKit's
    // source-local capture. Its new pixels/revision do not have the previous
    // source's damage-coordinate identity: force a full upload rather than
    // silently using a stale hint at the wrong cropped coordinate.
    if let Some(capture) = native.cpu_capture.as_mut() {
        capture.damage = None;
    }
    if native.dmabuf.is_some() {
        let mut sample = native.dma_sample.unwrap_or(scene::DmaSample {
            full_width: full.width,
            full_height: full.height,
            crop_x: 0,
            crop_y: 0,
        });
        sample.crop_x = sample.crop_x.checked_add(x as u32)?;
        sample.crop_y = sample.crop_y.checked_add(y as u32)?;
        native.dma_sample = Some(sample);
    }
    placement.frame.x = left as i32;
    placement.frame.y = top as i32;
    placement.frame.width = width as u32;
    placement.frame.height = height as u32;
    Some(placement)
}

/// A published revision is acknowledged only when its *own* staged image
/// generation is retained and the whole scene transaction committed. Old GPU
/// images remain valid to display but never acknowledge a refused successor.
fn committed_native_gpu_revisions(
    state: &Swapchain,
    effective: &[ScenePlacement],
    published: &HashMap<usize, u64>,
    staged: &HashMap<usize, StagedNativeIdentity>,
    refused: &[usize],
) -> Vec<(usize, u64)> {
    effective
        .iter()
        .filter_map(|placement| {
            placement.native.as_ref()?;
            if refused.contains(&placement.id) {
                return None;
            }
            let staged_identity = staged.get(&placement.id)?;
            let original = *published.get(&placement.id)?;
            state
                .scene_layers
                .iter()
                .any(|source| {
                    source.id == placement.id
                        && source.ready
                        && source.native_revision == Some(staged_identity.revision)
                        && source.native_dma_sample == staged_identity.dma_sample
                        && source.native_published_revision == Some(original)
                })
                .then_some((placement.id, original))
        })
        .collect()
}

fn layer_compositor_worker(window: usize, entry: Arc<WindowSwapchain>) {
    let mut prepare_failures = 0u32;
    let mut acquire_timeouts = 0u32;
    // Do not request a CPU redraw for an identical rejected producer frame on
    // every unrelated CA transaction. New native revisions can request again.
    let mut refused_revisions = std::collections::HashMap::<usize, (u64, u64)>::new();
    loop {
        // Wait without polling when clean. The writer sets composite_dirty under
        // the same mutex before signaling us, so notifications are not lost.
        let mut state = entry.state.lock().expect("window swapchain");
        while !state.composite_dirty
            && !state.scene_restore_pending
            && entry
                .pending_scene
                .lock()
                .expect("AppKit scene pending")
                .is_none()
            && entry.alive.load(Ordering::Acquire)
            && !entry.gpu.lost.load(Ordering::Acquire)
            && entry.present_failure.load(Ordering::Acquire) == 0
        {
            // The idle worker must not retain a window or device after
            // terminal loss. Unlike Condvar notification, unpark's token is
            // retained when a loss races the transition to sleep.
            let snapshot_wait = state.direct.as_ref().and_then(|direct| {
                direct.submitted_slot.and_then(|_| {
                    DIRECT_IDLE_SNAPSHOT_DELAY.checked_sub(direct.queued_at.elapsed())
                })
            });
            if state.direct.as_ref().is_some_and(|direct| {
                direct.submitted_slot.is_some()
                    && direct.queued_at.elapsed() >= DIRECT_IDLE_SNAPSHOT_DELAY
            }) {
                break;
            }
            #[cfg(test)]
            TEST_COMPOSITOR_IDLE_PARKED.store(window, Ordering::Release);
            drop(state);
            if let Some(wait) = snapshot_wait {
                std::thread::park_timeout(wait);
            } else {
                std::thread::park();
            }
            #[cfg(test)]
            TEST_COMPOSITOR_IDLE_PARKED.store(0, Ordering::Release);
            state = entry.state.lock().expect("window swapchain");
        }
        if !entry.alive.load(Ordering::Acquire)
            || entry.gpu.lost.load(Ordering::Acquire)
            || entry.present_failure.load(Ordering::Acquire) != 0
        {
            return;
        }
        if state.scene_restore_pending {
            drop(state);
            let _presenting = PresentingGuard {
                held: Some(entry.presenting.lock().expect("window presentation")),
                gpu: entry.gpu,
            };
            let mut state = entry.state.lock().expect("window swapchain");
            if state.scene_restore_pending {
                // Deterministically exercise the same nonblocking deferral
                // as an unsignaled Vulkan frame slot without sleeping on GPU.
                #[cfg(test)]
                if state.injected_restore_busy_cycles > 0 {
                    state.injected_restore_busy_cycles -= 1;
                    state.restore_busy_cycles_seen += 1;
                    drop(state);
                    drop(_presenting);
                    std::thread::park_timeout(Duration::from_millis(1));
                    continue;
                }
                if let Err(error) = restore_native_rollback(&mut state) {
                    let lost = error.contains("ERROR_DEVICE_LOST")
                        || entry.gpu.lost.load(Ordering::Acquire);
                    gpu::trace(format!(
                        "native rollback GPU reconstruction deferred: {error}"
                    ));
                    drop(state);
                    drop(_presenting);
                    if lost {
                        record_device_loss(entry.gpu);
                        return;
                    }
                    std::thread::park_timeout(Duration::from_millis(10));
                    continue;
                }
                match state.redisplay_scene(window) {
                    Ok(true) => {
                        state.scene_restore_pending = false;
                        state.scene_repaint_needed = false;
                    }
                    Ok(false) => {
                        // The restoration slot is busy. The last incomplete
                        // accumulator must not reach prepare_composite().
                        drop(state);
                        drop(_presenting);
                        std::thread::park_timeout(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => {
                        let lost = error.contains("ERROR_DEVICE_LOST")
                            || entry.gpu.lost.load(Ordering::Acquire);
                        gpu::trace(format!("native scene restoration deferred: {error}"));
                        drop(state);
                        drop(_presenting);
                        if lost {
                            record_device_loss(entry.gpu);
                            return;
                        }
                        std::thread::park_timeout(Duration::from_millis(10));
                        continue;
                    }
                }
            }
            // WSI may now publish the restored window, or process a later
            // complete proposal. No partially staged scene was ever exposed.
            continue;
        }
        if entry
            .pending_scene
            .lock()
            .expect("AppKit scene pending")
            .is_some()
        {
            drop(state);
            let _presenting = PresentingGuard {
                held: Some(entry.presenting.lock().expect("window presentation")),
                gpu: entry.gpu,
            };
            let snapshot = entry
                .pending_scene
                .lock()
                .expect("AppKit scene pending")
                .take();
            let mut retry_delay = None;
            let mut device_lost = false;
            let mut terminal_group_budget = false;
            let mut admitted_update: Option<Vec<(usize, u64)>> = None;
            if let Some(live) = snapshot {
                // Remember the AppKit publication identity *before* clipping
                // CPU sources or mapping a rejected linear DMA-BUF. The GPU
                // snapshot's own revision may be a crop-derived cache key.
                let published: HashMap<usize, u64> = live
                    .iter()
                    .filter_map(|p| p.native.as_ref().map(|n| (p.id, n.revision)))
                    .collect();
                let live: Vec<ScenePlacement> = live
                    .into_iter()
                    .filter_map(|placement| {
                        if placement.native.is_none() {
                            Some(placement)
                        } else {
                            crop_native_to_window(placement)
                        }
                    })
                    .collect();
                refused_revisions.retain(|id, _| live.iter().any(|p| p.id == *id));
                let mut state = entry.state.lock().expect("window swapchain");
                if state.direct.is_some() {
                    // A direct Metal source is not in scene_layers. Materialize
                    // it *before* capturing pre-batch ownership, or rollback
                    // would discard the only visible source. The transition is
                    // a real retained GPU generation, not an owned drawable
                    // that may be restored by copying a pointer.
                    match state.stage_direct_snapshot(true) {
                        Ok(Some(true)) => {}
                        Ok(_) => {
                            requeue_scene(&entry.pending_scene, live);
                            drop(state);
                            drop(_presenting);
                            std::thread::park_timeout(Duration::from_millis(1));
                            continue;
                        }
                        Err(error) => {
                            let lost = error.contains("ERROR_DEVICE_LOST")
                                || entry.gpu.lost.load(Ordering::Acquire);
                            gpu::trace(format!("direct scene transition deferred: {error}"));
                            if !lost && !error.contains(TERMINAL_GROUP_BUDGET) {
                                requeue_scene(&entry.pending_scene, live);
                            }
                            drop(state);
                            drop(_presenting);
                            if lost {
                                record_device_loss(entry.gpu);
                                return;
                            }
                            std::thread::park_timeout(Duration::from_millis(10));
                            continue;
                        }
                    }
                }
                // Stage native sources before committing the proposed view
                // hierarchy. A permanently refused DMA-BUF must not prune
                // previously retained Metal images, direct sources or scene
                // membership; only a complete import-ready snapshot may
                // replace the last accepted scene. Scene metadata is applied
                // in the subsequent synchronize_scene/redisplay_scene pass.
                let previous_scene = state.scene_placements.clone();
                // Every native staging submission may replace both a retained
                // snapshot and the actual GPU window accumulator. Keep the
                // previously committed image leases alive through the whole
                // batch; metadata-only rollback is not sufficient.
                // Spare cached native images are not part of the last
                // committed pixels needed for rollback. Never pin them into
                // an otherwise impossible atomic replacement.
                state.shed_scene_snapshot_spares();
                let mut previous_layers = state
                    .scene_layers
                    .iter()
                    .cloned()
                    .map(SceneRollbackLayer::from_gpu)
                    .collect::<Vec<_>>();
                state.transaction_pinned = state
                    .scene_layers
                    .iter()
                    .map(|layer| Arc::clone(&layer.snapshot))
                    .collect();
                // The projected scene does not retain old native GPU texels
                // when a layer is removed *or* the same layer ID publishes a
                // different image generation. Keep their old leases pinned
                // for rollback and charged to the 768MiB physical ledger.
                state.transaction_retiring_bytes = state
                    .scene_layers
                    .iter()
                    .filter(|old| {
                        old.native_revision.is_some()
                            && !live
                                .iter()
                                .any(|proposed| native_image_matches_placement(old, proposed))
                    })
                    .map(|old| scene::scene_image_bytes(PresentSource::from(&old.snapshot.image)))
                    .sum();
                let previous_order = state.layers.clone();
                let previous_frames = state.scene_frames.clone();
                let mut native_ready = true;
                let mut refused = Vec::new();
                let mut staged_ids = HashMap::<usize, StagedNativeIdentity>::new();
                let mut unrepresented = false;
                for layer in live.iter().filter(|p| p.native.is_some()) {
                    let revision = layer.native.as_ref().expect("filtered").revision;
                    let committed = state.projected_committed_scene_bytes();
                    // A hard-rejected revision cannot become admissible until
                    // the committed footprint drops or the source changes.
                    if refused_revisions
                        .get(&layer.id)
                        .is_some_and(|&(old, floor)| old == revision && committed >= floor)
                    {
                        if !state
                            .scene_layers
                            .iter()
                            .any(|old| old.id == layer.id && old.ready)
                            || !state
                                .scene_placements
                                .iter()
                                .any(|old| old.id == layer.id && old.native.is_some())
                        {
                            unrepresented = true;
                        }
                        refused.push(layer.id);
                        continue;
                    }
                    let staged = stage_native_transaction_source(
                        &mut state,
                        layer,
                        &mut previous_layers,
                        &live,
                    );
                    match staged {
                        Ok(Some(effective)) => {
                            let original = published.get(&layer.id).copied();
                            if let Some(image) = state.scene_layers.iter_mut().find(|image| {
                                image.id == layer.id
                                    && image.ready
                                    && image.native_revision == Some(effective.revision)
                                    && image.native_dma_sample == effective.dma_sample
                            }) {
                                // Bind the exact original publication to the
                                // retained image, not simply to the layer ID.
                                image.native_published_revision = original;
                                staged_ids.insert(layer.id, effective);
                            } else {
                                native_ready = false;
                                break;
                            }
                        }
                        Ok(None) => {
                            native_ready = false;
                            break;
                        }
                        Err(reason)
                            if reason == "native-dmabuf-import-refused"
                                || reason == "native AppKit GPU scene budget exhausted"
                                || reason
                                    == "native AppKit atomic GPU transition peak exhausted" =>
                        {
                            // Keep the last successfully published window
                            // image. Its producer will receive the changed
                            // importer capability and next supply CPU pixels;
                            // retries of the unchanged DMA-BUF cannot help.
                            if refused_revisions.insert(layer.id, (revision, committed))
                                != Some((revision, committed))
                            {
                                #[cfg(test)]
                                if reason.contains("budget") || reason.contains("peak") {
                                    NATIVE_HARD_BUDGET_REFUSALS.fetch_add(1, Ordering::Relaxed);
                                }
                                gpu::trace(format!(
                                    "native AppKit source {} revision {} permanently refused: {}",
                                    layer.id, revision, reason
                                ));
                                if reason == "native-dmabuf-import-refused" {
                                    appkit::view::request_native_cpu_fallback(
                                        window as sdl3::Window,
                                        layer.id,
                                    );
                                }
                            }
                            if !state
                                .scene_layers
                                .iter()
                                .any(|old| old.id == layer.id && old.ready)
                                || !state
                                    .scene_placements
                                    .iter()
                                    .any(|old| old.id == layer.id && old.native.is_some())
                            {
                                // Without an old GPU snapshot, omitting this
                                // foreground would publish the wrong pixels.
                                unrepresented = true;
                            }
                            refused.push(layer.id);
                            continue;
                        }
                        Err(reason) => {
                            #[cfg(test)]
                            if reason == "native-dmabuf-import-retryable" {
                                TEST_NATIVE_RETRYABLE_FAILURES.fetch_add(1, Ordering::Relaxed);
                            }
                            gpu::trace(format!("native AppKit scene staging failed: {reason}"));
                            device_lost = reason.contains("ERROR_DEVICE_LOST")
                                || entry.gpu.lost.load(Ordering::Acquire);
                            terminal_group_budget = reason.contains(TERMINAL_GROUP_BUDGET);
                            native_ready = false;
                            break;
                        }
                    }
                }
                let effective = preserve_refused_native_sources(&state, live, &refused);
                let mut rollback_needed = false;
                if unrepresented {
                    // A new source with neither GPU admission nor readable
                    // fallback cannot be composited. Do not commit a scene
                    // in which it simply vanishes. Restore the last complete
                    // scene (including retained Metal) and await the AppKit
                    // producer's CPU-replacement publication. Unlike an
                    // ordinary retry, this does not poll every 10 ms.
                    rollback_needed = true;
                    state.layers = previous_order;
                    state.scene_frames = previous_frames;
                    state.scene_placements = previous_scene;
                    // The accumulator contains submitted partial stages.
                    // Retry reconstruction before WSI or another proposal.
                    state.scene_repaint_needed = true;
                    state.scene_restore_pending = true;
                } else if !native_ready {
                    rollback_needed = true;
                    state.layers = previous_order;
                    state.scene_frames = previous_frames;
                    state.scene_placements = previous_scene;
                    state.scene_repaint_needed = true;
                    state.scene_restore_pending = true;
                    if !device_lost {
                        // Terminal capacity is a permanent inability to show
                        // this exact immutable scene within the 512 MiB ceiling.
                        // Retain its previous complete frame and await changed
                        // producer metadata, not a busy polling/requeue loop.
                        if !terminal_group_budget {
                            requeue_scene(&entry.pending_scene, effective);
                        }
                        retry_delay = Some(Duration::from_millis(10));
                    }
                } else {
                    // Old accepted pixels are already in scene_layers; the
                    // source corresponding to a refused id must not be staged
                    // again merely because its AppKit metadata changed.
                    state.synchronize_scene(window, &effective);
                    if state.scene_repaint_needed {
                        match state.redisplay_scene(window) {
                            Ok(true) => {
                                state.scene_repaint_needed = false;
                                wake_compositor_worker(&entry);
                            }
                            Ok(false) => {
                                // The compositor could defer even after native
                                // stage submissions altered the accumulator.
                                // Restore pinned pre-batch image generations.
                                rollback_needed = true;
                                state.layers = previous_order.clone();
                                state.scene_frames = previous_frames.clone();
                                state.scene_placements = previous_scene.clone();
                                state.scene_repaint_needed = true;
                                state.scene_restore_pending = true;
                                requeue_scene(&entry.pending_scene, effective.clone());
                                retry_delay = Some(Duration::from_millis(1));
                            }
                            Err(error) => {
                                gpu::trace(format!(
                                    "recomposing changed layer tree failed: {error}"
                                ));
                                rollback_needed = true;
                                state.layers = previous_order.clone();
                                state.scene_frames = previous_frames.clone();
                                state.scene_placements = previous_scene.clone();
                                state.scene_repaint_needed = true;
                                state.scene_restore_pending = true;
                                device_lost = error.contains("ERROR_DEVICE_LOST")
                                    || entry.gpu.lost.load(Ordering::Acquire);
                                if !device_lost {
                                    // A transient recording/submission error must
                                    // not discard scene-only damage. Keep any
                                    // newer AppKit snapshot ahead of this retry.
                                    if !error.contains(TERMINAL_GROUP_BUDGET) {
                                        requeue_scene(&entry.pending_scene, effective.clone());
                                    } else {
                                        gpu::trace(
                                            "Core Animation group scene permanently refused at the retained GPU memory ceiling",
                                        );
                                    }
                                    retry_delay = Some(Duration::from_millis(10));
                                }
                            }
                        }
                    }
                    if retry_delay.is_none() && !device_lost {
                        let confirmed = committed_native_gpu_revisions(
                            &state,
                            &effective,
                            &published,
                            &staged_ids,
                            &refused,
                        );
                        admitted_update = Some(confirmed);
                    }
                }
                // Failure leaves the previous scene authoritative in a
                // rollback manifest. Do not republish the partially staged
                // accumulator before every escrowed image is reconstructed.
                if rollback_needed {
                    state.scene_layers.clear();
                    state.pending_scene_rollback = Some(previous_layers);
                }
                state.transaction_pinned.clear();
                state.transaction_retiring_bytes = 0;
            }
            drop(_presenting);
            // Never acquire AppKit backing/scene locks while holding the
            // Vulkan state or WSI presentation mutex.
            if let Some(admitted) = admitted_update {
                appkit::backing::set_native_gpu_admission(window as sdl3::Window, &admitted);
            }
            if device_lost {
                record_device_loss(entry.gpu);
                return;
            }
            if let Some(delay) = retry_delay {
                // Park outside the presentation mutex to avoid a busy retry
                // loop when the GPU is still unavailable.
                std::thread::park_timeout(delay);
            }
            continue;
        }
        if !state.composite_dirty {
            drop(state);
            let _presenting = PresentingGuard {
                held: Some(entry.presenting.lock().expect("window presentation")),
                gpu: entry.gpu,
            };
            let mut state = entry.state.lock().expect("window swapchain");
            match state.snapshot_idle_direct() {
                Ok(_) => {}
                Err(error) => {
                    let device_lost = state.last_wsi_error.take()
                        == Some(vk::Result::ERROR_DEVICE_LOST)
                        || error.contains("ERROR_DEVICE_LOST");
                    drop(state);
                    drop(_presenting);
                    if device_lost {
                        record_device_loss(entry.gpu);
                        return;
                    }
                    gpu::trace(format!("idle direct snapshot failed: {error}"));
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            continue;
        }
        if let Some(remaining) = state
            .composite_dirty_since
            .and_then(|since| pending_batch_delay(&state).checked_sub(since.elapsed()))
        {
            let _wait = entry
                .wake
                .wait_timeout(state, remaining)
                .expect("compositor batch wait");
            continue;
        }
        drop(state);

        let _presenting = PresentingGuard {
            held: Some(entry.presenting.lock().expect("window presentation")),
            gpu: entry.gpu,
        };
        if !entry.alive.load(Ordering::Acquire) || entry.gpu.lost.load(Ordering::Acquire) {
            return;
        }
        let mut state = entry.state.lock().expect("window swapchain");
        // A resize may have reset the accumulator while we acquired the
        // presentation lock. Do not try to submit a retired image.
        if !uses_layer_compositor(state.present_mode) || !state.composite_dirty {
            continue;
        }
        // A source batch may have submitted GPU copies before it failed.
        // Never submit those accumulator pixels to WSI until restoration.
        if state.scene_restore_pending {
            continue;
        }
        let prepared = state.prepare_composite(window as sdl3::Window);
        let typed_device_loss = state.last_wsi_error.take() == Some(vk::Result::ERROR_DEVICE_LOST);
        let gpu = state.gpu;
        drop(state);
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                if typed_device_loss || error.contains("ERROR_DEVICE_LOST") {
                    drop(_presenting);
                    record_device_loss(gpu);
                    return;
                }
                if recoverable_composite_rebuild_failure(&error) {
                    prepare_failures = 0;
                    drop(_presenting);
                    std::thread::sleep(Duration::from_millis(80));
                    continue;
                }
                prepare_failures += 1;
                if prepare_failures >= 3 {
                    gpu::trace(format!(
                        "presentation disabled after persistent preparation failures: {error}"
                    ));
                    entry.present_failure.store(2, Ordering::Release);
                    return;
                }
                drop(_presenting);
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        #[cfg(test)]
        let injected = entry
            .injected_acquire_error
            .lock()
            .expect("WSI injection")
            .take();
        #[cfg(not(test))]
        let injected: Option<vk::Result> = None;
        // Every mode waits for its image. Polling MAILBOX with a backoff let
        // the retry miss the image's release, so frames were superseded
        // before they reached WSI.
        let wait = acquire_timeouts == 0;
        let acquired = match injected {
            Some(error) => CompositorAcquire::Terminal(error),
            None => {
                crate::stats::count(crate::stats::Counter::PresentAcquireCalls);
                let start = wait.then(crate::stats::timer).flatten();
                let acquired =
                    crate::stats::timed(crate::stats::Counter::PresentAcquireMicros, || {
                        acquire_compositor_image(gpu, prepared.request, wait)
                    });
                if wait {
                    crate::stats::count(crate::stats::Counter::PresentAcquireWaitCalls);
                    crate::stats::finish_timer(
                        crate::stats::Counter::PresentAcquireWaitMicros,
                        start,
                    );
                }
                if matches!(acquired, CompositorAcquire::TimedOut) {
                    crate::stats::count(crate::stats::Counter::PresentAcquireTimeouts);
                }
                acquired
            }
        };
        if matches!(acquired, CompositorAcquire::TimedOut) {
            // Retain the dirty accumulator. Only this window waits; another
            // window's presenter is completely independent. Wait with the
            // presentation lock released: a minimized or occluded window can
            // time out indefinitely, and every producer needs that lock.
            acquire_timeouts = acquire_timeouts.saturating_add(1);
            drop(_presenting);
            std::thread::park_timeout(compositor_acquire_backoff(acquire_timeouts));
            continue;
        }
        acquire_timeouts = 0;
        let ready = match acquired {
            CompositorAcquire::Ready(frame) => frame,
            CompositorAcquire::TimedOut => unreachable!(),
            CompositorAcquire::Terminal(error) => {
                gpu::trace(format!(
                    "presentation disabled after terminal WSI acquire error: {error:?}"
                ));
                drop(_presenting);
                handle_terminal_error(gpu, &entry, error);
                return;
            }
        };
        let mut state = entry.state.lock().expect("window swapchain");
        let old_swapchain = state.handle;
        match state.complete_composite(prepared, Ok(ready)) {
            Ok(None) => {
                prepare_failures = 0;
                #[cfg(test)]
                {
                    state.wsi_publishes += 1;
                }
                if state.handle != old_swapchain {
                    // Recovery lost the accumulator. Request a new store
                    // upload outside the WSI and AppKit store mutexes.
                    drop(state);
                    drop(_presenting);
                    appkit::backing::present_again(window as sdl3::Window);
                    continue;
                }
            }
            Ok(Some(error)) => {
                gpu::trace(format!(
                    "presentation disabled after terminal queue presentation error: {error:?}"
                ));
                drop(state);
                drop(_presenting);
                handle_terminal_error(gpu, &entry, error);
                return;
            }
            Err(error) => {
                let typed_device_loss =
                    state.last_wsi_error.take() == Some(vk::Result::ERROR_DEVICE_LOST);
                if typed_device_loss || error.contains("ERROR_DEVICE_LOST") {
                    drop(state);
                    drop(_presenting);
                    record_device_loss(gpu);
                    return;
                }
                if recoverable_composite_rebuild_failure(&error) {
                    prepare_failures = 0;
                    drop(state);
                    drop(_presenting);
                    std::thread::sleep(Duration::from_millis(80));
                    continue;
                }
                prepare_failures += 1;
                if prepare_failures >= 3 {
                    gpu::trace(format!(
                        "presentation disabled after persistent presentation failures: {error}"
                    ));
                    entry.present_failure.store(2, Ordering::Release);
                    return;
                }
                gpu::trace(format!("window composite presentation failed: {error}"));
                drop(state);
                drop(_presenting);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

impl Swapchain {
    /// Move an obsolete direct drawable to its WSI submit fence so that a
    /// source cannot be released while the GPU is still copying from it.
    fn retire_direct(&mut self) {
        let Some(mut direct) = self.direct.take() else {
            return;
        };
        if let Some(slot) = direct.submitted_slot
            && self.frames[slot].submitted
        {
            debug_assert!(self.frames[slot].source.is_none());
            debug_assert!(self.frames[slot].backing.is_none());
            self.frames[slot].source = direct.owner.take();
            self.frames[slot].backing = direct.backing.take();
        }
        // An unsubmitted drawable or a completed WSI copy is safe to release.
    }

    fn queue_direct(
        &mut self,
        layer: usize,
        frame: &appkit::view::Frame,
        source: PresentSource,
        owner: &mut SourceOwner,
        backing: Option<Arc<BackingImageLease>>,
    ) {
        self.retire_direct();
        self.direct = Some(DirectLayer {
            layer,
            frame: DirectFrame::from_frame(frame),
            source,
            owner: owner.take(),
            backing,
            submitted_slot: None,
            queued_at: Instant::now(),
        });
        self.composite_dirty = true;
        self.composite_dirty_since = Some(Instant::now());
    }

    /// Materialize the last directly displayed layer only when another layer
    /// actually joins. This preserves a static underlying layer while avoiding
    /// a full-frame intermediate copy in the steady-state single-layer case.
    fn stage_direct_snapshot(&mut self, nonblocking: bool) -> Result<Option<bool>, String> {
        let Some(mut direct) = self.direct.take() else {
            return Ok(None);
        };
        let mut owner = SourceOwner(direct.owner.take());
        let regions = self.live_scene_regions(direct.layer, &direct.frame.frame());
        let result = self.stage_composite_source(
            direct.layer,
            &direct.frame.frame(),
            direct.source,
            regions.as_deref(),
            &mut owner,
            nonblocking,
            true,
        );
        if matches!(result, Ok(Some(true))) {
            let slot = self.frame.wrapping_sub(1) % self.frames.len();
            debug_assert!(self.frames[slot].backing.is_none());
            self.frames[slot].backing = direct.backing.take();
        } else {
            direct.owner = owner.take();
            self.direct = Some(direct);
        }
        result
    }

    /// After an idle window is already on screen, snapshot it once and return
    /// its drawable to the guest. No extra WSI present is needed for this copy.
    /// Called only under the window presentation mutex, on the worker thread.
    fn snapshot_idle_direct(&mut self) -> Result<bool, String> {
        if self.composite_dirty
            || !self.direct.as_ref().is_some_and(|direct| {
                direct.submitted_slot.is_some()
                    && direct.queued_at.elapsed() >= DIRECT_IDLE_SNAPSHOT_DELAY
            })
        {
            return Ok(false);
        }
        let slot = self.frame % self.frames.len();
        if !matches!(self.stage_direct_snapshot(false)?, Some(true)) {
            return Ok(false);
        }
        // This is an idle-only transfer: waiting on the compositor worker
        // returns the retained drawable immediately without delaying AppKit.
        self.gpu.wait(self.frames[slot].done)?;
        self.frames[slot].submitted = false;
        finish_source(self.frames[slot].source.take());
        self.frames[slot].backing.take();
        self.frames[slot].scene_reads.clear();
        self.composite_dirty = false;
        self.composite_dirty_since = None;
        Ok(true)
    }

    /// Copy a synthetic test drawable into the window accumulator without
    /// publishing an intermediate WSI frame. Production uses the per-layer
    /// scene path to retain independent GPU-owned snapshots.
    #[cfg(test)]
    fn stage_composite(
        &mut self,
        frame: &appkit::view::Frame,
        source: &GpuImage,
        regions: Option<&[Region]>,
        owner: &mut SourceOwner,
        nonblocking: bool,
    ) -> Result<Option<bool>, String> {
        #[cfg(test)]
        if let Some(error) = self.injected_stage_vk_error.take() {
            self.last_wsi_error = Some(error);
            return Err(format!("injected composite staging failure: {error:?}"));
        }
        self.stage_composite_source(
            0,
            frame,
            PresentSource::from(source),
            regions,
            owner,
            nonblocking,
            false,
        )
    }

    fn stage_composite_layer(
        &mut self,
        layer: usize,
        frame: &appkit::view::Frame,
        source: &GpuImage,
        regions: Option<&[Region]>,
        owner: &mut SourceOwner,
        nonblocking: bool,
    ) -> Result<Option<bool>, String> {
        #[cfg(test)]
        if let Some(error) = self.injected_stage_vk_error.take() {
            self.last_wsi_error = Some(error);
            return Err(format!("injected composite staging failure: {error:?}"));
        }
        self.stage_composite_source(
            layer,
            frame,
            PresentSource::from(source),
            regions,
            owner,
            nonblocking,
            false,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Explicit source and ownership state is required at GPU admission"
    )]
    fn stage_composite_source(
        &mut self,
        layer: usize,
        frame: &appkit::view::Frame,
        source: PresentSource,
        regions: Option<&[Region]>,
        owner: &mut SourceOwner,
        nonblocking: bool,
        force_retained: bool,
    ) -> Result<Option<bool>, String> {
        let slot = self.frame % self.frames.len().max(1);
        let this = &self.frames[slot];
        let (command_buffer, done) = (this.command_buffer, this.done);
        // vkQueueSubmit may fail after resetting the fence, without queueing
        // any work to signal it. Only wait for slots actually submitted.
        // AppKit must not wait for a GPU frame held by another operation.
        if this.submitted {
            if nonblocking {
                // SAFETY: the fence belongs to a submitted frame on this GPU.
                if !self.gpu.fence_signaled(done)? {
                    return Ok(None);
                }
            } else {
                self.gpu.wait(done)?;
            }
        }
        self.frames[slot].submitted = false;
        finish_source(self.frames[slot].source.take());
        self.frames[slot].backing.take();
        self.frames[slot].scene_reads.clear();
        let composite = self.composite()?;
        let scene_work = match self.prepare_scene(layer, frame, source, regions, force_retained)? {
            ScenePrepared::SingleLayer => None,
            ScenePrepared::Deferred => return Ok(None),
            ScenePrepared::Ready(work) => Some(work),
        };

        let device = &self.gpu.device;
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let staged = (|| -> Result<(), String> {
            self.prepare_opacity_group_targets(slot)?;
            // SAFETY: this command buffer's fence has completed, and all images
            // remain in GENERAL for copies between guest layers.
            unsafe {
                device
                    .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
                    .map_err(|e| format!("vkResetCommandBuffer failed: {e:?}"))?;
                device
                    .begin_command_buffer(command_buffer, &begin)
                    .map_err(|e| format!("vkBeginCommandBuffer failed: {e:?}"))?;
                prepare_source(device, command_buffer, source.image);
                // A previous window presentation may still be reading the
                // accumulator (transfer or corners compute), even when its layer
                // writes have finished. Order those reads before a new write.
                let image_barrier = vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_access_mask(
                        vk::AccessFlags::TRANSFER_READ
                            | vk::AccessFlags::TRANSFER_WRITE
                            | vk::AccessFlags::SHADER_READ,
                    )
                    .dst_access_mask(
                        vk::AccessFlags::TRANSFER_WRITE | vk::AccessFlags::TRANSFER_READ,
                    )
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(composite)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    });
                device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::TRANSFER | vk::PipelineStageFlags::COMPUTE_SHADER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[image_barrier],
                );
                if let Some(work) = &scene_work {
                    self.record_scene(device, command_buffer, composite, source, work)?;
                } else {
                    match regions {
                        Some(regions) => {
                            for region in regions {
                                let part = appkit::view::Frame {
                                    x: region.x,
                                    y: region.y,
                                    width: region.width.max(0) as u32,
                                    height: region.height.max(0) as u32,
                                    ..*frame
                                };
                                if let Some(into) = clamp(&part, self.extent) {
                                    blit(
                                        device,
                                        command_buffer,
                                        source.image,
                                        source_of(into, frame, (source.width, source.height)),
                                        composite,
                                        vk::ImageLayout::GENERAL,
                                        into,
                                    );
                                }
                            }
                        }
                        None => {
                            if let Some(into) = clamp(frame, self.extent) {
                                blit(
                                    device,
                                    command_buffer,
                                    source.image,
                                    source_of(into, frame, (source.width, source.height)),
                                    composite,
                                    vk::ImageLayout::GENERAL,
                                    into,
                                );
                            }
                        }
                    }
                }
                device
                    .end_command_buffer(command_buffer)
                    .map_err(|e| format!("vkEndCommandBuffer failed: {e:?}"))?;
            }
            // The Vulkan queue orders this composite update after the guest's
            // Metal command buffer and before a later WSI copy. No WSI semaphore
            // is required: the composite is an ordinary device-owned image.
            #[cfg(test)]
            if let Some(error) = self.injected_post_scene_record_error.take() {
                return Err(format!("injected post-scene recording failure: {error:?}"));
            }
            self.gpu
                .submit_synchronized(command_buffer, done, &[], &[])?;
            Ok(())
        })();
        if let Err(error) = staged {
            if let Some(work) = scene_work {
                self.rollback_scene(work);
            }
            return Err(error);
        }
        self.frames[slot].submitted = true;
        if let Some(work) = scene_work {
            self.complete_scene(slot, work);
        }
        self.frames[slot].source = owner.take();
        self.frame = self.frame.wrapping_add(1);
        if !self.composite_dirty {
            self.composite_dirty_since = Some(Instant::now());
        }
        self.composite_dirty = true;
        Ok(Some(true))
    }

    /// Present the whole layer accumulator once instead of placing one
    /// WSI entry in the display queue for each Metal layer update.
    fn prepare_composite(&mut self, window: sdl3::Window) -> Result<PreparedComposite, String> {
        if self.retired {
            self.rebuild(self.requested)?;
        }
        let direct = self.direct.is_some();
        let (frame, source) = if let Some(single) = self.direct.as_ref() {
            (single.frame.frame(), single.source)
        } else {
            (
                appkit::view::Frame {
                    window,
                    x: 0,
                    y: 0,
                    width: self.extent.width,
                    height: self.extent.height,
                    window_width: self.requested.width,
                    window_height: self.requested.height,
                },
                PresentSource::from(self.composite.as_ref().expect("window layer accumulator")),
            )
        };
        let (prepared, request) = self
            .prepare_present(if direct { None } else { Some(&[]) }, false)?
            .ok_or("layer compositor failed to reserve present frame")?;
        Ok(PreparedComposite {
            frame,
            source,
            prepared,
            request,
            direct,
        })
    }

    fn complete_composite(
        &mut self,
        ready: PreparedComposite,
        acquired: Result<Option<(u32, bool)>, vk::Result>,
    ) -> Result<Option<vk::Result>, String> {
        let before = self.handle;
        let frame_before = self.frame;
        let direct_slot = ready.prepared.slot;
        let result = self.finish_present(
            &ready.frame,
            ready.source,
            if ready.direct { None } else { Some(&[]) },
            ready.prepared,
            acquired,
        );
        if ready.direct
            && self.frame != frame_before
            && let Some(single) = self.direct.as_mut()
        {
            single.submitted_slot = Some(direct_slot);
        }
        if self.acquired_unsubmitted.is_some() {
            // A failed WSI copy must consume the acquisition semaphore and
            // retire the acquired image before another compositor acquisition.
            if let Err(error) = &result {
                gpu::trace(format!(
                    "window presentation submission dropped after acquire: {error}"
                ));
            }
            if let Err(error) = self.recover_unsubmitted_acquire() {
                // Never retry with unresolved WSI synchronization.
                gpu::trace(format!("window acquire recovery failed: {error}"));
                return Ok(Some(vk::Result::ERROR_UNKNOWN));
            }
            return Ok(None);
        }
        let failed = result?;
        // An out-of-date rebuild retained the previous composite snapshot.
        // Clear dirty only when that snapshot reached the new swapchain.
        if failed.is_none() && self.handle == before {
            self.composite_dirty = false;
            self.composite_dirty_since = None;
        }
        Ok(failed)
    }

    /// Deterministic WSI integration-test entry point. The production worker
    /// has a bounded acquire and does not hold the swapchain-state lock.
    #[cfg(test)]
    fn flush_composite(&mut self, window: sdl3::Window) -> Result<(), String> {
        if !self.composite_dirty {
            return Ok(());
        }
        let ready = self.prepare_composite(window)?;
        let acquired = acquire_image(self.gpu, ready.request, false);
        match self.complete_composite(ready, acquired)? {
            None => Ok(()),
            Some(error) => Err(format!("terminal queue presentation error: {error:?}")),
        }
    }
}

/// Hand back every drawable whose copy into a swapchain image has finished.
///
/// A frame slot otherwise keeps its source until the slot comes round again,
/// several presentations later. Core Animation has a drawable back once it is
/// on screen, and a guest that holds one drawable of three - Unity keeps the
/// one it fetched on resize - runs its ring dry waiting for a presentation
/// that needs a drawable to happen. Must not be called with the drawable
/// cache locked: finishing a source takes that lock.
pub(crate) fn retire_copied() {
    retire_copied_in(&every_swapchain());
}

fn every_swapchain() -> Vec<Arc<WindowSwapchain>> {
    SWAPCHAINS
        .lock()
        .expect("window swapchains")
        .as_ref()
        .map(|all| all.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
}

/// Whether any of `swapchains` had a drawable to hand back.
fn retire_copied_in(swapchains: &[Arc<WindowSwapchain>]) -> bool {
    let mut any = false;
    for swapchain in swapchains {
        // Retiring copied drawables is opportunistic. A presentation may be
        // waiting for a swapchain image under this window's lock; do not make
        // drawable acquisition wait behind it merely to check a fence.
        let retired = match swapchain.state.try_lock() {
            Ok(mut state) => state.retire_copied(),
            Err(TryLockError::WouldBlock) => false,
            Err(TryLockError::Poisoned(error)) => error.into_inner().retire_copied(),
        };
        if retired {
            // Source ownership has already returned to its drawable cache.
            // Wake consumers only after releasing the swapchain state mutex.
            swapchain.gpu.presenter.notify_progress();
            any = true;
        }
    }
    any
}

/// [`retire_copied`], waiting up to `timeout` for a copy still running when
/// none has finished yet, because nothing signals the drawable cache when one
/// does. Whether there was a copy to retire or wait for, so that the caller
/// knows to look at its ring again before waiting on anything else. Must not
/// be called with the drawable cache locked.
pub(crate) fn wait_copied(timeout: std::time::Duration) -> bool {
    let entries = every_swapchain();
    // Every window first: the ring being waited on may be another window's
    // than the first one with a copy still running.
    if retire_copied_in(&entries) {
        return true;
    }
    // Hold a single window's state while using its fences: resize and teardown
    // can destroy them, but cannot do so while this guard is held. Never hold
    // the global swapchain-map lock across a Vulkan wait.
    for entry in &entries {
        let state = match entry.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
        };
        let fences = state.copying();
        if fences.is_empty() {
            continue;
        }
        // SAFETY: fences of this device's, each attached to a submission,
        // and kept alive by the state guard.
        let waited = unsafe {
            state
                .gpu
                .device
                .wait_for_fences(&fences, false, timeout.as_nanos() as u64)
        };
        drop(state);
        retire_copied_in(&entries);
        // A wait that failed outright did not wait, and the caller must not
        // take it as having done so or it spins on a lost device.
        return matches!(waited, Ok(()) | Err(vk::Result::TIMEOUT));
    }
    false
}

fn finish_source(source: Option<usize>) {
    if let Some(source) = source {
        crate::present::presentation_complete(source);
    }
}

/// A rectangle of window pixels, top-left origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl From<appkit::backing::Rect> for Region {
    fn from(rect: appkit::backing::Rect) -> Region {
        Region {
            x: rect.x.clamp(0, i64::from(i32::MAX)) as i32,
            y: rect.y.clamp(0, i64::from(i32::MAX)) as i32,
            width: rect.width.clamp(0, i64::from(i32::MAX)) as i32,
            height: rect.height.clamp(0, i64::from(i32::MAX)) as i32,
        }
    }
}

/// `whole` less every rectangle in `holes`, as rectangles that do not overlap.
pub(crate) fn subtract(whole: Region, holes: &[Region]) -> Vec<Region> {
    let mut pieces = vec![whole];
    for hole in holes {
        let mut next = Vec::with_capacity(pieces.len() + 4);
        for piece in pieces {
            let left = piece.x.max(hole.x);
            let top = piece.y.max(hole.y);
            let right = (piece.x + piece.width).min(hole.x + hole.width);
            let bottom = (piece.y + piece.height).min(hole.y + hole.height);
            if left >= right || top >= bottom {
                next.push(piece);
                continue;
            }
            let (piece_right, piece_bottom) = (piece.x + piece.width, piece.y + piece.height);
            let bands = [
                (piece.x, piece.y, piece.width, top - piece.y),
                (piece.x, bottom, piece.width, piece_bottom - bottom),
                (piece.x, top, left - piece.x, bottom - top),
                (right, top, piece_right - right, bottom - top),
            ];
            next.extend(
                bands
                    .into_iter()
                    .filter(|&(_, _, width, height)| width > 0 && height > 0)
                    .map(|(x, y, width, height)| Region {
                        x,
                        y,
                        width,
                        height,
                    }),
            );
        }
        pieces = next;
    }
    pieces
}

impl Swapchain {
    fn create(
        gpu: &'static Gpu,
        window: sdl3::Window,
        extent: vk::Extent2D,
        surface: u64,
        instance: u64,
    ) -> Result<Self, String> {
        // Establish VkSurface ownership before *any* validation may fail.
        // The window metadata can be invalidated by SDL resizing the window
        // after its surface has been prepared on the video thread.
        let surface = gpu::Guard::new(vk::SurfaceKHR::from_raw(surface), |surface| {
            release_created_surface(instance, surface.as_raw());
        });
        #[cfg(test)]
        if TEST_FAIL_FIRST_CREATE_WINDOW
            .compare_exchange(window as usize, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Err("the window has no area to present into".into());
        }
        #[cfg(test)]
        if TEST_FAIL_DEVICE_CREATE_WINDOW
            .compare_exchange(window as usize, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Err("vkCreateSwapchainKHR failed with ERROR_DEVICE_LOST".into());
        }
        // Never an OpenGL window. SDL will convert one in place, but it
        // converts the flags and the loaded libraries and not the backend's
        // own state: on Wayland the `EGLSurface` SDL created with the window
        // stays alive on the `wl_surface`, and when the Vulkan driver claims
        // the same surface the compositor answers
        // `wp_linux_drm_syncobj_manager_v1: surface already exists` and
        // closes the connection.
        //
        // A window with no graphics API - which is how this runtime makes
        // them, exactly so that this is possible - has no `EGLSurface` and
        // is converted safely, so only a guest that has actually asked for
        // an `NSOpenGLContext` ends up here.
        let snapshot = sdl3::video::presentation_window(window)
            .ok_or("window video properties have not been published")?;
        if snapshot.flags & sdl3::video::flags::OPENGL != 0 {
            return Err("the window belongs to OpenGL, which cannot share it".into());
        }
        // The transferred surface is already guarded above.
        if !gpu.can_present(*surface) {
            return Err("the Vulkan queue Metal submits to cannot present to this window".into());
        }
        // Serialize Vulkan WSI-object setup, not unrelated shader compilation.
        let wsi = gpu.wsi.lock().expect("Vulkan WSI");
        let pool_info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(gpu.queue_family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        // SAFETY: a well-formed create info on this live device.
        let pool = unsafe { gpu.device.create_command_pool(&pool_info, None) }
            .map_err(|error| format!("vkCreateCommandPool failed with {error:?}"))?;
        // The command buffers are the pool's, so they go with it.
        let pool = gpu::Guard::new(pool, |pool| {
            // SAFETY: a pool of this device's, nothing allocated from it
            // having been submitted.
            unsafe { gpu.device.destroy_command_pool(pool, None) };
        });
        let allocate = vk::CommandBufferAllocateInfo::default()
            .command_pool(*pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(FRAMES_IN_FLIGHT as u32);
        // SAFETY: the pool was just created by this device.
        let command_buffers = unsafe { gpu.device.allocate_command_buffers(&allocate) }
            .map_err(|error| format!("vkAllocateCommandBuffers failed with {error:?}"))?;
        let mut frames = gpu::GuardedVec::new(|frame: &Frame| {
            // SAFETY: objects of this device's, never submitted.
            unsafe {
                gpu.device.destroy_semaphore(frame.acquired, None);
                gpu.device.destroy_fence(frame.acquire_done, None);
                gpu.device.destroy_fence(frame.done, None);
            }
        });
        for command_buffer in command_buffers {
            // A fence created signalled, because the first use of every frame
            // waits on it before it has ever been submitted.
            let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
            // SAFETY: a well-formed create info on this live device.
            let acquired = unsafe {
                gpu.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            }
            .map_err(|error| format!("vkCreateSemaphore failed with {error:?}"))?;
            let acquired = gpu::Guard::new(acquired, |acquired| {
                // SAFETY: a semaphore of this device's, never submitted.
                unsafe { gpu.device.destroy_semaphore(acquired, None) };
            });
            // SAFETY: a well-formed create info on this live device.
            let done = unsafe { gpu.device.create_fence(&fence_info, None) }
                .map_err(|error| format!("vkCreateFence failed with {error:?}"))?;
            let done = gpu::Guard::new(done, |fence| {
                // A failed acquisition-fence creation must not leak this fence.
                unsafe { gpu.device.destroy_fence(fence, None) };
            });
            // A separate acquire fence proves the WSI signal finished
            // even if neither the graphics nor recovery queue submit works.
            let acquire_done = unsafe {
                gpu.device
                    .create_fence(&vk::FenceCreateInfo::default(), None)
            }
            .map_err(|error| format!("vkCreateFence failed with {error:?}"))?;
            frames.push(Frame {
                command_buffer,
                acquired: acquired.take(),
                acquire_done,
                acquire_pending: false,
                done: done.take(),
                submitted: false,
                source: None,
                backing: None,
                scene_reads: Vec::new(),
            });
        }
        drop(wsi);
        // Only a window made with room for rounded corners has a surface
        // that shows alpha; see `appkit::window`.
        let transparent = snapshot.flags & sdl3::video::flags::TRANSPARENT != 0;
        let corners = match transparent {
            true => corners::Corners::new(gpu)
                .inspect_err(|reason| {
                    gpu::trace(format!(
                        "window corners unavailable, presenting opaque: {reason}"
                    ))
                })
                .ok(),
            false => None,
        };
        let mut swapchain = Self {
            window: window as usize,
            gpu,
            surface: surface.take(),
            handle: vk::SwapchainKHR::null(),
            format: vk::Format::UNDEFINED,
            extent,
            requested: extent,
            pending_rebuild: None,
            retired: false,
            images: Vec::new(),
            blitted: Vec::new(),
            presented: Vec::new(),
            composite: None,
            direct: None,
            layers: Vec::new(),
            scene_layers: Vec::new(),
            native_cpu_bases: Vec::new(),
            unsupported_native_dma: Vec::new(),
            transaction_pinned: Vec::new(),
            transaction_retiring_bytes: 0,
            scene_placements: Vec::new(),
            scene_blender: None,
            group_targets: (0..FRAMES_IN_FLIGHT).map(|_| Vec::new()).collect(),
            scene_tile_edge: 0,
            #[cfg(test)]
            last_scene_repaint_pixels: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            last_scene_dispatches: std::sync::atomic::AtomicU64::new(0),
            scene_frames: Vec::new(),
            scene_repaint_needed: false,
            scene_restore_pending: false,
            pending_scene_rollback: None,
            #[cfg(test)]
            injected_restore_busy_cycles: 0,
            #[cfg(test)]
            restore_busy_cycles_seen: 0,
            #[cfg(test)]
            wsi_publishes: 0,
            corners,
            pool: pool.take(),
            frames: frames.take(),
            frame: 0,
            present_mode: vk::PresentModeKHR::FIFO,
            composite_dirty: false,
            composite_shape: None,
            composite_dirty_since: None,
            compositor_batch_delay: Duration::from_millis(4),
            last_wsi_error: None,
            stalled_present_fence: false,
            #[cfg(test)]
            injected_unsignaled_present_fence: false,
            #[cfg(test)]
            skipped_stalled_present_fence: false,
            #[cfg(test)]
            injected_build_vk_error: None,
            #[cfg(test)]
            injected_stage_vk_error: None,
            #[cfg(test)]
            injected_post_scene_record_error: None,
            #[cfg(test)]
            injected_queue_present_error: None,
            #[cfg(test)]
            #[cfg(test)]
            injected_queue_submit_error: None,
            acquired_unsubmitted: None,
            #[cfg(test)]
            injected_recovery_error: None,
            #[cfg(test)]
            injected_build_failure: false,
            #[cfg(test)]
            injected_unavailable_extent: false,
            #[cfg(test)]
            rebuild_attempts: 0,
            #[cfg(test)]
            last_create_old_swapchain: vk::SwapchainKHR::null(),
        };
        // From here the `Drop` above owns everything, including whatever
        // `build` leaves behind when it fails.
        swapchain.build(extent)?;
        Ok(swapchain)
    }

    /// Create the `VkSwapchainKHR` itself, replacing whatever is there.
    ///
    /// Every replacement is built whole before any of it is installed: a
    /// failure part-way leaves the swapchain exactly as it was, rather than a
    /// `self.handle` naming a swapchain this call has already destroyed.
    fn build(&mut self, requested: vk::Extent2D) -> Result<(), String> {
        self.last_wsi_error = None;
        #[cfg(test)]
        {
            self.rebuild_attempts += 1;
            if self.injected_unavailable_extent {
                return Err("the window has no area to present into".into());
            }
        }
        let surface_api = crate::vulkan::surface_api().ok_or("VK_KHR_surface is gone")?;
        let api = self.gpu.swapchain_api().ok_or("VK_KHR_swapchain is gone")?;
        // SAFETY: a physical device and a surface of this instance's.
        let capabilities = unsafe {
            surface_api.get_physical_device_surface_capabilities(self.gpu.physical, self.surface)
        }
        .map_err(|error| format!("vkGetPhysicalDeviceSurfaceCapabilitiesKHR failed: {error:?}"))?;
        let formats = unsafe {
            surface_api.get_physical_device_surface_formats(self.gpu.physical, self.surface)
        }
        .map_err(|error| format!("vkGetPhysicalDeviceSurfaceFormatsKHR failed: {error:?}"))?;
        let chosen = choose_format(&formats).ok_or("the surface reports no format")?;
        let present_modes = unsafe {
            surface_api.get_physical_device_surface_present_modes(self.gpu.physical, self.surface)
        }
        .map_err(|error| format!("vkGetPhysicalDeviceSurfacePresentModesKHR failed: {error:?}"))?;
        let present_mode = choose_present_mode(&present_modes);
        let alpha = composite_alpha(&capabilities, self.corners.is_some());

        // `0xffff_ffff` in either direction is a surface which takes its size
        // from the swapchain rather than the other way round.
        let extent = match capabilities.current_extent.width {
            u32::MAX => vk::Extent2D {
                width: requested.width.clamp(
                    capabilities.min_image_extent.width,
                    capabilities.max_image_extent.width,
                ),
                height: requested.height.clamp(
                    capabilities.min_image_extent.height,
                    capabilities.max_image_extent.height,
                ),
            },
            _ => capabilities.current_extent,
        };
        if extent.width == 0 || extent.height == 0 {
            return Err("the window has no area to present into".into());
        }
        // WSI can change currentExtent without a new AppKit window size.
        // Preserve the old direct image before replacing its dimensions, not
        // just when the last requested size changed.
        if self.direct.is_some() && extent != self.extent {
            let slot = self.frame % self.frames.len();
            if matches!(self.stage_direct_snapshot(false)?, Some(true)) {
                self.gpu.wait(self.frames[slot].done)?;
                self.frames[slot].submitted = false;
                finish_source(self.frames[slot].source.take());
                self.frames[slot].backing.take();
                self.frames[slot].scene_reads.clear();
            }
        }
        // DXVK requests the surface minimum for IMMEDIATE and min+1 for
        // queued modes. Do not expand WSI's queue to hide extra host presents.
        let count = desired_image_count(
            present_mode,
            capabilities.min_image_count,
            capabilities.max_image_count,
        );
        gpu::trace(format!(
            "window swapchain: present modes {present_modes:?}, selected {present_mode:?}, images min={} max={} requested={count}",
            capabilities.min_image_count, capabilities.max_image_count
        ));
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(count)
            .image_format(chosen.format)
            .image_color_space(chosen.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            // The frame arrives as a blit, never as an attachment: the guest
            // rendered into its own drawable long before this.
            .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(capabilities.current_transform)
            .composite_alpha(alpha)
            .present_mode(present_mode)
            .clipped(true)
            .old_swapchain(if self.retired {
                vk::SwapchainKHR::null()
            } else {
                self.handle
            });
        // A prior attempt retired this handle, even if a later allocation
        // failed. The driver may reject an unrelated new swapchain while the
        // retired old one still occupies the native window. Rebuild called
        // idle() first, so it is safe to destroy this old handle now.
        if self.retired && self.handle != vk::SwapchainKHR::null() {
            unsafe { api.destroy_swapchain(self.handle, None) };
            self.handle = vk::SwapchainKHR::null();
        }
        // Supplying a live oldSwapchain retires it even on creation failure.
        if self.handle != vk::SwapchainKHR::null() {
            self.retired = true;
        }
        #[cfg(test)]
        {
            self.last_create_old_swapchain = info.old_swapchain;
        }
        let wsi = self.gpu.wsi.lock().expect("Vulkan WSI");
        #[cfg(test)]
        if let Some(error) = self.injected_build_vk_error.take() {
            self.last_wsi_error = Some(error);
            return Err(format!(
                "injected vkCreateSwapchainKHR failed with {error:?}"
            ));
        }
        let handle = unsafe { api.create_swapchain(&info, None) }.map_err(|error| {
            self.last_wsi_error = Some(error);
            format!("vkCreateSwapchainKHR failed with {error:?}")
        })?;
        let handle = gpu::Guard::new(handle, |handle| {
            // SAFETY: a swapchain of this device's which was never installed,
            // so nothing has acquired an image from it.
            unsafe { api.destroy_swapchain(handle, None) };
        });
        #[cfg(test)]
        if std::mem::take(&mut self.injected_build_failure) {
            return Err("injected failure after oldSwapchain retirement".into());
        }
        // SAFETY: the swapchain was just created by this device.
        let images = unsafe { api.get_swapchain_images(*handle) }.map_err(|error| {
            self.last_wsi_error = Some(error);
            format!("vkGetSwapchainImagesKHR failed with {error:?}")
        })?;
        if crate::stats::enabled() {
            eprintln!(
                "rosemary: Metal: swapchain mode={present_mode:?} images min={} max={} requested={count} actual={}",
                capabilities.min_image_count,
                capabilities.max_image_count,
                images.len()
            );
        }
        // One semaphore per image, made before anything is installed so that a
        // failure half way through leaves neither these nor the swapchain.
        let mut blitted = gpu::GuardedVec::new(|semaphore: &vk::Semaphore| {
            // SAFETY: a semaphore of this device's, never submitted.
            unsafe { self.gpu.device.destroy_semaphore(*semaphore, None) };
        });
        for _ in &images {
            // SAFETY: a well-formed create info on this live device.
            blitted.push(
                unsafe {
                    self.gpu
                        .device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                }
                .map_err(|error| {
                    self.last_wsi_error = Some(error);
                    format!("vkCreateSemaphore failed with {error:?}")
                })?,
            );
        }
        let mut presented = gpu::GuardedVec::new(|present: &PresentFence| {
            // SAFETY: a fence of this device's, never submitted.
            unsafe { self.gpu.device.destroy_fence(present.fence, None) };
        });
        if self.gpu.has_present_fences() {
            for _ in &images {
                // Present fences must be unsignalled when passed to
                // vkQueuePresentKHR.
                let fence = unsafe {
                    self.gpu
                        .device
                        .create_fence(&vk::FenceCreateInfo::default(), None)
                }
                .map_err(|error| {
                    self.last_wsi_error = Some(error);
                    format!("vkCreateFence failed with {error:?}")
                })?;
                presented.push(PresentFence {
                    fence,
                    pending: false,
                });
            }
        }

        drop(wsi);
        // A static Metal layer cannot be redrawn by the host after a
        // resize. Preserve the window's GPU composite across the replacement.
        let keep_fifo = uses_layer_compositor(self.present_mode)
            && uses_layer_compositor(present_mode)
            && self.composite.is_some();
        let keep_direct = self.direct.is_some() && uses_layer_compositor(present_mode);
        let replace_composite = if keep_fifo {
            let old = self.composite.as_ref().expect("existing window snapshot");
            // The corners pass keeps the composite in its own format, never
            // the swapchain's, so compare with what would be made.
            let description =
                Self::composite_description(chosen.format, extent, self.corners.is_some());
            if old.width == extent.width
                && old.height == extent.height
                && old.format == description.format
            {
                None
            } else {
                let replacement = CompositeImage::create(self.gpu, self.window, &description)?;
                let copied = self.gpu.one_shot_owned(
                    (Arc::clone(old), Arc::clone(&replacement)),
                    |device, command_buffer, resources| unsafe {
                        keep_top_left(
                            device,
                            command_buffer,
                            &resources.0.lease.image,
                            &resources.1.lease.image,
                        );
                    },
                );
                copied?;
                Some(replacement)
            }
        } else {
            None
        };

        // All fallible allocations and copies completed.
        let blitted = blitted.take();
        let presented = presented.take();
        let old = std::mem::replace(&mut self.handle, handle.take());
        if old != vk::SwapchainKHR::null() {
            // SAFETY: retired by the creation above, and every submission
            // touching its images has been waited for by the caller.
            unsafe { api.destroy_swapchain(old, None) };
        }
        // The old swapchain's images are gone with it, and so is the meaning
        // of a semaphore indexed by one.
        for semaphore in std::mem::replace(&mut self.blitted, blitted) {
            // SAFETY: `idle` waited either the presentation fences or the
            // queue before the swapchain these belonged to was destroyed.
            unsafe { self.gpu.device.destroy_semaphore(semaphore, None) };
        }
        for present in std::mem::replace(&mut self.presented, presented) {
            // SAFETY: `idle` waited every pending use of this fence.
            unsafe { self.gpu.device.destroy_fence(present.fence, None) };
        }
        self.requested = requested;
        self.acquired_unsubmitted = None;
        self.retired = false;
        self.present_mode = present_mode;
        self.composite_dirty = keep_fifo || keep_direct;
        self.composite_shape = None;
        self.composite_dirty_since = (keep_fifo || keep_direct).then(Instant::now);
        self.format = chosen.format;
        self.extent = extent;
        self.images = images;
        if !keep_fifo {
            self.composite = None;
        } else if let Some(replacement) = replace_composite {
            self.composite = Some(replacement);
        }
        // A surface that cannot show premultiplied alpha is presented
        // opaque, which the pass would only get wrong.
        if alpha != vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED {
            self.corners = None;
        }
        if let Some(corners) = self.corners.as_mut() {
            corners.forget_output();
        }
        Ok(())
    }

    /// Finish the source of every submitted frame whose fence has signalled,
    /// and say whether there was one.
    fn retire_copied(&mut self) -> bool {
        let mut retired = false;
        // Direct WSI may resume after CA group opacity disappears.
        // Retire optional GPU targets only after their command fence signals.
        if !self
            .scene_placements
            .iter()
            .any(|p| !p.opacity_groups.is_empty())
        {
            for (index, frame) in self.frames.iter().enumerate() {
                if !frame.submitted || self.gpu.fence_signaled(frame.done).unwrap_or(false) {
                    self.group_targets[index].clear();
                }
            }
        }
        for frame in &mut self.frames {
            if (frame.source.is_none() && frame.backing.is_none() && frame.scene_reads.is_empty())
                || !frame.submitted
            {
                continue;
            }
            // Both an Objective-C drawable and a ref-counted store image are
            // released only after their last GPU read has completed.
            if self.gpu.fence_signaled(frame.done).unwrap_or(false) {
                finish_source(frame.source.take());
                frame.backing.take();
                frame.scene_reads.clear();
                retired = true;
            }
        }
        retired
    }

    /// The fences of the submitted frames still holding a source.
    fn copying(&self) -> Vec<vk::Fence> {
        self.frames
            .iter()
            .filter(|frame| {
                frame.submitted
                    && (frame.source.is_some()
                        || frame.backing.is_some()
                        || !frame.scene_reads.is_empty())
            })
            .map(|frame| frame.done)
            .collect()
    }

    /// Wait for every frame in flight and rebuild at `extent`.
    fn recover_unsubmitted_acquire(&mut self) -> Result<(), String> {
        let Some((semaphore, fence)) = self.acquired_unsubmitted else {
            return Ok(());
        };
        #[cfg(test)]
        if let Some(error) = self.injected_recovery_error.take() {
            return Err(format!("injected recovery submit failure: {error:?}"));
        }
        self.gpu.consume_acquired_semaphore(semaphore, fence)?;
        // A successful recovery consumed the binary semaphore, but WSI
        // still owns its acquired image until the swapchain is retired.
        self.acquired_unsubmitted = None;
        self.rebuild(self.requested)
    }

    fn rebuild(&mut self, extent: vk::Extent2D) -> Result<(), String> {
        self.idle();
        self.build(extent)
    }

    /// Block until nothing this swapchain gave the queue is still running.
    ///
    /// Presentation fences cover the presentation engine's access to the
    /// image and wait semaphore. On an older device, only making the queue
    /// idle supplies that guarantee.
    fn idle(&mut self) {
        // At least one present omitted its maintenance1 fence because an old
        // fence was still pending. Even if that *old* fence signals later, it
        // cannot cover subsequent fence-less presentations. Queue idle is
        // necessary to retire their WSI use of images and semaphores safely.
        if !self.gpu.has_present_fences() || self.stalled_present_fence {
            self.gpu.wait_queue_idle();
            self.stalled_present_fence = false;
            for present in &mut self.presented {
                present.pending = false;
            }
            for frame in &mut self.frames {
                frame.submitted = false;
                finish_source(frame.source.take());
                frame.backing.take();
                frame.scene_reads.clear();
            }
            return;
        }
        let submitted = self
            .frames
            .iter()
            .filter(|frame| frame.submitted)
            .map(|frame| frame.done);
        let presented = self
            .presented
            .iter()
            .filter(|present| present.pending)
            .map(|present| present.fence);
        let fences = submitted.chain(presented).collect::<Vec<_>>();
        if !fences.is_empty() {
            // SAFETY: each fence belongs to this device and is associated
            // with an operation which was successfully queued.
            let _ = unsafe { self.gpu.device.wait_for_fences(&fences, true, u64::MAX) };
        }
        for frame in &mut self.frames {
            frame.submitted = false;
            finish_source(frame.source.take());
            frame.backing.take();
            frame.scene_reads.clear();
        }
        for present in &mut self.presented {
            present.pending = false;
        }
    }

    /// The image layers accumulate into, made on the first frame that needs
    /// one and kept in `GENERAL` for the same reason every other image in
    /// this crate is.
    ///
    /// With the corners pass, the composite is one the pass can read.
    fn composite_description(
        _format: vk::Format,
        extent: vk::Extent2D,
        corners_enabled: bool,
    ) -> ImageDescription {
        if corners_enabled {
            corners::storage(extent.width, extent.height)
        } else {
            ImageDescription {
                image_type: vk::ImageType::TYPE_2D,
                view_type: vk::ImageViewType::TYPE_2D,
                format: vk::Format::R8G8B8A8_UNORM,
                width: extent.width,
                height: extent.height,
                depth: 1,
                levels: 1,
                layers: 1,
                samples: 1,
                usage: gpu::USAGE_SHADER_WRITE | gpu::USAGE_SHADER_READ,
                cube: false,
                components: vk::ComponentMapping::default(),
            }
        }
    }

    fn composite(&mut self) -> Result<vk::Image, String> {
        if self.composite.is_none() {
            let description =
                Self::composite_description(self.format, self.extent, self.corners.is_some());
            let image = CompositeImage::create(self.gpu, self.window, &description)?;
            // A fresh image's contents are undefined, and the frame that
            // brings the composite into existence has only the one layer
            // that asked for it - so everywhere the other layers have not
            // reached yet would be whatever the allocation held.
            let clear = vk::ClearColorValue { float32: [0.0; 4] };
            let range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            self.gpu
                .one_shot_owned(Arc::clone(&image), |device, command_buffer, owner| {
                    // SAFETY: recording into a command buffer the call owns; the
                    // image is in GENERAL from creation.
                    unsafe {
                        device.cmd_clear_color_image(
                            command_buffer,
                            owner.image,
                            vk::ImageLayout::GENERAL,
                            &clear,
                            &[range],
                        )
                    };
                })?;
            self.composite = Some(image);
        }
        Ok(self.composite.as_ref().expect("just made").image)
    }

    /// Prepare one frame and reserve its WSI semaphore before releasing the
    /// state lock for the blocking image acquisition.
    fn prepare_present(
        &mut self,
        regions: Option<&[Region]>,
        nonblocking: bool,
    ) -> Result<Option<(PreparedPresent, AcquireRequest)>, String> {
        // A direct layer was admitted as the whole of what the window shows
        // (`fast_direct_scene`), so it needs no accumulator and goes straight
        // into the image the presentation engine hands back.
        // The corners pass reads the composite, so a window with it always
        // has one.
        let direct = self.direct.is_some() && regions.is_none() && self.corners.is_none();
        let composite = match direct {
            true => None,
            false => Some(self.composite()?),
        };
        if let Some(corners) = self.corners.as_mut() {
            corners.prepare(self.extent.width, self.extent.height)?;
        }

        let in_flight = frame_slots(self.present_mode);
        let slot = self.frame % in_flight;
        let this = &self.frames[slot];
        let (command_buffer, acquired, acquire_done, done) = (
            this.command_buffer,
            this.acquired,
            this.acquire_done,
            this.done,
        );
        // SAFETY: a fence of this device's, signalled at creation or by the
        // submission this slot last made.
        if this.submitted {
            let timeout = if nonblocking { 0 } else { u64::MAX };
            if !self.gpu.wait_fence_timeout(done, timeout)? {
                return Ok(None);
            }
        }
        self.frames[slot].submitted = false;
        finish_source(self.frames[slot].source.take());
        self.frames[slot].backing.take();
        self.frames[slot].scene_reads.clear();
        if self.frames[slot].acquire_pending {
            // WSI may signal an acquisition fence after a successful
            // vkAcquireNextImageKHR returns. Never stall AppKit while it
            // waits; retry this frame slot at a subsequent pump instead.
            let timeout = if nonblocking { 0 } else { u64::MAX };
            if !self.gpu.wait_fence_timeout(acquire_done, timeout)? {
                return Ok(None);
            }
            self.frames[slot].acquire_pending = false;
            self.gpu.fence_result(
                unsafe { self.gpu.device.reset_fences(&[acquire_done]) },
                "vkResetFences",
            )?;
        }
        Ok(Some((
            PreparedPresent {
                slot,
                command_buffer,
                acquired,
                done,
                composite,
            },
            AcquireRequest {
                handle: self.handle,
                semaphore: acquired,
                fence: acquire_done,
            },
        )))
    }

    /// Finish a prepared frame after the presentation engine returns an image.
    fn finish_present(
        &mut self,
        frame: &appkit::view::Frame,
        source: PresentSource,
        regions: Option<&[Region]>,
        prepared: PreparedPresent,
        acquired: Result<Option<(u32, bool)>, vk::Result>,
    ) -> Result<Option<vk::Result>, String> {
        // An unsuccessful acquire did not submit this frame. Report its
        // raw Vulkan status to the caller so it can propagate DEVICE_LOST
        // across the logical device and mark SURFACE_LOST window-local.
        let acquired = match acquired {
            Ok(acquired) => acquired,
            Err(error) => return Ok(Some(error)),
        };
        let Some((index, _suboptimal)) = acquired else {
            // The swapchain was rebuilt underneath the acquisition. The frame
            // the guest drew is gone, which is what a resize costs; the next
            // one lands in the new swapchain.
            self.rebuild(self.requested)?;
            return Ok(None);
        };
        let PreparedPresent {
            slot,
            command_buffer,
            acquired,
            done,
            composite,
        } = prepared;
        self.frames[slot].acquire_pending = true;
        // Until a successful queue submission consumes the acquired
        // semaphore, all subsequent errors must retire the WSI swapchain.
        // This semaphore must be consumed by a queue wait even if the
        // subsequent image-copy queue submission fails.
        self.acquired_unsubmitted = Some((prepared.acquired, prepared.done));
        let target = self.images[index as usize];
        // Safe to signal: this image has just come back from the
        // presentation engine, so the presentation which last waited on this
        // semaphore is finished.
        let blitted = self.blitted[index as usize];
        let present_fence = if let Some(present) = self.presented.get_mut(index as usize) {
            #[cfg(test)]
            let forced_unsignaled = std::mem::take(&mut self.injected_unsignaled_present_fence);
            #[cfg(not(test))]
            let forced_unsignaled = false;
            if present.pending
                && (forced_unsignaled || !self.gpu.wait_fence_timeout(present.fence, 0)?)
            {
                // Successful acquisition means WSI returned this image, but
                // some implementations do not signal its old maintenance1
                // fence promptly. Do not block the compositor. Keep that
                // fence untouched and submit the new present without a fence.
                // Queue idle handles the fallback on rebuild/teardown.
                self.stalled_present_fence = true;
                #[cfg(test)]
                {
                    self.skipped_stalled_present_fence = true;
                }
                None
            } else {
                present.pending = false;
                // SAFETY: fresh or signaled; no old queued use remains.
                unsafe { self.gpu.device.reset_fences(&[present.fence]) }
                    .map_err(|error| format!("vkResetFences failed with {error:?}"))?;
                Some(present.fence)
            }
        } else {
            None
        };

        let record_timer = crate::stats::timer();
        let device = &self.gpu.device;
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: this slot's fence was waited for above, so nothing is
        // reading the command buffer.
        unsafe {
            device
                .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(|error| format!("vkResetCommandBuffer failed with {error:?}"))?;
            device
                .begin_command_buffer(command_buffer, &begin)
                .map_err(|error| format!("vkBeginCommandBuffer failed with {error:?}"))?;
        }

        // SAFETY: recording into a command buffer this call owns; every image
        // named below belongs to this device.
        unsafe {
            prepare_source(device, command_buffer, source.image);
            let whole = [
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: self.extent.width as i32,
                    y: self.extent.height as i32,
                    z: 1,
                },
            ];
            // The swapchain image is display-sized even if its NSWindow is
            // not. Clear it before copying the *clipped* accumulator or layer:
            // an overlay blended on the GPU can otherwise escape the frame.
            // Transfer clear is guaranteed to cover stale pixels from prior
            // presents; the compositor's excluded region is opaque black.
            let logical = logical_image_clip(frame.window, self.extent);
            let restricted = logical != Some(whole);
            transition_to_transfer_dst(device, command_buffer, target);
            if restricted {
                let black = vk::ClearColorValue {
                    float32: [0.0, 0.0, 0.0, 1.0],
                };
                device.cmd_clear_color_image(
                    command_buffer,
                    target,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &black,
                    &[vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    }],
                );
                // Transfer write -> transfer write/read within this command
                // buffer is ordered, but the barrier expresses the dependency.
                barrier(
                    device,
                    command_buffer,
                    target,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::TRANSFER_WRITE,
                );
            }
            match composite {
                // The layer's own rectangle of the accumulator, then the
                // whole accumulator into the image being presented.
                Some(composite) => {
                    // A layer wholly off the edge of its window contributes
                    // nothing; what the composite already holds is still
                    // what the window shows, so it is presented unchanged.
                    match regions {
                        Some(regions) => {
                            for region in regions {
                                let part = appkit::view::Frame {
                                    x: region.x,
                                    y: region.y,
                                    width: region.width.max(0) as u32,
                                    height: region.height.max(0) as u32,
                                    ..*frame
                                };
                                if let Some(into) = clamp(&part, self.extent) {
                                    blit(
                                        device,
                                        command_buffer,
                                        source.image,
                                        source_of(into, frame, (source.width, source.height)),
                                        composite,
                                        vk::ImageLayout::GENERAL,
                                        into,
                                    );
                                }
                            }
                        }
                        None => {
                            if let Some(into) = clamp(frame, self.extent) {
                                blit(
                                    device,
                                    command_buffer,
                                    source.image,
                                    source_of(into, frame, (source.width, source.height)),
                                    composite,
                                    vk::ImageLayout::GENERAL,
                                    into,
                                );
                            }
                        }
                    }
                    let shown = match &self.corners {
                        Some(corners) => {
                            let shape = self.composite_shape.unwrap_or_else(|| {
                                corners::Shape::of(
                                    frame.window,
                                    self.extent.width,
                                    self.extent.height,
                                )
                            });
                            let image = self.composite.as_ref().expect("made above");
                            corners.record(command_buffer, slot, image, shape)
                        }
                        None => {
                            // The blit above must land before the one below
                            // reads it.
                            barrier(
                                device,
                                command_buffer,
                                composite,
                                vk::ImageLayout::GENERAL,
                                vk::ImageLayout::GENERAL,
                                vk::AccessFlags::TRANSFER_WRITE,
                                vk::AccessFlags::TRANSFER_READ,
                            );
                            composite
                        }
                    };
                    if let Some(clipped) = logical {
                        blit(
                            device,
                            command_buffer,
                            shown,
                            clipped,
                            target,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            clipped,
                        );
                    }
                }
                None => {
                    if let Some((from, into)) = direct_blit_to(
                        frame,
                        self.extent,
                        appkit::window::presentation_clip(frame.window),
                        (source.width, source.height),
                    ) {
                        blit(
                            device,
                            command_buffer,
                            source.image,
                            from,
                            target,
                            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                            into,
                        );
                    }
                }
            }
            transition_to_present(device, command_buffer, target);
            device
                .end_command_buffer(command_buffer)
                .map_err(|error| format!("vkEndCommandBuffer failed with {error:?}"))?;
        }
        crate::stats::finish_timer(crate::stats::Counter::PresentRecordMicros, record_timer);

        #[cfg(test)]
        let injected_submit = self.injected_queue_submit_error.take();
        #[cfg(not(test))]
        let injected_submit: Option<vk::Result> = None;
        crate::stats::timed(crate::stats::Counter::PresentSubmitMicros, || {
            if let Some(error) = injected_submit {
                return Err(format!("injected vkQueueSubmit failure: {error:?}"));
            }
            self.gpu.submit_synchronized(
                command_buffer,
                done,
                &[(acquired, vk::PipelineStageFlags::TRANSFER)],
                &[blitted],
            )
        })?;
        // The acquire semaphore is consumed only after vkQueueSubmit succeeds.
        self.acquired_unsubmitted = None;
        self.frames[slot].submitted = true;
        self.frames[slot].source = None;
        self.frames[slot].backing = None;
        self.frame = self.frame.wrapping_add(1);

        crate::stats::count(crate::stats::Counter::QueuePresentCalls);
        let result = crate::stats::timed(crate::stats::Counter::QueuePresentMicros, || {
            self.gpu.present(self.handle, index, blitted, present_fence)
        });
        // The fault-injection test lets real Vulkan finish its queue operation,
        // then replaces its result to exercise the post-acquire error path.
        #[cfg(test)]
        let result = self.injected_queue_present_error.take().map_or(result, Err);
        // Exclude device loss and allocation failures from present-fence
        // waits. The Gamescope WSI opt-in can also omit a signal on ordinary
        // failed presents; keep that driver quirk out of normal WSI paths.
        static GAMESCOPE_WSI: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
            std::env::var("ENABLE_GAMESCOPE_WSI").as_deref() == Ok("1")
        });
        if present_fence.is_some() && present_fence_will_signal(&result, *GAMESCOPE_WSI) {
            self.presented[index as usize].pending = true;
        }
        match result {
            Ok(suboptimal) => {
                if suboptimal {
                    // The image still reached the window; the next frame is
                    // the one that gets a swapchain matching it.
                    self.rebuild(self.requested)?;
                }
                Ok(None)
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.rebuild(self.requested)?;
                Ok(None)
            }
            Err(error) => {
                // The blit submission owns its source until the frame fence,
                // but the compositor worker must see terminal errors and stop.
                gpu::trace(format!("vkQueuePresentKHR failed with {error:?}"));
                Ok(Some(error))
            }
        }
    }
}

/// Bound the wait for a compositor image. A minimized X11 window may stop
/// returning images entirely; UINT64_MAX would block window teardown while
/// the worker held the presentation mutex. Timeout leaves the acquisition
/// semaphore untouched, and the dirty accumulator remains pending.
/// Waits up to a frame for an image when `wait`, and otherwise only polls:
/// the caller holds the window's presentation lock throughout.
fn acquire_compositor_image(
    gpu: &'static Gpu,
    request: AcquireRequest,
    wait: bool,
) -> CompositorAcquire {
    const TIMEOUT_NS: u64 = 20_000_000;
    let Some(api) = gpu.swapchain_api() else {
        return CompositorAcquire::Terminal(vk::Result::ERROR_DEVICE_LOST);
    };
    let timeout = if wait { TIMEOUT_NS } else { 0 };
    // SAFETY: the live window swapchain and its unused acquisition semaphore
    // remain owned by the worker while it holds the presentation mutex.
    classify_compositor_acquire(unsafe {
        api.acquire_next_image(request.handle, timeout, request.semaphore, request.fence)
    })
}

/// Copy what `old` held into `new` unscaled, from the top-left corner as the
/// window's store keeps it across a resize, and clear the rest: stretched,
/// the stale frame would be presented distorted until every layer redraws.
///
/// # Safety
///
/// Recording into `command_buffer`, with both images of `device`'s and in
/// `GENERAL`.
unsafe fn keep_top_left(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    old: &GpuImage,
    new: &GpuImage,
) {
    let range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    let kept = [
        vk::Offset3D { x: 0, y: 0, z: 0 },
        vk::Offset3D {
            x: old.width.min(new.width).max(1) as i32,
            y: old.height.min(new.height).max(1) as i32,
            z: 1,
        },
    ];
    unsafe {
        device.cmd_clear_color_image(
            command_buffer,
            new.image,
            vk::ImageLayout::GENERAL,
            &vk::ClearColorValue { float32: [0.0; 4] },
            &[range],
        );
        barrier(
            device,
            command_buffer,
            new.image,
            vk::ImageLayout::GENERAL,
            vk::ImageLayout::GENERAL,
            vk::AccessFlags::TRANSFER_WRITE,
            vk::AccessFlags::TRANSFER_WRITE,
        );
        blit(
            device,
            command_buffer,
            old.image,
            kept,
            new.image,
            vk::ImageLayout::GENERAL,
            kept,
        );
    }
}

/// How long the compositor worker sleeps, unlocked, after its `timeouts`th
/// acquisition in a row that found no image.
fn compositor_acquire_backoff(timeouts: u32) -> Duration {
    Duration::from_millis(1 << timeouts.saturating_sub(1).min(5))
}

fn classify_compositor_acquire(outcome: Result<(u32, bool), vk::Result>) -> CompositorAcquire {
    match outcome {
        Ok((index, suboptimal)) => CompositorAcquire::Ready(Some((index, suboptimal))),
        Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => CompositorAcquire::Ready(None),
        Err(vk::Result::TIMEOUT | vk::Result::NOT_READY) => CompositorAcquire::TimedOut,
        Err(error) => CompositorAcquire::Terminal(error),
    }
}

#[cfg(test)]
fn acquire_image(
    gpu: &'static Gpu,
    request: AcquireRequest,
    nonblocking: bool,
) -> Result<Option<(u32, bool)>, vk::Result> {
    let api = gpu
        .swapchain_api()
        .ok_or(vk::Result::ERROR_EXTENSION_NOT_PRESENT)?;
    // SAFETY: a live swapchain and semaphore of this device's; no fence, so
    // the semaphore is the only signal.
    match unsafe {
        api.acquire_next_image(
            request.handle,
            if nonblocking { 0 } else { u64::MAX },
            request.semaphore,
            request.fence,
        )
    } {
        Ok((index, suboptimal)) => Ok(Some((index, suboptimal))),
        Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Nothing in this runtime destroys an `SDL_Window` today - `-[NSWindow
/// close]` orders it out and the window stays in the list - so a swapchain is
/// only dropped if one is taken out of the map. Teardown lives here anyway
/// rather than in a function nothing calls: whichever way a swapchain stops
/// being reachable, the surface, the semaphores and the pool go with it, and
/// the order below is the one Vulkan requires.
impl Drop for Swapchain {
    fn drop(&mut self) {
        self.idle();
        // If both queue submissions failed, the WSI acquire may still be
        // scheduling its semaphore signal. The separate acquire fence proves
        // completion of that signal; queue idle alone does not.
        if let Some((semaphore, _)) = self.acquired_unsubmitted
            && let Some(frame) = self.frames.iter().find(|f| f.acquired == semaphore)
            && let Err(reason) = self.gpu.wait(frame.acquire_done)
        {
            // A device-lost wait counts as completed for resource
            // lifetime under Vulkan's lost-device rules.
            gpu::trace(format!("unconsumed WSI acquire teardown: {reason}"));
        }
        let device = &self.gpu.device;
        // The swapchain first: its images are what the semaphores below were
        // signalled for, and a surface outlives the swapchain made on it.
        if self.handle != vk::SwapchainKHR::null()
            && let Some(api) = self.gpu.swapchain_api()
        {
            // SAFETY: teardown has finished references to this swapchain.
            unsafe { api.destroy_swapchain(self.handle, None) };
        }
        for frame in &self.frames {
            // SAFETY: objects of this device's, with nothing in flight after
            // the wait above.
            unsafe {
                device.destroy_semaphore(frame.acquired, None);
                device.destroy_fence(frame.acquire_done, None);
                device.destroy_fence(frame.done, None);
            }
        }
        for semaphore in std::mem::take(&mut self.blitted) {
            // SAFETY: as above, and the swapchain whose presentations waited
            // on these is destroyed first.
            unsafe { device.destroy_semaphore(semaphore, None) };
        }
        for present in std::mem::take(&mut self.presented) {
            // SAFETY: the wait above retired every pending presentation.
            unsafe { device.destroy_fence(present.fence, None) };
        }
        // SAFETY: the pool owns the command buffers, which are freed with it.
        unsafe { device.destroy_command_pool(self.pool, None) };
        self.composite = None;
        if let Some(instance) = crate::vulkan::instance_handle() {
            sdl3::video::vulkan::destroy_surface(instance, self.surface.as_raw());
        }
    }
}
