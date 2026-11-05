use std::ffi::{CStr, c_char};
use std::path::PathBuf;

use super::kern::*;
use super::space::{self, BOOTSTRAP, MACH_PORT_NULL, Name};
use super::wire::{self, Address};

/// `mach_port_t bootstrap_port` - the data symbol a program passes to every
/// call below.
///
/// A real one is a send right to launchd. This one is a name with nothing
/// behind it, checked for rather than sent to, because the calls that take it
/// are implemented here instead of being messages.
pub static BOOTSTRAP_PORT: Name = BOOTSTRAP;

/// The directory services register themselves in.
///
/// Private to this user: a service name is a rendezvous token, and another
/// user able to write one could answer a lookup with a port of their own.
fn namespace() -> Option<PathBuf> {
    server::location::private_runtime_directory("bootstrap").ok()
}

/// The file a service name maps to.
///
/// Hex, because a service name is an arbitrary string - `com.apple.x`, but also
/// anything with a `/` in it - and a name that escaped the directory would be a
/// service registering itself over a file elsewhere.
fn entry(service: &str) -> Option<PathBuf> {
    if service.is_empty() || service.len() > 128 {
        return None;
    }
    let mut file = String::with_capacity(service.len() * 2);
    for byte in service.as_bytes() {
        file.push_str(&format!("{byte:02x}"));
    }
    Some(namespace()?.join(file))
}

/// The address registered for `service`, if one is and its holder is alive.
fn registered(service: &str) -> Option<Address> {
    let text = std::fs::read_to_string(entry(service)?).ok()?;
    let address = u64::from_str_radix(text.trim(), 16).ok()?;
    wire::is_bound(address).then_some(address)
}

fn record(service: &str, address: Address) -> bool {
    let Some(path) = entry(service) else {
        return false;
    };
    std::fs::write(path, format!("{address:016x}")).is_ok()
}

/// # Safety
///
/// `service` is a NUL-terminated string, or null.
unsafe fn service_name(service: *const c_char) -> Option<String> {
    if service.is_null() {
        return None;
    }
    // SAFETY: the caller's NUL-terminated string.
    unsafe { CStr::from_ptr(service) }
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// `kern_return_t bootstrap_check_in(mach_port_t bp, const char *service,
/// mach_port_t *sp)`
///
/// The server half: creates the port and publishes it. On a Mac launchd may
/// already hold a receive right for a service declared in a plist and hand that
/// one over; nothing here has a plist, so the port is always fresh.
///
/// # Safety
///
/// `service` is a NUL-terminated string and `out` a writable `mach_port_t`.
pub unsafe extern "C" fn bootstrap_check_in(
    bp: Name,
    service: *const c_char,
    out: *mut Name,
) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    if bp != BOOTSTRAP {
        return KERN_INVALID_TASK;
    }
    // SAFETY: the caller's string.
    let Some(service) = (unsafe { service_name(service) }) else {
        return KERN_INVALID_ARGUMENT;
    };
    if namespace().is_none() {
        return BOOTSTRAP_NO_MEMORY;
    }
    if registered(&service).is_some() {
        return BOOTSTRAP_NAME_IN_USE;
    }

    let mut name = MACH_PORT_NULL;
    // SAFETY: a live local for the out-parameter.
    let rc =
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut name) };
    if rc != KERN_SUCCESS {
        return rc;
    }
    let Some(address) = space::address_of(name) else {
        return KERN_FAILURE;
    };
    if !record(&service, address) {
        space::destroy(name);
        return BOOTSTRAP_NO_MEMORY;
    }
    // SAFETY: the caller's `mach_port_t`.
    unsafe { out.write(name) };
    BOOTSTRAP_SUCCESS
}

/// `kern_return_t bootstrap_look_up(mach_port_t bp, const char *service,
/// mach_port_t *sp)`
///
/// The client half, and the one that answers with a **send** right: the caller
/// gets somewhere to send, not somewhere to listen.
///
/// # Safety
///
/// As [`bootstrap_check_in`].
pub unsafe extern "C" fn bootstrap_look_up(
    bp: Name,
    service: *const c_char,
    out: *mut Name,
) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    if bp != BOOTSTRAP {
        return KERN_INVALID_TASK;
    }
    // SAFETY: the caller's string.
    let Some(service) = (unsafe { service_name(service) }) else {
        return KERN_INVALID_ARGUMENT;
    };
    let Some(address) = registered(&service) else {
        return BOOTSTRAP_UNKNOWN_SERVICE;
    };
    // SAFETY: the caller's `mach_port_t`.
    unsafe { out.write(space::insert_send(address)) };
    BOOTSTRAP_SUCCESS
}

/// `kern_return_t bootstrap_look_up2(mach_port_t, const char *, mach_port_t *,
/// pid_t, uint64_t flags)` - the same lookup with a target process and flags
/// that only matter to launchd.
///
/// # Safety
///
/// As [`bootstrap_look_up`].
pub unsafe extern "C" fn bootstrap_look_up2(
    bp: Name,
    service: *const c_char,
    out: *mut Name,
    _target: i32,
    _flags: u64,
) -> i32 {
    // SAFETY: the caller's, as documented.
    unsafe { bootstrap_look_up(bp, service, out) }
}

/// `kern_return_t bootstrap_register(mach_port_t bp, const char *service,
/// mach_port_t sp)`
///
/// Publishes a port the caller already holds, rather than making one. Passing
/// `MACH_PORT_NULL` withdraws the registration, which is the spelling a server
/// uses on the way out.
///
/// # Safety
///
/// `service` is a NUL-terminated string.
pub unsafe extern "C" fn bootstrap_register(bp: Name, service: *const c_char, port: Name) -> i32 {
    if bp != BOOTSTRAP {
        return KERN_INVALID_TASK;
    }
    // SAFETY: the caller's string.
    let Some(service) = (unsafe { service_name(service) }) else {
        return KERN_INVALID_ARGUMENT;
    };
    if port == MACH_PORT_NULL {
        if let Some(path) = entry(&service) {
            let _ = std::fs::remove_file(path);
        }
        return BOOTSTRAP_SUCCESS;
    }
    let Some(address) = space::address_of(port) else {
        return KERN_INVALID_RIGHT;
    };
    if registered(&service).is_some_and(|existing| existing != address) {
        return BOOTSTRAP_NAME_IN_USE;
    }
    match record(&service, address) {
        true => BOOTSTRAP_SUCCESS,
        false => BOOTSTRAP_NO_MEMORY,
    }
}

/// `kern_return_t bootstrap_parent(mach_port_t bp, mach_port_t *parent)`
///
/// There is one namespace here, so it is its own parent.
///
/// # Safety
///
/// `out` is a writable `mach_port_t`.
pub unsafe extern "C" fn bootstrap_parent(bp: Name, out: *mut Name) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    if bp != BOOTSTRAP {
        return KERN_INVALID_TASK;
    }
    // SAFETY: the caller's `mach_port_t`.
    unsafe { out.write(BOOTSTRAP) };
    BOOTSTRAP_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A service name that no other test - or run - will collide with.
    fn unique(prefix: &str) -> std::ffi::CString {
        std::ffi::CString::new(format!("{prefix}.{:016x}", wire::fresh_address())).unwrap()
    }

    #[test]
    fn a_service_that_checked_in_can_be_looked_up_and_sent_to() {
        let service = unique("com.rosemary.test.checkin");
        let mut server = MACH_PORT_NULL;
        // SAFETY: a NUL-terminated name and a live local for the out-parameter.
        let rc = unsafe { bootstrap_check_in(BOOTSTRAP, service.as_ptr(), &mut server) };
        assert_eq!(rc, BOOTSTRAP_SUCCESS);
        assert!(
            space::receiver(server).is_some(),
            "the server gets a receive right"
        );

        let mut client = MACH_PORT_NULL;
        let rc = unsafe { bootstrap_look_up(BOOTSTRAP, service.as_ptr(), &mut client) };
        assert_eq!(rc, BOOTSTRAP_SUCCESS);
        assert!(
            space::receiver(client).is_none(),
            "and the client only a send right"
        );
        assert_eq!(space::address_of(client), space::address_of(server));

        // A second check-in while the first holder is alive is refused.
        let mut again = MACH_PORT_NULL;
        let rc = unsafe { bootstrap_check_in(BOOTSTRAP, service.as_ptr(), &mut again) };
        assert_eq!(rc, BOOTSTRAP_NAME_IN_USE);

        space::destroy(server);
        space::mach_port_deallocate(space::TASK_SELF, client);
        // SAFETY: withdrawing the registration this test made.
        unsafe { bootstrap_register(BOOTSTRAP, service.as_ptr(), MACH_PORT_NULL) };
    }

    #[test]
    fn a_name_nothing_registered_is_unknown_rather_than_a_port_to_nowhere() {
        let service = unique("com.rosemary.test.absent");
        let mut port = MACH_PORT_NULL;
        // SAFETY: a NUL-terminated name and a live local for the out-parameter.
        let rc = unsafe { bootstrap_look_up(BOOTSTRAP, service.as_ptr(), &mut port) };
        assert_eq!(rc, BOOTSTRAP_UNKNOWN_SERVICE);
        assert_eq!(port, MACH_PORT_NULL);
    }

    /// The failure a directory of files has and a live server would not: a
    /// process that died leaves its name behind. The next check-in must take
    /// the name over rather than be told it is in use forever.
    #[test]
    fn a_registration_whose_port_is_gone_is_taken_over_rather_than_honoured() {
        let service = unique("com.rosemary.test.stale");
        let mut first = MACH_PORT_NULL;
        // SAFETY: a NUL-terminated name and a live local for the out-parameter.
        unsafe { bootstrap_check_in(BOOTSTRAP, service.as_ptr(), &mut first) };
        space::destroy(first);

        let mut port = MACH_PORT_NULL;
        assert_eq!(
            unsafe { bootstrap_look_up(BOOTSTRAP, service.as_ptr(), &mut port) },
            BOOTSTRAP_UNKNOWN_SERVICE
        );
        let mut second = MACH_PORT_NULL;
        assert_eq!(
            unsafe { bootstrap_check_in(BOOTSTRAP, service.as_ptr(), &mut second) },
            BOOTSTRAP_SUCCESS
        );

        space::destroy(second);
        // SAFETY: withdrawing the registration this test made.
        unsafe { bootstrap_register(BOOTSTRAP, service.as_ptr(), MACH_PORT_NULL) };
    }

    /// A service name is an arbitrary string and the namespace is a directory,
    /// so a name with a path in it must not escape it.
    #[test]
    fn a_service_name_cannot_name_a_file_outside_the_namespace() {
        let escaping = entry("../../etc/passwd").expect("a name, however hostile");
        assert_eq!(escaping.parent(), namespace().as_deref());
        assert!(entry("").is_none());
    }
}
