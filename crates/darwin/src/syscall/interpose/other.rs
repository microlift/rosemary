use std::io;

/// No syscall user dispatch here: macOS takes the guest's syscalls itself, and
/// other hosts (Linux on aarch64, and musl, included) have nothing to trap
/// them with here.
pub fn install_syscall_handler() -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn arm_current_thread() {}
