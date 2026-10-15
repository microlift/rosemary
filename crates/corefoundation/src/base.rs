use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;

/// A retained, non-null CoreFoundation object.
pub type CFTypeRef = *const c_void;

/// `CFIndex`: a signed `long`, and negative for "not found".
pub type CFIndex = isize;

/// `CFTypeID`. Apple's is a small integer; here it is the class pointer, which
/// is the same thing said more directly - `CFGetTypeID(x) == CFStringGetTypeID()`
/// asks whether `x` is a string, and the class answers that exactly.
pub type CFTypeID = usize;

/// The type ID of `class`.
pub fn type_id(class: &'static Class) -> CFTypeID {
    objc::class::ptr(class) as usize
}

/// The classes that are CoreFoundation types rather than private subclasses
/// of one.
///
/// `__NSCFConstantString` is a `__NSCFString` and must answer
/// `CFStringGetTypeID()`, not a type ID of its own: a guest that switches on
/// `CFGetTypeID` - and `libYoYoGamepad` does, on every value IOKit hands it -
/// would otherwise fall through every arm for a string that came from an image
/// rather than the heap.
static TYPES: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

fn note_type(class: &'static Class) {
    TYPES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(objc::class::ptr(class) as usize);
}

/// The `CFTypeID` an object of `class` reports: the nearest ancestor that is a
/// CoreFoundation type, or zero for something that is not one at all.
pub fn type_id_of(class: &'static Class) -> CFTypeID {
    let types = TYPES.lock().unwrap_or_else(|e| e.into_inner());
    class
        .chain()
        .map(|c| objc::class::ptr(c) as usize)
        .find(|c| types.contains(c))
        .unwrap_or(0)
}

/// The class of `object`, or `None` for null.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
pub unsafe fn class_of(object: CFTypeRef) -> Option<&'static Class> {
    unsafe { objc::object::class_of(object as Id) }
}

/// Whether `object` is an instance of `class` or one of its subclasses.
///
/// # Safety
///
/// As [`class_of`].
pub unsafe fn is_kind_of(object: CFTypeRef, class: &'static Class) -> bool {
    unsafe { class_of(object) }.is_some_and(|c| c.chain().any(|k| std::ptr::eq(k, class)))
}

/// Register a CoreFoundation class: a subclass of `NSObject` with no ivars of
/// the runtime's own.
///
/// Every CF object's layout is a `#[repr(C)]` struct in this crate rather than a
/// list of ivars, because the layouts are not ours to choose - a `__cfstring`
/// literal in the guest's `__DATA` is four words the compiler laid out, and
/// nothing gets a chance to convert it.
/// An abstract class with no storage of its own, created if it is not there.
///
/// `NSArray` and `NSMutableArray` are registered here rather than in a
/// Foundation crate, for the same reason Apple's CoreFoundation registers them:
/// `__NSCFArray` has to descend from them or a guest's `isKindOfClass:` and its
/// `_OBJC_CLASS_$_NSArray` binding do not agree with the object it is holding.
/// Foundation then adds the methods. The two crates meet in the runtime's class
/// table rather than through a dependency, which is what lets the arrow point
/// this way at all.
pub fn abstract_class(name: &std::ffi::CStr, superclass: Option<&'static Class>) -> &'static Class {
    register(name, superclass, size_of::<usize>(), false)
}

/// The chain `NSFoo` -> `NSMutableFoo`, for a concrete class to descend from.
pub fn mutable_pair(immutable: &std::ffi::CStr, mutable: &std::ffi::CStr) -> &'static Class {
    let base = abstract_class(immutable, None);
    abstract_class(mutable, Some(base))
}

pub fn register(
    name: &std::ffi::CStr,
    superclass: Option<&'static Class>,
    instance_size: usize,
    is_type: bool,
) -> &'static Class {
    objc::bootstrap();
    if let Some(existing) = objc::class::lookup(name) {
        return existing;
    }
    let superclass = superclass.unwrap_or_else(|| {
        objc::class::lookup(c"NSObject").expect("NSObject is the runtime's own")
    });
    // The extra bytes are everything past what the superclass already accounts
    // for. `class_addIvar` is not used: the fields are this crate's, and naming
    // them to the runtime would only invite something to move them.
    let extra = instance_size.saturating_sub(superclass.instance_size());
    let class = match objc::class::allocate_pair(Some(superclass), name, extra) {
        Some(class) => class,
        // Another thread registered this name between the lookup above and
        // here. Its class is as good as this one would have been, and two
        // classes of one name is the outcome that must not happen.
        None => return objc::class::lookup(name).expect("taken by another thread"),
    };
    objc::class::register_pair(class);
    if is_type {
        note_type(class);
    }
    class
}

/// Mark `class` as the CoreFoundation type its subclasses report.
///
/// Used where the type is the *abstract* class - `CFGetTypeID` of any array
/// answers `CFArrayGetTypeID()`, whether the object is a `__NSCFArray` or
/// something a Foundation crate added underneath `NSArray` later.
pub fn note_type_class(class: &'static Class) {
    note_type(class);
}

/// Allocate an instance of `class` whose body is `T`, and hand back the body.
///
/// The returned pointer is the object *and* the struct: `T`'s first field is the
/// `isa`, which is what makes a CF object an Objective-C one.
///
/// # Safety
///
/// `T` must begin with an `isa`-sized field and be the layout `class` was
/// registered with.
pub unsafe fn allocate<T>(class: &'static Class) -> *mut T {
    // A class registered abstract has room for its `isa` and nothing else, so
    // writing `T` into one walks off the end of the allocation and corrupts the
    // heap - which surfaces as a trap in `malloc` somewhere else entirely. The
    // size is what the registration decided, so this is a mismatch between two
    // lines of this runtime and belongs here rather than in a caller.
    assert!(
        class.instance_size() >= size_of::<T>(),
        "{} was registered with {} bytes but is allocated as {} of {}",
        class.name_str(),
        class.instance_size(),
        std::any::type_name::<T>(),
        size_of::<T>(),
    );
    let object = objc::object::create_instance(class, 0);
    object as *mut T
}

/// Handles this tree hands out that are *not* objects.
///
/// An `IOHIDDeviceRef` is an `Arc` and an `IOHIDElementRef` is a Rust struct:
/// neither begins with an isa, and a guest is entitled to pass either to
/// `CFRetain`, because both are CoreFoundation types as far as its headers are
/// concerned.
/// Without this they look exactly like an object some other CoreFoundation made
/// - the isa check in [`crate::foreign`] cannot tell them apart - and retaining
/// one calls Apple's `objc_retain` on a structure this tree laid out.
///
/// Retaining and releasing one is a no-op: a manager, a device and its element
/// handles live for as long as the subsystem.
static OPAQUE: std::sync::Mutex<std::collections::BTreeMap<usize, CFTypeID>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Record a handle before it is handed to a guest.
pub fn note_opaque(handle: *const c_void) {
    note_typed_opaque(handle, 0);
}

/// Record an opaque handle and the CoreFoundation type it reports.
///
/// IOKit's references are CF types even though their implementation here is a
/// Rust allocation with no Objective-C `isa`. Keeping the type beside the
/// address lets `CFGetTypeID` answer without reading the allocation as an
/// object.
pub fn note_typed_opaque(handle: *const c_void, type_id: CFTypeID) {
    if handle.is_null() {
        return;
    }
    OPAQUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(handle as usize, type_id);
}

/// Forget one, for the handles that are freed rather than immortal.
pub fn forget_opaque(handle: *const c_void) {
    OPAQUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(handle as usize));
}

/// Whether `handle` is one of them.
pub fn is_opaque(handle: CFTypeRef) -> bool {
    !handle.is_null()
        && OPAQUE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&(handle as usize))
}

/// The registered type of an opaque handle, including `Some(0)` for an
/// untyped one.
pub fn opaque_type_id(handle: CFTypeRef) -> Option<CFTypeID> {
    (!handle.is_null())
        .then(|| {
            OPAQUE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&(handle as usize))
                .copied()
        })
        .flatten()
}

/// Each `(Mach-O name, text)` as a cell holding the interned string.
///
/// A pointer-typed global is a *variable*, so the symbol is a cell and the
/// string goes inside it - see [`global_cell`] and [`interned_name`] for the two
/// halves. Seven modules wrote this loop out; the tables they wrote it over are
/// data already, so this is a function rather than anything cleverer.
pub fn string_globals(entries: &[(&str, &str)]) -> Vec<(String, u64)> {
    entries
        .iter()
        .map(|(symbol, text)| (symbol.to_string(), global_cell(symbol, interned_name(text))))
        .collect()
}

/// [`string_globals`] for the globals whose text *is* their name.
///
/// The Mach-O underscore is added here, so the symbol and the string it holds
/// come from one spelling rather than two that could disagree.
pub fn named_string_globals(names: &[&str]) -> Vec<(String, u64)> {
    names
        .iter()
        .map(|name| {
            let symbol = format!("_{name}");
            let cell = global_cell(&symbol, interned_name(name));
            (symbol, cell)
        })
        .collect()
}

/// A constant string global's object, the same one every time it is asked for.
///
/// Five modules had a copy of this, and each had its own table - so the same
/// text reached from two of them was two objects. Identity is the whole point:
/// a guest compares the constant it was handed against the one it imported, and
/// two strings with equal text are not the same string.
///
/// Not [`crate::string::immortal`], which builds a `__CFConstantString`. That is
/// a different class, and `preferences` wants it deliberately; this is the
/// ordinary string every other caller was building.
pub fn interned_name(text: &str) -> u64 {
    static STRINGS: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());
    crate::bootstrap();
    let mut strings = STRINGS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = strings.iter().find(|(t, _)| t == text) {
        return *object;
    }
    let object = crate::string::from_bytes(text.as_bytes(), false);
    // Retained forever: a global a guest may release is one it can make dangle.
    objc::arc::make_immortal(object as objc::object::Id);
    let address = object as usize as u64;
    strings.push((text.to_string(), address));
    address
}

/// The address of the **variable** a pointer-typed global names.
///
/// `kCFRunLoopDefaultMode` is a `CFStringRef` variable, not an object. A guest
/// loads the symbol's address out of its GOT and then loads the object out of
/// *that*, so a symbol defined as the object itself hands the guest the object's
/// `isa` - and `kCFAllocatorDefault`, whose value is null, hands it address
/// zero. Interned by name, because the cell has to be the same one every time a
/// provider is built.
pub fn global_cell(name: &str, value: u64) -> u64 {
    static CELLS: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());
    let mut cells = CELLS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, addr)) = cells.iter().find(|(n, _)| n == name) {
        return *addr;
    }
    // Leaked: the guest holds the address for the life of the process, which is
    // exactly as long as a real framework's `__DATA` word lives.
    let addr = Box::leak(Box::new(value)) as *mut u64 as usize as u64;
    cells.push((name.to_string(), addr));
    addr
}
