//! FreeBSD: `sysctl(KERN_PROC)` and friends, and `LOCAL_PEERCRED`.

pub mod signal;

/// No call reaches this process's memory through the kernel more directly
/// than a pipe does.
pub mod memory {
    pub use crate::memory::piped::{read, write};

    /// Removed in FreeBSD 11, which always overcommits anonymous memory.
    pub const NORESERVE: libc::c_int = 0;

    pub fn object() -> std::io::Result<std::os::fd::OwnedFd> {
        use std::os::fd::FromRawFd;
        // SAFETY: a NUL-terminated name, which only labels the object.
        match unsafe { libc::memfd_create(c"rosemary-memory-object".as_ptr(), libc::MFD_CLOEXEC) } {
            -1 => Err(std::io::Error::last_os_error()),
            // SAFETY: a descriptor the call just opened.
            fd => Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }),
        }
    }

    pub fn exclude_from_fork(address: usize, len: usize) -> std::io::Result<()> {
        // SAFETY: changes only what a child inherits of the range.
        match unsafe { libc::minherit(address as *mut core::ffi::c_void, len, libc::INHERIT_NONE) }
        {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }
}

use std::ffi::{CStr, c_char, c_void};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use super::{kinfo, sysctl};
use crate::process::{
    Audit, Descriptor, Footprint, Identity, Process, Region, State, Thread, Usage,
};
use crate::socket::Peer;
use crate::system::{CpuTicks, Memory, Swap, page_size};

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
    let (tty_major, tty_minor) = match proc.ki_tdev {
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
        start_sec: proc.ki_start.tv_sec,
        start_usec: proc.ki_start.tv_usec as i32,
    })
}

/// When the machine booted, on the wall clock.
fn boot_time() -> Option<libc::timeval> {
    sysctl::by_name::<libc::timeval>(c"kern.boottime")
}

pub fn footprint(pid: i32) -> Option<Footprint> {
    let proc = kinfo_proc(pid)?;
    let resident_size = (proc.ki_rssize.max(0) as u64).saturating_mul(page_size());
    Some(Footprint {
        virtual_size: proc.ki_size as u64,
        resident_size,
        resident_peak: (proc.ki_rusage.ru_maxrss.max(0) as u64 * 1024).max(resident_size),
        shared_size: 0,
    })
}

pub fn usage(pid: i32) -> Option<Usage> {
    let proc = kinfo_proc(pid)?;
    let usage = proc.ki_rusage;
    let resident_size = (proc.ki_rssize.max(0) as u64).saturating_mul(page_size());
    let regions = region_count(pid).unwrap_or(0);
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

pub fn thread(pid: i32, tid: i32) -> Option<Thread> {
    threads(pid)?.into_iter().find(|thread| thread.tid == tid)
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

/// `ROSEMARY_REGION_COUNT_REUSE=1`: reuse a region count for up to
/// [`REGION_COUNT_REUSE`]. `KERN_PROC_VMMAP` also counts every entry's resident
/// pages under the map lock, tens of milliseconds in a game's address space,
/// and engines poll the count several times a second. Off by default, as the
/// count can then be out of date.
const REGION_COUNT_REUSE_ENV: &str = "ROSEMARY_REGION_COUNT_REUSE";

/// How long a region count is reused when [`REGION_COUNT_REUSE_ENV`] asks.
const REGION_COUNT_REUSE: Duration = Duration::from_secs(1);

fn reuse_region_count() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os(REGION_COUNT_REUSE_ENV).is_some_and(|v| v != "0"))
}

pub fn region_count(pid: i32) -> Option<u32> {
    if !reuse_region_count() {
        return regions(pid).ok().map(|regions| regions.len() as u32);
    }
    // FIXME (ACCURACY): the count can be up to `REGION_COUNT_REUSE` old.
    // Held across the walk so concurrent callers share one instead of each
    // contending for the map lock.
    static LAST: Mutex<Option<(i32, Instant, u32)>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((counted, when, count)) = *last
        && counted == pid
        && when.elapsed() < REGION_COUNT_REUSE
    {
        return Some(count);
    }
    let bytes = sysctl::bytes(&proc_mib(libc::KERN_PROC_VMMAP, pid)).ok()?;
    let count = vmentries(&bytes).count() as u32;
    *last = Some((pid, Instant::now(), count));
    Some(count)
}

// `<sys/user.h>` flags the libc crate does not declare.
const KVME_FLAG_SYSVSHM: i32 = 0x0000_0080;
const KVME_FLAG_POSIXSHM: i32 = 0x0000_0100;

pub fn regions(pid: i32) -> io::Result<Vec<Region>> {
    let bytes = sysctl::bytes(&proc_mib(libc::KERN_PROC_VMMAP, pid))?;
    Ok(vmentries(&bytes)
        .map(|entry| Region {
            start: entry.kve_start,
            end: entry.kve_end,
            read: entry.kve_protection & libc::KVME_PROT_READ != 0,
            write: entry.kve_protection & libc::KVME_PROT_WRITE != 0,
            execute: entry.kve_protection & libc::KVME_PROT_EXEC != 0,
            shared: shared(&entry),
            offset: entry.kve_offset,
        })
        .collect())
}

/// Whether an entry maps its object `MAP_SHARED`. The map does not record the
/// flag, but a private mapping of a file or a shared memory object is
/// copy-on-write, and a shared one is not. A device can only be mapped shared.
// FIXME (ACCURACY): anonymous memory is never copy-on-write and has no object
// of its own to name, so a shared anonymous mapping reads as private.
fn shared(entry: &libc::kinfo_vmentry) -> bool {
    if matches!(
        entry.kve_type,
        libc::KVME_TYPE_DEVICE | libc::KVME_TYPE_MGTDEVICE
    ) {
        return true;
    }
    let object = entry.kve_type == libc::KVME_TYPE_VNODE
        || entry.kve_flags & (KVME_FLAG_POSIXSHM | KVME_FLAG_SYSVSHM) != 0;
    object && entry.kve_flags & libc::KVME_FLAG_COW == 0
}

/// FreeBSD's audit ids are behind `getaudit_addr`, which needs the audit
/// subsystem configured; without it there is nothing to report.
pub fn audit() -> Audit {
    Audit::default()
}

// `p_comm` is copied from the executable at `exec` and nothing changes it
// after; `setproctitle` rewrites the arguments, not the name.
pub fn set_name(_name: &CStr) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
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

/// FreeBSD keeps no count of sockets apart from its topology description.
pub fn packages() -> Option<u32> {
    None
}

pub fn cpu_frequency() -> Option<u64> {
    const HZ_PER_MHZ: u64 = 1_000_000;
    sysctl::by_name::<libc::c_int>(c"hw.clockrate")
        .filter(|&mhz| mhz > 0)
        .map(|mhz| mhz as u64 * HZ_PER_MHZ)
}

pub fn booted() -> Option<std::time::SystemTime> {
    let boot = boot_time()?;
    let since = std::time::Duration::new(boot.tv_sec as u64, boot.tv_usec as u32 * 1000);
    Some(std::time::UNIX_EPOCH + since)
}

/// Swap is reported per device, through `vm.swap_info`, which is not read here.
pub fn swap() -> Option<Swap> {
    None
}

pub fn max_processes() -> Option<u32> {
    sysctl::by_name::<libc::c_int>(c"kern.maxproc").map(|n| n as u32)
}

pub fn host_uuid() -> Option<[u8; 16]> {
    crate::system::parse_uuid(&sysctl::string_by_name(c"kern.hostuuid")?)
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

/// The only namespace [`xattr_name`] lets through.
const NAMESPACE: libc::c_int = libc::EXTATTR_NAMESPACE_USER;

/// The file an attribute is on: an open descriptor, or a path and whether a
/// final symlink is followed.
#[derive(Clone, Copy)]
enum XattrFile<'a> {
    Fd(RawFd),
    Path(&'a CStr, bool),
}

/// Every attribute this crate is asked for is spelled as Linux does, and only
/// the `user.` namespace is one a caller may touch without privilege. FreeBSD
/// carries the namespace beside the name rather than inside it.
fn xattr_name(name: &CStr) -> io::Result<&CStr> {
    match name.to_bytes_with_nul().strip_prefix(b"user.") {
        Some(rest) if rest.len() > 1 => {
            Ok(CStr::from_bytes_with_nul(rest).map_err(|_| io::ErrorKind::InvalidInput)?)
        }
        _ => Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)),
    }
}

/// `extattr_get_*` on `file`: up to `n` bytes into `p`, or the length for a
/// null `p`.
///
/// # Safety
///
/// `p` is null or `n` writable bytes.
unsafe fn xattr_read(file: XattrFile<'_>, name: &CStr, p: *mut c_void, n: usize) -> isize {
    // SAFETY: NUL-terminated strings, and the caller's buffer.
    unsafe {
        match file {
            XattrFile::Fd(fd) => libc::extattr_get_fd(fd, NAMESPACE, name.as_ptr(), p, n),
            XattrFile::Path(path, true) => {
                libc::extattr_get_file(path.as_ptr(), NAMESPACE, name.as_ptr(), p, n)
            }
            XattrFile::Path(path, false) => {
                libc::extattr_get_link(path.as_ptr(), NAMESPACE, name.as_ptr(), p, n)
            }
        }
    }
}

fn xattr_result(rc: isize) -> io::Result<usize> {
    usize::try_from(rc).map_err(|_| io::Error::last_os_error())
}

fn xattr_get(file: XattrFile<'_>, name: &CStr, buf: &mut [u8]) -> io::Result<usize> {
    let name = xattr_name(name)?;
    // FreeBSD truncates into a short buffer where Linux refuses with `ERANGE`,
    // and a truncated value reported as a whole one is the answer a caller
    // cannot tell is wrong: ask for the length first.
    // SAFETY: a null buffer asks for the length.
    let length = xattr_result(unsafe { xattr_read(file, name, std::ptr::null_mut(), 0) })?;
    if buf.is_empty() {
        return Ok(length);
    }
    if length > buf.len() {
        return Err(io::Error::from_raw_os_error(libc::ERANGE));
    }
    // SAFETY: a buffer of its own length.
    xattr_result(unsafe { xattr_read(file, name, buf.as_mut_ptr().cast(), buf.len()) })
}

fn xattr_set(file: XattrFile<'_>, name: &CStr, value: &[u8]) -> io::Result<()> {
    let name = xattr_name(name)?;
    let (p, n) = (value.as_ptr().cast(), value.len());
    // SAFETY: NUL-terminated strings and the value's own bytes.
    let rc = unsafe {
        match file {
            XattrFile::Fd(fd) => libc::extattr_set_fd(fd, NAMESPACE, name.as_ptr(), p, n),
            XattrFile::Path(path, true) => {
                libc::extattr_set_file(path.as_ptr(), NAMESPACE, name.as_ptr(), p, n)
            }
            XattrFile::Path(path, false) => {
                libc::extattr_set_link(path.as_ptr(), NAMESPACE, name.as_ptr(), p, n)
            }
        }
    };
    xattr_result(rc).map(drop)
}

fn xattr_remove(file: XattrFile<'_>, name: &CStr) -> io::Result<()> {
    let name = xattr_name(name)?;
    // SAFETY: NUL-terminated strings.
    let rc = unsafe {
        match file {
            XattrFile::Fd(fd) => libc::extattr_delete_fd(fd, NAMESPACE, name.as_ptr()),
            XattrFile::Path(path, true) => {
                libc::extattr_delete_file(path.as_ptr(), NAMESPACE, name.as_ptr())
            }
            XattrFile::Path(path, false) => {
                libc::extattr_delete_link(path.as_ptr(), NAMESPACE, name.as_ptr())
            }
        }
    };
    match rc {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

pub fn xattr_get_fd(fd: RawFd, name: &CStr, buf: &mut [u8]) -> io::Result<usize> {
    xattr_get(XattrFile::Fd(fd), name, buf)
}

pub fn xattr_get_path(path: &CStr, name: &CStr, buf: &mut [u8], follow: bool) -> io::Result<usize> {
    xattr_get(XattrFile::Path(path, follow), name, buf)
}

pub fn xattr_set_fd(fd: RawFd, name: &CStr, value: &[u8]) -> io::Result<()> {
    xattr_set(XattrFile::Fd(fd), name, value)
}

pub fn xattr_set_path(path: &CStr, name: &CStr, value: &[u8], follow: bool) -> io::Result<()> {
    xattr_set(XattrFile::Path(path, follow), name, value)
}

pub fn xattr_remove_fd(fd: RawFd, name: &CStr) -> io::Result<()> {
    xattr_remove(XattrFile::Fd(fd), name)
}

pub fn xattr_remove_path(path: &CStr, name: &CStr, follow: bool) -> io::Result<()> {
    xattr_remove(XattrFile::Path(path, follow), name)
}

pub fn xattr_is_missing(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENOATTR)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without `ROSEMARY_REGION_COUNT_REUSE`, a count follows the map at once.
    #[test]
    fn a_new_mapping_is_counted_at_once() {
        if reuse_region_count() {
            return;
        }
        let pid = crate::process::me();
        let page = page_size() as usize;
        // A mapping of an object of its own, which the kernel never merges
        // into a neighbouring entry.
        // SAFETY: a name and no flags.
        let fd = unsafe { libc::memfd_create(c"region-count".as_ptr(), 0) };
        assert!(fd >= 0);
        // SAFETY: this test's own descriptor.
        assert_eq!(unsafe { libc::ftruncate(fd, page as libc::off_t) }, 0);
        let before = region_count(pid).expect("a count");
        // SAFETY: a fresh mapping of that object, unmapped below.
        let at = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                page,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert_ne!(at, libc::MAP_FAILED);
        let after = region_count(pid).expect("a count");
        // SAFETY: the mapping and descriptor made above.
        unsafe {
            libc::munmap(at, page);
            libc::close(fd);
        }
        assert!(after > before, "{before} then {after}");
    }
}

pub fn enter_total_store_order() -> bool {
    false
}
