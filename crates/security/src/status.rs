//! `SecCopyErrorMessageString`: what an `OSStatus` from this framework means.
#![allow(non_snake_case)]

use corefoundation::base::CFTypeRef;

/// The text macOS gives each status this framework returns, as the host's own
/// `SecCopyErrorMessageString` spells it.
const MESSAGES: &[(i32, &str)] = &[
    (0, "No error."),
    (-4, "Function or operation not implemented."),
    (
        -50,
        "One or more parameters passed to a function were not valid.",
    ),
    (-108, "Failed to allocate memory."),
    (-128, "User canceled the operation."),
    (-25263, "No Trust Settings were found."),
    (-25299, "The specified item already exists in the keychain."),
    (
        -25300,
        "The specified item could not be found in the keychain.",
    ),
    (-34018, "A required entitlement is not present."),
    (
        -67050,
        "code failed to satisfy specified code requirement(s)",
    ),
    (-67052, "invalid or corrupted code requirement(s)"),
    (-67062, "code object is not signed at all"),
    (-67068, "cannot find code object on disk"),
    (-67069, "a required pointer argument was NULL"),
    (-67071, "invalid API object reference"),
    (-67072, "unimplemented code signing feature"),
];

/// The message for `status`.
///
/// FIXME (ACCURACY): a status outside [`MESSAGES`] is spelled `OSStatus <n>`,
/// which is what macOS answers only for a status it has no message for.
fn message(status: i32) -> String {
    match MESSAGES.iter().find(|(known, _)| *known == status) {
        Some((_, text)) => (*text).to_string(),
        None => format!("OSStatus {status}"),
    }
}

/// `CFStringRef SecCopyErrorMessageString(OSStatus status, void *reserved)`
pub extern "C" fn SecCopyErrorMessageString(status: i32, _reserved: *mut u8) -> CFTypeRef {
    corefoundation::string::from_bytes(message(status).as_bytes(), false)
}

exports::exports! {
    functions {
        "SecCopyErrorMessageString" => SecCopyErrorMessageString,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spells_an_unknown_status_by_number() {
        assert_eq!(message(-67062), "code object is not signed at all");
        assert_eq!(message(-12345), "OSStatus -12345");
    }
}
