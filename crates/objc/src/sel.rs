use std::collections::HashSet;
use std::ffi::{CStr, CString, c_char};
use std::sync::{LazyLock, RwLock};

pub type Sel = *const c_char;

/// Every name registered so far, leaked so the pointers stay valid forever.
static TABLE: LazyLock<RwLock<HashSet<&'static CStr>>> =
    LazyLock::new(|| RwLock::new(HashSet::new()));

/// The selector for `name`, registering it if this is the first sighting.
pub fn register(name: &CStr) -> Sel {
    if let Some(found) = TABLE.read().unwrap_or_else(|e| e.into_inner()).get(name) {
        return found.as_ptr();
    }
    let mut table = TABLE.write().unwrap_or_else(|e| e.into_inner());
    // Re-checked under the write lock: two threads can miss the read.
    if let Some(found) = table.get(name) {
        return found.as_ptr();
    }
    let leaked: &'static CStr = Box::leak(CString::from(name).into_boxed_c_str());
    table.insert(leaked);
    leaked.as_ptr()
}

const RECENT_SLOTS: usize = 256;

thread_local! {
    static RECENT: [std::cell::Cell<(usize, usize, Sel)>; RECENT_SLOTS] =
        const { [const { std::cell::Cell::new((0, 0, std::ptr::null())) }; RECENT_SLOTS] };
}

/// The selector for a Rust string, for this crate's own classes.
///
/// Panics on an interior NUL, which only a bug here could produce.
pub fn get(name: &str) -> Sel {
    let address = name.as_ptr() as usize;
    let slot = (crate::address::mix((address ^ name.len()) as u64) >> 56) as usize % RECENT_SLOTS;
    let (at, length, sel) = RECENT.with(|recent| recent[slot].get());
    // SAFETY: a non-null cached selector is an interned, NUL-terminated name.
    if at == address
        && length == name.len()
        && !sel.is_null()
        && unsafe { CStr::from_ptr(sel) }.to_bytes() == name.as_bytes()
    {
        return sel;
    }
    let c = CString::new(name).expect("a selector name has no interior NUL");
    let sel = register(&c);
    RECENT.with(|recent| recent[slot].set((address, name.len(), sel)));
    sel
}

/// A selector's name, which is the selector.
pub fn name(sel: Sel) -> *const c_char {
    validate(sel);
    sel
}

pub(crate) fn validate(sel: Sel) {
    if crate::debug::enabled()
        && !sel.is_null()
        && !TABLE
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|known| known.as_ptr() == sel)
    {
        crate::fail!("objc debug: invalid selector handle {sel:p}");
    }
}

/// Whether two selectors are the same.
///
/// Pointer equality, because registration is what makes them equal. Null is
/// compared as a value so `sel_isEqual(NULL, NULL)` is true, as it is on Apple's.
pub fn is_equal(a: Sel, b: Sel) -> bool {
    validate(a);
    validate(b);
    a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reused_name_buffer_gets_its_new_selector() {
        let mut name = String::with_capacity(32);
        name.push_str("cacheProbeAlpha");
        let alpha = get(&name);
        name.clear();
        name.push_str("cacheProbeBravo");
        let bravo = get(&name);
        assert_ne!(alpha, bravo);
        assert_eq!(bravo, register(c"cacheProbeBravo"));
        assert_eq!(alpha, register(c"cacheProbeAlpha"));
    }

    #[test]
    fn the_same_name_registers_to_the_same_pointer() {
        let a = get("countByEnumeratingWithState:objects:count:");
        let b = get("countByEnumeratingWithState:objects:count:");
        assert_eq!(a, b);
        assert_ne!(a, get("count"));
    }

    #[test]
    fn a_selector_is_readable_as_its_own_name() {
        let sel = get("initWithBytes:length:encoding:");
        // SAFETY: `register` leaked a NUL-terminated copy of the name.
        let read = unsafe { CStr::from_ptr(name(sel)) };
        assert_eq!(read.to_str().unwrap(), "initWithBytes:length:encoding:");
    }
}
