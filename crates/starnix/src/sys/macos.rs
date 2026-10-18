//! macOS: `libproc`, `sysctl` and the Mach host statistics.

pub mod signal;

use std::ffi::{CStr, c_int, c_void};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use super::sysctl;
use crate::process::{
    Audit, Descriptor, DescriptorKind, Identity, Process, Region, State, Thread, Usage,
};
use crate::socket::Peer;
use crate::system::{CpuTicks, Memory, page_size};

/// `<sys/proc_info.h>` flavours the `libc` crate does not name.
const PROC_PIDREGIONINFO: c_int = 7;
const PROC_PIDTHREADID64INFO: c_int = 15;
const PROC_PIDLISTTHREADIDS: c_int = 28;

// Declared here rather than taken from `libc`, which deprecates its Mach
// bindings in favour of a crate this one does not otherwise need.
#[repr(C)]
struct Timebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut Timebase) -> c_int;
    fn mach_host_self() -> libc::mach_port_t;
}

/// `NODEV`: no controlling terminal.
const NODEV: u32 = u32::MAX;

/// `struct proc_regioninfo`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RegionInfo {
    protection: u32,
    max_protection: u32,
    inheritance: u32,
    flags: u32,
    offset: u64,
    behavior: u32,
    user_wired_count: u32,
    user_tag: u32,
    pages_resident: u32,
    pages_shared_now_private: u32,
    pages_swapped_out: u32,
    pages_dirtied: u32,
    ref_count: u32,
    shadow_depth: u32,
    share_mode: u32,
    private_pages_resident: u32,
    shared_pages_resident: u32,
    obj_id: u32,
    depth: u32,
    address: u64,
    size: u64,
}

const _: () = assert!(size_of::<RegionInfo>() == 96);

/// One fixed-size `proc_pidinfo` record, or `None` if the process is not
/// there or not this user's.
fn pidinfo<T: Copy>(pid: i32, flavor: c_int, arg: u64) -> Option<T> {
    // SAFETY: every `proc_*info` structure is plain data, valid when zeroed.
    let mut value: T = unsafe { std::mem::zeroed() };
    let size = size_of::<T>() as c_int;
    // SAFETY: `value` is writable for `size` bytes.
    let got = unsafe { libc::proc_pidinfo(pid, flavor, arg, (&raw mut value).cast(), size) };
    (got == size).then_some(value)
}

/// A `proc_pidinfo` list of `T`, with room for a few more than the `expected`
/// count, since the list can grow between asking and reading.
fn pidinfo_list<T: Copy>(pid: i32, flavor: c_int, expected: usize) -> Option<Vec<T>> {
    let mut list: Vec<T> = Vec::with_capacity(expected + 16);
    let room = (list.capacity() * size_of::<T>()) as c_int;
    // SAFETY: `list` has capacity for `room` bytes.
    let got = unsafe { libc::proc_pidinfo(pid, flavor, 0, list.as_mut_ptr().cast(), room) };
    if got <= 0 {
        return None;
    }
    // SAFETY: the kernel wrote `got` bytes of whole records.
    unsafe { list.set_len(got as usize / size_of::<T>()) };
    Some(list)
}

fn bsdinfo(pid: i32) -> Option<libc::proc_bsdinfo> {
    pidinfo(pid, libc::PROC_PIDTBSDINFO, 0)
}

fn state(status: u32) -> State {
    // `p_stat`: `SIDL`, `SRUN`, `SSLEEP`, `SSTOP`, `SZOMB`.
    match status {
        1 | 2 => State::Running,
        4 => State::Stopped,
        5 => State::Zombie,
        _ => State::Sleeping,
    }
}

/// Mach absolute time to nanoseconds, which on Apple silicon differ.
fn abstime_ns(ticks: u64) -> u64 {
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = Timebase { numer: 0, denom: 0 };
        // SAFETY: one writable structure.
        unsafe { mach_timebase_info(&mut info) };
        (info.numer.max(1) as u64, info.denom.max(1) as u64)
    });
    (ticks as u128 * numer as u128 / denom as u128) as u64
}

fn executable(pid: i32) -> Option<PathBuf> {
    let mut path = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `path` is writable for its length.
    let n = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    let n = usize::try_from(n).ok().filter(|&n| n > 0)?;
    Some(PathBuf::from(
        String::from_utf8_lossy(&path[..n]).into_owned(),
    ))
}

/// libproc omits exiting processes before they become zombies;
/// KERN_PROC_PID retains them until their parent reaps.
/// Offsets are the 64-bit kinfo_proc layout in <sys/sysctl.h>.
fn kernel_process(pid: i32) -> Option<Process> {
    let bytes = sysctl::bytes(&[libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid]).ok()?;
    if bytes.len() != 648 {
        return None;
    }
    let word = |at| i32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
    let wide = |at| i64::from_ne_bytes(bytes[at..at + 8].try_into().unwrap());
    if word(40) != pid {
        return None;
    }
    let tty = word(572) as u32;
    let (tty_major, tty_minor) = if tty == NODEV {
        (-1, -1)
    } else {
        (((tty >> 24) & 0xff) as i32, (tty & 0xff_ffff) as i32)
    };
    Some(Process {
        ppid: word(560),
        pgid: word(564),
        tty_major,
        tty_minor,
        tpgid: word(576),
        uid: word(392) as u32,
        euid: word(420) as u32,
        suid: word(396) as u32,
        gid: word(400) as u32,
        egid: word(428) as u32,
        sgid: word(404) as u32,
        state: state(bytes[36] as u32),
        nice: bytes[242] as i8 as i32,
        start_sec: wide(0),
        start_usec: word(8),
        ..Process::default()
    })
}

pub fn identity(pid: i32) -> Option<Identity> {
    let Some(info) = bsdinfo(pid) else {
        let process = kernel_process(pid)?;
        return Some(Identity {
            start: (process.start_sec as u64)
                .wrapping_mul(1_000_000)
                .wrapping_add(process.start_usec as u64),
            exe: (0, 0),
        });
    };
    let exe = executable(pid)
        .and_then(|path| std::fs::metadata(path).ok())
        // A zombie has no executable any more, and is still the same process.
        .map_or((0, 0), |meta| (meta.dev(), meta.ino()));
    Some(Identity {
        start: info
            .pbi_start_tvsec
            .wrapping_mul(1_000_000)
            .wrapping_add(info.pbi_start_tvusec),
        exe,
    })
}

pub fn process(pid: i32) -> Option<Process> {
    let Some(info) = bsdinfo(pid) else {
        return kernel_process(pid);
    };
    let (tty_major, tty_minor) = match info.e_tdev {
        NODEV => (-1, -1),
        dev => (((dev >> 24) & 0xff) as i32, (dev & 0xff_ffff) as i32),
    };
    // SAFETY: `getsid` only reads its argument.
    let sid = unsafe { libc::getsid(pid) };
    Some(Process {
        ppid: info.pbi_ppid as i32,
        pgid: info.pbi_pgid as i32,
        sid: sid.max(0),
        tty_major,
        tty_minor,
        tpgid: info.e_tpgid as i32,
        uid: info.pbi_ruid,
        euid: info.pbi_uid,
        suid: info.pbi_svuid,
        gid: info.pbi_rgid,
        egid: info.pbi_gid,
        sgid: info.pbi_svgid,
        state: state(info.pbi_status),
        nice: info.pbi_nice,
        start_sec: info.pbi_start_tvsec as i64,
        start_usec: info.pbi_start_tvusec as i32,
    })
}

/// `pid`'s mappings, walked one `PROC_PIDREGIONINFO` at a time.
fn regions_of(pid: i32) -> Vec<Region> {
    let mut regions = Vec::new();
    let mut address = 0u64;
    while let Some(info) = pidinfo::<RegionInfo>(pid, PROC_PIDREGIONINFO, address) {
        if info.size == 0 {
            break;
        }
        // `SM_SHARED`, `SM_TRUESHARED`, `SM_SHARED_ALIASED`.
        let shared = matches!(info.share_mode, 3 | 5 | 7);
        regions.push(Region {
            start: info.address,
            end: info.address + info.size,
            read: info.protection & 1 != 0,
            write: info.protection & 2 != 0,
            execute: info.protection & 4 != 0,
            shared,
            offset: info.offset,
        });
        address = info.address + info.size;
    }
    regions
}

pub fn usage(pid: i32) -> Option<Usage> {
    let task: libc::proc_taskinfo = pidinfo(pid, libc::PROC_PIDTASKINFO, 0)?;
    // SAFETY: plain data, valid when zeroed.
    let mut rusage: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY: `rusage` is the structure the flavour asks for.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            (&raw mut rusage).cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return None;
    }
    let faults = task.pti_faults.max(0) as u64;
    let pageins = task.pti_pageins.max(0) as u64;
    Some(Usage {
        virtual_size: task.pti_virtual_size,
        resident_size: task.pti_resident_size,
        // Not in this flavour of `rusage_info`.
        resident_peak: task.pti_resident_size,
        shared_size: 0,
        user_ns: abstime_ns(rusage.ri_user_time),
        system_ns: abstime_ns(rusage.ri_system_time),
        minor_faults: faults.saturating_sub(pageins),
        major_faults: pageins,
        // Counted only as one total.
        voluntary_switches: task.pti_csw.max(0) as u64,
        involuntary_switches: 0,
        threads: task.pti_threadnum.max(0) as u32,
        running_threads: task.pti_numrunning.max(0) as u32,
        regions: regions_of(pid).len() as u32,
        read_bytes: rusage.ri_diskio_bytesread,
        write_bytes: rusage.ri_diskio_byteswritten,
        start_ns: abstime_ns(rusage.ri_proc_start_abstime),
    })
}

pub fn threads(pid: i32) -> Option<Vec<Thread>> {
    let task: libc::proc_taskinfo = pidinfo(pid, libc::PROC_PIDTASKINFO, 0)?;
    let nice = bsdinfo(pid).map_or(0, |info| info.pbi_nice);
    let ids: Vec<u64> = pidinfo_list(
        pid,
        PROC_PIDLISTTHREADIDS,
        task.pti_threadnum.max(0) as usize,
    )?;
    Some(
        ids.into_iter()
            .filter_map(|id| {
                // A thread that exited between the listing and this read is
                // gone, not an error.
                let info: libc::proc_threadinfo = pidinfo(pid, PROC_PIDTHREADID64INFO, id)?;
                let name = unsafe { CStr::from_ptr(info.pth_name.as_ptr()) };
                Some(Thread {
                    tid: id as i32,
                    name: name.to_string_lossy().into_owned(),
                    user_ns: info.pth_user_time,
                    system_ns: info.pth_system_time,
                    // `TH_STATE_*`: running, stopped, waiting,
                    // uninterruptible, halted.
                    state: match info.pth_run_state {
                        1 => State::Running,
                        2 | 5 => State::Stopped,
                        4 => State::Waiting,
                        _ => State::Sleeping,
                    },
                    nice,
                })
            })
            .collect(),
    )
}

pub fn has_thread(tid: i32) -> bool {
    threads(crate::process::me()).is_some_and(|threads| threads.iter().any(|t| t.tid == tid))
}

/// `KERN_PROCARGS2`: `argc`, the executable path, padding, then the arguments,
/// which are what comes back.
pub fn command_line(pid: i32) -> Option<Vec<u8>> {
    let bytes = sysctl::bytes(&[libc::CTL_KERN, libc::KERN_PROCARGS2, pid]).ok()?;
    let argc = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
    let rest = &bytes[4..];
    let path_end = rest.iter().position(|&b| b == 0)?;
    let rest = &rest[path_end..];
    let start = rest.iter().position(|&b| b != 0)?;
    let mut out = Vec::new();
    for argument in rest[start..].split(|&b| b == 0).take(argc.max(0) as usize) {
        out.extend_from_slice(argument);
        out.push(0);
    }
    Some(out)
}

pub fn descriptors() -> io::Result<Vec<Descriptor>> {
    let me = crate::process::me();
    // SAFETY: a null buffer asks for the size.
    let needed =
        unsafe { libc::proc_pidinfo(me, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        return Err(io::Error::last_os_error());
    }
    let expected = needed as usize / size_of::<libc::proc_fdinfo>();
    let fds: Vec<libc::proc_fdinfo> =
        pidinfo_list(me, libc::PROC_PIDLISTFDS, expected).ok_or_else(io::Error::last_os_error)?;
    Ok(fds
        .into_iter()
        .map(|entry| Descriptor {
            fd: entry.proc_fd,
            kind: match entry.proc_fdtype as c_int {
                libc::PROX_FDTYPE_SOCKET => DescriptorKind::Socket,
                libc::PROX_FDTYPE_PIPE => DescriptorKind::Pipe,
                libc::PROX_FDTYPE_KQUEUE => DescriptorKind::Queue,
                _ => DescriptorKind::File,
            },
        })
        .collect())
}

pub fn regions() -> io::Result<Vec<Region>> {
    Ok(regions_of(crate::process::me()))
}

/// The real BSM ids are a Darwin API, answered by the host itself to anything
/// that asks; nothing here needs to stand in for them.
pub fn audit() -> Audit {
    Audit::default()
}

fn host() -> libc::mach_port_t {
    static HOST: OnceLock<libc::mach_port_t> = OnceLock::new();
    // SAFETY: no preconditions; the right is kept for the process's life.
    *HOST.get_or_init(|| unsafe { mach_host_self() })
}

pub fn memory() -> Memory {
    // SAFETY: plain data, valid when zeroed.
    let mut vm: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `vm` is `count` words long.
    let rc = unsafe {
        libc::host_statistics64(
            host(),
            libc::HOST_VM_INFO64,
            (&raw mut vm).cast(),
            &mut count,
        )
    };
    if rc != 0 {
        return Memory::default();
    }
    let page = page_size();
    let bytes = |pages: u32| pages as u64 * page;
    Memory {
        free: bytes(vm.free_count),
        active: bytes(vm.active_count),
        inactive: bytes(vm.inactive_count),
        wired: bytes(vm.wire_count),
        file_backed: bytes(vm.external_page_count),
        anonymous: bytes(vm.internal_page_count),
    }
}

pub fn cpu_ticks() -> CpuTicks {
    // SAFETY: plain data, valid when zeroed.
    let mut load: libc::host_cpu_load_info = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: `load` is `count` words long.
    let rc = unsafe {
        libc::host_statistics(
            host(),
            libc::HOST_CPU_LOAD_INFO,
            (&raw mut load).cast(),
            &mut count,
        )
    };
    if rc != 0 {
        return CpuTicks::default();
    }
    // `CPU_STATE_USER`, `SYSTEM`, `IDLE`, `NICE`.
    let [user, system, idle, nice] = load.cpu_ticks.map(u64::from);
    CpuTicks {
        user,
        nice,
        system,
        idle,
    }
}

pub fn physical_cores() -> Option<u32> {
    sysctl::by_name::<c_int>(c"hw.physicalcpu")
        .filter(|&cores| cores > 0)
        .map(|cores| cores as u32)
}

pub fn cpu_brand() -> Option<String> {
    sysctl::string_by_name(c"machdep.cpu.brand_string")
}

pub fn peer(fd: RawFd) -> Option<Peer> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: two writable ids.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return None;
    }
    let mut pid: libc::pid_t = 0;
    let mut len = size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: `pid` is a `pid_t` and `len` says so.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast::<c_void>(),
            &mut len,
        )
    };
    Some(Peer {
        pid: (rc == 0 && pid > 0).then_some(pid),
        uid,
    })
}

pub fn in_use(_pid: i32) -> Option<crate::process::InUse> {
    None
}

pub fn fd_path(fd: RawFd) -> Option<PathBuf> {
    let mut path = [0u8; libc::MAXPATHLEN as usize];
    // SAFETY: `F_GETPATH` writes at most `MAXPATHLEN` bytes.
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, path.as_mut_ptr()) } != 0 {
        return None;
    }
    let path = CStr::from_bytes_until_nul(&path).ok()?;
    Some(PathBuf::from(path.to_string_lossy().into_owned()))
}

pub fn fd_target(fd: RawFd) -> Option<PathBuf> {
    fd_path(fd)
}

pub fn errno_location() -> *mut libc::c_int {
    // SAFETY: takes no argument and returns this thread's own slot.
    unsafe { libc::__error() }
}

fn xattr_result(rc: libc::ssize_t) -> io::Result<usize> {
    usize::try_from(rc).map_err(|_| io::Error::last_os_error())
}

fn xattr_status(rc: libc::c_int) -> io::Result<()> {
    match rc {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

fn xattr_options(follow: bool) -> libc::c_int {
    if follow { 0 } else { libc::XATTR_NOFOLLOW }
}

pub fn xattr_get_fd(fd: RawFd, name: &CStr, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: a NUL-terminated name and a buffer of its own length.
    xattr_result(unsafe {
        libc::fgetxattr(fd, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, 0)
    })
}

pub fn xattr_get_path(path: &CStr, name: &CStr, buf: &mut [u8], follow: bool) -> io::Result<usize> {
    // SAFETY: NUL-terminated strings and a buffer of its own length.
    xattr_result(unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
            xattr_options(follow),
        )
    })
}

pub fn xattr_set_fd(fd: RawFd, name: &CStr, value: &[u8]) -> io::Result<()> {
    // SAFETY: a NUL-terminated name and the value's own bytes.
    xattr_status(unsafe {
        libc::fsetxattr(fd, name.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0)
    })
}

pub fn xattr_set_path(path: &CStr, name: &CStr, value: &[u8], follow: bool) -> io::Result<()> {
    // SAFETY: NUL-terminated strings and the value's own bytes.
    xattr_status(unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            xattr_options(follow),
        )
    })
}

pub fn xattr_remove_fd(fd: RawFd, name: &CStr) -> io::Result<()> {
    // SAFETY: a NUL-terminated name.
    xattr_status(unsafe { libc::fremovexattr(fd, name.as_ptr(), 0) })
}

pub fn xattr_remove_path(path: &CStr, name: &CStr, follow: bool) -> io::Result<()> {
    // SAFETY: NUL-terminated strings.
    xattr_status(unsafe { libc::removexattr(path.as_ptr(), name.as_ptr(), xattr_options(follow)) })
}

pub fn xattr_is_missing(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOATTR)
}
