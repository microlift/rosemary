//! `CWWiFiClient` and `CWInterface`: the two classes, and the five selectors.
//!
//! Five selectors, and every one of them is a question this host can answer:
//!
//! | selector | answer | where it comes from |
//! |---|---|---|
//! | `+[CWWiFiClient sharedWiFiClient]` | the one client, non-nil | see below |
//! | `-[CWWiFiClient interface]` | the default interface, or nil | [`crate::radio`] |
//! | `-[CWWiFiClient interfaces]` | an `NSArray` of them | [`crate::radio`] |
//! | `-[CWInterface powerOn]` | measured | [`crate::radio`] |
//! | `-[CWInterface interfaceName]` | the kernel's own name | [`crate::radio`] |
//!
//! Everything else CoreWLAN answers is **not installed**, and that is the
//! deliverable as much as these five are. An absent selector on a published
//! class is `objc`'s own refusal by name - `unrecognized selector sent to
//! instance of CWInterface: scanForNetworksWithName:error:` - which says what
//! is missing and who wants it. See the crate note for the full refused list and
//! why each one has no truthful value here.

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;
use std::ffi::{c_char, CStr};

use crate::radio;

/// Install both classes and their methods. Idempotent.
pub fn install(client: &'static Class, interface: &'static Class) {
    use crate::{class_methods, methods};

    // `+[CWWiFiClient sharedWiFiClient]`: zero colons, so zero arguments, and
    // the encoding is one return word. The same shape
    // `+[ODSession defaultSession]` and `+[NSFileManager defaultManager]` use.
    class_methods!(client, {
        "sharedWiFiClient" => shared_wifi_client, c"@16@0:8";
    });
    methods!(client, {
        "interface" => default_interface, c"@16@0:8";
        "interfaces" => all_interfaces, c"@16@0:8";
    });
    methods!(interface, {
        "powerOn" => power_on, c"c16@0:8";
        "interfaceName" => interface_name, c"@16@0:8";
    });
}

/// `-[CWInterface]`'s payload: the `isa` the runtime insists on, and the
/// interface's own name.
///
/// The name is a `*const c_char` leaked once per interface, because an
/// interface's name is a fact about the machine that does not change while the
/// process runs, and the object has to be able to answer for itself rather than
/// re-globbing sysfs on every `-interfaceName`.
#[repr(C)]
pub(crate) struct Body {
    isa: *const Class,
    name: *const c_char,
}

/// The one `CWWiFiClient`, made on first ask and kept for the life of the
/// process.
///
/// **Non-nil, and the reason is the point of this crate.** `+[ODSession
/// defaultSession]` answers nil here because the service it is a handle *on* -
/// `directoryd` - is not on this machine, and a handle to nothing is the
/// truthful answer. A `CWWiFiClient` is the same kind of handle, onto a Wi-Fi
/// radio, and this machine **has** one: `wlp0s20f3`, up, associated, and not
/// blocked by rfkill. So the handle is real and the client is real, and every
/// answer it gives is a reading of this host.
///
/// A nil here would also be the answer that *looks* right. It would make
/// `SystemGeolocationSourceApple::IsWifiEnabled()` return false, which reads as
/// "no radio, nothing to report" - and it would be a claim about this machine
/// that one `ls /sys/class/net` disproves.
unsafe extern "C" fn shared_wifi_client(class: Id, _sel: Sel) -> Id {
    static CLIENT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CLIENT.get_or_init(|| {
        let interfaces = radio::interfaces();
        let powered = radio::radio_on(&interfaces);
        radio::announce(&interfaces, powered);
        // SAFETY: `class` is `CWWiFiClient`, registered by `bootstrap()`.
        let client = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const Class).expect("a class"),
                0,
            )
        };
        // A singleton a guest can release is one it can make dangle by
        // releasing what it never owned - the same reason
        // `+[NSFileManager defaultManager]` retains.
        objc::arc::retain(client);
        client as usize
    }) as Id
}

/// `-[CWWiFiClient interface]` - the default interface, or nil if there is none.
///
/// The guest's own nil branch is the source, not a hope:
///
/// ```text
/// 83e6852  mov  rsi,[rip+0x6302e5f]   # sharedWiFiClient
/// 83e6860  call r14                   ; objc_msgSend
/// 83e686e  mov  rsi,[rip+0x6305cdb]   # interface
/// 83e6878  call r14
/// 83e6886  test rax,rax                ; <-- on the *interface*
/// 83e6889  je   0x83e68a3              ; <-- -> xor r15d,r15d
/// 83e68a3  xor  r15d,r15d              ;    the answer is NO
/// ```
///
/// which is `return (interface && interface.powerOn);` -
/// `services/device/public/cpp/geolocation/system_geolocation_source_apple.mm`
/// line 31, fetched at tag `130.0.6723.91`. The same shape is at `0x83e7709`.
unsafe extern "C" fn default_interface(_client: Id, _sel: Sel) -> Id {
    let interfaces = radio::interfaces();
    // The first in kernel order, which is what "the" interface means when a
    // machine has one. Two interfaces would make "default" a question this
    // platform cannot answer, and the count is printed by `announce` so a run
    // with more than one says so.
    match interfaces.first() {
        Some(interface) => object_for(interface),
        None => std::ptr::null_mut(),
    }
}

/// `-[CWWiFiClient interfaces]` - every interface, as an `NSArray`.
///
/// An **empty** array would be the convenient answer and it is a lie: it says
/// "I looked at this machine's Wi-Fi interfaces and there are none", and there
/// is one. It is also the answer that would be *invisible*: the caller
/// (`services/device/geolocation/wifi_data_provider_mac.mm`,
/// `CoreWlanApi::GetAccessPointData`) counts its own failures and returns
/// `interface_error_count == 0`, so an empty array returns **true** - "the scan
/// worked" - and the geolocation provider then sends a network-location request
/// carrying no access points and gets a worse fix, with nothing anywhere
/// reporting that the scan never happened.
///
/// So the array is the truth, and the first selector the guest sends to what is
/// in it is `-[CWInterface scanForNetworksWithName:error:]` - which this crate
/// does not implement, because CoreWLAN answers it from the airport kext. That
/// refusal is the correct outcome and it is the next thing to be said out loud.
unsafe extern "C" fn all_interfaces(_client: Id, _sel: Sel) -> Id {
    let interfaces = radio::interfaces();
    let values: Vec<corefoundation::base::CFTypeRef> = interfaces
        .iter()
        .map(|interface| object_for(interface) as corefoundation::base::CFTypeRef)
        .collect();
    let array = corefoundation::collections::new_array(
        &values,
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    objc::arc::autorelease(array as Id)
}

/// The `CWInterface` object for one of this host's interfaces, made once.
///
/// Keyed by the interface name rather than cached blindly, so an interface that
/// disappears from sysfs stops being answered instead of being answered from a
/// stale reading. An entry for an interface that has gone is dropped and this
/// crate's retain released, which is ordinary ARC: a guest still holding one
/// keeps it alive, and the table cannot grow without bound on a host whose
/// interfaces are renamed.
fn object_for(interface: &radio::Interface) -> Id {
    use std::sync::Mutex;
    static MADE: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some((_, object)) = made.iter().find(|(name, _)| *name == interface.name) {
        return *object as Id;
    }
    let class = match objc::class::lookup(c"CWInterface") {
        Some(class) => class,
        None => return std::ptr::null_mut(),
    };
    // Leaked once per interface name, and the table below keeps the object for
    // as long as the interface exists, so the pointer is live for as long as it
    // is read.
    let name = std::ffi::CString::new(interface.name.as_str())
        .unwrap_or_else(|_| std::ffi::CString::new("").expect("no interior nul"));
    let name = name.into_raw();
    // SAFETY: `Body` begins with an `isa` and `CWInterface` is registered with
    // `size_of::<Body>()` bytes - see `bootstrap()`.
    let object: *mut Body = unsafe { corefoundation::base::allocate(class) };
    if object.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a newly allocated instance whose payload is ours to write.
    unsafe {
        (*object).name = name;
        objc::arc::retain(object as Id);
    }
    // SAFETY: `name` was just leaked and is stored in the object above, so the
    // only copy left is the one this table owns through the object.
    let live: Vec<String> = radio::interfaces()
        .into_iter()
        .map(|found| found.name)
        .chain(std::iter::once(interface.name.clone()))
        .collect();
    let mut dropped = Vec::new();
    made.retain(|(held, object)| {
        if live.iter().any(|name| name == held) {
            return true;
        }
        dropped.push(*object as Id);
        false
    });
    for object in dropped {
        objc::arc::release(object);
    }
    made.push((interface.name.clone(), object as usize));
    object as Id
}

/// `-[CWInterface powerOn]` - measured, never assumed.
///
/// The only selector here whose answer the guest *branches* on for something
/// other than a nil check: `was_wifi_enabled_` is read at `0x83e6046`
/// (`cmp BYTE PTR [rax+0xa0],0x1`) to choose which `CLError` it reports to the
/// renderer. So the value is load-bearing, and the honest thing is to read the
/// radio rather than to pick the branch that is convenient.
unsafe extern "C" fn power_on(interface: Id, _sel: Sel) -> u8 {
    // SAFETY: `name_of` reads the receiver's own payload and checks its class
    // first; `interface` is whatever the runtime passed, null included.
    match unsafe { name_of(interface) } {
        Some(name) => radio::interfaces()
            .iter()
            .find(|candidate| candidate.name == name)
            .is_some_and(|candidate| candidate.powered) as u8,
        // An object this crate did not make. Saying `NO` would be a reading of
        // nothing, so refuse by name instead of answering.
        None => refuse(
            "-[CWInterface powerOn]: the receiver is not a CWInterface of this host, so there is \
             no radio to report on. The five selectors this crate answers are \
             +[CWWiFiClient sharedWiFiClient], -[CWWiFiClient interface], -[CWWiFiClient \
             interfaces], -[CWInterface powerOn] and -[CWInterface interfaceName]",
        ),
    }
}

/// `-[CWInterface interfaceName]` - the kernel's own name for it.
///
/// Not spelled like a Mac's `en0`, and not spelled like one on purpose: the
/// interface on this machine is called `wlp0s20f3`, and renaming it would make
/// every name-matching caller wrong in a way that looks like a working one. The
/// guest sends this selector exactly once, at `0xbbde8c4`, and compares the
/// answer against a name it already holds - so a renamed interface would make
/// that comparison fail for a reason nobody could see.
unsafe extern "C" fn interface_name(interface: Id, _sel: Sel) -> Id {
    // SAFETY: as in `power_on` above.
    match unsafe { name_of(interface) } {
        Some(name) => foundation::string::from_str(&name),
        None => refuse(
            "-[CWInterface interfaceName]: the receiver is not a CWInterface of this host, so it \
             has no name to give",
        ),
    }
}

/// Refuse, by name, and never come back.
///
/// `objc::fail!` prints and aborts, so this has no return path at all - which is
/// the point. There is no value to fall back on here: the alternative to naming
/// the problem is answering a question about an object this crate knows nothing
/// about.
fn refuse(reason: &str) -> ! {
    objc::fail!("{reason}")
}

/// The interface name an object stands for, read out of the object itself.
unsafe fn name_of(interface: Id) -> Option<String> {
    if interface.is_null() {
        return None;
    }
    // SAFETY: a non-null receiver. A `CWInterface` is this crate's `Body`; the
    // class check below is what makes reading the second field sound for
    // anything else.
    let Some(class) = (unsafe { objc::object::class_of(interface) }) else {
        return None;
    };
    let Some(ours) = objc::class::lookup(c"CWInterface") else {
        return None;
    };
    if !std::ptr::eq(class, ours) {
        return None;
    }
    // SAFETY: the object is a `Body` this crate allocated and never resized.
    let name = unsafe { (*(interface as *const Body)).name };
    if name.is_null() {
        return None;
    }
    // SAFETY: a `CString` this crate leaked and has not freed.
    Some(unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned())
}
