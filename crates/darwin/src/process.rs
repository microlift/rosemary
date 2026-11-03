//! Processes as a guest sees them: itself, and every other guest the process
//! server knows.
//!
//! The server holds the table; this module turns its answers into Darwin's
//! structures - `kinfo_proc`, `KERN_PROCARGS2` - and answers about this
//! process locally, so a process with no server still sees itself.
use std::sync::OnceLock;

use server::protocol::{Checkin, Process, State, Thread, Usage};

/// What this process checked in as.
struct Me {
    path: String,
    argv: Vec<Vec<u8>>,
}

static ME: OnceLock<Me> = OnceLock::new();

/// This process's host pid.
pub(crate) fn host_self() -> i32 {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    starnix::process::me()
}

/// This process's pid as guests know it.
fn pid() -> i32 {
    guest_pid(host_self())
}

/// A host pid as guests know it: the prefix's launchd is pid 1, as on a Mac,
/// where launchd's own start-up refuses to run as anything else.
pub fn guest_pid(host: i32) -> i32 {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    match server::client::launchd() == Some(host) {
        true => server::registry::LAUNCHD,
        false => host,
    }
}

/// A pid a guest named, as the host knows it. The host's own pid 1 is never a
/// guest's to signal or ask about, so pid 1 can only mean launchd.
pub fn host_pid(guest: i32) -> i32 {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    match (guest, server::client::launchd()) {
        (server::registry::LAUNCHD, Some(host)) => host,
        _ => guest,
    }
}

/// `pid_t getpid(void)`.
pub extern "C" fn getpid() -> libc::pid_t {
    pid()
}

/// `pid_t getppid(void)`.
pub extern "C" fn getppid() -> libc::pid_t {
    // SAFETY: `getppid` has no preconditions.
    guest_pid(unsafe { libc::getppid() })
}

/// `int reboot(int howto)`.
///
/// launchd calls it last, once every job is stopped; for a prefix that is
/// the machine going down, and its end is launchd's exit, after which the
/// server ends whatever is left. Nobody else may: a guest that wants the
/// prefix down asks launchd, which is how it is done on a Mac too.
pub extern "C" fn reboot(_howto: libc::c_int) -> libc::c_int {
    if pid() != server::registry::LAUNCHD {
        crate::errno::set(libc::EPERM);
        return -1;
    }
    // FIXME (ACCURACY): RB_AUTOBOOT would boot the prefix again; every
    // request halts it.
    // SAFETY: `_exit` has no preconditions. Not `exit`: the kernel does not
    // run a rebooting launchd's `atexit` handlers either.
    unsafe { libc::_exit(0) }
}

/// Record this process as the guest at `path`, tell the server if there is
/// one, and settle who the process is - which may be somebody else than the
/// user who ran it, if the image is set-id.
///
/// `path` is the guest's spelling. Called once, by the runtime, before the
/// program's first instruction.
pub fn check_in(path: &str, argv: Vec<Vec<u8>>) {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    let arch = crate::sysctl::guest_arch();
    let setid = crate::credentials::setid_of(&prefix::ns::host_path(path));
    let checkin = Checkin {
        path: path.to_owned(),
        argv: argv.clone(),
        cputype: arch.cpu_type(),
        cpusubtype: arch.cpu_subtype(),
        translated: crate::sysctl::is_translated(),
        credentials: crate::credentials::offered(),
        fresh: true,
        setid,
        // Before the check-in, so a signal the server sends once it knows
        // this process finds the handler in place.
        serviceable: crate::mach::service::install(),
        ..Checkin::default()
    };
    let _ = ME.set(Me {
        path: path.to_owned(),
        argv,
    });
    let checked_in = server::client::check_in(checkin);
    crate::credentials::start(checked_in, setid);
}

/// This process, described without the server.
fn me() -> Option<Process> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    let host = starnix::process::process(host_self())?;
    let (path, argv) = match ME.get() {
        Some(me) => (me.path.clone(), me.argv.clone()),
        // Not started as a guest - a test, or a tool. Its host name is the
        // only one it has.
        None => {
            let exe = crate::exe::current().unwrap_or_default();
            (
                exe.to_string_lossy().into_owned(),
                std::env::args_os()
                    .map(|arg| arg.as_encoded_bytes().to_vec())
                    .collect(),
            )
        }
    };
    let arch = crate::sysctl::guest_arch();
    let credentials = crate::credentials::current();
    Some(Process {
        pid: pid(),
        // Without the server there is no telling whether the parent is a
        // guest, and a parent that is not one is launchd's place.
        ppid: server::registry::LAUNCHD,
        pgid: host.pgid,
        sid: host.sid,
        tty_major: host.tty_major,
        tty_minor: host.tty_minor,
        tpgid: host.tpgid,
        uid: credentials.ruid,
        euid: credentials.euid,
        suid: credentials.svuid,
        gid: credentials.rgid,
        egid: credentials.egid,
        sgid: credentials.svgid,
        groups: credentials.groups,
        state: host.state,
        nice: host.nice,
        start_sec: host.start_sec,
        start_usec: host.start_usec,
        path,
        argv,
        cputype: arch.cpu_type(),
        cpusubtype: arch.cpu_subtype(),
        translated: crate::sysctl::is_translated(),
        image: server::client::image(),
    })
}

/// Every guest process this one can see, in pid order.
pub fn list() -> Vec<Process> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    let mut list = server::client::processes().unwrap_or_default();
    if !list.iter().any(|process| process.pid == pid())
        && let Some(me) = me()
    {
        list.push(me);
        list.sort_by_key(|process| process.pid);
    }
    list
}

/// The guest process `pid`, if it is one this process can see.
pub fn describe(pid: i32) -> Option<Process> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    match server::client::process(pid) {
        Some(process) => Some(process),
        None if pid == self::pid() => me(),
        None => None,
    }
}

/// What `pid` has used. Answered locally for this process, which is the
/// common case and needs no round trip.
pub fn usage(pid: i32) -> Option<Usage> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    match host_pid(pid) == host_self() {
        true => starnix::process::usage(host_self()),
        false => server::client::usage(pid),
    }
}

pub fn threads(pid: i32) -> Option<Vec<Thread>> {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    match host_pid(pid) == host_self() {
        true => starnix::process::threads(host_self()),
        false => server::client::threads(pid),
    }
}

/// Whether `pid` is a guest process this one may hold a task port for.
pub fn visible(pid: i32) -> bool {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    pid == self::pid() || server::client::process(pid).is_some()
}

/// The host kernel's thread identifier, used by the process server.
/// macOS Mach ports are separate names and cannot index libproc's threads.
pub(crate) fn thread_id() -> i32 {
    #[cfg(target_os = "macos")]
    {
        let mut id = 0u64;
        unsafe { libc::pthread_threadid_np(0, &mut id) };
        id as i32
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::pthread::id() as i32
    }
}

/// Tell the server a thread's full name, which the host may have cut short.
pub fn name_thread(name: &str) {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    if server::client::configured() {
        server::client::name_thread(thread_id(), name);
    }
}

/// `pid_t fork(void)`.
///
/// The host's, and then the child checks in as itself straight away - before
/// `fork` returns to it - so it is listed even if it never asks the server
/// anything, which a daemonising child usually does not.
unsafe extern "C" fn fork() -> libc::pid_t {
    let _host = crate::syscall::ScopedHostSyscallContext::new();
    // SAFETY: the host's `fork`; the server's lock is held across it by the
    // client's own `pthread_atfork` handlers.
    let child = unsafe { libc::fork() };
    if child == 0 {
        server::client::forked();
    }
    child
}

/// `struct timeval` on a 64-bit Darwin: a `long` and an `int`, padded.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Timeval {
    sec: i64,
    usec: i32,
    _pad: i32,
}

/// `struct extern_proc`, the first half of `kinfo_proc`. Every pointer is a
/// kernel address on a Mac and zero here.
#[repr(C)]
#[derive(Clone, Copy)]
struct ExternProc {
    p_starttime: Timeval,
    p_vmspace: u64,
    p_sigacts: u64,
    p_flag: i32,
    p_stat: i8,
    p_pid: i32,
    p_oppid: i32,
    p_dupfd: i32,
    user_stack: u64,
    exit_thread: u64,
    p_debugger: i32,
    sigwait: i32,
    p_estcpu: u32,
    p_cpticks: i32,
    p_pctcpu: u32,
    p_wchan: u64,
    p_wmesg: u64,
    p_swtime: u32,
    p_slptime: u32,
    p_realtimer: [Timeval; 2],
    p_rtime: Timeval,
    p_uticks: u64,
    p_sticks: u64,
    p_iticks: u64,
    p_traceflag: i32,
    p_tracep: u64,
    p_siglist: i32,
    p_textvp: u64,
    p_holdcnt: i32,
    p_sigmask: u32,
    p_sigignore: u32,
    p_sigcatch: u32,
    p_priority: u8,
    p_usrpri: u8,
    p_nice: i8,
    p_comm: [u8; 17],
    p_pgrp: u64,
    p_addr: u64,
    p_xstat: u16,
    p_acflag: u16,
    p_ru: u64,
}

/// `struct _pcred`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Pcred {
    pc_lock: [u8; 72],
    pc_ucred: u64,
    p_ruid: u32,
    p_svuid: u32,
    p_rgid: u32,
    p_svgid: u32,
    p_refcnt: i32,
}

/// `struct _ucred`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Ucred {
    cr_ref: i32,
    cr_uid: u32,
    cr_ngroups: i16,
    cr_groups: [u32; 16],
}

/// `struct vmspace`, as user space declares it.
#[repr(C)]
#[derive(Clone, Copy)]
struct Vmspace {
    vm_refcnt: i32,
    vm_shm: u64,
    vm_rssize: i32,
    vm_swrss: i32,
    vm_tsize: i32,
    vm_dsize: i32,
    vm_ssize: i32,
    vm_taddr: u64,
    vm_daddr: u64,
    vm_maxsaddr: u64,
}

/// `struct eproc`, the second half.
#[repr(C)]
#[derive(Clone, Copy)]
struct Eproc {
    e_paddr: u64,
    e_sess: u64,
    e_pcred: Pcred,
    e_ucred: Ucred,
    e_vm: Vmspace,
    e_ppid: i32,
    e_pgid: i32,
    e_jobc: i16,
    e_tdev: i32,
    e_tpgid: i32,
    e_tsess: u64,
    e_wmesg: [u8; 8],
    e_xsize: i32,
    e_xrssize: i16,
    e_xccount: i16,
    e_xswrss: i16,
    e_flag: i32,
    e_login: [u8; 12],
    e_spare: [i32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KinfoProc {
    kp_proc: ExternProc,
    kp_eproc: Eproc,
}

const _: () = {
    use std::mem::offset_of;
    assert!(size_of::<ExternProc>() == 296);
    assert!(offset_of!(ExternProc, p_stat) == 36);
    assert!(offset_of!(ExternProc, p_pid) == 40);
    assert!(offset_of!(ExternProc, p_comm) == 243);
    assert!(offset_of!(ExternProc, p_ru) == 288);
    assert!(offset_of!(Eproc, e_pcred) + offset_of!(Pcred, p_ruid) == 392 - 296);
    assert!(offset_of!(Eproc, e_ucred) + offset_of!(Ucred, cr_uid) == 420 - 296);
    assert!(offset_of!(Eproc, e_ppid) == 560 - 296);
    assert!(offset_of!(Eproc, e_tdev) == 572 - 296);
    assert!(offset_of!(Eproc, e_login) == 616 - 296);
    assert!(size_of::<KinfoProc>() == 648);
};

/// `p_flag` bits.
const P_CONTROLT: i32 = 0x0000_0002;
const P_LP64: i32 = 0x0000_0004;
const P_EXEC: i32 = 0x0000_4000;
const P_TRANSLATED: i32 = 0x0002_0000;

/// `p_stat` values.
const SRUN: i8 = 2;
const SSLEEP: i8 = 3;
const SSTOP: i8 = 4;
const SZOMB: i8 = 5;

/// `e_flag` bits.
const EPROC_CTTY: i32 = 0x01;
const EPROC_SLEADER: i32 = 0x02;

/// `NODEV`, the terminal of a process without one.
const NODEV: i32 = -1;

/// Darwin's default user priority, `BASEPRI_DEFAULT`.
const BASEPRI_DEFAULT: u8 = 31;

/// The controlling terminal as the guest's `stat` would report the device,
/// which is the host's encoding passed through, so `devname` finds it in `/dev`.
fn terminal(process: &Process) -> i32 {
    if process.tty_major < 0 {
        return NODEV;
    }
    libc::makedev(process.tty_major as _, process.tty_minor as _) as i32
}

/// One `kinfo_proc` record.
pub fn kinfo_proc(process: &Process) -> Vec<u8> {
    // SAFETY: every field is an integer or an array of them, for which zero is
    // a valid value - and the right one for every kernel pointer.
    let mut record: KinfoProc = unsafe { std::mem::zeroed() };
    let has_tty = process.tty_major >= 0;

    let proc = &mut record.kp_proc;
    proc.p_starttime = Timeval {
        sec: process.start_sec,
        usec: process.start_usec,
        _pad: 0,
    };
    proc.p_flag = P_LP64 | P_EXEC;
    if has_tty {
        proc.p_flag |= P_CONTROLT;
    }
    if process.translated {
        proc.p_flag |= P_TRANSLATED;
    }
    proc.p_stat = match process.state {
        State::Running => SRUN,
        State::Sleeping | State::Waiting => SSLEEP,
        State::Stopped => SSTOP,
        State::Zombie => SZOMB,
    };
    proc.p_pid = process.pid;
    // FIXME (ACCURACY): Darwin's priority moves with load and nice; this is
    // its default, which is what a process that never changed either has.
    proc.p_priority = BASEPRI_DEFAULT;
    proc.p_usrpri = BASEPRI_DEFAULT;
    proc.p_nice = process.nice.clamp(-20, 20) as i8;
    let command = process.command().as_bytes();
    let n = command.len().min(proc.p_comm.len() - 1);
    proc.p_comm[..n].copy_from_slice(&command[..n]);

    let eproc = &mut record.kp_eproc;
    eproc.e_pcred.p_ruid = process.uid;
    eproc.e_pcred.p_svuid = process.suid;
    eproc.e_pcred.p_rgid = process.gid;
    eproc.e_pcred.p_svgid = process.sgid;
    eproc.e_ucred.cr_ref = 1;
    eproc.e_ucred.cr_uid = process.euid;
    // `cr_groups[0]` is the effective group, as XNU keeps it.
    let groups = std::iter::once(process.egid)
        .chain(
            process
                .groups
                .iter()
                .copied()
                .filter(|gid| *gid != process.egid),
        )
        .take(eproc.e_ucred.cr_groups.len());
    let mut count = 0;
    for (slot, gid) in eproc.e_ucred.cr_groups.iter_mut().zip(groups) {
        *slot = gid;
        count += 1;
    }
    eproc.e_ucred.cr_ngroups = count;
    eproc.e_ppid = process.ppid;
    eproc.e_pgid = process.pgid;
    eproc.e_tdev = terminal(process);
    eproc.e_tpgid = process.tpgid;
    if has_tty {
        eproc.e_flag |= EPROC_CTTY;
    }
    if process.pid == process.sid {
        eproc.e_flag |= EPROC_SLEADER;
    }

    // SAFETY: a plain-old-data structure viewed as its own bytes.
    unsafe { std::slice::from_raw_parts((&raw const record).cast::<u8>(), size_of::<KinfoProc>()) }
        .to_vec()
}

/// `KERN_PROCARGS2`: `argc`, the executable path, padding, then `argv` and
/// the environment, each NUL-terminated - the top of the process's initial
/// stack, as the kernel copies it out.
///
/// The environment is left empty: the server does not hold one, and another
/// process's environment is the last thing worth inventing.
pub fn procargs2(process: &Process) -> Vec<u8> {
    let mut out = (process.argv.len() as i32).to_ne_bytes().to_vec();
    out.extend_from_slice(process.path.as_bytes());
    out.push(0);
    // The kernel pads the path to a word, and `ps` skips NULs to find argv.
    while !out.len().is_multiple_of(size_of::<usize>()) {
        out.push(0);
    }
    for arg in &process.argv {
        out.extend_from_slice(arg);
        out.push(0);
    }
    out
}

/// `KERN_PROC` selectors.
const KERN_PROC_ALL: i32 = 0;
const KERN_PROC_PID: i32 = 1;
const KERN_PROC_PGRP: i32 = 2;
const KERN_PROC_SESSION: i32 = 3;
const KERN_PROC_TTY: i32 = 4;
const KERN_PROC_UID: i32 = 5;
const KERN_PROC_RUID: i32 = 6;

/// The `kinfo_proc` records `KERN_PROC` selects, concatenated.
///
/// `None` for a selector this does not know, which the caller reports as
/// unanswered rather than as an empty list.
pub fn select(selector: i32, argument: Option<i32>) -> Option<Vec<u8>> {
    let records = |keep: &dyn Fn(&Process) -> bool| -> Vec<u8> {
        list()
            .iter()
            .filter(|process| keep(process))
            .flat_map(kinfo_proc)
            .collect()
    };
    Some(match (selector, argument) {
        (KERN_PROC_ALL, _) => records(&|_| true),
        // One process, asked after directly; no table walk needed.
        (KERN_PROC_PID, Some(pid)) => describe(pid).map(|p| kinfo_proc(&p)).unwrap_or_default(),
        (KERN_PROC_PGRP, Some(pgid)) => records(&|p| p.pgid == pgid),
        (KERN_PROC_SESSION, Some(sid)) => records(&|p| p.sid == sid),
        (KERN_PROC_TTY, Some(tty)) => records(&|p| terminal(p) == tty),
        (KERN_PROC_UID, Some(uid)) => records(&|p| p.euid == uid as u32),
        (KERN_PROC_RUID, Some(uid)) => records(&|p| p.uid == uid as u32),
        _ => return None,
    })
}

exports::exports! {
    functions {
        "fork" => fork,
        "getpid" => getpid,
        "getppid" => getppid,
        "reboot" => reboot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(bytes: &[u8], at: usize) -> i32 {
        i32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn a_record_carries_its_identity_where_ps_reads_it() {
        let process = Process {
            pid: 4242,
            ppid: 1,
            pgid: 4242,
            sid: 4242,
            tty_major: -1,
            tty_minor: -1,
            uid: 501,
            euid: 501,
            state: State::Zombie,
            path: "/Applications/Some Long Application Name.app/Contents/MacOS/Some".into(),
            translated: true,
            ..Process::default()
        };
        let record = kinfo_proc(&process);
        assert_eq!(record.len(), 648);
        assert_eq!(word(&record, 40), 4242);
        assert_eq!(record[36] as i8, SZOMB);
        assert_ne!(word(&record, 32) & P_TRANSLATED, 0);
        assert_eq!(word(&record, 32) & P_CONTROLT, 0);
        assert_eq!(&record[243..248], b"Some\0");
        assert_eq!(word(&record, 392) as u32, 501);
        assert_eq!(word(&record, 420) as u32, 501);
        assert_eq!(word(&record, 560), 1);
        assert_eq!(word(&record, 572), NODEV);
        assert_ne!(word(&record, 612) & EPROC_SLEADER, 0);
    }

    #[test]
    fn a_long_command_is_cut_to_p_comm_and_terminated() {
        let process = Process {
            path: "/bin/abcdefghijklmnopqrstuvwxyz".into(),
            ..Process::default()
        };
        let record = kinfo_proc(&process);
        assert_eq!(&record[243..259], b"abcdefghijklmnop");
        assert_eq!(record[259], 0);
    }

    #[test]
    fn procargs_lead_with_argc_and_the_path_then_the_arguments() {
        let process = Process {
            path: "/usr/bin/top".into(),
            argv: vec![b"top".to_vec(), b"-l".to_vec(), b"1".to_vec()],
            ..Process::default()
        };
        let bytes = procargs2(&process);
        assert_eq!(word(&bytes, 0), 3);
        assert_eq!(&bytes[4..17], b"/usr/bin/top\0");
        let rest: Vec<&[u8]> = bytes[17..]
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(rest, vec![&b"top"[..], b"-l", b"1"]);
    }

    #[test]
    fn this_process_sees_itself_without_a_server() {
        let me = describe(pid()).expect("a process always sees itself");
        assert_eq!(me.pid, pid());
        assert!(list().iter().any(|p| p.pid == pid()));
        let records = select(KERN_PROC_PID, Some(pid())).unwrap();
        assert_eq!(records.len(), 648);
        assert!(select(KERN_PROC_PID, Some(i32::MAX)).unwrap().is_empty());
        assert!(usage(pid()).is_some_and(|u| u.resident_size > 0));
        assert!(threads(pid()).is_some_and(|t| !t.is_empty()));
        assert!(visible(pid()));
        assert!(!visible(1));
    }
}
