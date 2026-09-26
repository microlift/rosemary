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

/// `kCFTypeSetCallBacks`.
///
/// `CFSetCallBacks` and `CFDictionaryKeyCallBacks` are the same six fields in
/// the same order, so this is the same structure with the same five functions -
/// which is not an approximation but the two declarations Apple's `CFSet.h` and
/// `CFDictionary.h` actually make.
pub static TYPE_SET_CALLBACKS: KeyCallBacks = KeyCallBacks {
    version: 0,
    retain: Some(retain),
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
}

impl Policy {
    /// Null callbacks means "these are not objects": no retain, no release, and
    /// equality by pointer. That is Apple's behaviour and a guest relies on it
    /// when it puts integers in an array.
    pub const NONE: Policy = Policy {
        retain: None,
        release: None,
        equal: None,
    };

    /// The policy `kCFTypeArrayCallBacks` describes: retain, release, and
    /// compare with `CFEqual`. What a collection built through Foundation gets.
    pub fn from_type_callbacks() -> Policy {
        Policy {
            retain: TYPE_VALUE_CALLBACKS.retain,
            release: TYPE_VALUE_CALLBACKS.release,
            equal: TYPE_VALUE_CALLBACKS.equal,
        }
    }

    /// The same for keys, which differ only in also carrying a hash.
    pub fn from_key_callbacks() -> Policy {
        Policy {
            retain: TYPE_KEY_CALLBACKS.retain,
            release: TYPE_KEY_CALLBACKS.release,
            equal: TYPE_KEY_CALLBACKS.equal,
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

    pub fn equal(&self, a: CFTypeRef, b: CFTypeRef) -> bool {
        match self.equal {
            Some(f) => unsafe { f(a, b) != 0 },
            None => a == b,
        }
    }
}
