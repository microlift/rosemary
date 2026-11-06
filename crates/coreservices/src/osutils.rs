//! Toolbox Utilities' bit tests (`BitAnd`/`BitOr`/`BitTst`), the one
//! geographic-location record a Mac that never had its location set answers
//! with (`ReadLocation`), and Multiprocessing Services' critical regions - a
//! recursive mutex apiece, since nothing before Mac OS X's own threading
//! primitives ever needed more than that.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

type OSStatus = i32;

const NO_ERR: OSStatus = 0;
/// `paramErr`.
const PARAM_ERR: OSStatus = -50;
/// `kMPTimeoutErr`.
const MP_TIMEOUT_ERR: OSStatus = -29296;

/// `kDurationImmediate`: do not wait at all.
const K_DURATION_IMMEDIATE: i32 = 0;
/// `kDurationForever`.
const K_DURATION_FOREVER: i32 = 0x7fff_ffff;

/// `long BitAnd(long value1, long value2)`
pub extern "C" fn BitAnd(value1: i64, value2: i64) -> i64 {
    value1 & value2
}

/// `long BitOr(long value1, long value2)`
pub extern "C" fn BitOr(value1: i64, value2: i64) -> i64 {
    value1 | value2
}

/// `Boolean BitTst(const void *bytePtr, long bitNum)`.
///
/// Bits are numbered from 0, the high-order (leftmost) bit of the byte at
/// `bytePtr`, continuing through the following bytes - the numbering the
/// 68K's `BFTST` used, which this trap traces back to, and the one every
/// other classic bit-indexed structure (`KeyMap` included) still assumes.
///
/// # Safety
///
/// `byte_ptr` is null, or readable through byte `bit_num / 8`.
pub unsafe extern "C" fn BitTst(byte_ptr: *const c_void, bit_num: i64) -> u8 {
    if byte_ptr.is_null() || bit_num < 0 {
        return 0;
    }
    let byte_index = (bit_num / 8) as usize;
    let bit_in_byte = 7 - (bit_num % 8) as u32;
    // SAFETY: the caller's contract.
    let byte = unsafe { byte_ptr.cast::<u8>().add(byte_index).read() };
    u8::from(byte & (1 << bit_in_byte) != 0)
}

/// `MachineLocation { Fract latitude; Fract longitude; union { long gmtDelta;
/// struct { SInt8 pad[3]; SInt8 Delta; } dls; } u; }` - three `SInt32`s, laid
/// out exactly as the guest's.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MachineLocation {
    pub latitude: i32,
    pub longitude: i32,
    pub u: i32,
}

/// `void ReadLocation(MachineLocation *loc)`.
///
/// "If the geographic location record has never been set, all fields contain
/// 0" - Apple's own documented answer, and the only location this host could
/// ever have been told it is at.
///
/// # Safety
///
/// `loc` is null or writable for a `MachineLocation`.
pub unsafe extern "C" fn ReadLocation(loc: *mut MachineLocation) {
    if loc.is_null() {
        return;
    }
    // SAFETY: the caller's contract.
    unsafe { loc.write_unaligned(MachineLocation::default()) };
}

/// A live `MPCriticalRegionID`: a recursive mutex, as Apple's own
/// documentation describes `MPEnterCriticalRegion` ("may be entered
/// recursively by the same task").
struct CriticalRegion {
    state: Mutex<RegionState>,
    released: Condvar,
}

#[derive(Default)]
struct RegionState {
    /// The thread currently holding the region, and how many unmatched
    /// `MPEnterCriticalRegion` calls it has made.
    owner: Option<(std::thread::ThreadId, u32)>,
}

/// Live regions, by address, so [`MPDeleteCriticalRegion`] knows the
/// `MPCriticalRegionID` it was handed is one this module made rather than an
/// arbitrary pointer.
static REGIONS: Mutex<Option<HashMap<usize, ()>>> = Mutex::new(None);

/// `OSStatus MPCreateCriticalRegion(MPCriticalRegionID *criticalRegion)`
///
/// # Safety
///
/// `out` is null or writable for a pointer.
pub unsafe extern "C" fn MPCreateCriticalRegion(out: *mut *mut c_void) -> OSStatus {
    if out.is_null() {
        return PARAM_ERR;
    }
    let region = Box::into_raw(Box::new(CriticalRegion {
        state: Mutex::new(RegionState::default()),
        released: Condvar::new(),
    }));
    let mut held = REGIONS.lock().unwrap_or_else(|e| e.into_inner());
    held.get_or_insert_with(HashMap::default)
        .insert(region as usize, ());
    // SAFETY: checked non-null above.
    unsafe { *out = region.cast() };
    NO_ERR
}

/// `OSStatus MPDeleteCriticalRegion(MPCriticalRegionID criticalRegion)`
///
/// # Safety
///
/// `region` is a live [`MPCreateCriticalRegion`] result not yet deleted.
pub unsafe extern "C" fn MPDeleteCriticalRegion(region: *mut c_void) -> OSStatus {
    let mut held = REGIONS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(map) = held.as_mut() else {
        return PARAM_ERR;
    };
    if map.remove(&(region as usize)).is_none() {
        return PARAM_ERR;
    }
    // SAFETY: a `CriticalRegion` this module boxed, removed from `REGIONS`
    // exactly once, so this runs at most once per region.
    drop(unsafe { Box::from_raw(region.cast::<CriticalRegion>()) });
    NO_ERR
}

/// `timeout` as a `Duration` to actually wait, or `None` for
/// [`K_DURATION_FOREVER`] - positive is milliseconds, negative is the
/// absolute value in microseconds, and [`K_DURATION_IMMEDIATE`] (0) is
/// "don't wait at all", which a zero `Duration` already expresses.
fn wait_duration(timeout: i32) -> Option<Duration> {
    match timeout {
        K_DURATION_FOREVER => None,
        K_DURATION_IMMEDIATE => Some(Duration::ZERO),
        1.. => Some(Duration::from_millis(timeout as u64)),
        _ => Some(Duration::from_micros(i64::from(-timeout) as u64)),
    }
}

/// `OSStatus MPEnterCriticalRegion(MPCriticalRegionID criticalRegion,
/// Duration timeout)`
///
/// # Safety
///
/// `region` is a live [`MPCreateCriticalRegion`] result not yet deleted.
pub unsafe extern "C" fn MPEnterCriticalRegion(region: *mut c_void, timeout: i32) -> OSStatus {
    if region.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's contract.
    let region = unsafe { &*region.cast::<CriticalRegion>() };
    let this = std::thread::current().id();
    let mut state = region.state.lock().unwrap_or_else(|e| e.into_inner());
    let deadline = wait_duration(timeout).map(|d| std::time::Instant::now() + d);
    loop {
        match &mut state.owner {
            Some((owner, depth)) if *owner == this => {
                *depth += 1;
                return NO_ERR;
            }
            None => {
                state.owner = Some((this, 1));
                return NO_ERR;
            }
            Some(_) => {}
        }
        // The region is held by another thread: wait for it to release, or
        // answer the timeout - rechecked at the top of the loop rather than
        // trusted from `wait_timeout`'s own report, so a release that lands in
        // the same instant the deadline passes is still seen as a release.
        state = match deadline {
            None => region
                .released
                .wait(state)
                .unwrap_or_else(|e| e.into_inner()),
            Some(deadline) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    return MP_TIMEOUT_ERR;
                }
                let (next, _result) = region
                    .released
                    .wait_timeout(state, deadline - now)
                    .unwrap_or_else(|e| e.into_inner());
                next
            }
        };
    }
}

/// `OSStatus MPExitCriticalRegion(MPCriticalRegionID criticalRegion)`
///
/// # Safety
///
/// `region` is a live [`MPCreateCriticalRegion`] result currently held by the
/// calling thread (through [`MPEnterCriticalRegion`]).
pub unsafe extern "C" fn MPExitCriticalRegion(region: *mut c_void) -> OSStatus {
    if region.is_null() {
        return PARAM_ERR;
    }
    // SAFETY: the caller's contract.
    let region = unsafe { &*region.cast::<CriticalRegion>() };
    let this = std::thread::current().id();
    let mut state = region.state.lock().unwrap_or_else(|e| e.into_inner());
    match &mut state.owner {
        Some((owner, depth)) if *owner == this => {
            *depth -= 1;
            if *depth == 0 {
                state.owner = None;
                region.released.notify_one();
            }
            NO_ERR
        }
        // Not held by this thread - `paramErr` is the honest answer for a
        // call that does not balance an `MPEnterCriticalRegion`, rather than
        // silently releasing a region this thread never entered.
        _ => PARAM_ERR,
    }
}

exports::exports! {
    functions {
        "BitAnd" => BitAnd,
        "BitOr" => BitOr,
        "BitTst" => BitTst,
        "ReadLocation" => ReadLocation,
        "MPCreateCriticalRegion" => MPCreateCriticalRegion,
        "MPDeleteCriticalRegion" => MPDeleteCriticalRegion,
        "MPEnterCriticalRegion" => MPEnterCriticalRegion,
        "MPExitCriticalRegion" => MPExitCriticalRegion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_and_or_are_plain_arithmetic() {
        assert_eq!(BitAnd(0b1100, 0b1010), 0b1000);
        assert_eq!(BitOr(0b1100, 0b1010), 0b1110);
    }

    /// Bit 0 is the high-order bit of the first byte, counting on through
    /// the second - the classic Mac numbering, not a little-endian one.
    #[test]
    fn bit_tst_counts_from_the_high_bit_of_the_first_byte() {
        let bytes = [0b1000_0001_u8, 0b0000_0001_u8];
        // SAFETY: `bytes` is readable through its own length.
        unsafe {
            assert_eq!(BitTst(bytes.as_ptr().cast(), 0), 1);
            assert_eq!(BitTst(bytes.as_ptr().cast(), 1), 0);
            assert_eq!(BitTst(bytes.as_ptr().cast(), 7), 1);
            assert_eq!(BitTst(bytes.as_ptr().cast(), 15), 1);
            assert_eq!(BitTst(bytes.as_ptr().cast(), 8), 0);
        }
    }

    #[test]
    fn read_location_answers_all_zero() {
        let mut loc = MachineLocation {
            latitude: 1,
            longitude: 1,
            u: 1,
        };
        // SAFETY: `loc` is writable.
        unsafe { ReadLocation(&mut loc) };
        assert_eq!((loc.latitude, loc.longitude, loc.u), (0, 0, 0));
    }

    #[test]
    fn a_critical_region_is_recursive_on_the_same_thread() {
        let mut region = std::ptr::null_mut();
        // SAFETY: storage for an `MPCriticalRegionID`.
        assert_eq!(unsafe { MPCreateCriticalRegion(&mut region) }, NO_ERR);
        // SAFETY: `region` is live.
        unsafe {
            assert_eq!(MPEnterCriticalRegion(region, K_DURATION_FOREVER), NO_ERR);
            // Entering again from the same thread nests rather than blocks.
            assert_eq!(MPEnterCriticalRegion(region, K_DURATION_IMMEDIATE), NO_ERR);
            assert_eq!(MPExitCriticalRegion(region), NO_ERR);
            assert_eq!(MPExitCriticalRegion(region), NO_ERR);
            // Exiting past the matching enter count is refused.
            assert_eq!(MPExitCriticalRegion(region), PARAM_ERR);
            assert_eq!(MPDeleteCriticalRegion(region), NO_ERR);
        }
    }

    #[test]
    fn an_immediate_enter_times_out_against_another_thread() {
        let mut region = std::ptr::null_mut();
        // SAFETY: storage for an `MPCriticalRegionID`.
        assert_eq!(unsafe { MPCreateCriticalRegion(&mut region) }, NO_ERR);
        let region_addr = region as usize;
        let held = std::sync::Arc::new(std::sync::Barrier::new(2));
        let held2 = held.clone();
        let handle = std::thread::spawn(move || {
            let region = region_addr as *mut c_void;
            // SAFETY: `region` outlives this thread; the test joins it below
            // before deleting the region.
            unsafe { assert_eq!(MPEnterCriticalRegion(region, K_DURATION_FOREVER), NO_ERR) };
            held2.wait();
            held2.wait();
            // SAFETY: as above.
            unsafe { assert_eq!(MPExitCriticalRegion(region), NO_ERR) };
        });
        held.wait();
        // SAFETY: `region` is live and held by the other thread.
        assert_eq!(
            unsafe { MPEnterCriticalRegion(region, K_DURATION_IMMEDIATE) },
            MP_TIMEOUT_ERR
        );
        held.wait();
        handle.join().unwrap();
        // SAFETY: `region` is live and no longer held.
        unsafe { assert_eq!(MPDeleteCriticalRegion(region), NO_ERR) };
    }
}
