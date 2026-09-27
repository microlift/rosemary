//! What this host actually has, read from sysfs at the moment it is asked.
//!
//! This module is the *only* source of every non-nil answer in this crate. It
//! reads two things and nothing else:
//!
//! - `/sys/class/net/<if>/wireless` - present exactly when the network
//!   interface has a wireless PHY. This is the same test `iw dev` uses to
//!   decide what to print under `Interface`.
//! - `/sys/class/net/<if>/phy80211/rfkill*/{soft,hard}` - the radio switch for
//!   *that* interface. `/sys/class/net/wlp0s20f3/phy80211/rfkill3` is a real
//!   directory on this host, so the mapping from an interface to its own radio
//!   switch is a symlink walk rather than a guess.
//!
//! # Why these two and not anything else
//!
//! The predicate `-[CWInterface powerOn]` asks is "is the Wi-Fi radio turned
//! on", and on Darwin that is answered by `-[CWInterface setPower:]`'s inverse
//! - the same call `components/wifi/wifi_service_mac.mm` makes twice
//! (`setPower:NO` then `setPower:YES`, at `0x8f13b44` and `0x8f13ba4`). A radio
//! that has been switched off is what `rfkill` calls blocked, so rfkill is the
//! same fact under this kernel's name, and it is the only one of the two that is
//! about the *radio* rather than about a link.
//!
//! Deliberately **not** part of the predicate:
//!
//! - `operstate`. CoreWLAN's `powerOn` is about the radio, not about being
//!   associated: a Mac with Wi-Fi switched on and no network joined answers
//!   `YES`. Requiring an up interface would silently turn "radio on, not
//!   connected" into `NO`, which is a different question.
//! - `carrier`, `address`, `/proc/net/wireless`. Those are the *network's*
//!   properties - and the network's name, BSSID and signal are exactly what
//!   this crate refuses to answer. See the crate note.
//!
//! # Read live, never cached
//!
//! Every call re-reads sysfs. A cached answer would be a claim about when the
//! guest first asked rather than about now, and the guest asks on every
//! position update for the life of the process. The readings are four `stat`s
//! and two or three small file reads, which is less work than the `objc_msgSend`
//! that led here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// One wireless network interface, and whether its radio is switched on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    /// The kernel's name for it: `wlp0s20f3` on this host.
    pub name: String,
    /// `false` when this interface's own `rfkill` switch reports soft or hard
    /// blocked. `true` when no `rfkill` node is reachable for the interface *and*
    /// no `wlan` switch on the machine is blocked - the coarser reading, which
    /// only applies to a driver that does not expose `phy80211/rfkill*`.
    pub powered: bool,
}

/// Every wireless network interface this host has, in kernel order.
pub fn interfaces() -> Vec<Interface> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !has_wireless_phy(&path) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let powered = radio_powered(&path);
        out.push(Interface { name, powered });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Whether this machine's Wi-Fi radio is on.
///
/// A machine-level statement on purpose, and the reason is in the module note: a
/// Mac has one Wi-Fi radio, and `powerOn` is a statement about that radio. The
/// per-interface reading in [`Interface::powered`] is the same fact asked of one
/// interface, and [`announce`] prints both counts so a run's log says whether
/// they could have differed.
pub fn radio_on(interfaces: &[Interface]) -> bool {
    if !interfaces.is_empty() && interfaces.iter().all(|i| i.powered) {
        return true;
    }
    !wlan_switch_blocked()
}

/// The one line a run prints, so the log carries the measurement and not only
/// the conclusion.
pub fn announce(interfaces: &[Interface], powered: bool) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if SAID.swap(true, Ordering::Relaxed) {
        return;
    }
    let names: Vec<&str> = interfaces.iter().map(|i| i.name.as_str()).collect();
    eprintln!(
        "rosetta3: corewlan: {} wireless interface(s) [{}]; Wi-Fi radio {} - read live from \
         /sys/class/net and /sys/class/rfkill, not assumed. CoreWLAN's Apple-sourced state (the \
         scan, bssid, ssid, rssiValue, wlanChannel, noiseMeasurement, supportsSecurity:, setPower:, \
         setPairwiseMasterKey:error:, associateToNetwork:password:error:) has no counterpart on \
         this host and is refused by name, one selector at a time",
        interfaces.len(),
        names.join(", "),
        if powered { "is on" } else { "is off or absent" },
    );
}

/// `/sys/class/net/<if>/wireless` exists iff the interface has a wireless PHY.
fn has_wireless_phy(interface: &Path) -> bool {
    interface.join("wireless").is_dir()
}

/// The radio switch belonging to *this* interface, or the machine's, in that
/// order. Both are documented in the module note; the order is not a guess,
/// because the per-interface node is a real symlink into `phy80211`.
fn radio_powered(interface: &Path) -> bool {
    if let Some(powered) = own_rfkill(interface) {
        return powered;
    }
    !wlan_switch_blocked()
}

/// `None` when the driver exposes no `rfkill` node under this interface's phy.
fn own_rfkill(interface: &Path) -> Option<bool> {
    let phy = interface.join("phy80211");
    let entries = std::fs::read_dir(&phy).ok()?;
    let mut any = false;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("rfkill") {
            continue;
        }
        any = true;
        if blocked(&phy.join(name)) {
            return Some(false);
        }
    }
    any.then_some(true)
}

/// Whether any `wlan` rfkill switch on the machine is in the off position.
fn wlan_switch_blocked() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/rfkill") else {
        return false;
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| read_trimmed(&path.join("type")).as_deref() == Some("wlan"))
        .any(|path| blocked(&path))
}

/// An rfkill node is blocked when either switch says so. A switch whose file
/// cannot be read is **not** treated as blocked: rfkill files are world-readable
/// and a missing one is a kernel that does not implement the attribute, which is
/// not a measurement, and inventing a block out of it would be a refusal to look.
fn blocked(rfkill: &PathBuf) -> bool {
    read_trimmed(&rfkill.join("soft")).as_deref() == Some("1")
        || read_trimmed(&rfkill.join("hard")).as_deref() == Some("1")
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|text| text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The positive control the whole crate rests on, run against the real
    /// sysfs: this host's reading is whatever it is, and the code must report
    /// *that* rather than a constant.
    ///
    /// It asserts nothing about the answer, because the answer is a fact about
    /// the machine and this test must not become a claim that the machine has a
    /// radio. It asserts that the two predicates agree with the files they read,
    /// which is the part that could be wrong in the code.
    #[test]
    fn the_reading_agrees_with_the_files_it_reads() {
        let found = interfaces();
        for interface in &found {
            let path = PathBuf::from("/sys/class/net").join(&interface.name);
            assert!(
                path.join("wireless").is_dir(),
                "{} was reported without a wireless PHY",
                interface.name
            );
            assert_eq!(
                interface.powered,
                radio_powered(&path),
                "{}: the cached struct and a fresh read disagree",
                interface.name
            );
        }
        assert_eq!(
            radio_on(&found),
            found.iter().all(|i| i.powered) || !wlan_switch_blocked(),
            "radio_on must be exactly the predicate it documents"
        );
    }

    /// A negative with a positive control: `lo` has no `wireless` directory, so
    /// a scan that reported every interface would fail here.
    #[test]
    fn a_loopback_interface_is_not_a_wireless_interface() {
        if Path::new("/sys/class/net/lo/wireless").exists() {
            return; // nothing to say on a host where that is true
        }
        assert!(
            !interfaces().iter().any(|i| i.name == "lo"),
            "lo has no wireless PHY and must not be reported as one"
        );
    }
}
