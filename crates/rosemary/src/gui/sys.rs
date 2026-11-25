use std::ffi::{CString, c_void};

/// The frameworks the overlay may have to bring in itself, by absolute path.
///
/// A path rather than a leaf name, because `dlopen("CoreText.framework")` is
/// answered by the loader's search paths and this must not depend on what the
/// guest has set in its environment - the guest sets `DYLD_*` variables and it
/// is not this module's business to be affected by them.
const FRAMEWORKS: &[&str] = &[
    "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation",
    "/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics",
    "/System/Library/Frameworks/CoreText.framework/CoreText",
    "/System/Library/Frameworks/OpenGL.framework/OpenGL",
    "/System/Library/Frameworks/Carbon.framework/Carbon",
];

/// Bring the frameworks in, once.
fn load_frameworks() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for path in FRAMEWORKS {
            let Ok(c) = CString::new(*path) else { continue };
            // SAFETY: a NUL-terminated path and a valid mode. The handle is
            // deliberately dropped - these are process-lifetime frameworks and
            // nothing here ever closes one.
            unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        }
    });
}

/// A function or variable by C name, from the frameworks this process selected,
/// or `None` if they do not define it.
///
/// Under `builtin` the guest's objects are this tree's, so the answer must come
/// from this tree's frameworks and libSystem: a host framework handed one of them
/// would read a foreign structure as its own.
pub fn symbol(name: &str) -> Option<*mut c_void> {
    let found = match crate::frameworks::using_builtin() {
        true => builtin_symbol(name),
        false => host_symbol(name),
    };
    found.map(|addr| addr as usize as *mut c_void)
}

fn builtin_symbol(name: &str) -> Option<u64> {
    let mangled = format!("_{name}");
    crate::frameworks::symbol(name)
        .or_else(|| crate::dyld::libsystem_symbol(name))
        .or_else(|| {
            crate::objc::host::block_data()
                .into_iter()
                .find_map(|(symbol, addr)| (symbol == mangled).then_some(addr))
        })
}

fn host_symbol(name: &str) -> Option<u64> {
    crate::host::symbol(name).or_else(|| {
        load_frameworks();
        crate::host::symbol(name)
    })
}

/// [`symbol`] as a typed function pointer, or `None`.
///
/// # Safety
///
/// The caller states that `name` has the signature `F`. Nothing checks it and nothing
/// can.
pub unsafe fn function<F: Copy>(name: &str) -> Option<F> {
    const {
        assert!(
            size_of::<F>() == size_of::<*mut c_void>(),
            "F must be a bare function pointer, not a fat one",
        );
    }
    let raw = symbol(name)?;
    // SAFETY: `F` is the caller's claim about the symbol, and the size
    // assertion above is what rules out transmuting an address into something
    // wider than a pointer.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&raw) })
}

/// An opaque `CFTypeRef`. Never dereferenced here.
pub type CfRef = *mut c_void;

/// `CGFloat`, which on every 64-bit Apple platform is a `double`.
pub type CgFloat = f64;

/// `CGPoint`, `CGSize` and `CGRect` from `<CoreGraphics/CGGeometry.h>`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Point {
    pub x: CgFloat,
    pub y: CgFloat,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Size {
    pub width: CgFloat,
    pub height: CgFloat,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

/// `kCFStringEncodingUTF8`.
pub const CF_UTF8: u32 = 0x0800_0100;

/// A `CFStringRef` for `s`, or null.
///
/// The caller owns it and must `CFRelease` it - a `Create` function is +1 by
/// the Core Foundation naming rule, and the overlay creates a handful of these
/// exactly once, so they are kept rather than released.
pub fn cf_string(s: &str) -> CfRef {
    // `CFStringCreateWithBytes(CFAllocatorRef, const UInt8*, CFIndex,
    //  CFStringEncoding, Boolean)` - bytes rather than a C string so a `&str`
    // does not have to be copied into a `CString` first.
    type Create = unsafe extern "C" fn(CfRef, *const u8, isize, u32, u8) -> CfRef;
    // SAFETY: the declaration above is `<CoreFoundation/CFString.h>`'s.
    let Some(create) = (unsafe { function::<Create>("CFStringCreateWithBytes") }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `s` is a live UTF-8 slice for the length of the call, the
    // encoding is a real one, and a null allocator means the default.
    unsafe {
        create(
            std::ptr::null_mut(),
            s.as_ptr(),
            s.len() as isize,
            CF_UTF8,
            0,
        )
    }
}
