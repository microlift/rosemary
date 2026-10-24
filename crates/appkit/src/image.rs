use std::ffi::c_void;
use std::{cell::RefCell, ptr};

use foundation::geometry::{NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct Image {
    isa: *const c_void,
    /// The bytes, retained as the `NSData` the guest handed over.
    data: Id,
    /// The representations added to it, as a retained `NSMutableArray`, or nil
    /// for the usual image that was made from data and has none.
    reps: Id,
    size: NSSize,
    /// A retained `CGImage` for an image made with `-initWithCGImage:size:`,
    /// which is the one kind this module can draw.
    cg_image: *const c_void,
    /// `-setTemplate:`. Only read back: nothing here tints an image.
    template: usize,
    /// RGBA pixels for an image made with `-initWithSize:` and subsequently
    /// locked for offscreen drawing.
    focused_pixels: *mut Vec<u8>,
    /// The copied `BOOL (^)(NSRect)` of `+imageWithSize:flipped:drawingHandler:`,
    /// run once, when the image's pixels are first wanted.
    handler: Id,
    handler_flipped: usize,
}

pub const INSTANCE_SIZE: usize = size_of::<Image>();

/// A container this module recognises, and the size it declares.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Recognised {
    format: &'static str,
    size: NSSize,
}

/// What `bytes` is, by its header alone.
///
/// Header sniffing rather than decoding: every format below states its
/// dimensions in a fixed place near the front, so the size is a read rather than
/// an inference. A container that is not one of these is `None`, and `None` is
/// what makes `-initWithData:` answer nil.
fn recognise(bytes: &[u8]) -> Option<Recognised> {
    let be32 = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let le32 = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };

    // PNG: the eight-byte signature, then an `IHDR` whose first two fields are
    // the dimensions.
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Recognised {
            format: "PNG",
            size: NSSize {
                width: be32(16)? as f64,
                height: be32(20)? as f64,
            },
        });
    }
    // BMP: `BM`, then a DIB header whose width and height are signed and whose
    // height is negative for a top-down image - so the magnitude is the size.
    if bytes.starts_with(b"BM") {
        let width = le32(18)? as i32;
        let height = le32(22)? as i32;
        return Some(Recognised {
            format: "BMP",
            size: NSSize {
                width: width.unsigned_abs() as f64,
                height: height.unsigned_abs() as f64,
            },
        });
    }
    // GIF: `GIF87a`/`GIF89a`, then two little-endian sixteen-bit fields.
    if bytes.starts_with(b"GIF8") {
        let le16 = |at: usize| -> Option<u16> {
            Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
        };
        return Some(Recognised {
            format: "GIF",
            size: NSSize {
                width: le16(6)? as f64,
                height: le16(8)? as f64,
            },
        });
    }
    // ICNS and JPEG state their sizes inside a chunk walk rather than at a fixed
    // offset. Recognised so the diagnostic can name them, with no size - which
    // `-size` reports as zero, the same as Apple does for an image it has not
    // measured yet.
    // PDF: the first page's media box, in points, like every size here.
    if bytes.starts_with(b"%PDF-") {
        let (width, height) = crate::pdf::page_size(bytes)?;
        return Some(Recognised {
            format: "PDF",
            size: NSSize { width, height },
        });
    }
    if bytes.starts_with(b"icns") {
        return Some(Recognised {
            format: "ICNS",
            size: NSSize::default(),
        });
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some(Recognised {
            format: "JPEG",
            size: NSSize::default(),
        });
    }
    None
}

fn image_of<'a>(receiver: Id) -> &'a mut Image {
    // SAFETY: an image of this crate's.
    unsafe { &mut *(receiver as *mut Image) }
}

thread_local! {
    /// Images locked for offscreen drawing, innermost last.
    static IMAGE_FOCUS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

fn pixel_extent(size: NSSize) -> Option<(usize, usize)> {
    if !size.width.is_finite()
        || !size.height.is_finite()
        || size.width <= 0.0
        || size.height <= 0.0
    {
        return None;
    }
    let width = size.width.ceil() as usize;
    let height = size.height.ceil() as usize;
    width.checked_mul(height)?.checked_mul(4)?;
    Some((width, height))
}

/// `-[NSImage lockFocus]`: make the receiver the destination for immediate
/// offscreen drawing.
unsafe extern "C" fn lock_focus(receiver: Id, _sel: Sel) {
    let image = image_of(receiver);
    let Some((width, height)) = pixel_extent(image.size) else {
        return;
    };
    let len = width * height * 4;
    if image.focused_pixels.is_null() {
        image.focused_pixels = Box::into_raw(Box::new(vec![0; len]));
    } else {
        // SAFETY: this field is created only by this method and remains owned
        // by the image.
        unsafe { (*image.focused_pixels).resize(len, 0) };
    }
    IMAGE_FOCUS.with(|stack| stack.borrow_mut().push(receiver as usize));
}

/// `-[NSImage unlockFocus]`.
unsafe extern "C" fn unlock_focus(receiver: Id, _sel: Sel) {
    IMAGE_FOCUS.with(|stack| {
        let mut stack = stack.borrow_mut();
        if stack.last().copied() == Some(receiver as usize) {
            stack.pop();
        }
    });
}

/// Copy a rectangle from the currently focused image into an RGBA bitmap.
fn focused_bitmap(rect: NSRect) -> Option<crate::raster::Bitmap> {
    let receiver = IMAGE_FOCUS.with(|stack| stack.borrow().last().copied())? as Id;
    let image = image_of(receiver);
    let (image_width, image_height) = pixel_extent(image.size)?;
    // A focused-view rect uses integral device pixels for this one-scale
    // offscreen image. Clamp it rather than reading outside a partially
    // intersecting image.
    let left = rect.origin.x.floor().max(0.0) as usize;
    let bottom = rect.origin.y.floor().max(0.0) as usize;
    let width = rect.size.width.ceil().max(0.0) as usize;
    let height = rect.size.height.ceil().max(0.0) as usize;
    let width = width.min(image_width.saturating_sub(left));
    let height = height.min(image_height.saturating_sub(bottom));
    if width == 0 || height == 0 || image.focused_pixels.is_null() {
        return None;
    }
    // SAFETY: allocated and sized for this image by `lock_focus`.
    let source = unsafe { &*image.focused_pixels };
    let mut pixels = vec![0; width.checked_mul(height)?.checked_mul(4)?];
    for row in 0..height {
        let source_row = image_height - bottom - height + row;
        let from = (source_row * image_width + left) * 4;
        let to = row * width * 4;
        pixels[to..to + width * 4].copy_from_slice(&source[from..from + width * 4]);
    }
    Some(crate::raster::Bitmap {
        width: width as u32,
        height: height as u32,
        pixels,
    })
}

/// The bytes an image holds.
fn bytes_of<'a>(receiver: Id) -> &'a [u8] {
    let data = image_of(receiver).data;
    if data.is_null() {
        return &[];
    }
    // SAFETY: an `NSData` this object retained.
    match unsafe { corefoundation::collections::data(data as *const c_void) } {
        Some(bytes) => bytes.as_slice(),
        None => &[],
    }
}

/// Whether `object` is an image, checked rather than assumed: the selectors
/// that reach the two below take an `id`, and a program that passes something
/// else would otherwise have an arbitrary object read as an `NSImage`.
fn is_image(object: Id) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: an object this runtime allocated.
    unsafe { objc::object::class_of(object) }.is_some_and(|c| {
        objc::class::lookup(c"NSImage").is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
    })
}

/// The size an image reports, in points, which a caller relating it to the
/// pixels [`bitmap`] hands back needs: a 2x representation makes the two
/// differ, and a cursor's point is in points.
pub fn declared_size(object: Id) -> NSSize {
    match is_image(object) {
        true => image_of(object).size,
        false => NSSize::default(),
    }
}

/// Whether `object` is an image with nothing in it: made by `-initWithSize:`
/// and never given data, a representation, or anything drawn.
///
/// AppKit draws that as nothing, which is how a program makes an invisible
/// `NSCursor`.
pub fn is_blank(object: Id) -> bool {
    if !is_image(object) {
        return false;
    }
    let image = image_of(object);
    let no_reps = image.reps.is_null()
        // SAFETY: an array of representations this object made.
        || unsafe { corefoundation::api::CFArrayGetCount(image.reps as *const c_void) } == 0;
    image.data.is_null()
        && no_reps
        && image.cg_image.is_null()
        && image.focused_pixels.is_null()
        && image.handler.is_null()
}

/// An image's pixels, for a caller outside AppKit that needs them - the
/// application icon is the one that does.
pub fn bitmap(object: Id) -> Option<crate::raster::Bitmap> {
    if !is_image(object) {
        return None;
    }
    let bytes = bytes_of(object);
    if bytes.starts_with(b"%PDF-") {
        // Twice the page's size, so the artwork stays sharp on a
        // high-density display; drawing scales it to the image's size.
        return crate::pdf::render(bytes, 2.0);
    }
    crate::raster::decode(bytes).or_else(|| {
        // Built by `-initWithSize:` plus `-addRepresentation:`, which is how a
        // program that generated its pixels rather than loading a file makes
        // one. The first representation is the image, as it is in Apple's for
        // an image with only one.
        let reps = image_of(object).reps;
        if reps.is_null() {
            return None;
        }
        // SAFETY: an array of representations this object made.
        let first =
            unsafe { corefoundation::api::CFArrayGetValueAtIndex(reps as *const c_void, 0) as Id };
        bitmap::pixels_of(first)
    })
}

/// `-initWithData:`.
///
/// **Nil for data this runtime does not recognise**, which is `NSImage`'s own
/// contract rather than a shortcut: Apple returns nil for data it cannot make an
/// image of, and a program handing over an arbitrary file is expected to check.
/// What would be a guess is answering non-nil and then drawing nothing.
unsafe extern "C" fn init_with_data(receiver: Id, _sel: Sel, data: Id) -> Id {
    let image = image_of(receiver);
    if data.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: null or an `NSData`.
    let Some(bytes) = (unsafe { corefoundation::collections::data(data as *const c_void) }) else {
        return std::ptr::null_mut();
    };
    let Some(recognised) = recognise(bytes) else {
        return std::ptr::null_mut();
    };
    image.data = objc::arc::retain(data);
    image.size = recognised.size;
    if image.size.width <= 0.0 || image.size.height <= 0.0 {
        // A container whose header does not say how big it is - `.icns` - is
        // measured by decoding it, since `-size` is asked before anything is
        // drawn. The decode is kept rather than repeated at draw time.
        if let Some(cg_image) = cg_image_of(receiver) {
            // SAFETY: a `CGImage` this image holds.
            image.size = unsafe {
                NSSize {
                    width: crate::graphics::CGImageGetWidth(cg_image) as f64,
                    height: crate::graphics::CGImageGetHeight(cg_image) as f64,
                }
            };
        }
    }
    receiver
}

/// The image's pixels as a `CGImage` it holds, decoding them on first use.
///
/// `None` only for a container [`crate::raster`] has no decoder for.
fn cg_image_of(receiver: Id) -> Option<*const c_void> {
    let image = image_of(receiver);
    if image.cg_image.is_null() {
        let pixels = match image.handler.is_null() {
            true => bitmap(receiver)?,
            false => render_handler(receiver)?,
        };
        image.cg_image = crate::graphics::image_from_bitmap(pixels);
    }
    (!image.cg_image.is_null()).then_some(image.cg_image)
}

/// `-CGImageForProposedRect:context:hints:`.
///
/// The image's one bitmap, whatever rect was proposed: there is only one
/// representation to choose from, so the rect is left as the caller passed it.
/// The `CGImage` belongs to the image, as Apple's does, and lives as long as it.
unsafe extern "C" fn cg_image_for_proposed_rect(
    receiver: Id,
    sel: Sel,
    _rect: *mut NSRect,
    _context: Id,
    _hints: Id,
) -> *const c_void {
    let image = image_of(receiver);
    if !has_content(image) {
        // Nothing to make one of, which Apple answers with NULL.
        return std::ptr::null();
    }
    match cg_image_of(receiver) {
        Some(cg_image) => cg_image,
        // SAFETY: `needs_pixels` reads only the receiver and selector.
        None => unsafe { needs_pixels(receiver, sel) },
    }
}

unsafe extern "C" fn init_with_contents_of_file(receiver: Id, sel: Sel, path: Id) -> Id {
    // SAFETY: an `NSString`, or nil.
    let path = unsafe { foundation::string::bytes(path) };
    let Ok(path) = String::from_utf8(path) else {
        return std::ptr::null_mut();
    };
    init_with_guest_file(receiver, sel, &path)
}

/// `-initWithContentsOfURL:`, for a `file:` URL; nil for any other.
unsafe extern "C" fn init_with_contents_of_url(receiver: Id, sel: Sel, url: Id) -> Id {
    // SAFETY: an `NSURL`, or nil.
    let path = unsafe { corefoundation::url::as_url(url as _) }.and_then(|url| url.file_path());
    match path {
        Some(path) => init_with_guest_file(receiver, sel, &path),
        None => std::ptr::null_mut(),
    }
}

fn init_with_guest_file(receiver: Id, sel: Sel, path: &str) -> Id {
    // The guest named it, so it is read at this host's spelling. Without this a
    // prefixed guest gets nil for a file that is plainly there, and a program
    // that does not expect nil from its own splash screen crashes on it.
    let Ok(bytes) = std::fs::read(prefix::ns::host_path(path)) else {
        return std::ptr::null_mut();
    };
    let data = corefoundation::collections::new_data(bytes);
    // SAFETY: `-initWithData:` is `id (id, SEL, id)` and the data is a fresh
    // object this call owns a reference to.
    let image = unsafe { init_with_data(receiver, sel, data as Id) };
    // SAFETY: as above; the image retained it if it kept it.
    unsafe { corefoundation::api::CFRelease(data) };
    image
}

/// A fresh image of `size` with nothing in it, owned by the caller.
///
/// What `-initWithSize:` makes, for a caller that needs an image to hand back
/// and has no pixels to put in it.
pub fn empty(size: NSSize) -> Id {
    let class = objc::class::lookup(c"NSImage").expect("declared before its methods");
    let image = objc::object::create_instance(class, 0);
    image_of(image).size = size;
    image
}

/// The extensions `+imageNamed:` tries after the name as given, in the order
/// Apple's lookup prefers them.
const NAMED_EXTENSIONS: &[&str] = &[
    "png", "tiff", "tif", "pdf", "icns", "jpg", "jpeg", "gif", "bmp",
];

/// `+[NSImage imageNamed:]`.
///
/// The main bundle's resource of that name, with or without an extension, and
/// `NSApplicationIcon` for the application's icon. The answer is cached per
/// name and never released, because Apple's is: a program compares the
/// images it gets for one name by identity, and does not retain them.
///
/// Nil for a name nothing has, which is Apple's answer too - and every other
/// system image name (`NSImageNameStatusAvailable` and friends) is one of
/// those here, because the artwork for them is Apple's and not in the bundle.
unsafe extern "C" fn image_named(_class: Id, _sel: Sel, name: Id) -> Id {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static NAMED: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: an `NSString`.
    let Ok(name) = String::from_utf8(unsafe { foundation::string::bytes(name) }) else {
        return std::ptr::null_mut();
    };
    let mut named = NAMED.lock().unwrap_or_else(|e| e.into_inner());
    let named = named.get_or_insert_default();
    if let Some(image) = named.get(&name) {
        return *image as Id;
    }
    if name == "NSApplicationIcon" {
        let icon = crate::application::icon_image();
        if !icon.is_null() {
            // Not cached: the program may set another one later.
            return icon;
        }
    }
    let path = foundation::bundle::main().and_then(|bundle| {
        let path = match name.as_str() {
            "NSApplicationIcon" => return bundle.icon(),
            name => bundle.resource(name, ""),
        };
        path.or_else(|| {
            NAMED_EXTENSIONS
                .iter()
                .find_map(|extension| bundle.resource(&name, extension))
        })
    });
    let Some(bytes) = path.and_then(|path| std::fs::read(path).ok()) else {
        return std::ptr::null_mut();
    };
    let class = objc::class::lookup(c"NSImage").expect("declared before its methods");
    let image = objc::object::create_instance(class, 0);
    let data = corefoundation::collections::new_data(bytes);
    // SAFETY: `-initWithData:` is `id (id, SEL, id)`, and the data is a fresh
    // object this call owns a reference to.
    let initialised = unsafe { init_with_data(image, objc::sel::get("initWithData:"), data as Id) };
    // SAFETY: as above; the image retained it if it kept it.
    unsafe { corefoundation::api::CFRelease(data) };
    if initialised.is_null() {
        objc::arc::release(image);
        return std::ptr::null_mut();
    }
    mark_template_by_name(image, &name);
    named.insert(name, image as usize);
    image
}

/// AppKit's convention for `+imageNamed:` and `-[NSBundle imageForResource:]`:
/// a resource whose name ends in "Template" is a template image, which a
/// program tints itself when it sees one.
fn mark_template_by_name(image: Id, name: &str) {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    if stem.ends_with("Template") {
        image_of(image).template = 1;
    }
}

/// `-[NSBundle pathForImageResource:]` and `-URLForImageResource:`, AppKit
/// categories: the bundle's own `sel` (`pathForResource:ofType:` or
/// `URLForResource:withExtension:`) asked for the name as given, then with
/// each extension `+imageNamed:` tries.
fn image_resource(bundle: Id, sel: &str, name: Id) -> Id {
    if name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: both selectors are `id (id, SEL, id, id)` on an `NSBundle`.
    let lookup = |kind: Id| unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(bundle, objc::sel::get(sel), name, kind)
    };
    let found = lookup(std::ptr::null_mut());
    if !found.is_null() {
        return found;
    }
    NAMED_EXTENSIONS
        .iter()
        .find_map(|extension| {
            let kind = foundation::string::from_str(extension);
            let found = lookup(kind);
            objc::arc::release(kind);
            (!found.is_null()).then_some(found)
        })
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn path_for_image_resource(bundle: Id, _sel: Sel, name: Id) -> Id {
    image_resource(bundle, "pathForResource:ofType:", name)
}

unsafe extern "C" fn url_for_image_resource(bundle: Id, _sel: Sel, name: Id) -> Id {
    image_resource(bundle, "URLForResource:withExtension:", name)
}

/// `-[NSBundle imageForResource:]`: the image file `-pathForImageResource:`
/// finds, loaded.
unsafe extern "C" fn image_for_resource(bundle: Id, _sel: Sel, name: Id) -> Id {
    let path = image_resource(bundle, "pathForResource:ofType:", name);
    if path.is_null() {
        return std::ptr::null_mut();
    }
    let class = objc::class::lookup(c"NSImage").expect("declared before its methods");
    let image = objc::object::create_instance(class, 0);
    // SAFETY: a fresh image and an NSString path.
    let image = unsafe {
        init_with_contents_of_file(image, objc::sel::get("initWithContentsOfFile:"), path)
    };
    if !image.is_null()
        && let Ok(name) = String::from_utf8(unsafe { foundation::string::bytes(name) })
    {
        mark_template_by_name(image, &name);
    }
    objc::arc::autorelease(image)
}

/// `-addRepresentation:`, and the list it appends to.
///
/// An image built this way carries its pixels in the representation rather than
/// in encoded bytes, which is why [`bitmap`] asks the representation when there
/// is no data.
unsafe extern "C" fn add_representation(receiver: Id, _sel: Sel, rep: Id) {
    if rep.is_null() {
        return;
    }
    let image = image_of(receiver);
    if image.reps.is_null() {
        image.reps = objc::arc::retain(corefoundation::collections::new_array(
            &[],
            corefoundation::callbacks::Policy::from_type_callbacks(),
            true,
        ) as Id);
    }
    // SAFETY: an array this function made, and an object to put in it.
    unsafe {
        corefoundation::api::CFArrayAppendValue(image.reps as *const c_void, rep as *const c_void)
    };
}

unsafe extern "C" fn representations(receiver: Id, _sel: Sel) -> Id {
    let image = image_of(receiver);
    if !image.reps.is_null() {
        return image.reps;
    }
    corefoundation::collections::new_array(
        &[],
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    ) as Id
}

/// `-initWithSize:`: an image with a size and no pixels yet.
///
/// What a program builds before drawing into it or adding a representation, so
/// the empty result is the correct one - and the family below is what says so
/// if it is asked for pixels that were never put there.
unsafe extern "C" fn init_with_size(receiver: Id, _sel: Sel, size: NSSize) -> Id {
    image_of(receiver).size = size;
    receiver
}

unsafe extern "C" fn set_template(receiver: Id, _sel: Sel, flag: u8) {
    image_of(receiver).template = usize::from(flag != 0);
}

unsafe extern "C" fn is_template(receiver: Id, _sel: Sel) -> u8 {
    u8::from(image_of(receiver).template != 0)
}

/// `+imageWithSize:flipped:drawingHandler:`: an image of that size, whose
/// pixels are what the handler draws.
///
/// AppKit calls the handler each time the image is drawn, at the destination's
/// resolution. Here it is called once, the first time the pixels are wanted,
/// and what it drew is kept: a handler that tints an icon draws the same thing
/// every time.
unsafe extern "C" fn image_with_size(
    class: Id,
    _sel: Sel,
    size: NSSize,
    flipped: u8,
    handler: Id,
) -> Id {
    // SAFETY: the receiver of a class method is a class.
    let Some(class) = (unsafe { objc::class::from_ptr(class as *const objc::class::Class) }) else {
        return std::ptr::null_mut();
    };
    let image = objc::object::create_instance(class, 0);
    let state = image_of(image);
    state.size = size;
    if !handler.is_null() {
        state.handler = objc::block::copy(handler);
        state.handler_flipped = usize::from(flipped != 0);
    }
    objc::arc::autorelease(image)
}

/// Run an image's drawing handler into a fresh bitmap.
fn render_handler(receiver: Id) -> Option<crate::raster::Bitmap> {
    let image = image_of(receiver);
    // SAFETY: a block this image copied, whose `invoke` the `@?` in
    // `+imageWithSize:flipped:drawingHandler:`'s encoding made callable.
    let invoke = unsafe { crate::animation::invoke(image.handler) }?;
    let (width, height) = pixel_extent(image.size)?;
    let key = receiver as usize;
    crate::backing::begin_offscreen(
        key,
        width as u32,
        height as u32,
        1.0,
        image.handler_flipped != 0,
    );
    let rect = NSRect {
        origin: foundation::geometry::NSPoint::default(),
        size: image.size,
    };
    // SAFETY: `BOOL (^)(NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, NSRect) -> u8 = std::mem::transmute(invoke);
        f(image.handler, rect);
    }
    let store = crate::backing::end_offscreen(key)?;
    // Premultiplied B, G, R, A to the straight R, G, B, A a bitmap holds.
    let mut pixels = store.pixels;
    for texel in pixels.as_chunks_mut::<4>().0 {
        let [b, g, r, a] = *texel;
        let straight = |c: u8| match a {
            0 => 0,
            a => ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8,
        };
        *texel = [straight(r), straight(g), straight(b), a];
    }
    Some(crate::raster::Bitmap {
        width: store.width,
        height: store.height,
        pixels,
    })
}

/// Whether there is anything to make pixels of.
fn has_content(image: &Image) -> bool {
    !image.cg_image.is_null()
        || !image.data.is_null()
        || !image.reps.is_null()
        || !image.handler.is_null()
}

/// `-copyWithZone:`: another image of the same pixels.
///
/// A program copies an image to change the copy - tint it, resize it - and
/// the original must not change with it.
unsafe extern "C" fn copy_with_zone(receiver: Id, _sel: Sel, _zone: *mut c_void) -> Id {
    // SAFETY: a live object this runtime allocated.
    let Some(class) = (unsafe { objc::object::class_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    let copy = objc::object::create_instance(class, 0);
    let (from, to) = (image_of(receiver), image_of(copy));
    to.data = objc::arc::retain(from.data);
    to.reps = objc::arc::retain(from.reps);
    to.size = from.size;
    to.template = from.template;
    let description = crate::props::object(receiver, "accessibilityDescription");
    if !description.is_null() {
        let key = objc::sel::get("accessibilityDescription") as *const c_void;
        objc::arc::set_associated_with_policy(copy, key, description, 3);
    }
    if !from.cg_image.is_null() {
        // SAFETY: a `CGImage` the original holds.
        to.cg_image = unsafe { corefoundation::api::CFRetain(from.cg_image) };
    }
    if !from.handler.is_null() {
        to.handler = objc::block::copy(from.handler);
        to.handler_flipped = from.handler_flipped;
    }
    copy
}

unsafe extern "C" fn size(receiver: Id, _sel: Sel) -> NSSize {
    image_of(receiver).size
}

unsafe extern "C" fn set_size(receiver: Id, _sel: Sel, size: NSSize) {
    image_of(receiver).size = size;
}

/// `-initWithCGImage:size:`, where a zero size means the image's pixel size.
unsafe extern "C" fn init_with_cg_image(
    receiver: Id,
    _sel: Sel,
    cg_image: *const c_void,
    size: NSSize,
) -> Id {
    if cg_image.is_null() {
        return std::ptr::null_mut();
    }
    let image = image_of(receiver);
    // SAFETY: a `CGImage` the caller holds a reference to.
    image.cg_image = unsafe { corefoundation::api::CFRetain(cg_image) };
    image.size = match size.width > 0.0 && size.height > 0.0 {
        true => size,
        false => unsafe {
            NSSize {
                width: crate::graphics::CGImageGetWidth(cg_image) as f64,
                height: crate::graphics::CGImageGetHeight(cg_image) as f64,
            }
        },
    };
    receiver
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    let image = image_of(receiver);
    if !image.cg_image.is_null() {
        // SAFETY: the reference `-initWithCGImage:size:` took.
        unsafe { corefoundation::api::CFRelease(image.cg_image) };
        image.cg_image = std::ptr::null();
    }
    if !image.focused_pixels.is_null() {
        // SAFETY: allocated by `lock_focus`, and the image owns it.
        drop(unsafe { Box::from_raw(image.focused_pixels) });
        image.focused_pixels = ptr::null_mut();
    }
    if !image.handler.is_null() {
        objc::block::release(image.handler);
        image.handler = ptr::null_mut();
    }
    objc::arc::finalize(receiver);
}

/// Draw a `CGImage`-backed image into the focused view.
#[allow(clippy::too_many_arguments)]
fn draw(
    receiver: Id,
    sel: Sel,
    destination: NSRect,
    from: NSRect,
    operation: usize,
    fraction: f64,
    respect_flipped: bool,
) {
    let image = image_of(receiver);
    if !has_content(image) {
        // An image made with `-initWithSize:` and given nothing: there is
        // nothing to draw, and Apple draws nothing.
        return;
    }
    let Some(cg_image) = cg_image_of(receiver) else {
        // SAFETY: `needs_pixels` reads only the receiver and selector.
        unsafe { needs_pixels(receiver, sel) };
    };
    let Some((width, height, pixels)) = crate::graphics::image_bgra(cg_image) else {
        return;
    };
    crate::backing::draw_image(
        &pixels,
        width,
        height,
        image.size,
        destination,
        from,
        operation,
        fraction,
        respect_flipped,
    );
}

/// Whether `-setTemplate:` marked the image, which is what a tint colour applies to.
pub(crate) fn is_template_image(object: Id) -> bool {
    is_image(object) && image_of(object).template != 0
}

/// Draw a template image filled with `tint` (straight RGBA) where it is
/// opaque, as `NSButton.contentTintColor` does.
pub(crate) fn draw_tinted(receiver: Id, destination: NSRect, tint: [f64; 4]) {
    let image = image_of(receiver);
    if !has_content(image) {
        return;
    }
    let Some(cg_image) = cg_image_of(receiver) else {
        return;
    };
    let Some((width, height, mut pixels)) = crate::graphics::image_bgra(cg_image) else {
        return;
    };
    let channel = |value: f64| value.clamp(0.0, 1.0);
    let [red, green, blue, alpha] = tint.map(channel);
    for texel in pixels.as_chunks_mut::<4>().0 {
        // Premultiplied: the image's coverage scales the colour and its alpha.
        let coverage = f64::from(texel[3]) / 255.0 * alpha;
        texel[0] = (blue * coverage * 255.0).round() as u8;
        texel[1] = (green * coverage * 255.0).round() as u8;
        texel[2] = (red * coverage * 255.0).round() as u8;
        texel[3] = (coverage * 255.0).round() as u8;
    }
    crate::backing::draw_image(
        &pixels,
        width,
        height,
        image.size,
        destination,
        NSRect::default(),
        2,
        1.0,
        false,
    );
}

/// `NSDrawNinePartImage`: corners at their own size, edges and center
/// stretched over what is left.
///
/// FIXME (ACCURACY): Apple tiles the edge and center fills rather than
/// stretching them; the two agree for the uniform fills this is used with.
#[allow(non_snake_case, clippy::too_many_arguments)]
pub unsafe extern "C" fn NSDrawNinePartImage(
    frame: NSRect,
    top_left: Id,
    top: Id,
    top_right: Id,
    left: Id,
    center: Id,
    right: Id,
    bottom_left: Id,
    bottom: Id,
    bottom_right: Id,
    operation: usize,
    fraction: f64,
    flipped: u8,
) {
    let size = |image: Id| match image.is_null() {
        true => NSSize {
            width: 0.0,
            height: 0.0,
        },
        false => image_of(image).size,
    };
    let (tl, br) = (size(top_left), size(bottom_right));
    let x = [
        frame.origin.x,
        frame.origin.x + tl.width,
        frame.origin.x + frame.size.width - br.width,
    ];
    let widths = [tl.width, (x[2] - x[1]).max(0.0), br.width];
    // Rows bottom to top in an unflipped context, top to bottom in a flipped one.
    let (first, last) = match flipped != 0 {
        true => (tl.height, br.height),
        false => (br.height, tl.height),
    };
    let y = [
        frame.origin.y,
        frame.origin.y + first,
        frame.origin.y + frame.size.height - last,
    ];
    let heights = [first, (y[2] - y[1]).max(0.0), last];
    let grid = [
        [bottom_left, bottom, bottom_right],
        [left, center, right],
        [top_left, top, top_right],
    ];
    let sel = objc::sel::get("drawInRect:fromRect:operation:fraction:respectFlipped:hints:");
    for (row, images) in grid.iter().enumerate() {
        // Grid rows run bottom to top; a flipped context lays them out the other way.
        let at = match flipped != 0 {
            true => 2 - row,
            false => row,
        };
        for (column, &image) in images.iter().enumerate() {
            if image.is_null() || widths[column] <= 0.0 || heights[at] <= 0.0 {
                continue;
            }
            let destination = NSRect {
                origin: foundation::geometry::NSPoint {
                    x: x[column],
                    y: y[at],
                },
                size: NSSize {
                    width: widths[column],
                    height: heights[at],
                },
            };
            let whole = NSRect::default();
            draw(
                image,
                sel,
                destination,
                whole,
                operation,
                fraction,
                flipped != 0,
            );
        }
    }
}

unsafe extern "C" fn draw_in_rect_respect_flipped(
    receiver: Id,
    sel: Sel,
    destination: NSRect,
    from: NSRect,
    operation: usize,
    fraction: f64,
    respect_flipped: u8,
    _hints: Id,
) {
    draw(
        receiver,
        sel,
        destination,
        from,
        operation,
        fraction,
        respect_flipped != 0,
    );
}

/// `-drawInRect:`: the whole image, source-over, respecting flipped
/// contexts, as Apple documents it.
unsafe extern "C" fn draw_in_rect_plain(receiver: Id, sel: Sel, destination: NSRect) {
    draw(receiver, sel, destination, NSRect::default(), 2, 1.0, true);
}

unsafe extern "C" fn draw_in_rect(
    receiver: Id,
    sel: Sel,
    destination: NSRect,
    from: NSRect,
    operation: usize,
    fraction: f64,
) {
    draw(receiver, sel, destination, from, operation, fraction, false);
}

/// Everything that still needs a decoded raster this module cannot produce.
///
/// One implementation for the whole family, naming the selector it was reached
/// by and the format that was handed in - which together are the whole of what
/// somebody adding a decoder needs to know.
unsafe extern "C" fn needs_pixels(receiver: Id, sel: Sel) -> ! {
    // SAFETY: a registered selector is a NUL-terminated interned name.
    let name = unsafe { std::ffi::CStr::from_ptr(sel) }.to_string_lossy();
    let format = recognise(bytes_of(receiver)).map_or("unrecognised data", |r| r.format);
    objc::fail!(
        "-[NSImage {name}]: this runtime has no decoder for {format} - see appkit::raster for the \
         ones it does have"
    );
}

/// `-TIFFRepresentation`: the image, decoded and re-encoded.
///
/// The round trip is the guest's design rather than this runtime's. A program
/// that wants an image's pixels asks `NSImage` for a TIFF and hands that to
/// `NSBitmapImageRep`, so both ends of the conversion are here and
/// `appkit::raster` keeps them agreeing.
///
/// Nil when the container has no decoder, which is what Apple answers for an
/// image it could not read - and the guest checks, because it has just come
/// down this path from its own reader failing.
unsafe extern "C" fn tiff_representation(receiver: Id, _sel: Sel) -> Id {
    let Some(bitmap) = crate::raster::decode(bytes_of(receiver)) else {
        return std::ptr::null_mut();
    };
    let tiff = corefoundation::collections::new_data(crate::raster::encode_tiff(&bitmap));
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(tiff) as Id }
}

pub(crate) fn install() {
    let bundle = objc::class::lookup(c"NSBundle").expect("Foundation registered NSBundle");
    methods!(bundle, {
        "pathForImageResource:" => path_for_image_resource, c"@24@0:8@16";
        "URLForImageResource:" => url_for_image_resource, c"@24@0:8@16";
        "imageForResource:" => image_for_resource, c"@24@0:8@16";
    });
    let class = objc::class::lookup(c"NSImage").expect("declared before its methods");
    class_methods!(class, {
        "imageNamed:" => image_named, c"@24@0:8@16";
        "imageWithSize:flipped:drawingHandler:" => image_with_size, c"@44@0:8{CGSize=dd}16c32@?36";
    });
    methods!(class, {
        "initWithData:" => init_with_data, c"@24@0:8@16";
        "initWithSize:" => init_with_size, c"@32@0:8{CGSize=dd}16";
        "initWithCGImage:size:" => init_with_cg_image, c"@40@0:8^{CGImage=}16{CGSize=dd}24";
        "dealloc" => dealloc, c"v16@0:8";
        "addRepresentation:" => add_representation, c"v24@0:8@16";
        "representations" => representations, c"@16@0:8";
        "initWithContentsOfFile:" => init_with_contents_of_file, c"@24@0:8@16";
        "initWithContentsOfURL:" => init_with_contents_of_url, c"@24@0:8@16";
        // Read now rather than on first draw, which differs only for a file
        // that changes or disappears in between.
        "initByReferencingFile:" => init_with_contents_of_file, c"@24@0:8@16";
        "initByReferencingURL:" => init_with_contents_of_url, c"@24@0:8@16";
        "size" => size, c"{CGSize=dd}16@0:8";
        "setSize:" => set_size, c"v32@0:8{CGSize=dd}16";
        "setTemplate:" => set_template, c"v20@0:8c16";
        "isTemplate" => is_template, c"c16@0:8";
        "copyWithZone:" => copy_with_zone, c"@24@0:8^v16";
    });
    // Read back only: nothing here speaks to assistive technology.
    crate::props::properties!(class, {
        copied "accessibilityDescription", "setAccessibilityDescription:";
    });
    // The pixel family, each reached by the same implementation so that the one
    // that is wanted first names itself.
    methods!(class, {
        "TIFFRepresentation" => tiff_representation, c"@16@0:8";
        "CGImageForProposedRect:context:hints:" => cg_image_for_proposed_rect,
            c"^v40@0:8^{CGRect={CGPoint=dd}{CGSize=dd}}16@24@32";
    });
    // The pixel family, each reached by the same implementation so that the one
    // that is wanted first names itself. The encodings are still each method's
    // own: the runtime marshals a guest's send by decoding exactly this string,
    // and the shared implementation does not make the arguments go away.
    for (selector, types) in [(
        "bestRepresentationForRect:context:hints:",
        c"@64@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16@48@56",
    )] {
        class.replace_method(
            objc::sel::get(selector),
            needs_pixels as *const c_void,
            types,
        );
    }
    methods!(class, {
        "lockFocus" => lock_focus, c"v16@0:8";
        "unlockFocus" => unlock_focus, c"v16@0:8";
        "drawInRect:" => draw_in_rect_plain, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "drawInRect:fromRect:operation:fraction:" => draw_in_rect,
            c"v96@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16{CGRect={CGPoint=dd}{CGSize=dd}}48Q80d88";
        "drawInRect:fromRect:operation:fraction:respectFlipped:hints:" =>
            draw_in_rect_respect_flipped,
            c"v108@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16{CGRect={CGPoint=dd}{CGSize=dd}}48Q80d88c96@100";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two containers Deltarune actually hands over, measured from their
    /// headers - and the BMP is the one that matters, because the file is called
    /// `splash.png`.
    #[test]
    fn a_header_says_how_big_the_image_is() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(
            recognise(&png).map(|r| (r.format, r.size.width, r.size.height)),
            Some(("PNG", 640.0, 480.0))
        );

        let mut bmp = b"BM".to_vec();
        bmp.resize(18, 0);
        bmp.extend_from_slice(&150i32.to_le_bytes());
        // Negative, as a top-down BMP writes it: the size is the magnitude.
        bmp.extend_from_slice(&(-57i32).to_le_bytes());
        assert_eq!(
            recognise(&bmp).map(|r| (r.format, r.size.width, r.size.height)),
            Some(("BMP", 150.0, 57.0))
        );
    }

    /// Anything else is `None`, which is what makes `-initWithData:` nil rather
    /// than an image that would draw nothing.
    #[test]
    fn a_name_nothing_has_is_nil() {
        crate::bootstrap();
        let name = foundation::string::from_str("RosemaryNoSuchImage");
        // SAFETY: `+imageNamed:` is `id (Class, SEL, id)`.
        let image =
            unsafe { image_named(std::ptr::null_mut(), objc::sel::get("imageNamed:"), name) };
        assert!(image.is_null());
    }

    #[test]
    fn unrecognised_data_is_not_an_image() {
        assert_eq!(recognise(b"not an image at all"), None);
        assert_eq!(recognise(b""), None);
        // A truncated header is not a size to be guessed at either.
        assert_eq!(recognise(b"\x89PNG\r\n\x1a\n"), None);
    }
}

/// `NSBitmapImageRep`: a decoded raster a program can read or write directly.
///
/// The one AppKit class in this tree a guest gets *memory* out of rather than
/// answers. `-bitmapData` is a pointer the guest reads pixel by pixel and hands
/// to `glTexImage2D`, so the buffer has to stay put for as long as the rep does
/// and its layout has to be exactly what the accessors say it is.
pub mod bitmap {
    use super::*;
    use crate::define::{class_methods, methods};

    #[repr(C)]
    pub struct BitmapRep {
        isa: *const c_void,
        /// The pixels, owned here. Leaked with the object rather than freed at
        /// `-dealloc`, because the guest may still be holding the pointer
        /// `-bitmapData` gave it - Apple's rep owns its buffer for exactly as
        /// long as the rep, and this runtime has no `-dealloc` to hang the free
        /// on that would be reached at the right time.
        pixels: *mut Vec<u8>,
        /// A buffer the *guest* supplied, which this object must not free and
        /// must not copy: the guest writes through it.
        supplied: *mut u8,
        width: usize,
        height: usize,
        samples: usize,
        bits_per_sample: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        has_alpha: usize,
        /// `-setSize:`, the rep's size in *points*, or zero for one that has
        /// not been told a size.
        ///
        /// Zero rather than a copy of the pixel dimensions at init, so that
        /// `-size` answers the pixels for as long as nobody has said otherwise:
        /// the two differ only for a rep that stands for a scaled image - a 2x
        /// rep is half as many points as pixels - and that is exactly what the
        /// setter is for.
        declared: NSSize,
    }

    pub const INSTANCE_SIZE: usize = size_of::<BitmapRep>();

    fn rep_of<'a>(receiver: Id) -> &'a mut BitmapRep {
        // SAFETY: a bitmap rep of this crate's.
        unsafe { &mut *(receiver as *mut BitmapRep) }
    }

    /// Fill in the fields a decoded bitmap determines.
    fn adopt(receiver: Id, bitmap: crate::raster::Bitmap) -> Id {
        let rep = rep_of(receiver);
        rep.width = bitmap.width as usize;
        rep.height = bitmap.height as usize;
        rep.samples = 4;
        rep.bits_per_sample = 8;
        rep.bits_per_pixel = 32;
        rep.bytes_per_row = bitmap.bytes_per_row();
        rep.has_alpha = 1;
        rep.pixels = Box::into_raw(Box::new(bitmap.pixels));
        receiver
    }

    unsafe extern "C" fn init_with_data(receiver: Id, _sel: Sel, data: Id) -> Id {
        if data.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: null or an `NSData`.
        let Some(bytes) = (unsafe { corefoundation::collections::data(data as *const c_void) })
        else {
            return std::ptr::null_mut();
        };
        match crate::raster::decode(bytes) {
            Some(bitmap) => adopt(receiver, bitmap),
            None => std::ptr::null_mut(),
        }
    }

    /// `-initWithCGImage:`, the other direction of the conversion `NSImage`'s
    /// `-initWithCGImage:size:` does.
    ///
    /// The pixels are *copied* rather than shared with the `CGImage`: a rep
    /// hands out `-bitmapData` for the guest to write through, and writing
    /// through it must not edit an image someone else still holds. Nil for a
    /// pixel layout [`crate::graphics::image_bgra`] does not decode, which is
    /// `NSBitmapImageRep`'s own answer for an image it cannot represent.
    unsafe extern "C" fn init_with_cg_image(
        receiver: Id,
        _sel: Sel,
        cg_image: *const c_void,
    ) -> Id {
        if cg_image.is_null() {
            return std::ptr::null_mut();
        }
        let Some((width, height, mut pixels)) = crate::graphics::image_bgra(cg_image) else {
            return std::ptr::null_mut();
        };
        // `image_bgra` composites, and a rep's samples are R, G, B, A.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        adopt(
            receiver,
            crate::raster::Bitmap {
                width: width as u32,
                height: height as u32,
                pixels,
            },
        )
    }

    /// `-initWithFocusedViewRect:` snapshots the current offscreen image.
    unsafe extern "C" fn init_with_focused_view_rect(receiver: Id, _sel: Sel, rect: NSRect) -> Id {
        match focused_bitmap(rect) {
            Some(bitmap) => adopt(receiver, bitmap),
            None => std::ptr::null_mut(),
        }
    }

    /// The eleven-argument designated initialiser.
    ///
    /// `planes` is the guest's own memory when it supplies one and a request to
    /// allocate when it is null or points at null - Apple's rule, and the reason
    /// the two cases are kept apart here rather than always copying: a guest that
    /// passed its own buffer goes on writing through it, and a copy would leave
    /// it drawing into memory nothing reads.
    unsafe extern "C" fn init_with_planes(
        receiver: Id,
        _sel: Sel,
        planes: *mut *mut u8,
        width: isize,
        height: isize,
        bits_per_sample: isize,
        samples: isize,
        has_alpha: u8,
        is_planar: u8,
        _colour_space: Id,
        _format: usize,
        bytes_per_row: isize,
        bits_per_pixel: isize,
    ) -> Id {
        if is_planar != 0 {
            objc::fail!(
                "-[NSBitmapImageRep initWithBitmapDataPlanes:...]: a planar rep keeps each \
                 channel in its own buffer, and this runtime only has the interleaved layout"
            );
        }
        if width <= 0 || height <= 0 || bits_per_sample != 8 || !matches!(samples, 3 | 4) {
            objc::fail!(
                "-[NSBitmapImageRep initWithBitmapDataPlanes:...]: this runtime makes \
                 eight-bit-per-sample reps of three or four samples, not {samples} samples of \
                 {bits_per_sample} bits"
            );
        }
        let rep = rep_of(receiver);
        rep.width = width as usize;
        rep.height = height as usize;
        rep.samples = samples as usize;
        rep.bits_per_sample = 8;
        rep.has_alpha = (has_alpha != 0) as usize;
        rep.bits_per_pixel = match bits_per_pixel > 0 {
            true => bits_per_pixel as usize,
            false => samples as usize * 8,
        };
        rep.bytes_per_row = match bytes_per_row > 0 {
            true => bytes_per_row as usize,
            false => rep.width * rep.bits_per_pixel / 8,
        };

        // SAFETY: `planes` is null or an array of at least one pointer, which is
        // what the method's contract says.
        let supplied = match planes.is_null() {
            true => std::ptr::null_mut(),
            false => unsafe { *planes },
        };
        match supplied.is_null() {
            true => rep.pixels = Box::into_raw(Box::new(vec![0u8; rep.bytes_per_row * rep.height])),
            false => rep.supplied = supplied,
        }
        receiver
    }

    /// The ten-argument form every SDL 2 Cocoa build uses: the designated
    /// initialiser with `NSBitmapFormat` zero, which is what Apple documents it as.
    unsafe extern "C" fn init_with_planes_default_format(
        receiver: Id,
        sel: Sel,
        planes: *mut *mut u8,
        width: isize,
        height: isize,
        bits_per_sample: isize,
        samples: isize,
        has_alpha: u8,
        is_planar: u8,
        colour_space: Id,
        bytes_per_row: isize,
        bits_per_pixel: isize,
    ) -> Id {
        // SAFETY: the same contract, with the format filled in.
        unsafe {
            init_with_planes(
                receiver,
                sel,
                planes,
                width,
                height,
                bits_per_sample,
                samples,
                has_alpha,
                is_planar,
                colour_space,
                0,
                bytes_per_row,
                bits_per_pixel,
            )
        }
    }

    /// The rep's pixels as a bitmap, for the image that holds it.
    ///
    /// Only the layout this module produces and accepts: eight bits a sample,
    /// four samples, no padding between rows. A rep of any other shape is
    /// `None` rather than reinterpreted, because the bytes would be read as a
    /// format they are not.
    pub fn pixels_of(object: Id) -> Option<crate::raster::Bitmap> {
        if object.is_null() {
            return None;
        }
        // SAFETY: null or an object this runtime allocated.
        let is_rep = unsafe { objc::object::class_of(object) }.is_some_and(|c| {
            objc::class::lookup(c"NSBitmapImageRep")
                .is_some_and(|k| c.chain().any(|x| std::ptr::eq(x, k)))
        });
        if !is_rep {
            return None;
        }
        let rep = rep_of(object);
        let stride = rep.width.checked_mul(4)?;
        if rep.bits_per_sample != 8 || rep.bits_per_pixel != 32 || rep.bytes_per_row != stride {
            return None;
        }
        let wanted = stride.checked_mul(rep.height)?;
        let pixels = match rep.supplied.is_null() {
            // SAFETY: the buffer the guest supplied, which it declared covers
            // `bytes_per_row * height`.
            false => unsafe { std::slice::from_raw_parts(rep.supplied, wanted) }.to_vec(),
            // SAFETY: a buffer this object leaked.
            true => unsafe { rep.pixels.as_ref() }?.clone(),
        };
        if pixels.len() < wanted {
            return None;
        }
        Some(crate::raster::Bitmap {
            width: rep.width as u32,
            height: rep.height as u32,
            pixels,
        })
    }

    unsafe extern "C" fn bitmap_data(receiver: Id, _sel: Sel) -> *mut u8 {
        let rep = rep_of(receiver);
        if !rep.supplied.is_null() {
            return rep.supplied;
        }
        match rep.pixels.is_null() {
            true => std::ptr::null_mut(),
            // SAFETY: a buffer this object leaked, which outlives every caller.
            false => unsafe { (*rep.pixels).as_mut_ptr() },
        }
    }

    unsafe extern "C" fn pixels_wide(receiver: Id, _sel: Sel) -> isize {
        rep_of(receiver).width as isize
    }

    unsafe extern "C" fn pixels_high(receiver: Id, _sel: Sel) -> isize {
        rep_of(receiver).height as isize
    }

    unsafe extern "C" fn samples_per_pixel(receiver: Id, _sel: Sel) -> isize {
        rep_of(receiver).samples as isize
    }

    unsafe extern "C" fn bits_per_sample(receiver: Id, _sel: Sel) -> isize {
        rep_of(receiver).bits_per_sample as isize
    }

    unsafe extern "C" fn bits_per_pixel(receiver: Id, _sel: Sel) -> isize {
        rep_of(receiver).bits_per_pixel as isize
    }

    unsafe extern "C" fn bytes_per_row(receiver: Id, _sel: Sel) -> isize {
        rep_of(receiver).bytes_per_row as isize
    }

    unsafe extern "C" fn has_alpha(receiver: Id, _sel: Sel) -> u8 {
        (rep_of(receiver).has_alpha != 0) as u8
    }

    unsafe extern "C" fn is_planar(_receiver: Id, _sel: Sel) -> u8 {
        0
    }

    /// `-size`: what the rep was told it measures, in points, and the pixel
    /// count until it is told. A rep whose size differs from its pixels is a
    /// scaled one, and only `-setSize:` can say so - nothing here reads a DPI
    /// out of an image.
    unsafe extern "C" fn rep_size(receiver: Id, _sel: Sel) -> NSSize {
        let rep = rep_of(receiver);
        match rep.declared.width > 0.0 && rep.declared.height > 0.0 {
            true => rep.declared,
            false => NSSize {
                width: rep.width as f64,
                height: rep.height as f64,
            },
        }
    }

    /// `-setSize:`. A zero or negative size puts the rep back on its pixel
    /// dimensions rather than recording a size no drawing could use: AppKit
    /// treats a rep's size as the extent it draws into, and zero is not one.
    unsafe extern "C" fn set_rep_size(receiver: Id, _sel: Sel, size: NSSize) {
        rep_of(receiver).declared = match size.width > 0.0 && size.height > 0.0 {
            true => size,
            false => NSSize::default(),
        };
    }

    unsafe extern "C" fn tiff_representation(receiver: Id, _sel: Sel) -> Id {
        let rep = rep_of(receiver);
        // SAFETY: `-bitmapData` is `void * (id, SEL)` and this module's own.
        let data = unsafe { bitmap_data(receiver, objc::sel::get("bitmapData")) };
        if data.is_null() || rep.samples != 4 {
            return std::ptr::null_mut();
        }
        // SAFETY: the buffer is `bytes_per_row * height` bytes, which is what
        // both initialisers established.
        let pixels =
            unsafe { std::slice::from_raw_parts(data, rep.bytes_per_row * rep.height) }.to_vec();
        let bitmap = crate::raster::Bitmap {
            width: rep.width as u32,
            height: rep.height as u32,
            pixels,
        };
        let tiff = corefoundation::collections::new_data(crate::raster::encode_tiff(&bitmap));
        // SAFETY: a fresh object.
        unsafe { corefoundation::api::CFAutorelease(tiff) as Id }
    }

    pub(crate) fn install() {
        let class = objc::class::lookup(c"NSBitmapImageRep").expect("declared before its methods");
        class_methods!(class, {
            "imageRepWithData:" => init_with_data, c"@24@0:8@16";
        });
        methods!(class, {
            "initWithData:" => init_with_data, c"@24@0:8@16";
            "initWithCGImage:" => init_with_cg_image, c"@24@0:8^{CGImage=}16";
            "initWithFocusedViewRect:" => init_with_focused_view_rect,
                c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
            "initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bitmapFormat:bytesPerRow:bitsPerPixel:"
                => init_with_planes, c"@104@0:8^*16q24q32q40q48c56c64@72Q80q88q96";
            "initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:"
                => init_with_planes_default_format, c"@96@0:8^*16q24q32q40q48c56c64@72q80q88";
            "bitmapData" => bitmap_data, c"^v16@0:8";
            "pixelsWide" => pixels_wide, c"q16@0:8";
            "pixelsHigh" => pixels_high, c"q16@0:8";
            "samplesPerPixel" => samples_per_pixel, c"q16@0:8";
            "bitsPerSample" => bits_per_sample, c"q16@0:8";
            "bitsPerPixel" => bits_per_pixel, c"q16@0:8";
            "bytesPerRow" => bytes_per_row, c"q16@0:8";
            "hasAlpha" => has_alpha, c"c16@0:8";
            "isPlanar" => is_planar, c"c16@0:8";
            "size" => rep_size, c"{CGSize=dd}16@0:8";
            "setSize:" => set_rep_size, c"v32@0:8{CGSize=dd}16";
            "TIFFRepresentation" => tiff_representation, c"@16@0:8";
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The pipeline the runner uses, end to end: an image made from a BMP,
        /// asked for a TIFF, handed to a rep, and read as pixels.
        #[test]
        fn a_bmp_reaches_the_guest_as_rgba_through_a_tiff() {
            crate::bootstrap();
            let mut bmp = b"BM".to_vec();
            bmp.resize(10, 0);
            bmp.extend_from_slice(&54u32.to_le_bytes());
            bmp.extend_from_slice(&40u32.to_le_bytes());
            bmp.extend_from_slice(&1i32.to_le_bytes());
            bmp.extend_from_slice(&1i32.to_le_bytes());
            bmp.extend_from_slice(&1u16.to_le_bytes());
            bmp.extend_from_slice(&24u16.to_le_bytes());
            bmp.extend_from_slice(&0u32.to_le_bytes());
            bmp.resize(54, 0);
            // One pixel: blue, green, red in BMP order.
            bmp.extend_from_slice(&[0x10, 0x20, 0x30, 0]);

            let data = corefoundation::collections::new_data(bmp);
            let image =
                objc::object::create_instance(objc::class::lookup(c"NSImage").expect("NSImage"), 0);
            // SAFETY: `-initWithData:` is `id (id, SEL, id)`.
            let image = unsafe {
                super::super::init_with_data(image, objc::sel::get("initWithData:"), data as Id)
            };
            assert!(
                !image.is_null(),
                "a BMP is an image this runtime recognises"
            );

            // SAFETY: `-TIFFRepresentation` is `id (id, SEL)`.
            let tiff = unsafe {
                super::super::tiff_representation(image, objc::sel::get("TIFFRepresentation"))
            };
            assert!(!tiff.is_null());

            let rep = objc::object::create_instance(
                objc::class::lookup(c"NSBitmapImageRep").expect("NSBitmapImageRep"),
                0,
            );
            // SAFETY: `-initWithData:` is `id (id, SEL, id)`.
            let rep = unsafe { init_with_data(rep, objc::sel::get("initWithData:"), tiff) };
            assert!(!rep.is_null(), "the rep reads the TIFF this tree wrote");

            // SAFETY: `-bitmapData` is `void * (id, SEL)`.
            let pixels = unsafe { bitmap_data(rep, objc::sel::get("bitmapData")) };
            // SAFETY: one RGBA pixel, as the rep reports.
            let rgba = unsafe { std::slice::from_raw_parts(pixels, 4) };
            assert_eq!(rgba, &[0x30, 0x20, 0x10, 0xff], "red, green, blue, opaque");

            // The same image hands out a `CGImage` of those pixels, and the
            // same one each time, because the image owns it.
            let sel = objc::sel::get("CGImageForProposedRect:context:hints:");
            let null = std::ptr::null_mut();
            // SAFETY: `id (id, SEL, NSRect *, id, id)`, all of which may be nil.
            let cg_image = unsafe {
                super::super::cg_image_for_proposed_rect(
                    image,
                    sel,
                    std::ptr::null_mut(),
                    null,
                    null,
                )
            };
            assert!(!cg_image.is_null());
            assert_eq!(
                unsafe {
                    super::super::cg_image_for_proposed_rect(
                        image,
                        sel,
                        std::ptr::null_mut(),
                        null,
                        null,
                    )
                },
                cg_image
            );
            let (width, height, bgra) =
                crate::graphics::image_bgra(cg_image).expect("a layout image_bgra reads");
            assert_eq!((width, height), (1, 1));
            assert_eq!(bgra, [0x10, 0x20, 0x30, 0xff]);
            // SAFETY: both are `NSInteger (id, SEL)`.
            unsafe {
                assert_eq!(pixels_wide(rep, objc::sel::get("pixelsWide")), 1);
                assert_eq!(samples_per_pixel(rep, objc::sel::get("samplesPerPixel")), 4);
            }
        }

        /// A `CGImage` read back as a rep: the same pixels, in the rep's RGBA
        /// order rather than the compositor's BGRA, and in the rep's own buffer.
        #[test]
        fn a_cg_image_reads_back_as_rgba_samples() {
            crate::bootstrap();
            let cg_image = crate::graphics::image_from_bitmap(crate::raster::Bitmap {
                width: 2,
                height: 1,
                pixels: vec![0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0xff],
            });
            let rep = objc::object::create_instance(
                objc::class::lookup(c"NSBitmapImageRep").expect("NSBitmapImageRep"),
                0,
            );
            // SAFETY: `-initWithCGImage:` is `id (id, SEL, CGImageRef)`.
            let rep = unsafe {
                init_with_cg_image(rep, objc::sel::get("initWithCGImage:"), cg_image.cast())
            };
            assert!(!rep.is_null());
            // SAFETY: each is the accessor its selector declares.
            unsafe {
                assert_eq!(pixels_wide(rep, objc::sel::get("pixelsWide")), 2);
                assert_eq!(pixels_high(rep, objc::sel::get("pixelsHigh")), 1);
                assert_eq!(bytes_per_row(rep, objc::sel::get("bytesPerRow")), 8);
                assert_eq!(has_alpha(rep, objc::sel::get("hasAlpha")), 1);
                let data = bitmap_data(rep, objc::sel::get("bitmapData"));
                // SAFETY: two RGBA pixels, as the rep reports.
                let pixels = std::slice::from_raw_parts(data, 8);
                assert_eq!(pixels, &[0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0xff]);
                // The rep copied: releasing the image leaves it readable.
                corefoundation::api::CFRelease(cg_image);
                assert_eq!(*pixels.first().expect("a pixel"), 0x10);
            }
        }

        /// A rep measures its pixels until it is told otherwise, and then
        /// measures what it was told - which is how a 2x rep says it is half as
        /// many points as pixels.
        #[test]
        fn a_rep_reports_the_size_it_was_given() {
            crate::bootstrap();
            let rep = objc::object::create_instance(
                objc::class::lookup(c"NSBitmapImageRep").expect("NSBitmapImageRep"),
                0,
            );
            let rep = adopt(rep, crate::raster::Bitmap::new(64, 32));
            let get = objc::sel::get("size");
            let set = objc::sel::get("setSize:");
            // SAFETY: `-size` is `NSSize (id, SEL)` and `-setSize:` its setter.
            unsafe {
                assert_eq!(
                    rep_size(rep, get),
                    NSSize {
                        width: 64.0,
                        height: 32.0
                    },
                    "the pixels, until something says otherwise"
                );
                let half = NSSize {
                    width: 32.0,
                    height: 16.0,
                };
                set_rep_size(rep, set, half);
                assert_eq!(rep_size(rep, get), half);
                // The pixels are unchanged: a size is points, not samples.
                assert_eq!(pixels_wide(rep, objc::sel::get("pixelsWide")), 64);
                assert_eq!(bytes_per_row(rep, objc::sel::get("bytesPerRow")), 256);
                // And a size no drawing could use puts it back on its pixels.
                set_rep_size(rep, set, NSSize::default());
                assert_eq!(
                    rep_size(rep, get),
                    NSSize {
                        width: 64.0,
                        height: 32.0
                    }
                );
            }
        }

        /// Nil rather than an empty rep for what is not an image at all, which
        /// is what a caller checking the result is told.
        #[test]
        fn a_non_image_makes_no_rep() {
            crate::bootstrap();
            let rep = objc::object::create_instance(
                objc::class::lookup(c"NSBitmapImageRep").expect("NSBitmapImageRep"),
                0,
            );
            let sel = objc::sel::get("initWithCGImage:");
            // SAFETY: `-initWithCGImage:` with null, and with an object that is
            // not a `CGImage`.
            unsafe {
                assert!(init_with_cg_image(rep, sel, std::ptr::null()).is_null());
                let string = foundation::string::from_str("not an image");
                assert!(init_with_cg_image(rep, sel, string.cast()).is_null());
            }
        }

        /// A buffer the guest supplied is the buffer `-bitmapData` gives back,
        /// not a copy of it - the guest draws through that pointer.
        #[test]
        fn a_supplied_plane_is_not_copied() {
            crate::bootstrap();
            let mut buffer = vec![0u8; 16];
            let mut planes = [buffer.as_mut_ptr()];
            let rep = objc::object::create_instance(
                objc::class::lookup(c"NSBitmapImageRep").expect("NSBitmapImageRep"),
                0,
            );
            // SAFETY: the prototype is the eleven-argument initialiser's.
            let rep = unsafe {
                init_with_planes(
                    rep,
                    objc::sel::get("initWithBitmapDataPlanes:"),
                    planes.as_mut_ptr(),
                    2,
                    2,
                    8,
                    4,
                    1,
                    0,
                    std::ptr::null_mut(),
                    0,
                    8,
                    32,
                )
            };
            // SAFETY: `-bitmapData` is `void * (id, SEL)`.
            let got = unsafe { bitmap_data(rep, objc::sel::get("bitmapData")) };
            assert_eq!(got, buffer.as_mut_ptr());
        }

        /// The legacy AppKit text path snapshots a blank `NSImage` through the
        /// current focus and expects a stable four-channel bitmap buffer.
        #[test]
        fn a_focused_image_becomes_a_bitmap_rep() {
            crate::bootstrap();
            let image = super::super::empty(NSSize {
                width: 7.0,
                height: 3.0,
            });
            // SAFETY: the image is live for the whole focus interval.
            unsafe { super::super::lock_focus(image, objc::sel::get("lockFocus")) };
            let rep = objc::object::create_instance(
                objc::class::lookup(c"NSBitmapImageRep").expect("NSBitmapImageRep"),
                0,
            );
            let rect = NSRect {
                origin: foundation::geometry::NSPoint::default(),
                size: NSSize {
                    width: 7.0,
                    height: 3.0,
                },
            };
            // SAFETY: the designated initialiser takes this live focused rect.
            let rep = unsafe {
                init_with_focused_view_rect(rep, objc::sel::get("initWithFocusedViewRect:"), rect)
            };
            // SAFETY: balances the lock above.
            unsafe { super::super::unlock_focus(image, objc::sel::get("unlockFocus")) };
            assert!(!rep.is_null());
            // SAFETY: accessors on the rep just created.
            unsafe {
                assert_eq!(pixels_wide(rep, objc::sel::get("pixelsWide")), 7);
                assert_eq!(pixels_high(rep, objc::sel::get("pixelsHigh")), 3);
                assert_eq!(bytes_per_row(rep, objc::sel::get("bytesPerRow")), 28);
                assert!(!bitmap_data(rep, objc::sel::get("bitmapData")).is_null());
            }
        }
    }
}
