// The C spellings are the interface; renaming them would only hide it.
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::mem::size_of;

use crate::object::Id;
use crate::sel::Sel;

/// Send `receiver` a nullary message returning an object.
///
/// # Safety
///
/// `receiver` must be null or a live object.
unsafe fn send0(receiver: Id, sel: Sel) -> Id {
    let Some(class) = (unsafe { crate::object::class_of(receiver) }) else {
        return std::ptr::null_mut();
    };
    crate::class::ensure_initialized(class);
    let Some(imp) = class.method(sel) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a nullary message returning an object is `id (*)(id, SEL)`.
    let f: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(imp.implementation()) };
    unsafe { f(receiver, sel) }
}

/// Send `receiver` a one-argument message returning a `BOOL`.
///
/// # Safety
///
/// As [`send0`], and the method must take one word and return a `BOOL`.
unsafe fn send1_bool(receiver: Id, sel: Sel, argument: usize) -> bool {
    let Some(class) = (unsafe { crate::object::class_of(receiver) }) else {
        return false;
    };
    crate::class::ensure_initialized(class);
    let Some(imp) = class.method(sel) else {
        return false;
    };
    let f: unsafe extern "C" fn(Id, Sel, usize) -> bool =
        unsafe { std::mem::transmute(imp.implementation()) };
    unsafe { f(receiver, sel, argument) }
}

/// Stop on a `cls` this runtime did not build.
///
/// The usual one is a guest `class_t` whose registration was refused, which
/// the guest still holds in its class references. Read as a class, its
/// garbage method-table lock hangs the first lookup instead of failing.
fn expect_class(cls: Id, function: &str) {
    if crate::debug::enabled()
        && !cls.is_null()
        && !crate::class::is_known(cls as *const crate::class::Class)
    {
        crate::fail!(
            "{function}({cls:p}): not a class this runtime built; a guest class whose \
             registration was refused (see the refusals logged at load) is the usual cause"
        );
    }
}

pub unsafe extern "C" fn objc_alloc(cls: Id) -> Id {
    expect_class(cls, "objc_alloc");
    // SAFETY: a class is an object.
    unsafe { send0(cls, crate::sel::get("alloc")) }
}

pub unsafe extern "C" fn objc_allocWithZone(cls: Id) -> Id {
    expect_class(cls, "objc_allocWithZone");
    unsafe { send0(cls, crate::sel::get("allocWithZone:")) }
}

pub unsafe extern "C" fn objc_alloc_init(cls: Id) -> Id {
    expect_class(cls, "objc_alloc_init");
    // SAFETY: checked a class above.
    let object = unsafe { send0(cls, crate::sel::get("alloc")) };
    // SAFETY: a freshly allocated object.
    unsafe { send0(object, crate::sel::get("init")) }
}

pub unsafe extern "C" fn objc_opt_new(cls: Id) -> Id {
    expect_class(cls, "objc_opt_new");
    // SAFETY: a class is an object.
    unsafe { send0(cls, crate::sel::get("new")) }
}

pub unsafe extern "C" fn objc_opt_self(object: Id) -> Id {
    crate::debug::validate(object, "objc_opt_self");
    object
}

pub unsafe extern "C" fn objc_opt_class(object: Id) -> *mut c_void {
    if object.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a live object.
    let Some(class) = (unsafe { crate::object::class_of(object) }) else {
        return std::ptr::null_mut();
    };
    // A class answers `-class` with itself, not with its metaclass, so the
    // metaclass hop `object_getClass` would take has to be undone here.
    match class.is_meta() {
        true => object,
        false => crate::class::ptr(class) as *mut c_void,
    }
}

pub unsafe extern "C" fn objc_opt_isKindOfClass(object: Id, cls: *const c_void) -> bool {
    // SAFETY: a live object, and a class pointer.
    unsafe { send1_bool(object, crate::sel::get("isKindOfClass:"), cls as usize) }
}

pub unsafe extern "C" fn objc_opt_respondsToSelector(object: Id, sel: Sel) -> bool {
    // SAFETY: a live object, and a selector.
    unsafe { send1_bool(object, crate::sel::get("respondsToSelector:"), sel as usize) }
}

/// A lock stripe per address, as libobjc's `StripedMap` hashes them. Each
/// accessor family has its own table, like Apple's, so a struct copy never
/// waits on an object property that happens to share a stripe.
struct Stripes([std::sync::Mutex<()>; 64]);

impl Stripes {
    const fn new() -> Stripes {
        Stripes([const { std::sync::Mutex::new(()) }; 64])
    }

    fn index(address: *const c_void) -> usize {
        let address = address as usize;
        ((address >> 4) ^ (address >> 9)) % 64
    }

    fn lock(&self, address: *const c_void) -> std::sync::MutexGuard<'_, ()> {
        self.0[Self::index(address)]
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Both stripes, in index order so two copies running in opposite
    /// directions cannot each hold the lock the other waits for.
    fn lock_two(
        &self,
        a: *const c_void,
        b: *const c_void,
    ) -> (
        std::sync::MutexGuard<'_, ()>,
        Option<std::sync::MutexGuard<'_, ()>>,
    ) {
        let (a, b) = (Self::index(a), Self::index(b));
        let lock = |i: usize| self.0[i].lock().unwrap_or_else(|e| e.into_inner());
        let first = lock(a.min(b));
        let second = (a != b).then(|| lock(a.max(b)));
        (first, second)
    }
}

static PROPERTY_LOCKS: Stripes = Stripes::new();
static STRUCT_LOCKS: Stripes = Stripes::new();
static CPP_OBJECT_LOCKS: Stripes = Stripes::new();

/// `@property` accessors the compiler folds, for a property whose ivar it names
/// by byte offset.
///
/// An atomic getter retains under the slot's stripe lock and an atomic setter
/// swaps under it, so a getter can never retain a value a racing setter has
/// already released. The release itself happens outside the lock, since it can
/// run `-dealloc`.
pub unsafe extern "C" fn objc_getProperty(
    receiver: Id,
    _sel: Sel,
    offset: isize,
    atomic: bool,
) -> Id {
    if receiver.is_null() {
        return std::ptr::null_mut();
    }
    crate::debug::validate_range(receiver, offset, size_of::<Id>(), "objc_getProperty");
    // SAFETY: the compiler computed `offset` against this object's own class.
    let slot = unsafe { (receiver as *const u8).offset(offset) as *const Id };
    let value = match atomic {
        true => {
            let _guard = PROPERTY_LOCKS.lock(slot.cast());
            crate::arc::retain(unsafe { *slot })
        }
        false => crate::arc::retain(unsafe { *slot }),
    };
    crate::arc::autorelease(value)
}

/// Darwin's `shouldCopy` encoding: 1 is `-copy`, 2 is `-mutableCopy`.
const MUTABLE_COPY: i8 = 2;

pub unsafe extern "C" fn objc_setProperty(
    receiver: Id,
    _sel: Sel,
    offset: isize,
    value: Id,
    atomic: bool,
    copy: i8,
) {
    let how = match copy {
        0 => Store::Retain,
        MUTABLE_COPY => Store::MutableCopy,
        _ => Store::Copy,
    };
    // SAFETY: as `objc_getProperty`.
    unsafe { store_property(receiver, offset, value, atomic, how) }
}

/// The specialized spellings, whose arguments are in a different order from
/// [`objc_setProperty`]'s: no `atomic` or `copy`, because the name says both.
pub unsafe extern "C" fn objc_setProperty_atomic(
    receiver: Id,
    _sel: Sel,
    value: Id,
    offset: isize,
) {
    unsafe { store_property(receiver, offset, value, true, Store::Retain) }
}

pub unsafe extern "C" fn objc_setProperty_nonatomic(
    receiver: Id,
    _sel: Sel,
    value: Id,
    offset: isize,
) {
    unsafe { store_property(receiver, offset, value, false, Store::Retain) }
}

pub unsafe extern "C" fn objc_setProperty_atomic_copy(
    receiver: Id,
    _sel: Sel,
    value: Id,
    offset: isize,
) {
    unsafe { store_property(receiver, offset, value, true, Store::Copy) }
}

pub unsafe extern "C" fn objc_setProperty_nonatomic_copy(
    receiver: Id,
    _sel: Sel,
    value: Id,
    offset: isize,
) {
    unsafe { store_property(receiver, offset, value, false, Store::Copy) }
}

/// The getter and setter of a struct-typed property. An atomic copy holds the
/// stripes of both ends, so a reader never sees half of a writer's struct;
/// `has_strong` only matters to a garbage collector.
pub unsafe extern "C" fn objc_copyStruct(
    dest: *mut c_void,
    src: *const c_void,
    size: isize,
    atomic: bool,
    _has_strong: bool,
) {
    if size <= 0 {
        return;
    }
    let _guards = atomic.then(|| STRUCT_LOCKS.lock_two(src, dest));
    // SAFETY: the compiler passes an ivar and a local of `size` bytes each.
    unsafe { std::ptr::copy(src as *const u8, dest as *mut u8, size as usize) };
}

/// `void objc_copyCppObjectAtomic(void *dest, const void *src,
/// void (*copyHelper)(void *dest, const void *source))`: the accessor of an
/// `atomic` property of C++ class type. The helper is the class's copy
/// assignment; the locks are what make it atomic against other accessors of
/// either object.
pub unsafe extern "C" fn objc_copyCppObjectAtomic(
    dest: *mut c_void,
    src: *const c_void,
    helper: Option<unsafe extern "C" fn(*mut c_void, *const c_void)>,
) {
    let Some(helper) = helper else {
        return;
    };
    let _guards = CPP_OBJECT_LOCKS.lock_two(src, dest);
    // SAFETY: the compiler's helper for these two objects.
    unsafe { helper(dest, src) };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Store {
    Retain,
    Copy,
    MutableCopy,
}

/// libobjc's `reallySetProperty`.
///
/// # Safety
///
/// `offset` must be an ivar offset of `receiver`'s own class.
unsafe fn store_property(receiver: Id, offset: isize, value: Id, atomic: bool, how: Store) {
    if receiver.is_null() {
        return;
    }
    crate::debug::validate_range(receiver, offset, size_of::<Id>(), "objc_setProperty");
    let slot = unsafe { (receiver as *mut u8).offset(offset) as *mut Id };
    // The new value is owned (+1) before it is published.
    let value = match how {
        Store::Copy => copy_of(value, "copy"),
        Store::MutableCopy => copy_of(value, "mutableCopy"),
        Store::Retain => {
            // SAFETY: the caller's slot; a racy read only skips a no-op store.
            if unsafe { *slot } == value {
                return;
            }
            crate::arc::retain(value)
        }
    };
    let previous = match atomic {
        true => {
            let _guard = PROPERTY_LOCKS.lock(slot.cast());
            unsafe { slot.replace(value) }
        }
        false => unsafe { slot.replace(value) },
    };
    crate::arc::release(previous);
}

/// An owned `-copy` (or `-mutableCopy`), or the object itself retained where
/// the class does not implement it.
fn copy_of(value: Id, selector: &str) -> Id {
    if value.is_null() {
        return value;
    }
    // SAFETY: a live object.
    let copied = unsafe { send0(value, crate::sel::get(selector)) };
    match copied.is_null() {
        true => crate::arc::retain(value),
        false => copied,
    }
}

/// `objc_retainBlock` is `_Block_copy` under another name.
///
/// A stack block has to be *copied*, not retained: retaining without copying
/// would hand back a pointer into a frame that is about to go away. See
/// [`crate::block`], which is this tree's libclosure.
pub unsafe extern "C" fn objc_retainBlock(block: Id) -> Id {
    crate::block::copy(block)
}

/// The handler `objc_setEnumerationMutationHandler` installed, or zero.
static MUTATION_HANDLER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Mutating a collection while enumerating it: the program's handler if it set
/// one, otherwise a named abort, as on Apple's.
pub unsafe extern "C" fn objc_enumerationMutation(object: Id) {
    let handler = MUTATION_HANDLER.load(std::sync::atomic::Ordering::Acquire);
    if handler != 0 {
        // SAFETY: only `objc_setEnumerationMutationHandler` stores here, and it
        // stores a `void (*)(id)`.
        let handler: unsafe extern "C" fn(Id) = unsafe { std::mem::transmute(handler) };
        return unsafe { handler(object) };
    }
    // SAFETY: a live object.
    let class = unsafe { crate::object::class_of(object) };
    crate::fail!(
        "collection was mutated while being enumerated: {}",
        class.map_or("nil", |c| c.name_str())
    );
}

pub unsafe extern "C" fn objc_setEnumerationMutationHandler(
    handler: Option<unsafe extern "C" fn(Id)>,
) {
    let handler = handler.map_or(0, |h| h as usize);
    MUTATION_HANDLER.store(handler, std::sync::atomic::Ordering::Release);
}

/// The `std::terminate` an uncaught Objective-C exception reaches.
pub unsafe extern "C" fn objc_terminate() -> ! {
    crate::fail!("objc_terminate");
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn assign(dest: *mut c_void, src: *const c_void) {
        unsafe { *(dest as *mut [u64; 4]) = *(src as *const [u64; 4]) };
    }

    #[test]
    fn a_cpp_object_copy_runs_the_helper_between_the_two_objects() {
        let src = [1u64, 2, 3, 4];
        let mut dest = [0u64; 4];
        unsafe {
            objc_copyCppObjectAtomic(
                (&mut dest as *mut [u64; 4]).cast(),
                (&src as *const [u64; 4]).cast(),
                Some(assign),
            )
        };
        assert_eq!(dest, src);
    }

    #[test]
    fn a_cpp_object_copy_onto_itself_does_not_deadlock() {
        let mut both = [5u64; 4];
        let p = (&mut both as *mut [u64; 4]).cast::<c_void>();
        unsafe { objc_copyCppObjectAtomic(p, p, Some(assign)) };
        assert_eq!(both, [5; 4]);
    }

    use crate::api::{
        class_addMethod, class_createInstance, class_getInstanceSize, objc_allocateClassPair,
        objc_autoreleasePoolPop, objc_autoreleasePoolPush, objc_registerClassPair, objc_release,
    };
    use crate::class::Imp;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const ALIVE: u64 = 0xA11E;
    const DEAD: u64 = 0xDEAD;
    const COPIED: u64 = 0xC0B1;

    fn nsobject() -> *mut c_void {
        crate::bootstrap();
        crate::class::ptr(crate::class::lookup(c"NSObject").unwrap()).cast()
    }

    /// The word after an instance's own ivars, where these tests keep a marker.
    unsafe fn marker(object: Id) -> *mut u64 {
        let size = unsafe {
            class_getInstanceSize(
                crate::object::class_of(object)
                    .map_or(std::ptr::null(), |c| crate::class::ptr(c).cast()),
            )
        };
        unsafe { (object as *mut u8).add(size).cast() }
    }

    // Marks rather than frees, so a racing reader can still look.
    unsafe extern "C" fn mark_dead(object: Id, _sel: Sel) {
        unsafe { *marker(object) = DEAD };
    }

    unsafe extern "C" fn copy_marked(object: Id, _sel: Sel) -> Id {
        let class = unsafe { crate::object::class_of(object) }.unwrap();
        let copy = unsafe { class_createInstance(crate::class::ptr(class).cast(), 8) };
        unsafe { *marker(copy) = COPIED };
        copy
    }

    fn value_class(name: &std::ffi::CStr) -> *mut c_void {
        unsafe {
            let cls = objc_allocateClassPair(nsobject(), name.as_ptr(), 0);
            assert!(!cls.is_null());
            assert!(class_addMethod(
                cls,
                crate::sel::get("dealloc"),
                mark_dead as Imp,
                c"v@:".as_ptr()
            ));
            assert!(class_addMethod(
                cls,
                crate::sel::get("copy"),
                copy_marked as Imp,
                c"@@:".as_ptr()
            ));
            objc_registerClassPair(cls);
            cls
        }
    }

    struct Shared(Id);
    unsafe impl Send for Shared {}
    unsafe impl Sync for Shared {}

    /// A getter that read the slot just before a setter swapped and released it
    /// must still hand back a live object: the retain happens under the same
    /// lock as the swap.
    #[test]
    fn an_atomic_getter_never_returns_a_value_its_setter_released() {
        let cls = value_class(c"AtomicGetterRaceValue");
        let holder = Arc::new(Shared(unsafe { class_createInstance(nsobject(), 8) }));
        let offset = unsafe { class_getInstanceSize(nsobject()) } as isize;
        let done = Arc::new(AtomicBool::new(false));
        let sel = crate::sel::get("value");

        let readers: Vec<_> = (0..3)
            .map(|_| {
                let (holder, done) = (Arc::clone(&holder), Arc::clone(&done));
                std::thread::spawn(move || {
                    let sel = crate::sel::get("value");
                    while !done.load(Ordering::Relaxed) {
                        let pool = unsafe { objc_autoreleasePoolPush() };
                        for _ in 0..64 {
                            let v = unsafe { objc_getProperty(holder.0, sel, offset, true) };
                            if !v.is_null() {
                                assert_eq!(unsafe { *marker(v) }, ALIVE, "got a released value");
                            }
                        }
                        unsafe { objc_autoreleasePoolPop(pool) };
                    }
                })
            })
            .collect();
        for _ in 0..100_000 {
            unsafe {
                let v = class_createInstance(cls, 8);
                *marker(v) = ALIVE;
                objc_setProperty_atomic(holder.0, sel, v, offset);
                objc_release(v);
            }
        }
        done.store(true, Ordering::Relaxed);
        for reader in readers {
            reader.join().expect("a reader saw a released value");
        }
        unsafe { objc_setProperty_atomic(holder.0, sel, std::ptr::null_mut(), offset) };
    }

    /// The generic setter's copy flag stores an owned copy, not the original
    /// and not a copy retained twice.
    #[test]
    fn the_generic_setter_honours_its_copy_flag() {
        let cls = value_class(c"GenericSetterCopyValue");
        let holder = unsafe { class_createInstance(nsobject(), 8) };
        let offset = unsafe { class_getInstanceSize(nsobject()) } as isize;
        let sel = crate::sel::get("value");
        unsafe {
            let v = class_createInstance(cls, 8);
            *marker(v) = ALIVE;
            objc_setProperty(holder, sel, offset, v, true, 1);
            let stored = *((holder as *const u8).offset(offset) as *const Id);
            assert_ne!(stored, v, "the original was stored");
            assert_eq!(*marker(stored), COPIED);
            assert_eq!(
                crate::arc::retain_count(stored),
                1,
                "the copy is owned once"
            );
            assert_eq!(
                crate::arc::retain_count(v),
                1,
                "the original is not retained"
            );

            objc_setProperty(holder, sel, offset, v, false, 0);
            assert_eq!(*marker(stored), DEAD, "the copy was released");
            assert_eq!(crate::arc::retain_count(v), 2);
            objc_setProperty(holder, sel, offset, std::ptr::null_mut(), false, 0);
            assert_eq!(crate::arc::retain_count(v), 1);
            objc_release(v);
        }
    }

    /// An atomic struct property never reads half of a write.
    #[test]
    fn an_atomic_struct_copy_is_never_torn() {
        let ivar = Arc::new(Shared(Box::into_raw(Box::new([0u64; 16])).cast()));
        let done = Arc::new(AtomicBool::new(false));
        let reader = {
            let (ivar, done) = (Arc::clone(&ivar), Arc::clone(&done));
            std::thread::spawn(move || {
                let mut local = [0u64; 16];
                while !done.load(Ordering::Relaxed) {
                    unsafe {
                        objc_copyStruct(
                            (&mut local as *mut [u64; 16]).cast(),
                            ivar.0,
                            size_of::<[u64; 16]>() as isize,
                            true,
                            false,
                        )
                    };
                    assert!(local.iter().all(|w| *w == local[0]), "torn: {local:?}");
                }
            })
        };
        for i in 0..200_000u64 {
            let value = [i; 16];
            unsafe {
                objc_copyStruct(
                    ivar.0,
                    (&value as *const [u64; 16]).cast(),
                    size_of::<[u64; 16]>() as isize,
                    true,
                    false,
                )
            };
        }
        done.store(true, Ordering::Relaxed);
        reader.join().expect("a torn struct read");
        drop(unsafe { Box::from_raw(ivar.0.cast::<[u64; 16]>()) });
    }

    const CHILD: &str = "ROSEMARY_OBJC_ALLOC_CHILD";

    #[test]
    fn allocating_from_a_pointer_that_is_not_a_class_stops_with_its_name() {
        if std::env::var(CHILD).is_ok() {
            // The class check is one of the `--objc-debug` checks.
            crate::debug::enable();
            crate::bootstrap();
            // Shaped like a guest `class_t` that never registered: an `isa`
            // word and nothing this runtime made.
            let raw = [0usize; 8];
            // SAFETY: the call is expected to stop before using it.
            unsafe { objc_alloc(raw.as_ptr() as Id) };
            return;
        }
        crate::bootstrap();
        let class = crate::class::lookup(c"NSObject").expect("the runtime's own");
        // SAFETY: a registered class.
        let object = unsafe { objc_alloc(crate::class::ptr(class) as Id) };
        assert!(!object.is_null());
        crate::arc::release(object);

        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "shorthand::tests::allocating_from_a_pointer_that_is_not_a_class_stops_with_its_name",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("run child");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "survived: {stderr}");
        assert!(
            stderr.contains("objc_alloc(") && stderr.contains("not a class"),
            "{stderr}"
        );
    }
}
