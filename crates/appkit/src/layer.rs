use std::ffi::c_void;

use foundation::geometry::{NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Layer {
    isa: *const c_void,
    /// `-contentsScale`. Never zero: see the module note.
    contents_scale: f64,
    /// The view whose layer this is, or null. Unretained, the mirror of the
    /// view's own retained reference: a layer never outlives the view that
    /// holds it, and a retain here would be a cycle neither side could break.
    ///
    /// AppKit spells this `-superlayer` and a real one is a layer, not a
    /// view. What the runtime actually needs it for is presentation: a
    /// `CAMetalLayer`'s frame reaches a window through the view that holds
    /// it, and nothing else in the object graph points that way.
    view: Id,
    /// `-delegate`, a `__weak` slot as Core Animation declares it: a view is
    /// the usual delegate and owns the layer, so a retain would be a cycle.
    delegate: Id,
    bounds: NSRect,
    /// `-mask`, retained: Core Animation declares the property `strong`, and a
    /// program that builds a mask layer, hands it over and drops its own
    /// reference expects the layer it set to still be there.
    mask: Id,
    opaque: u8,
    needs_display: u8,
    needs_layout: u8,
}

pub const INSTANCE_SIZE: usize = size_of::<Layer>();

#[repr(C)]
struct MetalLayer {
    layer: Layer,
    device: Id,
    pixel_format: u64,
    drawable_size: NSSize,
    framebuffer_only: u8,
    display_sync_enabled: u8,
}

pub const METAL_INSTANCE_SIZE: usize = size_of::<MetalLayer>();

fn layer_of<'a>(receiver: Id) -> &'a mut Layer {
    // SAFETY: a layer of this crate's, which is what every method here is sent
    // to.
    unsafe { &mut *(receiver as *mut Layer) }
}

fn metal_layer_of<'a>(receiver: Id) -> &'a mut MetalLayer {
    // SAFETY: a CAMetalLayer, whose registered layout is MetalLayer.
    unsafe { &mut *(receiver as *mut MetalLayer) }
}

/// `+[CALayer layer]`, autoreleased as a `+`-less constructor is.
unsafe extern "C" fn new_layer(class: Id, _sel: Sel) -> Id {
    // SAFETY: `class` is `CALayer` or one of its declared subclasses.
    let object = unsafe {
        objc::object::create_instance(
            objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
            0,
        )
    };
    let layer = layer_of(object);
    layer.contents_scale = 1.0;
    layer.view = std::ptr::null_mut();
    layer.delegate = std::ptr::null_mut();
    layer.bounds = NSRect::default();
    layer.mask = std::ptr::null_mut();
    layer.opaque = 0;
    layer.needs_display = 0;
    layer.needs_layout = 0;
    if objc::class::lookup(c"CAMetalLayer").is_some_and(|metal| {
        // SAFETY: the receiver of a class method is a class object.
        unsafe { objc::class::from_ptr(class as *const objc::class::Class) }.is_some_and(|actual| {
            actual
                .chain()
                .any(|candidate| std::ptr::eq(candidate, metal))
        })
    }) {
        let metal = metal_layer_of(object);
        // CAMetalLayer's documented defaults.
        metal.pixel_format = 80; // MTLPixelFormatBGRA8Unorm
        metal.framebuffer_only = 1;
        metal.display_sync_enabled = 1;
    }
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(object as corefoundation::base::CFTypeRef) as Id }
}

/// Record which view holds `layer`, as `-[NSView setLayer:]` decides it.
///
/// # Safety
///
/// `layer` is null or a layer of this crate's, and `view` is null or a view
/// of this crate's which outlives the layer.
pub unsafe fn set_view(layer: Id, view: Id) {
    if layer.is_null() || !is_layer(layer) {
        return;
    }
    layer_of(layer).view = view;
}

/// The view a layer was given to, or null.
///
/// # Safety
///
/// As [`set_view`].
pub unsafe fn view_of(layer: Id) -> Id {
    match !layer.is_null() && is_layer(layer) {
        true => layer_of(layer).view,
        false => std::ptr::null_mut(),
    }
}

/// A layer's `-bounds`, for the layer tree in `crate::coreanimation`.
pub(crate) fn bounds_of(layer: Id) -> NSRect {
    match is_layer(layer) {
        true => layer_of(layer).bounds,
        false => NSRect::default(),
    }
}

pub(crate) fn set_bounds_of(layer: Id, bounds: NSRect) {
    if is_layer(layer) {
        layer_of(layer).bounds = bounds;
    }
}

pub(crate) fn set_masks_to_bounds_of(layer: Id, masks: bool) {
    if is_layer(layer) {
        crate::coreanimation::set_masks(layer, masks);
    }
}

/// A layer's `-contentsScale`, one for something that is not a layer.
pub(crate) fn contents_scale_of(layer: Id) -> f64 {
    match is_layer(layer) {
        true => layer_of(layer).contents_scale,
        false => 1.0,
    }
}

/// Whether `layer` is a `CAMetalLayer`, whose pixels are presented by Metal
/// rather than drawn by the window's views.
pub(crate) fn is_metal(layer: Id) -> bool {
    objc::class::lookup(c"CAMetalLayer").is_some_and(|metal| {
        // SAFETY: null or an object the runtime allocated.
        unsafe { objc::object::class_of(layer) }.is_some_and(|class| {
            class
                .chain()
                .any(|candidate| std::ptr::eq(candidate, metal))
        })
    })
}

/// A layer's `-delegate`, or null for something that is not a layer.
pub(crate) fn delegate_of(layer: Id) -> Id {
    match is_layer(layer) {
        true => layer_of(layer).delegate,
        false => std::ptr::null_mut(),
    }
}

pub(crate) fn set_delegate_of(layer: Id, delegate: Id) {
    if is_layer(layer) {
        objc::arc::store_weak(&mut layer_of(layer).delegate, delegate);
    }
}

/// Apply an `NSViewLayerContentsPlacement` to a backing layer, as AppKit maps
/// each placement to the `contentsGravity` of the same name.
pub(crate) fn set_contents_gravity_for_placement(layer: Id, placement: isize) {
    use crate::coreanimation::Gravity;
    let gravity = match placement {
        1 => Gravity::ResizeAspect,
        2 => Gravity::ResizeAspectFill,
        3 => Gravity::Center,
        4 => Gravity::Top,
        5 => Gravity::TopRight,
        6 => Gravity::Right,
        7 => Gravity::BottomRight,
        8 => Gravity::Bottom,
        9 => Gravity::BottomLeft,
        10 => Gravity::Left,
        11 => Gravity::TopLeft,
        _ => Gravity::Resize,
    };
    if is_layer(layer) {
        crate::coreanimation::set_gravity(layer, gravity);
    }
}

fn is_layer(object: Id) -> bool {
    objc::class::lookup(c"CALayer").is_some_and(|base| {
        // SAFETY: null or an object the runtime allocated.
        unsafe { objc::object::class_of(object) }
            .is_some_and(|class| class.chain().any(|candidate| std::ptr::eq(candidate, base)))
    })
}

unsafe extern "C" fn contents_scale(receiver: Id, _sel: Sel) -> f64 {
    layer_of(receiver).contents_scale
}

/// `-setContentsScale:`, which refuses a scale of zero.
///
/// A caller that reads the scale back and multiplies by it is the reason the
/// class is here at all, and a zero stored on the way in would reintroduce the
/// bug from the other side. Apple's clamps too.
unsafe extern "C" fn set_contents_scale(receiver: Id, _sel: Sel, scale: f64) {
    if scale > 0.0 {
        layer_of(receiver).contents_scale = scale;
    }
}

unsafe extern "C" fn set_opaque(receiver: Id, _sel: Sel, opaque: u8) {
    layer_of(receiver).opaque = opaque;
}

unsafe extern "C" fn is_opaque(receiver: Id, _sel: Sel) -> u8 {
    layer_of(receiver).opaque
}

unsafe extern "C" fn bounds(receiver: Id, _sel: Sel) -> NSRect {
    layer_of(receiver).bounds
}

unsafe extern "C" fn set_bounds(receiver: Id, _sel: Sel, bounds: NSRect) {
    layer_of(receiver).bounds = bounds;
    crate::coreanimation::changed(receiver);
}

unsafe extern "C" fn set_delegate(receiver: Id, _sel: Sel, delegate: Id) {
    objc::arc::store_weak(&mut layer_of(receiver).delegate, delegate);
}

unsafe extern "C" fn delegate(receiver: Id, _sel: Sel) -> Id {
    layer_of(receiver).delegate
}

/// The layer's delegate, if it implements `name`.
///
/// Checked here rather than left to the send: every `CALayerDelegate` method
/// is optional, and a missing one selects a different path, not a no-op.
fn delegate_responding(receiver: Id, name: &str) -> Option<(Id, Sel)> {
    let delegate = layer_of(receiver).delegate;
    let sel = objc::sel::get(name);
    // SAFETY: null or a live object, which the weak slot guarantees.
    unsafe { objc::object::class_of(delegate) }?.method(sel)?;
    Some((delegate, sel))
}

fn send_void(receiver: Id, name: &str) {
    // SAFETY: a `void (id, SEL)` method on a live layer.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(name));
    }
}

/// `-display`: the delegate's `-displayLayer:` if it has one, otherwise a
/// context sized to the bounds for `-drawInContext:`.
///
/// What `-drawInContext:` draws is discarded rather than becoming the layer's
/// `contents`, which needs `CGBitmapContextCreateImage`; the calls are made
/// because guests put their state updates in them. A delegate that sets
/// `contents` itself from `-displayLayer:` is drawn.
unsafe extern "C" fn display(receiver: Id, _sel: Sel) {
    layer_of(receiver).needs_display = 0;
    if let Some((delegate, sel)) = delegate_responding(receiver, "displayLayer:") {
        // SAFETY: `-displayLayer:` is `void (id, SEL, CALayer *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(delegate, sel, receiver);
        }
        return;
    }
    if let Some((delegate, sel)) = delegate_responding(receiver, "layerWillDraw:") {
        // SAFETY: `-layerWillDraw:` is `void (id, SEL, CALayer *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(delegate, sel, receiver);
        }
    }
    let layer = layer_of(receiver);
    let scale = layer.contents_scale;
    let width = (layer.bounds.size.width * scale).ceil();
    let height = (layer.bounds.size.height * scale).ceil();
    if !(width >= 1.0 && height >= 1.0) {
        return;
    }
    // SAFETY: an owned buffer of the given size; 1 is premultiplied RGBA.
    let context = unsafe {
        crate::cgbitmap::CGBitmapContextCreate(
            std::ptr::null_mut(),
            width as usize,
            height as usize,
            8,
            0,
            std::ptr::null(),
            1,
        )
    };
    if context.is_null() {
        return;
    }
    // SAFETY: `-drawInContext:` is `void (id, SEL, CGContextRef)`, and the
    // context is released only after the send returns.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, *const c_void) =
            std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("drawInContext:"), context.cast());
        corefoundation::api::CFRelease(context);
    }
}

/// `-drawInContext:`, whose base implementation is the delegate's
/// `-drawLayer:inContext:`.
unsafe extern "C" fn draw_in_context(receiver: Id, _sel: Sel, context: *const c_void) {
    if let Some((delegate, sel)) = delegate_responding(receiver, "drawLayer:inContext:") {
        // SAFETY: `-drawLayer:inContext:` is `void (id, SEL, CALayer *, CGContextRef)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id, *const c_void) =
                std::mem::transmute(objc::msg::entry());
            f(delegate, sel, receiver, context);
        }
    }
}

/// `-setNeedsDisplay`. A view's layer is displayed in its window's next display
/// pass, which is where a layer that needs display is sent `-display`.
unsafe extern "C" fn set_needs_display(receiver: Id, _sel: Sel) {
    let layer = layer_of(receiver);
    layer.needs_display = 1;
    if !layer.view.is_null() {
        crate::backing::needs_display(layer.view);
    }
}

unsafe extern "C" fn needs_display(receiver: Id, _sel: Sel) -> u8 {
    layer_of(receiver).needs_display
}

/// `-displayIfNeeded`. There is no transaction commit to do this implicitly,
/// so a layer is only redrawn when asked.
unsafe extern "C" fn display_if_needed(receiver: Id, _sel: Sel) {
    if layer_of(receiver).needs_display != 0 {
        send_void(receiver, "display");
    }
}

/// `-layoutSublayers`, whose base implementation is the delegate's
/// `-layoutSublayersOfLayer:`.
unsafe extern "C" fn layout_sublayers(receiver: Id, _sel: Sel) {
    if let Some((delegate, sel)) = delegate_responding(receiver, "layoutSublayersOfLayer:") {
        // SAFETY: `-layoutSublayersOfLayer:` is `void (id, SEL, CALayer *)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            f(delegate, sel, receiver);
        }
    }
}

unsafe extern "C" fn set_needs_layout(receiver: Id, _sel: Sel) {
    layer_of(receiver).needs_layout = 1;
}

unsafe extern "C" fn needs_layout(receiver: Id, _sel: Sel) -> u8 {
    layer_of(receiver).needs_layout
}

unsafe extern "C" fn layout_if_needed(receiver: Id, _sel: Sel) {
    let layer = layer_of(receiver);
    if layer.needs_layout != 0 {
        layer.needs_layout = 0;
        send_void(receiver, "layoutSublayers");
    }
}

/// `-actionForKey:`. The delegate is asked first, and its `NSNull` means "no
/// action" without falling through. Nothing animates here, so there is no
/// default action to fall through to either.
unsafe extern "C" fn action_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    let Some((delegate, sel)) = delegate_responding(receiver, "actionForLayer:forKey:") else {
        return std::ptr::null_mut();
    };
    // SAFETY: `-actionForLayer:forKey:` is `id (id, SEL, CALayer *, NSString *)`.
    let action = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(delegate, sel, receiver, key)
    };
    let null = objc::class::lookup(c"NSNull");
    // SAFETY: null or an object the delegate returned.
    match unsafe { objc::object::class_of(action) } {
        Some(class) if null.is_some_and(|null| class.chain().any(|c| std::ptr::eq(c, null))) => {
            std::ptr::null_mut()
        }
        _ => action,
    }
}

/// `-dealloc`, which must unregister the weak delegate slot before the memory
/// holding it is freed.
unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    crate::coreanimation::forget(receiver);
    objc::arc::destroy_weak(&mut layer_of(receiver).delegate);
    let mask = std::mem::replace(&mut layer_of(receiver).mask, std::ptr::null_mut());
    objc::arc::release(mask);
    if objc::class::lookup(c"CAMetalLayer").is_some_and(|metal| {
        // SAFETY: a live layer being deallocated.
        unsafe { objc::object::class_of(receiver) }.is_some_and(|class| {
            class
                .chain()
                .any(|candidate| std::ptr::eq(candidate, metal))
        })
    }) {
        let device = std::mem::replace(&mut metal_layer_of(receiver).device, std::ptr::null_mut());
        objc::arc::release(device);
    }
    objc::arc::finalize(receiver);
}

/// `-setMask:`, the layer whose alpha clips this one.
///
/// Stored, retained and answered - and nothing is clipped. Nothing in this tree
/// composites a layer tree: a view draws itself, and a `CAMetalLayer` hands its
/// drawable to the window. A mask is a *property* here, which is what a program
/// that sets one, reads it back, and animates its bounds actually depends on;
/// pixels outside it are not cut, and a program whose visual effect is entirely
/// the mask gets an unclipped layer rather than a wrong one.
///
/// A layer masking *itself* is refused rather than stored: Core Animation calls
/// it undefined, and the retain it would take here is one the layer holds on
/// itself - a cycle that never comes apart. Refused quietly, because a real
/// AppKit draws such a layer wrongly rather than killing the program, and this
/// runtime has no drawing to get wrong.
unsafe extern "C" fn set_mask(receiver: Id, _sel: Sel, mask: Id) {
    if mask == receiver {
        return;
    }
    let layer = layer_of(receiver);
    if layer.mask != mask {
        let old = layer.mask;
        layer.mask = objc::arc::retain(mask);
        objc::arc::release(old);
    }
}

unsafe extern "C" fn mask(receiver: Id, _sel: Sel) -> Id {
    layer_of(receiver).mask
}

unsafe extern "C" fn set_device(receiver: Id, _sel: Sel, device: Id) {
    let layer = metal_layer_of(receiver);
    if layer.device != device {
        let old = layer.device;
        layer.device = objc::arc::retain(device);
        objc::arc::release(old);
    }
}

unsafe extern "C" fn device(receiver: Id, _sel: Sel) -> Id {
    metal_layer_of(receiver).device
}

unsafe extern "C" fn set_pixel_format(receiver: Id, _sel: Sel, format: u64) {
    metal_layer_of(receiver).pixel_format = format;
}

unsafe extern "C" fn pixel_format(receiver: Id, _sel: Sel) -> u64 {
    metal_layer_of(receiver).pixel_format
}

unsafe extern "C" fn set_drawable_size(receiver: Id, _sel: Sel, size: NSSize) {
    metal_layer_of(receiver).drawable_size = size;
}

unsafe extern "C" fn drawable_size(receiver: Id, _sel: Sel) -> NSSize {
    metal_layer_of(receiver).drawable_size
}

unsafe extern "C" fn set_framebuffer_only(receiver: Id, _sel: Sel, value: u8) {
    metal_layer_of(receiver).framebuffer_only = value;
}

unsafe extern "C" fn framebuffer_only(receiver: Id, _sel: Sel) -> u8 {
    metal_layer_of(receiver).framebuffer_only
}

unsafe extern "C" fn set_display_sync_enabled(receiver: Id, _sel: Sel, value: u8) {
    metal_layer_of(receiver).display_sync_enabled = value;
}

unsafe extern "C" fn display_sync_enabled(receiver: Id, _sel: Sel) -> u8 {
    metal_layer_of(receiver).display_sync_enabled
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"CALayer").expect("declared before its methods");
    class_methods!(class, {
        "layer" => new_layer, c"@16@0:8";
    });
    methods!(class, {
        "contentsScale" => contents_scale, c"d16@0:8";
        "setContentsScale:" => set_contents_scale, c"v24@0:8d16";
        "setOpaque:" => set_opaque, c"v20@0:8c16";
        "isOpaque" => is_opaque, c"c16@0:8";
        "bounds" => bounds, c"{CGRect={CGPoint=dd}{CGSize=dd}}16@0:8";
        "setBounds:" => set_bounds, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "mask" => mask, c"@16@0:8";
        "setMask:" => set_mask, c"v24@0:8@16";
        "delegate" => delegate, c"@16@0:8";
        "setDelegate:" => set_delegate, c"v24@0:8@16";
        "display" => display, c"v16@0:8";
        "drawInContext:" => draw_in_context, c"v24@0:8^{CGContext=}16";
        "setNeedsDisplay" => set_needs_display, c"v16@0:8";
        "needsDisplay" => needs_display, c"c16@0:8";
        "displayIfNeeded" => display_if_needed, c"v16@0:8";
        "layoutSublayers" => layout_sublayers, c"v16@0:8";
        "setNeedsLayout" => set_needs_layout, c"v16@0:8";
        "needsLayout" => needs_layout, c"c16@0:8";
        "layoutIfNeeded" => layout_if_needed, c"v16@0:8";
        "actionForKey:" => action_for_key, c"@24@0:8@16";
        "dealloc" => dealloc, c"v16@0:8";
        // Accepted and not kept. A filter only matters when contents are
        // scaled to the bounds, and the compositor here scales with its own;
        // asynchronous drawing is a scheduling hint.
        "setMagnificationFilter:" => ignore_object, c"v24@0:8@16";
        "setMinificationFilter:" => ignore_object, c"v24@0:8@16";
        "setDrawsAsynchronously:" => ignore_flag, c"v20@0:8c16";
    });
    let metal = objc::class::lookup(c"CAMetalLayer").expect("declared before its methods");
    methods!(metal, {
        "setDevice:" => set_device, c"v24@0:8@16";
        "device" => device, c"@16@0:8";
        "setPixelFormat:" => set_pixel_format, c"v24@0:8Q16";
        "pixelFormat" => pixel_format, c"Q16@0:8";
        "setDrawableSize:" => set_drawable_size, c"v32@0:8{CGSize=dd}16";
        "drawableSize" => drawable_size, c"{CGSize=dd}16@0:8";
        "setFramebufferOnly:" => set_framebuffer_only, c"v20@0:8c16";
        "framebufferOnly" => framebuffer_only, c"c16@0:8";
        "setDisplaySyncEnabled:" => set_display_sync_enabled, c"v20@0:8c16";
        "displaySyncEnabled" => display_sync_enabled, c"c16@0:8";
        // Presentation here never joins a Core Animation transaction, and
        // every drawable is presented in the colour space it was drawn in.
        "setPresentsWithTransaction:" => ignore_flag, c"v20@0:8c16";
        "setColorspace:" => ignore_object, c"v24@0:8^{CGColorSpace=}16";
        // FIXME (ACCURACY): presentation is SDR only. SDL sets this on every
        // swapchain, and to YES only for an HDR composition it asked for.
        "setWantsExtendedDynamicRangeContent:" => ignore_flag, c"v20@0:8c16";
    });
}

unsafe extern "C" fn ignore_object(_receiver: Id, _sel: Sel, _value: Id) {}

unsafe extern "C" fn ignore_flag(_receiver: Id, _sel: Sel, _value: u8) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer() -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(c"CALayer").expect("CALayer");
        // SAFETY: the class this crate declared.
        unsafe { new_layer(objc::class::ptr(class) as Id, objc::sel::get("layer")) }
    }

    /// A mask is a strong property: the layer that was set is the layer that
    /// comes back, and it survives the setter dropping its own reference.
    #[test]
    fn a_layer_keeps_the_mask_it_was_given() {
        let masked = layer();
        let first = layer();
        let second = layer();
        let get = objc::sel::get("mask");
        let set = objc::sel::get("setMask:");
        let before = objc::arc::retain_count(first);
        // SAFETY: `-mask` and `-setMask:` on layers this test made.
        unsafe {
            assert!(mask(masked, get).is_null(), "a fresh layer has no mask");
            set_mask(masked, set, first);
            assert_eq!(mask(masked, get), first);
            assert_eq!(
                objc::arc::retain_count(first),
                before + 1,
                "the layer holds the mask it was given"
            );
            // Replacing one releases the old and retains the new.
            set_mask(masked, set, second);
            assert_eq!(mask(masked, get), second);
            assert_eq!(objc::arc::retain_count(first), before);
            // Setting the same mask twice does not retain it twice.
            let held = objc::arc::retain_count(second);
            set_mask(masked, set, second);
            assert_eq!(objc::arc::retain_count(second), held);
            // And nil clears it, releasing what was there.
            set_mask(masked, set, std::ptr::null_mut());
            assert!(mask(masked, get).is_null());
            assert_eq!(objc::arc::retain_count(second), held - 1);
        }
    }

    /// A layer that masked itself would hold a reference to itself, which no
    /// release could ever reach.
    #[test]
    fn a_layer_does_not_mask_itself() {
        let layer = layer();
        let before = objc::arc::retain_count(layer);
        // SAFETY: `-setMask:` is `void (id, SEL, CALayer *)`.
        unsafe {
            set_mask(layer, objc::sel::get("setMask:"), layer);
            assert!(mask(layer, objc::sel::get("mask")).is_null());
        }
        assert_eq!(objc::arc::retain_count(layer), before, "no cycle taken");
    }

    /// The mask goes when the layer does, which is what makes the retain above
    /// an ownership rather than a leak.
    #[test]
    fn deallocating_a_layer_releases_its_mask() {
        crate::bootstrap();
        let class = objc::class::lookup(c"CALayer").expect("CALayer");
        let owner = objc::object::create_instance(class, 0);
        let mask_layer = layer();
        let held = objc::arc::retain_count(mask_layer);
        // SAFETY: `-setMask:` on a layer this test owns outright.
        unsafe { set_mask(owner, objc::sel::get("setMask:"), mask_layer) };
        assert_eq!(objc::arc::retain_count(mask_layer), held + 1);
        objc::arc::release(owner);
        assert_eq!(
            objc::arc::retain_count(mask_layer),
            held,
            "-dealloc gave the mask back"
        );
    }

    /// The whole reason the class exists: a scale of zero is what a message to
    /// nil answers, and a caller multiplying its bounds by it gets nothing.
    #[test]
    fn a_new_layer_has_a_scale_of_one() {
        let layer = layer();
        // SAFETY: a layer this test made.
        assert_eq!(unsafe { contents_scale(layer, objc::sel::get("x")) }, 1.0);
    }

    #[test]
    fn a_metal_layer_inherits_the_layer_constructor() {
        crate::bootstrap();
        let class = objc::class::lookup(c"CAMetalLayer").expect("CAMetalLayer");
        let selector = objc::sel::get("layer");
        let implementation = class
            .metaclass()
            .method(selector)
            .expect("CAMetalLayer inherits +[CALayer layer]")
            .implementation();
        // SAFETY: the inherited method is `id (Class, SEL)`.
        let call: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(implementation) };
        let object = unsafe { call(objc::class::ptr(class) as Id, selector) };
        // SAFETY: a live object returned by `+layer`.
        assert!(
            unsafe { objc::object::class_of(object) }.is_some_and(|actual| {
                actual
                    .chain()
                    .any(|candidate| std::ptr::eq(candidate, class))
            })
        );
    }

    #[test]
    fn a_scale_reads_back_and_zero_is_refused() {
        let layer = layer();
        let sel = objc::sel::get("x");
        // SAFETY: a layer this test made.
        unsafe {
            set_contents_scale(layer, sel, 2.0);
            assert_eq!(contents_scale(layer, sel), 2.0);
            set_contents_scale(layer, sel, 0.0);
            assert_eq!(contents_scale(layer, sel), 2.0, "zero must not be stored");
        }
    }

    thread_local! {
        static CALLS: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn record(name: &'static str) {
        CALLS.with(|calls| calls.borrow_mut().push(name));
    }

    fn take_calls() -> Vec<&'static str> {
        CALLS.with(|calls| std::mem::take(&mut *calls.borrow_mut()))
    }

    unsafe extern "C" fn will_draw(_: Id, _: Sel, _: Id) {
        record("layerWillDraw:");
    }

    unsafe extern "C" fn draw_layer(_: Id, _: Sel, _: Id, context: *const c_void) {
        assert!(!context.is_null());
        record("drawLayer:inContext:");
    }

    unsafe extern "C" fn display_layer(_: Id, _: Sel, _: Id) {
        record("displayLayer:");
    }

    unsafe extern "C" fn layout_layer(_: Id, _: Sel, _: Id) {
        record("layoutSublayersOfLayer:");
    }

    unsafe extern "C" fn null_action(_: Id, _: Sel, _: Id, _: Id) -> Id {
        let class = objc::class::lookup(c"NSNull").expect("NSNull");
        // SAFETY: `+null` is `id (Class, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            f(objc::class::ptr(class) as Id, objc::sel::get("null"))
        }
    }

    fn delegate_class(name: &std::ffi::CStr, displays: bool) -> &'static objc::class::Class {
        static REGISTERING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        crate::bootstrap();
        let _guard = REGISTERING.lock().unwrap();
        objc::class::lookup(name).unwrap_or_else(|| {
            let base = objc::class::lookup(c"NSObject").expect("NSObject");
            let class = objc::class::allocate_pair(Some(base), name, 0).expect("a fresh class");
            let add = |sel: &str, imp: *const c_void, types: &std::ffi::CStr| {
                assert!(class.add_method(objc::sel::get(sel), imp as objc::class::Imp, types));
            };
            add("layerWillDraw:", will_draw as *const c_void, c"v24@0:8@16");
            add(
                "drawLayer:inContext:",
                draw_layer as *const c_void,
                c"v32@0:8@16^v24",
            );
            add(
                "layoutSublayersOfLayer:",
                layout_layer as *const c_void,
                c"v24@0:8@16",
            );
            add(
                "actionForLayer:forKey:",
                null_action as *const c_void,
                c"@32@0:8@16@24",
            );
            if displays {
                add(
                    "displayLayer:",
                    display_layer as *const c_void,
                    c"v24@0:8@16",
                );
            }
            objc::class::register_pair(class);
            class
        })
    }

    fn layer_with_delegate(name: &std::ffi::CStr, displays: bool) -> (Id, Id) {
        let layer = layer();
        let delegate = objc::object::create_instance(delegate_class(name, displays), 0);
        // SAFETY: a layer this test made.
        unsafe { set_delegate(layer, objc::sel::get("setDelegate:"), delegate) };
        take_calls();
        (layer, delegate)
    }

    #[test]
    fn display_prefers_the_delegates_display_layer() {
        let (layer, _delegate) = layer_with_delegate(c"RosemaryDisplayingLayerDelegate", true);
        let sel = objc::sel::get("x");
        // SAFETY: a layer this test made.
        unsafe {
            set_bounds(
                layer,
                sel,
                NSRect {
                    size: NSSize {
                        width: 4.0,
                        height: 4.0,
                    },
                    ..Default::default()
                },
            );
            display(layer, sel);
        }
        assert_eq!(take_calls(), ["displayLayer:"]);
    }

    #[test]
    fn display_draws_through_the_delegate_into_a_context() {
        let (layer, delegate) = layer_with_delegate(c"RosemaryDrawingLayerDelegate", false);
        let sel = objc::sel::get("x");
        // SAFETY: a layer this test made.
        unsafe {
            assert_eq!(delegate_of(layer), delegate);
            set_bounds(
                layer,
                sel,
                NSRect {
                    size: NSSize {
                        width: 4.0,
                        height: 4.0,
                    },
                    ..Default::default()
                },
            );
            set_needs_display(layer, sel);
            display_if_needed(layer, sel);
            display_if_needed(layer, sel);
        }
        assert_eq!(take_calls(), ["layerWillDraw:", "drawLayer:inContext:"]);
    }

    #[test]
    fn layout_and_actions_consult_the_delegate() {
        let (layer, _delegate) = layer_with_delegate(c"RosemaryDrawingLayerDelegate", false);
        let sel = objc::sel::get("x");
        // SAFETY: a layer this test made.
        unsafe {
            set_needs_layout(layer, sel);
            layout_if_needed(layer, sel);
            layout_if_needed(layer, sel);
            assert!(action_for_key(layer, sel, std::ptr::null_mut()).is_null());
        }
        assert_eq!(take_calls(), ["layoutSublayersOfLayer:"]);
    }

    #[test]
    fn the_delegate_is_not_retained_and_is_zeroed_when_it_goes() {
        let (layer, delegate) = layer_with_delegate(c"RosemaryDrawingLayerDelegate", false);
        assert_eq!(objc::arc::retain_count(delegate), 1);
        objc::arc::release(delegate);
        // SAFETY: a layer this test made.
        assert!(unsafe { delegate_of(layer) }.is_null());
    }

    unsafe fn delegate_of(layer: Id) -> Id {
        // SAFETY: as the caller's.
        unsafe { delegate(layer, objc::sel::get("delegate")) }
    }
}
