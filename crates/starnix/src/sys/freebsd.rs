//! FreeBSD: `sysctl(KERN_PROC)` and friends, and `LOCAL_PEERCRED`.

pub mod signal;

use std::ffi::{CStr, c_char};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use super::{kinfo, sysctl};
use crate::process::{Audit, Descriptor, Identity, Process, Region, State, Thread, Usage};
use crate::socket::Peer;
use crate::system::{CpuTicks, Memory, page_size};

/// `NODEV`: no controlling terminal.
const NODEV: u64 = u64::MAX;

/// The first `kinfo_*` record of `bytes`, read into a zeroed `T`.
///
/// The kernel packs some tables, trimming each record's tail, so a record can
/// be shorter than the structure; what it leaves out stays zero.
fn record<T: Copy>(bytes: &[u8]) -> T {
    // SAFETY: every `kinfo_*` structure is plain data, valid when zeroed.
    let mut value: T = unsafe { std::mem::zeroed() };
    let n = bytes.len().min(size_of::<T>());
    // SAFETY: `n` bytes fit in both.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), (&raw mut value).cast::<u8>(), n) };
    value
}

fn proc_mib(flavor: libc::c_int, pid: i32) -> [libc::c_int; 4] {
    [libc::CTL_KERN, libc::KERN_PROC, flavor, pid]
}

/// Every `kinfo_proc` the MIB answers with, one per thread when it asks for
/// threads. An empty answer is a pid with no process.
fn kinfo_procs(flavor: libc::c_int, pid: i32) -> Option<Vec<libc::kinfo_proc>> {
    let bytes = sysctl::bytes(&proc_mib(flavor, pid)).ok()?;
    let size = size_of::<libc::kinfo_proc>();
    let procs: Vec<libc::kinfo_proc> = bytes.chunks_exact(size).map(record).collect();
    (!procs.is_empty()).then_some(procs)
}

fn kinfo_proc(pid: i32) -> Option<libc::kinfo_proc> {
    kinfo_procs(libc::KERN_PROC_PID, pid)?.into_iter().next()
}

fn text(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .map(|&c| c as u8)
        .take_while(|&b| b != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn state(stat: c_char) -> State {
    match stat {
        libc::SRUN | libc::SIDL => State::Running,
        libc::SSTOP => State::Stopped,
        libc::SZOMB => State::Zombie,
        libc::SLOCK => State::Waiting,
        _ => State::Sleeping,
    }
}

fn timeval_ns(time: libc::timeval) -> u64 {
    (time.tv_sec.max(0) as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(time.tv_usec.max(0) as u64 * 1_000)
}

pub fn identity(pid: i32) -> Option<Identity> {
    let proc = kinfo_proc(pid)?;
    let exe = sysctl::bytes(&proc_mib(libc::KERN_PROC_PATHNAME, pid))
        .ok()
        .and_then(|path| {
            let path = CStr::from_bytes_until_nul(&path).ok()?;
            std::fs::metadata(path.to_str().ok()?).ok()
        })
        // A zombie has no executable any more, and is still the same process.
        .map_or((0, 0), |meta| (meta.dev(), meta.ino()));
    Some(Identity {
        start: (proc.ki_start.tv_sec as u64)
            .wrapping_mul(1_000_000)
            .wrapping_add(proc.ki_start.tv_usec as u64),
        exe,
    })
}

pub fn process(pid: i32) -> Option<Process> {
    let proc = kinfo_proc(pid)?;
    let (tty_major, tty_minor) = match proc.ki_tdev as u64 {
        NODEV => (-1, -1),
        dev => (libc::major(dev as _), libc::minor(dev as _)),
    };
    Some(Process {
        ppid: proc.ki_ppid,
        pgid: proc.ki_pgid,
        sid: proc.ki_sid,
        tty_major,
        tty_minor,
        tpgid: proc.ki_tpgid,
        uid: proc.ki_ruid,
        euid: proc.ki_uid,
        suid: proc.ki_svuid,
        gid: proc.ki_rgid,
        // The effective gid is the first of the groups.
        egid: match proc.ki_ngroups {
            0 => proc.ki_rgid,
            _ => proc.ki_groups[0],
        },
        sgid: proc.ki_svgid,
        state: state(proc.ki_stat),
        nice: proc.ki_nice as i32,
        start_sec: proc.ki_start.tv_sec as i64,
        start_usec: proc.ki_start.tv_usec as i32,
    })
}

/// When the machine booted, on the wall clock.
fn boot_time() -> Option<libc::timeval> {
    sysctl::by_name::<libc::timeval>(c"kern.boottime")
}

pub fn usage(pid: i32) -> Option<Usage> {
    let proc = kinfo_proc(pid)?;
    let usage = proc.ki_rusage;
    let resident_size = (proc.ki_rssize.max(0) as u64).saturating_mul(page_size());
    let regions = sysctl::bytes(&proc_mib(libc::KERN_PROC_VMMAP, pid))
        .map_or(0, |bytes| vmentries(&bytes).count() as u32);
    let threads = threads(pid).unwrap_or_default();
    // The monotonic clock counts from boot, and the start time is wall-clock.
    let start_ns = boot_time().map_or(0, |boot| {
        timeval_ns(proc.ki_start).saturating_sub(timeval_ns(boot))
    });
    Some(Usage {
        virtual_size: proc.ki_size as u64,
        resident_size,
        resident_peak: (usage.ru_maxrss.max(0) as u64 * 1024).max(resident_size),
        // Neither the share of resident memory nor bytes moved is counted;
        // `ru_inblock` and `ru_oublock` are operations, not bytes.
        shared_size: 0,
        user_ns: timeval_ns(usage.ru_utime),
        system_ns: timeval_ns(usage.ru_stime),
        minor_faults: usage.ru_minflt.max(0) as u64,
        major_faults: usage.ru_majflt.max(0) as u64,
        voluntary_switches: usage.ru_nvcsw.max(0) as u64,
        involuntary_switches: usage.ru_nivcsw.max(0) as u64,
        threads: (threads.len() as u32).max(proc.ki_numthreads.max(0) as u32),
        running_threads: threads
            .iter()
            .filter(|thread| thread.state == State::Running)
            .count() as u32,
        regions,
        read_bytes: 0,
        write_bytes: 0,
        start_ns,
    })
}

pub fn threads(pid: i32) -> Option<Vec<Thread>> {
    let procs = kinfo_procs(libc::KERN_PROC_PID | libc::KERN_PROC_INC_THREAD, pid)?;
    Some(
        procs
            .iter()
            .map(|proc| Thread {
                tid: proc.ki_tid,
                // A long name spills from `ki_tdname` into `ki_moretdname`.
                name: text(&proc.ki_tdname) + &text(&proc.ki_moretdname),
                user_ns: timeval_ns(proc.ki_rusage.ru_utime),
                system_ns: timeval_ns(proc.ki_rusage.ru_stime),
                state: state(proc.ki_stat),
                nice: proc.ki_nice as i32,
            })
            .collect(),
    )
}

pub fn has_thread(tid: i32) -> bool {
    threads(crate::process::me()).is_some_and(|threads| threads.iter().any(|t| t.tid == tid))
}

pub fn command_line(pid: i32) -> Option<Vec<u8>> {
    sysctl::bytes(&proc_mib(libc::KERN_PROC_ARGS, pid)).ok()
}

pub fn descriptors() -> io::Result<Vec<Descriptor>> {
    let bytes = sysctl::bytes(&proc_mib(libc::KERN_PROC_FILEDESC, crate::process::me()))?;
    kinfo::descriptors(&bytes)
}

/// `KERN_PROC_VMMAP`'s packed records.
fn vmentries(bytes: &[u8]) -> impl Iterator<Item = libc::kinfo_vmentry> + '_ {
    let mut offset = 0;
    std::iter::from_fn(move || {
        let size = i32::from_ne_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?);
        let record = bytes.get(offset..offset + usize::try_from(size).ok().filter(|&s| s > 0)?)?;
        offset += record.len();
        Some(self::record::<libc::kinfo_vmentry>(record))
    })
}

pub fn regions() -> io::Result<Vec<Region>> {
    let bytes = sysctl::bytes(&proc_mib(libc::KERN_PROC_VMMAP, crate::process::me()))?;
    Ok(vmentries(&bytes)
        .map(|entry| Region {
            start: entry.kve_start,
            end: entry.kve_end,
            read: entry.kve_protection & libc::KVME_PROT_READ != 0,
            write: entry.kve_protection & libc::KVME_PROT_WRITE != 0,
            execute: entry.kve_protection & libc::KVME_PROT_EXEC != 0,
            // The map entry does not record `MAP_SHARED`, and nothing beside
            // it stands for it reliably.
            shared: false,
            offset: entry.kve_offset,
        })
        .collect())
}

/// FreeBSD's audit ids are behind `getaudit_addr`, which needs the audit
/// subsystem configured; without it there is nothing to report.
pub fn audit() -> Audit {
    Audit::default()
}

pub fn memory() -> Memory {
    let pages = |name: &CStr| sysctl::by_name::<u32>(name).unwrap_or(0) as u64 * page_size();
    Memory {
        free: pages(c"vm.stats.vm.v_free_count"),
        active: pages(c"vm.stats.vm.v_active_count"),
        inactive: pages(c"vm.stats.vm.v_inactive_count"),
        wired: pages(c"vm.stats.vm.v_wire_count"),
        // Neither is kept as a machine-wide count.
        file_backed: 0,
        anonymous: 0,
    }
}

pub fn cpu_ticks() -> CpuTicks {
    // `CP_USER`, `CP_NICE`, `CP_SYS`, `CP_INTR`, `CP_IDLE`.
    let Some(ticks) = sysctl::by_name::<[libc::c_long; 5]>(c"kern.cp_time") else {
        return CpuTicks::default();
    };
    let tick = |n: usize| ticks[n].max(0) as u64;
    CpuTicks {
        user: tick(0),
        nice: tick(1),
        system: tick(2),
        idle: tick(4),
    }
}

pub fn physical_cores() -> Option<u32> {
    sysctl::by_name::<libc::c_int>(c"kern.smp.cores")
        .filter(|&cores| cores > 0)
        .map(|cores| cores as u32)
}

pub fn cpu_brand() -> Option<String> {
    sysctl::string_by_name(c"hw.model")
}

pub fn peer(fd: RawFd) -> Option<Peer> {
    // SAFETY: `xucred` is plain data, valid when zeroed.
    let mut cred: libc::xucred = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::xucred>() as libc::socklen_t;
    // SAFETY: `cred` is an `xucred` and `len` says so.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc != 0 || cred.cr_version != libc::XUCRED_VERSION {
        return None;
    }
    // SAFETY: the union's only live member; FreeBSD before 13 leaves it zero.
    let pid = unsafe { cred.cr_pid__c_anonymous_union.cr_pid };
    Some(Peer {
        pid: (pid > 0).then_some(pid),
        uid: cred.cr_uid,
    })
}

pub fn in_use(_pid: i32) -> Option<crate::process::InUse> {
    None
}

pub fn fd_path(fd: RawFd) -> Option<PathBuf> {
    // SAFETY: `kinfo_file` is plain data, valid when zeroed.
    let mut info: libc::kinfo_file = unsafe { std::mem::zeroed() };
    info.kf_structsize = libc::KINFO_FILE_SIZE;
    // SAFETY: `info` is a `kinfo_file` with its size filled in, as `F_KINFO`
    // requires.
    if unsafe { libc::fcntl(fd, libc::F_KINFO, &raw mut info) } != 0 {
        return None;
    }
    let path = text(&info.kf_path);
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub fn fd_target(fd: RawFd) -> Option<PathBuf> {
    fd_path(fd)
}

pub fn errno_location() -> *mut libc::c_int {
    // SAFETY: takes no argument and returns this thread's own slot.
    unsafe { libc::__error() }
}

// FIXME (PORTABILITY): FreeBSD keeps these with `extattr_*`, not wired up yet.

pub fn xattr_get_fd(_: RawFd, _: &std::ffi::CStr, _: &mut [u8]) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_get_path(
    _: &std::ffi::CStr,
    _: &std::ffi::CStr,
    _: &mut [u8],
    _: bool,
) -> io::Result<usize> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_set_fd(_: RawFd, _: &std::ffi::CStr, _: &[u8]) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_set_path(_: &std::ffi::CStr, _: &std::ffi::CStr, _: &[u8], _: bool) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_remove_fd(_: RawFd, _: &std::ffi::CStr) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_remove_path(_: &std::ffi::CStr, _: &std::ffi::CStr, _: bool) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn xattr_is_missing(_: &io::Error) -> bool {
    false
}
