#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use foundation::geometry::{NSPoint, NSRect, NSSize};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CGAffineTransform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

pub const IDENTITY: CGAffineTransform = CGAffineTransform {
    a: 1.0,
    b: 0.0,
    c: 0.0,
    d: 1.0,
    tx: 0.0,
    ty: 0.0,
};

impl CGAffineTransform {
    /// `self` then `then`, as `CGAffineTransformConcat(self, then)`.
    pub fn then(self, then: CGAffineTransform) -> CGAffineTransform {
        CGAffineTransform {
            a: self.a * then.a + self.b * then.c,
            b: self.a * then.b + self.b * then.d,
            c: self.c * then.a + self.d * then.c,
            d: self.c * then.b + self.d * then.d,
            tx: self.tx * then.a + self.ty * then.c + then.tx,
            ty: self.tx * then.b + self.ty * then.d + then.ty,
        }
    }

    pub fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.tx,
            self.b * x + self.d * y + self.ty,
        )
    }
}

#[derive(Clone, Copy)]
struct GState {
    ctm: CGAffineTransform,
    /// Straight (not premultiplied) R, G, B, A.
    fill: [f64; 4],
    alpha: f64,
    text_matrix: CGAffineTransform,
    font_size: f64,
    antialias: bool,
    /// In device pixels: left, bottom, right, top.
    clip: Option<[f64; 4]>,
}

impl Default for GState {
    fn default() -> Self {
        GState {
            ctm: IDENTITY,
            fill: [0.0, 0.0, 0.0, 1.0],
            alpha: 1.0,
            text_matrix: IDENTITY,
            font_size: 0.0,
            antialias: true,
            clip: None,
        }
    }
}

struct State {
    current: GState,
    saved: Vec<GState>,
    /// Storage the context allocated because the caller passed none, which
    /// `data` points into; held only to keep it alive.
    _storage: Option<Vec<u8>>,
}

#[repr(C)]
struct BitmapContext {
    isa: *const c_void,
    data: *mut u8,
    width: usize,
    height: usize,
    bytes_per_row: usize,
    bits_per_component: usize,
    bitmap_info: u32,
    state: *mut State,
    /// Drawing goes to the view `-drawRect:` focused rather than to `data`,
    /// which is null: the context `-[NSGraphicsContext graphicsPort]` answers.
    focus: bool,
}

#[repr(C)]
struct Color {
    isa: *const c_void,
    components: [f64; 4],
}

#[repr(C)]
struct Path {
    isa: *const c_void,
    bounds: *mut Option<NSRect>,
}

pub fn install() {
    let context =
        corefoundation::base::register(c"__CGContext", None, size_of::<BitmapContext>(), true);
    context.replace_method(
        objc::sel::get("dealloc"),
        context_dealloc as *const c_void,
        c"v16@0:8",
    );
    corefoundation::base::register(c"__CGColor", None, size_of::<Color>(), true);
    let path = corefoundation::base::register(c"__CGPath", None, size_of::<Path>(), true);
    path.replace_method(
        objc::sel::get("dealloc"),
        path_dealloc as *const c_void,
        c"v16@0:8",
    );
    corefoundation::base::register(c"__CGPattern", None, size_of::<usize>(), true);
}

unsafe extern "C" fn context_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGContext`, whose state came from `Box::into_raw`.
    unsafe {
        let context = &mut *(receiver as *mut BitmapContext);
        if !context.state.is_null() {
            drop(Box::from_raw(context.state));
            context.state = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

unsafe extern "C" fn path_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGPath`, whose bounds came from `Box::into_raw`.
    unsafe {
        let path = &mut *(receiver as *mut Path);
        if !path.bounds.is_null() {
            drop(Box::from_raw(path.bounds));
            path.bounds = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

fn context_of<'a>(context: CFTypeRef) -> Option<&'a mut BitmapContext> {
    let class = objc::class::lookup(c"__CGContext").expect("installed during bootstrap");
    // SAFETY: null or an object a guest received from this runtime.
    if context.is_null() || !unsafe { corefoundation::base::is_kind_of(context, class) } {
        return None;
    }
    // SAFETY: a `__CGContext`, whose body is `BitmapContext`.
    Some(unsafe { &mut *(context as *mut BitmapContext) })
}

fn state_of<'a>(context: CFTypeRef) -> Option<&'a mut GState> {
    let context = context_of(context)?;
    // SAFETY: every context is created with a state.
    Some(unsafe { &mut (*context.state).current })
}

pub unsafe extern "C" fn CGBitmapContextCreate(
    data: *mut c_void,
    width: usize,
    height: usize,
    bits_per_component: usize,
    bytes_per_row: usize,
    _space: CFTypeRef,
    bitmap_info: u32,
) -> CFTypeRef {
    if width == 0 || height == 0 || bits_per_component != 8 {
        return std::ptr::null();
    }
    let bytes_per_row = match bytes_per_row {
        0 => width * 4,
        n => n,
    };
    if bytes_per_row < width {
        return std::ptr::null();
    }
    let mut owned = data.is_null().then(|| vec![0u8; bytes_per_row * height]);
    let data = owned
        .as_mut()
        .map_or(data.cast::<u8>(), |buffer| buffer.as_mut_ptr());
    let class = objc::class::lookup(c"__CGContext").expect("installed during bootstrap");
    // SAFETY: `BitmapContext` is the registered class layout.
    let context = unsafe { corefoundation::base::allocate::<BitmapContext>(class) };
    // SAFETY: a fresh allocation; the pixel storage is the caller's or owned
    // by the state, which lives as long as the context.
    unsafe {
        (*context).data = data;
        (*context).width = width;
        (*context).height = height;
        (*context).bytes_per_row = bytes_per_row;
        (*context).bits_per_component = bits_per_component;
        (*context).bitmap_info = bitmap_info;
        (*context).state = Box::into_raw(Box::new(State {
            current: GState::default(),
            saved: Vec::new(),
            _storage: owned,
        }));
    }
    context as CFTypeRef
}

thread_local! {
    /// The focus context, and the focus it was last answered for.
    static FOCUS_CONTEXT: std::cell::Cell<(usize, Option<(usize, usize)>)> =
        const { std::cell::Cell::new((0, None)) };
}

/// The `CGContextRef` of the view being drawn, or null with nothing focused.
///
/// One per thread, its graphics state reset whenever the focus changes, since
/// AppKit hands each `-drawRect:` a fresh state. Its user space is the view's
/// own, in points, so drawing lands where the same rectangle would through
/// `NSImage`.
pub(crate) fn focus_context() -> CFTypeRef {
    let Some(device) = crate::backing::focused_device() else {
        return std::ptr::null();
    };
    let focus = Some((device.key, device.depth));
    let (mut context, last) = FOCUS_CONTEXT.with(|cell| cell.get());
    if context == 0 {
        let class = objc::class::lookup(c"__CGContext").expect("installed during bootstrap");
        // SAFETY: `BitmapContext` is the registered class layout; the object
        // lives for the thread, so it is never released.
        let fresh = unsafe { corefoundation::base::allocate::<BitmapContext>(class) };
        unsafe {
            (*fresh).focus = true;
            (*fresh).state = Box::into_raw(Box::new(State {
                current: GState::default(),
                saved: Vec::new(),
                _storage: None,
            }));
        }
        context = fresh as usize;
    } else if last != focus {
        // SAFETY: the context made above, whose state is never freed.
        let state = unsafe { &mut *(*(context as *mut BitmapContext)).state };
        state.current = GState::default();
        state.saved.clear();
    }
    FOCUS_CONTEXT.with(|cell| cell.set((context, focus)));
    context as CFTypeRef
}

pub unsafe extern "C" fn CGBitmapContextGetData(context: CFTypeRef) -> *mut c_void {
    context_of(context).map_or(std::ptr::null_mut(), |context| context.data.cast())
}

pub unsafe extern "C" fn CGBitmapContextGetWidth(context: CFTypeRef) -> usize {
    context_of(context).map_or(0, |context| context.width)
}

pub unsafe extern "C" fn CGBitmapContextGetHeight(context: CFTypeRef) -> usize {
    context_of(context).map_or(0, |context| context.height)
}

pub unsafe extern "C" fn CGContextRetain(context: CFTypeRef) -> CFTypeRef {
    if context.is_null() {
        return context;
    }
    unsafe { corefoundation::api::CFRetain(context) }
}

pub unsafe extern "C" fn CGContextRelease(context: CFTypeRef) {
    if !context.is_null() {
        unsafe { corefoundation::api::CFRelease(context) };
    }
}

pub unsafe extern "C" fn CGContextSaveGState(context: CFTypeRef) {
    if let Some(context) = context_of(context) {
        // SAFETY: every context is created with a state.
        let state = unsafe { &mut *context.state };
        state.saved.push(state.current);
    }
}

pub unsafe extern "C" fn CGContextRestoreGState(context: CFTypeRef) {
    if let Some(context) = context_of(context) {
        let state = unsafe { &mut *context.state };
        if let Some(saved) = state.saved.pop() {
            state.current = saved;
        }
    }
}

pub unsafe extern "C" fn CGContextConcatCTM(context: CFTypeRef, transform: CGAffineTransform) {
    if let Some(state) = state_of(context) {
        state.ctm = transform.then(state.ctm);
    }
}

pub unsafe extern "C" fn CGContextGetCTM(context: CFTypeRef) -> CGAffineTransform {
    state_of(context).map_or(IDENTITY, |state| state.ctm)
}

pub unsafe extern "C" fn CGContextTranslateCTM(context: CFTypeRef, x: f64, y: f64) {
    unsafe { CGContextConcatCTM(context, CGAffineTransformMakeTranslation(x, y)) };
}

pub unsafe extern "C" fn CGContextScaleCTM(context: CFTypeRef, x: f64, y: f64) {
    unsafe { CGContextConcatCTM(context, CGAffineTransformMakeScale(x, y)) };
}

pub unsafe extern "C" fn CGContextSetRGBFillColor(
    context: CFTypeRef,
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
) {
    if let Some(state) = state_of(context) {
        state.fill = [red, green, blue, alpha];
    }
}

pub unsafe extern "C" fn CGContextSetGrayFillColor(context: CFTypeRef, gray: f64, alpha: f64) {
    unsafe { CGContextSetRGBFillColor(context, gray, gray, gray, alpha) };
}

fn color_of<'a>(color: CFTypeRef) -> Option<&'a Color> {
    let class = objc::class::lookup(c"__CGColor").expect("installed during bootstrap");
    // SAFETY: null or an object a guest received from this runtime.
    if color.is_null() || !unsafe { corefoundation::base::is_kind_of(color, class) } {
        return None;
    }
    // SAFETY: a `__CGColor`, whose body is `Color`.
    Some(unsafe { &*(color as *const Color) })
}

pub unsafe extern "C" fn CGContextSetFillColorWithColor(context: CFTypeRef, color: CFTypeRef) {
    if let (Some(state), Some(color)) = (state_of(context), color_of(color)) {
        state.fill = color.components;
    }
}

pub unsafe extern "C" fn CGContextSetAlpha(context: CFTypeRef, alpha: f64) {
    if let Some(state) = state_of(context) {
        state.alpha = alpha.clamp(0.0, 1.0);
    }
}

pub unsafe extern "C" fn CGContextSetShouldAntialias(context: CFTypeRef, antialias: u8) {
    if let Some(state) = state_of(context) {
        state.antialias = antialias != 0;
    }
}

pub unsafe extern "C" fn CGContextSetFontSize(context: CFTypeRef, size: f64) {
    if let Some(state) = state_of(context) {
        state.font_size = size;
    }
}

pub unsafe extern "C" fn CGContextSetTextMatrix(context: CFTypeRef, matrix: CGAffineTransform) {
    if let Some(state) = state_of(context) {
        state.text_matrix = matrix;
    }
}

pub unsafe extern "C" fn CGContextGetTextMatrix(context: CFTypeRef) -> CGAffineTransform {
    state_of(context).map_or(IDENTITY, |state| state.text_matrix)
}

pub unsafe extern "C" fn CGContextSetTextPosition(context: CFTypeRef, x: f64, y: f64) {
    if let Some(state) = state_of(context) {
        state.text_matrix.tx = x;
        state.text_matrix.ty = y;
    }
}

pub unsafe extern "C" fn CGContextClipToRect(context: CFTypeRef, rect: NSRect) {
    let Some(state) = state_of(context) else {
        return;
    };
    let [left, bottom, right, top] = device_bounds(state.ctm, rect);
    state.clip = Some(match state.clip {
        Some([l, b, r, t]) => [left.max(l), bottom.max(b), right.min(r), top.min(t)],
        None => [left, bottom, right, top],
    });
}

fn device_bounds(ctm: CGAffineTransform, rect: NSRect) -> [f64; 4] {
    let (x0, y0) = (rect.origin.x, rect.origin.y);
    let (x1, y1) = (x0 + rect.size.width, y0 + rect.size.height);
    let corners = [
        ctm.apply(x0, y0),
        ctm.apply(x1, y0),
        ctm.apply(x0, y1),
        ctm.apply(x1, y1),
    ];
    corners.iter().fold(
        [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
        |[l, b, r, t], &(x, y)| [l.min(x), b.min(y), r.max(x), t.max(y)],
    )
}

pub unsafe extern "C" fn CGContextFillRect(context: CFTypeRef, rect: NSRect) {
    let Some(ctx) = context_of(context) else {
        return;
    };
    // SAFETY: every context is created with a state.
    let state = unsafe { (*ctx.state).current };
    let [left, bottom, right, top] = device_bounds(state.ctm, rect);
    let (x0, x1) = (left.round() as i64, right.round() as i64);
    let (y0, y1) = (bottom.round() as i64, top.round() as i64);
    for y in y0..y1 {
        for x in x0..x1 {
            blend(ctx, &state, x, y, 1.0);
        }
    }
}

pub unsafe extern "C" fn CGContextClearRect(context: CFTypeRef, rect: NSRect) {
    let Some(ctx) = context_of(context) else {
        return;
    };
    let state = unsafe { (*ctx.state).current };
    let [left, bottom, right, top] = device_bounds(state.ctm, rect);
    for y in bottom.round() as i64..top.round() as i64 {
        for x in left.round() as i64..right.round() as i64 {
            if let Some(pixel) = pixel(ctx, &state, x, y) {
                pixel.fill(0);
            }
        }
    }
}

/// Draws an image into a bitmap context, applying the current transform and
/// nearest-neighbor scaling. Image pixels arrive premultiplied BGRA from the
/// shared CoreGraphics representation.
pub unsafe extern "C" fn CGContextDrawImage(context: CFTypeRef, rect: NSRect, image: CFTypeRef) {
    let Some(ctx) = context_of(context) else {
        return;
    };
    let Some((source_width, source_height, source)) = crate::graphics::image_bgra(image) else {
        return;
    };
    if rect.size.width == 0.0 || rect.size.height == 0.0 {
        return;
    }
    // SAFETY: every context is created with a state.
    let state = unsafe { (*ctx.state).current };
    if ctx.focus {
        let [left, bottom, right, top] = device_bounds(state.ctm, rect);
        const SOURCE_OVER: usize = 2;
        crate::backing::draw_image(
            &source,
            source_width,
            source_height,
            NSSize {
                width: source_width as f64,
                height: source_height as f64,
            },
            NSRect {
                origin: NSPoint { x: left, y: bottom },
                size: NSSize {
                    width: right - left,
                    height: top - bottom,
                },
            },
            NSRect::default(),
            SOURCE_OVER,
            state.alpha,
            false,
        );
        return;
    }
    let transform = state.ctm;
    let determinant = transform.a * transform.d - transform.b * transform.c;
    if determinant == 0.0 {
        return;
    }
    let [left, bottom, right, top] = device_bounds(transform, rect);
    for y in bottom.floor() as i64..top.ceil() as i64 {
        for x in left.floor() as i64..right.ceil() as i64 {
            // Map the device-pixel center back into the untransformed image
            // rectangle. This preserves vertical flips used by cocos2d-x.
            let dx = x as f64 + 0.5 - transform.tx;
            let dy = y as f64 + 0.5 - transform.ty;
            let user_x = (transform.d * dx - transform.c * dy) / determinant;
            let user_y = (-transform.b * dx + transform.a * dy) / determinant;
            let u = (user_x - rect.origin.x) / rect.size.width;
            let v = (user_y - rect.origin.y) / rect.size.height;
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            let source_x = (u * source_width as f64) as usize;
            let source_y = ((1.0 - v) * source_height as f64) as usize;
            let source_y = source_y.min(source_height - 1);
            let at = (source_y * source_width + source_x) * 4;
            write_bgra(ctx, &state, x, y, source[at..at + 4].try_into().unwrap());
        }
    }
}

/// The bytes of the pixel at device `(x, y)`, or `None` outside the bitmap or
/// the clip.
fn pixel<'a>(ctx: &BitmapContext, state: &GState, x: i64, y: i64) -> Option<&'a mut [u8]> {
    if let Some([l, b, r, t]) = state.clip {
        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
        if px < l || px >= r || py < b || py >= t {
            return None;
        }
    }
    if x < 0 || y < 0 || x as usize >= ctx.width || y as usize >= ctx.height {
        return None;
    }
    let channels = (ctx.bytes_per_row / ctx.width).clamp(1, 4);
    let row = ctx.height - 1 - y as usize;
    let at = row * ctx.bytes_per_row + x as usize * channels;
    // SAFETY: the bitmap is `bytes_per_row * height` bytes, and `at` is a
    // pixel start inside it.
    Some(unsafe { std::slice::from_raw_parts_mut(ctx.data.add(at), channels) })
}

/// Writes one premultiplied BGRA source pixel in the context's component
/// order. The texture-loading path clears its destination before drawing, so
/// replacing rather than blending also avoids rounding already-premultiplied
/// channels a second time.
fn write_bgra(ctx: &BitmapContext, state: &GState, x: i64, y: i64, bgra: [u8; 4]) {
    let Some(pixel) = pixel(ctx, state, x, y) else {
        return;
    };
    if pixel.len() == 1 {
        pixel[0] = bgra[3];
        return;
    }
    let [b, g, r, a] = bgra;
    let info = ctx.bitmap_info & ALPHA_INFO_MASK;
    let alpha_first = matches!(info, 2 | 4 | 6);
    let mut components = match alpha_first {
        true => [a, r, g, b],
        false => [r, g, b, a],
    };
    if ctx.bitmap_info & BYTE_ORDER_MASK == BYTE_ORDER_32_LITTLE && pixel.len() == 4 {
        components.reverse();
    }
    pixel.copy_from_slice(&components[..pixel.len()]);
}

const ALPHA_INFO_MASK: u32 = 0x1f;
const BYTE_ORDER_32_LITTLE: u32 = 0x2000;
const BYTE_ORDER_MASK: u32 = 0x7000;

/// Blend the fill color into device pixel `(x, y)` with `coverage`, source over.
fn blend(ctx: &BitmapContext, state: &GState, x: i64, y: i64, coverage: f64) {
    let Some(pixel) = pixel(ctx, state, x, y) else {
        return;
    };
    let [r, g, b, a] = state.fill;
    let alpha = (a * state.alpha * coverage).clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let over = |destination: u8, source: f64| -> u8 {
        (source * 255.0 * alpha + f64::from(destination) * (1.0 - alpha)).round() as u8
    };
    if pixel.len() == 1 {
        // Alpha-only, or gray with no alpha, which both read as coverage of
        // the fill's luminance.
        let value = match ctx.bitmap_info & ALPHA_INFO_MASK {
            7 => 1.0,
            _ => 0.299 * r + 0.587 * g + 0.114 * b,
        };
        pixel[0] = over(pixel[0], value);
        return;
    }
    let info = ctx.bitmap_info & ALPHA_INFO_MASK;
    let little = ctx.bitmap_info & BYTE_ORDER_MASK == BYTE_ORDER_32_LITTLE;
    let alpha_first = matches!(info, 2 | 4 | 6);
    // Component order as written, before the byte order reverses it.
    let mut order = match alpha_first {
        true => [3usize, 0, 1, 2],
        false => [0usize, 1, 2, 3],
    };
    if little && pixel.len() == 4 {
        order.reverse();
    }
    let source = [r, g, b, 1.0];
    for (slot, &component) in order.iter().enumerate().take(pixel.len()) {
        pixel[slot] = over(pixel[slot], source[component]);
    }
}

/// Blend a coverage mask whose top-left pixel is at device `(left, top)`,
/// rows running down, into the context with its fill color.
pub(crate) fn fill_coverage(
    context: CFTypeRef,
    left: i64,
    top: i64,
    width: usize,
    coverage: &[f32],
) {
    let Some(ctx) = context_of(context) else {
        return;
    };
    // SAFETY: every context is created with a state.
    let state = unsafe { (*ctx.state).current };
    for (at, &value) in coverage.iter().enumerate() {
        if value <= 0.0 {
            continue;
        }
        let x = left + (at % width) as i64;
        let y = top - (at / width) as i64;
        let value = match state.antialias {
            true => f64::from(value.min(1.0)),
            false => f64::from(u8::from(value >= 0.5)),
        };
        blend(ctx, &state, x, y, value);
    }
}

/// The transform from text space to device space, and the font size set on
/// the context.
pub(crate) fn text_to_device(context: CFTypeRef) -> Option<CGAffineTransform> {
    let state = state_of(context)?;
    Some(state.text_matrix.then(state.ctm))
}

pub unsafe extern "C" fn CGAffineTransformMake(
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
) -> CGAffineTransform {
    CGAffineTransform { a, b, c, d, tx, ty }
}

pub unsafe extern "C" fn CGAffineTransformMakeScale(x: f64, y: f64) -> CGAffineTransform {
    CGAffineTransform {
        a: x,
        d: y,
        ..IDENTITY
    }
}

pub unsafe extern "C" fn CGAffineTransformMakeTranslation(x: f64, y: f64) -> CGAffineTransform {
    CGAffineTransform {
        tx: x,
        ty: y,
        ..IDENTITY
    }
}

pub unsafe extern "C" fn CGAffineTransformConcat(
    first: CGAffineTransform,
    second: CGAffineTransform,
) -> CGAffineTransform {
    first.then(second)
}

pub unsafe extern "C" fn CGAffineTransformScale(
    transform: CGAffineTransform,
    x: f64,
    y: f64,
) -> CGAffineTransform {
    unsafe { CGAffineTransformMakeScale(x, y) }.then(transform)
}

pub unsafe extern "C" fn CGAffineTransformTranslate(
    transform: CGAffineTransform,
    x: f64,
    y: f64,
) -> CGAffineTransform {
    unsafe { CGAffineTransformMakeTranslation(x, y) }.then(transform)
}

pub unsafe extern "C" fn CGAffineTransformInvert(t: CGAffineTransform) -> CGAffineTransform {
    let determinant = t.a * t.d - t.b * t.c;
    if determinant == 0.0 {
        return t;
    }
    let (a, b, c, d) = (
        t.d / determinant,
        -t.b / determinant,
        -t.c / determinant,
        t.a / determinant,
    );
    CGAffineTransform {
        a,
        b,
        c,
        d,
        tx: -(t.tx * a + t.ty * c),
        ty: -(t.tx * b + t.ty * d),
    }
}

pub unsafe extern "C" fn CGAffineTransformIsIdentity(t: CGAffineTransform) -> u8 {
    u8::from(t == IDENTITY)
}

pub unsafe extern "C" fn CGRectApplyAffineTransform(rect: NSRect, t: CGAffineTransform) -> NSRect {
    let [left, bottom, right, top] = device_bounds(t, rect);
    NSRect {
        origin: NSPoint { x: left, y: bottom },
        size: NSSize {
            width: right - left,
            height: top - bottom,
        },
    }
}

fn new_color(components: [f64; 4]) -> CFTypeRef {
    let class = objc::class::lookup(c"__CGColor").expect("installed during bootstrap");
    // SAFETY: `Color` is the registered class layout.
    let color = unsafe { corefoundation::base::allocate::<Color>(class) };
    // SAFETY: a fresh allocation.
    unsafe { (*color).components = components };
    color as CFTypeRef
}

/// `CGColorCreate`, reading the components as gray and alpha or as R, G, B
/// and alpha by how many the color space has - which, with no color space
/// objects that know their model, is the component count of a device RGB
/// space unless the caller made a gray one.
pub unsafe extern "C" fn CGColorCreate(_space: CFTypeRef, components: *const f64) -> CFTypeRef {
    if components.is_null() {
        return std::ptr::null();
    }
    // SAFETY: an RGB color has four components.
    let c = unsafe { std::slice::from_raw_parts(components, 4) };
    new_color([c[0], c[1], c[2], c[3]])
}

pub unsafe extern "C" fn CGColorCreateGenericRGB(r: f64, g: f64, b: f64, a: f64) -> CFTypeRef {
    new_color([r, g, b, a])
}

pub unsafe extern "C" fn CGColorCreateGenericGray(gray: f64, alpha: f64) -> CFTypeRef {
    new_color([gray, gray, gray, alpha])
}

pub unsafe extern "C" fn CGColorRelease(color: CFTypeRef) {
    if !color.is_null() {
        unsafe { corefoundation::api::CFRelease(color) };
    }
}

pub unsafe extern "C" fn CGColorGetComponents(color: CFTypeRef) -> *const f64 {
    color_of(color).map_or(std::ptr::null(), |color| color.components.as_ptr())
}

pub unsafe extern "C" fn CGColorGetNumberOfComponents(color: CFTypeRef) -> usize {
    color_of(color).map_or(0, |_| 4)
}

pub unsafe extern "C" fn CGColorGetAlpha(color: CFTypeRef) -> f64 {
    color_of(color).map_or(0.0, |color| color.components[3])
}

/// `kCGColorBlack`, `kCGColorWhite` and `kCGColorClear` by name, for the
/// `CFStringRef` constants [`data_symbols`] publishes.
pub unsafe extern "C" fn CGColorGetConstantColor(name: CFTypeRef) -> CFTypeRef {
    static COLORS: std::sync::OnceLock<[usize; 3]> = std::sync::OnceLock::new();
    let colors = COLORS.get_or_init(|| {
        [
            new_color([0.0, 0.0, 0.0, 1.0]) as usize,
            new_color([1.0, 1.0, 1.0, 1.0]) as usize,
            new_color([0.0, 0.0, 0.0, 0.0]) as usize,
        ]
    });
    for (at, key) in ["kCGColorBlack", "kCGColorWhite", "kCGColorClear"]
        .into_iter()
        .enumerate()
    {
        let key = foundation::notification::name_object(key) as CFTypeRef;
        // SAFETY: both are strings.
        if !name.is_null() && unsafe { corefoundation::api::CFEqual(name, key) } != 0 {
            return colors[at] as CFTypeRef;
        }
    }
    std::ptr::null()
}

pub unsafe extern "C" fn CGColorGetTypeID() -> corefoundation::base::CFTypeID {
    corefoundation::base::type_id_of(
        objc::class::lookup(c"__CGColor").expect("installed during bootstrap"),
    )
}

/// Every colour here is kept as RGBA, so its space is device RGB.
pub unsafe extern "C" fn CGColorGetColorSpace(color: CFTypeRef) -> CFTypeRef {
    static SPACE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    if color_of(color).is_none() {
        return std::ptr::null();
    }
    // SAFETY: a fresh space, kept for the process as Apple's shared ones are.
    *SPACE.get_or_init(|| unsafe { crate::graphics::CGColorSpaceCreateDeviceRGB() } as usize)
        as CFTypeRef
}

/// `kCGColorSpaceModelRGB` for any space, since only RGB ones are made here.
pub unsafe extern "C" fn CGColorSpaceGetModel(space: CFTypeRef) -> i32 {
    match space.is_null() {
        true => -1,
        false => 1,
    }
}

// Paths are not kept on a context (see `CGContextAddPath`), so neither is a rect.
unsafe extern "C" fn ignored_context_rect(_context: CFTypeRef, _rect: NSRect) {}

pub unsafe extern "C" fn CGPathCreateMutable() -> CFTypeRef {
    let class = objc::class::lookup(c"__CGPath").expect("installed during bootstrap");
    // SAFETY: `Path` is the registered class layout.
    let path = unsafe { corefoundation::base::allocate::<Path>(class) };
    // SAFETY: a fresh allocation.
    unsafe { (*path).bounds = Box::into_raw(Box::new(None)) };
    path as CFTypeRef
}

pub unsafe extern "C" fn CGPathRelease(path: CFTypeRef) {
    if !path.is_null() {
        unsafe { corefoundation::api::CFRelease(path) };
    }
}

fn extend_path(path: CFTypeRef, transform: *const CGAffineTransform, rect: NSRect) {
    let class = objc::class::lookup(c"__CGPath").expect("installed during bootstrap");
    // SAFETY: null or an object a guest received from this runtime.
    if path.is_null() || !unsafe { corefoundation::base::is_kind_of(path, class) } {
        return;
    }
    let rect = match transform.is_null() {
        true => rect,
        // SAFETY: the caller's transform.
        false => unsafe { CGRectApplyAffineTransform(rect, *transform) },
    };
    // SAFETY: a `__CGPath`, created with bounds.
    let bounds = unsafe { &mut *(*(path as *mut Path)).bounds };
    *bounds = Some(match *bounds {
        None => rect,
        Some(old) => {
            let left = old.origin.x.min(rect.origin.x);
            let bottom = old.origin.y.min(rect.origin.y);
            let right = (old.origin.x + old.size.width).max(rect.origin.x + rect.size.width);
            let top = (old.origin.y + old.size.height).max(rect.origin.y + rect.size.height);
            NSRect {
                origin: NSPoint { x: left, y: bottom },
                size: NSSize {
                    width: right - left,
                    height: top - bottom,
                },
            }
        }
    });
}

fn point_rect(x: f64, y: f64) -> NSRect {
    NSRect {
        origin: NSPoint { x, y },
        size: NSSize::default(),
    }
}

pub unsafe extern "C" fn CGPathMoveToPoint(
    path: CFTypeRef,
    transform: *const CGAffineTransform,
    x: f64,
    y: f64,
) {
    extend_path(path, transform, point_rect(x, y));
}

pub unsafe extern "C" fn CGPathAddLineToPoint(
    path: CFTypeRef,
    transform: *const CGAffineTransform,
    x: f64,
    y: f64,
) {
    extend_path(path, transform, point_rect(x, y));
}

pub unsafe extern "C" fn CGPathAddQuadCurveToPoint(
    path: CFTypeRef,
    transform: *const CGAffineTransform,
    cx: f64,
    cy: f64,
    x: f64,
    y: f64,
) {
    extend_path(path, transform, point_rect(cx, cy));
    extend_path(path, transform, point_rect(x, y));
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGPathAddCurveToPoint(
    path: CFTypeRef,
    transform: *const CGAffineTransform,
    c1x: f64,
    c1y: f64,
    c2x: f64,
    c2y: f64,
    x: f64,
    y: f64,
) {
    extend_path(path, transform, point_rect(c1x, c1y));
    extend_path(path, transform, point_rect(c2x, c2y));
    extend_path(path, transform, point_rect(x, y));
}

pub unsafe extern "C" fn CGPathAddRect(
    path: CFTypeRef,
    transform: *const CGAffineTransform,
    rect: NSRect,
) {
    extend_path(path, transform, rect);
}

#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn CGPathAddArc(
    path: CFTypeRef,
    transform: *const CGAffineTransform,
    x: f64,
    y: f64,
    radius: f64,
    _start: f64,
    _end: f64,
    _clockwise: u8,
) {
    extend_path(
        path,
        transform,
        NSRect {
            origin: NSPoint {
                x: x - radius,
                y: y - radius,
            },
            size: NSSize {
                width: radius * 2.0,
                height: radius * 2.0,
            },
        },
    );
}

pub unsafe extern "C" fn CGPathCloseSubpath(_path: CFTypeRef) {}

pub unsafe extern "C" fn CGPathGetBoundingBox(path: CFTypeRef) -> NSRect {
    let class = objc::class::lookup(c"__CGPath").expect("installed during bootstrap");
    // SAFETY: null or an object a guest received from this runtime.
    if path.is_null() || !unsafe { corefoundation::base::is_kind_of(path, class) } {
        return NSRect::default();
    }
    // SAFETY: a `__CGPath`, created with bounds.
    unsafe { (*(*(path as *const Path)).bounds).unwrap_or_default() }
}

/// `CGPathApply` visits every element; a path here records only its bounds,
/// so there are none to visit.
pub unsafe extern "C" fn CGPathApply(
    _path: CFTypeRef,
    _info: *mut c_void,
    _function: *const c_void,
) {
}

unsafe extern "C" fn ignored_context(_context: CFTypeRef) {}

unsafe extern "C" fn ignored_context_int(_context: CFTypeRef, _value: i32) {}

unsafe extern "C" fn ignored_context_f64(_context: CFTypeRef, _value: f64) {}

unsafe extern "C" fn ignored_context_pointer(_context: CFTypeRef, _value: *const c_void) {}

unsafe extern "C" fn ignored_line_dash(
    _context: CFTypeRef,
    _phase: f64,
    _lengths: *const f64,
    _count: usize,
) {
}

unsafe extern "C" fn ignored_pattern_phase(_context: CFTypeRef, _phase: NSSize) {}

unsafe extern "C" fn ignored_fill_pattern(
    _context: CFTypeRef,
    _pattern: CFTypeRef,
    _components: *const f64,
) {
}

unsafe extern "C" fn null_object() -> CFTypeRef {
    std::ptr::null()
}

unsafe extern "C" fn release_object(object: CFTypeRef) {
    if !object.is_null() {
        unsafe { corefoundation::api::CFRelease(object) };
    }
}

exports::exports! {
    functions {
        "CGBitmapContextCreate" => CGBitmapContextCreate,
        "CGBitmapContextGetData" => CGBitmapContextGetData,
        "CGBitmapContextGetWidth" => CGBitmapContextGetWidth,
        "CGBitmapContextGetHeight" => CGBitmapContextGetHeight,
        "CGContextRetain" => CGContextRetain,
        "CGContextRelease" => CGContextRelease,
        "CGContextSaveGState" => CGContextSaveGState,
        "CGContextRestoreGState" => CGContextRestoreGState,
        "CGContextConcatCTM" => CGContextConcatCTM,
        "CGContextGetCTM" => CGContextGetCTM,
        "CGContextTranslateCTM" => CGContextTranslateCTM,
        "CGContextScaleCTM" => CGContextScaleCTM,
        "CGContextSetRGBFillColor" => CGContextSetRGBFillColor,
        "CGContextSetGrayFillColor" => CGContextSetGrayFillColor,
        "CGContextSetFillColorWithColor" => CGContextSetFillColorWithColor,
        "CGContextSetAlpha" => CGContextSetAlpha,
        "CGContextSetShouldAntialias" => CGContextSetShouldAntialias,
        "CGContextSetFontSize" => CGContextSetFontSize,
        "CGContextSetTextMatrix" => CGContextSetTextMatrix,
        "CGContextGetTextMatrix" => CGContextGetTextMatrix,
        "CGContextSetTextPosition" => CGContextSetTextPosition,
        "CGContextClipToRect" => CGContextClipToRect,
        "CGContextFillRect" => CGContextFillRect,
        "CGContextClearRect" => CGContextClearRect,
        "CGContextDrawImage" => CGContextDrawImage,
        "CGContextSetShouldSmoothFonts" | "CGContextSetInterpolationQuality" | "CGContextSetLineCap" | "CGContextSetLineJoin" | "CGContextSetTextDrawingMode" | "CGContextSetBlendMode" => ignored_context_int,
        "CGContextSetLineWidth" => ignored_context_f64,
        "CGContextSetLineDash" => ignored_line_dash,
        "CGContextSetStrokeColorWithColor" | "CGContextSetFillColorSpace" | "CGContextAddPath" | "CGContextDrawShading" => ignored_context_pointer,
        "CGContextSetFillPattern" => ignored_fill_pattern,
        "CGContextSetPatternPhase" => ignored_pattern_phase,
        "CGContextBeginPath" | "CGContextFillPath" | "CGContextEOFillPath" | "CGContextStrokePath" | "CGContextClip" | "CGContextEOClip" | "CGContextSynchronize" => ignored_context,
        "CGAffineTransformMake" => CGAffineTransformMake,
        "CGAffineTransformMakeScale" => CGAffineTransformMakeScale,
        "CGAffineTransformMakeTranslation" => CGAffineTransformMakeTranslation,
        "CGAffineTransformConcat" => CGAffineTransformConcat,
        "CGAffineTransformScale" => CGAffineTransformScale,
        "CGAffineTransformTranslate" => CGAffineTransformTranslate,
        "CGAffineTransformInvert" => CGAffineTransformInvert,
        "CGAffineTransformIsIdentity" => CGAffineTransformIsIdentity,
        "CGRectApplyAffineTransform" => CGRectApplyAffineTransform,
        "CGColorCreate" => CGColorCreate,
        "CGColorCreateGenericRGB" => CGColorCreateGenericRGB,
        "CGColorCreateGenericGray" => CGColorCreateGenericGray,
        "CGColorRelease" => CGColorRelease,
        "CGColorGetComponents" => CGColorGetComponents,
        "CGColorGetNumberOfComponents" => CGColorGetNumberOfComponents,
        "CGColorGetAlpha" => CGColorGetAlpha,
        "CGColorGetConstantColor" => CGColorGetConstantColor,
        "CGColorGetTypeID" => CGColorGetTypeID,
        "CGColorGetColorSpace" => CGColorGetColorSpace,
        "CGColorSpaceGetModel" => CGColorSpaceGetModel,
        "CGContextAddRect" | "CGContextAddEllipseInRect" => ignored_context_rect,
        "CGColorSpaceCreatePattern" | "CGPatternCreate" => null_object,
        "CGPatternRelease" | "CGShadingRelease" => release_object,
        "CGPathCreateMutable" => CGPathCreateMutable,
        "CGPathRelease" => CGPathRelease,
        "CGPathMoveToPoint" => CGPathMoveToPoint,
        "CGPathAddLineToPoint" => CGPathAddLineToPoint,
        "CGPathAddQuadCurveToPoint" => CGPathAddQuadCurveToPoint,
        "CGPathAddCurveToPoint" => CGPathAddCurveToPoint,
        "CGPathAddRect" => CGPathAddRect,
        "CGPathAddArc" => CGPathAddArc,
        "CGPathCloseSubpath" => CGPathCloseSubpath,
        "CGPathGetBoundingBox" => CGPathGetBoundingBox,
        "CGPathApply" => CGPathApply,
    }
}

/// The `CFStringRef` color names `CGColorGetConstantColor` takes.
pub fn data_symbols() -> Vec<(String, u64)> {
    ["kCGColorBlack", "kCGColorWhite", "kCGColorClear"]
        .into_iter()
        .map(|name| {
            let symbol = format!("_{name}");
            let value = foundation::notification::name_object(name) as usize as u64;
            let cell = corefoundation::base::global_cell(&symbol, value);
            (symbol, cell)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_an_image_through_a_vertical_context_flip() {
        crate::bootstrap();
        let image = crate::graphics::image_from_bitmap(crate::raster::Bitmap {
            width: 1,
            height: 2,
            pixels: vec![255, 0, 0, 255, 0, 255, 0, 255],
        });
        let mut pixels = [0u8; 8];
        // SAFETY: `pixels` holds two writable RGBA rows and `image` is live.
        unsafe {
            let context =
                CGBitmapContextCreate(pixels.as_mut_ptr().cast(), 1, 2, 8, 4, std::ptr::null(), 3);
            CGContextTranslateCTM(context, 0.0, 2.0);
            CGContextScaleCTM(context, 1.0, -1.0);
            CGContextDrawImage(
                context,
                NSRect {
                    origin: NSPoint { x: 0.0, y: 0.0 },
                    size: NSSize {
                        width: 1.0,
                        height: 2.0,
                    },
                },
                image,
            );
            CGContextRelease(context);
            crate::graphics::CGImageRelease(image);
        }
        assert_eq!(pixels, [0, 255, 0, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn fills_a_translated_rect_in_a_little_endian_bitmap() {
        crate::bootstrap();
        let mut pixels = [0u32; 4];
        // SAFETY: `pixels` is two writable rows of two pixels.
        unsafe {
            let context = CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                2,
                2,
                8,
                8,
                std::ptr::null(),
                0x2006,
            );
            CGContextSetRGBFillColor(context, 1.0, 0.0, 0.0, 1.0);
            CGContextTranslateCTM(context, 1.0, 1.0);
            CGContextFillRect(
                context,
                NSRect {
                    origin: NSPoint { x: 0.0, y: 0.0 },
                    size: NSSize {
                        width: 1.0,
                        height: 1.0,
                    },
                },
            );
            CGContextRelease(context);
        }
        // Device (1, 1) is the top row's second pixel, 0xAARRGGBB with the
        // skipped byte opaque as Qt's RGB32 images need it.
        assert_eq!(pixels, [0, 0xffff_0000, 0, 0]);
    }

    #[test]
    fn saved_state_restores_the_transform() {
        crate::bootstrap();
        // SAFETY: a context over its own storage.
        unsafe {
            let context =
                CGBitmapContextCreate(std::ptr::null_mut(), 4, 4, 8, 16, std::ptr::null(), 1);
            CGContextSaveGState(context);
            CGContextScaleCTM(context, 2.0, 3.0);
            assert_eq!(CGContextGetCTM(context).d, 3.0);
            CGContextRestoreGState(context);
            assert_eq!(CGContextGetCTM(context), IDENTITY);
            let inverse = CGAffineTransformInvert(CGAffineTransformMakeScale(2.0, 4.0));
            assert_eq!(inverse.a, 0.5);
            assert_eq!(inverse.d, 0.25);
            CGContextRelease(context);
        }
    }
}
