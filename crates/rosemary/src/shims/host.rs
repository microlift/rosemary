use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::abi::{AbiError, Result};

type HostSymbols = BTreeMap<&'static str, u64>;

static HOST_SYMBOLS: AtomicPtr<HostSymbols> = AtomicPtr::new(std::ptr::null_mut());
static HOST_SYMBOL_WRITER: Mutex<()> = Mutex::new(());

/// [`host_or_refuse`], except that the tree's own framework answers first when
/// the builtin frameworks are selected: a host function would be handed this
/// tree's objects, which it cannot read.
pub(crate) fn builtin_or_host(symbol: &str) -> Result<u64> {
    match crate::frameworks::symbol(darwin::c_name(symbol)) {
        Some(address) => Ok(address),
        None => host_or_refuse(symbol),
    }
}

/// Resolve a shim entry point, refusing by name when this host has no answer.
pub(crate) fn host_or_refuse(symbol: &str) -> Result<u64> {
    let name = darwin::c_name(symbol);

    let snapshot = HOST_SYMBOLS.load(Ordering::Acquire);
    if !snapshot.is_null() {
        // SAFETY: snapshots are leaked before publication and remain valid for
        // the lifetime of the process.
        if let Some(&address) = unsafe { &*snapshot }.get(name) {
            return Ok(address);
        }
    }

    let address =
        darwin::shim_symbol(name).ok_or_else(|| match darwin::divergent::reason(name) {
            Some(reason) => AbiError::Refused {
                symbol: symbol.to_string(),
                reason,
            },
            None => AbiError::NoShim {
                symbol: symbol.to_string(),
            },
        })?;

    // Publish a new immutable map. The lock is only on the uncommon successful
    // miss path; all later lookups use the atomic snapshot above.
    let _writer = HOST_SYMBOL_WRITER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = HOST_SYMBOLS.load(Ordering::Acquire);
    let mut next = if current.is_null() {
        HostSymbols::new()
    } else {
        // SAFETY: `current` points to a leaked snapshot.
        unsafe { (&*current).clone() }
    };
    if let Some(&cached) = next.get(name) {
        return Ok(cached);
    }
    next.insert(Box::leak(name.to_owned().into_boxed_str()), address);
    let published = Box::into_raw(Box::new(next));
    HOST_SYMBOLS.store(published, Ordering::Release);

    Ok(address)
}
