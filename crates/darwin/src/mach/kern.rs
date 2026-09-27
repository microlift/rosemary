#![cfg(not(target_os = "macos"))]

use std::ffi::c_char;

pub const KERN_SUCCESS: i32 = 0;
pub const KERN_INVALID_ADDRESS: i32 = 1;
pub const KERN_NO_SPACE: i32 = 3;
pub const KERN_INVALID_ARGUMENT: i32 = 4;
pub const KERN_FAILURE: i32 = 5;
pub const KERN_RESOURCE_SHORTAGE: i32 = 6;
pub const KERN_NOT_RECEIVER: i32 = 7;
pub const KERN_INVALID_NAME: i32 = 15;
pub const KERN_INVALID_TASK: i32 = 16;
pub const KERN_INVALID_RIGHT: i32 = 17;
pub const KERN_INVALID_VALUE: i32 = 18;
pub const KERN_UREFS_OVERFLOW: i32 = 19;
pub const KERN_INVALID_CAPABILITY: i32 = 20;
pub const KERN_RIGHT_EXISTS: i32 = 21;
pub const KERN_NOT_IN_SET: i32 = 23;
/// `KERN_NOT_SUPPORTED`, 46.
///
/// Darwin's own comment for it is "Empty thread activation (No thread linked to
/// it)", which is narrower than the uses it has; the meaning a caller should
/// read is "this kernel does not implement the operation", and that is the
/// answer `media_unavailable`'s `mach_make_memory_entry_64` gives. It is kept
/// next to `KERN_OPERATION_TIMED_OUT` (49) rather than in numeric order
/// because the whole block above is in the order `<mach/kern_return.h>`
/// declares, and this one is simply the last of that order that is used.
pub const KERN_NOT_SUPPORTED: i32 = 46;
pub const KERN_OPERATION_TIMED_OUT: i32 = 49;

/// The `mach_msg` half of the numbering. `0x1000_0xxx` is a send failure and
/// `0x1000_4xxx` a receive one, which is why they cannot be folded into the
/// `KERN_` values above.
pub const MACH_MSG_SUCCESS: i32 = 0;
pub const MACH_SEND_INVALID_DEST: i32 = 0x1000_0003u32 as i32;
pub const MACH_SEND_TIMED_OUT: i32 = 0x1000_0004u32 as i32;
pub const MACH_SEND_INVALID_DATA: i32 = 0x1000_0009u32 as i32;
pub const MACH_SEND_INVALID_RIGHT: i32 = 0x1000_000bu32 as i32;
pub const MACH_SEND_INVALID_TYPE: i32 = 0x1000_000fu32 as i32;
pub const MACH_SEND_NO_BUFFER: i32 = 0x1000_0013u32 as i32;
pub const MACH_SEND_TOO_LARGE: i32 = 0x1000_0014u32 as i32;
pub const MACH_RCV_INVALID_NAME: i32 = 0x1000_4002u32 as i32;
pub const MACH_RCV_TIMED_OUT: i32 = 0x1000_4003u32 as i32;
pub const MACH_RCV_TOO_LARGE: i32 = 0x1000_4004u32 as i32;
pub const MACH_RCV_BODY_ERROR: i32 = 0x1000_400eu32 as i32;

/// `bootstrap_*`, a third numbering again, and the one `bootstrap_strerror`
/// names.
pub const BOOTSTRAP_SUCCESS: i32 = 0;
pub const BOOTSTRAP_NAME_IN_USE: i32 = 1101;
pub const BOOTSTRAP_UNKNOWN_SERVICE: i32 = 1102;
pub const BOOTSTRAP_NO_MEMORY: i32 = 1105;

/// The message `mach_error_string` gives for `code`.
///
/// Only the codes this directory can produce are named. Anything else is
/// reported as its number rather than as a neighbouring message, for the reason
/// `darwin::errno` gives: a wrong name for a failure is harder to act on than no
/// name at all.
pub fn describe(code: i32) -> Option<&'static str> {
    Some(match code {
        KERN_SUCCESS => "(os/kern) successful",
        KERN_INVALID_ADDRESS => "(os/kern) invalid address",
        KERN_NO_SPACE => "(os/kern) no space available",
        KERN_INVALID_ARGUMENT => "(os/kern) invalid argument",
        KERN_FAILURE => "(os/kern) failure",
        KERN_RESOURCE_SHORTAGE => "(os/kern) resource shortage",
        KERN_NOT_RECEIVER => "(os/kern) not receiver",
        KERN_INVALID_NAME => "(os/kern) invalid name",
        KERN_INVALID_TASK => "(os/kern) invalid task",
        KERN_INVALID_RIGHT => "(os/kern) invalid right",
        KERN_INVALID_VALUE => "(os/kern) invalid value",
        KERN_UREFS_OVERFLOW => "(os/kern) urefs overflow",
        KERN_INVALID_CAPABILITY => "(os/kern) invalid capability",
        KERN_RIGHT_EXISTS => "(os/kern) right exists",
        KERN_NOT_IN_SET => "(os/kern) not in set",
        KERN_NOT_SUPPORTED => "(os/kern) not supported",
        KERN_OPERATION_TIMED_OUT => "(os/kern) operation timed out",
        MACH_SEND_INVALID_DEST => "(ipc/send) invalid destination port",
        MACH_SEND_TIMED_OUT => "(ipc/send) send timed out",
        MACH_SEND_INVALID_DATA => "(ipc/send) invalid data",
        MACH_SEND_INVALID_RIGHT => "(ipc/send) invalid port right",
        MACH_SEND_INVALID_TYPE => "(ipc/send) invalid type descriptor",
        MACH_SEND_NO_BUFFER => "(ipc/send) no message buffer available",
        MACH_SEND_TOO_LARGE => "(ipc/send) message too large",
        MACH_RCV_INVALID_NAME => "(ipc/rcv) invalid name",
        MACH_RCV_TIMED_OUT => "(ipc/rcv) receive timed out",
        MACH_RCV_TOO_LARGE => "(ipc/rcv) message too large",
        MACH_RCV_BODY_ERROR => "(ipc/rcv) bad message body",
        _ => return None,
    })
}

/// One `CString` per code handed out, kept forever.
///
/// `mach_error_string` returns a pointer the caller does not free and may print
/// at any later point, so the storage has to outlive the call. The set of codes
/// is small and closed, so the map cannot grow without bound.
static NAMED: std::sync::Mutex<Option<std::collections::HashMap<i32, &'static str>>> =
    std::sync::Mutex::new(None);

/// `char *mach_error_string(mach_error_t)`
pub extern "C" fn mach_error_string(code: i32) -> *const c_char {
    let mut guard = NAMED.lock().unwrap_or_else(|e| e.into_inner());
    let table = guard.get_or_insert_with(Default::default);
    let text: &'static str = table.entry(code).or_insert_with(|| {
        let owned = match describe(code) {
            Some(known) => format!("{known}\0"),
            None => format!("unknown error code {code:#x}\0"),
        };
        // Leaked on purpose: see [`NAMED`].
        Box::leak(owned.into_boxed_str())
    });
    text.as_ptr().cast()
}

/// `char *bootstrap_strerror(kern_return_t)`
pub extern "C" fn bootstrap_strerror(code: i32) -> *const c_char {
    let text = match code {
        BOOTSTRAP_SUCCESS => c"Success",
        BOOTSTRAP_NAME_IN_USE => c"Service name already exists",
        BOOTSTRAP_UNKNOWN_SERVICE => c"Unknown service name",
        BOOTSTRAP_NO_MEMORY => c"Out of memory",
        _ => return mach_error_string(code),
    };
    text.as_ptr()
}

/// `void mach_error(const char *prefix, mach_error_t)` - the printing form.
///
/// # Safety
///
/// `prefix` is a NUL-terminated string or null.
pub unsafe extern "C" fn mach_error(prefix: *const c_char, code: i32) {
    let described = mach_error_string(code);
    // SAFETY: `described` is one of this module's own NUL-terminated strings.
    let described = unsafe { std::ffi::CStr::from_ptr(described) };
    let prefix = match prefix.is_null() {
        true => std::borrow::Cow::Borrowed(""),
        // SAFETY: the caller's NUL-terminated string.
        false => unsafe { std::ffi::CStr::from_ptr(prefix) }.to_string_lossy(),
    };
    eprintln!("{prefix}{}", described.to_string_lossy());
}

/// The `errno` a failed socket call left, as the `mach_msg` code that means the
/// same thing.
///
/// A send to an address nothing is bound to is `ECONNREFUSED` here and
/// "destination port is dead" there, which is the one mapping a caller acts on:
/// it is how a client learns the other end has gone.
pub fn send_failure(errno: i32) -> i32 {
    match errno {
        libc::ECONNREFUSED | libc::ENOENT | libc::EPIPE | libc::ENOTCONN => MACH_SEND_INVALID_DEST,
        libc::EAGAIN | libc::ENOBUFS | libc::ENOMEM => MACH_SEND_NO_BUFFER,
        libc::EMSGSIZE => MACH_SEND_TOO_LARGE,
        _ => MACH_SEND_INVALID_DEST,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_code_keeps_the_same_pointer_and_an_unnamed_one_reports_its_number() {
        let first = mach_error_string(KERN_INVALID_NAME);
        let again = mach_error_string(KERN_INVALID_NAME);
        assert_eq!(first, again, "the storage is handed out once and kept");
        // SAFETY: a NUL-terminated string this module owns.
        let text = unsafe { std::ffi::CStr::from_ptr(first) };
        assert_eq!(text.to_str().unwrap(), "(os/kern) invalid name");

        let unknown = unsafe { std::ffi::CStr::from_ptr(mach_error_string(0x7fff_0001)) };
        assert!(unknown.to_str().unwrap().contains("0x7fff0001"));
    }

    /// The receive codes must stay in the `0x1000_4xxx` block: a caller tells a
    /// timeout from a real failure by the number, and the stub crash handler
    /// this exists for switches on exactly these three.
    #[test]
    fn the_receive_codes_are_the_ones_a_caller_switches_on() {
        assert_eq!(MACH_RCV_TIMED_OUT as u32, 0x1000_4003);
        assert_eq!(MACH_RCV_TOO_LARGE as u32, 0x1000_4004);
        assert_eq!(MACH_SEND_INVALID_DEST as u32, 0x1000_0003);
    }
}
