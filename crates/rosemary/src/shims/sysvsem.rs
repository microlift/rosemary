use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{GuestMemory, Result};

use super::set_int_return;
use crate::shims::bind;

/// The shim for `name`, if this module has one.
pub fn shim_for(name: &str) -> Option<super::Shim> {
    match name {
        "semctl" => Some(bind!("semctl" => semctl)),
        _ => None,
    }
}

/// `int semctl(int semid, int semnum, int cmd, ...)`
fn semctl(_symbol: &str, ctx: &mut RegisterContext, _mem: &mut dyn GuestMemory) -> Result<()> {
    let semid = ctx.rdi as i32;
    let semnum = ctx.rsi as i32;
    let cmd = ctx.rdx as i32;
    let arg = if takes_fourth_argument(cmd) {
        ctx.rcx
    } else {
        0
    };

    // SAFETY: the fourth parameter is an `int` or a pointer into the guest,
    // which is identity-mapped and so is a pointer here; the commands that read
    // it are exactly the ones listed below. Whether the semaphore set exists is
    // the kernel's judgement, reported through the return value.
    let rc = unsafe { libc::semctl(semid, semnum, cmd, arg as usize) };
    set_int_return(ctx, rc);
    Ok(())
}

/// The `semctl` command names the `libc` crate declares on Linux and not on
/// FreeBSD. These are FreeBSD's own numbers, which are 4.4BSD's and so Darwin's
/// as well - they are *not* Linux's.
#[cfg(target_os = "freebsd")]
#[allow(dead_code)]
mod sem {
    pub const GETNCNT: i32 = 3;
    pub const GETPID: i32 = 4;
    pub const GETVAL: i32 = 5;
    pub const GETALL: i32 = 6;
    pub const GETZCNT: i32 = 7;
    pub const SETVAL: i32 = 8;
    pub const SETALL: i32 = 9;
}
#[cfg(not(target_os = "freebsd"))]
use libc as sem;

/// Whether `cmd` reads a fourth argument.
fn takes_fourth_argument(cmd: i32) -> bool {
    matches!(
        cmd,
        libc::IPC_STAT | libc::IPC_SET | sem::GETALL | sem::SETALL | sem::SETVAL
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This shim reads no guest memory: the union argument is passed by value or
    /// by address, untouched either way.
    struct NoMemory;

    impl GuestMemory for NoMemory {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            Err(machish::Error::OutOfBounds {
                addr: va,
                len: out.len(),
            })
        }
        fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
            Err(machish::Error::OutOfBounds {
                addr: va,
                len: data.len(),
            })
        }
    }

    #[test]
    fn only_the_commands_with_a_union_argument_pass_a_register_on() {
        for cmd in [
            libc::IPC_STAT,
            libc::IPC_SET,
            sem::GETALL,
            sem::SETALL,
            sem::SETVAL,
        ] {
            assert!(takes_fourth_argument(cmd), "{cmd}");
        }
        for cmd in [
            libc::IPC_RMID,
            sem::GETNCNT,
            sem::GETPID,
            sem::GETVAL,
            sem::GETZCNT,
        ] {
            assert!(!takes_fourth_argument(cmd), "{cmd}");
        }
    }

    #[test]
    fn a_semaphore_set_can_be_made_read_and_removed_through_the_shim() {
        // The whole path, against the real kernel.
        let key = libc::IPC_PRIVATE;
        // SAFETY: `IPC_PRIVATE` always makes a fresh set, owned by this process.
        let id = unsafe { libc::semget(key, 1, 0o600 | libc::IPC_CREAT) };
        if id < 0 {
            // System V semaphores can be absent from a sandbox; the shim's own
            // decision table is still checked by the test above.
            return;
        }
        // Some sandboxes allow creating a set but refuse every semctl operation.
        if unsafe { libc::semctl(id, 0, sem::SETVAL, 7) } < 0 {
            let error = std::io::Error::last_os_error().raw_os_error();
            if matches!(error, Some(libc::EPERM | libc::EACCES)) {
                return;
            }
            panic!("semctl SETVAL failed: {error:?}");
        }

        let mut ctx = RegisterContext::new();
        let mut mem = NoMemory;

        ctx.rdi = id as u64;
        ctx.rsi = 0;
        ctx.rdx = sem::SETVAL as u64;
        ctx.rcx = 7;
        semctl("semctl", &mut ctx, &mut mem).unwrap();
        assert_eq!(ctx.rax as i32, 0);

        ctx.rdx = sem::GETVAL as u64;
        ctx.rcx = 0;
        semctl("semctl", &mut ctx, &mut mem).unwrap();
        assert_eq!(ctx.rax as i32, 7);

        ctx.rdx = libc::IPC_RMID as u64;
        semctl("semctl", &mut ctx, &mut mem).unwrap();
        assert_eq!(ctx.rax as i32, 0);
    }
}
