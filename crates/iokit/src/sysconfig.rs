//! The three SystemConfiguration functions this guest imports, and the module
//! note for why they are here rather than in `corefoundation`.
//!
//! # Why this file exists, and why it is in `iokit`
//!
//! The Framework's own two-level namespace says where these three come from:
//! the unbound-import report attributes all of them to
//! `/System/Library/Frameworks/IOKit.framework/Versions/A/IOKit`, which is the
//! image's indirect symbol table speaking, not a guess from the name. IOKit
//! links SystemConfiguration, so a guest that reached `SCDynamicStore*` through
//! IOKit has those symbols attributed there.
//!
//! The rest of the `SCDynamicStore` family is already implemented in
//! `corefoundation::sysconfig`, which is the coherent home for it. Two crates
//! publishing one family is a wart, but splitting the *behaviour* across them
//! is worse: the two halves have to agree about what a store is, and
//! `SCDynamicStoreCreate` - which is what makes one - is in `corefoundation`.
//! So this file reads and writes the store that module made and says nothing
//! about how.
//!
//! # What is actually true on this host
//!
//! There is no `configd`, no SystemConfiguration daemon and no dynamic store
//! behind the name `SCDynamicStoreCreate` hands out. What there *is* is a store
//! object with keys this tree can answer ([`corefoundation::sysconfig`] answers
//! `SCDynamicStoreCopyProxies`, `SCDynamicStoreCopyValue` and
//! `SCDynamicStoreKeyCreateNetworkGlobalEntity`). So:
//!
//! - a key set can be recorded, and is;
//! - no key will ever change, because nothing here changes them;
//! - so there is no notification channel to hand out, and
//!   [`SCDynamicStoreCreateRunLoopSource`] declines.
//!
//! Those three statements are the whole model, and each answer below is one of
//! them. The asymmetry between the second and the third is forced by the guest,
//! and is verified rather than assumed - see the two functions.

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::Mutex;

use corefoundation::api as cf;
use corefoundation::base::CFTypeRef;

/// The keys each store has been asked to be notified about, as the store's own
/// address to the keys' text.
///
/// Keyed by the `SCDynamicStoreRef` pointer, which is stable for the life of
/// the store: `corefoundation::sysconfig::SCDynamicStoreCreate` allocates the
/// object and the caller only ever releases it, never moves it.
static KEYS: std::sync::LazyLock<Mutex<HashMap<usize, Vec<Vec<u8>>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// The keys `store` has been registered for, or an empty list.
pub fn notification_keys(store: CFTypeRef) -> Vec<Vec<u8>> {
    KEYS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(store as usize))
        .cloned()
        .unwrap_or_default()
}

/// `Boolean SCDynamicStoreSetNotificationKeys(SCDynamicStoreRef, CFArrayRef keys,
/// CFIndex keyCount)`.
///
/// **TRUE, and the keys are kept** - so the claim is literally true rather than
/// merely harmless. [`notification_keys`] is that record; a later
/// `SCDynamicStoreCopyProxies`-shaped query for the same key set would see
/// what it registered.
///
/// The alternative is not available. The guest *depends* on this answer, and
/// both of its call sites are fatal on false:
///
/// - `Framework+0x6752bbf` (`test al,al; je 0x6752e45`) in Chromium's
///   dynamic-store proxy-configuration watcher;
/// - `Framework+0x674ae57` (`test al,al; je 0x674ae8c`), where the false arm is
///   `int3` followed by `ud2` - a `CHECK` compiled to a trap.
///
/// So `false` is not a refusal the guest can act on, it is a crash. What makes
/// `true` honest is that it is not a claim about notifications at all: it is a
/// claim that the store accepted the key set, and it did. Whether it will ever
/// be *told* about a change is a separate question, and
/// [`SCDynamicStoreCreateRunLoopSource`] answers that one truthfully.
///
/// # Safety
///
/// `store` is an `SCDynamicStoreRef` this runtime made, and `keys` a
/// `CFArrayRef` of `CFStringRef`, or null for "no keys".
pub unsafe extern "C" fn SCDynamicStoreSetNotificationKeys(
    store: CFTypeRef,
    keys: CFTypeRef,
    key_count: corefoundation::base::CFIndex,
) -> bool {
    if store.is_null() {
        return false;
    }
    let count = if keys.is_null() {
        0
    } else {
        // SAFETY: the caller's `CFArrayRef`.
        let available = unsafe { cf::CFArrayGetCount(keys) };
        // `keyCount` is the caller's stated prefix; the array is the bound.
        key_count.clamp(0, available)
    };
    let mut recorded = Vec::with_capacity(count as usize);
    for i in 0..count {
        // SAFETY: `i` is below the count just clamped to the array's length.
        let key = unsafe { cf::CFArrayGetValueAtIndex(keys, i) };
        // SAFETY: a `CFStringRef` or null; a null key is recorded as no text.
        let text = unsafe { corefoundation::string::as_string(key) }
            .map(|s| s.utf8())
            .unwrap_or_default();
        recorded.push(text);
    }
    KEYS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(store as usize, recorded);
    true
}

/// `CFRunLoopSourceRef SCDynamicStoreCreateRunLoopSource(SCDynamicStoreRef,
/// CFIndex order)`.
///
/// **NULL, and null is the answer that says the right thing.** A run loop
/// source is how a notification is *delivered*, and there is nothing here that
/// can deliver one: no `configd` watches the keys
/// [`SCDynamicStoreSetNotificationKeys`] recorded, and nothing in this runtime
/// changes them. A source would be a real object that can never fire, and the
/// caller would sit on it believing it was registered for proxy changes.
///
/// The guest checks for null and declines cleanly - `test r15,r15; je
/// 0x67550da` at `Framework+0x6754f54`, which logs and returns. So this is a
/// feature-detect whose false arm the guest wrote, which is the case a truthful
/// refusal is for. It matches
/// [`crate::power::IOPSNotificationCreateRunLoopSource`], which declines for
/// the same reason.
///
/// # Safety
///
/// `store` is not dereferenced. The `order` is the order the caller wants
/// notifications in, and there are none to order.
pub unsafe extern "C" fn SCDynamicStoreCreateRunLoopSource(
    _store: CFTypeRef,
    _order: corefoundation::base::CFIndex,
) -> CFTypeRef {
    std::ptr::null_mut()
}

/// `SCError(SCStatus)`.
///
/// **The status, unchanged, as a 32-bit value** - and the reason is worth
/// stating because it is not what the SystemConfiguration header declares.
///
/// Apple declares `CFStringRef SCError(SCStatus)`. The guest does not use an
/// import of that shape, and two independent things in this tree say so:
///
/// - the machine code. `Framework+0x5094941` calls it and the next meaningful
///   instruction is `mov r12d, eax` - a **32-bit** store, which a compiler that
///   believed the return was a `CFStringRef` pointer could not emit. The value
///   is then passed straight to `SCErrorString` as an `SCStatus`
///   (`mov edi, r12d; call _SCErrorString`), and `SCErrorString` returns a
///   `const char *` that the guest `strlen`s. So this import reads as
///   `SCStatus`-shaped at both ends, and matching that is the only choice that
///   cannot fault.
/// - `crates/rosetta3/src/abi/table.rs`, which has `sig!("SCError", I32)` - one
///   argument, 32-bit return - next to `sig!("SCErrorString", P, I32)`.
///
/// Given a shape that takes a status and yields a status, **returning it
/// unchanged is the only truthful answer**: it invents nothing, destroys
/// nothing, and hands `SCErrorString` a real `SCStatus` to name. A
/// `CFStringRef` here would be a pointer the guest truncates to 32 bits and then
/// uses as a status, which is worse than useless.
///
/// The only place the value is used is a `VLOG(2)` block
/// (`Framework+0x5094946` guards it), so this runs only under verbose logging
/// about SystemConfiguration - and it is off the `--headless --dump-dom
/// about:blank` path with it.
pub extern "C" fn SCError(status: i32) -> i32 {
    status
}

exports::exports! {
    functions {
        "SCDynamicStoreSetNotificationKeys" => SCDynamicStoreSetNotificationKeys,
        "SCDynamicStoreCreateRunLoopSource" => SCDynamicStoreCreateRunLoopSource,
        "SCError" => SCError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use corefoundation::callbacks::Policy;
    use corefoundation::collections;

    /// The whole claim of `SCDynamicStoreSetNotificationKeys` is that the keys
    /// are held, so they have to be readable afterwards. If this were `true`
    /// with nothing recorded it would be the silent accept this project exists
    /// to avoid.
    #[test]
    fn keys_that_were_set_are_keys_that_can_be_read_back() {
        corefoundation::bootstrap();
        let key = corefoundation::string::from_bytes(b"State:/Network/Global/IPv4", false);
        let keys = collections::new_array(&[key], Policy::from_type_callbacks(), false);
        // SAFETY: a store address this test made up and an array it just made.
        let store = 0x1234_5678usize as CFTypeRef;
        // SAFETY: as documented.
        assert!(unsafe { SCDynamicStoreSetNotificationKeys(store, keys, 1) });
        assert_eq!(
            notification_keys(store),
            vec![b"State:/Network/Global/IPv4".to_vec()]
        );

        // SAFETY: a null array is "no keys", and a null store is refused.
        assert!(unsafe { SCDynamicStoreSetNotificationKeys(store, std::ptr::null_mut(), 0) });
        assert!(notification_keys(store).is_empty());
        assert!(!unsafe {
            SCDynamicStoreSetNotificationKeys(std::ptr::null_mut(), keys, 1)
        });

        // SAFETY: both are objects this test made.
        unsafe {
            cf::CFRelease(keys);
            cf::CFRelease(key);
        }
        // Leave nothing behind for the next test in this binary to find.
        KEYS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(store as usize));
    }

    /// `keyCount` is the caller's stated prefix and the array is the bound: a
    /// count larger than the array must not read past it.
    #[test]
    fn a_generous_key_count_does_not_read_past_the_array() {
        corefoundation::bootstrap();
        let key = corefoundation::string::from_bytes(b"Proxies", false);
        let keys = collections::new_array(&[key], Policy::from_type_callbacks(), false);
        let store = 0x9abc_def0usize as CFTypeRef;
        // SAFETY: an array of one and a count of a hundred; the clamp is the
        // point.
        assert!(unsafe { SCDynamicStoreSetNotificationKeys(store, keys, 100) });
        assert_eq!(notification_keys(store), vec![b"Proxies".to_vec()]);
        // SAFETY: an object this test made.
        unsafe {
            cf::CFRelease(keys);
            cf::CFRelease(key);
        }
        KEYS.lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(store as usize));
    }

    /// There is no notification channel here, so there is no source. The guest
    /// tests for exactly this at `Framework+0x6754f54`.
    #[test]
    fn there_is_no_dynamic_store_run_loop_source() {
        // SAFETY: nothing is dereferenced.
        assert!(unsafe { SCDynamicStoreCreateRunLoopSource(0x1234_5678usize as CFTypeRef, 0) }.is_null());
        assert!(unsafe { SCDynamicStoreCreateRunLoopSource(std::ptr::null_mut(), 0) }.is_null());
    }

    /// The codes this runtime actually produces must come back named, and an
    /// unnamed one must come back as its number rather than as a neighbour's
    /// message.
    #[test]
    fn an_error_status_comes_back_unchanged_for_the_string_that_names_it() {
        // The guest's shape is `SCErrorString(SCError(status))`, so the only
        // contract that can hold is that the status survives the trip. Anything
        // else here is a value invented for a log line.
        for code in [0, 1, 4, 5, 0x7fff_0001u32 as i32, i32::MIN] {
            assert_eq!(SCError(code), code, "{code:#x} must survive SCError");
        }
    }
}
