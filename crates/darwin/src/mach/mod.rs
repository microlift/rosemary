// The clock below is compiled and tested on macOS and bound to nothing there:
// the host's own Mach is the guest's. See [`symbol`].
#![cfg_attr(target_os = "macos", allow(dead_code))]

use std::ffi::c_void;

pub mod bootstrap;
pub mod clock;
pub mod info;
pub mod kern;
pub mod msg;
pub mod space;
pub mod sync;
pub mod task;
pub mod vm;
pub mod wire;

/// `mach_timebase_info_data_t`: the ratio from ticks to nanoseconds.
#[repr(C)]
struct TimebaseInfo {
    numer: u32,
    denom: u32,
}

/// Nanoseconds from `clock`, or zero if the host declines to say.
fn nanoseconds(clock: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes the `timespec` it is given and nothing else.
    if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .wrapping_mul(1_000_000_000)
        .wrapping_add(ts.tv_nsec as u64)
}

unsafe extern "C" fn absolute_time() -> u64 {
    nanoseconds(libc::CLOCK_MONOTONIC)
}

/// Ticks including time the machine spent asleep.
///
/// `CLOCK_BOOTTIME` where there is one; `CLOCK_MONOTONIC` otherwise, which
/// differs only across a suspend and is the closer of the two answers available.
unsafe extern "C" fn continuous_time() -> u64 {
    #[cfg(target_os = "linux")]
    let clock = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    let clock = libc::CLOCK_MONOTONIC;
    nanoseconds(clock)
}

/// `kern_return_t mach_timebase_info(mach_timebase_info_t)`.
unsafe extern "C" fn timebase_info(info: *mut c_void) -> i32 {
    if info.is_null() {
        // `KERN_INVALID_ARGUMENT`.
        return 4;
    }
    // SAFETY: the caller passes a `mach_timebase_info_data_t` to fill in, which
    // is the two words written here.
    unsafe {
        *(info as *mut TimebaseInfo) = TimebaseInfo { numer: 1, denom: 1 };
    }
    0
}

/// Host time in seconds on the same timeline as guest `mach_absolute_time`.
pub fn media_time() -> f64 {
    nanoseconds(libc::CLOCK_MONOTONIC) as f64 / 1e9
}

/// `CACurrentMediaTime`, which is `mach_absolute_time` in seconds.
///
/// Not Mach's, but the same clock read through a different framework, and a
/// guest that mixes the two must see one timeline.
unsafe extern "C" fn current_media_time() -> f64 {
    media_time()
}

/// Empty on macOS, where the host's own Mach is the guest's.
#[cfg(target_os = "macos")]
exports::exports! {}

#[cfg(not(target_os = "macos"))]
exports::exports! {
    functions {
        "mach_absolute_time" => absolute_time,
        "mach_continuous_time" => continuous_time,
        "mach_timebase_info" => timebase_info,
        "CACurrentMediaTime" => current_media_time,

        "mach_error" => kern::mach_error,
        "mach_error_string" => kern::mach_error_string,
        "bootstrap_strerror" => kern::bootstrap_strerror,

        "mach_task_self" => task::mach_task_self,
        "mach_host_self" => task::mach_host_self,
        "mach_thread_self" => task::mach_thread_self,
        "task_info" => info::task_info,
        "host_page_size" => info::host_page_size,
        "host_statistics" => info::host_statistics,
        "host_statistics64" => info::host_statistics64,
        "host_info" => info::host_info,
        "host_get_clock_service" => clock::host_get_clock_service,
        "clock_get_time" => clock::clock_get_time,
        "task_get_special_port" => task::task_get_special_port,
        "task_set_special_port" => task::task_set_special_port,

        // The exception-port family. `task`, `host` and `thread` are the same
        // three calls with a different first argument, and nothing here
        // delivers from any of them, so one implementation answers all three.
        "task_set_exception_ports" => task::task_set_exception_ports,
        "thread_set_exception_ports" => task::thread_set_exception_ports,
        "host_set_exception_ports" => task::host_set_exception_ports,
        "task_get_exception_ports"
            | "host_get_exception_ports"
            | "thread_get_exception_ports" => task::task_get_exception_ports,
        "task_swap_exception_ports"
            | "host_swap_exception_ports"
            | "thread_swap_exception_ports" => task::task_swap_exception_ports,

        "mach_port_allocate" => space::mach_port_allocate,
        "mach_port_deallocate" => space::mach_port_deallocate,
        "mach_port_destroy" => space::mach_port_destroy,
        "mach_port_mod_refs" => space::mach_port_mod_refs,
        "mach_port_get_refs" => space::mach_port_get_refs,
        "mach_port_insert_right" => space::mach_port_insert_right,
        "mach_port_extract_right" => space::mach_port_extract_right,
        "mach_port_type" => space::mach_port_type,
        "mach_port_insert_member" => space::mach_port_insert_member,
        "mach_port_move_member" => space::mach_port_move_member,
        "mach_port_set_attributes" => space::mach_port_set_attributes,
        "mach_port_request_notification" => space::mach_port_request_notification,

        "vm_region_64" => vm::vm_region_64,
        "vm_deallocate" => vm::vm_deallocate,
        "vm_read_overwrite" => vm::vm_read_overwrite,

        "mach_msg" => msg::mach_msg,
        "mach_msg_overwrite" => msg::mach_msg_overwrite,
        "mach_msg_destroy" => msg::mach_msg_destroy,
        "mig_get_reply_port" => msg::mig_get_reply_port,
        "mig_put_reply_port" => msg::mig_put_reply_port,
        "mig_dealloc_reply_port" => msg::mig_dealloc_reply_port,

        "bootstrap_check_in" => bootstrap::bootstrap_check_in,
        "bootstrap_look_up" => bootstrap::bootstrap_look_up,
        "bootstrap_look_up2" => bootstrap::bootstrap_look_up2,
        "bootstrap_register" => bootstrap::bootstrap_register,
        "bootstrap_parent" => bootstrap::bootstrap_parent,

        "semaphore_create" => sync::semaphore_create,
        "semaphore_destroy" => sync::semaphore_destroy,
        "semaphore_signal" => sync::semaphore_signal,
        "semaphore_signal_all" => sync::semaphore_signal_all,
        "semaphore_signal_thread" => sync::semaphore_signal_thread,
        "semaphore_wait" => sync::semaphore_wait,
        "semaphore_timedwait" => sync::semaphore_timedwait,
    }
    // Unlike a function, an unbound data import is not refused at the call - it
    // binds to zero and the guest's next instruction dereferences it, which is a
    // segfault at address zero with no symbol anywhere near it. These are the
    // reason this module has to exist at all rather than leaving the family
    // denied.
    data {
        "_mach_task_self_" => &raw const task::TASK_SELF_PORT as usize as u64,
        "_bootstrap_port" => &raw const bootstrap::BOOTSTRAP_PORT as usize as u64,
        "_NDR_record" => (&raw const task::NDR_RECORD).cast::<u8>() as usize as u64,
        "_vm_page_size" => vm_page_size(),
        "_vm_page_mask" => vm_page_mask(),
        "_vm_page_shift" => vm_page_shift(),
    }
}

/// Where each page-size global lives, with the three filled before any is
/// handed out.
///
/// The loader binds the address and the guest reads the word through it, so the
/// value has to be there before the symbol is published rather than before it is
/// read. `publish_page_size` is idempotent, so asking three times is free.
#[cfg(not(target_os = "macos"))]
fn vm_page_size() -> u64 {
    info::publish_page_size();
    &raw const info::VM_PAGE_SIZE as usize as u64
}

#[cfg(not(target_os = "macos"))]
fn vm_page_mask() -> u64 {
    info::publish_page_size();
    &raw const info::VM_PAGE_MASK as usize as u64
}

#[cfg(not(target_os = "macos"))]
fn vm_page_shift() -> u64 {
    info::publish_page_size();
    &raw const info::VM_PAGE_SHIFT as usize as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_moves_forward_and_the_timebase_is_nanoseconds() {
        // SAFETY: neither takes an argument, and `timebase_info` is given a
        // real `mach_timebase_info_data_t`.
        unsafe {
            let first = absolute_time();
            let second = absolute_time();
            assert!(second >= first, "the monotonic clock went backwards");

            let mut info = TimebaseInfo { numer: 0, denom: 0 };
            assert_eq!(timebase_info(&mut info as *mut _ as *mut c_void), 0);
            assert_eq!((info.numer, info.denom), (1, 1));
            assert_eq!(timebase_info(std::ptr::null_mut()), 4);
        }
    }
}
