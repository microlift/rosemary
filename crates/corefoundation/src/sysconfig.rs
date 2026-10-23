// The C spellings are the interface, as they are in `api`.
#![allow(non_snake_case)]

use std::ffi::c_void;

use objc::class::Class;

use crate::base::CFTypeRef;
use crate::callbacks::Policy;

/// The host's SystemConfiguration entry point of that name, or `None`.
///
/// The framework is not linked by this tree, so it is opened on demand. A host
/// that has no SystemConfiguration - anything but macOS - never finds one, which
/// is what makes every caller below fall through to the declining answer.
#[cfg(target_os = "macos")]
fn host(name: &std::ffi::CStr) -> Option<*mut c_void> {
    use std::sync::OnceLock;
    static LOADED: OnceLock<bool> = OnceLock::new();
    if !crate::foreign::forwarding() {
        return None;
    }
    // Opened once and never closed: the entry points below are called for the
    // life of the process. `RTLD_DEFAULT` alone is not enough - nothing in this
    // tree links the framework, so it may not be in the process yet.
    LOADED.get_or_init(|| {
        // SAFETY: a NUL-terminated path, and a handle deliberately leaked.
        !unsafe {
            libc::dlopen(
                c"/System/Library/Frameworks/SystemConfiguration.framework/SystemConfiguration"
                    .as_ptr(),
                libc::RTLD_LAZY,
            )
        }
        .is_null()
    });
    crate::foreign::entry(name)
}

/// `SCNetworkReachabilityFlags`, of which only the two below are answered.
///
/// `kSCNetworkReachabilityFlagsReachable` is the bit every caller tests;
/// `IsLocalAddress` distinguishes a target on this machine, which is reachable
/// whatever the routing table says.
mod flag {
    pub const REACHABLE: u32 = 1 << 1;
    pub const IS_LOCAL_ADDRESS: u32 = 1 << 16;
}

/// `SCNetworkReachabilityContext`, as `<SCNetworkReachability.h>` lays it out.
///
/// The `info` a caller registers is *its* object, and the retain and release it
/// passes alongside are how it stays alive for as long as this holds it. A
/// runtime that kept the pointer and skipped the retain hands the callback a
/// freed object, which is the crossing this tree spends its time chasing.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Context {
    version: i64,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: *const c_void,
}

type Callback = unsafe extern "C" fn(CFTypeRef, u32, *mut c_void);

/// An `SCNetworkReachabilityRef` of this runtime's.
///
/// A real object rather than a handle, because the caller `CFRelease`s it and
/// that walks the isa.
#[cfg(not(target_os = "macos"))]
#[repr(C)]
struct Reachability {
    isa: *const Class,
    /// Boxed and leaked with the object: `crate::base::allocate` gives fixed
    /// storage, and the state has a lock and a callback in it.
    state: *mut std::sync::Mutex<TargetState>,
}

#[cfg(not(target_os = "macos"))]
#[derive(Default)]
struct TargetState {
    callback: Option<Callback>,
    info: *mut c_void,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    /// Whether the target names something on this machine, which is reachable
    /// with or without a route to the outside world.
    local: bool,
}

#[cfg(not(target_os = "macos"))]
fn target_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let class = crate::base::register(
            c"SCNetworkReachability",
            None,
            size_of::<Reachability>(),
            true,
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a pointer to a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// The state of a target of *this runtime's*, or `None` for anything else.
///
/// Type-checked rather than assumed, for the reason `foreign::is` exists on the
/// other host: reading another class's second word as a `Mutex` pointer is a
/// dereference of whatever that field happens to hold.
///
/// # Safety
///
/// `target` must be null or an object this runtime allocated.
#[cfg(not(target_os = "macos"))]
unsafe fn target_state(target: CFTypeRef) -> Option<&'static std::sync::Mutex<TargetState>> {
    if !unsafe { crate::base::is_kind_of(target, target_class()) } {
        return None;
    }
    // SAFETY: an instance of the class above, whose `state` this module set.
    unsafe { (*(target as *const Reachability)).state.as_ref() }
}

/// A target for something at `local`, as a new object the caller owns.
#[cfg(not(target_os = "macos"))]
fn new_target(local: bool) -> CFTypeRef {
    // SAFETY: `Reachability` begins with `isa` and is the layout `target_class`
    // was registered with.
    let object: *mut Reachability = unsafe { crate::base::allocate(target_class()) };
    if object.is_null() {
        return std::ptr::null();
    }
    let state = Box::into_raw(Box::new(std::sync::Mutex::new(TargetState {
        local,
        ..TargetState::default()
    })));
    // SAFETY: a fresh allocation of this module's own layout.
    unsafe { (*object).state = state };
    object as CFTypeRef
}

/// Whether a packet leaving this machine has anywhere to go.
#[cfg(not(target_os = "macos"))]
fn reachable_flags(local: bool) -> u32 {
    match local {
        true => flag::REACHABLE | flag::IS_LOCAL_ADDRESS,
        false => match routed() {
            true => flag::REACHABLE,
            // Zero is what Apple's reports for a host with no route, and it is
            // the answer a client turns into "offline" rather than into a wait.
            false => 0,
        },
    }
}

/// Whether a Darwin `struct sockaddr` names *this machine*, which is reachable
/// with or without a route to anywhere else.
///
/// **Darwin's layout, not this host's**, and that is the whole of why this is
/// read by hand: Darwin is `{ uint8_t sa_len; uint8_t sa_family; ... }` where
/// Linux is a `uint16_t sa_family` at offset zero, so a `sockaddr` the guest
/// filled in has its family read out of the *length* byte by anything that
/// takes it for the host's. `AF_INET` is 2 on both; `AF_INET6` is 30 there and
/// 10 here.
///
/// # Safety
///
/// `address` must be null or a readable Darwin `struct sockaddr`.
#[cfg(not(target_os = "macos"))]
unsafe fn names_this_machine(address: *const c_void) -> bool {
    const DARWIN_AF_INET: u8 = 2;
    const DARWIN_AF_INET6: u8 = 30;
    if address.is_null() {
        return false;
    }
    // SAFETY: the caller's obligation; both reads are within the two-byte
    // header every `sockaddr` has whatever its family.
    let family = unsafe { address.cast::<u8>().add(1).read() };
    match family {
        DARWIN_AF_INET => {
            // `sockaddr_in`: two bytes of header, two of port, then the address
            // in network order - the same offsets on both platforms.
            // SAFETY: an `AF_INET` address is sixteen bytes.
            unsafe { address.cast::<u8>().add(4).read() == 127 }
        }
        DARWIN_AF_INET6 => {
            // `sockaddr_in6`: header, port, flow label, then sixteen bytes.
            // SAFETY: an `AF_INET6` address is twenty-eight bytes.
            let octets = unsafe { address.cast::<u8>().add(8).cast::<[u8; 16]>().read() };
            let mut loopback = [0u8; 16];
            loopback[15] = 1;
            octets == loopback
        }
        // A family this does not know is not claimed to be local, so it is
        // answered from the routing table like anything else.
        _ => false,
    }
}

/// The five reachability entry points, forwarded to Apple's framework.
///
/// A Mac has the configuration daemon and its opinion is the true one, so
/// nothing here is answered locally. The asynchronous half stays declined: a
/// foreign target scheduled on this tree's run loop would be read as a
/// structure it is not.
#[cfg(target_os = "macos")]
mod darwin {
    use super::*;

    /// `SCNetworkReachabilityRef SCNetworkReachabilityCreateWithAddress(CFAllocatorRef, const struct sockaddr *)`
    pub unsafe extern "C" fn SCNetworkReachabilityCreateWithAddress(
        allocator: CFTypeRef,
        address: *const c_void,
    ) -> CFTypeRef {
        let Some(f) = host(c"SCNetworkReachabilityCreateWithAddress") else {
            return std::ptr::null();
        };
        // SAFETY: the declaration above is the framework's own, and `address` is
        // the caller's `struct sockaddr` - a host layout on the one host this
        // reaches. The allocator is passed along; every caller in practice
        // passes the default, which is null in Apple's CoreFoundation as well as
        // in this one.
        unsafe {
            let f: unsafe extern "C" fn(CFTypeRef, *const c_void) -> CFTypeRef =
                std::mem::transmute(f);
            f(allocator, address)
        }
    }

    /// `SCNetworkReachabilityRef SCNetworkReachabilityCreateWithName(CFAllocatorRef, const char *)`
    pub unsafe extern "C" fn SCNetworkReachabilityCreateWithName(
        allocator: CFTypeRef,
        name: *const c_void,
    ) -> CFTypeRef {
        let Some(f) = host(c"SCNetworkReachabilityCreateWithName") else {
            return std::ptr::null();
        };
        // SAFETY: as above; `name` is the caller's NUL-terminated host name.
        unsafe {
            let f: unsafe extern "C" fn(CFTypeRef, *const c_void) -> CFTypeRef =
                std::mem::transmute(f);
            f(allocator, name)
        }
    }

    /// `Boolean SCNetworkReachabilityGetFlags(SCNetworkReachabilityRef, SCNetworkReachabilityFlags *)`
    ///
    /// False for a target this tree did not get from the host, and the flags are
    /// left alone: a caller that reads them after a false return is reading its
    /// own uninitialised memory on Apple's too.
    pub unsafe extern "C" fn SCNetworkReachabilityGetFlags(
        target: CFTypeRef,
        flags: *mut u32,
    ) -> u8 {
        if target.is_null() {
            return 0;
        }
        // Checked rather than assumed: reading one of this runtime's objects as
        // Apple's structure is the failure `crate::foreign` exists to stop.
        // SAFETY: null or an object.
        if !unsafe { crate::foreign::is(target) } {
            return 0;
        }
        let Some(f) = host(c"SCNetworkReachabilityGetFlags") else {
            return 0;
        };
        // SAFETY: a target Apple's framework made, handed back to it, and a
        // `u32` out-parameter the caller owns.
        unsafe {
            let f: unsafe extern "C" fn(CFTypeRef, *mut u32) -> u8 = std::mem::transmute(f);
            f(target, flags)
        }
    }

    /// `Boolean SCNetworkReachabilitySetCallback(SCNetworkReachabilityRef, SCNetworkReachabilityCallBack, SCNetworkReachabilityContext *)`
    ///
    /// Declined even where the target is the host's: a callback only ever
    /// arrives through the run loop the target is scheduled on, and scheduling
    /// is what cannot be forwarded.
    pub unsafe extern "C" fn SCNetworkReachabilitySetCallback(
        _target: CFTypeRef,
        _callback: *const c_void,
        _context: *mut c_void,
    ) -> u8 {
        0
    }

    /// `Boolean SCNetworkReachabilityScheduleWithRunLoop(SCNetworkReachabilityRef, CFRunLoopRef, CFStringRef)`
    pub unsafe extern "C" fn SCNetworkReachabilityScheduleWithRunLoop(
        _target: CFTypeRef,
        _loop: CFTypeRef,
        _mode: CFTypeRef,
    ) -> u8 {
        0
    }

    /// `Boolean SCNetworkReachabilityUnscheduleFromRunLoop(SCNetworkReachabilityRef, CFRunLoopRef, CFStringRef)`
    pub unsafe extern "C" fn SCNetworkReachabilityUnscheduleFromRunLoop(
        _target: CFTypeRef,
        _loop: CFTypeRef,
        _mode: CFTypeRef,
    ) -> u8 {
        0
    }
}

/// The same five, answered here, because there is no daemon to ask. See the
/// module note for why NULL was the wrong answer.
#[cfg(not(target_os = "macos"))]
mod local {
    use super::*;

    /// `SCNetworkReachabilityRef SCNetworkReachabilityCreateWithAddress(CFAllocatorRef, const struct sockaddr *)`
    pub unsafe extern "C" fn SCNetworkReachabilityCreateWithAddress(
        _allocator: CFTypeRef,
        address: *const c_void,
    ) -> CFTypeRef {
        // Apple's answers NULL for a null address, and callers check for it.
        if address.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the caller's `struct sockaddr`, in *Darwin's* layout.
        new_target(unsafe { names_this_machine(address) })
    }

    /// `SCNetworkReachabilityRef SCNetworkReachabilityCreateWithName(CFAllocatorRef, const char *)`
    ///
    /// The name is not resolved. Reachability is a question about the *network*,
    /// not about DNS - Apple's answers for a name that does not resolve - and
    /// resolving here would block the caller inside what it expects to be a
    /// cheap constructor.
    pub unsafe extern "C" fn SCNetworkReachabilityCreateWithName(
        _allocator: CFTypeRef,
        name: *const c_void,
    ) -> CFTypeRef {
        if name.is_null() {
            return std::ptr::null();
        }
        // SAFETY: the caller's NUL-terminated host name.
        let local = unsafe { std::ffi::CStr::from_ptr(name.cast()) }.to_bytes() == b"localhost";
        new_target(local)
    }

    /// `Boolean SCNetworkReachabilityGetFlags(SCNetworkReachabilityRef, SCNetworkReachabilityFlags *)`
    pub unsafe extern "C" fn SCNetworkReachabilityGetFlags(
        target: CFTypeRef,
        flags: *mut u32,
    ) -> u8 {
        // SAFETY: null or an object.
        let Some(state) = (unsafe { target_state(target) }) else {
            return 0;
        };
        if flags.is_null() {
            return 0;
        }
        let local = state.lock().unwrap_or_else(|e| e.into_inner()).local;
        // SAFETY: the caller's out-parameter.
        unsafe { flags.write(reachable_flags(local)) };
        1
    }

    /// `Boolean SCNetworkReachabilitySetCallback(SCNetworkReachabilityRef, SCNetworkReachabilityCallBack, SCNetworkReachabilityContext *)`
    ///
    /// The context's `retain` is called on the way in and the previous `info`'s
    /// `release` on the way out, because the caller's object has to outlive the
    /// registration and nothing else here keeps it alive.
    ///
    /// A null callback clears the registration, which is how a caller detaches.
    pub unsafe extern "C" fn SCNetworkReachabilitySetCallback(
        target: CFTypeRef,
        callback: *const c_void,
        context: *mut c_void,
    ) -> u8 {
        let Some(state) = (unsafe { target_state(target) }) else {
            return 0;
        };
        // SAFETY: null, or the caller's `SCNetworkReachabilityContext`.
        let context = match context.is_null() {
            true => None,
            false => Some(unsafe { context.cast::<Context>().read() }),
        };
        // Apple refuses a context whose version it does not know rather than
        // reading fields that may not be there.
        if context.is_some_and(|c| c.version != 0) {
            return 0;
        }
        let info = context.map_or(std::ptr::null_mut(), |c| match c.retain {
            // SAFETY: the caller's own retain, on the caller's own object.
            Some(retain) if !c.info.is_null() => unsafe { retain(c.info) }.cast_mut(),
            _ => c.info,
        });

        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        if let (Some(release), false) = (state.release, state.info.is_null()) {
            // SAFETY: the release registered with the info it was given.
            unsafe { release(state.info) };
        }
        // SAFETY: the caller's function pointer, of the declared shape.
        state.callback = (!callback.is_null())
            .then(|| unsafe { std::mem::transmute::<*const c_void, Callback>(callback) });
        state.info = info;
        state.release = context.and_then(|c| c.release);
        1
    }

    /// `Boolean SCNetworkReachabilityScheduleWithRunLoop(SCNetworkReachabilityRef, CFRunLoopRef, CFStringRef)`
    ///
    /// Delivers one callback with the current flags, which is what Apple's does
    /// when a target is first scheduled and is the whole reason a client
    /// schedules rather than polls.
    ///
    /// Two departures, both written down because a caller cannot see them: the
    /// callback runs on **this** thread rather than on the run loop's, since a
    /// foreign target cannot be handed to that loop; and it is the *only* one
    /// that ever arrives, because nothing here watches the routing table for a
    /// change. A client that comes up online stays told it is online.
    pub unsafe extern "C" fn SCNetworkReachabilityScheduleWithRunLoop(
        target: CFTypeRef,
        _loop: CFTypeRef,
        _mode: CFTypeRef,
    ) -> u8 {
        let Some(state) = (unsafe { target_state(target) }) else {
            return 0;
        };
        // The lock is dropped before the callback: it runs the caller's code,
        // which may ask this target its flags and would deadlock on a lock this
        // thread still held.
        let delivery = {
            let state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.callback.map(|f| (f, state.info, state.local))
        };
        if let Some((callback, info, local)) = delivery {
            // SAFETY: the callback and info the caller registered, with the
            // target it registered them against.
            unsafe { callback(target, reachable_flags(local), info) };
        }
        1
    }

    /// `Boolean SCNetworkReachabilityUnscheduleFromRunLoop(SCNetworkReachabilityRef, CFRunLoopRef, CFStringRef)`
    pub unsafe extern "C" fn SCNetworkReachabilityUnscheduleFromRunLoop(
        target: CFTypeRef,
        _loop: CFTypeRef,
        _mode: CFTypeRef,
    ) -> u8 {
        u8::from(unsafe { target_state(target) }.is_some())
    }
}

#[cfg(target_os = "macos")]
use darwin::*;
#[cfg(not(target_os = "macos"))]
use local::*;

/// `CFDictionaryRef SCDynamicStoreCopyProxies(SCDynamicStoreRef)`
///
/// NULL, which is what Apple's returns when there is no proxy configuration to
/// report - and there is none here.
pub unsafe extern "C" fn SCDynamicStoreCopyProxies(_store: CFTypeRef) -> CFTypeRef {
    std::ptr::null()
}

/// `kSCStatus*`, from `<SystemConfiguration/SystemConfiguration.h>`, each
/// with the text configd's `SCErrorString` gives it.
mod status {
    pub const OK: i32 = 0;
    pub const FAILED: i32 = 1001;
    pub const INVALID_ARGUMENT: i32 = 1002;
    pub const NO_KEY: i32 = 1004;
    pub const NO_STORE_SESSION: i32 = 2001;
    pub const NOTIFIER_ACTIVE: i32 = 2003;

    pub const MESSAGES: &[(i32, &std::ffi::CStr)] = &[
        (1003, c"Permission denied"),
        (
            5002,
            c"Network connection information not available at this time",
        ),
        (5001, c"Network service for connection not available"),
        (FAILED, c"Failed!"),
        (INVALID_ARGUMENT, c"Invalid argument"),
        (1005, c"Key already defined"),
        (1006, c"Lock already held"),
        (3006, c"Maximum link count exceeded"),
        (1007, c"Lock required for this operation"),
        (2002, c"Configuration daemon not (no longer) available"),
        (NO_STORE_SESSION, c"Configuration daemon session not active"),
        (3003, c"Configuration file not found"),
        (NO_KEY, c"No such key"),
        (3004, c"No such link"),
        (3001, c"Preference session not active"),
        (NOTIFIER_ACTIVE, c"Notifier is currently active"),
        (OK, c"Success!"),
        (3002, c"Preferences update currently in progress"),
        (4001, c"Network reachability cannot be determined"),
        (3005, c"Write attempted on stale version of object"),
    ];
}

thread_local! {
    /// The status `SCError` answers: the last failure on this thread, as
    /// configd keeps it in thread-specific data.
    static LAST_ERROR: std::cell::Cell<i32> = const { std::cell::Cell::new(status::OK) };
}

/// configd's `_SCErrorSet`.
fn set_error(code: i32) {
    LAST_ERROR.with(|e| e.set(code));
}

/// `int SCError(void)`.
pub unsafe extern "C" fn SCError() -> i32 {
    LAST_ERROR.with(|e| e.get())
}

/// `const char *SCErrorString(int status)`: configd's message for its own
/// codes, `strerror` for an errno, and otherwise the Mach error text, which
/// for a code that is none of these is `strerror`'s "Unknown error".
pub unsafe extern "C" fn SCErrorString(code: i32) -> *const std::ffi::c_char {
    if let Some((_, message)) = status::MESSAGES.iter().find(|(c, _)| *c == code) {
        return message.as_ptr();
    }
    // `ELAST` on Darwin; a Darwin errno is the guest's, and the host's
    // `strerror` has the same text for the POSIX ones.
    const ELAST: i32 = 106;
    if (1..=ELAST).contains(&code) {
        let host = darwin::errno::to_host(code);
        // SAFETY: `strerror` answers a string that lives until the next call.
        return unsafe { libc::strerror(host) };
    }
    // FIXME (ACCURACY): `bootstrap_strerror` and `mach_error_string` are not
    // consulted for the codes they own; their unknown-code answer is this.
    // SAFETY: as above.
    unsafe { libc::strerror(code) }
}

/// `SCDynamicStoreCallBack`: `(store, changedKeys, info)`.
type StoreCallBack = unsafe extern "C" fn(CFTypeRef, CFTypeRef, *mut c_void);

/// An `SCDynamicStoreRef`.
///
/// The store below answers from the host's own interfaces rather than from a
/// configd session, so nothing changes in it and nothing is ever notified - but
/// a caller registers for notifications all the same, and gets back what
/// Apple's gives it: its keys kept and a run loop source it can schedule.
/// FIXME: the host's network changes (netlink, on Linux) are not watched, so
/// the source is never signalled and the callout never runs.
#[repr(C)]
struct Store {
    isa: *const Class,
    /// Boxed and freed with the object: `crate::base::allocate` gives fixed
    /// storage, and the state has a lock in it.
    state: *mut std::sync::Mutex<StoreState>,
}

#[derive(Default)]
struct StoreState {
    /// Kept for the change delivery the FIXME on [`Store`] names.
    #[allow(dead_code)]
    callout: Option<StoreCallBack>,
    /// The context's `info`, retained by its own `retain` when it has one.
    info: usize,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    /// `SCDynamicStoreSetNotificationKeys`' keys and patterns, as text.
    keys: Option<Vec<Vec<u8>>>,
    patterns: Option<Vec<Vec<u8>>>,
    /// The one run loop source `SCDynamicStoreCreateRunLoopSource` hands out,
    /// whose reference the store holds.
    source: usize,
}

// SAFETY: `info` is the caller's opaque pointer, handed back and never read;
// the callbacks are code addresses.
unsafe impl Send for StoreState {}

fn store_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let class = crate::base::register(c"SCDynamicStore", None, size_of::<Store>(), true);
        class.replace_method(
            objc::sel::get("dealloc"),
            store_dealloc as *const c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a pointer to a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// configd's `__SCDynamicStoreDeallocate`: the run loop source invalidated
/// and released, and the context's `info` released.
unsafe extern "C" fn store_dealloc(receiver: objc::object::Id, _sel: objc::sel::Sel) {
    // SAFETY: an object of `store_class`, whose state `SCDynamicStoreCreate`
    // boxed, taken back once here.
    let state = unsafe {
        let store = receiver as *mut Store;
        let state = std::mem::replace(&mut (*store).state, std::ptr::null_mut());
        (!state.is_null()).then(|| {
            Box::from_raw(state)
                .into_inner()
                .unwrap_or_else(|e| e.into_inner())
        })
    };
    if let Some(state) = state {
        if state.source != 0 {
            // SAFETY: the source this store made and holds a reference to.
            unsafe {
                crate::api::CFRunLoopSourceInvalidate(state.source as CFTypeRef);
                crate::api::CFRelease(state.source as CFTypeRef);
            }
        }
        if let Some(release) = state.release {
            // SAFETY: the caller's own release for its own `info`.
            unsafe { release(state.info as *const c_void) };
        }
    }
    objc::arc::finalize(receiver);
}

/// The state of a store of this runtime's, or `None` for anything else -
/// configd's `isA_SCDynamicStore`.
///
/// # Safety
///
/// `store` is null or a live object.
unsafe fn store_state<'a>(store: CFTypeRef) -> Option<&'a std::sync::Mutex<StoreState>> {
    if store.is_null() {
        return None;
    }
    // SAFETY: a live object, whose class is read to check it.
    let class = unsafe { objc::object::class_of(store as objc::object::Id) }?;
    if !std::ptr::eq(class, store_class()) {
        return None;
    }
    // SAFETY: a store, whose state lives as long as it does.
    let state = unsafe { (*(store as *const Store)).state };
    // SAFETY: as above.
    (!state.is_null()).then(|| unsafe { &*state })
}

/// `SCDynamicStoreRef SCDynamicStoreCreate(CFAllocatorRef, CFStringRef, SCDynamicStoreCallBack, SCDynamicStoreContext *)`
///
/// The store is created rather than declined, because a caller that gets NULL
/// here asks nothing further - and the keys below can be answered. The
/// context is copied and its `info` retained, as configd's is.
pub unsafe extern "C" fn SCDynamicStoreCreate(
    _allocator: CFTypeRef,
    _name: CFTypeRef,
    callout: Option<StoreCallBack>,
    context: *const Context,
) -> CFTypeRef {
    let mut state = StoreState {
        callout,
        ..StoreState::default()
    };
    if !context.is_null() {
        // SAFETY: the caller's live context.
        let context = unsafe { *context };
        state.info = match context.retain {
            // SAFETY: the caller's own retain for its own `info`.
            Some(retain) => (unsafe { retain(context.info) }) as usize,
            None => context.info as usize,
        };
        state.release = context.release;
    }
    // SAFETY: `Store` begins with `isa` and is the layout `store_class` was
    // registered with.
    let store: *mut Store = unsafe { crate::base::allocate(store_class()) };
    // SAFETY: freshly allocated.
    unsafe { (*store).state = Box::into_raw(Box::new(std::sync::Mutex::new(state))) };
    store as CFTypeRef
}

/// The text of each string in an array, or `None` for an array holding
/// anything else - what configd's `_SCSerialize` would fail on.
///
/// # Safety
///
/// `array` is a live object.
unsafe fn texts(array: CFTypeRef) -> Option<Vec<Vec<u8>>> {
    // SAFETY: a live object; anything but an array is refused.
    let contents = unsafe { crate::collections::array(array) }?;
    contents
        .values
        .iter()
        // SAFETY: each element is a live object.
        .map(|&value| unsafe { text_of(value) })
        .collect()
}

/// `Boolean SCDynamicStoreSetNotificationKeys(SCDynamicStoreRef, CFArrayRef
/// keys, CFArrayRef patterns)`: the keys and patterns to be told about,
/// replacing the last ones; a null array clears its half.
pub unsafe extern "C" fn SCDynamicStoreSetNotificationKeys(
    store: CFTypeRef,
    keys: CFTypeRef,
    patterns: CFTypeRef,
) -> u8 {
    // SAFETY: the caller's object or null.
    let Some(state) = (unsafe { store_state(store) }) else {
        set_error(status::NO_STORE_SESSION);
        return 0;
    };
    let read = |array: CFTypeRef| match array.is_null() {
        true => Some(None),
        // SAFETY: a live object.
        false => unsafe { texts(array) }.map(Some),
    };
    let (Some(keys), Some(patterns)) = (read(keys), read(patterns)) else {
        set_error(status::FAILED);
        return 0;
    };
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    state.keys = keys;
    state.patterns = patterns;
    1
}

/// `CFRunLoopSourceRef SCDynamicStoreCreateRunLoopSource(CFAllocatorRef,
/// SCDynamicStoreRef, CFIndex order)`: the store's one source, made the first
/// time and the same one after, with a reference for the caller each time.
pub unsafe extern "C" fn SCDynamicStoreCreateRunLoopSource(
    _allocator: CFTypeRef,
    store: CFTypeRef,
    _order: isize,
) -> CFTypeRef {
    // SAFETY: the caller's object or null.
    let Some(state) = (unsafe { store_state(store) }) else {
        set_error(status::NO_STORE_SESSION);
        return std::ptr::null();
    };
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    if state.source == 0 {
        // Never signalled: see `Store`.
        let context = crate::api::CFRunLoopSourceContext {
            version: 0,
            info: store as *mut c_void,
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
            equal: std::ptr::null(),
            hash: std::ptr::null(),
            schedule: std::ptr::null(),
            cancel: std::ptr::null(),
            perform: None,
        };
        // SAFETY: a version-0 context that lives for the call.
        state.source =
            unsafe { crate::api::CFRunLoopSourceCreate(std::ptr::null(), 0, &context) } as usize;
        if state.source == 0 {
            set_error(status::FAILED);
            return std::ptr::null();
        }
    }
    // SAFETY: the source made above, which the store holds.
    unsafe { crate::api::CFRetain(state.source as CFTypeRef) }
}

/// `CFStringRef SCDynamicStoreKeyCreateProxies(CFAllocatorRef)`: configd's
/// `State:/Network/Global/Proxies`.
pub unsafe extern "C" fn SCDynamicStoreKeyCreateProxies(allocator: CFTypeRef) -> CFTypeRef {
    let domain = crate::base::interned_name("State:") as CFTypeRef;
    let entity = crate::base::interned_name("Proxies") as CFTypeRef;
    // SAFETY: two strings.
    unsafe { SCDynamicStoreKeyCreateNetworkGlobalEntity(allocator, domain, entity) }
}

/// `CFStringRef SCDynamicStoreCopyComputerName(SCDynamicStoreRef, CFStringEncoding *)`
///
/// The host's name without its domain, which is the closest thing it has to
/// the Sharing pane's computer name. `encoding`, when asked for, is UTF-8.
pub unsafe extern "C" fn SCDynamicStoreCopyComputerName(
    _store: CFTypeRef,
    encoding: *mut u32,
) -> CFTypeRef {
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;
    let mut buffer = [0u8; 256];
    // SAFETY: a buffer described by its own length, one byte kept for the NUL.
    if unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() - 1) } != 0 {
        return std::ptr::null();
    }
    let end = buffer
        .iter()
        .position(|b| *b == 0 || *b == b'.')
        .unwrap_or(0);
    if end == 0 {
        return std::ptr::null();
    }
    if !encoding.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { encoding.write(UTF8) };
    }
    crate::string::from_bytes(&buffer[..end], false)
}

/// `CFStringRef SCDynamicStoreKeyCreateNetworkGlobalEntity(CFAllocatorRef, CFStringRef domain, CFStringRef entity)`
///
/// The key is a path, and its spelling is the interface: `State:` and `IPv4`
/// make `State:/Network/Global/IPv4`, which is what [`SCDynamicStoreCopyValue`]
/// matches on.
pub unsafe extern "C" fn SCDynamicStoreKeyCreateNetworkGlobalEntity(
    _allocator: CFTypeRef,
    domain: CFTypeRef,
    entity: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: null or an object of this runtime's.
    let (Some(domain), Some(entity)) = (unsafe { text_of(domain) }, unsafe { text_of(entity) })
    else {
        return std::ptr::null();
    };
    let mut key = domain;
    key.extend_from_slice(b"/Network/Global/");
    key.extend_from_slice(&entity);
    crate::string::from_bytes(&key, false)
}

/// `CFStringRef SCDynamicStoreKeyCreateProxies(CFAllocatorRef)`
pub unsafe extern "C" fn SCDynamicStoreKeyCreateProxies(_allocator: CFTypeRef) -> CFTypeRef {
    crate::string::from_bytes(b"State:/Network/Global/Proxies", false)
}

/// `Boolean SCDynamicStoreSetNotificationKeys(SCDynamicStoreRef, CFArrayRef, CFArrayRef)`
///
/// Accepted and never delivered on: the values above do not change while the
/// program runs, so there is nothing to notify.
pub unsafe extern "C" fn SCDynamicStoreSetNotificationKeys(
    store: CFTypeRef,
    _keys: CFTypeRef,
    _patterns: CFTypeRef,
) -> u8 {
    u8::from(!store.is_null())
}

/// `CFRunLoopSourceRef SCDynamicStoreCreateRunLoopSource(CFAllocatorRef, SCDynamicStoreRef, CFIndex)`
///
/// A source that never fires, for the reason [`SCDynamicStoreSetNotificationKeys`]
/// gives. A caller that gets NULL instead hands it straight to
/// `CFRunLoopAddSource`.
pub unsafe extern "C" fn SCDynamicStoreCreateRunLoopSource(
    allocator: CFTypeRef,
    store: CFTypeRef,
    order: isize,
) -> CFTypeRef {
    if store.is_null() {
        return std::ptr::null();
    }
    let context = crate::api::CFRunLoopSourceContext {
        version: 0,
        info: std::ptr::null_mut(),
        retain: std::ptr::null(),
        release: std::ptr::null(),
        copy_description: std::ptr::null(),
        equal: std::ptr::null(),
        hash: std::ptr::null(),
        schedule: std::ptr::null(),
        cancel: std::ptr::null(),
        perform: None,
    };
    // SAFETY: a version-0 context that lives across the call.
    unsafe { crate::api::CFRunLoopSourceCreate(allocator.cast(), order, &context) }
}

/// `CFPropertyListRef SCDynamicStoreCopyValue(SCDynamicStoreRef, CFStringRef key)`
///
/// One key is answered: the global IPv4 entity, whose `PrimaryInterface` is what
/// a program reads to find out how it is on the network. Every other key is NULL,
/// which is what Apple's returns for a key the store does not hold.
pub unsafe extern "C" fn SCDynamicStoreCopyValue(_store: CFTypeRef, key: CFTypeRef) -> CFTypeRef {
    let Some(key) = (unsafe { text_of(key) }) else {
        return std::ptr::null();
    };
    if key != b"State:/Network/Global/IPv4" {
        return std::ptr::null();
    }
    let Some(interface) = primary_interface() else {
        return std::ptr::null();
    };
    let name = crate::string::from_bytes(interface.as_bytes(), false);
    let value = crate::collections::new_dictionary(
        &[(
            crate::base::interned_name(PRIMARY_INTERFACE) as CFTypeRef,
            name,
        )],
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    );
    // The dictionary retained it; this call's own reference is spent.
    // SAFETY: a string this call just made.
    unsafe { crate::api::CFRelease(name) };
    value
}

/// `SCPreferencesRef SCPreferencesCreate(CFAllocatorRef, CFStringRef, CFStringRef)`
///
/// NULL: the preferences file is the *system's* network configuration, and
/// nothing here has one to read. Apple's returns NULL for a caller that may not
/// open it, so a program already handles the answer.
pub unsafe extern "C" fn SCPreferencesCreate(
    _allocator: CFTypeRef,
    _name: CFTypeRef,
    _prefs_id: CFTypeRef,
) -> CFTypeRef {
    std::ptr::null()
}

/// `CFPropertyListRef SCPreferencesGetValue(SCPreferencesRef, CFStringRef)`
pub unsafe extern "C" fn SCPreferencesGetValue(_prefs: CFTypeRef, _key: CFTypeRef) -> CFTypeRef {
    std::ptr::null()
}

/// `SCNetworkServiceRef SCNetworkServiceCopy(SCPreferencesRef, CFStringRef)`
///
/// NULL, and it cannot be otherwise: the only way to a service is through the
/// preferences [`SCPreferencesCreate`] declines to open.
pub unsafe extern "C" fn SCNetworkServiceCopy(
    _prefs: CFTypeRef,
    _service_id: CFTypeRef,
) -> CFTypeRef {
    std::ptr::null()
}

/// `SCNetworkInterfaceRef SCNetworkServiceGetInterface(SCNetworkServiceRef)`
pub unsafe extern "C" fn SCNetworkServiceGetInterface(_service: CFTypeRef) -> CFTypeRef {
    std::ptr::null()
}

/// `CFStringRef SCNetworkInterfaceGetInterfaceType(SCNetworkInterfaceRef)`
pub unsafe extern "C" fn SCNetworkInterfaceGetInterfaceType(_interface: CFTypeRef) -> CFTypeRef {
    std::ptr::null()
}

/// The characters of a string of this runtime's, or `None` for anything else.
///
/// # Safety
///
/// `string` must be null or an object this runtime allocated.
unsafe fn text_of(string: CFTypeRef) -> Option<Vec<u8>> {
    unsafe { crate::string::as_string(string) }.map(|s| s.utf8())
}

/// The interface packets to the outside world leave by, as the routing table
/// says. `None` when nothing is routable, which is the honest answer for a
/// machine that is offline.
fn primary_interface() -> Option<String> {
    let local = starnix::net::route_v4()?;
    starnix::net::interfaces()
        .into_iter()
        .find(|interface| interface.v4.contains(&local))
        .map(|interface| interface.name)
}

/// Whether the routing table has a way out to the public internet, by either
/// family. `network`'s path monitor asks the same question.
#[cfg(not(target_os = "macos"))]
fn routed() -> bool {
    starnix::net::route_v4().is_some() || starnix::net::route_v6().is_some()
}

/// `kSCDynamicStorePropNetPrimaryInterface`, which is also the dictionary key
/// the answer above is read out of.
const PRIMARY_INTERFACE: &str = "PrimaryInterface";

/// The `kSC*` globals, each of which is a `CFStringRef` **variable** holding the
/// path component it names.
///
/// Published even where nothing here answers a call that uses one - see
/// `corefoundation::data_symbols`: an unresolved data import binds to zero and
/// the guest dereferences it.
const KEY_STRINGS: &[(&str, &str)] = &[
    ("_kSCDynamicStoreDomainState", "State:"),
    ("_kSCDynamicStoreDomainSetup", "Setup:"),
    ("_kSCDynamicStorePropNetPrimaryInterface", PRIMARY_INTERFACE),
    ("_kSCDynamicStorePropNetPrimaryService", "PrimaryService"),
    ("_kSCEntNetIPv4", "IPv4"),
    ("_kSCEntNetIPv6", "IPv6"),
    ("_kSCEntNetInterface", "Interface"),
    ("_kSCEntNetLink", "Link"),
    ("_kSCPrefNetworkServices", "NetworkServices"),
    ("_kSCPropNetInterfaceDeviceName", "DeviceName"),
    ("_kSCPropNetInterfaceType", "Type"),
    ("_kSCNetworkInterfaceTypeEthernet", "Ethernet"),
    ("_kSCNetworkInterfaceTypeIEEE80211", "IEEE80211"),
    // The keys of the proxies entity, which a browser reads at startup.
    ("_kSCEntNetProxies", "Proxies"),
    ("_kSCPropNetProxiesExceptionsList", "ExceptionsList"),
    ("_kSCPropNetProxiesFTPEnable", "FTPEnable"),
    ("_kSCPropNetProxiesFTPPort", "FTPPort"),
    ("_kSCPropNetProxiesFTPProxy", "FTPProxy"),
    ("_kSCPropNetProxiesHTTPEnable", "HTTPEnable"),
    ("_kSCPropNetProxiesHTTPPort", "HTTPPort"),
    ("_kSCPropNetProxiesHTTPProxy", "HTTPProxy"),
    ("_kSCPropNetProxiesHTTPSEnable", "HTTPSEnable"),
    ("_kSCPropNetProxiesHTTPSPort", "HTTPSPort"),
    ("_kSCPropNetProxiesHTTPSProxy", "HTTPSProxy"),
    (
        "_kSCPropNetProxiesProxyAutoConfigEnable",
        "ProxyAutoConfigEnable",
    ),
    (
        "_kSCPropNetProxiesProxyAutoConfigURLString",
        "ProxyAutoConfigURLString",
    ),
    ("_kSCPropNetProxiesSOCKSEnable", "SOCKSEnable"),
    ("_kSCPropNetProxiesSOCKSPort", "SOCKSPort"),
    ("_kSCPropNetProxiesSOCKSProxy", "SOCKSProxy"),
];

/// The `kSC*` globals, as `(Mach-O name, address)`.
pub fn data_symbols() -> Vec<(String, u64)> {
    crate::bootstrap();
    // The schema's keys, less any this module already lists.
    let schema = crate::sysconfig_schema::SCHEMA
        .iter()
        .filter(|(symbol, _)| !KEY_STRINGS.iter().any(|(s, _)| s == symbol));
    KEY_STRINGS
        .iter()
        .chain(schema)
        .map(|(symbol, text)| {
            (
                (*symbol).to_string(),
                crate::base::global_cell(
                    symbol,
                    crate::base::interned_name(text) as CFTypeRef as usize as u64,
                ),
            )
        })
        .collect()
}

exports::exports! {
    functions {
        "SCNetworkReachabilityCreateWithAddress" => SCNetworkReachabilityCreateWithAddress,
        "SCNetworkReachabilityCreateWithName" => SCNetworkReachabilityCreateWithName,
        "SCNetworkReachabilityGetFlags" => SCNetworkReachabilityGetFlags,
        "SCNetworkReachabilitySetCallback" => SCNetworkReachabilitySetCallback,
        "SCNetworkReachabilityScheduleWithRunLoop" => SCNetworkReachabilityScheduleWithRunLoop,
        "SCNetworkReachabilityUnscheduleFromRunLoop" => SCNetworkReachabilityUnscheduleFromRunLoop,
        "SCDynamicStoreCopyProxies" => SCDynamicStoreCopyProxies,
        "SCDynamicStoreCreate" => SCDynamicStoreCreate,
        "SCDynamicStoreSetNotificationKeys" => SCDynamicStoreSetNotificationKeys,
        "SCDynamicStoreCreateRunLoopSource" => SCDynamicStoreCreateRunLoopSource,
        "SCDynamicStoreKeyCreateProxies" => SCDynamicStoreKeyCreateProxies,
        "SCError" => SCError,
        "SCErrorString" => SCErrorString,
        "SCDynamicStoreCopyComputerName" => SCDynamicStoreCopyComputerName,
        "SCDynamicStoreKeyCreateNetworkGlobalEntity" => SCDynamicStoreKeyCreateNetworkGlobalEntity,
        "SCDynamicStoreCopyValue" => SCDynamicStoreCopyValue,
        "SCDynamicStoreKeyCreateProxies" => SCDynamicStoreKeyCreateProxies,
        "SCDynamicStoreSetNotificationKeys" => SCDynamicStoreSetNotificationKeys,
        "SCDynamicStoreCreateRunLoopSource" => SCDynamicStoreCreateRunLoopSource,
        "SCPreferencesCreate" => SCPreferencesCreate,
        "SCPreferencesGetValue" => SCPreferencesGetValue,
        "SCNetworkServiceCopy" => SCNetworkServiceCopy,
        "SCNetworkServiceGetInterface" => SCNetworkServiceGetInterface,
        "SCNetworkInterfaceGetInterfaceType" => SCNetworkInterfaceGetInterfaceType,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A null argument is the one case that still answers NULL, on both hosts.
    #[test]
    fn a_null_argument_makes_no_target() {
        crate::bootstrap();
        // SAFETY: both take a null allocator and a null argument, which is what
        // "no target" means to them.
        unsafe {
            assert!(
                SCNetworkReachabilityCreateWithAddress(std::ptr::null(), std::ptr::null())
                    .is_null()
            );
            assert!(
                SCNetworkReachabilityCreateWithName(std::ptr::null(), std::ptr::null()).is_null()
            );
        }
    }

    /// A Darwin `sockaddr_in` for `address`, laid out as the *guest* writes one:
    /// `sa_len` then `sa_family`, which is the divergence this reads by hand.
    #[cfg(not(target_os = "macos"))]
    fn darwin_sockaddr_in(address: [u8; 4]) -> [u8; 16] {
        let mut bytes = [0u8; 16];
        bytes[0] = 16;
        bytes[1] = 2;
        bytes[4..8].copy_from_slice(&address);
        bytes
    }

    /// The whole point of the module off Darwin: a target is made, its flags are
    /// answered, and a scheduled callback *arrives*. A client that registers one
    /// and waits is the case NULL never served.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_scheduled_callback_arrives_for_a_local_target() {
        crate::bootstrap();
        static SEEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        unsafe extern "C" fn record(_target: CFTypeRef, flags: u32, info: *mut c_void) {
            SEEN.store(flags, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(info as usize, 0x1234, "the registered info is handed back");
        }

        let address = darwin_sockaddr_in([127, 0, 0, 1]);
        let mut context = Context {
            version: 0,
            info: 0x1234 as *mut c_void,
            retain: None,
            release: None,
            copy_description: std::ptr::null(),
        };
        // SAFETY: a live `sockaddr`, a live context, and a callback of the
        // declared shape.
        unsafe {
            let target = SCNetworkReachabilityCreateWithAddress(
                std::ptr::null(),
                std::ptr::from_ref(&address).cast(),
            );
            assert!(!target.is_null());

            let mut flags = 0u32;
            assert_ne!(SCNetworkReachabilityGetFlags(target, &mut flags), 0);
            assert_ne!(
                flags & flag::REACHABLE,
                0,
                "the loopback address is reachable whatever the routing table says"
            );

            assert_ne!(
                SCNetworkReachabilitySetCallback(
                    target,
                    record as *const c_void,
                    std::ptr::from_mut(&mut context).cast(),
                ),
                0
            );
            assert_ne!(
                SCNetworkReachabilityScheduleWithRunLoop(
                    target,
                    std::ptr::null(),
                    std::ptr::null()
                ),
                0
            );
            assert_eq!(
                SEEN.load(std::sync::atomic::Ordering::Relaxed) & flag::REACHABLE,
                flag::REACHABLE,
                "scheduling delivers the current flags"
            );
            assert_ne!(
                SCNetworkReachabilityUnscheduleFromRunLoop(
                    target,
                    std::ptr::null(),
                    std::ptr::null()
                ),
                0
            );
            crate::api::CFRelease(target);
        }
    }

    /// A context whose version this does not know is refused rather than read,
    /// and a target of another class is refused rather than cast.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_target_that_is_not_one_and_a_context_that_is_not_v0_are_both_refused() {
        crate::bootstrap();
        let not_a_target = crate::string::from_bytes(b"not a target", false);
        let mut flags = 0u32;
        // SAFETY: a live object of this runtime's, of the wrong class.
        unsafe {
            assert_eq!(SCNetworkReachabilityGetFlags(not_a_target, &mut flags), 0);
            assert_eq!(flags, 0, "the flags of a refused target are left alone");
            assert_eq!(
                SCNetworkReachabilityScheduleWithRunLoop(
                    not_a_target,
                    std::ptr::null(),
                    std::ptr::null()
                ),
                0
            );
            crate::api::CFRelease(not_a_target);
        }

        let address = darwin_sockaddr_in([1, 1, 1, 1]);
        let mut context = Context {
            version: 1,
            info: std::ptr::null_mut(),
            retain: None,
            release: None,
            copy_description: std::ptr::null(),
        };
        // SAFETY: a live `sockaddr` and a live context.
        unsafe {
            let target = SCNetworkReachabilityCreateWithAddress(
                std::ptr::null(),
                std::ptr::from_ref(&address).cast(),
            );
            assert_eq!(
                SCNetworkReachabilitySetCallback(
                    target,
                    std::ptr::null(),
                    std::ptr::from_mut(&mut context).cast(),
                ),
                0,
                "a context this does not understand is not read"
            );
            crate::api::CFRelease(target);
        }
    }

    /// The family is read from Darwin's offset, not this host's. A Linux reader
    /// takes the *length* byte for the low half of `sa_family` and sees 16, not
    /// `AF_INET` - so a loopback address would come back as not local.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_family_is_read_out_of_darwins_second_byte() {
        // SAFETY: both are sixteen readable bytes in Darwin's layout.
        unsafe {
            assert!(names_this_machine(
                std::ptr::from_ref(&darwin_sockaddr_in([127, 0, 0, 1])).cast()
            ));
            assert!(!names_this_machine(
                std::ptr::from_ref(&darwin_sockaddr_in([1, 1, 1, 1])).cast()
            ));
            assert!(!names_this_machine(
                std::ptr::from_ref(&darwin_sockaddr_in([0, 0, 0, 0])).cast()
            ));
        }
    }

    /// The property that keeps the forwarding safe: a target that is not the
    /// host's is never read as one. Nothing here makes an object of this
    /// runtime's a target, so a string stands in for whatever a guest might
    /// pass by mistake.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_target_this_runtime_made_is_refused_rather_than_forwarded() {
        crate::bootstrap();
        let not_a_target = crate::string::from_bytes(b"not a target", false);
        let mut flags = 0u32;
        // SAFETY: a live object of this runtime's, and a `u32` out-parameter.
        unsafe {
            assert_eq!(SCNetworkReachabilityGetFlags(not_a_target, &mut flags), 0);
            assert_eq!(flags, 0, "the flags of a refused target are left alone");
            crate::api::CFRelease(not_a_target);
        }
    }

    /// The host's own opinion, asked the way a program asks it. Reachability to
    /// a name that never resolves is still a *reachable* answer - the flags say
    /// how - so the assertion is that a target is made at all, which is what
    /// declining to answer never did.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_name_gets_a_target_from_the_host() {
        crate::bootstrap();
        // SAFETY: a null allocator is the default one, and the name is a
        // NUL-terminated C string.
        unsafe {
            let target =
                SCNetworkReachabilityCreateWithName(std::ptr::null(), c"localhost".as_ptr().cast());
            assert!(!target.is_null(), "the host answers for a resolvable name");
            let mut flags = 0u32;
            assert_ne!(
                SCNetworkReachabilityGetFlags(target, &mut flags),
                0,
                "a target the host made answers its own flags"
            );
            crate::api::CFRelease(target);
        }
    }

    unsafe extern "C" fn retain_info(info: *const c_void) -> *const c_void {
        // SAFETY: the counter this test passes as `info`.
        unsafe {
            (*(info as *const std::sync::atomic::AtomicIsize))
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        };
        info
    }

    unsafe extern "C" fn release_info(info: *const c_void) {
        // SAFETY: as above.
        unsafe {
            (*(info as *const std::sync::atomic::AtomicIsize))
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
        };
    }

    /// configd's `SCDynamicStoreCreateRunLoopSource`: one source per store, a
    /// reference to it for each call; and its context's `info` retained for
    /// the store's life.
    #[test]
    fn a_store_has_one_run_loop_source_and_keeps_its_info() {
        use std::sync::atomic::{AtomicIsize, Ordering};
        crate::bootstrap();
        let held = AtomicIsize::new(0);
        let context = Context {
            version: 0,
            info: &held as *const AtomicIsize as *mut c_void,
            retain: Some(retain_info),
            release: Some(release_info),
            copy_description: std::ptr::null(),
        };
        let name = crate::string::from_bytes(b"test", false);
        // SAFETY: a live context and name; the objects are released below.
        unsafe {
            let store = SCDynamicStoreCreate(std::ptr::null(), name, None, &context);
            assert!(!store.is_null());
            assert_eq!(held.load(Ordering::SeqCst), 1, "the store retains its info");
            let first = SCDynamicStoreCreateRunLoopSource(std::ptr::null(), store, 0);
            let second = SCDynamicStoreCreateRunLoopSource(std::ptr::null(), store, 0);
            assert!(!first.is_null());
            assert_eq!(first, second, "the same source each time");
            crate::api::CFRelease(first);
            crate::api::CFRelease(second);
            crate::api::CFRelease(store);
            assert_eq!(
                held.load(Ordering::SeqCst),
                0,
                "and releases it with itself"
            );
            crate::api::CFRelease(name);
        }
    }

    /// `SCDynamicStoreSetNotificationKeys` keeps keys and patterns, and a
    /// call that is not about a store is `kSCStatusNoStoreSession`, which
    /// `SCError` then reports and `SCErrorString` describes as configd does.
    #[test]
    fn notification_keys_are_kept_and_a_missing_store_is_an_error() {
        crate::bootstrap();
        // SAFETY: the objects are made here and released below.
        unsafe {
            let store =
                SCDynamicStoreCreate(std::ptr::null(), std::ptr::null(), None, std::ptr::null());
            let proxies = SCDynamicStoreKeyCreateProxies(std::ptr::null());
            assert_eq!(
                text_of(proxies).as_deref(),
                Some(&b"State:/Network/Global/Proxies"[..])
            );
            let pattern = crate::string::from_bytes(b"State:/Network/Interface/[^/]+/IPv4", false);
            let keys =
                crate::collections::new_array(&[proxies], Policy::from_type_callbacks(), false);
            let patterns =
                crate::collections::new_array(&[pattern], Policy::from_type_callbacks(), false);
            assert_eq!(SCDynamicStoreSetNotificationKeys(store, keys, patterns), 1);
            {
                let state = store_state(store).unwrap().lock().unwrap();
                assert_eq!(
                    state.keys.as_deref(),
                    Some(&[b"State:/Network/Global/Proxies".to_vec()][..])
                );
                assert_eq!(state.patterns.as_ref().map(Vec::len), Some(1));
            }
            assert_eq!(
                SCDynamicStoreSetNotificationKeys(store, std::ptr::null(), std::ptr::null()),
                1
            );
            assert!(
                store_state(store).unwrap().lock().unwrap().keys.is_none(),
                "null clears"
            );

            assert_eq!(
                SCDynamicStoreSetNotificationKeys(std::ptr::null(), keys, patterns),
                0
            );
            assert_eq!(SCError(), status::NO_STORE_SESSION);
            assert!(SCDynamicStoreCreateRunLoopSource(std::ptr::null(), proxies, 0).is_null());
            assert_eq!(SCError(), status::NO_STORE_SESSION);
            let text = std::ffi::CStr::from_ptr(SCErrorString(SCError()));
            assert_eq!(text, c"Configuration daemon session not active");
            assert_eq!(std::ffi::CStr::from_ptr(SCErrorString(0)), c"Success!");
            let not_found = std::ffi::CStr::from_ptr(SCErrorString(2))
                .to_string_lossy()
                .into_owned();
            assert_eq!(
                not_found, "No such file or directory",
                "an errno is strerror's"
            );

            for object in [keys, patterns, pattern, proxies, store] {
                crate::api::CFRelease(object);
            }
        }
    }
}
