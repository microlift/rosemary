use std::ffi::c_void;

use crate::base::CFTypeRef;

/// `CFArrayCallBacks` and `CFDictionaryValueCallBacks`, which are the same five
/// words.
#[repr(C)]
pub struct ValueCallBacks {
    pub version: isize,
    pub retain: Option<unsafe extern "C" fn(*const c_void, CFTypeRef) -> CFTypeRef>,
    pub release: Option<unsafe extern "C" fn(*const c_void, CFTypeRef)>,
    pub copy_description: Option<unsafe extern "C" fn(CFTypeRef) -> CFTypeRef>,
    pub equal: Option<unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> u8>,
}

/// `CFDictionaryKeyCallBacks`: the same, plus a hash.
#[repr(C)]
pub struct KeyCallBacks {
    pub version: isize,
    pub retain: Option<unsafe extern "C" fn(*const c_void, CFTypeRef) -> CFTypeRef>,
    pub release: Option<unsafe extern "C" fn(*const c_void, CFTypeRef)>,
    pub copy_description: Option<unsafe extern "C" fn(CFTypeRef) -> CFTypeRef>,
    pub equal: Option<unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> u8>,
    pub hash: Option<unsafe extern "C" fn(CFTypeRef) -> usize>,
}

unsafe extern "C" fn retain(_allocator: *const c_void, value: CFTypeRef) -> CFTypeRef {
    // SAFETY: `kCFType*CallBacks` says the values are CoreFoundation objects.
    unsafe { crate::api::CFRetain(value) }
}

unsafe extern "C" fn release(_allocator: *const c_void, value: CFTypeRef) {
    unsafe { crate::api::CFRelease(value) }
}

unsafe extern "C" fn equal(a: CFTypeRef, b: CFTypeRef) -> u8 {
    unsafe { crate::api::CFEqual(a, b) }
}

unsafe extern "C" fn hash(value: CFTypeRef) -> usize {
    unsafe { crate::api::CFHash(value) }
}

/// `kCFTypeArrayCallBacks` and `kCFTypeDictionaryValueCallBacks`.
pub static TYPE_VALUE_CALLBACKS: ValueCallBacks = ValueCallBacks {
    version: 0,
    retain: Some(retain),
    release: Some(release),
    copy_description: None,
    equal: Some(equal),
};

/// `kCFTypeDictionaryKeyCallBacks`.
pub static TYPE_KEY_CALLBACKS: KeyCallBacks = KeyCallBacks {
    version: 0,
    retain: Some(retain),
    release: Some(release),
    copy_description: None,
    equal: Some(equal),
    hash: Some(hash),
};

unsafe extern "C" fn copy_string(_allocator: *const c_void, value: CFTypeRef) -> CFTypeRef {
    // SAFETY: `kCFCopyStringDictionaryKeyCallBacks` says the keys are strings.
    unsafe { crate::api::CFStringCreateCopy(std::ptr::null(), value) }
}

/// `kCFCopyStringDictionaryKeyCallBacks`: keys are copied rather than retained,
/// so a mutable string used as a key cannot change under the dictionary.
pub static COPY_STRING_KEY_CALLBACKS: KeyCallBacks = KeyCallBacks {
    version: 0,
    retain: Some(copy_string),
    release: Some(release),
    copy_description: None,
    equal: Some(equal),
    hash: Some(hash),
};

/// How a collection treats the things it holds, resolved from the two
/// structures a caller may or may not have passed.
#[derive(Clone, Copy)]
pub struct Policy {
    pub retain: Option<unsafe extern "C" fn(*const c_void, CFTypeRef) -> CFTypeRef>,
    pub release: Option<unsafe extern "C" fn(*const c_void, CFTypeRef)>,
    pub equal: Option<unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> u8>,
    pub hash: Option<unsafe extern "C" fn(CFTypeRef) -> usize>,
}

impl Policy {
    /// Null callbacks means "these are not objects": no retain, no release, and
    /// equality by pointer. That is Apple's behaviour and a guest relies on it
    /// when it puts integers in an array.
    pub const NONE: Policy = Policy {
        retain: None,
        release: None,
        equal: None,
        hash: None,
    };

    /// Retain, release, compare with `CFEqual` and hash with `CFHash`. What a
    /// collection built through Foundation gets, whose sets and keys use
    /// `-hash` as much as `-isEqual:`.
    pub fn from_type_callbacks() -> Policy {
        Policy::from_key_callbacks()
    }

    /// The policy `kCFTypeDictionaryKeyCallBacks` describes.
    pub fn from_key_callbacks() -> Policy {
        Policy {
            retain: TYPE_KEY_CALLBACKS.retain,
            release: TYPE_KEY_CALLBACKS.release,
            equal: TYPE_KEY_CALLBACKS.equal,
            hash: TYPE_KEY_CALLBACKS.hash,
        }
    }

    /// # Safety
    ///
    /// `p` must be null or a live `CFArrayCallBacks`/`CFDictionaryValueCallBacks`.
    pub unsafe fn from_value(p: *const ValueCallBacks) -> Policy {
        if p.is_null() {
            return Policy::NONE;
        }
        let c = unsafe { &*p };
        Policy {
            retain: c.retain,
            release: c.release,
            equal: c.equal,
            hash: None,
        }
    }

    /// # Safety
    ///
    /// `p` must be null or a live `CFDictionaryKeyCallBacks`.
    pub unsafe fn from_key(p: *const KeyCallBacks) -> Policy {
        if p.is_null() {
            return Policy::NONE;
        }
        let c = unsafe { &*p };
        Policy {
            retain: c.retain,
            release: c.release,
            equal: c.equal,
            hash: c.hash,
        }
    }

    pub fn retain(&self, value: CFTypeRef) -> CFTypeRef {
        match self.retain {
            // SAFETY: a callback the caller supplied, called as it declared it.
            Some(f) => unsafe { f(std::ptr::null(), value) },
            None => value,
        }
    }

    pub fn release(&self, value: CFTypeRef) {
        if let Some(f) = self.release {
            unsafe { f(std::ptr::null(), value) };
        }
    }

    /// Identical pointers are equal without asking the callback, as Apple's
    /// collections do.
    pub fn equal(&self, a: CFTypeRef, b: CFTypeRef) -> bool {
        a == b || self.equal.is_some_and(|f| unsafe { f(a, b) != 0 })
    }

    /// No hash callback means the pointer is the hash, as Apple's does.
    pub fn hash(&self, value: CFTypeRef) -> usize {
        match self.hash {
            // SAFETY: a callback the caller supplied, called as it declared it.
            Some(f) => unsafe { f(value) },
            None => value as usize,
        }
    }
}
