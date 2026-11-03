//! Interposition of the raw syscalls guest code makes on the host CPU.
//!
//! TODO (ALL): Make it so all relocations made by Rosemary that point to
//! non-Darwin implementations, AKA synthetic dylibs, point to trampolines
//! that set the appropriate SUD state, like how WINE has this exact mechanism.
//! WINE goes as follows:
//!
//! Unix sided runtime -> Switch to guest profile -> Call PE runtime entry point
//! -> Call guest code -> Call reimplementations (PE side) -> Make a host/Unix call
//! -> Restores host profile -> Trampoline restores guest profile -> Returns to PE side

mod generic;
mod interpose;
pub mod numbers;
pub mod scope;

use std::{ffi::c_int, io, sync::LazyLock};

pub use scope::{ScopedGuestSyscallContext, ScopedHostSyscallContext};

use numbers::{SYSCALL_CLASS_MASK, SYSCALL_CLASS_SHIFT, SYSCALL_NUMBER_MASK};

/// Splits a guest's `%rax` at `syscall` into its `SYSCALL_CLASS_*` and the
/// number within that class's table.
pub fn split_syscall_number(rax: u64) -> (u32, u32) {
    // XNU ignores the upper half of the 64-bit syscall number.
    let rax = rax as u32;
    (
        (rax & SYSCALL_CLASS_MASK) >> SYSCALL_CLASS_SHIFT,
        rax & SYSCALL_NUMBER_MASK,
    )
}

pub fn setup_syscall_handler() -> io::Result<()> {
    static SYSCALL_INTERPOSE: LazyLock<bool> = LazyLock::new(|| {
        if let Ok(val) = std::env::var("ROSEMARY_SYSCALL_INTERPOSE") {
            val == "1"
        } else {
            false
        }
    });
    if *SYSCALL_INTERPOSE {
        interpose::install_syscall_handler()
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "syscall interposition was not enabled",
        ))
    }
}

pub unsafe fn clock_gettime(clock: libc::clockid_t, ts: *mut libc::timespec) -> c_int {
    let _host = ScopedHostSyscallContext::new();
    // SAFETY: the caller's guarantee.
    unsafe { libc::clock_gettime(clock, ts) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use numbers::{SYSCALL_CLASS_MACH, SYSCALL_CLASS_UNIX, bsd, mach};

    #[test]
    fn splits_class_from_number() {
        let write = u64::from(SYSCALL_CLASS_UNIX << SYSCALL_CLASS_SHIFT | bsd::SYS_write);
        assert_eq!(
            split_syscall_number(write),
            (SYSCALL_CLASS_UNIX, bsd::SYS_write)
        );
        let mach_msg = u64::from(SYSCALL_CLASS_MACH << SYSCALL_CLASS_SHIFT | mach::mach_msg_trap);
        assert_eq!(
            split_syscall_number(0xdead_0000_0000 | mach_msg),
            (SYSCALL_CLASS_MACH, mach::mach_msg_trap)
        );
    }
}
