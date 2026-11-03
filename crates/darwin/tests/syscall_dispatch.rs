//! Syscall user dispatch is per process once installed, so it is tested in a
//! process of its own.
#![cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]

use darwin::syscall::{ScopedGuestSyscallContext, ScopedHostSyscallContext, setup_syscall_handler};

/// Class 2 (BSD), number 8: unassigned in XNU's table (the retired `creat`),
/// so it answers ENOSYS however many syscalls are implemented.
const DARWIN_UNASSIGNED: u64 = 0x200_0008;
const DARWIN_ENOSYS: u64 = 78;

fn raw_syscall(number: u64) -> (u64, bool) {
    let rax: u64;
    let carry: u8;
    // SAFETY: callers pass syscalls that take no arguments and touch no memory.
    unsafe {
        std::arch::asm!(
            "syscall",
            "setc {carry}",
            carry = out(reg_byte) carry,
            inlateout("rax") number => rax,
            out("rcx") _,
            out("r11") _,
            options(nostack),
        );
    }
    (rax, carry != 0)
}

/// Interposition is opt-in; every test asks for it before anything reads it.
fn enable() {
    static ENABLE: std::sync::Once = std::sync::Once::new();
    // SAFETY: every reader of the environment in this process is behind `ENABLE`
    // or std's own lock.
    ENABLE.call_once(|| unsafe { std::env::set_var("ROSEMARY_SYSCALL_INTERPOSE", "1") });
}

fn on_a_dispatching_thread(test: impl FnOnce() + Send + 'static) {
    enable();
    std::thread::spawn(move || {
        if let Err(error) = setup_syscall_handler() {
            eprintln!("skipping: no syscall user dispatch ({error})");
            return;
        }
        test();
    })
    .join()
    .expect("the test thread panicked");
}

#[test]
fn a_guest_syscall_answers_enosys() {
    on_a_dispatching_thread(|| {
        let _guest = ScopedGuestSyscallContext::new();
        assert_eq!(raw_syscall(DARWIN_UNASSIGNED), (DARWIN_ENOSYS, true));
    });
}

#[test]
fn a_host_context_inside_a_guest_one_reaches_the_kernel() {
    on_a_dispatching_thread(|| {
        let _guest = ScopedGuestSyscallContext::new();
        let _host = ScopedHostSyscallContext::new();
        let (pid, _) = raw_syscall(libc::SYS_getpid as u64);
        assert_eq!(pid, u64::from(std::process::id()));
    });
}

#[test]
fn a_thread_that_enters_guest_code_later_is_armed_too() {
    on_a_dispatching_thread(|| {
        let later = std::thread::spawn(|| {
            let _guest = ScopedGuestSyscallContext::new();
            raw_syscall(DARWIN_UNASSIGNED)
        });
        assert_eq!(later.join().unwrap(), (DARWIN_ENOSYS, true));
    });
}

#[test]
fn libc_syscalls_pass_through_a_guest_context() {
    on_a_dispatching_thread(|| {
        let _guest = ScopedGuestSyscallContext::new();
        // SAFETY: no arguments.
        assert_eq!(unsafe { libc::getpid() } as u32, std::process::id());
    });
}

/// The dynamic loader makes its own syscalls, outside libc.
#[test]
fn a_host_dlopen_works_inside_a_guest_context() {
    on_a_dispatching_thread(|| {
        let _guest = ScopedGuestSyscallContext::new();
        // SAFETY: a library with no initializers that matter here.
        let handle = unsafe { libc::dlopen(c"libresolv.so.2".as_ptr(), libc::RTLD_NOW) };
        assert!(!handle.is_null(), "dlopen failed inside a guest context");
    });
}
