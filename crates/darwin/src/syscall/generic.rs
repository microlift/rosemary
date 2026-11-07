use static_init::dynamic;

use crate::{
    errno::{self, DARWIN_ENOSYS},
    mach,
    syscall::numbers::{self, SYSCALL_CLASS_MACH, SYSCALL_CLASS_UNIX},
};

/// Calls a shim that reports failure C-style, `-1` with the reason in `errno`,
/// on guest registers narrowed to its parameter types, and answers as the raw
/// syscall would. `errno` itself is left alone by the caller's `errno::Held`.
macro_rules! unix_call {
    (unsafe $($function:ident)::+ ($($arg:expr),* $(,)?)) => {
        match unsafe { $($function)::+($($arg as _),*) } as i64 {
            -1 => Err(errno::guest_errno()),
            value => Ok(value as u64),
        }
    };
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord)]
enum SyscallTracingMode {
    None,
    All,
    Unknown,
}

fn get_syscall_tracing_mode() -> SyscallTracingMode {
    #[dynamic]
    static MODE: SyscallTracingMode = {
        match std::env::var("ROSEMARY_SYSCALL_TRACE").as_deref() {
            Ok("all") => SyscallTracingMode::All,
            Ok("unknown") => SyscallTracingMode::Unknown,
            _ => SyscallTracingMode::None,
        }
    };

    *MODE
}

fn handle_unix_syscall(number: u32, args: [u64; 6]) -> Result<u64, i32> {
    match number {
        numbers::bsd::SYS_openat => {
            unix_call!(unsafe crate::stat::openat(args[0], args[1], args[2], args[3]))
        }
        numbers::bsd::SYS_open => {
            unix_call!(unsafe crate::stat::open(args[0], args[1], args[2]))
        }
        numbers::bsd::SYS_close => {
            unix_call!(unsafe crate::net::descriptor::close(args[0]))
        }
        numbers::bsd::SYS_dup => {
            unix_call!(unsafe crate::net::descriptor::dup(args[0]))
        }
        numbers::bsd::SYS_dup2 => {
            unix_call!(unsafe crate::net::descriptor::dup2(args[0], args[1]))
        }
        numbers::bsd::SYS_getpid => Ok(crate::process::getpid() as _),
        numbers::bsd::SYS_getppid => Ok(crate::process::getppid() as _),
        numbers::bsd::SYS_munmap => {
            unix_call!(unsafe crate::vmmap::munmap(args[0], args[1]))
        }
        numbers::bsd::SYS_mprotect => {
            unix_call!(unsafe crate::vmmap::mprotect(args[1], args[2], args[3]))
        }
        numbers::bsd::SYS_madvise => {
            unix_call!(unsafe crate::vmmap::madvise(args[1], args[2], args[3]))
        }
        numbers::bsd::SYS_fcntl => {
            unix_call!(unsafe crate::fcntl::entry::fcntl(args[0], args[1], args[2]))
        }
        numbers::bsd::SYS_socket => {
            unix_call!(unsafe crate::net::socket::socket(args[0], args[1], args[2]))
        }
        numbers::bsd::SYS_connect => {
            unix_call!(unsafe crate::net::socket::connect(args[0], args[1], args[2]))
        }
        numbers::bsd::SYS_disconnectx => {
            unix_call!(unsafe crate::net::socket::disconnectx(args[0], args[1], args[2]))
        }
        numbers::bsd::SYS_socketpair => {
            unix_call!(unsafe crate::net::socket::socketpair(args[0], args[1], args[2], args[3]))
        }
        numbers::bsd::SYS_exit => unsafe {
            libc::_exit(args[0] as _);
        },
        numbers::bsd::SYS_open_dprotected_np => {
            unix_call!(unsafe crate::stat::open_dprotected_np(args[0], args[1], args[2], args[3], args[4]))
        }
        numbers::bsd::SYS_access => {
            unix_call!(unsafe crate::stat::access(args[0], args[1]))
        }
        numbers::bsd::SYS_stat | numbers::bsd::SYS_stat64 => {
            unix_call!(unsafe crate::stat::stat(args[0], args[1]))
        }
        numbers::bsd::SYS_fstat | numbers::bsd::SYS_fstat64 => {
            unix_call!(unsafe crate::stat::fstat(args[0], args[1]))
        }
        numbers::bsd::SYS_statfs | numbers::bsd::SYS_statfs64 => {
            unix_call!(unsafe crate::stat::statfs(args[0], args[1]))
        }
        numbers::bsd::SYS_fstatfs | numbers::bsd::SYS_fstatfs64 => {
            unix_call!(unsafe crate::stat::fstatfs(args[0], args[1]))
        }
        _ => Err(DARWIN_ENOSYS),
    }
}

fn handle_mach_syscall(number: u32, args: [u64; 6]) -> Result<u64, i32> {
    match number {
        numbers::mach::task_self_trap => Ok(mach::task::mach_task_self().into()),
        numbers::mach::_kernelrpc_mach_vm_allocate_trap => Ok(unsafe {
            mach::vm::vm_allocate(args[0] as _, args[1] as _, args[2], args[3] as _) as _
        }),
        numbers::mach::_kernelrpc_mach_vm_protect_trap => Ok(unsafe {
            mach::vm::vm_protect(args[0] as _, args[1], args[2], args[3] as _, args[4] as _) as _
        }),
        numbers::mach::_kernelrpc_mach_vm_map_trap => Ok(unsafe {
            mach::vm::mach_vm_map_trap(
                args[0] as _,
                args[1] as _,
                args[2],
                args[3],
                args[4] as _,
                args[5] as _,
            ) as _
        }),
        numbers::mach::_kernelrpc_mach_vm_deallocate_trap => {
            Ok(unsafe { mach::vm::vm_deallocate(args[0] as _, args[1], args[2]) as _ })
        }
        numbers::mach::thread_self_trap => Ok(mach::task::mach_thread_self().into()),
        numbers::mach::host_self_trap => Ok(mach::task::mach_host_self().into()),
        // FIXME (ACCCURAY): task_for_pid is the privileged variant of task_name_for_pid, they are not equal
        numbers::mach::task_name_for_pid | numbers::mach::task_for_pid => {
            Ok(unsafe { mach::tasks::task_for_pid(args[0] as _, args[1] as _, args[2] as _) } as _)
        }
        numbers::mach::mach_wait_until_trap => Ok(unsafe { mach::wait_until(args[0]) } as _),
        _ => Err(DARWIN_ENOSYS),
    }
}

/// A Darwin syscall the guest made directly: `number` as it was in `%rax`, class
/// bits included, and the six argument registers. Answers the value for `%rax`,
/// or a Darwin `errno`.
///
/// Must be async-signal-safe: it runs in the `SIGSYS` handler.
pub(crate) fn generic_syscall_entry(class: u32, number: u32, args: [u64; 6]) -> Result<u64, i32> {
    let tracing_mode = get_syscall_tracing_mode();

    if tracing_mode == SyscallTracingMode::All {
        eprintln!("rosemary: SYSCALL BEGIN: {number:#x} CLASS {class:#x} ARGS {args:?}");
    }

    let res = match class {
        SYSCALL_CLASS_UNIX => handle_unix_syscall(number, args),
        SYSCALL_CLASS_MACH => handle_mach_syscall(number, args),
        _ => Err(DARWIN_ENOSYS),
    };

    if tracing_mode == SyscallTracingMode::All {
        match res {
            Ok(res) => {
                eprintln!("rosemary: SYSCALL SUCCESS: {number:#x} CLASS {class:#x} RES {res}")
            }
            Err(errno) => {
                eprintln!("rosemary: SYSCALL FAILED: {number:#x} CLASS {class:#x} ERR {errno}")
            }
        }
    } else if tracing_mode == SyscallTracingMode::Unknown
        && let Err(errno) = res
        && errno == DARWIN_ENOSYS
    {
        eprintln!("rosemary: UNRECOGNIZED SYSCALL {number:#x} CLASS {class:#x} ARGS {args:?}");
    }
    res
}
