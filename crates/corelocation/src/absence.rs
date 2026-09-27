//! What is missing from this host, measured rather than assumed.
//!
//! CoreLocation is not a library this host is missing one entry point of. It is
//! a **client of an Apple daemon** - `locationd`, which fronts
//! `com.apple.pastelocation` - and the guest itself says so. Four measurements:
//! one from the guest, three from this machine, and together they turn "there is
//! no location service here" from a default into a reading.
//!
//! # 1. The guest names the daemon
//!
//! `com.apple.pastelocation` is in the Framework's own `__TEXT,__cstring`, at
//! **`0xd806bad`**, in the embedded sandbox profile - a `mach-lookup`
//! `global-name` allowance. A binary with no location provider to talk to, whose
//! own sandbox profile allows a lookup of the provider: a positive, from the
//! guest, about the subsystem this crate answers for. It is the same shape of
//! evidence `opendirectory` rests on its four
//! `com.apple.system.opendirectoryd.*` allowances.
//!
//! # 2. The daemons are not on this machine
//!
//! | what | measured |
//! |---|---|
//! | `locationd` | not on `PATH`; absent from a `find / -xdev -maxdepth 4`; no matching `systemd` unit |
//! | `PastelLocation` | same |
//! | `wifid` | same |
//! | a Mach bootstrap port | `/var/run/mach_port` does not exist, and libc exports no `bootstrap_look_up` |
//! | `/System`, `/Library` | do not exist |
//!
//! So there is no process that owns a position, and no transport either could
//! be delivered over.
//!
//! # 3. Wi-Fi is scannable here, and that is *not* the missing thing
//!
//! Worth separating, because it is the fact most likely to be read backwards.
//! This host has a wireless interface and can see access points:
//!
//! ```text
//! $ nmcli -t -f SSID,BSSID,SIGNAL dev wifi list | head -3
//! agustiani2_AP:22\:23\:51\:27\:B0\:C6:67
//! :20\:23\:51\:57\:B0\:C6:67
//! TP-Link_D0E7:C0\:06\:C3\:EA\:D0\:E7:47
//! ```
//!
//! So "there is no Wi-Fi" would be false. What is missing is the step *after*
//! the scan: turning an observed BSSID into a position needs Apple's Wi-Fi
//! location database and the daemon that queries it, and `/Library` does not
//! exist. A BSSID observed here is a fact about the radio; a latitude and
//! longitude derived from it would be an invention, and this crate does not make
//! one. That is also why `-[CLLocationManager startUpdatingLocation]` is refused
//! rather than wired to `nmcli`: the two halves are not interchangeable, and the
//! half that is missing is the half that produces a position.
//!
//! # 4. The guest links the framework and imports three names
//!
//! `LC_LOAD_DYLIB` at load-command offset **`0x20fb`** names
//! `/System/Library/Frameworks/CoreLocation.framework/Versions/A/CoreLocation`
//! - the Framework links it, immediately after CoreWLAN. And the whole import
//! set is **three data symbols and zero functions**, which is why
//! `corelocation::provide` is `None` and this crate's job is a class:
//!
//! | symbol | where it is bound | what this crate does about it |
//! |---|---|---|
//! | `_OBJC_CLASS_$_CLLocationManager` | `__objc_classrefs` at `0xe6e7a80` | **published here**, as a real class |
//! | `_kCLLocationAccuracyBest` | `__DATA_CONST,__got` at `0xdd51948` | already `darwin`'s named zeroed refusal |
//! | `_kCLLocationAccuracyHundredMeters` | `__DATA_CONST,__got` at `0xdd51940` | already `darwin`'s named zeroed refusal |
//!
//! The two accuracy constants are **deliberately not** re-published here, and the
//! reason is not that their values are unknowable - Apple's public
//! `CLLocationManager.h` states them (`-1.0` and `100.0`). The reason is that a
//! second definition of a name `darwin` already publishes is the
//! `no_framework_defines_a_symbol_another_one_does` failure, and that they are
//! inert where they are. The guest loads the *pointer* at `0x83e76a9` /
//! `0x83e76b2`, dereferences it at `0x83e76b9` (`movsd xmm0,[rax]`) and passes
//! the value to `-setDesiredAccuracy:`, whose result is discarded one
//! instruction later - so the value reaches a method that has no use for it,
//! because no position will ever be produced. A refused `0.0` and a refused
//! `-1.0` are the same answer here: neither is read back by anything.
//!
//! # The refusal, and what it is not
//!
//! [`say`] prints one line per selector, once, naming the selector, the missing
//! capability and the caller that needs it. It is not an abort: four of the five
//! selectors are `void` and the guest discards the return, and the fifth is a
//! property setter - the disassembly is in [`crate`]. A refusal that aborted
//! would be worse than the one it refuses, because it would stop a run with
//! nothing left to get wrong on this path.

use std::sync::Mutex;

/// The one reason every line in this crate gives, in one sentence.
///
/// In one place, because a set of refusals that each say something different is
/// a set nobody can read.
pub const WHY: &str = "there is no CoreLocation service on this host - the guest's own sandbox \
                   profile (0xd806bad) allows mach-lookup of com.apple.pastelocation, and \
                   locationd, PastelLocation and wifid are all absent, so no position can be \
                   produced and no authorization decision can be reached (Wi-Fi is scannable \
                   here; nothing on this host turns a BSSID into a position)";

/// Say once on stderr what this crate did with a selector, and why.
///
/// `claim` is what happened - which is not always "nothing", because
/// `-setDelegate:` is genuinely answered; [`WHY`] is appended so the reason is
/// said once, in one place, rather than five slightly different ways.
///
/// Once per `what`, because `-[CLLocationManager startUpdatingLocation]` is
/// reachable from every start/stop cycle in principle and a line that repeats
/// per cycle buries the rest of the run. This is the same discipline as
/// `opendirectory::refusal::say`, `corefoundation::base::refuse_once` and
/// `security::status::refuse`: a refusal that says nothing is indistinguishable
/// from a value.
pub fn say(what: &str, claim: &str) {
    use std::collections::HashSet;
    static SAID: Mutex<Option<HashSet<String>>> = Mutex::new(None);
    let fresh = SAID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_default()
        .insert(what.to_string());
    if fresh {
        eprintln!("rosetta3: corelocation: {what}: {claim}, because {WHY}.");
    }
}
