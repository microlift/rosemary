#![allow(non_snake_case)]

use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;

exports::exports! {
    functions {
        "NSSelectorFromString" => NSSelectorFromString,
        "NSStringFromSelector" => NSStringFromSelector,
        "NSClassFromString" => NSClassFromString,
        "NSStringFromClass" => NSStringFromClass,
        "NSFileTypeForHFSTypeCode" => NSFileTypeForHFSTypeCode,
        "NSUserName" => NSUserName,
        "NSFullUserName" => NSFullUserName,
        "NSSetUncaughtExceptionHandler" => NSSetUncaughtExceptionHandler,
        "NSGetUncaughtExceptionHandler" => NSGetUncaughtExceptionHandler,
        "CFCharacterSetGetPredefined" => crate::characterset::CFCharacterSetGetPredefined,
        "CFCharacterSetIsCharacterMember" => crate::characterset::CFCharacterSetIsCharacterMember,
        "CFCharacterSetIsLongCharacterMember" => crate::characterset::CFCharacterSetIsLongCharacterMember,
        "CFCharacterSetCreateMutable" => crate::characterset::CFCharacterSetCreateMutable,
        "CFCharacterSetAddCharactersInRange" => crate::characterset::CFCharacterSetAddCharactersInRange,
        "_NSDictionaryOfVariableBindings" => _NSDictionaryOfVariableBindings,
    }
}

/// The most variables `NSDictionaryOfVariableBindings` reads.
const MAX_BINDINGS: usize = 16;

/// `NSDictionary *_NSDictionaryOfVariableBindings(NSString *keys, id first, ...)`,
/// behind the `NSDictionaryOfVariableBindings(a, b, ...)` macro: `keys` is the
/// macro's arguments as written, comma separated, and the values follow.
///
/// Variadic, read as fixed parameters: on the System V ABI pointer-sized
/// variadic arguments travel exactly as named ones, and the parameters past
/// the caller's last are never read.
///
/// # Safety
///
/// `keys` is an NSString naming as many values as were passed.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn _NSDictionaryOfVariableBindings(
    keys: Id,
    v0: Id,
    v1: Id,
    v2: Id,
    v3: Id,
    v4: Id,
    v5: Id,
    v6: Id,
    v7: Id,
    v8: Id,
    v9: Id,
    v10: Id,
    v11: Id,
    v12: Id,
    v13: Id,
    v14: Id,
    v15: Id,
) -> Id {
    let values = [
        v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15,
    ];
    // SAFETY: an NSString.
    let text = String::from_utf8_lossy(&unsafe { crate::string::bytes(keys) }).into_owned();
    let names: Vec<&str> = text
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    if names.len() > MAX_BINDINGS {
        objc::fail!(
            "NSDictionaryOfVariableBindings: {} variables is more than the {MAX_BINDINGS} \
             this runtime reads out of a variadic call",
            names.len()
        );
    }
    let mut pairs = Vec::with_capacity(names.len());
    for (name, value) in names.iter().zip(values) {
        if value.is_null() {
            objc::fail!(
                "NSInvalidArgumentException: NSDictionaryOfVariableBindings: '{name}' is nil"
            );
        }
        pairs.push((crate::string::from_str(name), value));
    }
    let entries: Vec<(
        corefoundation::base::CFTypeRef,
        corefoundation::base::CFTypeRef,
    )> = pairs.iter().map(|(k, v)| (*k as _, *v as _)).collect();
    let dictionary = corefoundation::collections::new_dictionary(
        &entries,
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for (key, _) in pairs {
        objc::arc::release(key);
    }
    objc::arc::autorelease(dictionary as Id)
}

/// `NSString *NSUserName(void)`: the login name, which names `/Users/<name>`.
unsafe extern "C" fn NSUserName() -> Id {
    autoreleased(&prefix::layout::user_name())
}

/// `NSString *NSFullUserName(void)`: the first field of the passwd comment,
/// or the login name when that is empty, as Darwin falls back.
unsafe extern "C" fn NSFullUserName() -> Id {
    // SAFETY: `getpwuid` answers null or a record in a static buffer, read
    // before anything else can call it on this thread.
    let full = unsafe {
        let passwd = libc::getpwuid(libc::geteuid());
        match passwd.is_null() || (*passwd).pw_gecos.is_null() {
            true => None,
            false => std::ffi::CStr::from_ptr((*passwd).pw_gecos)
                .to_str()
                .ok()
                .and_then(|g| g.split(',').next())
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
        }
    };
    autoreleased(&full.unwrap_or_else(prefix::layout::user_name))
}

/// The handler `NSSetUncaughtExceptionHandler` was given, for the getter.
/// Kept, not called: an uncaught exception already ends the run here with its
/// reason and backtrace, which is what the handler of a game reports anyway.
static UNCAUGHT_HANDLER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `void NSSetUncaughtExceptionHandler(NSUncaughtExceptionHandler *)`
unsafe extern "C" fn NSSetUncaughtExceptionHandler(handler: *const std::ffi::c_void) {
    UNCAUGHT_HANDLER.store(handler as usize, std::sync::atomic::Ordering::Relaxed);
}

/// `NSUncaughtExceptionHandler *NSGetUncaughtExceptionHandler(void)`
unsafe extern "C" fn NSGetUncaughtExceptionHandler() -> *const std::ffi::c_void {
    UNCAUGHT_HANDLER.load(std::sync::atomic::Ordering::Relaxed) as *const std::ffi::c_void
}

/// The text of an `NSString` argument, or `None` when it is nil or not one.
///
/// # Safety
///
/// `string` is nil or an object this runtime allocated.
unsafe fn text(string: Id) -> Option<String> {
    if string.is_null() {
        return None;
    }
    let bytes = unsafe { crate::string::bytes(string) };
    String::from_utf8(bytes).ok()
}

/// An autoreleased `NSString`, as every `NSStringFrom...` returns.
fn autoreleased(text: &str) -> Id {
    let string = corefoundation::string::from_bytes(text.as_bytes(), false);
    // SAFETY: a fresh object this call owns.
    unsafe { corefoundation::api::CFAutorelease(string) as Id }
}

/// `SEL NSSelectorFromString(NSString *)`.
///
/// Registers the name rather than only finding it, which is Apple's behaviour:
/// a selector is its name, and one nothing has sent yet is still a selector.
unsafe extern "C" fn NSSelectorFromString(name: Id) -> Sel {
    // SAFETY: the caller passed an `NSString` or nil.
    let Some(name) = (unsafe { text(name) }) else {
        return std::ptr::null();
    };
    // A selector name with an interior NUL is not one; `objc::sel::get` would
    // rather panic than register half of it.
    if name.contains('\0') {
        return std::ptr::null();
    }
    objc::sel::get(&name)
}

/// `NSString *NSStringFromSelector(SEL)`.
unsafe extern "C" fn NSStringFromSelector(selector: Sel) -> Id {
    if selector.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a registered selector, whose name is a live NUL-terminated string.
    let name = unsafe { std::ffi::CStr::from_ptr(objc::sel::name(selector)) };
    autoreleased(&name.to_string_lossy())
}

/// `Class NSClassFromString(NSString *)`, nil for a class this tree has not
/// registered - which is what a guest checks before using a class it can do
/// without.
unsafe extern "C" fn NSClassFromString(name: Id) -> *const std::ffi::c_void {
    // SAFETY: the caller passed an `NSString` or nil.
    let Some(name) = (unsafe { text(name) }) else {
        return std::ptr::null();
    };
    let Ok(name) = std::ffi::CString::new(name) else {
        return std::ptr::null();
    };
    objc::class::lookup(&name)
        .map(|class| class as *const objc::class::Class as *const std::ffi::c_void)
        .unwrap_or(std::ptr::null())
}

/// `NSString *NSStringFromClass(Class)`.
unsafe extern "C" fn NSStringFromClass(class: *const objc::class::Class) -> Id {
    // SAFETY: a live class object, as the caller's `Class` is.
    let Some(class) = (unsafe { class.as_ref() }) else {
        return std::ptr::null_mut();
    };
    autoreleased(class.name_str())
}

/// `NSString *NSFileTypeForHFSTypeCode(OSType)`.
///
/// HFS types use the quoted four-byte spelling Foundation accepts as a file
/// type, with the most significant byte first. Studio asks for `'APPL'` while
/// classifying a workspace notification.
unsafe extern "C" fn NSFileTypeForHFSTypeCode(code: u32) -> Id {
    let bytes = code.to_be_bytes();
    let mut encoded = [0u8; 6];
    encoded[0] = b'\'';
    encoded[1..5].copy_from_slice(&bytes);
    encoded[5] = b'\'';
    let string = corefoundation::string::from_bytes(&encoded, false);
    // SAFETY: a fresh string returned by a non-create Foundation function.
    unsafe { corefoundation::api::CFAutorelease(string) as Id }
}

/// `-className`, the receiver's class name as an `NSString`.
///
/// Foundation declares it on `NSObject`, so every object answers it - which is
/// what a caller logging or dispatching on a type relies on. Autoreleased,
/// because the name it is built from is the class's and the string is not.
unsafe extern "C" fn class_name(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a live object.
    let name = match unsafe { objc::object::class_of(receiver) } {
        Some(class) => class.name_str().to_string(),
        // Nil has no class, and `-className` on it would not have been dispatched
        // - this is the unreachable branch rather than a guess.
        None => String::new(),
    };
    let string = crate::string::from_str(&name);
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id }
}

/// `-description` and `-debugDescription`, `<ClassName: 0xaddress>` as Foundation
/// spells them. Callers send these to any object without checking first (Qt's
/// `QDebug << id` does), so a missing one aborts the guest.
unsafe extern "C" fn description(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a live object.
    let name = unsafe { objc::object::class_of(receiver) }.map_or("nil", |c| c.name_str());
    let string = crate::string::from_str(&format!("<{name}: {receiver:p}>"));
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(string as corefoundation::base::CFTypeRef) as Id }
}

pub(crate) fn install() {
    let nsobject = objc::class::lookup(c"NSObject").expect("the runtime's own NSObject");
    methods!(nsobject, {
        "className" => class_name, c"@16@0:8";
        "description" => description, c"@16@0:8";
        "debugDescription" => description, c"@16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hfs_type_preserves_four_character_byte_order() {
        crate::bootstrap();
        // SAFETY: the returned object is a live Foundation string.
        let encoded = unsafe { NSFileTypeForHFSTypeCode(u32::from_be_bytes(*b"APPL")) };
        // SAFETY: `encoded` is the string created immediately above.
        assert_eq!(unsafe { crate::string::bytes(encoded) }, b"'APPL'");
    }

    #[test]
    fn description_names_the_class_unless_it_has_its_own() {
        crate::bootstrap();
        let base = objc::class::lookup(c"NSObject").expect("NSObject");
        let class = objc::class::allocate_pair(Some(base), c"DescriptionTestLayer", 0)
            .expect("a fresh class name");
        objc::class::register_pair(class);
        let object = objc::object::create_instance(class, 0);
        let string = crate::string::from_str("as written");
        // SAFETY: live objects; `-description` is `id (id, SEL)`.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc::msg::entry());
            for sel in ["description", "debugDescription"] {
                let text = crate::string::bytes(send(object, objc::sel::get(sel)));
                assert_eq!(
                    text,
                    format!("<DescriptionTestLayer: {object:p}>").as_bytes()
                );
            }
            let text = crate::string::bytes(send(string, objc::sel::get("description")));
            assert_eq!(text, b"as written");
        }
    }
}
