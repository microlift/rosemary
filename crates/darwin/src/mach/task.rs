use super::kern::*;
use super::space::{self, BOOTSTRAP, HOST_SELF, MACH_PORT_NULL, Name, TASK_SELF};

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
/// One name per thread, as on a real system, and like a real one each call
/// adds a user reference the caller is expected to deallocate. The name is a
/// [`Right::Thread`], so `thread_info` on it answers about this thread.
pub extern "C" fn mach_thread_self() -> Name {
    let pid = crate::process::host_self();
    let tid = crate::process::thread_id();
    space::insert_thread(pid, tid)
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

/// What a `..._set_exception_ports` call asked for.
///
/// Kept so that the matching `..._get_exception_ports` answers with what the
/// caller registered rather than with nothing: a crash reporter that installs a
/// handler, reads the ports back, and finds none concludes its installation
/// failed.
struct Registration {
    mask: u32,
    port: Name,
    behavior: i32,
    flavor: i32,
    /// The send right the kernel takes for itself when a port is registered, and
    /// keeps after the caller deallocates its own - which a crash reporter does
    /// straight away. Without it the handler's port would look abandoned the
    /// moment it was installed. Closed when the registration is replaced, and
    /// by the host when this process exits.
    _held: Option<std::os::fd::OwnedFd>,
}

fn registrations() -> &'static std::sync::Mutex<Vec<Registration>> {
    static PORTS: std::sync::OnceLock<std::sync::Mutex<Vec<Registration>>> =
        std::sync::OnceLock::new();
    PORTS.get_or_init(Default::default)
}

fn record(symbol: &str, mask: u32, port: Name, behavior: i32, flavor: i32) {
    announce(symbol);
    let mut held = registrations().lock().unwrap_or_else(|e| e.into_inner());
    held.retain(|existing| existing.mask & mask != existing.mask);
    if port != MACH_PORT_NULL {
        let claim = super::space::address_of(port).and_then(super::wire::claim);
        held.push(Registration {
            mask,
            port,
            behavior,
            flavor,
            _held: claim,
        });
    }
}

fn announce(symbol: &str) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rosemary: {symbol} accepted but not honoured - a Mach exception message carries this \
             CPU's thread state for a fault inside a translation, not the guest's registers, so \
             the registration is recorded and no exception is ever delivered"
        );
    }
}

/// Fill in the five out-parameter arrays a `..._get_exception_ports` takes.
///
/// # Safety
///
/// `count` is a writable `mach_msg_type_number_t` and the four arrays hold at
/// least `*count` elements, which is what the caller declared.
unsafe fn answer_registrations(
    mask: u32,
    count: *mut u32,
    masks: *mut u32,
    ports: *mut Name,
    behaviors: *mut i32,
    flavors: *mut i32,
) -> i32 {
    if count.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    // SAFETY: the caller's count, which states its arrays' capacity.
    let capacity = unsafe { count.read() } as usize;
    let held = registrations().lock().unwrap_or_else(|e| e.into_inner());
    let matching: Vec<&Registration> = held
        .iter()
        .filter(|entry| entry.mask & mask != 0)
        .take(capacity)
        .collect();

    for (i, entry) in matching.iter().enumerate() {
        // SAFETY: `i` is below `capacity`, which the caller declared each array
        // holds; a null array is skipped.
        unsafe {
            if !masks.is_null() {
                masks.add(i).write(entry.mask);
            }
            if !ports.is_null() {
                ports.add(i).write(entry.port);
            }
            if !behaviors.is_null() {
                behaviors.add(i).write(entry.behavior);
            }
            if !flavors.is_null() {
                flavors.add(i).write(entry.flavor);
            }
        }
    }
    // SAFETY: the caller's count.
    unsafe { count.write(matching.len() as u32) };
    KERN_SUCCESS
}

/// `kern_return_t task_set_exception_ports(task_t, exception_mask_t,
/// mach_port_t, exception_behavior_t, thread_state_flavor_t)`
pub extern "C" fn task_set_exception_ports(
    _task: Name,
    mask: u32,
    port: Name,
    behavior: i32,
    flavor: i32,
) -> i32 {
    record("task_set_exception_ports", mask, port, behavior, flavor);
    KERN_SUCCESS
}

/// `thread_set_exception_ports`, which differs only in what the first argument
/// names. The registrations are kept in one list: nothing delivers from it, so
/// a per-thread one would only be a longer way of recording the same thing.
pub extern "C" fn thread_set_exception_ports(
    _thread: Name,
    mask: u32,
    port: Name,
    behavior: i32,
    flavor: i32,
) -> i32 {
    record("thread_set_exception_ports", mask, port, behavior, flavor);
    KERN_SUCCESS
}

/// `host_set_exception_ports`, which needs a `host_priv_t` a program without
/// host privileges could not have had in the first place.
pub extern "C" fn host_set_exception_ports(
    _host: Name,
    mask: u32,
    port: Name,
    behavior: i32,
    flavor: i32,
) -> i32 {
    record("host_set_exception_ports", mask, port, behavior, flavor);
    KERN_SUCCESS
}

/// `kern_return_t task_get_exception_ports(task_t, exception_mask_t,
/// exception_mask_array_t, mach_msg_type_number_t *, exception_port_array_t,
/// exception_behavior_array_t, thread_state_flavor_array_t)`
///
/// # Safety
///
/// As [`answer_registrations`].
pub unsafe extern "C" fn task_get_exception_ports(
    _task: Name,
    mask: u32,
    masks: *mut u32,
    count: *mut u32,
    ports: *mut Name,
    behaviors: *mut i32,
    flavors: *mut i32,
) -> i32 {
    // SAFETY: the caller's arrays, as documented.
    unsafe { answer_registrations(mask, count, masks, ports, behaviors, flavors) }
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
/// # Safety
///
/// As [`answer_registrations`].
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn task_swap_exception_ports(
    _task: Name,
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
    // SAFETY: the caller's arrays, as documented. The old registrations are
    // answered before the new one replaces them, which is what "swap" means.
    let rc = unsafe { answer_registrations(mask, count, masks, ports, behaviors, flavors) };
    record("task_swap_exception_ports", mask, port, behavior, flavor);
    rc
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

    /// Registering exception ports and reading them back has to agree, or a
    /// crash reporter that checks its own installation concludes it failed.
    #[test]
    fn the_exception_ports_read_back_as_the_ones_that_were_registered() {
        /// `EXC_MASK_BAD_ACCESS | EXC_MASK_CRASH`, near enough.
        const MASK: u32 = 0x0000_2002;
        let mut port = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(TASK_SELF, space::RIGHT_RECEIVE, &mut port) };

        assert_eq!(
            task_set_exception_ports(TASK_SELF, MASK, port, 1, 0),
            KERN_SUCCESS
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
        assert_eq!(count, 1);
        assert_eq!(masks[0], MASK);
        assert_eq!(ports[0], port);
        assert_eq!(behaviors[0], 1);

        // Clearing puts it back, so this test does not leak into the next.
        task_set_exception_ports(TASK_SELF, MASK, MACH_PORT_NULL, 0, 0);
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
