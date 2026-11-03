//! Asking another task to do something to itself, through the process server.
//!
//! The local counterpart is [`super::local`]: the same work, which the other
//! task runs from its service handler when the server passes it on.

use server::task::{TaskOp, TaskOutcome};

use super::kern::*;
use super::space::{self, Name};

/// Have task `pid` at `image`, which `name` is this process's right to, do
/// `op`. A task that is gone turns `name` into a dead name, as the kernel
/// does on Darwin, and answers what sending to one answers.
pub fn perform(name: Name, pid: i32, image: u64, op: TaskOp) -> Result<TaskOutcome, i32> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    match server::client::perform(pid, image, op) {
        Some(TaskOutcome::Gone) => {
            space::task_died(name);
            Err(MACH_SEND_INVALID_DEST)
        }
        Some(TaskOutcome::Failed(code)) => Err(code),
        // No server, or a host it cannot reach the task on.
        None | Some(TaskOutcome::Unreachable) => Err(KERN_FAILURE),
        Some(outcome) => Ok(outcome),
    }
}

/// [`perform`] for an operation that answers nothing but success.
pub fn done(name: Name, pid: i32, image: u64, op: TaskOp) -> i32 {
    match perform(name, pid, image, op) {
        Ok(_) => KERN_SUCCESS,
        Err(code) => code,
    }
}
