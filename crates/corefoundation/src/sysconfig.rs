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
struct Context {
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
        false => match routed_source_address().is_some() {
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
/// The host's proxy configuration, in Apple's dictionary, read from the host
/// rather than assumed - see [`host_proxy`] for where the answer comes from and
/// [`PROXY_KEYS`] for the spelling of every key.
///
/// # What this host has, measured rather than assumed
///
/// The brief's instruction was to find out what this host actually has
/// configured, with a positive control, before deciding - because **a host with
/// a proxy configured and an answer of "no proxy" is the worst outcome
/// available**, and the answer being derivable is an empirical question.
///
/// It is a host with **no** proxy, and every source agrees:
///
/// | source | what it says |
/// |---|---|
/// | `gsettings org.gnome.system.proxy mode` | `'none'`, the **schema default** - `dconf dump /system/proxy/` is empty |
/// | `org.gnome.system.proxy.http enabled` | `false`; `host` `''`; `port` `8080` (a default, and `mode` is what makes it mean nothing) |
/// | `https`, `socks`, `ftp` `host` | `''` on all three |
/// | the `*_proxy` environment variables | none set |
/// | `/etc/environment`, `/etc/profile`, `/etc/profile.d/*` | no proxy in any of them |
/// | NetworkManager `proxy.method` | `none` on all six connections; no `proxy.pac-url` |
/// | DHCP `OPTION[23] requested_wpad` | requested, and `proxy.method: none` - WPAD is not in use |
/// | `~/.config/kioslaverc` | does not exist |
/// | a listening local proxy daemon | none |
///
/// **The positive control, because "no proxy everywhere" is also what a broken
/// instrument reports.** Two of them:
///
/// * The dconf store is **live and writable-in-principle**, not inert: `dconf
///   dump /` returns dozens of stored keys, and one of them differs from its
///   schema default - `/org/gnome/desktop/background/picture-uri` is stored as
///   `blobs-l.svg` where the schema's default is `adwaita-l.jxl`. So a
///   `gsettings get` that reports a *stored* value is reporting the store.
/// * A **non-default** value was read back through the same path: a keyfile
///   backend with `mode='manual'` written into it returned... the defaults, not
///   what was written. **That control failed**, and it is recorded here rather
///   than quietly dropped, because it is the reason the answer rests on the
///   dconf dump rather than on a round trip: the keyfile backend was not
///   consulted at all (wrong directory layout, or a backend that ignores it
///   here), so "I set it and read it back" is not available as evidence on this
///   host. What *is* available - a live store with a stored non-default value,
///   and an empty `/system/proxy/` subtree - is what the table above rests on.
///
/// There is a Mullvad WireGuard interface on this host (`wg0-mullvad`), which
/// is worth naming because it is the obvious "but there is a tunnel" objection:
/// a VPN is not a proxy, `nmcli` reports `proxy.method: none` for it, and no
/// source above reports a proxy for it.
///
/// The shape is Apple's, transcribed from the two headers that specify it:
///
/// * `<SystemConfiguration/SCDynamicStoreCopySpecific.h>` gives the result
///   contract: *"Returns a dictionary containing key-value pairs that represent
///   the current internet proxy settings; NULL if no proxy settings have been
///   defined **or if an error was encountered**."* Both of this function's
///   possible answers are therefore Apple's own answers, and [`refusal`] says
///   which one this is and why.
/// * `SCSchemaDefinitions.h` gives each key's name and its type - `CFNumber (0
///   or 1)` for the enable flags, **not** `CFBoolean`, which is the kind of
///   detail that is invisible until a program reads the wrong accessor.
///
/// # Why the answer is not NULL on a host with no proxy
///
/// The obvious thing is to keep returning NULL, because that is what Apple's
/// header permits and because this host has no proxy. That would be the
/// *refusal* this project ranks worst, wearing the right value: a Mac with no
/// proxy still has a `Proxies` dictionary for every network service, so
/// `SCDynamicStoreCopyProxies` returns a non-NULL dictionary of zeroed enable
/// flags, and the guest can tell "configured, and off" from "there is no
/// configuration here" - which is the difference between a browser that knows
/// it needs no proxy and one that has not been told.
///
/// The binary settles it rather than the guess. Chromium 130's
/// `net/proxy_resolution/proxy_config_service_mac.cc` has
/// `DCHECK(config_dict)` on the line after this call, which a release build
/// compiles out and a debug build does not. A `DCHECK` is a statement by the
/// caller about what the callee really returns, and the brief's rule is that
/// the binary is the authority. So a non-NULL dictionary is the answer, with
/// every enable flag carrying the host's real opinion and the host/port/URL keys
/// present only when the host has such a value.
pub unsafe extern "C" fn SCDynamicStoreCopyProxies(_store: CFTypeRef) -> CFTypeRef {
    let config = host_proxy();
    if let Some(why) = &config.error {
        // Apple's documented answer for "an error was encountered", and the
        // reason it is NULL is named rather than left to be inferred: a NULL
        // that is a refusal and a NULL that is a fact about the host are the
        // same value, and only one of them is true.
        refusal(&format!(
            "SCDynamicStoreCopyProxies: no proxy configuration could be read: {why}.\n  \
             The caller is net/proxy_resolution/proxy_config_service_mac.cc's \
             GetCurrentProxyConfig, which reads this dictionary and then reads every \
             kSCPropNetProxies* key out of it. Apple returns NULL here for an error as \
             well as for an absent configuration (SCDynamicStoreCopySpecific.h), and this \
             is the error case: reporting it as 'no proxy' would be a guess that reads \
             exactly like a fact.\n"
        ));
        return std::ptr::null();
    }
    let Some(config) = &config.proxy else {
        // No source on this host can be asked about a proxy at all. That is
        // also a derivation rather than a guess - there is nowhere for a proxy
        // to have been configured - and it is traced like any other answer.
        return std::ptr::null();
    };
    let dictionary = dictionary_for(&config.proxy);
    trace(
        "answer",
        &format!(
            "SCDynamicStoreCopyProxies: answered from {}: {}",
            config.source,
            describe(&config.proxy)
        ),
    );
    // The read-back, through **the guest's own two steps**: the key out of
    // [`PROXY_KEYS`] — which is the string the published constant holds — and
    // then `CFDictionaryGetValue`, which is the call the guest makes at
    // `0x674badd`. This is what makes the trace evidence rather than a
    // statement about a dictionary nobody has queried: it prints, per key, the
    // value `CFDictionaryGetValue` returns for the key the guest will use.
    //
    // It is here rather than in a test because a test can only prove it for a
    // fixture, and the claim is about *this host*. The guest-address
    // breakpoints that would prove it end to end do not fire on this image —
    // Chromium installs its own `SIGTRAP` handler during start-up, which takes
    // the trap away from the debugger before gdb can see it, so "no
    // breakpoints fired" and "no breakpoints were watching" are the same log
    // line. `test/tools/watch-proxy-keys.py` is that instrument, and its
    // fingerprint control is what distinguishes the two cases.
    // The lines are **collected and then written**, because the group is
    // one-shot and a per-line flag would print the first key and stop: the
    // first version of this flipped its flag on the first line, so a
    // seventeen-key read-back reported exactly one key - which reads as
    // "the other sixteen are fine" when it means "the other sixteen were never
    // asked".
    let readback: Vec<String> = PROXY_KEYS
        .iter()
        .map(|(symbol, _)| {
            let key = key_of(symbol);
            // SAFETY: a dictionary this call just made, and a key out of the
            // same table the guest's constant is published from.
            let value = unsafe { crate::api::CFDictionaryGetValue(dictionary, key) };
            if value.is_null() {
                return format!("{symbol} ({}) -> absent", spelling(key));
            }
            let what = match unsafe { text_of(value) } {
                Some(text) => format!("a string of {} byte(s)", text.len()),
                // A number, which is what every enable flag is, and an array,
                // which is what the bypass list is.
                None => match unsafe { crate::collections::number(value) } {
                    Some(number) => format!("the number {}", number.as_i64()),
                    // `collections::array` borrows the contents rather than
                    // handing back the object, so the count comes from the
                    // contents and the object is only needed to reach them.
                    None => match unsafe { crate::collections::array(value) } {
                        Some(array) => {
                            format!("an array of {} item(s)", array.values.len())
                        }
                        None => "a value of another type".to_string(),
                    },
                },
            };
            format!("{symbol} ({}) -> {what}", spelling(key))
        })
        .collect();
    trace_group("read-back", &readback);
    dictionary
}

/// The text of an object of this runtime's, or `None` when it is not a string.
fn spelling(value: CFTypeRef) -> String {
    // SAFETY: null or a live object of this runtime's.
    match unsafe { text_of(value) } {
        Some(text) => String::from_utf8_lossy(&text).into_owned(),
        None => "<not a string>".to_string(),
    }
}

/// A proxy as one host source described it.
///
/// Every field is `Option` or empty because "this source has no opinion about
/// HTTPS" and "this source says HTTPS is off" are different facts, and only the
/// second one is a claim about the host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HostProxy {
    /// A GNOME-style mode: `none`, `manual`, `auto` or `pac`. `None` from a
    /// source with no such concept - the environment only ever names schemes.
    mode: Option<String>,
    /// The PAC URL, where the host has one.
    pac_url: Option<String>,
    /// WPAD / proxy auto-discovery, which is `ProxyAutoDiscoveryEnable`.
    auto_discovery: bool,
    http: Endpoint,
    https: Endpoint,
    socks: Endpoint,
    ftp: Endpoint,
    /// The bypass list, which is `ExceptionsList`.
    exceptions: Vec<String>,
    /// `ExcludeSimpleHostnames`: bypass every name with no dot in it.
    exclude_simple_hostnames: bool,
    /// GNOME's "use the HTTP proxy for every scheme", which decides what the
    /// per-scheme hosts below mean when only HTTP names one.
    use_same_proxy: bool,
}

/// One scheme's proxy, as a source described it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Endpoint {
    /// Whether the source says the scheme is proxied.
    enabled: Option<bool>,
    host: Option<String>,
    port: Option<u16>,
}

impl HostProxy {
    /// Whether this source had anything to say.
    ///
    /// A source that read successfully and reported nothing configured is
    /// still an answer - it is the answer "off" - so this is about *silence*,
    /// and it is what lets a later source be consulted.
    fn has_an_opinion(&self) -> bool {
        self.mode.is_some()
            || self.pac_url.is_some()
            || self.auto_discovery
            || self.http.has_an_opinion()
            || self.https.has_an_opinion()
            || self.socks.has_an_opinion()
            || self.ftp.has_an_opinion()
            || !self.exceptions.is_empty()
            || self.exclude_simple_hostnames
    }

    /// Whether any scheme is actually proxied.
    ///
    /// Not used to decide the dictionary - the dictionary carries the host's
    /// opinion either way - but the question the guest asks when it reads it,
    /// and the one worth having an answer to: a configuration that proxies
    /// nothing and a configuration that is absent reach the guest by different
    /// routes, and only the first of the two has this answer.
    #[cfg(test)]
    fn proxies_anything(&self) -> bool {
        let on = |e: &Endpoint| {
            e.enabled.unwrap_or_else(|| e.host.as_deref().is_some_and(|h| !h.is_empty()))
                && e.host.as_deref().is_some_and(|h| !h.is_empty())
        };
        self.pac_url.is_some()
            || self.auto_discovery
            || on(&self.http)
            || on(&self.https)
            || on(&self.socks)
            || on(&self.ftp)
    }
}

impl Endpoint {
    fn has_an_opinion(&self) -> bool {
        self.enabled.is_some() || self.host.is_some() || self.port.is_some()
    }
}

/// What one source said, and where it was read from, so that the answer is
/// auditable: a proxy answer nobody can trace is a silent answer.
#[derive(Clone, Debug)]
struct Answered {
    source: &'static str,
    proxy: Box<HostProxy>,
}

/// The host's proxy configuration, and the refusal when it could not be read.
#[derive(Clone, Debug)]
struct HostAnswer {
    proxy: Option<Answered>,
    /// Why no source could be read, when that is what happened.
    error: Option<String>,
}

impl Default for HostAnswer {
    fn default() -> Self {
        Self {
            proxy: None,
            error: None,
        }
    }
}

/// Ask every source this host has, in the order [`host_proxy`] explains, and
/// take the first one with an opinion.
///
/// Cached, because it is read on the guest's start-up path and the host's proxy
/// configuration does not change under a running process: a program that wants
/// it again after a change is asking a question macOS's own store answers with
/// a notification this tree does not deliver, and the module note says so.
fn host_proxy() -> HostAnswer {
    static ANSWER: std::sync::OnceLock<HostAnswer> = std::sync::OnceLock::new();
    ANSWER
        .get_or_init(|| {
            let mut failures = Vec::new();
            for (name, read) in SOURCES {
                match read() {
                    Ok(Some(proxy)) => {
                        return HostAnswer {
                            proxy: Some(Answered {
                                source: name,
                                proxy: Box::new(proxy),
                            }),
                            error: None,
                        };
                    }
                    // A source that does not exist on this host is information,
                    // not a failure: there is nowhere here for a proxy to have
                    // been configured, and saying so is part of the answer.
                    Ok(None) => {}
                    Err(why) => failures.push(format!("{name}: {why}")),
                }
            }
            HostAnswer {
                proxy: None,
                error: (!failures.is_empty()).then(|| failures.join("; ")),
            }
        })
        .clone()
}

/// Every source, in the order they are asked, as `(name, read)`.
///
/// The order **is** the precedence, and each entry says why it sits where it
/// does:
///
/// 1. **`gsettings org.gnome.system.proxy`** - the session's own configuration,
///    which is the closest analogue of the per-network-service settings Apple's
///    store merges, and the one every application on a desktop reads. First
///    because it is the machine's configured answer rather than one process's.
/// 2. **NetworkManager** - the system's per-connection settings. Second for
///    the same reason, and *partial* on this host: see
///    [`networkmanager_proxy`].
/// 3. **The process environment** - a *process's* statement, so it cannot
///    outrank the machine's, but it is what a machine with no desktop session
///    has instead.
/// 4. **`/etc/environment` and `/etc/profile`** - the least specific, and last
///    for that reason rather than because it is less real.
static SOURCES: &[(&str, fn() -> Result<Option<HostProxy>, String>)] = &[
    ("gsettings org.gnome.system.proxy", gsettings_proxy),
    ("NetworkManager (nmcli)", networkmanager_proxy),
    ("the process environment", environment_proxy),
    ("/etc/environment and /etc/profile", files_proxy),
];

/// What the answer is, in one line, for `ROSETTA3_TRACE_PROXY=1`.
fn describe(config: &HostProxy) -> String {
    let mut out = Vec::new();
    for (name, endpoint) in [
        ("HTTP", &config.http),
        ("HTTPS", &config.https),
        ("SOCKS", &config.socks),
        ("FTP", &config.ftp),
    ] {
        let enabled = endpoint.enabled.unwrap_or(false);
        match (&endpoint.host, enabled) {
            (Some(host), true) => out.push(format!(
                "{name} -> {host}:{}",
                endpoint
                    .port
                    .map_or_else(|| "<no port>".to_string(), |p| p.to_string())
            )),
            (Some(host), false) => out.push(format!("{name} off (host {host} present, not enabled)")),
            (None, true) => out.push(format!("{name} on with NO HOST")),
            (None, false) => out.push(format!("{name} off")),
        }
    }
    if let Some(url) = &config.pac_url {
        out.push(format!("PAC {url}"));
    }
    if config.auto_discovery {
        out.push("WPAD".to_string());
    }
    if !config.exceptions.is_empty() {
        out.push(format!("{} bypass rule(s)", config.exceptions.len()));
    }
    if config.exclude_simple_hostnames {
        out.push("bypass simple hostnames".to_string());
    }
    if out.is_empty() {
        return "no scheme proxied".to_string();
    }
    out.join(", ")
}

/// The dictionary Apple's key names and types, built from the host's answer.
///
/// Every enable key is always present, because "this host says HTTP is off" is
/// a fact and "this host has never heard of HTTP proxies" is a different one.
/// Every host/port/URL key is present only when the host has such a value,
/// because publishing an empty proxy host would be a value nobody measured.
fn dictionary_for(config: &HostProxy) -> CFTypeRef {
    /// One pair, holding the objects it made so they can be released.
    struct Made {
        key: &'static str,
        value: CFTypeRef,
        owned: bool,
    }

    let mut made: Vec<Made> = Vec::new();
    let mut push = |key: &'static str, value: CFTypeRef, owned: bool| {
        made.push(Made { key, value, owned });
    };

    let flag = |on: bool| {
        crate::collections::new_number(crate::collections::Number::Integer(i64::from(on)))
            as CFTypeRef
    };
    let text = |s: &str| crate::string::from_bytes(s.as_bytes(), false) as CFTypeRef;

    let same = config.use_same_proxy;
    // GNOME's schema has no `https.enabled` or `socks.enabled`: a non-empty
    // `host` is its own statement that the scheme is configured, and `mode`
    // decides whether any of it applies. Recorded here because it is a
    // derivation and not a transcription.
    let enabled = |endpoint: &Endpoint, fallback: Option<bool>| -> bool {
        endpoint.enabled.unwrap_or_else(|| {
            endpoint.host.as_deref().is_some_and(|host| !host.is_empty())
        }) || fallback.unwrap_or(false)
    };

    push(
        "_kSCPropNetProxiesProxyAutoDiscoveryEnable",
        flag(config.auto_discovery),
        true,
    );
    let pac = config.pac_url.as_deref().is_some_and(|url| !url.is_empty());
    push(
        "_kSCPropNetProxiesProxyAutoConfigEnable",
        flag(pac),
        true,
    );
    if let Some(url) = config.pac_url.as_deref().filter(|url| !url.is_empty()) {
        push(
            "_kSCPropNetProxiesProxyAutoConfigURLString",
            text(url),
            true,
        );
    }

    for (enable_key, proxy_key, port_key, endpoint, http_fallback) in [
        (
            "_kSCPropNetProxiesHTTPEnable",
            "_kSCPropNetProxiesHTTPProxy",
            "_kSCPropNetProxiesHTTPPort",
            &config.http,
            None,
        ),
        (
            "_kSCPropNetProxiesHTTPSEnable",
            "_kSCPropNetProxiesHTTPSProxy",
            "_kSCPropNetProxiesHTTPSPort",
            &config.https,
            same.then(|| config.http.enabled.unwrap_or(false) && config.http.host.is_some()),
        ),
        (
            "_kSCPropNetProxiesSOCKSEnable",
            "_kSCPropNetProxiesSOCKSProxy",
            "_kSCPropNetProxiesSOCKSPort",
            &config.socks,
            same.then(|| config.http.enabled.unwrap_or(false) && config.http.host.is_some()),
        ),
        (
            "_kSCPropNetProxiesFTPEnable",
            "_kSCPropNetProxiesFTPProxy",
            "_kSCPropNetProxiesFTPPort",
            &config.ftp,
            same.then(|| config.http.enabled.unwrap_or(false) && config.http.host.is_some()),
        ),
    ] {
        // A port is published only **with its proxy host**. A source reports a
        // port for every scheme whether or not that scheme is configured -
        // GNOME's schema default for `http.port` is 8080 on a host with no
        // proxy at all - so publishing it alone would put a real number under
        // a key whose scheme is off, and a reader that took the enable flag
        // from one key and the port from another would get 8080 for a proxy
        // that does not exist. `HTTPPort` beside `HTTPEnable: 0` is
        // harmless, and a port with no host is a value nobody measured.
        // GNOME's `use-same-proxy` means *every other scheme uses the HTTP
        // proxy*, so the host and port to publish are the HTTP ones wherever a
        // scheme has none of its own. Resolving it here rather than in the
        // source keeps the fallback in one place, and it is the same
        // substitution GNOME makes - the flag is on the HTTP proxy, and
        // Chromium reads one host and one port per scheme.
        let own_host = endpoint.host.as_deref().filter(|host| !host.is_empty());
        let (host, port): (Option<&str>, Option<u16>) = match (own_host, same) {
            (Some(host), _) => (Some(host), endpoint.port),
            (None, true) => (
                config.http.host.as_deref().filter(|h| !h.is_empty()),
                config.http.port,
            ),
            (None, false) => (None, None),
        };
        let on = enabled(endpoint, http_fallback) && host.is_some();
        push(enable_key, flag(on), true);
        if on {
            // `on` implies a host, and the flag is what decides it.
            let host = host.expect("an enabled scheme has a host");
            push(proxy_key, text(host), true);
            // With a proxy on and no port in the source, Apple's dictionary has
            // no port either, and Chromium falls back to its own per-scheme
            // default - which is the documented behaviour of
            // `ProxyDictionaryToProxyChain`, which passes `port_key` down and
            // calls `ProxyServer::GetDefaultPortForScheme` when the key is
            // absent.
            if let Some(port) = port {
                push(
                    port_key,
                    crate::collections::new_number(crate::collections::Number::Integer(
                        i64::from(port),
                    )) as CFTypeRef,
                    true,
                );
            }
        }
    }

    if !config.exceptions.is_empty() {
        let items: Vec<CFTypeRef> = config
            .exceptions
            .iter()
            .map(|item| crate::string::from_bytes(item.as_bytes(), false))
            .collect();
        let array = crate::collections::new_array(
            &items,
            Policy::from_type_callbacks(),
            false,
        );
        for item in &items {
            // SAFETY: a string `new_array` just made, which it has retained.
            unsafe { crate::api::CFRelease(*item) };
        }
        push("_kSCPropNetProxiesExceptionsList", array, true);
    }
    push(
        "_kSCPropNetProxiesExcludeSimpleHostnames",
        flag(config.exclude_simple_hostnames),
        true,
    );

    let pairs: Vec<(CFTypeRef, CFTypeRef)> = made
        .iter()
        .map(|m| (key_of(m.key), m.value))
        .collect();
    let dictionary = crate::collections::new_dictionary(
        &pairs,
        Policy::from_key_callbacks(),
        Policy::from_type_callbacks(),
        false,
    );
    // The dictionary retained every value; this function's own references to
    // the ones it made are spent.
    for m in &made {
        if m.owned {
            // SAFETY: an object made just above and retained by `dictionary`.
            unsafe { crate::api::CFRelease(m.value) };
        }
    }
    dictionary
}

/// The `kSCPropNetProxies*` keys, each `(Mach-O name, string value)`.
///
/// **The values are transcribed, not inferred.** Apple's
/// `<SystemConfiguration/SCSchemaDefinitions.h>` prints the string beside every
/// one of them in its schema table - the file is a *table*, and the third column
/// is the value, so the value is not something this tree has to know:
///
/// ```text
///  *   kSCPropNetProxiesExceptionsList      "ExceptionsList"        CFArray[CFString]
///  *   kSCPropNetProxiesHTTPEnable          "HTTPEnable"            CFNumber (0 or 1)
///  *   kSCPropNetProxiesHTTPPort            "HTTPPort"              CFNumber
///  *   kSCPropNetProxiesHTTPProxy           "HTTPProxy"             CFString
///  *   kSCPropNetProxiesHTTPSEnable         "HTTPSEnable"           CFNumber (0 or 1)
///  *   kSCPropNetProxiesProxyAutoConfigURLString "ProxyAutoConfigURLString" CFString
/// ```
///
/// Every value is the symbol's own name with the `kSCPropNetProxies` prefix
/// removed, which is a coincidence of the naming and **not** the reason to
/// believe them: the rule would also have predicted `HTTPSEnabled`, and the
/// table says `HTTPSEnable`. That is the whole reason this is transcribed from
/// the table rather than derived from the symbol.
///
/// The **types** matter as much as the names. `SCDynamicStoreCopyValue`'s
/// sibling `SCDynamicStoreCopyProxies` is read by Chromium through
/// `CFNumberGetValue(..., kCFNumberIntType, ...)` on every enable flag, so a
/// `CFBoolean` here would be read through the wrong accessor; the table's
/// `CFNumber (0 or 1)` is what [`dictionary_for`] publishes.
///
/// These are the 17 the Framework imports, and they are the 17 the table has
/// an entry for *that the image names*; the check is mechanical and
/// `the_proxy_keys_are_apple_s_spelling_and_not_the_symbol_s_tail` asserts the
/// count. Fourteen further keys are in the same table and are **not** published -
/// `FTPPassive`, `Gopher{Enable,Port,Proxy,User}`, `RTSP{Enable,Port,Proxy,User}`,
/// `{HTTP,HTTPS,SOCKS,FTP}User` and `ProxyAutoConfigJavaScript` - because the
/// image imports none of them, and a name nothing imports is a constant this tree
/// has no reader for.
const PROXY_KEYS: &[(&str, &str)] = &[
    ("_kSCPropNetProxiesExceptionsList", "ExceptionsList"),
    ("_kSCPropNetProxiesExcludeSimpleHostnames", "ExcludeSimpleHostnames"),
    ("_kSCPropNetProxiesFTPEnable", "FTPEnable"),
    ("_kSCPropNetProxiesFTPPort", "FTPPort"),
    ("_kSCPropNetProxiesFTPProxy", "FTPProxy"),
    ("_kSCPropNetProxiesHTTPEnable", "HTTPEnable"),
    ("_kSCPropNetProxiesHTTPPort", "HTTPPort"),
    ("_kSCPropNetProxiesHTTPProxy", "HTTPProxy"),
    ("_kSCPropNetProxiesHTTPSEnable", "HTTPSEnable"),
    ("_kSCPropNetProxiesHTTPSPort", "HTTPSPort"),
    ("_kSCPropNetProxiesHTTPSProxy", "HTTPSProxy"),
    ("_kSCPropNetProxiesSOCKSEnable", "SOCKSEnable"),
    ("_kSCPropNetProxiesSOCKSPort", "SOCKSPort"),
    ("_kSCPropNetProxiesSOCKSProxy", "SOCKSProxy"),
    (
        "_kSCPropNetProxiesProxyAutoConfigEnable",
        "ProxyAutoConfigEnable",
    ),
    (
        "_kSCPropNetProxiesProxyAutoConfigURLString",
        "ProxyAutoConfigURLString",
    ),
    (
        "_kSCPropNetProxiesProxyAutoDiscoveryEnable",
        "ProxyAutoDiscoveryEnable",
    ),
];

/// The key object for one of [`PROXY_KEYS`], interned.
///
/// Out of [`PROXY_KEYS`]'s own table rather than a second list of the strings,
/// so the key the dictionary is built with and the constant the guest reads
/// cannot be two different spellings of the same Apple key. That is the
/// property that makes answering safe at all: a dictionary keyed `"HTTPEnable"`
/// read with a constant holding `"HTTPPort"` reports "no proxy" and looks
/// correct doing it.
fn key_of(symbol: &str) -> CFTypeRef {
    PROXY_KEYS
        .iter()
        .find_map(|(name, text)| (*name == symbol).then(|| {
            crate::base::interned_name(text) as CFTypeRef
        }))
        .unwrap_or(std::ptr::null())
}

/// The first source: the session's own desktop proxy configuration.
///
/// `gsettings list-recursively org.gnome.system.proxy`, which is what the
/// control centre itself writes and what every GNOME application reads. It
/// reports the **effective** value - a stored one, or the schema's default if
/// nothing is stored - which is the right thing to answer here, because "the
/// system's proxy configuration" means the value in force, not the value a
/// user typed.
///
/// `Err` only when the tool ran and said something this cannot read, which is
/// the one case where "no proxy" would be a guess rather than a fact.
const SCHEMA: &str = "org.gnome.system.proxy";

fn gsettings_proxy() -> Result<Option<HostProxy>, String> {
    let output = match run(&["gsettings", "list-recursively", "org.gnome.system.proxy"]) {
        // `gsettings` is not installed, so this host has no such mechanism.
        None => return Ok(None),
        Some(output) => output?,
    };
    parse_gsettings(parse_list_recursively(&output, SCHEMA))
}

/// Read a `list-recursively` transcript into a [`HostProxy`].
///
/// Separate from the process spawn so the parse can be tested against a
/// transcript rather than against whatever this host happens to be running
/// today - which is the difference between a test that can fail and one that
/// can only ever confirm itself.
fn parse_gsettings(values: Settings) -> Result<Option<HostProxy>, String> {
    let mut config = HostProxy {
        use_same_proxy: values.boolean("org.gnome.system.proxy.use-same-proxy"),
        ..HostProxy::default()
    };
    let mode = values
        .string("org.gnome.system.proxy.mode")
        .ok_or_else(|| "no mode in the output".to_string())?;
    config.mode = Some(mode.clone());
    config.auto_discovery = matches!(mode.as_str(), "auto");
    config.pac_url = values
        .string("org.gnome.system.proxy.autoconfig-url")
        .filter(|url| !url.is_empty())
        .or_else(|| matches!(mode.as_str(), "pac").then(String::new));
    config.exceptions = values.list("org.gnome.system.proxy.ignore-hosts");
    for (endpoint, schema) in [
        (&mut config.http, "http"),
        (&mut config.https, "https"),
        (&mut config.socks, "socks"),
        (&mut config.ftp, "ftp"),
    ] {
        endpoint.host = values
            .string(&format!("org.gnome.system.proxy.{schema}.host"))
            .filter(|host| !host.is_empty());
        endpoint.port = values.number(&format!("org.gnome.system.proxy.{schema}.port"));
        // `http.enabled` is the only one of the four this schema has.
        // **The host wins over `mode`, and the order is load-bearing.**
        // `http.enabled` is the key that exists; `mode` is only a *fallback* for
        // the three schemes whose schema has no `enabled` at all. A host that
        // says `http.enabled = true` with `mode = 'none'` means it: the flag is
        // the specific statement and `mode` is the general one, and reading
        // `mode` first is how a configured HTTP proxy gets reported as off.
        // Verified against this host's own schema, which has
        // `http.enabled` and no `https.enabled`, `socks.enabled` or
        // `ftp.enabled`.
        let flag = values.boolean_opt(&format!("org.gnome.system.proxy.{schema}.enabled"));
        let configured = endpoint.host.as_ref().is_some_and(|host| !host.is_empty());
        endpoint.enabled = match flag {
            Some(flag) => Some(flag),
            // No flag of its own: `manual` with a host is a manual proxy, and
            // nothing else is.
            None => Some(configured && matches!(mode.as_str(), "manual")),
        };
    }
    Ok(Some(config))
}

/// The second source: the system's per-connection proxy configuration.
///
/// NetworkManager, which is where a Linux host's connections get their proxy
/// settings, and which is the closest analogue of macOS's per-network-service
/// settings - the thing `SCDynamicStoreCopyProxies` merges.
///
/// **Partial, and the limit is measured rather than assumed.** This
/// NetworkManager (1.4x) exposes `proxy.method`, `proxy.browser-only`,
/// `proxy.pac-url` and `proxy.pac-script` through `nmcli con show` and *no*
/// per-scheme host or port, so it answers `ProxyAutoConfigEnable`,
/// `ProxyAutoConfigURLString` and `ProxyAutoDiscoveryEnable` - three of the
/// eight keys the guest reads on the measured path - and says nothing about
/// the four proxies themselves. Where it is silent, a later source answers.
fn networkmanager_proxy() -> Result<Option<HostProxy>, String> {
    // The fields have to be asked for **per connection**: `nmcli con show` with
    // no name refuses them outright, and prints
    // `invalid field 'proxy.method'` - so the names come first, and each is
    // asked about in turn. `nmcli -t -f NAME con show` splits on `:` and a
    // connection name may itself contain one (`Akbar's iPhone` is a name here),
    // so the output is read as whole lines and never split on a separator.
    let Some(names) = run(&["nmcli", "-t", "-f", "NAME", "con", "show"]) else {
        return Ok(None);
    };
    let names = names?;

    let mut method: Option<String> = None;
    let mut pac_url: Option<String> = None;
    for name in names.lines().filter(|line| !line.is_empty()) {
        let Some(output) = run(&["nmcli", "-t", "-f", "proxy.method,proxy.pac-url", "con", "show", name])
        else {
            continue;
        };
        // A connection that cannot be read is that connection's business; the
        // others still answer, and macOS merges per-service settings the same
        // way, so one unreadable profile is not a failure of the whole source.
        let Ok(output) = output else { continue };
        for line in output.lines() {
            // nmcli's terse format is `field:value`, and a value may contain a
            // `:` (a PAC URL has two), so the field is split off the front and
            // the rest is left whole.
            let Some((field, value)) = line.split_once(':') else {
                continue;
            };
            match field {
                // The first profile that names a method speaks for the host, and
                // a later one overriding it is not modelled: macOS's own
                // `__SCOPED__` array is per *interface*, and which interface a
                // request uses is not knowable from here.
                "proxy.method" if method.is_none() && !value.is_empty() => {
                    method = Some(value.to_string())
                }
                "proxy.pac-url" if pac_url.is_none() && !value.is_empty() => {
                    pac_url = Some(value.to_string())
                }
                _ => {}
            }
        }
    }
    let Some(method) = method else {
        return Err("no connection reported a proxy.method".to_string());
    };
    Ok(Some(HostProxy {
        auto_discovery: method == "auto",
        pac_url,
        mode: Some(method),
        ..HostProxy::default()
    }))
}

/// The third source: the process's own environment.
///
/// The variables every Linux HTTP stack reads, in both spellings. This is a
/// *process's* statement rather than the machine's, which is why it is third:
/// macOS's `SCDynamicStoreCopyProxies` reports the system's configuration and
/// has no environment equivalent, so an environment variable is the closest
/// thing this host has to a per-process override, and it is consulted only when
/// no system source had an opinion.
fn environment_proxy() -> Result<Option<HostProxy>, String> {
    let read = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    Ok(parse_proxy_variables(&read))
}

/// The fourth source: the same variables, in the files a session is built from.
///
/// A process whose environment was not assembled by a login shell - a service,
/// a container entry point - never sees `/etc/profile`, and a proxy configured
/// there is real. This is the last source because it is the least specific: it
/// is what a machine's users were told, not what the machine is doing.
fn files_proxy() -> Result<Option<HostProxy>, String> {
    let mut text = String::new();
    for path in ["/etc/environment", "/etc/profile"] {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                text.push_str(&contents);
                text.push('\n');
            }
            // Unreadable is not an error: `/etc/profile` is absent on a minimal
            // image, and a file that is not there cannot hold a setting.
            Err(_) => {}
        }
    }
    if let Ok(entries) = std::fs::read_dir("/etc/profile.d") {
        let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            if let Ok(contents) = std::fs::read_to_string(&path) {
                text.push_str(&contents);
                text.push('\n');
            }
        }
    }
    let read = |name: &str| {
        for line in text.lines() {
            let line = line.trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() == name {
                return Some(value.trim().trim_matches(['"', '\'']).to_string());
            }
        }
        None
    };
    Ok(parse_proxy_variables(&read))
}

/// `scheme://host:port` out of the `*_proxy` variables, which is the spelling
/// every one of these stacks documents and the only one that carries a port.
fn parse_proxy_variables(read: &dyn Fn(&str) -> Option<String>) -> Option<HostProxy> {
    let exceptions: Vec<String> = read("no_proxy")
        .or_else(|| read("NO_PROXY"))
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut config = HostProxy {
        exceptions,
        ..HostProxy::default()
    };
    for (names, endpoint) in [
        (&["http_proxy", "HTTP_PROXY"][..], &mut config.http),
        (&["https_proxy", "HTTPS_PROXY"][..], &mut config.https),
        (&["socks_proxy", "SOCKS_proxy"][..], &mut config.socks),
        (&["ftp_proxy", "FTP_PROXY"][..], &mut config.ftp),
    ] {
        let Some(url) = names.iter().find_map(|name| read(name)) else {
            continue;
        };
        let (host, port) = match url.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), port.parse::<u16>().ok()),
            None => (url.clone(), None),
        };
        let host = host
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_start_matches("socks5://")
            .trim_start_matches("socks5h://")
            .trim_start_matches("socks4://")
            .trim_end_matches('/')
            .to_string();
        if host.is_empty() {
            continue;
        }
        endpoint.enabled = Some(true);
        endpoint.host = Some(host);
        endpoint.port = port;
    }
    // `all_proxy` is the catch-all these stacks apply to every scheme, and is
    // the environment's own version of GNOME's `use-same-proxy`.
    if let Some(url) = read("all_proxy").or_else(|| read("ALL_PROXY")) {
        for endpoint in [&mut config.http, &mut config.https, &mut config.socks, &mut config.ftp] {
            if endpoint.host.is_some() {
                continue;
            }
            let (host, port) = match url.rsplit_once(':') {
                Some((host, port)) => (host.to_string(), port.parse::<u16>().ok()),
                None => (url.clone(), None),
            };
            let host = host
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .trim_start_matches("socks5://")
                .trim_end_matches('/')
                .to_string();
            if host.is_empty() {
                continue;
            }
            endpoint.enabled = Some(true);
            endpoint.host = Some(host);
            endpoint.port = port;
        }
    }
    config.has_an_opinion().then_some(config)
}

/// Run a host tool and return its standard output.
///
/// `None` when the tool does not exist - which is a fact about the host and not
/// a failure. `Err` when it ran and did not succeed, which *is* a failure: a
/// tool that is installed and cannot answer has not told us "no proxy", and
/// reading its silence as "no proxy" is the guess this whole module exists to
/// avoid.
fn run(command: &[&str]) -> Option<Result<String, String>> {
    let output = std::process::Command::new(command[0])
        .args(&command[1..])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return Some(Err(format!(
            "{} exited with {}",
            command[0],
            output.status
        )));
    }
    Some(Ok(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// The `key value` lines of `gsettings list-recursively`, for `schemas` only.
struct Settings {
    values: Vec<(String, String)>,
}

impl Settings {
    fn raw(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// A string value, with GLib's quoting removed.
    ///
    /// An `Err` rather than a `None` for a value that is present but is not a
    /// string, because "this key is something I cannot read" and "this key is
    /// not set" must not both become an absent value.
    fn string(&self, key: &str) -> Option<String> {
        let raw = self.raw(key)?;
        unquote(raw).ok().flatten()
    }

    /// A boolean, or `None` when the key is absent.
    fn boolean(&self, key: &str) -> bool {
        self.boolean_opt(key).unwrap_or(false)
    }

    fn boolean_opt(&self, key: &str) -> Option<bool> {
        match self.raw(key)? {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    fn number(&self, key: &str) -> Option<u16> {
        let raw = self.raw(key)?;
        raw.parse().ok()
    }

    /// A `CFArray[CFString]`, in the `[a, b]` form GLib prints.
    fn list(&self, key: &str) -> Vec<String> {
        let Some(raw) = self.raw(key) else {
            return Vec::new();
        };
        // `@as [...]` is the typed empty form and `['as', ...]` the typed
        // general one; both print a comma-separated list of quoted strings.
        let body = raw
            .strip_prefix("@as ")
            .or_else(|| raw.strip_prefix("['as', "))
            .unwrap_or(raw);
        let Some(body) = body.strip_prefix('[').and_then(|b| b.strip_suffix(']')) else {
            return Vec::new();
        };
        body.split(',')
            .filter_map(|item| unquote(item.trim()).ok().flatten())
            .filter(|item| !item.is_empty())
            .collect()
    }
}

/// `gsettings list-recursively` output, keyed by the full dotted name.
///
/// **The line is `SCHEMA KEY VALUE`, but the dotted name spans all three and is
/// not a prefix of the printed text with a space in it.** Real output from this
/// host:
///
/// ```text
/// org.gnome.system.proxy mode 'none'
/// org.gnome.system.proxy.http host 'proxy.example.invalid'
/// ```
///
/// `mode` and `http host` are *separate keys in separate schemas* -
/// `org.gnome.system.proxy` and `org.gnome.system.proxy.http` - and the printed
/// line is schema, space, key, space, value. So the stored name is the schema
/// and the key joined with a **dot**, and the schema itself contains dots.
///
/// Two readings were wrong here and both produced a *refusal* rather than an
/// error, which is the shape this project ranks worst:
///
/// * splitting on the first space and keeping the left half makes every key
///   `org.gnome.system.proxy`, and a lookup for `...http.host` matches nothing -
///   so `SCDynamicStoreCopyProxies` reported `no mode in the output` on a host
///   whose mode is `'none'`;
/// * keeping `schema` + `' '` + `key` as the name is the printed line, which is
///   not the dotted name either, and the same lookup misses.
///
/// So the schema prefix is matched on a **dot boundary** and the remainder is
/// split once: everything up to the first space is the schema, and the rest
/// begins with the key.
///
/// A line of another schema is skipped rather than guessed at, and a key this
/// module needs is caught by [`Settings::string`] and its siblings, which are
/// strict.
fn parse_list_recursively(output: &str, schema: &str) -> Settings {
    let mut values = Vec::new();
    for line in output.lines() {
        let Some((printed_schema, rest)) = line.split_once(' ') else {
            continue;
        };
        // A sibling schema - `org.gnome.system.proxy.https` against
        // `org.gnome.system.proxy` - must not be read as ours, so the prefix has
        // to end on a dot boundary rather than merely start with the text.
        let ours = printed_schema == schema
            || printed_schema
                .strip_prefix(schema)
                .is_some_and(|rest| rest.starts_with('.'));
        if !ours {
            continue;
        }
        // `rest` is `KEY VALUE`; a key has no space in it, so one split leaves
        // the value whole - a PAC URL and a bypass list both contain spaces,
        // and neither may be cut in half.
        let Some((key, value)) = rest.split_once(' ') else {
            continue;
        };
        values.push((
            format!("{printed_schema}.{key}"),
            value.trim().to_string(),
        ));
    }
    Settings { values }
}

/// GLib's textual GVariant syntax, for the shapes these keys have.
///
/// `Ok(None)` for a value that is *not* a quoted string - a number, a boolean -
/// which is a fact about the value and lets a caller tell it from a quoting it
/// failed to parse. `Err` for quoting it cannot read, because that is a bug or
/// an unexpected shape and must not become a silent absence.
fn unquote(text: &str) -> Result<Option<String>, String> {
    // An opening quote with no closing one is a shape this cannot read, and
    // saying `Ok(None)` for it would report a half-written value as "not a
    // string" - the silent absence this whole function exists to refuse.
    let open = text.starts_with('\'') || text.starts_with('"');
    let inner = text
        .strip_prefix('\'')
        .or_else(|| text.strip_prefix('"'))
        .and_then(|rest| rest.strip_suffix(['\'', '"']));
    let Some(inner) = inner else {
        return match open {
            true => Err(format!("{text:?} opens a quote and does not close it")),
            false => Ok(None),
        };
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some(other) => {
                return Err(format!("{text:?} has an escape this cannot read: \\{other}"));
            }
            None => return Err(format!("{text:?} ends in a backslash")),
        }
    }
    Ok(Some(out))
}

/// Say, on stderr, that a question has no answer here.
///
/// `write(2)` for the reason every other refusal in this tree gives: the guest
/// is about to be told something it cannot check, so the line has to be out
/// before the value is.
fn refusal(why: &str) {
    let line = format!("rosetta3: {why}");
    // SAFETY: a live buffer for the length written.
    unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
}

/// The derivation behind the answer, for `ROSETTA3_TRACE_PROXY=1`.
///
/// Additive and off by default, so the default run's output is unchanged and
/// the only thing that moves is the proxy decision - which moves because the
/// host was asked, not because a trace was turned on.
fn trace(topic: &'static str, line: &str) {
    static SAID: std::sync::Mutex<std::collections::BTreeSet<&'static str>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    if !tracing() {
        return;
    }
    // One-shot **per topic**, named by the caller rather than parsed out of the
    // text. An earlier version took the topic from the text, so a line whose
    // *subject* was a key name became its own topic and the read-back printed
    // one key and stopped.
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.insert(topic) {
        refusal(&format!("TRACE_PROXY: {line}\n"));
    }
}

/// A **group** of lines, printed once as a group.
///
/// A diagnostic that interleaves with its neighbour's is not a diagnostic: an
/// earlier version printed the whole seventeen-key read-back through one
/// `format!` with embedded newlines, and the result landed in the middle of the
/// next subsystem's line - `"...3 bypass rule(s)rosetta3: security: ..."` - two
/// subsystems' output fused into one unreadable line. So the group is built as
/// a `Vec`, gated by a single flag, and written a line at a time.
///
/// The flag is on the **group**, not the line, for the other reason: a
/// per-line flag prints the first line and stops, so a seventeen-key report
/// showed one key - which reads as "the other sixteen are fine" when it means
/// "the other sixteen were never asked".
fn trace_group(topic: &'static str, lines: &[String]) {
    static SAID: std::sync::Mutex<std::collections::BTreeSet<&'static str>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    if !tracing() {
        return;
    }
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if !said.insert(topic) {
        return;
    }
    // One `write` for the whole group, with the newlines in it: `write(2)` per
    // line would interleave with another thread's line between two of them,
    // and this tree has other subsystems writing to the same descriptor.
    let mut out = String::new();
    for line in lines {
        out.push_str("rosetta3: TRACE_PROXY:   ");
        out.push_str(line);
        out.push('\n');
    }
    // SAFETY: a live buffer for the length written.
    unsafe { libc::write(2, out.as_ptr().cast(), out.len()) };
}

/// Whether `ROSETTA3_TRACE_PROXY` is set, read once.
fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ROSETTA3_TRACE_PROXY").is_some())
}

/// An `SCDynamicStoreRef`.
///
/// A handle and nothing else: the store below answers from the host's own
/// interfaces rather than from a session, and the name and callback a guest
/// passes name a notification channel this tree does not deliver on. It is a
/// real object of this runtime's because the caller releases it with
/// `CFRelease`, which walks the isa.
#[repr(C)]
struct Store {
    isa: *const Class,
}

fn store_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let class = crate::base::register(c"SCDynamicStore", None, size_of::<Store>(), true);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a pointer to a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// `SCDynamicStoreRef SCDynamicStoreCreate(CFAllocatorRef, CFStringRef, SCDynamicStoreCallBack, SCDynamicStoreContext *)`
///
/// The store is created rather than declined, because a caller that gets NULL
/// here asks nothing further - and the keys below can be answered.
pub unsafe extern "C" fn SCDynamicStoreCreate(
    _allocator: CFTypeRef,
    _name: CFTypeRef,
    _callout: *const c_void,
    _context: *mut c_void,
) -> CFTypeRef {
    // SAFETY: `Store` begins with `isa` and is the layout `store_class` was
    // registered with.
    let store: *mut Store = unsafe { crate::base::allocate(store_class()) };
    store as CFTypeRef
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

/// The interface packets to the outside world leave by.
///
/// Asked of the routing table rather than guessed at, and asked without sending
/// anything: `connect` on a datagram socket picks the route and binds a local
/// address, which `getifaddrs` then names. `None` when nothing is routable,
/// which is the honest answer for a machine that is offline.
fn primary_interface() -> Option<String> {
    let local = routed_source_address()?;
    // SAFETY: `getifaddrs` fills in the pointer or returns non-zero.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut found = None;
        let mut entry = list;
        while !entry.is_null() {
            let address = (*entry).ifa_addr;
            if !address.is_null() && (*address).sa_family as i32 == libc::AF_INET {
                let address = &*(address as *const libc::sockaddr_in);
                if address.sin_addr.s_addr == local {
                    found = std::ffi::CStr::from_ptr((*entry).ifa_name)
                        .to_str()
                        .ok()
                        .map(str::to_string);
                    break;
                }
            }
            entry = (*entry).ifa_next;
        }
        libc::freeifaddrs(list);
        found
    }
}

/// The local IPv4 address a packet to the public internet would go out from, in
/// network order.
fn routed_source_address() -> Option<u32> {
    // SAFETY: every call below is checked, and the socket is closed on every
    // path out.
    unsafe {
        let socket = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if socket < 0 {
            return None;
        }
        // A public address that is not contacted: `connect` on a datagram
        // socket only consults the routing table.
        let mut to: libc::sockaddr_in = std::mem::zeroed();
        to.sin_family = libc::AF_INET as libc::sa_family_t;
        to.sin_port = 53u16.to_be();
        to.sin_addr.s_addr = u32::from_be_bytes([1, 1, 1, 1]).to_be();
        let mut local: libc::sockaddr_in = std::mem::zeroed();
        let mut len = size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let routed = libc::connect(
            socket,
            std::ptr::from_ref(&to).cast(),
            size_of::<libc::sockaddr_in>() as libc::socklen_t,
        ) == 0
            && libc::getsockname(socket, std::ptr::from_mut(&mut local).cast(), &mut len) == 0;
        libc::close(socket);
        routed.then_some(local.sin_addr.s_addr)
    }
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
];

/// The `kSC*` globals, as `(Mach-O name, address)`.
pub fn data_symbols() -> Vec<(String, u64)> {
    crate::bootstrap();
    KEY_STRINGS
        .iter()
        .chain(PROXY_KEYS)
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
        "SCDynamicStoreKeyCreateNetworkGlobalEntity" => SCDynamicStoreKeyCreateNetworkGlobalEntity,
        "SCDynamicStoreCopyValue" => SCDynamicStoreCopyValue,
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

    /// The text of an object of this runtime's, or `None`.
    fn spelling(value: CFTypeRef) -> Option<String> {
        // SAFETY: null or a live object of this runtime's.
        let bytes = unsafe { text_of(value) }?;
        String::from_utf8(bytes).ok()
    }

    /// The value out of a dictionary this module built, as a UTF-8 string.
    ///
    /// Goes through `CFDictionaryGetValue` with the key **out of
    /// [`PROXY_KEYS`]**, not out of a literal in the test. That is the whole
    /// point: the guest looks its key up with the constant, so a test that
    /// wrote the string itself would pass even if the constant were the wrong
    /// string, and a wrong key string is indistinguishable from an absent one.
    fn value_for(dictionary: CFTypeRef, symbol: &str) -> Option<String> {
        let key = key_of(symbol);
        assert!(!key.is_null(), "{symbol} is not one of PROXY_KEYS");
        // SAFETY: a dictionary this module built and a key out of its own table.
        let value = unsafe { crate::api::CFDictionaryGetValue(dictionary, key) };
        if value.is_null() {
            return None;
        }
        spelling(value)
    }

    /// The integer out of a dictionary this module built.
    fn int_for(dictionary: CFTypeRef, symbol: &str) -> Option<i64> {
        let key = key_of(symbol);
        // SAFETY: as `value_for`.
        let value = unsafe { crate::api::CFDictionaryGetValue(dictionary, key) };
        if value.is_null() {
            return None;
        }
        // SAFETY: the value is a number this module just made.
        unsafe { crate::collections::number(value) }.map(|n| n.as_i64())
    }

    /// A `gsettings list-recursively` transcript of a host with **no** proxy.
    ///
    /// Taken verbatim from this host, which is why it is a fixture rather than
    /// something invented: it is what the first source really printed, and the
    /// answer `SCDynamicStoreCopyProxies` gives today is derived from it.
    const NO_PROXY: &str = "\
org.gnome.system.proxy mode 'none'
org.gnome.system.proxy autoconfig-url ''
org.gnome.system.proxy ignore-hosts ['localhost', '127.0.0.0/8', '::1']
org.gnome.system.proxy use-same-proxy true
org.gnome.system.proxy.ftp host ''
org.gnome.system.proxy.ftp port 0
org.gnome.system.proxy.http authentication-password ''
org.gnome.system.proxy.http authentication-user ''
org.gnome.system.proxy.http enabled false
org.gnome.system.proxy.http host ''
org.gnome.system.proxy.http port 8080
org.gnome.system.proxy.http use-authentication false
org.gnome.system.proxy.https host ''
org.gnome.system.proxy.https port 0
org.gnome.system.proxy.socks host ''
org.gnome.system.proxy.socks port 0
";

    /// The same source on a host with a manual HTTP proxy, in the same format.
    const MANUAL_PROXY: &str = "\
org.gnome.system.proxy mode 'manual'
org.gnome.system.proxy autoconfig-url ''
org.gnome.system.proxy ignore-hosts ['localhost', '127.0.0.1']
org.gnome.system.proxy use-same-proxy true
org.gnome.system.proxy.ftp host ''
org.gnome.system.proxy.ftp port 0
org.gnome.system.proxy.http enabled true
org.gnome.system.proxy.http host 'proxy.example.invalid'
org.gnome.system.proxy.http port 3128
org.gnome.system.proxy.https host ''
org.gnome.system.proxy.https port 0
org.gnome.system.proxy.socks host ''
org.gnome.system.proxy.socks port 0
";

    /// Parse a `list-recursively` transcript the way [`gsettings_proxy`] does.
    ///
    /// The bodies of the two readers are the same code; what differs is where
    /// the text comes from, and that is the one thing a test has to substitute
    /// to be able to test anything at all. A `list-recursively` transcript is
    /// the *effective* configuration - stored values, or the schema default
    /// where nothing is stored - which is what "the system's proxy
    /// configuration" means, and it is why this fixture carries
    /// `http.port 8080` on a host with no proxy: that is GNOME's default and
    /// the desktop would ignore it.
    fn from_transcript(transcript: &str) -> Result<Option<HostProxy>, String> {
        parse_gsettings(parse_list_recursively(transcript, SCHEMA))
    }

    /// A `SCDynamicStoreCopyProxies` answer, built the way the function builds
    /// it, from a transcript.
    fn dictionary_from(transcript: &str) -> CFTypeRef {
        crate::bootstrap();
        let config = from_transcript(transcript)
            .expect("a readable transcript")
            .expect("an answer");
        dictionary_for(&config)
    }

    /// The transcript is read the way the live source reads it, so a test that
    /// passes here has exercised the real key spelling. This is the assertion
    /// that would have caught the qualified-key bug: the fixture is real output
    /// from this host, and the reader has to find `mode` in it.
    #[test]
    fn a_transcript_is_read_with_the_keys_the_output_actually_uses() {
        let values = parse_list_recursively(NO_PROXY, SCHEMA);
        assert_eq!(
            values.string("org.gnome.system.proxy.mode").as_deref(),
            Some("none"),
            "the key in the output is bare, so the lookup is schema + '.' + key"
        );
        assert_eq!(
            values.string("org.gnome.system.proxy.http.host").as_deref(),
            Some(""),
        );
        assert_eq!(values.number("org.gnome.system.proxy.http.port"), Some(8080));
        assert!(!values.boolean("org.gnome.system.proxy.http.enabled"));
        assert!(values.boolean("org.gnome.system.proxy.use-same-proxy"));
        assert_eq!(
            values.list("org.gnome.system.proxy.ignore-hosts"),
            ["localhost", "127.0.0.0/8", "::1"]
        );
        // A key that is genuinely absent is absent, which is a different
        // statement from a key whose value is empty.
        assert_eq!(values.string("org.gnome.system.proxy.https.enabled"), None);
    }

    /// **The positive control this module's whole claim rests on:** a host that
    /// *has* a proxy is given the proxy, with the host and the port the host
    /// really has.
    ///
    /// The negative case - this host, no proxy - is the one the inherited code
    /// already got "right", which is exactly why it proved nothing. A test that
    /// only ever sees a host with no proxy cannot tell an answer from a
    /// constant, so this one is the whole of the evidence.
    #[test]
    fn a_configured_proxy_reaches_the_guest_as_the_host_configured_it() {
        let dictionary = dictionary_from(MANUAL_PROXY);
        // SAFETY: a dictionary this module built.
        unsafe {
            assert_eq!(
                int_for(dictionary, "_kSCPropNetProxiesHTTPEnable"),
                Some(1),
                "the host says HTTP is proxied, and the guest is told so"
            );
            assert_eq!(
                value_for(dictionary, "_kSCPropNetProxiesHTTPProxy"),
                Some("proxy.example.invalid".to_string()),
                "the proxy host is the host's, not a placeholder"
            );
            assert_eq!(
                int_for(dictionary, "_kSCPropNetProxiesHTTPPort"),
                Some(3128),
                "the proxy port is the host's"
            );
            // `use-same-proxy` is true in this transcript, so the other schemes
            // follow the HTTP one - which is GNOME's own rule and is why they
            // are on rather than absent.
            assert_eq!(int_for(dictionary, "_kSCPropNetProxiesHTTPSEnable"), Some(1));
            assert_eq!(
                value_for(dictionary, "_kSCPropNetProxiesHTTPSProxy"),
                Some("proxy.example.invalid".to_string())
            );
            crate::api::CFRelease(dictionary);
        }
    }

    /// The bypass list is a `CFArray[CFString]`, and it holds every entry rather
    /// than the first. `kSCPropNetProxiesExceptionsList` is
    /// `CFArray[CFString]` in Apple's table, and a one-element list built from
    /// a two-element list is the kind of quiet truncation that looks like a
    /// working proxy configuration.
    #[test]
    fn the_bypass_list_is_an_array_of_every_host_name() {
        crate::bootstrap();
        let dictionary = dictionary_from(MANUAL_PROXY);
        // SAFETY: a dictionary this module built.
        let list = unsafe {
            let key = key_of("_kSCPropNetProxiesExceptionsList");
            let value = crate::api::CFDictionaryGetValue(dictionary, key);
            assert!(!value.is_null(), "the host has two bypass rules");
            value
        };
        // SAFETY: a live array of this runtime's.
        let count = unsafe { crate::api::CFArrayGetCount(list) };
        assert_eq!(count, 2, "both of the host's bypass rules are present");
        // SAFETY: a live array, and an index below its count.
        let mut names = Vec::new();
        for index in 0..count {
            let item = unsafe { crate::api::CFArrayGetValueAtIndex(list, index) };
            // SAFETY: an element of a live array of strings.
            names.push(spelling(item).expect("a string element"));
        }
        assert_eq!(names, ["localhost", "127.0.0.1"]);
        // SAFETY: a dictionary and an array this module just made.
        unsafe { crate::api::CFRelease(dictionary) };
    }

    /// **This host**, as it is: no proxy anywhere. The answer is a dictionary
    /// that says so in Apple's own vocabulary, rather than a NULL that says it
    /// by being absent - and the enable flags are `0`, not missing, because
    /// "off" and "never heard of it" are different facts.
    #[test]
    fn a_host_with_no_proxy_is_told_every_scheme_is_off() {
        let dictionary = dictionary_from(NO_PROXY);
        // SAFETY: a dictionary this module built.
        unsafe {
            for symbol in [
                "_kSCPropNetProxiesHTTPEnable",
                "_kSCPropNetProxiesHTTPSEnable",
                "_kSCPropNetProxiesSOCKSEnable",
                "_kSCPropNetProxiesFTPEnable",
                "_kSCPropNetProxiesProxyAutoConfigEnable",
                "_kSCPropNetProxiesProxyAutoDiscoveryEnable",
                "_kSCPropNetProxiesExcludeSimpleHostnames",
            ] {
                assert_eq!(
                    int_for(dictionary, symbol),
                    Some(0),
                    "{symbol} is off on a host with no proxy, and says so"
                );
            }
            // A host with no proxy has no proxy host, so no host key is
            // published: an empty `HTTPProxy` string would be a value nobody
            // measured, and it would read as "the proxy is the empty string".
            for symbol in [
                "_kSCPropNetProxiesHTTPProxy",
                "_kSCPropNetProxiesHTTPSProxy",
                "_kSCPropNetProxiesSOCKSProxy",
                "_kSCPropNetProxiesFTPProxy",
                "_kSCPropNetProxiesProxyAutoConfigURLString",
            ] {
                assert_eq!(
                    value_for(dictionary, symbol),
                    None,
                    "{symbol} is absent rather than empty"
                );
            }
            // ... including the ports, even though GNOME's default for HTTP is
            // 8080 and the transcript carries it. `mode` is what decides
            // whether a port means anything, and it is 'none'.
            assert_eq!(int_for(dictionary, "_kSCPropNetProxiesHTTPPort"), None);
            crate::api::CFRelease(dictionary);
        }
    }

    /// The live host, read through the real function.
    ///
    /// Not a fixture: this asks the actual sources on the actual machine and
    /// checks that the answer is one of the two legal shapes and that the
    /// traceable derivation agrees with the dictionary. It cannot assert a
    /// particular value - the host's configuration is not this test's to
    /// choose - so what it pins is the *invariant*: a host with nothing
    /// configured gets a dictionary that says every scheme is off, and a host
    /// with something configured gets that something in the value.
    #[test]
    fn the_live_host_gets_a_dictionary_that_matches_what_was_read() {
        crate::bootstrap();
        // SAFETY: a null store, which is what Chromium passes.
        let dictionary = unsafe { SCDynamicStoreCopyProxies(std::ptr::null()) };
        let answer = host_proxy();
        if let Some(why) = &answer.error {
            // Named, so a NULL here is the documented error case and not an
            // absent configuration. Nothing to assert beyond that.
            assert!(
                dictionary.is_null(),
                "an error was named ({why}) and the answer is Apple's own for one"
            );
            return;
        }
        let Some(answered) = &answer.proxy else {
            assert!(dictionary.is_null(), "no source on this host, and no answer");
            return;
        };
        assert!(!dictionary.is_null(), "a source answered, so there is a value");
        // SAFETY: a dictionary this module built, and strings out of it.
        let flags: Vec<Option<i64>> = [
            "_kSCPropNetProxiesHTTPEnable",
            "_kSCPropNetProxiesHTTPSEnable",
            "_kSCPropNetProxiesSOCKSEnable",
            "_kSCPropNetProxiesFTPEnable",
        ]
        .into_iter()
        .map(|s| int_for(dictionary, s))
        .collect();
        if !answered.proxy.proxies_anything() {
            // The host has nothing configured, so every flag must read 0 -
            // the whole claim is that the flags follow the host rather than a
            // constant.
            assert_eq!(
                flags,
                vec![Some(0), Some(0), Some(0), Some(0)],
                "this host has nothing proxied ({}) and the dictionary says so",
                describe(&answered.proxy)
            );
        } else {
            assert!(
                flags.contains(&Some(1)),
                "the host proxies something and at least one flag says so"
            );
        }
        // SAFETY: a dictionary this module built.
        unsafe { crate::api::CFRelease(dictionary) };
    }

    /// **The census diff**, in the only form that counts: every key is
    /// **published** - it reaches `data_symbols()`, which is the function the
    /// loader reads - and every one is at a **live address in this process**.
    ///
    /// The second half is the one that catches the failure this whole change
    /// could plausibly have made. `exports!`'s `data` section and
    /// `base::global_cell` are different mechanisms, and a row published
    /// through the wrong one is a *value* where the guest expects the address
    /// of storage: the number reaches the slot, the slot holds the number, and
    /// the guest's next instruction — `mov rsi, [rax]` — dereferences the
    /// number. A test that only checked publication would pass with a key
    /// string's characters sitting in the slot.
    ///
    /// And the value at each address has to be the key object itself, which is
    /// what a guest's two-instruction read (`mov rax,[rip+slot]; mov rsi,[rax]`)
    /// actually retrieves.
    #[test]
    fn every_proxy_key_is_published_at_live_storage_holding_the_key() {
        crate::bootstrap();
        let published: std::collections::BTreeMap<String, u64> =
            super::data_symbols().into_iter().collect();
        let live = |addr: u64| {
            let addr = addr as usize;
            std::fs::read_to_string("/proc/self/maps")
                .unwrap_or_default()
                .lines()
                .filter_map(|line| {
                    let (lo, hi) = line.split_whitespace().next()?.split_once('-')?;
                    Some((
                        usize::from_str_radix(lo, 16).ok()?,
                        usize::from_str_radix(hi, 16).ok()?,
                    ))
                })
                .any(|(lo, hi)| addr >= lo && addr < hi)
        };
        for (symbol, _) in PROXY_KEYS {
            let addr = published
                .get(*symbol)
                .unwrap_or_else(|| panic!("{symbol} is not published at all"));
            assert!(live(*addr), "{symbol} at {addr:#x} is not this process's storage");
            // SAFETY: the address came from `data_symbols`, which publishes the
            // address of a `static` that outlives the process.
            let held = unsafe { std::ptr::read_volatile(*addr as *const u64) };
            assert_ne!(held, 0, "{symbol} publishes a null cell");
            assert_eq!(
                key_of(symbol) as u64, held,
                "{symbol}: the published cell does not hold its own key, so the \
                 guest reads one string and looks up another"
            );
        }
    }

    /// Every key [`PROXY_KEYS`] names is a real string, and it is the string
    /// Apple's table gives rather than the symbol's own tail.
    ///
    /// The second half is the load-bearing half. "The value is the name minus
    /// the prefix" is true of all seventeen *and* it is a coincidence: the same
    /// rule predicts `HTTPSEnabled`, and Apple's table says `HTTPSEnable`. A
    /// test that only checked the coincidence would pass on both spellings, so
    /// the expected strings are written out here rather than computed.
    #[test]
    fn the_proxy_keys_are_apple_s_spelling_and_not_the_symbol_s_tail() {
        crate::bootstrap();
        assert_eq!(PROXY_KEYS.len(), 17, "the 17 the Framework imports");
        for (symbol, text) in PROXY_KEYS {
            assert!(!key_of(symbol).is_null(), "{symbol} has no key");
            let spelled = spelling(key_of(symbol)).expect("a string");
            assert_eq!(spelled, *text, "{symbol}");
            // The trap this table exists to avoid, stated as a trap: the
            // rule-of-thumb answer and the transcribed answer differ for HTTPS.
            if *symbol == "_kSCPropNetProxiesHTTPSEnable" {
                assert_eq!(spelled, "HTTPSEnable");
                assert_ne!(spelled, "HTTPSEnabled");
            }
        }
    }

    /// GLib's textual GVariant syntax, for the shapes these keys have, and
    /// nothing else. A shape that is not recognised is an error rather than a
    /// guess, because a value this cannot read must not become an absent one.
    #[test]
    fn a_gvariant_value_is_read_or_refused_and_never_guessed() {
        assert_eq!(unquote("'none'").unwrap().as_deref(), Some("none"));
        assert_eq!(unquote("''").unwrap().as_deref(), Some(""));
        assert_eq!(unquote("'::1'").unwrap().as_deref(), Some("::1"));
        assert_eq!(
            unquote("'proxy.example.invalid'").unwrap().as_deref(),
            Some("proxy.example.invalid")
        );
        assert_eq!(unquote("'a\\'b'").unwrap().as_deref(), Some("a'b"));
        assert_eq!(unquote("'a\\\\b'").unwrap().as_deref(), Some("a\\b"));
        // Not a string: a fact about the value, and `None` says so.
        assert_eq!(unquote("8080").unwrap(), None);
        assert_eq!(unquote("true").unwrap(), None);
        // A quoting this cannot read is an error, never a `None`.
        assert!(unquote("'a\\qb'").is_err());
        assert!(unquote("'unterminated").is_err());
    }

    /// A `*_proxy` variable is read as `scheme://host:port`, which is the only
    /// spelling that carries a port, and `all_proxy` fills the schemes nothing
    /// else named.
    #[test]
    fn a_proxy_variable_is_read_with_its_port_and_its_scheme() {
        let vars: std::collections::BTreeMap<&str, String> = [
            ("http_proxy", "http://proxy.example.invalid:3128"),
            ("https_proxy", "https://secure.example.invalid:8443"),
            ("socks_proxy", "socks5://127.0.0.1:1080"),
            ("no_proxy", "localhost, 127.0.0.1 ,"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect();
        let config = parse_proxy_variables(&|name: &str| vars.get(name).cloned())
            .expect("an environment with proxies in it");
        assert_eq!(config.http.host.as_deref(), Some("proxy.example.invalid"));
        assert_eq!(config.http.port, Some(3128));
        assert_eq!(config.https.host.as_deref(), Some("secure.example.invalid"));
        assert_eq!(config.https.port, Some(8443));
        assert_eq!(config.socks.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(config.socks.port, Some(1080));
        // A trailing empty item is dropped rather than becoming a bypass rule
        // for the empty string, which would bypass everything.
        assert_eq!(config.exceptions, ["localhost", "127.0.0.1"]);
        // Nothing named FTP and `all_proxy` is unset, so FTP has no opinion.
        assert!(!config.ftp.has_an_opinion());
    }

    /// `all_proxy` is the environment's own `use-same-proxy`.
    #[test]
    fn all_proxy_fills_the_schemes_nothing_else_named() {
        let vars: std::collections::BTreeMap<&str, String> = [
            ("ALL_PROXY", "socks5://127.0.0.1:9050"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect();
        let config = parse_proxy_variables(&|name: &str| vars.get(name).cloned())
            .expect("an environment with all_proxy in it");
        for endpoint in [&config.http, &config.https, &config.socks, &config.ftp] {
            assert_eq!(endpoint.host.as_deref(), Some("127.0.0.1"));
            assert_eq!(endpoint.port, Some(9050));
            assert_eq!(endpoint.enabled, Some(true));
        }
    }

    /// An environment with no proxy variables in it is silence, not "off", so
    /// it does not answer - which is what lets a later source be asked.
    #[test]
    fn an_environment_with_nothing_in_it_does_not_answer() {
        assert_eq!(parse_proxy_variables(&|_| None), None);
    }

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
}
