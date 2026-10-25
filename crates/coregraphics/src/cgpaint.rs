//! The Quartz objects that paint rather than draw: `CGPattern`, `CGFunction`
//! and `CGShading`, and `CGLayer`, an offscreen a context draws into and
//! composites from.

#![allow(non_snake_case)]

use std::ffi::c_void;

use crate::{CGPoint, CGRect, CGSize};
use corefoundation::base::CFTypeRef;

use crate::cgcontext::CGAffineTransform;

/// `CGPatternCallbacks`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PatternCallbacks {
    version: u32,
    draw: Option<unsafe extern "C" fn(*mut c_void, CFTypeRef)>,
    release: Option<unsafe extern "C" fn(*mut c_void)>,
}

/// A rendered tile, kept so a pattern drawn many times at one scale calls its
/// program back once.
struct Tile {
    width: u32,
    height: u32,
    /// Premultiplied R, G, B, A, top row first, as drawn.
    rgba: Vec<u8>,
}

#[repr(C)]
struct Pattern {
    isa: *const c_void,
    info: *mut c_void,
    bounds: CGRect,
    matrix: CGAffineTransform,
    x_step: f64,
    y_step: f64,
    colored: bool,
    callbacks: PatternCallbacks,
    cache: *mut Option<Tile>,
}

/// `CGFunctionCallbacks`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FunctionCallbacks {
    version: u32,
    evaluate: Option<unsafe extern "C" fn(*mut c_void, *const f64, *mut f64)>,
    release: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[repr(C)]
struct Function {
    isa: *const c_void,
    info: *mut c_void,
    domain: [f64; 2],
    outputs: usize,
    callbacks: FunctionCallbacks,
}

#[repr(C)]
struct Shading {
    isa: *const c_void,
    radial: bool,
    components: usize,
    start: CGPoint,
    end: CGPoint,
    radii: (f64, f64),
    function: CFTypeRef,
    extend: (bool, bool),
}

#[repr(C)]
struct Layer {
    isa: *const c_void,
    context: CFTypeRef,
    size: CGSize,
}

pub fn install() {
    let pattern = corefoundation::base::register(c"__CGPattern", None, size_of::<Pattern>(), true);
    pattern.replace_method(
        objc::sel::get("dealloc"),
        pattern_dealloc as *const c_void,
        c"v16@0:8",
    );
    let function =
        corefoundation::base::register(c"__CGFunction", None, size_of::<Function>(), true);
    function.replace_method(
        objc::sel::get("dealloc"),
        function_dealloc as *const c_void,
        c"v16@0:8",
    );
    let shading = corefoundation::base::register(c"__CGShading", None, size_of::<Shading>(), true);
    shading.replace_method(
        objc::sel::get("dealloc"),
        shading_dealloc as *const c_void,
        c"v16@0:8",
    );
    let layer = corefoundation::base::register(c"__CGLayer", None, size_of::<Layer>(), true);
    layer.replace_method(
        objc::sel::get("dealloc"),
        layer_dealloc as *const c_void,
        c"v16@0:8",
    );
}

fn object_of<'a, T>(object: CFTypeRef, class: &std::ffi::CStr) -> Option<&'a mut T> {
    let class = objc::class::lookup(class)?;
    // SAFETY: null or an object a guest received from this runtime.
    if object.is_null() || !unsafe { corefoundation::base::is_kind_of(object, class) } {
        return None;
    }
    // SAFETY: checked to be an instance of the class whose body is `T`.
    Some(unsafe { &mut *(object as *mut T) })
}

fn allocate<T>(class: &std::ffi::CStr) -> *mut T {
    let class = objc::class::lookup(class).expect("installed during bootstrap");
    // SAFETY: `T` is the registered class layout.
    unsafe { corefoundation::base::allocate::<T>(class) }
}

unsafe extern "C" fn pattern_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGPattern`.
    unsafe {
        let pattern = &mut *(receiver as *mut Pattern);
        if let Some(release) = pattern.callbacks.release {
            release(pattern.info);
        }
        if !pattern.cache.is_null() {
            drop(Box::from_raw(pattern.cache));
        }
        objc::arc::finalize(receiver);
    }
}

unsafe extern "C" fn function_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGFunction`.
    unsafe {
        let function = &mut *(receiver as *mut Function);
        if let Some(release) = function.callbacks.release {
            release(function.info);
        }
        objc::arc::finalize(receiver);
    }
}

unsafe extern "C" fn shading_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGShading`, which retained its function.
    unsafe {
        let shading = &mut *(receiver as *mut Shading);
        if !shading.function.is_null() {
            corefoundation::api::CFRelease(shading.function);
        }
        objc::arc::finalize(receiver);
    }
}

unsafe extern "C" fn layer_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGLayer`, which owns its context.
    unsafe {
        let layer = &mut *(receiver as *mut Layer);
        if !layer.context.is_null() {
            corefoundation::api::CFRelease(layer.context);
        }
        objc::arc::finalize(receiver);
    }
}

/// `CGPatternRef CGPatternCreate(void *info, CGRect bounds, CGAffineTransform
/// matrix, CGFloat xStep, CGFloat yStep, CGPatternTiling, bool isColored,
/// const CGPatternCallbacks *)`
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGPatternCreate(
    info: *mut c_void,
    bounds: CGRect,
    matrix: CGAffineTransform,
    x_step: f64,
    y_step: f64,
    _tiling: i32,
    colored: bool,
    callbacks: *const PatternCallbacks,
) -> CFTypeRef {
    if callbacks.is_null() {
        return std::ptr::null();
    }
    let pattern = allocate::<Pattern>(c"__CGPattern");
    // SAFETY: a fresh allocation and the caller's callbacks, copied.
    unsafe {
        (*pattern).info = info;
        (*pattern).bounds = bounds;
        (*pattern).matrix = matrix;
        // A zero step is the bounds' size, as Quartz takes it.
        (*pattern).x_step = if x_step == 0.0 {
            bounds.size.width
        } else {
            x_step
        };
        (*pattern).y_step = if y_step == 0.0 {
            bounds.size.height
        } else {
            y_step
        };
        (*pattern).colored = colored;
        (*pattern).callbacks = *callbacks;
        (*pattern).cache = Box::into_raw(Box::new(None));
    }
    pattern as CFTypeRef
}

pub unsafe extern "C" fn CGPatternRelease(pattern: CFTypeRef) {
    if !pattern.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(pattern) };
    }
}

pub unsafe extern "C" fn CGPatternRetain(pattern: CFTypeRef) -> CFTypeRef {
    if !pattern.is_null() {
        // SAFETY: a pattern the caller holds.
        unsafe { corefoundation::api::CFRetain(pattern) };
    }
    pattern
}

/// The pattern's matrix: pattern space to the context's base space.
pub fn pattern_matrix(pattern: CFTypeRef) -> Option<CGAffineTransform> {
    object_of::<Pattern>(pattern, c"__CGPattern").map(|p| p.matrix)
}

pub struct RenderedTile {
    /// In the target's channel order.
    pub pixmap: tiny_skia::Pixmap,
    /// Tile pixels to pattern space.
    pub to_pattern: CGAffineTransform,
}

/// One cell of the pattern, drawn by the program's callback at `scale` device
/// pixels per pattern unit. An uncoloured pattern is painted `color`.
pub fn render_tile(
    pattern: CFTypeRef,
    scale: f64,
    color: [f64; 4],
    swap: bool,
) -> Option<RenderedTile> {
    /// A tile larger than this is drawn at a lower resolution rather than
    /// allocated: a pattern cell is meant to repeat, not to cover the window.
    const LIMIT: f64 = 2048.0;
    let p = object_of::<Pattern>(pattern, c"__CGPattern")?;
    let (x_step, y_step) = (p.x_step.abs(), p.y_step.abs());
    if x_step == 0.0 || y_step == 0.0 {
        return None;
    }
    let width = (x_step * scale).ceil().clamp(1.0, LIMIT) as u32;
    let height = (y_step * scale).ceil().clamp(1.0, LIMIT) as u32;
    // SAFETY: the cache this pattern was created with.
    let cache = unsafe { &mut *p.cache };
    if cache
        .as_ref()
        .is_none_or(|t| (t.width, t.height) != (width, height))
    {
        let draw = p.callbacks.draw?;
        // SAFETY: a context over its own storage; the callback is the
        // program's own, called as Quartz calls it, with the context then
        // released.
        let rgba = unsafe {
            let context = crate::cgbitmap::CGBitmapContextCreate(
                std::ptr::null_mut(),
                width as usize,
                height as usize,
                8,
                width as usize * 4,
                std::ptr::null(),
                1,
            );
            if context.is_null() {
                return None;
            }
            crate::cgcontext::CGContextScaleCTM(
                context,
                f64::from(width) / x_step,
                f64::from(height) / y_step,
            );
            draw(p.info, context);
            let data = crate::cgbitmap::CGBitmapContextGetData(context) as *const u8;
            let rgba = std::slice::from_raw_parts(data, (width * height * 4) as usize).to_vec();
            crate::cgcontext::CGContextRelease(context);
            rgba
        };
        *cache = Some(Tile {
            width,
            height,
            rgba,
        });
    }
    let tile = cache.as_ref()?;
    let mut pixels = tile.rgba.clone();
    if !p.colored {
        // An uncoloured pattern is a stencil: its coverage in the colour given.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            let a = f64::from(pixel[3]) / 255.0 * color[3].clamp(0.0, 1.0);
            let c = |v: f64| (v.clamp(0.0, 1.0) * a * 255.0).round() as u8;
            pixel.copy_from_slice(&[
                c(color[0]),
                c(color[1]),
                c(color[2]),
                (a * 255.0).round() as u8,
            ]);
        }
    }
    if swap {
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
    }
    Some(RenderedTile {
        pixmap: tiny_skia::Pixmap::from_vec(pixels, tiny_skia::IntSize::from_wh(width, height)?)?,
        to_pattern: CGAffineTransform {
            a: x_step / f64::from(width),
            b: 0.0,
            c: 0.0,
            d: -y_step / f64::from(height),
            tx: 0.0,
            ty: y_step,
        },
    })
}

/// `CGFunctionRef CGFunctionCreate(void *info, size_t domainDimension, const
/// CGFloat *domain, size_t rangeDimension, const CGFloat *range, const
/// CGFunctionCallbacks *)`
pub unsafe extern "C" fn CGFunctionCreate(
    info: *mut c_void,
    domain_dimension: usize,
    domain: *const f64,
    range_dimension: usize,
    _range: *const f64,
    callbacks: *const FunctionCallbacks,
) -> CFTypeRef {
    if callbacks.is_null() || domain_dimension != 1 {
        return std::ptr::null();
    }
    let function = allocate::<Function>(c"__CGFunction");
    // SAFETY: a fresh allocation; `domain` is two values for one input.
    unsafe {
        (*function).info = info;
        (*function).domain = match domain.is_null() {
            true => [0.0, 1.0],
            false => [*domain, *domain.add(1)],
        };
        (*function).outputs = range_dimension;
        (*function).callbacks = *callbacks;
    }
    function as CFTypeRef
}

pub unsafe extern "C" fn CGFunctionRelease(function: CFTypeRef) {
    if !function.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(function) };
    }
}

pub unsafe extern "C" fn CGFunctionRetain(function: CFTypeRef) -> CFTypeRef {
    if !function.is_null() {
        // SAFETY: a function the caller holds.
        unsafe { corefoundation::api::CFRetain(function) };
    }
    function
}

fn retain(object: CFTypeRef) -> CFTypeRef {
    match object.is_null() {
        true => object,
        // SAFETY: the caller's live object.
        false => unsafe { corefoundation::api::CFRetain(object) },
    }
}

/// `CGShadingRef CGShadingCreateAxial(CGColorSpaceRef, CGPoint start, CGPoint
/// end, CGFunctionRef, bool extendStart, bool extendEnd)`
pub unsafe extern "C" fn CGShadingCreateAxial(
    space: CFTypeRef,
    start: CGPoint,
    end: CGPoint,
    function: CFTypeRef,
    extend_start: bool,
    extend_end: bool,
) -> CFTypeRef {
    make_shading(
        space,
        false,
        start,
        end,
        (0.0, 0.0),
        function,
        (extend_start, extend_end),
    )
}

/// `CGShadingRef CGShadingCreateRadial(CGColorSpaceRef, CGPoint start, CGFloat
/// startRadius, CGPoint end, CGFloat endRadius, CGFunctionRef, bool, bool)`
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGShadingCreateRadial(
    space: CFTypeRef,
    start: CGPoint,
    start_radius: f64,
    end: CGPoint,
    end_radius: f64,
    function: CFTypeRef,
    extend_start: bool,
    extend_end: bool,
) -> CFTypeRef {
    make_shading(
        space,
        true,
        start,
        end,
        (start_radius, end_radius),
        function,
        (extend_start, extend_end),
    )
}

fn make_shading(
    space: CFTypeRef,
    radial: bool,
    start: CGPoint,
    end: CGPoint,
    radii: (f64, f64),
    function: CFTypeRef,
    extend: (bool, bool),
) -> CFTypeRef {
    if object_of::<Function>(function, c"__CGFunction").is_none() {
        return std::ptr::null();
    }
    let shading = allocate::<Shading>(c"__CGShading");
    // SAFETY: a fresh allocation; the function is retained for its life.
    unsafe {
        (*shading).radial = radial;
        (*shading).components = crate::cgcolor::space_components(space).unwrap_or(3).max(1);
        (*shading).start = start;
        (*shading).end = end;
        (*shading).radii = radii;
        (*shading).function = retain(function);
        (*shading).extend = extend;
    }
    shading as CFTypeRef
}

pub unsafe extern "C" fn CGShadingRelease(shading: CFTypeRef) {
    if !shading.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(shading) };
    }
}

pub unsafe extern "C" fn CGShadingRetain(shading: CFTypeRef) -> CFTypeRef {
    retain(shading)
}

/// A shading as the engine paints it: its geometry and its function sampled
/// into straight R, G, B, A stops.
pub fn shading_paint(shading: CFTypeRef) -> Option<crate::quartz::Shading> {
    /// Enough stops that the eye cannot find the joins in a smooth function.
    const SAMPLES: usize = 64;
    let s = object_of::<Shading>(shading, c"__CGShading")?;
    let f = object_of::<Function>(s.function, c"__CGFunction")?;
    let evaluate = f.callbacks.evaluate?;
    let mut out = vec![0.0f64; f.outputs.max(4)];
    let mut stops = Vec::with_capacity(SAMPLES + 1);
    for i in 0..=SAMPLES {
        let at = i as f64 / SAMPLES as f64;
        let input = f.domain[0] + at * (f.domain[1] - f.domain[0]);
        out.iter_mut().for_each(|v| *v = 0.0);
        // SAFETY: the program's evaluator, with one input and room for every
        // output it declared.
        unsafe { evaluate(f.info, &input, out.as_mut_ptr()) };
        let rgba = match s.components {
            1 => {
                let alpha = if f.outputs >= 2 { out[1] } else { 1.0 };
                [out[0], out[0], out[0], alpha]
            }
            _ => {
                let alpha = if f.outputs >= 4 { out[3] } else { 1.0 };
                [out[0], out[1], out[2], alpha]
            }
        };
        stops.push((at as f32, rgba));
    }
    Some(crate::quartz::Shading {
        radial: s.radial.then_some(s.radii),
        start: (s.start.x, s.start.y),
        end: (s.end.x, s.end.y),
        extend: s.extend,
        stops,
    })
}

/// `CGLayerRef CGLayerCreateWithContext(CGContextRef, CGSize, CFDictionaryRef)`
///
/// An offscreen at the context's device resolution, so a layer drawn back at
/// its own size is as sharp as drawing directly.
pub unsafe extern "C" fn CGLayerCreateWithContext(
    context: CFTypeRef,
    size: CGSize,
    _auxiliary: CFTypeRef,
) -> CFTypeRef {
    if size.width <= 0.0 || size.height <= 0.0 {
        return std::ptr::null();
    }
    let scale = crate::quartz::frame_of(context).map_or(1.0, |f| f.scale);
    let width = (size.width * scale).ceil().max(1.0) as usize;
    let height = (size.height * scale).ceil().max(1.0) as usize;
    // SAFETY: a context over its own storage.
    let backing = unsafe {
        crate::cgbitmap::CGBitmapContextCreate(
            std::ptr::null_mut(),
            width,
            height,
            8,
            width * 4,
            std::ptr::null(),
            crate::cgimage::PREMULTIPLIED_BGRA,
        )
    };
    if backing.is_null() {
        return std::ptr::null();
    }
    // SAFETY: the fresh context.
    unsafe {
        crate::cgcontext::CGContextScaleCTM(
            backing,
            width as f64 / size.width,
            height as f64 / size.height,
        )
    };
    let layer = allocate::<Layer>(c"__CGLayer");
    // SAFETY: a fresh allocation, which takes the context's reference.
    unsafe {
        (*layer).context = backing;
        (*layer).size = size;
    }
    layer as CFTypeRef
}

pub unsafe extern "C" fn CGLayerGetContext(layer: CFTypeRef) -> CFTypeRef {
    object_of::<Layer>(layer, c"__CGLayer").map_or(std::ptr::null(), |l| l.context)
}

pub unsafe extern "C" fn CGLayerGetSize(layer: CFTypeRef) -> CGSize {
    object_of::<Layer>(layer, c"__CGLayer").map_or(CGSize::default(), |l| l.size)
}

pub unsafe extern "C" fn CGLayerRelease(layer: CFTypeRef) {
    if !layer.is_null() {
        // SAFETY: a reference the caller owns.
        unsafe { corefoundation::api::CFRelease(layer) };
    }
}

pub unsafe extern "C" fn CGLayerRetain(layer: CFTypeRef) -> CFTypeRef {
    retain(layer)
}

/// A layer's pixels as premultiplied B, G, R, A, top row first, with its size
/// in pixels.
pub fn layer_pixels(layer: CFTypeRef) -> Option<(usize, usize, Vec<u8>)> {
    let l = object_of::<Layer>(layer, c"__CGLayer")?;
    let ctx = crate::quartz::context_of(l.context)?;
    Some((ctx.width, ctx.height, crate::quartz::snapshot_bgra(ctx)?))
}

/// `CGContextDrawLayerInRect(CGContextRef, CGRect, CGLayerRef)`
pub unsafe extern "C" fn CGContextDrawLayerInRect(
    context: CFTypeRef,
    rect: CGRect,
    layer: CFTypeRef,
) {
    if let Some((width, height, bgra)) = layer_pixels(layer) {
        crate::quartz::draw_pixels(context, rect, width, height, bgra);
    }
}

/// `CGContextDrawLayerAtPoint(CGContextRef, CGPoint, CGLayerRef)`: at the
/// layer's own size.
pub unsafe extern "C" fn CGContextDrawLayerAtPoint(
    context: CFTypeRef,
    point: CGPoint,
    layer: CFTypeRef,
) {
    // SAFETY: the same arguments, with the rectangle the point and size make.
    unsafe {
        let size = CGLayerGetSize(layer);
        CGContextDrawLayerInRect(
            context,
            CGRect {
                origin: point,
                size,
            },
            layer,
        );
    }
}

exports::exports! {
    functions {
        "CGPatternCreate" => CGPatternCreate,
        "CGPatternRelease" => CGPatternRelease,
        "CGPatternRetain" => CGPatternRetain,
        "CGFunctionCreate" => CGFunctionCreate,
        "CGFunctionRelease" => CGFunctionRelease,
        "CGFunctionRetain" => CGFunctionRetain,
        "CGShadingCreateAxial" => CGShadingCreateAxial,
        "CGShadingCreateRadial" => CGShadingCreateRadial,
        "CGShadingRelease" => CGShadingRelease,
        "CGShadingRetain" => CGShadingRetain,
        "CGLayerCreateWithContext" => CGLayerCreateWithContext,
        "CGLayerGetContext" => CGLayerGetContext,
        "CGLayerGetSize" => CGLayerGetSize,
        "CGLayerRelease" => CGLayerRelease,
        "CGLayerRetain" => CGLayerRetain,
        "CGContextDrawLayerInRect" => CGContextDrawLayerInRect,
        "CGContextDrawLayerAtPoint" => CGContextDrawLayerAtPoint,
    }
}
