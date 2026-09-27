//! `CLLocationManager`: the one CoreLocation class the guest sends messages to,
//! and the five answers this host can give.
//!
//! # Every selector the guest sends to it, and where
//!
//! `CLLocationManager`'s `__objc_classrefs` slot is `0xe6e7a80`, and a
//! whole-`__TEXT,__text` scan for rip-relative references to it - asking at
//! **every** byte offset, not only at four-byte-aligned ones and not only at
//! `0x48`-prefixed loads, and re-disassembling every candidate to confirm it -
//! finds **one**:
//!
//! ```text
//! 0x83e690e  mov  rdi,QWORD PTR [rip+0x630116b]   # 0xe6e7a80
//! 0x83e6915  call 0xcca0912                      ; _objc_alloc_init
//! ```
//!
//! So the class is named in exactly one place. Everything else reaches an
//! instance through the source's ivar, `[rbx+0x20]`, and the selectors below
//! are the ones its own file sends there. Each has **one** real send site (the
//! one apparent second reference to `startUpdatingLocation`, at `0x47edee1`, is
//! a false positive: it is the immediate in
//! `mov DWORD PTR [rbp-0x4],0x3054fbb3`, not a displacement), and each site
//! brackets between `__FILE__` markers that both name
//! `services/device/public/cpp/geolocation/system_geolocation_source_apple.mm`
//! (`0x83e734a`, `0x83e7783`, `0x83e7916`).
//!
//! | selector | encoding | send site | what the guest does with the answer |
//! |---|---|---|---|
//! | `init` | `@16@0:8` | `0x83e6915`, via `_objc_alloc_init` | the result is the manager; `NSObject`'s `init` answers it |
//! | `setDelegate:` | `v24@0:8@16` | `0x83e6b40` | **discarded** - see below |
//! | `setDesiredAccuracy:` | `v24@0:8d16` | `0x83e6c4` | **discarded** |
//! | `startUpdatingLocation` | `v16@0:8` | `0x83e6d2` | **discarded** |
//! | `stopUpdatingLocation` | `v16@0:8` | `0x83e787a` | **discarded** |
//! | `requestWhenInUseAuthorization` | `v16@0:8` | `0x83e7a50` | **discarded** (a tail `jmp`) |
//!
//! Five void-or-discarded answers, and the tolerance for each is in the
//! disassembly rather than in the source:
//!
//! - `setDelegate:` is the fault this crate was written for. The call is at
//!   `0x83e6b47` and the **next** instruction, `0x83e6b4d`, is the
//!   `__stack_chk_guard` load. There is no test of the return anywhere between.
//! - `setDesiredAccuracy:` and `startUpdatingLocation` are back to back: `0x83e6c4`
//!   and `0x83e6d2`, and `0x83e6c7` reloads `rdi` from `[rbx+0x20]` for the
//!   second one without looking at what the first returned.
//! - `stopUpdatingLocation` is at `0x83e787a` and `0x83e7880` moves on to the
//!   network-change timer.
//! - `requestWhenInUseAuthorization` ends the function: `0x83e7a4f` pops the
//!   frame and `0x83e7a50` tail-jumps to `objc_msgSend`, so there is no
//!   instruction after the send at all.
//!
//! **Void is therefore a tolerated answer at all five**, and this crate gives
//! void. What it does not do is pretend the event happened: see
//! [`crate`] for what each one is refusing and for the two `BOOL`s that turn
//! out not to be ours to answer.

use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::absence;

/// The instance layout.
///
/// `isa` is the runtime's and is accounted for by the superclass, so what is
/// left here is this crate's: one unretained delegate, exactly as CoreLocation
/// declares it (`@property(assign, nonatomic, nullable) id<CLLocationManagerDelegate>
/// delegate`). Nothing reads it - there is no callback to make - and it is
/// written so that a future reader of this file can see that the *only* thing
/// standing between this object and a delegate call is the absence of a service,
/// not an oversight.
#[repr(C)]
pub struct Manager {
    /// Never dereferenced by this crate. See the type note.
    delegate: Id,
}

/// The delegate this manager was last given, read back.
///
/// The field is deliberately not `pub`: nothing in the crate acts on it, and the
/// only reader is a test that wants to see the store happen. Keeping the door
/// this narrow is the point - a future change that starts *calling* the delegate
/// has to come through here and say so.
///
/// # Safety
///
/// `manager` must be a live instance of this class.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) unsafe fn delegate_of(manager: Id) -> Id {
    // SAFETY: the caller's obligation, checked above.
    unsafe { (*(manager as *const Manager)).delegate }
}

/// Install `CLLocationManager`'s instance methods.
pub fn install(class: &'static Class) {
    use crate::methods;
    methods!(class, {
        "setDelegate:" => set_delegate, c"v24@0:8@16";
        "setDesiredAccuracy:" => set_desired_accuracy, c"v24@0:8d16";
        "startUpdatingLocation" => start_updating_location, c"v16@0:8";
        "stopUpdatingLocation" => stop_updating_location, c"v16@0:8";
        "requestWhenInUseAuthorization" => request_when_in_use_authorization, c"v16@0:8";
    });
}

/// `-[CLLocationManager setDelegate:]`
///
/// **Accepted, and never acted on.** The guest set a delegate; recording that is
/// a fact about the guest, not a claim about the world, so it is recorded. What
/// must not happen is the callback - and the reason is not caution:
///
/// `-[LocationManagerDelegate locationManager:didChangeAuthorizationStatus:]` is
/// the *only* writer of the guest's `_permissionInitialized` and
/// `_hasPermission` (the two `BOOL`s at `[self+0x8]` and `[self+0x9]`, zeroed by
/// the guest's own `initWithManager:` at `0x83e5cba`,
/// `mov WORD PTR [r14+0x8],0x0`, and read by the guest's own `hasPermission` at
/// `0x83e5db4` and `permissionInitialized` at `0x68fdb40`). Delivering that
/// message with any `CLAuthorizationStatus` would be inventing an authorization
/// decision. Delivering it with `kCLAuthorizationStatusNotDetermined` would be
/// reporting a decision that was never made. So it is not delivered, and the
/// fields stay at the guest's own initial value, which is the truth: no
/// decision has been reached.
///
/// That leaves the guest's `GetSystemPermission()` answering
/// `kNotDetermined` (`esi = 0` at `0x83e6f6d`), which is the right answer on
/// this host. `-[CLLocationManager requestWhenInUseAuthorization]` below is where
/// that decision would have been asked for, and there is nothing to ask.
///
/// # Safety
///
/// Called by the runtime with a live `CLLocationManager`.
unsafe extern "C" fn set_delegate(this: Id, _sel: Sel, delegate: Id) {
    absence::say(
        "-[CLLocationManager setDelegate:]",
        "the delegate was recorded and nothing was sent to it - no location will ever arrive \
         and no authorization status will ever change",
    );
    // SAFETY: the runtime called a method on a live instance of this class, and
    // `Manager` is this class's instance layout.
    unsafe { (*(this as *mut Manager)).delegate = delegate };
}

/// `-[CLLocationManager setDesiredAccuracy:]`
///
/// **Void, and the value is not read.** The guest dereferences
/// `_kCLLocationAccuracyBest` / `_kCLLocationAccuracyHundredMeters` at
/// `0x83e76b9` and passes the double in `%xmm0`; both globals are `darwin`'s
/// named zeroed refusals (see [`crate::absence`]), so the value this receives is
/// `0.0`. It is discarded one instruction after the call. An accuracy is an
/// *input* to a service that does not exist, so there is nothing for it to
/// configure, and echoing a number back would claim a positioning request had
/// been made.
///
/// # Safety
///
/// Called by the runtime with a live `CLLocationManager`.
unsafe extern "C" fn set_desired_accuracy(_this: Id, _sel: Sel, _metres: f64) {
    absence::say(
        "-[CLLocationManager setDesiredAccuracy:]",
        "the accuracy is not stored and nothing is configured, because it is an input to a \
         positioning service rather than a setting of its own",
    );
}

/// `-[CLLocationManager startUpdatingLocation]`
///
/// **Void, and no position will ever be delivered.** This is the one that matters
/// most: a stub here that produced a position would be the exact failure this
/// project exists to avoid, and there is no position to produce - see
/// [`crate::absence`] for the four measurements. The guest's own source waits for
/// `-locationManager:didUpdateLocations:`, which will not come, and the run
/// continues correctly with the geolocation subsystem silent.
///
/// # Safety
///
/// Called by the runtime with a live `CLLocationManager`.
unsafe extern "C" fn start_updating_location(_this: Id, _sel: Sel) {
    absence::say(
        "-[CLLocationManager startUpdatingLocation]",
        "nothing was started and no position can be delivered, so \
         -[LocationManagerDelegate locationManager:didUpdateLocations:] will never be sent",
    );
}

/// `-[CLLocationManager stopUpdatingLocation]`
///
/// **Void, and that is a complete answer rather than a shrug.** Nothing is
/// running, so there is nothing to stop; the guest's own next action is to stop
/// the network-change timer (`0x83e7880`), which is C++ it owns and does not
/// depend on this.
///
/// # Safety
///
/// Called by the runtime with a live `CLLocationManager`.
unsafe extern "C" fn stop_updating_location(_this: Id, _sel: Sel) {
    absence::say(
        "-[CLLocationManager stopUpdatingLocation]",
        "nothing was stopped, because nothing was ever started",
    );
}

/// `-[CLLocationManager requestWhenInUseAuthorization]`
///
/// **Void, and no prompt was shown and no decision was reached.** On macOS this
/// is the message that would put the app's use of location services in front of
/// the user; there is no authorization service to ask and no user to ask. What
/// the guest does next is to keep reporting `kNotDetermined`
/// (`-[LocationManagerDelegate permissionInitialized]` is the guest's own field
/// and stays `NO`), which is the correct state for a permission that was never
/// asked about.
///
/// # Safety
///
/// Called by the runtime with a live `CLLocationManager`.
unsafe extern "C" fn request_when_in_use_authorization(_this: Id, _sel: Sel) {
    absence::say(
        "-[CLLocationManager requestWhenInUseAuthorization]",
        "nothing was prompted for and no decision was reached, so the guest's own \
         _permissionInitialized stays NO and the permission stays kNotDetermined",
    );
}
