/// Darwin's `errno` values, paired with this host's for the same name.
///
/// The first thirty-four agree and are still listed: the point of the table is
/// that it is the whole answer, and a reader should not have to know where the
/// agreement stops.
///
/// Darwin's `EPROCLIM`, `EBADRPC`, `ERPCMISMATCH`, `EPROGUNAVAIL`,
/// `EPROGMISMATCH`, `EPROCUNAVAIL`, `EFTYPE`, `EAUTH`, `ENEEDAUTH`, `EPWROFF`,
/// `EDEVERR`, `EBADEXEC`, `EBADARCH`, `ESHLIBVERS`, `EBADMACHO`, `ENOPOLICY`
/// and `EQFULL` are absent because this host has no counterpart to pair them
/// with. So is `ENOATTR` on Linux, which a shim reports with [`set_guest`].
#[cfg(not(target_os = "macos"))]
const NUMBERS: &[(i32, i32)] = &[
    (1, libc::EPERM),
    (2, libc::ENOENT),
    (3, libc::ESRCH),
    (4, libc::EINTR),
    (5, libc::EIO),
    (6, libc::ENXIO),
    (7, libc::E2BIG),
    (8, libc::ENOEXEC),
    (9, libc::EBADF),
    (10, libc::ECHILD),
    (11, libc::EDEADLK),
    (12, libc::ENOMEM),
    (13, libc::EACCES),
    (14, libc::EFAULT),
    (15, libc::ENOTBLK),
    (16, libc::EBUSY),
    (17, libc::EEXIST),
    (18, libc::EXDEV),
    (19, libc::ENODEV),
    (20, libc::ENOTDIR),
    (21, libc::EISDIR),
    (22, libc::EINVAL),
    (23, libc::ENFILE),
    (24, libc::EMFILE),
    (25, libc::ENOTTY),
    (26, libc::ETXTBSY),
    (27, libc::EFBIG),
    (28, libc::ENOSPC),
    (29, libc::ESPIPE),
    (30, libc::EROFS),
    (31, libc::EMLINK),
    (32, libc::EPIPE),
    (33, libc::EDOM),
    (34, libc::ERANGE),
    // Everything below here is renumbered.
    (35, libc::EAGAIN),
    (36, libc::EINPROGRESS),
    (37, libc::EALREADY),
    (38, libc::ENOTSOCK),
    (39, libc::EDESTADDRREQ),
    (40, libc::EMSGSIZE),
    (41, libc::EPROTOTYPE),
    (42, libc::ENOPROTOOPT),
    (43, libc::EPROTONOSUPPORT),
    (44, libc::ESOCKTNOSUPPORT),
    // Darwin has two names for it and this host has one: `ENOTSUP` and
    // `EOPNOTSUPP` are both 95 here. `ENOTSUP` is listed first so that it is the
    // one a host 95 comes back as, which is what Darwin's own libc returns for
    // an unsupported operation on a file.
    (45, libc::ENOTSUP),
    (46, libc::EPFNOSUPPORT),
    (47, libc::EAFNOSUPPORT),
    (48, libc::EADDRINUSE),
    (49, libc::EADDRNOTAVAIL),
    (50, libc::ENETDOWN),
    (51, libc::ENETUNREACH),
    (52, libc::ENETRESET),
    (53, libc::ECONNABORTED),
    (54, libc::ECONNRESET),
    (55, libc::ENOBUFS),
    (56, libc::EISCONN),
    (57, libc::ENOTCONN),
    (58, libc::ESHUTDOWN),
    (59, libc::ETOOMANYREFS),
    (60, libc::ETIMEDOUT),
    (61, libc::ECONNREFUSED),
    (62, libc::ELOOP),
    (63, libc::ENAMETOOLONG),
    (64, libc::EHOSTDOWN),
    (65, libc::EHOSTUNREACH),
    (66, libc::ENOTEMPTY),
    (68, libc::EUSERS),
    (69, libc::EDQUOT),
    (70, libc::ESTALE),
    (71, libc::EREMOTE),
    (77, libc::ENOLCK),
    (78, libc::ENOSYS),
    (84, libc::EOVERFLOW),
    (89, libc::ECANCELED),
    (90, libc::EIDRM),
    (91, libc::ENOMSG),
    (92, libc::EILSEQ),
    #[cfg(target_os = "freebsd")]
    (93, libc::ENOATTR),
    (94, libc::EBADMSG),
    (95, libc::EMULTIHOP),
    #[cfg(not(target_os = "freebsd"))]
    (96, libc::ENODATA),
    (97, libc::ENOLINK),
    #[cfg(not(target_os = "freebsd"))]
    (98, libc::ENOSR),
    #[cfg(not(target_os = "freebsd"))]
    (99, libc::ENOSTR),
    (100, libc::EPROTO),
    #[cfg(not(target_os = "freebsd"))]
    (101, libc::ETIME),
    // The second host 95, listed after `ENOTSUP` so it never wins `from_host`.
    (102, libc::EOPNOTSUPP),
    (104, libc::ENOTRECOVERABLE),
    (105, libc::EOWNERDEAD),
];

/// Darwin's `EINVAL`, and what a number with no Darwin name is reported as.
///
/// A host error this table cannot name is reported as `EINVAL` rather than kept
/// as its own number, because Darwin's table is dense below 106: passing, say,
/// this host's `EL2NSYNC` (45) through would name Darwin's `ENOTSUP`. An
/// unfamiliar number is no use to a guest either way; a familiar wrong one sends
/// it down a handling path it should not be on.
const DARWIN_EINVAL: i32 = 22;

/// This host's `errno` as Darwin's.
#[cfg(not(target_os = "macos"))]
pub fn from_host(host: i32) -> i32 {
    if host == 0 {
        return 0;
    }
    match NUMBERS.iter().find(|(_, ours)| *ours == host) {
        Some((theirs, _)) => *theirs,
        None => DARWIN_EINVAL,
    }
}

/// Identity on macOS, where the guest's numbers are the host's.
#[cfg(target_os = "macos")]
pub fn from_host(host: i32) -> i32 {
    host
}

/// Darwin numbers this host can stand for one way only: [`to_host`] gives the
/// nearest host number, and [`from_host`] keeps answering that number's own
/// Darwin name.
#[cfg(target_os = "linux")]
const TO_HOST_ONLY: &[(i32, i32)] = &[
    // `ENOATTR`: Linux reports a missing attribute as `ENODATA`.
    (93, libc::ENODATA),
];
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const TO_HOST_ONLY: &[(i32, i32)] = &[];

/// A guest's `errno` as this host's.
#[cfg(not(target_os = "macos"))]
pub fn to_host(darwin: i32) -> i32 {
    if darwin == 0 {
        return 0;
    }
    match NUMBERS
        .iter()
        .chain(TO_HOST_ONLY)
        .find(|(theirs, _)| *theirs == darwin)
    {
        Some((_, ours)) => *ours,
        None => libc::EINVAL,
    }
}

/// Identity on macOS, as [`from_host`].
#[cfg(target_os = "macos")]
pub fn to_host(darwin: i32) -> i32 {
    darwin
}

pub use starnix::errno::{get, location, set};

/// The guest's own `errno`, in Darwin's numbering, one per thread.
///
/// The host's slot cannot be handed over: the guest would read this host's
/// numbers out of it. So the guest gets a slot of its own, and [`error`]
/// reconciles the two each time it is called.
///
/// `Cell` rather than an atomic because it is per-thread by construction, and
/// `const`-initialised so that no destructor is registered - a `LocalKey::with`
/// after the thread's TLS has been torn down would otherwise panic inside a
/// guest's last `errno` read.
#[cfg(not(target_os = "macos"))]
mod guest_slot {
    use std::cell::Cell;

    thread_local! {
        /// What the guest sees, in Darwin's numbering.
        pub static VALUE: Cell<libc::c_int> = const { Cell::new(0) };
        /// The host number [`super::error`] last wrote into the host's slot, so
        /// a later call can tell "the host changed it" from "the guest did".
        pub static PUSHED: Cell<libc::c_int> = const { Cell::new(0) };
    }
}

/// `int *__error(void)`, as the guest imports it.
///
/// A real function rather than a rebound host symbol, so the guest gets the same
/// answer whichever spelling this host uses - and, off Darwin, so the number in
/// the slot is Darwin's rather than this host's.
///
/// ## Reconciling two slots
///
/// `errno` is a macro for `(*__error())`, so a C guest calls this immediately
/// before every read *and* every write. That is the whole hook available: the
/// write itself cannot be seen. Each call therefore does three things.
///
/// 1. If the host's slot differs from what this function last put there, a host
///    call changed it; the guest's slot is refreshed from it.
/// 2. Otherwise the guest's slot is left alone, which is what makes `errno = 0`
///    stick across an intervening host call that succeeded quietly.
/// 3. The guest's value is pushed back to the host's slot and remembered, so
///    that a shim in this crate reading [`get`] sees what the guest set.
///
/// The one case it cannot get right is a guest write and a host write between
/// the same pair of calls, where the host's wins. There is no `errno` read
/// between them, so nothing observes the difference until the next call, which
/// is exactly where a host error is the more useful answer.
unsafe extern "C" fn error() -> *mut libc::c_int {
    guest_location()
}

/// The slot a guest reads its `errno` out of, reconciled with the host's.
///
/// [`error`] is this under the name the guest imports. Public because the
/// thread-local storage a guest reaches `errno` through by another route -
/// Darwin's own pthread TSD, see [`crate::tls`] - has to name the same slot.
pub fn guest_location() -> *mut libc::c_int {
    #[cfg(target_os = "macos")]
    {
        location()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let host = get();
        guest_slot::PUSHED.with(|pushed| {
            guest_slot::VALUE.with(|value| {
                if host != pushed.get() {
                    value.set(from_host(host));
                }
                let now = to_host(value.get());
                pushed.set(now);
                set(now);
                value.as_ptr()
            })
        })
    }
}

/// Set the guest's `errno` to Darwin's `darwin`, for an error this host's own
/// number would name differently or not at all.
///
/// Both slots are written, and the host one is remembered as pushed, so the
/// next reconciliation keeps `darwin` rather than renaming it from the host's.
pub fn set_guest(darwin: i32) {
    #[cfg(target_os = "macos")]
    set(darwin);
    #[cfg(not(target_os = "macos"))]
    {
        let host = to_host(darwin);
        guest_slot::VALUE.with(|value| value.set(darwin));
        guest_slot::PUSHED.with(|pushed| pushed.set(host));
        set(host);
    }
}

/// The guest's `errno`, in Darwin's numbering.
///
/// What the guest would read; the reconciliation in [`error`] happens first, so
/// this is the same value a guest calling `__error` and dereferencing it gets.
#[inline(always)]
pub fn guest_errno() -> i32 {
    // SAFETY: `guest_location` returns a live slot for this thread.
    unsafe { *guest_location() }
}

/// The guest's `errno`, put back exactly as it was.
///
/// A host library that returns successfully is still entitled to leave `errno` wherever it likes, and a guest
/// that checks it after a call this runtime made on its behalf sees the runtime's failure rather than its
/// own.
pub struct Held(i32);

impl Held {
    #[inline(always)]
    pub fn now() -> Held {
        Held(get())
    }
}

impl Drop for Held {
    #[inline(always)]
    fn drop(&mut self) {
        set(self.0);
    }
}

// Both spellings, for one entry point. Darwin exports `___error` alone - the C
// identifier `__error` with Mach-O's underscore on top - but `base_name` strips
// exactly one underscore, and this is the rare identifier that begins with one
// of its own. So a caller holding the Mach-O name lands on `__error` and one
// holding the C name lands on `_error`, and only listing both answers both.
exports::exports! {
    functions {
        "_error" => error,
        "__error" => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slot_is_this_thread_s_own() {
        set(libc::EPIPE);
        let mine = location() as usize;
        let theirs = std::thread::spawn(|| {
            set(libc::ENOENT);
            location() as usize
        })
        .join()
        .expect("the thread reads its own errno");
        assert_ne!(mine, theirs, "two threads must not share one slot");
        assert_eq!(get(), libc::EPIPE, "and this one is unchanged");
        set(0);
    }

    /// The three that bite, named. Each is a *live* number on the other
    /// platform, so an untranslated one is a different error rather than an
    /// unrecognised one.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_numbers_that_collide_are_translated() {
        assert_eq!(from_host(libc::EAGAIN), 35);
        assert_eq!(libc::EAGAIN, 11, "which is Darwin's EDEADLK");
        assert_eq!(from_host(libc::ETIMEDOUT), 60);
        assert_eq!(from_host(libc::ENAMETOOLONG), 63);
        assert_eq!(libc::ENAMETOOLONG, 36, "which is Darwin's EINPROGRESS");
        assert_eq!(to_host(35), libc::EAGAIN);
        assert_eq!(to_host(60), libc::ETIMEDOUT);
    }

    #[test]
    fn zero_is_success_on_both_and_stays_zero() {
        assert_eq!(from_host(0), 0);
        assert_eq!(to_host(0), 0);
    }

    /// Every number this crate can name must survive the round trip, or a guest
    /// that reads `errno` and hands it back is handing back a different error.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn every_named_error_round_trips() {
        for (darwin, _) in NUMBERS {
            // `EOPNOTSUPP` shares this host's 95 with `ENOTSUP`, which wins.
            if *darwin == 102 {
                continue;
            }
            assert_eq!(
                from_host(to_host(*darwin)),
                *darwin,
                "errno {darwin} does not come back as itself"
            );
        }
    }

    /// A host error with no Darwin name must not keep its number: Darwin's table
    /// is dense, so the number already names something else there.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_host_error_with_no_darwin_name_is_not_passed_through() {
        // `EL2NSYNC`, which is Darwin's `ENOTSUP` if passed along.
        assert_eq!(from_host(45), DARWIN_EINVAL);
    }

    /// What the guest reads through `__error` is Darwin's number, not this
    /// host's - which is the whole point of the slot being a separate one.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_guests_slot_holds_darwins_numbering() {
        set(libc::EAGAIN);
        let slot = unsafe { error() };
        // SAFETY: the slot this call just returned.
        assert_eq!(unsafe { *slot }, 35, "Darwin's EAGAIN");
        set(0);
    }

    /// A guest clearing `errno` must find it clear afterwards, even though the
    /// host's slot still holds whatever the last host call left there.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_guest_write_survives_a_host_call_that_changed_nothing() {
        set(libc::EAGAIN);
        // SAFETY: as above; the guest reads, then clears.
        unsafe {
            let slot = error();
            assert_eq!(*slot, 35);
            *slot = 0;
        }
        // A host call that succeeds and leaves `errno` alone.
        // SAFETY: the accessor is re-entered exactly as a guest would.
        assert_eq!(unsafe { *error() }, 0, "the guest's own write was lost");
        set(0);
    }

    /// A number this host cannot name survives to the guest, and is not
    /// renamed by the next reconciliation.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_darwin_only_number_reaches_the_guest() {
        set(libc::ENOENT);
        set_guest(93);
        assert_eq!(guest_errno(), 93);
        assert_eq!(guest_errno(), 93, "and stays after a second read");
        set(0);
    }

    #[test]
    fn holding_errno_puts_it_back() {
        set(libc::EPIPE);
        {
            let _held = Held::now();
            set(libc::ENOENT);
        }
        assert_eq!(get(), libc::EPIPE);
        set(0);
    }

    /// The imported spelling and the accessor must reach the same slot, which is the
    /// only thing that makes the guest's `errno` and a shim's `errno` the same
    /// variable.
    ///
    /// Off Darwin that slot is [`guest_location`] rather than [`location`]: the
    /// two hold the same error in the two platforms' numbering, and handing the
    /// guest the host's own would be handing it the host's numbers.
    #[test]
    fn the_imported_error_is_the_accessor() {
        let at = symbol("_error").expect("___error is defined");
        // SAFETY: `at` is this module's own `error`, whose prototype is `int *(void)`.
        let f: unsafe extern "C" fn() -> *mut libc::c_int =
            unsafe { std::mem::transmute(at as usize as *const ()) };
        assert_eq!(unsafe { f() }, guest_location());
    }
}
