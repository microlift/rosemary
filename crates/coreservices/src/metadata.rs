//! Spotlight's metadata query API, `MDItem*`.
//!
//! There is no index here - this host has never run `mdworker` over anything -
//! so every item this tree hands out answers the way a file Spotlight has
//! never indexed does: it exists and names a path, and every attribute beyond
//! that is `NULL`, Apple's own answer for an attribute an item does not carry.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Mutex;

use corefoundation::base::CFTypeRef;

/// `kMDItem*` attribute names this tree publishes as globals, each a
/// `CFStringRef` whose text is its own name - see
/// `corefoundation::base::named_string_globals`.
///
/// Only the one a caller actually asks for by the literal name rather than by
/// this global; [`MDItemCopyAttribute`] answers `NULL` for it regardless, since
/// "when a file was last used" is exactly the kind of fact only a real
/// Spotlight index carries.
pub const ATTRIBUTE_NAMES: &[&str] = &["kMDItemLastUsedDate"];

pub fn data_symbols() -> Vec<(String, u64)> {
    corefoundation::base::named_string_globals(ATTRIBUTE_NAMES)
}

/// Every item [`MDItemCreate`] has handed out and not yet had collected, by
/// its own address.
///
/// Leaked for the life of the process, like every opaque handle in this tree -
/// `MDItemRef` has no matching "destroy" call for a caller to balance one
/// against.
static ITEMS: Mutex<Option<HashMap<usize, PathBuf>>> = Mutex::new(None);

/// `MDItemRef MDItemCreate(CFAllocatorRef allocator, CFStringRef path)`
///
/// `allocator` is accepted and ignored, as it is everywhere else in this tree.
///
/// # Safety
///
/// `path` is null or a `CFStringRef`.
pub unsafe extern "C" fn MDItemCreate(_allocator: CFTypeRef, path: CFTypeRef) -> CFTypeRef {
    // SAFETY: the caller's contract.
    let Some(path) = (unsafe { corefoundation::string::as_string(path) }) else {
        return std::ptr::null();
    };
    let path = PathBuf::from(String::from_utf8_lossy(&path.utf8()).into_owned());
    let handle: *mut c_void = Box::into_raw(Box::new(())).cast();
    corefoundation::base::note_opaque(handle);
    let mut held = ITEMS.lock().unwrap_or_else(|e| e.into_inner());
    held.get_or_insert_with(HashMap::new)
        .insert(handle as usize, path);
    handle.cast()
}

/// `CFTypeRef MDItemCopyAttribute(MDItemRef item, CFStringRef name)`
///
/// Every attribute beyond the item's own path and name is unanswered - see the
/// module note - which is `NULL`, not an error: Apple's documented answer for
/// an attribute an item does not carry is the same whether that is because the
/// file has it and was never indexed, or never had it at all.
///
/// # Safety
///
/// `item` is null or one [`MDItemCreate`] made; `name` is null or a
/// `CFStringRef`.
pub unsafe extern "C" fn MDItemCopyAttribute(item: CFTypeRef, name: CFTypeRef) -> CFTypeRef {
    let held = ITEMS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(path) = held.as_ref().and_then(|items| items.get(&(item as usize))) else {
        return std::ptr::null();
    };
    // SAFETY: the caller's contract.
    let Some(name) = (unsafe { corefoundation::string::as_string(name) }) else {
        return std::ptr::null();
    };
    let text = match String::from_utf8_lossy(&name.utf8()).as_ref() {
        "kMDItemFSName" => path.file_name().map(|n| n.to_string_lossy().into_owned()),
        // The same path [`MDItemCreate`] was given: this tree does no
        // translation of its own between that and a host path.
        "kMDItemPath" => Some(path.to_string_lossy().into_owned()),
        _ => None,
    };
    match text {
        Some(text) => corefoundation::string::from_bytes(text.as_bytes(), false),
        None => std::ptr::null(),
    }
}

exports::exports! {
    functions {
        "MDItemCreate" => MDItemCreate,
        "MDItemCopyAttribute" => MDItemCopyAttribute,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfstring(text: &str) -> CFTypeRef {
        corefoundation::string::from_bytes(text.as_bytes(), false)
    }

    fn text_of(value: CFTypeRef) -> Option<String> {
        // SAFETY: a `CFStringRef` this module made, or null.
        unsafe { corefoundation::string::as_string(value) }
            .map(|s| String::from_utf8_lossy(&s.utf8()).into_owned())
    }

    /// The item's own name comes back; an attribute no index here ever filled
    /// in does not.
    #[test]
    fn a_fresh_item_answers_its_name_and_nothing_indexed() {
        let path = cfstring("/tmp/example.txt");
        // SAFETY: a `CFStringRef` this test just made.
        let item = unsafe { MDItemCreate(std::ptr::null(), path) };
        assert!(!item.is_null());
        // SAFETY: an item this test just made, and a key naming a real one.
        let name = unsafe { MDItemCopyAttribute(item, cfstring("kMDItemFSName")) };
        assert_eq!(text_of(name), Some("example.txt".to_string()));
        // SAFETY: as above, for an attribute no index here has ever filled in.
        let used = unsafe { MDItemCopyAttribute(item, cfstring("kMDItemLastUsedDate")) };
        assert!(used.is_null());
    }

    /// An item nothing here made is not one, and its attributes are `NULL`
    /// rather than read out of whatever the pointer happens to be.
    #[test]
    fn a_foreign_pointer_has_no_attributes() {
        let bogus = std::ptr::without_provenance::<c_void>(0x1234).cast();
        // SAFETY: a pointer this module never handed out, read only as a table
        // key.
        let name = unsafe { MDItemCopyAttribute(bogus, cfstring("kMDItemFSName")) };
        assert!(name.is_null());
    }
}
