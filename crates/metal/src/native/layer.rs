//! Native CAMetalLayer and CAMetalDrawable bridge for the built-in guest runtime.
//! One retained host layer per guest layer, with lifetime tied to CALayer dealloc.
//! Cocoa window attachment is deliberately kept separate from basic drawable
//! creation, so an unpresentable/unknown layer does not corrupt an SDL window.

use super::*;
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
}

struct Entry {
    layer: Arc<HostLayer>,
    attached: Option<Attachment>,
}

static HOST_LAYERS: LazyLock<Mutex<HashMap<usize, Entry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A request retains the guest CALayer until the video thread consumes it.
/// The registry itself MUST NOT retain the guest, or dealloc would never run.
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
    appkit::layer::add_metal_dealloc_hook(forget);
    appkit::event::add_pump_hook(service_video_thread);
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

fn request_attachment(layer: Id) {
    // A full-window native view is attached once, not recreated per drawable.
    if HOST_LAYERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(layer as usize))
        .is_some_and(|entry| entry.attached.is_some())
    {
        return;
    }
    // nextDrawable is generally called from a guest render thread, not the
    // video thread. Do not touch SDL here; put a retained guest layer on a
    // queue the video owner services on its next event-pump iteration.
    let view = unsafe { appkit::layer::view_of(layer) };
    if view.is_null() {
        return;
    }
    let retained = objc::arc::retain(layer);
    let mut requests = ATTACH_REQUESTS.lock().unwrap_or_else(|e| e.into_inner());
    if requests.contains(&(layer as usize)) {
        drop(requests);
        objc::arc::release(retained);
        return;
    }
    requests.push(layer as usize);
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
    for address in pending {
        let guest = address as Id;
        attach_one(guest);
        objc::arc::release(guest);
    }
}

fn attach_one(guest: Id) {
    let view = unsafe { appkit::layer::view_of(guest) };
    if view.is_null() {
        return;
    }
    let Some(frame) = (unsafe { appkit::view::presentation_frame(view) }) else {
        return;
    };
    if !frame.covers_window() || frame.window.is_null() {
        return;
    }
    let window = frame.window as usize;
    let Some(window_id) = sdl3::video::live_window_id(frame.window) else {
        return;
    };
    {
        let entries = HOST_LAYERS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = entries.get(&(guest as usize)) else {
            return;
        };
        if entry.attached.is_some()
            || entries.values().any(|item| {
                item.attached
                    .is_some_and(|attached| attached.window == window)
            })
        {
            return;
        }
    }
    let Some((native_view, native_layer)) = sdl3::video::metal::create(frame.window) else {
        gpu::trace("native Metal could not attach SDL Metal view".to_string());
        return;
    };
    // SDL owns the view and a reference to its layer. Retain the layer for
    // commands in flight, independently of SDL_Metal_DestroyView.
    let native_host = unsafe { host_retain(native_layer) };
    let attachment = Attachment {
        window,
        window_id,
        view: native_view as usize,
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
        // There is no owner left for this view (e.g. the guest layer was
        // retired during a host Cocoa callback). Undo the installation.
        destroy_views(vec![attachment]);
    } else {
        gpu::trace(format!(
            "native Metal attached SDL view to guest CAMetalLayer (window {window_id})"
        ));
    }
    drop(old);
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

fn guest_object(guest: Id, name: &str) -> Id {
    let call: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe { call(guest, objc::sel::get(name)) }
}

fn guest_u64(guest: Id, name: &str) -> u64 {
    let call: unsafe extern "C" fn(Id, Sel) -> u64 =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe { call(guest, objc::sel::get(name)) }
}

fn guest_u8(guest: Id, name: &str) -> u8 {
    let call: unsafe extern "C" fn(Id, Sel) -> u8 =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe { call(guest, objc::sel::get(name)) }
}

fn guest_size(guest: Id, name: &str) -> foundation::geometry::NSSize {
    let call: unsafe extern "C" fn(Id, Sel) -> foundation::geometry::NSSize =
        unsafe { std::mem::transmute(objc::msg::entry()) };
    unsafe { call(guest, objc::sel::get(name)) }
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
    let supplied = guest_object(layer, "device");
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
    let mut size = guest_size(guest, "drawableSize");
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
            guest_u64(guest, "pixelFormat"),
        );
        host_send_void_size(layer, sel(c"setDrawableSize:"), size);
        host_send_void_bool(
            layer,
            sel(c"setFramebufferOnly:"),
            guest_u8(guest, "framebufferOnly") != 0,
        );
        host_send_void_bool(
            layer,
            sel(c"setDisplaySyncEnabled:"),
            guest_u8(guest, "displaySyncEnabled") != 0,
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
    let Some(layer) = host_layer(receiver) else {
        return std::ptr::null_mut();
    };
    request_attachment(receiver);
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
