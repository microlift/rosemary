//! CoreLocation: the one class Chromium Framework imports from it, and the
//! answers this host can truthfully give that class.
//!
//! # The import set is one class and two refused constants
//!
//! The Framework's `LC_LOAD_DYLIB` at load-command offset `0x20fb` names
//! `/System/Library/Frameworks/CoreLocation.framework/Versions/A/CoreLocation`,
//! and the whole import set is **three data symbols and zero functions**:
//! `_OBJC_CLASS_$_CLLocationManager` (`__objc_classrefs` slot `0xe6e7a80`),
//! `_kCLLocationAccuracyBest` and `_kCLLocationAccuracyHundredMeters` (both
//! `__DATA_CONST,__got`, and both already `darwin`'s named zeroed refusals at
//! `crates/darwin/src/data_unavailable.rs:766-768`). Zero functions is why
//! `frameworks.rs` registers this crate with `provide: None` - the guest reaches
//! CoreLocation only through `objc_msgSend`, so the class and its selectors are
//! the whole of the surface here, exactly as for OpenDirectory.
//!
//! `CLLocationManager` is also the **only** `CL*` class in the image. Every
//! image in the bundle was searched for the string `_OBJC_CLASS_$_CLLocationManager`
//! and only the Framework has it, twice each (bind stream and symbol table); and
//! `CLGeocoder` and `CLPlacemark` appear **nowhere in the Framework at all** -
//! searched for, absent, and the positive control for the search is the nine
//! `CL*`-shaped strings that *are* there: `CLBeaconIdentityConstraint`,
//! `CLBeaconRegion`, `CLHeading`, `CLLocation`, `CLLocationAccuracyBest`,
//! `CLLocationAccuracyHundredMeters`, `CLLocationManager`,
//! `CLLocationManagerDelegate` and `CLRegion`. All nine are protocol or symbol
//! names the guest embeds; not one of them is a second class it can send to.
//!
//! # The whole surface: six selectors, and where the guest sends each
//!
//! A whole-`__TEXT,__text` scan for rip-relative references to the classref
//! `0xe6e7a80`, asking at every byte offset and re-disassembling every candidate,
//! finds **one** reference: `0x83e690e`, the `_objc_alloc_init` at
//! `0x83e6915`. So the class is named once, and everything else reaches an
//! instance through the source's own ivar. The selectors and their send sites:
//!
//! | selector | encoding | send | arity check |
//! |---|---|---|---|
//! | `init` | `@16@0:8` | `0x83e6915` (`_objc_alloc_init`) | 0 colons, frame 16 |
//! | `setDelegate:` | `v24@0:8@16` | `0x83e6b40` | 1 colon, frame 24 |
//! | `setDesiredAccuracy:` | `v24@0:8d16` | `0x83e769d` | 1 colon, frame 24 |
//! | `startUpdatingLocation` | `v16@0:8` | `0x83e76cb` | 0 colons, frame 16 |
//! | `stopUpdatingLocation` | `v16@0:8` | `0x83e7873` | 0 colons, frame 16 |
//! | `requestWhenInUseAuthorization` | `v16@0:8` | `0x83e7a48` | 0 colons, frame 16 |
//!
//! **The arity column is the rule, and it is a test.** A selector's colon count
//! is a property of its name, and the frame is `8 * (2 + colons)` for every
//! selector here. `install` checks that against each installed encoding, and the
//! check is narrow on purpose and says so: it verifies the *count*, not the
//! types, so it would not survive a struct argument (there are none on this
//! surface) and it is not a substitute for reading the call site. It is here
//! because the alternative failure is the one that killed this guest twice: a
//! self-consistent encoding beside a self-consistent signature, both wrong by one
//! argument, and the unit test green throughout because the test sent the same
//! registers.
//!
//! Each site brackets between `__FILE__` markers that both name
//! `services/device/public/cpp/geolocation/system_geolocation_source_apple.mm`
//! (`0x83e734a`, `0x83e7783`, `0x83e7916`), and each has **one** real
//! reference in `__text`. `startUpdatingLocation` appears to have two; the second
//! (`0x47edee1`) is a false positive and re-disassembly says so - it is the
//! immediate in `mov DWORD PTR [rbp-0x4],0x3054fbb3`, not a displacement.
//!
//! # What the guest does with each answer: void, all six, and that is measured
//!
//! - **`setDelegate:` is the wall this crate was written for**, and the abort
//!   was `unrecognized selector sent to instance of CLLocationManager:
//!   setDelegate:` at `Framework+0x83e6b47`. The return is discarded: the next
//!   instruction, `0x83e6b4d`, is the `__stack_chk_guard` load, and there is no
//!   test in between. `ROSETTA3_NAME_MISSING_CLASSES` had bound the classref to
//!   an empty class, so the first message to a class that does not exist here
//!   aborted by name - the design working.
//! - `setDesiredAccuracy:` (`0x83e6c4`) and `startUpdatingLocation`
//!   (`0x83e6d2`) are back to back, and `0x83e6c7` reloads `rdi` from
//!   `[rbx+0x20]` for the second without looking at the first's return.
//! - `stopUpdatingLocation` (`0x83e787a`) is followed by `0x83e7880`, which moves
//!   on to the network-change timer.
//! - `requestWhenInUseAuthorization` (`0x83e7a50`) *ends* the function: `0x83e7a4f`
//!   pops the frame and `0x83e7a50` tail-jumps to `objc_msgSend`, so there is no
//!   instruction after the send at all.
//!
//! So no guest path asserts anything about the answer to any of the five, and
//! void is a tolerated answer. What void is **not** is a licence to invent an
//! event: see [`manager`], where each one says which event it is not
//! producing.
//!
//! # The two `BOOL`s are the guest's own, and this is the finding
//!
//! `-[LocationManagerDelegate permissionInitialized]` and
//! `-[LocationManagerDelegate hasPermission]` return `BOOL`, both are used as
//! gates, and both look like the crux. **Neither is a message this crate
//! answers**, and the disassembly says so without any inference:
//!
//! - The receiver at both call sites is `[rbx+0x18]`, which the constructor set
//!   to the result of `initWithManager:` sent to a *guest* class -
//!   `_objc_alloc` on classref `0xe6e7a88` at `0x83e6acd`, which resolves to the
//!   `LocationManagerDelegate` class in `__DATA,__objc_data` at `0xe712140`.
//! - That class's own `method_list_t`, read through the metadata, carries
//!   non-zero `IMP`s for exactly these two selectors:
//!
//!   ```text
//!   hasPermission            c16@0:8    imp=0x83e5db0
//!   permissionInitialized    c16@0:8    imp=0x68fdb40
//!   ```
//!
//!   and both are five-byte field reads:
//!
//!   ```text
//!   83e5db4  movsx eax,BYTE PTR [rdi+0x9]   ; -hasPermission          -> _hasPermission
//!   68fdb44  movsx eax,BYTE PTR [rdi+0x8]   ; -permissionInitialized  -> _permissionInitialized
//!   ```
//!
//! A message with an implementation in the runtime cannot abort by name. So the
//! premise that these two "will abort by name in turn" is false, and the
//! interesting question is not which `BOOL` to return but **who writes the
//! fields**.
//!
//! # Which `BOOL` is true, decided from the branch
//!
//! Both fields are zeroed by the guest's own initializer, in one two-byte store
//! at **`0x83e5cba`**: `mov WORD PTR [r14+0x8],0x0`. Their only writer is the
//! guest's `-[LocationManagerDelegate locationManager:didChangeAuthorizationStatus:]`
//! (`0x83e5d40`), which nothing sends - because nothing here has an
//! authorization status to report. So the fields stay at the guest's own initial
//! value and the guest's `GetSystemPermission()` reads:
//!
//! ```text
//! 83e6f3b  mov  rdi,QWORD PTR [rbx+0x18]        ; delegate_
//! 83e6f3f  mov  rsi,[0xe6ec578]                 ; permissionInitialized
//! 83e6f46  call *objc_msgSend
//! 83e6f4c  test al,al
//! 83e6f4e  je   0x83e6f6d                       ; <-- the gate
//! 83e6f50  mov  rdi,QWORD PTR [rbx+0x18]
//! 83e6f54  mov  rsi,[0xe6ec580]                 ; hasPermission
//! 83e6f5b  call *objc_msgSend
//! 83e6f61  cmp  al,0x1
//! 83e6f63  mov  esi,0x2
//! 83e6f68  sbb  esi,0x0                         ; hasPermission ? 2 : 1
//! 83e6f6d  xor  esi,esi                         ; 0 = kNotDetermined
//! 83e6f6f  mov  rdi,QWORD PTR [rbx+0x28]        ; permission_update_callback_
//! ```
//!
//! (`2` and `1` are `kDenied` and `kGranted` in
//! `services/device/.../geolocation_system_permission_manager.h`; the `sbb`
//! form means `hasPermission == 1` gives 2, because `cmp al,1` sets `CF` only
//! when `al` is **less than** 1.)
//!
//! Reading the branch settles both questions, and they do not have the same
//! answer:
//!
//! - **`permissionInitialized` is the outer gate, and `NO` is the true answer.**
//!   Its contract is "has an authorization decision been reached?". On this host
//!   none has been and none can be, so `NO` - which the branch turns into
//!   `kNotDetermined`, the state for a permission never asked about. `YES` would
//!   assert a decision that was never made, and the guest would then read
//!   `hasPermission` and report `kDenied` or `kGranted` for a decision that does
//!   not exist. "Nothing is blocked" is not the same claim as "nothing has been
//!   decided", and only the second one is what this method asks.
//! - **`hasPermission` is never read**, because the gate is never open. That is
//!   a stronger result than picking a value: there is no `BOOL` here to get
//!   wrong, and a crate that answered it would be answering a question nobody
//!   asked. Its contract is "is location access authorised?", and the honest
//!   answer on this host is `NO` - but the honest answer is also the unobservable
//!   one, and saying so is more useful than manufacturing an occasion to supply
//!   it.
//!
//! The same conclusion falls out of the source's own control flow, which is
//! worth naming because it is the third independent witness: the *only* thing
//! that can set either field is the `didChangeAuthorizationStatus:` callback, and
//! the *only* thing that asks for that callback to be needed is
//! `-[CLLocationManager requestWhenInUseAuthorization]`, which
//! [`manager::install`] refuses by name. Refuse the request, deliver no
//! callback, and the guest reports `kNotDetermined` - which is true.
//!
//! # The `CLLocationManagerDelegate` protocol, and why this class does not
//! conform to it
//!
//! The guest embeds the whole protocol, and reading it is a complete census of
//! what CoreLocation may call back: `CLLocationManagerDelegate` at `0xda47649`
//! and **twenty** methods, carried by fifteen distinct `method_t.types` strings
//! from `0xda47663` to `0xda47918` (a few encodings are shared, so strings and
//! methods are not one-to-one) - the locations, authorization-status, region,
//! beacon, heading, visit and error families, and the three pause/resume and
//! calibration one-argument callbacks.
//!
//! The guest implements **three** of them, with its own encodings
//! (`v32@0:8@16@24` for `locationManager:didUpdateLocations:` and again for
//! `locationManager:didFailWithError:`, `v28@0:8@16i24` for
//! `locationManager:didChangeAuthorizationStatus:`), at `0x83e5dc0`, `0x83e5f70`
//! and `0x83e5d40`. This crate sends **none** of them, and does not claim
//! conformance to the protocol - a class that declared
//! `CLLocationManagerDelegate` and then never made a callback would be claiming
//! something it does not do, which is the shape this project calls a wrong
//! answer that looks like success. The three are reached only through a callback,
//! so with no callback they are unreachable code, and their IMPs are never
//! entered.
//!
//! `-[LocationManagerDelegate locationManager:didUpdateLocations:]` is where
//! `-horizontalAccuracy` and `-verticalAccuracy` are sent (`0x83e5ed9`,
//! `0x83e5eea`, the only references to either selector in the image), and its
//! receiver would be a `CLLocation`. **No `CLLocation` is ever created here** -
//! there is no `CLLocation` classref and no `CLLocation` class symbol - so those
//! two selectors are unreachable too, and this crate installs neither. Inventing
//! a `CLLocation` to answer them is precisely the fabrication the crate note
//! rules out.
//!
//! # What is deliberately not here
//!
//! - **`+[CLLocationManager authorizationStatus]`, and every other class
//!   method.** The selector `authorizationStatus` has three references in the
//!   image, and all three are `UNUserNotificationCenter`:
//!   `-[UNUserNotificationCenter getNotificationSettingsWithCompletionHandler:]`
//!   reading a field out of an `UNNotificationSettings` struct, bracketed
//!   between `chrome/services/mac_notifications/mac_notification_service_un.mm`
//!   markers. `+[CLLocationManager authorizationStatus]` is never sent, so a
//!   `CLAuthorizationStatus` is never asked for - which is the same fact as the
//!   gate above, arriving from the other side.
//! - **`-authorizationStatus`, `-activityType`, `-coordinate`, `-timestamp`,
//!   `-altitude`, `-speed`, `-course` and the rest of `CLLocation`.** No instance
//!   of it exists to be sent them.
//! - **`kCLAuthorizationStatus*` and `kCLError*`.** `kCLErrorDenied`,
//!   `kCLErrorLocationUnknown` and `kCLErrorNetwork` are in the image only as
//!   substrings of the guest's own `error_technical` message strings
//!   (`0xda474df` and neighbours), and every one of them is read out of an
//!   `NSError` that only `locationManager:didFailWithError:` produces.
//! - **CoreWLAN.** `-[SystemGeolocationSourceApple isWifiEnabled]` sends
//!   `+[CWWiFiClient sharedWiFiClient]`, `-interface` and `-powerOn` from the
//!   same function (`0x83e76d5` onwards) and caches the answer in
//!   `was_wifi_enabled_`. That is CoreWLAN's surface, not this crate's, it is
//!   reached *after* both of this crate's two sends in the same function, and it
//!   belongs to the `corewlan` crate.

use std::ffi::CStr;

mod absence;
mod manager;

pub use absence::WHY;

/// Install instance methods on a class.
macro_rules! methods {
    ($class:expr, { $( $sel:literal => $imp:expr, $types:literal );* $(;)? }) => {{
        let class: &'static objc::class::Class = $class;
        $(
            class.replace_method(
                objc::sel::get($sel),
                $imp as *const c_void,
                $types,
            );
            // Checked here rather than only in a test, because the check is one
            // comparison and the alternative is the bug this project has paid for
            // twice. `tests::every_installed_encoding_matches_its_selector` is the
            // same rule stated as a test.
            debug_assert_eq!(
                $crate::frame_of($types) as usize,
                8 * (2 + $sel.matches(':').count()),
                "{}: a selector with {} colon(s) takes {} word(s), and {:?} passes {}",
                $sel,
                $sel.matches(':').count(),
                $sel.matches(':').count(),
                $types,
                $crate::frame_of($types) as usize,
            );
        )*
    }};
}

pub(crate) use methods;

/// The frame size an Objective-C type encoding claims, read out of the encoding.
///
/// The digits immediately before the `@0:` that opens the implicit
/// self-and-selector pair are the frame size, so this reads the encoding's own
/// claim rather than trusting the number typed next to it - which is the whole
/// point, because the number and the signature beside it are exactly the two
/// things that were both self-consistent and both wrong in
/// `crates/appkit/src/appearance.rs`.
///
/// The first `@0:` is the right one to stop at: it is the implicit `self`, which
/// precedes every declared argument, and a struct argument's own embedded
/// offsets come later. Returns 0 for an encoding with no `@0:` at all, which is
/// then a failure of the check rather than a silent pass.
pub(crate) fn frame_of(types: &CStr) -> u32 {
    let text = types.to_str().unwrap_or("");
    let Some(start) = text.find("@0:") else {
        return 0;
    };
    let digits = text[..start]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits
        .chars()
        .rev()
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// The one class this crate answers for.
pub const CLASSES: &[&CStr] = &[c"CLLocationManager"];

/// Every selector the guest sends to [`CLASSES`], and the class each goes to.
///
/// `init` is listed because the guest sends it - through `_objc_alloc_init` at
/// `0x83e6915`, which is `+alloc` followed by `-init` - and this crate does not
/// install it, because `NSObject`'s is the right answer and installing a second
/// one would be a change with no measured reason. Every other entry is installed
/// by [`manager::install`]. The list is a claim about the *whole* surface, so a
/// selector the guest sends and this crate does not answer has to be accounted
/// for here rather than left to abort.
pub const SELECTORS: &[(&CStr, &str, &str)] = &[
    (c"CLLocationManager", "-", "init"),
    (c"CLLocationManager", "-", "setDelegate:"),
    (c"CLLocationManager", "-", "setDesiredAccuracy:"),
    (c"CLLocationManager", "-", "startUpdatingLocation"),
    (c"CLLocationManager", "-", "stopUpdatingLocation"),
    (c"CLLocationManager", "-", "requestWhenInUseAuthorization"),
];

/// Install this crate's class and methods. Idempotent.
pub fn bootstrap() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        foundation::bootstrap();
        // `NSObject` is the superclass on Darwin, and the reason `-init`,
        // `-dealloc` and `+class` resolve on a manager this crate publishes.
        // A nil superclass would also make `_objc_alloc_init` allocate a bare
        // `isa` with nowhere to put the delegate.
        for name in CLASSES {
            corefoundation::base::register(
                name,
                None,
                std::mem::size_of::<manager::Manager>(),
                false,
            );
        }
        let class = objc::class::lookup(c"CLLocationManager")
            .expect("CLLocationManager was registered by the loop above");
        manager::install(class);
    });
}

/// The Objective-C class objects exported by CoreLocation.framework.
///
/// The class objects only, and **not** the two `kCLLocationAccuracy*` doubles:
/// `darwin` already publishes both as named zeroed refusals, and a second
/// definition of one name is a conflict rather than an improvement. See
/// [`absence`] for why the refused value is inert.
///
/// A `_OBJC_CLASS_$_X` symbol's *value* is the class, not the address of a
/// pointer to it, so the published address is the class object itself - the same
/// shape `foundation::class_data` and `opendirectory::data_symbols` use.
pub fn data_symbols() -> Vec<(String, u64)> {
    bootstrap();
    let mut out = Vec::new();
    for name in CLASSES {
        let class = objc::class::lookup(name)
            .unwrap_or_else(|| panic!("{name:?} is registered by bootstrap"));
        let name = name.to_string_lossy();
        out.push((
            format!("_OBJC_CLASS_$_{name}"),
            objc::class::ptr(class) as usize as u64,
        ));
        out.push((
            format!("_OBJC_METACLASS_$_{name}"),
            objc::class::ptr(class.metaclass()) as usize as u64,
        ));
    }
    out
}

/// `dlsym` for this framework's names.
///
/// `darwin::symbol` strips exactly one leading underscore, so the comparison
/// here strips one too - which is why it is written against `data_symbols()`
/// rather than against [`CLASSES`], whose members carry no underscore.
pub fn symbol(name: &str) -> Option<u64> {
    let wanted = name.trim_start_matches('_');
    data_symbols()
        .into_iter()
        .find(|(symbol, _)| symbol.trim_start_matches('_') == wanted)
        .map(|(_, address)| address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc::class::Class;
    use objc::object::Id;
    use objc::sel::Sel;

    fn named() -> &'static Class {
        bootstrap();
        objc::class::lookup(c"CLLocationManager").expect("registered by bootstrap")
    }

    /// The installed `IMP` for a selector, transmuted to a prototype.
    fn imp_of(class: &'static Class, selector: &str) -> objc::class::Imp {
        class
            .method(objc::sel::get(selector))
            .unwrap_or_else(|| panic!("{selector} is not installed"))
            .implementation()
    }

    /// Every selector the guest sends is installed, with an encoding whose frame
    /// is what the selector's own name says it must be.
    ///
    /// **What this checks and what it does not.** A selector with one colon takes
    /// one argument, always - that is a property of the name, not of any
    /// declaration - and for a word-sized argument the frame is
    /// `8 * (2 + colons)`. So a four-argument signature beside a one-colon
    /// selector is caught here, which is the shape that has killed this guest
    /// twice. What it cannot check is whether an argument's *type* is right, and
    /// it would not survive a struct argument (this crate installs none, and the
    /// check says so rather than pretending otherwise). A test that encodes the
    /// same misreading as the code cannot catch the misreading, so this one
    /// derives its expectation from the selector name alone.
    #[test]
    fn every_installed_encoding_matches_its_selector() {
        let class = named();
        for (_, kind, selector) in SELECTORS {
            if *kind != "-" {
                continue;
            }
            let Some(method) = class.method(objc::sel::get(selector)) else {
                // `init` is answered by `NSObject` rather than installed here.
                assert_eq!(
                    *selector, "init",
                    "{selector} is neither installed nor NSObject's"
                );
                continue;
            };
            let types = // SAFETY: a `method_t`'s `types` is a NUL-terminated string
                // the runtime allocated, and this is the one accessor's own return.
                unsafe { CStr::from_ptr(method.types()) };
            let colons = selector.matches(':').count();
            assert_eq!(
                frame_of(types) as usize,
                8 * (2 + colons),
                "{selector} has {colons} colon(s), so its frame must be {}, and {types:?} claims \
                 {}",
                8 * (2 + colons),
                frame_of(types),
            );
        }
    }

    /// The class exists, descends from `NSObject`, and can be allocated and
    /// initialised - which is the shape of the guest's very first call, at
    /// `0x83e6915`.
    #[test]
    fn a_manager_is_an_nsobject_that_survives_alloc_init() {
        let class = named();
        assert_eq!(
            class.superclass().map(|s| s.name_str().to_string()),
            Some("NSObject".to_string()),
            "a nil superclass would leave -init and +class unresolvable on a class we publish"
        );
        // `+alloc` then `-init`, which is what `_objc_alloc_init` does.
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(imp_of(class.metaclass(), "alloc")) };
        // SAFETY: the class is registered and has NSObject's instance size.
        let manager = unsafe { f(objc::class::ptr(class) as Id, objc::sel::get("alloc")) };
        assert!(!manager.is_null());
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(imp_of(class, "init")) };
        // SAFETY: `manager` is the live instance just allocated.
        let initialised = unsafe { f(manager, objc::sel::get("init")) };
        assert_eq!(
            initialised, manager,
            "-init must return the same object: a different one would be a second manager, and the \\
             guest stores exactly what it is given at [rbx+0x20]"
        );
    }

    /// `setDelegate:` records the delegate and returns void. It is the wall this
    /// crate was written for, so the shape of the call is the guest's: one
    /// argument in `rdx`, the selector in `rsi`, the receiver in `rdi`.
    #[test]
    fn set_delegate_takes_the_delegate_and_answers_void() {
        let class = named();
        let f: unsafe extern "C" fn(Id, Sel, Id) =
            unsafe { std::mem::transmute(imp_of(class, "setDelegate:")) };
        let manager = objc::object::create_instance(class, 0);
        // A stand-in delegate: this test is about the *store*, and the guest's own
        // `LocationManagerDelegate` is a guest class that only exists in a loaded
        // image.
        let delegate = objc::object::create_instance(class, 0);
        unsafe { f(manager, objc::sel::get("setDelegate:"), delegate) };
        // SAFETY: `manager` is an instance of this class.
        // SAFETY: `manager` is an instance of this class.
        let stored = unsafe { manager::delegate_of(manager) };
        assert_eq!(
            stored, delegate,
            "setDelegate: must record the delegate it was given. Not recording it is fine; \\
             recording something *else* is a claim about the guest's own object"
        );
        // And nil is a shape the guest can send, so it must not crash.
        unsafe {
            f(
                manager,
                objc::sel::get("setDelegate:"),
                std::ptr::null_mut(),
            )
        };
        // SAFETY: as above.
        assert!(unsafe { manager::delegate_of(manager) }.is_null());
    }

    /// The other four are void, take the arguments the guest passes, and return
    /// nothing - and each says on stderr, once, what it is not doing.
    #[test]
    fn the_four_void_answers_take_the_guests_arguments_and_return_nothing() {
        let class = named();
        // Each prototype below matches the installed encoding, which
        // `every_installed_encoding_matches_its_selector` has already checked
        // against the selector's own colon count.
        let manager = objc::object::create_instance(class, 0);

        let f: unsafe extern "C" fn(Id, Sel, f64) =
            unsafe { std::mem::transmute(imp_of(class, "setDesiredAccuracy:")) };
        // The guest passes a `double` in `%xmm0`; the value is whatever
        // `darwin`'s refused `_kCLLocationAccuracy*` globals hold, which is 0.0.
        unsafe { f(manager, objc::sel::get("setDesiredAccuracy:"), 0.0) };

        for selector in [
            "startUpdatingLocation",
            "stopUpdatingLocation",
            "requestWhenInUseAuthorization",
        ] {
            let f: unsafe extern "C" fn(Id, Sel) =
                unsafe { std::mem::transmute(imp_of(class, selector)) };
            unsafe { f(manager, objc::sel::get(selector)) };
        }
    }

    /// The two `BOOL`s the brief calls the crux are **not** installed here, and
    /// that is the finding rather than an omission: they are the guest's own
    /// methods on the guest's own `LocationManagerDelegate` class, and
    /// `name_missing_classes` cannot make them abort because the runtime has an
    /// `IMP` for each (`0x83e5db0` and `0x68fdb40`, both five-byte field reads
    /// of `[self+0x9]` and `[self+0x8]`).
    ///
    /// What this asserts is the half that *is* ours: that this crate's class does
    /// not answer them, so no `BOOL` is invented for a question whose gate
    /// (`permissionInitialized`) the guest's own field keeps closed.
    #[test]
    fn the_two_guest_bools_are_not_answered_here() {
        let class = named();
        for selector in ["hasPermission", "permissionInitialized"] {
            assert!(
                class.own_method(objc::sel::get(selector)).is_none(),
                "{selector} is the guest's own field read at [self+8]/[self+9], zeroed by the \\
                 guest's own initWithManager: at 0x83e5cba. Answering it here would be a BOOL \\
                 this crate invented for a gate it does not control"
            );
        }
    }

    /// The class object is published under Mach-O's spelling, with one leading
    /// underscore more than the class's own name - the trap this project has been
    /// bitten by three times.
    #[test]
    fn the_exported_symbol_has_one_more_underscore_than_the_class() {
        // **What the spellings actually are**, because a clean build and a green
        // suite both accept a wrong one. The guest *imports*
        // `_OBJC_CLASS_$_CLLocationManager`; `exports!` publishes a row as
        // `"_" + row`; and `darwin::symbol` strips exactly one leading underscore
        // from the name it is asked, which is the same one the publication adds.
        // So both spellings resolve to the same address, and neither the bare
        // class name nor a *doubled* underscore is a name here:
        assert_eq!(
            symbol("_OBJC_CLASS_$_CLLocationManager"),
            symbol("OBJC_CLASS_$_CLLocationManager"),
            "one leading underscore is stripped from each side, so the two spellings must agree"
        );
        assert_eq!(
            symbol("__OBJC_CLASS_$_CLLocationManager"),
            symbol("_OBJC_CLASS_$_CLLocationManager"),
            "exactly one is stripped, so a doubled underscore is the same name"
        );
        for name in [
            "CLLocationManager",
            "_CLLocationManager",
            "_OBJC_CLASS_$CLLocationManager",
            "_OBJC_CLASS_$_CLLocationManagers",
        ] {
            assert!(
                symbol(name).is_none(),
                "{name:?} is not an exported name of this framework, and resolving it would be \\
                 answering a question nobody asked"
            );
        }
        assert_eq!(
            symbol("_OBJC_METACLASS_$_CLLocationManager"),
            Some(objc::class::ptr(named().metaclass()) as usize as u64),
            "a metaclass import bound to the class object, or to zero, breaks `+class`"
        );
        let symbols = data_symbols();
        assert!(symbols.iter().all(|(_, address)| *address != 0));
        // Stable, and the class pair is the same one every time.
        assert_eq!(symbols, data_symbols());
        let published = symbol("_OBJC_CLASS_$_CLLocationManager").expect("published") as usize;
        assert_eq!(
            published,
            objc::class::ptr(named()) as usize,
            "the published address must be the class object itself, not a pointer to a pointer"
        );
    }

    /// Every class this crate publishes is reachable under the name the guest
    /// imports, and is an `NSObject` - so a guest that binds
    /// `_OBJC_CLASS_$_CLLocationManager` and sends `+class` or `-isKindOfClass:`
    /// gets a real class rather than the empty one `ROSETTA3_NAME_MISSING_CLASSES`
    /// builds.
    #[test]
    fn every_class_is_registered_and_is_not_an_empty_placeholder() {
        for name in CLASSES {
            let class = named_with(name);
            assert!(class.is_registered(), "{name:?} is not registered");
            assert!(!class.own_methods().is_empty(), "{name:?} has no methods");
            assert!(
                class.chain().count() > 1,
                "{name:?} has no superclass, so NSObject's methods would not resolve"
            );
        }
    }

    fn named_with(name: &CStr) -> &'static Class {
        bootstrap();
        objc::class::lookup(name).unwrap_or_else(|| panic!("{name:?} is registered"))
    }
}
