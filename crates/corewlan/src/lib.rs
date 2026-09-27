//! The three selectors of CoreWLAN that a geolocation provider reads a Wi-Fi
//! radio through, answered from this host's own radio.
//!
//! # The decision, and the evidence for it
//!
//! **Five selectors are answered, all five from a live reading of this host's
//! Wi-Fi hardware. The other twelve the guest sends - fourteen class/selector
//! pairs across three classes - are refused by name, and the most important of
//! them is `bssid` - a MAC address - which is not answered, not approximated,
//! and not made up.**
//!
//! # What the guest imports, which is the whole entry surface
//!
//! One symbol. `ROSETTA3_UNBOUND_DETAIL=all` names it exactly:
//!
//! ```text
//! _OBJC_CLASS_$_CWWiFiClient from /System/Library/Frameworks/CoreWLAN.framework/Versions/A/CoreWLAN
//! ```
//!
//! **Zero CoreWLAN function imports.** Every answer the guest wants from this
//! framework arrives through `objc_msgSend`, so the class and its selectors *are*
//! the surface - the same conclusion `frameworks.rs` records for OpenDirectory.
//! `CWInterface` and `CWNetwork` are never named by the guest at all: the
//! headers give them typed pointers, so they are reached as `id` and appear in
//! no `__objc_classrefs` slot. That is why one published class is enough.
//!
//! # The wall: three selectors deep, and it stops at a `BOOL`
//!
//! `+[CWWiFiClient sharedWiFiClient]` is sent from **five** places
//! (`objcenc_sites.py`: `0x3cd6e3b`, `0x83e6852`, `0x83e76dc`, `0x8f1173c`,
//! `0xbbde7c7`) and that is every way into CoreWLAN. From the site that is the
//! current wall - `0x83e6852` - the guest walks exactly two more selectors and
//! consumes a `BOOL`:
//!
//! ```text
//! 83e6840  push rbp / mov rbp,rsp / push r15 / push r14 / push r12 / push rbx   ; the function
//! 83e684b  mov  rdi,[rip+0x6300c56]   # 0xe6e74a8   _OBJC_CLASS_$_CWWiFiClient
//! 83e6852  mov  rsi,[rip+0x6302e5f]   # 0xe6e96b8   +sharedWiFiClient
//! 83e6860  call r14                                                  ; objc_msgSend
//! 83e686e  mov  rsi,[rip+0x6305cdb]   # 0xe6ec550   -interface
//! 83e6878  call r14
//! 83e6886  test rax,rax                ; <-- the INTERFACE, not the client
//! 83e6889  je   0x83e68a3
//! 83e688b  mov  rsi,[rip+0x6305cc6]   # 0xe6ec558   -powerOn
//! 83e6895  call QWORD PTR [rip+0x596979d]
//! 83e689b  test al,al
//! 83e689d  setne r15b
//! 83e68a3  xor  r15d,r15d              ; nil interface -> NO
//! ```
//!
//! which is, line for line:
//!
//! ```objc
//! CWWiFiClient* wifi_client = [CWWiFiClient sharedWiFiClient];
//! CWInterface* interface = wifi_client.interface;
//! return (interface && interface.powerOn);
//! ```
//!
//! `services/device/public/cpp/geolocation/system_geolocation_source_apple.mm`
//! lines 29-31, fetched at tag `130.0.6723.91`. **No SSID. No MAC address. No
//! channel, no rate, no noise, no country code** - the chain a few hundred bytes
//! from the network scan terminates in one `BOOL`.
//!
//! Two corrections to the brief this crate answers, both from the bytes:
//!
//! - The enclosing function is **`+0x83e6840`**, not `+0x83e67c0`. It opens
//!   with a prologue at `0x83e6840` and closes with `ret` at `0x83e68c4`.
//!   `0x83e67c0` is a *different* function - a destructor that releases a list,
//!   destroys a mutex at `0x83e6824` and tail-jumps to `operator delete` at
//!   `0x83e6836`. PLAN §7 records `macho.py`'s function starts as unreliable
//!   and this is one of them.
//! - The CWWiFiClient site is **not inside** the constructor at `+0x83e68d0`.
//!   The two are neighbours: the constructor begins twelve bytes after that
//!   `ret`, and builds the `CLLocationManager` at `+0x83e6b47`
//!   (`+[CLLocationManager initWithManager:]` at `0x83e6b20`, `setDelegate:` at
//!   `0x83e6b47` - another agent's wall). This function's caller is
//!   `+0x83e5ab0`, and it is reached from the *error* path, which is why
//!   CoreWLAN sits behind `CLLocationManager` rather than beside it.
//!
//! # What the `BOOL` is used for, so that the answer matters
//!
//! `IsWifiEnabled()` is stored as `was_wifi_enabled_` (source line 97) and read
//! back when a position error is reported:
//!
//! ```text
//! 83e6046  cmp  BYTE PTR [rax+0xa0],0x1   ; was_wifi_enabled_
//! 83e604d  jne  0x83e607a
//! 83e6068  call 0x83e6840                 ; IsWifiEnabled(), re-asked
//! 83e6074  je   0x83e6186                 ; radio off -> a different path
//! 83e607a  mov  DWORD PTR [rbp-0x60],ecx  ; -> {3, "Position update is
//! 83e607d  lea  rsi,[rip+0x496be9c]       ;    unavailable",
//! 83e6084  mov  edx,0x1e                 ;    30 bytes]
//! 83e6096  lea  rsi,[rip+0x56614ea]       ;    "CoreLocationProvider: CoreLocation
//! 83e609d  mov  edx,0x58                 ;    framework reported a
//!                                         ;    kCLErrorLocationUnknown failure."}
//! ```
//!
//! So `powerOn` chooses **which failure the renderer is told about**, and both
//! branches are truthful strings. Getting it wrong does not crash and does not
//! corrupt anything - it mislabels a geolocation error. That is still a wrong
//! answer, and it is the reason this crate reads the radio instead of choosing a
//! branch.
//!
//! # The measurement, and why it is the truthful one
//!
//! This machine has a Wi-Fi radio. Measured, not assumed:
//!
//! | what | reading |
//! |---|---|
//! | `/sys/class/net` | `enp67s0`, `lo`, `wlp0s20f3` |
//! | `wlp0s20f3/wireless` | present - it has a wireless PHY |
//! | `wlp0s20f3` `operstate` / `carrier` | `up` / `1` |
//! | `rfkill0` (`acer-wireless`, `wlan`) | `soft=0 hard=0` |
//! | `rfkill3` (`phy0`, `wlan`) | `soft=0 hard=0` |
//! | `lspci` | `Intel 700 Series Chipset Family CNVi Wi-Fi` |
//! | `nmcli` | `wlp0s20f3: connected` |
//!
//! So `-[CWInterface powerOn]` answers **`YES`**, and
//! `+[CWWiFiClient sharedWiFiClient]` answers **non-nil**.
//!
//! That is the decision that could have gone the other way, so here is the
//! argument for it in full. The question is *not* "does this host have Wi-Fi" -
//! it does - and it is not "is there a CoreWLAN interface object here", which is
//! circular, because the object is what this crate is being asked to create. The
//! question is what the object's **content** should claim, and the only claim
//! available is a claim about the machine. On this machine the Wi-Fi radio is
//! on. So `YES`.
//!
//! The tempting alternative - `-[CWWiFiClient interface]` answering nil, which
//! `SystemGeolocationSourceApple`'s own `interface &&` is written to tolerate -
//! is a **false statement about this host**, checkable with one `ls
//! /sys/class/net`, and it is the *dangerous* kind of wrong here: it does not
//! crash, it does not mislabel a MAC, and it makes the geolocation provider take
//! its "the radio is off" branch, which reads as a working answer. It is
//! refused for the same reason `+[ODSession defaultSession]` is answered nil -
//! and the difference is the whole of this crate: `directoryd` is *absent*, so
//! a handle to it would be a handle to nothing; a Wi-Fi radio is *present*, so a
//! handle to it is a handle to something.
//!
//! `-[CWInterface interfaceName]` answers `wlp0s20f3`, which is **not** spelled
//! like a Mac's `en0` and must not be: the guest sends this selector exactly
//! once, at `0xbbde8c4`, and compares the answer with `isEqualToString:` against
//! a name it already holds. A renamed interface would make that comparison fail
//! for a reason no log line would ever show.
//!
//! # The twelve selectors that are refused, and the MAC address
//!
//! The census of what the guest sends past `+sharedWiFiClient`, from
//! `test/tools/objcenc_sites.py --detail`, with the receivers read out of the
//! disassembly and cross-checked against the three source files:
//!
//! | selector | sites | receiver | CoreWLAN answers it from | here |
//! |---|---:|---|---|---|
//! | `scanForNetworksWithName:error:` | 3 | `CWInterface` | the airport kext | **refused** |
//! | `bssid` | 4 | `CWInterface` ×3, `CWNetwork` ×1 | **the network's MAC** | **refused** |
//! | `ssid` | 7 | `CWInterface` ×4, `CWNetwork` ×2 | the network's name | **refused** |
//! | `wlanChannel` | 2 | `CWNetwork` -> `CWChannel` | the kext | **refused** |
//! | `channelNumber` | 1 | `CWChannel` | the kext | **refused** |
//! | `channelBand` | 1 | `CWChannel` | the kext | **refused** |
//! | `rssiValue` | 1 | `CWNetwork` | the kext | **refused** |
//! | `noiseMeasurement` | 1 | `CWNetwork` | the kext | **refused** |
//! | `supportsSecurity:` | 1 | `CWNetwork` | the kext | **refused** |
//! | `setPower:error:` | 2 | `CWInterface` | the kext | **refused** |
//! | `setPairwiseMasterKey:error:` | 3 | `CWInterface` | 802.11 auth | **refused** |
//! | `associateToNetwork:password:error:` | 1 | `CWInterface` | the kext | **refused** |
//! | `anyObject` | 1 | `NSSet` | Foundation's | Foundation's |
//!
//! **The finding, stated plainly: the Wi-Fi half of CoreWLAN cannot be answered
//! on this host, and `bssid` is the reason it matters.**
//!
//! Chromium's own source says what `bssid` is, in a comment that is in the
//! source tree precisely because the value is surprising
//! (`services/device/geolocation/wifi_data_provider_mac.mm`):
//!
//! ```objc
//! // -[CWNetwork bssid] uses colons to separate the components of the MAC
//! // address, but AccessPointData requires they be separated with a dash.
//! access_point_data.mac_address = base::SysNSStringToUTF8([network.bssid
//!     stringByReplacingOccurrencesOfString:@":" withString:@"-"]);
//! access_point_data.radio_signal_strength = network.rssiValue;
//! access_point_data.channel = network.wlanChannel.channelNumber;
//! ```
//!
//! and the binary agrees: `0x3cd71b8` loads the `bssid` selref, `0x3cd71c6`
//! loads `stringByReplacingOccurrencesOfString:withString:`, and the result
//! becomes an `AccessPointData`. An `AccessPointData` is what
//! `WifiDataProviderCommon` hands the network-location provider, which **sends
//! it to Google's geolocation service**. The second consumer,
//! `components/wifi/wifi_service_mac.mm`, puts the same BSSID and the SSID into
//! `chrome.wifi`'s `NetworkProperties`, which extensions can read.
//!
//! So a MAC address from this host's real access point would be reported by the
//! emulator to a remote service and to extensions. There are three reasons that
//! answer is not given, and they are independent:
//!
//! 1. **It is not derivable.** CoreWLAN's answers come from the airport kext, a
//!    private plist, and a keychain item. None of the three exists here, and
//!    nothing in the image constrains what they would have contained. Any
//!    6-byte value would be an invention.
//! 2. **It is not this runtime's to publish.** Even the host's *real* AP
//!    identifier is a third party's, and handing it to a geolocation endpoint is
//!    a decision for the person running the machine, not for a shim library.
//! 3. **Nothing would ever check it.** A wrong MAC silently produces a
//!    different user agent, or a different geolocation fix, and both look like
//!    working output. That is the exact failure this project exists to refuse.
//!
//! Each refusal names itself: an absent selector on a published class is
//! `objc`'s own `unrecognized selector sent to instance of CWInterface:
//! scanForNetworksWithName:error:`, which is a better outcome than an unbound
//! import (a jump through a zero slot) and a far better one than a plausible
//! address.
//!
//! # Eight names in the brief's list that the guest never sends
//!
//! `__TEXT,__objc_methname` holds 4593 selector strings, and a selector the
//! guest sends has to be one of them. These are **absent**, so implementing them
//! would be inventing surface for a caller that does not exist:
//!
//! ```text
//! supportsWPA   supportsWPA2   supportsWPA3   wlanChannelNumber   noiseLevel
//! transmitRate  countryCode    isPowerOn      hwAddress
//! ```
//!
//! So, checked against the 712-selector dead list in `test/tools/OBJCENC.md` as
//! the brief asks: **none** of the five selectors installed here is on it. All
//! five are ones the guest demonstrably sends, so all five earn their place.
//!
//! # One measured negative worth keeping
//!
//! `wifi_data_provider_mac.mm` reads `interface.interfaceName` twice, inside
//! `DLOG(WARNING)` and `DVLOG(1)`. `interfaceName` has exactly **one**
//! reference in `__TEXT,__text` (`0xbbde8c4`, in the fourth consumer) - so both
//! of those reads are **absent from the binary**, which is what a `DLOG`/`DVLOG`
//! compiled out of a release build looks like. The same thing is recorded for
//! OpenDirectory's six `DLOG(WARNING)` messages. A selector count is a better
//! census than a source read, and this is what it caught.
//!
//! # The fourth consumer, named as far as the evidence goes
//!
//! `0xbbde760` walks `+sharedWiFiClient`, then `-interfaces`, then per element
//! `-interfaceName` / `isEqualToString:` and, on a match, `-ssid`, returning a
//! `bool` through an out-parameter. `test/tools/whichfile.py` brackets it
//! between `../../base/observer_list.h` (+0x46ad) and
//! `../../components/content_settings/core/common/cookie_settings_base.cc`
//! (-0x8f4) - two template headers, which is the weak bracket PLAN §7 warns
//! about, and there is no `__FILE__` marker inside the region. **I could not
//! attribute it, and I am not going to guess.** What matters for this crate is
//! already measured: it needs `-ssid` for an interface whose name it compares
//! against one it holds, so it lands in the refused column like the other two.

use std::ffi::CStr;

mod client;
mod radio;

/// Install instance methods on a class.
macro_rules! methods {
    ($class:expr, { $( $sel:literal => $imp:expr, $types:literal );* $(;)? }) => {{
        let class: &'static objc::class::Class = $class;
        $(
            class.replace_method(
                objc::sel::get($sel),
                $imp as *const std::ffi::c_void,
                $types,
            );
        )*
    }};
}

/// Install class methods, which live on the metaclass.
macro_rules! class_methods {
    ($class:expr, { $( $sel:literal => $imp:expr, $types:literal );* $(;)? }) => {{
        let class: &'static objc::class::Class = $class;
        let meta = class.metaclass();
        $(
            meta.replace_method(
                objc::sel::get($sel),
                $imp as *const std::ffi::c_void,
                $types,
            );
        )*
    }};
}

pub(crate) use {class_methods, methods};

/// The two classes this crate answers for.
pub const CLASSES: &[&CStr] = &[c"CWWiFiClient", c"CWInterface"];

/// The one class the image imports.
///
/// `__objc_classrefs` names no CoreWLAN class but this one, so publishing
/// `_OBJC_CLASS_$_CWInterface` would be publishing a name nothing asks for -
/// and a name that can only ever be wrong. The class is still *registered*
/// (above) because the object `-interface` hands back has to be a real one.
pub const IMPORTED: &[&CStr] = &[c"CWWiFiClient"];

/// Every selector the guest sends to those two classes, and which class each one
/// goes to.
///
/// The five with an answer are installed. The twelve after them are the census's
/// refused half, kept here so a run that stops on one of them has the list in
/// the same place as the reason - and so a test can assert that none of them has
/// quietly grown an implementation.
pub const SELECTORS: &[(&CStr, &str, &str)] = &[
    (c"CWWiFiClient", "+", "sharedWiFiClient"),
    (c"CWWiFiClient", "-", "interface"),
    (c"CWWiFiClient", "-", "interfaces"),
    (c"CWInterface", "-", "powerOn"),
    (c"CWInterface", "-", "interfaceName"),
];

/// The selectors the guest sends that this crate does **not** answer.
///
/// Each is a question about a Wi-Fi *network* rather than about a radio, and
/// CoreWLAN answers them from the airport kext, a private plist and a keychain
/// item. None of those exists here. See the crate note for why `bssid` in
/// particular is not invented.
pub const REFUSED: &[(&CStr, &str, &str)] = &[
    (c"CWInterface", "-", "scanForNetworksWithName:error:"),
    (c"CWInterface", "-", "bssid"),
    (c"CWInterface", "-", "ssid"),
    (c"CWInterface", "-", "setPower:error:"),
    (c"CWInterface", "-", "setPairwiseMasterKey:error:"),
    (c"CWInterface", "-", "associateToNetwork:password:error:"),
    (c"CWNetwork", "-", "bssid"),
    (c"CWNetwork", "-", "ssid"),
    (c"CWNetwork", "-", "wlanChannel"),
    (c"CWNetwork", "-", "rssiValue"),
    (c"CWNetwork", "-", "noiseMeasurement"),
    (c"CWNetwork", "-", "supportsSecurity:"),
    (c"CWChannel", "-", "channelNumber"),
    (c"CWChannel", "-", "channelBand"),
];

/// Install this crate's classes and methods. Idempotent.
pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        // `NSObject` is the superclass of both on Darwin, and the reason
        // `+class` and `isKindOfClass:` resolve without this crate implementing
        // anything.
        //
        // `CWWiFiClient` needs no payload, so the abstract shape is enough.
        corefoundation::base::abstract_class(c"CWWiFiClient", None);
        // `CWInterface` carries one word - the interface's own name - and a
        // class's instance size is fixed at registration and cannot be grown
        // afterwards (`class_add_ivar` refuses a registered class). So the size
        // is given here, at registration, and the class is **not** also created
        // by a loop over [`CLASSES`] first: `register` returns an existing class
        // unchanged, so registering it as abstract first would leave it eight
        // bytes, and `corefoundation::base::allocate` refuses the sixteen-byte
        // `Body` rather than corrupting the heap. It did exactly that, once, and
        // that assertion is why the order is written down.
        let interface = corefoundation::base::register(
            c"CWInterface",
            None,
            size_of::<client::Body>(),
            false,
        );
        let class = |name: &CStr| {
            objc::class::lookup(name).unwrap_or_else(|| {
                unreachable!("{name:?} was registered by the loop above")
            })
        };
        client::install(class(c"CWWiFiClient"), interface);
    });
}

/// The Objective-C class objects exported by CoreWLAN.framework.
///
/// The class object and its metaclass, which is the shape
/// `foundation::class_data`, `webkit::data_symbols` and `opendirectory` use.
/// The census says the image imports `_OBJC_CLASS_$_CWWiFiClient` and nothing
/// else from this framework, so [`IMPORTED`] is what is published; the
/// metaclass is published beside it because every Objective-C class exports one
/// and a synthetic dylib that answered half a pair would be a new trap.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut out = Vec::new();
    for name in IMPORTED {
        let class = objc::class::lookup(name)
            .unwrap_or_else(|| panic!("{name:?} is registered by bootstrap"));
        let spelling = name.to_string_lossy();
        out.push((
            format!("_OBJC_CLASS_$_{spelling}"),
            objc::class::ptr(class) as usize as u64,
        ));
        out.push((
            format!("_OBJC_METACLASS_$_{spelling}"),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ));
    }
    out
}

/// `dlsym` for this framework's names.
///
/// `darwin::symbol` strips exactly one leading underscore, so the comparison
/// here strips one too - which is why it is written against [`data_symbols`]
/// rather than against [`IMPORTED`], whose members carry no underscore.
pub fn symbol(name: &str) -> Option<u64> {
    let wanted = name.trim_start_matches('_');
    data_symbols()
        .into_iter()
        .find(|(symbol, _)| symbol.trim_start_matches('_') == wanted)
        .map(|(_, address)| address)
}

#[cfg(test)]
mod tests;
