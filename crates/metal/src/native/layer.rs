//! Native CAMetalLayer and CAMetalDrawable bridge for the built-in guest runtime.
//! One retained host layer per guest layer, with lifetime tied to CALayer dealloc.
//! Cocoa window attachment is deliberately kept separate from basic drawable
//! creation, so an unpresentable/unknown layer does not corrupt an SDL window.

use super::*;
use appkit::view::Frame;
use foundation::geometry::{NSPoint, NSRect, NSSize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

struct HostLayer(usize);

impl HostLayer {
    fn get(&self) -> HostId {
        self.0 as HostId
    }
}

impl Drop for HostLayer {
    fn drop(&mut self) {
        unsafe { host_release(self.get()) };
    }
}

/// SDL view attachment is owned exclusively by the SDL video thread.
/// A host CAMetalLayer remains retained independently of its NSView so a
/// concurrent Metal command can finish after a window has been retired.
#[derive(Clone, Copy)]
struct Attachment {
    window: usize,
    window_id: u32,
    view: usize,
    /// Where the SDL view was last placed, so a moved or resized guest view
    /// is placed again rather than left over its old rectangle.
    placement: Placement,
}

/// A presentation frame's geometry, without its window.
#[derive(Clone, Copy, PartialEq)]
struct Placement {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    window_width: u32,
    window_height: u32,
}

impl From<&Frame> for Placement {
    fn from(frame: &Frame) -> Self {
        Self {
            x: frame.x,
            y: frame.y,
            width: frame.width,
            height: frame.height,
            window_width: frame.window_width,
            window_height: frame.window_height,
        }
    }
}

struct Entry {
    layer: Arc<HostLayer>,
    attached: Option<Attachment>,
}

static HOST_LAYERS: LazyLock<Mutex<HashMap<usize, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Guest CALayers awaiting the video thread, each retained until it is
/// serviced. The registry itself MUST NOT retain the guest, or dealloc would
/// never run.
static ATTACH_REQUESTS: LazyLock<Mutex<Vec<usize>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static PENDING_DESTROYS: LazyLock<Mutex<Vec<Attachment>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

#[cfg(test)]
pub(super) fn tracked_layer_count() -> usize {
    HOST_LAYERS.lock().unwrap_or_else(|e| e.into_inner()).len()
}

pub(super) struct NativeDrawable {
    host: HostId,
    texture: Id,
    layer: Id,
}

pub(super) fn install() {
    // The runtime Metal bootstrap already installed Vulkan-backed nextDrawable
    // on the guest CAMetalLayer. Only the explicit native selection replaces it.
    let layer = objc::class::lookup(c"CAMetalLayer").expect("AppKit CAMetalLayer");
    methods!(layer, {
        "nextDrawable" => next_drawable, c"@16@0:8";
    });
    let drawable = object::register::<NativeDrawable>(c"RosemaryNativeCAMetalDrawable");
    methods!(drawable, {
        "texture" => drawable_texture, c"@16@0:8";
        "layer" => drawable_layer, c"@16@0:8";
        "present" => drawable_present, c"v16@0:8";
        "drawableID" => drawable_id, c"Q16@0:8";
        "dealloc" => drawable_dealloc, c"v16@0:8";
    });
    appkit::layer::add_metal_view_hook(view_changed);
    appkit::layer::add_metal_dealloc_hook(forget);
    appkit::event::add_pump_hook(service_video_thread);
    appkit::event::add_modal_presentation_hook(service_video_thread);
    appkit::window::add_destroy_hook(forget_window);
}

fn forget(guest_layer: Id) {
    let removed = HOST_LAYERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(guest_layer as usize));
    if let Some(attachment) = removed.as_ref().and_then(|entry| entry.attached) {
        PENDING_DESTROYS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(attachment);
    }
    // Releasing a native CALayer can execute host callbacks. Do it outside
    // the map lock so a reentrant nextDrawable cannot deadlock the registry.
    drop(removed);
}

fn view_changed(layer: Id) {
    // The view's old back-reference has already been cleared. Reconcile its
    // attachment even if the guest keeps the previous CALayer alive.
    if !HOST_LAYERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(&(layer as usize))
    {
        return;
    }
    request_attachment(layer);
}

/// Reconcile the layer's SDL attachment. The video thread attaches at once.
/// Any other thread queues the layer for the next pump and keeps drawing to
/// the host layer it has, as a CAMetalLayer not yet on screen still vends
/// drawables; blocking instead could stall a video thread waiting on it.
fn request_attachment(layer: Id) {
    if sdl3::video::is_video_thread() {
        attach_one(layer);
        return;
    }
    let view = unsafe { appkit::layer::view_of(layer) };
    let attached = HOST_LAYERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(layer as usize))
        .and_then(|entry| entry.attached);
    let destination = (!view.is_null())
        .then(|| unsafe { appkit::view::presentation_frame(view) })
        .flatten()
        .and_then(|frame| sdl3::video::live_window_id(frame.window).map(|id| (frame, id)));
    let settled = match (attached, destination) {
        (Some(attached), Some((frame, id))) => {
            attached.window == frame.window as usize
                && attached.window_id == id
                && attached.placement == Placement::from(&frame)
        }
        // Detached and nowhere to go. A windowed view still awaiting its
        // presentation snapshot is not settled: the pump publishes it.
        (None, None) => {
            view.is_null() || {
                let window = crate::present::send_id(view, "window");
                window.is_null() || unsafe { appkit::window::sdl_window(window) }.is_null()
            }
        }
        _ => false,
    };
    if settled {
        return;
    }
    let mut requests = ATTACH_REQUESTS.lock().unwrap_or_else(|e| e.into_inner());
    if !requests.contains(&(layer as usize)) {
        requests.push(objc::arc::retain(layer) as usize);
    }
}

fn destroy_views(views: Vec<Attachment>) {
    debug_assert!(sdl3::video::is_video_thread());
    for item in views {
        // Either the SDL window is still registered, or this runs from the
        // window-destroy hook immediately before SDL_DestroyWindow.
        unsafe { sdl3::video::metal::destroy(item.view as *mut c_void) };
    }
}

fn service_video_thread() {
    if !sdl3::video::is_video_thread() {
        return;
    }
    let deferred = std::mem::take(&mut *PENDING_DESTROYS.lock().unwrap_or_else(|e| e.into_inner()));
    destroy_views(deferred);
    let pending = std::mem::take(&mut *ATTACH_REQUESTS.lock().unwrap_or_else(|e| e.into_inner()));
    for guest in pending {
        attach_one(guest as Id);
        objc::arc::release(guest as Id);
    }
}

fn attach_one(guest: Id) {
    debug_assert!(sdl3::video::is_video_thread());
    let view = unsafe { appkit::layer::view_of(guest) };
    let candidate = if view.is_null() {
        None
    } else {
        unsafe { appkit::view::presentation_frame(view) }.and_then(|frame| {
            if frame.window.is_null() {
                return None;
            }
            let window_id = sdl3::video::live_window_id(frame.window)?;
            Some((frame, window_id))
        })
    };
    // Resolve the layer's currently owning window on the SDL video thread.
    // A layer may move without either window being destroyed.
    let previous = {
        let mut entries = HOST_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = entries.get_mut(&(guest as usize)) else {
            return;
        };
        if let (Some(attached), Some((frame, id))) = (entry.attached.as_mut(), candidate)
            && attached.window == frame.window as usize
            && attached.window_id == id
        {
            let placement = Placement::from(&frame);
            if attached.placement != placement {
                place(attached.view as *mut c_void, &frame);
                attached.placement = placement;
            }
            return;
        }
        entry.attached.take()
    };
    if let Some(old) = previous {
        // Do not keep an SDL view bound to a stale window. The HostLayer Arc
        // remains retained for in-flight drawables while SDL retires its view.
        destroy_views(vec![old]);
    }
    let Some((frame, window_id)) = candidate else {
        return;
    };
    let target_window = frame.window;
    let window = target_window as usize;
    {
        let entries = HOST_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
        if !entries.contains_key(&(guest as usize))
            || entries.values().any(|entry| {
                entry
                    .attached
                    .is_some_and(|attached| attached.window == window)
            })
        {
            return;
        }
    }
    let Some((native_view, native_layer)) = sdl3::video::metal::create(target_window) else {
        gpu::trace("native Metal could not attach SDL Metal view".to_string());
        return;
    };
    place(native_view, &frame);
    let native_host = unsafe { host_retain(native_layer) };
    let attachment = Attachment {
        window,
        window_id,
        view: native_view as usize,
        placement: Placement::from(&frame),
    };
    let replacement = Arc::new(HostLayer(native_host as usize));
    let old = {
        let mut entries = HOST_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
        match entries.get_mut(&(guest as usize)) {
            Some(entry) if entry.attached.is_none() => {
                entry.attached = Some(attachment);
                Some(std::mem::replace(&mut entry.layer, replacement))
            }
            _ => None,
        }
    };
    if old.is_none() {
        destroy_views(vec![attachment]);
    } else {
        gpu::trace(format!(
            "native Metal attached SDL view to guest CAMetalLayer (window {window_id})"
        ));
    }
    drop(old);
}

/// Put SDL's Metal view over the guest view's rectangle. SDL sizes it to the
/// whole content view, which in a titled window also spans the title bar.
fn place(native_view: *mut c_void, frame: &Frame) {
    debug_assert!(sdl3::video::is_video_thread());
    let Some(snapshot) = sdl3::video::presentation_window(frame.window) else {
        return;
    };
    let (points_w, points_h) = snapshot.logical_size;
    let (pixels_w, pixels_h) = snapshot.pixel_size;
    if pixels_w <= 0 || pixels_h <= 0 {
        return;
    }
    let sx = f64::from(points_w) / f64::from(pixels_w);
    let sy = f64::from(points_h) / f64::from(pixels_h);
    let width = f64::from(frame.width) * sx;
    let height = f64::from(frame.height) * sy;
    let top = f64::from(frame.y) * sy;
    let _pool = HostPool::new();
    let view = native_view as HostId;
    let superview = unsafe { host_send_id(view, sel(c"superview")) };
    // The frame counts from the top left; an unflipped superview's origin
    // is its bottom left.
    let flipped = !superview.is_null() && unsafe { host_send_bool(superview, sel(c"isFlipped")) };
    let y = if flipped {
        top
    } else {
        f64::from(points_h) - top - height
    };
    let rect = NSRect {
        origin: NSPoint {
            x: f64::from(frame.x) * sx,
            y,
        },
        size: NSSize { width, height },
    };
    unsafe { host_send_void_rect(view, sel(c"setFrame:"), rect) };
}

fn forget_window(window: sdl3::Window) {
    if !sdl3::video::is_video_thread() {
        return;
    }
    let key = window as usize;
    // AppKit calls this hook before SDL_DestroyWindow while its identity is
    // still readable. A recycled SDL pointer must never destroy a new view.
    let id = sdl3::video::window_id(window);
    let mut retired = Vec::new();
    {
        let mut layers = HOST_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
        for entry in layers.values_mut() {
            if entry
                .attached
                .is_some_and(|a| a.window == key && a.window_id == id)
            {
                if let Some(attachment) = entry.attached.take() {
                    retired.push(attachment);
                }
            }
        }
    }
    let mut pending = PENDING_DESTROYS.lock().unwrap_or_else(|e| e.into_inner());
    let mut i = 0;
    while i < pending.len() {
        if pending[i].window == key && pending[i].window_id == id {
            retired.push(pending.swap_remove(i));
        } else {
            i += 1;
        }
    }
    drop(pending);
    destroy_views(retired);
}

fn cocoa_layer_class() -> HostId {
    static QUARTZ_CORE: OnceLock<usize> = OnceLock::new();
    let handle = *QUARTZ_CORE.get_or_init(|| unsafe {
        libc::dlopen(
            c"/System/Library/Frameworks/QuartzCore.framework/QuartzCore".as_ptr(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        ) as usize
    });
    if handle == 0 {
        gpu::trace("native CAMetalLayer: could not load QuartzCore".to_string());
        return std::ptr::null_mut();
    }
    unsafe { host_get_class(c"CAMetalLayer".as_ptr()) }
}

fn host_layer(guest_layer: Id) -> Option<Arc<HostLayer>> {
    let key = guest_layer as usize;
    if let Some(host) = HOST_LAYERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .map(|entry| Arc::clone(&entry.layer))
    {
        return Some(host);
    }
    // Never construct or release a host Objective-C object with the registry
    // lock held. Cocoa may call back into guest window/layer handling.
    let _pool = HostPool::new();
    let class = cocoa_layer_class();
    if class.is_null() {
        return None;
    }
    let host = unsafe { host_send_id(class, sel(c"new")) };
    if host.is_null() {
        return None;
    }
    let new_host = Arc::new(HostLayer(host as usize));
    let selected = {
        let mut map = HOST_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = map.get(&key) {
            Arc::clone(&entry.layer)
        } else {
            map.insert(
                key,
                Entry {
                    layer: Arc::clone(&new_host),
                    attached: None,
                },
            );
            Arc::clone(&new_host)
        }
    };
    drop(new_host);
    Some(selected)
}

fn guest_device(layer: Id) -> Id {
    let supplied = crate::present::send_id(layer, "device");
    if supplied.is_null() {
        default_device()
    } else if object::is_kind(supplied, c"RosemaryNativeMTLDevice") {
        supplied
    } else {
        gpu::trace("native CAMetalLayer has non-native Metal device".to_string());
        std::ptr::null_mut()
    }
}

fn configure_host(layer: HostId, guest: Id, device: Id) {
    let _pool = HostPool::new();
    let host_device = unsafe { object::body::<NativeDevice>(device) }.host;
    let (width, height) = crate::present::send_size(guest, "drawableSize");
    let mut size = foundation::geometry::NSSize { width, height };
    if size.width <= 0.0 || size.height <= 0.0 {
        let view = unsafe { appkit::layer::view_of(guest) };
        if let Some(frame) = unsafe { appkit::view::presentation_frame(view) } {
            size.width = f64::from(frame.width);
            size.height = f64::from(frame.height);
        }
    }
    if size.width <= 0.0 || size.height <= 0.0 {
        // An unattached layer may remain uninitialized until a guest sets
        // its bounds. Do not force a bogus on-screen extent for it.
        return;
    }
    unsafe {
        host_send_void_id(layer, sel(c"setDevice:"), host_device);
        host_send_void_u64(
            layer,
            sel(c"setPixelFormat:"),
            crate::present::send_u64(guest, "pixelFormat"),
        );
        host_send_void_size(layer, sel(c"setDrawableSize:"), size);
        host_send_void_bool(
            layer,
            sel(c"setFramebufferOnly:"),
            crate::present::send_bool(guest, "framebufferOnly"),
        );
        host_send_void_bool(
            layer,
            sel(c"setDisplaySyncEnabled:"),
            crate::present::send_bool(guest, "displaySyncEnabled"),
        );
    }
}

pub(super) unsafe extern "C" fn next_drawable(receiver: Id, _sel: Sel) -> Id {
    if !object::is_kind(receiver, c"CAMetalLayer") {
        return std::ptr::null_mut();
    }
    let device = guest_device(receiver);
    if device.is_null() {
        return std::ptr::null_mut();
    }
    // The placeholder host layer must exist before attachment can replace
    // it, so that a video-thread first frame already draws to SDL's layer.
    let Some(_) = host_layer(receiver) else {
        return std::ptr::null_mut();
    };
    request_attachment(receiver);
    let Some(layer) = host_layer(receiver) else {
        return std::ptr::null_mut();
    };
    configure_host(layer.get(), receiver, device);
    let _pool = HostPool::new();
    let host_drawable = unsafe { host_send_id(layer.get(), sel(c"nextDrawable")) };
    if host_drawable.is_null() {
        return std::ptr::null_mut();
    }
    let host_drawable = unsafe { host_retain(host_drawable) };
    let host_texture = unsafe { host_send_id(host_drawable, sel(c"texture")) };
    if host_texture.is_null() {
        unsafe { host_release(host_drawable) };
        return std::ptr::null_mut();
    }
    let texture = texture::wrap_host_texture(unsafe { host_retain(host_texture) }, device);
    if texture.is_null() {
        unsafe { host_release(host_drawable) };
        return std::ptr::null_mut();
    }
    objc::arc::retain(receiver);
    let guest = object::make(
        c"RosemaryNativeCAMetalDrawable",
        NativeDrawable {
            host: host_drawable,
            texture,
            layer: receiver,
        },
    );
    if guest.is_null() {
        objc::arc::release(texture);
        objc::arc::release(receiver);
        unsafe { host_release(host_drawable) };
    }
    objc::arc::autorelease(guest)
}

pub(super) unsafe extern "C" fn drawable_texture(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeDrawable>(receiver) }.texture
}

unsafe extern "C" fn drawable_layer(receiver: Id, _sel: Sel) -> Id {
    unsafe { object::body::<NativeDrawable>(receiver) }.layer
}

unsafe extern "C" fn drawable_id(receiver: Id, _sel: Sel) -> u64 {
    let host = unsafe { object::body::<NativeDrawable>(receiver) }.host;
    let _pool = HostPool::new();
    unsafe { host_send_u64(host, sel(c"drawableID")) }
}

unsafe extern "C" fn drawable_present(receiver: Id, _sel: Sel) {
    let host = unsafe { object::body::<NativeDrawable>(receiver) }.host;
    let _pool = HostPool::new();
    unsafe { host_send_void(host, sel(c"present")) };
}

unsafe extern "C" fn drawable_dealloc(receiver: Id, _sel: Sel) {
    let drawable = unsafe { object::body::<NativeDrawable>(receiver) };
    unsafe { host_release(drawable.host) };
    objc::arc::release(drawable.texture);
    objc::arc::release(drawable.layer);
    objc::arc::finalize(receiver);
}

pub(super) unsafe extern "C" fn present_drawable(receiver: Id, _sel: Sel, guest: Id) {
    if !object::is_kind(guest, c"RosemaryNativeCAMetalDrawable") {
        gpu::trace("native Metal presentDrawable requires a native drawable".to_string());
        return;
    }
    let command_device = unsafe { command_device(receiver, std::ptr::null()) };
    let texture = unsafe { object::body::<NativeDrawable>(guest) }.texture;
    if unsafe { object::body::<texture::NativeTexture>(texture) }.device != command_device {
        gpu::trace("native Metal drawable belongs to another device".to_string());
        return;
    }
    let host_drawable = unsafe { object::body::<NativeDrawable>(guest) }.host;
    let host_command = unsafe { object::body::<NativeCommandBuffer>(receiver) }.host;
    let _pool = HostPool::new();
    unsafe { host_send_void_id(host_command, sel(c"presentDrawable:"), host_drawable) };
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn live_guest_metal_view_rebinds_from_first_live_window_to_second() {
        if !super::super::run_native_test_here() {
            return;
        }
        if !sdl3::isolated::run_here() {
            return;
        }
        crate::bootstrap();
        // Install native CAMetalLayer dispatch and AppKit view-change hooks
        // before constructing either presentation layer.
        assert!(!super::super::default_device().is_null());
        if !sdl3::may_pump() {
            return;
        }
        let pool = objc::arc::pool_push();
        let rect = foundation::geometry::NSRect {
            origin: foundation::geometry::NSPoint { x: 0.0, y: 0.0 },
            size: foundation::geometry::NSSize {
                width: 320.0,
                height: 240.0,
            },
        };
        let probe =
            sdl3::video::create_window(c"metal-move-probe", 1, 1, sdl3::video::flags::HIDDEN);
        if probe.is_none() {
            eprintln!(
                "skipped: native SDL video is unavailable in test session: {}",
                sdl3::error()
            );
            objc::arc::pool_pop(pool);
            return;
        }
        let probe = probe.unwrap();
        sdl3::video::destroy_window(probe);
        let win_class = objc::class::lookup(c"NSWindow").unwrap();
        let view_class = objc::class::lookup(c"NSView").unwrap();
        let layer_class = objc::class::lookup(c"CAMetalLayer").unwrap();
        let make_window: unsafe extern "C" fn(
            Id,
            Sel,
            foundation::geometry::NSRect,
            usize,
            usize,
            u8,
        ) -> Id = unsafe { std::mem::transmute(objc::msg::entry()) };
        let make_view: unsafe extern "C" fn(Id, Sel, foundation::geometry::NSRect) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_id: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let mut windows = [std::ptr::null_mut(); 2];
        for window in &mut windows {
            let obj = objc::object::create_instance(win_class, 0);
            *window = unsafe {
                make_window(
                    obj,
                    objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                    rect,
                    0,
                    0,
                    0,
                )
            };
            assert!(!window.is_null());
        }
        let sdl_a = unsafe { appkit::window::sdl_window(windows[0]) };
        let sdl_b = unsafe { appkit::window::sdl_window(windows[1]) };
        assert!(!sdl_a.is_null() && !sdl_b.is_null() && sdl_a != sdl_b);
        assert!(sdl3::video::is_video_thread());
        sdl3::pump();
        let content_a = unsafe { send_id(windows[0], objc::sel::get("contentView")) };
        let content_b = unsafe { send_id(windows[1], objc::sel::get("contentView")) };
        let view = unsafe {
            make_view(
                objc::object::create_instance(view_class, 0),
                objc::sel::get("initWithFrame:"),
                rect,
            )
        };
        let metal =
            unsafe { send_id(objc::class::ptr(layer_class) as Id, objc::sel::get("layer")) };
        assert!(!view.is_null() && !metal.is_null());
        unsafe {
            send_void_id(view, objc::sel::get("setLayer:"), metal);
            send_void_id(content_a, objc::sel::get("addSubview:"), view);
        }
        let before = unsafe { appkit::view::presentation_frame(view) }.unwrap();
        assert_eq!(before.window, sdl_a);
        assert!(before.covers_window());
        let original = host_layer(metal).unwrap();
        request_attachment(metal);
        service_video_thread();
        let first = HOST_LAYERS
            .lock()
            .unwrap()
            .get(&(metal as usize))
            .and_then(|entry| entry.attached)
            .expect("first native Metal attachment");
        assert_eq!(first.window, sdl_a as usize);
        assert_eq!(first.window_id, sdl3::video::window_id(sdl_a));
        let original_host_layer = original.get() as usize;
        // Retain the previous host layer across the swap so its identity
        // cannot be reused; SDL may recycle the view address after destroy.
        unsafe { send_void_id(content_b, objc::sel::get("addSubview:"), view) };
        sdl3::pump();
        let moved = unsafe { appkit::view::presentation_frame(view) }.unwrap();
        assert_eq!(moved.window, sdl_b);
        assert!(moved.covers_window());
        request_attachment(metal);
        service_video_thread();
        let second = HOST_LAYERS
            .lock()
            .unwrap()
            .get(&(metal as usize))
            .and_then(|entry| entry.attached)
            .expect("rebound native Metal attachment");
        assert_eq!(second.window, sdl_b as usize);
        assert_eq!(second.window_id, sdl3::video::window_id(sdl_b));
        assert_ne!(
            host_layer(metal).unwrap().get() as usize,
            original_host_layer,
            "native host layer remained bound to the prior window"
        );
        drop(original);
        assert!(
            sdl3::video::live_window_id(sdl_a).is_some(),
            "window A was destroyed"
        );
        assert!(
            sdl3::video::live_window_id(sdl_b).is_some(),
            "window B was destroyed"
        );
        // Retain A independently of the NSView. Replacing it must free the
        // window's single SDL_MetalView slot without deallocating A.
        objc::arc::retain(metal);
        let replacement_layer =
            unsafe { send_id(objc::class::ptr(layer_class) as Id, objc::sel::get("layer")) };
        assert!(!replacement_layer.is_null());
        unsafe { send_void_id(view, objc::sel::get("setLayer:"), replacement_layer) };
        let detached = HOST_LAYERS
            .lock()
            .unwrap()
            .get(&(metal as usize))
            .and_then(|entry| entry.attached);
        assert!(
            detached.is_none(),
            "retained detached CAMetalLayer A still occupies the window"
        );
        let initial = unsafe { next_drawable(replacement_layer, std::ptr::null()) };
        assert!(
            !initial.is_null(),
            "the first drawable of replacement CAMetalLayer B is unavailable"
        );
        let live = HOST_LAYERS
            .lock()
            .unwrap()
            .get(&(replacement_layer as usize))
            .and_then(|entry| entry.attached)
            .expect("replacement B must attach on the first drawable");
        assert_eq!(live.window, sdl_b as usize);
        assert_eq!(live.window_id, sdl3::video::window_id(sdl_b));
        assert!(sdl3::video::live_window_id(sdl_a).is_some());
        assert!(sdl3::video::live_window_id(sdl_b).is_some());

        // The first frame may originate on the guest's render thread while
        // SDL presentation is owned by the video thread, which may itself be
        // waiting on that render thread. Without any pump, the render thread
        // must still get a drawable, and the next pump attaches the layer.
        let worker_layer =
            unsafe { send_id(objc::class::ptr(layer_class) as Id, objc::sel::get("layer")) };
        assert!(!worker_layer.is_null());
        unsafe { send_void_id(view, objc::sel::get("setLayer:"), worker_layer) };
        let key = worker_layer as usize;
        let worker = std::thread::spawn(move || {
            let _darwin = darwin::tls::DarwinThread::enter();
            let pool = objc::arc::pool_push();
            let initial = unsafe { next_drawable(key as Id, std::ptr::null()) };
            let owned = objc::arc::retain(initial) as usize;
            objc::arc::pool_pop(pool);
            owned
        });
        let initial = worker.join().unwrap() as Id;
        assert!(
            !initial.is_null(),
            "render-thread first drawable returned nil"
        );
        assert!(
            ATTACH_REQUESTS.lock().unwrap().contains(&key),
            "render-thread drawable did not queue its SDL attachment"
        );
        service_video_thread();
        let worker_attachment = HOST_LAYERS
            .lock()
            .unwrap()
            .get(&key)
            .and_then(|entry| entry.attached)
            .expect("the next pump must attach the render-thread layer");
        assert_eq!(worker_attachment.window, sdl_b as usize);
        assert_eq!(worker_attachment.window_id, sdl3::video::window_id(sdl_b));
        objc::arc::release(initial);
        unsafe { send_void_id(view, objc::sel::get("setLayer:"), std::ptr::null_mut()) };
        objc::arc::release(metal);
        objc::arc::release(view);
        for window in windows {
            objc::arc::release(window)
        }
        service_video_thread();
        objc::arc::pool_pop(pool);
    }

    #[test]
    fn removing_a_retained_metal_view_frees_its_sdl_slot_for_replacement() {
        if !super::super::run_native_test_here() {
            return;
        }
        if !sdl3::isolated::run_here() {
            return;
        }
        crate::bootstrap();
        assert!(!super::super::default_device().is_null());
        if !sdl3::may_pump() {
            return;
        }
        let pool = objc::arc::pool_push();
        let rect = foundation::geometry::NSRect {
            origin: foundation::geometry::NSPoint { x: 0.0, y: 0.0 },
            size: foundation::geometry::NSSize {
                width: 320.0,
                height: 240.0,
            },
        };
        let probe =
            sdl3::video::create_window(c"metal-detach-probe", 1, 1, sdl3::video::flags::HIDDEN);
        let Some(probe) = probe else {
            eprintln!("skipped: SDL video is unavailable: {}", sdl3::error());
            objc::arc::pool_pop(pool);
            return;
        };
        sdl3::video::destroy_window(probe);
        let window_class = objc::class::lookup(c"NSWindow").unwrap();
        let view_class = objc::class::lookup(c"NSView").unwrap();
        let layer_class = objc::class::lookup(c"CAMetalLayer").unwrap();
        let make_window: unsafe extern "C" fn(
            Id,
            Sel,
            foundation::geometry::NSRect,
            usize,
            usize,
            u8,
        ) -> Id = unsafe { std::mem::transmute(objc::msg::entry()) };
        let make_view: unsafe extern "C" fn(Id, Sel, foundation::geometry::NSRect) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_id: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void_id: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(objc::msg::entry()) };
        let send_void: unsafe extern "C" fn(Id, Sel) =
            unsafe { std::mem::transmute(objc::msg::entry()) };

        let window = unsafe {
            make_window(
                objc::object::create_instance(window_class, 0),
                objc::sel::get("initWithContentRect:styleMask:backing:defer:"),
                rect,
                0,
                0,
                0,
            )
        };
        assert!(!window.is_null());
        let sdl = unsafe { appkit::window::sdl_window(window) };
        assert!(!sdl.is_null());
        sdl3::pump();
        let content = unsafe { send_id(window, objc::sel::get("contentView")) };
        let view = unsafe {
            make_view(
                objc::object::create_instance(view_class, 0),
                objc::sel::get("initWithFrame:"),
                rect,
            )
        };
        let metal =
            unsafe { send_id(objc::class::ptr(layer_class) as Id, objc::sel::get("layer")) };
        assert!(!view.is_null() && !metal.is_null());
        unsafe {
            send_void_id(view, objc::sel::get("setLayer:"), metal);
            send_void_id(content, objc::sel::get("addSubview:"), view);
        }
        let first = unsafe { next_drawable(metal, std::ptr::null()) };
        assert!(
            !first.is_null(),
            "initial Metal drawable must attach to SDL"
        );
        assert_eq!(
            HOST_LAYERS
                .lock()
                .unwrap()
                .get(&(metal as usize))
                .and_then(|entry| entry.attached)
                .unwrap()
                .window,
            sdl as usize,
        );

        // Keep the NSView (and its assigned CAMetalLayer) alive after removal.
        objc::arc::retain(view);
        unsafe { send_void(view, objc::sel::get("removeFromSuperview")) };
        appkit::event::pump(std::ptr::null_mut());
        assert_eq!(unsafe { appkit::layer::view_of(metal) }, view);
        assert!(unsafe { appkit::view::presentation_frame(view) }.is_none());
        assert!(
            HOST_LAYERS
                .lock()
                .unwrap()
                .get(&(metal as usize))
                .and_then(|entry| entry.attached)
                .is_none(),
            "detached live NSView still holds an SDL Metal view",
        );

        let replacement = unsafe {
            make_view(
                objc::object::create_instance(view_class, 0),
                objc::sel::get("initWithFrame:"),
                rect,
            )
        };
        let replacement_layer =
            unsafe { send_id(objc::class::ptr(layer_class) as Id, objc::sel::get("layer")) };
        unsafe {
            send_void_id(replacement, objc::sel::get("setLayer:"), replacement_layer);
            send_void_id(content, objc::sel::get("addSubview:"), replacement);
        }
        assert!(
            !unsafe { next_drawable(replacement_layer, std::ptr::null()) }.is_null(),
            "replacement Metal layer could not acquire its first drawable",
        );
        assert_eq!(
            HOST_LAYERS
                .lock()
                .unwrap()
                .get(&(replacement_layer as usize))
                .and_then(|entry| entry.attached)
                .unwrap()
                .window,
            sdl as usize,
        );

        unsafe {
            send_void_id(view, objc::sel::get("setLayer:"), std::ptr::null_mut());
            send_void_id(
                replacement,
                objc::sel::get("setLayer:"),
                std::ptr::null_mut(),
            );
        }
        objc::arc::release(view);
        objc::arc::release(view);
        objc::arc::release(replacement);
        objc::arc::release(window);
        service_video_thread();
        objc::arc::pool_pop(pool);
    }
}
