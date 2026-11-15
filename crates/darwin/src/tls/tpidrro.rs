//! `TPIDRRO_EL0` on aarch64 Linux, which only the kernel can write: Rosemary's
//! module (`kernel/linux`) keeps a per-thread value there across context
//! switches, as XNU does with a thread's TSD base.

use std::{
    ffi::{c_int, c_ulong},
    fmt,
    fs::File,
    io,
    os::fd::{AsRawFd, RawFd},
    sync::LazyLock,
};

/// `kernel/linux/rosemary.h`; change the two together.
mod abi {
    use std::ffi::c_ulong;

    pub const VERSION: u32 = 1;
    pub const DEVICE: &str = "/dev/rosemary";

    const MAGIC: c_ulong = 0xD7;
    // Linux's generic `_IOC` layout, which arm64 uses.
    const NRSHIFT: c_ulong = 0;
    const TYPESHIFT: c_ulong = 8;
    const SIZESHIFT: c_ulong = 16;
    const DIRSHIFT: c_ulong = 30;
    const WRITE: c_ulong = 1;
    const READ: c_ulong = 2;

    const fn ioc<T>(dir: c_ulong, nr: c_ulong) -> c_ulong {
        (dir << DIRSHIFT)
            | ((size_of::<T>() as c_ulong) << SIZESHIFT)
            | (MAGIC << TYPESHIFT)
            | (nr << NRSHIFT)
    }

    pub const IOC_VERSION: c_ulong = ioc::<u32>(READ, 0);
    pub const IOC_INFO: c_ulong = ioc::<u64>(READ, 1);
    pub const IOC_SET_TPIDRRO: c_ulong = ioc::<u64>(WRITE, 2);

    pub const INFO_KPTI: u64 = 1 << 0;
}

/// Why the register cannot be relied on.
#[derive(Debug)]
enum Unavailable {
    NoDevice(io::Error),
    Version(u32),
    Kpti,
    Lost(&'static str),
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unavailable::NoDevice(e) => write!(
                f,
                "Darwin arm64 code needs TPIDRRO_EL0, which needs Rosemary's kernel module \
                 ({} could not be opened: {e})",
                abi::DEVICE
            ),
            Unavailable::Version(v) => write!(
                f,
                "Rosemary's kernel module speaks ABI {v}, this build needs {}",
                abi::VERSION
            ),
            Unavailable::Kpti => write!(
                f,
                "this kernel uses KPTI, whose trampoline clears TPIDRRO_EL0 on every exception"
            ),
            Unavailable::Lost(when) => write!(
                f,
                "this kernel does not keep TPIDRRO_EL0 {when}, likely because of its \
                 exception-entry trampoline"
            ),
        }
    }
}

static DEVICE: LazyLock<Result<File, Unavailable>> = LazyLock::new(probe);

pub(super) fn available() -> bool {
    DEVICE.is_ok()
}

pub(super) fn refusal() -> Option<String> {
    DEVICE.as_ref().err().map(ToString::to_string)
}

/// Sets this thread's register, or says there is no way to.
pub(super) fn set(value: u64) -> bool {
    let Ok(device) = DEVICE.as_ref() else {
        return false;
    };
    let rc = ioctl_set(device.as_raw_fd(), value);
    assert_eq!(rc, 0, "failed to install the Darwin thread pointer");
    true
}

fn ioctl_set(fd: RawFd, value: u64) -> c_int {
    // SAFETY: the module reads one `u64` through the pointer.
    unsafe { libc::ioctl(fd, abi::IOC_SET_TPIDRRO, &value as *const u64) }
}

fn read() -> u64 {
    let value: u64;
    // SAFETY: EL0 may always read TPIDRRO_EL0.
    unsafe {
        std::arch::asm!("mrs {}, tpidrro_el0", out(reg) value, options(nomem, nostack, preserves_flags))
    };
    value
}

fn ioctl_get<T: Default>(fd: RawFd, request: c_ulong) -> io::Result<T> {
    let mut out = T::default();
    // SAFETY: `request` writes one `T` through the pointer.
    match unsafe { libc::ioctl(fd, request, &mut out as *mut T) } {
        0 => Ok(out),
        _ => Err(io::Error::last_os_error()),
    }
}

/// Checks the guarantee itself rather than kernel configuration: a value set on
/// this thread must still be there after a syscall, a reschedule and a move to
/// another CPU. Leaves the thread with no value.
fn probe() -> Result<File, Unavailable> {
    /// Any value a released thread would not read back.
    const SENTINEL: u64 = 0x0052_4f53_454d_4100;

    let device = File::open(abi::DEVICE).map_err(Unavailable::NoDevice)?;
    let fd = device.as_raw_fd();
    let version = ioctl_get::<u32>(fd, abi::IOC_VERSION).map_err(Unavailable::NoDevice)?;
    if version != abi::VERSION {
        return Err(Unavailable::Version(version));
    }
    let info = ioctl_get::<u64>(fd, abi::IOC_INFO).map_err(Unavailable::NoDevice)?;
    if info & abi::INFO_KPTI != 0 {
        return Err(Unavailable::Kpti);
    }

    if ioctl_set(fd, SENTINEL) != 0 {
        return Err(Unavailable::NoDevice(io::Error::last_os_error()));
    }
    let kept = keeps(SENTINEL);
    ioctl_set(fd, 0);
    kept?;
    // SAFETY: the handler only touches this thread's own state.
    unsafe { libc::pthread_atfork(None, None, Some(rearm_in_child)) };
    Ok(device)
}

fn keeps(value: u64) -> Result<(), Unavailable> {
    if read() != value {
        return Err(Unavailable::Lost("after it is set"));
    }
    // SAFETY: takes nothing and cannot fail.
    unsafe { libc::getppid() };
    if read() != value {
        return Err(Unavailable::Lost("across a syscall"));
    }
    // SAFETY: as above.
    unsafe { libc::sched_yield() };
    if read() != value {
        return Err(Unavailable::Lost("across a reschedule"));
    }
    if migrate() && read() != value {
        return Err(Unavailable::Lost("across a move to another CPU"));
    }
    Ok(())
}

/// Moves this thread to another CPU it may run on and back to its old mask.
/// False when there is nowhere else to go.
fn migrate() -> bool {
    // SAFETY: zeroed is an empty `cpu_set_t`.
    let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::cpu_set_t>();
    // SAFETY: a live set of `size` bytes, for this thread.
    if unsafe { libc::sched_getaffinity(0, size, &mut allowed) } != 0 {
        return false;
    }
    // SAFETY: takes nothing.
    let here = unsafe { libc::sched_getcpu() };
    let Some(there) = (0..libc::CPU_SETSIZE as usize)
        // SAFETY: `cpu` is below `CPU_SETSIZE`.
        .find(|&cpu| cpu as c_int != here && unsafe { libc::CPU_ISSET(cpu, &allowed) })
    else {
        return false;
    };
    // SAFETY: as above.
    let mut only: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: as above.
    unsafe { libc::CPU_SET(there, &mut only) };
    // SAFETY: live sets of `size` bytes, for this thread. Changing the mask
    // moves the thread before the call returns.
    let moved = unsafe { libc::sched_setaffinity(0, size, &only) } == 0;
    // SAFETY: as above.
    unsafe { libc::sched_setaffinity(0, size, &allowed) };
    moved
}

/// The module ties a value to a task, and `fork`'s child is a new one.
extern "C" fn rearm_in_child() {
    let installed = super::INSTALLED.with(|at| at.get());
    if installed != 0
        && let Ok(device) = DEVICE.as_ref()
    {
        ioctl_set(device.as_raw_fd(), installed);
    }
}
