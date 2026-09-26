use objc::object::Id;
use objc::sel::Sel;

use crate::define::class_methods;

#[repr(C)]
struct Body {
    isa: *const objc::class::Class,
    kind: usize,
}

const URL_PATH: usize = 1;

/// The class the standard sets this module hands out are instances of.
///
/// `NSCharacterSet` itself is CoreFoundation's, and it is claimed by
/// `corefoundation::charset` - as an *abstract* class, with room for its `isa`
/// and nothing else - before this crate's `install()` ever runs, because
/// `foundation::bootstrap()` calls `corefoundation::bootstrap()` first. A class's
/// instance size is fixed at registration and cannot be grown afterwards
/// (`class_add_ivar` refuses a registered class), so putting the extra word on
/// `NSCharacterSet` itself would mean allocating sixteen bytes into an
/// eight-byte class.
///
/// So the word goes on a subclass, which is the shape every other Foundation
/// class in this tree takes - `__NSCFNumber` under `NSNumber`, `__NSCFTimeZone`
/// under `NSTimeZone`, `__NSCFCharacterSet` under `NSCharacterSet` - and which
/// keeps the answer an `NSCharacterSet` to `isKindOfClass:` and to a guest's
/// `_OBJC_CLASS_$_NSCharacterSet` binding.
fn set_class() -> &'static objc::class::Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        let base = corefoundation::base::abstract_class(c"NSCharacterSet", None);
        objc::class::ptr(
            corefoundation::base::register(
                c"__NSFURLPathCharacterSet",
                Some(base),
                size_of::<Body>(),
                false,
            ),
        ) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const objc::class::Class) }.expect("a set class")
}

pub(crate) unsafe extern "C" fn url_path_allowed_character_set(_class: Id, _sel: Sel) -> Id {
    static SET: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SET.get_or_init(|| {
        let class = set_class();
        // SAFETY: `Body` begins with `isa` and is the class's registered size.
        let object: *mut Body = unsafe { corefoundation::base::allocate(class) };
        // SAFETY: a newly allocated instance whose payload belongs to us.
        unsafe { (*object).kind = URL_PATH };
        // Process-lifetime singleton, as Foundation's standard sets are.
        objc::arc::retain(object as Id);
        object as usize
    }) as Id
}

/// Whether `set` is this module's URL-path set and it admits `byte`.
///
/// Only this module's own class is read. The previous test was
/// `is_kind_of(NSCharacterSet)`, which admitted CoreFoundation's
/// `__NSCFCharacterSet` too - and that class is `{ isa, contents }`, so the word
/// read as a `kind` was a *pointer*. A guest may pass any character set here
/// (`-stringByAddingPercentEncodingWithAllowedCharacters:` and
/// `-trimmingCharactersInSet:` both do), so a set this module does not know the
/// contents of answers false rather than reading a pointer as a tag.
pub(crate) unsafe fn contains(set: Id, byte: u8) -> bool {
    if set.is_null() {
        return false;
    }
    // SAFETY: `set_class` is a class registered for the process.
    if !unsafe { corefoundation::base::is_kind_of(set.cast(), set_class()) } {
        return false;
    }
    // SAFETY: instances of `set_class` have `Body`'s layout.
    match unsafe { (*(set as *const Body)).kind } {
        URL_PATH => byte.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&byte),
        _ => false,
    }
}

pub(crate) fn install() {
    let class = corefoundation::base::abstract_class(c"NSCharacterSet", None);
    class_methods!(class, {
        "URLPathAllowedCharacterSet" => url_path_allowed_character_set, c"@16@0:8"
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_url_path_set_is_a_stable_object() {
        crate::bootstrap();
        // SAFETY: a nullary class method returning an object.
        let first = unsafe {
            url_path_allowed_character_set(
                objc::class::ptr(objc::class::lookup(c"NSCharacterSet").unwrap()) as Id,
                objc::sel::get("URLPathAllowedCharacterSet"),
            )
        };
        let second = unsafe {
            url_path_allowed_character_set(
                objc::class::ptr(objc::class::lookup(c"NSCharacterSet").unwrap()) as Id,
                objc::sel::get("URLPathAllowedCharacterSet"),
            )
        };
        assert!(!first.is_null());
        assert_eq!(first, second);
    }

    /// The word this module stores must be reachable through the *class it
    /// registered for it*, which is the whole reason the set is a subclass: the
    /// test above only proves the object exists.
    #[test]
    fn the_set_reads_back_the_characters_it_admits() {
        crate::bootstrap();
        // SAFETY: a nullary class method returning an object.
        let set = unsafe {
            url_path_allowed_character_set(
                objc::class::ptr(objc::class::lookup(c"NSCharacterSet").unwrap()) as Id,
                objc::sel::get("URLPathAllowedCharacterSet"),
            )
        };
        // SAFETY: `set` is the singleton made just above.
        unsafe {
            assert!(contains(set, b'a'));
            assert!(contains(set, b'-'));
            assert!(!contains(set, b' '));
            assert!(!contains(set, b'%'));
        }
    }

    /// A guest may hand `-stringByAddingPercentEncodingWithAllowedCharacters:`
    /// any character set, including one this runtime did not make. Reading such
    /// a set's payload as this module's tag would answer from a pointer value.
    #[test]
    fn a_character_set_this_runtime_did_not_make_admits_nothing() {
        crate::bootstrap();
        // SAFETY: an `NSCharacterSet` CoreFoundation made, through its own
        // documented constructor.
        let theirs = unsafe {
            corefoundation::charset::CFCharacterSetGetPredefined(
                corefoundation::charset::Predefined::Whitespace.identifier() as isize,
            ) as Id
        };
        if theirs.is_null() {
            return;
        }
        // SAFETY: `theirs` is a live character set.
        assert!(!unsafe { contains(theirs, b'a') });
    }
}
