//! Other tasks and their threads: the calls a process monitor walks the
//! system with.
//!
//! A task port here is a pid and an image of it in the port space, and every
//! question about the task behind it is the process server's to answer (see
//! `crate::process`); every change to it, the task's own to make, which the
//! server asks it to (see `super::service`). Only guests are tasks: a host
//! process is not part of the system a guest can see, and `task_for_pid` on
//! one fails as it would for a process the caller may not inspect.
use std::ffi::c_void;

use server::task::TaskOp;

use super::info::{TimeValue, publish};
use super::kern::*;
use super::space::{self, HOST_SELF, Name, PSET_CONTROL, PSET_NAME, Target};
use server::protocol::{State, Thread};

/// Copy `names` into fresh pages, as the kernel hands an out-of-line array
/// back: the caller releases it with `vm_deallocate`, which unmaps it.
///
/// # Safety
///
/// `array` and `count` are writable.
unsafe fn out_of_line(names: &[Name], array: *mut *mut Name, count: *mut u32) -> i32 {
    if names.is_empty() {
        // SAFETY: the caller's out-parameters.
        unsafe {
            array.write(std::ptr::null_mut());
            count.write(0);
        }
        return KERN_SUCCESS;
    }
    let bytes = std::mem::size_of_val(names);
    // SAFETY: a fresh anonymous mapping, owned by the caller from here on.
    let pages = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            bytes,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if pages == libc::MAP_FAILED {
        return KERN_RESOURCE_SHORTAGE;
    }
    // SAFETY: the mapping holds `bytes`, and the out-parameters are the
    // caller's.
    unsafe {
        std::ptr::copy_nonoverlapping(names.as_ptr(), pages.cast::<Name>(), names.len());
        array.write(pages.cast());
        count.write(names.len() as u32);
    }
    KERN_SUCCESS
}

/// `kern_return_t task_for_pid(mach_port_name_t target_tport, int pid,
/// mach_port_name_t *t)`
///
/// # Safety
///
/// `out` is a writable `mach_port_name_t`.
pub unsafe extern "C" fn task_for_pid(target: Name, pid: i32, out: *mut Name) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if space::task_pid(target).is_none() {
        return KERN_INVALID_ARGUMENT;
    }
    let Some(process) = crate::process::describe(pid) else {
        // What a Mac answers for a pid the caller may not have, and for one
        // that does not exist - the two are deliberately indistinguishable.
        return KERN_FAILURE;
    };
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(space::insert_task(pid, process.image)) };
    KERN_SUCCESS
}

/// `kern_return_t pid_for_task(mach_port_name_t t, int *x)`
///
/// # Safety
///
/// `out` is a writable `int`.
pub unsafe extern "C" fn pid_for_task(task: Name, out: *mut i32) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    match space::task_pid(task) {
        Some(pid) => {
            // SAFETY: the caller's out-parameter.
            unsafe { out.write(pid) };
            KERN_SUCCESS
        }
        None => {
            // SAFETY: as above; a Mac writes -1 for a name that is no task.
            unsafe { out.write(-1) };
            KERN_FAILURE
        }
    }
}

/// `kern_return_t host_processor_sets(host_priv_t, processor_set_name_array_t
/// *, mach_msg_type_number_t *)`: the one processor set.
///
/// # Safety
///
/// `array` and `count` are writable.
pub unsafe extern "C" fn host_processor_sets(
    host: Name,
    array: *mut *mut Name,
    count: *mut u32,
) -> i32 {
    if array.is_null() || count.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if host != HOST_SELF {
        return KERN_INVALID_HOST;
    }
    // SAFETY: the caller's out-parameters.
    unsafe { out_of_line(&[PSET_NAME], array, count) }
}

/// `kern_return_t processor_set_default(host_t, processor_set_name_t *)`
///
/// # Safety
///
/// `out` is writable.
pub unsafe extern "C" fn processor_set_default(host: Name, out: *mut Name) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if host != HOST_SELF {
        return KERN_INVALID_HOST;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(PSET_NAME) };
    KERN_SUCCESS
}

/// `kern_return_t host_processor_set_priv(host_priv_t, processor_set_name_t,
/// processor_set_t *)`: the control port for the set, which is what lists
/// tasks.
///
/// # Safety
///
/// `out` is writable.
pub unsafe extern "C" fn host_processor_set_priv(host: Name, name: Name, out: *mut Name) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if host != HOST_SELF {
        return KERN_INVALID_HOST;
    }
    if name != PSET_NAME && name != PSET_CONTROL {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's out-parameter.
    unsafe { out.write(PSET_CONTROL) };
    KERN_SUCCESS
}

/// `kern_return_t processor_set_tasks(processor_set_t, task_array_t *,
/// mach_msg_type_number_t *)`: a send right to every guest task.
///
/// # Safety
///
/// `array` and `count` are writable.
pub unsafe extern "C" fn processor_set_tasks(
    pset: Name,
    array: *mut *mut Name,
    count: *mut u32,
) -> i32 {
    if array.is_null() || count.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if pset != PSET_CONTROL {
        return KERN_INVALID_ARGUMENT;
    }
    let tasks: Vec<Name> = crate::process::list()
        .iter()
        .map(|process| space::insert_task(process.pid, process.image))
        .collect();
    // SAFETY: the caller's out-parameters.
    unsafe { out_of_line(&tasks, array, count) }
}

/// `kern_return_t processor_set_tasks_with_flavor(processor_set_t,
/// mach_task_flavor_t, task_array_t *, mach_msg_type_number_t *)`.
///
/// Every flavour - control, read, inspect, name - is the same port here: the
/// server answers questions and nothing here changes another task, so there is
/// no capability for a weaker flavour to withhold.
///
/// # Safety
///
/// As [`processor_set_tasks`].
pub unsafe extern "C" fn processor_set_tasks_with_flavor(
    pset: Name,
    flavor: u32,
    array: *mut *mut Name,
    count: *mut u32,
) -> i32 {
    /// `TASK_FLAVOR_NAME`, the last of the four.
    const TASK_FLAVOR_NAME: u32 = 3;
    if flavor > TASK_FLAVOR_NAME {
        return KERN_INVALID_ARGUMENT;
    }
    // SAFETY: the caller's out-parameters.
    unsafe { processor_set_tasks(pset, array, count) }
}

/// `kern_return_t task_threads(task_inspect_t, thread_act_array_t *,
/// mach_msg_type_number_t *)`
///
/// # Safety
///
/// `array` and `count` are writable.
pub unsafe extern "C" fn task_threads(task: Name, array: *mut *mut Name, count: *mut u32) -> i32 {
    if array.is_null() || count.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    let Some(pid) = space::task_pid(task) else {
        return KERN_INVALID_ARGUMENT;
    };
    let Some(threads) = crate::process::threads(pid) else {
        return KERN_INVALID_ARGUMENT;
    };
    let names: Vec<Name> = threads
        .iter()
        .map(|thread| space::insert_thread(pid, thread.tid))
        .collect();
    // SAFETY: the caller's out-parameters.
    unsafe { out_of_line(&names, array, count) }
}

const THREAD_BASIC_INFO: u32 = 3;
const THREAD_IDENTIFIER_INFO: u32 = 4;
const THREAD_EXTENDED_INFO: u32 = 5;

/// `TH_STATE_*`.
const TH_STATE_RUNNING: i32 = 1;
const TH_STATE_STOPPED: i32 = 2;
const TH_STATE_WAITING: i32 = 3;
const TH_STATE_UNINTERRUPTIBLE: i32 = 4;
const TH_STATE_HALTED: i32 = 5;

/// `POLICY_TIMESHARE`, every thread's policy here.
const POLICY_TIMESHARE: i32 = 1;

/// `struct thread_basic_info`.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct ThreadBasicInfo {
    user_time: TimeValue,
    system_time: TimeValue,
    cpu_usage: i32,
    policy: i32,
    run_state: i32,
    flags: i32,
    suspend_count: i32,
    sleep_time: i32,
}

/// `struct thread_identifier_info`.
#[repr(C, packed(4))]
#[derive(Clone, Copy, Default)]
struct ThreadIdentifierInfo {
    thread_id: u64,
    thread_handle: u64,
    dispatch_qaddr: u64,
}

/// `struct thread_extended_info`.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct ThreadExtendedInfo {
    pth_user_time: u64,
    pth_system_time: u64,
    pth_cpu_usage: i32,
    pth_policy: i32,
    pth_run_state: i32,
    pth_flags: i32,
    pth_sleep_time: i32,
    pth_curpri: i32,
    pth_priority: i32,
    pth_maxpriority: i32,
    pth_name: [u8; 64],
}

const _: () = {
    assert!(size_of::<ThreadBasicInfo>() == 40);
    assert!(size_of::<ThreadIdentifierInfo>() == 24);
    assert!(size_of::<ThreadExtendedInfo>() == 112);
};

fn run_state(state: State) -> i32 {
    match state {
        State::Running => TH_STATE_RUNNING,
        State::Sleeping => TH_STATE_WAITING,
        State::Waiting => TH_STATE_UNINTERRUPTIBLE,
        State::Stopped => TH_STATE_STOPPED,
        State::Zombie => TH_STATE_HALTED,
    }
}

/// Darwin's default user priority, `BASEPRI_DEFAULT`.
const BASEPRI_DEFAULT: i32 = 31;

/// `kern_return_t thread_info(thread_inspect_t, thread_flavor_t,
/// thread_info_t, mach_msg_type_number_t *)`
///
/// # Safety
///
/// `out` is writable for `*count` 32-bit words, and `count` is writable.
pub unsafe extern "C" fn thread_info(
    thread: Name,
    flavor: u32,
    out: *mut c_void,
    count: *mut u32,
) -> i32 {
    if out.is_null() || count.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    let Some((pid, tid)) = space::thread_of(thread) else {
        return KERN_INVALID_ARGUMENT;
    };
    let found: Option<Thread> = crate::process::threads(pid)
        .and_then(|threads| threads.into_iter().find(|candidate| candidate.tid == tid));
    let Some(found) = found else {
        // The thread has ended, which is the ordinary race for anything
        // walking another task's threads.
        return KERN_INVALID_ARGUMENT;
    };
    match flavor {
        // SAFETY: the caller's obligation, checked against the flavour's size.
        THREAD_BASIC_INFO => unsafe {
            publish(
                ThreadBasicInfo {
                    user_time: TimeValue::from_ns(found.user_ns),
                    system_time: TimeValue::from_ns(found.system_ns),
                    policy: POLICY_TIMESHARE,
                    run_state: run_state(found.state),
                    ..ThreadBasicInfo::default()
                },
                out,
                count,
            )
        },
        THREAD_IDENTIFIER_INFO => unsafe {
            publish(
                ThreadIdentifierInfo {
                    thread_id: found.tid as u64,
                    ..ThreadIdentifierInfo::default()
                },
                out,
                count,
            )
        },
        THREAD_EXTENDED_INFO => unsafe {
            let mut name = [0u8; 64];
            let n = found.name.len().min(name.len() - 1);
            name[..n].copy_from_slice(&found.name.as_bytes()[..n]);
            publish(
                ThreadExtendedInfo {
                    pth_user_time: found.user_ns,
                    pth_system_time: found.system_ns,
                    pth_cpu_usage: 0,
                    pth_policy: POLICY_TIMESHARE,
                    pth_run_state: run_state(found.state),
                    pth_flags: 0,
                    pth_sleep_time: 0,
                    pth_curpri: BASEPRI_DEFAULT,
                    pth_priority: BASEPRI_DEFAULT,
                    pth_maxpriority: 63,
                    pth_name: name,
                },
                out,
                count,
            )
        },
        _ => KERN_INVALID_ARGUMENT,
    }
}

/// `struct ipc_info_space_basic`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IpcInfoSpaceBasic {
    iisb_genno_mask: u32,
    iisb_table_size: u32,
    iisb_table_next: u32,
    iisb_table_inuse: u32,
    iisb_reserved: [u32; 2],
}

/// `kern_return_t mach_port_space_basic_info(ipc_space_read_t,
/// ipc_info_space_basic_t *)`
///
/// This task's own space only. Another task's lives in that process, and a
/// count invented for it would be read as a measurement.
///
/// # Safety
///
/// `out` is a writable `ipc_info_space_basic_t`.
pub unsafe extern "C" fn mach_port_space_basic_info(task: Name, out: *mut c_void) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ARGUMENT;
    }
    if space::task_pid(task) != Some(crate::process::host_self()) {
        return KERN_INVALID_ARGUMENT;
    }
    let names = space::count() as u32;
    let info = IpcInfoSpaceBasic {
        iisb_table_size: names,
        iisb_table_next: names,
        iisb_table_inuse: names,
        ..IpcInfoSpaceBasic::default()
    };
    // SAFETY: the caller's structure.
    unsafe { out.cast::<IpcInfoSpaceBasic>().write_unaligned(info) };
    KERN_SUCCESS
}

/// `kern_return_t task_policy_get(task_policy_get_t, task_policy_flavor_t,
/// task_policy_t, mach_msg_type_number_t *, boolean_t *)`
///
/// Refused: the policies are QoS and importance state no host keeps, and a
/// caller reads their absence the way it reads a failure.
pub extern "C" fn task_policy_get(
    _task: Name,
    _flavor: u32,
    _info: *mut c_void,
    _count: *mut u32,
    _default: *mut u32,
) -> i32 {
    KERN_INVALID_ARGUMENT
}

/// `kern_return_t task_suspend(task_t)`
///
/// The task's threads park where they are, and it answers for its memory
/// while they do: the suspension is the task's own, kept by the task (see
/// `super::thread`), whichever process asked. Suspending this task parks the
/// caller too, as on Darwin, until another process resumes it.
pub extern "C" fn task_suspend(task: Name) -> i32 {
    match space::target(task) {
        Err(code) => code,
        Ok(Target::Local) => {
            let rc = super::thread::suspend_task();
            if rc == KERN_SUCCESS {
                // SAFETY: not in a signal handler; there is no context.
                unsafe { super::thread::park_self(std::ptr::null_mut()) };
            }
            rc
        }
        Ok(Target::Remote { pid, image }) => super::remote::done(task, pid, image, TaskOp::Suspend),
    }
}

/// `kern_return_t task_resume(task_t)`
///
/// A task with no suspension outstanding is refused, as Darwin refuses it.
pub extern "C" fn task_resume(task: Name) -> i32 {
    match space::target(task) {
        Err(code) => code,
        Ok(Target::Local) => super::thread::resume_task(),
        Ok(Target::Remote { pid, image }) => super::remote::done(task, pid, image, TaskOp::Resume),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mach::space::TASK_SELF;

    fn me() -> i32 {
        crate::process::host_self()
    }

    #[test]
    fn this_task_is_its_own_port_and_its_own_pid() {
        let mut task = 0;
        assert_eq!(
            unsafe { task_for_pid(TASK_SELF, me(), &mut task) },
            KERN_SUCCESS
        );
        assert_eq!(task, TASK_SELF);
        let mut pid = 0;
        assert_eq!(unsafe { pid_for_task(task, &mut pid) }, KERN_SUCCESS);
        assert_eq!(pid, me());
    }

    #[test]
    fn a_process_that_is_not_a_guest_has_no_task() {
        let mut task = 0;
        assert_eq!(
            unsafe { task_for_pid(TASK_SELF, 1, &mut task) },
            KERN_FAILURE
        );
        let mut pid = 0;
        assert_eq!(unsafe { pid_for_task(0xdead_0003, &mut pid) }, KERN_FAILURE);
        assert_eq!(pid, -1);
    }

    #[test]
    fn the_processor_set_lists_this_task_in_pages_the_caller_frees() {
        let (mut psets, mut count) = (std::ptr::null_mut(), 0);
        assert_eq!(
            unsafe { host_processor_sets(HOST_SELF, &mut psets, &mut count) },
            KERN_SUCCESS
        );
        assert_eq!(count, 1);
        let mut pset = 0;
        assert_eq!(
            unsafe { host_processor_set_priv(HOST_SELF, *psets, &mut pset) },
            KERN_SUCCESS
        );
        let (mut tasks, mut ntasks) = (std::ptr::null_mut(), 0);
        assert_eq!(
            unsafe { processor_set_tasks_with_flavor(pset, 1, &mut tasks, &mut ntasks) },
            KERN_SUCCESS
        );
        let tasks_slice = unsafe { std::slice::from_raw_parts(tasks, ntasks as usize) };
        assert!(tasks_slice.contains(&TASK_SELF));
        unsafe {
            assert_eq!(
                crate::mach::vm::vm_deallocate(TASK_SELF, tasks as u64, ntasks as u64 * 4),
                KERN_SUCCESS
            );
            assert_eq!(
                crate::mach::vm::vm_deallocate(TASK_SELF, psets as u64, 4),
                KERN_SUCCESS
            );
        }
    }

    #[test]
    fn this_tasks_threads_answer_thread_info() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let held = barrier.clone();
        let other = std::thread::Builder::new()
            .name("tasks-probe".into())
            .spawn(move || held.wait())
            .unwrap();
        let (mut threads, mut count) = (std::ptr::null_mut(), 0);
        assert_eq!(
            unsafe { task_threads(TASK_SELF, &mut threads, &mut count) },
            KERN_SUCCESS
        );
        let names = unsafe { std::slice::from_raw_parts(threads, count as usize) }.to_vec();
        let mut probed = false;
        for name in names {
            let mut info = [0u8; 112];
            let mut words = 28u32;
            if unsafe {
                thread_info(
                    name,
                    THREAD_EXTENDED_INFO,
                    info.as_mut_ptr().cast(),
                    &mut words,
                )
            } == KERN_SUCCESS
                && info[48..59] == *b"tasks-probe"
            {
                probed = true;
            }
        }
        barrier.wait();
        other.join().unwrap();
        assert!(probed, "the named thread was not among the task's");

        // This thread's own port answers the basic flavour.
        let me = crate::mach::task::mach_thread_self();
        let mut info = [0u8; 40];
        let mut words = 10u32;
        assert_eq!(
            unsafe { thread_info(me, THREAD_BASIC_INFO, info.as_mut_ptr().cast(), &mut words) },
            KERN_SUCCESS
        );
        assert_eq!(
            i32::from_le_bytes(info[24..28].try_into().unwrap()),
            TH_STATE_RUNNING
        );
    }

    #[test]
    fn the_port_space_counts_this_tasks_names_and_no_other_tasks() {
        let mut info = IpcInfoSpaceBasic::default();
        assert_eq!(
            unsafe { mach_port_space_basic_info(TASK_SELF, (&raw mut info).cast()) },
            KERN_SUCCESS
        );
        assert!(info.iisb_table_inuse >= 5, "the fixed names are in use");
        assert_eq!(
            unsafe { mach_port_space_basic_info(HOST_SELF, (&raw mut info).cast()) },
            KERN_INVALID_ARGUMENT
        );
    }

    /// Nothing outstanding is refused, and so is a task this process cannot
    /// reach without a server.
    #[test]
    fn a_resume_needs_a_suspension_and_another_task_needs_a_server() {
        assert_eq!(task_resume(TASK_SELF), KERN_FAILURE);
        assert_eq!(task_suspend(0xdead_0003), MACH_SEND_INVALID_DEST);
        assert_eq!(task_suspend(HOST_SELF), KERN_INVALID_ARGUMENT);
        let other = space::insert_task(me() + 1_000_000, 7);
        assert_eq!(task_suspend(other), KERN_FAILURE);
    }
}
