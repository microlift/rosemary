use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use foundation::geometry::{CGFloat, NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

#[repr(C)]
pub struct View {
    isa: *const c_void,
    appearance: Id,
    /// Unretained, as AppKit's is: a view's window owns the view.
    window: Id,
    superview: Id,
    subviews: Id,
    tracking_areas: Id,
    /// Retained, as AppKit's is: a layer outlives the call that set it.
    layer: Id,
    /// `-setAllowedTouchTypes:`, stored so `-allowedTouchTypes` reads back what
    /// was set. Nothing in this tree delivers a touch event, so the mask only
    /// ever says which ones the view *would* accept.
    allowed_touch_types: usize,
    frame: NSRect,
    wants_best_resolution: usize,
    /// `-setPostsFrameChangedNotifications:` and its bounds twin, as the two
    /// low bits. A view posts nothing until it is asked to, which is AppKit's
    /// default for a view built with `-initWithFrame:`.
    posts: usize,
    /// `NSAutoresizingMaskOptions`, which says which of the six lengths around
    /// and inside this view give when its superview resizes. See
    /// [`resize_subviews`].
    autoresizing_mask: usize,
    /// `-setAutoresizesSubviews:`, which is `YES` on a fresh view - so the
    /// field is stored inverted, and zero means the AppKit default.
    does_not_autoresize_subviews: usize,
    /// `-setHidden:`.
    hidden: usize,
    /// `-setAlphaValue:`, as the bits of `1 - alpha` so that zero is AppKit's
    /// fully opaque default.
    transparency: u64,
    /// `-setTranslatesAutoresizingMaskIntoConstraints:`, inverted like
    /// `does_not_autoresize_subviews` so that zero is AppKit's `YES`.
    constrained: usize,
    /// `-setNextResponder:`, unretained as AppKit's is; null means the
    /// superview, which is a view's next responder until something is put
    /// between them.
    next_responder: Id,
    /// `-setWantsLayer:` and whether the layer is one AppKit made for it, as
    /// [`WANTS_LAYER`] and [`BACKING_LAYER`], and `-setLayerContentsPlacement:`
    /// from [`PLACEMENT_SHIFT`] up.
    layer_backing: usize,
    /// `-setBoundsOrigin:`: where the view's own coordinates start, which is
    /// how a clip view scrolls its document. The bounds' size is the frame's.
    bounds_origin: NSPoint,
}

/// The bits of [`View::layer_backing`]: the view asked for a layer, and the
/// layer it has is the one `-makeBackingLayer` made rather than one a program
/// set with `-setLayer:`.
const WANTS_LAYER: usize = 1;
const BACKING_LAYER: usize = 2;
const CLIPS_TO_BOUNDS: usize = 4;
const PLACEMENT_SHIFT: u32 = 8;

/// The bits of [`View::posts`].
const POSTS_FRAME: usize = 1;
const POSTS_BOUNDS: usize = 2;

/// `NSAutoresizingMaskOptions`.
mod autoresizing {
    pub const MIN_X_MARGIN: usize = 1;
    pub const WIDTH_SIZABLE: usize = 2;
    pub const MAX_X_MARGIN: usize = 4;
    pub const MIN_Y_MARGIN: usize = 8;
    pub const HEIGHT_SIZABLE: usize = 16;
    pub const MAX_Y_MARGIN: usize = 32;
}

/// The notification a view posts when its frame changes.
pub const FRAME_DID_CHANGE: &str = "NSViewFrameDidChangeNotification";
/// ...and when its bounds do.
pub const BOUNDS_DID_CHANGE: &str = "NSViewBoundsDidChangeNotification";

/// A live Metal layer in bottom-to-top AppKit view order. IDs are identities,
/// not owned Objective-C references: consumers must not dereference them.
/// A rounded CA masksToBounds ancestor in physical window coordinates.
/// All nested rounded masks are retained rather than collapsing them into a
/// rectangular intersection which would reveal corner pixels incorrectly.
#[derive(Clone, Copy)]
pub struct RoundedSceneClip {
    pub frame: Frame,
    pub radius: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpacityGroup {
    /// A Core Animation tree identity, captured on AppKit's video thread.
    pub id: usize,
    pub opacity: f32,
}

#[derive(Clone)]
pub struct MetalSceneLayer {
    pub layer: usize,
    pub opacity_groups: Vec<OpacityGroup>,
    pub frame: Frame,
    pub opacity: f32,
    pub opaque: bool,
    /// Clip from enclosing AppKit/CA bounds, expressed in physical window pixels.
    pub clip: Option<Frame>,
    pub rounded: Vec<RoundedSceneClip>,
    /// Live opaque AppKit/native coverage above this Metal layer, in window
    /// pixels. The compositor must not retain coverage from a past drawable.
    pub occluded: Vec<Frame>,
    /// Independent CPU-backed native content stacked above a Metal layer.
    /// None for actual CAMetalLayers. The GPU worker uploads immutable bytes.
    pub native_pixels: Option<std::sync::Arc<Vec<u8>>>,
    pub native_cpu_capture: Option<crate::backing::NativeCpuCapture>,
    pub native_dmabuf: Option<std::sync::Arc<crate::backing::Dmabuf>>,
    pub native_revision: u64,
    /// Full native frame and origin of this tile in the DMA source's scaled
    /// image space. Unlike cropped CPU bytes, the DMA allocation is imported
    /// without cloning or changing its underlying extent.
    pub native_dma_sample: Option<(u32, u32, u32, u32)>,
    /// Original NSView pointer that owns an independent CPU/DMA image. A
    /// large view can publish multiple tiles with distinct GPU layer IDs.
    pub native_owner: usize,
}

static NATIVE_TILE_IDS: std::sync::LazyLock<Mutex<HashMap<(usize, u32), usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static NATIVE_NEXT_TILE: AtomicUsize = AtomicUsize::new(1);

pub(crate) fn descendant_view_layers(view: Id) -> Vec<usize> {
    fn collect(view: Id, result: &mut Vec<usize>, depth: usize) {
        if depth >= 64 || !is_view(view) {
            return;
        }
        let children = view_of(view).subviews;
        if let Some(children) =
            unsafe { corefoundation::collections::array(children as *const c_void) }
        {
            for &child in &children.values {
                let child = child as Id;
                if !is_view(child) {
                    continue;
                }
                let layer = view_of(child).layer;
                if !layer.is_null() {
                    result.push(layer as usize);
                }
                collect(child, result, depth + 1);
            }
        }
    }
    let mut result = Vec::new();
    collect(view, &mut result, 0);
    result
}

fn native_tile_id(view: Id, index: u32) -> usize {
    if index == 0 {
        return view as usize | (1usize << (usize::BITS - 1));
    }
    let mut ids = NATIVE_TILE_IDS.lock().expect("native tile IDs");
    *ids.entry((view as usize, index)).or_insert_with(|| {
        (3usize << (usize::BITS - 2)) | NATIVE_NEXT_TILE.fetch_add(1, Ordering::Relaxed)
    })
}

pub(crate) fn forget_native_tile_ids(view: Id) {
    NATIVE_TILE_IDS
        .lock()
        .expect("native tile IDs")
        .retain(|(owner, _), _| *owner != view as usize);
}

fn native_tile_owner(scene_layer: usize) -> usize {
    NATIVE_TILE_IDS
        .lock()
        .expect("native tile IDs")
        .iter()
        .find_map(|((owner, _), &layer)| (layer == scene_layer).then_some(*owner))
        .unwrap_or(scene_layer & !(1usize << (usize::BITS - 1)))
}

// SAFETY: a snapshot owns only integer layer identities and a copied SDL
// window handle, which is compared as an identity and never dereferenced by
// a compositor worker. The AppKit event thread creates the values.
unsafe impl Send for MetalSceneLayer {}

/// AppKit sends value-only snapshots; Metal owns GPU resources independently.
/// The hook must not wait for a presentation lock or the Vulkan device.
static METAL_SCENE_CHANGED: std::sync::OnceLock<fn(sdl3::Window, Vec<MetalSceneLayer>)> =
    std::sync::OnceLock::new();
/// Event-thread-published scene snapshots. The Metal presenter must not walk
/// mutable NSView subview arrays from a separate GPU presentation thread.
static METAL_SCENES: std::sync::Mutex<
    Option<std::collections::HashMap<usize, Vec<MetalSceneLayer>>>,
> = std::sync::Mutex::new(None);

// No guest setter thread is allowed to borrow an NSView's subviews. Mark a
// transaction dirty and let the video owner capture it once per event turn.
static METAL_SCENE_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static METAL_VIEW_SCENE_DIRTY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// SDL window identities are published without borrowing NSViews. Targeted
/// AppKit changes only recapture the affected window; unresolved reparenting
/// still goes through the process-wide fallback flag above.
// Swapchain initialization can occur after an otherwise unchanged AppKit
// publication. Replay only the registered window on its owner thread.
static METAL_SCENE_REPLAY_WINDOWS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

static METAL_VIEW_DIRTY_WINDOWS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));
static METAL_CA_DAMAGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// CA roots are numerical identities; only the video owner associates them
/// with live windows after walking the NSView tree.
static METAL_CA_ROOTS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
/// DMA-BUF import failures originate on the GPU worker. Only the AppKit
/// owner can safely ask a still-live NSView to redraw via CPU pixels.
static NATIVE_CPU_FALLBACKS: std::sync::Mutex<Vec<(usize, usize)>> =
    std::sync::Mutex::new(Vec::new());

pub fn request_native_cpu_fallback(window: sdl3::Window, scene_layer: usize) {
    let mut pending = NATIVE_CPU_FALLBACKS
        .lock()
        .expect("native CPU redraw requests");
    if !pending.contains(&(window as usize, scene_layer)) {
        pending.push((window as usize, scene_layer));
    }
    invalidate_metal_scene_for_window(window);
}

fn find_scene_native_view(root: Id, owner: usize, depth: usize) -> Option<Id> {
    if root.is_null() || depth >= 64 || !is_view(root) {
        return None;
    }
    if root as usize == owner {
        return Some(root);
    }
    for child in unsafe { children(root) } {
        if let Some(found) = find_scene_native_view(child, owner, depth + 1) {
            return Some(found);
        }
    }
    None
}

fn service_native_cpu_fallbacks() {
    let pending = std::mem::take(
        &mut *NATIVE_CPU_FALLBACKS
            .lock()
            .expect("native CPU redraw requests"),
    );
    for (sdl, layer) in pending {
        let window = crate::window::window_for(sdl as sdl3::Window);
        if window.is_null() {
            continue;
        }
        if let Some(view) = find_scene_native_view(
            crate::window::content_view_of(window),
            native_tile_owner(layer),
            0,
        ) {
            crate::backing::needs_display(view);
        }
    }
}

static METAL_SCENE_OWNERS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<usize, Vec<usize>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
// Capture-time index. Each live owner retains its view until the next
// capture or window teardown, so the event pump never dereferences a freed
// Objective-C instance after a concurrent view removal.
struct MetalSurfacePollOwner {
    view: usize,
}

impl MetalSurfacePollOwner {
    fn new(view: Id) -> Self {
        objc::arc::retain(view);
        Self {
            view: view as usize,
        }
    }
}

impl Drop for MetalSurfacePollOwner {
    fn drop(&mut self) {
        objc::arc::release(self.view as Id);
    }
}

static METAL_SURFACE_POLL_OWNERS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<usize, Vec<std::sync::Arc<MetalSurfacePollOwner>>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(test)]
static METAL_SURFACE_POLL_VISITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

static METAL_SCENE_WAS_ANIMATING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static METAL_SCENE_LAST_ANIM_FRAME: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);
static METAL_LAST_ANIM_ROOTS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
#[cfg(test)]
static METAL_SCENE_CAPTURES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg(test)]
static METAL_SCENE_DAMAGED_WINDOWS: std::sync::Mutex<Vec<usize>> =
    std::sync::Mutex::new(Vec::new());

/// Queue from any guest thread, including while AppKit is reparenting views.
/// No Objective-C pointer or mutable NSView field is touched here.
/// Until the main event pump captures an invalidation, the Metal fast path
/// must fail closed: a previous snapshot can omit a newly installed CA mask.
pub fn metal_scene_pending() -> bool {
    METAL_SCENE_DIRTY.load(std::sync::atomic::Ordering::Acquire)
}

pub(crate) fn invalidate_ca_layer(root: usize) {
    // Never inspect NSView or SDL from the setter thread. A root not in the
    // published owner table triggers a conservative full-window fallback.
    let mut roots = METAL_CA_ROOTS.lock().expect("CA dirty roots");
    if !roots.contains(&root) {
        roots.push(root);
    }
    METAL_SCENE_DIRTY.store(true, std::sync::atomic::Ordering::Release);
}

pub(crate) fn invalidate_metal_scene(ca_damage: bool) {
    METAL_VIEW_SCENE_DIRTY.store(true, std::sync::atomic::Ordering::Release);
    if ca_damage {
        METAL_CA_DAMAGE.store(true, std::sync::atomic::Ordering::Release);
    }
    METAL_SCENE_DIRTY.store(true, std::sync::atomic::Ordering::Release);
}

/// Safe on any thread: only numeric SDL identities cross this queue.
pub(crate) fn invalidate_metal_scene_for_window(sdl: sdl3::Window) {
    if sdl.is_null() {
        invalidate_metal_scene(false);
        return;
    }
    METAL_VIEW_DIRTY_WINDOWS
        .lock()
        .expect("view dirty windows")
        .insert(sdl as usize);
    METAL_SCENE_DIRTY.store(true, std::sync::atomic::Ordering::Release);
}

/// Request from the Vulkan initializer. No AppKit/NSView state is read on
/// that thread; the next video pump recaptures and publishes even if identical.
pub fn request_metal_scene_replay(sdl: sdl3::Window) {
    if sdl.is_null()
        || !METAL_SCENES
            .lock()
            .expect("AppKit scene snapshots")
            .as_ref()
            .is_some_and(|scenes| scenes.contains_key(&(sdl as usize)))
    {
        return;
    }
    METAL_SCENE_REPLAY_WINDOWS
        .lock()
        .expect("Metal swapchain replay requests")
        .insert(sdl as usize);
    invalidate_metal_scene_for_window(sdl);
}

/// Ordinary view setters run in AppKit's owner context. Never dereference an
/// NSView on a Metal presentation worker or other unknown guest thread.
pub(crate) fn invalidate_metal_scene_for_view(view: Id) {
    if !sdl3::video::is_video_thread() || !is_view(view) {
        invalidate_metal_scene(false);
        return;
    }
    let window = unsafe { window_of(view) };
    if window.is_null() {
        invalidate_metal_scene(false);
        return;
    }
    invalidate_metal_scene_for_window(unsafe { crate::window::sdl_window(window) });
}

/// Called by the video-thread event pump after dispatching guest input.
/// An invalidation racing a capture stays dirty for the following turn.
/// IOSurface writers need not call setContents: again. Poll only the CA
/// descendants which belong to a Metal-backed view, on AppKit's event thread.
fn poll_metal_surface_publications() {
    if !crate::coreanimation::has_surface_publications() {
        return;
    }
    let owners = METAL_SURFACE_POLL_OWNERS
        .lock()
        .expect("Metal IOSurface owner registry")
        .iter()
        .flat_map(|(window, views)| {
            views
                .iter()
                .cloned()
                .map(|view| (*window, view))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for (window, owner) in owners {
        let view = owner.view as Id;
        let attached = unsafe { window_of(view) };
        if attached.is_null() || unsafe { crate::window::sdl_window(attached) } as usize != window {
            continue;
        }
        let layer = view_of(view).layer;
        if layer.is_null() || !crate::layer::is_metal(layer) {
            continue;
        }
        #[cfg(test)]
        METAL_SURFACE_POLL_VISITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(frame) = unsafe { presentation_frame(view) } {
            let rect = crate::backing::Rect {
                x: i64::from(frame.x),
                y: i64::from(frame.y),
                width: i64::from(frame.width),
                height: i64::from(frame.height),
            };
            let viewport = crate::backing::Rect {
                x: 0,
                y: 0,
                width: i64::from(frame.window_width),
                height: i64::from(frame.window_height),
            };
            if crate::coreanimation::metal_surface_publication_changed(view, rect, viewport) {
                crate::coreanimation::changed(layer);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn surface_poll_owner_count_for_test(window: sdl3::Window) -> usize {
    METAL_SURFACE_POLL_OWNERS
        .lock()
        .expect("Metal IOSurface owner registry")
        .get(&(window as usize))
        .map_or(0, Vec::len)
}

#[cfg(test)]
pub(crate) fn surface_poll_visits_for_test() -> usize {
    METAL_SURFACE_POLL_VISITS.load(std::sync::atomic::Ordering::Relaxed)
}

fn capture_surface_poll_owners(window: sdl3::Window) {
    let mut owners = Vec::new();
    if crate::coreanimation::has_surface_publications() {
        let nswindow = crate::window::window_for(window);
        if !nswindow.is_null() {
            fn visit(
                view: Id,
                depth: usize,
                owners: &mut Vec<std::sync::Arc<MetalSurfacePollOwner>>,
            ) {
                if depth >= 64 || !is_view(view) {
                    return;
                }
                let layer = view_of(view).layer;
                if !layer.is_null()
                    && crate::layer::is_metal(layer)
                    && crate::coreanimation::metal_subtree_has_surface(layer)
                {
                    owners.push(std::sync::Arc::new(MetalSurfacePollOwner::new(view)));
                }
                let children = view_of(view).subviews;
                if let Some(children) =
                    unsafe { corefoundation::collections::array(children as *const c_void) }
                {
                    for &child in &children.values {
                        visit(child as Id, depth + 1, owners);
                    }
                }
            }
            visit(crate::window::content_view_of(nswindow), 0, &mut owners);
        }
    }
    // Destructors release NSViews and may invalidate another scene. Drop old
    // registry entries without holding this registry's lock.
    let old = METAL_SURFACE_POLL_OWNERS
        .lock()
        .expect("Metal IOSurface owner registry")
        .insert(window as usize, owners);
    drop(old);
}

/// Animation frames intentionally publish without waiting for new drawables.
pub(crate) fn flush_metal_scenes() {
    if !sdl3::video::is_video_thread() {
        return;
    }
    service_native_cpu_fallbacks();
    poll_metal_surface_publications();
    let now = darwin::mach::media_time();
    let animated_roots = crate::coreanimation::scene_animation_roots(now);
    let animating = !animated_roots.is_empty();
    let was_animating =
        METAL_SCENE_WAS_ANIMATING.swap(animating, std::sync::atomic::Ordering::AcqRel);
    if animating && !metal_scene_pending() {
        let last = METAL_SCENE_LAST_ANIM_FRAME
            .lock()
            .expect("Metal animation tick");
        if last.is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(4)) {
            return;
        }
    }
    let replay = std::mem::take(
        &mut *METAL_SCENE_REPLAY_WINDOWS
            .lock()
            .expect("Metal swapchain replay requests"),
    );
    let dirty = METAL_SCENE_DIRTY.swap(false, std::sync::atomic::Ordering::AcqRel);
    let damage = METAL_CA_DAMAGE.swap(false, std::sync::atomic::Ordering::AcqRel);
    let view_dirty = METAL_VIEW_SCENE_DIRTY.swap(false, std::sync::atomic::Ordering::AcqRel);
    let targeted =
        std::mem::take(&mut *METAL_VIEW_DIRTY_WINDOWS.lock().expect("view dirty windows"));
    if (damage || view_dirty || !targeted.is_empty() || !replay.is_empty()) && !dirty {
        if damage {
            METAL_CA_DAMAGE.store(true, std::sync::atomic::Ordering::Release);
        }
        if view_dirty {
            METAL_VIEW_SCENE_DIRTY.store(true, std::sync::atomic::Ordering::Release);
        }
        METAL_VIEW_DIRTY_WINDOWS
            .lock()
            .expect("view dirty windows")
            .extend(targeted.iter().copied());
        METAL_SCENE_REPLAY_WINDOWS
            .lock()
            .expect("Metal swapchain replay requests")
            .extend(replay.iter().copied());
        METAL_SCENE_DIRTY.store(true, std::sync::atomic::Ordering::Release);
    }
    if !dirty && !animating && !was_animating {
        return;
    }
    if animating {
        *METAL_SCENE_LAST_ANIM_FRAME
            .lock()
            .expect("Metal animation tick") = Some(std::time::Instant::now());
    }
    // Drain only the roots pending at this transaction boundary. A CA setter
    // racing the drain re-sets DIRTY and its root persists to the next turn.
    let roots = std::mem::take(&mut *METAL_CA_ROOTS.lock().expect("CA dirty roots"));
    let mut all_roots = roots.clone();
    {
        let mut last = METAL_LAST_ANIM_ROOTS
            .lock()
            .expect("last CA animation roots");
        if was_animating && !animating {
            all_roots.extend(last.iter().copied());
        }
        *last = animated_roots.clone();
    }
    all_roots.extend(animated_roots);
    let owners = METAL_SCENE_OWNERS.lock().expect("CA owner windows");
    let mut affected = std::collections::HashSet::new();
    let mut unknown = false;
    for root in all_roots {
        let mut matched = false;
        for (window, layers) in owners.iter() {
            if layers.contains(&root) {
                affected.insert(*window);
                matched = true;
            }
        }
        if !matched && roots.contains(&root) {
            // A newly mutated CA root without a known window still needs a
            // conservative recapture. An idle detached animation does not:
            // it must not redraw unrelated windows every video-thread tick.
            unknown = true;
        }
    }
    drop(owners);
    // Known view/overlay/window mutations only affect that live SDL owner.
    // Unknown lifecycle owners and unscoped CA damage conservatively fan out.
    let global = damage || view_dirty || unknown;
    affected.extend(targeted);
    affected.extend(replay.iter().copied());
    for window in crate::window::all() {
        let sdl = unsafe { crate::window::sdl_window(window) };
        if sdl.is_null() {
            continue;
        }
        if !global && !affected.contains(&(sdl as usize)) {
            continue;
        }
        let underlay_animating =
            (animating || was_animating) && crate::backing::underlay_animation_tick(sdl, now);
        if underlay_animating
            || damage
            || !roots.is_empty() && (unknown || affected.contains(&(sdl as usize)))
        {
            #[cfg(test)]
            METAL_SCENE_DAMAGED_WINDOWS
                .lock()
                .expect("CA damage audit")
                .push(sdl as usize);
            crate::backing::damage_view(crate::window::content_view_of(window));
        }
        publish_metal_scene_at(sdl, now, replay.contains(&(sdl as usize)));
    }
}

pub fn set_metal_scene_changed(hook: fn(sdl3::Window, Vec<MetalSceneLayer>)) {
    let _ = METAL_SCENE_CHANGED.set(hook);
}

/// Capture visible CAMetalLayer views in the order their enclosing NSViews
/// would paint: ancestors precede descendants, later siblings are on top.
/// A missing AppKit window is distinct from a window with no Metal layers.
/// Gate backing pixel redraw recaptures by published Metal membership, not
/// previously captured native pixels. The first drawRect: of a native view
/// *creates* the source and must be discovered even if none existed before.
/// Unrelated software-only windows still avoid scene captures.
pub(crate) fn native_metal_scene_active(window: sdl3::Window) -> bool {
    METAL_SCENES
        .lock()
        .expect("AppKit scene snapshots")
        .as_ref()
        .and_then(|all| all.get(&(window as usize)))
        .is_some_and(|scene| {
            scene
                .iter()
                .any(|item| item.native_pixels.is_none() && item.native_dmabuf.is_none())
        })
}

pub fn metal_scene(window: sdl3::Window) -> Option<Vec<MetalSceneLayer>> {
    METAL_SCENES
        .lock()
        .expect("AppKit scene snapshots")
        .as_ref()
        .and_then(|all| all.get(&(window as usize)).cloned())
}

/// Evaluate CA opacity/visibility of every NSView backing layer from the
/// view root down. The CA superlayer chain can omit an NSView's enclosing
/// view layer; scene_properties_below stops at that enclosing layer so the
/// two representations of the same ancestor are not multiplied twice.
/// This function is only valid on AppKit's event thread.
pub(crate) fn native_scene_ca_opacity(view: Id, now: f64) -> f32 {
    let mut chain = Vec::new();
    let mut at = view;
    for _ in 0..64 {
        if at.is_null() || !is_view(at) {
            break;
        }
        chain.push(at);
        at = view_of(at).superview;
    }
    let mut opacity = 1.0;
    let mut parent_layer = std::ptr::null_mut();
    for &view in chain.iter().rev() {
        let layer = view_of(view).layer;
        if layer.is_null() {
            continue;
        }
        let (hidden, local_opacity) =
            crate::coreanimation::scene_properties_below(layer, parent_layer, now);
        if hidden {
            return 0.0;
        }
        opacity *= local_opacity;
        parent_layer = layer;
    }
    opacity
}

/// Project an intermediate CA mask through its enclosing NSView's bounds
/// into the same physical pixel coordinate system as the Metal compositor.
fn project_ca_mask(ancestor_view: Id, rect: NSRect) -> Option<Frame> {
    let ancestor = unsafe { presentation_frame(ancestor_view) }?;
    let bounds = bounds_of(ancestor_view);
    if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
        return None;
    }
    let sx = f64::from(ancestor.width) / bounds.size.width;
    let sy = f64::from(ancestor.height) / bounds.size.height;
    let left = f64::from(ancestor.x) + (rect.origin.x - bounds.origin.x) * sx;
    let top = if is_flipped(ancestor_view) {
        f64::from(ancestor.y) + (rect.origin.y - bounds.origin.y) * sy
    } else {
        f64::from(ancestor.y)
            + (bounds.origin.y + bounds.size.height - rect.origin.y - rect.size.height) * sy
    };
    let right = left + rect.size.width * sx;
    let bottom = top + rect.size.height * sy;
    if ![left, top, right, bottom].iter().all(|f| f.is_finite()) {
        return None;
    }
    let clamp = |n: f64| n.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let (x0, y0, x1, y1) = (clamp(left), clamp(top), clamp(right), clamp(bottom));
    Some(Frame {
        x: x0,
        y: y0,
        width: i64::from(x1)
            .saturating_sub(i64::from(x0))
            .clamp(0, i64::from(u32::MAX)) as u32,
        height: i64::from(y1)
            .saturating_sub(i64::from(y0))
            .clamp(0, i64::from(u32::MAX)) as u32,
        ..ancestor
    })
}

/// Opaque native coverage can only subtract a rectangle from Metal if no
/// ancestor's rounded CA mask removes any of its pixels. Otherwise both
/// sources must be drawn and the native source gets its rounded GPU mask.
pub(crate) fn has_rounded_mask_ancestor(view: Id) -> bool {
    let mut at = view;
    for _ in 0..64 {
        if at.is_null() || !is_view(at) {
            break;
        }
        let layer = view_of(at).layer;
        let parent_view = view_of(at).superview;
        let mut parent = parent_view;
        let mut ancestor_layer = std::ptr::null_mut();
        for _ in 0..64 {
            if parent.is_null() || !is_view(parent) {
                break;
            }
            ancestor_layer = view_of(parent).layer;
            if !ancestor_layer.is_null() {
                break;
            }
            parent = view_of(parent).superview;
        }
        if !layer.is_null()
            && ((crate::coreanimation::scene_masks_to_bounds(layer)
                && crate::coreanimation::scene_corner_radius(layer) > 0.0)
                || crate::coreanimation::scene_has_rounded_masks_below(layer, ancestor_layer))
        {
            return true;
        }
        at = parent_view;
    }
    false
}

/// Only invoked on the AppKit mutator thread, not from Metal's presenter.
fn capture_metal_scene(
    window: sdl3::Window,
    now: f64,
) -> Option<(Vec<MetalSceneLayer>, Vec<usize>)> {
    // CA content reads may release their last guest reference. Defer such
    // releases until no backing/view locks are held by this capture.
    let _owed = crate::coreanimation::defer_releases();
    let nswindow = crate::window::window_for(window);
    if nswindow.is_null() {
        return None;
    }
    let content = crate::window::content_view_of(nswindow);
    let mut result = Vec::new();
    let mut roots = Vec::new();
    let mut seen_metal = false;
    visit_metal_scene(
        content,
        window,
        &mut result,
        &mut roots,
        &mut seen_metal,
        None,
        1.0,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &[],
        &[],
        now,
        0,
    );
    // Live native occlusion is orthogonal to drawable submission order: a
    // toolbar may move after Metal's last present or before CPU backing exists.
    for (layer, coverage) in crate::backing::live_metal_occlusion(window, &result) {
        if let Some(scene) = result.iter_mut().find(|item| item.layer == layer) {
            scene.occluded = coverage
                .into_iter()
                .map(|rect| Frame {
                    window,
                    x: rect.x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                    y: rect.y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                    width: rect.width.clamp(0, i64::from(u32::MAX)) as u32,
                    height: rect.height.clamp(0, i64::from(u32::MAX)) as u32,
                    ..scene.frame
                })
                .collect();
        }
    }
    #[cfg(test)]
    METAL_SCENE_CAPTURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some((result, roots))
}

#[allow(clippy::too_many_arguments)]
fn capture_metal_subtree_segment(
    view: Id,
    frame: Frame,
    selected: &[usize],
    isolated_group_ids: &[usize],
    ca_clip: Option<Frame>,
    rounded: &[RoundedSceneClip],
    opacity: f32,
    opacity_groups: &[OpacityGroup],
    tile: &mut u32,
    scene: &mut Vec<MetalSceneLayer>,
) {
    let viewport = Frame {
        x: 0,
        y: 0,
        width: frame.window_width,
        height: frame.window_height,
        ..frame
    };
    let descendants = crate::coreanimation::metal_sublayers_extent_selected(
        view,
        crate::backing::Rect {
            x: i64::from(frame.x),
            y: i64::from(frame.y),
            width: i64::from(frame.width),
            height: i64::from(frame.height),
        },
        selected,
    )
    .unwrap_or_default();
    let extent = Frame {
        x: descendants
            .x
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y: descendants
            .y
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        width: descendants.width.clamp(0, i64::from(u32::MAX)) as u32,
        height: descendants.height.clamp(0, i64::from(u32::MAX)) as u32,
        ..frame
    };
    let visible = intersect_scene_clips(extent, viewport);
    let visible = ca_clip.map_or(visible, |clip| intersect_scene_clips(visible, clip));
    const TILE_EDGE: u32 = 2048;
    let mut y = 0u32;
    while y < visible.height {
        let h = (visible.height - y).min(TILE_EDGE);
        let mut x = 0u32;
        while x < visible.width {
            let w = (visible.width - x).min(TILE_EDGE);
            let part = Frame {
                x: visible.x.saturating_add(x as i32),
                y: visible.y.saturating_add(y as i32),
                width: w,
                height: h,
                ..visible
            };
            let crop = crate::backing::Rect {
                x: i64::from(part.x) - i64::from(frame.x),
                y: i64::from(part.y) - i64::from(frame.y),
                width: i64::from(w),
                height: i64::from(h),
            };
            if let Some((pixels, revision)) = crate::backing::metal_sublayers_segment_pixels_cropped(
                view,
                frame.width,
                frame.height,
                crop,
                selected,
                isolated_group_ids,
            ) {
                scene.push(MetalSceneLayer {
                    opacity_groups: opacity_groups.to_vec(),
                    layer: native_tile_id(view, *tile),
                    native_owner: view as usize,
                    frame: part,
                    opacity,
                    opaque: false,
                    clip: ca_clip,
                    rounded: rounded.to_vec(),
                    occluded: Vec::new(),
                    native_cpu_capture: None,
                    native_pixels: Some(pixels),
                    native_dmabuf: None,
                    native_revision: revision,
                    native_dma_sample: None,
                });
            }
            *tile = tile.saturating_add(1);
            x += w;
        }
        y += h;
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Scene traversal forwards inherited clipping and opacity state without copying layers"
)]
fn visit_metal_scene(
    view: Id,
    window: sdl3::Window,
    scene: &mut Vec<MetalSceneLayer>,
    roots: &mut Vec<usize>,
    seen_metal: &mut bool,
    inherited_clip: Option<Frame>,
    inherited_layer_opacity: f32,
    parent_layer: Id,
    parent_view: Id,
    inherited_rounded: &[RoundedSceneClip],
    inherited_groups: &[OpacityGroup],
    now: f64,
    depth: usize,
) {
    if depth >= 64 || !is_view(view) || view_of(view).hidden != 0 {
        return;
    }
    let layer = view_of(view).layer;
    if !layer.is_null() {
        roots.push(layer as usize);
    }
    let (hidden, own_layer_opacity) = if layer.is_null() {
        (false, 1.0)
    } else {
        let excluded_ids = inherited_groups
            .iter()
            .map(|group| group.id)
            .collect::<Vec<_>>();
        crate::coreanimation::scene_properties_below_excluding(
            layer,
            parent_layer,
            now,
            &excluded_ids,
        )
    };
    // Retain an invisible source's identity and immutable CPU pixels so an
    // opacity-only transition back to visible does not force a new raster.
    // The Metal compositor excludes its zero-alpha placement from drawing
    // and group scratch preflight, rather than dropping the source itself.
    if hidden {
        return;
    }
    let descendants = unsafe { children(view) };
    let backed_descendants = !descendants.is_empty();
    // Both NSView alphaValue and CALayer opacity attenuate their *completed*
    // subtree. A scalar applied separately to retained children leaks the
    // lower opaque child through an opaque higher child.
    let own_view_alpha = alpha_of(view) as f32;
    let has_view_group = own_view_alpha < 1.0
        && (backed_descendants
            || !layer.is_null()
                && crate::layer::is_metal(layer)
                && crate::coreanimation::has_renderable_sublayers(layer));
    let has_layer_group = !layer.is_null()
        && own_layer_opacity < 1.0
        && (backed_descendants
            || crate::layer::is_metal(layer)
                && crate::coreanimation::has_renderable_sublayers(layer));
    let mut opacity_groups = inherited_groups.to_vec();
    if has_view_group {
        opacity_groups.push(OpacityGroup {
            id: view as usize,
            opacity: own_view_alpha,
        });
    }
    if has_layer_group {
        opacity_groups.push(OpacityGroup {
            id: layer as usize,
            opacity: own_layer_opacity,
        });
    }
    let source_view_alpha = if has_view_group { 1.0 } else { own_view_alpha };
    let layer_opacity = inherited_layer_opacity
        * if has_layer_group {
            1.0
        } else {
            own_layer_opacity
        };
    let has_ca_mask = !layer.is_null() && crate::coreanimation::scene_masks_to_bounds(layer);
    let current_masks = view_of(view).layer_backing & CLIPS_TO_BOUNDS != 0 || has_ca_mask;
    let own_frame = current_masks
        .then(|| unsafe { presentation_frame(view) })
        .flatten();
    let mut rounded = inherited_rounded.to_vec();
    let mut ca_clip = inherited_clip;
    if !layer.is_null() && !parent_view.is_null() {
        for (mask, radius) in crate::coreanimation::scene_masks_below(layer, parent_layer, now) {
            if let Some(frame) = project_ca_mask(parent_view, mask) {
                ca_clip = Some(ca_clip.map_or(frame, |clip| intersect_scene_clips(frame, clip)));
                if radius > 0.0 {
                    let bounds = bounds_of(parent_view).size;
                    let parent = unsafe { presentation_frame(parent_view) };
                    if let Some(parent) = parent {
                        let sx =
                            f64::from(parent.width) / bounds.width.abs().max(f64::MIN_POSITIVE);
                        let sy =
                            f64::from(parent.height) / bounds.height.abs().max(f64::MIN_POSITIVE);
                        let physical = radius * sx.min(sy);
                        if physical.is_finite() && physical > 0.0 {
                            rounded.push(RoundedSceneClip {
                                frame,
                                radius: physical as f32,
                            });
                        }
                    }
                }
            }
        }
    }
    if has_ca_mask {
        let radius = crate::coreanimation::scene_corner_radius(layer);
        if radius > 0.0
            && let Some(frame) = own_frame
        {
            let bounds = bounds_of(view).size;
            let scale_x = f64::from(frame.width) / bounds.width.abs().max(f64::MIN_POSITIVE);
            let scale_y = f64::from(frame.height) / bounds.height.abs().max(f64::MIN_POSITIVE);
            let physical = radius * scale_x.min(scale_y);
            if physical.is_finite() && physical > 0.0 {
                rounded.push(RoundedSceneClip {
                    frame,
                    radius: physical as f32,
                });
            }
        }
    }
    if !layer.is_null()
        && crate::layer::is_metal(layer)
        && let Some(frame) = unsafe { presentation_frame(view) }
        && frame.window == window
    {
        scene.push(MetalSceneLayer {
            opacity_groups: opacity_groups.clone(),
            layer: layer as usize,
            frame,
            opacity: source_view_alpha * layer_opacity,
            opaque: crate::layer::opaque_of(layer),
            clip: ca_clip,
            rounded: rounded.clone(),
            occluded: Vec::new(),
            native_pixels: None,
            native_cpu_capture: None,
            native_dmabuf: None,
            native_revision: 0,
            native_dma_sample: None,
            native_owner: 0,
        });
        *seen_metal = true;
    }
    let child_clip = if current_masks {
        own_frame.map(|frame| ca_clip.map_or(frame, |clip| intersect_scene_clips(frame, clip)))
    } else {
        ca_clip
    };
    // The effective CA hierarchy, not the NSView child list alone, owns the
    // stacking order when ordinary layers interleave with backed views.
    let children = view_of(view).subviews;
    let child_views: Vec<Id> =
        unsafe { corefoundation::collections::array(children as *const c_void) }
            .map_or_else(Vec::new, |array| {
                array.values.iter().map(|child| *child as Id).collect()
            });
    let mut ca_visited = std::collections::HashSet::new();
    if !layer.is_null()
        && crate::layer::is_metal(layer)
        && let Some(frame) = unsafe { presentation_frame(view) }
        && frame.window == window
    {
        let backed: Vec<(usize, Id)> = child_views
            .iter()
            .copied()
            .filter_map(|child| {
                let backing = crate::view::layer_of(child);
                (!backing.is_null()).then_some((backing as usize, child))
            })
            .collect();
        let layer_ids = backed.iter().map(|entry| entry.0).collect::<Vec<_>>();
        let paint = crate::coreanimation::metal_paint_steps(layer, &layer_ids, now);
        let mut tile = 0u32;
        for step in paint {
            match step {
                crate::coreanimation::MetalPaintStep::Native { selected, groups } => {
                    let mut combined = opacity_groups.clone();
                    combined.extend(groups.iter().copied());
                    let isolated_ids = groups.iter().map(|g| g.id).collect::<Vec<_>>();
                    capture_metal_subtree_segment(
                        view,
                        frame,
                        &selected,
                        &isolated_ids,
                        ca_clip,
                        &rounded,
                        source_view_alpha * layer_opacity,
                        &combined,
                        &mut tile,
                        scene,
                    );
                }
                crate::coreanimation::MetalPaintStep::View { id, groups } => {
                    if let Some((_, child)) = backed.iter().find(|entry| entry.0 == id) {
                        let mut combined = opacity_groups.clone();
                        combined.extend(groups.iter().copied());
                        ca_visited.insert(*child as usize);
                        visit_metal_scene(
                            *child,
                            window,
                            scene,
                            roots,
                            seen_metal,
                            child_clip,
                            layer_opacity,
                            layer,
                            view,
                            &rounded,
                            &combined,
                            now,
                            depth + 1,
                        );
                    }
                }
            }
        }
    }
    // Per-view native content needs its own source above the preceding
    // drawable. The high bit reserves an identity namespace distinct from
    // user-space Objective-C layer pointers and SDL window identities.
    if *seen_metal
        && (layer.is_null() || !crate::layer::is_metal(layer))
        && let Some(frame) = unsafe { presentation_frame(view) }
        && frame.window == window
    {
        // A 6K NSView clipped into a small Metal window only needs
        // that visible rectangle in GPU memory. Preserve source-local
        // coordinates while publishing the cropped frame in window
        // pixels; Metal never gets the flattened-underlay version.
        let viewport = Frame {
            x: 0,
            y: 0,
            width: frame.window_width,
            height: frame.window_height,
            ..frame
        };
        let visible = ca_clip.map_or(intersect_scene_clips(frame, viewport), |clip| {
            intersect_scene_clips(intersect_scene_clips(frame, viewport), clip)
        });
        let dma = crate::backing::native_scene_dma_extent_supported(frame.width, frame.height)
            .then(|| crate::backing::native_dma_overlay(view))
            .flatten();
        // The 64 MiB capture bound applies to one allocation, not an
        // entire visible NSView. Publish 2048x2048 pixel tiles in the
        // same paint order, each with an independent GPU lease.
        let large_visible =
            !crate::backing::native_scene_extent_supported(visible.width, visible.height);
        let tile_edge = if large_visible { 2048 } else { u32::MAX };
        let mut index = 0u32;
        let mut y = 0u32;
        while y < visible.height {
            let height = (visible.height - y).min(tile_edge);
            let mut x = 0u32;
            while x < visible.width {
                let width = (visible.width - x).min(tile_edge);
                let part = Frame {
                    x: visible.x.saturating_add(x as i32),
                    y: visible.y.saturating_add(y as i32),
                    width,
                    height,
                    ..visible
                };
                let cropped = crate::backing::Rect {
                    x: i64::from(part.x) - i64::from(frame.x),
                    y: i64::from(part.y) - i64::from(frame.y),
                    width: i64::from(width),
                    height: i64::from(height),
                };
                let cpu = crate::backing::native_scene_pixels_cropped_with_damage(
                    view,
                    frame.width,
                    frame.height,
                    cropped,
                );
                // A DMA source remains one coherent GPU image. A CPU
                // tile never borrows a DMA producer's unrelated frame.
                if cpu.is_some() || dma.is_some() {
                    let sample = dma.as_ref().map(|_| {
                        (
                            frame.width,
                            frame.height,
                            cropped.x as u32,
                            cropped.y as u32,
                        )
                    });
                    // Moving a large static DMA view can keep the
                    // visible output frame unchanged while changing
                    // which producer texels are sampled.
                    let revision = cpu.as_ref().map_or(0, |(_, rev, _)| *rev)
                        ^ dma.as_ref().map_or(0, |(_, rev)| rev.rotate_left(19))
                        ^ sample.map_or(0, |(w, h, x, y)| {
                            (u64::from(w) << 32 | u64::from(h)).rotate_left(11)
                                ^ (u64::from(x) << 32 | u64::from(y)).rotate_left(37)
                        });
                    scene.push(MetalSceneLayer {
                        opacity_groups: opacity_groups.clone(),
                        layer: native_tile_id(view, index),
                        native_owner: view as usize,
                        frame: part,
                        opacity: source_view_alpha * layer_opacity,
                        opaque: false,
                        clip: ca_clip,
                        rounded: rounded.clone(),
                        occluded: Vec::new(),
                        native_cpu_capture: cpu.as_ref().map(|(_, rev, damage)| {
                            crate::backing::NativeCpuCapture {
                                revision: *rev,
                                damage: *damage,
                            }
                        }),
                        native_pixels: cpu.map(|(pixels, _, _)| pixels),
                        native_dmabuf: dma.as_ref().map(|(p, _)| std::sync::Arc::clone(p)),
                        native_dma_sample: sample,
                        native_revision: revision,
                    });
                }
                index += 1;
                x += width;
            }
            y += height;
        }
    }
    for child in child_views {
        if !ca_visited.contains(&(child as usize)) {
            visit_metal_scene(
                child,
                window,
                scene,
                roots,
                seen_metal,
                child_clip,
                layer_opacity,
                if layer.is_null() { parent_layer } else { layer },
                if layer.is_null() { parent_view } else { view },
                &rounded,
                &opacity_groups,
                now,
                depth + 1,
            );
        }
    }
}

/// Clipping uses independently rounded window-pixel bounds. Saturating i64
/// endpoints prevent a malformed guest view rectangle from wrapping i32.
fn intersect_scene_clips(a: Frame, b: Frame) -> Frame {
    let left = i64::from(a.x).max(i64::from(b.x));
    let top = i64::from(a.y).max(i64::from(b.y));
    let right = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width));
    let bottom = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height));
    Frame {
        x: left.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y: top.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        width: right.saturating_sub(left).clamp(0, i64::from(u32::MAX)) as u32,
        height: bottom.saturating_sub(top).clamp(0, i64::from(u32::MAX)) as u32,
        ..a
    }
}

fn publish_metal_scene_at(sdl: sdl3::Window, now: f64, force: bool) {
    debug_assert!(sdl3::video::is_video_thread());
    let (scene, roots) = capture_metal_scene(sdl, now).unwrap_or_default();
    capture_surface_poll_owners(sdl);
    METAL_SCENE_OWNERS
        .lock()
        .expect("CA owner windows")
        .insert(sdl as usize, roots);
    let changed = METAL_SCENES
        .lock()
        .expect("AppKit scene snapshots")
        .get_or_insert_with(std::collections::HashMap::new)
        .insert(sdl as usize, scene.clone());
    // Avoid waking the GPU worker for identical snapshots after a scene
    // mutation or an animation that has reached its final presentation value.
    // Metadata comparisons are value-only and never dereference guest IDs.
    let differs = changed
        .as_ref()
        .is_none_or(|old| !same_metal_scene(old, &scene));
    if (differs || force)
        && let Some(hook) = METAL_SCENE_CHANGED.get()
    {
        hook(sdl, scene);
    }
}

fn same_metal_scene(a: &[MetalSceneLayer], b: &[MetalSceneLayer]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.layer == b.layer
                && a.opacity_groups == b.opacity_groups
                && a.frame == b.frame
                && a.opacity == b.opacity
                && a.opaque == b.opaque
                && a.clip == b.clip
                && a.occluded == b.occluded
                && a.native_revision == b.native_revision
                && a.native_dma_sample == b.native_dma_sample
                && a.native_pixels.is_some() == b.native_pixels.is_some()
                && a.native_dmabuf.is_some() == b.native_dmabuf.is_some()
                && a.rounded.len() == b.rounded.len()
                && a.rounded
                    .iter()
                    .zip(&b.rounded)
                    .all(|(a, b)| a.frame == b.frame && a.radius == b.radius)
        })
}

pub fn forget_metal_scene(sdl: sdl3::Window) {
    METAL_SCENE_REPLAY_WINDOWS
        .lock()
        .expect("Metal swapchain replay requests")
        .remove(&(sdl as usize));
    let old = METAL_SURFACE_POLL_OWNERS
        .lock()
        .expect("Metal IOSurface owner registry")
        .remove(&(sdl as usize));
    drop(old);
    METAL_SCENE_OWNERS
        .lock()
        .expect("CA owner windows")
        .remove(&(sdl as usize));
    if let Some(cache) = METAL_SCENES
        .lock()
        .expect("AppKit scene snapshots")
        .as_mut()
    {
        cache.remove(&(sdl as usize));
    }
    invalidate_metal_scene_for_window(sdl);
}

fn notify_metal_scene(view: Id) {
    invalidate_metal_scene_for_view(view);
}

/// Post the frame and bounds notifications a resized view owes its observers.
///
/// Both, because this tree's `-bounds` is the frame's size at the origin: a
/// change to one *is* a change to the other, and a guest watching only the
/// bounds - which is what `-setPostsBoundsChangedNotifications:` asks for -
/// would otherwise never hear that the view resized.
fn note_geometry_change(receiver: Id, before: NSRect) {
    let view = view_of(receiver);
    if view.frame == before {
        return;
    }
    if view.frame.size != before.size {
        resize_subviews(receiver, before.size);
    }
    // AppKit keeps a view's layer the size of its bounds.
    let layer = view_of(receiver).layer;
    if !layer.is_null() {
        crate::layer::set_bounds_of(layer, bounds_of(receiver));
    }
    crate::layout::solve();
    let view = view_of(receiver);
    let posts = view.posts;
    if posts & POSTS_FRAME != 0 {
        foundation::notification::post(FRAME_DID_CHANGE, receiver, std::ptr::null_mut());
    }
    if posts & POSTS_BOUNDS != 0 && view.frame.size != before.size {
        foundation::notification::post(BOUNDS_DID_CHANGE, receiver, std::ptr::null_mut());
    }
    // Moving or resizing the focused editor also moves the host IME UI.
    // SAFETY: `receiver` is the live view whose geometry changed.
    unsafe { crate::window::refresh_text_input_area_for(receiver) };
    notify_metal_scene(receiver);
}

/// Resize `receiver`'s subviews for a superview that went from `old` to its
/// current size, as `-resizeSubviewsWithOldSize:` does.
///
/// This is what makes a window resize reach the view a program actually draws
/// into. AppKit resizes subviews on *every* frame change of their superview,
/// and a program's GL view is a subview of the window's content view - so
/// without this the content view follows the window and the view underneath it
/// keeps its original frame forever. The program then asks its own view how big
/// it is, gets the size the window had when it opened, and renders that size
/// for the rest of the run: a window that resizes and a picture that does not.
///
/// Each subview is *sent* `-resizeWithOldSuperviewSize:` rather than having its
/// frame written here, because that is the method AppKit documents as the one a
/// view overrides to lay itself out - so a guest's override has to be what runs.
fn resize_subviews(receiver: Id, old: NSSize) {
    let view = view_of(receiver);
    if view.does_not_autoresize_subviews != 0 || view.subviews.is_null() {
        return;
    }
    let list = view.subviews;
    // SAFETY: an array this module made and only ever fills with views.
    let Some(array) = (unsafe { corefoundation::collections::array(list as *const c_void) }) else {
        return;
    };
    let subviews: Vec<Id> = array.values.iter().map(|v| *v as Id).collect();
    let sel = objc::sel::get("resizeWithOldSuperviewSize:");
    for subview in subviews {
        if !is_view(subview) {
            continue;
        }
        // SAFETY: `-resizeWithOldSuperviewSize:` is `void (id, SEL, NSSize)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSSize) = std::mem::transmute(objc::msg::entry());
            f(subview, sel, old);
        }
    }
}

/// `-resizeWithOldSuperviewSize:`: put this view where its autoresizing mask
/// says it belongs, now that its superview has gone from `old` to its current
/// size.
///
/// A view whose mask is `NSViewNotSizable` keeps its frame, which is a real
/// answer and not a gap: a program that positions a fixed-size control in a
/// resizing window means it.
unsafe extern "C" fn resize_with_old_superview_size(receiver: Id, _sel: Sel, old: NSSize) {
    let view = view_of(receiver);
    let superview = view.superview;
    // A view placed by constraints is not also placed by its mask.
    if !is_view(superview) || view.constrained != 0 {
        return;
    }
    let new = view_of(superview).frame.size;
    let mask = view.autoresizing_mask;
    let before = view.frame;
    let (x, width) = distribute(
        mask & autoresizing::MIN_X_MARGIN != 0,
        mask & autoresizing::WIDTH_SIZABLE != 0,
        mask & autoresizing::MAX_X_MARGIN != 0,
        before.origin.x,
        before.size.width,
        old.width,
        new.width,
    );
    let (y, height) = distribute(
        mask & autoresizing::MIN_Y_MARGIN != 0,
        mask & autoresizing::HEIGHT_SIZABLE != 0,
        mask & autoresizing::MAX_Y_MARGIN != 0,
        before.origin.y,
        before.size.height,
        old.height,
        new.height,
    );
    let frame = NSRect {
        origin: NSPoint { x, y },
        size: NSSize { width, height },
    };
    if frame == before {
        return;
    }
    // Sent rather than written, for the reason `-[NSWindow setContentView:]`
    // sends it: a view that tracks its own size overrides `-setFrame:`, and
    // this path also has to reach the subviews and observers of *this* view.
    // SAFETY: `-setFrame:` is `void (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("setFrame:"), frame);
    }
}

/// How a view its autoresizing mask places follows its superview: what its
/// origin and its size gain per point the superview grows, along x then y.
/// The solver reads this as AppKit reads a mask into constraints, so a
/// constraint on such a view can size its superview. `None` when the
/// superview does not resize its subviews.
pub(crate) fn autoresizing_rates(view: Id) -> Option<[(CGFloat, CGFloat); 2]> {
    let v = view_of(view);
    if !is_view(v.superview) || view_of(v.superview).does_not_autoresize_subviews != 0 {
        return None;
    }
    let old = view_of(v.superview).frame.size;
    let mask = v.autoresizing_mask;
    let rate = |leading, sizable, trailing, origin: CGFloat, size: CGFloat, old: CGFloat| {
        let (o, s) = distribute(leading, sizable, trailing, origin, size, old, old + 1.0);
        (o - origin, s - size)
    };
    Some([
        rate(
            mask & autoresizing::MIN_X_MARGIN != 0,
            mask & autoresizing::WIDTH_SIZABLE != 0,
            mask & autoresizing::MAX_X_MARGIN != 0,
            v.frame.origin.x,
            v.frame.size.width,
            old.width,
        ),
        rate(
            mask & autoresizing::MIN_Y_MARGIN != 0,
            mask & autoresizing::HEIGHT_SIZABLE != 0,
            mask & autoresizing::MAX_Y_MARGIN != 0,
            v.frame.origin.y,
            v.frame.size.height,
            old.height,
        ),
    ])
}

/// Spread a superview's change along one axis over the three lengths that make
/// it up: the leading margin, the subview itself, and the trailing margin.
///
/// Only the flexible ones give, and they give in proportion to their current
/// sizes - which is AppKit's rule and the one that matters, because a view that
/// is `WidthSizable` with two fixed margins must absorb the whole change. When
/// every flexible length is currently zero the change is split evenly between
/// them instead, since proportional shares of nothing are all nothing and the
/// view would not move at all.
///
/// Nothing flexible means nothing moves: `NSViewNotSizable` is a real answer
/// and a view that asked for it keeps its frame.
#[allow(clippy::too_many_arguments)]
fn distribute(
    leading: bool,
    sizable: bool,
    trailing: bool,
    origin: CGFloat,
    size: CGFloat,
    old: CGFloat,
    new: CGFloat,
) -> (CGFloat, CGFloat) {
    let delta = new - old;
    if delta == 0.0 {
        return (origin, size);
    }
    let trailing_margin = old - origin - size;
    let parts = [
        (leading, origin),
        (sizable, size),
        (trailing, trailing_margin),
    ];
    let flexible: Vec<CGFloat> = parts
        .iter()
        .filter(|(is, _)| *is)
        .map(|(_, length)| *length)
        .collect();
    if flexible.is_empty() {
        return (origin, size);
    }
    let total: CGFloat = flexible.iter().sum();
    let share = |length: CGFloat| match total > 0.0 {
        true => delta * (length / total),
        false => delta / flexible.len() as CGFloat,
    };
    let origin = match leading {
        true => origin + share(origin),
        false => origin,
    };
    let size = match sizable {
        true => (size + share(size)).max(0.0),
        false => size,
    };
    (origin, size)
}

unsafe extern "C" fn set_autoresizing_mask(receiver: Id, _sel: Sel, mask: usize) {
    view_of(receiver).autoresizing_mask = mask;
}

unsafe extern "C" fn autoresizing_mask(receiver: Id, _sel: Sel) -> usize {
    view_of(receiver).autoresizing_mask
}

unsafe extern "C" fn set_autoresizes_subviews(receiver: Id, _sel: Sel, flag: u8) {
    view_of(receiver).does_not_autoresize_subviews = usize::from(flag == 0);
}

unsafe extern "C" fn autoresizes_subviews(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).does_not_autoresize_subviews == 0)
}

/// `-resizeSubviewsWithOldSize:`, which a program may call itself and which an
/// `NSView` subclass overriding `-resizeWithOldSuperviewSize:` expects to be
/// the thing that drives it.
unsafe extern "C" fn resize_subviews_with_old_size(receiver: Id, _sel: Sel, old: NSSize) {
    resize_subviews(receiver, old);
}

unsafe extern "C" fn set_posts_frame_changed(receiver: Id, _sel: Sel, flag: u8) {
    set_posts(receiver, POSTS_FRAME, flag);
}

unsafe extern "C" fn set_posts_bounds_changed(receiver: Id, _sel: Sel, flag: u8) {
    set_posts(receiver, POSTS_BOUNDS, flag);
}

fn set_posts(receiver: Id, bit: usize, flag: u8) {
    let view = view_of(receiver);
    view.posts = match flag != 0 {
        true => view.posts | bit,
        false => view.posts & !bit,
    };
}

unsafe extern "C" fn posts_frame_changed(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).posts & POSTS_FRAME != 0)
}

unsafe extern "C" fn posts_bounds_changed(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).posts & POSTS_BOUNDS != 0)
}

/// `-setLayer:`, which stores the layer and points it back at this view.
///
/// The view does not draw the layer. A guest that hands its view a
/// `CAMetalLayer` draws through the layer's own drawables, which are Metal's
/// business rather than the view's, and what the view owes it is to give the
/// same object back from `-layer`, because that is where the guest reaches
/// for it on every frame.
///
/// The back pointer is what makes those drawables presentable. A drawable
/// knows its layer and nothing else, and the window it belongs in is up the
/// view tree from here; see [`presentation_frame`].
unsafe extern "C" fn set_layer(receiver: Id, _sel: Sel, layer: Id) {
    let view = view_of(receiver);
    if view.layer == layer {
        return;
    }
    let previous = view.layer;
    view.layer = objc::arc::retain(layer);
    // Whatever replaced AppKit's layer is the program's; `-setWantsLayer:`
    // marks its own again once this returns.
    view.layer_backing &= !BACKING_LAYER;
    // SAFETY: a layer this runtime made, and a view which outlives it - the
    // view holds the only reference the layer is reached through.
    unsafe {
        crate::layer::set_view(previous, std::ptr::null_mut());
        crate::layer::set_view(layer, receiver);
    }
    if view.layer_backing & CLIPS_TO_BOUNDS != 0 && !layer.is_null() {
        crate::layer::set_masks_to_bounds_of(layer, true);
    }
    objc::arc::release(previous);
    notify_metal_scene(receiver);
}

/// `-setWantsLayer:`. Asking for a layer makes the view layer-backed: AppKit
/// sends the view `-makeBackingLayer` and then `-setLayer:` with what it
/// answered, so a subclass that overrides either - Qt's `QNSView` overrides
/// both - is the one that decides. A view that already has a layer, one a
/// program set, keeps it. Giving the wish up takes away a layer AppKit made
/// and leaves one a program set.
unsafe extern "C" fn set_wants_layer(receiver: Id, _sel: Sel, wants: u8) {
    let view = view_of(receiver);
    if wants == 0 {
        let made = view.layer_backing & BACKING_LAYER != 0;
        view.layer_backing &= !(WANTS_LAYER | BACKING_LAYER);
        if made {
            send_object(receiver, "setLayer:", std::ptr::null_mut());
        }
        return;
    }
    view.layer_backing |= WANTS_LAYER;
    if !view.layer.is_null() {
        return;
    }
    // SAFETY: `-makeBackingLayer` is `CALayer *(id, SEL)`, answered at +0.
    let layer = unsafe {
        let make: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        make(receiver, objc::sel::get("makeBackingLayer"))
    };
    if layer.is_null() {
        return;
    }
    crate::layer::set_bounds_of(layer, bounds_of(receiver));
    // A backing layer's delegate is its view, which is what routes the
    // layer's `-display` to the view.
    if crate::layer::delegate_of(layer).is_null() {
        crate::layer::set_delegate_of(layer, receiver);
    }
    crate::layer::set_contents_gravity_for_placement(layer, contents_placement(receiver));
    send_object(receiver, "setLayer:", layer);
    // `-setLayer:` may be overridden to refuse; the flag describes the layer
    // the view ended up with.
    let view = view_of(receiver);
    if view.layer == layer {
        view.layer_backing |= BACKING_LAYER;
    }
}

fn send_object(receiver: Id, name: &str, object: Id) {
    // SAFETY: a `void (id, SEL, id)` method on a live view.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(name), object);
    }
}

/// The view's `-layerContentsPlacement`, sent so an override answers.
fn contents_placement(view: Id) -> isize {
    // SAFETY: `-layerContentsPlacement` is `NSInteger (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
        f(view, objc::sel::get("layerContentsPlacement"))
    }
}

/// `-makeBackingLayer`: a plain `CALayer`, autoreleased.
unsafe extern "C" fn make_backing_layer(_receiver: Id, _sel: Sel) -> Id {
    let Some(class) = objc::class::lookup(c"CALayer") else {
        return std::ptr::null_mut();
    };
    // SAFETY: `+layer` is `id (Class, SEL)`.
    unsafe {
        let make: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
        make(objc::class::ptr(class) as Id, objc::sel::get("layer"))
    }
}

unsafe extern "C" fn wants_layer(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).layer_backing & WANTS_LAYER != 0)
}

/// `-layerContentsPlacement`, whose default of zero is
/// `NSViewLayerContentsPlacementScaleAxesIndependently`.
unsafe extern "C" fn layer_contents_placement(receiver: Id, _sel: Sel) -> isize {
    (view_of(receiver).layer_backing >> PLACEMENT_SHIFT) as isize
}

/// `-setLayerContentsPlacement:`, which AppKit applies to a backing layer's
/// `contentsGravity`.
unsafe extern "C" fn set_layer_contents_placement(receiver: Id, _sel: Sel, placement: isize) {
    let view = view_of(receiver);
    let flags = view.layer_backing & ((1 << PLACEMENT_SHIFT) - 1);
    view.layer_backing = flags | ((placement.clamp(0, 11) as usize) << PLACEMENT_SHIFT);
    if view.layer_backing & BACKING_LAYER != 0 {
        crate::layer::set_contents_gravity_for_placement(view.layer, placement);
    }
}

/// `-[NSView setClipsToBounds:]`.
///
/// Sets whether the view confines its drawing and that of its subviews to its
/// bounds. If the view is layer-backed, setting this sets the backing layer's
/// `masksToBounds`.
unsafe extern "C" fn set_clips_to_bounds(receiver: Id, _sel: Sel, clips: u8) {
    let view = view_of(receiver);
    let was_clipped = view.layer_backing & CLIPS_TO_BOUNDS != 0;
    if clips != 0 {
        view.layer_backing |= CLIPS_TO_BOUNDS;
    } else {
        view.layer_backing &= !CLIPS_TO_BOUNDS;
    }
    let layer = view.layer;
    if !layer.is_null() {
        crate::layer::set_masks_to_bounds_of(layer, clips != 0);
    }
    if was_clipped != (clips != 0) {
        notify_metal_scene(receiver);
    }
}

/// `-[NSView clipsToBounds]`.
unsafe extern "C" fn clips_to_bounds(receiver: Id, _sel: Sel) -> u8 {
    let view = view_of(receiver);
    u8::from(view.layer_backing & CLIPS_TO_BOUNDS != 0)
}

/// Whether `view` is layer-backed: it asked for a layer and has one.
pub(crate) fn is_layer_backed(view: Id) -> bool {
    is_view(view)
        && view_of(view).layer_backing & WANTS_LAYER != 0
        && !view_of(view).layer.is_null()
}

unsafe extern "C" fn layer(receiver: Id, _sel: Sel) -> Id {
    view_of(receiver).layer
}

/// The layer `-setLayer:` gave a view, or null.
pub(crate) fn layer_of(view: Id) -> Id {
    match is_view(view) {
        true => view_of(view).layer,
        false => std::ptr::null_mut(),
    }
}

/// Whether `-setHidden:` hid this view itself, ancestors aside.
pub(crate) fn is_hidden_itself(view: Id) -> bool {
    is_view(view) && view_of(view).hidden != 0
}

/// `-alphaValue` of `view` alone, ancestors aside.
pub(crate) fn alpha_of(view: Id) -> f64 {
    match is_view(view) {
        true => 1.0 - f64::from_bits(view_of(view).transparency),
        false => 1.0,
    }
}

/// The opacity `view` is composited with: its own `-alphaValue` times every
/// ancestor's, since AppKit fades a view's whole subtree with it.
pub(crate) fn effective_alpha(view: Id) -> f64 {
    let mut alpha = 1.0;
    let mut current = view;
    for _ in 0..64 {
        if !is_view(current) {
            break;
        }
        alpha *= alpha_of(current);
        current = view_of(current).superview;
    }
    alpha
}

unsafe extern "C" fn set_alpha_value(receiver: Id, _sel: Sel, alpha: CGFloat) {
    // NaN clamps to opaque rather than poisoning every blend below it.
    let alpha = match alpha.is_nan() {
        true => 1.0,
        false => alpha.clamp(0.0, 1.0),
    };
    let before = alpha_of(receiver);
    view_of(receiver).transparency = (1.0 - alpha).to_bits();
    if before != alpha {
        crate::backing::needs_display(receiver);
        notify_metal_scene(receiver);
    }
}

unsafe extern "C" fn alpha_value(receiver: Id, _sel: Sel) -> CGFloat {
    alpha_of(receiver)
}

unsafe extern "C" fn set_allowed_touch_types(receiver: Id, _sel: Sel, mask: usize) {
    view_of(receiver).allowed_touch_types = mask;
}

unsafe extern "C" fn allowed_touch_types(receiver: Id, _sel: Sel) -> usize {
    view_of(receiver).allowed_touch_types
}

pub const INSTANCE_SIZE: usize = size_of::<View>();

fn view_of<'a>(receiver: Id) -> &'a mut View {
    // SAFETY: a view of this crate's, which is what every method here is sent to.
    unsafe { &mut *(receiver as *mut View) }
}

pub(crate) fn is_view(object: Id) -> bool {
    // SAFETY: null or an object this runtime allocated, which is what every
    // caller has.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSView").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

/// Tell a view which window it is in, from `-[NSWindow setContentView:]`.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn set_window(view: Id, window: Id) {
    if !is_view(view) {
        return;
    }
    let previous = unsafe { window_of(view) };
    view_of(view).window = window;
    if !previous.is_null() {
        invalidate_metal_scene_for_window(unsafe { crate::window::sdl_window(previous) });
    }
    notify_metal_scene(view);
}

/// Forget `window` as the window `view` is the content view of, if it still is.
///
/// # Safety
///
/// As [`set_window`].
pub unsafe fn detach_window(view: Id, window: Id) {
    if is_view(view) && view_of(view).window == window {
        let sdl = unsafe { crate::window::sdl_window(window) };
        view_of(view).window = std::ptr::null_mut();
        if !sdl.is_null() {
            invalidate_metal_scene_for_window(sdl);
        }
    }
}

/// The window a view is in, following the superview chain.
///
/// # Safety
///
/// As [`set_window`].
pub unsafe fn window_of(view: Id) -> Id {
    let mut current = view;
    // Bounded rather than a bare loop: a subview list a guest built with a cycle
    // in it would otherwise hang inside a method that looks like an accessor.
    for _ in 0..64 {
        if !is_view(current) {
            return std::ptr::null_mut();
        }
        let v = view_of(current);
        if !v.window.is_null() {
            return v.window;
        }
        if v.superview.is_null() {
            return std::ptr::null_mut();
        }
        current = v.superview;
    }
    std::ptr::null_mut()
}

/// One line per view, describing the tree under `view`.
///
/// The question this exists for is "what size does the *guest's* view think it
/// is", and it cannot be answered any other way: a release build inlines the
/// whole geometry path out of gdb's reach - `appkit::event::post` and
/// `foundation::notification::post` both resolve to "Function not defined" -
/// and the view that matters is one the guest made, so it has no symbol either.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn describe(view: Id, depth: usize, out: &mut String) {
    use std::fmt::Write;

    if !is_view(view) || depth > 16 {
        return;
    }
    let v = view_of(view);
    // The class is what names it: `NSView` is this crate's, and anything else is
    // the guest's own subclass, which is the one the question is about.
    // SAFETY: a live object.
    let class = unsafe { objc::object::class_of(view) }.map_or("?", objc::class::Class::name_str);
    let _ = writeln!(
        out,
        "rosemary: appkit: {:indent$}{class} {view:p} frame {}x{} at ({},{}) mask {:#x}{}",
        "",
        v.frame.size.width,
        v.frame.size.height,
        v.frame.origin.x,
        v.frame.origin.y,
        v.autoresizing_mask,
        match v.does_not_autoresize_subviews {
            0 => "",
            _ => " autoresizesSubviews=NO",
        },
        indent = depth * 2,
    );
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let children = unsafe { corefoundation::collections::array(v.subviews as *const c_void) };
    for child in children.map(|a| a.values.clone()).unwrap_or_default() {
        // SAFETY: an object this runtime allocated.
        unsafe { describe(child as Id, depth + 1, out) };
    }
}

/// The deepest view under a point, as `-[NSView hitTest:]` finds it.
///
/// `point` is in the *superview's* coordinates, which is the argument AppKit's
/// takes. Subviews are tried last-first, because the last one added is the one
/// drawn on top.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn hit_test(view: Id, point: NSPoint) -> Id {
    unsafe { hit(view, point, 0) }
}

/// Where the pointer is in this view's own coordinates, or `None` for a view
/// in no window - which is every view before it is installed, and the point
/// where a cursor rectangle cannot be about the pointer at all.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn pointer_in(view: Id) -> Option<NSPoint> {
    if !is_view(view) {
        return None;
    }
    let window = unsafe { window_of(view) };
    if window.is_null() {
        return None;
    }
    Some(point_from_window(
        view,
        crate::window::pointer_location(window),
    ))
}

/// `-isFlipped`, sent so that a subclass's override answers: Qt's views are
/// all flipped, and nothing about one says so except that method.
pub fn is_flipped(view: Id) -> bool {
    if !is_view(view) {
        return false;
    }
    // SAFETY: a live view; the selector is `BOOL (id, SEL)`.
    unsafe {
        let send: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        send(view, objc::sel::get("isFlipped")) != 0
    }
}

/// The vertical coordinate `y` of a view's own space as its superview's.
///
/// Both ends can be flipped: `flipped` is the view's, `outer_flipped` the
/// superview's, and a view in no superview is in its window, which is not.
fn y_to_superview(frame: NSRect, flipped: bool, outer_flipped: bool, y: f64) -> f64 {
    let from_top = match flipped {
        true => y,
        false => frame.size.height - y,
    };
    match outer_flipped {
        true => frame.origin.y + from_top,
        false => frame.origin.y + frame.size.height - from_top,
    }
}

/// The inverse of [`y_to_superview`].
fn y_from_superview(frame: NSRect, flipped: bool, outer_flipped: bool, y: f64) -> f64 {
    let from_top = match outer_flipped {
        true => y - frame.origin.y,
        false => frame.origin.y + frame.size.height - y,
    };
    match flipped {
        true => from_top,
        false => frame.size.height - from_top,
    }
}

/// A point in `view`'s coordinates, in its superview's.
fn to_superview(view: Id, point: NSPoint) -> NSPoint {
    let v = view_of(view);
    let local = NSPoint {
        x: point.x - v.bounds_origin.x,
        y: point.y - v.bounds_origin.y,
    };
    NSPoint {
        x: local.x + v.frame.origin.x,
        y: y_to_superview(v.frame, is_flipped(view), is_flipped(v.superview), local.y),
    }
}

/// A point in `view`'s superview's coordinates, in the view's own.
fn from_superview(view: Id, point: NSPoint) -> NSPoint {
    let v = view_of(view);
    NSPoint {
        x: point.x - v.frame.origin.x + v.bounds_origin.x,
        y: y_from_superview(v.frame, is_flipped(view), is_flipped(v.superview), point.y)
            + v.bounds_origin.y,
    }
}

/// A point in `view`'s coordinates, in its window's base coordinates.
///
/// Every `-isFlipped` on the way up applies, which is what puts a flipped
/// superview's subview at `y = 0` along its top edge. Null is the window.
pub fn point_to_window(view: Id, point: NSPoint) -> NSPoint {
    let mut point = point;
    let mut node = view;
    // Bounded for the reason [`window_of`] is.
    for _ in 0..64 {
        if !is_view(node) {
            break;
        }
        point = to_superview(node, point);
        node = view_of(node).superview;
    }
    point
}

/// The inverse of [`point_to_window`].
pub fn point_from_window(view: Id, point: NSPoint) -> NSPoint {
    let mut chain = Vec::new();
    let mut node = view;
    for _ in 0..64 {
        if !is_view(node) {
            break;
        }
        chain.push(node);
        node = view_of(node).superview;
    }
    chain
        .into_iter()
        .rev()
        .fold(point, |point, node| from_superview(node, point))
}

/// A rectangle from one coordinate space to another by its corners, which
/// swap vertically when exactly one of the two spaces is flipped.
fn rect_between(rect: NSRect, map: impl Fn(NSPoint) -> NSPoint) -> NSRect {
    let a = map(rect.origin);
    let b = map(NSPoint {
        x: rect.origin.x + rect.size.width,
        y: rect.origin.y + rect.size.height,
    });
    NSRect {
        origin: NSPoint {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
        },
        size: NSSize {
            width: (b.x - a.x).abs(),
            height: (b.y - a.y).abs(),
        },
    }
}

/// A view's bounds in its window's base coordinates.
pub fn rect_in_window(view: Id) -> NSRect {
    let bounds = match is_view(view) {
        true => NSRect {
            origin: view_of(view).bounds_origin,
            size: view_of(view).frame.size,
        },
        false => NSRect::default(),
    };
    rect_between(bounds, |point| point_to_window(view, point))
}

/// Where a view's subviews' frames are measured from: its bounds origin, which
/// a clip view moves to scroll.
pub(crate) fn bounds_origin_of(view: Id) -> NSPoint {
    match is_view(view) {
        true => view_of(view).bounds_origin,
        false => NSPoint::default(),
    }
}

/// Whether a rectangle of a view's holds a point, for [`crate::cursor`].
pub fn holds(view: Id, rect: NSRect, point: NSPoint) -> bool {
    is_view(view) && crate::geometry::mouse_in_rect(point, rect, is_flipped(view))
}

/// Where a view's layer lands in its window, in pixels from the top left.
///
/// This is the whole of the mapping from AppKit's view tree to a window a
/// compositor knows about. AppKit gives every view a frame in its
/// superview's coordinates, with the origin at the bottom left; a window
/// system wants one rectangle in the window's own pixels, counted from the
/// top left. So the origins are summed up the superview chain, the result is
/// flipped against the window's height, and both are scaled by the window's
/// pixel density - which is what separates a view's size in points from the
/// number of pixels a drawable for it has to cover.
///
/// `None` where the view is not in a window this runtime made, which is the
/// only answer presentation can act on: there is nothing to present to.
///
/// # Safety
///
/// `view` is null or a view of this crate's.
pub unsafe fn presentation_frame(view: Id) -> Option<Frame> {
    if !is_view(view) {
        return None;
    }
    // SAFETY: the caller's obligation, checked above.
    let window = unsafe { window_of(view) };
    // SAFETY: null or a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    if sdl.is_null() {
        return None;
    }
    let rect = rect_in_window(view);
    let mapped = crate::window::rect_to_surface(window, rect)?;
    // The SDL video thread publishes these dimensions. No native SDL
    // query is permitted from a renderer resolving a Metal view frame.
    let snapshot = sdl3::video::presentation_window(sdl)?;
    let host_points = snapshot.logical_size;
    let host_pixels = snapshot.pixel_size;
    let mut rounded = crate::window::pixel_rect_from_surface(mapped, host_points, host_pixels);
    // A view's source extent uses exactly the same separately rounded pixel
    // edges as the destination clip; the *unclipped* rectangle is needed for
    // Metal's destination-to-source mapping when views overhang the window.
    if rounded.w == 0 || rounded.h == 0 {
        let surface = crate::window::surface_frame(window)?;
        let inset = crate::window::content_inset(window).min(surface.size.height);
        rounded = crate::window::pixel_rect_from_surface(
            NSRect {
                origin: NSPoint {
                    x: surface.origin.x,
                    y: surface.origin.y + inset,
                },
                size: NSSize {
                    width: surface.size.width,
                    height: (surface.size.height - inset).max(0.0),
                },
            },
            host_points,
            host_pixels,
        );
    }
    let frame = Frame {
        window: sdl,
        x: rounded.x,
        y: rounded.y,
        width: rounded.w as u32,
        height: rounded.h as u32,
        window_width: host_pixels.0.max(0) as u32,
        window_height: host_pixels.1.max(0) as u32,
    };
    Some(frame)
}

/// The view's rectangle in SDL's logical window coordinates.
///
/// This is deliberately not [`presentation_frame`]: SDL text-input areas are
/// measured in window coordinates, while presentation uses drawable pixels.
/// AppKit and SDL also put their origins at opposite vertical edges.
///
/// # Safety
///
/// `view` is null or a view of this crate's.
pub unsafe fn input_frame(view: Id) -> Option<(sdl3::Window, sdl3::Rect)> {
    if !is_view(view) {
        return None;
    }
    // SAFETY: the caller's obligation, checked above.
    let window = unsafe { window_of(view) };
    // SAFETY: null or a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    if sdl.is_null() {
        return None;
    }
    let rect = rect_in_window(view);
    let mapped = crate::window::rect_to_surface(window, rect)?;
    let points = sdl3::video::window_size(sdl)?;
    // SDL text-input areas use window points rather than physical pixels,
    // but their half-open edges must follow the same rounding convention.
    Some((
        sdl,
        crate::window::pixel_rect_from_surface(mapped, points, points),
    ))
}

/// A view's layer as a window system sees it: a rectangle of pixels, and the
/// window it is a rectangle of.
#[derive(Clone, Copy, PartialEq)]
pub struct Frame {
    pub window: sdl3::Window,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub window_width: u32,
    pub window_height: u32,
}

impl Frame {
    /// Whether this frame is the whole window, so that whatever is drawn for
    /// it needs nothing composited under or around it.
    pub fn covers_window(&self) -> bool {
        self.x <= 0
            && self.y <= 0
            && self.width >= self.window_width
            && self.height >= self.window_height
    }
}

/// The subviews of a view, for a caller walking the tree outside this module.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn children(view: Id) -> Vec<Id> {
    if !is_view(view) {
        return Vec::new();
    }
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let list =
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) };
    list.map(|a| a.values.iter().map(|v| *v as Id).collect())
        .unwrap_or_default()
}

unsafe fn hit(view: Id, point: NSPoint, depth: usize) -> Id {
    if !is_view(view) || depth > 32 {
        return std::ptr::null_mut();
    }
    // A guest's own `-hitTest:` is the answer where it has one: a view that
    // reshapes its hit region - a round button, a transparent overlay - is
    // saying so in code this crate cannot see from the frame.
    let sel = objc::sel::get("hitTest:");
    // SAFETY: a live object this runtime allocated.
    if let Some(method) = unsafe { objc::object::class_of(view) }
        .and_then(|c| c.method(sel))
        .filter(|method| method.implementation() != hit_test_method as *const c_void)
    {
        // SAFETY: `-hitTest:` is `id (id, SEL, NSPoint)`.
        return unsafe {
            let f: unsafe extern "C" fn(Id, Sel, NSPoint) -> Id =
                std::mem::transmute(method.implementation());
            f(view, sel, point)
        };
    }
    // SAFETY: validated above.
    unsafe { hit_base(view, point, depth) }
}

/// The inherited `NSView` hit-test behavior, without redispatching an override
/// on `view`. This distinction matters when that override calls `super`.
unsafe fn hit_base(view: Id, point: NSPoint, depth: usize) -> Id {
    let frame = view_of(view).frame;
    let superview = view_of(view).superview;
    if !crate::geometry::mouse_in_rect(point, frame, is_flipped(superview)) {
        return std::ptr::null_mut();
    }
    let local = from_superview(view, point);
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let children =
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) };
    for child in children
        .map(|a| a.values.clone())
        .unwrap_or_default()
        .into_iter()
        .rev()
    {
        // SAFETY: an object this runtime allocated.
        let hit = unsafe { hit(child as Id, local, depth + 1) };
        if !hit.is_null() {
            return hit;
        }
    }
    view
}

/// `-[NSView hitTest:]`; subclasses with their own method are dispatched by
/// [`hit`] before this base implementation is considered.
unsafe extern "C" fn hit_test_method(receiver: Id, _sel: Sel, point: NSPoint) -> Id {
    // SAFETY: the receiver is a live view and the point is in its superview's
    // coordinates, as expected by this module's tree walk.
    unsafe { hit_base(receiver, point, 0) }
}

/// Send `-updateTrackingAreas` down a view tree.
///
/// AppKit sends it when a view is added to a window and whenever the geometry
/// changes, and it is the *only* time a program is asked to install its
/// tracking areas: a view's `-updateTrackingAreas` is where an application
/// removes the old area and adds one for its new bounds. A runtime that never
/// sends it has a program that never adds one - so `-mouseMoved:`,
/// `-mouseEntered:` and `-mouseExited:` are all events nothing ever asked for,
/// and the pointer appears to a game to be standing still.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn update_tracking_areas(view: Id) {
    if !is_view(view) {
        return;
    }
    let sel = objc::sel::get("updateTrackingAreas");
    // SAFETY: a live object this runtime allocated.
    if let Some(method) = unsafe { objc::object::class_of(view) }.and_then(|c| c.method(sel)) {
        // SAFETY: `-updateTrackingAreas` is `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(method.implementation());
            f(view, sel);
        }
    }
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let children =
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) };
    for child in children.map(|a| a.values.clone()).unwrap_or_default() {
        // SAFETY: an object this runtime allocated.
        unsafe { update_tracking_areas(child as Id) };
    }
}

/// Whether any view in this tree has a tracking area.
///
/// Not *which* area and not its rectangle: the question a caller has is whether
/// the program asked to be told about the pointer at all, and a view that added
/// one covering part of itself still wants `-mouseMoved:` while the pointer is
/// in it. Enter and exit are per-area, and [`crate::cursor::update_tracking`]
/// delivers those.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn any_tracking_area(view: Id) -> bool {
    if !is_view(view) {
        return false;
    }
    let v = view_of(view);
    // SAFETY: the list is an array of this runtime's.
    let areas = unsafe { corefoundation::collections::array(v.tracking_areas as *const c_void) };
    if areas.is_some_and(|a| !a.values.is_empty()) {
        return true;
    }
    // SAFETY: the subview list is an array of this runtime's, holding views.
    let children = unsafe { corefoundation::collections::array(v.subviews as *const c_void) };
    children
        .map(|a| a.values.clone())
        .unwrap_or_default()
        .into_iter()
        // SAFETY: an object this runtime allocated.
        .any(|child| unsafe { any_tracking_area(child as Id) })
}

/// The tracking areas a view holds, in the order they were added.
///
/// # Safety
///
/// `view` must be null or an object this runtime allocated.
pub unsafe fn tracking_areas_of(view: Id) -> Vec<Id> {
    if !is_view(view) {
        return Vec::new();
    }
    // SAFETY: the list is an array of this runtime's.
    let list = unsafe {
        corefoundation::collections::array(view_of(view).tracking_areas as *const c_void)
    };
    list.map(|a| a.values.iter().map(|v| *v as Id).collect())
        .unwrap_or_default()
}

/// The bounds a view reports, for a tracking area made with
/// `NSTrackingInVisibleRect`.
pub fn bounds_of(view: Id) -> NSRect {
    match is_view(view) {
        // SAFETY: a view of this crate's.
        true => unsafe { bounds(view, objc::sel::get("bounds")) },
        false => NSRect::default(),
    }
}

pub(crate) fn subviews_of(view: Id) -> Vec<Id> {
    if !is_view(view) {
        return Vec::new();
    }
    // SAFETY: a view holds an array of retained subviews.
    let list =
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) };
    list.map(|array| array.values.iter().map(|child| *child as Id).collect())
        .unwrap_or_default()
}

/// The view above this one, for walking the responder chain.
///
/// # Safety
///
/// As [`hit_test`].
pub unsafe fn superview_of(view: Id) -> Id {
    match is_view(view) {
        true => view_of(view).superview,
        false => std::ptr::null_mut(),
    }
}

/// The backing scale of a view's window, or one.
///
/// `wantsBestResolutionOpenGLSurface` controls the resolution AppKit gives an
/// OpenGL surface; it does not change the window's backing coordinate system.
/// SDL's OpenGL surface is always the high-density one because the window was
/// created with `HIGH_PIXEL_DENSITY`, so reporting one here for a view that did
/// not set that flag leaves the guest drawing a point-sized viewport into a
/// pixel-sized surface.
fn scale(receiver: Id) -> f64 {
    // SAFETY: a view of this crate's.
    let window = unsafe { window_of(receiver) };
    // SAFETY: null or a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(window) };
    match sdl.is_null() {
        true => 1.0,
        false => sdl3::video::window_pixel_density(sdl) as f64,
    }
}

/// `-init`, which is `-initWithFrame:` with an empty frame: sent, so a
/// subclass's designated initializer runs, as it does under AppKit.
unsafe extern "C" fn init(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            receiver,
            objc::sel::get("initWithFrame:"),
            NSRect::default(),
        )
    }
}

pub(crate) unsafe extern "C" fn init_with_frame(receiver: Id, _sel: Sel, frame: NSRect) -> Id {
    view_of(receiver).frame = frame;
    receiver
}

unsafe extern "C" fn frame(receiver: Id, _sel: Sel) -> NSRect {
    view_of(receiver).frame
}

unsafe extern "C" fn set_frame(receiver: Id, _sel: Sel, frame: NSRect) {
    let before = view_of(receiver).frame;
    view_of(receiver).frame = frame;
    note_geometry_change(receiver, before);
}

unsafe extern "C" fn set_frame_size(receiver: Id, _sel: Sel, size: NSSize) {
    let before = view_of(receiver).frame;
    view_of(receiver).frame.size = size;
    note_geometry_change(receiver, before);
}

unsafe extern "C" fn set_frame_origin(receiver: Id, _sel: Sel, origin: NSPoint) {
    let before = view_of(receiver).frame;
    view_of(receiver).frame.origin = origin;
    note_geometry_change(receiver, before);
}

/// `-bounds`: the frame's size at the origin, which is what a view with no
/// transform of its own has.
unsafe extern "C" fn bounds(receiver: Id, _sel: Sel) -> NSRect {
    NSRect {
        origin: view_of(receiver).bounds_origin,
        size: view_of(receiver).frame.size,
    }
}

/// FIXME (ACCURACY): a bounds size other than the frame's would scale the
/// view's coordinates; here it resizes the frame instead.
unsafe extern "C" fn set_bounds(receiver: Id, sel: Sel, bounds: NSRect) {
    let before = view_of(receiver).frame;
    view_of(receiver).frame.size = bounds.size;
    note_geometry_change(receiver, before);
    // SAFETY: as `-setBoundsOrigin:`.
    unsafe { set_bounds_origin(receiver, sel, bounds.origin) };
}

/// `-setBoundsOrigin:`: the subviews move the other way, and the whole view
/// is to be drawn again.
pub(crate) unsafe extern "C" fn set_bounds_origin(receiver: Id, _sel: Sel, origin: NSPoint) {
    let view = view_of(receiver);
    if view.bounds_origin == origin {
        return;
    }
    view.bounds_origin = origin;
    let layer = view.layer;
    if !layer.is_null() {
        crate::layer::set_bounds_of(layer, bounds_of(receiver));
    }
    crate::backing::needs_display(receiver);
    if view_of(receiver).posts & POSTS_BOUNDS != 0 {
        foundation::notification::post(BOUNDS_DID_CHANGE, receiver, std::ptr::null_mut());
    }
}

/// `-translateOriginToPoint:`.
unsafe extern "C" fn translate_origin(receiver: Id, sel: Sel, by: NSPoint) {
    let origin = view_of(receiver).bounds_origin;
    // SAFETY: as `-setBoundsOrigin:`.
    unsafe {
        set_bounds_origin(
            receiver,
            sel,
            NSPoint {
                x: origin.x - by.x,
                y: origin.y - by.y,
            },
        )
    };
}

/// `-visibleRect`: the part of the view's bounds no ancestor clips away, in
/// the view's own coordinates. Every view clips its subviews to its frame.
unsafe extern "C" fn visible_rect(receiver: Id, _sel: Sel) -> NSRect {
    visible_rect_of(receiver)
}

pub(crate) fn visible_rect_of(view: Id) -> NSRect {
    if !is_view(view) {
        return NSRect::default();
    }
    let mut visible = rect_in_window(view);
    let mut node = view_of(view).superview;
    for _ in 0..64 {
        if !is_view(node) {
            break;
        }
        visible = intersection(visible, rect_in_window(node));
        node = view_of(node).superview;
    }
    if visible.size.width <= 0.0 || visible.size.height <= 0.0 {
        return NSRect {
            origin: view_of(view).bounds_origin,
            size: NSSize::default(),
        };
    }
    rect_between(visible, |point| point_from_window(view, point))
}

fn intersection(a: NSRect, b: NSRect) -> NSRect {
    let left = a.origin.x.max(b.origin.x);
    let bottom = a.origin.y.max(b.origin.y);
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let top = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    NSRect {
        origin: NSPoint { x: left, y: bottom },
        size: NSSize {
            width: (right - left).max(0.0),
            height: (top - bottom).max(0.0),
        },
    }
}

/// `-convertRectToBacking:`, which is the whole reason a Retina program asks a
/// view anything.
unsafe extern "C" fn convert_rect_to_backing(receiver: Id, _sel: Sel, rect: NSRect) -> NSRect {
    let scale = scale(receiver);
    NSRect {
        origin: NSPoint {
            x: rect.origin.x * scale,
            y: rect.origin.y * scale,
        },
        size: NSSize {
            width: rect.size.width * scale,
            height: rect.size.height * scale,
        },
    }
}

unsafe extern "C" fn convert_rect_from_backing(receiver: Id, _sel: Sel, rect: NSRect) -> NSRect {
    let scale = scale(receiver);
    NSRect {
        origin: NSPoint {
            x: rect.origin.x / scale,
            y: rect.origin.y / scale,
        },
        size: NSSize {
            width: rect.size.width / scale,
            height: rect.size.height / scale,
        },
    }
}

unsafe extern "C" fn convert_size_to_backing(receiver: Id, _sel: Sel, size: NSSize) -> NSSize {
    let scale = scale(receiver);
    NSSize {
        width: size.width * scale,
        height: size.height * scale,
    }
}

/// `-mouse:inRect:`, which is `NSMouseInRect` with the view's own flippedness:
/// the edge a point on it counts as inside is the top one either way up.
unsafe extern "C" fn mouse_in_rect(receiver: Id, _sel: Sel, point: NSPoint, rect: NSRect) -> u8 {
    crate::geometry::mouse_in_rect(point, rect, is_flipped(receiver)) as u8
}

/// A point from `from`'s coordinates to `to`'s, through their window; `nil`
/// on either side is the window itself.
fn convert_point(point: NSPoint, from: Id, to: Id) -> NSPoint {
    point_from_window(to, point_to_window(from, point))
}

/// `-convertPoint:fromView:`, which is how Qt turns `-locationInWindow` into
/// a position in its flipped view.
unsafe extern "C" fn convert_point_from_view(
    receiver: Id,
    _sel: Sel,
    point: NSPoint,
    from: Id,
) -> NSPoint {
    convert_point(point, from, receiver)
}

unsafe extern "C" fn convert_point_to_view(
    receiver: Id,
    _sel: Sel,
    point: NSPoint,
    to: Id,
) -> NSPoint {
    convert_point(point, receiver, to)
}

unsafe extern "C" fn convert_rect_from_view(
    receiver: Id,
    _sel: Sel,
    rect: NSRect,
    from: Id,
) -> NSRect {
    convert_rect(rect, from, receiver)
}

/// `-convertRect:toView:`, which Qt uses to place a popup relative to the
/// window before asking the window for screen coordinates.
unsafe extern "C" fn convert_rect_to_view(receiver: Id, _sel: Sel, rect: NSRect, to: Id) -> NSRect {
    convert_rect(rect, receiver, to)
}

/// `-convertSize:toView:` and `-convertSize:fromView:`: a rect at the origin
/// converted, keeping only its extent, which is how scale applies to a size.
unsafe extern "C" fn convert_size_to_view(receiver: Id, _sel: Sel, size: NSSize, to: Id) -> NSSize {
    convert_rect(
        NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        },
        receiver,
        to,
    )
    .size
}

unsafe extern "C" fn convert_size_from_view(
    receiver: Id,
    _sel: Sel,
    size: NSSize,
    from: Id,
) -> NSSize {
    convert_rect(
        NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size,
        },
        from,
        receiver,
    )
    .size
}

/// `(view, next key view)`, unretained as Apple's are. Kept for the keyboard
/// loop a program wires and reads back; nothing here tabs through it yet.
fn key_views() -> &'static std::sync::Mutex<Vec<(usize, usize)>> {
    static KEY_VIEWS: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
    &KEY_VIEWS
}

unsafe extern "C" fn set_next_key_view(receiver: Id, _sel: Sel, next: Id) {
    let mut views = key_views().lock().unwrap_or_else(|e| e.into_inner());
    views.retain(|(view, _)| *view != receiver as usize);
    if !next.is_null() {
        views.push((receiver as usize, next as usize));
    }
}

unsafe extern "C" fn next_key_view(receiver: Id, _sel: Sel) -> Id {
    let views = key_views().lock().unwrap_or_else(|e| e.into_inner());
    views
        .iter()
        .find(|(view, _)| *view == receiver as usize)
        .map_or(std::ptr::null_mut(), |(_, next)| *next as Id)
}

/// `(view, retained tool tip)`. Kept for `-toolTip`; nothing here shows one.
fn tool_tips() -> &'static std::sync::Mutex<Vec<(usize, usize)>> {
    static TIPS: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
    &TIPS
}

unsafe extern "C" fn set_tool_tip(receiver: Id, _sel: Sel, tip: Id) {
    let mut tips = tool_tips().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = tips.iter().position(|(view, _)| *view == receiver as usize) {
        let (_, old) = tips.remove(at);
        objc::arc::release(old as Id);
    }
    if !tip.is_null() {
        tips.push((receiver as usize, objc::arc::retain(tip) as usize));
    }
}

unsafe extern "C" fn tool_tip(receiver: Id, _sel: Sel) -> Id {
    let tips = tool_tips().lock().unwrap_or_else(|e| e.into_inner());
    tips.iter()
        .find(|(view, _)| *view == receiver as usize)
        .map_or(std::ptr::null_mut(), |(_, tip)| *tip as Id)
}

/// `-tag`: a plain view has none, which Apple spells -1. `NSControl`
/// overrides it with a stored one.
unsafe extern "C" fn tag(_receiver: Id, _sel: Sel) -> isize {
    -1
}

/// `-viewWithTag:`: the receiver or the first descendant, depth first, whose
/// `-tag` is `wanted`.
unsafe extern "C" fn view_with_tag(receiver: Id, sel: Sel, wanted: isize) -> Id {
    // SAFETY: `-tag` is `NSInteger (id, SEL)` on every view.
    let own = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> isize = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("tag"))
    };
    if own == wanted {
        return receiver;
    }
    subviews_of(receiver)
        .into_iter()
        // SAFETY: each is a live subview, searched with the same selector.
        .map(|view| unsafe { view_with_tag(view, sel, wanted) })
        .find(|found| !found.is_null())
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn window(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a view of this crate's.
    unsafe { window_of(receiver) }
}

/// `-isInFullScreenMode`: the window's answer, because a view is only ever in
/// full screen by being in a window that is.
///
/// A view with no window is not, which is also what `-window` returning nil
/// means everywhere else here.
unsafe extern "C" fn is_in_full_screen_mode(receiver: Id, _sel: Sel) -> u8 {
    // SAFETY: a view of this crate's.
    let window = unsafe { window_of(receiver) };
    if window.is_null() {
        return 0;
    }
    let sel = objc::sel::get("isInFullScreenMode");
    // SAFETY: `-isInFullScreenMode` is `BOOL (id, SEL)` on a window this crate
    // made.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(window, sel)
    }
}

unsafe extern "C" fn superview(receiver: Id, _sel: Sel) -> Id {
    view_of(receiver).superview
}

unsafe extern "C" fn next_responder(receiver: Id, _sel: Sel) -> Id {
    let view = view_of(receiver);
    match view.next_responder.is_null() {
        true => view.superview,
        false => view.next_responder,
    }
}

unsafe extern "C" fn set_next_responder(receiver: Id, _sel: Sel, responder: Id) {
    view_of(receiver).next_responder = responder;
}

/// `-setSubviews:`: the current subviews removed and these added, in order.
unsafe extern "C" fn set_subviews(receiver: Id, _sel: Sel, views: Id) {
    // SAFETY: a copy of the current list, so removing does not disturb the
    // walk, and the new list, each an `NSArray` of views or nil.
    unsafe {
        let current =
            corefoundation::collections::array(subviews(receiver, objc::sel::get("subviews")) as _)
                .map(|a| a.values.clone())
                .unwrap_or_default();
        let wanted = corefoundation::collections::array(views as _)
            .map(|a| a.values.clone())
            .unwrap_or_default();
        for view in current {
            if !wanted.contains(&view) {
                remove_from_superview(view as Id, objc::sel::get("removeFromSuperview"));
            }
        }
        for view in wanted {
            add_subview(receiver, objc::sel::get("addSubview:"), view as Id);
        }
    }
}

unsafe extern "C" fn subviews(receiver: Id, _sel: Sel) -> Id {
    let view = view_of(receiver);
    if view.subviews.is_null() {
        let array = corefoundation::collections::new_array(
            &[],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            true,
        );
        view.subviews = array as Id;
    }
    view.subviews
}

/// Send `selector`, taking one object, to `view` and every view under it.
fn tell_tree(view: Id, selector: &str, argument: Id) {
    if !is_view(view) {
        return;
    }
    let sel = objc::sel::get(selector);
    // SAFETY: `void (id, SEL, id)`, which the hierarchy hooks are.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(view, sel, argument);
    }
    for child in subviews_of(view) {
        tell_tree(child, selector, argument);
    }
}

/// [`tell_tree`] for a hook that takes nothing.
fn tell_tree_bare(view: Id, selector: &str) {
    if !is_view(view) {
        return;
    }
    let sel = objc::sel::get(selector);
    // SAFETY: `void (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        f(view, sel);
    }
    for child in subviews_of(view) {
        tell_tree_bare(child, selector);
    }
}

fn tell(view: Id, selector: &str, argument: Id) {
    // SAFETY: `void (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(view, objc::sel::get(selector), argument);
    }
}

fn tell_bare(view: Id, selector: &str) {
    // SAFETY: `void (id, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        f(view, objc::sel::get(selector));
    }
}

/// `view` and its subtree are leaving `old` for `new`: the will-move hooks.
pub(crate) fn will_move_to_window(view: Id, old: Id, new: Id) {
    if old != new {
        tell_tree(view, "viewWillMoveToWindow:", new);
    }
}

/// ...and the did-move ones, once the move is made.
pub(crate) fn did_move_to_window(view: Id, old: Id, new: Id) {
    if old != new {
        crate::layout::note_tree_changed();
        tell_tree_bare(view, "viewDidMoveToWindow");
    }
}

/// `-enclosingScrollView`: the nearest scroll view above this one.
unsafe extern "C" fn enclosing_scroll_view(receiver: Id, _sel: Sel) -> Id {
    let Some(scroll) = objc::class::lookup(c"NSScrollView") else {
        return std::ptr::null_mut();
    };
    // SAFETY: a view of this crate's.
    let mut at = unsafe { superview_of(receiver) };
    while is_view(at) {
        // SAFETY: a live view.
        if unsafe { corefoundation::base::is_kind_of(at.cast(), scroll) } {
            return at;
        }
        // SAFETY: as above.
        at = unsafe { superview_of(at) };
    }
    std::ptr::null_mut()
}

/// `-isDescendantOf:`, counting the view itself.
unsafe extern "C" fn is_descendant_of(receiver: Id, _sel: Sel, other: Id) -> u8 {
    let mut at = receiver;
    while is_view(at) {
        if at == other {
            return 1;
        }
        // SAFETY: a view of this crate's.
        at = unsafe { superview_of(at) };
    }
    0
}

unsafe extern "C" fn hook_with_object(_receiver: Id, _sel: Sel, _argument: Id) {}

unsafe extern "C" fn hook(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn add_subview(receiver: Id, sel: Sel, subview: Id) {
    if !is_view(subview) {
        return;
    }
    // A view has one superview: moving it takes it out of the old list, or the
    // old parent keeps a reference and, when it goes, frees what it no longer owns.
    // Held across the move because the old list may own the only reference.
    objc::arc::retain(subview);
    // SAFETY: both are views of this crate's.
    let (old_window, new_window) = unsafe { (window_of(subview), window_of(receiver)) };
    will_move_to_window(subview, old_window, new_window);
    tell(subview, "viewWillMoveToSuperview:", receiver);
    if !view_of(subview).superview.is_null() {
        detach_from_superview(subview);
    }
    view_of(subview).superview = receiver;
    // A window's content view that becomes a subview is in its new parent's
    // window, and a link left here would win over the superview chain.
    view_of(subview).window = std::ptr::null_mut();
    // SAFETY: `-subviews` is `id (id, SEL)` and answers a mutable array.
    let list = unsafe { subviews(receiver, sel) };
    if let Some(array) = unsafe { corefoundation::collections::array(list as *const c_void) } {
        array
            .values
            .push(array.policy.retain(subview as *const c_void));
    }
    tell_bare(subview, "viewDidMoveToSuperview");
    tell(receiver, "didAddSubview:", subview);
    did_move_to_window(subview, old_window, new_window);
    objc::arc::release(subview);
    // SAFETY: a view this crate made, now in a window.
    unsafe { update_tracking_areas(subview) };
    crate::backing::needs_display_in_tree(subview, 0);
    crate::layout::solve();
    notify_metal_scene(receiver);
}

/// `NSWindowOrderingMode`'s `NSWindowBelow`; `NSWindowAbove` is everything else.
const ORDER_BELOW: isize = -1;

/// `-addSubview:positioned:relativeTo:`: `-addSubview:`, then moved to sit just
/// above or below `other`, or above or below every sibling when `other` is not
/// one.
unsafe extern "C" fn add_subview_positioned(
    receiver: Id,
    sel: Sel,
    subview: Id,
    place: isize,
    other: Id,
) {
    // SAFETY: as `-addSubview:`.
    unsafe { add_subview(receiver, sel, subview) };
    if !is_view(subview) || view_of(subview).superview != receiver {
        return;
    }
    let list = view_of(receiver).subviews;
    // SAFETY: an array this module made.
    let Some(array) = (unsafe { corefoundation::collections::array(list as *const c_void) }) else {
        return;
    };
    let Some(from) = array
        .values
        .iter()
        .rposition(|v| *v == subview as *const c_void)
    else {
        return;
    };
    let entry = array.values.remove(from);
    let anchor = (other != subview)
        .then(|| {
            array
                .values
                .iter()
                .position(|v| *v == other as *const c_void)
        })
        .flatten();
    let at = match (anchor, place == ORDER_BELOW) {
        (Some(i), true) => i,
        (Some(i), false) => i + 1,
        (None, true) => 0,
        (None, false) => array.values.len(),
    };
    array.values.insert(at, entry);
    notify_metal_scene(receiver);
}

unsafe extern "C" fn set_translates_autoresizing_mask(receiver: Id, _sel: Sel, flag: u8) {
    view_of(receiver).constrained = usize::from(flag == 0);
    crate::layout::solve();
}

unsafe extern "C" fn translates_autoresizing_mask(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).constrained == 0)
}

/// Whether constraints, rather than the frame a program sets, place `view`.
pub(crate) fn is_constrained(view: Id) -> bool {
    is_view(view) && view_of(view).constrained != 0
}

pub(crate) fn frame_of(view: Id) -> NSRect {
    view_of(view).frame
}

/// A rectangle in `from`'s coordinates, in `to`'s.
pub(crate) fn convert_rect(rect: NSRect, from: Id, to: Id) -> NSRect {
    rect_between(rect, |point| convert_point(point, from, to))
}

/// `-removeFromSuperview`, with the hooks a view is told of around it.
unsafe extern "C" fn remove_from_superview(receiver: Id, _sel: Sel) {
    let parent = view_of(receiver).superview;
    if !is_view(parent) {
        view_of(receiver).superview = std::ptr::null_mut();
        return;
    }
    // Held: the superview's list may own the only reference.
    objc::arc::retain(receiver);
    // SAFETY: a view of this crate's.
    let old_window = unsafe { window_of(receiver) };
    tell(parent, "willRemoveSubview:", receiver);
    will_move_to_window(receiver, old_window, std::ptr::null_mut());
    tell(receiver, "viewWillMoveToSuperview:", std::ptr::null_mut());
    detach_from_superview(receiver);
    tell_bare(receiver, "viewDidMoveToSuperview");
    did_move_to_window(receiver, old_window, std::ptr::null_mut());
    objc::arc::release(receiver);
}

/// Take `receiver` out of its superview's list, telling no one.
fn detach_from_superview(receiver: Id) {
    let view = view_of(receiver);
    // Notify after detachment but before potentially releasing the last
    // reference, using the old parent's window as the notification identity.
    let parent = view.superview;
    view.superview = std::ptr::null_mut();
    if !is_view(parent) {
        return;
    }
    let list = view_of(parent).subviews;
    if list.is_null() {
        notify_metal_scene(parent);
        return;
    }
    // SAFETY: an array this module made.
    if let Some(array) = unsafe { corefoundation::collections::array(list as *const c_void) } {
        let before = array.values.len();
        array.values.retain(|v| *v != receiver as *const c_void);
        // The list retained the view once per `-addSubview:`, which now moves
        // out of a single entry at most; last, as it may free the receiver.
        for _ in array.values.len()..before {
            array.policy.release(receiver as *const c_void);
        }
    }
    // The old parent's child array must no longer contain the removed view
    // when the compositor snapshots its visible membership.
    notify_metal_scene(parent);
}

/// `-dealloc`. Subviews are told their superview is gone before the list that
/// retains them is released: a subview something else still holds would
/// otherwise walk its ancestors into freed memory.
unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let view = view_of(receiver);
    objc::arc::release(std::mem::replace(
        &mut view.appearance,
        std::ptr::null_mut(),
    ));
    let subviews = std::mem::replace(&mut view.subviews, std::ptr::null_mut());
    // SAFETY: an array this module made, or null.
    if let Some(array) = unsafe { corefoundation::collections::array(subviews as *const c_void) } {
        for child in &array.values {
            let child = *child as Id;
            if is_view(child) && view_of(child).superview == receiver {
                view_of(child).superview = std::ptr::null_mut();
            }
        }
    }
    objc::arc::release(subviews);
    crate::layout::forget(receiver);
    crate::backing::forget(receiver);
    let tracking_areas = std::mem::replace(&mut view.tracking_areas, std::ptr::null_mut());
    objc::arc::release(tracking_areas);
    let layer = std::mem::replace(&mut view.layer, std::ptr::null_mut());
    // SAFETY: the layer this view set, which must not point back at it.
    unsafe { crate::layer::set_view(layer, std::ptr::null_mut()) };
    objc::arc::release(layer);
    crate::cursor::discard_rects(receiver);
    objc::arc::finalize(receiver);
}

/// `-addTrackingArea:` and its two companions.
///
/// The area is kept in the subview-shaped list a view already has for it, so
/// `-removeTrackingArea:` can find the object it was given back. Delivery is
/// [`crate::cursor::update_tracking`]'s, which walks these lists on every move.
unsafe extern "C" fn add_tracking_area(receiver: Id, _sel: Sel, area: Id) {
    let view = view_of(receiver);
    if view.tracking_areas.is_null() {
        let array = corefoundation::collections::new_array(
            &[],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            true,
        );
        view.tracking_areas = array as Id;
    }
    // SAFETY: an array this module made.
    if let Some(array) =
        unsafe { corefoundation::collections::array(view.tracking_areas as *const c_void) }
    {
        array
            .values
            .push(array.policy.retain(area as *const c_void));
    }
}

unsafe extern "C" fn remove_tracking_area(receiver: Id, _sel: Sel, area: Id) {
    let list = view_of(receiver).tracking_areas;
    if list.is_null() {
        return;
    }
    // SAFETY: an array this module made.
    if let Some(array) = unsafe { corefoundation::collections::array(list as *const c_void) } {
        array.values.retain(|v| *v != area as *const c_void);
    }
}

/// `-addTrackingRect:owner:userData:assumeInside:`, the API tracking areas
/// replaced: an always-active enter-and-exit area, whose tag is the area.
///
/// `userData` is not carried to the events: an area's `userInfo` is a
/// dictionary, and the events built from it answer that.
unsafe extern "C" fn add_tracking_rect(
    receiver: Id,
    sel: Sel,
    rect: NSRect,
    owner: Id,
    _data: *mut c_void,
    inside: u8,
) -> isize {
    use crate::cursor::tracking;
    let Some(class) = objc::class::lookup(c"NSTrackingArea") else {
        return 0;
    };
    let mut options = tracking::MOUSE_ENTERED_AND_EXITED | tracking::ACTIVE_ALWAYS;
    if inside != 0 {
        options |= tracking::ASSUME_INSIDE;
    }
    let area = objc::object::create_instance(class, 0);
    // SAFETY: `-initWithRect:options:owner:userInfo:` is
    // `id (id, SEL, NSRect, NSUInteger, id, id)`.
    let area = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect, usize, Id, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            area,
            objc::sel::get("initWithRect:options:owner:userInfo:"),
            rect,
            options,
            owner,
            std::ptr::null_mut(),
        )
    };
    if area.is_null() {
        return 0;
    }
    // SAFETY: the list retains the area, so this reference is given up.
    unsafe { add_tracking_area(receiver, sel, area) };
    objc::arc::release(area);
    area as isize
}

unsafe extern "C" fn remove_tracking_rect(receiver: Id, sel: Sel, tag: isize) {
    // SAFETY: a tag is the area `-addTrackingRect:...` added, compared by
    // address only.
    unsafe { remove_tracking_area(receiver, sel, tag as Id) };
}

/// `-animator`: the view itself, so that what a program animates is set at
/// once, which is where the animation would have ended.
unsafe extern "C" fn animator(receiver: Id, _sel: Sel) -> Id {
    receiver
}

unsafe extern "C" fn tracking_areas(receiver: Id, sel: Sel) -> Id {
    let view = view_of(receiver);
    if view.tracking_areas.is_null() {
        // SAFETY: `-addTrackingArea:` is `void (id, SEL, id)`, and adding nil
        // is what makes the empty list without adding anything to it.
        unsafe { add_tracking_area(receiver, sel, std::ptr::null_mut()) };
        unsafe { remove_tracking_area(receiver, sel, std::ptr::null_mut()) };
    }
    view.tracking_areas
}

unsafe extern "C" fn set_wants_best_resolution(receiver: Id, _sel: Sel, wants: u8) {
    view_of(receiver).wants_best_resolution = wants as usize;
}

unsafe extern "C" fn wants_best_resolution(receiver: Id, _sel: Sel) -> u8 {
    (view_of(receiver).wants_best_resolution != 0) as u8
}

/// Answered as true, because a view here is always ready to be drawn into.
unsafe extern "C" fn yes(_receiver: Id, _sel: Sel) -> u8 {
    1
}

unsafe extern "C" fn no(_receiver: Id, _sel: Sel) -> u8 {
    0
}

/// Whether `imp` is NSView's own `-acceptsFirstResponder`. It answers yes for
/// every view, so it says nothing about whether a click should focus one.
pub(crate) fn is_default_accepts_first_responder(imp: objc::class::Imp) -> bool {
    imp == yes as objc::class::Imp
}

/// `-display`: draw the view now.
///
/// **This is what calls `-drawRect:`,** and a no-op here is a program that
/// renders nothing while looking like it is running. Deltarune's frame is
/// exactly this path: its display link sends `-[YYGLView display]`, AppKit turns
/// that into `-drawRect:`, and the guest's override makes its GL context current
/// and swaps. With `-display` empty the game ticks, the display link fires, and
/// the window stays black.
///
/// Sent rather than called, so the *guest's* override is what runs, with the
/// view's focus locked so that what it draws lands in the window's store. A
/// view with no `-drawRect:` of its own draws nothing, which is what an empty
/// view does.
unsafe extern "C" fn display(receiver: Id, _sel: Sel) {
    crate::backing::display_view(receiver);
}

unsafe extern "C" fn draw_rect(_receiver: Id, _sel: Sel, _rect: NSRect) {}

unsafe extern "C" fn set_needs_display(receiver: Id, _sel: Sel, flag: u8) {
    if flag != 0 {
        crate::backing::needs_display(receiver);
    }
}

/// `-setNeedsDisplay` with no argument: not in AppKit's headers, but programs
/// written against `UIView` habits send it and a Mac's AppKit answers it.
unsafe extern "C" fn mark_needs_display(receiver: Id, _sel: Sel) {
    crate::backing::needs_display(receiver);
}

unsafe extern "C" fn set_needs_display_in_rect(receiver: Id, _sel: Sel, rect: NSRect) {
    crate::backing::needs_display_in_rect(receiver, rect);
}

unsafe extern "C" fn set_hidden(receiver: Id, _sel: Sel, hidden: u8) {
    let was = view_of(receiver).hidden != 0;
    view_of(receiver).hidden = usize::from(hidden != 0);
    if was && hidden == 0 {
        crate::backing::needs_display(receiver);
    }
    if was != (hidden != 0) {
        notify_metal_scene(receiver);
    }
}

unsafe extern "C" fn is_hidden(receiver: Id, _sel: Sel) -> u8 {
    u8::from(view_of(receiver).hidden != 0)
}

unsafe extern "C" fn is_hidden_or_has_hidden_ancestor_method(receiver: Id, _sel: Sel) -> u8 {
    u8::from(is_hidden_or_has_hidden_ancestor(receiver))
}

/// Whether `view` or any view above it is hidden, which is what decides
/// whether it is drawn or presented at all.
pub fn is_hidden_or_has_hidden_ancestor(view: Id) -> bool {
    let mut current = view;
    for _ in 0..64 {
        if !is_view(current) {
            return false;
        }
        let node = view_of(current);
        if node.hidden != 0 {
            return true;
        }
        current = node.superview;
    }
    false
}

unsafe extern "C" fn ignored(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn ignored_object(_receiver: Id, _sel: Sel, _object: Id) {}

unsafe extern "C" fn ignored_flag(_receiver: Id, _sel: Sel, _flag: u8) {}

unsafe extern "C" fn ignored_integer(_receiver: Id, _sel: Sel, _value: isize) {}

/// `-addCursorRect:cursor:`, which is only ever called from inside a view's own
/// `-resetCursorRects`: the rectangle it records lasts until the next
/// `-discardCursorRects`, and [`crate::cursor`] decides from the whole set
/// which cursor the pointer is over.
unsafe extern "C" fn add_cursor_rect(receiver: Id, _sel: Sel, rect: NSRect, cursor: Id) {
    crate::cursor::add_rect(receiver, rect, cursor);
}

unsafe extern "C" fn discard_cursor_rects(receiver: Id, _sel: Sel) {
    crate::cursor::discard_rects(receiver);
}

unsafe extern "C" fn backing_scale_factor(receiver: Id, _sel: Sel) -> CGFloat {
    scale(receiver)
}

unsafe extern "C" fn appearance(receiver: Id, _sel: Sel) -> Id {
    unsafe { (*(receiver as *const View)).appearance }
}

unsafe extern "C" fn set_appearance(receiver: Id, _sel: Sel, appearance: Id) {
    let appearance = objc::arc::retain(appearance);
    let old = unsafe { std::mem::replace(&mut (*(receiver as *mut View)).appearance, appearance) };
    objc::arc::release(old);
}

unsafe extern "C" fn effective_appearance(receiver: Id, sel: Sel) -> Id {
    let own = unsafe { appearance(receiver, sel) };
    if !own.is_null() {
        return own;
    }
    let view = view_of(receiver);
    let source = if !view.superview.is_null() {
        view.superview
    } else if !view.window.is_null() {
        view.window
    } else {
        crate::application::shared()
    };
    crate::appearance::effective_of(source)
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSView").expect("declared before its methods");
    methods!(class, {
        "appearance" => appearance, c"@16@0:8";
        "setAppearance:" => set_appearance, c"v24@0:8@16";
        "effectiveAppearance" => effective_appearance, c"@16@0:8";
        "init" => init, c"@16@0:8";
        "initWithFrame:" => init_with_frame, c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "frame" => frame, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "setFrame:" => set_frame, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "setFrameSize:" => set_frame_size, c"v32@0:8{CGSize=dd}16";
        "setFrameOrigin:" => set_frame_origin, c"v32@0:8{CGPoint=dd}16";
        "bounds" => bounds, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "visibleRect" => visible_rect, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "setBounds:" => set_bounds, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "setBoundsOrigin:" => set_bounds_origin, c"v32@0:8{CGPoint=dd}16";
        "translateOriginToPoint:" => translate_origin, c"v32@0:8{CGPoint=dd}16";
        "convertRectToBacking:" => convert_rect_to_backing,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "convertRectFromBacking:" => convert_rect_from_backing,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "convertSizeToBacking:" => convert_size_to_backing, c"{CGSize=dd}32@0:8{CGSize=dd}16";
        "convertPoint:fromView:" => convert_point_from_view, c"{CGPoint=dd}40@0:8{CGPoint=dd}16@32";
        "convertPoint:toView:" => convert_point_to_view, c"{CGPoint=dd}40@0:8{CGPoint=dd}16@32";
        "mouse:inRect:" => mouse_in_rect,
            c"c56@0:8{CGPoint=dd}16{CGRect={CGPoint=dd}{CGSize=dd}}32";
        "convertRect:fromView:" => convert_rect_from_view,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "convertRect:toView:" => convert_rect_to_view,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "convertSize:toView:" => convert_size_to_view, c"{CGSize=dd}40@0:8{CGSize=dd}16@32";
        "convertSize:fromView:" => convert_size_from_view, c"{CGSize=dd}40@0:8{CGSize=dd}16@32";
        "tag" => tag, c"q16@0:8";
        "setNextKeyView:" => set_next_key_view, c"v24@0:8@16";
        "setToolTip:" => set_tool_tip, c"v24@0:8@16";
        "toolTip" => tool_tip, c"@16@0:8";
        "nextKeyView" => next_key_view, c"@16@0:8";
        "viewWithTag:" => view_with_tag, c"@24@0:8q16";
        "window" => window, c"@16@0:8";
        "isInFullScreenMode" => is_in_full_screen_mode, c"c16@0:8";
        "superview" => superview, c"@16@0:8";
        "nextResponder" => next_responder, c"@16@0:8";
        "setNextResponder:" => set_next_responder, c"v24@0:8@16";
        "setSubviews:" => set_subviews, c"v24@0:8@16";
        "subviews" => subviews, c"@16@0:8";
        "hitTest:" => hit_test_method, c"@32@0:8{CGPoint=dd}16";
        "addSubview:" => add_subview, c"v24@0:8@16";
        "addSubview:positioned:relativeTo:" => add_subview_positioned, c"v40@0:8@16q24@32";
        "setTranslatesAutoresizingMaskIntoConstraints:" => set_translates_autoresizing_mask,
            c"v20@0:8c16";
        "translatesAutoresizingMaskIntoConstraints" => translates_autoresizing_mask, c"c16@0:8";
        "enclosingScrollView" => enclosing_scroll_view, c"@16@0:8";
        "isDescendantOf:" => is_descendant_of, c"c24@0:8@16";
        "viewWillMoveToWindow:" => hook_with_object, c"v24@0:8@16";
        "viewDidMoveToWindow" => hook, c"v16@0:8";
        "viewWillMoveToSuperview:" => hook_with_object, c"v24@0:8@16";
        "viewDidMoveToSuperview" => hook, c"v16@0:8";
        "didAddSubview:" => hook_with_object, c"v24@0:8@16";
        "willRemoveSubview:" => hook_with_object, c"v24@0:8@16";
        "removeFromSuperview" => remove_from_superview, c"v16@0:8";
        "dealloc" => dealloc, c"v16@0:8";
        "setWantsBestResolutionOpenGLSurface:" => set_wants_best_resolution, c"v20@0:8c16";
        "wantsBestResolutionOpenGLSurface" => wants_best_resolution, c"c16@0:8";
        "backingScaleFactor" => backing_scale_factor, c"d16@0:8";
        "acceptsFirstResponder" => yes, c"c16@0:8";
        "canBecomeKeyView" => yes, c"c16@0:8";
        "isOpaque" => yes, c"c16@0:8";
        "isFlipped" => no, c"c16@0:8";
        "isHidden" => is_hidden, c"c16@0:8";
        // No live-resize session is ever started here: the window system resizes
        // a window in one step and nothing posts NSWindowWillStartLiveResize.
        "inLiveResize" => no, c"c16@0:8";
        "isHiddenOrHasHiddenAncestor" => is_hidden_or_has_hidden_ancestor_method, c"c16@0:8";
        "display" => display, c"v16@0:8";
        "displayIfNeeded" => display, c"v16@0:8";
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "setNeedsDisplay:" => set_needs_display, c"v20@0:8c16";
        "setNeedsDisplay" => mark_needs_display, c"v16@0:8";
        // There is no trackpad touch stream to opt in to.
        "setAcceptsTouchEvents:" => ignored_flag, c"v20@0:8c16";
        "setHidden:" => set_hidden, c"v20@0:8c16";
        "setAlphaValue:" => set_alpha_value, c"v24@0:8d16";
        "alphaValue" => alpha_value, c"d16@0:8";
        // Drawing focus rings belongs to AppKit's control renderer, which this
        // view does not have; accepting the policy still lets Qt construct it.
        "setFocusRingType:" => ignored_integer, c"v24@0:8Q16";
        // Drag delivery is not wired to SDL yet, but registration itself has
        // no observable result other than which future drag messages arrive.
        "registerForDraggedTypes:" => ignored_object, c"v24@0:8@16";
        "unregisterDraggedTypes" => ignored, c"v16@0:8";
        "setAutoresizingMask:" => set_autoresizing_mask, c"v24@0:8Q16";
        "autoresizingMask" => autoresizing_mask, c"Q16@0:8";
        "setAutoresizesSubviews:" => set_autoresizes_subviews, c"v20@0:8c16";
        "autoresizesSubviews" => autoresizes_subviews, c"c16@0:8";
        "resizeSubviewsWithOldSize:" => resize_subviews_with_old_size,
            c"v32@0:8{CGSize=dd}16";
        "resizeWithOldSuperviewSize:" => resize_with_old_superview_size,
            c"v32@0:8{CGSize=dd}16";
        "setNeedsDisplayInRect:" => set_needs_display_in_rect,
            c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "addCursorRect:cursor:" => add_cursor_rect,
            c"v56@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48";
        "discardCursorRects" => discard_cursor_rects, c"v16@0:8";
        // AppKit's own is empty; subclasses override it and may call super.
        "resetCursorRects" => ignored, c"v16@0:8";
        "updateTrackingAreas" => ignored, c"v16@0:8";
        "addTrackingArea:" => add_tracking_area, c"v24@0:8@16";
        "removeTrackingArea:" => remove_tracking_area, c"v24@0:8@16";
        "trackingAreas" => tracking_areas, c"@16@0:8";
        "addTrackingRect:owner:userData:assumeInside:" => add_tracking_rect,
            c"q68@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48^v56c64";
        "removeTrackingRect:" => remove_tracking_rect, c"v24@0:8q16";
        "animator" => animator, c"@16@0:8";
        "setPostsFrameChangedNotifications:" => set_posts_frame_changed, c"v20@0:8c16";
        "setPostsBoundsChangedNotifications:" => set_posts_bounds_changed, c"v20@0:8c16";
        "postsFrameChangedNotifications" => posts_frame_changed, c"c16@0:8";
        "postsBoundsChangedNotifications" => posts_bounds_changed, c"c16@0:8";
        "setLayer:" => set_layer, c"v24@0:8@16";
        "layer" => layer, c"@16@0:8";
        "setWantsLayer:" => set_wants_layer, c"v20@0:8c16";
        // A hint about when the backing store may be reused across a resize.
        // Nothing here keeps one, so every policy draws the same.
        "setLayerContentsRedrawPolicy:" => ignored_integer, c"v24@0:8q16";
        "setLayerContentsPlacement:" => set_layer_contents_placement, c"v24@0:8q16";
        "layerContentsPlacement" => layer_contents_placement, c"q16@0:8";
        "makeBackingLayer" => make_backing_layer, c"@16@0:8";
        // A view draws with `-drawRect:` unless it says otherwise.
        "wantsUpdateLayer" => no, c"c16@0:8";
        "updateLayer" => ignored, c"v16@0:8";
        "wantsLayer" => wants_layer, c"c16@0:8";
        "setAllowedTouchTypes:" => set_allowed_touch_types, c"v24@0:8Q16";
        "allowedTouchTypes" => allowed_touch_types, c"Q16@0:8";
        "setClipsToBounds:" => set_clips_to_bounds, c"v20@0:8c16";
        "clipsToBounds" => clips_to_bounds, c"c16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AppKit unit fixtures have no Vulkan worker. Model a completed GPU
    /// commit explicitly; publication alone must not grant admission.
    fn accept_published_native_for_tests(sdl: sdl3::Window) {
        let admitted = metal_scene(sdl)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.native_pixels.is_some() || p.native_dmabuf.is_some())
            .map(|p| (p.layer, p.native_revision))
            .collect::<Vec<_>>();
        crate::backing::set_native_gpu_admission(sdl, &admitted);
    }

    fn view(frame: NSRect) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSView").expect("NSView");
        let object = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
        unsafe { init_with_frame(object, objc::sel::get("initWithFrame:"), frame) }
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect {
            origin: NSPoint { x, y },
            size: NSSize { width, height },
        }
    }

    /// A superview and one subview of the given mask, wired as `-addSubview:`
    /// wires them.
    fn tree(parent: NSRect, child: NSRect, mask: usize) -> (Id, Id) {
        let superview = view(parent);
        let subview = view(child);
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(superview, objc::sel::get("addSubview:"), subview) };
        view_of(subview).autoresizing_mask = mask;
        (superview, subview)
    }

    /// A view starts opaque, reads back the clamped alpha it was given, and
    /// fades its subviews with it.
    #[test]
    fn alpha_value_is_clamped_and_multiplies_down_the_tree() {
        let (superview, subview) = tree(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 5.0, 5.0), 0);
        let set = objc::sel::get("setAlphaValue:");
        let get = objc::sel::get("alphaValue");
        // SAFETY: `-setAlphaValue:` is `void (id, SEL, CGFloat)` and
        // `-alphaValue` is `CGFloat (id, SEL)`.
        unsafe {
            assert_eq!(alpha_value(subview, get), 1.0);
            set_alpha_value(superview, set, 0.5);
            set_alpha_value(subview, set, 2.0);
            assert_eq!(alpha_value(subview, get), 1.0);
            set_alpha_value(subview, set, 0.5);
        }
        assert_eq!(effective_alpha(subview), 0.25);
        objc::arc::release(superview);
    }

    #[test]
    fn a_freed_superview_leaves_its_retained_subview_detached() {
        let (superview, subview) = tree(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 5.0, 5.0), 0);
        objc::arc::retain(subview);
        objc::arc::release(superview);
        assert!(view_of(subview).superview.is_null());
        assert!(!is_hidden_or_has_hidden_ancestor(subview));
        objc::arc::release(subview);
    }

    /// Qt makes a window's content view a subview elsewhere when the window
    /// becomes a child; the old window must not keep answering `-window`.
    #[test]
    fn a_content_view_added_as_a_subview_leaves_its_window() {
        let parent = view(rect(0.0, 0.0, 10.0, 10.0));
        let content = view(rect(0.0, 0.0, 5.0, 5.0));
        let window = parent;
        // SAFETY: two views this test made; the window pointer is only compared.
        unsafe {
            set_window(content, window);
            add_subview(parent, objc::sel::get("addSubview:"), content);
            assert!(window_of(content).is_null());
        }
    }

    #[test]
    fn detaching_a_window_only_forgets_that_window() {
        let content = view(rect(0.0, 0.0, 5.0, 5.0));
        let (window, other) = (
            view(rect(0.0, 0.0, 1.0, 1.0)),
            view(rect(0.0, 0.0, 1.0, 1.0)),
        );
        // SAFETY: views this test made; the window pointers are only compared.
        unsafe {
            set_window(content, window);
            detach_window(content, other);
            assert_eq!(window_of(content), window);
            detach_window(content, window);
            assert!(window_of(content).is_null());
        }
    }

    #[test]
    fn moving_a_subview_takes_it_out_of_its_old_superview() {
        let (first, subview) = tree(rect(0.0, 0.0, 10.0, 10.0), rect(0.0, 0.0, 5.0, 5.0), 0);
        let second = view(rect(0.0, 0.0, 10.0, 10.0));
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(second, objc::sel::get("addSubview:"), subview) };
        assert_eq!(view_of(subview).superview, second);
        assert!(children(first).is_empty());
        assert_eq!(children(second), vec![subview]);
        assert_eq!(objc::arc::retain_count(subview), 2);
    }

    #[test]
    fn converts_a_rect_between_a_subview_and_its_window() {
        let (superview, subview) = tree(
            rect(10.0, 20.0, 100.0, 100.0),
            rect(5.0, 7.0, 50.0, 50.0),
            0,
        );
        let sel = objc::sel::get("convertRect:toView:");
        // SAFETY: both are views of this module's, and nil is the window.
        unsafe {
            assert_eq!(
                convert_rect_to_view(subview, sel, rect(1.0, 1.0, 4.0, 4.0), std::ptr::null_mut()),
                rect(16.0, 28.0, 4.0, 4.0)
            );
            assert_eq!(
                convert_rect_to_view(subview, sel, rect(1.0, 1.0, 4.0, 4.0), superview),
                rect(6.0, 8.0, 4.0, 4.0)
            );
            assert_eq!(
                convert_rect_from_view(subview, sel, rect(6.0, 8.0, 4.0, 4.0), superview),
                rect(1.0, 1.0, 4.0, 4.0)
            );
        }
    }

    unsafe extern "C" fn flipped_yes(_receiver: Id, _sel: Sel) -> u8 {
        1
    }

    fn flipped_view(frame: NSRect) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(c"RosemaryTestFlippedView").unwrap_or_else(|| {
            let class = corefoundation::base::register(
                c"RosemaryTestFlippedView",
                objc::class::lookup(c"NSView"),
                INSTANCE_SIZE,
                false,
            );
            methods!(class, {
                "isFlipped" => flipped_yes, c"c16@0:8";
            });
            class
        });
        let object = objc::object::create_instance(class, 0);
        // SAFETY: the object has `View` storage.
        unsafe { init_with_frame(object, objc::sel::get("initWithFrame:"), frame) }
    }

    #[test]
    fn the_top_window_edge_hits_the_topmost_view_in_either_orientation() {
        for parent_flipped in [false, true] {
            for child_flipped in [false, true] {
                let make = |flipped, frame| match flipped {
                    true => flipped_view(frame),
                    false => view(frame),
                };
                let content = make(parent_flipped, rect(0.0, 0.0, 200.0, 100.0));
                let upper_y = if parent_flipped { 0.0 } else { 80.0 };
                let lower_y = if parent_flipped { 20.0 } else { 60.0 };
                let upper = make(child_flipped, rect(0.0, upper_y, 200.0, 20.0));
                let lower = make(child_flipped, rect(0.0, lower_y, 200.0, 20.0));
                // SAFETY: live views; hit-test points are in window coordinates.
                unsafe {
                    add_subview(content, objc::sel::get("addSubview:"), upper);
                    add_subview(content, objc::sel::get("addSubview:"), lower);
                    let top = crate::geometry::point_in_window(0.0, 0.0, 100.0);
                    assert_eq!(hit_test(content, top), upper);
                    let shared_edge = crate::geometry::point_in_window(0.0, 20.0, 100.0);
                    assert_eq!(hit_test(content, shared_edge), lower);
                    for (x, y) in [(200.0, 0.0), (0.0, 100.0), (0.0, -0.1)] {
                        let outside = crate::geometry::point_in_window(x, y, 100.0);
                        assert!(hit_test(content, outside).is_null());
                    }
                }
                objc::arc::release(upper);
                objc::arc::release(lower);
                objc::arc::release(content);
            }
        }
    }

    #[test]
    fn tracking_rectangles_include_the_visual_top_edge_in_either_orientation() {
        for flipped in [false, true] {
            let frame = rect(0.0, 0.0, 200.0, 100.0);
            let content = match flipped {
                true => flipped_view(frame),
                false => view(frame),
            };
            for (x, y, expected) in [
                (0.0, 0.0, true),
                (100.0, 0.0, true),
                (200.0, 0.0, false),
                (0.0, 100.0, false),
                (0.0, -0.1, false),
            ] {
                let window_point = crate::geometry::point_in_window(x, y, 100.0);
                let local = point_from_window(content, window_point);
                assert_eq!(holds(content, frame, local), expected);
            }
            objc::arc::release(content);
        }
    }

    /// Qt's views are flipped, and a subview at `y = 0` of one is along its
    /// top edge: in window coordinates, the top of the window.
    #[test]
    fn a_flipped_superview_places_its_subviews_from_the_top() {
        let content = flipped_view(rect(0.0, 0.0, 200.0, 100.0));
        let ribbon = flipped_view(rect(0.0, 0.0, 200.0, 20.0));
        let plain = view(rect(10.0, 30.0, 50.0, 40.0));
        // SAFETY: `-addSubview:` is `void (id, SEL, id)` on views.
        unsafe {
            add_subview(content, objc::sel::get("addSubview:"), ribbon);
            add_subview(content, objc::sel::get("addSubview:"), plain);
        }
        assert_eq!(rect_in_window(ribbon), rect(0.0, 80.0, 200.0, 20.0));
        assert_eq!(rect_in_window(plain), rect(10.0, 30.0, 50.0, 40.0));

        // A window point near the top is near the top of the flipped ribbon,
        // and one inside the unflipped view counts up from its bottom.
        let top = NSPoint { x: 5.0, y: 95.0 };
        assert_eq!(point_from_window(ribbon, top), NSPoint { x: 5.0, y: 5.0 });
        assert_eq!(
            point_from_window(plain, NSPoint { x: 15.0, y: 35.0 }),
            NSPoint { x: 5.0, y: 5.0 }
        );
        assert_eq!(point_to_window(ribbon, NSPoint { x: 5.0, y: 5.0 }), top);
        let sel = objc::sel::get("convertPoint:fromView:");
        // SAFETY: a view of this module's, and nil is the window.
        let local = unsafe { convert_point_from_view(ribbon, sel, top, std::ptr::null_mut()) };
        assert_eq!(local, NSPoint { x: 5.0, y: 5.0 });

        // The hit test takes the point in the content view's superview's
        // coordinates, which for a content view are the window's.
        // SAFETY: live views.
        assert_eq!(unsafe { hit_test(content, top) }, ribbon);
        assert_eq!(
            unsafe { hit_test(content, NSPoint { x: 15.0, y: 35.0 }) },
            plain
        );
    }

    fn children(view: Id) -> Vec<Id> {
        // SAFETY: an array this module made.
        unsafe { corefoundation::collections::array(view_of(view).subviews as *const c_void) }
            .map(|a| a.values.iter().map(|v| *v as Id).collect())
            .unwrap_or_default()
    }

    fn set_frame_of(view: Id, frame: NSRect) {
        // SAFETY: `-setFrame:` is `void (id, SEL, NSRect)`.
        unsafe { set_frame(view, objc::sel::get("setFrame:"), frame) };
    }

    /// The case every windowed program is in: a render view that fills the
    /// content view, and a window the person resizes. Without this the program
    /// asks its own view how big it is and gets the size the window opened at.
    #[test]
    fn a_sizable_subview_follows_its_superview() {
        let (superview, subview) = tree(
            rect(0.0, 0.0, 1000.0, 665.0),
            rect(0.0, 0.0, 1000.0, 665.0),
            autoresizing::WIDTH_SIZABLE | autoresizing::HEIGHT_SIZABLE,
        );
        set_frame_of(superview, rect(0.0, 0.0, 1200.0, 760.0));
        assert_eq!(view_of(subview).frame, rect(0.0, 0.0, 1200.0, 760.0));
    }

    /// `NSViewNotSizable` is a real answer: a fixed-size control in a resizing
    /// window keeps both its size and its place.
    #[test]
    fn a_not_sizable_subview_keeps_its_frame() {
        let (superview, subview) = tree(
            rect(0.0, 0.0, 400.0, 400.0),
            rect(10.0, 10.0, 80.0, 20.0),
            0,
        );
        set_frame_of(superview, rect(0.0, 0.0, 800.0, 400.0));
        assert_eq!(view_of(subview).frame, rect(10.0, 10.0, 80.0, 20.0));
    }

    /// A view that is sizable with a fixed leading margin absorbs the whole
    /// change and stays where it started.
    #[test]
    fn a_fixed_margin_holds_a_sizable_subview_in_place() {
        let (superview, subview) = tree(
            rect(0.0, 0.0, 200.0, 100.0),
            rect(20.0, 0.0, 180.0, 100.0),
            autoresizing::WIDTH_SIZABLE,
        );
        set_frame_of(superview, rect(0.0, 0.0, 300.0, 100.0));
        assert_eq!(view_of(subview).frame, rect(20.0, 0.0, 280.0, 100.0));
    }

    /// A resize reaches all the way down, because a subview that resized has
    /// subviews of its own to tell.
    #[test]
    fn a_resize_reaches_a_grandchild() {
        let both = autoresizing::WIDTH_SIZABLE | autoresizing::HEIGHT_SIZABLE;
        let (superview, subview) = tree(
            rect(0.0, 0.0, 100.0, 100.0),
            rect(0.0, 0.0, 100.0, 100.0),
            both,
        );
        let grandchild = view(rect(0.0, 0.0, 100.0, 100.0));
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(subview, objc::sel::get("addSubview:"), grandchild) };
        view_of(grandchild).autoresizing_mask = both;
        set_frame_of(superview, rect(0.0, 0.0, 200.0, 50.0));
        assert_eq!(view_of(grandchild).frame, rect(0.0, 0.0, 200.0, 50.0));
    }

    /// A superview told not to autoresize its subviews does not.
    #[test]
    fn autoresizes_subviews_no_leaves_the_subviews_alone() {
        let (superview, subview) = tree(
            rect(0.0, 0.0, 100.0, 100.0),
            rect(0.0, 0.0, 100.0, 100.0),
            autoresizing::WIDTH_SIZABLE | autoresizing::HEIGHT_SIZABLE,
        );
        // SAFETY: `-setAutoresizesSubviews:` is `void (id, SEL, BOOL)`.
        unsafe {
            set_autoresizes_subviews(superview, objc::sel::get("setAutoresizesSubviews:"), 0)
        };
        set_frame_of(superview, rect(0.0, 0.0, 200.0, 200.0));
        assert_eq!(view_of(subview).frame, rect(0.0, 0.0, 100.0, 100.0));
    }

    static OVERRIDE_RAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static HIT_OVERRIDE_RUNS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "C" fn overridden_resize(_receiver: Id, _sel: Sel, _old: NSSize) {
        OVERRIDE_RAN.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    unsafe extern "C" fn overridden_hit_test(receiver: Id, sel: Sel, point: NSPoint) -> Id {
        HIT_OVERRIDE_RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Model an Objective-C override's `[super hitTest:point]`: enter the
        // inherited implementation without redispatching this override.
        unsafe { hit_test_method(receiver, sel, point) }
    }

    #[test]
    fn a_hit_test_override_can_call_super_once() {
        crate::bootstrap();
        let class = corefoundation::base::register(
            c"RosemaryTestHitView",
            objc::class::lookup(c"NSView"),
            INSTANCE_SIZE,
            false,
        );
        methods!(class, {
            "hitTest:" => overridden_hit_test, c"@32@0:8{CGPoint=dd}16";
        });
        let object = objc::object::create_instance(class, 0);
        // SAFETY: the object has `View` storage and the method signatures match.
        unsafe {
            init_with_frame(
                object,
                objc::sel::get("initWithFrame:"),
                rect(0.0, 0.0, 100.0, 100.0),
            );
        }
        HIT_OVERRIDE_RUNS.store(0, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: a live view and a point inside it.
        assert_eq!(
            unsafe { hit_test(object, NSPoint { x: 5.0, y: 5.0 }) },
            object
        );
        assert_eq!(
            HIT_OVERRIDE_RUNS.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    /// The subview is *sent* `-resizeWithOldSuperviewSize:`, so a program that
    /// overrides it lays itself out and the mask is not consulted behind its
    /// back. AppKit's contract, and the reason a view can size itself against
    /// something the mask cannot express.
    #[test]
    fn a_subclasss_own_resize_method_is_what_runs() {
        crate::bootstrap();
        let class = corefoundation::base::register(
            c"RosemaryTestResizeView",
            objc::class::lookup(c"NSView"),
            INSTANCE_SIZE,
            false,
        );
        methods!(class, {
            "resizeWithOldSuperviewSize:" => overridden_resize, c"v32@0:8{CGSize=dd}16";
        });
        let superview = view(rect(0.0, 0.0, 100.0, 100.0));
        let subview = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
        unsafe {
            init_with_frame(
                subview,
                objc::sel::get("initWithFrame:"),
                rect(0.0, 0.0, 100.0, 100.0),
            )
        };
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(superview, objc::sel::get("addSubview:"), subview) };
        view_of(subview).autoresizing_mask =
            autoresizing::WIDTH_SIZABLE | autoresizing::HEIGHT_SIZABLE;

        set_frame_of(superview, rect(0.0, 0.0, 200.0, 200.0));
        assert!(OVERRIDE_RAN.load(std::sync::atomic::Ordering::Relaxed));
        // The override did not call through, so nothing moved the frame.
        assert_eq!(view_of(subview).frame, rect(0.0, 0.0, 100.0, 100.0));
    }

    /// A view with no window scales by one, whatever it was asked - which is
    /// the case a headless test and a program before `-setContentView:` are in.
    #[test]
    fn a_view_with_no_window_does_not_scale_its_backing() {
        let rect = NSRect {
            origin: NSPoint::default(),
            size: NSSize {
                width: 640.0,
                height: 480.0,
            },
        };
        let v = view(rect);
        // SAFETY: `-convertRectToBacking:` is `NSRect (id, SEL, NSRect)`.
        let backing =
            unsafe { convert_rect_to_backing(v, objc::sel::get("convertRectToBacking:"), rect) };
        assert_eq!(backing, rect);
    }

    /// A subview knows its superview, and the superview lists it.
    #[test]
    fn a_subview_is_in_its_superviews_list() {
        let parent = view(NSRect::default());
        let child = view(NSRect::default());
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(parent, objc::sel::get("addSubview:"), child) };
        // SAFETY: `-superview` is `id (id, SEL)`.
        assert_eq!(
            unsafe { superview(child, objc::sel::get("superview")) },
            parent
        );
        // SAFETY: `-subviews` is `id (id, SEL)`.
        let list = unsafe { subviews(parent, objc::sel::get("subviews")) };
        // SAFETY: an array this module made.
        let array =
            unsafe { corefoundation::collections::array(list as *const c_void) }.expect("an array");
        assert_eq!(array.values.len(), 1);

        // SAFETY: `-removeFromSuperview` is `void (id, SEL)`.
        unsafe { remove_from_superview(child, objc::sel::get("removeFromSuperview")) };
        let array =
            unsafe { corefoundation::collections::array(list as *const c_void) }.expect("an array");
        assert!(array.values.is_empty());
    }

    /// A view finds its window through its superview, which is how a guest's
    /// `YYGLView` inside a content view reaches one.
    #[test]
    fn a_view_finds_its_window_up_the_chain() {
        let parent = view(NSRect::default());
        let child = view(NSRect::default());
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(parent, objc::sel::get("addSubview:"), child) };
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        // SAFETY: both are objects this runtime allocated.
        unsafe { set_window(parent, window) };
        assert_eq!(unsafe { window_of(child) }, window);
    }

    static FIRST_NATIVE_DRAW_CONTENT: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(true);

    unsafe extern "C" fn first_native_draw_rect(
        _receiver: Id,
        _selector: objc::sel::Sel,
        _rect: NSRect,
    ) {
        if !FIRST_NATIVE_DRAW_CONTENT.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        assert!(crate::backing::draw_focused(|pixels, width, height| {
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel.copy_from_slice(&[0, 0, 255, 255]);
            }
            Some(crate::backing::Rect {
                x: 0,
                y: 0,
                width: i64::from(width),
                height: i64::from(height),
            })
        }));
    }

    #[test]
    fn first_native_draw_rect_after_static_metal_publishes_independent_scene() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 320.0, 240.0))
        } {
            return;
        }
        FIRST_NATIVE_DRAW_CONTENT.store(true, std::sync::atomic::Ordering::Relaxed);
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 320.0, 240.0));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let metal_view = view(rect(0.0, 0.0, 320.0, 240.0));
        let cls = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(cls.metaclass().method(maker).unwrap().implementation()) };
        let metal = unsafe { make(cls as *const _ as Id, maker) };
        let draw_class =
            objc::class::lookup(c"RosemaryFirstNativeDrawTestView").unwrap_or_else(|| {
                let class = objc::class::allocate_pair(
                    objc::class::lookup(c"NSView"),
                    c"RosemaryFirstNativeDrawTestView",
                    0,
                )
                .expect("native draw test class");
                assert!(class.add_method(
                    objc::sel::get("drawRect:"),
                    first_native_draw_rect as objc::class::Imp,
                    c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16",
                ));
                objc::class::register_pair(class);
                class
            });
        let native = unsafe {
            init_with_frame(
                objc::object::create_instance(draw_class, 0),
                objc::sel::get("initWithFrame:"),
                rect(40.0, 35.0, 30.0, 25.0),
            )
        };
        unsafe {
            set_layer(metal_view, objc::sel::get("setLayer:"), metal);
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
            add_subview(content, objc::sel::get("addSubview:"), native);
        }
        flush_metal_scenes();
        assert_eq!(
            metal_scene(sdl).unwrap().len(),
            1,
            "a native view with no drawRect pixels must not publish a source"
        );
        assert!(
            native_metal_scene_active(sdl),
            "first native drawing must invalidate an already-published Metal window"
        );
        let set_needs: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_needs(native, objc::sel::get("setNeedsDisplay:"), 1) };
        crate::backing::display_view(native);
        crate::backing::compose_window_for_tests(window);
        flush_metal_scenes();
        let scene = metal_scene(sdl).unwrap();
        assert_eq!(
            scene.len(),
            2,
            "first drawRect: failed to publish native source"
        );
        assert_eq!(
            &scene[1].native_pixels.as_ref().unwrap()[..4],
            &[0, 0, 255, 255]
        );
        // Repeated normal setNeedsDisplay:/drawRect: can also empty the
        // native store. That transition must remove the stale source.
        FIRST_NATIVE_DRAW_CONTENT.store(false, std::sync::atomic::Ordering::Relaxed);
        unsafe { set_needs(native, objc::sel::get("setNeedsDisplay:"), 1) };
        crate::backing::display_view(native);
        crate::backing::compose_window_for_tests(window);
        flush_metal_scenes();
        assert_eq!(
            metal_scene(sdl).unwrap().len(),
            1,
            "empty redraw left native pixels above the static Metal source"
        );
        crate::backing::forget(native);
        objc::arc::release(window);
    }

    #[test]
    fn opaque_native_occlusion_responds_to_ancestor_ca_opacity() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 320.0, 240.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 320.0, 240.0));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let metal_view = view(rect(0.0, 0.0, 320.0, 240.0));
        let parent = view(rect(0.0, 0.0, 320.0, 240.0));
        let native = view(rect(40.0, 35.0, 30.0, 25.0));
        let metal_cls = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make_metal: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = unsafe {
            std::mem::transmute(
                metal_cls
                    .metaclass()
                    .method(maker)
                    .unwrap()
                    .implementation(),
            )
        };
        let metal = unsafe { make_metal(metal_cls as *const _ as Id, maker) };
        unsafe {
            set_layer(metal_view, objc::sel::get("setLayer:"), metal);
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
            add_subview(content, objc::sel::get("addSubview:"), parent);
            add_subview(parent, objc::sel::get("addSubview:"), native);
        }
        crate::backing::set_native_view_pixels_for_tests(
            native,
            30,
            25,
            [0u8, 0, 255, 255].repeat(30 * 25),
        );
        let covers = || {
            crate::backing::live_metal_occlusion(
                sdl,
                &capture_metal_scene(sdl, darwin::mach::media_time())
                    .unwrap()
                    .0,
            )
            .into_iter()
            .find(|(id, _)| *id == metal as usize)
            .unwrap()
            .1
        };
        assert_eq!(
            covers().len(),
            1,
            "fully opaque native pixels should occlude metal"
        );
        let ca = objc::class::lookup(c"CALayer").unwrap();
        let make_ca: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(ca.metaclass().method(maker).unwrap().implementation()) };
        let ancestor = unsafe { make_ca(ca as *const _ as Id, maker) };
        unsafe { set_layer(parent, objc::sel::get("setLayer:"), ancestor) };
        let opacity: unsafe extern "C" fn(Id, objc::sel::Sel, f32) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { opacity(ancestor, objc::sel::get("setOpacity:"), 0.5) };
        assert!(
            covers().is_empty(),
            "fractional ancestor CA opacity incorrectly punched a metal hole"
        );
        unsafe { opacity(ancestor, objc::sel::get("setOpacity:"), 1.0) };
        assert_eq!(covers().len(), 1);
        let radius: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mask: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            radius(ancestor, objc::sel::get("setCornerRadius:"), 16.0);
            mask(ancestor, objc::sel::get("setMasksToBounds:"), 1);
        }
        assert!(
            covers().is_empty(),
            "rounded ancestor mask must not punch a rectangular hole in Metal"
        );
        flush_metal_scenes();
        assert!(
            metal_scene(sdl)
                .unwrap()
                .iter()
                .any(|item| item.native_pixels.is_some() && !item.rounded.is_empty())
        );
        unsafe { mask(ancestor, objc::sel::get("setMasksToBounds:"), 0) };
        assert_eq!(covers().len(), 1);
        // A rectangular CA superlayer clips the native source more tightly
        // than its NSView frame. Metal must not be excluded outside the very
        // same GPU clip used by the retained native layer.
        let middle = unsafe { make_ca(ca as *const _ as Id, maker) };
        let own = unsafe { make_ca(ca as *const _ as Id, maker) };
        let set_frame: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let add_ca: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            set_layer(native, objc::sel::get("setLayer:"), own);
            set_frame(
                middle,
                objc::sel::get("setFrame:"),
                rect(42.0, 38.0, 14.0, 11.0),
            );
            add_ca(ancestor, objc::sel::get("addSublayer:"), middle);
            add_ca(middle, objc::sel::get("addSublayer:"), own);
            mask(middle, objc::sel::get("setMasksToBounds:"), 1);
        }
        let expected_coverage = || {
            let (scene, _) = capture_metal_scene(sdl, darwin::mach::media_time()).unwrap();
            let native = scene
                .iter()
                .find(|entry| entry.native_pixels.is_some())
                .unwrap();
            let masked = native
                .clip
                .expect("middle rectangular mask projects onto native");
            crate::backing::Rect {
                x: i64::from(masked.x),
                y: i64::from(masked.y),
                width: i64::from(masked.width),
                height: i64::from(masked.height),
            }
        };
        assert_eq!(covers(), vec![expected_coverage()]);
        assert!(covers()[0].width < 30 && covers()[0].height < 25);
        unsafe {
            set_frame(
                middle,
                objc::sel::get("setFrame:"),
                rect(42.0, 38.0, 8.0, 7.0),
            )
        };
        assert_eq!(covers(), vec![expected_coverage()]);
        assert!(covers()[0].width <= 8 && covers()[0].height <= 7);
        unsafe { mask(middle, objc::sel::get("setMasksToBounds:"), 0) };
        assert_eq!(covers().len(), 1);
        let hidden: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { hidden(ancestor, objc::sel::get("setHidden:"), 1) };
        assert!(
            covers().is_empty(),
            "hidden ancestor CA layer still occluded metal"
        );
        objc::arc::release(window);
    }

    #[test]
    fn metal_root_nonview_ca_sublayers_are_independent_ordered_native_pixels() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 100.0, 80.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 100.0, 80.0));
        let metal_view = view(rect(0.0, 0.0, 100.0, 80.0));
        let metal_cls = objc::class::lookup(c"CAMetalLayer").unwrap();
        let ordinary = objc::class::lookup(c"CALayer").unwrap();
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let metal = unsafe { make(metal_cls as *const _ as Id, objc::sel::get("layer")) };
        let child = unsafe { make(ordinary as *const _ as Id, objc::sel::get("layer")) };
        let set_layer: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_frame: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let add_layer: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let color = unsafe { crate::cgcontext::CGColorCreateGenericRGB(1., 0., 0., 1.) };
        unsafe {
            set_layer(window, objc::sel::get("setContentView:"), content);
            set_layer(metal_view, objc::sel::get("setLayer:"), metal);
            add_layer(content, objc::sel::get("addSubview:"), metal_view);
            set_frame(child, objc::sel::get("setFrame:"), rect(10., 10., 22., 15.));
            set_layer(child, objc::sel::get("setBackgroundColor:"), color as Id);
            add_layer(metal, objc::sel::get("addSublayer:"), child);
        }
        flush_metal_scenes();
        let initial = metal_scene(sdl).unwrap();
        assert_eq!(initial[0].layer, metal as usize);
        let native = initial
            .iter()
            .find(|item| item.native_pixels.is_some())
            .expect("non-view CALayer child missing above Metal drawable");
        assert_eq!(native.native_owner, metal_view as usize);
        let before = native.native_pixels.as_ref().unwrap();
        assert!(before.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0));
        let full = crate::backing::metal_sublayers_pixels_cropped(
            metal_view,
            100,
            80,
            crate::backing::Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 80,
            },
        )
        .expect("full retained CA raster")
        .0;
        assert!(
            full.as_chunks::<4>().0.iter().any(|pixel| pixel[3] == 0),
            "root Metal background must not be rasterized over its drawable"
        );
        let original_revision = native.native_revision;
        let original_frame = native.frame;
        let original_pixels = std::sync::Arc::clone(before);
        // Move the non-view child; no new Metal drawable is submitted.
        unsafe {
            set_frame(child, objc::sel::get("setFrame:"), rect(44., 20., 22., 15.));
        }
        flush_metal_scenes();
        let moved = metal_scene(sdl).unwrap();
        assert_eq!(moved[0].layer, metal as usize);
        let changed = moved
            .iter()
            .find(|item| item.native_pixels.is_some())
            .unwrap();
        assert!(
            changed.frame != original_frame,
            "moving CA child must move scene geometry"
        );
        assert_eq!(changed.native_revision, original_revision);
        assert_eq!(
            changed.native_pixels.as_ref().unwrap().as_slice(),
            original_pixels.as_slice()
        );
        assert!(std::sync::Arc::ptr_eq(&original_pixels, before));
        // Root opacity is scene metadata, not pre-applied to sublayer pixels.
        unsafe {
            crate::coreanimation::set_opacity_for_test(metal, 0.5);
        }
        flush_metal_scenes();
        let faded = metal_scene(sdl).unwrap();
        let group = [OpacityGroup {
            id: metal as usize,
            opacity: 0.5,
        }];
        assert_eq!(faded[0].opacity_groups, group);
        assert_eq!(faded[1].opacity_groups, group);
        assert_eq!(faded[1].opacity, 1.0);
        assert_eq!(faded[1].native_revision, changed.native_revision);
        // Descendant opacity modifies CPU pixels, unlike Metal root opacity.
        unsafe {
            crate::coreanimation::set_opacity_for_test(metal, 1.0);
        }
        unsafe {
            crate::coreanimation::set_opacity_for_test(child, 0.5);
        }
        flush_metal_scenes();
        let translucent = metal_scene(sdl).unwrap();
        let alpha = translucent[1]
            .native_pixels
            .as_ref()
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[3])
            .max()
            .unwrap();
        assert!((127..=129).contains(&alpha));
        assert_ne!(translucent[1].native_revision, changed.native_revision);
        // Removing the descendant must restore the prior Metal-only scene.
        let remove: unsafe extern "C" fn(Id, objc::sel::Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            remove(child, objc::sel::get("removeFromSuperlayer"));
        }
        flush_metal_scenes();
        assert_eq!(
            metal_scene(sdl)
                .unwrap()
                .iter()
                .filter(|layer| layer.native_pixels.is_some())
                .count(),
            0
        );
        objc::arc::release(window);
    }

    #[test]
    fn unmasked_metal_root_child_extends_beyond_view_and_masks_clip_it() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0., 0., 100., 80.))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0., 0., 100., 80.));
        let metal_view = view(rect(20., 20., 40., 30.));
        let class = objc::class::lookup(c"CAMetalLayer").unwrap();
        let child_cls = objc::class::lookup(c"CALayer").unwrap();
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let root = unsafe { make(class as *const _ as Id, objc::sel::get("layer")) };
        let child = unsafe { make(child_cls as *const _ as Id, objc::sel::get("layer")) };
        let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_frame: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_mask: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let red = unsafe { crate::cgcontext::CGColorCreateGenericRGB(1., 0., 0., 1.) };
        unsafe {
            set(window, objc::sel::get("setContentView:"), content);
            set(metal_view, objc::sel::get("setLayer:"), root);
            set(content, objc::sel::get("addSubview:"), metal_view);
            set_frame(child, objc::sel::get("setFrame:"), rect(-15., 5., 25., 12.));
            set(child, objc::sel::get("setBackgroundColor:"), red as Id);
            set(root, objc::sel::get("addSublayer:"), child);
        }
        flush_metal_scenes();
        let unmasked = metal_scene(sdl).unwrap();
        let drawable = unmasked
            .iter()
            .find(|item| item.layer == root as usize)
            .unwrap();
        let unmasked_raster = unmasked
            .iter()
            .find(|item| item.native_pixels.is_some())
            .unwrap();
        assert!(
            unmasked_raster.frame.x < drawable.frame.x,
            "unmasked CA descendant was clipped at the Metal view boundary"
        );
        assert!(
            unmasked_raster
                .native_pixels
                .as_ref()
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] > 0)
        );
        unsafe {
            set_mask(root, objc::sel::get("setMasksToBounds:"), 1);
        }
        flush_metal_scenes();
        let masked = metal_scene(sdl).unwrap();
        let masked_raster = masked
            .iter()
            .find(|item| item.native_pixels.is_some())
            .unwrap();
        assert!(
            masked_raster.frame.x >= drawable.frame.x,
            "masksToBounds did not clip the escaped CA child"
        );
        // The CA-ordered NSView-backed path must inherit the Metal root clip,
        // just like the ordinary CALayer raster path above.
        let backed_view = view(rect(-15., 0., 25., 12.));
        let backed_layer = unsafe { make(child_cls as *const _ as Id, objc::sel::get("layer")) };
        unsafe {
            set(backed_view, objc::sel::get("setLayer:"), backed_layer);
            set(metal_view, objc::sel::get("addSubview:"), backed_view);
            set_frame(
                backed_layer,
                objc::sel::get("setFrame:"),
                rect(-15., 0., 25., 12.),
            );
            set(
                backed_layer,
                objc::sel::get("setBackgroundColor:"),
                red as Id,
            );
            set(root, objc::sel::get("addSublayer:"), backed_layer);
        }
        flush_metal_scenes();
        let scene = metal_scene(sdl).unwrap();
        let backed = scene
            .iter()
            .find(|item| item.native_owner == backed_view as usize)
            .expect("view-backed CA child missing from ordered scene");
        let clip = backed.clip.expect("Metal root masksToBounds clip missing");
        assert!(clip.x >= drawable.frame.x && clip.width <= drawable.frame.width);
        objc::arc::release(window);
    }

    #[test]
    fn metal_ca_sublayers_interleave_view_backings_in_effective_order() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0., 0., 100., 80.))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0., 0., 100., 80.));
        let metal_view = view(rect(0., 0., 100., 80.));
        let blue_view = view(rect(10., 10., 20., 20.));
        let metal_class = objc::class::lookup(c"CAMetalLayer").unwrap();
        let ca_class = objc::class::lookup(c"CALayer").unwrap();
        let create: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let metal = unsafe { create(metal_class as *const _ as Id, objc::sel::get("layer")) };
        let a = unsafe { create(ca_class as *const _ as Id, objc::sel::get("layer")) };
        let b = unsafe { create(ca_class as *const _ as Id, objc::sel::get("layer")) };
        let c = unsafe { create(ca_class as *const _ as Id, objc::sel::get("layer")) };
        let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_frame: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let color = |r, g, bl| unsafe { crate::cgcontext::CGColorCreateGenericRGB(r, g, bl, 1.) };
        let (red, blue, green) = (color(1., 0., 0.), color(0., 0., 1.), color(0., 1., 0.));
        unsafe {
            set(window, objc::sel::get("setContentView:"), content);
            set(metal_view, objc::sel::get("setLayer:"), metal);
            set(blue_view, objc::sel::get("setLayer:"), b);
            set(content, objc::sel::get("addSubview:"), metal_view);
            set(metal_view, objc::sel::get("addSubview:"), blue_view);
            set_frame(a, objc::sel::get("setFrame:"), rect(10., 10., 20., 20.));
            set_frame(b, objc::sel::get("setFrame:"), rect(10., 10., 20., 20.));
            set_frame(c, objc::sel::get("setFrame:"), rect(10., 10., 20., 20.));
            set(a, objc::sel::get("setBackgroundColor:"), red as Id);
            set(b, objc::sel::get("setBackgroundColor:"), blue as Id);
            set(c, objc::sel::get("setBackgroundColor:"), green as Id);
            set(metal, objc::sel::get("addSublayer:"), a);
            set(metal, objc::sel::get("addSublayer:"), b);
            set(metal, objc::sel::get("addSublayer:"), c);
        }
        let verify = |expected: &[usize]| {
            flush_metal_scenes();
            let scene = metal_scene(sdl).unwrap();
            assert_eq!(scene[0].layer, metal as usize);
            let owners = scene
                .iter()
                .skip(1)
                .filter(|item| item.native_pixels.is_some())
                .map(|item| item.native_owner)
                .collect::<Vec<_>>();
            assert_eq!(owners, expected);

            scene
                .iter()
                .skip(1)
                .filter_map(|item| item.native_pixels.as_ref())
                .map(|data| {
                    data.as_chunks::<4>()
                        .0
                        .iter()
                        .find(|p| p[3] == 255)
                        .map(|pixel| pixel.to_vec())
                        .unwrap()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            verify(&[metal_view as usize, blue_view as usize, metal_view as usize]),
            vec![
                vec![0, 0, 255, 255],
                vec![255, 0, 0, 255],
                vec![0, 255, 0, 255]
            ]
        );
        // Changing the CA tree order must change GPU paint order, even though
        // the NSView hierarchy itself and all content bytes remain unchanged.
        let insert: unsafe extern "C" fn(Id, objc::sel::Sel, Id, u32) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            insert(metal, objc::sel::get("insertSublayer:atIndex:"), c, 0);
        }
        assert_eq!(
            verify(&[metal_view as usize, blue_view as usize]),
            vec![vec![0, 0, 255, 255], vec![255, 0, 0, 255]]
        );
        let hidden: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            hidden(c, objc::sel::get("setHidden:"), 1);
        }
        assert_eq!(
            verify(&[metal_view as usize, blue_view as usize]),
            vec![vec![0, 0, 255, 255], vec![255, 0, 0, 255]]
        );
        let remove: unsafe extern "C" fn(Id, objc::sel::Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            remove(c, objc::sel::get("removeFromSuperlayer"));
        }
        assert_eq!(
            verify(&[metal_view as usize, blue_view as usize]),
            vec![vec![0, 0, 255, 255], vec![255, 0, 0, 255]]
        );
        objc::arc::release(window);
    }

    #[test]
    fn interleaved_view_and_native_children_share_one_isolated_ca_opacity_group() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0., 0., 100., 80.))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0., 0., 100., 80.));
        let metal_view = view(rect(0., 0., 100., 80.));
        let blue_view = view(rect(10., 10., 20., 20.));
        let metal_class = objc::class::lookup(c"CAMetalLayer").unwrap();
        let ca_class = objc::class::lookup(c"CALayer").unwrap();
        let create: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let metal = unsafe { create(metal_class as *const _ as Id, objc::sel::get("layer")) };
        let wrap = unsafe { create(ca_class as *const _ as Id, objc::sel::get("layer")) };
        let a = unsafe { create(ca_class as *const _ as Id, objc::sel::get("layer")) };
        let b = unsafe { create(ca_class as *const _ as Id, objc::sel::get("layer")) };
        let c = unsafe { create(ca_class as *const _ as Id, objc::sel::get("layer")) };
        let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_frame: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let rgba = |r, g, bl| unsafe { crate::cgcontext::CGColorCreateGenericRGB(r, g, bl, 1.) };
        let (red, blue, green) = (rgba(1., 0., 0.), rgba(0., 0., 1.), rgba(0., 1., 0.));
        unsafe {
            set(window, objc::sel::get("setContentView:"), content);
            set(metal_view, objc::sel::get("setLayer:"), metal);
            set(blue_view, objc::sel::get("setLayer:"), b);
            set(content, objc::sel::get("addSubview:"), metal_view);
            set(metal_view, objc::sel::get("addSubview:"), blue_view);
            set_frame(wrap, objc::sel::get("setFrame:"), rect(0., 0., 100., 80.));
            for layer in [a, b, c] {
                set_frame(layer, objc::sel::get("setFrame:"), rect(10., 10., 20., 20.));
            }
            set(a, objc::sel::get("setBackgroundColor:"), red as Id);
            set(b, objc::sel::get("setBackgroundColor:"), blue as Id);
            set(c, objc::sel::get("setBackgroundColor:"), green as Id);
            set(metal, objc::sel::get("addSublayer:"), wrap);
            for child in [a, b, c] {
                set(wrap, objc::sel::get("addSublayer:"), child);
            }
            crate::coreanimation::set_opacity_for_test(wrap, 0.5);
        }
        let capture = || {
            flush_metal_scenes();
            metal_scene(sdl).unwrap()
        };
        let scene = capture();
        assert_eq!(scene.len(), 4, "A/B/C must remain independent sources");
        assert_eq!(scene[0].layer, metal as usize);
        let path = [OpacityGroup {
            id: wrap as usize,
            opacity: 0.5,
        }];
        for part in &scene[1..] {
            assert_eq!(part.opacity_groups, path);
            assert_eq!(part.opacity, 1.0, "container alpha was applied per source");
        }
        assert_eq!(
            scene
                .iter()
                .skip(1)
                .map(|p| p.native_owner)
                .collect::<Vec<_>>(),
            [metal_view as usize, blue_view as usize, metal_view as usize]
        );
        for native in [1, 2, 3] {
            let pixel = scene[native]
                .native_pixels
                .as_ref()
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .find(|px| px[3] != 0)
                .unwrap();
            assert_eq!(pixel[3], 255, "CPU capture premultiplied GPU group alpha");
        }
        unsafe {
            crate::coreanimation::set_opacity_for_test(wrap, 0.25);
        }
        let quarter = capture();
        assert!(quarter[1..].iter().all(|p| p.opacity_groups
            == [OpacityGroup {
                id: wrap as usize,
                opacity: 0.25
            }]
            && p.opacity == 1.0));
        // A live CABasicAnimation changes the shared isolation-group alpha
        // without altering any individual source opacity or guest Metal pixel.
        let animation_class =
            objc::class::ptr(objc::class::lookup(c"CABasicAnimation").unwrap()) as Id;
        let key = foundation::string::from_str("opacity");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let anim = unsafe {
            make(
                animation_class,
                objc::sel::get("animationWithKeyPath:"),
                key,
            )
        };
        let number = |v| {
            corefoundation::collections::new_number(corefoundation::collections::Number::Float(v))
                as Id
        };
        let (from, to) = (number(0.2), number(0.8));
        let set_obj: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_duration: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let add: unsafe extern "C" fn(Id, objc::sel::Sel, Id, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            set_obj(anim, objc::sel::get("setFromValue:"), from);
            set_obj(anim, objc::sel::get("setToValue:"), to);
            set_duration(anim, objc::sel::get("setDuration:"), 0.18);
            add(wrap, objc::sel::get("addAnimation:forKey:"), anim, key);
        }
        let initial = capture();
        let start_alpha = initial[1].opacity_groups[0].opacity;
        std::thread::sleep(std::time::Duration::from_millis(48));
        let animated = capture();
        let next_alpha = animated[1].opacity_groups[0].opacity;
        assert!(
            next_alpha > start_alpha + 0.08,
            "independently retained children failed to track animated group opacity"
        );
        assert!(animated[1..].iter().all(|source| source.opacity_groups
            == [OpacityGroup {
                id: wrap as usize,
                opacity: next_alpha
            }]
            && source.opacity == 1.0));
        std::thread::sleep(std::time::Duration::from_millis(190));
        assert!((capture()[1].opacity_groups[0].opacity - 0.25).abs() < 0.001);
        let remove: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { remove(wrap, objc::sel::get("removeAnimationForKey:"), key) };
        for object in [key, from, to] {
            objc::arc::release(object);
        }

        let insert: unsafe extern "C" fn(Id, objc::sel::Sel, Id, u32) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            insert(wrap, objc::sel::get("insertSublayer:atIndex:"), a, 2);
        }
        let reordered = capture();
        assert_eq!(
            reordered
                .iter()
                .skip(1)
                .map(|p| p.native_owner)
                .collect::<Vec<_>>(),
            [blue_view as usize, metal_view as usize]
        );
        let hidden: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            hidden(a, objc::sel::get("setHidden:"), 1);
        }
        let fewer = capture();
        assert_eq!(fewer.len(), reordered.len());
        assert_ne!(
            fewer.last().unwrap().native_pixels.as_ref().unwrap(),
            reordered.last().unwrap().native_pixels.as_ref().unwrap(),
            "hidden top child kept its previous opaque CPU content"
        );
        objc::arc::release(window);
    }

    #[test]
    fn transparent_ca_sibling_is_neutral_to_admitted_native_foreground() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 160.0, 120.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 160.0, 120.0));
        let metal_view = view(rect(0.0, 0.0, 160.0, 120.0));
        let label = view(rect(25.0, 20.0, 30.0, 22.0));
        let empty = view(rect(55.0, 25.0, 24.0, 16.0));
        let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let metal_class = objc::class::lookup(c"CAMetalLayer").unwrap();
        let ca_class = objc::class::lookup(c"CALayer").unwrap();
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let metal = unsafe { make(metal_class as *const _ as Id, objc::sel::get("layer")) };
        let ca = unsafe { make(ca_class as *const _ as Id, objc::sel::get("layer")) };
        let transparent = unsafe { crate::cgcontext::CGColorCreateGenericRGB(1.0, 0.0, 0.0, 0.0) };
        let opaque = unsafe { crate::cgcontext::CGColorCreateGenericRGB(1.0, 0.0, 0.0, 1.0) };
        unsafe {
            set(window, objc::sel::get("setContentView:"), content);
            set(metal_view, objc::sel::get("setLayer:"), metal);
            set(empty, objc::sel::get("setLayer:"), ca);
            set(ca, objc::sel::get("setBackgroundColor:"), transparent as Id);
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
            add_subview(content, objc::sel::get("addSubview:"), label);
        }
        crate::backing::set_native_view_pixels_for_tests(
            label,
            30,
            22,
            [0u8, 0, 255, 255].repeat(30 * 22),
        );
        flush_metal_scenes();
        assert_eq!(metal_scene(sdl).unwrap().len(), 2);
        assert!(
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(160, 120))
                .is_none()
        );
        accept_published_native_for_tests(sdl);
        let bottom =
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(160, 120))
                .expect("admitted red foreground is separated from Metal");
        unsafe { add_subview(content, objc::sel::get("addSubview:"), empty) };
        flush_metal_scenes();
        let scene = metal_scene(sdl).unwrap();
        assert_eq!(
            scene.len(),
            2,
            "all-transparent CA view unexpectedly published a GPU image"
        );
        assert_eq!(scene[1].native_owner, label as usize);
        assert_eq!(
            &scene[1].native_pixels.as_ref().unwrap()[..4],
            &[0, 0, 255, 255]
        );
        let visible_frame = scene[1].frame;
        let underlay_at_label =
            ((visible_frame.y as usize * 160 + visible_frame.x as usize) * 4) as usize;
        assert_eq!(
            &bottom.pixels[underlay_at_label..underlay_at_label + 4],
            &[237, 237, 237, 255],
            "visible native foreground was incorrectly baked into the Metal underlay"
        );
        assert!(
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(160, 120))
                .is_some(),
            "known-empty CA sibling blocked the admitted foreground"
        );

        // New visible pixels invalidate the negative cache, but cannot be
        // removed from the bottom source before GPU admission.
        unsafe { set(ca, objc::sel::get("setBackgroundColor:"), opaque as Id) };
        flush_metal_scenes();
        assert_eq!(metal_scene(sdl).unwrap().len(), 3);
        assert!(
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(160, 120))
                .is_none(),
            "an unadmitted newly opaque CA sibling was treated as empty"
        );
        accept_published_native_for_tests(sdl);
        assert!(
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(160, 120))
                .is_some()
        );
        unsafe { set(ca, objc::sel::get("setBackgroundColor:"), transparent as Id) };
        flush_metal_scenes();
        assert_eq!(metal_scene(sdl).unwrap().len(), 2);
        assert!(
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(160, 120))
                .is_some()
        );
        let remove: unsafe extern "C" fn(Id, objc::sel::Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { remove(empty, objc::sel::get("removeFromSuperview")) };
        flush_metal_scenes();
        assert_eq!(metal_scene(sdl).unwrap().len(), 2);
        assert!(
            std::sync::Arc::ptr_eq(
                &bottom,
                &crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(160, 120))
                    .unwrap()
            ),
            "transparent sibling changed the independently cached underlay"
        );
        objc::arc::release(window);
    }

    #[test]
    fn transparent_four_k_ca_capture_is_negatively_cached_and_invalidated() {
        crate::bootstrap();
        let large = view(rect(0.0, 0.0, 3840.0, 2160.0));
        let other = view(rect(0.0, 0.0, 16.0, 16.0));
        let cls = objc::class::lookup(c"CALayer").unwrap();
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let ca = unsafe { make(cls as *const _ as Id, objc::sel::get("layer")) };
        let unrelated = unsafe { make(cls as *const _ as Id, objc::sel::get("layer")) };
        let transparent = unsafe { crate::cgcontext::CGColorCreateGenericRGB(0.0, 0.0, 1.0, 0.0) };
        let opaque = unsafe { crate::cgcontext::CGColorCreateGenericRGB(0.0, 0.0, 1.0, 1.0) };
        unsafe {
            set(large, objc::sel::get("setLayer:"), ca);
            set(other, objc::sel::get("setLayer:"), unrelated);
            set(ca, objc::sel::get("setBackgroundColor:"), transparent as Id);
        }
        let before = crate::backing::native_render_count_for_tests();
        assert!(crate::backing::native_scene_pixels(large, 3840, 2160).is_none());
        for _ in 0..24 {
            crate::coreanimation::changed(unrelated);
            assert!(crate::backing::native_scene_pixels(large, 3840, 2160).is_none());
        }
        assert_eq!(
            crate::backing::native_render_count_for_tests() - before,
            1,
            "unchanged 4K transparent CA was rasterized more than once"
        );
        unsafe { set(ca, objc::sel::get("setBackgroundColor:"), opaque as Id) };
        let image = crate::backing::native_scene_pixels(large, 3840, 2160)
            .expect("a changed opaque CA layer must invalidate empty cache");
        assert!(image.0.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0));
        assert_eq!(crate::backing::native_render_count_for_tests() - before, 2);
        unsafe { set(ca, objc::sel::get("setBackgroundColor:"), transparent as Id) };
        assert!(crate::backing::native_scene_pixels(large, 3840, 2160).is_none());
        assert_eq!(crate::backing::native_render_count_for_tests() - before, 3);
        crate::backing::forget(large);
        crate::backing::forget(other);
        objc::arc::release(large);
        objc::arc::release(other);
    }

    #[test]
    fn native_cpu_pixels_are_separate_above_metal_and_underneath_below_it() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 320.0, 240.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 320.0, 240.0));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let metal_view = view(rect(0.0, 0.0, 320.0, 240.0));
        let native = view(rect(40.0, 35.0, 30.0, 25.0));
        let cls = objc::class::lookup(c"CAMetalLayer").expect("CAMetalLayer");
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(cls.metaclass().method(maker).unwrap().implementation()) };
        let metal = unsafe { make(cls as *const _ as Id, maker) };
        unsafe {
            set_layer(metal_view, objc::sel::get("setLayer:"), metal);
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
            add_subview(content, objc::sel::get("addSubview:"), native);
        }
        crate::backing::set_native_view_pixels_for_tests(
            native,
            30,
            25,
            [0u8, 0, 128, 128].repeat(30 * 25),
        );
        flush_metal_scenes();
        assert!(
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(320, 240),)
                .is_none(),
            "publishing AppKit pixels before Vulkan admission must not exclude them"
        );
        accept_published_native_for_tests(sdl);
        assert!(
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(320, 240),)
                .is_some(),
            "completed GPU scene admission must enable independent native stacking"
        );
        let scene = metal_scene(sdl).expect("live AppKit Metal/native scene");
        assert_eq!(scene.len(), 2);
        assert_eq!(scene[0].layer, metal as usize);
        assert_eq!(
            scene[1].layer,
            (native as usize) | (1usize << (usize::BITS - 1))
        );
        assert_eq!(scene[1].native_pixels.as_ref().unwrap().len(), 30 * 25 * 4);
        let original_revision = scene[1].native_revision;
        let previous_underlay =
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(320, 240))
                .expect("GPU first acknowledgment produced native separation");
        // A new source with the *same* view address is only a proposal until
        // Metal commits those new pixels. The previously accepted revision
        // must not exclude current pixels beneath a translucent drawable.
        crate::backing::set_native_view_pixels_for_tests(
            native,
            30,
            25,
            [1u8, 0, 128, 128].repeat(30 * 25),
        );
        flush_metal_scenes();
        let proposed = metal_scene(sdl).unwrap();
        assert_ne!(proposed[1].native_revision, original_revision);
        assert!(
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(320, 240),)
                .is_none(),
            "published revision was subtracted without GPU admission"
        );
        accept_published_native_for_tests(sdl);
        let accepted_underlay =
            crate::backing::gpu_under_metal_cached(sdl, &crate::backing::Store::new(320, 240))
                .expect("newly committed revision reenabled separation");
        assert!(
            std::sync::Arc::ptr_eq(&previous_underlay, &accepted_underlay),
            "unchanged lower-content underlay was rebuilt for a foreground admission revision"
        );
        // Restore the original test pixels after checking the transition.
        crate::backing::set_native_view_pixels_for_tests(
            native,
            30,
            25,
            [0u8, 0, 128, 128].repeat(30 * 25),
        );
        flush_metal_scenes();
        accept_published_native_for_tests(sdl);
        // Software-backed view overlays follow the same above-Metal order,
        // rather than being silently baked into the bottom CPU image.
        crate::backing::set_overlay(
            native,
            crate::backing::Overlay {
                width: 30,
                height: 25,
                picture: crate::backing::Picture::Pixels([0u8, 0, 64, 128].repeat(30 * 25)),
            },
        );
        invalidate_metal_scene(false);
        flush_metal_scenes();
        let with_overlay = metal_scene(sdl).unwrap();
        assert_eq!(
            &with_overlay[1].native_pixels.as_ref().unwrap()[..4],
            &[0, 0, 127, 191]
        );
        crate::backing::remove_overlay(native);
        invalidate_metal_scene(false);
        flush_metal_scenes();
        // Root Core Animation opacity belongs in the scene placement, not in
        // pre-faded CPU pixels. That would double-fade CA content while view
        // drawRect pixels remained unaffected.
        let ordinary = objc::class::lookup(c"CALayer").expect("CALayer");
        let native_layer = unsafe {
            let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = std::mem::transmute(
                ordinary
                    .metaclass()
                    .method(objc::sel::get("layer"))
                    .unwrap()
                    .implementation(),
            );
            make(ordinary as *const _ as Id, objc::sel::get("layer"))
        };
        unsafe { set_layer(native, objc::sel::get("setLayer:"), native_layer) };
        unsafe { crate::coreanimation::set_opacity_for_test(native_layer, 0.5) };
        flush_metal_scenes();
        let faded = metal_scene(sdl).unwrap();
        assert!((faded[1].opacity - 0.5).abs() <= f32::EPSILON);
        // Metadata-only CA animation/opacity changes must reuse both the CPU
        // capture and immutable Arc; the GPU revision must stay identical.
        let captured = crate::backing::native_render_count_for_tests();
        let pixels = faded[1].native_pixels.as_ref().unwrap().clone();
        for step in 0..32 {
            unsafe { crate::coreanimation::set_opacity_for_test(native_layer, step as f32 / 31.0) };
            flush_metal_scenes();
            let latest = metal_scene(sdl).unwrap();
            assert!(std::sync::Arc::ptr_eq(
                latest[1].native_pixels.as_ref().unwrap(),
                &pixels,
            ));
            assert_eq!(latest[1].native_revision, faded[1].native_revision);
        }
        assert_eq!(
            crate::backing::native_render_count_for_tests(),
            captured,
            "opacity-only scene updates re-rendered unchanged native pixels"
        );
        assert_eq!(
            &faded[1].native_pixels.as_ref().unwrap()[..4],
            &[0, 0, 128, 128]
        );
        unsafe { crate::coreanimation::set_opacity_for_test(native_layer, 1.0) };
        flush_metal_scenes();
        let flat = crate::backing::Store::new(320, 240);
        let underneath = crate::backing::gpu_under_metal(sdl, &flat)
            .expect("native top view requires separately composited bottom source");
        let old = scene[1].frame;
        let pixel = &underneath.pixels[(old.y as usize * 320 + old.x as usize) * 4..][..4];
        assert_eq!(
            pixel,
            &[237, 237, 237, 255],
            "translucent top native pixels were incorrectly baked under Metal"
        );
        unsafe {
            set_frame_origin(
                native,
                objc::sel::get("setFrameOrigin:"),
                NSPoint { x: 90.0, y: 60.0 },
            )
        };
        flush_metal_scenes();
        let moved = metal_scene(sdl).unwrap();
        assert_ne!(moved[1].frame.x, old.x);
        assert_eq!(moved[1].native_revision, scene[1].native_revision);
        unsafe {
            add_subview_positioned(
                content,
                objc::sel::get("addSubview:positioned:relativeTo:"),
                native,
                ORDER_BELOW,
                metal_view,
            )
        };
        flush_metal_scenes();
        assert_eq!(
            metal_scene(sdl)
                .unwrap()
                .iter()
                .map(|p| p.layer)
                .collect::<Vec<_>>(),
            [metal as usize],
            "native content below Metal must remain in the base backing"
        );
        assert!(crate::backing::gpu_under_metal(sdl, &flat).is_none());
        unsafe { remove_from_superview(native, objc::sel::get("removeFromSuperview")) };
        flush_metal_scenes();
        assert_eq!(metal_scene(sdl).unwrap().len(), 1);
        objc::arc::release(window);
    }

    /// The compositor consumes authoritative live AppKit order, not the order
    /// in which a guest submits Metal command buffers. Hidden and removed
    /// views must disappear from its scene before their next drawable.
    #[test]
    fn metal_scene_tracks_sibling_order_hiding_and_removal() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 320.0, 240.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 320.0, 240.0));
        let set_content = objc::sel::get("setContentView:");
        unsafe {
            let f: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
                std::mem::transmute(objc::msg::entry());
            f(window, set_content, content);
        }
        let metal = objc::class::lookup(c"CAMetalLayer").expect("CAMetalLayer");
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = unsafe {
            std::mem::transmute(metal.metaclass().method(maker).unwrap().implementation())
        };
        let (a, b) = (
            view(rect(0.0, 0.0, 320.0, 240.0)),
            view(rect(25.0, 35.0, 120.0, 70.0)),
        );
        let (first, second) = unsafe {
            (
                make(metal as *const _ as Id, maker),
                make(metal as *const _ as Id, maker),
            )
        };
        unsafe {
            set_layer(a, objc::sel::get("setLayer:"), first);
            set_layer(b, objc::sel::get("setLayer:"), second);
            add_subview(content, objc::sel::get("addSubview:"), a);
            add_subview(content, objc::sel::get("addSubview:"), b);
        }
        let metal_scene = |sdl| {
            flush_metal_scenes();
            super::metal_scene(sdl)
        };
        let ids = |sdl| {
            metal_scene(sdl)
                .unwrap()
                .iter()
                .map(|entry| entry.layer)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(sdl), [first as usize, second as usize]);
        let baseline = METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed);
        // Bulk CA mutations do not traverse views synchronously. The
        // last model value is captured once at the event-pump boundary.
        let set_opacity: unsafe extern "C" fn(Id, objc::sel::Sel, f32) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        for index in 0..64 {
            unsafe { set_opacity(second, objc::sel::get("setOpacity:"), index as f32 / 64.0) };
        }
        assert_eq!(
            METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed),
            baseline
        );
        flush_metal_scenes();
        let after = METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            after - baseline <= crate::window::all().len(),
            "bulk layout triggered more than one scene capture per window"
        );
        assert!((metal_scene(sdl).unwrap()[1].opacity - 63.0 / 64.0).abs() < 0.0001);
        unsafe { set_opacity(second, objc::sel::get("setOpacity:"), 1.0) };
        assert_eq!(ids(sdl), [first as usize, second as usize]);
        unsafe { set_hidden(b, objc::sel::get("setHidden:"), 1) };
        assert_eq!(ids(sdl), [first as usize]);
        unsafe { set_hidden(b, objc::sel::get("setHidden:"), 0) };
        assert_eq!(ids(sdl), [first as usize, second as usize]);
        // A CAMetalLayer can change opacity or visibility without its NSView
        // doing so. The published scene must reflect both transitions.
        unsafe {
            let hidden: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
                std::mem::transmute(objc::msg::entry());
            hidden(second, objc::sel::get("setHidden:"), 1);
        }
        assert_eq!(ids(sdl), [first as usize]);
        unsafe {
            let hidden: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
                std::mem::transmute(objc::msg::entry());
            hidden(second, objc::sel::get("setHidden:"), 0);
            let opacity: unsafe extern "C" fn(Id, objc::sel::Sel, f32) =
                std::mem::transmute(objc::msg::entry());
            opacity(second, objc::sel::get("setOpacity:"), 0.5);
        }
        let current = metal_scene(sdl).expect("published layer snapshot");
        assert_eq!(current.len(), 2);
        assert!((current[1].opacity - 0.5).abs() <= f32::EPSILON);
        assert!(current[1].opaque, "CAMetalLayer should default to opaque");
        // The model opacity stays at 0.5 while the presentation-layer
        // animation interpolates on each event turn, without new Metal pixels.
        let animation_class =
            objc::class::ptr(objc::class::lookup(c"CABasicAnimation").expect("CABasicAnimation"))
                as Id;
        let key = foundation::string::from_str("opacity");
        let send_obj: unsafe extern "C" fn(Id, objc::sel::Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let animation = unsafe {
            send_obj(
                animation_class,
                objc::sel::get("animationWithKeyPath:"),
                key,
            )
        };
        let num = |value| {
            corefoundation::collections::new_number(corefoundation::collections::Number::Float(
                value,
            )) as Id
        };
        let (from, to) = (num(0.0), num(1.0));
        unsafe {
            let set_obj: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
                std::mem::transmute(objc::msg::entry());
            let set_duration: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
                std::mem::transmute(objc::msg::entry());
            let add: unsafe extern "C" fn(Id, objc::sel::Sel, Id, Id) =
                std::mem::transmute(objc::msg::entry());
            set_obj(animation, objc::sel::get("setFromValue:"), from);
            set_obj(animation, objc::sel::get("setToValue:"), to);
            set_duration(animation, objc::sel::get("setDuration:"), 0.16);
            add(
                second,
                objc::sel::get("addAnimation:forKey:"),
                animation,
                key,
            );
        }
        flush_metal_scenes();
        let animated_start = metal_scene(sdl).unwrap()[1].opacity;
        std::thread::sleep(std::time::Duration::from_millis(45));
        flush_metal_scenes();
        let animated_next = metal_scene(sdl).unwrap()[1].opacity;
        assert!(
            animated_next > animated_start + 0.10,
            "presentation opacity did not advance without another drawable"
        );
        std::thread::sleep(std::time::Duration::from_millis(160));
        flush_metal_scenes();
        assert!(
            (metal_scene(sdl).unwrap()[1].opacity - 0.5).abs() < 0.001,
            "removed-on-completion CA animation did not restore model alpha"
        );
        unsafe {
            let remove: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
                std::mem::transmute(objc::msg::entry());
            remove(second, objc::sel::get("removeAnimationForKey:"), key);
        }
        for value in [key, from, to] {
            objc::arc::release(value);
        }
        let set_opaque: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_opaque(second, objc::sel::get("setOpaque:"), 0) };
        assert!(!metal_scene(sdl).unwrap()[1].opaque);
        unsafe { set_opaque(second, objc::sel::get("setOpaque:"), 1) };
        assert!(metal_scene(sdl).unwrap()[1].opaque);
        // Moving a live Metal layer changes placement without requiring it to
        // submit another drawable. Sibling reordering changes compositing
        // order even if neither layer publishes a new frame.
        let old_frame = current[1].frame;
        unsafe {
            set_frame_origin(
                b,
                objc::sel::get("setFrameOrigin:"),
                NSPoint { x: 42.0, y: 50.0 },
            )
        };
        let moved = metal_scene(sdl).expect("updated geometry");
        assert_ne!(moved[1].frame.x, old_frame.x);
        assert_ne!(moved[1].frame.y, old_frame.y);
        unsafe {
            add_subview_positioned(
                content,
                objc::sel::get("addSubview:positioned:relativeTo:"),
                a,
                1,
                b,
            )
        };
        assert_eq!(ids(sdl), [second as usize, first as usize]);
        unsafe {
            add_subview_positioned(
                content,
                objc::sel::get("addSubview:positioned:relativeTo:"),
                a,
                ORDER_BELOW,
                b,
            )
        };
        assert_eq!(ids(sdl), [first as usize, second as usize]);
        // Reparent B beneath a container. Hiding an ancestor must retire the
        // nested Metal layer without changing B's own hidden flag.
        let container = view(rect(15.0, 10.0, 280.0, 200.0));
        unsafe {
            add_subview(content, objc::sel::get("addSubview:"), container);
            add_subview(container, objc::sel::get("addSubview:"), b);
        }
        assert_eq!(ids(sdl), [first as usize, second as usize]);
        // The parent view's ordinary CA backing layer is not necessarily a
        // superlayer of its child's CAMetalLayer. Its hidden/opacity state
        // must nonetheless be inherited by the child view's presentation.
        let ca = objc::class::lookup(c"CALayer").expect("CALayer");
        let parent_layer = unsafe { make(ca as *const _ as Id, maker) };
        unsafe { set_layer(container, objc::sel::get("setLayer:"), parent_layer) };
        let ca_opacity: unsafe extern "C" fn(Id, objc::sel::Sel, f32) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let ca_hidden: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { ca_opacity(parent_layer, objc::sel::get("setOpacity:"), 0.4) };
        let grouped = metal_scene(sdl).unwrap();
        assert_eq!(
            grouped[1].opacity_groups,
            [OpacityGroup {
                id: parent_layer as usize,
                opacity: 0.4
            }]
        );
        assert!((grouped[1].opacity - 0.5).abs() < 0.0001);
        unsafe { ca_hidden(parent_layer, objc::sel::get("setHidden:"), 1) };
        assert_eq!(ids(sdl), [first as usize]);
        unsafe { ca_hidden(parent_layer, objc::sel::get("setHidden:"), 0) };
        assert_eq!(ids(sdl), [first as usize, second as usize]);
        // A genuinely linked CA superlayer must not be multiplied twice.
        let ca_add: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { ca_add(parent_layer, objc::sel::get("addSublayer:"), second) };
        assert_eq!(
            metal_scene(sdl).unwrap()[1].opacity_groups,
            [OpacityGroup {
                id: parent_layer as usize,
                opacity: 0.4
            }]
        );
        // NSView alpha remains independent from the ordinary CA opacity.
        unsafe { set_alpha_value(container, objc::sel::get("setAlphaValue:"), 0.5) };
        assert_eq!(
            metal_scene(sdl).unwrap()[1].opacity_groups,
            [
                OpacityGroup {
                    id: container as usize,
                    opacity: 0.5
                },
                OpacityGroup {
                    id: parent_layer as usize,
                    opacity: 0.4
                }
            ]
        );
        unsafe { ca_opacity(parent_layer, objc::sel::get("setOpacity:"), 1.0) };
        let before_container_move = metal_scene(sdl).unwrap()[1].frame;
        unsafe {
            set_frame_origin(
                container,
                objc::sel::get("setFrameOrigin:"),
                NSPoint { x: 35.0, y: 22.0 },
            )
        };
        let after_container_move = metal_scene(sdl).unwrap()[1].frame;
        assert_ne!(after_container_move.x, before_container_move.x);
        // A clipped parent must constrain a retained descendant's pixels
        // even when the descendant never submits another drawable.
        unsafe {
            set_frame(
                container,
                objc::sel::get("setFrame:"),
                rect(35.0, 22.0, 60.0, 45.0),
            );
            set_clips_to_bounds(container, objc::sel::get("setClipsToBounds:"), 1);
        }
        let scene = metal_scene(sdl).unwrap();
        assert_eq!(scene.len(), 2);
        let expected = unsafe { presentation_frame(container) }.unwrap();
        let clip = scene[1].clip.expect("subview not clipped to parent bounds");
        assert_eq!(
            (clip.x, clip.y, clip.width, clip.height),
            (expected.x, expected.y, expected.width, expected.height)
        );
        // A CA rounded masksToBounds ancestor must reach the Metal scene
        // even when neither child view nor drawable publishes new content.
        let ca_masks: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let ca_radius: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            ca_masks(parent_layer, objc::sel::get("setMasksToBounds:"), 1);
            ca_radius(parent_layer, objc::sel::get("setCornerRadius:"), 80.0);
        }
        let rounded = metal_scene(sdl).unwrap();
        assert_eq!(rounded[1].rounded.len(), 1);
        assert_eq!(rounded[1].rounded[0].frame.x, expected.x);
        assert_eq!(rounded[1].rounded[0].frame.y, expected.y);
        assert!(
            rounded[1].rounded[0].radius
                > rounded[1].rounded[0]
                    .frame
                    .width
                    .min(rounded[1].rounded[0].frame.height) as f32
                    / 2.0,
            "intermediate CALayer radius incorrectly clamped before GPU scene capture"
        );
        unsafe {
            ca_masks(second, objc::sel::get("setMasksToBounds:"), 1);
            ca_radius(second, objc::sel::get("setCornerRadius:"), 180.0);
        }
        let both = metal_scene(sdl).unwrap();
        assert_eq!(both[1].rounded.len(), 2);
        assert!(
            both[1].rounded[1].radius
                > both[1].rounded[1]
                    .frame
                    .width
                    .min(both[1].rounded[1].frame.height) as f32
                    / 2.0,
            "NSView-backed CALayer radius incorrectly clamped before GPU scene capture"
        );
        unsafe {
            ca_masks(parent_layer, objc::sel::get("setMasksToBounds:"), 0);
            ca_masks(second, objc::sel::get("setMasksToBounds:"), 0);
        }
        assert!(metal_scene(sdl).unwrap()[1].rounded.is_empty());
        unsafe { set_clips_to_bounds(container, objc::sel::get("setClipsToBounds:"), 0) };
        assert!(metal_scene(sdl).unwrap()[1].clip.is_none());
        unsafe { set_hidden(container, objc::sel::get("setHidden:"), 1) };
        assert_eq!(ids(sdl), [first as usize]);
        unsafe { set_hidden(container, objc::sel::get("setHidden:"), 0) };
        assert_eq!(ids(sdl), [first as usize, second as usize]);
        // Moving an existing view into a second window must evict it from the
        // first compositor before the second window can accept its drawable.
        let other_window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        let mut other_sdl_for_teardown = std::ptr::null_mut();
        if unsafe {
            crate::window::init_with_content_rect_for_tests(
                other_window,
                rect(0.0, 0.0, 320.0, 240.0),
            )
        } {
            let other_sdl = unsafe { crate::window::sdl_window(other_window) };
            other_sdl_for_teardown = other_sdl;
            let other_content = view(rect(0.0, 0.0, 320.0, 240.0));
            unsafe {
                let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
                    std::mem::transmute(objc::msg::entry());
                set_content(
                    other_window,
                    objc::sel::get("setContentView:"),
                    other_content,
                );
                add_subview(other_content, objc::sel::get("addSubview:"), b);
            }
            assert_eq!(ids(sdl), [first as usize]);
            assert_eq!(ids(other_sdl), [second as usize]);
            METAL_SCENE_DAMAGED_WINDOWS.lock().unwrap().clear();
            let before = METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed);
            // A CA setter on the first window neither walks nor damages the
            // second window, although both remain alive and visible.
            unsafe { crate::coreanimation::set_opacity_for_test(first, 0.83) };
            assert_eq!(
                METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed),
                before
            );
            flush_metal_scenes();
            assert_eq!(
                METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed) - before,
                1,
                "one CA setter captured an unrelated NSWindow"
            );
            assert_eq!(
                *METAL_SCENE_DAMAGED_WINDOWS.lock().unwrap(),
                [sdl as usize],
                "CA property mutation damaged another window's CPU backing"
            );
            assert!((metal_scene(sdl).unwrap()[0].opacity - 0.83).abs() < 0.001);
            assert_eq!(metal_scene(other_sdl).unwrap()[0].opacity, 0.5);
            unsafe { remove_from_superview(b, objc::sel::get("removeFromSuperview")) };
            assert_eq!(ids(other_sdl), []);
            // Another guest thread may mutate an ordinary CALayer while the
            // video owner reparents and detaches its Metal view. Publication
            // cannot borrow the reparented view from the setter thread.
            std::thread::scope(|scope| {
                let parent = parent_layer as usize;
                let writer = scope.spawn(move || {
                    let layer = parent as Id;
                    for i in 0..256 {
                        unsafe {
                            crate::coreanimation::set_opacity_for_test(
                                layer,
                                (i % 100) as f32 / 100.0,
                            )
                        };
                    }
                });
                for _ in 0..32 {
                    unsafe {
                        add_subview(content, objc::sel::get("addSubview:"), b);
                        add_subview(other_content, objc::sel::get("addSubview:"), b);
                    }
                }
                unsafe { remove_from_superview(b, objc::sel::get("removeFromSuperview")) };
                writer.join().expect("CALayer mutation thread");
            });
            assert_eq!(ids(other_sdl), []);
            assert_eq!(ids(sdl), [first as usize]);
        } else {
            unsafe { remove_from_superview(b, objc::sel::get("removeFromSuperview")) };
        }
        assert_eq!(ids(sdl), [first as usize]);
        objc::arc::release(other_window);
        if !other_sdl_for_teardown.is_null() {
            assert!(
                super::metal_scene(other_sdl_for_teardown).is_none(),
                "destroyed NSWindow retained stale Metal scene metadata"
            );
        }
        objc::arc::release(window);
    }

    /// Opt-in AppKit owner-thread benchmark. Mutating N layered views in one
    /// transaction should make one O(N) capture, not N O(N) captures.
    #[test]
    fn batch_metal_scene_capture_cost() {
        if std::env::var_os("ROSEMARY_BENCH_APPKIT_SCENE").is_none() {
            return;
        }
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 640.0, 480.0))
        } {
            return;
        }
        let content = view(rect(0.0, 0.0, 640.0, 480.0));
        let attach: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { attach(window, objc::sel::get("setContentView:"), content) };
        let metal = objc::class::lookup(c"CAMetalLayer").expect("CAMetalLayer");
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = unsafe {
            std::mem::transmute(metal.metaclass().method(maker).unwrap().implementation())
        };
        let mut layers = Vec::new();
        for i in 0..128 {
            let child = view(rect((i % 16 * 36) as f64, (i / 16 * 50) as f64, 34.0, 45.0));
            let layer = unsafe { make(metal as *const _ as Id, maker) };
            unsafe {
                set_layer(child, objc::sel::get("setLayer:"), layer);
                add_subview(content, objc::sel::get("addSubview:"), child);
            }
            layers.push(layer);
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        flush_metal_scenes();
        let baseline = METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed);
        let start = std::time::Instant::now();
        for i in 0..2048usize {
            unsafe {
                crate::coreanimation::set_opacity_for_test(
                    layers[i % layers.len()],
                    ((i % 97) as f32 + 1.0) / 100.0,
                )
            };
        }
        let mutations_us = start.elapsed().as_secs_f64() * 1e6;
        let prior = METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            baseline, prior,
            "setter threads performed hierarchy captures"
        );
        let start = std::time::Instant::now();
        flush_metal_scenes();
        let one_us = start.elapsed().as_secs_f64() * 1e6;
        let after = METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed);
        assert!(after - prior <= crate::window::all().len());
        let start = std::time::Instant::now();
        for _ in 0..128 {
            let _ = capture_metal_scene(sdl, darwin::mach::media_time());
        }
        let naive_us = start.elapsed().as_secs_f64() * 1e6 / 128.0;
        let start = std::time::Instant::now();
        for _ in 0..2048 {
            flush_metal_scenes();
        }
        let clean_us = start.elapsed().as_secs_f64() * 1e6 / 2048.0;
        eprintln!(
            "AppKit 128 metal-layer/2048 setter benchmark: queued-mutations-total-us={mutations_us:.1} one-pump-us={one_us:.1} forced-capture-mean-us={naive_us:.1} idle-clean-pump-mean-us={clean_us:.3} captures-per-transaction={}",
            after - prior
        );
        assert_eq!(metal_scene(sdl).unwrap().len(), 128);
        objc::arc::release(window);
    }

    /// A subview's rectangle in the pixels of the window it is in, which is
    /// what presentation blits a `CAMetalLayer`'s drawable into.
    ///
    /// Needs a real window, because the height to flip against and the
    /// pixel density to scale by are SDL's answers and not this crate's, so
    /// it is skipped where no window can be made.
    #[test]
    fn a_subviews_frame_reaches_the_window_in_pixels() {
        crate::bootstrap();
        // SDL belongs to the video thread. Concurrent window tests can claim
        // it first; on another thread the presentation cache cannot be built.
        // Run this graphical integration test in a single-threaded harness.
        if !sdl3::may_pump() {
            return;
        }
        let window =
            objc::object::create_instance(objc::class::lookup(c"NSWindow").expect("NSWindow"), 0);
        // SAFETY: `-initWithContentRect:styleMask:backing:defer:` is
        // `id (id, SEL, NSRect, NSUInteger, NSUInteger, BOOL)`.
        let made = unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 400.0, 300.0))
        };
        if !made {
            return;
        }
        // SAFETY: a window this test made.
        let sdl = unsafe { crate::window::sdl_window(window) };
        let Some((_, height)) = sdl3::video::window_size(sdl) else {
            return;
        };
        let density = sdl3::video::window_pixel_density(sdl) as f64;

        let content = view(rect(0.0, 0.0, f64::from(height), f64::from(height)));
        // SAFETY: both are objects this runtime allocated.
        unsafe { set_window(content, window) };
        let child = view(rect(50.0, 40.0, 100.0, 80.0));
        // SAFETY: `-addSubview:` is `void (id, SEL, id)`.
        unsafe { add_subview(content, objc::sel::get("addSubview:"), child) };

        // SAFETY: a view of this crate's.
        let frame = unsafe { presentation_frame(child) }.expect("the view is in a window");
        assert_eq!(frame.window, sdl);
        // AppKit counts the origin up from the bottom of the content rect;
        // the answer counts down from the top of it.
        assert_eq!(frame.x, (50.0 * density).round() as i32);
        assert_eq!(
            frame.y,
            ((f64::from(height) - (40.0 + 80.0)) * density).round() as i32
        );
        assert_eq!(frame.width, (100.0 * density).round() as u32);
        assert_eq!(frame.height, (80.0 * density).round() as u32);
        // A subview of that size is not the window.
        assert!(!frame.covers_window());

        // A view the size of the window is, which is what puts a game's one
        // layer on the direct path.
        let whole = view(rect(0.0, 0.0, 400.0, f64::from(height)));
        unsafe { add_subview(content, objc::sel::get("addSubview:"), whole) };
        // SAFETY: a view of this crate's.
        let frame = unsafe { presentation_frame(whole) }.expect("the view is in a window");
        assert!(frame.covers_window());
    }

    /// A zero-sized view is fitted to its window's drawable with exactly the
    /// canonical rounded edges, including the content inset. No alternative
    /// origin/width rounding is permitted in the fallback.
    #[test]
    fn zero_sized_presentation_fallback_preserves_fractional_surface_edges() {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSWindow").unwrap();
        let window = objc::object::create_instance(class, 0);
        let made = unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 320.0, 240.0))
        };
        if !made {
            return;
        }
        // Only the SDL video thread can build the presentation cache.
        if !sdl3::video::is_video_thread() {
            return;
        }
        let child = view(rect(0.0, 0.0, 0.0, 0.0));
        unsafe { set_window(child, window) };
        let sdl = unsafe { crate::window::sdl_window(window) };
        let rounded = unsafe { presentation_frame(child) }.unwrap();
        let surface = crate::window::surface_frame(window).unwrap();
        let inset = crate::window::content_inset(window).min(surface.size.height);
        let expected = crate::window::pixel_rect_from_surface(
            NSRect {
                origin: NSPoint {
                    x: surface.origin.x,
                    y: surface.origin.y + inset,
                },
                size: NSSize {
                    width: surface.size.width,
                    height: (surface.size.height - inset).max(0.0),
                },
            },
            sdl3::video::window_size(sdl).unwrap(),
            sdl3::video::window_size_in_pixels(sdl).unwrap(),
        );
        assert_eq!(
            (rounded.x, rounded.y, rounded.width, rounded.height),
            (expected.x, expected.y, expected.w as u32, expected.h as u32)
        );
        objc::arc::release(child);
        objc::arc::release(window);
    }

    /// A view with no window has nowhere to be presented, which is what
    /// sends a drawable down the readback path instead.
    #[test]
    fn a_view_outside_any_window_has_no_presentation_frame() {
        let orphan = view(rect(0.0, 0.0, 100.0, 100.0));
        // SAFETY: a view of this crate's.
        assert!(unsafe { presentation_frame(orphan) }.is_none());
        // SAFETY: null is one of the values this accepts.
        assert!(unsafe { presentation_frame(std::ptr::null_mut()) }.is_none());
    }

    #[test]
    fn clips_to_bounds_round_trips_and_syncs_to_layer() {
        let view = view(rect(0.0, 0.0, 10.0, 10.0));
        let set_clips = objc::sel::get("setClipsToBounds:");
        let get_clips = objc::sel::get("clipsToBounds");
        let sel_wants_layer = objc::sel::get("setWantsLayer:");
        unsafe {
            assert_eq!(clips_to_bounds(view, get_clips), 0);
            set_clips_to_bounds(view, set_clips, 1);
            assert_eq!(clips_to_bounds(view, get_clips), 1);
            set_wants_layer(view, sel_wants_layer, 1);
            let layer = view_of(view).layer;
            assert!(!layer.is_null());
            let get_masks = objc::sel::get("masksToBounds");
            let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
            assert_eq!(f(layer, get_masks), 1);

            set_clips_to_bounds(view, set_clips, 0);
            assert_eq!(clips_to_bounds(view, get_clips), 0);
            assert_eq!(f(layer, get_masks), 0);
        }
        objc::arc::release(view);
    }

    #[test]
    fn animated_ca_ancestor_position_and_bounds_republish_static_metal_clipping() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 120.0, 90.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 120.0, 90.0));
        let metal_view = view(rect(0.0, 0.0, 120.0, 90.0));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let ca = objc::class::lookup(c"CALayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(ca.metaclass().method(maker).unwrap().implementation()) };
        let root = unsafe { make(ca as *const _ as Id, maker) };
        let mask = unsafe { make(ca as *const _ as Id, maker) };
        let metal_cls = objc::class::lookup(c"CAMetalLayer").unwrap();
        let make_metal: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = unsafe {
            std::mem::transmute(
                metal_cls
                    .metaclass()
                    .method(maker)
                    .unwrap()
                    .implementation(),
            )
        };
        let metal = unsafe { make_metal(metal_cls as *const _ as Id, maker) };
        let set_rect: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mask_clip: unsafe extern "C" fn(Id, objc::sel::Sel, u8) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            set_layer(content, objc::sel::get("setLayer:"), root);
            set_layer(metal_view, objc::sel::get("setLayer:"), metal);
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
            set_rect(
                mask,
                objc::sel::get("setFrame:"),
                rect(12.0, 8.0, 42.0, 30.0),
            );
            set_content(root, objc::sel::get("addSublayer:"), mask);
            set_content(mask, objc::sel::get("addSublayer:"), metal);
            mask_clip(mask, objc::sel::get("setMasksToBounds:"), 1);
        }
        flush_metal_scenes();
        let clip = || {
            metal_scene(sdl)
                .unwrap()
                .into_iter()
                .find(|source| source.layer == metal as usize)
                .and_then(|source| source.clip)
                .expect("animated middle CA mask clipped static Metal")
        };
        let model = clip();
        assert!(model.width > 0 && model.height > 0);
        let anim_cls = objc::class::lookup(c"CABasicAnimation").unwrap();
        let animate: unsafe extern "C" fn(Id, objc::sel::Sel, Id) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_obj: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let set_f64: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let add_animation: unsafe extern "C" fn(Id, objc::sel::Sel, Id, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let number = |value: f64| {
            corefoundation::collections::new_number(corefoundation::collections::Number::Float(
                value,
            )) as Id
        };
        let animate_path = |path: &str, from: f64, to: f64| {
            let key = foundation::string::from_str(path);
            let anim = unsafe {
                animate(
                    anim_cls as *const _ as Id,
                    objc::sel::get("animationWithKeyPath:"),
                    key,
                )
            };
            let start = number(from);
            let end = number(to);
            unsafe {
                set_obj(anim, objc::sel::get("setFromValue:"), start);
                set_obj(anim, objc::sel::get("setToValue:"), end);
                set_f64(anim, objc::sel::get("setDuration:"), 0.30);
                set_f64(
                    anim,
                    objc::sel::get("setBeginTime:"),
                    darwin::mach::media_time() - 0.15,
                );
                add_animation(mask, objc::sel::get("addAnimation:forKey:"), anim, key);
            }
            objc::arc::release(start);
            objc::arc::release(end);
            objc::arc::release(key);
        };
        animate_path("position.x", 33.0, 69.0);
        animate_path("bounds.size.width", 32.0, 70.0);
        assert!(
            !crate::coreanimation::scene_animation_roots(darwin::mach::media_time()).is_empty()
        );
        flush_metal_scenes();
        let middle = clip();
        assert!(
            middle.x != model.x || middle.width != model.width,
            "static Metal clipping did not reflect interpolated CA position/width"
        );
        std::thread::sleep(std::time::Duration::from_millis(30));
        flush_metal_scenes();
        let later = clip();
        assert!(
            middle.x != later.x || middle.width != later.width,
            "static Metal did not receive a subsequent geometry-only CA animation tick"
        );
        std::thread::sleep(std::time::Duration::from_millis(165));
        flush_metal_scenes();
        let final_clip = clip();
        assert_eq!(
            (
                final_clip.x,
                final_clip.y,
                final_clip.width,
                final_clip.height
            ),
            (model.x, model.y, model.width, model.height),
            "finished position/bounds animation did not restore model clip without new drawable"
        );
        objc::arc::release(window);
    }

    #[test]
    fn fully_visible_six_k_cpu_foreground_tiles_above_opaque_metal() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        const WIDTH: usize = 6016;
        const HEIGHT: usize = 3384;
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(
                window,
                rect(0.0, 0.0, WIDTH as f64, HEIGHT as f64),
            )
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, WIDTH as f64, HEIGHT as f64));
        let metal_view = view(rect(0.0, 0.0, WIDTH as f64, HEIGHT as f64));
        let above = view(rect(0.0, 0.0, WIDTH as f64, HEIGHT as f64));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let metal_cls = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = unsafe {
            std::mem::transmute(
                metal_cls
                    .metaclass()
                    .method(maker)
                    .unwrap()
                    .implementation(),
            )
        };
        let metal = unsafe { make(metal_cls as *const _ as Id, maker) };
        unsafe {
            set_layer(metal_view, objc::sel::get("setLayer:"), metal);
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
            add_subview(content, objc::sel::get("addSubview:"), above);
        }
        crate::backing::set_native_view_pixels_for_tests(
            above,
            WIDTH as u32,
            HEIGHT as u32,
            [0u8, 0, 255, 255].repeat(WIDTH * HEIGHT),
        );
        assert!(!crate::backing::native_scene_extent_supported(
            WIDTH as u32,
            HEIGHT as u32
        ));
        flush_metal_scenes();
        let published = metal_scene(sdl).expect("Metal/native scene");
        let tiles: Vec<_> = published
            .iter()
            .filter(|p| p.native_owner == above as usize)
            .collect();
        assert!(
            tiles.len() >= 6,
            "large visible native view is missing its tiled scene: {} tiles",
            tiles.len()
        );
        assert_eq!(published[0].layer, metal as usize);
        let mut ids = std::collections::HashSet::new();
        let mut total = 0u64;
        for tile in &tiles {
            assert!(ids.insert(tile.layer), "two tiles shared one GPU layer ID");
            assert!(tile.native_pixels.as_ref().unwrap().len() <= 64 * 1024 * 1024);
            assert_eq!(
                tile.native_pixels.as_ref().unwrap().len(),
                tile.frame.width as usize * tile.frame.height as usize * 4
            );
            assert_eq!(
                &tile.native_pixels.as_ref().unwrap()[..4],
                &[0, 0, 255, 255]
            );
            total += u64::from(tile.frame.width) * u64::from(tile.frame.height);
        }
        assert_eq!(
            total,
            (WIDTH * HEIGHT) as u64,
            "tile coverage had a hole or overlap"
        );
        assert!(
            crate::backing::gpu_under_metal_cached(
                sdl,
                &crate::backing::Store::new(WIDTH as u32, HEIGHT as u32)
            )
            .is_none(),
            "unadmitted large foreground was removed from flattened backup"
        );
        let admitted: Vec<_> = tiles.iter().map(|p| (p.layer, p.native_revision)).collect();
        crate::backing::set_native_gpu_admission(sdl, &admitted[..admitted.len() - 1]);
        assert!(
            crate::backing::gpu_under_metal_cached(
                sdl,
                &crate::backing::Store::new(WIDTH as u32, HEIGHT as u32)
            )
            .is_none(),
            "one accepted tile prematurely excluded the whole native view"
        );
        crate::backing::set_native_gpu_admission(sdl, &admitted);
        let underlay = crate::backing::gpu_under_metal_cached(
            sdl,
            &crate::backing::Store::new(WIDTH as u32, HEIGHT as u32),
        )
        .expect("all tile revisions were admitted");
        assert_eq!(underlay.pixels.len(), WIDTH * HEIGHT * 4);
        let first_capture = crate::backing::native_render_count_for_tests();
        let old: Vec<_> = tiles
            .iter()
            .map(|p| p.native_pixels.as_ref().unwrap().clone())
            .collect();
        for _ in 0..3 {
            invalidate_metal_scene_for_window(sdl);
            flush_metal_scenes();
            let current = metal_scene(sdl).unwrap();
            let actual: Vec<_> = current
                .iter()
                .filter(|p| p.native_owner == above as usize)
                .collect();
            assert_eq!(actual.len(), old.len());
            for (tile, pixels) in actual.iter().zip(old.iter()) {
                assert!(
                    std::sync::Arc::ptr_eq(tile.native_pixels.as_ref().unwrap(), pixels),
                    "unchanged 6K tile rerendered instead of using retained Arc"
                );
            }
        }
        assert_eq!(
            crate::backing::native_render_count_for_tests(),
            first_capture
        );
        // A DMA-BUF overlay is layered over, not substituted for, the 6K
        // CPU view. No GPU admission may erase CPU content until all six
        // *mixed* source revisions have been accepted, not a DMA-only tile.
        crate::backing::set_overlay(
            above,
            crate::backing::Overlay {
                width: WIDTH as u32,
                height: HEIGHT as u32,
                picture: crate::backing::Picture::Dmabuf(std::sync::Arc::new(
                    crate::backing::Dmabuf {
                        width: WIDTH as u32,
                        height: HEIGHT as u32,
                        fourcc: 0,
                        modifier: 0,
                        planes: vec![],
                    },
                )),
            },
        );
        flush_metal_scenes();
        let mixed_scene = metal_scene(sdl).unwrap();
        let mixed: Vec<_> = mixed_scene
            .iter()
            .filter(|source| source.native_owner == above as usize)
            .collect();
        assert_eq!(
            mixed.len(),
            6,
            "mixed CPU/DMA foreground lost bounded tiles"
        );
        let original = unsafe { presentation_frame(above) }.unwrap();
        for tile in &mixed {
            assert!(
                tile.native_pixels.is_some(),
                "DMA wrongly replaced CPU background"
            );
            assert!(tile.native_dmabuf.is_some());
            let (fw, fh, x, y) = tile
                .native_dma_sample
                .expect("tile producer sampling transform");
            assert_eq!((fw, fh), (WIDTH as u32, HEIGHT as u32));
            assert_eq!(
                (x as i64, y as i64),
                (
                    i64::from(tile.frame.x) - i64::from(original.x),
                    i64::from(tile.frame.y) - i64::from(original.y)
                )
            );
        }
        let accepted: Vec<_> = mixed.iter().map(|s| (s.layer, s.native_revision)).collect();
        crate::backing::set_native_gpu_admission(sdl, &accepted[..5]);
        assert!(
            crate::backing::gpu_under_metal(
                sdl,
                &crate::backing::Store::new(WIDTH as u32, HEIGHT as u32)
            )
            .is_none(),
            "partial GPU acceptance removed mixed CPU backing below opaque Metal"
        );
        crate::backing::set_native_gpu_admission(sdl, &accepted);
        assert!(
            crate::backing::gpu_under_metal_cached(
                sdl,
                &crate::backing::Store::new(WIDTH as u32, HEIGHT as u32)
            )
            .is_some(),
            "all mixed native source tiles were admitted but no underlay was produced"
        );
        crate::backing::remove_overlay(above);
        crate::backing::forget(above);
        objc::arc::release(window);
    }

    #[test]
    fn oversized_native_cpu_foreground_crops_visible_pixels_above_metal() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 320.0, 240.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 320.0, 240.0));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let metal_view = view(rect(0.0, 0.0, 320.0, 240.0));
        let oversized = view(rect(-43.0, -27.0, 6016.0, 3384.0));
        let cls = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(cls.metaclass().method(maker).unwrap().implementation()) };
        let metal = unsafe { make(cls as *const _ as Id, maker) };
        unsafe {
            set_layer(metal_view, objc::sel::get("setLayer:"), metal);
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
            add_subview(content, objc::sel::get("addSubview:"), oversized);
        }
        let width = 6016usize;
        let height = 3384usize;
        let mut pixels = vec![0u8; width * height * 4];
        for row in 0..height {
            for column in 0..width {
                let at = (row * width + column) * 4;
                pixels[at..at + 4].copy_from_slice(&[
                    (column % 256) as u8,
                    (row % 256) as u8,
                    220,
                    255,
                ]);
            }
        }
        crate::backing::set_native_view_pixels_for_tests(
            oversized,
            width as u32,
            height as u32,
            pixels,
        );
        flush_metal_scenes();
        accept_published_native_for_tests(sdl);
        let scene = metal_scene(sdl).unwrap();
        assert_eq!(scene.len(), 2, "oversize view disappeared beneath Metal");
        assert_eq!(scene[0].layer, metal as usize);
        assert_eq!(
            scene[1].layer,
            (oversized as usize) | (1usize << (usize::BITS - 1))
        );
        let frame = scene[1].frame;
        let logical = unsafe { presentation_frame(oversized) }.unwrap();
        assert_eq!((frame.x, frame.y), (0, 0));
        assert!(frame.width <= frame.window_width && frame.height <= frame.window_height);
        let crop_x = (i64::from(frame.x) - i64::from(logical.x)) as usize;
        let crop_y = (i64::from(frame.y) - i64::from(logical.y)) as usize;
        let source = scene[1].native_pixels.as_ref().unwrap();
        assert_eq!(
            source.len(),
            frame.width as usize * frame.height as usize * 4,
            "full 6K source was copied instead of bounded visible crop"
        );
        assert_eq!(
            &source[..4],
            &[(crop_x % 256) as u8, (crop_y % 256) as u8, 220, 255]
        );
        let at = (20 * frame.width as usize + 15) * 4;
        assert_eq!(
            &source[at..at + 4],
            &[
                ((crop_x + 15) % 256) as u8,
                ((crop_y + 20) % 256) as u8,
                220,
                255
            ]
        );
        assert!(
            crate::backing::gpu_under_metal(sdl, &crate::backing::Store::new(320, 240)).is_some()
        );
        // A move without new pixels has to recapture the different viewport.
        unsafe {
            set_frame_origin(
                oversized,
                objc::sel::get("setFrameOrigin:"),
                NSPoint { x: -58.0, y: -47.0 },
            )
        };
        flush_metal_scenes();
        let next = metal_scene(sdl).unwrap();
        let moved = unsafe { presentation_frame(oversized) }.unwrap();
        let cropped = next[1].frame;
        let moved_x = (i64::from(cropped.x) - i64::from(moved.x)) as usize;
        let moved_y = (i64::from(cropped.y) - i64::from(moved.y)) as usize;
        assert_eq!(
            &next[1].native_pixels.as_ref().unwrap()[..4],
            &[(moved_x % 256) as u8, (moved_y % 256) as u8, 220, 255]
        );
        assert_ne!((crop_x, crop_y), (moved_x, moved_y));
        crate::backing::forget(oversized);
        objc::arc::release(window);
    }

    #[test]
    fn changing_one_native_view_does_not_recapture_other_windows() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let cls = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(cls.metaclass().method(maker).unwrap().implementation()) };
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut windows = Vec::new();
        let mut natives = Vec::new();
        let mut sdl = Vec::new();
        for _ in 0..2 {
            let window =
                objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
            if !unsafe {
                crate::window::init_with_content_rect_for_tests(
                    window,
                    rect(0.0, 0.0, 160.0, 120.0),
                )
            } {
                for old in windows {
                    objc::arc::release(old);
                }
                return;
            }
            let content = view(rect(0.0, 0.0, 160.0, 120.0));
            unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
            let metal_view = view(rect(0.0, 0.0, 160.0, 120.0));
            let native = view(rect(10.0, 10.0, 60.0, 40.0));
            unsafe {
                set_layer(
                    metal_view,
                    objc::sel::get("setLayer:"),
                    make(cls as *const _ as Id, maker),
                );
                add_subview(content, objc::sel::get("addSubview:"), metal_view);
                add_subview(content, objc::sel::get("addSubview:"), native);
            }
            crate::backing::set_native_view_pixels_for_tests(
                native,
                60,
                40,
                [0, 0, 255, 255].repeat(60 * 40),
            );
            windows.push(window);
            natives.push(native);
            sdl.push(unsafe { crate::window::sdl_window(window) });
        }
        flush_metal_scenes();
        assert_eq!(metal_scene(sdl[0]).unwrap().len(), 2);
        assert_eq!(metal_scene(sdl[1]).unwrap().len(), 2);
        for n in 0..12 {
            let old = METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed);
            crate::backing::set_native_view_pixels_for_tests(
                natives[0],
                60,
                40,
                [n as u8, 0, 255, 255].repeat(60 * 40),
            );
            flush_metal_scenes();
            assert_eq!(
                METAL_SCENE_CAPTURES.load(std::sync::atomic::Ordering::Relaxed) - old,
                1,
                "native mutation in window A recaptured unrelated window B"
            );
        }
        for native in natives {
            crate::backing::forget(native);
        }
        for window in windows {
            objc::arc::release(window);
        }
    }

    #[test]
    fn moved_static_dma_source_sampling_must_invalidate_identical_output_frame() {
        let mut old = MetalSceneLayer {
            opacity_groups: Vec::new(),
            layer: 7,
            frame: Frame {
                window: std::ptr::null_mut(),
                x: 0,
                y: 0,
                width: 160,
                height: 120,
                window_width: 160,
                window_height: 120,
            },
            native_owner: 3,
            opacity: 1.0,
            opaque: false,
            clip: None,
            rounded: vec![],
            occluded: vec![],
            native_pixels: None,
            native_cpu_capture: None,
            native_dmabuf: Some(std::sync::Arc::new(crate::backing::Dmabuf {
                width: 6016,
                height: 3384,
                fourcc: 0,
                modifier: 0,
                planes: vec![],
            })),
            native_dma_sample: Some((6016, 3384, 5500, 3200)),
            native_revision: 88,
        };
        let previous = vec![old.clone()];
        old.native_dma_sample = Some((6016, 3384, 5800, 3200));
        assert!(
            !same_metal_scene(&previous, &[old]),
            "moving a static producer behind the same visible window must publish a new source"
        );
    }

    #[test]
    fn independent_dma_overlays_preserve_view_ownership_and_oversize_fallback() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 320.0, 240.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 320.0, 240.0));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let below = view(rect(0.0, 0.0, 100.0, 100.0));
        let metal_view = view(rect(0.0, 0.0, 320.0, 240.0));
        let above = view(rect(20.0, 20.0, 100.0, 100.0));
        let huge = view(rect(0.0, 0.0, 6016.0, 3384.0));
        let metal_class = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = unsafe {
            std::mem::transmute(
                metal_class
                    .metaclass()
                    .method(maker)
                    .unwrap()
                    .implementation(),
            )
        };
        let metal = unsafe { make(metal_class as *const _ as Id, maker) };
        unsafe {
            set_layer(metal_view, objc::sel::get("setLayer:"), metal);
            add_subview(content, objc::sel::get("addSubview:"), below);
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
            add_subview(content, objc::sel::get("addSubview:"), above);
            add_subview(content, objc::sel::get("addSubview:"), huge);
        }
        // The regular presenter only collects overlays from shown windows.
        let front: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { front(window, objc::sel::get("orderFront:"), std::ptr::null_mut()) };
        let shared = std::sync::Arc::new(crate::backing::Dmabuf {
            width: 100,
            height: 100,
            fourcc: 0,
            modifier: 0,
            planes: vec![],
        });
        for native in [below, above, huge] {
            crate::backing::set_overlay(
                native,
                crate::backing::Overlay {
                    width: 100,
                    height: 100,
                    picture: crate::backing::Picture::Dmabuf(std::sync::Arc::clone(&shared)),
                },
            );
        }
        flush_metal_scenes();
        assert!(
            crate::backing::native_dma_above_metal(sdl).is_empty(),
            "unadmitted DMA source must remain in original backing overlay path"
        );
        accept_published_native_for_tests(sdl);
        let scene = metal_scene(sdl).unwrap();
        assert_eq!(
            scene
                .iter()
                .filter(|layer| layer.native_dmabuf.is_some())
                .count(),
            2
        );
        assert!(
            scene
                .iter()
                .any(|layer| layer.layer == ((above as usize) | (1usize << (usize::BITS - 1))))
        );
        let admitted = crate::backing::native_dma_above_metal(sdl);
        assert_eq!(
            admitted,
            [above as usize, huge as usize],
            "both the small upper overlay and 6K DMA-only view are independent"
        );
        assert!(!crate::backing::native_scene_extent_supported(6016, 3384));
        assert!(crate::backing::native_scene_dma_extent_supported(
            6016, 3384
        ));
        assert!(!crate::backing::native_scene_dma_extent_supported(
            9000, 9000
        ));
        let overlays = crate::backing::gpu_overlay_instances_for_tests(
            sdl,
            &crate::backing::Store::new(320, 240),
        );
        assert_eq!(overlays.len(), 3);
        assert_eq!(
            overlays
                .iter()
                .map(|overlay| overlay.view)
                .collect::<Vec<_>>(),
            [below as usize, above as usize, huge as usize]
        );
        assert!(
            overlays
                .iter()
                .all(|overlay| std::sync::Arc::ptr_eq(&overlay.picture, &shared))
        );
        // A 6K DMA source is accepted without a full CPU allocation and
        // must remain an independent top layer rather than being flattened.
        assert!(
            crate::backing::gpu_under_metal(sdl, &crate::backing::Store::new(320, 240)).is_some()
        );
        // An extreme geometry unsupported by the bounded GPU importer must
        // remain in the original backing overlay path, never be excluded.
        let set_size: unsafe extern "C" fn(Id, objc::sel::Sel, NSSize) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            set_size(
                huge,
                objc::sel::get("setFrameSize:"),
                NSSize {
                    width: 9000.0,
                    height: 9000.0,
                },
            )
        };
        flush_metal_scenes();
        assert_eq!(
            crate::backing::native_dma_above_metal(sdl),
            [above as usize]
        );
        assert!(
            crate::backing::gpu_under_metal(sdl, &crate::backing::Store::new(320, 240)).is_none()
        );
        for native in [below, above, huge] {
            crate::backing::remove_overlay(native);
        }
        objc::arc::release(window);
    }

    #[test]
    fn flattened_root_ca_opacity_animation_refreshes_underlay_and_final_tick() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 160.0, 120.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 160.0, 120.0));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let back = view(rect(0.0, 0.0, 160.0, 120.0));
        let middle = view(rect(0.0, 0.0, 160.0, 120.0));
        let front = view(rect(16.0, 18.0, 48.0, 32.0));
        let ca = objc::class::lookup(c"CALayer").unwrap();
        let metal = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(ca.metaclass().method(maker).unwrap().implementation()) };
        let make_metal: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = unsafe {
            std::mem::transmute(metal.metaclass().method(maker).unwrap().implementation())
        };
        let back_layer = unsafe { make(ca as *const _ as Id, maker) };
        crate::layer::set_bounds_of(back_layer, bounds_of(back));
        unsafe {
            set_layer(back, objc::sel::get("setLayer:"), back_layer);
            set_layer(
                middle,
                objc::sel::get("setLayer:"),
                make_metal(metal as *const _ as Id, maker),
            );
            add_subview(content, objc::sel::get("addSubview:"), back);
            add_subview(content, objc::sel::get("addSubview:"), middle);
            add_subview(content, objc::sel::get("addSubview:"), front);
        }
        crate::backing::set_native_view_pixels_for_tests(
            front,
            48,
            32,
            [0, 255, 0, 255].repeat(48 * 32),
        );
        flush_metal_scenes();
        accept_published_native_for_tests(sdl);
        let flat = crate::backing::Store::new(160, 120);
        crate::coreanimation::set_underlay_animation_for_test(back_layer, None);
        let original = crate::backing::gpu_under_metal_cached(sdl, &flat).unwrap();
        let sample = (64 * original.width as usize + 100) * 4;
        let initial = &original.pixels[sample..sample + 4];
        assert_eq!(initial[2], 255);
        for fraction in [0.2, 0.5, 0.8] {
            crate::coreanimation::set_underlay_animation_for_test(back_layer, Some(fraction));
            assert!(crate::coreanimation::flattened_underlay_content_animating(
                back_layer,
                darwin::mach::media_time()
            ));
            assert!(
                !crate::coreanimation::native_scene_content_animating(
                    back_layer,
                    darwin::mach::media_time()
                ),
                "foreground native captures must still exclude root-opacity-only animation"
            );
            let redraw_before = crate::backing::gpu_underlay_render_count_for_tests();
            let updated = crate::backing::gpu_under_metal_cached(sdl, &flat).unwrap();
            assert!(!std::sync::Arc::ptr_eq(&original, &updated));
            assert!(crate::backing::gpu_underlay_render_count_for_tests() > redraw_before);
            let red = updated.pixels[sample + 2];
            assert!(
                red > 0 && red < 255,
                "interpolated background opacity missing: {red}"
            );
            assert!(crate::backing::underlay_animation_tick(
                sdl,
                darwin::mach::media_time()
            ));
            flush_metal_scenes();
        }
        crate::coreanimation::set_underlay_animation_for_test(back_layer, None);
        // End-of-animation model opacity is restored, even if the previous
        // content epoch and the static geometry remain unchanged.
        assert!(crate::backing::underlay_animation_tick(
            sdl,
            darwin::mach::media_time()
        ));
        let final_pixels = crate::backing::gpu_under_metal_cached(sdl, &flat).unwrap();
        assert_eq!(final_pixels.pixels[sample + 2], 255);
        assert!(!std::sync::Arc::ptr_eq(&original, &final_pixels));
        assert_eq!(
            crate::backing::gpu_under_metal_cached(sdl, &flat)
                .unwrap()
                .pixels,
            final_pixels.pixels
        );
        objc::arc::release(window);
    }

    #[test]
    fn destroyed_view_only_evicts_its_window_underlay() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let metal = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = unsafe {
            std::mem::transmute(metal.metaclass().method(maker).unwrap().implementation())
        };
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut windows = Vec::new();
        let mut sdls = Vec::new();
        let mut transient = Vec::new();
        for _ in 0..2 {
            let window =
                objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
            if !unsafe {
                crate::window::init_with_content_rect_for_tests(
                    window,
                    rect(0.0, 0.0, 160.0, 120.0),
                )
            } {
                for old in windows {
                    objc::arc::release(old);
                }
                return;
            }
            let sdl = unsafe { crate::window::sdl_window(window) };
            let content = view(rect(0.0, 0.0, 160.0, 120.0));
            unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
            let beneath = view(rect(5.0, 6.0, 70.0, 40.0));
            let metal_view = view(rect(0.0, 0.0, 160.0, 120.0));
            let above = view(rect(10.0, 10.0, 40.0, 30.0));
            unsafe {
                set_layer(
                    metal_view,
                    objc::sel::get("setLayer:"),
                    make(metal as *const _ as Id, maker),
                );
                add_subview(content, objc::sel::get("addSubview:"), beneath);
                add_subview(content, objc::sel::get("addSubview:"), metal_view);
                add_subview(content, objc::sel::get("addSubview:"), above);
            }
            crate::backing::set_native_view_pixels_for_tests(
                above,
                40,
                30,
                [0, 0, 255, 255].repeat(40 * 30),
            );
            windows.push(window);
            sdls.push(sdl);
            transient.push(beneath);
        }
        flush_metal_scenes();
        for &sdl in &sdls {
            accept_published_native_for_tests(sdl);
        }
        let flat = crate::backing::Store::new(160, 120);
        let first = crate::backing::gpu_under_metal_cached(sdls[0], &flat).unwrap();
        let second = crate::backing::gpu_under_metal_cached(sdls[1], &flat).unwrap();
        let before = crate::backing::gpu_underlay_render_count_for_tests();
        // Simulate the exact deallocation invalidation before Objective-C
        // recycles an address with the same initial backing generation.
        crate::backing::forget(transient[0]);
        let unchanged = crate::backing::gpu_under_metal_cached(sdls[1], &flat).unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&second, &unchanged),
            "destroying window A view evicted window B cached backing"
        );
        assert_eq!(
            crate::backing::gpu_underlay_render_count_for_tests(),
            before
        );
        let recreated = crate::backing::gpu_under_metal_cached(sdls[0], &flat).unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&first, &recreated),
            "view address reuse must not preserve its former cached underlay"
        );
        for window in windows {
            objc::arc::release(window);
        }
    }

    #[test]
    fn native_foreground_revision_changes_require_gpu_acknowledgment() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 3840.0, 2160.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 3840.0, 2160.0));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let metal_view = view(rect(0.0, 0.0, 3840.0, 2160.0));
        let foreground = view(rect(40.0, 35.0, 30.0, 25.0));
        let metal_class = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id = unsafe {
            std::mem::transmute(
                metal_class
                    .metaclass()
                    .method(maker)
                    .unwrap()
                    .implementation(),
            )
        };
        unsafe {
            set_layer(
                metal_view,
                objc::sel::get("setLayer:"),
                make(metal_class as *const _ as Id, maker),
            );
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
            add_subview(content, objc::sel::get("addSubview:"), foreground);
        }
        let mut flat = crate::backing::Store::new(3840, 2160);
        crate::backing::set_native_view_pixels_for_tests(
            foreground,
            30,
            25,
            [0, 0, 255, 255].repeat(30 * 25),
        );
        flush_metal_scenes();
        accept_published_native_for_tests(sdl);
        let initial =
            crate::backing::gpu_under_metal_cached(sdl, &flat).expect("independent foreground");
        let count = crate::backing::gpu_underlay_render_count_for_tests();
        assert_eq!(initial.pixels.len(), 3840 * 2160 * 4);
        let start = std::time::Instant::now();
        for index in 1..49u8 {
            crate::backing::set_native_view_pixels_for_tests(
                foreground,
                30,
                25,
                [index, 0, 255, 255].repeat(30 * 25),
            );
            flush_metal_scenes();
            // Every draw dirties the *flattened* fallback store.
            flat.pixels[0] = index;
            assert!(
                crate::backing::gpu_under_metal_cached(sdl, &flat).is_none(),
                "unadmitted new view content excluded its own fallback pixels"
            );
            accept_published_native_for_tests(sdl);
            let after = crate::backing::gpu_under_metal_cached(sdl, &flat).unwrap();
            assert!(
                std::sync::Arc::ptr_eq(&initial, &after),
                "revised foreground acknowledgment rebuilt the unchanged lower underlay"
            );
        }
        assert_eq!(
            crate::backing::gpu_underlay_render_count_for_tests(),
            count,
            "48 foreground-only updates allocated new full-window underlays"
        );
        eprintln!(
            "4K underlay: 48 native revisions in {:?}; unchanged lower pixels = {} bytes retained, 0 reconstructed",
            start.elapsed(),
            initial.pixels.len()
        );
        assert_eq!(&initial.pixels[0..4], &[237, 237, 237, 255]);
        crate::backing::set_native_view_pixels_for_tests(
            content,
            3840,
            2160,
            [0, 255, 0, 255].repeat(3840 * 2160),
        );
        flush_metal_scenes();
        let changed = crate::backing::gpu_under_metal_cached(sdl, &flat).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&initial, &changed));
        let offset = (100 * 3840 + 200) * 4;
        assert_eq!(&changed.pixels[offset..offset + 4], &[0, 255, 0, 255]);
        objc::arc::release(window);
    }

    #[test]
    fn native_pixel_cache_does_not_invalidate_from_another_window_ca_tree() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let ca = objc::class::lookup(c"CALayer").unwrap();
        let maker = objc::sel::get("layer");
        let make_ca: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(ca.metaclass().method(maker).unwrap().implementation()) };
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut windows = Vec::new();
        let mut views = Vec::new();
        let mut layers = Vec::new();
        for _ in 0..2 {
            let window =
                objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
            if !unsafe {
                crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 80.0, 60.0))
            } {
                for old in windows {
                    objc::arc::release(old);
                }
                return;
            }
            let content = view(rect(0.0, 0.0, 80.0, 60.0));
            unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
            let native = view(rect(0.0, 0.0, 40.0, 30.0));
            let layer = unsafe { make_ca(ca as *const _ as Id, maker) };
            unsafe {
                set_layer(native, objc::sel::get("setLayer:"), layer);
                add_subview(content, objc::sel::get("addSubview:"), native);
            }
            crate::backing::set_native_view_pixels_for_tests(
                native,
                40,
                30,
                [0, 0, 255, 255].repeat(40 * 30),
            );
            windows.push(window);
            views.push(native);
            layers.push(layer);
        }
        let first = crate::backing::native_scene_pixels(views[0], 40, 30).unwrap();
        let scanned = crate::backing::native_render_count_for_tests();
        for _ in 0..30 {
            crate::coreanimation::changed(layers[1]);
            let next = crate::backing::native_scene_pixels(views[0], 40, 30).unwrap();
            assert!(std::sync::Arc::ptr_eq(&first.0, &next.0));
            assert_eq!(first.1, next.1);
        }
        assert_eq!(crate::backing::native_render_count_for_tests(), scanned);
        crate::coreanimation::changed(layers[0]);
        let updated = crate::backing::native_scene_pixels(views[0], 40, 30).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&first.0, &updated.0));
        for view in views {
            crate::backing::forget(view);
        }
        for window in windows {
            objc::arc::release(window);
        }
    }

    #[test]
    fn intermediate_ca_superlayer_mask_updates_static_metal_scene() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0.0, 0.0, 320.0, 240.0))
        } {
            return;
        }
        let sdl = unsafe { crate::window::sdl_window(window) };
        let content = view(rect(0.0, 0.0, 320.0, 240.0));
        let set_content: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let metal_view = view(rect(0.0, 0.0, 320.0, 240.0));
        let ca = objc::class::lookup(c"CALayer").unwrap();
        let metal_class = objc::class::lookup(c"CAMetalLayer").unwrap();
        let maker = objc::sel::get("layer");
        let make: unsafe extern "C" fn(Id, objc::sel::Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let root = unsafe { make(ca as *const _ as Id, maker) };
        let middle = unsafe { make(ca as *const _ as Id, maker) };
        let metal = unsafe { make(metal_class as *const _ as Id, maker) };
        unsafe {
            set_layer(content, objc::sel::get("setLayer:"), root);
            set_layer(metal_view, objc::sel::get("setLayer:"), metal);
            add_subview(content, objc::sel::get("addSubview:"), metal_view);
        }
        let set_frame: unsafe extern "C" fn(Id, objc::sel::Sel, NSRect) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let add_ca: unsafe extern "C" fn(Id, objc::sel::Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let radius: unsafe extern "C" fn(Id, objc::sel::Sel, f64) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            set_frame(
                middle,
                objc::sel::get("setFrame:"),
                rect(30.0, 20.0, 100.0, 90.0),
            );
            add_ca(root, objc::sel::get("addSublayer:"), middle);
            add_ca(middle, objc::sel::get("addSublayer:"), metal);
            radius(middle, objc::sel::get("setCornerRadius:"), 17.0);
        }
        crate::coreanimation::set_masks(middle, true);
        flush_metal_scenes();
        let first = metal_scene(sdl).unwrap();
        let entry = first
            .iter()
            .find(|layer| layer.layer == metal as usize)
            .unwrap();
        assert_eq!(
            entry.rounded.len(),
            1,
            "intermediate CA radius was not inherited"
        );
        let mask = entry.rounded[0];
        assert_eq!(
            (mask.frame.x, mask.frame.width, mask.frame.height),
            (30, 100, 90)
        );
        assert_eq!(mask.radius, 17.0);
        assert!(entry.clip.is_some());
        crate::coreanimation::set_masks(middle, false);
        flush_metal_scenes();
        let unmasked = metal_scene(sdl).unwrap();
        let entry = unmasked
            .iter()
            .find(|layer| layer.layer == metal as usize)
            .unwrap();
        assert!(entry.rounded.is_empty());
        assert!(entry.clip.is_none());
        // Re-masking only changes CA metadata. No guest Metal submission.
        crate::coreanimation::set_masks(middle, true);
        flush_metal_scenes();
        assert_eq!(
            metal_scene(sdl)
                .unwrap()
                .iter()
                .find(|layer| layer.layer == metal as usize)
                .unwrap()
                .rounded
                .len(),
            1
        );
        objc::arc::release(window);
    }
}

#[cfg(test)]
mod native_partial_draw_regression {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect {
            origin: NSPoint { x, y },
            size: NSSize { width, height },
        }
    }

    fn view(frame: NSRect) -> Id {
        unsafe {
            init_with_frame(
                objc::object::create_instance(objc::class::lookup(c"NSView").unwrap(), 0),
                objc::sel::get("initWithFrame:"),
                frame,
            )
        }
    }

    static DRAW_PHASE: AtomicU8 = AtomicU8::new(0);
    static PARTIAL_RECT_OBSERVED: AtomicU8 = AtomicU8::new(0);

    unsafe extern "C" fn paint_native_partial_rect(receiver: Id, _selector: Sel, dirty: NSRect) {
        match DRAW_PHASE.load(Ordering::Relaxed) {
            0 => {
                assert!(crate::backing::draw_focused(|pixels, width, height| {
                    for pixel in pixels.as_chunks_mut::<4>().0.iter_mut() {
                        pixel.copy_from_slice(&[255, 0, 0, 255]);
                    }
                    Some(crate::backing::Rect {
                        x: 0,
                        y: 0,
                        width: width as i64,
                        height: height as i64,
                    })
                }));
            }
            1 => {
                let expected = NSRect {
                    origin: NSPoint { x: 6., y: 19. },
                    size: NSSize {
                        width: 1.,
                        height: 1.,
                    },
                };
                let passed_dirty = dirty == expected;
                let mut rects = std::ptr::null();
                let mut count = 0isize;
                let send: unsafe extern "C" fn(Id, Sel, *mut *const NSRect, *mut isize) =
                    unsafe { std::mem::transmute(objc::msg::entry()) };
                unsafe {
                    send(
                        receiver,
                        objc::sel::get("getRectsBeingDrawn:count:"),
                        &mut rects,
                        &mut count,
                    )
                };
                PARTIAL_RECT_OBSERVED.store(
                    u8::from(
                        passed_dirty
                            && count == 1
                            && !rects.is_null()
                            && unsafe { *rects } == expected,
                    ),
                    Ordering::Relaxed,
                );
                // Draw a full view on purpose: AppKit clips it to the
                // partial invalidation and keeps every outside blue texel.
                crate::backing::paint_coverage(0, 0, 32, &vec![1.0; 32 * 24], [0., 1., 0., 1.]);
            }
            _ => {
                assert!(crate::backing::draw_focused(|pixels, _, _| {
                    pixels[..4].copy_from_slice(&[0, 0, 255, 255]);
                    Some(crate::backing::Rect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    })
                }));
            }
        }
    }

    #[test]
    fn live_draw_rect_partial_invalidation_preserves_old_pixels_and_capture_damage() {
        crate::bootstrap();
        if !sdl3::may_pump() {
            return;
        }
        let window = objc::object::create_instance(objc::class::lookup(c"NSWindow").unwrap(), 0);
        if !unsafe {
            crate::window::init_with_content_rect_for_tests(window, rect(0., 0., 160., 120.))
        } {
            return;
        }
        let content = view(rect(0., 0., 160., 120.));
        let set_content: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { set_content(window, objc::sel::get("setContentView:"), content) };
        let cls = objc::class::lookup(c"RosemaryPartialDrawRectTestView").unwrap_or_else(|| {
            let class = objc::class::allocate_pair(
                objc::class::lookup(c"NSView"),
                c"RosemaryPartialDrawRectTestView",
                0,
            )
            .unwrap();
            assert!(class.add_method(
                objc::sel::get("drawRect:"),
                paint_native_partial_rect as objc::class::Imp,
                c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16"
            ));
            objc::class::register_pair(class);
            class
        });
        let child = unsafe {
            init_with_frame(
                objc::object::create_instance(cls, 0),
                objc::sel::get("initWithFrame:"),
                rect(12., 12., 32., 24.),
            )
        };
        let add_subview: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe { add_subview(content, objc::sel::get("addSubview:"), child) };
        DRAW_PHASE.store(0, Ordering::Relaxed);
        PARTIAL_RECT_OBSERVED.store(0, Ordering::Relaxed);
        crate::backing::display_view(child);
        let crop = crate::backing::Rect {
            x: 0,
            y: 0,
            width: 32,
            height: 24,
        };
        let first =
            crate::backing::native_scene_pixels_cropped_with_damage(child, 32, 24, crop).unwrap();
        assert_eq!(first.2, None);
        DRAW_PHASE.store(1, Ordering::Relaxed);
        let invalidate: unsafe extern "C" fn(Id, Sel, NSRect) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        unsafe {
            invalidate(
                child,
                objc::sel::get("setNeedsDisplayInRect:"),
                NSRect {
                    origin: NSPoint { x: 6., y: 19. },
                    size: NSSize {
                        width: 1.,
                        height: 1.,
                    },
                },
            )
        };
        crate::backing::display_view(child);
        assert_eq!(PARTIAL_RECT_OBSERVED.load(Ordering::Relaxed), 1);
        let next =
            crate::backing::native_scene_pixels_cropped_with_damage(child, 32, 24, crop).unwrap();
        assert_eq!(
            next.2,
            Some(crate::backing::NativeCpuDamage {
                from_revision: first.1,
                rect: crate::backing::Rect {
                    x: 6,
                    y: 4,
                    width: 1,
                    height: 1
                },
            })
        );
        assert_eq!(
            &next.0[(4 * 32 + 6) * 4..(4 * 32 + 7) * 4],
            &[0, 255, 0, 255]
        );
        assert_eq!(&next.0[..4], &[255, 0, 0, 255]);
        assert_eq!(
            &next.0[(4 * 32 + 7) * 4..(4 * 32 + 8) * 4],
            &[255, 0, 0, 255]
        );
        // A raw canvas callback bypasses clipped drawing primitives. Such a
        // callback cannot make an authoritative small-region damage claim.
        DRAW_PHASE.store(2, Ordering::Relaxed);
        unsafe {
            invalidate(
                child,
                objc::sel::get("setNeedsDisplayInRect:"),
                NSRect {
                    origin: NSPoint { x: 6., y: 19. },
                    size: NSSize {
                        width: 1.,
                        height: 1.,
                    },
                },
            )
        };
        crate::backing::display_view(child);
        let opaque =
            crate::backing::native_scene_pixels_cropped_with_damage(child, 32, 24, crop).unwrap();
        assert_eq!(opaque.2.unwrap().rect, crop);
        crate::backing::forget(child);
        objc::arc::release(window);
    }
}
