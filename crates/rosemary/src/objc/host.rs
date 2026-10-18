use std::ffi::c_char;

use static_init::dynamic;

/// This tree's own runtime, under a name that does not read as `crate::objc`.
use ::objc as builtin;

/// An opaque `Class`. A pointer, and never dereferenced here - the whole argument for
/// using the public API is that the layout stays the runtime's business.
pub type Class = *mut std::ffi::c_void;
/// An opaque `Protocol *`.
pub type Protocol = *mut std::ffi::c_void;
/// A uniqued selector.
pub type Sel = *const std::ffi::c_void;
/// An `Ivar` description.
pub type Ivar = *const std::ffi::c_void;
/// A `Method` description: an `IMP` and a type encoding, which is the pair
/// [`crate::objc::dispatch`] needs and the reason a message is callable at all.
pub type Method = *const std::ffi::c_void;
/// A method implementation. Always one of this runtime's arm64 thunks when we
/// are the ones installing it.
pub type Imp = *const std::ffi::c_void;

/// `objc_property_attribute_t`. Both runtimes lay it out this way, because both
/// are answering the same C header.
#[repr(C)]
pub struct PropertyAttribute {
    pub name: *const c_char,
    pub value: *const c_char,
}

/// Which Objective-C runtime answers.
///
/// `Native` is the host's own libobjc, and exists only where there is one.
/// `Builtin` is [`objc`], this tree's own. The point of keeping both selectable
/// on macOS is that `Builtin` is then exercised against the same tests, on a host
/// where the answers can be compared with Apple's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    Native,
    Builtin,
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Backend::Native => "native",
            Backend::Builtin => "builtin",
        })
    }
}

/// `ROSEMARY_OBJC`: `native` or `builtin`.
///
/// Native is the default where there is a host runtime to be native to, and is
/// not offered at all where there is not.
pub fn backend() -> Backend {
    #[dynamic]
    static CHOICE: Backend = match std::env::var("ROSEMARY_OBJC").as_deref() {
        Ok("builtin") => Backend::Builtin,
        Ok("native") if cfg!(target_os = "macos") => Backend::Native,
        Ok(other) if !other.is_empty() => {
            panic!("ROSEMARY_OBJC={other}: expected `native` or `builtin`")
        }
        _ if cfg!(target_os = "macos") => Backend::Native,
        _ => Backend::Builtin,
    };
    *CHOICE
}

/// The address `name` has in the selected runtime, or `None` if it defines no
/// such symbol.
///
/// `name` is the C identifier, without the leading underscore a Mach-O export
/// carries.
pub fn symbol(name: &str) -> Option<u64> {
    match backend() {
        Backend::Native => native::symbol(name),
        Backend::Builtin => builtin::symbol(name),
    }
}

/// The class objects the selected runtime exports as *data*, Mach-O spelling
/// included.
///
/// Empty under `native`: the host's libobjc exports `_OBJC_CLASS_$_NSObject`
/// itself and an ordinary bind finds it. Under `builtin` nothing else would.
pub fn class_data() -> Vec<(String, u64)> {
    match backend() {
        Backend::Native => Vec::new(),
        Backend::Builtin => builtin::class_data(),
    }
}

/// The `_NSConcrete*Block` values, which are data and which a block literal in
/// the guest's image names directly.
///
/// Empty under `native`, where libSystem exports them and an ordinary bind finds
/// them. Under `builtin` they are [`objc::block`]'s own classes, and they have
/// to be: that module compares a block's `isa` against them to tell a block from
/// an object.
pub fn block_data() -> Vec<(String, u64)> {
    match backend() {
        Backend::Native => Vec::new(),
        Backend::Builtin => builtin::block::data_symbols(),
    }
}

/// Let a send through a `_fixup` entry point name the guest `objc_class` at
/// `guest` and reach the registered class `class` instead.
///
/// Nothing to do under `native`, whose libobjc realizes the guest's own
/// structure in place.
pub fn alias_class(guest: u64, class: u64) {
    if backend() == Backend::Builtin {
        builtin::msg::alias_class(guest as usize, class as usize as *const std::ffi::c_void);
    }
}

/// Stop the *host's* class objects reaching a runtime that is not the host's.
///
/// A guest's `Cat : NSString` binds `_OBJC_CLASS_$_NSString` as data and hands
/// whatever it gets to `class_addMethod` and to its own superclass slot. Under
/// `builtin` that must not be Apple's class object: this runtime would read a
/// foreign structure as one of its own, which is a crash inside the first method
/// table it touches. Denied, the bind is unresolved and the guest is refused by
/// name - the true answer until this tree has a Foundation of its own.
///
/// Nothing to do under `native`, where the host's class objects are the right
/// ones and `machish`'s own denials already keep the *functions* out.
pub fn deny_foreign_classes(provider: &mut machish::resolve::NativeDylibProvider) {
    if backend() == Backend::Native {
        return;
    }
    for prefix in ["_OBJC_CLASS_$_", "_OBJC_METACLASS_$_", "_OBJC_EHTYPE_$_"] {
        provider.deny_prefix(prefix);
    }
}

/// The host's own libobjc, reached by `dlsym`.
///
/// Empty off macOS, where [`backend`] never selects it.
mod native {
    #[cfg(target_os = "macos")]
    pub fn symbol(name: &str) -> Option<u64> {
        let c = std::ffi::CString::new(name).ok()?;
        // SAFETY: `RTLD_DEFAULT` is `(void *)-2` on macOS - search every loaded
        // image - and `c` is a NUL-terminated string that outlives the call. A
        // missing symbol is null, which is checked rather than dereferenced.
        let ptr = unsafe { libc::dlsym(-2isize as *mut core::ffi::c_void, c.as_ptr()) };
        (!ptr.is_null()).then_some(ptr as usize as u64)
    }

    #[cfg(not(target_os = "macos"))]
    pub fn symbol(_name: &str) -> Option<u64> {
        None
    }
}

/// Declare the runtime entry points this crate calls.
///
/// Each becomes a field holding the selected runtime's address and a wrapper of
/// the same name, so a call site reads as it would against a plain `extern` block.
/// A name the runtime does not define is a panic at the call rather than at load:
/// the diagnostic names the symbol and the backend, which a null call would not.
macro_rules! entries {
    ($(
        $(#[$meta:meta])*
        fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;
    )*) => {
        #[allow(non_snake_case)]
        struct Entries {
            $($name: Option<unsafe extern "C" fn($($ty),*) $(-> $ret)?>,)*
        }

        // SAFETY: every field is a code address in an image that outlives the
        // process, shared read-only from the moment this initializer returns.
        unsafe impl Sync for Entries {}

        #[dynamic]
        static ENTRIES: Entries = Entries {
            $($name: symbol(stringify!($name)).map(|a| {
                // SAFETY: the selected runtime defines this name, and the
                // prototype is the one its header declares.
                unsafe { std::mem::transmute::<usize, unsafe extern "C" fn($($ty),*) $(-> $ret)?>(a as usize) }
            }),)*
        };

        $(
            $(#[$meta])*
            ///
            /// # Safety
            ///
            /// The arguments must be what the runtime's own header declares.
            #[inline]
            #[allow(non_snake_case)]
            pub unsafe fn $name($($arg: $ty),*) $(-> $ret)? {
                let f = match ENTRIES.$name {
                    Some(f) => f,
                    None => missing(stringify!($name)),
                };
                unsafe { f($($arg),*) }
            }
        )*
    };
}

/// A runtime entry point the selected backend does not define.
#[cold]
#[inline(never)]
fn missing(name: &str) -> ! {
    panic!(
        "the {} Objective-C runtime does not define {name}",
        backend()
    )
}

entries! {
    fn objc_getClass(name: *const c_char) -> Class;
    fn objc_lookUpClass(name: *const c_char) -> Class;
    fn objc_allocateClassPair(superclass: Class, name: *const c_char, extra: usize) -> Class;
    fn objc_registerClassPair(cls: Class);
    fn objc_disposeClassPair(cls: Class);

    fn object_getClass(obj: Class) -> Class;
    fn class_getName(cls: Class) -> *const c_char;
    fn class_getSuperclass(cls: Class) -> Class;
    fn class_getInstanceSize(cls: Class) -> usize;
    fn class_addMethod(cls: Class, name: Sel, imp: Imp, types: *const c_char) -> bool;
    fn class_addIvar(
        cls: Class,
        name: *const c_char,
        size: usize,
        alignment: u8,
        types: *const c_char,
    ) -> bool;
    fn class_addProtocol(cls: Class, protocol: Protocol) -> bool;
    fn class_addProperty(
        cls: Class,
        name: *const c_char,
        attributes: *const PropertyAttribute,
        count: u32,
    ) -> bool;
    fn class_getInstanceVariable(cls: Class, name: *const c_char) -> Ivar;
    fn class_getInstanceMethod(cls: Class, name: Sel) -> Method;
    /// Allocate and **zero** an instance of `cls`.
    fn class_createInstance(cls: Class, extra: usize) -> Class;
    fn class_getClassMethod(cls: Class, name: Sel) -> Method;
    fn class_getMethodImplementation(cls: Class, name: Sel) -> Imp;

    /// The `IMP` and the type encoding of a method, which together are the whole of
    /// what a message send needs to know.
    fn method_getImplementation(method: Method) -> Imp;
    fn method_getTypeEncoding(method: Method) -> *const c_char;
    fn method_setImplementation(method: Method, imp: Imp) -> Imp;

    fn ivar_getOffset(ivar: Ivar) -> isize;

    fn sel_registerName(name: *const c_char) -> Sel;
    fn sel_getName(sel: Sel) -> *const c_char;

    fn objc_getProtocol(name: *const c_char) -> Protocol;
    fn objc_allocateProtocol(name: *const c_char) -> Protocol;
    fn objc_registerProtocol(protocol: Protocol);
    fn protocol_addProtocol(protocol: Protocol, addition: Protocol);
    fn protocol_addMethodDescription(
        protocol: Protocol,
        name: Sel,
        types: *const c_char,
        is_required: bool,
        is_instance: bool,
    );

    /// Pushing and popping an autorelease pool. Through the selector like
    /// everything else: under `builtin` the host's pool is a different pool.
    fn objc_autoreleasePoolPush() -> *mut std::ffi::c_void;
    fn objc_autoreleasePoolPop(token: *mut std::ffi::c_void);
}

/// `objc_msgSend`'s address, to be cast to the prototype the method has.
///
/// Never declared as a function: it takes its arguments in whatever registers the
/// call site used, which no Rust prototype can state.
pub fn msg_send() -> *const std::ffi::c_void {
    match symbol("objc_msgSend") {
        Some(addr) => addr as usize as *const std::ffi::c_void,
        None => missing("objc_msgSend"),
    }
}

/// A `&str` as a NUL-terminated C string, for a call that borrows it.
pub fn c_string(s: &str) -> Option<std::ffi::CString> {
    std::ffi::CString::new(s).ok()
}
