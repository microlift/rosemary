//! `NSURLCredential`, in its most common shape - a user name and a password.
//! Apple's class also carries an `NSURLCredential` built from a client
//! identity or a server trust, for certificate-based authentication; neither
//! is implemented here, since nothing in this tree has a TLS identity store
//! or a trust evaluator to hand one to.

use corefoundation::base::CFTypeRef;
use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

struct Body {
    /// Never read: see `sortdescriptor::Body`'s own note.
    #[allow(dead_code)]
    isa: *const Class,
    contents: *mut Contents,
}

struct Contents {
    /// Retained, or null for a credential with no user name.
    user: Id,
    /// Retained, or null for one with no password.
    password: Id,
    persistence: i64,
}

impl Drop for Contents {
    fn drop(&mut self) {
        objc::arc::release(self.user);
        objc::arc::release(self.password);
    }
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *CLASS.get_or_init(|| {
        let class = corefoundation::base::register(
            c"NSURLCredential",
            None,
            std::mem::size_of::<Body>(),
            false,
        );
        unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
            // SAFETY: reached with the last reference gone.
            unsafe {
                let body = receiver as *mut Body;
                if !(*body).contents.is_null() {
                    drop(Box::from_raw((*body).contents));
                    (*body).contents = std::ptr::null_mut();
                }
                objc::arc::finalize(receiver);
            }
        }
        class.replace_method(
            objc::sel::get("dealloc"),
            dealloc as *const std::ffi::c_void,
            c"v16@0:8",
        );
        objc::class::ptr(class) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(p as *const Class).expect("a URL credential class") }
}

/// # Safety
///
/// `receiver` must be null or an object this runtime allocated.
unsafe fn contents<'a>(receiver: Id) -> Option<&'a mut Contents> {
    if !unsafe { corefoundation::base::is_kind_of(receiver as CFTypeRef, class()) } {
        return None;
    }
    // SAFETY: an instance of `class()` is a `Body`.
    unsafe {
        let body = receiver as *mut Body;
        (!(*body).contents.is_null()).then(|| &mut *(*body).contents)
    }
}

fn make(user: Id, password: Id, persistence: i64) -> Id {
    // SAFETY: `class()`'s instances are `Body`.
    let body: *mut Body = unsafe { corefoundation::base::allocate(class()) };
    if body.is_null() {
        return body as Id;
    }
    // SAFETY: a live instance whose second word is ours to fill in.
    unsafe {
        (*body).contents = Box::into_raw(Box::new(Contents {
            user: objc::arc::retain(user),
            password: objc::arc::retain(password),
            persistence,
        }));
    }
    body as Id
}

unsafe extern "C" fn init_with_user_password_persistence(
    _receiver: Id,
    _sel: Sel,
    user: Id,
    password: Id,
    persistence: i64,
) -> Id {
    make(user, password, persistence)
}

unsafe extern "C" fn credential_with_user_password_persistence(
    _class: Id,
    _sel: Sel,
    user: Id,
    password: Id,
    persistence: i64,
) -> Id {
    // SAFETY: a fresh object.
    unsafe {
        corefoundation::api::CFAutorelease(make(user, password, persistence) as CFTypeRef) as Id
    }
}

unsafe extern "C" fn get_user(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSURLCredential`.
    unsafe { contents(receiver) }.map_or(std::ptr::null_mut(), |c| c.user)
}

unsafe extern "C" fn get_password(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: an `NSURLCredential`.
    unsafe { contents(receiver) }.map_or(std::ptr::null_mut(), |c| c.password)
}

unsafe extern "C" fn get_persistence(receiver: Id, _sel: Sel) -> i64 {
    // SAFETY: an `NSURLCredential`.
    unsafe { contents(receiver) }.map_or(0, |c| c.persistence)
}

unsafe extern "C" fn has_password(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: an `NSURLCredential`.
    unsafe { contents(receiver) }.is_some_and(|c| !c.password.is_null())
}

pub(crate) fn install() {
    let class = class();
    class_methods!(class, {
        "credentialWithUser:password:persistence:" => credential_with_user_password_persistence,
            c"@40@0:8@16@24q32";
    });
    methods!(class, {
        "initWithUser:password:persistence:" => init_with_user_password_persistence,
            c"@40@0:8@16@24q32";
        "user" => get_user, c"@16@0:8";
        "password" => get_password, c"@16@0:8";
        "persistence" => get_persistence, c"q16@0:8";
        "hasPassword" => has_password, c"B16@0:8";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_credential_holds_its_user_and_password() {
        crate::bootstrap();
        let user = crate::string::from_str("rose");
        let password = crate::string::from_str("secret");
        // SAFETY: two live strings.
        let credential = unsafe {
            init_with_user_password_persistence(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                user,
                password,
                0,
            )
        };
        // SAFETY: a live credential.
        unsafe {
            assert_eq!(get_user(credential, objc::sel::get("x")), user);
            assert_eq!(get_password(credential, objc::sel::get("x")), password);
            assert!(has_password(credential, objc::sel::get("x")));
        }
    }

    #[test]
    fn a_credential_with_no_password_reports_it_has_none() {
        crate::bootstrap();
        let user = crate::string::from_str("rose");
        // SAFETY: a live string and a null password.
        let credential = unsafe {
            init_with_user_password_persistence(
                std::ptr::null_mut(),
                objc::sel::get("x"),
                user,
                std::ptr::null_mut(),
                0,
            )
        };
        // SAFETY: a live credential.
        unsafe {
            assert!(!has_password(credential, objc::sel::get("x")));
        }
    }
}
