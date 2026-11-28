//! Multiprocessing Services: the task identity calls that survive in CarbonCore.

use std::ffi::c_void;

/// `MPTaskID MPCurrentTaskID(void)`
///
/// CarbonCore names a task by its thread, so every thread has an identity of
/// its own and the same one on each call.
pub unsafe extern "C" fn MPCurrentTaskID() -> *mut c_void {
    // SAFETY: no preconditions.
    unsafe { darwin::pthread::handle::pthread_self() as usize as *mut c_void }
}

/// `Boolean MPTaskIsPreemptive(MPTaskID)`
///
/// Every task on Mac OS X is a preemptively scheduled thread.
pub unsafe extern "C" fn MPTaskIsPreemptive(_task: *mut c_void) -> u8 {
    1
}

exports::exports! {
    functions {
        "MPCurrentTaskID" => MPCurrentTaskID,
        "MPTaskIsPreemptive" => MPTaskIsPreemptive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_thread_is_a_preemptive_task_of_its_own() {
        // SAFETY: no preconditions.
        let mine = unsafe { MPCurrentTaskID() };
        // SAFETY: as above.
        assert_eq!(unsafe { MPCurrentTaskID() }, mine);
        let theirs = std::thread::spawn(|| unsafe { MPCurrentTaskID() } as usize)
            .join()
            .unwrap();
        assert_ne!(theirs, mine as usize);
        // SAFETY: any task identity is accepted.
        assert_eq!(unsafe { MPTaskIsPreemptive(mine) }, 1);
    }
}
