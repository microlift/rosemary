//! `CGColor` and `CGColorSpace`: colours and the spaces they are read in.

#![allow(non_snake_case)]

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;

#[repr(C)]
pub(crate) struct Color {
    isa: *const c_void,
    pub(crate) components: [f64; 4],
}

#[repr(C)]
pub(crate) struct ColorSpace {
    isa: *const c_void,
    /// The `kCGColorSpace...` name it was made with, retained, or null for one
    /// made without a name.
    name: CFTypeRef,
    /// Colour components: 3 for RGB, 1 for gray, 0 for a pattern space.
    components: usize,
}

pub fn install() {
    corefoundation::base::register(c"__CGColor", None, size_of::<Color>(), true);
    let space =
        corefoundation::base::register(c"__CGColorSpace", None, size_of::<ColorSpace>(), true);
    space.replace_method(
        objc::sel::get("dealloc"),
        color_space_dealloc as *const c_void,
        c"v16@0:8",
    );
}

unsafe extern "C" fn color_space_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: `-dealloc` of a `__CGColorSpace`, whose body is `ColorSpace`.
    unsafe {
        let space = &mut *(receiver as *mut ColorSpace);
        let name = std::mem::replace(&mut space.name, std::ptr::null());
        if !name.is_null() {
            corefoundation::api::CFRelease(name);
        }
        objc::arc::finalize(receiver);
    }
}

pub(crate) fn color_of<'a>(color: CFTypeRef) -> Option<&'a Color> {
    let class = objc::class::lookup(c"__CGColor").expect("installed during bootstrap");
    // SAFETY: null or an object a guest received from this runtime.
    if color.is_null() || !unsafe { corefoundation::base::is_kind_of(color, class) } {
        return None;
    }
    // SAFETY: a `__CGColor`, whose body is `Color`.
    Some(unsafe { &*(color as *const Color) })
}

/// R, G, B, A from `components` read as the space's colour and an alpha.
///
/// # Safety
///
/// `components` holds `space_components + 1` values, or is null.
pub(crate) unsafe fn rgba_from(
    components: *const f64,
    space_components: usize,
) -> Option<[f64; 4]> {
    if components.is_null() {
        return None;
    }
    // SAFETY: the caller's count.
    let c = unsafe { std::slice::from_raw_parts(components, space_components + 1) };
    Some(match space_components {
        1 => [c[0], c[0], c[0], c[1]],
        _ => [c[0], c[1], c[2], c[3]],
    })
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
/// and alpha by how many the color space has. A space this runtime did not
/// make is taken as RGB.
pub unsafe extern "C" fn CGColorCreate(space: CFTypeRef, components: *const f64) -> CFTypeRef {
    let count = space_components(space).unwrap_or(3).max(1);
    // SAFETY: the space's components and an alpha.
    match unsafe { rgba_from(components, count) } {
        Some(rgba) => new_color(rgba),
        None => std::ptr::null(),
    }
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
        let key = corefoundation::base::interned_name(key) as CFTypeRef;
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
    *SPACE.get_or_init(|| unsafe { CGColorSpaceCreateDeviceRGB() } as usize) as CFTypeRef
}

/// The model of a space: `kCGColorSpaceModelMonochrome`, `RGB` or `Pattern`.
pub unsafe extern "C" fn CGColorSpaceGetModel(space: CFTypeRef) -> i32 {
    match space_components(space) {
        Some(1) => 0,
        Some(0) => 6,
        Some(_) => 1,
        None => -1,
    }
}

pub unsafe extern "C" fn CGColorSpaceCreateWithName(name: CFTypeRef) -> CFTypeRef {
    let gray = !name.is_null() && {
        // SAFETY: a string, as the parameter is declared.
        let text = unsafe { corefoundation::string::as_string(name) }
            .map(|text| text.utf8())
            .unwrap_or_default();
        text.windows(4).any(|w| w == b"Gray")
    };
    make_space(name, if gray { 1 } else { 3 })
}

fn make_space(name: CFTypeRef, components: usize) -> CFTypeRef {
    let class = objc::class::lookup(c"__CGColorSpace").expect("installed during bootstrap");
    // SAFETY: `ColorSpace` is the registered class layout.
    let space = unsafe { corefoundation::base::allocate::<ColorSpace>(class) };
    // SAFETY: a fresh allocation, and null or the caller's string.
    unsafe {
        (*space).name = match name.is_null() {
            true => name,
            false => corefoundation::api::CFRetain(name),
        };
        (*space).components = components;
    }
    space as CFTypeRef
}

pub unsafe extern "C" fn CGColorSpaceCreateDeviceGray() -> CFTypeRef {
    make_space(std::ptr::null(), 1)
}

/// `CGColorSpaceCreateWithPlatformColorSpace(const void *profile)`: a
/// ColorSync profile, which is RGB for every display.
pub unsafe extern "C" fn CGColorSpaceCreateWithPlatformColorSpace(
    _profile: *const c_void,
) -> CFTypeRef {
    make_space(std::ptr::null(), 3)
}

/// `CGColorSpaceCreatePattern(CGColorSpaceRef base)`: a space whose colours
/// are patterns. The base, for an uncoloured pattern, is not kept: every
/// colour here is RGB.
pub unsafe extern "C" fn CGColorSpaceCreatePattern(_base: CFTypeRef) -> CFTypeRef {
    make_space(std::ptr::null(), 0)
}

pub unsafe extern "C" fn CGColorSpaceGetNumberOfComponents(space: CFTypeRef) -> usize {
    color_space_of(space).map_or(0, |space| space.components)
}

/// The components of a space, or `None` for something that is not one.
pub fn space_components(space: CFTypeRef) -> Option<usize> {
    color_space_of(space).map(|space| space.components)
}

fn color_space_of<'a>(space: CFTypeRef) -> Option<&'a ColorSpace> {
    let class = objc::class::lookup(c"__CGColorSpace").expect("installed during bootstrap");
    // SAFETY: null or an object a guest received from this runtime.
    let is_space = !space.is_null() && unsafe { corefoundation::base::is_kind_of(space, class) };
    // SAFETY: checked to be a `__CGColorSpace`, whose body is `ColorSpace`.
    is_space.then(|| unsafe { &*(space as *const ColorSpace) })
}

/// `CFStringRef CGColorSpaceGetName(CGColorSpaceRef)`, not retained.
pub unsafe extern "C" fn CGColorSpaceGetName(space: CFTypeRef) -> CFTypeRef {
    color_space_of(space).map_or(std::ptr::null(), |space| space.name)
}

/// `CFStringRef CGColorSpaceCopyName(CGColorSpaceRef)`, retained.
pub unsafe extern "C" fn CGColorSpaceCopyName(space: CFTypeRef) -> CFTypeRef {
    // SAFETY: as `CGColorSpaceGetName`.
    let name = unsafe { CGColorSpaceGetName(space) };
    match name.is_null() {
        true => name,
        // SAFETY: a live string the space holds.
        false => unsafe { corefoundation::api::CFRetain(name) },
    }
}

/// `CFPropertyListRef CGColorSpaceCopyPropertyList(CGColorSpaceRef)`: a named
/// space is represented by its name. One made without a name - the device
/// space - has no representation to give, and gets NULL.
pub unsafe extern "C" fn CGColorSpaceCopyPropertyList(space: CFTypeRef) -> CFTypeRef {
    // SAFETY: as `CGColorSpaceCopyName`.
    unsafe { CGColorSpaceCopyName(space) }
}

pub unsafe extern "C" fn CGColorSpaceCreateDeviceRGB() -> CFTypeRef {
    make_space(std::ptr::null(), 3)
}

pub unsafe extern "C" fn CGColorSpaceRelease(space: CFTypeRef) {
    if !space.is_null() {
        unsafe { corefoundation::api::CFRelease(space) };
    }
}

/// The `CFStringRef` color names `CGColorGetConstantColor` takes.
pub fn data_symbols() -> Vec<(String, u64)> {
    ["kCGColorBlack", "kCGColorWhite", "kCGColorClear"]
        .into_iter()
        .map(|name| {
            let symbol = format!("_{name}");
            let value = corefoundation::base::interned_name(name) as usize as u64;
            let cell = corefoundation::base::global_cell(&symbol, value);
            (symbol, cell)
        })
        .collect()
}

exports::exports! {
    functions {
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
        "CGColorSpaceCreateWithName" => CGColorSpaceCreateWithName,
        "CGColorSpaceCreateDeviceRGB" => CGColorSpaceCreateDeviceRGB,
        "CGColorSpaceCreateDeviceGray" => CGColorSpaceCreateDeviceGray,
        "CGColorSpaceCreateWithPlatformColorSpace" => CGColorSpaceCreateWithPlatformColorSpace,
        "CGColorSpaceCreatePattern" => CGColorSpaceCreatePattern,
        "CGColorSpaceGetNumberOfComponents" => CGColorSpaceGetNumberOfComponents,
        "CGColorSpaceRelease" => CGColorSpaceRelease,
        "CGColorSpaceGetName" => CGColorSpaceGetName,
        "CGColorSpaceCopyName" => CGColorSpaceCopyName,
        "CGColorSpaceCopyPropertyList" => CGColorSpaceCopyPropertyList,
        "CGColorSpaceGetModel" => CGColorSpaceGetModel,
    }
}
