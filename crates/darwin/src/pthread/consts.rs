pub const PTHREAD_CREATE_JOINABLE: i32 = 1;
pub const PTHREAD_CREATE_DETACHED: i32 = 2;

pub const PTHREAD_INHERIT_SCHED: i32 = 1;
pub const PTHREAD_EXPLICIT_SCHED: i32 = 2;

pub const PTHREAD_SCOPE_SYSTEM: i32 = 1;
pub const PTHREAD_SCOPE_PROCESS: i32 = 2;

pub const PTHREAD_PROCESS_SHARED: i32 = 1;
pub const PTHREAD_PROCESS_PRIVATE: i32 = 2;

pub const PTHREAD_PRIO_NONE: i32 = 0;
pub const PTHREAD_PRIO_INHERIT: i32 = 1;
pub const PTHREAD_PRIO_PROTECT: i32 = 2;

pub const PTHREAD_MUTEX_NORMAL: i32 = 0;
pub const PTHREAD_MUTEX_ERRORCHECK: i32 = 1;
pub const PTHREAD_MUTEX_RECURSIVE: i32 = 2;
pub const PTHREAD_MUTEX_DEFAULT: i32 = PTHREAD_MUTEX_NORMAL;

pub const PTHREAD_MUTEX_POLICY_FAIRSHARE_NP: i32 = 1;
pub const PTHREAD_MUTEX_POLICY_FIRSTFIT_NP: i32 = 3;

pub const PTHREAD_CANCEL_ENABLE: i32 = 0x01;
pub const PTHREAD_CANCEL_DISABLE: i32 = 0x00;
pub const PTHREAD_CANCEL_DEFERRED: i32 = 0x02;
pub const PTHREAD_CANCEL_ASYNCHRONOUS: i32 = 0x00;

pub const PTHREAD_CANCELED: u64 = 1;

pub const SCHED_OTHER: i32 = 1;
pub const SCHED_FIFO: i32 = 4;
pub const SCHED_RR: i32 = 2;

pub const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
pub const QOS_CLASS_USER_INITIATED: u32 = 0x19;
pub const QOS_CLASS_DEFAULT: u32 = 0x15;
pub const QOS_CLASS_UTILITY: u32 = 0x11;
pub const QOS_CLASS_BACKGROUND: u32 = 0x09;
pub const QOS_CLASS_UNSPECIFIED: u32 = 0x00;

pub const QOS_MIN_RELATIVE_PRIORITY: i32 = -15;

pub mod err {
    pub const EINVAL: i32 = 22;
    pub const EBUSY: i32 = 16;
    pub const EDEADLK: i32 = 11;
    pub const ENOMEM: i32 = 12;
    pub const EPERM: i32 = 1;
    pub const ESRCH: i32 = 3;
    pub const ETIMEDOUT: i32 = 60;
    pub const ENOTSUP: i32 = 45;
    pub const EAGAIN: i32 = 35;

    pub fn from_host(rc: i32) -> i32 {
        crate::errno::from_host(rc)
    }
}

pub fn detach_state_to_host(darwin: i32) -> Option<libc::c_int> {
    Some(match darwin {
        PTHREAD_CREATE_JOINABLE => libc::PTHREAD_CREATE_JOINABLE,
        PTHREAD_CREATE_DETACHED => libc::PTHREAD_CREATE_DETACHED,
        _ => return None,
    })
}

pub fn sched_policy_to_host(darwin: i32) -> Option<libc::c_int> {
    Some(match darwin {
        SCHED_OTHER => libc::SCHED_OTHER,
        SCHED_FIFO => libc::SCHED_FIFO,
        SCHED_RR => libc::SCHED_RR,
        _ => return None,
    })
}

pub fn sched_policy_from_host(host: libc::c_int) -> i32 {
    match host {
        libc::SCHED_FIFO => SCHED_FIFO,
        libc::SCHED_RR => SCHED_RR,
        _ => SCHED_OTHER,
    }
}

pub const DARWIN_PRIORITY_MIN: i32 = 15;
pub const DARWIN_PRIORITY_DEFAULT: i32 = 31;
pub const DARWIN_PRIORITY_MAX: i32 = 47;

/// Scale Darwin's fixed priority range onto the selected host policy's range.
pub fn sched_priority_to_host(darwin: i32, host_policy: libc::c_int) -> Option<i32> {
    if !(DARWIN_PRIORITY_MIN..=DARWIN_PRIORITY_MAX).contains(&darwin) {
        return None;
    }
    // SAFETY: the policy has already been translated to a host policy.
    let (min, max) = unsafe {
        (
            libc::sched_get_priority_min(host_policy),
            libc::sched_get_priority_max(host_policy),
        )
    };
    if min < 0 || max < min {
        return None;
    }
    if min == max {
        return Some(min);
    }
    Some(
        min + (darwin - DARWIN_PRIORITY_MIN) * (max - min)
            / (DARWIN_PRIORITY_MAX - DARWIN_PRIORITY_MIN),
    )
}

/// Scale a host policy's priority back into Darwin's fixed range.
pub fn sched_priority_from_host(host: i32, host_policy: libc::c_int) -> i32 {
    // SAFETY: `host_policy` came from the host scheduler.
    let (min, max) = unsafe {
        (
            libc::sched_get_priority_min(host_policy),
            libc::sched_get_priority_max(host_policy),
        )
    };
    if min < 0 || max <= min {
        return DARWIN_PRIORITY_DEFAULT;
    }
    DARWIN_PRIORITY_MIN
        + (host.clamp(min, max) - min) * (DARWIN_PRIORITY_MAX - DARWIN_PRIORITY_MIN) / (max - min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn the_constants_are_the_ones_darwin_declares() {
        assert_eq!(PTHREAD_MUTEX_NORMAL, libc::PTHREAD_MUTEX_NORMAL);
        assert_eq!(PTHREAD_MUTEX_ERRORCHECK, libc::PTHREAD_MUTEX_ERRORCHECK);
        assert_eq!(PTHREAD_MUTEX_RECURSIVE, libc::PTHREAD_MUTEX_RECURSIVE);
        assert_eq!(PTHREAD_CREATE_JOINABLE, libc::PTHREAD_CREATE_JOINABLE);
        assert_eq!(PTHREAD_CREATE_DETACHED, libc::PTHREAD_CREATE_DETACHED);
        assert_eq!(PTHREAD_PROCESS_SHARED, libc::PTHREAD_PROCESS_SHARED);
        assert_eq!(PTHREAD_PROCESS_PRIVATE, libc::PTHREAD_PROCESS_PRIVATE);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_two_mutex_types_that_matter_are_swapped() {
        assert_ne!(
            PTHREAD_MUTEX_RECURSIVE,
            libc::PTHREAD_MUTEX_RECURSIVE,
            "if these ever agree, the translation is a no-op and this file can shrink"
        );
        assert_ne!(PTHREAD_MUTEX_ERRORCHECK, libc::PTHREAD_MUTEX_ERRORCHECK);
        assert_ne!(PTHREAD_CREATE_JOINABLE, libc::PTHREAD_CREATE_JOINABLE);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_type_nobody_declares_is_refused_rather_than_defaulted() {
        assert_eq!(
            detach_state_to_host(0),
            None,
            "0 is not a Darwin detach state"
        );
        assert_eq!(sched_policy_to_host(99), None);
    }

    #[test]
    fn scheduling_priorities_translate_in_both_directions() {
        let other = sched_policy_to_host(SCHED_OTHER).unwrap();
        let other_default = sched_priority_to_host(DARWIN_PRIORITY_DEFAULT, other).unwrap();
        let other_min = unsafe { libc::sched_get_priority_min(other) };
        let other_max = unsafe { libc::sched_get_priority_max(other) };
        assert!((other_min..=other_max).contains(&other_default));
        if other_min == other_max {
            assert_eq!(other_default, other_min);
        }
        assert_eq!(
            sched_priority_from_host(other_default, other),
            DARWIN_PRIORITY_DEFAULT
        );
        assert_eq!(sched_priority_to_host(DARWIN_PRIORITY_MIN - 1, other), None);

        let fifo = sched_policy_to_host(SCHED_FIFO).unwrap();
        let low = sched_priority_to_host(DARWIN_PRIORITY_MIN, fifo).unwrap();
        let high = sched_priority_to_host(DARWIN_PRIORITY_MAX, fifo).unwrap();
        assert_eq!(low, unsafe { libc::sched_get_priority_min(fifo) });
        assert_eq!(high, unsafe { libc::sched_get_priority_max(fifo) });
        assert_eq!(sched_priority_from_host(low, fifo), DARWIN_PRIORITY_MIN);
        assert_eq!(sched_priority_from_host(high, fifo), DARWIN_PRIORITY_MAX);
    }
}
