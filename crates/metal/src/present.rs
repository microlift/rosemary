use ash::vk;
use objc::object::Id;
use objc::sel::Sel;

use crate::constants::*;
use crate::gpu::{self, Gpu, GpuImage, ImageDescription};
use crate::object::{self, methods};
use crate::resource::Texture;

std::thread_local! {
    /// Set on a device's presentation worker.
    static ON_PRESENT_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One device's presentation, in the order its drawables were handed over.
///
/// Presenting waits: for a swapchain image to come back from the presentation
/// engine, for the copy into the last one to finish, or for a whole readback.
/// That belongs on a thread of its own, not on the guest's committing thread -
/// where it also held every other thread's commit behind it - and not on the
/// completion-handler thread a guest's `-present` in a scheduled handler runs
/// on. The drawable ring bounds the queue: a guest cannot be vended a fourth
/// drawable before one of three has been presented.
pub(crate) struct Presenter {
    pending: std::sync::Mutex<std::collections::VecDeque<usize>>,
    ready: std::sync::Condvar,
    worker: std::sync::Once,
    /// Resource-completion notifications observed across detached batches.
    progress: std::sync::atomic::AtomicU64,
}

impl Presenter {
    pub(crate) fn new() -> Self {
        Self {
            pending: std::sync::Mutex::new(std::collections::VecDeque::new()),
            ready: std::sync::Condvar::new(),
            worker: std::sync::Once::new(),
            progress: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Retain deferred ordering, but never sleep past a submission made
    /// while the dispatcher was processing the detached batch.
    fn requeue_deferred(
        &self,
        deferred: std::collections::VecDeque<usize>,
        delay: std::time::Duration,
        observed_progress: u64,
    ) -> bool {
        let mut pending = self.pending.lock().expect("presentation queue");
        let new_work = !pending.is_empty();
        prepend_deferred(&mut pending, deferred);
        if !new_work
            && self.progress.load(std::sync::atomic::Ordering::Acquire) == observed_progress
        {
            // Queue insertion and completion both take this mutex before
            // notifying. A completion during dispatch changes the generation,
            // so it cannot be lost before the wait starts.
            let _wait = self
                .ready
                .wait_timeout(pending, delay)
                .expect("deferred presentation wait");
        }
        new_work
    }

    /// DXVK signals resource waiters on submission completion. The fallback
    /// timer remains bounded, but an unlocked presentation permit can advance
    /// a deferred drawable without waiting for the next 1 ms timeout.
    pub(crate) fn notify_progress(&self) {
        // The worker's own release of a window cannot unblock what it just
        // deferred. Counting it would skip the retry delay every time, and
        // the worker would spin on a frame slot whose fence is pending.
        if ON_PRESENT_WORKER.get() {
            return;
        }
        let _pending = self.pending.lock().expect("presentation queue");
        self.progress
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        self.ready.notify_one();
    }
}

pub(crate) struct Drawable {
    pub gpu: &'static Gpu,
    pub texture: Id,
    pub layer: Id,
    /// The buffer the frame is read back into, kept for the life of the
    /// drawable. Making one per frame is a `vkCreateBuffer` and a range of the
    /// heap for every frame, and the drawable's size never changes: a layer
    /// which resizes vends a new drawable.
    readback: Option<crate::gpu::GpuBuffer>,
}

/// Three drawable images let the guest record ahead without ever writing an
/// image which an earlier presentation still consumes.
const DRAWABLES_IN_FLIGHT: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotState {
    Available,
    Vended,
    Submitted,
}

struct CachedSlot {
    drawable: usize,
    state: SlotState,
}

impl CachedSlot {
    fn submit(&mut self) -> bool {
        if self.state != SlotState::Vended {
            return false;
        }
        self.state = SlotState::Submitted;
        true
    }

    fn complete(&mut self) {
        self.state = SlotState::Available;
    }
}

/// The drawable ring a layer currently vends, remade when its description
/// changes. Cache references keep every slot alive while it is reusable.
struct Cached {
    layer: usize,
    width: u32,
    height: u32,
    pixel_format: u64,
    stale: bool,
    slots: Vec<CachedSlot>,
}

type DrawableCache = (std::sync::Mutex<Vec<Cached>>, std::sync::Condvar);

static CACHE: DrawableCache = (std::sync::Mutex::new(Vec::new()), std::sync::Condvar::new());

/// `-[CAMetalLayer nextDrawable]`.
pub(crate) unsafe extern "C" fn next_drawable(receiver: Id, _sel: Sel) -> Id {
    let device = send_id(receiver, "device");
    let device = if device.is_null() {
        // SAFETY: called on the Metal framework's own thread.
        unsafe { crate::MTLCreateSystemDefaultDevice() }
    } else {
        device
    };
    let Some(gpu) = crate::gpu_of_device(device) else {
        return std::ptr::null_mut();
    };
    let size = send_size(receiver, "drawableSize");
    let (mut width, mut height) = (size.0 as u32, size.1 as u32);
    if width == 0 || height == 0 {
        // A layer with no drawable size of its own is the size of the view
        // holding it, which is what Core Animation gives it; a layer no view
        // holds falls back to the window, as it did before layers could be
        // placed at all.
        let Some((w, h)) = layer_frame(receiver)
            .map(|frame| (frame.width, frame.height))
            .or_else(window_size)
        else {
            return std::ptr::null_mut();
        };
        width = w;
        height = h;
    }
    let pixel_format = send_u64(receiver, "pixelFormat");
    let pixel_format = if pixel_format == MTL_PIXEL_FORMAT_INVALID {
        MTL_PIXEL_FORMAT_BGRA8_UNORM
    } else {
        pixel_format
    };

    let format = gpu::pixel_format(pixel_format);
    if format == vk::Format::UNDEFINED {
        gpu::trace(format!(
            "CAMetalLayer has no Vulkan form for MTLPixelFormat {pixel_format}"
        ));
        return std::ptr::null_mut();
    }
    // Before the cache is locked, which finishing a copied source needs.
    crate::swapchain::retire_copied();
    let mut retired = retire_stale(&mut CACHE.0.lock().expect("drawable cache"));
    release_slots(&mut retired);
    let mut cache = CACHE.0.lock().expect("drawable cache");
    for entry in cache
        .iter_mut()
        .filter(|entry| entry.layer == receiver as usize)
    {
        if (entry.width, entry.height, entry.pixel_format) != (width, height, pixel_format) {
            entry.stale = true;
        }
    }
    let current = cache.iter().position(|entry| {
        !entry.stale
            && entry.layer == receiver as usize
            && (entry.width, entry.height, entry.pixel_format) == (width, height, pixel_format)
    });
    let at = match current {
        Some(at) => at,
        None => {
            let mut slots = Vec::with_capacity(DRAWABLES_IN_FLIGHT);
            for _ in 0..DRAWABLES_IN_FLIGHT {
                let Some(drawable) =
                    make_drawable(gpu, receiver, width, height, pixel_format, format)
                else {
                    release_slots(&mut slots);
                    return std::ptr::null_mut();
                };
                slots.push(CachedSlot {
                    drawable: drawable as usize,
                    state: SlotState::Available,
                });
            }
            cache.push(Cached {
                layer: receiver as usize,
                width,
                height,
                pixel_format,
                stale: false,
                slots,
            });
            cache.len() - 1
        }
    };

    let matches = |entry: &Cached| {
        !entry.stale
            && entry.layer == receiver as usize
            && (entry.width, entry.height, entry.pixel_format) == (width, height, pixel_format)
    };
    debug_assert!(matches(&cache[at]));
    let Some((_cache, drawable)) = vend(
        &CACHE,
        cache,
        matches,
        || crate::swapchain::wait_copied(RECLAIM_POLL),
        RECLAIM_POLL,
    ) else {
        return std::ptr::null_mut();
    };
    // Under the lock, which keeps the slot from being reclaimed as dropped
    // before it is retained. The cache's own reference is not the caller's
    // to spend.
    objc::arc::autorelease(objc::arc::retain(drawable as Id))
}

/// Mark a slot of the ring `matches` picks out vended, waiting for one while
/// the ring is full, and hand it back with the lock still held. `None` once
/// the ring has been retired. `retire` returns presented drawables whose
/// copies are done, saying whether there was a copy to retire or wait for.
fn vend<'a>(
    pair: &'a DrawableCache,
    mut cache: std::sync::MutexGuard<'a, Vec<Cached>>,
    matches: impl Fn(&Cached) -> bool,
    mut retire: impl FnMut() -> bool,
    poll: std::time::Duration,
) -> Option<(std::sync::MutexGuard<'a, Vec<Cached>>, usize)> {
    // Whether the swapchain had no copy left to retire or wait for, so that
    // only another thread can free a slot. Not on the first pass: a copy may
    // have finished, or still be running, since the caller last retired.
    let mut idle = false;
    loop {
        // Every pass checks the ring under the lock it may then wait with, so
        // a slot returned while the lock was given up is found here rather
        // than waited for.
        reclaim_dropped(&mut cache);
        // Looked up again each time round: the lock is given up below, and
        // the ring may have been retired meanwhile.
        let entry = cache.iter_mut().find(|entry| matches(entry))?;
        if let Some(slot) = entry
            .slots
            .iter_mut()
            .find(|slot| slot.state == SlotState::Available)
        {
            slot.state = SlotState::Vended;
            crate::stats::count(crate::stats::Counter::DrawableVended);
            let drawable = slot.drawable;
            return Some((cache, drawable));
        }
        crate::stats::count(crate::stats::Counter::DrawablePressure);
        if idle {
            // Timed, because a guest dropping a drawable it never presented
            // does not signal the condition variable.
            cache = pair.1.wait_timeout(cache, poll).expect("drawable cache").0;
        }
        // A presented drawable whose copy is done comes back through the
        // swapchain, which locks the cache to return it.
        drop(cache);
        idle = !retire();
        cache = pair.0.lock().expect("drawable cache");
    }
}

/// How often a `nextDrawable` waiting on a full ring looks for a drawable the
/// guest has let go of, which is also as long as it waits on a copy at a time.
const RECLAIM_POLL: std::time::Duration = std::time::Duration::from_millis(2);

/// Return to its ring every vended drawable the guest no longer holds.
///
/// Core Animation takes back a drawable released without being presented, and
/// Unity relies on it: its main thread fetches drawables for frames it then
/// abandons. Holding those as vended starves the ring, and the frame that
/// would retire the last presented one can never get a drawable to draw into.
/// The cache's own reference is the one left when nobody else holds it; an
/// autoreleased one still counts as held until its pool drains.
fn reclaim_dropped(cache: &mut [Cached]) {
    for slot in cache.iter_mut().flat_map(|entry| entry.slots.iter_mut()) {
        if slot.state == SlotState::Vended && objc::arc::retain_count(slot.drawable as Id) <= 1 {
            slot.state = SlotState::Available;
            crate::stats::count(crate::stats::Counter::DrawableReclaimed);
        }
    }
}

fn make_drawable(
    gpu: &'static Gpu,
    layer: Id,
    width: u32,
    height: u32,
    pixel_format: u64,
    format: vk::Format,
) -> Option<Id> {
    let image = gpu
        .create_image(&ImageDescription {
            image_type: vk::ImageType::TYPE_2D,
            view_type: vk::ImageViewType::TYPE_2D,
            format,
            width,
            height,
            depth: 1,
            levels: 1,
            layers: 1,
            samples: 1,
            usage: gpu::USAGE_RENDER_TARGET | gpu::USAGE_SHADER_READ,
            cube: false,
            components: vk::ComponentMapping::default(),
        })
        .map_err(|reason| gpu::trace(format!("drawable image creation failed: {reason}")))
        .ok()?;
    let texture = crate::resource::drawable_texture(gpu, image, pixel_format);
    if texture.is_null() {
        return None;
    }
    let drawable = object::make(
        c"RosemaryCAMetalDrawable",
        Drawable {
            gpu,
            texture,
            layer: objc::arc::retain(layer),
            readback: None,
        },
    );
    if drawable.is_null() {
        return None;
    }
    objc::arc::retain(texture);
    gpu::milestone("drawable");
    Some(drawable)
}

fn release_slots(slots: &mut Vec<CachedSlot>) {
    for slot in slots.drain(..) {
        let drawable = slot.drawable as Id;
        // SAFETY: a drawable made above, whose cache reference is being given up.
        let texture = unsafe { object::body::<Drawable>(drawable) }.texture;
        objc::arc::release(texture);
        objc::arc::release(drawable);
    }
}

/// Take out the rings nothing can use again, for the caller to release once the
/// cache is unlocked: the last drawable of an abandoned layer frees the layer,
/// and its teardown must not run under the lock.
#[must_use]
fn retire_stale(cache: &mut Vec<Cached>) -> Vec<CachedSlot> {
    reclaim_dropped(cache);
    // Every ring's drawables hold their layer, stale rings' included, so a
    // layer only its own drawables hold is one the guest has let go of.
    let mut held = std::collections::HashMap::<usize, usize>::new();
    for entry in cache.iter() {
        *held.entry(entry.layer).or_default() += entry.slots.len();
    }
    let mut retired = Vec::new();
    let mut index = 0;
    while index < cache.len() {
        let entry = &cache[index];
        let abandoned = objc::arc::retain_count(entry.layer as Id) <= held[&entry.layer];
        if (entry.stale || abandoned)
            && entry
                .slots
                .iter()
                .all(|slot| slot.state == SlotState::Available)
        {
            retired.append(&mut cache.remove(index).slots);
        } else {
            index += 1;
        }
    }
    retired
}

/// Transfer a vended slot to a command buffer. A duplicate presentation of
/// the same drawable is ignored rather than creating two owners of one image.
pub(crate) fn submit(drawable: Id) -> bool {
    let mut cache = CACHE.0.lock().expect("drawable cache");
    let Some(slot) = cache
        .iter_mut()
        .flat_map(|entry| entry.slots.iter_mut())
        .find(|slot| slot.drawable == drawable as usize)
    else {
        return false;
    };
    slot.submit()
}

#[cfg(test)]
pub(crate) fn drawable_is_submitted_for_test(drawable: Id) -> bool {
    CACHE
        .0
        .lock()
        .expect("drawable cache")
        .iter()
        .flat_map(|cache| &cache.slots)
        .any(|slot| slot.drawable == drawable as usize && slot.state == SlotState::Submitted)
}

/// Return a vended or submitted slot to its layer's ring.
pub(crate) fn complete(drawable: Id) {
    let mut cache = CACHE.0.lock().expect("drawable cache");
    if let Some(slot) = cache
        .iter_mut()
        .flat_map(|entry| entry.slots.iter_mut())
        .find(|slot| slot.drawable == drawable as usize)
    {
        slot.complete();
        CACHE.1.notify_all();
    }
    let mut retired = retire_stale(&mut cache);
    drop(cache);
    release_slots(&mut retired);
}

unsafe extern "C" fn drawable_texture(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a drawable of this crate's.
    unsafe { object::body::<Drawable>(receiver) }.texture
}

unsafe extern "C" fn drawable_layer(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<Drawable>(receiver) }.layer
}

unsafe extern "C" fn drawable_present(receiver: Id, _sel: Sel) {
    if submit(receiver) {
        enqueue(objc::arc::retain(receiver));
    }
}

/// `-[CAMetalDrawable dealloc]`.
///
/// A drawable outlives the cache entry that vended it whenever a command
/// buffer still holds it, so this - and not the resize which evicted it - is
/// where its image and readback buffer go. Everything which could have the
/// device touching them has released its reference by now: a committed
/// command buffer releases the drawable when its submission retires, and the
/// readback path waits for its own copy before it returns.
unsafe extern "C" fn drawable_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: a drawable of this crate's.
    let drawable = unsafe { object::body::<Drawable>(receiver) };
    if let Some(readback) = drawable.readback.take() {
        drawable.gpu.destroy_buffer(readback);
    }
    // The texture owns the drawable's image, and destroys it in its own
    // `dealloc`.
    objc::arc::release(std::mem::replace(
        &mut drawable.texture,
        std::ptr::null_mut(),
    ));
    objc::arc::release(std::mem::replace(&mut drawable.layer, std::ptr::null_mut()));
    // SAFETY: the body was established by `object::make` and is not read again.
    unsafe { std::ptr::drop_in_place(drawable as *mut Drawable) };
    objc::arc::finalize(receiver);
}

/// Hand one owned drawable reference to its device's presentation thread.
///
/// The caller has made sure the work which renders the drawable is on the
/// queue already, so whatever the thread submits lands after it.
pub(crate) fn enqueue(drawable: Id) {
    if drawable.is_null() || !object::is_kind(drawable, c"RosemaryCAMetalDrawable") {
        objc::arc::release(drawable);
        return;
    }
    // SAFETY: a drawable of this crate's, checked above.
    let gpu = unsafe { object::body::<Drawable>(drawable) }.gpu;
    let presenter = &gpu.presenter;
    presenter.worker.call_once(|| {
        std::thread::Builder::new()
            .name("metal:present".to_string())
            .spawn(move || present_worker(gpu))
            .expect("a thread for Metal presentation");
    });
    let mut pending = presenter.pending.lock().expect("presentation queue");
    pending.push_back(drawable as usize);
    presenter.ready.notify_one();
}

/// Resolve the SDL window without the transient presentation-geometry cache.
/// A resize may invalidate its snapshot while submitted drawables are queued.
fn layer_window(layer: Id) -> Option<usize> {
    // SAFETY: the drawable retains its layer, which retains its AppKit view.
    let view = unsafe { appkit::layer::view_of(layer) };
    // SAFETY: a live AppKit view or null.
    let window = unsafe { appkit::view::window_of(view) };
    // SAFETY: a live AppKit window or null.
    let sdl = unsafe { appkit::window::sdl_window(window) };
    (!sdl.is_null()).then_some(sdl as usize)
}

/// Identify the drawable's window even when its pixel geometry is stale.
pub(crate) fn drawable_window(drawable: Id) -> Option<usize> {
    // SAFETY: the presentation queue retains a live drawable and its layer.
    let layer = unsafe { object::body::<Drawable>(drawable) }.layer;
    layer_window(layer)
}

/// Keep deferred drawables ahead of submissions which arrived mid-batch.
/// Appending a deferred A1 to the shared queue would otherwise let a newly
/// queued A3 overtake it after the dispatcher has scanned A2.
fn prepend_deferred(
    shared: &mut std::collections::VecDeque<usize>,
    mut deferred: std::collections::VecDeque<usize>,
) {
    deferred.append(shared);
    *shared = deferred;
}

struct ReadbackFrame {
    window: usize,
    window_id: u32,
    layer: usize,
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    format: vk::Format,
    placement: (i32, i32, u32, u32),
}

static READBACKS: std::sync::Mutex<Vec<ReadbackFrame>> = std::sync::Mutex::new(Vec::new());

/// Dispatches only *completed* GPU readbacks. SDL_GetWindowSurface and
/// SDL_UpdateWindowSurface never run on `metal:present`.
fn flush_readbacks() {
    if !sdl3::video::is_video_thread() {
        return;
    }
    let queued = std::mem::take(&mut *READBACKS.lock().expect("readback queue"));
    for frame in queued {
        let window = frame.window as sdl3::Window;
        // Check a registry-only lifetime identity before any SDL query.
        // A completed GPU readback may carry a pointer already destroyed by
        // the SDL video owner or logically retired by AppKit.
        if sdl3::video::readback_window_id(window) != Some(frame.window_id) {
            continue;
        }
        if sdl3::video::presentation_window(window)
            .is_none_or(|properties| properties.id != frame.window_id)
            || sdl3::video::vulkan::owns_window(window)
        {
            continue;
        }
        let drew = crate::stats::timed(crate::stats::Counter::BlitMicros, || {
            blit_to_window(
                &frame.pixels,
                frame.width,
                frame.height,
                frame.format,
                Some(window),
                frame.placement,
            )
        });
        if drew {
            appkit::backing::layer_drew_on_surface(window, frame.layer);
        }
    }
}

fn enqueue_completed_readback(frame: ReadbackFrame) {
    let mut pending = READBACKS.lock().expect("readback queue");
    // AppKit teardown revokes publication before taking this same mutex.
    // A completed readback which checked the window before teardown must
    // revalidate its lifetime/generation at the actual insertion point.
    if sdl3::video::readback_window_id(frame.window as sdl3::Window) != Some(frame.window_id) {
        return;
    }
    // Coalesce repeat frames without discarding unrelated one-shot layers.
    push_completed_readback(&mut pending, frame);
}

fn push_completed_readback(pending: &mut Vec<ReadbackFrame>, frame: ReadbackFrame) {
    pending.retain(|old| {
        old.window != frame.window || old.window_id != frame.window_id || old.layer != frame.layer
    });
    pending.push(frame);
}

/// Process one FIFO batch without letting a later frame of a deferred
/// window overtake it if SDL republishes its geometry during the scan.
pub(crate) fn dispatch_batch(
    batch: std::collections::VecDeque<usize>,
    mut window_of: impl FnMut(usize) -> Option<usize>,
    mut present: impl FnMut(usize) -> PresentAttempt,
    mut finished: impl FnMut(usize, PresentAttempt),
) -> std::collections::VecDeque<usize> {
    let mut blocked = std::collections::HashSet::new();
    let mut deferred = std::collections::VecDeque::new();
    for drawable in batch {
        let window = window_of(drawable);
        if window.is_some_and(|window| blocked.contains(&window)) {
            deferred.push_back(drawable);
            continue;
        }
        match present(drawable) {
            PresentAttempt::Deferred | PresentAttempt::DeferredBusy => {
                if let Some(window) = window {
                    blocked.insert(window);
                }
                deferred.push_back(drawable);
            }
            result => finished(drawable, result),
        }
    }
    deferred
}

/// Back off when metadata or a surface remains unavailable at idle.
fn deferred_retry_delay(streak: u32) -> std::time::Duration {
    std::time::Duration::from_millis((16u64 << streak.min(4)).min(250))
}

/// DXVK's submission completion path wakes resource waiters immediately;
/// likewise an occupied drawable ring must not wait for an exponential idle
/// backoff. A short bounded poll also covers races with a missed GPU/WSI wake.
fn presenter_retry_delay(streak: u32, busy: bool) -> std::time::Duration {
    if busy {
        std::time::Duration::from_millis(1)
    } else {
        deferred_retry_delay(streak)
    }
}

fn present_worker(gpu: &'static Gpu) {
    ON_PRESENT_WORKER.set(true);
    let presenter = &gpu.presenter;
    let mut deferred_streak = 0u32;
    loop {
        // Remove an entire batch under one lock: concurrent enqueue goes
        // after this batch, preserving ordering even when items are deferred.
        let (batch, observed_progress) = {
            let mut pending = presenter.pending.lock().expect("presentation queue");
            while pending.is_empty() {
                pending = presenter.ready.wait(pending).expect("presentation queue");
            }
            (
                std::mem::take(&mut *pending),
                presenter
                    .progress
                    .load(std::sync::atomic::Ordering::Acquire),
            )
        };
        let submitted = batch.len();
        let mut has_busy_deferral = false;
        let deferred = dispatch_batch(
            batch,
            |drawable| drawable_window(drawable as Id),
            |drawable| {
                let result = enqueue_swapchain(drawable as Id);
                has_busy_deferral |= result == PresentAttempt::DeferredBusy;
                result
            },
            |drawable, result| {
                let drawable = drawable as Id;
                if matches!(result, PresentAttempt::Readback) {
                    present_readback(drawable);
                    complete(drawable);
                }
                objc::arc::release(drawable);
            },
        );
        if !deferred.is_empty() {
            deferred_streak = if deferred.len() == submitted {
                deferred_streak.saturating_add(1)
            } else {
                0
            };
            // A producer may have notified while this batch was in flight.
            // The queue check must precede the condvar wait under one lock.
            presenter.requeue_deferred(
                deferred,
                presenter_retry_delay(deferred_streak, has_busy_deferral),
                observed_progress,
            );
        } else {
            deferred_streak = 0;
        }
    }
}

#[cfg(test)]
#[test]
fn deferred_presenter_retry_backs_off_without_reordering() {
    assert_eq!(deferred_retry_delay(0).as_millis(), 16);
    assert_eq!(deferred_retry_delay(1).as_millis(), 32);
    assert_eq!(deferred_retry_delay(2).as_millis(), 64);
    assert_eq!(deferred_retry_delay(3).as_millis(), 128);
    assert_eq!(deferred_retry_delay(4).as_millis(), 250);
    assert_eq!(deferred_retry_delay(u32::MAX).as_millis(), 250);
    for streak in [0, 1, 2, 3, 4, u32::MAX] {
        assert_eq!(presenter_retry_delay(streak, true).as_millis(), 1);
        assert_eq!(
            presenter_retry_delay(streak, false),
            deferred_retry_delay(streak)
        );
    }
    let mut pending = std::collections::VecDeque::from([3usize]);
    prepend_deferred(&mut pending, std::collections::VecDeque::from([1usize, 2]));
    assert_eq!(pending.into_iter().collect::<Vec<_>>(), [1, 2, 3]);
}

#[cfg(test)]
#[test]
fn deferred_window_does_not_delay_another_window_enqueued_during_dispatch() {
    use std::collections::VecDeque;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    let presenter = Arc::new(Presenter::new());
    let (processing_a, processing_rx) = mpsc::channel();
    let (producer_done, produced_rx) = mpsc::channel();
    let producer_queue = Arc::clone(&presenter);
    let producer = std::thread::spawn(move || {
        processing_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // This notify occurs before requeue_deferred can enter its wait,
        // reproducing the lost-wakeup interleaving from the V24 review.
        {
            let mut pending = producer_queue.pending.lock().unwrap();
            pending.push_back(20); // healthy window B
            pending.push_back(12); // newer frame from blocked window A
            producer_queue.ready.notify_one();
        }
        producer_done.send(()).unwrap();
    });

    // A1 and A2 were detached from the shared queue before B was enqueued.
    let deferred = dispatch_batch(
        VecDeque::from([10, 11]),
        |frame| Some(if frame == 20 { 2 } else { 1 }),
        |frame| {
            if frame == 10 {
                processing_a.send(()).unwrap();
                produced_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            PresentAttempt::Deferred
        },
        |_, _| unreachable!("A's frame cannot complete while unavailable"),
    );
    producer.join().unwrap();
    assert_eq!(deferred, VecDeque::from([10, 11]));

    let start = Instant::now();
    assert!(
        presenter.requeue_deferred(
            deferred,
            Duration::from_millis(250),
            presenter
                .progress
                .load(std::sync::atomic::Ordering::Acquire),
        ),
        "new B frame must bypass deferred A's 250ms backoff"
    );
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "new work slept through the deferred retry timeout"
    );

    // A1/A2 must precede A3, even though the unrelated B frame can proceed
    // without waiting for A's surface to become available.
    let batch = std::mem::take(&mut *presenter.pending.lock().unwrap());
    assert_eq!(batch, VecDeque::from([10, 11, 20, 12]));
    let mut completed = Vec::new();
    let still_deferred = dispatch_batch(
        batch,
        |frame| Some(if frame == 20 { 2 } else { 1 }),
        |frame| {
            if frame == 20 {
                PresentAttempt::Handled
            } else {
                PresentAttempt::Deferred
            }
        },
        |frame, outcome| {
            assert_eq!(outcome, PresentAttempt::Handled);
            completed.push(frame);
        },
    );
    assert_eq!(completed, [20], "healthy B must dispatch in the next batch");
    assert_eq!(still_deferred, VecDeque::from([10, 11, 12]));
}

#[cfg(test)]
#[test]
fn progress_during_dispatch_skips_the_busy_wait() {
    let presenter = Presenter::new();
    let observed = presenter
        .progress
        .load(std::sync::atomic::Ordering::Acquire);
    presenter.notify_progress();
    let start = std::time::Instant::now();
    assert!(!presenter.requeue_deferred(
        std::collections::VecDeque::from([7]),
        std::time::Duration::from_millis(250),
        observed,
    ));
    assert!(start.elapsed() < std::time::Duration::from_millis(100));
    assert_eq!(presenter.pending.lock().unwrap().pop_front(), Some(7));
}

#[cfg(test)]
#[test]
fn progress_after_requeue_wakes_its_condvar() {
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};
    let presenter = Arc::new(Presenter::new());
    let observed = presenter
        .progress
        .load(std::sync::atomic::Ordering::Acquire);
    let (started, receiver) = mpsc::channel();
    let waiting = Arc::clone(&presenter);
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        let at = Instant::now();
        waiting.requeue_deferred(
            std::collections::VecDeque::from([9]),
            Duration::from_secs(1),
            observed,
        );
        at.elapsed()
    });
    receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    presenter.notify_progress();
    assert!(worker.join().unwrap() < Duration::from_millis(500));
    assert_eq!(presenter.pending.lock().unwrap().pop_front(), Some(9));
}

#[cfg(test)]
#[test]
fn busy_deferred_frame_stays_ahead_of_later_frames() {
    use std::collections::VecDeque;
    let mut completed = Vec::new();
    let deferred = dispatch_batch(
        VecDeque::from([1, 2, 3]),
        |_| Some(10),
        |frame| match frame {
            1 => PresentAttempt::DeferredBusy,
            _ => panic!("do not submit a younger frame before the busy one"),
        },
        |frame, outcome| completed.push((frame, outcome)),
    );
    assert!(completed.is_empty());
    assert_eq!(deferred, VecDeque::from([1, 2, 3]));
}

#[cfg(test)]
#[test]
fn cpu_readback_window_surface_requires_the_sdl_video_thread() {
    // SDL's video owner is a process-global OS thread, not a libtest worker.
    // Keep this actual SDL surface assertion while avoiding four-thread suite
    // interference with other graphical unit tests.
    if !sdl3::isolated::run_here() {
        return;
    }
    let Some(window) = sdl3::video::create_window(c"SDL CPU fallback", 64, 64, 0) else {
        return;
    };
    let pixels = vec![0u8, 0, 0, 255];
    let key = window as usize;
    assert!(
        !std::thread::spawn(move || {
            blit_to_window(
                &pixels,
                1,
                1,
                vk::Format::B8G8R8A8_UNORM,
                Some(key as sdl3::Window),
                (0, 0, 1, 1),
            )
        })
        .join()
        .expect("background blit")
    );
    assert!(
        blit_to_window(
            &[0, 0, 0, 255],
            1,
            1,
            vk::Format::B8G8R8A8_UNORM,
            Some(window),
            (0, 0, 1, 1)
        ),
        "SDL video thread must be able to update an unconverted window surface"
    );
    sdl3::video::destroy_window(window);
}

#[cfg(test)]
#[test]
fn distinct_one_shot_readbacks_are_not_evicted_at_128() {
    let mut queued = Vec::new();
    for layer in 0..257 {
        push_completed_readback(
            &mut queued,
            ReadbackFrame {
                window: 0x1731 + layer,
                window_id: 17,
                layer,
                pixels: vec![layer as u8; 4],
                width: 1,
                height: 1,
                format: vk::Format::B8G8R8A8_UNORM,
                placement: (0, 0, 1, 1),
            },
        );
    }
    assert_eq!(queued.len(), 257);
    for layer in 0..257 {
        assert!(queued.iter().any(|frame| frame.layer == layer));
    }
    let first = queued[0].window;
    push_completed_readback(
        &mut queued,
        ReadbackFrame {
            window: first,
            window_id: 17,
            layer: 0,
            pixels: vec![9; 4],
            width: 1,
            height: 1,
            format: vk::Format::B8G8R8A8_UNORM,
            placement: (0, 0, 1, 1),
        },
    );
    assert_eq!(queued.len(), 257, "same-layer refresh must coalesce");
    assert_eq!(
        queued.iter().find(|frame| frame.layer == 0).unwrap().pixels,
        [9; 4]
    );
}

#[cfg(test)]
#[test]
fn cpu_readbacks_remain_queued_off_the_sdl_video_thread() {
    let frame = ReadbackFrame {
        window: 0x1731,
        window_id: 1,
        layer: 0x73,
        pixels: vec![0, 0, 0, 255],
        width: 1,
        height: 1,
        format: vk::Format::B8G8R8A8_UNORM,
        placement: (0, 0, 1, 1),
    };
    // This test is about off-owner queue draining. The synthetic address is
    // deliberately not an SDL window, so bypass the separate live-window
    // check in enqueue_completed_readback (covered by the teardown test).
    let mut queued = READBACKS.lock().expect("readback queue");
    queued.clear();
    push_completed_readback(&mut queued, frame);
    drop(queued);
    std::thread::spawn(flush_readbacks)
        .join()
        .expect("readback worker");
    assert_eq!(READBACKS.lock().expect("readback queue").len(), 1);
    READBACKS.lock().expect("readback queue").clear();
}

#[cfg(test)]
#[test]
fn deferred_presentations_keep_per_window_order_across_concurrent_enqueue() {
    use std::collections::VecDeque;
    let mut later = VecDeque::from([30usize, 40]);
    prepend_deferred(&mut later, VecDeque::from([10usize, 20]));
    assert_eq!(later, VecDeque::from([10, 20, 30, 40]));
}

/// Copy a drawable into its window's swapchain and present it. The swapchain
/// owns a retained reference until its copy fence signals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PresentAttempt {
    Handled,
    Readback,
    /// A surface/metadata failure may need progressively longer retries.
    Deferred,
    /// An existing swapchain is temporarily busy; do not starve its three
    /// already-submitted drawables behind a long timer.
    DeferredBusy,
}

pub(crate) fn enqueue_swapchain(drawable: Id) -> PresentAttempt {
    if drawable.is_null() || !object::is_kind(drawable, c"RosemaryCAMetalDrawable") {
        return PresentAttempt::Readback;
    }
    // SAFETY: a drawable of this crate's, checked above.
    let described = unsafe { object::body::<Drawable>(drawable) };
    // SAFETY: a drawable's texture is this crate's.
    let texture = unsafe { object::body::<Texture>(described.texture) };
    // SAFETY: a `CAMetalLayer` this runtime made.
    let view = unsafe { appkit::layer::view_of(described.layer) };
    if appkit::view::is_hidden_or_has_hidden_ancestor(view) {
        complete(drawable);
        return PresentAttempt::Handled;
    }
    let Some(frame) = layer_frame(described.layer) else {
        // SDL window metadata is deliberately invalidated between resize,
        // movement, scale changes and the next video-thread publication.
        // A live window must not lose an already-submitted drawable during
        // that interval, especially once SDL has converted it to Vulkan.
        return if layer_waits_for_window_metadata(described.layer) {
            if layer_window(described.layer).is_some_and(|window| {
                crate::swapchain::window_has_active_swapchain(window as sdl3::Window)
            }) {
                PresentAttempt::DeferredBusy
            } else {
                PresentAttempt::Deferred
            }
        } else {
            PresentAttempt::Readback
        };
    };
    if forced_readback() {
        return PresentAttempt::Readback;
    }
    // Where a web view sits over this layer, the window's store carries its
    // page, and the layer's frame must not paint over it.
    let covered: Vec<(usize, crate::swapchain::Region)> =
        appkit::backing::covered(frame.window, described.layer)
            .into_iter()
            .map(|rect| (frame.window as usize, crate::swapchain::Region::from(rect)))
            .collect();
    let owner = objc::arc::retain(drawable) as usize;
    match crate::swapchain::present_owned(
        described.gpu,
        described.layer as usize,
        &frame,
        &texture.image,
        &covered,
        owner,
    ) {
        Ok(Some(true)) => {
            gpu::milestone("presented frame");
            crate::stats::frame();
            // The readback path reports from `present_readback`; without this
            // a swapchain run never prints its statistics at all.
            crate::stats::tick();
            PresentAttempt::Handled
        }
        Ok(Some(false)) => PresentAttempt::Readback,
        Ok(None) => {
            if crate::swapchain::window_has_active_swapchain(frame.window) {
                PresentAttempt::DeferredBusy
            } else {
                PresentAttempt::Deferred
            }
        }
        Err(reason) => {
            gpu::trace(format!("swapchain presentation failed: {reason}"));
            PresentAttempt::Readback
        }
    }
}

pub(crate) fn layer_waits_for_window_metadata(layer: Id) -> bool {
    layer_window(layer)
        .is_some_and(|window| sdl3::video::presentation_window(window as sdl3::Window).is_none())
}

/// Called by the swapchain once the copy which reads a drawable has finished.
pub(crate) fn presentation_complete(drawable: usize) {
    let drawable = drawable as Id;
    complete(drawable);
    objc::arc::release(drawable);
}

fn present_readback(drawable: Id) {
    crate::stats::timed(crate::stats::Counter::PresentMicros, || {
        present_inner(drawable)
    });
    crate::stats::tick();
}

fn present_inner(drawable: Id) {
    if drawable.is_null() || !object::is_kind(drawable, c"RosemaryCAMetalDrawable") {
        return;
    }
    // SAFETY: a drawable of this crate's, checked above.
    let described = unsafe { object::body::<Drawable>(drawable) };
    // SAFETY: a drawable's texture is this crate's.
    let texture = unsafe { object::body::<Texture>(described.texture) };
    let gpu = described.gpu;
    // SAFETY: a `CAMetalLayer` this runtime made.
    let view = unsafe { appkit::layer::view_of(described.layer) };
    if appkit::view::is_hidden_or_has_hidden_ancestor(view) {
        return;
    }
    let frame = layer_frame(described.layer);

    // The layer's own window where it has one, and the first window
    // otherwise - which is a guess, but the only one available for a layer
    // this runtime cannot place. It is right for every guest with one
    // window, which is every guest the readback path has ever run.
    let window = frame
        .as_ref()
        .map(|frame| frame.window)
        .or_else(|| appkit::window::sdl_windows().first().copied());
    // Where the layer sits, so that the chrome and the rest of the store the
    // window surface already shows are not painted over.
    let placement = frame.as_ref().map_or((0, 0, u32::MAX, u32::MAX), |frame| {
        (frame.x, frame.y, frame.width, frame.height)
    });

    let width = texture.image.width;
    let height = texture.image.height;
    let bytes = u64::from(width) * u64::from(height) * 4;

    if described.readback.is_none() {
        match gpu.create_buffer(bytes) {
            Ok(staging) => described.readback = Some(staging),
            Err(reason) => return gpu::trace(format!("presentation staging failed: {reason}")),
        }
    }
    let staging = described.readback.as_ref().expect("just made");
    let copy = vk::BufferImageCopy::default()
        .buffer_offset(0)
        .buffer_row_length(width)
        .buffer_image_height(height)
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        });
    let image = texture.image.image;
    let result = crate::stats::timed(crate::stats::Counter::ReadbackMicros, || {
        gpu.one_shot(|device, command_buffer| {
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            // SAFETY: recording into a command buffer the call owns; the image
            // is in GENERAL layout from creation.
            unsafe {
                device.cmd_pipeline_barrier(
                    command_buffer,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[barrier],
                );
                device.cmd_copy_image_to_buffer(
                    command_buffer,
                    image,
                    vk::ImageLayout::GENERAL,
                    staging.buffer,
                    &[copy],
                )
            };
        })
    });
    if let Err(reason) = result {
        gpu::trace(format!("presentation readback failed: {reason}"));
    } else {
        // SAFETY: the readback completed and the staging buffer is mapped over
        // the whole allocation.
        let pixels = unsafe { std::slice::from_raw_parts(staging.contents, bytes as usize) };
        gpu::milestone("presented frame");
        crate::stats::frame();
        dump_frame(pixels, width, height, texture.image.format);
        if let Some(window) = window {
            if let Some(properties) = sdl3::video::presentation_window(window) {
                enqueue_completed_readback(ReadbackFrame {
                    window: window as usize,
                    window_id: properties.id,
                    layer: described.layer as usize,
                    pixels: pixels.to_vec(),
                    width,
                    height,
                    format: texture.image.format,
                    placement,
                });
            }
        }
    }
}

/// Where a layer sits in its window, or `None` for a layer no view holds.
/// `ROSEMARY_PRESENT_READBACK`, read once: presentation asks every frame.
fn forced_readback() -> bool {
    #[static_init::dynamic]
    static FORCED: bool = std::env::var_os("ROSEMARY_PRESENT_READBACK").is_some();
    *FORCED
}

pub(crate) fn layer_frame(layer: Id) -> Option<appkit::view::Frame> {
    // SAFETY: a `CAMetalLayer` this runtime made, whose view outlives it.
    let view = unsafe { appkit::layer::view_of(layer) };
    // SAFETY: null or a view of AppKit's.
    unsafe { appkit::view::presentation_frame(view) }
}

/// Write a presented frame to `ROSEMARY_FRAME_OUT` as a binary PPM.
///
/// What reached the window is the one thing a log cannot say, and a headless
/// or remote session has no other way to see it. `ROSEMARY_FRAME_INTERVAL`
/// says how many frames to skip between files; the default keeps a run's
/// output to a few files a second rather than one per frame.
fn dump_frame(pixels: &[u8], width: u32, height: u32, format: vk::Format) {
    use std::sync::atomic::{AtomicU64, Ordering};

    let Some(directory) = std::env::var_os("ROSEMARY_FRAME_OUT") else {
        return;
    };
    static PRESENTED: AtomicU64 = AtomicU64::new(0);
    let frame = PRESENTED.fetch_add(1, Ordering::Relaxed);
    let interval = std::env::var("ROSEMARY_FRAME_INTERVAL")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|interval| *interval > 0)
        .unwrap_or(30);
    let limit = std::env::var("ROSEMARY_FRAME_LIMIT")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(8);
    if !capture_due(frame, interval, limit) {
        return;
    }
    let directory = std::path::PathBuf::from(directory);
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    let swap_red_and_blue =
        !(format == vk::Format::R8G8B8A8_UNORM || format == vk::Format::R8G8B8A8_SRGB);
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    out.reserve(width as usize * height as usize * 3);
    for texel in pixels.as_chunks::<4>().0 {
        match swap_red_and_blue {
            true => out.extend_from_slice(&[texel[2], texel[1], texel[0]]),
            false => out.extend_from_slice(&[texel[0], texel[1], texel[2]]),
        }
    }
    let _ = std::fs::write(
        directory.join(format!("frame-{}-{frame:06}.ppm", std::process::id())),
        out,
    );
}

// Count scheduled captures, not bytes or successful writes, to bound work too.
fn capture_due(frame: u64, interval: u64, limit: u64) -> bool {
    interval != 0 && frame.is_multiple_of(interval) && frame / interval < limit
}

#[cfg(test)]
mod capture_tests {
    use super::capture_due;

    #[test]
    fn capture_limit_includes_first_frame_and_excludes_boundary() {
        assert!(capture_due(0, 30, 2));
        assert!(!capture_due(1, 30, 2));
        assert!(capture_due(30, 30, 2));
        assert!(!capture_due(60, 30, 2));
    }

    #[test]
    fn disabled_and_extreme_capture_settings_are_bounded() {
        assert!(!capture_due(0, 30, 0));
        assert!(!capture_due(0, 0, 8));
        assert!(capture_due(u64::MAX, u64::MAX, 2));
        assert!(!capture_due(u64::MAX, 1, u64::MAX));
    }
}

/// Source interval surviving both physical SDL bounds and the smaller logical
/// AppKit window. Return indices relative to the *original* source, not the
/// cropped destination, so oversized/scaled child images do not shift.
fn visible_source_span(
    start: i64,
    length: u32,
    limit: i32,
    offset: i32,
    span: i32,
) -> Option<(usize, usize)> {
    let first = (i64::from(offset).max(0) - start).max(0);
    let last = i64::from(length).min(
        i64::from(limit.max(0)).min(i64::from(offset).saturating_add(i64::from(span.max(0))))
            - start,
    );
    (first < last).then_some((first as usize, last as usize))
}

/// Clear stale pixels from the physical SDL surface outside the smaller
/// AppKit logical window. Clipping the new frame alone would leave previous
/// fullscreen pixels exposed in the host container after a shrink.
fn mask_readback_surface(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    pitch: usize,
    clip: sdl3::Rect,
) -> bool {
    let mut masked = false;
    for band in clip.complements((width as i32, height as i32)) {
        let (x, w) = (band.x as usize, band.w as usize);
        for y in band.y as usize..(band.y + band.h) as usize {
            let row = &mut pixels[y * pitch + x * 4..y * pitch + (x + w) * 4];
            for pixel in row.chunks_exact_mut(4) {
                pixel.copy_from_slice(&[0, 0, 0, 255]);
            }
        }
        masked = true;
    }
    masked
}

/// SDL's window surface is `SDL_PIXELFORMAT_XRGB8888` on every backend this
/// runs on, which is B, G, R, X in memory. A drawable is usually
/// `MTLPixelFormatBGRA8Unorm`, which is the same order, so the common case is
/// a row copy: the byte the window ignores is the one the drawable calls
/// alpha. A drawable in R, G, B, A order needs its two ends exchanged, which
/// is done a pixel at a time but a whole pixel at a time.
fn blit_to_window(
    pixels: &[u8],
    width: u32,
    height: u32,
    format: vk::Format,
    window: Option<sdl3::Window>,
    (left, top, frame_width, frame_height): (i32, i32, u32, u32),
) -> bool {
    let swap_red_and_blue =
        format == vk::Format::R8G8B8A8_UNORM || format == vk::Format::R8G8B8A8_SRGB;
    let Some(window) = window else {
        return false;
    };
    // A window with room for rounded corners shows its surface's alpha, and
    // a Mac's content never shows the guest's.
    let opaque = sdl3::video::presentation_window(window)
        .is_some_and(|snapshot| snapshot.flags & sdl3::video::flags::TRANSPARENT != 0);
    sdl3::video::present_window(window, |surface| {
        let clip = appkit::window::presentation_clip(window).unwrap_or(sdl3::Rect {
            x: 0,
            y: 0,
            w: surface.width,
            h: surface.height,
        });
        let blanked =
            if clip.x > 0 || clip.y > 0 || clip.w < surface.width || clip.h < surface.height {
                // SAFETY: the SDL surface owns exactly `pitch * height` bytes for
                // the duration of the present-window callback.
                let bytes = unsafe {
                    std::slice::from_raw_parts_mut(
                        surface.pixels,
                        surface.pitch as usize * surface.height.max(0) as usize,
                    )
                };
                mask_readback_surface(
                    bytes,
                    surface.width.max(0) as usize,
                    surface.height.max(0) as usize,
                    surface.pitch as usize,
                    clip,
                )
            } else {
                false
            };
        let (left, top) = (i64::from(left), i64::from(top));
        let Some((first_row, rows)) = visible_source_span(
            top,
            height.min(frame_height),
            surface.height,
            clip.y,
            clip.h,
        ) else {
            return blanked;
        };
        let Some((first_column, columns)) =
            visible_source_span(left, width.min(frame_width), surface.width, clip.x, clip.w)
        else {
            return blanked;
        };
        let count = columns - first_column;
        for row in first_row..rows {
            let source = (row * width as usize + first_column) * 4;
            let Some(from) = pixels.get(source..source + count * 4) else {
                continue;
            };
            let target = (top + row as i64) as usize * surface.pitch as usize
                + (left + first_column as i64) as usize * 4;
            // SAFETY: SDL reported `pitch` bytes for each of `height` rows, and
            // `visible` kept every write below inside the surface's row.
            let to = unsafe { surface.pixels.add(target) };
            if !swap_red_and_blue {
                // SAFETY: `count * 4` bytes, from a slice of that length into
                // the visible part of the row.
                unsafe { std::ptr::copy_nonoverlapping(from.as_ptr(), to, count * 4) };
                if opaque {
                    for column in 0..count {
                        // SAFETY: `column` is below `count`, inside this row.
                        unsafe { to.add(column * 4 + 3).write(0xff) };
                    }
                }
                continue;
            }
            for (column, texel) in from.as_chunks::<4>().0.iter().enumerate() {
                // SAFETY: `column` is below `count`, inside this row.
                unsafe {
                    let out = to.add(column * 4);
                    out.write(texel[2]);
                    out.add(1).write(texel[1]);
                    out.add(2).write(texel[0]);
                    out.add(3).write(0xff);
                }
            }
        }
        true
    })
}

fn window_size() -> Option<(u32, u32)> {
    let window = appkit::window::sdl_windows().first().copied()?;
    let (width, height) = sdl3::video::presentation_window(window)?.pixel_size;
    Some((width.max(1) as u32, height.max(1) as u32))
}

// The layer is AppKit's object, so its properties are read by message rather
// than by reaching into a layout this crate does not own.

fn send_id(receiver: Id, selector: &str) -> Id {
    let sel = objc::sel::get(selector);
    // SAFETY: null or an object the runtime allocated.
    let Some(method) = unsafe { objc::object::class_of(receiver) }.and_then(|c| c.method(sel))
    else {
        return std::ptr::null_mut();
    };
    // SAFETY: an `id (id, SEL)` accessor.
    let call: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(receiver, sel) }
}

fn send_u64(receiver: Id, selector: &str) -> u64 {
    let sel = objc::sel::get(selector);
    // SAFETY: null or an object the runtime allocated.
    let Some(method) = unsafe { objc::object::class_of(receiver) }.and_then(|c| c.method(sel))
    else {
        return 0;
    };
    // SAFETY: an `NSUInteger (id, SEL)` accessor.
    let call: unsafe extern "C" fn(Id, Sel) -> u64 =
        unsafe { std::mem::transmute(method.implementation()) };
    unsafe { call(receiver, sel) }
}

fn send_size(receiver: Id, selector: &str) -> (f64, f64) {
    let sel = objc::sel::get(selector);
    // SAFETY: null or an object the runtime allocated.
    let Some(method) = unsafe { objc::object::class_of(receiver) }.and_then(|c| c.method(sel))
    else {
        return (0.0, 0.0);
    };
    // SAFETY: a `CGSize (id, SEL)` accessor.
    let call: unsafe extern "C" fn(Id, Sel) -> foundation::geometry::NSSize =
        unsafe { std::mem::transmute(method.implementation()) };
    let size = unsafe { call(receiver, sel) };
    (size.width, size.height)
}

pub(crate) fn install() {
    appkit::bootstrap();
    appkit::backing::set_presenter(present_store);
    appkit::backing::set_importer(crate::overlay::importer);
    appkit::event::add_pump_hook(flush_readbacks);
    appkit::event::add_modal_presentation_hook(flush_readbacks);
    appkit::window::add_destroy_hook(forget_window);
    object::register::<Drawable>(c"RosemaryCAMetalDrawable");
    let drawable = objc::class::lookup(c"RosemaryCAMetalDrawable").expect("just registered");
    methods!(drawable, {
        "dealloc" => drawable_dealloc, c"v16@0:8";
        "texture" => drawable_texture, c"@16@0:8";
        "layer" => drawable_layer, c"@16@0:8";
        "present" => drawable_present, c"v16@0:8";
        "drawableID" => drawable_id, c"Q16@0:8";
    });
    let Some(layer) = objc::class::lookup(c"CAMetalLayer") else {
        return;
    };
    methods!(layer, {
        "nextDrawable" => next_drawable, c"@16@0:8";
        "setMaximumDrawableCount:" => crate::no_op, c"v24@0:8Q16";
    });
}

unsafe extern "C" fn drawable_id(_receiver: Id, _sel: Sel) -> u64 {
    0
}

/// A window's CPU-drawn contents on the device: see [`present_store`].
struct StoreImage {
    gpu: &'static Gpu,
    image: GpuImage,
    staging: gpu::GpuBuffer,
    /// The store with dma-buf overlays blended over it, made the first time
    /// the window has any.
    blended: Option<GpuImage>,
}

impl StoreImage {
    /// Every presentation that could name one of its images has finished.
    fn destroy(self) {
        self.gpu.destroy_buffer(self.staging);
        self.gpu.destroy_image(self.image);
        if let Some(blended) = self.blended {
            self.gpu.destroy_image(blended);
        }
    }
}

// SAFETY: Vulkan handles and a permanent mapping, used under `STORE_IMAGES`.
unsafe impl Send for StoreImage {}

/// Keyed by the SDL window's address.
static STORE_IMAGES: std::sync::Mutex<Vec<(usize, StoreImage)>> = std::sync::Mutex::new(Vec::new());

/// CPU backing-store contents and overlay references owned through completion
/// of an asynchronous GPU upload. The SDL event thread only copies the bytes.
struct StoreUpload {
    window: usize,
    identity: u32,
    generation: u64,
    store: appkit::backing::Store,
    holes: Vec<appkit::backing::Hole>,
    overlays: Vec<appkit::backing::GpuOverlay>,
}

impl StoreUpload {
    fn snapshot(
        window: sdl3::Window,
        identity: u32,
        store: &appkit::backing::Store,
        holes: &[appkit::backing::Hole],
        overlays: &[appkit::backing::GpuOverlay],
    ) -> Self {
        let mut owned = appkit::backing::Store::new(store.width, store.height);
        owned.pixels.clone_from(&store.pixels);
        Self {
            window: window as usize,
            identity,
            generation: store.generation(),
            store: owned,
            holes: holes.to_vec(),
            overlays: overlays
                .iter()
                .map(|overlay| appkit::backing::GpuOverlay {
                    picture: std::sync::Arc::clone(&overlay.picture),
                    rect: overlay.rect,
                })
                .collect(),
        }
    }

    fn matches(
        &self,
        identity: u32,
        store: &appkit::backing::Store,
        holes: &[appkit::backing::Hole],
        overlays: &[appkit::backing::GpuOverlay],
    ) -> bool {
        // Not the pixels, which would compare a whole store on the event
        // thread for every finished upload.
        self.identity == identity
            && self.generation == store.generation()
            && self.store.width == store.width
            && self.store.height == store.height
            && self.holes.len() == holes.len()
            && self
                .holes
                .iter()
                .zip(holes)
                .all(|(a, b)| a.layer == b.layer && a.rect == b.rect)
            && self.overlays.len() == overlays.len()
            && self
                .overlays
                .iter()
                .zip(overlays)
                .all(|(a, b)| a.rect == b.rect && std::sync::Arc::ptr_eq(&a.picture, &b.picture))
    }
}

struct StoreUploadState {
    pending: Option<StoreUpload>,
    completed: Option<(StoreUpload, appkit::backing::PresentOutcome)>,
    busy: bool,
    closed: bool,
}

struct StoreUploadWorker {
    state: std::sync::Mutex<StoreUploadState>,
    wake: std::sync::Condvar,
}

static STORE_UPLOADS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<usize, std::sync::Arc<StoreUploadWorker>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
static TEST_PAUSE_STORE_UPLOAD: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_STORE_UPLOAD_PAUSED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_STORE_UPLOAD_RETIRED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static TEST_ACTIVE_STORE_UPLOADS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn store_upload_worker(
    worker: std::sync::Arc<StoreUploadWorker>,
    retired: std::sync::Arc<std::sync::atomic::AtomicBool>,
    key: usize,
) {
    loop {
        let job = {
            let mut state = worker.state.lock().expect("store uploads");
            while state.pending.is_none() && !state.closed {
                state = worker.wake.wait(state).expect("store uploads");
            }
            if state.closed {
                break;
            }
            state.busy = true;
            state.pending.take().expect("queued store")
        };
        #[cfg(test)]
        if TEST_PAUSE_STORE_UPLOAD.load(std::sync::atomic::Ordering::Acquire) == key {
            TEST_STORE_UPLOAD_PAUSED.store(key, std::sync::atomic::Ordering::Release);
            while TEST_PAUSE_STORE_UPLOAD.load(std::sync::atomic::Ordering::Acquire) == key {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            TEST_STORE_UPLOAD_PAUSED.store(0, std::sync::atomic::Ordering::Release);
        }
        let outcome = present_store_in_turn(&worker, &job);
        let mut state = worker.state.lock().expect("store uploads");
        state.busy = false;
        if !state.closed {
            state.completed = Some((job, outcome));
        }
    }
    // Cleanup is worker-owned because waiting on STORE_IMAGES could wait on
    // the device's GPU queue mutex. Never do this on the AppKit event thread.
    let mut images = STORE_IMAGES.lock().expect("store images");
    if let Some(at) = images.iter().position(|(window, _)| *window == key) {
        let (_, old) = images.remove(at);
        RETIRED
            .lock()
            .expect("retired images")
            .push((old, FRAMES_RETIRED));
    }
    drop(images);
    #[cfg(test)]
    TEST_STORE_UPLOAD_RETIRED.store(key, std::sync::atomic::Ordering::Release);
    retired.store(true, std::sync::atomic::Ordering::Release);
    #[cfg(test)]
    TEST_ACTIVE_STORE_UPLOADS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
}

/// How long a store upload waits for a window's other presenters before
/// handing the store back to the event pump.
const STORE_TURN_LIMIT: std::time::Duration = std::time::Duration::from_millis(250);

/// Present `job`, waiting out a window's other presenters.
///
/// A window with a `CAMetalLayer` drawing every frame holds its presentation
/// lock nearly all the time. Giving up whenever it is held leaves the store -
/// and a web view's page over it - on screen only when a pump happens to
/// fall in a gap. This worker is off the event thread, so it can wait.
fn present_store_in_turn(
    worker: &StoreUploadWorker,
    job: &StoreUpload,
) -> appkit::backing::PresentOutcome {
    use appkit::backing::PresentOutcome::Deferred;
    let window = job.window as sdl3::Window;
    let deadline = std::time::Instant::now() + STORE_TURN_LIMIT;
    loop {
        if worker.state.lock().expect("store uploads").closed {
            return Deferred;
        }
        let waiting = std::time::Instant::now() < deadline;
        // Checked before uploading, so that waiting does not copy the store
        // to the GPU again on every try.
        if waiting && crate::swapchain::window_presenting_busy(window) {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        let outcome = present_store_synchronous(window, &job.store, &job.holes, &job.overlays);
        // Only a live swapchain makes a deferral a matter of contention; one
        // still initializing is the event pump's to retry.
        if outcome == Deferred && waiting && crate::swapchain::window_has_active_swapchain(window) {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        return outcome;
    }
}

/// Images replaced by a resize, with how many more presentations to keep them
/// for: a presentation already submitted may still be blitting one.
static RETIRED: std::sync::Mutex<Vec<(StoreImage, usize)>> = std::sync::Mutex::new(Vec::new());

/// More than the swapchain's frames in flight, so that every submission which
/// could name a retired image has finished before it is destroyed.
const FRAMES_RETIRED: usize = 4;

/// Present what AppKit's views drew on the CPU for `window`.
///
/// The store is uploaded and composited into the window's swapchain as one
/// more layer covering the window, so that it and any `CAMetalLayer`s share
/// the one surface the window has. A layer presenting afterwards lands over
/// it, which is AppKit's order for a layer-backed subview.
/// Drop what this module and the swapchains hold for a window being destroyed.
fn forget_window(window: sdl3::Window) {
    let key = window as usize;
    // Revoke before locking the queue: either a preceding publication is
    // cleared by this hook or a later one sees the revoked identity.
    sdl3::video::revoke_window_readbacks(window);
    READBACKS
        .lock()
        .expect("readback queue")
        .retain(|frame| frame.window != key);
    let worker = STORE_UPLOADS.lock().expect("store uploads").remove(&key);
    if let Some(worker) = worker {
        let mut state = worker.state.lock().expect("store uploads");
        state.closed = true;
        state.pending = None;
        state.completed = None;
        worker.wake.notify_one();
    }
    // Only a store-upload worker can create a StoreImage. Closing that
    // worker retires its image asynchronously, so window destruction never
    // waits for a *different* window holding the GPU-wide upload mutex.
    crate::swapchain::forget_window(window);
}

#[cfg(test)]
pub(crate) fn test_has_store_upload_worker(window: sdl3::Window) -> bool {
    STORE_UPLOADS
        .lock()
        .expect("store uploads")
        .contains_key(&(window as usize))
}

// SDL window surfaces cannot present a window already converted to Vulkan.
// Keep failed GPU damage dirty until the next pump instead of clearing it as
// though the CPU fallback had actually displayed the store.
fn store_failure_outcome(window: sdl3::Window) -> appkit::backing::PresentOutcome {
    if sdl3::video::vulkan::owns_window(window) {
        appkit::backing::PresentOutcome::Deferred
    } else {
        appkit::backing::PresentOutcome::Fallback
    }
}

// Device loss disables a converted Vulkan surface, not an ordinary SDL
// window whose CPU backing store is still presentable.
fn store_terminal_outcome(window: sdl3::Window) -> appkit::backing::PresentOutcome {
    if sdl3::video::vulkan::owns_window(window) {
        appkit::backing::PresentOutcome::Displayed
    } else {
        appkit::backing::PresentOutcome::Fallback
    }
}

#[cfg(test)]
#[test]
fn converted_vulkan_window_preserves_dirty_store_after_gpu_failure() {
    use appkit::backing::PresentOutcome::{Deferred, Fallback};
    let Some(window) = sdl3::video::create_window(c"GPU fallback state", 80, 64, 0) else {
        return;
    };
    assert_eq!(store_failure_outcome(window), Fallback);
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        sdl3::video::destroy_window(window);
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        sdl3::video::destroy_window(window);
        return;
    };
    if adapters
        .first()
        .and_then(|adapter| adapter.gpu().ok())
        .is_none()
    {
        sdl3::video::destroy_window(window);
        return;
    }
    let instance = crate::vulkan::instance_handle().expect("initialized Vulkan instance");
    let surface = sdl3::video::vulkan::create_surface(window, instance, crate::vulkan::library())
        .expect("initialized Vulkan window surface");
    assert!(sdl3::video::vulkan::owns_window(window));
    assert_eq!(
        store_failure_outcome(window),
        Deferred,
        "SDL_GetWindowSurface cannot recover a Vulkan-converted window"
    );
    assert_eq!(
        store_terminal_outcome(window),
        appkit::backing::PresentOutcome::Displayed,
        "terminal device loss suppresses SDL readback only after conversion"
    );
    sdl3::video::vulkan::destroy_surface(instance, surface);
    sdl3::video::destroy_window(window);
}

#[cfg(test)]
#[test]
fn lost_vulkan_gpu_retains_unconverted_window_cpu_fallback() {
    use appkit::backing::PresentOutcome::Fallback;
    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Ok(adapters) = crate::vulkan::adapters() else {
        return;
    };
    let Some(gpu) = adapters.first().and_then(|adapter| adapter.gpu().ok()) else {
        return;
    };
    let Some(window) = sdl3::video::create_window(c"unconverted CPU fallback", 72, 48, 0) else {
        return;
    };
    sdl3::video::presentation_window(window).expect("SDL window");
    assert!(!sdl3::video::vulkan::owns_window(window));
    let store = appkit::backing::Store::new(72, 48);
    gpu.lost.store(true, std::sync::atomic::Ordering::Release);
    let outcome = present_store(window, &store, &[], &[]);
    gpu.lost.store(false, std::sync::atomic::Ordering::Release);
    assert_eq!(
        outcome, Fallback,
        "CPU store is still presentable after GPU loss"
    );
    assert_eq!(store_terminal_outcome(window), Fallback);
    sdl3::video::destroy_window(window);
}

pub(crate) fn present_store(
    window: sdl3::Window,
    store: &appkit::backing::Store,
    holes: &[appkit::backing::Hole],
    overlays: &[appkit::backing::GpuOverlay],
) -> appkit::backing::PresentOutcome {
    use appkit::backing::PresentOutcome::{Deferred, Displayed, DisplayedEarlier, Fallback};
    if forced_readback() {
        return store_failure_outcome(window);
    }
    if let Some(converted) = crate::swapchain::window_refused(window) {
        return if converted { Displayed } else { Fallback };
    }
    // A busy presentation lock is left to the upload worker to wait out.
    if crate::swapchain::window_initialization_pending(window) {
        return Deferred;
    }
    let device = unsafe { crate::MTLCreateSystemDefaultDevice() };
    let Some(gpu) = crate::gpu_of_device(device) else {
        return store_failure_outcome(window);
    };
    if gpu.swapchain_api().is_none() || crate::vulkan::surface_api().is_none() {
        return store_failure_outcome(window);
    }
    if gpu.lost.load(std::sync::atomic::Ordering::Acquire)
        || crate::swapchain::window_rendering_disabled(window)
    {
        return store_terminal_outcome(window);
    }
    let Some(identity) = sdl3::video::presentation_window(window).map(|p| p.id) else {
        return Deferred;
    };
    let key = window as usize;
    let worker = {
        let mut all = match STORE_UPLOADS.try_lock() {
            Ok(all) => all,
            Err(_) => return Deferred,
        };
        if let Some(worker) = all.get(&key) {
            std::sync::Arc::clone(worker)
        } else {
            let worker = std::sync::Arc::new(StoreUploadWorker {
                state: std::sync::Mutex::new(StoreUploadState {
                    pending: None,
                    completed: None,
                    busy: false,
                    closed: false,
                }),
                wake: std::sync::Condvar::new(),
            });
            let retired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            if !sdl3::video::vulkan::hold_window_until(window, std::sync::Arc::clone(&retired)) {
                return Deferred; // SDL destroyed the window after the snapshot.
            }
            #[cfg(test)]
            TEST_ACTIVE_STORE_UPLOADS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let owned = std::sync::Arc::clone(&worker);
            std::thread::Builder::new()
                .name("rosemary-store-upload".into())
                .spawn(move || store_upload_worker(owned, retired, key))
                .expect("per-window CPU backing-store upload worker");
            all.insert(key, std::sync::Arc::clone(&worker));
            worker
        }
    };
    let mut state = match worker.state.try_lock() {
        Ok(state) => state,
        Err(_) => return Deferred,
    };
    if state.closed || state.busy || state.pending.is_some() {
        return Deferred;
    }
    // A store that changes every pump never matches the upload that just
    // finished, so that upload's outcome still has to count: the CPU path
    // shows the current store, and what the GPU showed was this window's.
    let earlier = match state.completed.take() {
        Some((completed, outcome)) if completed.identity == identity => match outcome {
            Fallback => return Fallback,
            Displayed if completed.matches(identity, store, holes, overlays) => return Displayed,
            Displayed => DisplayedEarlier,
            _ => Deferred,
        },
        _ => Deferred,
    };
    state.pending = Some(StoreUpload::snapshot(
        window, identity, store, holes, overlays,
    ));
    worker.wake.notify_one();
    earlier
}

/// Runs off the AppKit thread. Owns the CPU staging contents until one_shot
/// completes and holds the window's SDL lifetime token through GPU teardown.
fn present_store_synchronous(
    window: sdl3::Window,
    store: &appkit::backing::Store,
    holes: &[appkit::backing::Hole],
    overlays: &[appkit::backing::GpuOverlay],
) -> appkit::backing::PresentOutcome {
    use appkit::backing::PresentOutcome::{Deferred, Displayed, Fallback};
    if forced_readback() {
        return store_failure_outcome(window);
    }
    // Before GPU uploads or allocations, hand a contended window back to
    // `present_store_in_turn` to wait for.
    if let Some(converted) = crate::swapchain::window_refused(window) {
        return if converted { Displayed } else { Fallback };
    }
    if crate::swapchain::window_presenting_busy(window)
        || crate::swapchain::window_initialization_pending(window)
    {
        return Deferred;
    }
    // SAFETY: the Metal framework's exported constructor, which has no
    // preconditions.
    let device = unsafe { crate::MTLCreateSystemDefaultDevice() };
    let Some(gpu) = crate::gpu_of_device(device) else {
        return store_failure_outcome(window);
    };
    // Only converted windows lack a legal SDL CPU-surface fallback.
    if gpu.lost.load(std::sync::atomic::Ordering::Acquire)
        || crate::swapchain::window_rendering_disabled(window)
    {
        return store_terminal_outcome(window);
    }
    let mut images = STORE_IMAGES.lock().expect("store images");
    let at = images.iter().position(|(key, _)| *key == window as usize);
    let stale = at.is_some_and(|at| {
        let image = &images[at].1.image;
        (image.width, image.height) != (store.width, store.height)
    });
    if stale {
        let (_, old) = images.remove(at.expect("checked above"));
        RETIRED
            .lock()
            .expect("retired images")
            .push((old, FRAMES_RETIRED));
    }
    if at.is_none() || stale {
        let image = match gpu.create_image(&ImageDescription {
            image_type: vk::ImageType::TYPE_2D,
            view_type: vk::ImageViewType::TYPE_2D,
            format: vk::Format::B8G8R8A8_UNORM,
            width: store.width,
            height: store.height,
            depth: 1,
            levels: 1,
            layers: 1,
            samples: 1,
            usage: gpu::USAGE_SHADER_READ,
            cube: false,
            components: vk::ComponentMapping::default(),
        }) {
            Ok(image) => image,
            Err(reason) => {
                gpu::trace(format!("window store image creation failed: {reason}"));
                return store_failure_outcome(window);
            }
        };
        let staging = match gpu.create_buffer(store.pixels.len() as u64) {
            Ok(staging) => staging,
            Err(reason) => {
                gpu.destroy_image(image);
                gpu::trace(format!("window store staging failed: {reason}"));
                return store_failure_outcome(window);
            }
        };
        images.push((
            window as usize,
            StoreImage {
                gpu,
                image,
                staging,
                blended: None,
            },
        ));
    }
    let entry = &mut images
        .iter_mut()
        .find(|(key, _)| *key == window as usize)
        .expect("made above")
        .1;
    // SAFETY: the staging buffer is mapped over the store's whole length.
    unsafe {
        std::ptr::copy_nonoverlapping(
            store.pixels.as_ptr(),
            entry.staging.contents,
            store.pixels.len(),
        )
    };
    let copy = vk::BufferImageCopy::default()
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_extent(vk::Extent3D {
            width: store.width,
            height: store.height,
            depth: 1,
        });
    let (buffer, image) = (entry.staging.buffer, entry.image.image);
    let uploaded = entry.gpu.one_shot(|device, command_buffer| {
        // SAFETY: recording into a command buffer the call owns; the image is
        // in GENERAL layout from creation.
        unsafe {
            device.cmd_copy_buffer_to_image(
                command_buffer,
                buffer,
                image,
                vk::ImageLayout::GENERAL,
                &[copy],
            )
        };
    });
    if let Err(reason) = uploaded {
        gpu::trace(format!("window store upload failed: {reason}"));
        return store_failure_outcome(window);
    }
    let shown = match overlays.is_empty() {
        true => &entry.image,
        false => blend_overlays(window, entry, overlays),
    };
    let frame = appkit::view::Frame {
        window,
        x: 0,
        y: 0,
        width: store.width,
        height: store.height,
        window_width: store.width,
        window_height: store.height,
    };
    let holes: Vec<(usize, crate::swapchain::Region)> = holes
        .iter()
        .map(|hole| (hole.layer, crate::swapchain::Region::from(hole.rect)))
        .collect();
    let presented =
        match crate::swapchain::present_nonblocking(gpu, window as usize, &frame, shown, &holes) {
            Ok(Some(true)) => Displayed,
            Ok(Some(false)) => store_failure_outcome(window),
            Ok(None) => Deferred,
            Err(reason) => {
                gpu::trace(format!("window store presentation failed: {reason}"));
                store_failure_outcome(window)
            }
        };
    drop(images);
    // A deferred store was not staged. Its previous swapchain submissions
    // still refer to retired images, so only successful presentation attempts
    // advance their retirement count.
    let expired: Vec<StoreImage> = if presented == Deferred {
        Vec::new()
    } else {
        let mut retired = RETIRED.lock().expect("retired images");
        for (_, remaining) in retired.iter_mut() {
            *remaining = remaining.saturating_sub(1);
        }
        let (expired, kept) = std::mem::take(&mut *retired)
            .into_iter()
            .partition::<Vec<_>, _>(|(_, remaining)| *remaining == 0);
        *retired = kept;
        expired.into_iter().map(|(image, _)| image).collect()
    };
    for old in expired {
        old.destroy();
    }
    if presented == Displayed {
        gpu::milestone("window store");
    }
    presented
}

/// The store with `overlays` blended over it, or the store alone where they
/// cannot be; the window is then given pixel overlays from the next frame.
fn blend_overlays<'a>(
    window: sdl3::Window,
    entry: &'a mut StoreImage,
    overlays: &[appkit::backing::GpuOverlay],
) -> &'a GpuImage {
    let (width, height) = (entry.image.width, entry.image.height);
    if entry.blended.is_none() {
        match entry
            .gpu
            .create_image(&crate::overlay::target_description(width, height))
        {
            Ok(image) => entry.blended = Some(image),
            Err(reason) => {
                gpu::trace(format!("no image to blend overlays into: {reason}"));
                appkit::backing::refuse_dmabufs(window);
                return &entry.image;
            }
        }
    }
    let blended = entry.blended.as_ref().expect("made above");
    match crate::overlay::blend(entry.gpu, &entry.image, blended, overlays) {
        Ok(refused) => {
            if !refused.is_empty() {
                appkit::backing::refuse_dmabufs(window);
            }
            blended
        }
        Err(reason) => {
            gpu::trace(format!("blending overlays failed: {reason}"));
            appkit::backing::refuse_dmabufs(window);
            &entry.image
        }
    }
}

#[cfg(test)]
mod drawable_pool_tests {
    use super::*;

    #[test]
    fn cpu_fallback_blacks_out_previous_frame_outside_logical_bounds() {
        let pitch = 20usize;
        let mut pixels = vec![170u8; pitch * 3];
        assert!(mask_readback_surface(
            &mut pixels,
            4,
            3,
            pitch,
            sdl3::Rect {
                x: 1,
                y: 0,
                w: 3,
                h: 2
            }
        ));
        for y in 0..3usize {
            for x in 0..4usize {
                let from = y * pitch + x * 4;
                let color = &pixels[from..from + 4];
                if y < 2 && x >= 1 {
                    assert_eq!(color, &[170; 4]);
                } else {
                    assert_eq!(color, &[0, 0, 0, 255]);
                }
            }
            assert_eq!(&pixels[y * pitch + 16..y * pitch + 20], &[170; 4]);
        }
        assert!(!mask_readback_surface(
            &mut pixels,
            4,
            3,
            pitch,
            sdl3::Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 3
            }
        ));
    }

    #[test]
    fn cpu_metal_readback_keeps_original_source_offsets_across_logical_crop() {
        // Source starts at (-2,-3) and extends past its 10x8 SDL container.
        // The physical display is 10x8 but the logical window is y=0..5.
        assert_eq!(visible_source_span(-2, 12, 10, 0, 10), Some((2, 12)));
        assert_eq!(visible_source_span(-3, 12, 8, 0, 5), Some((3, 8)));
        assert_eq!(visible_source_span(0, 8, 8, 5, 0), None);
        // A later larger frame restores formerly excluded source rows.
        assert_eq!(visible_source_span(-3, 12, 8, 0, 8), Some((3, 11)));
        // Clip starts inside a shifted source: never rebase to the cropped edge.
        assert_eq!(visible_source_span(4, 7, 20, 6, 4), Some((2, 6)));
    }

    #[test]
    fn submitted_slot_is_not_available_until_completion() {
        let mut slot = CachedSlot {
            drawable: 1,
            state: SlotState::Vended,
        };
        assert!(slot.submit());
        assert_eq!(slot.state, SlotState::Submitted);
        assert!(!slot.submit());
        slot.complete();
        assert_eq!(slot.state, SlotState::Available);
    }

    #[test]
    fn duplicate_submission_does_not_create_a_second_owner() {
        let mut slot = CachedSlot {
            drawable: 1,
            state: SlotState::Submitted,
        };
        assert!(!slot.submit());
        assert_eq!(slot.state, SlotState::Submitted);
    }

    /// Long enough that a test which waits it out has failed.
    const NEVER: std::time::Duration = std::time::Duration::from_secs(60);

    fn full_ring() -> DrawableCache {
        let ring = Cached {
            layer: 7,
            width: 128,
            height: 128,
            pixel_format: 80,
            stale: false,
            slots: vec![CachedSlot {
                drawable: 1,
                state: SlotState::Submitted,
            }],
        };
        (std::sync::Mutex::new(vec![ring]), std::sync::Condvar::new())
    }

    fn ring_of_layer_7(entry: &Cached) -> bool {
        !entry.stale && entry.layer == 7
    }

    fn finish_copy(pair: &DrawableCache) {
        pair.0.lock().unwrap()[0].slots[0].complete();
        pair.1.notify_all();
    }

    #[test]
    fn vends_a_drawable_retired_while_the_lock_was_given_up() {
        let pair = full_ring();
        let started = std::time::Instant::now();
        let mut retirements = 0;
        // The completion's notification goes to nobody, and the swapchain
        // has no further copy, so only a recheck finds the slot.
        let vended = vend(
            &pair,
            pair.0.lock().unwrap(),
            ring_of_layer_7,
            || {
                retirements += 1;
                finish_copy(&pair);
                false
            },
            NEVER,
        );
        assert_eq!(vended.map(|(_, drawable)| drawable), Some(1));
        assert_eq!(retirements, 1);
        assert!(started.elapsed() < NEVER / 2);
    }

    #[test]
    fn retires_again_without_waiting_while_a_copy_is_in_flight() {
        let pair = full_ring();
        let started = std::time::Instant::now();
        let mut retirements = 0;
        let vended = vend(
            &pair,
            pair.0.lock().unwrap(),
            ring_of_layer_7,
            || {
                retirements += 1;
                if retirements == 2 {
                    finish_copy(&pair);
                }
                true
            },
            NEVER,
        );
        assert_eq!(vended.map(|(_, drawable)| drawable), Some(1));
        assert_eq!(retirements, 2);
        assert!(started.elapsed() < NEVER / 2);
    }

    #[test]
    fn wakes_for_a_drawable_returned_by_another_thread() {
        let pair = full_ring();
        let started = std::time::Instant::now();
        std::thread::scope(|scope| {
            let guard = pair.0.lock().unwrap();
            // Blocks on the lock until the vend below waits and gives it up.
            scope.spawn(|| finish_copy(&pair));
            let vended = vend(&pair, guard, ring_of_layer_7, || false, NEVER);
            assert_eq!(vended.map(|(_, drawable)| drawable), Some(1));
        });
        assert!(started.elapsed() < NEVER / 2);
    }

    #[test]
    fn gives_up_when_the_ring_is_retired_while_waiting() {
        let pair = full_ring();
        let vended = vend(
            &pair,
            pair.0.lock().unwrap(),
            ring_of_layer_7,
            || {
                pair.0.lock().unwrap()[0].stale = true;
                true
            },
            NEVER,
        );
        assert!(vended.is_none());
    }
}

#[cfg(test)]
#[test]
fn appkit_backing_upload_remains_responsive_with_another_window_holding_the_gpu_queue() {
    use appkit::backing::PresentOutcome::{Deferred, Displayed};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    if std::env::var_os("ROSEMARY_TEST_VULKAN").is_none() {
        return;
    }
    let Some(gpu) = crate::vulkan::adapters()
        .ok()
        .and_then(|adapters| adapters.first()?.gpu().ok())
    else {
        return;
    };
    let Some(first) = sdl3::video::create_window(c"held queue first window", 1024, 768, 0) else {
        return;
    };
    let second = sdl3::video::create_window(c"held queue second window", 1024, 768, 0)
        .expect("second SDL window");
    for window in [first, second] {
        sdl3::video::show_window(window);
        assert!(sdl3::video::presentation_window(window).is_some());
    }
    let mut store = appkit::backing::Store::new(1024, 768);
    store.pixels.fill(137);

    let held = std::sync::Arc::new(AtomicBool::new(false));
    let release = std::sync::Arc::new(AtomicBool::new(false));
    let ready = std::sync::Arc::clone(&held);
    let allowed = std::sync::Arc::clone(&release);
    let blocker = std::thread::spawn(move || {
        gpu.with_queue_locked_for_test(|| {
            ready.store(true, Ordering::Release);
            while !allowed.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
    });
    let start = Instant::now();
    while !held.load(Ordering::Acquire) {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "queue lock was not acquired"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut worst = Duration::ZERO;
    let mut samples = Vec::new();
    for _ in 0..100 {
        for window in [first, second] {
            let start = Instant::now();
            assert_eq!(present_store(window, &store, &[], &[]), Deferred);
            let elapsed = start.elapsed();
            worst = worst.max(elapsed);
            samples.push(elapsed);
        }
    }
    assert!(
        worst < Duration::from_millis(50),
        "AppKit was stalled behind a different window's GPU queue: {worst:?}"
    );
    // Teardown also must not wait for a different window's GPU work.
    let third = sdl3::video::create_window(c"contended teardown window", 96, 64, 0)
        .expect("third SDL window");
    sdl3::video::presentation_window(third).expect("third SDL snapshot");
    let closing = appkit::backing::Store::new(96, 64);
    assert_eq!(present_store(third, &closing, &[], &[]), Deferred);
    let close_started = Instant::now();
    forget_window(third);
    sdl3::video::destroy_window(third);
    let close_latency = close_started.elapsed();
    assert!(
        close_latency < Duration::from_millis(50),
        "AppKit teardown blocked on another window's GPU queue: {close_latency:?}"
    );
    release.store(true, Ordering::Release);
    blocker.join().expect("release artificial GPU contention");

    for window in [first, second] {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            sdl3::video::vulkan::service_surface_requests();
            match present_store(window, &store, &[], &[]) {
                Displayed => break,
                Deferred => {
                    assert!(
                        Instant::now() < deadline,
                        "the queued store never reached Vulkan"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                outcome => panic!("unexpected backing-store outcome: {outcome:?}"),
            }
        }
    }
    assert_eq!(samples.len(), 200);
    samples.sort_unstable();
    eprintln!(
        "AppKit GPU contention 200 backing-store calls: p95={:?}, p99={:?}, max={worst:?}, teardown={close_latency:?}",
        samples[189], samples[197],
    );

    for window in [first, second] {
        forget_window(window);
        sdl3::video::destroy_window(window);
    }
    let deadline = Instant::now() + Duration::from_secs(6);
    while TEST_ACTIVE_STORE_UPLOADS.load(Ordering::Acquire) != 0 {
        assert!(
            Instant::now() < deadline,
            "store upload workers did not retire"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    crate::swapchain::wait_for_test_retirements();
    sdl3::video::vulkan::service_surface_requests();
}

#[cfg(test)]
#[test]
fn window_teardown_between_readback_identity_lookup_and_publish_never_queues_stale_frame() {
    use std::sync::mpsc;
    use std::time::Duration;

    // SDL has one process-wide video owner, which must also perform real
    // window destruction. Isolate it from other libtest video owners.
    if !sdl3::isolated::run_here() {
        return;
    }

    for destroy_before_publish in [false, true] {
        let window =
            sdl3::video::create_window(c"readback teardown race", 90, 60, 0).expect("SDL window");
        let identity = sdl3::video::presentation_window(window)
            .expect("live SDL metadata")
            .id;
        let key = window as usize;
        let (observed, receive) = mpsc::channel();
        let (permit, resume) = mpsc::channel();
        let publisher = std::thread::spawn(move || {
            // This is exactly the metadata lookup present_inner performs
            // after the GPU has returned its mapped readback buffer.
            let before = sdl3::video::presentation_window(key as sdl3::Window)
                .expect("worker had an identity before destruction");
            observed.send(before.id).expect("notify video thread");
            resume.recv().expect("release readback publisher");
            enqueue_completed_readback(ReadbackFrame {
                window: key,
                window_id: before.id,
                layer: 0x15,
                pixels: vec![255; 4],
                width: 1,
                height: 1,
                format: vk::Format::B8G8R8A8_UNORM,
                placement: (0, 0, 1, 1),
            });
        });
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(2))
                .expect("worker identity"),
            identity
        );
        if destroy_before_publish {
            // Includes the AppKit hook's logical cancellation, queue drain,
            // and immediate native destruction before the GPU thread resumes.
            forget_window(window);
            sdl3::video::destroy_window(window);
            assert_eq!(sdl3::video::live_window_id(window), None);
        } else {
            // AppKit's destroy hook runs before SDL has actually disposed
            // of its native window. Even here publication is forbidden.
            forget_window(window);
            assert_eq!(sdl3::video::readback_window_id(window), None);
        }
        permit.send(()).expect("resume after cancellation");
        publisher.join().expect("readback publisher");
        assert!(
            READBACKS.lock().unwrap().is_empty(),
            "GPU readback was published after its window was retired"
        );
        if !destroy_before_publish {
            sdl3::video::destroy_window(window);
        }
        // Must not call SDL_GetWindowID/size on the freed pointer.
        assert_eq!(sdl3::video::presentation_window(window), None);
        flush_readbacks();
        assert!(READBACKS.lock().unwrap().is_empty());
    }

    // Also verify that a completed readback queued *before* the window
    // destruction hook is purged synchronously by that hook.
    let window =
        sdl3::video::create_window(c"queued readback teardown", 90, 60, 0).expect("SDL window");
    let identity = sdl3::video::presentation_window(window).unwrap().id;
    enqueue_completed_readback(ReadbackFrame {
        window: window as usize,
        window_id: identity,
        layer: 3,
        pixels: vec![33; 4],
        width: 1,
        height: 1,
        format: vk::Format::B8G8R8A8_UNORM,
        placement: (0, 0, 1, 1),
    });
    assert_eq!(READBACKS.lock().unwrap().len(), 1);
    forget_window(window);
    assert!(READBACKS.lock().unwrap().is_empty());
    sdl3::video::destroy_window(window);
    flush_readbacks();

    // A raw SDL window can also be destroyed without an AppKit destroy
    // hook. Already completed readbacks may remain in the queue; the pump
    // must reject that pointer before asking SDL for its metadata.
    let direct = sdl3::video::create_window(c"direct SDL readback teardown", 80, 55, 0)
        .expect("direct SDL window");
    let identity = sdl3::video::presentation_window(direct).unwrap().id;
    enqueue_completed_readback(ReadbackFrame {
        window: direct as usize,
        window_id: identity,
        layer: 4,
        pixels: vec![44; 4],
        width: 1,
        height: 1,
        format: vk::Format::B8G8R8A8_UNORM,
        placement: (0, 0, 1, 1),
    });
    assert_eq!(READBACKS.lock().unwrap().len(), 1);
    sdl3::video::destroy_window(direct);
    assert_eq!(sdl3::video::presentation_window(direct), None);
    flush_readbacks();
    assert!(READBACKS.lock().unwrap().is_empty());
}
