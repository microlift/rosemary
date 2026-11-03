use std::ffi::{c_int, c_void};

const PROC_PIDLISTFDS: c_int = 1;
const PROC_PIDTASKALLINFO: c_int = 2;
const PROC_PIDTBSDINFO: c_int = 3;
const PROC_PIDTASKINFO: c_int = 4;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcFdInfo {
    fd: i32,
    kind: u32,
}

const FDTYPE_VNODE: u32 = 1;
const FDTYPE_SOCKET: u32 = 2;
const FDTYPE_KQUEUE: u32 = 5;
const FDTYPE_PIPE: u32 = 6;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcTaskInfo {
    virtual_size: u64,
    resident_size: u64,
    total_user: u64,
    total_system: u64,
    threads_user: u64,
    threads_system: u64,
    policy: i32,
    faults: i32,
    pageins: i32,
    cow_faults: i32,
    messages_sent: i32,
    messages_received: i32,
    syscalls_mach: i32,
    syscalls_unix: i32,
    context_switches: i32,
    threadnum: i32,
    numrunning: i32,
    priority: i32,
}

const _: () = {
    assert!(size_of::<ProcFdInfo>() == 8);
    assert!(size_of::<ProcTaskInfo>() == 96);
};

/// This process's descriptors, as `proc_fdinfo` records.
fn open_descriptors() -> std::io::Result<Vec<ProcFdInfo>> {
    use starnix::process::DescriptorKind;
    Ok(starnix::process::descriptors()?
        .into_iter()
        .map(|entry| ProcFdInfo {
            fd: entry.fd,
            kind: match entry.kind {
                DescriptorKind::File => FDTYPE_VNODE,
                DescriptorKind::Socket => FDTYPE_SOCKET,
                DescriptorKind::Pipe => FDTYPE_PIPE,
                DescriptorKind::Queue => FDTYPE_KQUEUE,
            },
        })
        .collect())
}

pub unsafe extern "C" fn proc_pidinfo(
    pid: c_int,
    flavor: c_int,
    _arg: u64,
    buffer: *mut c_void,
    size: c_int,
) -> c_int {
    let me = crate::process::host_self();
    let pid = if pid == 0 {
        me
    } else {
        crate::process::host_pid(pid)
    };
    if size < 0 {
        crate::errno::set(libc::EINVAL);
        return 0;
    }
    // Another guest's task and BSD records come from the process server; its
    // descriptors live in that process and are not answered for.
    if pid != me {
        return match flavor {
            // SAFETY: the caller's buffer, as for this process below.
            PROC_PIDTASKINFO | PROC_PIDTBSDINFO | PROC_PIDTASKALLINFO => unsafe {
                other_process_info(pid, flavor, buffer, size)
            },
            _ => {
                crate::errno::set(libc::ESRCH);
                0
            }
        };
    }
    match flavor {
        // SAFETY: the caller's buffer.
        PROC_PIDTBSDINFO | PROC_PIDTASKALLINFO => unsafe {
            other_process_info(pid, flavor, buffer, size)
        },
        PROC_PIDLISTFDS => {
            let fds = match open_descriptors() {
                Ok(fds) => fds,
                Err(error) => {
                    crate::errno::set(error.raw_os_error().unwrap_or(libc::EIO));
                    return 0;
                }
            };
            let wanted = (fds.len() * size_of::<ProcFdInfo>()) as c_int;
            if buffer.is_null() {
                return wanted;
            }
            let room = (size as usize / size_of::<ProcFdInfo>()).min(fds.len());
            // SAFETY: `room` entries fit in what the caller said it has, and
            // the two do not overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(fds.as_ptr(), buffer.cast::<ProcFdInfo>(), room)
            };
            (room * size_of::<ProcFdInfo>()) as c_int
        }
        PROC_PIDTASKINFO => {
            let wanted = size_of::<ProcTaskInfo>() as c_int;
            if buffer.is_null() {
                return wanted;
            }
            if size < wanted {
                crate::errno::set(libc::ENOMEM);
                return 0;
            }
            let Some(info) = crate::process::usage(pid).map(|usage| task_record(&usage)) else {
                crate::errno::set(libc::ESRCH);
                return 0;
            };
            // SAFETY: the caller's buffer has room for the structure.
            unsafe { buffer.cast::<ProcTaskInfo>().write_unaligned(info) };
            wanted
        }
        _ => {
            crate::errno::set(libc::EINVAL);
            0
        }
    }
}

pub unsafe extern "C" fn proc_name(pid: c_int, buffer: *mut c_void, size: u32) -> c_int {
    // A guest's name is its guest executable's, whichever process asks.
    if let Some(process) = crate::process::describe(pid) {
        // SAFETY: the caller's buffer.
        return unsafe { copy_name(process.command(), buffer, size) };
    }
    let Some(name) = starnix::process::command_line(pid).and_then(|line| process_name(&line))
    else {
        crate::errno::set(libc::ESRCH);
        return 0;
    };
    // SAFETY: the caller's buffer.
    unsafe { copy_name(&name, buffer, size) }
}

/// `int proc_pidpath(int, void *, uint32_t)`, using the guest's executable.
///
/// # Safety
///
/// `buffer` is writable for `size` bytes.
pub unsafe extern "C" fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int {
    let Some(process) = crate::process::describe(pid) else {
        crate::errno::set(libc::ESRCH);
        return 0;
    };
    let path = process.path.as_bytes();
    if buffer.is_null() || size as usize <= path.len() {
        crate::errno::set(libc::EINVAL);
        return 0;
    }
    // SAFETY: the caller's buffer holds the path and its terminator.
    unsafe {
        std::ptr::copy_nonoverlapping(path.as_ptr(), buffer.cast::<u8>(), path.len());
        *buffer.cast::<u8>().add(path.len()) = 0;
    }
    path.len() as c_int
}

/// # Safety
///
/// `buffer` is null or writable for `size` bytes.
unsafe fn copy_name(name: &str, buffer: *mut c_void, size: u32) -> c_int {
    // FIXME (PORTABILITY): may be different on other systems
    const MAXCOMLEN: usize = 16;
    // `p_name` is 2 * MAXCOMLEN bytes.
    let name = &name.as_bytes()[..name.len().min(MAXCOMLEN * 2)];
    if buffer.is_null() || (size as usize) <= name.len() {
        crate::errno::set(libc::EINVAL);
        return 0;
    }
    // SAFETY: the caller's buffer holds `size` bytes, more than the name.
    unsafe {
        std::ptr::copy_nonoverlapping(name.as_ptr(), buffer.cast::<u8>(), name.len());
        *buffer.cast::<u8>().add(name.len()) = 0;
    }
    name.len() as c_int
}

fn process_name(cmdline: &[u8]) -> Option<String> {
    let arguments: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
    let base = |argument: &[u8]| {
        let text = String::from_utf8_lossy(argument).into_owned();
        text.rsplit('/').next().map(str::to_owned)
    };
    let program = base(arguments.first()?)?;
    if program == "rosemary"
        && arguments.get(1).is_some_and(|a| *a == b"run")
        && let Some(guest) = arguments[2..]
            .iter()
            .find(|a| !a.is_empty() && !a.starts_with(b"-"))
    {
        return base(guest);
    }
    (!program.is_empty()).then_some(program)
}

/// `proc_taskinfo` from what the host measured.
fn task_record(usage: &server::protocol::Usage) -> ProcTaskInfo {
    let clamp = |n: u64| n.min(i32::MAX as u64) as i32;
    ProcTaskInfo {
        virtual_size: usage.virtual_size,
        resident_size: usage.resident_size,
        total_user: usage.user_ns,
        total_system: usage.system_ns,
        threads_user: usage.user_ns,
        threads_system: usage.system_ns,
        // `POLICY_TIMESHARE`.
        policy: 1,
        faults: clamp(usage.minor_faults + usage.major_faults),
        pageins: clamp(usage.major_faults),
        context_switches: clamp(usage.voluntary_switches + usage.involuntary_switches),
        threadnum: usage.threads as i32,
        numrunning: usage.running_threads as i32,
        // `BASEPRI_DEFAULT`.
        priority: 31,
        ..ProcTaskInfo::default()
    }
}

/// `struct proc_bsdinfo`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [u8; 16],
    pbi_name: [u8; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

const _: () = assert!(size_of::<ProcBsdInfo>() == 136);

/// `struct proc_taskallinfo`.
#[repr(C)]
#[derive(Clone, Copy)]
struct ProcTaskAllInfo {
    pbsd: ProcBsdInfo,
    ptinfo: ProcTaskInfo,
}

const _: () = assert!(size_of::<ProcTaskAllInfo>() == 232);

/// `PROC_FLAG_*`.
const PROC_FLAG_LP64: u32 = 0x10;
const PROC_FLAG_SLEADER: u32 = 0x20;
const PROC_FLAG_CTTY: u32 = 0x40;

fn bsd_record(process: &server::protocol::Process) -> ProcBsdInfo {
    use server::protocol::State;
    let mut info = ProcBsdInfo {
        pbi_flags: PROC_FLAG_LP64,
        // `p_stat`: `SRUN`, `SSLEEP`, `SSTOP`, `SZOMB`.
        pbi_status: match process.state {
            State::Running => 2,
            State::Sleeping | State::Waiting => 3,
            State::Stopped => 4,
            State::Zombie => 5,
        },
        pbi_pid: process.pid as u32,
        pbi_ppid: process.ppid as u32,
        pbi_uid: process.euid,
        pbi_gid: process.egid,
        pbi_ruid: process.uid,
        pbi_rgid: process.gid,
        pbi_svuid: process.suid,
        pbi_svgid: process.sgid,
        pbi_pgid: process.pgid as u32,
        e_tdev: match process.tty_major {
            -1 => u32::MAX,
            major => libc::makedev(major as _, process.tty_minor as _) as u32,
        },
        e_tpgid: process.tpgid as u32,
        pbi_nice: process.nice,
        pbi_start_tvsec: process.start_sec as u64,
        pbi_start_tvusec: process.start_usec as u64,
        ..ProcBsdInfo::default()
    };
    if process.pid == process.sid {
        info.pbi_flags |= PROC_FLAG_SLEADER;
    }
    if process.tty_major >= 0 {
        info.pbi_flags |= PROC_FLAG_CTTY;
    }
    let command = process.command().as_bytes();
    let n = command.len().min(info.pbi_comm.len() - 1);
    info.pbi_comm[..n].copy_from_slice(&command[..n]);
    let n = command.len().min(info.pbi_name.len() - 1);
    info.pbi_name[..n].copy_from_slice(&command[..n]);
    info
}

/// Copy `record` out the way `proc_pidinfo` does: a short buffer is an error,
/// not a partial structure.
///
/// # Safety
///
/// `buffer` is null or writable for `size` bytes.
unsafe fn copy_record<T: Copy>(record: T, buffer: *mut c_void, size: c_int) -> c_int {
    let wanted = size_of::<T>() as c_int;
    if buffer.is_null() {
        return wanted;
    }
    if size < wanted {
        crate::errno::set(libc::ENOMEM);
        return 0;
    }
    // SAFETY: the caller's buffer has room for the structure.
    unsafe { buffer.cast::<T>().write_unaligned(record) };
    wanted
}

/// `PROC_PIDTASKINFO`, `PROC_PIDTBSDINFO` and `PROC_PIDTASKALLINFO` for any
/// guest, this one included.
///
/// # Safety
///
/// As [`copy_record`].
unsafe fn other_process_info(pid: c_int, flavor: c_int, buffer: *mut c_void, size: c_int) -> c_int {
    match flavor {
        PROC_PIDTBSDINFO => match crate::process::describe(pid) {
            // SAFETY: the caller's buffer.
            Some(process) => unsafe { copy_record(bsd_record(&process), buffer, size) },
            None => {
                crate::errno::set(libc::ESRCH);
                0
            }
        },
        PROC_PIDTASKINFO => match crate::process::usage(pid) {
            // SAFETY: the caller's buffer.
            Some(usage) => unsafe { copy_record(task_record(&usage), buffer, size) },
            None => {
                crate::errno::set(libc::ESRCH);
                0
            }
        },
        PROC_PIDTASKALLINFO => {
            match (crate::process::describe(pid), crate::process::usage(pid)) {
                (Some(process), Some(usage)) => {
                    let record = ProcTaskAllInfo {
                        pbsd: bsd_record(&process),
                        ptinfo: task_record(&usage),
                    };
                    // SAFETY: the caller's buffer.
                    unsafe { copy_record(record, buffer, size) }
                }
                _ => {
                    crate::errno::set(libc::ESRCH);
                    0
                }
            }
        }
        _ => {
            crate::errno::set(libc::ESRCH);
            0
        }
    }
}

/// `int proc_pid_rusage(int pid, int flavor, rusage_info_t *buffer)`.
///
/// Every version up to `RUSAGE_INFO_V5`, each a prefix of the next: the
/// fields are written as a run of words after the UUID, as far as the asked
/// version reaches. What the host does not count - wakeups, energy, QoS
/// buckets, instructions - reads zero.
///
/// # Safety
///
/// `buffer` is writable for the asked version's structure.
pub unsafe extern "C" fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int {
    /// Words after the 16-byte UUID, per version.
    const WORDS: [usize; 6] = [10, 16, 18, 27, 35, 36];
    let Some(&words) = usize::try_from(flavor).ok().and_then(|v| WORDS.get(v)) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    if buffer.is_null() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let Some(usage) = crate::process::usage(pid) else {
        crate::errno::set(libc::ESRCH);
        return -1;
    };
    let mut fields = [0u64; 36];
    fields[0] = usage.user_ns;
    fields[1] = usage.system_ns;
    fields[4] = usage.major_faults;
    fields[6] = usage.resident_size;
    fields[7] = usage.resident_size.saturating_sub(usage.shared_size);
    fields[8] = usage.start_ns;
    fields[16] = usage.read_bytes;
    fields[17] = usage.write_bytes;
    fields[27] = usage.write_bytes;
    // SAFETY: the caller's buffer holds the version it asked for: sixteen
    // bytes of UUID and then `words` words.
    unsafe {
        std::ptr::write_bytes(buffer.cast::<u8>(), 0, 16);
        for (i, field) in fields[..words].iter().enumerate() {
            buffer
                .cast::<u8>()
                .add(16 + i * 8)
                .cast::<u64>()
                .write_unaligned(*field);
        }
    }
    0
}

/// `proc_listpids` types.
const PROC_ALL_PIDS: u32 = 1;
const PROC_PGRP_ONLY: u32 = 2;
const PROC_TTY_ONLY: u32 = 3;
const PROC_UID_ONLY: u32 = 4;
const PROC_RUID_ONLY: u32 = 5;
const PROC_PPID_ONLY: u32 = 6;

/// The pids `kind` selects, or `None` for a kind nothing here knows.
fn select_pids(kind: u32, info: u32) -> Option<Vec<i32>> {
    let keep = |process: &server::protocol::Process| match kind {
        PROC_ALL_PIDS => true,
        PROC_PGRP_ONLY => process.pgid == info as i32,
        PROC_TTY_ONLY => {
            process.tty_major >= 0
                && libc::makedev(process.tty_major as _, process.tty_minor as _) as u32 == info
        }
        PROC_UID_ONLY => process.euid == info,
        PROC_RUID_ONLY => process.uid == info,
        PROC_PPID_ONLY => process.ppid == info as i32,
        _ => false,
    };
    if !(PROC_ALL_PIDS..=PROC_PPID_ONLY).contains(&kind) {
        return None;
    }
    Some(
        crate::process::list()
            .iter()
            .filter(|process| keep(process))
            .map(|process| process.pid)
            .collect(),
    )
}

/// Copy as many of `pids` as fit; the number of bytes written, or with a null
/// buffer the number a caller should allow for.
///
/// # Safety
///
/// `buffer` is null or writable for `size` bytes.
unsafe fn copy_pids(pids: &[i32], buffer: *mut c_void, size: c_int) -> c_int {
    let bytes = std::mem::size_of_val(pids);
    if buffer.is_null() {
        // Room for a few more: the list can grow between the two calls, and a
        // caller that sizes exactly would lose the newest process.
        return (bytes + 16 * size_of::<i32>()) as c_int;
    }
    let room = (size.max(0) as usize / size_of::<i32>()).min(pids.len());
    // SAFETY: `room` pids fit in the caller's buffer.
    unsafe { std::ptr::copy_nonoverlapping(pids.as_ptr(), buffer.cast::<i32>(), room) };
    (room * size_of::<i32>()) as c_int
}

/// `int proc_listpids(uint32_t type, uint32_t typeinfo, void *buffer, int
/// buffersize)`: bytes written.
///
/// # Safety
///
/// As [`copy_pids`].
pub unsafe extern "C" fn proc_listpids(
    kind: u32,
    info: u32,
    buffer: *mut c_void,
    size: c_int,
) -> c_int {
    match select_pids(kind, info) {
        // SAFETY: the caller's buffer.
        Some(pids) => unsafe { copy_pids(&pids, buffer, size) },
        None => {
            crate::errno::set(libc::EINVAL);
            -1
        }
    }
}

/// `int proc_listallpids(void *buffer, int buffersize)`: a count, not bytes.
///
/// # Safety
///
/// As [`copy_pids`].
pub unsafe extern "C" fn proc_listallpids(buffer: *mut c_void, size: c_int) -> c_int {
    let pids = select_pids(PROC_ALL_PIDS, 0).unwrap_or_default();
    // SAFETY: the caller's buffer.
    let bytes = unsafe { copy_pids(&pids, buffer, size) };
    bytes / size_of::<i32>() as c_int
}

/// `int memorystatus_control(uint32_t command, int32_t pid, uint32_t flags,
/// void *buffer, size_t buffersize)`.
///
/// Jetsam - the kernel ranking processes for killing under memory pressure -
/// does not exist here, and a priority list from it would be invention.
pub extern "C" fn memorystatus_control(
    _command: u32,
    _pid: i32,
    _flags: u32,
    _buffer: *mut c_void,
    _size: usize,
) -> c_int {
    crate::errno::set(libc::ENOTSUP);
    -1
}

/// `int kpersona_pidinfo(pid_t pid, struct kpersona_info *info)`.
///
/// Personas are a macOS kernel concept with no host equivalent, so no process
/// has one to report and a made-up id would be invention.
pub extern "C" fn kpersona_pidinfo(_pid: i32, _info: *mut c_void) -> c_int {
    crate::errno::set(libc::ENOTSUP);
    -1
}

/// `WAKEMON_DISABLE`: no wakeups monitor is armed.
const WAKEMON_DISABLE: i32 = 0x02;

/// `int proc_get_wakemon_params(pid_t pid, int *rate_hz, int *flags)`
///
/// Nothing here counts a process's wakeups, so every process reads as one
/// whose monitor is off - what xnu reports for a task with no limit. Like the
/// rest of libproc's `rlimit_control` wrappers, it returns the error number
/// rather than -1.
///
/// # Safety
///
/// `rate_hz` and `flags` are writable.
pub unsafe extern "C" fn proc_get_wakemon_params(
    pid: c_int,
    rate_hz: *mut i32,
    flags: *mut i32,
) -> c_int {
    let error = if rate_hz.is_null() || flags.is_null() {
        libc::EFAULT
    } else if !crate::process::visible(pid) {
        libc::ESRCH
    } else {
        // SAFETY: the caller's out-parameters.
        unsafe {
            rate_hz.write(-1);
            flags.write(WAKEMON_DISABLE);
        }
        return 0;
    };
    crate::errno::set(error);
    crate::errno::from_host(error)
}

exports::exports! {
    functions {
        "kpersona_pidinfo" => kpersona_pidinfo,
        "proc_get_wakemon_params" => proc_get_wakemon_params,
        "proc_pidinfo" => proc_pidinfo,
        "proc_name" => proc_name,
        "proc_pidpath" => proc_pidpath,
        "proc_pid_rusage" => proc_pid_rusage,
        "proc_listpids" => proc_listpids,
        "proc_listallpids" => proc_listallpids,
        "memorystatus_control" => memorystatus_control,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_guest_is_named_for_its_executable_not_the_runtime() {
        assert_eq!(
            process_name(b"/x/rosemary\0run\0--emulated\0/A.app/Contents/MacOS/Studio\0-p\0"),
            Some("Studio".to_owned())
        );
        assert_eq!(
            process_name(b"/usr/bin/bash\0-c\0"),
            Some("bash".to_owned())
        );
    }

    use super::*;

    #[test]
    fn the_descriptor_list_counts_what_this_process_has_open() {
        let mut fds = [ProcFdInfo::default(); 256];
        let size = std::mem::size_of_val(&fds) as c_int;
        // SAFETY: a buffer this test sized.
        let bytes = unsafe { proc_pidinfo(0, PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), size) };
        assert!(bytes > 0 && bytes % 8 == 0);
        let open = bytes as usize / size_of::<ProcFdInfo>();
        assert!(open >= 3, "at least the three standard descriptors");
        assert!(fds[..open].iter().all(|entry| entry.kind != 0));

        let wanted = unsafe { proc_pidinfo(0, PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        assert!(wanted > 0 && wanted % size_of::<ProcFdInfo>() as c_int == 0);
    }

    #[test]
    fn a_short_buffer_is_filled_to_its_own_length() {
        let mut fds = [ProcFdInfo::default(); 2];
        let bytes = unsafe { proc_pidinfo(0, PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), 16) };
        assert_eq!(bytes, 16);
    }

    #[test]
    fn the_task_flavour_reports_this_processs_memory() {
        let mut info = ProcTaskInfo::default();
        let bytes = unsafe {
            proc_pidinfo(
                0,
                PROC_PIDTASKINFO,
                0,
                (&raw mut info).cast(),
                size_of::<ProcTaskInfo>() as c_int,
            )
        };
        assert_eq!(bytes, 96);
        assert!(info.resident_size > 0);
        assert!(info.virtual_size >= info.resident_size);
        assert!(info.threadnum >= 1);
    }

    #[test]
    fn the_task_all_flavour_is_the_bsd_and_task_records_together() {
        // SAFETY: plain data, valid when zeroed.
        let mut info: ProcTaskAllInfo = unsafe { std::mem::zeroed() };
        let bytes = unsafe {
            proc_pidinfo(
                0,
                PROC_PIDTASKALLINFO,
                0,
                (&raw mut info).cast(),
                size_of::<ProcTaskAllInfo>() as c_int,
            )
        };
        assert_eq!(bytes, size_of::<ProcTaskAllInfo>() as c_int);
        let me = crate::process::guest_pid(crate::process::host_self());
        assert_eq!(info.pbsd.pbi_pid, me as u32);
        assert!(info.ptinfo.resident_size > 0);
    }

    #[test]
    fn another_process_and_an_unknown_flavour_are_refused() {
        let mut info = ProcTaskInfo::default();
        let mut ask = |pid, flavor| unsafe {
            proc_pidinfo(
                pid,
                flavor,
                0,
                (&raw mut info).cast(),
                size_of::<ProcTaskInfo>() as c_int,
            )
        };
        assert_eq!(ask(0, 999), 0);
        assert_eq!(ask(1, PROC_PIDTASKINFO), 0, "not this process");
    }

    #[test]
    fn the_wakeups_monitor_reads_as_disabled() {
        let (mut rate, mut flags) = (0, 0);
        let me = crate::process::guest_pid(crate::process::host_self());
        let rc = unsafe { proc_get_wakemon_params(me, &mut rate, &mut flags) };
        assert_eq!((rc, rate, flags), (0, -1, WAKEMON_DISABLE));
        let rc = unsafe { proc_get_wakemon_params(me, std::ptr::null_mut(), &mut flags) };
        assert_eq!(rc, crate::errno::from_host(libc::EFAULT));
    }
}
