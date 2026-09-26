#![allow(non_snake_case)]

//! `AuthorizationRef`: an empty right, and the absence of anyone to grant it.
//!
//! Two functions are real and four refuse, and the split is the honest one:
//!
//! - [`AuthorizationCreate`] succeeds, because `base::mac::authorization_util.mm:33`
//!   creates one with `rights = nullptr` and `kAuthorizationEmptyEnvironment` -
//!   that is, it asks for *no rights at all*, and an authorization with no rights
//!   is a handle to nothing. Handing back a real handle is therefore true rather
//!   than a courtesy.
//! - [`AuthorizationFree`] succeeds for the same reason, and because freeing a
//!   handle this module made is bookkeeping it can actually do.
//!
//! Everything that *exercises* a right refuses, and each refusal's status is one
//! whose documented meaning is true here. `errAuthorizationInteractionNotAllowed`
//! is `Authorization.h`'s own "Authorization was denied since interaction with the
//! user is not allowed" - and interaction with the user is not allowed on a
//! process with no user, no window server session, and no `securityd`.
//!
//! # Who calls these, and why none of it is on the milestone path
//!
//! - `base::mac::authorization_util.mm` - `CreateAuthorization`,
//!   `GetAuthorizationRightsWithPrompt`, `AuthorizationExecuteWithPrivilegesAndWait`.
//!   Used by the installer (`chrome/browser/mac/install_from_dmg.mm`) and by
//!   anything that escalates to root. `--headless --dump-dom` installs nothing.
//! - `chrome/browser/password_manager/password_manager_util_mac.mm` -
//!   `AuthorizationRightGet` / `AuthorizationRightSet`, for a
//!   `<bundle-id>.access-passwords` login right. Only reached when the password
//!   manager is asked to authenticate the user, which `about:blank` never asks.
//!
//! `AuthorizationExecuteWithPrivileges` is worth singling out: it runs a tool as
//! root. It is refused, and it is the one function in this crate where answering
//! "success" would be a *security* lie rather than merely an invented value.

use std::ffi::c_void;

use corefoundation::base::CFTypeRef;
use objc::class::Class;

use crate::status::{self, OSStatus};

/// An `AuthorizationRef`. It carries no rights, because none were asked for.
#[repr(C)]
struct Body {
    isa: *const Class,
    /// How many rights were requested, which is zero for every handle this
    /// module makes. Kept so the field is not an unexplained hole.
    rights: u32,
}

fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let pointer = *CLASS.get_or_init(|| {
        objc::class::ptr(corefoundation::base::register(
            c"__R3Authorization",
            None,
            size_of::<Body>(),
            true,
        )) as usize
    });
    // SAFETY: a class registered above and kept for the process.
    unsafe { objc::class::from_ptr(pointer as *const Class).expect("an authorization class") }
}

/// # Safety
///
/// `object` is null or an object this runtime allocated.
unsafe fn is_authorization(object: CFTypeRef) -> bool {
    // SAFETY: the caller's contract.
    unsafe { corefoundation::base::is_kind_of(object, class()) }
}

/// `OSStatus AuthorizationCreate(const AuthorizationRights *rights, const
/// AuthorizationEnvironment *environment, AuthorizationFlags flags,
/// AuthorizationRef *authorization)`
///
/// `errSecSuccess` and a real handle, for a request with no rights in it.
///
/// The guest's call, at `Framework+0x5d9fa05`, is
/// `AuthorizationCreate(/*rights=*/nullptr, kAuthorizationEmptyEnvironment,
/// kAuthorizationFlagDefaults, authorization.InitializeInto())` - so `rights` is
/// null and the answer is not an achievement: there is nothing in it to grant,
/// refuse or verify. A handle to nothing is exactly what was asked for.
///
/// # Safety
///
/// `rights` and `environment` are null or `AuthorizationRights`/
/// /`AuthorizationEnvironment` structures; `authorization` is null or writable.
pub unsafe extern "C" fn AuthorizationCreate(
    rights: *const c_void,
    _environment: *const c_void,
    _flags: u32,
    authorization: *mut CFTypeRef,
) -> OSStatus {
    if !authorization.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { authorization.write(std::ptr::null_mut()) };
    }
    if rights.is_null() {
        // SAFETY: `Body` is the registered layout.
        let body = unsafe { corefoundation::base::allocate::<Body>(class()) };
        if body.is_null() {
            return status::PARAM;
        }
        // SAFETY: a freshly allocated body.
        unsafe {
            (*body).rights = 0;
            authorization.write(body as CFTypeRef);
        }
        return status::AUTHORIZATION_SUCCESS;
    }
    // Rights *were* asked for, and this host has no way to say whether any of them
    // is granted, because there is nobody to grant them. Refusing is better than a
    // handle that would let the caller believe otherwise.
    status::refuse(
        "AuthorizationCreate",
        "rights were requested, and there is no Authorization Services on this host to say \
         whether they are granted: no login session, no `securityd`, no user to ask. A handle \
         returned here would stand for a grant that was never made.",
    );
    status::AUTHORIZATION_INTERACTION_NOT_ALLOWED
}

/// `OSStatus AuthorizationCopyRights(AuthorizationRef authorization, const
/// AuthorizationRights *rights, const AuthorizationEnvironment *environment,
/// AuthorizationFlags flags, AuthorizationRights **authorizedRights)`
///
/// Refused. Getting rights means asking a person, and there is no person.
///
/// `errAuthorizationInteractionNotAllowed` (`-60007`) - `Authorization.h`: "The
/// authorization was denied since no user interaction was possible" - which is
/// the truest sentence in Security for this host, and the one Chromium is quiet
/// about: `GetAuthorizationRightsWithPrompt` does
/// `if (status != errAuthorizationCanceled) { OSSTATUS_LOG(ERROR, status); }`
/// at `authorization_util.mm:97`.
///
/// `authorizedRights` is left null, because no subset of anything was authorized.
///
/// # Safety
///
/// `authorization` is null or an `AuthorizationRef`; the rest are null or
/// structures; `authorized_rights` is null or writable.
pub unsafe extern "C" fn AuthorizationCopyRights(
    _authorization: CFTypeRef,
    _rights: *const c_void,
    _environment: *const c_void,
    _flags: u32,
    authorized_rights: *mut *mut c_void,
) -> OSStatus {
    if !authorized_rights.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { authorized_rights.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "AuthorizationCopyRights",
        "obtaining a right means asking a person for it, and this process has no user to ask: \
         there is no login session and no window server. Interaction is not allowed, so \
         errAuthorizationInteractionNotAllowed rather than a handle that would stand for a \
         grant nobody made.",
    );
    status::AUTHORIZATION_INTERACTION_NOT_ALLOWED
}

/// `OSStatus AuthorizationExecuteWithPrivileges(AuthorizationRef authorization,
/// const char *pathToTool, AuthorizationFlags options, char *const *arguments,
/// FILE **communicationsPipe)`
///
/// **Refused, and this is the one refusal in the crate that would be a security
/// lie if it were not.** The function runs a tool as root. There is no root to
/// escalate to on this host, and no `securityd` to escalate through, so it cannot
/// be made to work; answering `errAuthorizationSuccess` would tell the caller a
/// privileged helper was launched, and `ExecuteWithPrivilegesAndGetPID` would then
/// read a PID out of a pipe nothing wrote to.
///
/// `errAuthorizationToolExecuteFailure` (`-60031`) rather than a made-up code:
/// `Authorization.h` defines it as "The specified program could not be executed",
/// which is exactly what happened - and the caller's only reaction to a non-zero
/// status is to return it.
///
/// `communicationsPipe` is left null so a caller that ignores the status does not
/// `fclose` an uninitialised `FILE *`.
///
/// # Safety
///
/// `authorization` is null or an `AuthorizationRef`; `path_to_tool` is null or a C
/// string; `arguments` is null or an array of C strings;
/// `communications_pipe` is null or writable.
pub unsafe extern "C" fn AuthorizationExecuteWithPrivileges(
    _authorization: CFTypeRef,
    _path_to_tool: *const u8,
    _options: u32,
    _arguments: *mut *const u8,
    communications_pipe: *mut *mut c_void,
) -> OSStatus {
    if !communications_pipe.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { communications_pipe.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "AuthorizationExecuteWithPrivileges",
        "this runs a tool as root, and there is nothing here to escalate to: no \
         Authorization Services, no `securityd`, no root-owned helper to launch. It is not \
         merely unimplemented - answering success would tell the caller a privileged process \
         had started, and it would then read a pid out of a pipe nothing wrote to.",
    );
    status::AUTHORIZATION_TOOL_EXECUTE_FAILURE
}

/// `OSStatus AuthorizationFree(AuthorizationRef authorization, AuthorizationFlags
/// flags)`
///
/// `errAuthorizationSuccess`, and the bookkeeping is real: a handle this module
/// made is released, so a guest that creates and frees in a loop does not grow
/// one object per iteration.
///
/// # Safety
///
/// `authorization` is null or an `AuthorizationRef` this module made.
pub unsafe extern "C" fn AuthorizationFree(authorization: CFTypeRef, _flags: u32) -> OSStatus {
    if authorization.is_null() {
        return status::PARAM;
    }
    // SAFETY: the caller's contract.
    if !unsafe { is_authorization(authorization) } {
        return status::CS_INVALID_OBJECT_REF;
    }
    // SAFETY: an object this module allocated, so `CFRelease` is its dealloc.
    unsafe { corefoundation::api::CFRelease(authorization) };
    status::AUTHORIZATION_SUCCESS
}

/// `OSStatus AuthorizationRightGet(const char *rightName, CFDictionaryRef
/// *rightDefinition)`
///
/// `errAuthorizationDenied` (`-60005`) and null - which `AuthorizationDB.h`
/// documents against this very function as "No definition found". A *right
/// definition* is a record in
/// `/var/db/authorizationdb`, describing what a named right demands and who may
/// perform it. There is no such database here and no way to hold a truthful
/// account of one, so there is no definition to return.
///
/// The guest's shape makes the consequence explicit and harmless:
/// `password_manager_util_mac.mm:34` reads this as "if the right already exists,
/// nothing to do", so a non-success sends it to `AuthorizationRightSet`, which
/// also fails, and `AuthenticateUser` returns false - i.e. "this user cannot be
/// asked to authorise", which is the truth.
///
/// # Safety
///
/// `right_name` is null or a C string; `right_definition` is null or writable.
pub unsafe extern "C" fn AuthorizationRightGet(
    _right_name: *const u8,
    right_definition: *mut CFTypeRef,
) -> OSStatus {
    if !right_definition.is_null() {
        // SAFETY: the caller states it is writable.
        unsafe { right_definition.write(std::ptr::null_mut()) };
    }
    status::refuse(
        "AuthorizationRightGet",
        "a right definition is a record in the authorization database saying what a named \
         right demands and who may perform it. This host has no such database, so there is \
         no definition of any right to return - and a made-up one would be an authority this \
         process could then appear to hold.",
    );
    status::AUTHORIZATION_DENIED
}

/// `OSStatus AuthorizationRightSet(AuthorizationRef authRef, const char *rightName,
/// CFTypeRef rightDefinition, CFStringRef descriptionKey, CFBundleRef bundle,
/// CFStringRef localeTableName)`
///
/// Refused, by name. This one *writes* a right into the authorization database,
/// which is the definition of a privilege the user would then appear to have
/// granted - the sharpest refusal in the set.
///
/// # Safety
///
/// `auth_ref` is null or an `AuthorizationRef`; `right_name` is null or a C
/// string; `right_definition` is null or an object; `description_key` and
/// `locale_table_name` are null or `CFString`s; `bundle` is null or a `CFBundle`.
pub unsafe extern "C" fn AuthorizationRightSet(
    _auth_ref: CFTypeRef,
    _right_name: *const u8,
    _right_definition: CFTypeRef,
    _description_key: CFTypeRef,
    _bundle: CFTypeRef,
    _locale_table_name: CFTypeRef,
) -> OSStatus {
    status::refuse(
        "AuthorizationRightSet",
        "this defines a new right in the authorization database - a privilege that a user \
         would thereafter appear to have granted. There is no such database on this host, and \
         recording a right nothing enforces would be the most consequential wrong answer in \
         the whole framework.",
    );
    status::AUTHORIZATION_DENIED
}

exports::exports! {
    functions {
        "AuthorizationCreate" => AuthorizationCreate,
        "AuthorizationCopyRights" => AuthorizationCopyRights,
        "AuthorizationExecuteWithPrivileges" => AuthorizationExecuteWithPrivileges,
        "AuthorizationFree" => AuthorizationFree,
        "AuthorizationRightGet" => AuthorizationRightGet,
        "AuthorizationRightSet" => AuthorizationRightSet,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pair `base::mac::authorization_util.mm` runs: create an
    /// authorization with no rights, then free it. Both succeed, and the handle
    /// in between is real.
    #[test]
    fn an_authorization_with_no_rights_in_it_is_real() {
        objc::bootstrap();
        let mut authorization: CFTypeRef = std::ptr::null();
        // SAFETY: `rights = nullptr` and `kAuthorizationEmptyEnvironment` are the
        // guest's own arguments at Framework+0x5d9fa05.
        assert_eq!(
            unsafe {
                AuthorizationCreate(
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    &mut authorization,
                )
            },
            status::AUTHORIZATION_SUCCESS
        );
        assert!(!authorization.is_null());
        // SAFETY: an authorization this test made.
        unsafe {
            assert!(is_authorization(authorization));
            // A handle that is live but is not an authorization is refused, which
            // is what keeps `AuthorizationFree` from freeing something it did not
            // make. (Freeing the *same* handle twice is a use-after-free and is
            // deliberately not tested.)
            let other = corefoundation::collections::new_data(vec![1, 2, 3]);
            assert_eq!(AuthorizationFree(other, 0), status::CS_INVALID_OBJECT_REF);
            corefoundation::api::CFRelease(other);
            assert_eq!(AuthorizationFree(authorization, 0), status::AUTHORIZATION_SUCCESS);
        }
        // SAFETY: null is the documented "no authorization" answer.
        assert_eq!(unsafe { AuthorizationFree(std::ptr::null(), 0) }, status::PARAM);
    }

    /// Rights asked for at creation time are refused: a handle would stand for a
    /// grant nobody made.
    #[test]
    fn creating_an_authorization_that_wants_rights_is_refused() {
        objc::bootstrap();
        let mut authorization: CFTypeRef = std::ptr::null();
        // A one-element `AuthorizationRights`, so the pointer is not null.
        let mut rights = [0u8; 16];
        // SAFETY: a readable structure, and a writable out-parameter.
        let status = unsafe {
            AuthorizationCreate(
                rights.as_mut_ptr().cast(),
                std::ptr::null(),
                0,
                &mut authorization,
            )
        };
        assert_ne!(status, status::AUTHORIZATION_SUCCESS);
        assert!(authorization.is_null(), "no handle stands for a grant");
    }

    /// Every right-exercising call refuses, leaves its out-parameters empty, and
    /// never claims success.
    #[test]
    fn every_right_exercising_call_refuses() {
        objc::bootstrap();
        let mut out: usize = 1;
        // SAFETY: null arguments throughout, which every one of these documents
        // as acceptable, and writable out-parameters.
        unsafe {
            let mut authorized: *mut c_void = 1_usize as *mut c_void;
            assert_eq!(
                AuthorizationCopyRights(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    &mut authorized
                ),
                status::AUTHORIZATION_INTERACTION_NOT_ALLOWED
            );
            assert!(authorized.is_null());

            let mut pipe: *mut c_void = 1_usize as *mut c_void;
            assert_eq!(
                AuthorizationExecuteWithPrivileges(
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                    std::ptr::null_mut(),
                    &mut pipe
                ),
                status::AUTHORIZATION_TOOL_EXECUTE_FAILURE
            );
            assert!(pipe.is_null(), "nothing will write to this pipe");

            let mut definition: CFTypeRef = 1_usize as CFTypeRef;
            assert_eq!(
                AuthorizationRightGet(std::ptr::null(), &mut definition),
                status::AUTHORIZATION_DENIED
            );
            assert!(definition.is_null());

            assert_eq!(
                AuthorizationRightSet(
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null()
                ),
                status::AUTHORIZATION_DENIED
            );
            let _ = &mut out;
        }
    }
}
