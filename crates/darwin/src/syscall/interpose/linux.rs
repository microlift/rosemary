use std::{
    cell::Cell,
    ffi::{c_int, c_ulong, c_void},
    io, mem,
    ops::Range,
    sync::{Mutex, OnceLock},
};

use starnix::process::Region;

use crate::{
    errno,
    syscall::{
        generic::generic_syscall_entry,
        scope::{SELECTOR, ScopedHostSyscallContext},
        split_syscall_number,
    },
};

const PR_SET_SYSCALL_USER_DISPATCH: c_int = 59;
const PR_SYS_DISPATCH_OFF: c_ulong = 0;
/// Dispatch everything outside the given range.
const PR_SYS_DISPATCH_EXCLUSIVE_ON: c_ulong = 1;
/// `si_code` of a `SIGSYS` that syscall user dispatch raised.
const SYS_USER_DISPATCH: c_int = 2;
const EFLAGS_CF: i64 = 1 << 0;
/// `syscall` is `0f 05`; the trap's `%rip` is past it.
const SYSCALL_LENGTH: u64 = 2;

/// The host libc's text, whose syscalls always reach the kernel. Set once the
/// handler is installed, which is what turns interposition on.
static ZONE: OnceLock<Region> = OnceLock::new();
/// Host text outside libc that makes its own syscalls from places no caller
/// can wrap - the dynamic loader inside any `dlopen`, and the vDSO's fallbacks.
/// A trap from here goes to the kernel as it was.
static HOST_TEXT: OnceLock<Vec<Range<u64>>> = OnceLock::new();
/// The `SIGSYS` disposition from before, for a `SIGSYS` that is not ours.
static PREVIOUS: OnceLock<libc::sigaction> = OnceLock::new();

thread_local! {
    /// Dispatch is per thread and not inherited by `clone`.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

fn libc_text() -> io::Result<Region> {
    let _host = ScopedHostSyscallContext::new();
    let inside = libc::syscall as *const () as usize as u64;
    starnix::process::regions()?
        .into_iter()
        .find(|region| region.execute && (region.start..region.end).contains(&inside))
        .ok_or_else(|| io::Error::other("the host libc's text is not mapped"))
}

/// The executable segments of the dynamic loader and the vDSO.
fn host_text() -> Vec<Range<u64>> {
    unsafe extern "C" fn each(info: *mut libc::dl_phdr_info, _: usize, out: *mut c_void) -> c_int {
        // SAFETY: `dl_iterate_phdr` passes a live description, and `out` is the
        // vector below.
        let (info, out) = unsafe { (&*info, &mut *(out as *mut Vec<Range<u64>>)) };
        // SAFETY: as above; `dlpi_phnum` headers start at `dlpi_phdr`.
        let headers =
            unsafe { std::slice::from_raw_parts(info.dlpi_phdr, info.dlpi_phnum as usize) };
        let loads = || {
            headers
                .iter()
                .filter(|header| header.p_type == libc::PT_LOAD)
        };
        let span = |header: &libc::Elf64_Phdr| {
            let start = info.dlpi_addr + header.p_vaddr;
            start..start + header.p_memsz
        };
        // SAFETY: plain queries of the auxiliary vector.
        let anchors = unsafe {
            [
                libc::getauxval(libc::AT_BASE),
                libc::getauxval(libc::AT_SYSINFO_EHDR),
            ]
        };
        let ours = loads().any(|header| {
            anchors
                .iter()
                .any(|anchor| *anchor != 0 && span(header).contains(anchor))
        });
        if ours {
            out.extend(
                loads()
                    .filter(|header| header.p_flags & libc::PF_X != 0)
                    .map(span),
            );
        }
        0
    }
    let mut ranges = Vec::new();
    // SAFETY: `each` reads only what it is given and writes only `ranges`.
    unsafe { libc::dl_iterate_phdr(Some(each), (&raw mut ranges).cast()) };
    ranges
}

/// Make a syscall the trap stopped, under the host's numbering and conventions.
/// Only from a [`ScopedHostSyscallContext`], or it traps again.
fn to_kernel(number: u64, args: [u64; 6]) -> u64 {
    let rax: u64;
    // SAFETY: the syscall the host code at the trap was making, with its own
    // registers, as if the trap had not happened.
    unsafe {
        std::arch::asm!(
            "syscall",
            inlateout("rax") number => rax,
            in("rdi") args[0],
            in("rsi") args[1],
            in("rdx") args[2],
            in("r10") args[3],
            in("r8") args[4],
            in("r9") args[5],
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    rax
}

fn arm(zone: &Region) -> io::Result<()> {
    let selector = SELECTOR.with(|selector| selector.as_ptr());
    // SAFETY: `selector` is this thread's own, at a fixed address for its life.
    let rc = unsafe {
        libc::prctl(
            PR_SET_SYSCALL_USER_DISPATCH,
            PR_SYS_DISPATCH_EXCLUSIVE_ON,
            zone.start as c_ulong,
            (zone.end - zone.start) as c_ulong,
            selector,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    ARMED.with(|armed| armed.set(true));
    Ok(())
}

fn disarm() {
    // SAFETY: turns dispatch off for this thread; the other arguments must be zero.
    unsafe {
        libc::prctl(
            PR_SET_SYSCALL_USER_DISPATCH,
            PR_SYS_DISPATCH_OFF,
            0 as c_ulong,
            0 as c_ulong,
            0 as c_ulong,
        )
    };
    ARMED.with(|armed| armed.set(false));
}

/// Arm dispatch on this thread, once interposition is on.
pub fn arm_current_thread() {
    let Some(zone) = ZONE.get() else {
        return;
    };
    if ARMED.with(|armed| armed.get()) {
        return;
    }
    // The calling thread was armed when the handler was installed, so a kernel
    // without the feature never gets here.
    arm(zone).expect("the kernel refused syscall user dispatch for a new thread");
}

/// The kernel drops dispatch in a forked child, whose copy of [`ARMED`] still
/// says otherwise; a thread that was guest code keeps being guest code there.
extern "C" fn rearm_in_child() {
    if ARMED.with(|armed| armed.replace(false)) {
        arm_current_thread();
    }
}

extern "C" fn handle_sigsys(signum: c_int, info: *mut libc::siginfo_t, context: *mut c_void) {
    // SAFETY: the kernel passes an `SA_SIGINFO` handler both, live for the call.
    let (code, ucontext) = unsafe { ((*info).si_code, &mut *(context as *mut libc::ucontext_t)) };
    if code != SYS_USER_DISPATCH {
        forward(signum, info, context);
        return;
    }
    // We don't want to mess with the guest's errno if we call into a Darwin function to handle
    // a system call.
    let _errno_guard = errno::Held::now();
    let _host = ScopedHostSyscallContext::new();
    let regs = &mut ucontext.uc_mcontext.gregs;
    let reg = |index: c_int| regs[index as usize] as u64;
    // The kernel rolls `%rax` back to the syscall number before raising.
    let number = reg(libc::REG_RAX);
    let args = [
        reg(libc::REG_RDI),
        reg(libc::REG_RSI),
        reg(libc::REG_RDX),
        reg(libc::REG_R10),
        reg(libc::REG_R8),
        reg(libc::REG_R9),
    ];
    let at = reg(libc::REG_RIP).wrapping_sub(SYSCALL_LENGTH);
    if HOST_TEXT
        .get()
        .is_some_and(|text| text.iter().any(|range| range.contains(&at)))
    {
        regs[libc::REG_RAX as usize] = to_kernel(number, args) as i64;
        return;
    }
    let (darwin_class, darwin_number) = split_syscall_number(number);
    // Darwin reports failure with the carry flag and a positive `errno`.
    let (value, flags) = match generic_syscall_entry(darwin_class, darwin_number, args) {
        Ok(value) => (value as i64, regs[libc::REG_EFL as usize] & !EFLAGS_CF),
        Err(errno) => (errno as i64, regs[libc::REG_EFL as usize] | EFLAGS_CF),
    };
    regs[libc::REG_RAX as usize] = value;
    regs[libc::REG_EFL as usize] = flags;
}

fn forward(signum: c_int, info: *mut libc::siginfo_t, context: *mut c_void) {
    type Handler1 = extern "C" fn(c_int);
    type Handler3 = extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void);
    let previous = PREVIOUS.get();
    match previous.map(|previous| previous.sa_sigaction) {
        Some(libc::SIG_IGN) => {}
        Some(handler) if handler != libc::SIG_DFL => {
            // SAFETY: a handler the host installed, called as it asked to be.
            unsafe {
                match previous.is_some_and(|p| p.sa_flags & libc::SA_SIGINFO != 0) {
                    true => mem::transmute::<libc::sighandler_t, Handler3>(handler)(
                        signum, info, context,
                    ),
                    false => mem::transmute::<libc::sighandler_t, Handler1>(handler)(signum),
                }
            }
        }
        // The signal is blocked while this runs, so the default action is taken
        // as soon as the handler returns.
        _ => unsafe {
            libc::signal(signum, libc::SIG_DFL);
            libc::raise(signum);
        },
    }
}

/// Install the `SIGSYS` handler and arm the calling thread; every thread that
/// enters guest code afterwards is armed as it does.
pub fn install_syscall_handler() -> io::Result<()> {
    static INSTALL: Mutex<()> = Mutex::new(());
    let _install = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
    if ZONE.get().is_some() {
        arm_current_thread();
        return Ok(());
    }
    let zone = libc_text()?;
    let _ = HOST_TEXT.set(host_text());
    // First, so a kernel without the feature (before 5.11) is reported here.
    arm(&zone)?;

    // SAFETY: zeroed is a valid `struct sigaction`.
    let mut act: libc::sigaction = unsafe { mem::zeroed() };
    act.sa_flags = libc::SA_SIGINFO;
    act.sa_sigaction = handle_sigsys as *const () as libc::sighandler_t;
    // SAFETY: as above.
    let mut previous: libc::sigaction = unsafe { mem::zeroed() };
    // SAFETY: two live structures.
    if unsafe { libc::sigaction(libc::SIGSYS, &act, &mut previous) } != 0 {
        let error = io::Error::last_os_error();
        disarm();
        return Err(error);
    }
    let _ = PREVIOUS.set(previous);
    // SAFETY: a child handler that touches only this thread's own state.
    unsafe { libc::pthread_atfork(None, None, Some(rearm_in_child)) };
    // A guest's own `SIGSYS` handler would take the traps from us.
    crate::signal::retain(crate::signal::signum::SIGSYS);
    let _ = ZONE.set(zone);
    Ok(())
}
