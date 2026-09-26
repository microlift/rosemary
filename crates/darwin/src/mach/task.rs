#![cfg(not(target_os = "macos"))]

use super::kern::*;
use super::space::{self, BOOTSTRAP, HOST_SELF, MACH_PORT_NULL, Name, Right, TASK_SELF};

/// The exception-port family, refused rather than accepted.
///
/// Declared from here rather than from `mach/mod.rs` so that the `exports!`
/// entries there keep naming `task::…` and need no edit: the six names
/// `mach/mod.rs` publishes are thin forwarders below. See the module for why a
/// refusal is the answer and what it replaced.
pub mod exception;

/// `mach_port_t mach_task_self_` - the data symbol behind the `mach_task_self()`
/// macro.
pub static TASK_SELF_PORT: Name = TASK_SELF;

/// `NDR_record_t NDR_record` - how MIG says what byte order and character set a
/// message's scalars are in.
///
/// Every field is zero on x86-64 and arm64 alike: `NDR_INT_LITTLE_ENDIAN`,
/// `NDR_CHAR_ASCII` and `NDR_FLOAT_IEEE` are all zero, and so are the version
/// bytes. It is a data import all the same, and an unbound one is the segfault
/// described above rather than a wrong byte order.
pub static NDR_RECORD: [u8; 8] = [0; 8];

/// `task_t mach_task_self(void)` - the function spelling, which some images
/// import instead of the variable.
pub extern "C" fn mach_task_self() -> Name {
    TASK_SELF
}

/// `host_t mach_host_self(void)`
pub extern "C" fn mach_host_self() -> Name {
    HOST_SELF
}

/// `thread_t mach_thread_self(void)`
///
/// One name per thread, kept for the life of the thread. A real one is a send
/// right the caller is expected to deallocate; deallocating this one is
/// harmless and does not invalidate the name, because the name is the thread's
/// identity and a thread that asked twice must get the same answer.
pub extern "C" fn mach_thread_self() -> Name {
    thread_local! {
        static SELF: std::cell::Cell<Name> = const { std::cell::Cell::new(MACH_PORT_NULL) };
    }
    SELF.with(|name| {
        if name.get() == MACH_PORT_NULL {
            name.set(space::insert(Right::Special));
        }
        name.get()
    })
}

/// `TASK_BOOTSTRAP_PORT`, the only special port anything here answers for.
const TASK_BOOTSTRAP_PORT: i32 = 4;

/// `kern_return_t task_get_special_port(task_t, int which, mach_port_t *)`
///
/// # Safety
///
/// `out` is a writable `mach_port_t`.
pub unsafe extern "C" fn task_get_special_port(task: Name, which: i32, out: *mut Name) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    if which != TASK_BOOTSTRAP_PORT {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's `mach_port_t`.
    unsafe { out.write(BOOTSTRAP) };
    KERN_SUCCESS
}

/// `kern_return_t task_set_special_port(task_t, int which, mach_port_t)`
///
/// Refused rather than accepted: replacing the bootstrap port redirects every
/// later lookup, and a caller told that succeeded would then look its services
/// up in a namespace that never changed.
pub extern "C" fn task_set_special_port(_task: Name, _which: i32, _port: Name) -> i32 {
    KERN_NOT_RECEIVER
}

/// `kern_return_t task_set_exception_ports(task_t, exception_mask_t,
/// mach_port_t, exception_behavior_t, thread_state_flavor_t)`
///
/// Refused. The forwarding is here only so that `mach/mod.rs`'s `exports!`
/// entry keeps naming `task::`; the reasoning, and what this replaced, are in
/// [`exception`].
pub extern "C" fn task_set_exception_ports(
    task: Name,
    mask: u32,
    port: Name,
    behavior: i32,
    flavor: i32,
) -> i32 {
    exception::task_set_exception_ports(task, mask, port, behavior, flavor)
}

/// `thread_set_exception_ports`, refused for the same reason.
pub extern "C" fn thread_set_exception_ports(
    thread: Name,
    mask: u32,
    port: Name,
    behavior: i32,
    flavor: i32,
) -> i32 {
    exception::thread_set_exception_ports(thread, mask, port, behavior, flavor)
}

/// `host_set_exception_ports`, refused for the same reason.
pub extern "C" fn host_set_exception_ports(
    host: Name,
    mask: u32,
    port: Name,
    behavior: i32,
    flavor: i32,
) -> i32 {
    exception::host_set_exception_ports(host, mask, port, behavior, flavor)
}

/// `kern_return_t task_get_exception_ports(task_t, exception_mask_t,
/// exception_mask_array_t, mach_msg_type_number_t *, exception_port_array_t,
/// exception_behavior_array_t, thread_state_flavor_array_t)`
///
/// `KERN_SUCCESS` with a count of zero, because nothing was ever registered.
/// See [`exception`].
///
/// # Safety
///
/// As [`exception::task_get_exception_ports`].
pub unsafe extern "C" fn task_get_exception_ports(
    task: Name,
    mask: u32,
    masks: *mut u32,
    count: *mut u32,
    ports: *mut Name,
    behaviors: *mut i32,
    flavors: *mut i32,
) -> i32 {
    // SAFETY: forwarded with the caller's own declarations, unchanged.
    unsafe {
        exception::task_get_exception_ports(task, mask, masks, count, ports, behaviors, flavors)
    }
}

/// `kern_return_t task_swap_exception_ports(task_t, exception_mask_t,
/// mach_port_t, exception_behavior_t, thread_state_flavor_t,
/// exception_mask_array_t, mach_msg_type_number_t *, exception_port_array_t,
/// exception_behavior_array_t, thread_state_flavor_array_t)`
///
/// **Ten parameters.** The seventh - the count - is past the six integer
/// registers SysV AMD64 has, so it arrives on the stack; a prototype one
/// argument short reads the caller's return address as a pointer. That is the
/// one thing about this signature that is easy to get wrong and is why the
/// forwarder spells every argument out.
///
/// # Safety
///
/// As [`exception::task_swap_exception_ports`].
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn task_swap_exception_ports(
    task: Name,
    mask: u32,
    port: Name,
    behavior: i32,
    flavor: i32,
    masks: *mut u32,
    count: *mut u32,
    ports: *mut Name,
    behaviors: *mut i32,
    flavors: *mut i32,
) -> i32 {
    // SAFETY: forwarded with the caller's own declarations, unchanged.
    unsafe {
        exception::task_swap_exception_ports(
            task, mask, port, behavior, flavor, masks, count, ports, behaviors, flavors,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The data symbol that is the whole reason for this module: it must hold a
    /// name the port calls accept, not zero.
    #[test]
    fn the_task_port_is_a_name_the_space_knows() {
        assert_ne!(TASK_SELF_PORT, MACH_PORT_NULL);
        assert_eq!(mach_task_self(), TASK_SELF_PORT);
        let mut port = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        let rc =
            unsafe { space::mach_port_allocate(TASK_SELF_PORT, space::RIGHT_RECEIVE, &mut port) };
        assert_eq!(rc, KERN_SUCCESS);
        space::destroy(port);
    }

    #[test]
    fn ndr_record_is_eight_zero_bytes_and_not_an_unbound_slot() {
        assert_eq!(NDR_RECORD, [0u8; 8]);
    }

    #[test]
    fn a_thread_gets_one_port_name_and_keeps_it() {
        let first = mach_thread_self();
        assert_eq!(mach_thread_self(), first);
        let other = std::thread::spawn(|| mach_thread_self()).join().unwrap();
        assert_ne!(other, first);
    }

    /// The exception ports are **refused**, and this is the test that says so
    /// from the outside: a registration must not answer `KERN_SUCCESS`, and a
    /// refused registration must not read back as one.
    ///
    /// It used to assert the opposite - that a registered port read back as the
    /// port that was registered - which is the hazard `mach::task::exception`
    /// exists to remove. A crash reporter that installs a handler, reads the
    /// ports back, and finds them has been told it is protected when nothing
    /// will ever be delivered to it. The assertions here are the ones that make
    /// that impossible to reintroduce by accident.
    #[test]
    fn the_exception_ports_refuse_rather_than_agree_with_each_other() {
        /// `EXC_MASK_BAD_ACCESS | EXC_MASK_CRASH`, near enough.
        const MASK: u32 = 0x0000_2002;
        let mut port = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(TASK_SELF, space::RIGHT_RECEIVE, &mut port) };

        assert_eq!(
            task_set_exception_ports(TASK_SELF, MASK, port, 1, 0),
            exception::NOT_HONOURED,
            "nothing will be delivered to that port, so success would be a lie"
        );

        let mut count = 4u32;
        let mut masks = [0u32; 4];
        let mut ports = [MACH_PORT_NULL; 4];
        let mut behaviors = [0i32; 4];
        let mut flavors = [0i32; 4];
        // SAFETY: four-element arrays, and `count` says four.
        let rc = unsafe {
            task_get_exception_ports(
                TASK_SELF,
                MASK,
                masks.as_mut_ptr(),
                &mut count,
                ports.as_mut_ptr(),
                behaviors.as_mut_ptr(),
                flavors.as_mut_ptr(),
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(
            count, 0,
            "and the get must agree with the set, or the pair tells the caller its \
             handler is installed when it is not"
        );
        space::destroy(port);
    }

    #[test]
    fn the_bootstrap_port_is_the_one_the_name_server_accepts() {
        let mut port = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        let rc = unsafe { task_get_special_port(TASK_SELF, TASK_BOOTSTRAP_PORT, &mut port) };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(port, BOOTSTRAP);
        assert_eq!(
            unsafe { task_get_special_port(TASK_SELF, 0, &mut port) },
            KERN_INVALID_ARGUMENT
        );
    }
}
