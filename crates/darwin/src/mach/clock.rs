#![cfg(not(target_os = "macos"))]

use super::kern::*;
use super::space::{self, HOST_SELF, Name};

/// `clock_id_t` values from `<mach/clock_types.h>`. `REALTIME_CLOCK` is an
/// older spelling of `SYSTEM_CLOCK`, and the kernel serves no third clock.
const SYSTEM_CLOCK: i32 = 0;
const CALENDAR_CLOCK: i32 = 1;

/// `mach_timespec_t`: an unsigned 32-bit second count and a signed 32-bit
/// nanosecond count, which is not `struct timespec`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MachTimespec {
    sec: u32,
    nsec: i32,
}

/// The host clock behind a clock-service name, or `None` for any other name.
///
/// The system clock is `mach_absolute_time`'s timeline, which stops across a
/// sleep; the calendar clock is the wall clock.
fn host_clock(name: Name) -> Option<libc::clockid_t> {
    match name {
        space::SYSTEM_CLOCK => Some(libc::CLOCK_MONOTONIC),
        space::CALENDAR_CLOCK => Some(libc::CLOCK_REALTIME),
        _ => None,
    }
}

/// `kern_return_t host_get_clock_service(host_t, clock_id_t, clock_serv_t *)`
///
/// # Safety
///
/// `out` is null or a writable `clock_serv_t`.
pub unsafe extern "C" fn host_get_clock_service(host: Name, id: i32, out: *mut Name) -> i32 {
    if host != HOST_SELF {
        return KERN_INVALID_ARGUMENT;
    }
    let name = match id {
        SYSTEM_CLOCK => space::SYSTEM_CLOCK,
        CALENDAR_CLOCK => space::CALENDAR_CLOCK,
        _ => return KERN_INVALID_ARGUMENT,
    };
    if out.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    // SAFETY: the caller's `clock_serv_t`, which guest memory may not align.
    unsafe { out.write_unaligned(name) };
    KERN_SUCCESS
}

/// `kern_return_t clock_get_time(clock_serv_t, mach_timespec_t *)`
///
/// # Safety
///
/// `out` is null or a writable `mach_timespec_t`.
pub unsafe extern "C" fn clock_get_time(clock: Name, out: *mut MachTimespec) -> i32 {
    let Some(host) = host_clock(clock) else {
        // A name with nothing behind it fails in the send, as the MIG call it
        // is would; a live name that is not a clock is the kernel's refusal.
        return match space::with_right(clock, |_| ()) {
            Some(()) => KERN_INVALID_ARGUMENT,
            None => MACH_SEND_INVALID_DEST,
        };
    };
    if out.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes the `timespec` it is given and nothing else.
    if unsafe { libc::clock_gettime(host, &mut ts) } != 0 {
        return KERN_FAILURE;
    }
    let value = MachTimespec {
        sec: ts.tv_sec as u32,
        nsec: ts.tv_nsec as i32,
    };
    // SAFETY: the caller's `mach_timespec_t`, which guest memory may not align.
    unsafe { out.write_unaligned(value) };
    KERN_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<mach/clock_types.h>` packs two 32-bit fields into eight bytes.
    #[test]
    fn mach_timespec_matches_the_darwin_sdk_abi() {
        assert_eq!(core::mem::size_of::<MachTimespec>(), 8);
        assert_eq!(core::mem::offset_of!(MachTimespec, nsec), 4);
    }

    fn service(id: i32) -> Name {
        let mut name = 0;
        // SAFETY: a live local `clock_serv_t`.
        assert_eq!(
            unsafe { host_get_clock_service(HOST_SELF, id, &mut name) },
            KERN_SUCCESS
        );
        name
    }

    fn read(clock: Name) -> MachTimespec {
        let mut ts = MachTimespec { sec: 0, nsec: -1 };
        // SAFETY: a live local `mach_timespec_t`.
        assert_eq!(unsafe { clock_get_time(clock, &mut ts) }, KERN_SUCCESS);
        assert!((0..1_000_000_000).contains(&ts.nsec));
        ts
    }

    /// The pattern callers use in place of `clock_gettime`: ask the host for
    /// the calendar clock, read it, and get a Unix timestamp back.
    #[test]
    fn calendar_clock_reads_the_wall_clock() {
        let now = read(service(CALENDAR_CLOCK));
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(u64::from(now.sec).abs_diff(wall) <= 1);
    }

    #[test]
    fn system_clock_is_uptime_and_moves_forward() {
        let clock = service(SYSTEM_CLOCK);
        let first = read(clock);
        let second = read(clock);
        assert!((second.sec, second.nsec) >= (first.sec, first.nsec));
        assert!(first.sec < read(service(CALENDAR_CLOCK)).sec);
        // Releasing the service is what callers do next, and must not fail.
        assert_eq!(
            space::mach_port_deallocate(space::TASK_SELF, clock),
            KERN_SUCCESS
        );
        read(clock);
    }

    #[test]
    fn unknown_clocks_and_hosts_are_refused() {
        let mut name = 0;
        // SAFETY: a live local `clock_serv_t`.
        unsafe {
            assert_eq!(
                host_get_clock_service(HOST_SELF, 2, &mut name),
                KERN_INVALID_ARGUMENT
            );
            assert_eq!(
                host_get_clock_service(space::TASK_SELF, SYSTEM_CLOCK, &mut name),
                KERN_INVALID_ARGUMENT
            );
        }
        let mut ts = MachTimespec { sec: 0, nsec: 0 };
        // SAFETY: a live local `mach_timespec_t`; neither call writes it.
        unsafe {
            assert_eq!(clock_get_time(HOST_SELF, &mut ts), KERN_INVALID_ARGUMENT);
            assert_eq!(clock_get_time(0xdead_0003, &mut ts), MACH_SEND_INVALID_DEST);
        }
    }
}
