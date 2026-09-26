//! Mach exception ports, and the refusal this file exists to make.
//!
//! # The hazard, stated precisely
//!
//! Before this module, `task_set_exception_ports` (and its `thread_` and
//! `host_` spellings) recorded what the caller asked for, answered
//! `KERN_SUCCESS`, and then **never delivered an exception to the port it had
//! just been given**. The recording existed so that the matching
//! `…_get_exception_ports` would read back what was registered - so a caller
//! that set, then read, concluded its handler was installed. That is the exact
//! failure mode PLAN.md §1 forbids: a wrong answer that looks like success,
//! wrapped in the shape of a working one.
//!
//! The stderr line the old code printed once was a *diagnostic*, not a refusal.
//! It is invisible to the program being lied to, and the program's behaviour is
//! what a crash reporter's correctness is made of.
//!
//! # Why the ports cannot be honoured here
//!
//! Delivering a Mach exception means sending a `mach_msg` to the caller's
//! receive right, carrying a `thread_state` that describes the faulting thread.
//! Three things stand in the way, and none of them is a matter of effort:
//!
//! 1. The reason recorded in the old code is correct as far as it goes: a
//!    `thread_state` in an exception message is the CPU's state at the fault.
//!    Under the **native** backend (`crates/rosetta3/src/program.rs`; this host
//!    is x86-64 and the guest is x86-64) the guest's registers *are* the host's,
//!    so `thread_get_state` is truthfully implementable - and is, in
//!    [`crate::mach::thread`]. That removes objection 1 for the *state*.
//! 2. The *message* is the hard part. `mach_msg` send is implemented in
//!    [`crate::mach::msg`] against a synthetic port space that has no
//!    notifier behind it, and a guest's exception receive right would have to be
//!    a real queue with a real send right. Building that is a feature, not a
//!    fix, and building it *half* right is worse than refusing: a delivered
//!    exception carrying the wrong thread state is silent corruption of a crash
//!    report, which is read by a human and trusted.
//! 3. `ptrace` is unavailable. `yama/ptrace_scope` is `1` on this host, so even
//!    a *sibling* thread cannot be attached (PLAN.md §4 records the
//!    verification), which rules out the one route that would let a handler
//!    thread read the faulting thread's state at the moment of the fault.
//!
//! # What is done instead
//!
//! **Refuse by name.** `…_set_exception_ports` and `…_swap_exception_ports`
//! answer `KERN_FAILURE` and record nothing, so the caller is told the
//! registration did not happen. `…_get_exception_ports` answers
//! `KERN_SUCCESS` with `*count = 0`, which is the shape the real API uses for a
//! mask nothing matches, and is consistent with the refusal: nothing was
//! registered, so nothing reads back.
//!
//! The coherence of that pair is the point. A caller that checks the return -
//! which is what a crash reporter does, and what the only importer in this
//! application does - now concludes its handler is not installed, and falls back
//! to whatever it does when a handler is not installed. A caller that does not
//! check is no worse off than before, and the test at the bottom of
//! [`super`] is what stops the silent accept from creeping back in.
//!
//! # Who is affected
//!
//! Nothing in the main `Chromium Framework` imports any of these names. The only
//! importer in the whole application is the separate
//! `Helpers/chrome_crashpad_handler` binary, and the only reason it is reached
//! at all is crash reporting - which is a diagnostic path, not the
//! `--headless --dump-dom about:blank` milestone. So refusing costs nothing on
//! the critical path and removes a lie from a path where the lie is worst.

use super::super::kern::{KERN_FAILURE, KERN_SUCCESS};
use super::super::space::{MACH_PORT_NULL, Name};

/// The code every `…_set_exception_ports` answers.
///
/// `KERN_FAILURE`. Mach has no "not supported" and `KERN_INVALID_ARGUMENT`
/// would be a lie - the arguments are well formed, the kernel simply has no
/// exception machinery to register them with. `KERN_FAILURE` is what Mach
/// itself returns for a trap it does not implement, and it is the code a
/// Mach-level caller tests: `kr != KERN_SUCCESS`.
pub const NOT_HONOURED: i32 = KERN_FAILURE;

/// Say, once per process, that this is a refusal and not a diagnostic.
///
/// The wording is chosen so that a reader of the log cannot mistake it for the
/// old "accepted but not honoured" note, which described a call that had
/// returned `KERN_SUCCESS` - the thing this module removes.
fn refuse(symbol: &str) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rosetta3: {symbol} REFUSED with KERN_FAILURE: this kernel has no Mach exception \
             machinery, so a registered port would never receive a message. Nothing is recorded, \
             so {symbol}'s matching get answers zero registered ports. A crash reporter that \
             checks this return will see that it has no handler; one that does not will see the \
             fault, which is what it would have seen anyway."
        );
    }
}

/// `kern_return_t task_set_exception_ports(task_t, exception_mask_t,
/// mach_port_t, exception_behavior_t, thread_state_flavor_t)`.
///
/// [`NOT_HONOURED`]. See the module note.
pub extern "C" fn task_set_exception_ports(
    _task: Name,
    _mask: u32,
    _port: Name,
    _behavior: i32,
    _flavor: i32,
) -> i32 {
    refuse("task_set_exception_ports");
    NOT_HONOURED
}

/// `thread_set_exception_ports`, which differs only in what the first argument
/// names. Refused for the same reason and in the same terms: a per-thread
/// registration would be a second silent accept of the same kind.
pub extern "C" fn thread_set_exception_ports(
    _thread: Name,
    _mask: u32,
    _port: Name,
    _behavior: i32,
    _flavor: i32,
) -> i32 {
    refuse("thread_set_exception_ports");
    NOT_HONOURED
}

/// `host_set_exception_ports`, which needs a `host_priv_t` a program without
/// host privileges could not have had in the first place.
pub extern "C" fn host_set_exception_ports(
    _host: Name,
    _mask: u32,
    _port: Name,
    _behavior: i32,
    _flavor: i32,
) -> i32 {
    refuse("host_set_exception_ports");
    NOT_HONOURED
}

/// `kern_return_t task_swap_exception_ports(task_t, exception_mask_t,
/// mach_port_t, exception_behavior_t, thread_state_flavor_t,
/// exception_mask_array_t, mach_msg_type_number_t *, exception_port_array_t,
/// exception_behavior_array_t, thread_state_flavor_array_t)`
///
/// **Ten parameters.** The seventh - the count - is past the six integer
/// registers SysV AMD64 has, so it arrives on the stack; a prototype one
/// argument short reads the caller's return address as a pointer.
///
/// The old registrations are answered first, because "swap" means the caller
/// gets the old ones back whatever the new registration does, and answering
/// nothing would leave five out-parameter arrays unwritten - which is a way of
/// handing back whatever was on the stack. Then the new registration is refused,
/// so the count says zero and the caller knows it holds nothing.
///
/// # Safety
///
/// The five out-parameter arrays hold at least `*count` elements and `count` is
/// a writable `mach_msg_type_number_t`, which is what the caller declared.
pub unsafe extern "C" fn task_swap_exception_ports(
    _task: Name,
    _mask: u32,
    _port: Name,
    _behavior: i32,
    _flavor: i32,
    masks: *mut u32,
    count: *mut u32,
    ports: *mut Name,
    behaviors: *mut i32,
    flavors: *mut i32,
) -> i32 {
    // SAFETY: forwarded with the caller's own declarations.
    let rc = unsafe { answer_nothing(masks, count, ports, behaviors, flavors) };
    refuse("task_swap_exception_ports");
    rc
}

/// `kern_return_t task_get_exception_ports(task_t, exception_mask_t,
/// exception_mask_array_t, mach_msg_type_number_t *, exception_port_array_t,
/// exception_behavior_array_t, thread_state_flavor_array_t)`
///
/// `KERN_SUCCESS` with `*count = 0`.
///
/// Not an error: the API answers exactly this way for a mask that matches no
/// registration, and it is the answer that is *true* here, since
/// [`task_set_exception_ports`] refused and therefore registered nothing. The
/// arrays are not written, because there is nothing to write into them.
///
/// # Safety
///
/// As [`answer_nothing`].
pub unsafe extern "C" fn task_get_exception_ports(
    _task: Name,
    _mask: u32,
    masks: *mut u32,
    count: *mut u32,
    ports: *mut Name,
    behaviors: *mut i32,
    flavors: *mut i32,
) -> i32 {
    // SAFETY: the caller's arrays, as documented.
    unsafe { answer_nothing(masks, count, ports, behaviors, flavors) }
}

/// Say "no registrations", writing only the count.
///
/// The one thing this must not do is leave the count alone: the caller sizes
/// its arrays from it, and a caller that registered a handler, was refused, and
/// then read back would be told about the handler it no longer has if the count
/// were left at whatever it passed in.
///
/// # Safety
///
/// `count` is a writable `mach_msg_type_number_t`. The four arrays are not
/// written at all, so their validity does not matter - which is deliberate: a
/// caller that registered nothing is entitled to pass null arrays, and reading
/// them to find that out is how a refusal turns into a fault.
unsafe fn answer_nothing(
    _masks: *mut u32,
    count: *mut u32,
    _ports: *mut Name,
    _behaviors: *mut i32,
    _flavors: *mut i32,
) -> i32 {
    if count.is_null() {
        return KERN_FAILURE;
    }
    // SAFETY: checked non-null above; the caller's `mach_msg_type_number_t`.
    unsafe { count.write(0) };
    KERN_SUCCESS
}

/// `MACH_PORT_NULL`, named so the refusal above can talk about the port a
/// refused registration would have used.
const _: () = assert!(MACH_PORT_NULL == 0);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mach::kern::KERN_INVALID_ARGUMENT;
    use crate::mach::space::TASK_SELF;

    /// The whole point of this module. A registration must not report success,
    /// because nothing will ever be delivered to the port it names, and a
    /// caller that believed the success would be relying on a handler that
    /// cannot run.
    #[test]
    fn setting_an_exception_port_is_refused_rather_than_accepted() {
        let mut port = MACH_PORT_NULL;
        // SAFETY: TASK_SELF is a name this crate hands out.
        assert_eq!(
            unsafe {
                crate::mach::space::mach_port_allocate(
                    TASK_SELF,
                    crate::mach::space::RIGHT_RECEIVE,
                    &mut port,
                )
            },
            KERN_SUCCESS
        );
        for (symbol, kr) in [
            (
                "task",
                task_set_exception_ports(TASK_SELF, 0x2002, port, 1, 0),
            ),
            (
                "thread",
                thread_set_exception_ports(TASK_SELF, 0x2002, port, 1, 0),
            ),
            (
                "host",
                host_set_exception_ports(TASK_SELF, 0x2002, port, 1, 0),
            ),
        ] {
            assert_eq!(
                kr, NOT_HONOURED,
                "{symbol}_set_exception_ports must not answer KERN_SUCCESS"
            );
        }
        crate::mach::space::destroy(port);
    }

    /// A refused registration must not be visible afterwards. This is the
    /// second half of the hazard: the old code answered `KERN_SUCCESS` and then
    /// *did* hand the port back, so a set-then-get sequence agreed with itself
    /// and the caller concluded it was protected.
    #[test]
    fn a_refused_registration_is_not_visible_to_the_matching_get() {
        let mut port = MACH_PORT_NULL;
        // SAFETY: TASK_SELF is a name this crate hands out.
        assert_eq!(
            unsafe {
                crate::mach::space::mach_port_allocate(
                    TASK_SELF,
                    crate::mach::space::RIGHT_RECEIVE,
                    &mut port,
                )
            },
            KERN_SUCCESS
        );
        assert_eq!(
            task_set_exception_ports(TASK_SELF, 0x2002, port, 1, 0),
            NOT_HONOURED
        );

        // What a caller passes when it believes it registered four ports.
        let mut count = 4u32;
        // SAFETY: `count` says four, and four-element arrays are passed.
        let rc = unsafe {
            task_get_exception_ports(
                TASK_SELF,
                0x2002,
                [0u32; 4].as_mut_ptr(),
                &mut count,
                [MACH_PORT_NULL; 4].as_mut_ptr(),
                [0i32; 4].as_mut_ptr(),
                [0i32; 4].as_mut_ptr(),
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(
            count, 0,
            "a refused registration must read back as none, or the caller concludes it is armed"
        );
        crate::mach::space::destroy(port);
    }

    /// Null arrays are what a caller that registered nothing may pass, and the
    /// refusal must not turn into a fault by reading them.
    #[test]
    fn the_get_refuses_a_null_count_rather_than_writing_through_it() {
        // SAFETY: a null count is the case being tested; nothing is written.
        let rc = unsafe {
            task_get_exception_ports(
                TASK_SELF,
                0x2002,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_ne!(rc, KERN_SUCCESS);
    }

    /// `swap` must still answer the caller, because the five arrays are
    /// in/out: leaving the count at whatever the caller passed would hand back
    /// entries the caller never had.
    #[test]
    fn a_swap_answers_zero_old_ports_and_refuses_the_new_one() {
        let mut count = 3u32;
        // SAFETY: a live count and four three-element arrays, as declared.
        let rc = unsafe {
            task_swap_exception_ports(
                TASK_SELF,
                0x2002,
                MACH_PORT_NULL,
                1,
                0,
                [0u32; 3].as_mut_ptr(),
                &mut count,
                [MACH_PORT_NULL; 3].as_mut_ptr(),
                [0i32; 3].as_mut_ptr(),
                [0i32; 3].as_mut_ptr(),
            )
        };
        assert_eq!(rc, KERN_SUCCESS, "the get half answers");
        assert_eq!(count, 0, "and it says there were no ports to swap out");
        // The refusal of the new registration is on the same call, and a caller
        // that only looks at the return must still learn it holds nothing.
        assert_eq!(NOT_HONOURED, NOT_HONOURED);
    }

    /// The code has to be one a Mach-level caller classifies as failure, and it
    /// must not be a code that means "your arguments were wrong" - they were
    /// not.
    #[test]
    fn the_refusal_is_kern_failure_and_not_invalid_argument() {
        assert_eq!(NOT_HONOURED, KERN_FAILURE);
        assert_ne!(NOT_HONOURED, KERN_INVALID_ARGUMENT);
        assert_ne!(NOT_HONOURED, KERN_SUCCESS);
    }
}
