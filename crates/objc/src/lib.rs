#![allow(clippy::missing_safety_doc, clippy::not_unsafe_ptr_arg_deref)]

pub mod allocator;
pub mod api;
pub mod arc;
pub mod block;
pub mod cache;
pub mod census;
pub mod class;
pub mod debug;
pub mod msg;
mod nsobject;
pub mod object;
pub mod property;
pub mod protocol;
pub mod sel;
pub mod shorthand;

use std::sync::Once;

/// Stop, having said why, from a place a panic cannot leave.
#[macro_export]
macro_rules! fail {
    ($($arg:tt)*) => {{
        eprintln!("rosetta3: objc: {}", format_args!($($arg)*));
        std::process::abort()
    }};
}

/// Install the classes this runtime owns. Idempotent, and cheap after the first.
///
/// `NSObject` is libobjc's on the system too, so a guest that subclasses it must
/// find it before any framework has loaded.
pub fn bootstrap() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        nsobject::install();
        block::install();
    });
}

exports::exports! {
    functions {
        "sel_registerName" | "sel_getUid" => api::sel_registerName,
        "sel_getName" => api::sel_getName,
        "sel_isEqual" => api::sel_isEqual,
        "objc_getClass" => api::objc_getClass,
        "objc_lookUpClass" => api::objc_lookUpClass,
        "objc_getRequiredClass" => api::objc_getRequiredClass,
        "objc_getMetaClass" => api::objc_getMetaClass,
        "objc_allocateClassPair" => api::objc_allocateClassPair,
        "objc_registerClassPair" => api::objc_registerClassPair,
        "objc_disposeClassPair" => api::objc_disposeClassPair,
        "class_getName" => api::class_getName,
        "class_getSuperclass" => api::class_getSuperclass,
        "class_isMetaClass" => api::class_isMetaClass,
        "class_getInstanceSize" => api::class_getInstanceSize,
        "class_addMethod" => api::class_addMethod,
        "class_replaceMethod" => api::class_replaceMethod,
        "class_addIvar" => api::class_addIvar,
        "class_getInstanceVariable" => api::class_getInstanceVariable,
        "class_getInstanceMethod" => api::class_getInstanceMethod,
        "class_getClassMethod" => api::class_getClassMethod,
        "class_getMethodImplementation" => api::class_getMethodImplementation,
        "class_respondsToSelector" => api::class_respondsToSelector,
        "class_addProtocol" => api::class_addProtocol,
        "class_conformsToProtocol" => api::class_conformsToProtocol,
        "class_createInstance" => api::class_createInstance,
        "method_getImplementation" => api::method_getImplementation,
        "method_getTypeEncoding" => api::method_getTypeEncoding,
        "method_getName" => api::method_getName,
        "method_setImplementation" => api::method_setImplementation,
        "method_exchangeImplementations" => api::method_exchangeImplementations,
        "ivar_getOffset" => api::ivar_getOffset,
        "ivar_getName" => api::ivar_getName,
        "class_copyPropertyList" => api::class_copyPropertyList,
        "class_getProperty" => api::class_getProperty,
        "class_addProperty" => api::class_addProperty,
        "class_replaceProperty" => api::class_replaceProperty,
        "property_getName" => api::property_getName,
        "property_getAttributes" => api::property_getAttributes,
        "property_copyAttributeList" => api::property_copyAttributeList,
        "object_getClass" => api::object_getClass,
        "object_setClass" => api::object_setClass,
        "object_getClassName" => api::object_getClassName,
        "object_getIndexedIvars" => api::object_getIndexedIvars,
        "object_getIvar" => api::object_getIvar,
        "object_setIvar" => api::object_setIvar,
        // The destruction entry point, and the one most easily confused with
        // `objc_destructInstance` two rows down: that one unwinds C++ ivars and
        // stops, and this one destroys the object. Nothing comes back from either
        // to tell a guest which it got. See `api::object_dispose`.
        "object_dispose" => api::object_dispose,
        "objc_constructInstance" => api::objc_constructInstance,
        "objc_destructInstance" => api::objc_destructInstance,
        // The per-exception preprocessor slot, and the return of the one that was
        // in it. Stored and handed back; never called, because the exception
        // machinery that would call it is refused by name. See
        // `api::objc_setExceptionPreprocessor`.
        "objc_setExceptionPreprocessor" => api::objc_setExceptionPreprocessor,
        "objc_loadClassref" => api::objc_loadClassref,
        "objc_getProtocol" => api::objc_getProtocol,
        "objc_allocateProtocol" => api::objc_allocateProtocol,
        "objc_registerProtocol" => api::objc_registerProtocol,
        "protocol_getName" => api::protocol_getName,
        "protocol_addProtocol" => api::protocol_addProtocol,
        "protocol_addMethodDescription" => api::protocol_addMethodDescription,
        "protocol_getMethodDescription" => api::protocol_getMethodDescription,
        "objc_retain" => api::objc_retain,
        "objc_release" => api::objc_release,
        "objc_autorelease" => api::objc_autorelease,
        "objc_autoreleasePoolPush" => api::objc_autoreleasePoolPush,
        "objc_autoreleasePoolPop" => api::objc_autoreleasePoolPop,
        "objc_autoreleaseReturnValue" => api::objc_autoreleaseReturnValue,
        "objc_retainAutorelease" => api::objc_retainAutorelease,
        "objc_retainAutoreleaseReturnValue" => api::objc_retainAutoreleaseReturnValue,
        "objc_retainAutoreleasedReturnValue" => api::objc_retainAutoreleasedReturnValue,
        "objc_unsafeClaimAutoreleasedReturnValue" => api::objc_unsafeClaimAutoreleasedReturnValue,
        "objc_storeStrong" => api::objc_storeStrong,
        "objc_initWeak" => api::objc_initWeak,
        "objc_storeWeak" => api::objc_storeWeak,
        "objc_loadWeak" => api::objc_loadWeak,
        "objc_loadWeakRetained" => api::objc_loadWeakRetained,
        "objc_destroyWeak" => api::objc_destroyWeak,
        "objc_copyWeak" => api::objc_copyWeak,
        "objc_moveWeak" => api::objc_moveWeak,
        "objc_sync_enter" => api::objc_sync_enter,
        "objc_sync_exit" => api::objc_sync_exit,
        "objc_setAssociatedObject" => api::objc_setAssociatedObject,
        "objc_getAssociatedObject" => api::objc_getAssociatedObject,
        "objc_alloc" => shorthand::objc_alloc,
        "objc_allocWithZone" => shorthand::objc_allocWithZone,
        "objc_alloc_init" => shorthand::objc_alloc_init,
        "objc_opt_new" => shorthand::objc_opt_new,
        "objc_opt_self" => shorthand::objc_opt_self,
        "objc_opt_class" => shorthand::objc_opt_class,
        "objc_opt_isKindOfClass" => shorthand::objc_opt_isKindOfClass,
        "objc_opt_respondsToSelector" => shorthand::objc_opt_respondsToSelector,
        "objc_getProperty" => shorthand::objc_getProperty,
        "objc_setProperty" => shorthand::objc_setProperty,
        "objc_setProperty_atomic" => shorthand::objc_setProperty_atomic,
        "objc_setProperty_nonatomic" => shorthand::objc_setProperty_nonatomic,
        "objc_setProperty_atomic_copy" => shorthand::objc_setProperty_atomic_copy,
        "objc_setProperty_nonatomic_copy" => shorthand::objc_setProperty_nonatomic_copy,
        "objc_retainBlock" => shorthand::objc_retainBlock,
        "objc_enumerationMutation" => shorthand::objc_enumerationMutation,
        "objc_terminate" => shorthand::objc_terminate,
        "objc_msgSend" => msg::entry() as usize as u64,
        "objc_msgSendSuper" => msg::entry_super() as usize as u64,
        "objc_msgSendSuper2" => msg::entry_super2() as usize as u64,
        "objc_msgSend_stret" => msg::entry_stret() as usize as u64,
        "objc_msgSendSuper_stret" => msg::entry_super_stret() as usize as u64,
        "objc_msgSendSuper2_stret" => msg::entry_super2_stret() as usize as u64,
    }
    // libclosure, which is this runtime's for the reason `block` gives.
    groups {
        block,
    }
}

/// The classes this crate owns, as opposed to a framework's.
///
/// One, and it is the one the system's libobjc owns too.
pub const ROOT_CLASSES: &[&str] = &["NSObject"];

/// The words a guest's class structures bind as *data*, Mach-O spelling and
/// all.
///
/// Two kinds, and they are bound by different machinery for different reasons.
///
/// A class object: a guest's `Greeter : NSObject` binds `_OBJC_CLASS_$_NSObject`
/// and stores the address it gets as its own superclass pointer, so the symbol
/// has to be the class itself and not a function that returns it.
///
/// The empty cache: the third word of every `objc_class` the compiler emits, for
/// every class in the image, and the runtime *writes* into it. See
/// [`cache::empty_cache`] for why one page for the whole process is the answer
/// and why a nil check is not.
pub fn class_data() -> Vec<(String, u64)> {
    bootstrap();
    let mut data = vec![(cache::EMPTY_CACHE.to_string(), cache::empty_cache())];
    for name in ROOT_CLASSES {
        let Some(class) = std::ffi::CString::new(*name)
            .ok()
            .and_then(|n| class::lookup(&n))
        else {
            continue;
        };
        data.push((
            format!("_OBJC_CLASS_$_{name}"),
            class::ptr(class) as usize as u64,
        ));
        data.push((
            format!("_OBJC_METACLASS_$_{name}"),
            class::ptr(class.metaclass()) as usize as u64,
        ));
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nsobject_is_installed_and_its_metaclass_chain_terminates() {
        bootstrap();
        let nsobject = class::lookup(c"NSObject").expect("NSObject is this runtime's own");
        assert!(nsobject.superclass().is_none(), "NSObject is a root class");
        let meta = nsobject.metaclass();
        assert!(meta.is_meta());
        // The root metaclass's superclass is the root class, so `+alloc` and
        // `-class` are both findable from a class object.
        assert!(std::ptr::eq(meta.superclass().unwrap(), nsobject));
        // And its own `isa` is itself, which is what stops the walk.
        assert!(std::ptr::eq(meta.metaclass(), meta));
    }

    /// The cache is published under the name a Mach-O spells it, and to the page
    /// [`cache::empty_cache`] hands out rather than to a second mapping of this
    /// module's own.
    ///
    /// The name is the fragile half. There are three underscores' worth of
    /// spelling here - `objc_empty_cache` in C, `__objc_empty_cache` in a
    /// Mach-O - and getting either wrong produces a symbol nothing imports,
    /// which looks exactly like the bug this fixed.
    #[test]
    fn the_empty_cache_is_published_as_data_under_its_mach_o_name() {
        let data = class_data();
        let (name, at) = data
            .iter()
            .find(|(name, _)| name == cache::EMPTY_CACHE)
            .expect("the empty cache is data this runtime publishes");
        assert_eq!(name, "__objc_empty_cache", "verbatim, underscore and all");
        assert_eq!(*at, cache::empty_cache(), "one page, not one per ask");
        assert_ne!(*at, 0, "an unbound data import is a store through nil");
    }
}

/// The two invariants `rosetta3`'s dispatch depends on, as tests.
///
/// Neither is stated by the C API, and breaking either produces a wrong `IMP`
/// months later rather than a failure here. See the crate doc.
#[cfg(test)]
mod invariants {
    use std::ffi::c_void;

    use super::*;
    use crate::object::Id;
    use crate::sel::Sel;

    unsafe extern "C" fn first(_receiver: Id, _sel: Sel) -> u64 {
        1
    }
    unsafe extern "C" fn second(_receiver: Id, _sel: Sel) -> u64 {
        2
    }

    /// A class of this name, freshly allocated. Each test gets its own, because
    /// class names are global and the tests share a process.
    fn class_named(name: &std::ffi::CStr) -> &'static class::Class {
        bootstrap();
        let nsobject = class::lookup(c"NSObject").expect("NSObject");
        let class = class::allocate_pair(Some(nsobject), name, 0).expect("a fresh name");
        class::register_pair(class);
        class
    }

    #[test]
    fn method_get_implementation_returns_the_pointer_that_was_installed() {
        let class = class_named(c"Rosetta3InvariantA");
        let sel = sel::get("answer");
        let installed = first as *const c_void;
        assert!(class.add_method(sel, installed, c"Q16@0:8"));

        let method = class.own_method(sel).expect("just added");
        assert_eq!(
            method.implementation(),
            installed,
            "an IMP that comes back wrapped makes every guest method look like a host one"
        );

        // And still exactly the pointer after a replacement.
        let replaced = second as *const c_void;
        assert_eq!(method.set_implementation(replaced), installed);
        assert_eq!(method.implementation(), replaced);
    }

    #[test]
    fn the_method_implementation_probe_moves_when_the_resolution_does() {
        let class = class_named(c"Rosetta3InvariantB");
        let sel = sel::get("answer");
        assert!(class.add_method(sel, first as *const c_void, c"Q16@0:8"));

        let before = class::method_implementation(class, sel);
        assert_eq!(before, first as *const c_void);

        // Swizzled. A cached plan keyed on the old probe must now miss.
        class
            .own_method(sel)
            .unwrap()
            .set_implementation(second as *const c_void);
        let after = class::method_implementation(class, sel);
        assert_ne!(
            before, after,
            "the probe did not move, so a cached dispatch plan would stay stale"
        );
        assert_eq!(after, second as *const c_void);
    }

    /// A subclass overriding a superclass's method moves the probe too, which is
    /// the same invariant reached by the other route: resolution, not storage.
    #[test]
    fn an_override_added_after_the_fact_moves_the_probe() {
        let base = class_named(c"Rosetta3InvariantC");
        let sel = sel::get("answer");
        assert!(base.add_method(sel, first as *const c_void, c"Q16@0:8"));
        let derived = class::allocate_pair(Some(base), c"Rosetta3InvariantCSub", 0).unwrap();
        class::register_pair(derived);

        assert_eq!(
            class::method_implementation(derived, sel),
            first as *const c_void,
            "inherited before the override"
        );
        assert!(derived.add_method(sel, second as *const c_void, c"Q16@0:8"));
        assert_eq!(
            class::method_implementation(derived, sel),
            second as *const c_void
        );
    }

    /// `+alloc`, `-init` and `-isKindOfClass:` on a subclass of `NSObject`,
    /// through the real `objc_msgSend`.
    ///
    /// The one test that exercises the naked trampoline rather than calling an
    /// `IMP` directly.
    #[test]
    fn a_message_reaches_an_inherited_implementation_through_msg_send() {
        let class = class_named(c"Rosetta3InvariantD");
        // SAFETY: `+alloc` and `-init` are `id (*)(id, SEL)`, and
        // `-isKindOfClass:` is `BOOL (*)(id, SEL, Class)`.
        unsafe {
            let send: extern "C" fn(Id, Sel) -> Id = std::mem::transmute(msg::entry());
            let object = send(class::ptr(class) as Id, sel::get("alloc"));
            assert!(!object.is_null(), "+alloc came back nil");
            let object = send(object, sel::get("init"));

            let is_kind: extern "C" fn(Id, Sel, Id) -> bool = std::mem::transmute(msg::entry());
            assert!(is_kind(
                object,
                sel::get("isKindOfClass:"),
                class::ptr(class) as Id
            ));
            let nsobject = class::lookup(c"NSObject").unwrap();
            assert!(is_kind(
                object,
                sel::get("isKindOfClass:"),
                class::ptr(nsobject) as Id
            ));

            let release: extern "C" fn(Id, Sel) = std::mem::transmute(msg::entry());
            release(object, sel::get("release"));
        }
    }

    /// Messaging nil is the language, not an error.
    #[test]
    fn a_message_to_nil_returns_zero() {
        bootstrap();
        // SAFETY: the nil path never reaches an IMP, so any prototype is safe.
        unsafe {
            let send: extern "C" fn(Id, Sel) -> u64 = std::mem::transmute(msg::entry());
            assert_eq!(send(std::ptr::null_mut(), sel::get("answer")), 0);
            let send_f: extern "C" fn(Id, Sel) -> f64 = std::mem::transmute(msg::entry());
            assert_eq!(send_f(std::ptr::null_mut(), sel::get("answer")), 0.0);
        }
    }
}
