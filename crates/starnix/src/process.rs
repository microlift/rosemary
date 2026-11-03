//! Processes as the host kernel sees them.

use std::io;

use crate::sys;

/// What makes a pid *this* process rather than a later one that reused the
/// number, or the same process after it `exec`ed something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Identity {
    /// When it started, in the host's own units; unchanged by `exec`.
    pub start: u64,
    /// The device and inode of its executable, which `exec` does change.
    /// `(0, 0)` where the host cannot say, as for a zombie.
    pub exe: (u64, u64),
}

/// A process's scheduling state, in the coarse terms every host shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    Running,
    Sleeping,
    /// Uninterruptible: waiting on I/O and not answering signals.
    Waiting,
    Stopped,
    Zombie,
}

impl State {
    /// A one-letter state, which Linux and the BSDs spell alike for the states
    /// they share.
    pub fn from_letter(letter: u8) -> State {
        match letter {
            b'R' => State::Running,
            b'D' => State::Waiting,
            b'T' | b't' => State::Stopped,
            b'Z' | b'X' => State::Zombie,
            _ => State::Sleeping,
        }
    }
}

/// One process's record: what `kinfo_proc` is built from.
#[derive(Clone, Debug, Default)]
pub struct Process {
    pub ppid: i32,
    pub pgid: i32,
    pub sid: i32,
    /// The controlling terminal as a major and minor number, or -1 for none.
    pub tty_major: i32,
    pub tty_minor: i32,
    pub tpgid: i32,
    pub uid: u32,
    pub euid: u32,
    pub suid: u32,
    pub gid: u32,
    pub egid: u32,
    pub sgid: u32,
    pub state: State,
    pub nice: i32,
    /// Wall-clock start time.
    pub start_sec: i64,
    pub start_usec: i32,
}

impl Process {
    pub fn has_tty(&self) -> bool {
        self.tty_major >= 0
    }
}

/// A process's resource use.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub virtual_size: u64,
    pub resident_size: u64,
    pub resident_peak: u64,
    /// Resident memory shared with another process; what is not shared is
    /// the closest thing to Mach's `phys_footprint`.
    pub shared_size: u64,
    pub user_ns: u64,
    pub system_ns: u64,
    pub minor_faults: u64,
    pub major_faults: u64,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
    pub threads: u32,
    pub running_threads: u32,
    pub regions: u32,
    pub read_bytes: u64,
    pub write_bytes: u64,
    /// When the process started, on the host's monotonic clock, in
    /// nanoseconds.
    pub start_ns: u64,
}

/// What a process has mapped and resident: the memory figures of [`Usage`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Footprint {
    pub virtual_size: u64,
    pub resident_size: u64,
    pub resident_peak: u64,
    /// As [`Usage::shared_size`].
    pub shared_size: u64,
}

impl From<&Usage> for Footprint {
    fn from(usage: &Usage) -> Footprint {
        Footprint {
            virtual_size: usage.virtual_size,
            resident_size: usage.resident_size,
            resident_peak: usage.resident_peak,
            shared_size: usage.shared_size,
        }
    }
}

/// One thread of a process.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Thread {
    pub tid: i32,
    /// The host's name for it, which the host may have cut short.
    pub name: String,
    pub user_ns: u64,
    pub system_ns: u64,
    pub state: State,
    pub nice: i32,
}

/// What an open descriptor refers to, as coarsely as every host can say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorKind {
    /// A file, directory, device or anything else with a vnode.
    File,
    Socket,
    /// A pipe or FIFO.
    Pipe,
    /// The host's event queue: kqueue, or epoll standing in for it.
    Queue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub fd: i32,
    pub kind: DescriptorKind,
}

/// One mapping in this process's address space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    /// Mapped shared rather than private, where the host says.
    pub shared: bool,
    /// The offset into the backing object.
    pub offset: u64,
}

/// The audit ids the host keeps for this process, where it keeps any.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Audit {
    /// The uid the session logged in as, which `su` does not change.
    pub login_uid: Option<u32>,
    pub session: Option<u32>,
}

/// This process's pid.
pub fn me() -> i32 {
    rustix::process::getpid().as_raw_pid()
}

pub fn identity(pid: i32) -> Option<Identity> {
    sys::identity(pid)
}

/// Whether `pid` is still the process `identity` was taken from.
///
/// A zombie keeps its start time and loses its executable, and is still the
/// process: its parent has yet to collect it, and `ps` shows it until then.
pub fn alive(pid: i32, identity: &Identity) -> bool {
    match self::identity(pid) {
        Some(now) => now.start == identity.start && (now.exe == identity.exe || now.exe == (0, 0)),
        None => false,
    }
}

pub fn process(pid: i32) -> Option<Process> {
    sys::process(pid)
}

pub fn usage(pid: i32) -> Option<Usage> {
    sys::usage(pid)
}

/// `pid`'s [`Footprint`], which costs far less than [`usage`] where the host
/// makes the rest expensive: Linux counts threads by reading each one's `stat`,
/// milliseconds in a process with a hundred of them.
pub fn footprint(pid: i32) -> Option<Footprint> {
    sys::footprint(pid)
}

/// `pid`'s threads, in tid order.
pub fn threads(pid: i32) -> Option<Vec<Thread>> {
    let mut threads = sys::threads(pid)?;
    threads.sort_by_key(|thread| thread.tid);
    Some(threads)
}

/// Whether `tid` is one of this process's threads.
pub fn has_thread(tid: i32) -> bool {
    sys::has_thread(tid)
}

/// `pid`'s arguments as the kernel holds them: each NUL-terminated, back to
/// back.
pub fn command_line(pid: i32) -> Option<Vec<u8>> {
    sys::command_line(pid)
}

/// This process's open descriptors, in fd order.
pub fn descriptors() -> io::Result<Vec<Descriptor>> {
    let mut fds = sys::descriptors()?;
    fds.sort_by_key(|entry| entry.fd);
    Ok(fds)
}

/// This process's mappings, in address order.
pub fn regions() -> io::Result<Vec<Region>> {
    regions_of(me())
}

/// `pid`'s mappings, in address order. Reading another process's map is a
/// question about it, not access to it, so hosts that guard their memory
/// against other processes (Yama) still answer it for the same user.
pub fn regions_of(pid: i32) -> io::Result<Vec<Region>> {
    let mut regions = sys::regions(pid)?;
    regions.sort_by_key(|region| region.start);
    Ok(regions)
}

pub fn audit() -> Audit {
    sys::audit()
}

/// A file by the host's device and inode for it, as `stat` gives them.
pub type FileId = (u64, u64);

/// The files a process is using, as `proc_listpidspath` looks for them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InUse {
    /// Its working directory and its root directory, sorted and deduplicated.
    pub directories: Vec<FileId>,
    /// What its descriptors have open, other than sockets, pipes and the
    /// host's event objects. Sorted and deduplicated.
    pub open: Vec<FileId>,
    /// What it has mapped from files, sorted and deduplicated.
    pub mapped: Vec<FileId>,
}

/// The files `pid` is using, or `None` for a process the host does not show
/// this one. Where the host shows only this process's own, its mappings are
/// left out.
pub fn in_use(pid: i32) -> Option<InUse> {
    sys::in_use(pid).or_else(|| (pid == me()).then(own_in_use))
}

/// What a host that does not show other processes' files can still tell about
/// this one's.
fn own_in_use() -> InUse {
    use std::os::unix::fs::MetadataExt;
    let id = |meta: std::fs::Metadata| (meta.dev(), meta.ino());
    let directories = [".", "/"]
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok().map(id))
        .collect();
    let open = descriptors()
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.kind == DescriptorKind::File)
        .filter_map(|entry| {
            // SAFETY: a zeroed `struct stat` is what the host is asked to fill.
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: `st` is a live local; a closed descriptor is refused.
            (unsafe { libc::fstat(entry.fd, &mut st) } == 0)
                .then_some((st.st_dev as u64, st.st_ino as u64))
        })
        .collect();
    let mut used = InUse {
        directories,
        open,
        mapped: Vec::new(),
    };
    for files in [&mut used.directories, &mut used.open] {
        files.sort_unstable();
        files.dedup();
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `proc_listpidspath` looks for: the working directory, an open
    /// file, and on Linux the executable this test runs from.
    #[test]
    fn a_process_shows_the_files_it_is_using() {
        use std::os::unix::fs::MetadataExt;
        let id = |path: &std::path::Path| {
            let meta = std::fs::metadata(path).unwrap();
            (meta.dev(), meta.ino())
        };
        let path = std::env::temp_dir().join(format!("starnix-in-use-{}", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let _cleanup = Cleanup(path.clone());
        let used = in_use(me()).unwrap();
        assert!(
            used.directories
                .contains(&id(&std::env::current_dir().unwrap()))
        );
        assert!(used.open.contains(&id(&path)));
        #[cfg(target_os = "linux")]
        assert!(used.mapped.contains(&id(&std::env::current_exe().unwrap())));
        drop(file);
    }

    /// Removes the file even when an assertion fails first.
    struct Cleanup(std::path::PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn this_process_is_alive_under_its_own_identity() {
        let identity = identity(me()).unwrap();
        assert!(alive(me(), &identity));
        let other = Identity {
            start: identity.start + 1,
            ..identity
        };
        assert!(!alive(me(), &other), "a reused pid passed for the original");
    }

    #[test]
    fn this_process_reads_back_its_own_credentials_and_threads() {
        let process = process(me()).unwrap();
        // SAFETY: neither has preconditions.
        assert_eq!(process.uid, unsafe { libc::getuid() });
        assert_eq!(process.ppid, unsafe { libc::getppid() });
        assert!(process.start_sec > 0);

        let blocker = std::sync::Arc::new(std::sync::Barrier::new(2));
        let held = blocker.clone();
        let other = std::thread::Builder::new()
            .name("starnix-probe".into())
            .spawn(move || {
                held.wait();
                held.wait();
            })
            .unwrap();
        // The new thread names itself before running its closure, so once it
        // reaches the barrier the name is there to find.
        blocker.wait();
        let threads = threads(me()).unwrap();
        blocker.wait();
        other.join().unwrap();
        assert!(threads.len() >= 2);
        assert!(threads.iter().any(|thread| thread.name == "starnix-probe"));
        // The main thread, whose tid is the pid on every host that numbers
        // threads that way, outlives any test.
        assert!(threads.iter().any(|thread| has_thread(thread.tid)));
        assert!(!has_thread(i32::MAX));
    }

    #[test]
    fn this_process_has_memory_and_has_used_time() {
        let usage = usage(me()).unwrap();
        assert!(usage.resident_size > 0);
        assert!(usage.virtual_size >= usage.resident_size);
        assert!(usage.resident_peak >= usage.resident_size);
        assert!(usage.regions > 0);
        assert!(usage.threads >= 1);
    }

    #[test]
    fn a_pid_nobody_has_is_not_answered() {
        assert!(identity(i32::MAX).is_none());
        assert!(process(i32::MAX).is_none());
        assert!(usage(i32::MAX).is_none());
        assert!(threads(i32::MAX).is_none());
        assert!(command_line(i32::MAX).is_none());
    }

    #[test]
    fn this_process_command_line_starts_with_its_program() {
        let line = command_line(me()).unwrap();
        let first = line.split(|b| *b == 0).next().unwrap();
        let arg0 = std::env::args_os().next().unwrap();
        assert_eq!(first, arg0.as_encoded_bytes());
    }

    #[test]
    fn the_descriptor_list_holds_an_open_pipe() {
        let mut ends = [0; 2];
        // SAFETY: two writable ints.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        let fds = descriptors().unwrap();
        // SAFETY: closing the two descriptors just opened.
        unsafe {
            libc::close(ends[0]);
            libc::close(ends[1]);
        }
        let read = fds.iter().find(|entry| entry.fd == ends[0]).unwrap();
        assert_eq!(read.kind, DescriptorKind::Pipe);
        assert!(fds.windows(2).all(|pair| pair[0].fd < pair[1].fd));
    }

    #[test]
    fn this_functions_code_is_in_an_executable_region() {
        let here = this_functions_code_is_in_an_executable_region as *const () as u64;
        let regions = regions().unwrap();
        let region = regions
            .iter()
            .find(|region| (region.start..region.end).contains(&here))
            .unwrap();
        assert!(region.read && region.execute && !region.write);
    }
}
