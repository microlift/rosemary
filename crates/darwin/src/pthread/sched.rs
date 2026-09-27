use std::ffi::c_int;

use crate::translation::DarwinTranslation;

#[repr(C)]
pub struct SchedParam {
    pub sched_priority: c_int,
}

impl DarwinTranslation<libc::sched_param> for SchedParam {
    fn to_host(&self) -> libc::sched_param {
        let mut param: libc::sched_param = unsafe { core::mem::zeroed() };
        param.sched_priority = self.sched_priority;
        param
    }

    fn from_host(host: &libc::sched_param) -> Self {
        Self {
            sched_priority: host.sched_priority,
        }
    }
}

/// On some operating systems, particularly Windows and some BSDs, inputting a NULL `policy` or `param` is considered invalid and not optional.
/// Additionally, SCHED_ constants are different between systems.
pub unsafe extern "C" fn pthread_getschedparam(
    thread: u64,
    policy: *mut c_int,
    param: *mut SchedParam,
) -> c_int {
    let mut host_policy: i32 = 0;
    let mut host_param = std::mem::MaybeUninit::<libc::sched_param>::uninit();
    let result = unsafe {
        libc::pthread_getschedparam(
            super::thread_of(thread),
            &mut host_policy,
            host_param.as_mut_ptr(),
        )
    };
    if result != 0 {
        // MAYBE_INACCURATE: Darwin can only return ESRCH - do we emulate those semantics?
        return crate::errno::from_host(result);
    }
    unsafe {
        if !param.is_null() {
            *param = SchedParam::from_host(&host_param.assume_init());
        }
        if !policy.is_null() {
            *policy = super::consts::sched_policy_from_host(host_policy);
        }
    }

    0
}

pub unsafe extern "C" fn pthread_setschedparam(
    thread: u64,
    policy: c_int,
    param: *const SchedParam,
) -> i32 {
    let Some(host_policy) = super::consts::sched_policy_to_host(policy) else {
        return super::consts::err::EINVAL;
    };
    if param.is_null() {
        return super::consts::err::EINVAL;
    }
    // SAFETY: the caller supplied a readable scheduling parameter.
    let mut host_param = unsafe { (*param).to_host() };
    let Some(priority) =
        super::consts::sched_priority_to_host(host_param.sched_priority, host_policy)
    else {
        return super::consts::err::EINVAL;
    };
    host_param.sched_priority = priority;
    crate::errno::from_host(unsafe {
        libc::pthread_setschedparam(super::thread_of(thread), host_policy, &host_param)
    })
}
