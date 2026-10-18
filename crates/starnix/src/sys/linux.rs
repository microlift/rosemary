//! Linux: `/proc`, `/sys` and `SO_PEERCRED`.

pub mod signal;

use std::io;
use std::os::fd::{BorrowedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::process::{
    Audit, Descriptor, DescriptorKind, FileId, Identity, InUse, Process, Region, State, Thread,
    Usage,
};
use crate::socket::Peer;
use crate::system::{CpuTicks, Memory, page_size};

/// `/proc/<pid>/stat` from the state onwards, split on whitespace.
///
/// The command name sits in parentheses before the state and may itself hold
/// spaces and parentheses, so the split starts after the *last* `)`.
struct Stat {
    fields: Vec<String>,
}

impl Stat {
    fn read(path: &str) -> Option<Stat> {
        let text = std::fs::read_to_string(path).ok()?;
        let rest = &text[text.rfind(')')? + 1..];
        Some(Stat {
            fields: rest.split_whitespace().map(str::to_owned).collect(),
        })
    }

    /// Field `n` as `proc(5)` numbers them, from one.
    fn field(&self, n: usize) -> &str {
        self.fields.get(n - 3).map_or("", String::as_str)
    }

    fn num(&self, n: usize) -> i64 {
        self.field(n).parse().unwrap_or(0)
    }

    fn state(&self) -> State {
        State::from_letter(self.field(3).bytes().next().unwrap_or(b'S'))
    }
}

fn ticks_per_second() -> u64 {
    static TICKS: OnceLock<u64> = OnceLock::new();
    // SAFETY: `sysconf` only reads its argument.
    *TICKS.get_or_init(|| (unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).max(1) as u64)
}

fn ticks_to_ns(ticks: i64) -> u64 {
    (ticks.max(0) as u64).saturating_mul(1_000_000_000) / ticks_per_second()
}

/// The wall-clock time the machine booted, in seconds.
fn boot_time() -> i64 {
    static BOOT: OnceLock<i64> = OnceLock::new();
    *BOOT.get_or_init(|| {
        std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("btime "))
                    .and_then(|value| value.trim().parse().ok())
            })
            .unwrap_or(0)
    })
}

/// A `Key:` line of `/proc/<pid>/status`, as its whitespace-separated words.
fn status_words<'a>(status: &'a str, key: &str) -> Vec<&'a str> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .map(|rest| rest.split_whitespace().collect())
        .unwrap_or_default()
}

fn status_number(status: &str, key: &str) -> u64 {
    status_words(status, key)
        .first()
        .and_then(|word| word.parse().ok())
        .unwrap_or(0)
}

pub fn identity(pid: i32) -> Option<Identity> {
    let stat = Stat::read(&format!("/proc/{pid}/stat"))?;
    let exe = std::fs::metadata(format!("/proc/{pid}/exe"))
        .map(|meta| (meta.dev(), meta.ino()))
        // A zombie has no executable any more, and is still the same process.
        .unwrap_or((0, 0));
    Some(Identity {
        start: stat.num(22) as u64,
        exe,
    })
}

pub fn process(pid: i32) -> Option<Process> {
    let stat = Stat::read(&format!("/proc/{pid}/stat"))?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let id = |key: &str, at: usize| -> u32 {
        status_words(&status, key)
            .get(at)
            .and_then(|word| word.parse().ok())
            .unwrap_or(0)
    };
    let tty = stat.num(7);
    let (tty_major, tty_minor) = match tty {
        0 => (-1, -1),
        nr => (
            ((nr >> 8) & 0xfff) as i32,
            ((nr & 0xff) | ((nr >> 12) & 0xfff00)) as i32,
        ),
    };
    let ticks = stat.num(22).max(0) as u64;
    let per_second = ticks_per_second();
    Some(Process {
        ppid: stat.num(4) as i32,
        pgid: stat.num(5) as i32,
        sid: stat.num(6) as i32,
        tty_major,
        tty_minor,
        tpgid: stat.num(8) as i32,
        uid: id("Uid:", 0),
        euid: id("Uid:", 1),
        suid: id("Uid:", 2),
        gid: id("Gid:", 0),
        egid: id("Gid:", 1),
        sgid: id("Gid:", 2),
        state: stat.state(),
        nice: stat.num(19) as i32,
        start_sec: boot_time() + (ticks / per_second) as i64,
        start_usec: ((ticks % per_second) * 1_000_000 / per_second) as i32,
    })
}

pub fn usage(pid: i32) -> Option<Usage> {
    let stat = Stat::read(&format!("/proc/{pid}/stat"))?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).unwrap_or_default();
    let mut pages = statm
        .split_whitespace()
        .map(|word| word.parse::<u64>().unwrap_or(0));
    let page = page_size();
    let virtual_size = pages.next().unwrap_or(0) * page;
    let resident_size = pages.next().unwrap_or(0) * page;
    let shared_size = pages.next().unwrap_or(0) * page;
    // Unreadable for a process that changed credentials, which leaves the
    // counters at zero rather than refusing the whole answer.
    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).unwrap_or_default();
    let io_number = |key: &str| status_number(&io, key);
    let regions = super::procmap::regions(&format!("/proc/{pid}/maps"))
        .map_or(0, |regions| regions.len() as u32);
    let threads = threads(pid).unwrap_or_default();
    Some(Usage {
        virtual_size,
        resident_size,
        resident_peak: (status_number(&status, "VmHWM:") * 1024).max(resident_size),
        shared_size,
        user_ns: ticks_to_ns(stat.num(14)),
        system_ns: ticks_to_ns(stat.num(15)),
        minor_faults: stat.num(10).max(0) as u64,
        major_faults: stat.num(12).max(0) as u64,
        voluntary_switches: status_number(&status, "voluntary_ctxt_switches:"),
        involuntary_switches: status_number(&status, "nonvoluntary_ctxt_switches:"),
        threads: (threads.len() as u32).max(stat.num(20).max(0) as u32),
        running_threads: threads
            .iter()
            .filter(|thread| thread.state == State::Running)
            .count() as u32,
        regions,
        read_bytes: io_number("read_bytes:"),
        write_bytes: io_number("write_bytes:"),
        start_ns: ticks_to_ns(stat.num(22)),
    })
}

fn thread(pid: i32, tid: i32) -> Option<Thread> {
    let base = format!("/proc/{pid}/task/{tid}");
    let stat = Stat::read(&format!("{base}/stat"))?;
    let name = std::fs::read_to_string(format!("{base}/comm"))
        .map(|name| name.trim_end_matches('\n').to_owned())
        .unwrap_or_default();
    Some(Thread {
        tid,
        name,
        user_ns: ticks_to_ns(stat.num(14)),
        system_ns: ticks_to_ns(stat.num(15)),
        state: stat.state(),
        nice: stat.num(19) as i32,
    })
}

pub fn threads(pid: i32) -> Option<Vec<Thread>> {
    // The kernel walks `task/` from the thread it last listed, and stops early
    // if that thread exits mid-walk; the threads after it go missing. The one
    // that exited was listed and no longer reads, so that is the sign to list
    // again and keep what either pass found.
    let mut found = std::collections::BTreeMap::new();
    for _ in 0..8 {
        let mut vanished = false;
        for entry in std::fs::read_dir(format!("/proc/{pid}/task"))
            .ok()?
            .flatten()
        {
            let Some(tid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
                continue;
            };
            match thread(pid, tid) {
                Some(thread) => {
                    found.insert(tid, thread);
                }
                None => vanished = true,
            }
        }
        if !vanished {
            break;
        }
    }
    // What an earlier pass found may have exited since.
    found.retain(|&tid, _| std::path::Path::new(&format!("/proc/{pid}/task/{tid}")).exists());
    Some(found.into_values().collect())
}

pub fn has_thread(tid: i32) -> bool {
    std::path::Path::new(&format!("/proc/self/task/{tid}")).exists()
}

pub fn command_line(pid: i32) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/cmdline")).ok()
}

pub fn descriptors() -> io::Result<Vec<Descriptor>> {
    Ok(descriptors_in("/proc/self/fd")?
        .map(|(descriptor, _)| descriptor)
        .collect())
}

/// The descriptors a `/proc/<pid>/fd` directory lists, each with its entry
/// when it leads to a file that can be stat'ed through `/proc`.
fn descriptors_in(
    dir: &str,
) -> io::Result<impl Iterator<Item = (Descriptor, Option<std::fs::DirEntry>)> + use<>> {
    Ok(std::fs::read_dir(dir)?.flatten().filter_map(|entry| {
        let fd = entry.file_name().to_str()?.parse::<i32>().ok()?;
        let target = std::fs::read_link(entry.path()).unwrap_or_default();
        let target = target.to_string_lossy();
        let kind = match () {
            _ if target.starts_with("socket:") => DescriptorKind::Socket,
            _ if target.starts_with("pipe:") => DescriptorKind::Pipe,
            _ if target.contains("[eventpoll]") => DescriptorKind::Queue,
            _ => DescriptorKind::File,
        };
        // An `anon_inode:` link is an eventfd, a timerfd and the like: an
        // object of the kernel's, not a file anyone else can name.
        let file =
            (kind == DescriptorKind::File && !target.starts_with("anon_inode:")).then_some(entry);
        Some((Descriptor { fd, kind }, file))
    }))
}

/// The device and inode of what `path` leads to.
fn file_id(path: &std::path::Path) -> Option<FileId> {
    std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

pub fn in_use(pid: i32) -> Option<InUse> {
    let base = format!("/proc/{pid}");
    let mut directories: Vec<FileId> = ["cwd", "root"]
        .iter()
        .filter_map(|link| file_id(std::path::Path::new(&format!("{base}/{link}"))))
        .collect();
    let mut open: Vec<FileId> = descriptors_in(&format!("{base}/fd"))
        .ok()?
        .filter_map(|(_, file)| file_id(&file?.path()))
        .collect();
    for files in [&mut directories, &mut open] {
        files.sort_unstable();
        files.dedup();
    }
    Some(InUse {
        directories,
        open,
        mapped: mapped_files(&base),
    })
}

/// What `/proc/<pid>` shows mapped from files. `map_files` leads to each
/// file itself; without leave to read it, `maps` names them, and each path is
/// stat'ed for the device `stat` reports, which on a filesystem with
/// subvolumes is not the one `maps` gives.
fn mapped_files(base: &str) -> Vec<FileId> {
    let mut files: Vec<FileId> = match std::fs::read_dir(format!("{base}/map_files")) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|entry| file_id(&entry.path()))
            .collect(),
        Err(_) => Vec::new(),
    };
    if files.is_empty() {
        let backing = super::procmap::backing_files(&format!("{base}/maps")).unwrap_or_default();
        files = backing
            .into_iter()
            .map(|file| {
                // The path is the process's own, so it is read through its root.
                let found = file.path.as_ref().and_then(|path| {
                    file_id(
                        &PathBuf::from(format!("{base}/root"))
                            .join(path.strip_prefix("/").unwrap_or(path)),
                    )
                });
                // A different file at the path is not the one mapped.
                found
                    .filter(|&(_, inode)| inode == file.inode)
                    .unwrap_or((file.dev, file.inode))
            })
            .collect();
    }
    files.sort_unstable();
    files.dedup();
    files
}

pub fn regions() -> io::Result<Vec<Region>> {
    super::procmap::regions("/proc/self/maps")
}

pub fn audit() -> Audit {
    /// Unset, in both `loginuid` and `sessionid`.
    const UNSET: u32 = u32::MAX;
    let id = |path: &str| {
        let id: u32 = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
        (id != UNSET).then_some(id)
    };
    Audit {
        login_uid: id("/proc/self/loginuid"),
        session: id("/proc/self/sessionid"),
    }
}

/// The value of `key` in `/proc/meminfo`, in bytes.
fn meminfo(text: &str, key: &str) -> u64 {
    text.lines()
        .find_map(|line| line.strip_prefix(key)?.strip_suffix(" kB"))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_mul(1024)
}

pub fn memory() -> Memory {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    Memory {
        free: meminfo(&text, "MemFree:"),
        active: meminfo(&text, "Active:"),
        inactive: meminfo(&text, "Inactive:"),
        wired: meminfo(&text, "Unevictable:"),
        file_backed: meminfo(&text, "Cached:"),
        anonymous: meminfo(&text, "AnonPages:"),
    }
}

pub fn cpu_ticks() -> CpuTicks {
    let text = std::fs::read_to_string("/proc/stat").unwrap_or_default();
    let Some(totals) = text.lines().find_map(|line| line.strip_prefix("cpu ")) else {
        return CpuTicks::default();
    };
    // `user nice system idle ...`, in that order.
    let mut fields = totals
        .split_whitespace()
        .map(|field| field.parse::<u64>().unwrap_or(0));
    let mut next = || fields.next().unwrap_or(0);
    CpuTicks {
        user: next(),
        nice: next(),
        system: next(),
        idle: next(),
    }
}

/// Distinct sibling sets in sysfs: one per core, however many threads it runs.
pub fn physical_cores() -> Option<u32> {
    let mut cores = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir("/sys/devices/system/cpu").ok()?.flatten() {
        let name = entry.file_name();
        let Some(index) = name.to_str().and_then(|n| n.strip_prefix("cpu")) else {
            continue;
        };
        if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(siblings) =
            std::fs::read_to_string(entry.path().join("topology/thread_siblings_list"))
        {
            cores.insert(siblings.trim().to_string());
        }
    }
    (!cores.is_empty()).then_some(cores.len() as u32)
}

pub fn cpu_brand() -> Option<String> {
    let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        matches!(key.trim(), "model name" | "Model name" | "Processor")
            .then(|| value.trim().to_string())
    })
}

pub fn peer(fd: RawFd) -> Option<Peer> {
    // SAFETY: `fd` is expected to be an open file descriptor
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let cred = rustix::net::sockopt::socket_peercred(fd).ok()?;
    Some(Peer {
        pid: Some(cred.pid.as_raw_pid()),
        uid: cred.uid.as_raw(),
    })
}

pub fn fd_path(fd: RawFd) -> Option<PathBuf> {
    Some(PathBuf::from(format!("/proc/self/fd/{fd}")))
}

pub fn fd_target(fd: RawFd) -> Option<PathBuf> {
    std::fs::read_link(fd_path(fd)?).ok()
}

pub fn errno_location() -> *mut libc::c_int {
    // SAFETY: takes no argument and returns this thread's own slot.
    unsafe { libc::__errno_location() }
}

fn xattr_result(rc: libc::ssize_t) -> io::Result<usize> {
    usize::try_from(rc).map_err(|_| io::Error::last_os_error())
}

pub fn xattr_get_fd(fd: RawFd, name: &std::ffi::CStr, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: a NUL-terminated name and a buffer of its own length.
    xattr_result(unsafe { libc::fgetxattr(fd, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) })
}

pub fn xattr_get_path(
    path: &std::ffi::CStr,
    name: &std::ffi::CStr,
    buf: &mut [u8],
    follow: bool,
) -> io::Result<usize> {
    let get = if follow {
        libc::getxattr
    } else {
        libc::lgetxattr
    };
    // SAFETY: NUL-terminated strings and a buffer of its own length.
    xattr_result(unsafe {
        get(
            path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    })
}

fn xattr_status(rc: libc::c_int) -> io::Result<()> {
    match rc {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

pub fn xattr_set_fd(fd: RawFd, name: &std::ffi::CStr, value: &[u8]) -> io::Result<()> {
    // SAFETY: a NUL-terminated name and the value's own bytes.
    xattr_status(unsafe {
        libc::fsetxattr(fd, name.as_ptr(), value.as_ptr().cast(), value.len(), 0)
    })
}

pub fn xattr_set_path(
    path: &std::ffi::CStr,
    name: &std::ffi::CStr,
    value: &[u8],
    follow: bool,
) -> io::Result<()> {
    let set = if follow {
        libc::setxattr
    } else {
        libc::lsetxattr
    };
    // SAFETY: NUL-terminated strings and the value's own bytes.
    xattr_status(unsafe {
        set(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    })
}

pub fn xattr_remove_fd(fd: RawFd, name: &std::ffi::CStr) -> io::Result<()> {
    // SAFETY: a NUL-terminated name.
    xattr_status(unsafe { libc::fremovexattr(fd, name.as_ptr()) })
}

pub fn xattr_remove_path(
    path: &std::ffi::CStr,
    name: &std::ffi::CStr,
    follow: bool,
) -> io::Result<()> {
    let remove = if follow {
        libc::removexattr
    } else {
        libc::lremovexattr
    };
    // SAFETY: NUL-terminated strings.
    xattr_status(unsafe { remove(path.as_ptr(), name.as_ptr()) })
}

pub fn xattr_is_missing(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ENODATA)
}

/// What `/sys/class/net/<name>` says carries the interface.
pub fn link_medium(name: &std::ffi::CStr) -> Option<crate::net::Link> {
    use crate::net::Link;
    let dir = std::path::Path::new("/sys/class/net").join(name.to_str().ok()?);
    if dir.join("wireless").exists() || dir.join("phy80211").exists() {
        return Some(Link::Wifi);
    }
    let uevent = std::fs::read_to_string(dir.join("uevent")).unwrap_or_default();
    if uevent.lines().any(|line| line == "DEVTYPE=wwan") {
        return Some(Link::Cellular);
    }
    // Bridges, veths and the like frame as Ethernet too, but only hardware
    // has a `device`; tunnels and PPP are other ARPHRD kinds.
    let kind = std::fs::read_to_string(dir.join("type")).ok()?;
    (kind.trim() == "1" && dir.join("device").exists()).then_some(Link::Wired)
}
